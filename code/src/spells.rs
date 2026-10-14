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
use mib_stdlib::prelude::{
    Location, Npc, NpcId, NpcKey, Player, PlayerDefeated, Session, SessionState,
};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::combat::commands::{
    StealthView, broadcast, deny, entity_name, find_target_in_room, text,
};
use crate::combat::rewards::{NpcDefeated, TbaDamage, alignment_for, reward_for};
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

/// Bit of [`ActiveBuff::rounds_left`] that marks a pinned buff: one a script
/// owns (a `dg_affect`, or a permanent stat adjustment). A cast never replaces
/// a pinned buff, and the rounds it counts down are the low bits.
pub const PINNED_BUFF: u32 = 1 << 31;
/// Bit of [`ActiveBuff::rounds_left`] that marks a pinned buff as a permanent
/// stat adjustment (a character's base value moved), not an affect.
pub const ADJUSTMENT_BUFF: u32 = 1 << 30;
/// The rounds of a pinned buff that never runs out.
pub const PINNED_FOREVER: u32 = ADJUSTMENT_BUFF - 1;

impl ActiveBuff {
    /// A pinned buff of `rounds` rounds (clamped to [`PINNED_FOREVER`]);
    /// `adjustment` marks a permanent stat adjustment.
    pub fn pinned(affect: AffectDef, rounds: u32, adjustment: bool) -> Self {
        let flags = PINNED_BUFF | if adjustment { ADJUSTMENT_BUFF } else { 0 };
        Self {
            affect,
            rounds_left: flags | rounds.clamp(1, PINNED_FOREVER),
        }
    }

    /// Whether a script owns this buff.
    pub fn is_pinned(&self) -> bool {
        self.rounds_left & PINNED_BUFF != 0
    }

    /// Whether this is a pinned permanent stat adjustment.
    pub fn is_adjustment(&self) -> bool {
        self.is_pinned() && self.rounds_left & ADJUSTMENT_BUFF != 0
    }

    /// The rounds left, without the pinned flags.
    pub fn rounds(&self) -> u32 {
        if self.is_pinned() {
            self.rounds_left & PINNED_FOREVER
        } else {
            self.rounds_left
        }
    }
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
            if buff.is_pinned() {
                // A pinned buff that lasts for ever keeps its rounds.
                let rounds = buff.rounds();
                if rounds >= PINNED_FOREVER {
                    return true;
                }
                buff.rounds_left = (buff.rounds_left & !PINNED_FOREVER) | rounds.saturating_sub(1);
                return rounds > 1;
            }
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

/// Whether a `detect` spell is true sight: it reveals concealed occupants
/// (Circle's `AFF_DETECT_INVIS`/`AFF_SENSE_LIFE`) rather than a narrower
/// divination such as `detect magic`/`detect alignment`.
fn grants_true_sight(spell_id: &str) -> bool {
    matches!(spell_id, "detect_invis" | "sense_life")
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
            store
                .buffs
                .retain(|buff| buff.is_pinned() || buff.affect.location != location);
            store.buffs.push(entry);
        }
        Err(_) => {
            commands
                .entity(target)
                .insert(TbaBuffs { buffs: vec![entry] });
        }
    }
}

/// A `cast` line resolved up to the point where mana is spent: the spell, the
/// caster and the entities the spell will affect.
pub(crate) struct CastPlan<'a> {
    pub spell_id: SmolStr,
    pub spell: &'a SpellDef,
    pub caster_level: u32,
    /// The mana the cast costs.
    pub cost: u32,
    pub room: Option<Entity>,
    /// The targets, in the order the effects apply.
    pub targets: Vec<Entity>,
}

/// What [`plan_cast`] made of a line.
pub(crate) enum CastOutcome<'a> {
    /// The line is not a `cast` command.
    NotCast,
    /// The cast is refused before anything happens, with the message to show.
    Denied(String),
    Plan(CastPlan<'a>),
}

/// Resolve a `cast` line the way `do_cast` does before `cast_spell`: the spell,
/// whether the caster may cast it now, its cost and its targets. Nothing is
/// spent or changed. [`handle_cast_command`] applies the plan; the DG `cast`
/// trigger gate resolves the same plan to learn the spell and target the
/// handler will use.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_cast<'a>(
    session: Entity,
    line: &str,
    content: &'a TbaContent,
    progress: &Query<&TbaProgress>,
    combatants: &Query<&mut TbaCombatant>,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
    hidden: &StealthView<'_, '_>,
) -> CastOutcome<'a> {
    let Some(parsed) = parse_cast_line(line) else {
        return CastOutcome::NotCast;
    };

    // Resolve the spell id. Quoted form resolves the phrase directly; the
    // unquoted form greedily peels the longest known spell off the front.
    let resolved = if parsed.quoted {
        resolve_spell(content, &parsed.spell).map(|(id, def)| (id.clone(), def, parsed.target))
    } else {
        split_unquoted(content, &parsed.spell).map(|(id, def, target)| (id.clone(), def, target))
    };
    let Some((spell_id, spell, target_arg)) = resolved else {
        return CastOutcome::Denied("Cast what?!".to_owned());
    };
    let spell_name = spell.name.clone();

    let Ok(caster_progress) = progress.get(session) else {
        return CastOutcome::Denied("You have no spellcasting knowledge.".to_owned());
    };
    let caster_class = caster_progress.class.clone();
    let caster_level = caster_progress.level.max(1);
    let Ok(caster) = combatants.get(session) else {
        return CastOutcome::Denied("You have no combat state.".to_owned());
    };
    if !caster.is_alive() {
        return CastOutcome::Denied("You can't cast spells while dead.".to_owned());
    }
    if caster.position < spell.min_position {
        return CastOutcome::Denied("You can't concentrate enough to cast that.".to_owned());
    }
    let mana = caster.mana;

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
        return CastOutcome::Denied(format!("You do not know the spell '{spell_name}'."));
    }

    // Level-scaled cost (`mag_manacost`, spell_parser.c:58-62).
    let cost = mana_cost(spell, &caster_class, caster_level);
    if mana < cost as i32 {
        return CastOutcome::Denied("You haven't the energy to cast that spell.".to_owned());
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
                .filter(|(entity, _, location)| *entity != session && Some(location.room) == room)
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
                if let Some(target) = find_target_in_room(session, name, players, npcs, hidden) {
                    vec![target]
                } else {
                    let own_name = players
                        .get(session)
                        .map(|(_, player, _)| player.name.to_lowercase())
                        .unwrap_or_default();
                    let needle = name.to_lowercase();
                    return CastOutcome::Denied(
                        if !needle.is_empty() && own_name.contains(&needle) {
                            "You shouldn't cast that on yourself.".to_owned()
                        } else {
                            format!("You don't see {name} here.")
                        },
                    );
                }
            }
            None if is_hostile(spell) => {
                return CastOutcome::Denied("Upon whom should the spell be cast?".to_owned());
            }
            None => vec![session],
        },
    };

    CastOutcome::Plan(CastPlan {
        spell_id,
        spell,
        caster_level,
        cost,
        room,
        targets,
    })
}

/// `BridgeSet::Game`: translate `cast '<spell>' [target]` into a spell effect.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_cast_command(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    players: Query<(Entity, &Player, &Location)>,
    npcs: Query<(Entity, &Npc, &Location)>,
    hidden: StealthView<'_, '_>,
    content: Option<Res<TbaContent>>,
    mut combatants: Query<&mut TbaCombatant>,
    progress: Query<&TbaProgress>,
    mut buffs: Query<&mut TbaBuffs>,
    mut rng: ResMut<TbaRng>,
    keys: Query<&NpcKey>,
    (mut defeated, mut npc_defeated, mut damage_events): (
        MessageWriter<PlayerDefeated>,
        MessageWriter<NpcDefeated>,
        MessageWriter<TbaDamage>,
    ),
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
    refused: Option<Res<crate::dg::intake::combat::RefusedCasts>>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    for msg in reader.read() {
        // A `cast` trigger that refused the spell has already taken the line.
        if refused
            .as_ref()
            .is_some_and(|refused| refused.contains(msg.id))
        {
            continue;
        }
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
        let plan = match plan_cast(
            session,
            line,
            content,
            &progress,
            &combatants,
            &players,
            &npcs,
            &hidden,
        ) {
            CastOutcome::NotCast => continue,
            CastOutcome::Denied(message) => {
                deny(&mut output, session, message);
                continue;
            }
            CastOutcome::Plan(plan) => plan,
        };
        let CastPlan {
            spell_id,
            spell,
            caster_level,
            cost,
            room,
            targets,
        } = plan;
        let spell_name = spell.name.clone();

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

        let cast = Cast {
            content,
            caster: session,
            caster_level,
            spell_id: &spell_id,
            spell,
            room,
            actor_name: &actor_name,
            caster_hears: true,
            defer_despawn: false,
        };
        for target in &targets {
            apply_spell_to_target(
                &cast,
                &players,
                &npcs,
                &progress,
                &keys,
                &mut combatants,
                &mut buffs,
                &mut rng,
                &mut commands,
                &mut output,
                &mut defeated,
                &mut npc_defeated,
                &mut damage_events,
                *target,
            );
        }

        write_prompt(&mut output, session);
    }
}

/// The cast being resolved: what is cast, by whom and from where.
struct Cast<'a> {
    content: &'a TbaContent,
    caster: Entity,
    caster_level: u32,
    spell_id: &'a SmolStr,
    spell: &'a SpellDef,
    room: Option<Entity>,
    actor_name: &'a str,
    /// Whether the caster is told what happens: false for a script's cast by
    /// a mob or the room and object proxies, which have no connection.
    caster_hears: bool,
    /// Whether a victim that dies is left in the world for the caller to
    /// extract at the end of the pulse (a script's cast) rather than
    /// despawned here.
    defer_despawn: bool,
}

/// Tell the caster a line, unless it has no connection to hear it.
fn tell_caster(
    output: &mut MessageWriter<OutputEvent>,
    hears: bool,
    session: Entity,
    line: String,
) {
    if hears {
        output.write(text(session, line));
    }
}

/// Resolve the cast's effect on one target: the saving throw, the damage,
/// heal, buff or message the spell's kind calls for, the affect a damage
/// spell carries, and the death it may cause.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn apply_spell_to_target(
    cast: &Cast<'_>,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
    progress: &Query<&TbaProgress>,
    keys: &Query<&NpcKey>,
    combatants: &mut Query<&mut TbaCombatant>,
    buffs: &mut Query<&mut TbaBuffs>,
    rng: &mut TbaRng,
    commands: &mut Commands,
    output: &mut MessageWriter<OutputEvent>,
    defeated: &mut MessageWriter<PlayerDefeated>,
    npc_defeated: &mut MessageWriter<NpcDefeated>,
    damage_events: &mut MessageWriter<TbaDamage>,
    target: Entity,
) {
    let Cast {
        content,
        caster: session,
        caster_level,
        spell_id,
        spell,
        room,
        actor_name,
        caster_hears,
        defer_despawn,
    } = *cast;
    let spell_name = spell.name.clone();
    let target_name = entity_name(target, players, npcs);
    let mut lethal = false;

    // Circle rolls the saving throw, then halves or negates. A `cast`
    // always resolves `SAVING_SPELL` (`call_magic`,
    // spell_parser.c:205-222) against the victim's class/level base
    // plus the `TbaSaves` affect modifiers.
    let saved = if spell.save == SaveEffect::None {
        false
    } else {
        let (victim_class, victim_level) = victim_class_level(target, content, progress, keys);
        let victim_saves = combatants
            .get(target)
            .map(|combatant| combatant.saves)
            .unwrap_or_default();
        mag_savingthrow(
            rng,
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
            tell_caster(
                output,
                caster_hears,
                session,
                format!("{target_name} resists your {spell_name}."),
            );
        }
        if let Some(room) = room {
            broadcast(
                players,
                room,
                session,
                output,
                &format!("{target_name} resists {actor_name}'s {spell_name}."),
            );
        }
        return;
    }

    match spell.kind {
        SpellKind::Damage => {
            let Some(dice) = spell.dice else {
                return;
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
                    return;
                }
                victim.hp = (victim.hp - damage).max(0);
                damage_events.write(TbaDamage {
                    attacker: session,
                    victim: target,
                    damage,
                    lethal: victim.hp == 0,
                });
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
                tell_caster(
                    output,
                    caster_hears,
                    session,
                    format!("{target_name} partially resists your {spell_name}."),
                );
                if let Some(room) = room {
                    broadcast(
                        players,
                        room,
                        session,
                        output,
                        &format!("{target_name} partially resists {actor_name}'s {spell_name}."),
                    );
                }
            }
            tell_caster(
                output,
                caster_hears,
                session,
                format!("You hit {target_name} for {damage} damage."),
            );
            if let Some(room) = room {
                broadcast(
                    players,
                    room,
                    session,
                    output,
                    &format!("{actor_name} hits {target_name} for {damage} damage."),
                );
            }
        }
        SpellKind::Heal => {
            let Some(dice) = spell.dice else {
                return;
            };
            let amount = (rng.roll(&dice).max(0) + spell.level_bonus.bonus(caster_level)).max(0);
            if let Ok(mut combatant) = combatants.get_mut(target) {
                if !combatant.is_alive() {
                    return;
                }
                combatant.hp = (combatant.hp + amount).min(combatant.max_hp);
            }
            if target == session {
                tell_caster(
                    output,
                    caster_hears,
                    session,
                    format!("You heal yourself for {amount}."),
                );
            } else {
                tell_caster(
                    output,
                    caster_hears,
                    session,
                    format!("You heal {target_name} for {amount}."),
                );
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
                return;
            };
            let rounds = affect_rounds(spell, caster_level);
            add_buff(target, affect.clone(), rounds, buffs, commands);
            if spell.kind == SpellKind::Debuff {
                if target != session {
                    tell_caster(
                        output,
                        caster_hears,
                        session,
                        format!("{target_name} is afflicted by {spell_name}."),
                    );
                }
                if players.get(target).is_ok() {
                    output.write(text(target, format!("You are afflicted by {spell_name}.")));
                }
            } else if target == session {
                tell_caster(
                    output,
                    caster_hears,
                    session,
                    format!("You feel the {spell_name} settle."),
                );
            } else if players.get(target).is_ok() {
                output.write(text(
                    target,
                    format!("{actor_name} blesses you with {spell_name}."),
                ));
            }
        }
        SpellKind::Detect => {
            tell_caster(
                output,
                caster_hears,
                session,
                format!("Your senses sharpen with {spell_name}."),
            );
            // True sight (`detect invisibility`, `sense life`) grants the
            // engine `Perception` carrier for the spell's duration so a
            // concealed occupant is perceived through the projection and
            // by the pack's own targeting. The grant rides the pack's
            // round-ticked `TbaTrueSight`, mirrored into `Perception` by
            // `mib_tba::stealth::sync_perception`.
            if grants_true_sight(spell_id) {
                crate::stealth::grant_true_sight(
                    target,
                    affect_rounds(spell, caster_level),
                    commands,
                );
            }
        }
        SpellKind::Utility => {
            // Issue #7: an unmodelled Circle routine is explicit, never
            // a silent no-op. The mana was already spent above.
            let reason = spell
                .unsupported
                .as_deref()
                .unwrap_or("the Circle routine is not modelled");
            tell_caster(
                output,
                caster_hears,
                session,
                format!("You cast {spell_name}, but {reason}."),
            );
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
        add_buff(target, affect.clone(), rounds, buffs, commands);
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
                npc: keys
                    .get(target)
                    .ok()
                    .map(|key| key.0.clone())
                    .unwrap_or_else(|| NpcId::from("")),
                experience,
                gold,
                alignment,
            });
            if !defer_despawn {
                commands.queue(move |world: &mut World| {
                    crate::dg::intake::combat::finish_kill(world, target, session);
                });
            }
        }
    }
}

/// Cast the catalog spell `spell_id` from `caster` at `level`, on behalf of a
/// script (`call_magic`'s effect half): the caster's class, position and mana
/// are not consulted, and no mana is spent. A `Victim` or `Caster` spell lands
/// on `victim` (a `Caster` spell on the caster when none is named); a `Room`
/// spell lands on everyone in the caster's room, the caster too unless the
/// spell is hostile; a `None` spell lands on no one. Returns whether the spell
/// exists. A caster with no connection (a mob, or the room and object proxies)
/// is told nothing, and an NPC the spell kills is left in the world (position
/// dead, `NpcDefeated` written) for the caller to extract at the end of its
/// pulse; `handle_cast_command` despawns it at once.
pub fn cast_spell_at_level(
    world: &mut World,
    caster: Entity,
    victim: Option<Entity>,
    spell_id: &str,
    level: u32,
) -> bool {
    use bevy::ecs::system::SystemState;

    type Players<'w, 's> = Query<'w, 's, (Entity, &'static Player, &'static Location)>;
    type Npcs<'w, 's> = Query<'w, 's, (Entity, &'static Npc, &'static Location)>;
    type Params<'w, 's> = (
        Players<'w, 's>,
        Npcs<'w, 's>,
        Query<'w, 's, &'static TbaProgress>,
        Query<'w, 's, &'static NpcKey>,
        Query<'w, 's, &'static mut TbaCombatant>,
        Query<'w, 's, &'static mut TbaBuffs>,
        Query<'w, 's, &'static Location>,
        Query<'w, 's, &'static Session>,
        Option<Res<'w, TbaContent>>,
        ResMut<'w, TbaRng>,
        Commands<'w, 's>,
        MessageWriter<'w, OutputEvent>,
        MessageWriter<'w, PlayerDefeated>,
        MessageWriter<'w, NpcDefeated>,
        MessageWriter<'w, TbaDamage>,
    );

    world.init_resource::<TbaRng>();
    world.init_resource::<Messages<OutputEvent>>();
    world.init_resource::<Messages<PlayerDefeated>>();
    world.init_resource::<Messages<NpcDefeated>>();
    world.init_resource::<Messages<TbaDamage>>();
    let mut state: SystemState<Params<'_, '_>> = SystemState::new(world);
    let found = {
        let Ok((
            players,
            npcs,
            progress,
            keys,
            mut combatants,
            mut buffs,
            locations,
            sessions,
            content,
            mut rng,
            mut commands,
            mut output,
            mut defeated,
            mut npc_defeated,
            mut damage_events,
        )) = state.get_mut(world)
        else {
            return false;
        };
        let empty = TbaContent::default();
        let content = content.as_deref().unwrap_or(&empty);
        let wanted = normalize(spell_id);
        let Some((spell_id, spell)) = content
            .spells
            .iter()
            .find(|(id, _)| normalize(id) == wanted)
        else {
            return false;
        };
        let room = locations.get(caster).ok().map(|location| location.room);
        let hostile = is_hostile(spell);
        let targets: Vec<Entity> = match spell.target {
            SpellTarget::Caster => vec![victim.unwrap_or(caster)],
            SpellTarget::Victim => victim.into_iter().collect(),
            SpellTarget::None => Vec::new(),
            SpellTarget::Room => {
                let mut in_room: Vec<Entity> = players
                    .iter()
                    .map(|(entity, _, location)| (entity, location.room))
                    .chain(
                        npcs.iter()
                            .map(|(entity, _, location)| (entity, location.room)),
                    )
                    .filter(|(entity, here)| Some(*here) == room && (*entity != caster || !hostile))
                    .map(|(entity, _)| entity)
                    .collect();
                in_room.sort_unstable();
                in_room
            }
        };
        let actor_name = entity_name(caster, &players, &npcs);
        let cast = Cast {
            content,
            caster,
            caster_level: level.max(1),
            spell_id,
            spell,
            room,
            actor_name: &actor_name,
            caster_hears: sessions.get(caster).is_ok(),
            defer_despawn: true,
        };
        for target in targets {
            // A hostile cast is an attack: it breaks stealth, as Circle's
            // `damage()` -> `appear()` does (`tbamud/src/fight.c:70-84`).
            if hostile {
                commands.entity(caster).remove::<TbaHidden>();
            }
            apply_spell_to_target(
                &cast,
                &players,
                &npcs,
                &progress,
                &keys,
                &mut combatants,
                &mut buffs,
                &mut rng,
                &mut commands,
                &mut output,
                &mut defeated,
                &mut npc_defeated,
                &mut damage_events,
                target,
            );
        }
        true
    };
    state.apply(world);
    found
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
