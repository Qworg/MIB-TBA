//! TBA-E18 — the `cast` command, spell mana, and transient buffs.
//!
//! CircleMUD's `cast '<spell>' [target]` has no structured protocol variant:
//! [`Command`](mib_protocol::prelude::Command) stops at `Attack`/`Flee`. So
//! [`handle_cast_command`] parses [`PlayerInput::RawLine`] itself, running in
//! [`BridgeSet::Game`](mib_core::prelude::BridgeSet) after the stdlib router
//! (the pack declares `cast` in `content/stdlib.verbs.yaml` so the router does
//! not reject it as an unknown verb before this system sees it).
//!
//! The spell catalog is [`TbaContent::spells`](crate::module::TbaContent::spells)
//! (Circle's `spell_info[]`, authored in `content/tba.spells.yaml`). The
//! caster's class `granted_skills` gate who may cast what, exactly as tbaMUD's
//! `do_cast` checks `GET_SKILL`. Effects are data-driven from each
//! [`SpellDef`]: a `Damage` roll, a capped `Heal`, an applied `Buff` affect, or
//! a `Detect`/`Utility` message.
//!
//! ## Buffs
//!
//! [`TbaBuffs`] is a **transient** runtime component: a list of [`ActiveBuff`]s,
//! each an [`AffectDef`] and a countdown. [`tick_buffs`] runs on the existing
//! [`TbaRound`](crate::combat::TbaRound) schedule (one tick per 3-second combat
//! round) and expires them; the affect recompute in
//! [`recompute_equipment_affects`](crate::equipment::recompute_equipment_affects)
//! includes the live buff affects in the same no-stacking base + worn + buffs
//! rebuild, so a buff is never applied twice and expires cleanly. Buffs are not
//! serialised and do not survive a restart — they are combat-round state, like
//! a melee target.

use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{Location, Npc, Player, PlayerDefeated, Session, SessionState};
use smol_str::SmolStr;

use crate::combat::commands::{broadcast, deny, entity_name, find_target_in_room, text};
use crate::combat::{TbaCombatant, TbaProgress, TbaRng, default_class};
use crate::content::{AffectDef, Position, SpellDef, SpellKind, SpellTarget};
use crate::module::TbaContent;

/// Combat rounds a cast buff lasts.
///
/// Circle's affects age on a different clock and `tba.spells` carries no
/// duration (the per-spell pulse counts live in `src/spells.c`'s
/// `mag_affects`), so a buff here lasts a fixed, documented number of
/// [`TbaRound`](crate::combat::TbaRound)s. A later pass can thread the source
/// duration through [`SpellDef`] without changing the cast/buff plumbing.
pub const DEFAULT_BUFF_ROUNDS: u32 = 6;

/// One live buff: the affect it contributes and the rounds left before it
/// expires.
#[derive(Debug, Clone, PartialEq, Eq, Component)]
pub struct ActiveBuff {
    pub affect: AffectDef,
    pub rounds_left: u32,
}

/// A combatant's transient buff list.
///
/// Deliberately not serialisable: buffs are combat-round state and the death /
/// restart paths do not persist them (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Component)]
pub struct TbaBuffs {
    pub buffs: Vec<ActiveBuff>,
}

impl TbaBuffs {
    /// The affects currently contributed by live buffs.
    pub fn affects(&self) -> impl Iterator<Item = &AffectDef> {
        self.buffs.iter().map(|buff| &buff.affect)
    }
}

/// `TbaRound`: age every buff by one round, dropping the expired ones.
///
/// The next frame's affect recompute rebuilds the combatant without the
/// dropped affects, so a buff expires without any second "remove" path.
pub fn tick_buffs(mut buffs: Query<&mut TbaBuffs>) {
    for mut store in &mut buffs {
        store.buffs.retain_mut(|buff| {
            buff.rounds_left = buff.rounds_left.saturating_sub(1);
            buff.rounds_left > 0
        });
    }
}

// -- line parsing -----------------------------------------------------------

/// A parsed `cast` line, before the spell name is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CastLine {
    /// The spell phrase (quoted content, or the whole unquoted remainder).
    spell: String,
    /// The target, when it was unambiguous (quoted form) or split off later.
    target: Option<String>,
    /// Whether the spell name was quoted.
    quoted: bool,
}

/// Parse a raw line as `cast ...`, or `None` when it is not a cast command.
///
/// Both `cast 'magic missile' goblin` and `cast magic missile goblin` parse;
/// the unquoted form leaves the spell/target boundary to
/// [`split_unquoted`], which resolves the longest known spell phrase.
fn parse_cast_line(line: &str) -> Option<CastLine> {
    let line = line.trim();
    let (verb, rest) = match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], &line[index + 1..]),
        None => (line, ""),
    };
    if !verb.eq_ignore_ascii_case("cast") {
        return None;
    }
    let rest = rest.trim();
    if rest.is_empty() {
        return Some(CastLine {
            spell: String::new(),
            target: None,
            quoted: false,
        });
    }
    for quote in ['\'', '"'] {
        if let Some(after) = rest.strip_prefix(quote) {
            let close = after.find(quote)?;
            let spell = after[..close].trim().to_owned();
            let target = after[close + quote.len_utf8()..].trim();
            return Some(CastLine {
                spell,
                target: (!target.is_empty()).then(|| target.to_owned()),
                quoted: true,
            });
        }
    }
    Some(CastLine {
        spell: rest.to_owned(),
        target: None,
        quoted: false,
    })
}

/// Fold a spell id or display name to a comparable form: lowercase, `_` to a
/// space, whitespace collapsed. `magic_missile` and `Magic Missile` both
/// become `magic missile`.
fn normalize(input: &str) -> String {
    input
        .to_lowercase()
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve a spell phrase against the catalog by id or display name.
fn resolve_spell<'a>(content: &'a TbaContent, phrase: &str) -> Option<(&'a SmolStr, &'a SpellDef)> {
    let needle = normalize(phrase);
    if needle.is_empty() {
        return None;
    }
    content
        .spells
        .iter()
        .find(|(id, def)| normalize(id) == needle || normalize(&def.name) == needle)
}

/// Split an unquoted `cast` remainder into the longest known spell phrase plus
/// an optional target (`cast magic missile goblin` -> `magic missile`,
/// `goblin`; `cast fireball` -> `fireball`, no target).
fn split_unquoted<'a>(
    content: &'a TbaContent,
    remainder: &str,
) -> Option<(&'a SmolStr, &'a SpellDef, Option<String>)> {
    let words: Vec<&str> = remainder.split_whitespace().collect();
    for take in (1..=words.len()).rev() {
        let phrase = words[..take].join(" ");
        if let Some((id, def)) = resolve_spell(content, &phrase) {
            let target = words[take..].join(" ");
            return Some((id, def, (!target.is_empty()).then_some(target)));
        }
    }
    None
}

/// Whether a spell is aimed aggressively (Circle's `violent` flag, folded to
/// the `Damage` kind): a hostile spell needs an explicit victim and cannot be
/// aimed at the caster.
fn is_hostile(spell: &SpellDef) -> bool {
    spell.kind == SpellKind::Damage
}

/// Install (or refresh) a buff affect on `target`.
///
/// A re-cast of the same apply location replaces the old entry rather than
/// stacking it, which is what keeps the recompute's additive affects bounded.
fn add_buff(
    target: Entity,
    affect: AffectDef,
    buffs: &mut Query<&mut TbaBuffs>,
    commands: &mut Commands,
) {
    let entry = ActiveBuff {
        affect,
        rounds_left: DEFAULT_BUFF_ROUNDS,
    };
    match buffs.get_mut(target) {
        Ok(mut store) => {
            let location = entry.affect.location;
            store.buffs.retain(|buff| buff.affect.location != location);
            store.buffs.push(entry);
        }
        Err(_) => {
            commands
                .entity(target)
                .insert(TbaBuffs { buffs: vec![entry] });
        }
    }
}

/// `BridgeSet::Game`: translate `cast '<spell>' [target]` into a spell effect.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_cast_command(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    players: Query<(Entity, &Player, &Location)>,
    npcs: Query<(Entity, &Npc, &Location)>,
    content: Option<Res<TbaContent>>,
    mut combatants: Query<&mut TbaCombatant>,
    progress: Query<&TbaProgress>,
    mut buffs: Query<&mut TbaBuffs>,
    mut rng: ResMut<TbaRng>,
    mut defeated: MessageWriter<PlayerDefeated>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    for msg in reader.read() {
        let session = msg.session;
        let playing = sessions
            .get(session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        let PlayerInput::RawLine(line) = &msg.input else {
            continue;
        };
        let Some(parsed) = parse_cast_line(line) else {
            continue;
        };

        // Resolve the spell id. Quoted form resolves the phrase directly; the
        // unquoted form greedily peels the longest known spell off the front.
        let resolved = if parsed.quoted {
            resolve_spell(content, &parsed.spell)
                .map(|(id, def)| (id.clone(), def, parsed.target.clone()))
        } else {
            split_unquoted(content, &parsed.spell)
                .map(|(id, def, target)| (id.clone(), def, target))
        };
        let Some((spell_id, spell, target_arg)) = resolved else {
            deny(&mut output, session, "Cast what?!");
            continue;
        };
        let spell_name = spell.name.clone();

        let Ok(progress) = progress.get(session) else {
            deny(&mut output, session, "You have no spellcasting knowledge.");
            continue;
        };
        let Ok(caster) = combatants.get(session) else {
            deny(&mut output, session, "You have no combat state.");
            continue;
        };
        if !caster.is_alive() {
            deny(&mut output, session, "You can't cast spells while dead.");
            continue;
        }
        let position = caster.position;
        let mana = caster.mana;

        if position < spell.min_position {
            deny(
                &mut output,
                session,
                "You can't concentrate enough to cast that.",
            );
            continue;
        }

        // The class gate. `granted_skills` is the same vocabulary as the spell
        // ids, so a mismatch is denied by name.
        let class = content
            .classes
            .get(&progress.class)
            .cloned()
            .unwrap_or_else(default_class);
        let known = class
            .granted_skills
            .iter()
            .any(|skill| normalize(skill) == normalize(&spell_id));
        if !known {
            deny(
                &mut output,
                session,
                format!("You do not know the spell '{spell_name}'."),
            );
            continue;
        }

        if mana < spell.mana as i32 {
            deny(
                &mut output,
                session,
                "You haven't the energy to cast that spell.",
            );
            continue;
        }

        let room = players
            .get(session)
            .map(|(_, _, location)| location.room)
            .ok();

        // Resolve the target list. A hostile spell needs a real victim; a
        // non-violent character spell with no argument defaults to the caster,
        // as tbaMUD's `do_cast` does.
        let targets: Vec<Entity> = match spell.target {
            SpellTarget::Caster => vec![session],
            SpellTarget::None => Vec::new(),
            SpellTarget::Room => {
                let mut in_room: Vec<Entity> = players
                    .iter()
                    .filter(|(entity, _, location)| {
                        *entity != session && Some(location.room) == room
                    })
                    .map(|(entity, _, _)| entity)
                    .collect();
                in_room.extend(
                    npcs.iter()
                        .filter(|(_, _, location)| Some(location.room) == room)
                        .map(|(entity, _, _)| entity),
                );
                if !is_hostile(spell) {
                    in_room.push(session);
                }
                in_room
            }
            SpellTarget::Victim => match target_arg.as_deref() {
                Some(name) => {
                    if let Some(target) = find_target_in_room(session, name, &players, &npcs) {
                        vec![target]
                    } else {
                        let own_name = players
                            .get(session)
                            .map(|(_, player, _)| player.name.to_lowercase())
                            .unwrap_or_default();
                        let needle = name.to_lowercase();
                        if !needle.is_empty() && own_name.contains(&needle) {
                            deny(&mut output, session, "You shouldn't cast that on yourself.");
                        } else {
                            deny(&mut output, session, format!("You don't see {name} here."));
                        }
                        continue;
                    }
                }
                None if is_hostile(spell) => {
                    deny(&mut output, session, "Upon whom should the spell be cast?");
                    continue;
                }
                None => vec![session],
            },
        };

        // Every check passed: spend the mana once and announce the casting.
        if spell.mana > 0
            && let Ok(mut caster) = combatants.get_mut(session)
        {
            caster.mana = (caster.mana - spell.mana as i32).max(0);
        }
        let actor_name = entity_name(session, &players, &npcs);
        output.write(text(session, format!("You cast {spell_name}.")));
        if let Some(room) = room {
            broadcast(
                &players,
                room,
                session,
                &mut output,
                &format!("{actor_name} casts {spell_name}."),
            );
        }

        for target in &targets {
            let target = *target;
            let target_name = entity_name(target, &players, &npcs);
            let mut lethal = false;

            match spell.kind {
                SpellKind::Damage => {
                    let Some(dice) = spell.dice else {
                        continue;
                    };
                    let damage = rng.roll(&dice).max(0);
                    if let Ok(mut victim) = combatants.get_mut(target) {
                        if !victim.is_alive() {
                            continue;
                        }
                        victim.hp = (victim.hp - damage).max(0);
                        if victim.hp == 0 {
                            victim.position = Position::Dead;
                            victim.target = None;
                            lethal = true;
                        }
                    }
                    output.write(text(
                        session,
                        format!("You hit {target_name} for {damage} damage."),
                    ));
                    if let Some(room) = room {
                        broadcast(
                            &players,
                            room,
                            session,
                            &mut output,
                            &format!("{actor_name} hits {target_name} for {damage} damage."),
                        );
                    }
                }
                SpellKind::Heal => {
                    let Some(dice) = spell.dice else {
                        continue;
                    };
                    let amount = rng.roll(&dice).max(0);
                    if let Ok(mut combatant) = combatants.get_mut(target) {
                        if !combatant.is_alive() {
                            continue;
                        }
                        combatant.hp = (combatant.hp + amount).min(combatant.max_hp);
                    }
                    if target == session {
                        output.write(text(session, format!("You heal yourself for {amount}.")));
                    } else {
                        output.write(text(
                            session,
                            format!("You heal {target_name} for {amount}."),
                        ));
                        if players.get(target).is_ok() {
                            output.write(text(
                                target,
                                format!("{actor_name} heals you for {amount}."),
                            ));
                        }
                    }
                }
                SpellKind::Buff => {
                    let Some(affect) = &spell.affect else {
                        continue;
                    };
                    add_buff(target, affect.clone(), &mut buffs, &mut commands);
                    if target == session {
                        output.write(text(session, format!("You feel the {spell_name} settle.")));
                    } else if players.get(target).is_ok() {
                        output.write(text(
                            target,
                            format!("{actor_name} blesses you with {spell_name}."),
                        ));
                    }
                }
                SpellKind::Detect => {
                    output.write(text(
                        session,
                        format!("Your senses sharpen with {spell_name}."),
                    ));
                }
                SpellKind::Utility => {
                    output.write(text(
                        session,
                        format!("You cast {spell_name}, but nothing happens yet."),
                    ));
                }
            }

            // A damage spell may also carry an affect (`chill touch`). Apply it
            // through the same transient buff store. `Buff` already ran above.
            if let Some(affect) = &spell.affect
                && spell.kind != SpellKind::Buff
                && !lethal
            {
                add_buff(target, affect.clone(), &mut buffs, &mut commands);
            }

            if lethal {
                if players.get(target).is_ok() {
                    defeated.write(PlayerDefeated { player: target });
                } else if npcs.get(target).is_ok() {
                    commands.entity(target).despawn();
                }
            }
        }

        write_prompt(&mut output, session);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parses_quoted_and_unquoted_cast_lines() {
        let quoted = parse_cast_line("cast 'magic missile' goblin").expect("quoted");
        assert_eq!(quoted.spell, "magic missile");
        assert_eq!(quoted.target.as_deref(), Some("goblin"));
        assert!(quoted.quoted);

        let bare = parse_cast_line("cast fireball").expect("bare");
        assert_eq!(bare.spell, "fireball");
        assert_eq!(bare.target, None);
        assert!(!bare.quoted);

        let unquoted = parse_cast_line("cast magic missile goblin").expect("unquoted");
        assert_eq!(unquoted.spell, "magic missile goblin");
        assert_eq!(unquoted.target, None);
        assert!(!unquoted.quoted);

        assert!(parse_cast_line("attack goblin").is_none());
        assert!(parse_cast_line("castx").is_none());
    }

    #[test]
    fn normalizes_ids_and_names() {
        assert_eq!(normalize("Magic_Missile"), "magic missile");
        assert_eq!(normalize("  magic   missile  "), "magic missile");
        assert_eq!(normalize("detect_invis"), "detect invis");
    }
}
