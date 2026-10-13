//! `give`: objects and coins, from a player to a player or to an NPC
//! (`do_give`, `perform_give` and `perform_give_gold` in `act.item.c`).
//!
//! The engine's `give` handles an object between players only, with a
//! different grammar (`give <item> to <player>`). The C's grammar, the NPC
//! receivers and the coins live here, so one command carries the C's triggers:
//! `give_otrigger`, then `receive_mtrigger`, and `bribe_mtrigger` once coins
//! have changed hands.

use dg_vm::Uid;
use dg_vm::dispatch::{bribe_mtrigger, give_otrigger, receive_mtrigger};
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    CarriedBy, ContainedBy, GroundItem, Item, Location, Npc, Player, QuestEvent,
};

use super::common::{
    Dots, article, atoi, carrying, find_all_dots, find_char_in_room, find_obj, is_number,
    matches_after, money_desc, names_thing, one_argument, split_number, tell, tell_room,
};
use super::gate::{has_extra_flag, item_name};
use crate::combat::TbaGold;
use crate::content::ItemExtraFlag;
use crate::dg::host::EcsHost;
use dg_vm::WorldHost;

/// The coins a non-player character carries.
///
/// The pack models a purse for players only (`TbaGold`); this holds what a
/// player gives an NPC, so `give <n> coins <mob>` and the `bribe` trigger have
/// something to move.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Component)]
pub struct DgNpcGold(pub u32);

/// What a `give` command hands over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gift {
    /// A named object: one, or `count` of them when the player gave a number.
    Objects { name: String, count: Option<i32> },
    /// `all` and `all.name`.
    Every(Dots),
}

/// `do_give`, parsing the C's grammar from the text after the verb.
pub fn do_give(host: &mut EcsHost<'_>, ch: Entity, args: &str) {
    let (arg, rest) = one_argument(args);
    if arg.is_empty() {
        tell(host.world, ch, "Give what to who?");
        return;
    }
    if is_number(&arg) {
        let amount = atoi(&arg);
        let (what, rest) = one_argument(rest);
        if what == "coins" || what == "coin" {
            let (victim, _) = one_argument(rest);
            if let Some(victim) = find_victim(host, ch, &victim) {
                give_gold(host, ch, victim, amount);
            }
        } else if what.is_empty() {
            tell(
                host.world,
                ch,
                &format!("What do you want to give {amount} of?"),
            );
        } else if let Some(victim) = find_victim(host, ch, rest) {
            give_objects(
                host,
                ch,
                victim,
                &Gift::Objects {
                    name: what,
                    count: Some(amount),
                },
            );
        }
        return;
    }
    let (victim_word, _) = one_argument(rest);
    let Some(victim) = find_victim(host, ch, &victim_word) else {
        return;
    };
    let gift = match find_all_dots(&arg) {
        Dots::Indiv => Gift::Objects {
            name: arg,
            count: None,
        },
        every => Gift::Every(every),
    };
    give_objects(host, ch, victim, &gift);
}

/// The structured form of the engine's `Command::Give`: an object phrase and a
/// receiver.
pub fn do_give_typed(host: &mut EcsHost<'_>, ch: Entity, item: &str, to: &str) {
    let (first, rest) = one_argument(item);
    let (second, _) = one_argument(rest);
    if is_number(&first) && (second == "coins" || second == "coin") {
        if let Some(victim) = find_victim(host, ch, to) {
            give_gold(host, ch, victim, atoi(&first));
        }
        return;
    }
    let Some(victim) = find_victim(host, ch, to) else {
        return;
    };
    give_objects(
        host,
        ch,
        victim,
        &Gift::Objects {
            name: item.trim().to_lowercase(),
            count: None,
        },
    );
}

/// `give_find_vict`.
fn find_victim(host: &mut EcsHost<'_>, ch: Entity, typed: &str) -> Option<Entity> {
    let typed = typed.trim();
    if typed.is_empty() {
        tell(host.world, ch, "To who?");
        return None;
    }
    let Some(victim) = find_char_in_room(host, ch, typed) else {
        tell(host.world, ch, "No one by that name here.");
        return None;
    };
    if victim == ch {
        tell(host.world, ch, "What's the point of that?");
        return None;
    }
    Some(victim)
}

/// The object half of `do_give`, once the receiver is known.
fn give_objects(host: &mut EcsHost<'_>, ch: Entity, victim: Entity, gift: &Gift) {
    let list = carrying(host, ch);
    match gift {
        Gift::Objects { name, count } => {
            let Some(first) = find_obj(host, &list, name) else {
                let message = if count.is_none() {
                    format!("You don't seem to have {} {name}.", article(name))
                } else {
                    format!("You don't seem to have any {name}s.")
                };
                tell(host.world, ch, &message);
                return;
            };
            // `N.name` is read once; the walk on from the first match uses the
            // bare name.
            let (_, bare) = split_number(name);
            let mut chain = vec![first];
            chain.extend(matches_after(host, &list, first, bare));
            // `while (obj && amount--)`: a negative count never runs out.
            let limit = match count {
                None => 1,
                Some(count) if *count < 0 => usize::MAX,
                Some(count) => usize::try_from(*count).unwrap_or(0),
            };
            for obj in chain.into_iter().take(limit) {
                perform_give(host, ch, victim, obj);
            }
        }
        Gift::Every(mode) => {
            if let Dots::AllDot(name) = mode
                && name.is_empty()
            {
                tell(host.world, ch, "All of what?");
                return;
            }
            if list.is_empty() {
                tell(host.world, ch, "You don't seem to be holding anything.");
                return;
            }
            for obj in list.iter().copied() {
                let wanted = match mode {
                    Dots::AllDot(name) => names_thing(name, &host.name(obj), &host.keywords(obj)),
                    _ => true,
                };
                if wanted {
                    perform_give(host, ch, victim, obj);
                }
            }
        }
    }
}

/// `perform_give`.
fn perform_give(host: &mut EcsHost<'_>, ch: Entity, victim: Entity, obj: Uid) {
    let (Some(ch_uid), Some(victim_uid)) = (host.uid(ch), host.uid(victim)) else {
        return;
    };
    if give_otrigger(host, obj, ch_uid, victim_uid) == 0 {
        return;
    }
    if receive_mtrigger(host, victim_uid, ch_uid, obj) == 0 {
        return;
    }
    // A script may have moved or destroyed either side.
    let Some(item) = host.entity(obj) else {
        return;
    };
    if host.world.get::<CarriedBy>(item).map(|carried| carried.0) != Some(ch)
        || host.entity(victim_uid).is_none()
    {
        return;
    }
    let name = item_name(host, item);
    if has_extra_flag(host, item, ItemExtraFlag::NoDrop) {
        tell(
            host.world,
            ch,
            &format!("You can't let go of {name}!!  Yeech!"),
        );
        return;
    }
    // Hands-full and weight limits are not modelled by the pack.
    host.world
        .entity_mut(item)
        .remove::<(Location, GroundItem, ContainedBy)>()
        .insert(CarriedBy(victim));
    host.stamp_order(item);

    let giver = host.name(ch_uid);
    let receiver = host.name(victim_uid);
    tell(host.world, ch, &format!("You give {name} to {receiver}."));
    tell(host.world, victim, &format!("{giver} gives you {name}."));
    tell_room(
        host.world,
        ch,
        &format!("{giver} gives {name} to {receiver}."),
        &[victim],
    );
    // The same event the engine's `give` raises for the player who now holds it.
    if host.world.get::<Player>(victim).is_some()
        && let Some(def_id) = host.world.get::<Item>(item).map(|item| item.def_id.clone())
        && let Some(mut events) = host.world.get_resource_mut::<Messages<QuestEvent>>()
    {
        events.write(QuestEvent::Collected {
            player: victim,
            item: def_id,
        });
    }
}

/// The coins a character holds.
pub fn gold_of(world: &World, ch: Entity) -> u32 {
    if world.get::<Npc>(ch).is_some() {
        world.get::<DgNpcGold>(ch).map_or(0, |gold| gold.0)
    } else {
        world.get::<TbaGold>(ch).map_or(0, |gold| gold.0)
    }
}

/// Set the coins a character holds.
fn set_gold(world: &mut World, ch: Entity, amount: u32) {
    if world.get::<Npc>(ch).is_some() {
        world.entity_mut(ch).insert(DgNpcGold(amount));
    } else if world.get::<Player>(ch).is_some() {
        world.entity_mut(ch).insert(TbaGold(amount));
    }
}

/// `perform_give_gold`.
fn give_gold(host: &mut EcsHost<'_>, ch: Entity, victim: Entity, amount: i32) {
    if amount <= 0 {
        tell(
            host.world,
            ch,
            "Heh heh heh ... we are jolly funny today, eh?",
        );
        return;
    }
    let held = gold_of(host.world, ch);
    let Ok(coins) = u32::try_from(amount) else {
        return;
    };
    if held < coins {
        tell(host.world, ch, "You don't have that many coins!");
        return;
    }
    tell(host.world, ch, "Okay.");
    let (Some(ch_uid), Some(victim_uid)) = (host.uid(ch), host.uid(victim)) else {
        return;
    };
    let giver = host.name(ch_uid);
    let receiver = host.name(victim_uid);
    let plural = if amount == 1 { "" } else { "s" };
    tell(
        host.world,
        victim,
        &format!("{giver} gives you {amount} gold coin{plural}."),
    );
    tell_room(
        host.world,
        ch,
        &format!("{giver} gives {} to {receiver}.", money_desc(amount)),
        &[victim],
    );
    set_gold(host.world, ch, held.saturating_sub(coins));
    let received = gold_of(host.world, victim).saturating_add(coins);
    set_gold(host.world, victim, received);
    bribe_mtrigger(host, victim_uid, ch_uid, amount);
}
