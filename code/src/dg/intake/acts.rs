//! `act` triggers for the lines a player's social shows to mobs.
//!
//! The C's `act()` hands every line it shows an NPC to `act_mtrigger`
//! (`perform_act`, `comm.c`), with the text that NPC sees: the actor's name as
//! `PERS` renders it for that recipient (`someone` when the recipient cannot
//! see the actor), the `$`-codes expanded, and a trailing `\r\n`. The pack's
//! social handler writes its lines to players only, so this module works out
//! the lines a social shows (the same ones `socials::run_social` writes) and
//! offers each to the mobs that would have received it.
//!
//! The pack has no `emote` command, so socials are the only acts a player
//! makes. A `say` is not one: `do_say` acts with `DG_NO_TRIG`.

use dg_vm::dispatch::act_mtrigger;
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::{CarriedBy, Item, Location, Npc, Player};

use crate::combat::{TbaCombatant, TbaProgress};
use crate::content::{Position, SocialDef};
use crate::dg::host::EcsHost;
use crate::dg::with_host;
use crate::module::TbaContent;
use crate::socials::{SocialCommand, SocialNames, gate_reason, substitute};

/// What `perform_act` renders for a recipient who cannot see the actor.
const SOMEONE: &str = "someone";

/// Who a line is shown to: `act`'s `TO_ROOM`, `TO_NOTVICT` and `TO_VICT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Audience {
    /// Everyone in the room but the actor.
    Room,
    /// Everyone in the room but the actor and the victim.
    NotVictim,
    /// The victim alone.
    Victim,
}

/// What a social was aimed at.
struct Target {
    entity: Entity,
    name: String,
    /// Whether the target is an item (resolved by the object fallback).
    is_item: bool,
}

/// The lines one social shows, ready to offer.
struct Lines {
    social: SocialDef,
    actor: Entity,
    room: Entity,
    target: Option<Target>,
    body: Option<String>,
    /// `(template, audience)` for each `act()` call the C makes, in order.
    acts: Vec<(String, Audience)>,
    /// Whether the target (a character) is the line's `vict_obj`.
    victim: Option<Entity>,
    /// Whether the target (an item) is the line's `obj`.
    object: Option<Entity>,
}

/// Offer the lines `command` shows to the mobs in the actor's room.
pub(super) fn social_acts(world: &mut World, actor: Entity, command: &SocialCommand) {
    let SocialCommand::Act { name, target, part } = command else {
        return;
    };
    with_host(world, |host| {
        if let Some(lines) = plan(host, actor, name, target, part) {
            deliver(host, &lines);
        }
    });
}

/// The `act()` calls `do_action` makes for this social, or `None` when it
/// shows nothing to a room (unknown social, a gate refuses it, no target).
/// The branches follow `socials::run_social`, which follows `do_action`.
fn plan(
    host: &mut EcsHost<'_>,
    actor: Entity,
    name: &str,
    needle: &str,
    part: &str,
) -> Option<Lines> {
    let social = host
        .world
        .get_resource::<TbaContent>()?
        .socials
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, social)| social.clone())?;
    let room = host.world.get::<Location>(actor)?.room;
    let target = find_target(host.world, needle, actor, room);
    let victim_position = target.as_ref().map(|t| position_of(host.world, t.entity));
    let level = host
        .world
        .get::<TbaProgress>(actor)
        .map_or(1, |progress| progress.level);
    if gate_reason(
        &social,
        position_of(host.world, actor),
        level,
        victim_position,
    )
    .is_some()
    {
        return None;
    }
    let body = (!part.is_empty()).then(|| part.to_owned());
    let mut lines = Lines {
        social,
        actor,
        room,
        target,
        body,
        acts: Vec::new(),
        victim: None,
        object: None,
    };
    choose_acts(&mut lines, needle)?;
    Some(lines)
}

/// Choose the templates and audiences for the branch of `do_action` taken.
fn choose_acts(lines: &mut Lines, needle: &str) -> Option<()> {
    let social = lines.social.clone();
    let mut acts: Vec<(String, Audience)> = Vec::new();
    let mut push = |text: &Option<String>, audience: Audience| {
        if let Some(text) = text {
            acts.push((text.clone(), audience));
        }
    };
    if needle.trim().is_empty() {
        push(&social.others_no_arg, Audience::Room);
        lines.acts = acts;
        return Some(());
    }
    let (entity, is_item) = lines
        .target
        .as_ref()
        .map(|target| (target.entity, target.is_item))?;
    if entity == lines.actor {
        push(&social.others_auto, Audience::Room);
        lines.acts = acts;
        return Some(());
    }
    if lines.body.is_some() {
        social.char_body_found.as_ref()?;
        push(&social.others_body_found, Audience::NotVictim);
        push(&social.vict_body_found, Audience::Victim);
    } else if !is_item {
        push(&social.others_found, Audience::NotVictim);
        push(&social.vict_found, Audience::Victim);
    } else {
        push(&social.others_obj_found, Audience::Room);
    }
    lines.acts = acts;
    if is_item {
        lines.object = Some(entity);
    } else {
        lines.victim = Some(entity);
    }
    Some(())
}

/// Offer each line, in the order `do_action` acts them, to every mob it
/// reaches, in the room's people order.
fn deliver(host: &mut EcsHost<'_>, lines: &Lines) {
    let Some(actor) = host.uid(lines.actor) else {
        return;
    };
    let Some(room) = host.uid(lines.room) else {
        return;
    };
    let victim = lines.victim.and_then(|entity| host.uid(entity));
    let object = lines.object.and_then(|entity| host.uid(entity));
    let actor_name = host
        .world
        .get::<Player>(lines.actor)
        .map(|player| player.name.clone())
        .unwrap_or_default();
    for (template, audience) in &lines.acts {
        // Every `act()` with text sets `dg_act_check`; an `act` trigger that
        // makes a `say` clears it for the rest of this walk.
        host.vm_state().act_check = true;
        for recipient in host.room_people(room) {
            show(
                host,
                lines,
                (template, *audience),
                (actor, &actor_name),
                (victim, object),
                recipient,
            );
        }
    }
}

/// One line to one recipient, if the recipient is a mob that is shown it.
fn show(
    host: &mut EcsHost<'_>,
    lines: &Lines,
    (template, audience): (&str, Audience),
    (actor, actor_name): (Uid, &str),
    (victim, object): (Option<Uid>, Option<Uid>),
    recipient: Uid,
) {
    let Some(to) = host.entity(recipient) else {
        return;
    };
    // `SENDOK`: a mob that is awake; `to != ch`.
    if host.world.get::<Npc>(to).is_none()
        || to == lines.actor
        || position_of(host.world, to) <= Position::Sleeping
    {
        return;
    }
    let is_victim = Some(to) == lines.victim;
    match audience {
        Audience::NotVictim if is_victim => return,
        Audience::Victim if !is_victim => return,
        _ => {}
    }
    let sees = host.can_see(recipient, actor);
    // `act(..., hide, ...)` skips a recipient who cannot see the actor; the
    // `TO_VICT` branch does not test it.
    if lines.social.hide && audience != Audience::Victim && !sees {
        return;
    }
    let target_name = lines.target.as_ref().map(|target| {
        let visible = target.is_item || victim.is_none_or(|uid| host.can_see(recipient, uid));
        if visible {
            target.name.clone()
        } else {
            SOMEONE.to_owned()
        }
    });
    let names = SocialNames {
        actor: if sees { actor_name } else { SOMEONE },
        target: target_name.as_deref(),
        object: target_name.as_deref().filter(|_| object.is_some()),
        body: lines.body.as_deref(),
    };
    let text = format!("{}\r\n", substitute(template, &names));
    // `dg_victim` is the recipient itself for `TO_VICT`, and the victim when
    // the text names it with `$N`, `$M`, `$S` or `$E`.
    let victim = victim.filter(|_| is_victim || names_victim(template));
    if !host.vm_state().act_check {
        return;
    }
    act_mtrigger(
        host,
        recipient,
        &text,
        Some(actor),
        victim,
        object,
        None,
        None,
    );
}

/// Whether an `act()` template contains a victim code (`$N`, `$M`, `$S`, `$E`).
fn names_victim(template: &str) -> bool {
    let mut chars = template.chars();
    while let Some(ch) = chars.next() {
        if ch == '$' && matches!(chars.next(), Some('N' | 'M' | 'S' | 'E')) {
            return true;
        }
    }
    false
}

/// `GET_POS`, standing for a character with no combat state.
fn position_of(world: &World, entity: Entity) -> Position {
    world
        .get::<TbaCombatant>(entity)
        .map_or(Position::Standing, |combatant| combatant.position)
}

/// Case-insensitive `contains`, the match `socials::find_target` uses.
fn name_matches(needle: &str, name: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    !needle.is_empty() && name.to_lowercase().contains(&needle)
}

/// A social's target as `socials::find_target` resolves it: a player in the
/// room, then an NPC, then an item the actor carries, then one on the floor.
fn find_target(world: &mut World, needle: &str, actor: Entity, room: Entity) -> Option<Target> {
    if needle.trim().is_empty() {
        return None;
    }
    let mut players = world.query::<(Entity, &Player, &Location)>();
    if let Some((entity, player, _)) = players
        .iter(world)
        .find(|(_, player, location)| location.room == room && name_matches(needle, &player.name))
    {
        return Some(Target {
            entity,
            name: player.name.clone(),
            is_item: false,
        });
    }
    let mut npcs = world.query::<(Entity, &Npc, &Location)>();
    if let Some((entity, npc, _)) = npcs
        .iter(world)
        .find(|(_, npc, location)| location.room == room && name_matches(needle, &npc.name))
    {
        return Some(Target {
            entity,
            name: npc.name.clone(),
            is_item: false,
        });
    }
    let mut carried = world.query::<(Entity, &Item, &CarriedBy)>();
    if let Some((entity, item, _)) = carried
        .iter(world)
        .find(|(_, item, holder)| holder.0 == actor && name_matches(needle, &item.name))
    {
        return Some(Target {
            entity,
            name: item.name.clone(),
            is_item: true,
        });
    }
    let mut ground = world.query::<(Entity, &Item, &Location)>();
    ground
        .iter(world)
        .find(|(_, item, location)| location.room == room && name_matches(needle, &item.name))
        .map(|(entity, item, _)| Target {
            entity,
            name: item.name.clone(),
            is_item: true,
        })
}
