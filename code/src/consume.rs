//! `eat`, `taste`, `drink`, `sip` and `quaff` (`do_eat` and `do_drink` in
//! `act.item.c`, `do_use` in `act.other.c` and the potion case of
//! `mag_objectmagic`), each running the object's `consume` trigger before
//! anything is used up.
//!
//! The pack models neither hunger, thirst and drunkenness nor the per-object
//! values a drink container or a nibbled food would change, so those parts of
//! the C are left out and what is left out is named at the place:
//! eating a whole food, quaffing a potion and drinking from a container work,
//! and a limited drink container is not emptied by drinking.

use dg_vm::dispatch::consume_otrigger;
use dg_vm::types::{OCMD_DRINK, OCMD_EAT, OCMD_QUAFF};
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::content::Sector;
use mib_stdlib::prelude::{Item, Location, Room};

use super::common::{
    article, carrying, find_obj, names_thing, pick, split_number, tell, tell_room,
};
use crate::content::{TbaItemDef, TbaItemKind, WearSlot};
use crate::dg::host::EcsHost;
use crate::dg::waits::DgWaits;
use crate::equipment::{TbaEquipment, forget_worn};
use crate::module::TbaContent;

/// `drinks[]`: what a drink container holds, by liquid number.
const LIQUIDS: [&str; 16] = [
    "water",
    "beer",
    "wine",
    "ale",
    "dark ale",
    "whisky",
    "lemonade",
    "firebreather",
    "local speciality",
    "slime mold juice",
    "milk",
    "tea",
    "coffee",
    "blood",
    "salt water",
    "clear water",
];

/// The prototype definition of an object.
fn def_of(host: &EcsHost<'_>, obj: Uid) -> Option<TbaItemDef> {
    let entity = host.entity(obj)?;
    let def_id = host.world.get::<Item>(entity)?.def_id.clone();
    host.world
        .get_resource::<TbaContent>()?
        .items
        .get(&def_id)
        .cloned()
}

/// `extract_obj` for a consumed object: out of any wearer's equipment, its
/// waits cancelled, and gone.
fn extract_obj(host: &mut EcsHost<'_>, obj: Uid) {
    let Some(entity) = host.entity(obj) else {
        return;
    };
    forget_worn(host.world, entity);
    if let Some(mut waits) = host.world.get_resource_mut::<DgWaits>() {
        waits.cancel_owner(obj);
    }
    host.world.despawn(entity);
}

/// `do_eat`; `taste` nibbles instead.
pub fn do_eat(host: &mut EcsHost<'_>, ch: Entity, args: &str, taste: bool) {
    let (arg, _) = super::common::one_argument(args);
    if arg.is_empty() {
        tell(host.world, ch, "Eat what?");
        return;
    }
    let list = carrying(host, ch);
    let Some(food) = find_obj(host, &list, &arg) else {
        tell(
            host.world,
            ch,
            &format!("You don't seem to have {} {arg}.", article(&arg)),
        );
        return;
    };
    let kind = def_of(host, food).map(|def| def.kind);
    if taste
        && matches!(
            kind,
            Some(TbaItemKind::DrinkContainer | TbaItemKind::Fountain)
        )
    {
        do_drink(host, ch, args, true);
        return;
    }
    if kind != Some(TbaItemKind::Food) {
        tell(host.world, ch, "You can't eat THAT!");
        return;
    }
    let Some(ch_uid) = host.uid(ch) else {
        return;
    };
    if consume_otrigger(host, food, ch_uid, OCMD_EAT) == 0 {
        return;
    }
    let name = host.name(food);
    let who = host.name(ch_uid);
    if taste {
        tell(
            host.world,
            ch,
            &format!("You nibble a little bit of {name}."),
        );
        tell_room(
            host.world,
            ch,
            &format!("{who} tastes a little bit of {name}."),
            &[],
        );
    } else {
        tell(host.world, ch, &format!("You eat {name}."));
        tell_room(host.world, ch, &format!("{who} eats {name}."), &[]);
    }
    // Hunger is not modelled.
    let poisoned = def_of(host, food).is_some_and(|def| def.values[3] != 0);
    if poisoned {
        tell(host.world, ch, "Oops, that tasted rather strange!");
        tell_room(
            host.world,
            ch,
            &format!("{who} coughs and utters some strange sounds."),
            &[],
        );
        host.unsupported("food_poison", "the pack has no poison affect to apply");
    }
    if taste {
        // A nibble takes one bite off the food's first value, which the pack
        // keeps per prototype only: the food stays as it was.
        host.unsupported("food_bites", "the pack keeps no per-object values");
    } else {
        extract_obj(host, food);
    }
}

/// The liquid a container holds (`DRINK_CON_LIQ`).
fn liquid(def: &TbaItemDef) -> &'static str {
    usize::try_from(def.values[2])
        .ok()
        .and_then(|index| LIQUIDS.get(index))
        .copied()
        .unwrap_or("water")
}

/// `do_drink`; `sip` takes a taste instead of a drink.
pub fn do_drink(host: &mut EcsHost<'_>, ch: Entity, args: &str, sip: bool) {
    let (arg, _) = super::common::one_argument(args);
    let Some(ch_uid) = host.uid(ch) else {
        return;
    };
    let who = host.name(ch_uid);
    if arg.is_empty() {
        let in_water = host
            .world
            .get::<Location>(ch)
            .and_then(|location| host.world.get::<Room>(location.room))
            .is_some_and(|room| {
                matches!(
                    room.sector,
                    Sector::WaterSwim | Sector::WaterNoSwim | Sector::Underwater
                )
            });
        if in_water {
            tell_room(
                host.world,
                ch,
                &format!("{who} takes a refreshing drink."),
                &[],
            );
            tell(host.world, ch, "You take a refreshing drink.");
        } else {
            tell(host.world, ch, "Drink from what?");
        }
        return;
    }
    // One count runs through the carried objects and then the room's.
    let (number, name) = split_number(&arg);
    let carried = carrying(host, ch);
    let lying = lying_in_room(host, ch);
    let all: Vec<(Uid, bool)> = carried
        .into_iter()
        .map(|uid| (uid, false))
        .chain(lying.into_iter().map(|uid| (uid, true)))
        .collect();
    let Some((temp, on_ground)) = pick(all.iter().copied(), number, |(uid, _)| {
        names_thing(name, &host.name(uid), &host.keywords(uid))
    }) else {
        tell(host.world, ch, "You can't find it!");
        return;
    };
    let Some(def) = def_of(host, temp) else {
        tell(host.world, ch, "You can't drink from that!");
        return;
    };
    if !matches!(
        def.kind,
        TbaItemKind::DrinkContainer | TbaItemKind::Fountain
    ) {
        tell(host.world, ch, "You can't drink from that!");
        return;
    }
    if on_ground && def.kind == TbaItemKind::DrinkContainer {
        tell(
            host.world,
            ch,
            "You have to be holding that to drink from it.",
        );
        return;
    }
    let limited = def.values[0] >= 0 && def.values[1] >= 0;
    if limited && def.values[1] < 1 {
        tell(host.world, ch, "It is empty.");
        return;
    }
    if consume_otrigger(host, temp, ch_uid, OCMD_DRINK) == 0 {
        return;
    }
    let liquid = liquid(&def);
    let name = host.name(temp);
    if sip {
        tell_room(host.world, ch, &format!("{who} sips from {name}."), &[]);
        tell(host.world, ch, &format!("It tastes like {liquid}."));
    } else {
        tell_room(
            host.world,
            ch,
            &format!("{who} drinks {liquid} from {name}."),
            &[],
        );
        tell(host.world, ch, &format!("You drink the {liquid}."));
    }
    // Thirst and drunkenness are not modelled.
    if def.values[3] != 0 {
        tell(host.world, ch, "Oops, it tasted rather strange!");
        tell_room(
            host.world,
            ch,
            &format!("{who} chokes and utters some strange sounds."),
            &[],
        );
        host.unsupported("drink_poison", "the pack has no poison affect to apply");
    }
    if limited {
        // The amount left is a per-object value the pack does not keep, so a
        // limited container is not emptied by drinking.
        host.unsupported("drink_level", "the pack keeps no per-object values");
    }
}

/// The objects lying in the character's room, oldest first.
fn lying_in_room(host: &EcsHost<'_>, ch: Entity) -> Vec<Uid> {
    let Some(room) = host
        .world
        .get::<Location>(ch)
        .and_then(|location| host.uid(location.room))
    else {
        return Vec::new();
    };
    host.room_contents(room)
}

/// `do_use` with `quaff`, and the potion case of `mag_objectmagic`.
pub fn do_quaff(host: &mut EcsHost<'_>, ch: Entity, args: &str) {
    let arg = args.split_whitespace().next().unwrap_or("").to_lowercase();
    if arg.is_empty() {
        tell(host.world, ch, "What do you want to quaff?");
        return;
    }
    // The held object comes first; otherwise the carried ones.
    let held = host
        .world
        .get::<TbaEquipment>(ch)
        .and_then(|equipment| equipment.worn(WearSlot::Hold))
        .and_then(|entity| host.uid(entity))
        .filter(|uid| super::common::is_name(&arg, &host.keywords(*uid)));
    let potion = match held {
        Some(uid) => uid,
        None => {
            let list = carrying(host, ch);
            let Some(uid) = find_obj(host, &list, &arg) else {
                tell(
                    host.world,
                    ch,
                    &format!("You don't seem to have {} {arg}.", article(&arg)),
                );
                return;
            };
            uid
        }
    };
    if def_of(host, potion).map(|def| def.kind) != Some(TbaItemKind::Potion) {
        tell(host.world, ch, "You can only quaff potions.");
        return;
    }
    let Some(ch_uid) = host.uid(ch) else {
        return;
    };
    if consume_otrigger(host, potion, ch_uid, OCMD_QUAFF) == 0 {
        return;
    }
    let name = host.name(potion);
    let who = host.name(ch_uid);
    tell(host.world, ch, &format!("You quaff {name}."));
    tell_room(host.world, ch, &format!("{who} quaffs {name}."), &[]);
    // The three spells a potion casts need a way to cast a spell by number at
    // a given level, which the pack's spell system does not offer outside a
    // typed `cast`.
    host.unsupported("potion_spells", &format!("quaffing {name}"));
    extract_obj(host, potion);
}
