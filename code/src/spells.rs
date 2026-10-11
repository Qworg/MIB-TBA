//! TBA-E18 — the `cast` command, spell mana, and round-counted buffs.
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
//! [`TbaBuffs`] is a list of [`ActiveBuff`]s, each an [`AffectDef`] and a
//! countdown. The countdown's initial value comes from the spell's
//! [`SpellDef::duration`] (Circle's `af[].duration` in `mag_affects`).
//! [`tick_buffs`] runs on the existing [`TbaRound`](crate::combat::TbaRound)
//! schedule (one tick per 3-second combat round) and expires them; the affect
//! recompute in
//! [`recompute_equipment_affects`](crate::equipment::recompute_equipment_affects)
//! includes the live buff affects in the same no-stacking base + worn + buffs
//! rebuild, so a buff is never applied twice and expires cleanly.
//!
//! [`TbaBuffs`] is a [`Component`] and registered with the stdlib
//! [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry) under
//! [`DocKind::Player`](mib_persist::prelude::DocKind::Player), so active buffs
//! and their remaining rounds survive a restart.

use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{Location, Npc, NpcKey, Player, PlayerDefeated, Session, SessionState};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::combat::commands::{Stealthed, broadcast, deny, entity_name, find_target_in_room, text};
use crate::combat::rewards::{NpcDefeated, alignment_for, reward_for};
use crate::combat::{
    SaveType, TbaCombatant, TbaHidden, TbaProgress, TbaRng, default_class, mag_savingthrow,
};
use crate::content::{AffectDef, Position, SaveEffect, SpellDef, SpellKind, SpellTarget};
use crate::module::TbaContent;

/// Combat rounds a cast buff lasts when the spell carries no source pulse
/// count.
///
/// Circle's `mag_affects` sets an explicit `af[].duration` per spell, authored
/// onto [`SpellDef::duration`]; this is the documented fallback for a spell
/// without one. It is the same value as
/// [`DEFAULT_SPELL_DURATION`](crate::content::DEFAULT_SPELL_DURATION).
pub const DEFAULT_BUFF_ROUNDS: u32 = crate::content::DEFAULT_SPELL_DURATION;

/// One live buff: the affect it contributes and the rounds left before it
/// expires.
#[derive(Debug, Clone, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct ActiveBuff {
    pub affect: AffectDef,
    pub rounds_left: u32,
}

/// A combatant's active buff list, persisted with the player.
///
/// Each entry's `rounds_left` starts at the casting spell's
/// [`SpellDef::duration`] and is decremented by [`tick_buffs`] once per
/// [`TbaRound`](crate::combat::TbaRound). Registered with the stdlib
/// [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry) under
/// [`DocKind::Player`](mib_persist::prelude::DocKind::Player), so a restart
/// restores both the affects and their remaining rounds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
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

/// Whether a spell is aimed aggressively: a hostile spell needs an explicit
/// victim and cannot be aimed at the caster.
///
/// Circle decides this from the `violent` column plus `TAR_NOT_SELF`; the
/// engine folds both into the `Damage`/`Debuff` kinds (the spells whose Circle
/// routine only ever lands on someone else).
fn is_hostile(spell: &SpellDef) -> bool {
    matches!(spell.kind, SpellKind::Damage | SpellKind::Debuff)
}

/// Circle's `mag_manacost` (`tbamud/src/spell_parser.c:58-62`):
///
/// ```text
/// MAX(mana_max - mana_change * (GET_LEVEL(ch) - min_level[class]), mana_min)
/// ```
///
/// `min_level` is the caster class's `spell_info[].min_level[]` value from
/// `init_spell_levels()` (`tbamud/src/class.c:1589-1665`); a class absent from
/// [`SpellDef::min_level`] pays the whole `mana_max` (treated as level 1).
///
/// The source only calls this once `do_cast` has confirmed
/// `GET_LEVEL(ch) >= min_level[class]`, so the subtraction never goes negative
/// there. This engine's class gate is a flat `granted_skills` list, so a
/// below-min-level caster is possible; the result is clamped to the
/// `[mana_min, mana_max]` band so such a caster pays `mana_max`, not more.
/// Either way the result is never below `mana_min` (the source's own floor).
pub fn mana_cost(spell: &SpellDef, class: &str, level: u32) -> u32 {
    let min_level = spell.min_level.get(class).copied().unwrap_or(1).max(1);
    let level = i64::from(level.max(1));
    let raw =
        i64::from(spell.mana_max) - i64::from(spell.mana_change) * (level - i64::from(min_level));
    raw.max(i64::from(spell.mana_min))
        .min(i64::from(spell.mana_max)) as u32
}

/// The `(class, level)` a saving throw is rolled against.
///
/// A player uses its [`TbaProgress`]; an NPC uses `warrior`, exactly as
/// `mag_savingthrow`'s `IS_NPC(ch)` branch does (`tbamud/src/magic.c:38-41`),
/// with the level from its `tba.npc_stats` record. An entity with neither falls
/// back to a level-1 warrior.
fn victim_class_level(
    target: Entity,
    content: &TbaContent,
    progress: &Query<&TbaProgress>,
    keys: &Query<&NpcKey>,
) -> (SmolStr, u32) {
    if let Ok(progress) = progress.get(target) {
        return (progress.class.clone(), progress.level.max(1));
    }
    if let Ok(key) = keys.get(target)
        && let Some(stats) = content.npc_stats.get(&key.0)
    {
        return (SmolStr::new("warrior"), stats.level.max(1));
    }
    (SmolStr::new("warrior"), 1)
}

/// The pulse count an affect is installed for: the spell's static
/// [`SpellDef::duration`] plus its per-level [`SpellDef::duration_bonus`], at
/// least one round so a level-1 caster never installs an inert affect.
fn affect_rounds(spell: &SpellDef, level: u32) -> u32 {
    let rounds = i64::from(spell.duration) + i64::from(spell.duration_bonus.bonus(level));
    rounds.max(1) as u32
}

/// Install (or refresh) a buff affect on `target`, lasting `rounds`.
///
/// A re-cast of the same apply location replaces the old entry rather than
/// stacking it, which is what keeps the recompute's additive affects bounded.
fn add_buff(
    target: Entity,
    affect: AffectDef,
    rounds: u32,
    buffs: &mut Query<&mut TbaBuffs>,
    commands: &mut Commands,
) {
    let entry = ActiveBuff {
        affect,
        rounds_left: rounds,
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
    hidden: Stealthed<'_, '_>,
    content: Option<Res<TbaContent>>,
    mut combatants: Query<&mut TbaCombatant>,
    progress: Query<&TbaProgress>,
    mut buffs: Query<&mut TbaBuffs>,
    mut rng: ResMut<TbaRng>,
    keys: Query<&NpcKey>,
    mut defeated: MessageWriter<PlayerDefeated>,
    mut npc_defeated: MessageWriter<NpcDefeated>,
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

        let Ok(caster_progress) = progress.get(session) else {
            deny(&mut output, session, "You have no spellcasting knowledge.");
            continue;
        };
        let caster_class = caster_progress.class.clone();
        let caster_level = caster_progress.level.max(1);
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
            .get(&caster_class)
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

        // Level-scaled cost (`mag_manacost`, spell_parser.c:58-62).
        let cost = mana_cost(spell, &caster_class, caster_level);
        if mana < cost as i32 {
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
                    if let Some(target) =
                        find_target_in_room(session, name, &players, &npcs, &hidden)
                    {
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
        if cost > 0
            && let Ok(mut caster) = combatants.get_mut(session)
        {
            caster.mana = (caster.mana - cost as i32).max(0);
        }
        // A hostile cast is an attack: it breaks stealth, as Circle's
        // `damage()` -> `appear()` does (`tbamud/src/fight.c:70-84`).
        if is_hostile(spell) {
            commands.entity(session).remove::<TbaHidden>();
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

            // Circle rolls the saving throw, then halves or negates. A `cast`
            // always resolves `SAVING_SPELL` (`call_magic`,
            // spell_parser.c:205-222) against the victim's class/level base
            // plus the `TbaSaves` affect modifiers.
            let saved = if spell.save == SaveEffect::None {
                false
            } else {
                let (victim_class, victim_level) =
                    victim_class_level(target, content, &progress, &keys);
                let victim_saves = combatants
                    .get(target)
                    .map(|combatant| combatant.saves)
                    .unwrap_or_default();
                mag_savingthrow(
                    &mut rng,
                    &victim_class,
                    victim_level,
                    &victim_saves,
                    SaveType::Spell,
                )
            };
            if saved && spell.save == SaveEffect::Negate {
                if players.get(target).is_ok() {
                    output.write(text(target, format!("You resist the {spell_name}.")));
                }
                if target != session {
                    output.write(text(
                        session,
                        format!("{target_name} resists your {spell_name}."),
                    ));
                }
                if let Some(room) = room {
                    broadcast(
                        &players,
                        room,
                        session,
                        &mut output,
                        &format!("{target_name} resists {actor_name}'s {spell_name}."),
                    );
                }
                continue;
            }

            match spell.kind {
                SpellKind::Damage => {
                    let Some(dice) = spell.dice else {
                        continue;
                    };
                    let rolled = rng.roll(&dice).max(0) + spell.level_bonus.bonus(caster_level);
                    // A successful save halves the damage (`dam /= 2`,
                    // magic.c:291).
                    let damage = if saved && spell.save == SaveEffect::Half {
                        rolled / 2
                    } else {
                        rolled
                    };
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
                    if saved && spell.save == SaveEffect::Half {
                        if players.get(target).is_ok() {
                            output.write(text(target, format!("You resist the {spell_name}.")));
                        }
                        output.write(text(
                            session,
                            format!("{target_name} partially resists your {spell_name}."),
                        ));
                        if let Some(room) = room {
                            broadcast(
                                &players,
                                room,
                                session,
                                &mut output,
                                &format!(
                                    "{target_name} partially resists {actor_name}'s {spell_name}."
                                ),
                            );
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
                    let amount =
                        (rng.roll(&dice).max(0) + spell.level_bonus.bonus(caster_level)).max(0);
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
                SpellKind::Buff | SpellKind::Debuff => {
                    let Some(affect) = &spell.affect else {
                        continue;
                    };
                    let rounds = affect_rounds(spell, caster_level);
                    add_buff(target, affect.clone(), rounds, &mut buffs, &mut commands);
                    if spell.kind == SpellKind::Debuff {
                        if target != session {
                            output.write(text(
                                session,
                                format!("{target_name} is afflicted by {spell_name}."),
                            ));
                        }
                    } else if target == session {
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
                    // Issue #7: an unmodelled Circle routine is explicit, never
                    // a silent no-op. The mana was already spent above.
                    let reason = spell
                        .unsupported
                        .as_deref()
                        .unwrap_or("the Circle routine is not modelled");
                    output.write(text(
                        session,
                        format!("You cast {spell_name}, but {reason}."),
                    ));
                }
            }

            // A damage spell may also carry an affect (`chill touch`); a save
            // shortens the source's duration (`af[0].duration = 1` on a save,
            // magic.c:322-331). `Buff`/`Debuff` already ran above.
            if let Some(affect) = &spell.affect
                && matches!(spell.kind, SpellKind::Damage)
                && !lethal
            {
                let rounds = if saved && spell.save == SaveEffect::Half {
                    affect_rounds(spell, caster_level).min(1)
                } else {
                    affect_rounds(spell, caster_level)
                };
                add_buff(target, affect.clone(), rounds, &mut buffs, &mut commands);
            }

            if lethal {
                if players.get(target).is_ok() {
                    defeated.write(PlayerDefeated { player: target });
                } else if npcs.get(target).is_ok() {
                    let (experience, gold) = keys
                        .get(target)
                        .ok()
                        .map(|key| reward_for(content, key))
                        .unwrap_or((0, 0));
                    let alignment = keys
                        .get(target)
                        .ok()
                        .map(|key| alignment_for(content, key))
                        .unwrap_or(0);
                    npc_defeated.write(NpcDefeated {
                        victim: target,
                        killer: session,
                        experience,
                        gold,
                        alignment,
                    });
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
