//! GitHub issue #25 — CircleMUD/TBA class abilities and stealth.
//!
//! TBA's four classes learn **skills** as well as spells
//! (`tba.classes.granted_skills`), and CircleMUD implements each as an
//! `ACMD` routine (`tbamud/src/act.offensive.c`, `src/act.other.c`,
//! `src/act.informative.c`). MIB's fixed command grammar has no variant for
//! them, so [`handle_skill_commands`] parses [`PlayerInput::RawLine`] itself in
//! [`BridgeSet::Game`](mib_core::prelude::BridgeSet::Game) after the stdlib
//! router, exactly as `spells.rs`/`socials.rs` do. The verbs are declared
//! (effect-free) in `content/stdlib.verbs.yaml` so the router claims them.
//!
//! ## Commands and their Circle source
//!
//! | Verb | Circle | Class gate (min level) |
//! |---|---|---|
//! | `backstab <target>` | `do_backstab`, `act.offensive.c:127` | thief 3 |
//! | `kick <target>` | `do_kick`, `act.offensive.c:493` | warrior 1 |
//! | `bash <target>` | `do_bash`, `act.offensive.c:270` | warrior 12 |
//! | `rescue <ally>` | `do_rescue`, `act.offensive.c:332` | warrior 3 |
//! | `assist [ally]` | `do_assist`, `act.offensive.c:24` | universal |
//! | `consider <target>` | `do_consider`, `act.informative.c:1982` | universal |
//! | `sneak` | `do_sneak`, `act.other.c:101` | thief 1 |
//! | `hide` | `do_hide`, `act.other.c:126` | thief 5 |
//! | `pick_lock <direction>` | `do_gen_door` (`pick`), `act.movement.c:589` | thief 2 |
//!
//! The per-class minimum levels live in `tba.classes.skill_levels`
//! ([`TbaContent::skill_levels`]), transcribed from `init_spell_levels()`
//! (`tbamud/src/class.c:1652-1664`).
//!
//! ## Lockpicking
//!
//! Circle's `SKILL_PICK_LOCK` is exercised by `do_gen_door`'s `pick` subcommand
//! (`tbamud/src/interpreter.c:238` maps `pick` to `do_gen_door`/`SCMD_PICK`),
//! not by a standalone command. The roll lives in `ok_pick`
//! (`tbamud/src/act.movement.c:589-608`): `percent = rand_number(1, 101)`,
//! `skill_lvl = GET_SKILL(ch, SKILL_PICK_LOCK) + dex_app_skill[dex].p_locks`,
//! and the lock yields when `percent <= skill_lvl`; the success branch is
//! `do_doorcmd`/`SCMD_PICK` (`act.movement.c:560-565`).
//!
//! MIB's stdlib owns the bare `pick` command
//! ([`mib_stdlib::doors::handle_pick`], a W4.3 stub) and the fixed router
//! handles it before the pack-verb registry, so the pack cannot override it.
//! This module therefore adds **`pick_lock <direction>`**, gated on the thief's
//! `granted_skills`/`skill_levels` like the other abilities. It resolves the
//! direction to the room's authored door, requires the door pickable and
//! locked, and on success mutates the runtime [`RoomState`] exactly like the
//! zone engine's `D` reset ([`crate::zones::set_door_state`]); the pack owns
//! that mutation. MIB models no practice system, so the roll uses
//! [`SKILL_LEARNED`] (100) as Circle's skill level and omits the DEX bonus.
//!
//! ## Effects
//!
//! * **backstab** rolls the skill, then a melee to-hit, and multiplies the
//!   damage by `backstab_mult(level)` (`class.c:1551-1566`, applied at
//!   `fight.c:929`). Circle refuses a victim who is already fighting
//!   (`FIGHTING(vict)`); the strike is Circle's opening move.
//! * **kick** succeeds on Circle's AC-scaled skill roll
//!   (`percent = ((10 - AC/10) * 2) + rand(1, 101)`, `act.offensive.c:519`) and
//!   deals `GET_LEVEL(ch) / 2` (`act.offensive.c:525`).
//! * **bash** deals 1 and knocks the victim to `POS_SITTING`
//!   (`act.offensive.c:294-326`); a miss drops the *attacker* to sitting, and
//!   `MOB_NOBASH` forces a failure.
//! * **rescue** finds whoever is fighting the ally and retargets that attacker
//!   onto the rescuer, freeing the ally (`act.offensive.c:332-380`).
//! * **assist** retargets the actor onto the ally's attacker
//!   (`act.offensive.c:24-66`).
//! * **consider** prints `do_consider`'s exact level-difference message
//!   (`act.informative.c:1982-2020`).
//! * **hide**/**sneak** set [`TbaHidden`]/[`TbaSneaking`]; both are runtime-only
//!   components (see their docs), and attacking clears [`TbaHidden`].
//!
//! ## Move points
//!
//! The issue requires the physical skills to cost movement. Classic Circle's
//! `do_kick`/`do_bash` deduct move points and refuse an exhausted actor; this
//! tbaMUD revision no longer contains that check (verified against
//! `tbamud/src/act.offensive.c`), so the pack reinstates it as
//! [`SKILL_MOVE_COST`] with Circle's `"You are too exhausted."`
//! (`act.movement.c:261`).

use bevy::ecs::system::SystemParam;
use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{
    DoorState, Doors, Location, Npc, NpcId, NpcKey, Player, PlayerDefeated, Room, RoomOccupants,
    RoomState, RoomTag, Session, SessionState,
};

use crate::combat::commands::{
    StealthView, broadcast, deny, entity_name, find_target_in_room, text,
};
use crate::combat::rewards::{NpcDefeated, TbaDamage, alignment_for, npc_reward};
use crate::combat::{
    TbaAttackerMemory, TbaCombatant, TbaHidden, TbaProgress, TbaRng, TbaSneaking, WieldedWeapon,
    combat_dice,
};
use crate::content::{NpcBehaviour, Position, TbaClassDef, TbaItemKind};
use crate::dg::intake::commands::{DgClaimed, is_claimed};
use crate::module::TbaContent;
use crate::party::TbaFollowing;

/// Circle's refusal for `backstab`/`rescue`/`sneak`/`hide` (`do_*`).
const MSG_UNKNOWN_LONG: &str = "You have no idea how to do that.";
/// Circle's refusal for `kick`/`bash` (`do_kick`/`do_bash`).
const MSG_UNKNOWN_SHORT: &str = "You have no idea how.";
/// The `guild` practice refusal when the level gate is not met
/// (`tbamud/src/spec_procs.c:166`), reused for the runtime level gate.
const MSG_LEVEL: &str = "You do not know of that skill.";
/// Circle's position refusal (`do_bandage`, `act.offensive.c:543`).
const MSG_POSITION: &str = "You are not in a proper position for that!";
/// Circle's exhausted-move refusal (`act.movement.c:261`).
const MSG_EXHAUSTED: &str = "You are too exhausted.";
/// The move-point cost the physical skills deduct (issue #25; classic Circle
/// `do_kick`/`do_bash`; this tbaMUD revision dropped the check).
pub const SKILL_MOVE_COST: i32 = 10;
/// `TYPE_PIERCE - TYPE_HIT` (`tbamud/src/spells.h:131,142`): the `value[3]`
/// offset a weapon must carry to be usable for `backstab`.
const WEAPON_TYPE_PIERCE: i32 = 11;

/// One parsed skill line, before any gate or target resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillCommand {
    /// `backstab <target>`.
    Backstab { target: String },
    /// `kick <target>`.
    Kick { target: String },
    /// `bash <target>`.
    Bash { target: String },
    /// `rescue <ally>`.
    Rescue { ally: String },
    /// `assist [ally]`.
    Assist { ally: String },
    /// `consider <target>`.
    Consider { target: String },
    /// `sneak`.
    Sneak,
    /// `hide`.
    Hide,
    /// `pick_lock <direction>`.
    PickLock { direction: String },
}

/// Parse a raw line as a class-skill command, or `None` when it is not one.
pub fn parse_skill_line(line: &str) -> Option<SkillCommand> {
    let line = line.trim();
    let (verb, rest) = match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index + 1..].trim()),
        None => (line, ""),
    };
    let arg = rest.split_whitespace().next().unwrap_or("").to_owned();
    match verb.to_ascii_lowercase().as_str() {
        "backstab" => Some(SkillCommand::Backstab { target: arg }),
        "kick" => Some(SkillCommand::Kick { target: arg }),
        "bash" => Some(SkillCommand::Bash { target: arg }),
        "rescue" => Some(SkillCommand::Rescue { ally: arg }),
        "assist" => Some(SkillCommand::Assist { ally: arg }),
        "consider" => Some(SkillCommand::Consider { target: arg }),
        "sneak" => Some(SkillCommand::Sneak),
        "hide" => Some(SkillCommand::Hide),
        "pick_lock" => Some(SkillCommand::PickLock { direction: arg }),
        _ => None,
    }
}

/// Circle's `backstab_mult(level)` (`tbamud/src/class.c:1551-1566`).
///
/// The source's immortal branch (20) is unreachable here: MIB models no
/// implementor level, so the mortal cap of 6 is the last branch.
pub fn backstab_mult(level: u32) -> i32 {
    if level <= 7 {
        2
    } else if level <= 13 {
        3
    } else if level <= 20 {
        4
    } else if level <= 28 {
        5
    } else {
        6
    }
}

/// Circle's `do_consider` level-difference message (`act.informative.c:2001-2020`).
pub fn consider_message(diff: i32) -> &'static str {
    if diff <= -10 {
        "Now where did that chicken go?"
    } else if diff <= -5 {
        "You could do it with a needle!"
    } else if diff <= -2 {
        "Easy."
    } else if diff <= -1 {
        "Fairly easy."
    } else if diff == 0 {
        "The perfect match!"
    } else if diff <= 1 {
        "You would need some luck!"
    } else if diff <= 2 {
        "You would need a lot of luck!"
    } else if diff <= 3 {
        "You would need a lot of luck and great equipment!"
    } else if diff <= 5 {
        "Do you feel lucky, punk?"
    } else if diff <= 10 {
        "Are you mad!?"
    } else {
        "You ARE mad!"
    }
}

/// The `skill_info[].min_level[class]` gate for one skill: the value from
/// `tba.classes.skill_levels`, defaulting to level 1 for a granted skill with
/// no explicit entry.
pub fn skill_min_level(content: &TbaContent, class: &str, skill: &str) -> u32 {
    content
        .skill_levels
        .get(class)
        .and_then(|levels| levels.get(skill))
        .copied()
        .unwrap_or(1)
}

/// Circle's `GET_SKILL` percentage roll (`rand_number(1, 101) > prob` is a
/// failure; 101% is always a failure). MIB has no `practice` system, so a
/// granted skill is treated as fully learned ([`SKILL_LEARNED`]).
pub fn skill_succeeds(rng: &mut TbaRng, probability: i32) -> bool {
    rng.range(1, 101) <= probability
}

/// A granted skill's proficiency: fully learned. (Circle would start at 0 and
/// raise it with practice sessions; MIB models none, so a skill the class
/// learns is usable.)
pub const SKILL_LEARNED: i32 = 100;

/// Is `skill` in a class's `granted_skills`?
fn knows_class_skill(class: &TbaClassDef, skill: &str) -> bool {
    class
        .granted_skills
        .iter()
        .any(|granted| granted.eq_ignore_ascii_case(skill))
}

/// The class/level gate. `Some(message)` is a denial, `None` means allowed.
pub(crate) fn skill_gate(
    content: &TbaContent,
    progress: &TbaProgress,
    skill: &str,
    unknown_message: &str,
) -> Option<String> {
    let Some(class) = content.classes.get(&progress.class) else {
        return Some(unknown_message.to_owned());
    };
    if !knows_class_skill(class, skill) {
        return Some(unknown_message.to_owned());
    }
    if progress.level < skill_min_level(content, &progress.class, skill) {
        return Some(MSG_LEVEL.to_owned());
    }
    None
}

/// The room an entity is in, from either the player or the NPC query.
fn entity_room(
    entity: Entity,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
) -> Option<Entity> {
    if let Ok((_, _, location)) = players.get(entity) {
        return Some(location.room);
    }
    if let Ok((_, _, location)) = npcs.get(entity) {
        return Some(location.room);
    }
    None
}

/// The target of a `kick`/`bash`: the named room-mate, or Circle's fallback to
/// the actor's current opponent when the name does not resolve.
fn resolve_combat_target(
    session: Entity,
    needle: &str,
    current_target: Option<Entity>,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
    hidden: &StealthView<'_, '_>,
) -> Option<Entity> {
    if !needle.is_empty()
        && let Some(target) = find_target_in_room(session, needle, players, npcs, hidden)
    {
        return Some(target);
    }
    let room = entity_room(session, players, npcs)?;
    current_target.filter(|target| entity_room(*target, players, npcs) == Some(room))
}

/// Does an NPC carry `flag` in its `tba.npc_stats` behaviour list?
fn npc_behaviour_contains(
    entity: Entity,
    flag: NpcBehaviour,
    content: &TbaContent,
    keys: &Query<&NpcKey>,
) -> bool {
    keys.get(entity)
        .ok()
        .and_then(|key| content.npc_stats.get(&key.0))
        .is_some_and(|stats| stats.behaviour.contains(&flag))
}

/// Is the actor's room `ROOM_PEACEFUL` (`tbamud/src/structs.h`, `RoomTag::Peaceful`)?
fn room_is_peaceful(room: Entity, rooms: &Query<(&Room, &RoomState)>) -> bool {
    rooms
        .get(room)
        .map(|(room, state)| room.effective_flags(state).contains(&RoomTag::Peaceful))
        .unwrap_or(false)
}

/// Start (or join) a fight: the attacker targets the victim, and the victim
/// retaliates only when it was not already fighting (Circle's `damage()`,
/// `fight.c:661-668`). A `MEMORY` mob also records the attacker.
fn engage(
    attacker: Entity,
    target: Entity,
    combatants: &mut Query<&mut TbaCombatant>,
    memories: &mut Query<&mut TbaAttackerMemory>,
) {
    if let Ok(mut actor) = combatants.get_mut(attacker) {
        actor.target = Some(target);
        actor.position = Position::Fighting;
    }
    if let Ok(mut victim) = combatants.get_mut(target)
        && victim.is_alive()
    {
        if victim.target.is_none() {
            victim.target = Some(attacker);
        }
        victim.position = Position::Fighting;
    }
    if let Ok(mut memory) = memories.get_mut(target) {
        memory.0.insert(attacker);
    }
}

/// Apply melee damage through the shared lethal path (NPC -> despawn +
/// `NpcDefeated`, player -> `PlayerDefeated`), returning whether the blow was
/// lethal. Mirrors [`crate::combat::resolve_rounds`].
#[allow(clippy::too_many_arguments)]
fn deal_damage(
    attacker: Entity,
    target: Entity,
    amount: i32,
    content: &TbaContent,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
    keys: &Query<&NpcKey>,
    purses: &Query<&crate::dg::chardata::TbaNpcPurse>,
    combatants: &mut Query<&mut TbaCombatant>,
    defeated: &mut MessageWriter<PlayerDefeated>,
    npc_defeated: &mut MessageWriter<NpcDefeated>,
    damage_events: &mut MessageWriter<TbaDamage>,
    commands: &mut Commands,
) -> bool {
    {
        let Ok(mut victim) = combatants.get_mut(target) else {
            return false;
        };
        if !victim.is_alive() {
            return false;
        }
        victim.hp = (victim.hp - amount.max(0)).max(0);
        damage_events.write(TbaDamage {
            attacker,
            victim: target,
            damage: amount,
            lethal: victim.hp == 0,
        });
        if victim.hp > 0 {
            return false;
        }
        victim.position = Position::Dead;
        victim.target = None;
    }

    if players.get(target).is_ok() {
        defeated.write(PlayerDefeated { player: target });
    } else if npcs.get(target).is_ok() {
        let key = keys.get(target).ok();
        let purse = purses.get(target).ok();
        let (experience, gold) = npc_reward(content, key, purse);
        let alignment = key.map(|key| alignment_for(content, key)).unwrap_or(0);
        npc_defeated.write(NpcDefeated {
            victim: target,
            killer: attacker,
            npc: key
                .map(|key| key.0.clone())
                .unwrap_or_else(|| NpcId::from("")),
            experience,
            gold,
            alignment,
        });
        commands.queue(move |world: &mut World| {
            crate::dg::intake::combat::finish_kill(world, target, attacker);
        });
    }
    true
}

/// The read-only queries a skill command consults, bundled so the system stays
/// within Bevy's 16-parameter tuple limit.
#[derive(SystemParam)]
pub struct SkillQueries<'w, 's> {
    pub players: Query<'w, 's, (Entity, &'static Player, &'static Location)>,
    pub npcs: Query<'w, 's, (Entity, &'static Npc, &'static Location)>,
    pub content: Option<Res<'w, TbaContent>>,
    pub progress: Query<'w, 's, &'static TbaProgress>,
    pub hidden: StealthView<'w, 's>,
    pub doors: Query<'w, 's, &'static Doors>,
    pub following: Query<'w, 's, &'static TbaFollowing>,
    pub keys: Query<'w, 's, &'static NpcKey>,
    pub occupants: Res<'w, RoomOccupants>,
    pub rooms: Query<'w, 's, (&'static Room, &'static RoomState)>,
    pub wielded: Query<'w, 's, &'static WieldedWeapon>,
    pub purses: Query<'w, 's, &'static crate::dg::chardata::TbaNpcPurse>,
}

/// `BridgeSet::Game`: translate the issue #25 skill verbs into effects.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_skill_commands(
    mut reader: MessageReader<CommandReceived>,
    claimed: Option<Res<DgClaimed>>,
    sessions: Query<&Session>,
    mut combatants: Query<&mut TbaCombatant>,
    mut memories: Query<&mut TbaAttackerMemory>,
    queries: SkillQueries,
    mut rng: ResMut<TbaRng>,
    mut defeated: MessageWriter<PlayerDefeated>,
    mut npc_defeated: MessageWriter<NpcDefeated>,
    mut damage_events: MessageWriter<TbaDamage>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TbaContent::default();
    let content = queries.content.as_deref().unwrap_or(&empty);

    for msg in reader.read() {
        let session = msg.session;
        let playing = sessions
            .get(session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        if is_claimed(claimed.as_deref(), msg.id) {
            continue;
        }
        let PlayerInput::RawLine(line) = &msg.input else {
            continue;
        };
        let Some(command) = parse_skill_line(line) else {
            continue;
        };

        match command {
            // ---------------------------------------------------------------
            // backstab
            // ---------------------------------------------------------------
            SkillCommand::Backstab { target } => {
                let Some(progress) = queries.progress.get(session).ok() else {
                    deny(&mut output, session, MSG_UNKNOWN_LONG);
                    continue;
                };
                if let Some(message) = skill_gate(content, progress, "backstab", MSG_UNKNOWN_LONG) {
                    deny(&mut output, session, message);
                    continue;
                }
                let Some(victim) = find_target_in_room(
                    session,
                    &target,
                    &queries.players,
                    &queries.npcs,
                    &queries.hidden,
                ) else {
                    deny(&mut output, session, "Backstab who?");
                    continue;
                };
                if victim == session {
                    deny(&mut output, session, "How can you sneak up on yourself?");
                    continue;
                }
                let Ok(actor) = combatants.get_mut(session) else {
                    deny(&mut output, session, "You have no combat state.");
                    continue;
                };
                if !actor.is_alive() || actor.position < Position::Fighting {
                    deny(&mut output, session, MSG_POSITION);
                    continue;
                }
                // A piercing weapon is required (`GET_OBJ_VAL(wielded, 3) ==
                // TYPE_PIERCE - TYPE_HIT`, `act.offensive.c:148`).
                let Ok(WieldedWeapon(item_id)) = queries.wielded.get(session) else {
                    deny(
                        &mut output,
                        session,
                        "You need to wield a weapon to make it a success.",
                    );
                    continue;
                };
                let piercing = content.items.get(item_id).is_some_and(|item| {
                    item.kind == TbaItemKind::Weapon && item.values[3] == WEAPON_TYPE_PIERCE
                });
                if !piercing {
                    deny(
                        &mut output,
                        session,
                        "Only piercing weapons can be used for backstabbing.",
                    );
                    continue;
                }
                let (victim_fighting, victim_awake, victim_ac) = match combatants.get_mut(victim) {
                    Ok(c) => (
                        c.target.is_some() || c.position == Position::Fighting,
                        c.position > Position::Sleeping,
                        c.armor_class,
                    ),
                    Err(_) => (false, true, 0),
                };
                if victim_fighting {
                    deny(
                        &mut output,
                        session,
                        "You can't backstab a fighting person -- they're too alert!",
                    );
                    continue;
                }
                if entity_room(session, &queries.players, &queries.npcs)
                    .is_some_and(|room| room_is_peaceful(room, &queries.rooms))
                {
                    deny(
                        &mut output,
                        session,
                        "This room just has such a peaceful, easy feeling...",
                    );
                    continue;
                }

                let was_hidden = queries
                    .hidden
                    .concealment
                    .get(session)
                    .is_ok_and(|concealment| concealment.hidden);
                // Attacking breaks stealth (`appear()`, `fight.c:70-84`).
                commands.entity(session).remove::<TbaHidden>();
                engage(session, victim, &mut combatants, &mut memories);

                let percent = rng.range(1, 101);
                let victim_name = entity_name(victim, &queries.players, &queries.npcs);
                if victim_awake && percent > SKILL_LEARNED {
                    output.write(text(
                        session,
                        format!("Your backstab misses {victim_name}."),
                    ));
                    continue;
                }
                let damage = {
                    let Ok(attacker) = combatants.get_mut(session) else {
                        continue;
                    };
                    let roll = rng.d20() + attacker.hitroll + attacker.attributes.to_hit_bonus();
                    let needed = attacker.thaco - victim_ac / 10;
                    if roll < needed {
                        output.write(text(
                            session,
                            format!("Your backstab misses {victim_name}."),
                        ));
                        continue;
                    }
                    let dice = combat_dice(queries.wielded.get(session).ok(), Some(content), None);
                    let base = (rng.roll(&dice) + attacker.damroll).max(1);
                    let multiplier = if was_hidden || !victim_fighting {
                        backstab_mult(progress.level)
                    } else {
                        1
                    };
                    base * multiplier
                };
                let lethal = deal_damage(
                    session,
                    victim,
                    damage,
                    content,
                    &queries.players,
                    &queries.npcs,
                    &queries.keys,
                    &queries.purses,
                    &mut combatants,
                    &mut defeated,
                    &mut npc_defeated,
                    &mut damage_events,
                    &mut commands,
                );
                let actor_name = entity_name(session, &queries.players, &queries.npcs);
                output.write(text(
                    session,
                    format!("You backstab {victim_name} for {damage} damage!"),
                ));
                if let Some(room) = entity_room(session, &queries.players, &queries.npcs) {
                    broadcast(
                        &queries.players,
                        room,
                        session,
                        &mut output,
                        &format!("{actor_name} backstabs {victim_name} for {damage} damage!"),
                    );
                }
                let _ = lethal;
            }

            // ---------------------------------------------------------------
            // kick
            // ---------------------------------------------------------------
            SkillCommand::Kick { target } => {
                let Some(progress) = queries.progress.get(session).ok() else {
                    deny(&mut output, session, MSG_UNKNOWN_SHORT);
                    continue;
                };
                if let Some(message) = skill_gate(content, progress, "kick", MSG_UNKNOWN_SHORT) {
                    deny(&mut output, session, message);
                    continue;
                }
                let current_target = combatants.get_mut(session).ok().and_then(|c| c.target);
                let victim = resolve_combat_target(
                    session,
                    &target,
                    current_target,
                    &queries.players,
                    &queries.npcs,
                    &queries.hidden,
                );
                let Some(victim) = victim else {
                    deny(&mut output, session, "Kick who?");
                    continue;
                };
                if victim == session {
                    deny(&mut output, session, "Aren't we funny today...");
                    continue;
                }
                let Ok(mut actor) = combatants.get_mut(session) else {
                    deny(&mut output, session, "You have no combat state.");
                    continue;
                };
                if !actor.is_alive() || actor.position < Position::Fighting {
                    deny(&mut output, session, MSG_POSITION);
                    continue;
                }
                if actor.move_points < SKILL_MOVE_COST {
                    deny(&mut output, session, MSG_EXHAUSTED);
                    continue;
                }
                if entity_room(session, &queries.players, &queries.npcs)
                    .is_some_and(|room| room_is_peaceful(room, &queries.rooms))
                {
                    deny(
                        &mut output,
                        session,
                        "This room just has such a peaceful, easy feeling...",
                    );
                    continue;
                }
                let level = progress.level;
                actor.move_points -= SKILL_MOVE_COST;
                let victim_ac = combatants
                    .get_mut(victim)
                    .ok()
                    .map_or(0, |combatant| combatant.armor_class);
                commands.entity(session).remove::<TbaHidden>();
                engage(session, victim, &mut combatants, &mut memories);

                // Circle's kick check (`act.offensive.c:519-525`):
                // `percent = ((10 - AC/10) * 2) + rand_number(1, 101)`.
                let percent = ((10 - victim_ac / 10) * 2) + rng.range(1, 101);
                let victim_name = entity_name(victim, &queries.players, &queries.npcs);
                let damage = if percent > SKILL_LEARNED {
                    0
                } else {
                    (level / 2) as i32
                };
                if damage > 0 {
                    let lethal = deal_damage(
                        session,
                        victim,
                        damage,
                        content,
                        &queries.players,
                        &queries.npcs,
                        &queries.keys,
                        &queries.purses,
                        &mut combatants,
                        &mut defeated,
                        &mut npc_defeated,
                        &mut damage_events,
                        &mut commands,
                    );
                    let actor_name = entity_name(session, &queries.players, &queries.npcs);
                    output.write(text(
                        session,
                        format!("You kick {victim_name} for {damage} damage."),
                    ));
                    if let Some(room) = entity_room(session, &queries.players, &queries.npcs) {
                        broadcast(
                            &queries.players,
                            room,
                            session,
                            &mut output,
                            &format!("{actor_name} kicks {victim_name}."),
                        );
                    }
                    let _ = lethal;
                } else {
                    output.write(text(session, format!("Your kick misses {victim_name}.")));
                }
            }

            // ---------------------------------------------------------------
            // bash
            // ---------------------------------------------------------------
            SkillCommand::Bash { target } => {
                let Some(progress) = queries.progress.get(session).ok() else {
                    deny(&mut output, session, MSG_UNKNOWN_SHORT);
                    continue;
                };
                if let Some(message) = skill_gate(content, progress, "bash", MSG_UNKNOWN_SHORT) {
                    deny(&mut output, session, message);
                    continue;
                }
                if entity_room(session, &queries.players, &queries.npcs)
                    .is_some_and(|room| room_is_peaceful(room, &queries.rooms))
                {
                    deny(
                        &mut output,
                        session,
                        "This room just has such a peaceful, easy feeling...",
                    );
                    continue;
                }
                if queries.wielded.get(session).is_err() {
                    deny(
                        &mut output,
                        session,
                        "You need to wield a weapon to make it a success.",
                    );
                    continue;
                }
                let current_target = combatants.get_mut(session).ok().and_then(|c| c.target);
                let victim = resolve_combat_target(
                    session,
                    &target,
                    current_target,
                    &queries.players,
                    &queries.npcs,
                    &queries.hidden,
                );
                let Some(victim) = victim else {
                    deny(&mut output, session, "Bash who?");
                    continue;
                };
                if victim == session {
                    deny(&mut output, session, "Aren't we funny today...");
                    continue;
                }
                if npc_behaviour_contains(victim, NpcBehaviour::NoKill, content, &queries.keys) {
                    deny(&mut output, session, "This mob is protected.");
                    continue;
                }
                let Ok(mut actor) = combatants.get_mut(session) else {
                    deny(&mut output, session, "You have no combat state.");
                    continue;
                };
                if !actor.is_alive() || actor.position < Position::Fighting {
                    deny(&mut output, session, MSG_POSITION);
                    continue;
                }
                if actor.move_points < SKILL_MOVE_COST {
                    deny(&mut output, session, MSG_EXHAUSTED);
                    continue;
                }
                actor.move_points -= SKILL_MOVE_COST;
                commands.entity(session).remove::<TbaHidden>();
                engage(session, victim, &mut combatants, &mut memories);

                let nobash =
                    npc_behaviour_contains(victim, NpcBehaviour::NoBash, content, &queries.keys);
                let percent = if nobash { 101 } else { rng.range(1, 101) };
                let victim_name = entity_name(victim, &queries.players, &queries.npcs);
                if percent > SKILL_LEARNED {
                    // A miss drops the attacker; the mob is untouched.
                    if let Ok(mut actor) = combatants.get_mut(session) {
                        actor.position = Position::Sitting;
                    }
                    output.write(text(
                        session,
                        format!("Your bash misses {victim_name}, and you fall to the ground!"),
                    ));
                    continue;
                }
                let lethal = deal_damage(
                    session,
                    victim,
                    1,
                    content,
                    &queries.players,
                    &queries.npcs,
                    &queries.keys,
                    &queries.purses,
                    &mut combatants,
                    &mut defeated,
                    &mut npc_defeated,
                    &mut damage_events,
                    &mut commands,
                );
                if !lethal {
                    if let Ok(mut victim) = combatants.get_mut(victim) {
                        victim.position = Position::Sitting;
                    }
                    output.write(text(
                        session,
                        format!("You bash {victim_name} to the ground!"),
                    ));
                    let actor_name = entity_name(session, &queries.players, &queries.npcs);
                    if let Some(room) = entity_room(session, &queries.players, &queries.npcs) {
                        broadcast(
                            &queries.players,
                            room,
                            session,
                            &mut output,
                            &format!("{actor_name} bashes {victim_name} to the ground!"),
                        );
                    }
                } else {
                    output.write(text(session, format!("You bash {victim_name}.")));
                }
            }

            // ---------------------------------------------------------------
            // rescue
            // ---------------------------------------------------------------
            SkillCommand::Rescue { ally } => {
                let Some(progress) = queries.progress.get(session).ok() else {
                    deny(&mut output, session, MSG_UNKNOWN_LONG);
                    continue;
                };
                if let Some(message) = skill_gate(content, progress, "rescue", MSG_UNKNOWN_LONG) {
                    deny(&mut output, session, message);
                    continue;
                }
                let Some(ally) = find_target_in_room(
                    session,
                    &ally,
                    &queries.players,
                    &queries.npcs,
                    &queries.hidden,
                ) else {
                    deny(&mut output, session, "Whom do you want to rescue?");
                    continue;
                };
                if ally == session {
                    deny(&mut output, session, "What about fleeing instead?");
                    continue;
                }
                if combatants.get_mut(session).ok().and_then(|c| c.target) == Some(ally) {
                    deny(
                        &mut output,
                        session,
                        "How can you rescue someone you are trying to kill?",
                    );
                    continue;
                }
                let Some(room) = entity_room(session, &queries.players, &queries.npcs) else {
                    continue;
                };
                // Whoever in the room is fighting the ally.
                let attacker = queries
                    .occupants
                    .occupants(room)
                    .iter()
                    .copied()
                    .find(|&other| {
                        other != ally
                            && combatants
                                .get_mut(other)
                                .ok()
                                .and_then(|c| c.target)
                                .is_some_and(|target| target == ally)
                    });
                let Some(attacker) = attacker else {
                    deny(
                        &mut output,
                        session,
                        format!(
                            "But nobody is fighting {}!",
                            entity_name(ally, &queries.players, &queries.npcs)
                        ),
                    );
                    continue;
                };
                if !skill_succeeds(&mut rng, SKILL_LEARNED) {
                    deny(&mut output, session, "You fail the rescue!");
                    continue;
                }
                // Retarget the attacker onto the rescuer and free the ally.
                if let Ok(mut attacker_combat) = combatants.get_mut(attacker) {
                    attacker_combat.target = Some(session);
                }
                engage(session, attacker, &mut combatants, &mut memories);
                if let Ok(mut ally_combat) = combatants.get_mut(ally)
                    && ally_combat.target == Some(attacker)
                {
                    ally_combat.target = None;
                }
                let actor_name = entity_name(session, &queries.players, &queries.npcs);
                let ally_name = entity_name(ally, &queries.players, &queries.npcs);
                output.write(text(session, "Banzai!  To the rescue..."));
                if queries.players.get(ally).is_ok() {
                    output.write(text(
                        ally,
                        format!("You are rescued by {actor_name}, you are confused!"),
                    ));
                }
                broadcast(
                    &queries.players,
                    room,
                    session,
                    &mut output,
                    &format!("{actor_name} heroically rescues {ally_name}!"),
                );
            }

            // ---------------------------------------------------------------
            // assist
            // ---------------------------------------------------------------
            SkillCommand::Assist { ally } => {
                let Ok(actor) = combatants.get_mut(session) else {
                    deny(&mut output, session, "You have no combat state.");
                    continue;
                };
                if actor.target.is_some() {
                    deny(
                        &mut output,
                        session,
                        "You're already fighting!  How can you assist someone else?",
                    );
                    continue;
                }
                // `assist` with no argument falls back to the leader the actor
                // follows; Circle requires an explicit name.
                let ally_entity = if ally.is_empty() {
                    queries
                        .following
                        .get(session)
                        .ok()
                        .map(|following| following.leader)
                        .filter(|leader| *leader != session)
                } else {
                    find_target_in_room(
                        session,
                        &ally,
                        &queries.players,
                        &queries.npcs,
                        &queries.hidden,
                    )
                };
                let Some(ally_entity) = ally_entity else {
                    deny(&mut output, session, "Whom do you wish to assist?");
                    continue;
                };
                if ally_entity == session {
                    deny(
                        &mut output,
                        session,
                        "You can't help yourself any more than this!",
                    );
                    continue;
                }
                let opponent = combatants
                    .get_mut(ally_entity)
                    .ok()
                    .and_then(|c| c.target)
                    .or_else(|| {
                        entity_room(session, &queries.players, &queries.npcs).and_then(|room| {
                            queries
                                .occupants
                                .occupants(room)
                                .iter()
                                .copied()
                                .find(|&other| {
                                    other != ally_entity
                                        && combatants
                                            .get_mut(other)
                                            .ok()
                                            .and_then(|c| c.target)
                                            .is_some_and(|target| target == ally_entity)
                                })
                        })
                    });
                let Some(opponent) = opponent else {
                    deny(
                        &mut output,
                        session,
                        format!(
                            "But nobody is fighting {}!",
                            entity_name(ally_entity, &queries.players, &queries.npcs)
                        ),
                    );
                    continue;
                };
                engage(session, opponent, &mut combatants, &mut memories);
                let actor_name = entity_name(session, &queries.players, &queries.npcs);
                let ally_name = entity_name(ally_entity, &queries.players, &queries.npcs);
                output.write(text(session, "You join the fight!"));
                if queries.players.get(ally_entity).is_ok() {
                    output.write(text(ally_entity, format!("{actor_name} assists you!")));
                }
                if let Some(room) = entity_room(session, &queries.players, &queries.npcs) {
                    broadcast(
                        &queries.players,
                        room,
                        session,
                        &mut output,
                        &format!("{actor_name} assists {ally_name}."),
                    );
                }
            }

            // ---------------------------------------------------------------
            // consider
            // ---------------------------------------------------------------
            SkillCommand::Consider { target } => {
                let Some(progress) = queries.progress.get(session).ok() else {
                    deny(&mut output, session, "You have no combat state.");
                    continue;
                };
                let Some(victim) = find_target_in_room(
                    session,
                    &target,
                    &queries.players,
                    &queries.npcs,
                    &queries.hidden,
                ) else {
                    deny(&mut output, session, "Consider killing who?");
                    continue;
                };
                if victim == session {
                    output.write(text(session, "Easy!  Very easy indeed!"));
                    continue;
                }
                if queries.players.get(victim).is_ok() {
                    output.write(text(
                        session,
                        "Would you like to borrow a cross and a shovel?",
                    ));
                    continue;
                }
                let victim_level = queries
                    .keys
                    .get(victim)
                    .ok()
                    .and_then(|key| content.npc_stats.get(&key.0))
                    .map(|stats| stats.level)
                    .unwrap_or(progress.level);
                let diff = victim_level as i32 - progress.level as i32;
                output.write(text(session, consider_message(diff)));
            }

            // ---------------------------------------------------------------
            // sneak
            // ---------------------------------------------------------------
            SkillCommand::Sneak => {
                let Some(progress) = queries.progress.get(session).ok() else {
                    deny(&mut output, session, MSG_UNKNOWN_LONG);
                    continue;
                };
                if let Some(message) = skill_gate(content, progress, "sneak", MSG_UNKNOWN_LONG) {
                    deny(&mut output, session, message);
                    continue;
                }
                // Circle removes the existing affect, then re-rolls.
                commands.entity(session).remove::<TbaSneaking>();
                output.write(text(
                    session,
                    "Okay, you'll try to move silently for a while.",
                ));
                if skill_succeeds(&mut rng, SKILL_LEARNED) {
                    commands.entity(session).insert(TbaSneaking);
                }
            }

            // ---------------------------------------------------------------
            // hide
            // ---------------------------------------------------------------
            SkillCommand::Hide => {
                let Some(progress) = queries.progress.get(session).ok() else {
                    deny(&mut output, session, MSG_UNKNOWN_LONG);
                    continue;
                };
                if let Some(message) = skill_gate(content, progress, "hide", MSG_UNKNOWN_LONG) {
                    deny(&mut output, session, message);
                    continue;
                }
                // Circle clears an existing hide, then re-rolls.
                commands.entity(session).remove::<TbaHidden>();
                output.write(text(session, "You attempt to hide yourself."));
                if skill_succeeds(&mut rng, SKILL_LEARNED) {
                    commands.entity(session).insert(TbaHidden);
                }
            }

            // ---------------------------------------------------------------
            // pick_lock
            // ---------------------------------------------------------------
            SkillCommand::PickLock { direction } => {
                let Some(progress) = queries.progress.get(session).ok() else {
                    deny(&mut output, session, MSG_UNKNOWN_LONG);
                    continue;
                };
                if let Some(message) = skill_gate(content, progress, "pick_lock", MSG_UNKNOWN_LONG)
                {
                    deny(&mut output, session, message);
                    continue;
                }
                // `pick` is `POS_STANDING` (`interpreter.c:238`).
                let position = combatants
                    .get(session)
                    .map(|combatant| combatant.position)
                    .unwrap_or(Position::Dead);
                if position < Position::Standing {
                    deny(&mut output, session, MSG_POSITION);
                    continue;
                }
                if direction.is_empty() {
                    deny(&mut output, session, "Pick what?");
                    continue;
                }
                let Some(room) = entity_room(session, &queries.players, &queries.npcs) else {
                    continue;
                };
                // `find_door` accepts a direction word (`act.movement.c:371`).
                let Some(direction) = ExitKey::from_word(&direction.to_lowercase()) else {
                    deny(&mut output, session, "That's not a direction.");
                    continue;
                };
                let Some(door) = queries
                    .doors
                    .get(room)
                    .ok()
                    .and_then(|doors| doors.by_dir.get(&direction).cloned())
                else {
                    deny(
                        &mut output,
                        session,
                        "I really don't see how you can pick anything there.",
                    );
                    continue;
                };
                // Runtime state is authoritative; the authored door is the
                // fallback when the entry is missing (`doors::state_at`).
                let state = queries
                    .rooms
                    .get(room)
                    .ok()
                    .and_then(|(_, state)| state.doors.get(&direction).cloned());
                let closed = state
                    .as_ref()
                    .map_or(door.default_closed, |door| door.closed);
                let locked = state
                    .as_ref()
                    .map_or(door.default_locked, |door| door.locked);
                // `do_gen_door`'s `pick` flags: `NEED_CLOSED | NEED_LOCKED`
                // (`act.movement.c:489-495, 653-657`).
                if !closed {
                    deny(&mut output, session, "But it's currently open!");
                    continue;
                }
                if !locked {
                    deny(&mut output, session, "Oh.. it wasn't locked, after all..");
                    continue;
                }
                // `ok_pick`'s `pickproof` branch (`act.movement.c:594`).
                if !door.pickable {
                    deny(&mut output, session, "It resists your attempts to pick it.");
                    continue;
                }
                // `ok_pick` (`act.movement.c:589-608`): `percent =
                // rand_number(1, 101)`, `skill_lvl = GET_SKILL(ch,
                // SKILL_PICK_LOCK)` (the DEX bonus is not modelled), success
                // on `percent <= skill_lvl`.
                if !skill_succeeds(&mut rng, SKILL_LEARNED) {
                    deny(&mut output, session, "You failed to pick the lock.");
                    continue;
                }
                // The lock yields: mutate the runtime `RoomState` exactly like
                // the zone engine's `D` reset (`zones/mod.rs::set_door_state`).
                let mut room_state = queries
                    .rooms
                    .get(room)
                    .map(|(_, state)| state.clone())
                    .unwrap_or_default();
                if let Some(door_state) = room_state.doors.get_mut(&direction) {
                    door_state.closed = true;
                    door_state.locked = false;
                } else {
                    room_state.doors.insert(
                        direction,
                        DoorState {
                            closed: true,
                            locked: false,
                            to: None,
                        },
                    );
                }
                commands.entity(room).insert(room_state);
                output.write(text(session, "The lock quickly yields to your skills."));
                let door_name = door.name.as_deref().unwrap_or("door");
                let actor_name = entity_name(session, &queries.players, &queries.npcs);
                broadcast(
                    &queries.players,
                    room,
                    session,
                    &mut output,
                    &format!("{actor_name} skillfully picks the lock on the {door_name}."),
                );
            }
        }

        write_prompt(&mut output, session);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use smol_str::SmolStr;

    fn content_with(class_id: &str, granted: &[&str], levels: &[(&str, u32)]) -> TbaContent {
        let mut content = TbaContent::default();
        content.classes.insert(
            SmolStr::new(class_id),
            TbaClassDef {
                name: class_id.to_owned(),
                description: String::new(),
                hp_dice: Dice::new(1, 10, 0),
                mana_dice: None,
                xp_table: Vec::new(),
                thaco_table: Vec::new(),
                prime_attribute: None,
                starting_items: Vec::new(),
                granted_skills: granted.iter().map(|s| SmolStr::new(*s)).collect(),
            },
        );
        content.skill_levels.insert(
            SmolStr::new(class_id),
            levels
                .iter()
                .map(|(skill, level)| (SmolStr::new(*skill), *level))
                .collect(),
        );
        content
    }

    #[test]
    fn parses_every_skill_verb() {
        assert_eq!(
            parse_skill_line("backstab goblin"),
            Some(SkillCommand::Backstab {
                target: "goblin".to_owned()
            })
        );
        assert_eq!(
            parse_skill_line("KICK goblin"),
            Some(SkillCommand::Kick {
                target: "goblin".to_owned()
            })
        );
        assert_eq!(
            parse_skill_line("bash"),
            Some(SkillCommand::Bash {
                target: String::new()
            })
        );
        assert_eq!(
            parse_skill_line("rescue ally"),
            Some(SkillCommand::Rescue {
                ally: "ally".to_owned()
            })
        );
        assert_eq!(
            parse_skill_line("assist"),
            Some(SkillCommand::Assist {
                ally: String::new()
            })
        );
        assert_eq!(
            parse_skill_line("consider goblin"),
            Some(SkillCommand::Consider {
                target: "goblin".to_owned()
            })
        );
        assert_eq!(parse_skill_line("sneak"), Some(SkillCommand::Sneak));
        assert_eq!(parse_skill_line("hide"), Some(SkillCommand::Hide));
        assert_eq!(
            parse_skill_line("pick_lock north"),
            Some(SkillCommand::PickLock {
                direction: "north".to_owned()
            })
        );
        assert_eq!(parse_skill_line("attack goblin"), None);
        // The bare stdlib `pick` stays with the router; the pack verb is
        // `pick_lock`.
        assert_eq!(parse_skill_line("pick"), None);
        assert_eq!(parse_skill_line("pick lock"), None);
    }

    #[test]
    fn backstab_multiplier_follows_the_circle_table() {
        // class.c:1551-1566: <=7 -> 2, <=13 -> 3, <=20 -> 4, <=28 -> 5, else 6.
        assert_eq!(backstab_mult(1), 2);
        assert_eq!(backstab_mult(7), 2);
        assert_eq!(backstab_mult(8), 3);
        assert_eq!(backstab_mult(13), 3);
        assert_eq!(backstab_mult(14), 4);
        assert_eq!(backstab_mult(20), 4);
        assert_eq!(backstab_mult(21), 5);
        assert_eq!(backstab_mult(28), 5);
        assert_eq!(backstab_mult(29), 6);
        assert_eq!(backstab_mult(34), 6);
    }

    #[test]
    fn consider_messages_follow_the_circle_table() {
        assert_eq!(consider_message(-10), "Now where did that chicken go?");
        assert_eq!(consider_message(-5), "You could do it with a needle!");
        assert_eq!(consider_message(-2), "Easy.");
        assert_eq!(consider_message(-1), "Fairly easy.");
        assert_eq!(consider_message(0), "The perfect match!");
        assert_eq!(consider_message(1), "You would need some luck!");
        assert_eq!(consider_message(2), "You would need a lot of luck!");
        assert_eq!(
            consider_message(3),
            "You would need a lot of luck and great equipment!"
        );
        assert_eq!(consider_message(5), "Do you feel lucky, punk?");
        assert_eq!(consider_message(10), "Are you mad!?");
        assert_eq!(consider_message(11), "You ARE mad!");
    }

    #[test]
    fn gates_on_class_and_level() {
        let content = content_with(
            "warrior",
            &["kick", "rescue", "bash"],
            &[("kick", 1), ("rescue", 3), ("bash", 12)],
        );
        let level_one = TbaProgress::new("warrior");
        assert!(skill_gate(&content, &level_one, "kick", MSG_UNKNOWN_SHORT).is_none());
        assert_eq!(
            skill_gate(&content, &level_one, "bash", MSG_UNKNOWN_SHORT).as_deref(),
            Some(MSG_LEVEL)
        );
        assert_eq!(
            skill_gate(&content, &level_one, "hide", MSG_UNKNOWN_LONG).as_deref(),
            Some(MSG_UNKNOWN_LONG),
            "a warrior does not know hide"
        );
        let mut level_twelve = TbaProgress::new("warrior");
        level_twelve.level = 12;
        assert!(skill_gate(&content, &level_twelve, "bash", MSG_UNKNOWN_SHORT).is_none());
        assert_eq!(
            skill_gate(&content, &level_twelve, "backstab", MSG_UNKNOWN_LONG).as_deref(),
            Some(MSG_UNKNOWN_LONG)
        );
    }

    #[test]
    fn skill_min_level_defaults_to_one() {
        let content = content_with("thief", &["sneak"], &[]);
        assert_eq!(skill_min_level(&content, "thief", "sneak"), 1);
        assert_eq!(skill_min_level(&content, "thief", "missing"), 1);
    }

    #[test]
    fn skill_roll_reaches_both_branches() {
        let mut always = TbaRng::from_seed(1);
        assert!(
            skill_succeeds(&mut always, 101),
            "101 means never a failure"
        );
        let mut never = TbaRng::from_seed(1);
        assert!(!skill_succeeds(&mut never, 0), "0 means never a success");
    }
}
