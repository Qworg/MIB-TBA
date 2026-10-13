//! Intake for fight, hitprcnt, damage, death and cast triggers.
//!
//! # Combat
//!
//! [`resolve_rounds`](crate::combat::resolve_rounds) resolves a round one
//! attacker at a time inside a single [`with_host`] call, as
//! `perform_violence` does, and calls the functions here at the C's points:
//!
//! - [`fight`] at the start of each blow (`hit`), before the attacker's
//!   target is looked at again;
//! - [`damage`] with the blow's damage, a miss included (`hit` calls
//!   `damage(.., 0, ..)` on a miss), before it is applied (`damage`): the
//!   trigger's return value replaces the damage and `-1` cancels the blow;
//! - [`death`] once a blow has reduced a character to zero hit points and
//!   before it is removed (`raw_kill`), with the victim still in its room;
//! - [`hitprcnt`] at the end of the blow, for the victim.
//!
//! The C runs `death_mtrigger` only for a death with a killer, and only on the
//! dying character: no room or object `death` trigger exists, so a player's
//! death runs just the player's own `death` triggers (those attached to the
//! player; the dispatcher finds none on an ordinary player).
//!
//! Kills made outside the combat round (a damage spell) reach [`finish_kill`],
//! which the spell handler queues in place of removing the victim itself.
//!
//! # Cast
//!
//! `call_magic` runs `cast_wtrigger`, `cast_otrigger` and `cast_mtrigger`
//! before the spell takes effect, and a refusal ends the cast: [`cast_gate`]
//! does this for the player `cast` command, resolving the spell and target the
//! handler will and marking the command handled on a refusal. [`cast_allowed`]
//! is the same three calls for any other caster.
//!
//! Losses against the C: the pack has no `death_cry`, so `death_mtrigger`'s
//! return value has nothing to decide; a spell's damage does not call
//! `damage_mtrigger` (the spell handler is not an exclusive system); and the
//! pack's blow has no weapon attack type, so `%attacktype%` is the C's
//! `TYPE_HIT` slot, which the C names `UNDEFINED`.

use bevy::ecs::system::SystemState;
use dg_vm::Uid;
use dg_vm::dispatch::{
    cast_mtrigger, cast_otrigger, cast_wtrigger, damage_mtrigger, death_mtrigger, fight_mtrigger,
    hitprcnt_mtrigger,
};
use mib_core::prelude::*;
use mib_stdlib::prelude::{Location, Npc, Player, Session, SessionState};

use super::DgIntake;
use crate::combat::commands::{StealthView, broadcast, text};
use crate::combat::{TbaCombatant, TbaProgress};
use crate::content::Position;
use crate::dg::host::EcsHost;
use crate::dg::script::DgDead;
use crate::dg::with_host;
use crate::module::TbaContent;
use crate::spells::{CastOutcome, plan_cast};

#[path = "combat/spell_numbers.rs"]
mod spell_numbers;

pub use spell_numbers::spell_number;

/// `TYPE_HIT` (`spells.h`): the attack type of a plain blow.
pub const TYPE_HIT: i32 = 300;

/// Register this module's systems.
pub fn register(app: &mut App) {
    app.add_systems(Update, cast_gate.in_set(DgIntake::PreCommit));
}

/// `fight_mtrigger(ch)`: the start of one of `attacker`'s blows.
pub fn fight(host: &mut EcsHost<'_>, attacker: Entity) {
    if let Some(uid) = host.uid(attacker) {
        fight_mtrigger(host, uid);
    }
}

/// `damage_mtrigger(attacker, victim, dam, TYPE_HIT)`: the damage a blow of
/// `dam` points will do after the victim's `damage` trigger, or `None` when
/// the trigger cancelled the blow (`-1`). The C clamps what remains to
/// `0..=100`; the pack, whose blows are not capped, clamps below only.
pub fn damage(host: &mut EcsHost<'_>, attacker: Entity, victim: Entity, dam: i32) -> Option<i32> {
    let (Some(actor), Some(target)) = (host.uid(attacker), host.uid(victim)) else {
        return Some(dam);
    };
    match damage_mtrigger(host, actor, Some(target), dam, TYPE_HIT) {
        -1 => None,
        changed => Some(changed.max(0)),
    }
}

/// `hitprcnt_mtrigger(victim)`: the end of a blow struck at `victim`.
pub fn hitprcnt(host: &mut EcsHost<'_>, victim: Entity) {
    if let Some(uid) = host.uid(victim) {
        hitprcnt_mtrigger(host, uid);
    }
}

/// `raw_kill`'s `death_mtrigger(victim, killer)`. Returns what the C uses to
/// decide the death cry (`false` when a script returned `0`).
///
/// The victim is still in the world, its hit points at zero. The C sets it
/// `POS_STANDING` first "to make ordinary commands work in scripts", and so
/// is done here, undone afterwards.
pub fn death(host: &mut EcsHost<'_>, victim: Entity, killer: Entity) -> bool {
    let Some(dying) = host.uid(victim) else {
        return true;
    };
    let killer = host.uid(killer);
    let was_dead = host
        .world
        .get_mut::<TbaCombatant>(victim)
        .filter(|combatant| combatant.position == Position::Dead)
        .map(|mut combatant| combatant.position = Position::Standing)
        .is_some();
    let result = death_mtrigger(host, dying, killer);
    if was_dead
        && let Some(mut combatant) = host.world.get_mut::<TbaCombatant>(victim)
        && combatant.position == Position::Standing
    {
        combatant.position = Position::Dead;
    }
    result != 0
}

/// A kill made outside the combat round: run the victim's `death` trigger
/// while it is still in the world, then remove it if it is an NPC. A player
/// stays; the defeat path owns it.
pub fn finish_kill(world: &mut World, victim: Entity, killer: Entity) {
    if world.get_entity(victim).is_err() {
        return;
    }
    with_host(world, |host| {
        death(host, victim, killer);
    });
    if world.get::<Npc>(victim).is_some() {
        world.despawn(victim);
    }
}

/// `call_magic`'s three trigger calls, in the C's order: the caster's room,
/// then the target object, then the target character. `false` cancels the
/// spell.
pub fn cast_allowed(
    host: &mut EcsHost<'_>,
    caster: Uid,
    victim: Option<Uid>,
    obj: Option<Uid>,
    spellnum: i32,
) -> bool {
    cast_wtrigger(host, caster, victim, obj, spellnum) != 0
        && cast_otrigger(host, caster, obj, spellnum) != 0
        && cast_mtrigger(host, caster, victim, spellnum) != 0
}

/// A `cast` line the gate resolved, to be tested against the triggers.
struct PendingCast {
    id: CommandId,
    caster: Entity,
    victim: Option<Entity>,
    spellnum: i32,
    spell_name: String,
    room: Option<Entity>,
}

type GateParams = (
    MessageReader<'static, 'static, CommandReceived>,
    Option<Res<'static, HandledCommands>>,
    Query<'static, 'static, &'static Session>,
    Query<'static, 'static, (Entity, &'static Player, &'static Location)>,
    Query<'static, 'static, (Entity, &'static Npc, &'static Location)>,
    StealthView<'static, 'static>,
    Option<Res<'static, TbaContent>>,
    Query<'static, 'static, &'static mut TbaCombatant>,
    Query<'static, 'static, &'static TbaProgress>,
    MessageWriter<'static, OutputEvent>,
);

/// `DgIntake::PreCommit`: the `cast` triggers of the player `cast` command.
///
/// The spell and its targets are resolved by the handler's own
/// [`plan_cast`], so a line the handler would refuse (unknown spell, no mana,
/// no such target) is left to it. For a line it would carry out, the triggers
/// run as `call_magic` runs them, before mana is spent or any effect applied;
/// when one refuses, the command is marked handled, so the handler skips it.
/// As in the C, the words are said (the caster and the room see the cast
/// announced) and nothing else happens. The target is the victim for a
/// single-target spell; an area spell or one without a target passes none,
/// as `do_cast` passes no `cvict` for `TAR_IGNORE`.
pub fn cast_gate(world: &mut World, mut state: Local<Option<SystemState<GateParams>>>) {
    let state = state.get_or_insert_with(|| SystemState::new(world));
    let mut pending: Vec<PendingCast> = Vec::new();
    {
        let Ok((
            mut reader,
            handled,
            sessions,
            players,
            npcs,
            hidden,
            content,
            combatants,
            progress,
            _,
        )) = state.get_mut(world)
        else {
            return;
        };
        let empty = TbaContent::default();
        let content = content.as_deref().unwrap_or(&empty);
        for msg in reader.read() {
            if handled
                .as_ref()
                .is_some_and(|handled| handled.is_handled(msg.id))
            {
                continue;
            }
            let playing = sessions
                .get(msg.session)
                .is_ok_and(|session| session.state == SessionState::Playing);
            let PlayerInput::RawLine(line) = &msg.input else {
                continue;
            };
            if !playing {
                continue;
            }
            let CastOutcome::Plan(plan) = plan_cast(
                msg.session,
                line.as_str(),
                content,
                &progress,
                &combatants,
                &players,
                &npcs,
                &hidden,
            ) else {
                continue;
            };
            let Some(spellnum) = spell_number(&plan.spell_id) else {
                continue;
            };
            let single = matches!(
                plan.spell.target,
                crate::content::SpellTarget::Victim | crate::content::SpellTarget::Caster
            );
            pending.push(PendingCast {
                id: msg.id,
                caster: msg.session,
                victim: single.then(|| plan.targets.first().copied()).flatten(),
                spellnum,
                spell_name: plan.spell.name.clone(),
                room: plan.room,
            });
        }
    }
    if pending.is_empty() {
        return;
    }

    let mut refused: Vec<PendingCast> = Vec::new();
    for cast in pending {
        let allowed = with_host(world, |host| {
            let Some(caster) = host.uid(cast.caster) else {
                return true;
            };
            let victim = cast.victim.and_then(|victim| host.uid(victim));
            cast_allowed(host, caster, victim, None, cast.spellnum)
        });
        if !allowed {
            refused.push(cast);
        }
    }
    if refused.is_empty() {
        return;
    }

    if let Some(mut handled) = world.get_resource_mut::<HandledCommands>() {
        for cast in &refused {
            handled.mark_handled(cast.id);
        }
    }
    let Ok((_, _, _, players, npcs, _, _, _, _, mut output)) = state.get_mut(world) else {
        return;
    };
    for cast in &refused {
        output.write(text(cast.caster, format!("You cast {}.", cast.spell_name)));
        if let Some(room) = cast.room {
            let name = players
                .get(cast.caster)
                .map(|(_, player, _)| player.name.clone())
                .or_else(|_| npcs.get(cast.caster).map(|(_, npc, _)| npc.name.clone()))
                .unwrap_or_else(|_| "someone".to_owned());
            broadcast(
                &players,
                room,
                cast.caster,
                &mut output,
                &format!("{name} casts {}.", cast.spell_name),
            );
        }
        mib_stdlib::dialogue::write_prompt(&mut output, cast.caster);
    }
}

/// Whether an entity is a living, un-extracted combatant.
pub fn is_live(world: &World, entity: Entity) -> bool {
    world.get_entity(entity).is_ok()
        && world.get::<DgDead>(entity).is_none()
        && world
            .get::<TbaCombatant>(entity)
            .is_some_and(TbaCombatant::is_alive)
}
