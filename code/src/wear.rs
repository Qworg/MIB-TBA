//! `wear`, `wield`, `hold` and `remove`: the CircleMUD verbs for the pack's
//! equipment model (`do_wear`, `do_wield`, `do_grab`, `do_remove` and their
//! `perform_*` helpers in `act.item.c`).
//!
//! They work on the same state as `equip` / `unequip` ([`TbaEquipment`] and its
//! persisted shadow), so the two vocabularies can be mixed. Each change runs the
//! object's `wear` or `remove` trigger first, as the C does.
//!
//! The pack does not model carrying capacity, so the C's "hands full" and
//! weight refusals do not exist here.

use dg_vm::dispatch::{remove_otrigger, wear_otrigger};
use dg_vm::{Uid, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::{CarriedBy, Item};

use super::common::{
    Dots, article, carrying, find_all_dots, find_obj, matches_after, names_thing, one_argument,
    pick, split_number, tell, tell_room,
};
use super::gate::{has_extra_flag, item_name};
use crate::combat::{BASE_ATTRIBUTE, TbaCombatant, TbaProgress};
use crate::content::{ItemExtraFlag, TbaItemKind, WearSlot};
use crate::dg::host::{EcsHost, item_wear_bit, wear_position};
use crate::equipment::{TbaEquipment, clear_worn, set_worn};
use crate::module::TbaContent;

/// The positions `find_eq_pos` knows by name, in the C's order, with the
/// position each names.
const POSITION_WORDS: [(&str, WearSlot); 12] = [
    ("finger", WearSlot::FingerR),
    ("neck", WearSlot::Neck1),
    ("body", WearSlot::Body),
    ("head", WearSlot::Head),
    ("legs", WearSlot::Legs),
    ("feet", WearSlot::Feet),
    ("hands", WearSlot::Hands),
    ("arms", WearSlot::Arms),
    ("shield", WearSlot::Shield),
    ("about", WearSlot::About),
    ("waist", WearSlot::Waist),
    ("wrist", WearSlot::WristR),
];

/// `str_app[].wield_w` for strength `0..=25`: the heaviest weapon that can be
/// wielded.
const WIELD_WEIGHT: [u32; 26] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 40, 40, 40, 40, 40, 40, 40,
];

/// `already_wearing[]`: what the C says when the position is taken.
fn already_wearing(slot: WearSlot) -> &'static str {
    match slot {
        WearSlot::Light => "You're already using a light.",
        WearSlot::FingerR | WearSlot::FingerL => {
            "You're already wearing something on both of your ring fingers."
        }
        WearSlot::Neck1 | WearSlot::Neck2 => "You can't wear anything else around your neck.",
        WearSlot::Body => "You're already wearing something on your body.",
        WearSlot::Head => "You're already wearing something on your head.",
        WearSlot::Legs => "You're already wearing something on your legs.",
        WearSlot::Feet => "You're already wearing something on your feet.",
        WearSlot::Hands => "You're already wearing something on your hands.",
        WearSlot::Arms => "You're already wearing something on your arms.",
        WearSlot::Shield => "You're already using a shield.",
        WearSlot::About => "You're already wearing something about your body.",
        WearSlot::Waist => "You already have something around your waist.",
        WearSlot::WristR | WearSlot::WristL => {
            "You're already wearing something around both of your wrists."
        }
        WearSlot::Wield => "You're already wielding a weapon.",
        WearSlot::Hold => "You're already holding something.",
    }
}

/// `wear_message[]`: what the wearer sees and what the room sees, with `$n`
/// the wearer and `$p` the object.
fn wear_message(slot: WearSlot, who: &str, what: &str) -> (String, String) {
    let (room, you) = match slot {
        WearSlot::Light => ("lights {p} and holds it.", "You light {p} and hold it."),
        WearSlot::FingerR => (
            "slides {p} on to {s} right ring finger.",
            "You slide {p} on to your right ring finger.",
        ),
        WearSlot::FingerL => (
            "slides {p} on to {s} left ring finger.",
            "You slide {p} on to your left ring finger.",
        ),
        WearSlot::Neck1 | WearSlot::Neck2 => (
            "wears {p} around {s} neck.",
            "You wear {p} around your neck.",
        ),
        WearSlot::Body => ("wears {p} on {s} body.", "You wear {p} on your body."),
        WearSlot::Head => ("wears {p} on {s} head.", "You wear {p} on your head."),
        WearSlot::Legs => ("puts {p} on {s} legs.", "You put {p} on your legs."),
        WearSlot::Feet => ("wears {p} on {s} feet.", "You wear {p} on your feet."),
        WearSlot::Hands => ("puts {p} on {s} hands.", "You put {p} on your hands."),
        WearSlot::Arms => ("wears {p} on {s} arms.", "You wear {p} on your arms."),
        WearSlot::Shield => (
            "straps {p} around {s} arm as a shield.",
            "You start to use {p} as a shield.",
        ),
        WearSlot::About => (
            "wears {p} about {s} body.",
            "You wear {p} around your body.",
        ),
        WearSlot::Waist => (
            "wears {p} around {s} waist.",
            "You wear {p} around your waist.",
        ),
        WearSlot::WristR => (
            "puts {p} on around {s} right wrist.",
            "You put {p} on around your right wrist.",
        ),
        WearSlot::WristL => (
            "puts {p} on around {s} left wrist.",
            "You put {p} on around your left wrist.",
        ),
        WearSlot::Wield => ("wields {p}.", "You wield {p}."),
        WearSlot::Hold => ("grabs {p}.", "You grab {p}."),
    };
    // The pack keeps no sex, so "$s" reads as the neutral "their".
    let fill = |text: &str| text.replace("{p}", what).replace("{s}", "their");
    (format!("{who} {}", fill(room)), fill(you))
}

/// `CAN_WEAR(obj, bit)`.
fn can_wear(host: &EcsHost<'_>, obj: Uid, bit: u32) -> bool {
    host.wear_flags(obj) & (1u32 << bit) != 0
}

/// The `ITEM_WEAR_*` bit a position needs on the object: any object can be
/// held or used as a light (`ITEM_WEAR_TAKE`).
fn required_bit(slot: WearSlot) -> u32 {
    match slot {
        WearSlot::Light | WearSlot::Hold => 0,
        other => item_wear_bit(other),
    }
}

/// The prototype definition of an object.
fn def_of<'a>(host: &'a EcsHost<'_>, obj: Entity) -> Option<&'a crate::content::TbaItemDef> {
    let def_id = &host.world.get::<Item>(obj)?.def_id;
    host.world.get_resource::<TbaContent>()?.items.get(def_id)
}

/// `GET_LEVEL(ch) < GET_OBJ_LEVEL(obj)`.
fn too_inexperienced(host: &EcsHost<'_>, ch: Entity, obj: Uid) -> bool {
    let Some(entity) = host.entity(obj) else {
        return false;
    };
    let needed = def_of(host, entity).map_or(0, |def| def.min_level);
    let level = host
        .world
        .get::<TbaProgress>(ch)
        .map_or(0, |progress| progress.level);
    level < needed
}

/// `find_eq_pos`: the position a `wear` argument names for the object, or
/// `None` (after saying so when the argument was no body part).
fn find_eq_pos(host: &mut EcsHost<'_>, ch: Entity, obj: Uid, arg: &str) -> Option<WearSlot> {
    if arg.is_empty() {
        let mut found = None;
        for (_, slot) in POSITION_WORDS {
            if can_wear(host, obj, item_wear_bit(slot)) {
                found = Some(slot);
            }
        }
        return found;
    }
    let slot = if arg.starts_with('!') {
        None
    } else {
        POSITION_WORDS
            .iter()
            .find(|(word, _)| word.starts_with(arg))
            .map(|(_, slot)| *slot)
    };
    if slot.is_none() {
        tell(
            host.world,
            ch,
            &format!("'{arg}'?  What part of your body is THAT?"),
        );
    }
    slot
}

/// `perform_wear`.
fn perform_wear(host: &mut EcsHost<'_>, ch: Entity, obj: Uid, mut slot: WearSlot) {
    let Some(entity) = host.entity(obj) else {
        return;
    };
    if !can_wear(host, obj, required_bit(slot)) {
        let name = item_name(host, entity);
        tell(host.world, ch, &format!("You can't wear {name} there."));
        return;
    }
    let worn = |host: &EcsHost<'_>, slot: WearSlot| {
        host.world
            .get::<TbaEquipment>(ch)
            .and_then(|equipment| equipment.worn(slot))
    };
    // For a ring, a necklace or a bracelet, the second position when the
    // first is full.
    if matches!(slot, WearSlot::FingerR | WearSlot::Neck1 | WearSlot::WristR)
        && worn(host, slot).is_some()
        && let Some(next) = slot.partner()
    {
        slot = next;
    }
    if worn(host, slot).is_some() {
        tell(host.world, ch, already_wearing(slot));
        return;
    }
    let Some(ch_uid) = host.uid(ch) else {
        return;
    };
    if wear_otrigger(host, obj, ch_uid, wear_position(slot)) == 0
        || host.world.get::<CarriedBy>(entity).map(|carried| carried.0) != Some(ch)
        || host.entity(obj).is_none()
    {
        return;
    }
    let who = host.name(ch_uid);
    let name = item_name(host, entity);
    let (room, you) = wear_message(slot, &who, &name);
    tell_room(host.world, ch, &room, &[]);
    tell(host.world, ch, &you);
    set_worn(host.world, ch, slot, entity);
    host.stamp_order(entity);
}

/// `do_wear`.
pub fn do_wear(host: &mut EcsHost<'_>, ch: Entity, args: &str) {
    let (arg1, rest) = one_argument(args);
    let (arg2, _) = one_argument(rest);
    if arg1.is_empty() {
        tell(host.world, ch, "Wear what?");
        return;
    }
    let mode = find_all_dots(&arg1);
    if !arg2.is_empty() && mode != Dots::Indiv {
        tell(
            host.world,
            ch,
            "You can't specify the same body location for more than one item!",
        );
        return;
    }
    let list = carrying(host, ch);
    match mode {
        Dots::All => {
            let mut worn = 0;
            for obj in list {
                let Some(slot) = find_eq_pos(host, ch, obj, "") else {
                    continue;
                };
                if host.entity(obj).is_none() {
                    continue;
                }
                if too_inexperienced(host, ch, obj) {
                    tell(
                        host.world,
                        ch,
                        "You are not experienced enough to use that.",
                    );
                } else {
                    worn += 1;
                    perform_wear(host, ch, obj, slot);
                }
            }
            if worn == 0 {
                tell(host.world, ch, "You don't seem to have anything wearable.");
            }
        }
        Dots::AllDot(name) => {
            if name.is_empty() {
                tell(host.world, ch, "Wear all of what?");
                return;
            }
            let Some(first) = find_obj(host, &list, &name) else {
                tell(
                    host.world,
                    ch,
                    &format!("You don't seem to have any {name}s."),
                );
                return;
            };
            if too_inexperienced(host, ch, first) {
                tell(
                    host.world,
                    ch,
                    "You are not experienced enough to use that.",
                );
                return;
            }
            let (_, bare) = split_number(&name);
            let mut chain = vec![first];
            chain.extend(matches_after(host, &list, first, bare));
            for obj in chain {
                if host.entity(obj).is_none() {
                    continue;
                }
                match find_eq_pos(host, ch, obj, "") {
                    Some(slot) => perform_wear(host, ch, obj, slot),
                    None => {
                        let name = host.name(obj);
                        tell(host.world, ch, &format!("You can't wear {name}."));
                    }
                }
            }
        }
        Dots::Indiv => {
            let Some(obj) = find_obj(host, &list, &arg1) else {
                tell(
                    host.world,
                    ch,
                    &format!("You don't seem to have {} {arg1}.", article(&arg1)),
                );
                return;
            };
            if too_inexperienced(host, ch, obj) {
                tell(
                    host.world,
                    ch,
                    "You are not experienced enough to use that.",
                );
                return;
            }
            match find_eq_pos(host, ch, obj, &arg2) {
                Some(slot) => perform_wear(host, ch, obj, slot),
                None if arg2.is_empty() => {
                    let name = host.name(obj);
                    tell(host.world, ch, &format!("You can't wear {name}."));
                }
                None => {}
            }
        }
    }
}

/// The heaviest weapon a character can wield (`str_app[].wield_w`).
fn wield_limit(host: &EcsHost<'_>, ch: Entity) -> u32 {
    let strength = host
        .world
        .get::<TbaCombatant>(ch)
        .map_or(BASE_ATTRIBUTE, |combatant| combatant.attributes.strength);
    let index = usize::try_from(strength.clamp(0, 25)).unwrap_or(0);
    WIELD_WEIGHT.get(index).copied().unwrap_or(0)
}

/// `do_wield`.
pub fn do_wield(host: &mut EcsHost<'_>, ch: Entity, args: &str) {
    let (arg, _) = one_argument(args);
    if arg.is_empty() {
        tell(host.world, ch, "Wield what?");
        return;
    }
    let list = carrying(host, ch);
    let Some(obj) = find_obj(host, &list, &arg) else {
        tell(
            host.world,
            ch,
            &format!("You don't seem to have {} {arg}.", article(&arg)),
        );
        return;
    };
    let weight = host
        .entity(obj)
        .and_then(|entity| def_of(host, entity))
        .map_or(0, |def| def.weight);
    if !can_wear(host, obj, item_wear_bit(WearSlot::Wield)) {
        tell(host.world, ch, "You can't wield that.");
    } else if weight > wield_limit(host, ch) {
        tell(host.world, ch, "It's too heavy for you to use.");
    } else if too_inexperienced(host, ch, obj) {
        tell(
            host.world,
            ch,
            "You are not experienced enough to use that.",
        );
    } else {
        perform_wear(host, ch, obj, WearSlot::Wield);
    }
}

/// `do_grab`, the `hold` command.
pub fn do_hold(host: &mut EcsHost<'_>, ch: Entity, args: &str) {
    let (arg, _) = one_argument(args);
    if arg.is_empty() {
        tell(host.world, ch, "Hold what?");
        return;
    }
    let list = carrying(host, ch);
    let Some(obj) = find_obj(host, &list, &arg) else {
        tell(
            host.world,
            ch,
            &format!("You don't seem to have {} {arg}.", article(&arg)),
        );
        return;
    };
    if too_inexperienced(host, ch, obj) {
        tell(
            host.world,
            ch,
            "You are not experienced enough to use that.",
        );
        return;
    }
    let kind = host
        .entity(obj)
        .and_then(|entity| def_of(host, entity))
        .map(|def| def.kind);
    if kind == Some(TbaItemKind::Light) {
        perform_wear(host, ch, obj, WearSlot::Light);
    } else if !can_wear(host, obj, item_wear_bit(WearSlot::Hold))
        && !matches!(
            kind,
            Some(
                TbaItemKind::Wand | TbaItemKind::Staff | TbaItemKind::Scroll | TbaItemKind::Potion
            )
        )
    {
        tell(host.world, ch, "You can't hold that.");
    } else {
        perform_wear(host, ch, obj, WearSlot::Hold);
    }
}

/// What a character wears, in position order.
fn equipment_of(host: &EcsHost<'_>, ch: Entity) -> Vec<(WearSlot, Uid)> {
    let mut worn: Vec<(WearSlot, Uid)> = host
        .world
        .get::<TbaEquipment>(ch)
        .map(|equipment| {
            equipment
                .slots
                .iter()
                .filter_map(|(slot, entity)| Some((*slot, host.uid(*entity)?)))
                .collect()
        })
        .unwrap_or_default();
    worn.sort_by_key(|(slot, _)| wear_position(*slot));
    worn
}

/// `perform_remove`.
fn perform_remove(host: &mut EcsHost<'_>, ch: Entity, slot: WearSlot) {
    let Some(item) = host
        .world
        .get::<TbaEquipment>(ch)
        .and_then(|equipment| equipment.worn(slot))
    else {
        return;
    };
    let (Some(obj), Some(ch_uid)) = (host.uid(item), host.uid(ch)) else {
        return;
    };
    let name = item_name(host, item);
    if has_extra_flag(host, item, ItemExtraFlag::NoDrop) {
        tell(
            host.world,
            ch,
            &format!("You can't remove {name}, it must be CURSED!"),
        );
        return;
    }
    if remove_otrigger(host, obj, ch_uid) == 0 {
        return;
    }
    // The script may have taken it off or destroyed it.
    if host
        .world
        .get::<TbaEquipment>(ch)
        .and_then(|equipment| equipment.worn(slot))
        != Some(item)
    {
        return;
    }
    clear_worn(host.world, ch, slot);
    host.stamp_order(item);
    let who = host.name(ch_uid);
    tell(host.world, ch, &format!("You stop using {name}."));
    tell_room(host.world, ch, &format!("{who} stops using {name}."), &[]);
}

/// `do_remove`.
pub fn do_remove(host: &mut EcsHost<'_>, ch: Entity, args: &str) {
    let (arg, _) = one_argument(args);
    if arg.is_empty() {
        tell(host.world, ch, "Remove what?");
        return;
    }
    let worn = equipment_of(host, ch);
    match find_all_dots(&arg) {
        Dots::All => {
            if worn.is_empty() {
                tell(host.world, ch, "You're not using anything.");
            }
            for (slot, _) in worn {
                perform_remove(host, ch, slot);
            }
        }
        Dots::AllDot(name) => {
            if name.is_empty() {
                tell(host.world, ch, "Remove all of what?");
                return;
            }
            let mut found = false;
            for (slot, obj) in worn {
                if names_thing(&name, &host.name(obj), &host.keywords(obj)) {
                    perform_remove(host, ch, slot);
                    found = true;
                }
            }
            if !found {
                tell(
                    host.world,
                    ch,
                    &format!("You don't seem to be using any {name}s."),
                );
            }
        }
        Dots::Indiv => {
            let (number, name) = split_number(&arg);
            let found = pick(worn.iter().copied(), number, |(_, obj)| {
                names_thing(name, &host.name(obj), &host.keywords(obj))
            });
            match found {
                Some((slot, _)) => perform_remove(host, ch, slot),
                None => tell(
                    host.world,
                    ch,
                    &format!("You don't seem to be using {} {arg}.", article(&arg)),
                ),
            }
        }
    }
}
