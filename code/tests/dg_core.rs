//! The DG runtime core: ids, list order, prototype scripts, the pulse clock,
//! waits, random and time triggers, player variables and the dead sweep.
//!
//! Expectations come from tbaMUD's C (`handler.c` list operations,
//! `dg_scripts.c`, `dg_triggers.c`, `comm.c` heartbeat) or from the design in
//! `mib_tba::dg`'s module docs, never from what the code happens to return.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dg_common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use dg_common::*;
use dg_vm::lookup::{WEAR_BODY, WEAR_HOLD, WEAR_LIGHT, WEAR_WIELD};
use dg_vm::types::{MOB_ID_BASE, OBJ_ID_BASE, ROOM_ID_BASE};
use dg_vm::{BaseHost, Kind, WorldHost};
use mib_content::prelude::{MibModule, SectionRegistry, registry_for};
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::dg::host::{item_type_number, item_wear_bit, npc_keywords, wear_position, wear_slot};
use mib_tba::dg::ids::{DgIds, vnum_from_id};
use mib_tba::dg::script::{DgDead, DgProtos, DgScript, init_entity};
use mib_tba::dg::vars::TbaTriggerVars;
use mib_tba::dg::waits::{DgPulse, DgWaits};
use mib_tba::dg::{DgLog, with_host};
use mib_tba::prelude::*;
use mib_tba::triggers::content::{TriggerDef, TriggerId};

// -- ids ---------------------------------------------------------------------

#[test]
fn ids_fall_in_the_ranges_that_say_what_they_are() {
    let mut app = app();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    let guard = npc(&mut app, temple, 3024, "the guard");
    let sword = item_on_ground(&mut app, temple, 3022, "a sword");

    assert_eq!(uid(&mut app, temple), ROOM_ID_BASE + 3001);
    assert_eq!(uid(&mut app, alice), 7);
    let guard_id = uid(&mut app, guard);
    assert!((MOB_ID_BASE + 1..ROOM_ID_BASE).contains(&guard_id));
    let sword_id = uid(&mut app, sword);
    assert!(sword_id > OBJ_ID_BASE);

    run(&mut app, |host| {
        assert_eq!(host.kind(ROOM_ID_BASE + 3001), Some(Kind::Room));
        assert_eq!(host.kind(7), Some(Kind::Char));
        assert_eq!(host.kind(guard_id), Some(Kind::Char));
        assert_eq!(host.kind(sword_id), Some(Kind::Obj));
        assert_eq!(host.kind(8), None);
        assert!(!host.exists(ROOM_ID_BASE + 9999));
    });
}

#[test]
fn mobs_and_objects_count_up_and_players_out_of_range_count_down() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let first = npc(&mut app, temple, 1, "a");
    let second = npc(&mut app, temple, 2, "b");
    let thing = item_on_ground(&mut app, temple, 1, "thing");
    let other = item_on_ground(&mut app, temple, 2, "other");
    // An account id outside the player range, zero, and a repeat of an id
    // already in use all take the top of the range, counting down.
    let huge = player(&mut app, temple, "Huge", 20_000_000);
    let zero = player(&mut app, temple, "Zero", 0);
    let twin_a = player(&mut app, temple, "TwinA", 5);
    let twin_b = player(&mut app, temple, "TwinB", 5);

    assert_eq!(uid(&mut app, first), MOB_ID_BASE + 1);
    assert_eq!(uid(&mut app, second), MOB_ID_BASE + 2);
    assert_eq!(uid(&mut app, thing), OBJ_ID_BASE + 1);
    assert_eq!(uid(&mut app, other), OBJ_ID_BASE + 2);
    let down: Vec<_> = [huge, zero, twin_b]
        .into_iter()
        .map(|entity| uid(&mut app, entity))
        .collect();
    assert_eq!(
        down.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [MOB_ID_BASE - 1, MOB_ID_BASE - 2, MOB_ID_BASE - 3].into()
    );
    assert_eq!(uid(&mut app, twin_a), 5);
}

#[test]
fn ids_are_stable_and_a_despawned_entity_stops_existing() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "a guard");
    let id = uid(&mut app, guard);
    assert_eq!(uid(&mut app, guard), id);
    run(&mut app, |host| {
        assert_eq!(host.entity(id), Some(guard));
        assert_eq!(host.uid(guard), Some(id));
    });

    app.world_mut().despawn(guard);
    run(&mut app, |host| {
        assert_eq!(host.entity(id), None);
        assert!(!host.exists(id));
        assert_eq!(host.kind(id), None);
        assert_eq!(host.name(id), "");
    });
    assert!(
        app.world().resource::<DgIds>().get(id).is_none(),
        "the index forgets the dead"
    );

    // Ids are not reused while the counter moves on.
    let next = npc(&mut app, temple, 1, "another guard");
    assert!(uid(&mut app, next) > id);
}

#[test]
fn a_reconnecting_player_keeps_the_account_id() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let first = player(&mut app, temple, "Alice", 42);
    assert_eq!(uid(&mut app, first), 42);
    app.world_mut().despawn(first);
    let second = player(&mut app, temple, "Alice", 42);
    assert_eq!(uid(&mut app, second), 42);
}

#[test]
fn vnums_come_from_the_trailing_number_of_the_converter_id() {
    assert_eq!(vnum_from_id("tba_npc_3001"), Some(3001));
    assert_eq!(vnum_from_id("tba.22"), Some(22));
    assert_eq!(vnum_from_id("tba_item_0"), Some(0));
    assert_eq!(vnum_from_id("tba_gold"), None);
    assert_eq!(vnum_from_id(""), None);
    assert_eq!(vnum_from_id("tba_npc_99999"), None, "beyond IDXTYPE");
    assert_eq!(vnum_from_id("tba_npc_65534"), Some(65534));
    assert_eq!(vnum_from_id("tba_npc_999999999999999999999"), None);
}

// -- list order --------------------------------------------------------------

#[test]
fn room_people_are_newest_first_as_char_to_room_prepends() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    let guard = npc(&mut app, temple, 1, "the guard");
    let bob = player(&mut app, temple, "Bob", 2);
    let (a, g, b) = (
        uid(&mut app, alice),
        uid(&mut app, guard),
        uid(&mut app, bob),
    );
    let room_id = uid(&mut app, temple);
    run(&mut app, |host| {
        assert_eq!(host.room_people(room_id), vec![b, g, a]);
        assert_eq!(host.in_room(g), Some(room_id));
        assert_eq!(host.in_room(room_id), None);
    });
}

#[test]
fn a_move_by_the_rest_of_the_game_puts_the_mover_at_the_head() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let altar = room(&mut app, 2);
    let alice = player(&mut app, temple, "Alice", 1);
    let guard = npc(&mut app, temple, 1, "the guard");
    let room_id = uid(&mut app, temple);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    // The stamp system needs one run to have seen the spawns.
    tick(&mut app, Duration::from_millis(100));
    run(&mut app, |host| {
        assert_eq!(host.room_people(room_id), vec![g, a]);
    });

    app.world_mut().get_mut::<Location>(alice).unwrap().room = altar;
    app.world_mut().get_mut::<Location>(alice).unwrap().room = temple;
    tick(&mut app, Duration::from_millis(100));
    run(&mut app, |host| {
        assert_eq!(
            host.room_people(room_id),
            vec![a, g],
            "the arrival is newest"
        );
    });
}

#[test]
fn a_move_by_the_host_is_stamped_directly() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    let guard = npc(&mut app, temple, 1, "the guard");
    let room_id = uid(&mut app, temple);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    run(&mut app, |host| {
        host.stamp_order(alice);
        assert_eq!(host.room_people(room_id), vec![a, g]);
        host.stamp_order(guard);
        assert_eq!(host.room_people(room_id), vec![g, a]);
    });
}

#[test]
fn room_contents_are_oldest_first_and_inventories_newest_first() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    let floor_1 = item_on_ground(&mut app, temple, 1, "first");
    let floor_2 = item_on_ground(&mut app, temple, 2, "second");
    let bag = item_carried(&mut app, alice, 3, "a bag");
    let coin = item_carried(&mut app, alice, 4, "a coin");
    let gem_in = item_inside(&mut app, bag, 5, "a gem");
    let pearl_in = item_inside(&mut app, bag, 6, "a pearl");

    let room_id = uid(&mut app, temple);
    let a = uid(&mut app, alice);
    let ids: Vec<_> = [floor_1, floor_2, bag, coin, gem_in, pearl_in]
        .into_iter()
        .map(|entity| uid(&mut app, entity))
        .collect();
    run(&mut app, |host| {
        assert_eq!(
            host.room_contents(room_id),
            vec![ids[0], ids[1]],
            "obj_to_room appends"
        );
        assert_eq!(
            host.carrying(a),
            vec![ids[3], ids[2]],
            "obj_to_char prepends"
        );
        assert_eq!(
            host.obj_contains(ids[2]),
            vec![ids[5], ids[4]],
            "obj_to_obj prepends"
        );
        assert_eq!(host.obj_in_obj(ids[4]), Some(ids[2]));
        assert_eq!(host.obj_carried_by(ids[2]), Some(a));
        assert_eq!(host.obj_carried_by(ids[4]), None);
        assert_eq!(host.in_room(ids[0]), Some(room_id));
        assert_eq!(host.in_room(ids[2]), None, "a carried item is in no room");
        assert_eq!(host.in_room(ids[4]), None);
    });
}

#[test]
fn the_world_lists_are_in_creation_order_newest_first() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let altar = room(&mut app, 2);
    let alice = player(&mut app, temple, "Alice", 1);
    let guard = npc(&mut app, temple, 1, "the guard");
    let bob = player(&mut app, altar, "Bob", 2);
    let sword = item_on_ground(&mut app, temple, 1, "a sword");
    let bag = item_carried(&mut app, alice, 2, "a bag");
    let ring = item_inside(&mut app, bag, 3, "a ring");
    let [a, g, b, s, bg, r] = [alice, guard, bob, sword, bag, ring].map(|e| uid(&mut app, e));

    // Moving does not reorder either list: the C never reorders them.
    tick(&mut app, Duration::from_millis(100));
    app.world_mut().get_mut::<Location>(alice).unwrap().room = altar;
    tick(&mut app, Duration::from_millis(100));
    run(&mut app, |host| {
        assert_eq!(host.character_list(), vec![b, g, a]);
        assert_eq!(host.object_list(), vec![r, bg, s]);
    });
}

#[test]
fn equipment_is_worn_not_carried() {
    let mut app = app();
    define_item(
        &mut app,
        10,
        TbaItemKind::Weapon,
        &[WearSlot::Wield],
        &["sword"],
    );
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    let pack = item_carried(&mut app, alice, 11, "a pack");
    let sword = item_worn(&mut app, alice, WearSlot::Wield, 10, "a sword");
    let helm = item_worn(&mut app, alice, WearSlot::Head, 12, "a helm");
    let (a, p, s, h) = (
        uid(&mut app, alice),
        uid(&mut app, pack),
        uid(&mut app, sword),
        uid(&mut app, helm),
    );
    run(&mut app, |host| {
        assert_eq!(host.carrying(a), vec![p], "worn items leave `carrying`");
        assert_eq!(host.equipment(a, WEAR_WIELD), Some(s));
        assert_eq!(host.equipment(a, dg_vm::lookup::WEAR_HEAD), Some(h));
        assert_eq!(host.equipment(a, WEAR_BODY), None);
        assert_eq!(host.equipment(a, 99), None);
        assert_eq!(host.equipment(a, -1), None);
        assert_eq!(host.obj_worn_by(s), Some(a));
        assert_eq!(host.obj_carried_by(s), None);
        assert_eq!(host.obj_worn_by(p), None);
        assert_eq!(host.obj_carried_by(p), Some(a));
        assert_eq!(host.in_room(s), None);
    });
}

#[test]
fn the_wear_vocabulary_maps_both_ways() {
    let slots = [
        WearSlot::Light,
        WearSlot::FingerR,
        WearSlot::FingerL,
        WearSlot::Neck1,
        WearSlot::Neck2,
        WearSlot::Body,
        WearSlot::Head,
        WearSlot::Legs,
        WearSlot::Feet,
        WearSlot::Hands,
        WearSlot::Arms,
        WearSlot::Shield,
        WearSlot::About,
        WearSlot::Waist,
        WearSlot::WristR,
        WearSlot::WristL,
        WearSlot::Wield,
        WearSlot::Hold,
    ];
    // `structs.h` WEAR_LIGHT..WEAR_HOLD are 0..=17, in this order.
    for (position, slot) in slots.into_iter().enumerate() {
        let position = i32::try_from(position).unwrap();
        assert_eq!(wear_position(slot), position);
        assert_eq!(wear_slot(position), Some(slot));
    }
    assert_eq!(wear_slot(18), None);
    assert_eq!(wear_slot(-1), None);

    // `ITEM_WEAR_*`: pairs share a bit; WIELD is 13 and HOLD 14.
    assert_eq!(item_wear_bit(WearSlot::FingerR), 1);
    assert_eq!(item_wear_bit(WearSlot::FingerL), 1);
    assert_eq!(item_wear_bit(WearSlot::Neck2), 2);
    assert_eq!(item_wear_bit(WearSlot::Body), 3);
    assert_eq!(item_wear_bit(WearSlot::Head), 4);
    assert_eq!(item_wear_bit(WearSlot::Legs), 5);
    assert_eq!(item_wear_bit(WearSlot::Feet), 6);
    assert_eq!(item_wear_bit(WearSlot::Hands), 7);
    assert_eq!(item_wear_bit(WearSlot::Arms), 8);
    assert_eq!(item_wear_bit(WearSlot::Shield), 9);
    assert_eq!(item_wear_bit(WearSlot::About), 10);
    assert_eq!(item_wear_bit(WearSlot::Waist), 11);
    assert_eq!(item_wear_bit(WearSlot::WristL), 12);
    assert_eq!(item_wear_bit(WearSlot::Wield), 13);
    assert_eq!(item_wear_bit(WearSlot::Hold), 14);
    assert_eq!(item_wear_bit(WearSlot::Light), 14);
}

#[test]
fn item_kinds_map_to_the_c_item_numbers() {
    // `structs.h` ITEM_UNDEFINED..ITEM_FOUNTAIN.
    let table = [
        (TbaItemKind::Undefined, 0),
        (TbaItemKind::Light, 1),
        (TbaItemKind::Scroll, 2),
        (TbaItemKind::Wand, 3),
        (TbaItemKind::Staff, 4),
        (TbaItemKind::Weapon, 5),
        (TbaItemKind::Furniture, 6),
        (TbaItemKind::Free, 7),
        (TbaItemKind::Treasure, 8),
        (TbaItemKind::Armor, 9),
        (TbaItemKind::Potion, 10),
        (TbaItemKind::Worn, 11),
        (TbaItemKind::Other, 12),
        (TbaItemKind::Trash, 13),
        (TbaItemKind::Free2, 14),
        (TbaItemKind::Container, 15),
        (TbaItemKind::Note, 16),
        (TbaItemKind::DrinkContainer, 17),
        (TbaItemKind::Key, 18),
        (TbaItemKind::Food, 19),
        (TbaItemKind::Money, 20),
        (TbaItemKind::Pen, 21),
        (TbaItemKind::Boat, 22),
        (TbaItemKind::Fountain, 23),
    ];
    for (kind, number) in table {
        assert_eq!(item_type_number(kind), number, "{kind:?}");
    }
}

// -- the world graph ---------------------------------------------------------

#[test]
fn names_keywords_vnums_and_levels() {
    let mut app = app();
    define_item(
        &mut app,
        3022,
        TbaItemKind::Weapon,
        &[WearSlot::Wield],
        &["sword", "long"],
    );
    define_npc_level(&mut app, 3024, 12);
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 1);
    app.world_mut().get_mut::<TbaProgress>(alice).unwrap().level = 9;
    let guard = npc(&mut app, temple, 3024, "The Temple Guard");
    let sword = item_on_ground(&mut app, temple, 3022, "a long sword");
    let stray = item_on_ground(&mut app, temple, 7, "a stray thing");
    let [r, a, g, s, y] = [temple, alice, guard, sword, stray].map(|e| uid(&mut app, e));

    run(&mut app, |host| {
        assert_eq!(host.name(r), "Room 3001");
        assert_eq!(host.name(a), "Alice");
        assert_eq!(host.name(g), "The Temple Guard");
        assert_eq!(host.name(s), "a long sword");

        assert_eq!(host.keywords(a), "Alice");
        assert_eq!(
            host.keywords(g),
            "temple guard",
            "lowercase words minus articles"
        );
        assert_eq!(host.keywords(s), "sword long", "from the prototype");
        assert_eq!(
            host.keywords(y),
            "stray thing",
            "derived when there is no prototype"
        );
        assert_eq!(host.keywords(r), "");

        assert_eq!(host.vnum(r), 3001);
        assert_eq!(host.vnum(g), 3024);
        assert_eq!(host.vnum(s), 3022);
        assert_eq!(host.vnum(a), -1);
        assert_eq!(host.vnum(123), -1);

        assert_eq!(host.level(a), 9);
        assert_eq!(host.level(g), 12);
        assert_eq!(host.level(s), 0);

        assert_eq!(host.real_room(3001), Some(r));
        assert_eq!(host.real_room(3002), None);
        assert_eq!(host.real_room(-1), None);
        assert_eq!(host.real_room(i32::MAX), None);
    });
    assert_eq!(
        npc_keywords("a"),
        "a",
        "nothing but an article is kept whole"
    );
    assert_eq!(npc_keywords("Mr. Smith, the baker"), "mr smith baker");
}

#[test]
fn object_type_and_wear_flags_come_from_the_prototype() {
    let mut app = app();
    define_item(&mut app, 1, TbaItemKind::Container, &[], &["bag"]);
    define_item(
        &mut app,
        2,
        TbaItemKind::Armor,
        &[WearSlot::FingerR, WearSlot::FingerL, WearSlot::Hold],
        &["ring"],
    );
    let temple = room(&mut app, 1);
    let bag = item_on_ground(&mut app, temple, 1, "a bag");
    let ring = item_on_ground(&mut app, temple, 2, "a ring");
    let nameless = item_on_ground(&mut app, temple, 3, "nothing in particular");
    let [r, b, g, n] = [temple, bag, ring, nameless].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        assert_eq!(host.obj_type(b), dg_vm::types::ITEM_CONTAINER);
        assert_eq!(host.obj_type(g), 9);
        assert_eq!(host.obj_type(n), 0);
        assert_eq!(host.obj_type(r), 0, "a room is not an object");
        assert_eq!(host.wear_flags(b), 1, "take only");
        assert_eq!(host.wear_flags(g), 1 | 1 << 1 | 1 << 14);
        assert_eq!(host.wear_flags(n), 1);
        assert_eq!(host.wear_flags(r), 0);
        assert!(dg_vm::lookup::can_wear_on_pos(
            &*host,
            g,
            dg_vm::lookup::WEAR_FINGER_L
        ));
        assert!(dg_vm::lookup::can_wear_on_pos(&*host, g, WEAR_HOLD));
        assert!(dg_vm::lookup::can_wear_on_pos(&*host, g, WEAR_LIGHT));
        assert!(!dg_vm::lookup::can_wear_on_pos(&*host, g, WEAR_BODY));
    });
}

#[test]
fn valid_targets_are_npcs_and_playing_players() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    let idle = player(&mut app, temple, "Idle", 2);
    app.world_mut().get_mut::<Session>(idle).unwrap().state = SessionState::CreateCharacter;
    let guard = npc(&mut app, temple, 1, "guard");
    let sword = item_on_ground(&mut app, temple, 1, "sword");
    let [a, i, g, s] = [alice, idle, guard, sword].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        for god in [false, true] {
            assert!(host.valid_dg_target(a, god));
            assert!(host.valid_dg_target(g, god));
            assert!(!host.valid_dg_target(i, god));
            assert!(!host.valid_dg_target(s, god));
            assert!(!host.valid_dg_target(404, god));
        }
        assert!(host.is_npc(g));
        assert!(!host.is_npc(a));
        assert!(!host.is_npc(s));
    });
}

#[test]
fn death_is_a_script_extraction_or_a_dead_position() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let marked = npc(&mut app, temple, 1, "marked");
    let slain = npc(&mut app, temple, 2, "slain");
    let well = npc(&mut app, temple, 3, "well");
    let [m, s, w] = [marked, slain, well].map(|e| uid(&mut app, e));
    app.world_mut().entity_mut(marked).insert(DgDead);
    let mut fighter = fallback_combatant();
    fighter.position = Position::Dead;
    app.world_mut().entity_mut(slain).insert(fighter);
    let mut fighter = fallback_combatant();
    fighter.position = Position::Standing;
    app.world_mut().entity_mut(well).insert(fighter);
    // Read without running the pass that would sweep the marked mob.
    let world = app.world_mut();
    let mut host = mib_tba::dg::host::EcsHost { world };
    assert!(host.is_dead(m));
    assert!(host.is_dead(s));
    assert!(!host.is_dead(w));
    assert!(!host.is_dead(404));
    let _ = &mut host;
}

#[test]
fn can_see_uses_the_stdlib_perception_rule() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    let rogue = npc(&mut app, temple, 1, "a rogue");
    let seer = npc(&mut app, temple, 2, "a seer");
    let sword = item_on_ground(&mut app, temple, 1, "sword");
    app.world_mut().entity_mut(rogue).insert(Concealment {
        hidden: true,
        invisible: false,
        stealth: 5,
    });
    app.world_mut().entity_mut(seer).insert(Perception {
        sight: 0,
        detect: 9,
    });
    let [a, r, s, w] = [alice, rogue, seer, sword].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        assert!(!host.can_see(a, r), "hidden beyond Alice's detection");
        assert!(host.can_see(s, r), "detection 9 beats stealth 5");
        assert!(host.can_see(r, r), "everyone sees themselves");
        assert!(host.can_see(a, s));
        assert!(host.can_see(a, w), "objects are always visible");
    });
}

#[test]
fn script_memory_round_trips() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let alice = player(&mut app, temple, "Alice", 1);
    let [g, a] = [guard, alice].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        assert!(host.script_mem(g).is_empty());
        let memory = vec![
            dg_vm::types::ScriptMemory {
                id: a,
                cmd: Some("say hello".to_owned()),
            },
            dg_vm::types::ScriptMemory { id: 77, cmd: None },
        ];
        host.set_script_mem(g, memory.clone());
        assert_eq!(host.script_mem(g), memory);
        host.set_script_mem(g, Vec::new());
        assert!(host.script_mem(g).is_empty());
        host.set_script_mem(404, memory);
    });
}

// -- base host ---------------------------------------------------------------

#[test]
fn rand_number_stays_in_range_and_swaps_reversed_bounds() {
    let mut app = app();
    run(&mut app, |host| {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..200 {
            let roll = host.rand_number(1, 6);
            assert!((1..=6).contains(&roll));
            seen.insert(roll);
            assert!((-3..=3).contains(&host.rand_number(3, -3)));
            assert_eq!(host.rand_number(5, 5), 5);
        }
        assert_eq!(seen.len(), 6, "every face comes up");
        let _ = host.rand_number(i64::MIN, i64::MAX);
        let _ = host.rand_number(i64::MAX, i64::MIN);
    });
}

#[test]
fn mud_time_and_pulse_come_from_the_clocks() {
    let mut app = app();
    {
        let mut clock = app.world_mut().resource_mut::<TbaObjectClock>();
        clock.hour_of_day = 13;
        clock.day = 20;
        clock.month = 5;
        clock.year = 42;
    }
    app.world_mut().resource_mut::<DgPulse>().pulse = 1234;
    run(&mut app, |host| {
        let time = host.mud_time();
        assert_eq!(
            (time.hours, time.day, time.month, time.year),
            (13, 20, 5, 42)
        );
        assert_eq!(host.pulse(), 1234);
        assert!(!host.script_players());
    });
}

#[test]
fn script_log_and_unsupported_are_counted_and_bounded() {
    let mut app = app();
    run(&mut app, |host| {
        host.script_log("Trigger: x, VNum 1. something broke");
        host.script_log("again");
        host.unsupported("mhunt", "no tracking");
        host.unsupported("mhunt", "still none");
        host.unsupported("char.pref", "");
    });
    let log = app.world().resource::<DgLog>();
    assert_eq!(log.count("script_log"), 2);
    assert_eq!(log.count("mhunt"), 2);
    assert_eq!(log.count("char.pref"), 1);
    assert_eq!(log.count("never"), 0);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], "Trigger: x, VNum 1. something broke");
    assert!(lines.contains(&"unsupported mhunt: no tracking"));

    run(&mut app, |host| {
        for n in 0..1000 {
            host.script_log(&format!("line {n}"));
        }
    });
    let log = app.world().resource::<DgLog>();
    assert_eq!(log.lines().count(), 256, "the ring is bounded");
    assert_eq!(log.lines().last(), Some("line 999"));
}

#[test]
fn a_script_is_created_on_demand_and_removed_with_its_globals() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        assert!(host.script(g).is_none());
        assert!(host.script_mut(g).is_none());
        host.ensure_script(g)
            .unwrap()
            .global_vars
            .add_var("x", "1", 0);
        assert_eq!(
            host.script(g).unwrap().global_vars.get("x").unwrap().value,
            "1"
        );
        let removed = host.remove_script(g).unwrap();
        assert_eq!(removed.global_vars.vars.len(), 1);
        assert!(host.script(g).is_none());
        assert!(host.remove_script(g).is_none());
        assert!(host.ensure_script(404).is_none());
    });
}

// -- prototypes --------------------------------------------------------------

fn def(vnum: i32, attach: &str, flags: &str) -> TriggerDef {
    TriggerDef {
        vnum,
        name: format!("t{vnum}"),
        attach_type: attach.to_owned(),
        flags: flags.to_owned(),
        types: Vec::new(),
        narg: 0,
        arglist: String::new(),
        script: "set a 1".to_owned(),
    }
}

#[test]
fn records_without_an_attach_type_or_flags_are_skipped_and_counted() {
    let triggers = [
        def(1, "mob", "g"),
        def(2, "", "g"),
        def(3, "monster", "g"),
        def(4, "obj", ""),
        def(5, "wld", "  "),
        def(6, "wld", "ab"),
    ]
    .into_iter()
    .map(|d| (TriggerId::from_vnum(d.vnum), d))
    .collect();
    let attachments = [
        (
            "npc:tba_npc_1".to_owned(),
            vec![TriggerId::from_vnum(1), TriggerId::from_vnum(2)],
        ),
        (
            "room:tba.5".to_owned(),
            vec![TriggerId::from_vnum(6), TriggerId::from_vnum(99)],
        ),
    ]
    .into_iter()
    .collect();
    let protos = DgProtos::build(&triggers, &attachments);
    assert_eq!(protos.len(), 2);
    assert_eq!(protos.skipped_records, 4);
    assert_eq!(
        protos.skipped_attachments, 2,
        "one skipped record, one unknown id"
    );
    assert_eq!(protos.owner("npc:tba_npc_1"), Some(&[1][..]));
    assert_eq!(protos.owner("room:tba.5"), Some(&[6][..]));
    assert!(protos.get(2).is_none());

    let wld = protos.get(6).unwrap();
    assert_eq!(wld.trigger_type, 0b11, "type letters a and b");
    assert_eq!(wld.attach_type, dg_vm::AttachType::Wld);
    assert_eq!(protos.get(1).unwrap().attach_type, dg_vm::AttachType::Mob);
}

#[test]
fn a_numeric_type_field_is_the_number() {
    let mut protos = DgProtos::default();
    let mut numeric = def(7, "mob", "8");
    numeric.script = "say hi".to_owned();
    assert!(protos.add(&numeric));
    assert_eq!(protos.get(7).unwrap().trigger_type, 8);
}

#[test]
fn proto_scripts_attach_in_attachment_order_once() {
    let mut app = app();
    for vnum in [100, 101, 102] {
        define(&mut app, vnum, "mob", "g", 0, "set a 1");
    }
    define(&mut app, 110, "obj", "g", 0, "set a 1");
    define(&mut app, 120, "wld", "g", 0, "set a 1");
    attach(&mut app, "npc:tba_npc_3001", &[102, 100, 101, 100]);
    attach(&mut app, "item:tba_item_5", &[110]);
    attach(&mut app, "room:tba.3001", &[120]);

    let temple = room(&mut app, 3001);
    let empty = room(&mut app, 3002);
    let guard = npc(&mut app, temple, 3001, "guard");
    let bystander = npc(&mut app, temple, 3002, "bystander");
    let sword = item_on_ground(&mut app, temple, 5, "sword");
    let [t, e, g, b, s] = [temple, empty, guard, bystander, sword].map(|x| uid(&mut app, x));

    assert_eq!(trigger_vnums(&mut app, g), vec![102, 100, 101, 100]);
    assert_eq!(trigger_vnums(&mut app, s), vec![110]);
    assert_eq!(trigger_vnums(&mut app, t), vec![120]);
    assert!(trigger_vnums(&mut app, e).is_empty());
    assert!(trigger_vnums(&mut app, b).is_empty());
    assert!(app.world().get::<DgScript>(bystander).is_none());

    // A second pass leaves them alone, and a script detach stays detached.
    run(&mut app, |host| {
        let id = host.script(g).unwrap().triggers[0].id;
        host.script_mut(g).unwrap().triggers.retain(|t| t.id != id);
    });
    assert_eq!(trigger_vnums(&mut app, g), vec![100, 101, 100]);
}

#[test]
fn load_triggers_fire_once_for_each_new_mob_and_object() {
    let mut app = app();
    // `n` is bit 13: the load type for both mobs and objects.
    define(
        &mut app,
        200,
        "mob",
        "n",
        100,
        "set loaded 1\nglobal loaded",
    );
    define(
        &mut app,
        201,
        "obj",
        "n",
        100,
        "set loaded 1\nglobal loaded",
    );
    attach(&mut app, "npc:tba_npc_1", &[200]);
    attach(&mut app, "item:tba_item_1", &[201]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let sword = item_on_ground(&mut app, temple, 1, "sword");
    let [g, s] = [guard, sword].map(|x| uid(&mut app, x));

    assert_eq!(global(&mut app, g, "loaded").as_deref(), Some("1"));
    assert_eq!(global(&mut app, s, "loaded").as_deref(), Some("1"));

    run(&mut app, |host| {
        host.script_mut(g)
            .unwrap()
            .global_vars
            .add_var("loaded", "seen", 0);
    });
    run(&mut app, |_| {});
    assert_eq!(
        global(&mut app, g, "loaded").as_deref(),
        Some("seen"),
        "the load trigger does not fire again"
    );
}

#[test]
fn a_nested_call_does_not_attach_or_sweep_and_init_entity_does_not_fire_load() {
    let mut app = app();
    define(
        &mut app,
        200,
        "mob",
        "n",
        100,
        "set loaded 1\nglobal loaded",
    );
    attach(&mut app, "npc:tba_npc_1", &[200]);
    let temple = room(&mut app, 1);
    run(&mut app, |_| {});

    let doomed = npc(&mut app, temple, 2, "doomed");
    let mut late = None;
    run(&mut app, |host| {
        host.world.entity_mut(doomed).insert(DgDead);
        let entity = host
            .world
            .spawn((
                Npc {
                    name: "late".to_owned(),
                    persona: String::new(),
                },
                NpcKey(NpcId::from("tba_npc_1")),
                Location { room: temple },
            ))
            .id();
        late = Some(entity);
        with_host(host.world, |inner| {
            assert!(
                inner.world.get::<DgScript>(entity).is_none(),
                "no attach pass"
            );
        });
        assert!(
            host.world.get_entity(doomed).is_ok(),
            "no sweep inside the outer call"
        );
    });
    let late = late.expect("spawned");
    assert!(app.world().get_entity(doomed).is_err(), "swept at the end");

    // The attach pass ran before the entity existed, so it waits for the next
    // call; `init_entity` does it at once, without the load trigger.
    assert!(app.world().get::<DgScript>(late).is_none());
    let id = init_entity(app.world_mut(), late).expect("initialised");
    assert_eq!(trigger_vnums(&mut app, id), vec![200]);
    assert_eq!(
        global(&mut app, id, "loaded"),
        None,
        "init_entity fires nothing"
    );
    run(&mut app, |_| {});
    assert_eq!(global(&mut app, id, "loaded"), None, "already initialised");
}

// -- running scripts ---------------------------------------------------------

#[test]
fn a_script_sets_a_global_waits_and_resumes_with_its_variables() {
    let mut app = app();
    define(
        &mut app,
        300,
        "mob",
        "g",
        0,
        "set keep hello\nset marker start\nglobal marker\nwait 2 s\nset seen %keep%\nglobal seen",
    );
    attach(&mut app, "npc:tba_npc_1", &[300]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);

    run_trigger(&mut app, g, 300);
    assert_eq!(global(&mut app, g, "marker").as_deref(), Some("start"));
    assert_eq!(global(&mut app, g, "seen"), None);
    {
        let waits = &app.world().resource::<DgWaits>().waits;
        assert_eq!(waits.len(), 1);
        assert_eq!(
            (waits[0].owner, waits[0].due),
            (g, 20),
            "wait 2 s is 20 pulses"
        );
    }

    tick_pulses(&mut app, 19);
    assert_eq!(app.world().resource::<DgPulse>().pulse, 19);
    assert_eq!(global(&mut app, g, "seen"), None, "not due yet");
    tick_pulses(&mut app, 1);
    assert_eq!(global(&mut app, g, "seen").as_deref(), Some("hello"));
    assert!(app.world().resource::<DgWaits>().waits.is_empty());
    assert!(log_lines(&app).is_empty(), "{:?}", log_lines(&app));
}

#[test]
fn several_waits_resume_earliest_first_in_one_tick() {
    let mut app = app();
    define(
        &mut app,
        310,
        "mob",
        "g",
        0,
        "wait 3 s\neval n %n%\nset n x\nwait 1 s\nset done 1\nglobal done",
    );
    define(
        &mut app,
        311,
        "mob",
        "g",
        0,
        "wait 1 s\nset first 1\nglobal first",
    );
    attach(&mut app, "npc:tba_npc_1", &[310, 311]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 310);
    run_trigger(&mut app, g, 311);
    let due: Vec<i64> = app
        .world()
        .resource::<DgWaits>()
        .waits
        .iter()
        .map(|wait| wait.due)
        .collect();
    assert_eq!(due, vec![30, 10]);

    // One long tick makes both due; the earlier resumes first, and the
    // follow-up wait of the later one is queued after the current pulse.
    tick_pulses(&mut app, 35);
    assert_eq!(global(&mut app, g, "first").as_deref(), Some("1"));
    assert_eq!(global(&mut app, g, "done"), None);
    assert_eq!(app.world().resource::<DgWaits>().waits[0].due, 45);
    tick_pulses(&mut app, 10);
    assert_eq!(global(&mut app, g, "done").as_deref(), Some("1"));
}

#[test]
fn a_wait_whose_owner_is_gone_is_dropped() {
    let mut app = app();
    define(
        &mut app,
        300,
        "mob",
        "g",
        0,
        "wait 1 s\nset late 1\nglobal late",
    );
    attach(&mut app, "npc:tba_npc_1", &[300]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 300);
    assert_eq!(app.world().resource::<DgWaits>().waits.len(), 1);

    app.world_mut().despawn(guard);
    tick_pulses(&mut app, 10);
    assert!(app.world().resource::<DgWaits>().waits.is_empty());
    assert!(
        log_lines(&app).is_empty(),
        "dropped quietly: {:?}",
        log_lines(&app)
    );
}

#[test]
fn a_script_can_attach_and_detach_triggers() {
    let mut app = app();
    // The first mob in a fresh world is the first id above MOB_ID_BASE; an
    // `attach` target is the numeric id, as `%self.id%` expands.
    let g = MOB_ID_BASE + 1;
    define(
        &mut app,
        400,
        "mob",
        "g",
        0,
        &format!("attach 401 {g}\nattach 402 {g}"),
    );
    define(&mut app, 401, "mob", "g", 0, "set a 1");
    define(&mut app, 402, "mob", "g", 0, "set a 1");
    define(&mut app, 403, "mob", "g", 0, &format!("detach 401 {g}"));
    define(&mut app, 404, "mob", "g", 0, &format!("detach all {g}"));
    attach(&mut app, "npc:tba_npc_1", &[400, 403, 404]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    assert_eq!(uid(&mut app, guard), g);

    run_trigger(&mut app, g, 400);
    assert_eq!(trigger_vnums(&mut app, g), vec![400, 403, 404, 401, 402]);
    run_trigger(&mut app, g, 403);
    assert_eq!(trigger_vnums(&mut app, g), vec![400, 403, 404, 402]);
    run_trigger(&mut app, g, 404);
    run(&mut app, |host| {
        assert!(host.script(g).is_none(), "detach all extracts the script");
    });
    assert!(app.world().get::<DgScript>(guard).is_none());
    assert!(log_lines(&app).is_empty(), "{:?}", log_lines(&app));
}

#[test]
fn detaching_cancels_the_triggers_wait() {
    let mut app = app();
    let g = MOB_ID_BASE + 1;
    define(
        &mut app,
        410,
        "mob",
        "g",
        0,
        "wait 5 s\nset late 1\nglobal late",
    );
    define(&mut app, 411, "mob", "g", 0, &format!("detach 410 {g}"));
    attach(&mut app, "npc:tba_npc_1", &[410, 411]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    assert_eq!(uid(&mut app, guard), g);
    run_trigger(&mut app, g, 410);
    assert_eq!(app.world().resource::<DgWaits>().waits.len(), 1);
    run_trigger(&mut app, g, 411);
    assert!(app.world().resource::<DgWaits>().waits.is_empty());
}

#[test]
fn random_triggers_fire_from_the_pulse_system_on_their_period() {
    let mut app = app();
    // Type `ab` is GLOBAL | RANDOM, so the mob needs no player in its zone;
    // narg 100 always wins the percent roll.
    define(&mut app, 500, "mob", "ab", 100, "set ran 1\nglobal ran");
    define(&mut app, 501, "obj", "b", 100, "set ran 1\nglobal ran");
    attach(&mut app, "npc:tba_npc_1", &[500]);
    attach(&mut app, "item:tba_item_1", &[501]);
    let temple = room(&mut app, 1);
    player(&mut app, temple, "Alice", 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let sword = item_on_ground(&mut app, temple, 1, "sword");
    let [g, s] = [guard, sword].map(|x| uid(&mut app, x));

    // PULSE_DG_SCRIPT is 13 seconds: pulse 130.
    tick_pulses(&mut app, 129);
    assert_eq!(global(&mut app, g, "ran"), None);
    assert_eq!(global(&mut app, s, "ran"), None);
    tick_pulses(&mut app, 1);
    assert_eq!(global(&mut app, g, "ran").as_deref(), Some("1"));
    assert_eq!(global(&mut app, s, "ran").as_deref(), Some("1"));

    // And again at pulse 260, not before.
    run(&mut app, |host| {
        for owner in [g, s] {
            host.script_mut(owner)
                .unwrap()
                .global_vars
                .remove_var("ran");
        }
    });
    tick_pulses(&mut app, 129);
    assert_eq!(global(&mut app, g, "ran"), None);
    tick_pulses(&mut app, 1);
    assert_eq!(global(&mut app, g, "ran").as_deref(), Some("1"));
}

#[test]
fn many_pulses_in_one_tick_run_the_random_sweep_once() {
    let mut app = app();
    define(
        &mut app,
        500,
        "mob",
        "ab",
        100,
        "eval count %count% + 1\nglobal count",
    );
    attach(&mut app, "npc:tba_npc_1", &[500]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.ensure_script(g)
            .unwrap()
            .global_vars
            .add_var("count", "0", 0);
    });
    tick_pulses(&mut app, 1000);
    assert_eq!(global(&mut app, g, "count").as_deref(), Some("1"));
}

#[test]
fn time_triggers_fire_when_the_mud_hour_changes() {
    let mut app = app();
    // Type `at` is GLOBAL | TIME; narg is the hour to fire at.
    define(&mut app, 510, "mob", "at", 6, "set fired 1\nglobal fired");
    define(&mut app, 511, "mob", "at", 7, "set fired 1\nglobal fired");
    attach(&mut app, "npc:tba_npc_1", &[510]);
    attach(&mut app, "npc:tba_npc_2", &[511]);
    let temple = room(&mut app, 1);
    let early = npc(&mut app, temple, 1, "early");
    let late = npc(&mut app, temple, 2, "late");
    let [e, l] = [early, late].map(|x| uid(&mut app, x));

    app.world_mut().resource_mut::<TbaObjectClock>().hour_of_day = 5;
    tick_pulses(&mut app, 1);
    assert_eq!(
        global(&mut app, e, "fired"),
        None,
        "the first tick only notes the hour"
    );

    app.world_mut().resource_mut::<TbaObjectClock>().hour_of_day = 6;
    tick_pulses(&mut app, 1);
    assert_eq!(global(&mut app, e, "fired").as_deref(), Some("1"));
    assert_eq!(global(&mut app, l, "fired"), None, "its narg is 7");

    run(&mut app, |host| {
        host.script_mut(e).unwrap().global_vars.remove_var("fired");
    });
    tick_pulses(&mut app, 5);
    assert_eq!(
        global(&mut app, e, "fired"),
        None,
        "the hour did not change"
    );

    app.world_mut().resource_mut::<TbaObjectClock>().hour_of_day = 7;
    tick_pulses(&mut app, 1);
    assert_eq!(global(&mut app, l, "fired").as_deref(), Some("1"));
}

// -- player variables --------------------------------------------------------

#[test]
fn a_remote_variable_persists_and_raises_the_quest_flag() {
    let mut app = app();
    for (vnum, value) in [(600, "1"), (601, "0"), (602, "2")] {
        define(
            &mut app,
            vnum,
            "mob",
            "g",
            0,
            &format!("set quest_done {value}\nremote quest_done 5"),
        );
    }
    define(
        &mut app,
        603,
        "mob",
        "g",
        0,
        "set Mixed_Case yes\nremote Mixed_Case 5",
    );
    attach(&mut app, "npc:tba_npc_1", &[600, 601, 602, 603]);
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 5);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);
    let remote = |app: &App| {
        app.world()
            .get::<TbaTriggerVars>(alice)
            .map(|vars| vars.remote.clone())
            .unwrap_or_default()
    };

    run_trigger(&mut app, g, 600);
    assert_eq!(
        remote(&app).get("quest_done").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        quest_flags(&mut app),
        vec![(alice, "quest_done".to_owned())]
    );

    // Still truthy: persisted, but not a new flag.
    run_trigger(&mut app, g, 602);
    assert_eq!(
        remote(&app).get("quest_done").map(String::as_str),
        Some("2")
    );
    assert!(quest_flags(&mut app).is_empty());

    // Falsy: persisted, no flag.
    run_trigger(&mut app, g, 601);
    assert_eq!(
        remote(&app).get("quest_done").map(String::as_str),
        Some("0")
    );
    assert!(quest_flags(&mut app).is_empty());

    // Falsy to truthy again raises it again.
    run_trigger(&mut app, g, 600);
    assert_eq!(
        quest_flags(&mut app),
        vec![(alice, "quest_done".to_owned())]
    );

    // Names are persisted lowercased, and `global` stays empty.
    run_trigger(&mut app, g, 603);
    assert_eq!(
        remote(&app).get("mixed_case").map(String::as_str),
        Some("yes")
    );
    assert_eq!(
        quest_flags(&mut app),
        vec![(alice, "mixed_case".to_owned())]
    );
    assert!(
        app.world()
            .get::<TbaTriggerVars>(alice)
            .unwrap()
            .global
            .is_empty()
    );
}

#[test]
fn a_player_is_seeded_from_the_old_two_map_format() {
    let mut app = app();
    define(&mut app, 600, "mob", "g", 0, "set d 4\nremote d 5");
    attach(&mut app, "npc:tba_npc_1", &[600]);
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 5);
    app.world_mut().entity_mut(alice).insert(TbaTriggerVars {
        remote: [("a", "1"), ("B", "2")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into(),
        global: [("b", "9"), ("c", "3")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into(),
    });
    let guard = npc(&mut app, temple, 1, "guard");
    let [g, a] = [guard, alice].map(|x| uid(&mut app, x));

    run(&mut app, |_| {});
    assert_eq!(global(&mut app, a, "a").as_deref(), Some("1"));
    assert_eq!(
        global(&mut app, a, "b").as_deref(),
        Some("2"),
        "remote wins a clash"
    );
    assert_eq!(global(&mut app, a, "c").as_deref(), Some("3"));
    assert!(
        quest_flags(&mut app).is_empty(),
        "loading saved variables is not a quest event"
    );
    assert_eq!(
        app.world()
            .get::<TbaTriggerVars>(alice)
            .unwrap()
            .global
            .len(),
        2,
        "an unchanged player is not rewritten"
    );

    run_trigger(&mut app, g, 600);
    let vars = app.world().get::<TbaTriggerVars>(alice).unwrap();
    assert_eq!(
        vars.remote
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
        vec![("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")]
    );
    assert!(
        vars.global.is_empty(),
        "global is empty from the first write"
    );
    assert_eq!(quest_flags(&mut app), vec![(alice, "d".to_owned())]);
}

#[test]
fn a_player_waiting_for_restore_is_not_seeded_early() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 5);
    app.world_mut()
        .entity_mut(alice)
        .insert(PendingBundleRestore);
    run(&mut app, |_| {});
    assert!(app.world().get::<DgScript>(alice).is_none());
    app.world_mut()
        .entity_mut(alice)
        .remove::<PendingBundleRestore>();
    run(&mut app, |_| {});
    assert!(app.world().get::<DgScript>(alice).is_some());
}

// -- extraction --------------------------------------------------------------

#[test]
fn extracted_characters_are_swept_when_the_outermost_call_ends() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let doomed = npc(&mut app, temple, 1, "doomed");
    let survivor = npc(&mut app, temple, 2, "survivor");
    let alice = player(&mut app, temple, "Alice", 5);
    let held = item_carried(&mut app, doomed, 1, "a bag");
    let inside = item_inside(&mut app, held, 2, "a gem");
    let worn = item_worn(&mut app, doomed, WearSlot::Head, 3, "a helm");
    let kept = item_carried(&mut app, survivor, 4, "a coin");
    let floor = item_on_ground(&mut app, temple, 5, "a rock");
    let d = uid(&mut app, doomed);

    run(&mut app, |host| {
        host.schedule_wait(d, 1, 50);
        host.world.entity_mut(doomed).insert(DgDead);
        host.world.entity_mut(alice).insert(DgDead);
        assert!(host.is_dead(d));
        assert!(host.exists(d), "still there until the end of the pulse");
        assert!(host.room_people(ROOM_ID_BASE + 1).contains(&d));
        // A nested call is not the end of the pulse.
        with_host(host.world, |_| {});
        assert!(host.world.get_entity(doomed).is_ok());
    });

    let world = app.world();
    for gone in [doomed, held, inside, worn] {
        assert!(world.get_entity(gone).is_err(), "{gone:?} should be swept");
    }
    for stays in [survivor, kept, floor, alice, temple] {
        assert!(world.get_entity(stays).is_ok(), "{stays:?} should remain");
    }
    assert!(
        world.get::<DgDead>(alice).is_none(),
        "a player is never despawned, only unmarked"
    );
    assert!(world.resource::<DgWaits>().waits.is_empty());
}

#[test]
fn the_sweep_waits_for_the_vm_to_go_idle() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let doomed = npc(&mut app, temple, 1, "doomed");
    run(&mut app, |host| {
        host.world.entity_mut(doomed).insert(DgDead);
        host.vm_state().depth = 1;
    });
    assert!(
        app.world().get_entity(doomed).is_ok(),
        "a script is still running"
    );
    run(&mut app, |host| {
        host.vm_state().depth = 0;
    });
    assert!(app.world().get_entity(doomed).is_err());
}

// -- the real content --------------------------------------------------------

fn engine_pack(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../MIB/packs")
        .join(name)
}

fn this_pack() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn registry() -> SectionRegistry {
    let modules: [&dyn MibModule; 2] = [&StdlibModule, &TbaModule];
    registry_for(&modules)
}

fn boot() -> App {
    let roots = [engine_pack("stdlib"), this_pack()];
    let mut app = App::new();
    app.add_plugins(StdlibPlugin);
    app.add_plugins(TbaPlugin);
    app.insert_resource(Modules::new(vec![
        Box::new(StdlibModule),
        Box::new(TbaModule),
    ]));
    app.insert_resource(ContentRegistry(registry()));
    let mut config = Config::default();
    config.game.packs = roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    app.insert_resource(config);
    app.world_mut().run_schedule(Startup);
    app
}

#[test]
fn the_real_pack_loads_every_trigger_and_resolves_every_attachment() {
    let mut app = boot();
    let content = app.world().resource::<TbaContent>().clone();
    let protos = app.world().resource::<DgProtos>().clone();

    assert_eq!(content.triggers.len(), 1461);
    assert_eq!(protos.len(), 1461, "every prototype loaded");
    assert_eq!(protos.skipped_records, 0);
    assert_eq!(protos.skipped_attachments, 0);
    assert!(!content.trigger_attachments.is_empty());
    assert_eq!(protos.owner_count(), content.trigger_attachments.len());

    for (owner, list) in &content.trigger_attachments {
        let vnums = protos
            .owner(owner)
            .unwrap_or_else(|| panic!("{owner} has no prototype list"));
        let expected: Vec<i32> = list.iter().map(|id| content.triggers[id].vnum).collect();
        assert_eq!(vnums, expected.as_slice(), "{owner} keeps its order");
        for vnum in vnums {
            assert!(protos.get(*vnum).is_some(), "{owner} -> {vnum}");
        }
    }

    // Every room in the booted world carries exactly what its list says.
    with_host(app.world_mut(), |_| {});
    let mut rooms = app.world_mut().query::<(Entity, &Room)>();
    let rooms: Vec<(Entity, String)> = rooms
        .iter(app.world())
        .map(|(entity, room)| (entity, format!("room:{}", room.key)))
        .collect();
    let mut with_scripts = 0;
    for (entity, key) in rooms {
        let expected = protos.owner(&key).filter(|list| !list.is_empty());
        let actual: Option<Vec<i32>> = app
            .world()
            .get::<DgScript>(entity)
            .map(|script| script.0.triggers.iter().map(|t| t.proto.vnum).collect());
        assert_eq!(actual.as_deref(), expected, "{key}");
        with_scripts += usize::from(actual.is_some());
    }
    let room_lists = content
        .trigger_attachments
        .iter()
        .filter(|(owner, list)| owner.starts_with("room:") && !list.is_empty())
        .count();
    assert_eq!(with_scripts, room_lists, "every room list found its room");
}

// -- review regressions -------------------------------------------------------

/// Spawn-then-move in one fixed interval: the entity got its first stamp after
/// the stamp system ran, and the game then moved it. handler.c `char_to_room`
/// prepends, so the arrival is the head.
#[test]
fn a_game_move_after_the_first_stamp_still_heads_the_room() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let altar = room(&mut app, 2);
    let old = npc(&mut app, temple, 1, "old guard");
    let room_id = uid(&mut app, temple);
    tick(&mut app, Duration::from_millis(100));

    let alice = player(&mut app, altar, "Alice", 1);
    let young = npc(&mut app, temple, 2, "young guard");
    tick(&mut app, Duration::from_millis(100));
    let (a, y, o) = (
        uid(&mut app, alice),
        uid(&mut app, young),
        uid(&mut app, old),
    );
    app.world_mut().get_mut::<Location>(alice).unwrap().room = temple;
    tick(&mut app, Duration::from_millis(100));
    run(&mut app, |host| {
        assert_eq!(host.room_people(room_id), vec![a, y, o]);
    });
}

/// A host stamp followed by a later game move: only the move newer than the
/// stamp restamps.
#[test]
fn a_game_move_after_a_host_stamp_is_newer_than_the_stamp() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let altar = room(&mut app, 2);
    let first = npc(&mut app, temple, 1, "first guard");
    let alice = player(&mut app, altar, "Alice", 1);
    let room_id = uid(&mut app, temple);
    let (f, a) = (uid(&mut app, first), uid(&mut app, alice));
    tick(&mut app, Duration::from_millis(100));

    run(&mut app, |host| host.stamp_order(alice));
    let second = npc(&mut app, temple, 2, "second guard");
    let s = uid(&mut app, second);
    app.world_mut().get_mut::<Location>(alice).unwrap().room = temple;
    tick(&mut app, Duration::from_millis(100));
    run(&mut app, |host| {
        assert_eq!(host.room_people(room_id), vec![a, s, f]);
    });
}

/// A move the host made and stamped is not stamped again by the system.
#[test]
fn a_host_stamped_move_keeps_its_stamp() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let altar = room(&mut app, 2);
    let alice = player(&mut app, altar, "Alice", 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let room_id = uid(&mut app, temple);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    tick(&mut app, Duration::from_millis(100));

    app.world_mut().get_mut::<Location>(alice).unwrap().room = temple;
    run(&mut app, |host| host.stamp_order(alice));
    let late = npc(&mut app, temple, 2, "late guard");
    let l = uid(&mut app, late);
    tick(&mut app, Duration::from_millis(100));
    run(&mut app, |host| {
        assert_eq!(host.room_people(room_id), vec![l, a, g]);
    });
}

/// handler.c `obj_to_char` prepends, and unequipping goes through it.
#[test]
fn taking_off_a_worn_item_puts_it_at_the_head_of_carrying() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    let helm = item_worn(&mut app, alice, WearSlot::Head, 12, "a helm");
    let coin = item_carried(&mut app, alice, 13, "a coin");
    let (a, h, c) = (
        uid(&mut app, alice),
        uid(&mut app, helm),
        uid(&mut app, coin),
    );
    tick(&mut app, Duration::from_millis(100));
    tick(&mut app, Duration::from_millis(100));
    app.world_mut()
        .get_mut::<TbaEquipment>(alice)
        .unwrap()
        .slots
        .clear();
    tick(&mut app, Duration::from_millis(100));
    run(&mut app, |host| {
        assert_eq!(host.carrying(a), vec![h, c]);
    });
}

#[test]
fn stamping_without_the_id_table_does_not_panic() {
    let mut world = World::new();
    let entity = world.spawn_empty().id();
    let mut host = mib_tba::dg::host::EcsHost { world: &mut world };
    host.stamp_order(entity);
}

/// HOST.md `schedule_wait`: fewer than one pulse is one.
#[test]
fn wait_zero_resumes_on_the_next_pulse_not_the_same_one() {
    let mut app = app();
    define(
        &mut app,
        300,
        "mob",
        "g",
        0,
        "wait 0\nset after 1\nglobal after",
    );
    attach(&mut app, "npc:tba_npc_1", &[300]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 300);
    assert_eq!(app.world().resource::<DgWaits>().waits[0].due, 1);
    assert_eq!(global(&mut app, g, "after"), None);
    tick_pulses(&mut app, 1);
    assert_eq!(global(&mut app, g, "after").as_deref(), Some("1"));
}

/// Truth is "non-empty and not starting with 0": `0abc` and `00` are falsy.
#[test]
fn quest_truth_is_not_starting_with_zero() {
    let mut app = app();
    for (vnum, value) in [(600, "0abc"), (601, "00"), (602, "abc0")] {
        define(
            &mut app,
            vnum,
            "mob",
            "g",
            0,
            &format!("set flag {value}\nremote flag 5"),
        );
    }
    attach(&mut app, "npc:tba_npc_1", &[600, 601, 602]);
    let temple = room(&mut app, 1);
    let _alice = player(&mut app, temple, "Alice", 5);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 600);
    run_trigger(&mut app, g, 601);
    assert!(quest_flags(&mut app).is_empty());
    run_trigger(&mut app, g, 602);
    assert_eq!(quest_flags(&mut app).len(), 1);
}

/// A saved TbaTriggerVars that lands after the player's script exists is
/// kept: the next write must not erase it.
#[test]
fn variables_restored_late_survive_the_next_write() {
    let mut app = app();
    define(&mut app, 600, "mob", "g", 0, "set d 4\nremote d 5");
    attach(&mut app, "npc:tba_npc_1", &[600]);
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 5);
    app.world_mut()
        .entity_mut(alice)
        .insert(PendingBundleRestore);
    let guard = npc(&mut app, temple, 1, "guard");
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    run(&mut app, |host| {
        use dg_vm::BaseHost;
        assert!(
            host.ensure_script(a).is_none(),
            "no script before the restore"
        );
    });

    // The restore lands after a script was created some other way.
    app.world_mut()
        .entity_mut(alice)
        .remove::<PendingBundleRestore>();
    run(&mut app, |_| {});
    assert!(app.world().get::<DgScript>(alice).is_some());
    let mut saved = TbaTriggerVars::default();
    saved.remote.insert("saved".into(), "1".into());
    app.world_mut().entity_mut(alice).insert(saved);

    run_trigger(&mut app, g, 600);
    let vars = app.world().get::<TbaTriggerVars>(alice).unwrap();
    assert_eq!(vars.remote.get("saved").map(String::as_str), Some("1"));
    assert_eq!(vars.remote.get("d").map(String::as_str), Some("4"));
    assert!(
        quest_flags(&mut app)
            .iter()
            .all(|(_, flag)| flag.as_str() == "d"),
        "restored variables are not new quest flags"
    );
}

/// dg_db_scripts.c `assign_triggers` appends to a script that exists.
#[test]
fn prototype_triggers_are_appended_to_an_existing_script() {
    let mut app = app();
    define(&mut app, 200, "mob", "g", 0, "set a 1");
    define(&mut app, 201, "mob", "g", 0, "set b 1");
    attach(&mut app, "npc:tba_npc_1", &[200]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let proto = app.world().resource::<DgProtos>().get(201).unwrap();
    let mut script = DgScript::default();
    script.0.add_trigger(proto, -1);
    app.world_mut().entity_mut(guard).insert(script);
    let g = uid(&mut app, guard);
    assert_eq!(trigger_vnums(&mut app, g), vec![201, 200]);
}

/// An item parked for an offline holder is not in the game: the C extracts a
/// quitting player's objects.
#[test]
fn items_parked_for_an_offline_holder_are_not_in_the_world() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    let coin = item_carried(&mut app, alice, 13, "a coin");
    let c = uid(&mut app, coin);
    app.world_mut()
        .entity_mut(coin)
        .insert(PendingHolder("alice".into()));
    run(&mut app, |host| {
        assert!(!host.object_list().contains(&c));
        assert!(!host.exists(c));
    });
    app.world_mut().entity_mut(coin).remove::<PendingHolder>();
    run(&mut app, |host| {
        assert!(host.object_list().contains(&c));
    });
}

#[test]
fn a_waiting_owner_the_game_despawned_is_forgotten_on_the_random_sweep() {
    let mut app = app();
    define(&mut app, 300, "mob", "g", 0, "wait 100000 s\nset late 1");
    attach(&mut app, "npc:tba_npc_1", &[300]);
    let temple = room(&mut app, 1);
    let guard = npc(&mut app, temple, 1, "guard");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 300);
    assert_eq!(app.world().resource::<DgWaits>().waits.len(), 1);
    app.world_mut().despawn(guard);
    tick_pulses(&mut app, dg_vm::types::PULSE_DG_SCRIPT as u64);
    assert!(app.world().resource::<DgWaits>().waits.is_empty());
}

#[test]
fn a_player_without_a_session_is_a_valid_target() {
    let mut app = app();
    let temple = room(&mut app, 1);
    let alice = player(&mut app, temple, "Alice", 1);
    app.world_mut().entity_mut(alice).remove::<Session>();
    let a = uid(&mut app, alice);
    run(&mut app, |host| assert!(host.valid_dg_target(a, false)));
}
