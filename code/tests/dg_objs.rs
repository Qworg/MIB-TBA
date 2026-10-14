//! Object and room facts: `ObjHost`, `RoomHost` and `DispatchHost` over the
//! ECS world.
//!
//! Expectations come from tbaMUD's C (`structs.h` bit numbers, `handler.c`,
//! `dg_objcmd.c`, `dg_scripts.c`) and `crates/dg-vm/HOST.md`, never from what
//! the code happens to return. Everything is driven through the host traits
//! or the VM's field tables, so no test depends on another stream's host.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dg_common;

use std::collections::BTreeMap;
use std::time::Duration;

use dg_common::*;
use dg_vm::WorldHost;
use dg_vm::dispatch::DispatchHost;
use dg_vm::fields_obj::{ITEM_TYPES, MAX_OBJ_AFFECT, ObjHost};
use dg_vm::fields_room::{RoomExit, RoomHost};
use dg_vm::types::{ROOM_ID_BASE, Uid};
use mib_core::prelude::*;
use mib_protocol::command::ExitKey;
use mib_stdlib::content::{RoomTag, Sector};
use mib_stdlib::prelude::*;
use mib_tba::dg::DgLog;
use mib_tba::items::{TbaItemTimer, attach_item_timers, tick_item_timers};
use mib_tba::prelude::*;

const EX_ISDOOR: u32 = 1;
const EX_CLOSED: u32 = 2;
const EX_LOCKED: u32 = 4;
const EX_PICKPROOF: u32 = 8;

// -- helpers -----------------------------------------------------------------

/// Change the prototype of `tba_item_<vnum>`.
fn tweak(app: &mut App, vnum: u32, change: impl FnOnce(&mut TbaItemDef)) {
    let mut content = app.world_mut().resource_mut::<TbaContent>();
    let def = content
        .items
        .get_mut(&ItemId::from(format!("tba_item_{vnum}")))
        .expect("item defined");
    change(def);
}

/// A defined item lying in a room, and its id.
fn object(app: &mut App, room: Entity, vnum: u32) -> (Entity, Uid) {
    let def_id = ItemId::from(format!("tba_item_{vnum}"));
    if !app
        .world()
        .resource::<TbaContent>()
        .items
        .contains_key(&def_id)
    {
        define_item(app, vnum, TbaItemKind::Treasure, &[], &["thing"]);
    }
    let entity = item_on_ground(app, room, vnum, "a thing");
    let id = uid(app, entity);
    (entity, id)
}

// -- values ------------------------------------------------------------------

#[test]
fn values_read_the_prototype_until_a_script_sets_one() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, sword) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| def.values = [11, 12, 13, 14]);
    let (_, twin) = object(&mut app, temple, 10);

    run(&mut app, |host| {
        assert_eq!(
            [0, 1, 2, 3].map(|idx| host.obj_val(sword, idx)),
            [11, 12, 13, 14]
        );
        host.obj_set_val(sword, 2, -99);
        host.obj_set_val(sword, 0, i32::MIN);
        assert_eq!(
            [0, 1, 2, 3].map(|idx| host.obj_val(sword, idx)),
            [i32::MIN, 12, -99, 14],
            "unset positions still read the prototype"
        );
        // The override is per instance: the prototype and its twin are intact.
        assert_eq!(
            [0, 1, 2, 3].map(|idx| host.obj_val(twin, idx)),
            [11, 12, 13, 14]
        );
        // Out-of-range positions read as zero and write nothing.
        assert_eq!(host.obj_val(sword, 4), 0);
        assert_eq!(host.obj_val(sword, usize::MAX), 0);
        host.obj_set_val(sword, 4, 5);
        host.obj_set_val(sword, usize::MAX, 5);
        assert_eq!(host.obj_val(sword, 4), 0);
    });
    let content = app.world().resource::<TbaContent>();
    assert_eq!(
        content.items[&ItemId::from("tba_item_10")].values,
        [11, 12, 13, 14],
        "the prototype is never written"
    );
}

#[test]
fn an_object_without_a_prototype_still_holds_what_a_script_sets() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let bare = item_on_ground(&mut app, temple, 77, "a bare thing");
    let id = uid(&mut app, bare);
    run(&mut app, |host| {
        assert_eq!(host.obj_val(id, 0), 0);
        assert_eq!((host.obj_cost(id), host.obj_rent(id)), (0, 0));
        assert_eq!((host.obj_weight(id), host.obj_timer(id)), (0, 0));
        assert_eq!(host.obj_apply(id, 0), (0, 0));
        assert!(!host.obj_extra_flagged(id, 0));
        assert!(!host.obj_affect_flagged(id, 1));
        host.obj_set_val(id, 1, 8);
        host.obj_set_cost(id, 40);
        assert_eq!((host.obj_val(id, 1), host.obj_cost(id)), (8, 40));
    });
}

#[test]
fn facts_of_something_that_is_not_an_object_are_neutral_and_writes_do_nothing() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "the guard");
    let guard_id = uid(&mut app, guard);
    let temple_id = uid(&mut app, temple);
    let gone = {
        let (entity, id) = object(&mut app, temple, 10);
        app.world_mut().despawn(entity);
        id
    };
    run(&mut app, |host| {
        for target in [guard_id, temple_id, gone, 0, i64::MAX, -1] {
            host.obj_set_val(target, 0, 5);
            host.obj_set_timer(target, 5);
            host.obj_set_cost(target, 5);
            host.obj_set_rent(target, 5);
            host.obj_set_weight(target, 5);
            host.obj_set_apply(target, 0, 3, 3);
            host.obj_set_alias(target, "x");
            host.obj_set_short_desc(target, "x");
            host.obj_set_long_desc(target, "x");
            assert_eq!(host.obj_val(target, 0), 0);
            assert_eq!(host.obj_timer(target), 0);
            assert_eq!(host.obj_cost(target), 0);
            assert_eq!(host.obj_rent(target), 0);
            assert_eq!(host.obj_weight(target), 0);
            assert_eq!(host.obj_apply(target, 0), (0, 0));
            assert!(!host.obj_extra_flagged(target, 0));
            assert!(!host.obj_affect_flagged(target, 1));
        }
    });
    assert!(app.world().get::<Npc>(guard).is_some());
}

// -- cost, rent, weight ------------------------------------------------------

#[test]
fn cost_rent_and_weight_fall_back_to_the_prototype() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, a) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| {
        def.cost = 500;
        def.rent = 7;
        def.weight = 30;
    });
    let (_, b) = object(&mut app, temple, 10);
    run(&mut app, |host| {
        assert_eq!(
            (host.obj_cost(a), host.obj_rent(a), host.obj_weight(a)),
            (500, 7, 30)
        );
        host.obj_set_cost(a, 0);
        host.obj_set_rent(a, -4);
        host.obj_set_weight(a, i32::MAX);
        assert_eq!(
            (host.obj_cost(a), host.obj_rent(a), host.obj_weight(a)),
            (0, -4, i32::MAX),
            "zero is a value, not a fall-back"
        );
        assert_eq!(
            (host.obj_cost(b), host.obj_rent(b), host.obj_weight(b)),
            (500, 7, 30)
        );
    });
}

#[test]
fn a_prototype_number_beyond_an_int_saturates() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, a) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| {
        def.cost = u32::MAX;
        def.weight = u32::MAX;
        def.rent = u32::MAX;
        def.timer = Some(u32::MAX);
    });
    run(&mut app, |host| {
        assert_eq!(host.obj_cost(a), i32::MAX);
        assert_eq!(host.obj_weight(a), i32::MAX);
        assert_eq!(host.obj_rent(a), i32::MAX);
        assert_eq!(host.obj_timer(a), i32::MAX);
    });
}

// -- timer -------------------------------------------------------------------

#[test]
fn a_script_timer_counts_down_by_mud_hours_and_the_item_decays() {
    let mut app = app();
    app.add_systems(FixedUpdate, tick_item_timers);
    let temple = room(&mut app, 1);
    let (entity, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| def.timer = Some(5));

    // Before a script sets one the prototype's value is the timer.
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 5));

    // A live pack timer is what a script reads.
    app.world_mut()
        .entity_mut(entity)
        .insert(TbaItemTimer { remaining: 3 });
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 3));

    // `otimer 4`, as the cooldown items do: one fact, the item's timer.
    run(&mut app, |host| host.obj_set_timer(id, 4));
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 4));
    assert_eq!(
        app.world().get::<TbaItemTimer>(entity).map(|t| t.remaining),
        Some(4)
    );

    // One MUD hour is 75 seconds.
    tick(&mut app, Duration::from_secs(75));
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 3));

    // limits.c:469-473: at zero the C runs the item's Timer trigger; this
    // item has none, so the pack's decay takes it (`tick_item_timers`).
    tick(&mut app, Duration::from_secs(75 * 10));
    assert!(
        app.world().get_entity(entity).is_err(),
        "a timed item without a timer trigger decays"
    );
}

#[test]
fn a_restored_item_timer_reads_its_saved_value() {
    let mut app = app();
    app.add_systems(FixedUpdate, tick_item_timers);
    let temple = room(&mut app, 1);
    let (entity, id) = object(&mut app, temple, 10);
    // `TbaItemTimer` is item-scoped and saved, so a restored item reads the
    // remaining life it was saved with, not a parked placeholder.
    app.world_mut()
        .entity_mut(entity)
        .insert(TbaItemTimer { remaining: 7 });
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 7));
    tick(&mut app, Duration::from_secs(75 * 30));
    assert!(
        app.world().get_entity(entity).is_err(),
        "a restored timer ages out"
    );
}

#[test]
fn a_restored_item_timer_is_not_clobbered_by_the_prototype() {
    let mut app = app();
    app.add_systems(FixedUpdate, (attach_item_timers, tick_item_timers).chain());
    let temple = room(&mut app, 1);
    let (entity, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| def.timer = Some(9));
    // A saved timer wins over the prototype: `attach_item_timers` leaves an
    // existing component alone.
    app.world_mut()
        .entity_mut(entity)
        .insert(TbaItemTimer { remaining: 3 });
    tick(&mut app, Duration::from_secs(75));
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 2));
}

#[test]
fn an_out_of_range_value_or_apply_slot_writes_and_counts_nothing() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, id) = object(&mut app, temple, 10);
    run(&mut app, |host| {
        host.obj_set_val(id, 9, 5);
        host.obj_set_apply(id, MAX_OBJ_AFFECT, 1, 1);
    });
    assert_eq!(app.world().resource::<DgLog>().count("obj.apply.effect"), 0);
    assert_eq!(app.world().resource::<DgLog>().count("obj.val.effect"), 0);
}

#[test]
fn a_timer_of_zero_or_less_is_a_spent_timer() {
    let mut app = app();
    app.add_systems(FixedUpdate, tick_item_timers);
    let temple = room(&mut app, 1);
    let (entity, id) = object(&mut app, temple, 10);
    app.world_mut()
        .entity_mut(entity)
        .insert(TbaItemTimer { remaining: 9 });
    tweak(&mut app, 10, |def| def.timer = Some(9));

    run(&mut app, |host| host.obj_set_timer(id, 0));
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 0));
    tick(&mut app, Duration::from_secs(75 * 20));
    assert!(app.world().get_entity(entity).is_ok(), "no timer, no decay");
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 0));

    run(&mut app, |host| {
        // A negative timer is the C's spent timer (`point_update` ages only a
        // positive one), which the pack stores as zero.
        host.obj_set_timer(id, -3);
        assert_eq!(host.obj_timer(id), 0);
        host.obj_set_timer(id, 2);
        assert_eq!(host.obj_timer(id), 2);
        host.obj_set_timer(id, i32::MAX);
        assert_eq!(host.obj_timer(id), i32::MAX);
        host.obj_set_timer(id, i32::MIN);
        assert_eq!(host.obj_timer(id), 0);
    });
}

#[test]
fn a_zero_timer_set_before_the_pack_attaches_its_own_is_not_undone() {
    let mut app = app();
    app.add_systems(FixedUpdate, (attach_item_timers, tick_item_timers).chain());
    let temple = room(&mut app, 1);
    let (entity, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| def.timer = Some(9));
    run(&mut app, |host| host.obj_set_timer(id, 0));

    tick(&mut app, Duration::from_secs(75 * 12));
    assert!(app.world().get_entity(entity).is_ok());
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 0));
}

#[test]
fn an_object_with_no_timer_anywhere_reads_zero() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| def.timer = Some(0));
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 0));
    tweak(&mut app, 10, |def| def.timer = None);
    run(&mut app, |host| assert_eq!(host.obj_timer(id), 0));
}

// -- flags -------------------------------------------------------------------

#[test]
fn every_extra_flag_maps_to_its_structs_h_bit() {
    use ItemExtraFlag::*;
    // `ITEM_GLOW` 0 ... `ITEM_QUEST` 17 (structs.h:412).
    let table = [
        (Glow, 0),
        (Hum, 1),
        (NoRent, 2),
        (NoDonate, 3),
        (NoInvis, 4),
        (Invisible, 5),
        (Magic, 6),
        (NoDrop, 7),
        (Bless, 8),
        (AntiGood, 9),
        (AntiEvil, 10),
        (AntiNeutral, 11),
        (AntiMagicUser, 12),
        (AntiCleric, 13),
        (AntiThief, 14),
        (AntiWarrior, 15),
        (NoSell, 16),
        (Quest, 17),
    ];
    let mut app = app();
    let temple = room(&mut app, 1);
    for (flag, bit) in table {
        let (_, id) = object(&mut app, temple, 10);
        tweak(&mut app, 10, |def| def.extra_flags = vec![flag]);
        run(&mut app, |host| {
            for probe in 0..40 {
                assert_eq!(
                    host.obj_extra_flagged(id, probe),
                    probe == bit,
                    "{flag:?} probed at bit {probe}"
                );
            }
            assert!(!host.obj_extra_flagged(id, usize::MAX));
        });
    }
}

#[test]
fn several_extra_flags_set_several_bits() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| {
        def.extra_flags = vec![
            ItemExtraFlag::Glow,
            ItemExtraFlag::Quest,
            ItemExtraFlag::NoDrop,
        ];
    });
    run(&mut app, |host| {
        let set: Vec<usize> = (0..32)
            .filter(|bit| host.obj_extra_flagged(id, *bit))
            .collect();
        assert_eq!(set, vec![0, 7, 17]);
    });
}

#[test]
fn every_affect_flag_maps_to_its_structs_h_bit() {
    use ItemAffectFlag::*;
    // `AFF_BLIND` 1 ... `AFF_CHARM` 22 (structs.h:272); 0, 9 and 21 are the
    // `DONTUSE`, `UNUSED` and `FREE` placeholders.
    let table = [
        (Blind, 1),
        (Invisible, 2),
        (DetectAlign, 3),
        (DetectInvis, 4),
        (DetectMagic, 5),
        (SenseLife, 6),
        (Waterwalk, 7),
        (Sanctuary, 8),
        (Curse, 10),
        (Infravision, 11),
        (Poison, 12),
        (ProtectEvil, 13),
        (ProtectGood, 14),
        (Sleep, 15),
        (NoTrack, 16),
        (Flying, 17),
        (Scuba, 18),
        (Sneak, 19),
        (Hide, 20),
        (Charm, 22),
    ];
    let mut app = app();
    let temple = room(&mut app, 1);
    for (flag, bit) in table {
        let (_, id) = object(&mut app, temple, 10);
        tweak(&mut app, 10, |def| def.affect_flags = vec![flag]);
        run(&mut app, |host| {
            for probe in 0..40 {
                assert_eq!(
                    host.obj_affect_flagged(id, probe),
                    probe == bit,
                    "{flag:?} probed at bit {probe}"
                );
            }
            assert!(!host.obj_affect_flagged(id, usize::MAX));
        });
    }
}

// -- applies -----------------------------------------------------------------

#[test]
fn apply_locations_map_to_apply_numbers() {
    use AffectLocation::*;
    // `APPLY_STR` 1 ... `APPLY_SAVING_SPELL` 24 (structs.h:434).
    let table = [
        (Str, 1),
        (Dex, 2),
        (Int, 3),
        (Wis, 4),
        (Con, 5),
        (Cha, 6),
        (MaxMana, 12),
        (MaxHp, 13),
        (MaxMove, 14),
        (ArmorClass, 17),
        (Hitroll, 18),
        (Damroll, 19),
        (SavePara, 20),
        (SaveRod, 21),
        (SavePetri, 22),
        (SaveBreath, 23),
        (SaveSpell, 24),
    ];
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| {
        def.affects = table
            .iter()
            .map(|(location, _)| AffectDef {
                location: *location,
                modifier: 2,
            })
            .collect();
    });
    run(&mut app, |host| {
        // The first MAX_OBJ_AFFECT blocks fill the slots in order.
        for (slot, (_, number)) in table.iter().take(MAX_OBJ_AFFECT).enumerate() {
            assert_eq!(host.obj_apply(id, slot), (*number, 2), "slot {slot}");
        }
        assert_eq!(host.obj_apply(id, MAX_OBJ_AFFECT), (0, 0));
        assert_eq!(host.obj_apply(id, MAX_OBJ_AFFECT + 1), (0, 0));
    });
    // Each location on its own, in slot zero.
    for (location, number) in table {
        tweak(&mut app, 10, |def| {
            def.affects = vec![AffectDef {
                location,
                modifier: -5,
            }];
        });
        run(&mut app, |host| {
            assert_eq!(host.obj_apply(id, 0), (number, -5), "{location:?}");
            assert_eq!(host.obj_apply(id, 1), (0, 0), "an empty slot");
        });
    }
}

#[test]
fn a_modifier_wraps_to_a_signed_byte() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| {
        def.affects = [i32::MAX, i32::MIN, 127, -128, 1000, -1000]
            .map(|modifier| AffectDef {
                location: AffectLocation::Str,
                modifier,
            })
            .to_vec();
    });
    run(&mut app, |host| {
        let modifiers: Vec<i8> = (0..6).map(|slot| host.obj_apply(id, slot).1).collect();
        // db.c:2290 assigns the int to an sbyte, truncating.
        let expected: Vec<i8> = [i32::MAX, i32::MIN, 127, -128, 1000, -1000]
            .iter()
            .map(|m| (*m & 0xff) as u8 as i8)
            .collect();
        assert_eq!(modifiers, expected);
        assert_eq!(modifiers[4], -24);
    });
}

#[test]
fn a_script_assigns_apply_slots_over_the_prototype() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| {
        def.affects = vec![
            AffectDef {
                location: AffectLocation::Dex,
                modifier: 1,
            },
            AffectDef {
                location: AffectLocation::Con,
                modifier: 2,
            },
        ];
    });
    let (_, twin) = object(&mut app, temple, 10);
    run(&mut app, |host| {
        host.obj_set_apply(id, 1, 18, -7);
        host.obj_set_apply(id, 4, 19, 3);
        // Clearing a slot is an assignment of (0, 0), not a fall-back.
        host.obj_set_apply(id, 0, 0, 0);
        assert_eq!(host.obj_apply(id, 0), (0, 0));
        assert_eq!(host.obj_apply(id, 1), (18, -7));
        assert_eq!(host.obj_apply(id, 4), (19, 3));
        assert_eq!(host.obj_apply(id, 2), (0, 0));
        assert_eq!(
            host.obj_apply(twin, 0),
            (2, 1),
            "the twin keeps the prototype"
        );
        assert_eq!(host.obj_apply(twin, 1), (5, 2));
        // A slot past MAX_OBJ_AFFECT holds nothing.
        host.obj_set_apply(id, MAX_OBJ_AFFECT, 5, 5);
        host.obj_set_apply(id, usize::MAX, 5, 5);
        assert_eq!(host.obj_apply(id, MAX_OBJ_AFFECT), (0, 0));
        assert_eq!(host.obj_apply(id, usize::MAX), (0, 0));
    });
}

// -- descriptions ------------------------------------------------------------

#[test]
fn descriptions_are_the_items_name_and_description() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (entity, id) = object(&mut app, temple, 10);
    let (other, other_id) = object(&mut app, temple, 10);
    run(&mut app, |host| {
        host.obj_set_short_desc(id, "a glowing orb");
        host.obj_set_long_desc(id, "A glowing orb floats here.");
        host.obj_set_alias(id, "orb glowing");
        assert_eq!(host.name(id), "a glowing orb");
        // `oset alias` changes the keywords scripts read back; the pack's own
        // matching still uses the prototype, so the write is counted.
        assert_eq!(host.keywords(id), "orb glowing");
        let long = "x".repeat(10_000);
        host.obj_set_short_desc(id, &long);
        assert_eq!(host.name(other_id), "a thing");
    });
    let item = app.world().get::<Item>(entity).unwrap();
    assert_eq!(item.name.len(), 4096, "the text is bounded");
    assert_eq!(item.description, "A glowing orb floats here.");
    assert_eq!(app.world().resource::<DgLog>().count("obj.alias"), 1);
    assert_eq!(app.world().get::<Item>(other).unwrap().name, "a thing");
}

// -- the field tables and script commands ------------------------------------

#[test]
fn the_object_field_table_reads_and_the_commands_write() {
    let mut app = app();
    define(
        &mut app,
        500,
        "obj",
        "g",
        0,
        "set v0 %self.val0%\nset v1 %self.val1%\nset v2 %self.val2%\nset v3 %self.val3%\n\
         set timer %self.timer%\nset cost %self.cost%\nset rent %self.cost_per_day%\n\
         set weight %self.weight%\nset glow %self.extra(GLOW)%\nset nodrop %self.extra(NO_DROP)%\n\
         set hum %self.extra(HUM)%\nset flying %self.affects(FLY)%\n\
         set short %self.shortdesc%\nset type %self.type%\n\
         global v0\nglobal v1\nglobal v2\nglobal v3\nglobal timer\nglobal cost\nglobal rent\nglobal weight\nglobal glow\nglobal nodrop\nglobal hum\nglobal flying\nglobal short\nglobal type",
    );
    define(
        &mut app,
        501,
        "obj",
        "g",
        0,
        "osetval 1 77\notimer 6\nset v1 %self.val1%\nset v0 %self.val0%\nset timer %self.timer%\n\
         global v1\nglobal v0\nglobal timer",
    );
    attach(&mut app, "item:tba_item_10", &[500, 501]);
    let temple = room(&mut app, 1);
    let (entity, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| {
        def.kind = TbaItemKind::Weapon;
        def.values = [4, 3, 2, 1];
        def.cost = 120;
        def.rent = 6;
        def.weight = 9;
        def.timer = Some(8);
        def.extra_flags = vec![ItemExtraFlag::Glow, ItemExtraFlag::NoDrop];
        def.affect_flags = vec![ItemAffectFlag::Flying];
    });

    run_trigger(&mut app, id, 500);
    let read = |app: &mut App, name: &str| global(app, id, name).unwrap();
    assert_eq!(read(&mut app, "v0"), "4");
    assert_eq!(read(&mut app, "v1"), "3");
    assert_eq!(read(&mut app, "v2"), "2");
    assert_eq!(read(&mut app, "v3"), "1");
    assert_eq!(read(&mut app, "timer"), "8");
    assert_eq!(read(&mut app, "cost"), "120");
    assert_eq!(read(&mut app, "rent"), "6");
    assert_eq!(read(&mut app, "weight"), "9");
    assert_eq!(read(&mut app, "glow"), "1");
    assert_eq!(read(&mut app, "nodrop"), "1");
    assert_eq!(read(&mut app, "hum"), "0");
    assert_eq!(read(&mut app, "flying"), "1");
    assert_eq!(read(&mut app, "short"), "a thing");
    assert_eq!(read(&mut app, "type"), "WEAPON");

    run_trigger(&mut app, id, 501);
    assert_eq!(read(&mut app, "v1"), "77");
    assert_eq!(
        read(&mut app, "v0"),
        "4",
        "the other positions still read the prototype"
    );
    assert_eq!(read(&mut app, "timer"), "6");
    assert!(app.world().get_entity(entity).is_ok());
    run(&mut app, |host| assert_eq!(host.obj_val(id, 1), 77));
}

#[test]
fn field_table_writes_to_cost_and_rent_reach_the_override() {
    let mut app = app();
    define(
        &mut app,
        500,
        "obj",
        "g",
        0,
        "eval cost %self.cost(+30)%\neval rent %self.cost_per_day(-2)%\n\
         set after %self.cost%\nglobal cost\nglobal rent\nglobal after",
    );
    attach(&mut app, "item:tba_item_10", &[500]);
    let temple = room(&mut app, 1);
    let (_, id) = object(&mut app, temple, 10);
    tweak(&mut app, 10, |def| {
        def.cost = 100;
        def.rent = 10;
    });
    run_trigger(&mut app, id, 500);
    assert_eq!(global(&mut app, id, "after").unwrap(), "130");
    run(&mut app, |host| {
        assert_eq!(host.obj_cost(id), 130);
        assert_eq!(host.obj_rent(id), 8);
    });
}

#[test]
fn every_item_kind_reads_as_its_type_name() {
    use TbaItemKind::*;
    let kinds = [
        Undefined,
        Light,
        Scroll,
        Wand,
        Staff,
        Weapon,
        Furniture,
        Free,
        Treasure,
        Armor,
        Potion,
        Worn,
        Other,
        Trash,
        Free2,
        Container,
        Note,
        DrinkContainer,
        Key,
        Food,
        Money,
        Pen,
        Boat,
        Fountain,
    ];
    let expected = [
        "UNDEFINED",
        "LIGHT",
        "SCROLL",
        "WAND",
        "STAFF",
        "WEAPON",
        "FURNITURE",
        "FREE",
        "TREASURE",
        "ARMOR",
        "POTION",
        "WORN",
        "OTHER",
        "TRASH",
        "FREE2",
        "CONTAINER",
        "NOTE",
        "LIQ CONTAINER",
        "KEY",
        "FOOD",
        "MONEY",
        "PEN",
        "BOAT",
        "FOUNTAIN",
    ];
    assert_eq!(kinds.len(), expected.len());
    let mut app = app();
    let temple = room(&mut app, 1);
    define(
        &mut app,
        500,
        "obj",
        "g",
        0,
        "set type %self.type%\nglobal type",
    );
    attach(&mut app, "item:tba_item_10", &[500]);
    for (number, (kind, name)) in kinds.into_iter().zip(expected).enumerate() {
        let (_, id) = object(&mut app, temple, 10);
        tweak(&mut app, 10, |def| def.kind = kind);
        run_trigger(&mut app, id, 500);
        assert_eq!(global(&mut app, id, "type").unwrap(), name, "{kind:?}");
        assert_eq!(
            sprint_type(number as i32),
            name,
            "the VM's table agrees with the C order for {kind:?}"
        );
        run(&mut app, |host| {
            assert_eq!(host.obj_type(id), number as i32)
        });
    }
}

fn sprint_type(number: i32) -> &'static str {
    usize::try_from(number)
        .ok()
        .and_then(|index| ITEM_TYPES.get(index))
        .copied()
        .unwrap_or("UNDEFINED")
}

// -- rooms: zone, sector, flags ----------------------------------------------

fn zone(app: &mut App, number: u32, name: &str, bot: u32, top: u32) {
    app.world_mut().resource_mut::<TbaContent>().zones.insert(
        number,
        ZoneDef {
            name: name.to_owned(),
            bot,
            top,
            lifespan_mins: 15,
            reset_mode: ResetMode::Always,
            commands: Vec::new(),
        },
    );
}

#[test]
fn a_rooms_zone_is_the_one_whose_range_holds_its_vnum() {
    let mut app = app();
    zone(&mut app, 30, "Northern Midgaard", 3000, 3099);
    zone(&mut app, 31, "Southern Midgaard", 3100, 3199);
    let temple = room(&mut app, 3001);
    let low = room(&mut app, 3000);
    let high = room(&mut app, 3199);
    let edge = room(&mut app, 3099);
    let nowhere = room(&mut app, 9000);
    let ids: Vec<Uid> = [temple, low, high, edge, nowhere]
        .into_iter()
        .map(|entity| uid(&mut app, entity))
        .collect();
    run(&mut app, |host| {
        let seen: Vec<(i32, String)> = ids
            .iter()
            .map(|id| (host.room_zone_number(*id), host.room_zone_name(*id)))
            .collect();
        assert_eq!(
            seen,
            vec![
                (30, "Northern Midgaard".to_owned()),
                (30, "Northern Midgaard".to_owned()),
                (31, "Southern Midgaard".to_owned()),
                (30, "Northern Midgaard".to_owned()),
                (0, String::new()),
            ]
        );
    });
}

#[test]
fn zone_of_something_that_is_not_a_room_is_neutral() {
    let mut app = app();
    zone(&mut app, 30, "Zone", 0, 9999);
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 1, "guard");
    let guard_id = uid(&mut app, guard);
    run(&mut app, |host| {
        assert_eq!(host.room_zone_number(guard_id), 0);
        assert_eq!(host.room_zone_name(guard_id), "");
        assert_eq!(host.room_zone_number(ROOM_ID_BASE + 5), 0);
        assert_eq!(host.room_sector(guard_id), 0);
        assert!(!host.room_flagged(guard_id, 3));
        assert_eq!(host.room_exit(guard_id, 0), None);
    });
}

#[test]
fn a_room_with_no_pack_zones_has_no_zone() {
    let mut app = app();
    let temple = room(&mut app, 3001);
    let id = uid(&mut app, temple);
    run(&mut app, |host| {
        assert_eq!(host.room_zone_number(id), 0);
        assert_eq!(host.room_zone_name(id), "");
    });
}

#[test]
fn every_sector_maps_to_its_sect_number() {
    use Sector::*;
    // `SECT_INSIDE` 0 ... `SECT_UNDERWATER` 9 (structs.h): flying is 8.
    let table = [
        (Inside, 0),
        (City, 1),
        (Field, 2),
        (Forest, 3),
        (Hills, 4),
        (Mountain, 5),
        (WaterSwim, 6),
        (WaterNoSwim, 7),
        (Flying, 8),
        (Underwater, 9),
    ];
    let mut app = app();
    for (sector, number) in table {
        let entity = room(&mut app, 100 + number as u32);
        app.world_mut().get_mut::<Room>(entity).unwrap().sector = sector;
        let id = uid(&mut app, entity);
        run(&mut app, |host| {
            assert_eq!(host.room_sector(id), number, "{sector:?}")
        });
    }
}

#[test]
fn every_room_tag_maps_to_its_room_bit() {
    // `ROOM_DARK` 0, `ROOM_NOMOB` 2, `ROOM_PEACEFUL` 4, `ROOM_SOUNDPROOF` 5,
    // `ROOM_TUNNEL` 8, `ROOM_PRIVATE` 9, `ROOM_GODROOM` 10 (structs.h).
    let table = [
        (RoomTag::Dark, 0),
        (RoomTag::NoMob, 2),
        (RoomTag::Peaceful, 4),
        (RoomTag::Soundproof, 5),
        (RoomTag::Tunnel, 8),
        (RoomTag::Private, 9),
        (RoomTag::Godroom, 10),
    ];
    let mut app = app();
    for (tag, bit) in table {
        let entity = room(&mut app, 200 + bit as u32);
        app.world_mut()
            .get_mut::<Room>(entity)
            .unwrap()
            .tags
            .insert(tag);
        let id = uid(&mut app, entity);
        run(&mut app, |host| {
            // Outdoor is not set, so the room is INDOORS (bit 3).
            let set: Vec<usize> = (0..40).filter(|b| host.room_flagged(id, *b)).collect();
            let mut want = vec![bit, 3];
            want.sort_unstable();
            assert_eq!(set, want, "{tag:?}");
            assert!(!host.room_flagged(id, usize::MAX));
        });
    }
}

#[test]
fn indoors_is_the_absence_of_the_outdoor_tag() {
    let mut app = app();
    let inside = room(&mut app, 1);
    let outside = room(&mut app, 2);
    app.world_mut()
        .get_mut::<Room>(outside)
        .unwrap()
        .tags
        .insert(RoomTag::Outdoor);
    let (inside, outside) = (uid(&mut app, inside), uid(&mut app, outside));
    run(&mut app, |host| {
        assert!(host.room_flagged(inside, 3));
        assert!(!host.room_flagged(outside, 3));
    });
}

#[test]
fn tags_with_no_room_bit_set_nothing() {
    let mut app = app();
    let entity = room(&mut app, 1);
    {
        let mut room = app.world_mut().get_mut::<Room>(entity).unwrap();
        room.tags.insert(RoomTag::Safe);
        room.tags.insert(RoomTag::NoRecall);
        room.tags.insert(RoomTag::StartRoom);
        room.tags.insert(RoomTag::Outdoor);
    }
    let id = uid(&mut app, entity);
    run(&mut app, |host| {
        assert!((0..40).all(|bit| !host.room_flagged(id, bit)));
    });
}

#[test]
fn runtime_flag_overrides_win_over_the_authored_tags() {
    let mut app = app();
    let entity = room(&mut app, 1);
    app.world_mut()
        .get_mut::<Room>(entity)
        .unwrap()
        .tags
        .extend([RoomTag::Dark, RoomTag::Outdoor]);
    app.world_mut().get_mut::<RoomState>(entity).unwrap().flags = BTreeMap::from([
        (RoomTag::Dark, false),
        (RoomTag::Private, true),
        (RoomTag::Outdoor, false),
    ]);
    let id = uid(&mut app, entity);
    run(&mut app, |host| {
        assert!(
            !host.room_flagged(id, 0),
            "authored DARK cleared at runtime"
        );
        assert!(host.room_flagged(id, 9), "PRIVATE set at runtime");
        assert!(host.room_flagged(id, 3), "OUTDOOR cleared means INDOORS");
    });
    // A room that lost its state still reads its authored tags.
    app.world_mut().entity_mut(entity).remove::<RoomState>();
    run(&mut app, |host| {
        assert!(host.room_flagged(id, 0));
        assert!(!host.room_flagged(id, 3));
    });
}

#[test]
fn the_weather_is_not_modelled() {
    let mut app = app();
    run(&mut app, |host| assert_eq!(host.room_sky(), 0));
}

// -- rooms: exits ------------------------------------------------------------

/// Give `from` an exit to `to` in `key`.
fn open_exit(app: &mut App, from: Entity, key: ExitKey, to: Entity) {
    let mut entity = app.world_mut().entity_mut(from);
    if let Some(mut exits) = entity.get_mut::<Exits>() {
        exits.map.insert(key, to);
    } else {
        let mut exits = Exits::default();
        exits.map.insert(key, to);
        entity.insert(exits);
    }
}

/// Put an authored door on `from`'s exit, with its runtime state.
fn door(
    app: &mut App,
    from: Entity,
    key: ExitKey,
    key_item: Option<&str>,
    pickable: bool,
    state: Option<(bool, bool)>,
) {
    let mut entity = app.world_mut().entity_mut(from);
    if entity.get::<Doors>().is_none() {
        entity.insert(Doors::default());
    }
    entity.get_mut::<Doors>().unwrap().by_dir.insert(
        key,
        Door {
            key: key_item.map(ItemId::from),
            name: Some("gate".to_owned()),
            pickable,
            difficulty: 0,
            default_closed: false,
            default_locked: false,
        },
    );
    if let Some((closed, locked)) = state {
        entity.get_mut::<RoomState>().unwrap().doors.insert(
            key,
            DoorState {
                closed,
                locked,
                to: None,
            },
        );
    }
}

#[test]
fn each_dg_direction_leads_where_its_engine_direction_does() {
    use ExitKey::*;
    // `types::DIRS`: north east south west up down northwest northeast
    // southeast southwest.
    let order = [
        North, East, South, West, Up, Down, Northwest, Northeast, Southeast, Southwest,
    ];
    let mut app = app();
    let hub = room(&mut app, 1);
    let mut targets = Vec::new();
    for (index, key) in order.into_iter().enumerate() {
        let to = room(&mut app, 10 + index as u32);
        open_exit(&mut app, hub, key, to);
        targets.push(to);
    }
    // `in` and `out` have no DG number.
    let extra = room(&mut app, 50);
    open_exit(&mut app, hub, In, extra);
    open_exit(&mut app, hub, Out, extra);
    let hub_id = uid(&mut app, hub);
    let target_ids: Vec<Uid> = targets.into_iter().map(|e| uid(&mut app, e)).collect();
    run(&mut app, |host| {
        for (dir, want) in target_ids.iter().enumerate() {
            let exit = host.room_exit(hub_id, dir).expect("exit");
            assert_eq!(exit.to_room, Some(*want), "direction {dir}");
            assert_eq!(exit.info, 0, "a plain exit is no door");
            assert_eq!(exit.key, 65_535, "no key is NOTHING as an ush_int");
            assert_eq!(host.vnum(*want), 10 + dir as i32);
        }
        assert_eq!(host.room_exit(hub_id, 10), None);
        assert_eq!(host.room_exit(hub_id, usize::MAX), None);
    });
}

#[test]
fn a_room_with_no_exits_has_none_in_any_direction() {
    let mut app = app();
    let bare = room(&mut app, 1);
    let empty = room(&mut app, 2);
    app.world_mut().entity_mut(empty).insert(Exits::default());
    let (bare, empty) = (uid(&mut app, bare), uid(&mut app, empty));
    run(&mut app, |host| {
        for dir in 0..10 {
            assert_eq!(host.room_exit(bare, dir), None);
            assert_eq!(host.room_exit(empty, dir), None);
        }
    });
}

#[test]
fn a_door_reports_its_state_key_and_pickability() {
    use ExitKey::*;
    let mut app = app();
    let hall = room(&mut app, 1);
    let vault = room(&mut app, 2);
    for key in [North, East, South, West, Up] {
        open_exit(&mut app, hall, key, vault);
    }
    // north: open door, no key, pickable.
    door(&mut app, hall, North, None, true, Some((false, false)));
    // east: closed.
    door(&mut app, hall, East, None, true, Some((true, false)));
    // south: closed and locked, with key item 3012, pickproof.
    door(
        &mut app,
        hall,
        South,
        Some("tba_item_3012"),
        false,
        Some((true, true)),
    );
    // west: pickproof, open.
    door(
        &mut app,
        hall,
        West,
        Some("tba_item_7"),
        false,
        Some((false, false)),
    );
    // up: authored closed and locked, with no runtime state yet.
    door(&mut app, hall, Up, None, true, None);
    {
        let mut doors = app.world_mut().get_mut::<Doors>(hall).unwrap();
        let up = doors.by_dir.get_mut(&Up).unwrap();
        up.default_closed = true;
        up.default_locked = true;
    }
    let hall_id = uid(&mut app, hall);
    let info = |app: &mut App, dir: usize| -> RoomExit {
        run(app, |host| host.room_exit(hall_id, dir)).expect("exit")
    };
    assert_eq!(info(&mut app, 0).info, EX_ISDOOR);
    assert_eq!(info(&mut app, 0).key, 65_535);
    assert_eq!(info(&mut app, 1).info, EX_ISDOOR | EX_CLOSED);
    let south = info(&mut app, 2);
    assert_eq!(south.info, EX_ISDOOR | EX_CLOSED | EX_LOCKED | EX_PICKPROOF);
    assert_eq!(south.key, 3012);
    let west = info(&mut app, 3);
    assert_eq!(west.info, EX_ISDOOR | EX_PICKPROOF);
    assert_eq!(west.key, 7);
    assert_eq!(
        info(&mut app, 4).info,
        EX_ISDOOR | EX_CLOSED | EX_LOCKED,
        "the authored default stands in for a missing runtime state"
    );
}

#[test]
fn opening_a_door_at_runtime_clears_its_bits() {
    let mut app = app();
    let hall = room(&mut app, 1);
    let vault = room(&mut app, 2);
    open_exit(&mut app, hall, ExitKey::North, vault);
    door(
        &mut app,
        hall,
        ExitKey::North,
        Some("tba_item_9"),
        true,
        Some((true, true)),
    );
    let hall_id = uid(&mut app, hall);
    run(&mut app, |host| {
        assert_eq!(
            host.room_exit(hall_id, 0).unwrap().info,
            EX_ISDOOR | EX_CLOSED | EX_LOCKED
        );
    });
    app.world_mut()
        .get_mut::<RoomState>(hall)
        .unwrap()
        .doors
        .insert(
            ExitKey::North,
            DoorState {
                closed: false,
                locked: false,
                to: None,
            },
        );
    run(&mut app, |host| {
        let exit = host.room_exit(hall_id, 0).unwrap();
        assert_eq!(exit.info, EX_ISDOOR);
        assert_eq!(exit.key, 9, "the key outlives the lock");
    });
}

#[test]
fn a_door_state_with_no_authored_door_is_not_a_door() {
    // The engine's movement gate treats a direction as a door only when
    // `Doors` has it, so a bare `RoomState` entry is not one.
    let mut app = app();
    let hall = room(&mut app, 1);
    let vault = room(&mut app, 2);
    open_exit(&mut app, hall, ExitKey::East, vault);
    app.world_mut()
        .get_mut::<RoomState>(hall)
        .unwrap()
        .doors
        .insert(
            ExitKey::East,
            DoorState {
                closed: true,
                locked: true,
                to: None,
            },
        );
    let hall_id = uid(&mut app, hall);
    run(&mut app, |host| {
        let exit = host.room_exit(hall_id, 1).unwrap();
        assert_eq!(exit.info, 0);
        assert_eq!(exit.key, 65_535);
    });
}

#[test]
fn a_hidden_exit_sets_ex_hidden() {
    // db.c:1547-1550: door codes 3 and 4 add EX_HIDDEN (bit 4).
    let mut app = app();
    let hall = room(&mut app, 1);
    let vault = room(&mut app, 2);
    open_exit(&mut app, hall, ExitKey::East, vault);
    open_exit(&mut app, hall, ExitKey::West, vault);
    app.world_mut()
        .get_mut::<Exits>(hall)
        .unwrap()
        .hidden
        .insert(ExitKey::East);
    door(&mut app, hall, ExitKey::East, None, false, None);
    let hall_id = uid(&mut app, hall);
    run(&mut app, |host| {
        assert_eq!(
            host.room_exit(hall_id, 1).unwrap().info,
            EX_ISDOOR | EX_PICKPROOF | 16
        );
        assert_eq!(host.room_exit(hall_id, 3).unwrap().info, 0);
    });
}

#[test]
fn a_door_state_for_a_direction_with_no_exit_is_no_exit() {
    let mut app = app();
    let hall = room(&mut app, 1);
    app.world_mut()
        .get_mut::<RoomState>(hall)
        .unwrap()
        .doors
        .insert(
            ExitKey::East,
            DoorState {
                closed: true,
                locked: true,
                to: None,
            },
        );
    door(&mut app, hall, ExitKey::West, None, true, None);
    let hall_id = uid(&mut app, hall);
    run(&mut app, |host| {
        assert_eq!(host.room_exit(hall_id, 1), None);
        assert_eq!(host.room_exit(hall_id, 3), None);
    });
}

#[test]
fn an_exit_whose_destination_is_gone_leads_nowhere_but_exists() {
    let mut app = app();
    let hall = room(&mut app, 1);
    let lost = room(&mut app, 2);
    open_exit(&mut app, hall, ExitKey::North, lost);
    let hall_id = uid(&mut app, hall);
    app.world_mut().despawn(lost);
    run(&mut app, |host| {
        let exit = host.room_exit(hall_id, 0).expect("the exit remains");
        assert_eq!(exit.to_room, None);
    });
    // An exit that points at something that is not a room is the same.
    let guard = npc(&mut app, hall, 1, "guard");
    open_exit(&mut app, hall, ExitKey::South, guard);
    run(&mut app, |host| {
        assert_eq!(host.room_exit(hall_id, 2).unwrap().to_room, None);
    });
}

#[test]
fn the_room_field_table_reads_exits_zones_and_flags() {
    let mut app = app();
    zone(&mut app, 30, "Midgaard", 3000, 3099);
    define(
        &mut app,
        600,
        "wld",
        "g",
        0,
        "set north %self.north%\nset northvnum %self.north(vnum)%\nset nkey %self.north(key)%\n\
         set south %self.south%\nset skey %self.south(key)%\nset west %self.west%\n\
         set zonenum %self.zonenumber%\nset zonename %self.zonename%\nset sector %self.sector%\n\
         set dark %self.roomflag(DARK)%\nset indoors %self.roomflag(INDOORS)%\n\
         set priv %self.roomflag(PRIVATE)%\nset house %self.roomflag(HOUSE)%\n\
         set weather %self.weather%\n\
         global north\nglobal northvnum\nglobal nkey\nglobal south\nglobal skey\nglobal west\nglobal zonenum\nglobal zonename\nglobal sector\nglobal dark\nglobal indoors\nglobal priv\nglobal house\nglobal weather",
    );
    attach(&mut app, "room:tba.3001", &[600]);
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    {
        let mut room = app.world_mut().get_mut::<Room>(temple).unwrap();
        room.sector = Sector::City;
        room.tags
            .extend([RoomTag::Dark, RoomTag::Outdoor, RoomTag::Private]);
    }
    open_exit(&mut app, temple, ExitKey::North, square);
    open_exit(&mut app, temple, ExitKey::South, square);
    door(
        &mut app,
        temple,
        ExitKey::South,
        Some("tba_item_3012"),
        true,
        Some((true, true)),
    );
    let id = uid(&mut app, temple);
    run_trigger(&mut app, id, 600);
    let read = |app: &mut App, name: &str| global(app, id, name).unwrap();
    assert_eq!(read(&mut app, "north"), "NOBITS ");
    assert_eq!(read(&mut app, "northvnum"), "3002");
    assert_eq!(read(&mut app, "nkey"), "65535");
    assert_eq!(read(&mut app, "south"), "DOOR CLOSED LOCKED ");
    assert_eq!(read(&mut app, "skey"), "3012");
    assert_eq!(read(&mut app, "west"), "", "no exit that way is empty");
    assert_eq!(read(&mut app, "zonenum"), "30");
    assert_eq!(read(&mut app, "zonename"), "Midgaard");
    assert_eq!(read(&mut app, "sector"), "City");
    assert_eq!(read(&mut app, "dark"), "1");
    assert_eq!(read(&mut app, "indoors"), "0");
    assert_eq!(read(&mut app, "priv"), "1");
    assert_eq!(read(&mut app, "house"), "0");
    assert_eq!(
        read(&mut app, "weather"),
        "sunny",
        "no weather: a cloudless sky"
    );
}

// -- dispatch ----------------------------------------------------------------

#[test]
fn freeing_a_proto_script_strips_the_prototype_only() {
    let mut app = app();
    define(&mut app, 700, "mob", "g", 0, "set a 1");
    define(&mut app, 701, "obj", "g", 0, "set a 1");
    attach(&mut app, "npc:tba_npc_5", &[700]);
    attach(&mut app, "item:tba_item_10", &[701]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 5, "guard");
    let (_, item_id) = object(&mut app, temple, 10);
    let guard_id = uid(&mut app, guard);
    assert_eq!(trigger_vnums(&mut app, guard_id), vec![700]);
    assert_eq!(trigger_vnums(&mut app, item_id), vec![701]);

    run(&mut app, |host| {
        host.disp_free_proto_script(guard_id);
        host.disp_free_proto_script(item_id);
    });
    // The live ones keep their triggers.
    assert_eq!(trigger_vnums(&mut app, guard_id), vec![700]);
    assert_eq!(trigger_vnums(&mut app, item_id), vec![701]);

    // What is made from the prototypes afterwards has none.
    let second_guard = npc(&mut app, temple, 5, "another guard");
    let (_, second_item_id) = object(&mut app, temple, 10);
    let second_guard_id = uid(&mut app, second_guard);
    assert_eq!(trigger_vnums(&mut app, second_guard_id), Vec::<i32>::new());
    assert_eq!(trigger_vnums(&mut app, second_item_id), Vec::<i32>::new());
    // Other prototypes are untouched.
    attach(&mut app, "npc:tba_npc_6", &[700]);
    let other = npc(&mut app, temple, 6, "other");
    let other_id = uid(&mut app, other);
    assert_eq!(trigger_vnums(&mut app, other_id), vec![700]);
}

#[test]
fn freeing_a_proto_script_of_an_entity_with_no_prototype_does_nothing() {
    let mut app = app();
    define(&mut app, 700, "mob", "g", 0, "set a 1");
    attach(&mut app, "npc:tba_npc_5", &[700]);
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 7);
    let alice_id = uid(&mut app, alice);
    let temple_id = uid(&mut app, temple);
    let ghost = npc(&mut app, temple, 5, "ghost");
    let ghost_id = uid(&mut app, ghost);
    app.world_mut().despawn(ghost);
    run(&mut app, |host| {
        host.disp_free_proto_script(alice_id);
        host.disp_free_proto_script(temple_id);
        host.disp_free_proto_script(ghost_id);
        host.disp_free_proto_script(0);
        host.disp_free_proto_script(i64::MAX);
    });
    let again = npc(&mut app, temple, 5, "again");
    let again_id = uid(&mut app, again);
    assert_eq!(
        trigger_vnums(&mut app, again_id),
        vec![700],
        "a dead entity's prototype is not stripped"
    );
}

#[test]
fn script_writes_the_pack_ignores_are_counted() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let (_, id) = object(&mut app, temple, 10);
    run(&mut app, |host| {
        host.obj_set_val(id, 0, 1);
        host.obj_set_cost(id, 1);
        host.obj_set_rent(id, 1);
        host.obj_set_weight(id, 1);
        host.obj_set_apply(id, 0, 1, 1);
    });
    let log = app.world().resource::<DgLog>();
    for key in [
        "obj.val.effect",
        "obj.cost.effect",
        "obj.rent.effect",
        "obj.weight.effect",
        "obj.apply.effect",
    ] {
        assert_eq!(log.count(key), 1, "{key}");
    }
}
