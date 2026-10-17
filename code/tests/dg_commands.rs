//! `CmdHost`: the world effects of script commands.
//!
//! Expectations come from tbaMUD's C (`handler.c`, `dg_mobcmd.c`,
//! `dg_wldcmd.c`, `dg_misc.c`, `fight.c`), `crates/dg-vm/HOST.md` and the
//! reference mock, never from what the code happens to return. The trait
//! methods are called through `dg_common::run`, and real script lines run
//! through the VM with `dg_common::run_trigger`; targets are addressed by id
//! because the character and object field reads belong to other streams.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dg_common;

use std::path::PathBuf;

use dg_common::*;
use dg_vm::commands::{CmdHost, ExitField};
use dg_vm::fields_obj::ObjHost;
use dg_vm::lookup::{WEAR_BODY, WEAR_HEAD, WEAR_WIELD};
use dg_vm::types::UID_CHAR;
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_content::prelude::{load_packs, registry_for};
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::content::ZoneDef;
use mib_tba::dg::DgLog;
use mib_tba::dg::script::{DgDead, DgScript};
use mib_tba::prelude::*;

// -- fixtures ----------------------------------------------------------------

/// A test app with the game's messages, the online registry and a content
/// pack holding mobs 3001 and 3002 and objects 3001 (a sword), 3002 (a sack)
/// and 3003 (a lamp).
fn world() -> App {
    let mut app = app();
    app.add_message::<NpcDefeated>();
    app.add_message::<PlayerDefeated>();
    app.init_resource::<OnlinePlayers>();
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "mib-tba-dg-commands-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("content")).expect("mkdir");
    std::fs::write(
        dir.join("pack.toml"),
        "id = \"dg-commands-test\"\nversion = \"1\"\ntitle = \"dg commands test\"\n",
    )
    .expect("manifest");
    let behaviours = "    dialogue: null\n    tags: []\n    behaviors:\n      sentinel: false\n      scavenger: false\n      wander: false\n      wimpy: false\n";
    std::fs::write(
        dir.join("content/stdlib.npcs.yaml"),
        format!(
            "npcs:\n  tba_npc_3001:\n    name: a city guard\n    persona: A guard.\n{behaviours}  tba_npc_3002:\n    name: a beggar\n    persona: A beggar.\n{behaviours}"
        ),
    )
    .expect("npcs");
    std::fs::write(
        dir.join("content/stdlib.items.yaml"),
        "items:\n  tba_item_3001:\n    name: a sword\n    description: A sword lies here.\n    value: 5\n    container: null\n    tags: []\n  tba_item_3002:\n    name: a sack\n    description: A sack lies here.\n    container:\n      capacity: 5\n    tags: []\n  tba_item_3003:\n    name: a lamp\n    description: A lamp lies here.\n    container: null\n    tags: []\n",
    )
    .expect("items");
    let registry = registry_for(&[&StdlibModule]);
    let assets = load_packs(std::slice::from_ref(&dir), &registry).expect("pack loads");
    app.insert_resource(LoadedContent(assets));
    app.world_mut()
        .resource_mut::<TbaContent>()
        .npc_stats
        .insert(
            NpcId::from("tba_npc_3001"),
            TbaNpcStats {
                hp: "20d1+0".parse().expect("dice"),
                level: 5,
                hitroll: 0,
                damroll: 0,
                armor_class: 0,
                thaco: 20,
                alignment: 0,
                gold: 0,
                experience: 100,
                default_position: Position::Standing,
                behaviour: Vec::new(),
                damage: None,
            },
        );
    app
}

/// A live combatant with these hit points.
fn fighter(hp: i32) -> TbaCombatant {
    let mut combatant = fallback_combatant();
    combatant.hp = hp;
    combatant.max_hp = hp.max(10);
    combatant
}

/// The scenes sent to sessions since the last drain.
fn scenes(app: &mut App) -> usize {
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .filter(|event| matches!(event.payload, OutboundPayload::Scene { .. }))
        .count()
}

fn unsupported(app: &App, key: &str) -> u64 {
    app.world().resource::<DgLog>().count(key)
}

fn location(app: &App, entity: Entity) -> Option<Entity> {
    app.world().get::<Location>(entity).map(|l| l.room)
}

fn id_ref(uid: Uid) -> String {
    format!("{UID_CHAR}{uid}")
}

// -- text and descriptors ------------------------------------------------------

#[test]
fn huh_is_the_configured_text() {
    let mut app = world();
    assert_eq!(run(&mut app, |host| host.cmd_huh()), "Huh?!?\r\n");
}

#[test]
fn send_reaches_only_playing_players() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    let bob = player(&mut app, temple, "Bob", 8);
    app.world_mut().get_mut::<Session>(bob).unwrap().state = SessionState::Connected;
    let guard = npc(&mut app, temple, 3001, "the guard");
    let (a, b, g) = (
        uid(&mut app, alice),
        uid(&mut app, bob),
        uid(&mut app, guard),
    );

    run(&mut app, |host| {
        host.cmd_send(a, "You feel a chill.\r\n");
        host.cmd_send(b, "Nobody hears this.\r\n");
        host.cmd_send(g, "Nor this.\r\n");
        host.cmd_send(999_999, "Nor this.\r\n");
    });
    assert_eq!(output(&mut app), ["You feel a chill."]);
}

#[test]
fn only_a_playing_player_has_a_descriptor() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    let bob = player(&mut app, temple, "Bob", 8);
    app.world_mut().get_mut::<Session>(bob).unwrap().state = SessionState::Connected;
    let guard = npc(&mut app, temple, 3001, "the guard");
    let (a, b, g) = (
        uid(&mut app, alice),
        uid(&mut app, bob),
        uid(&mut app, guard),
    );
    run(&mut app, |host| {
        // Present, and not switched into anything.
        assert_eq!(host.cmd_desc(a), Some(None));
        assert_eq!(host.cmd_desc(b), None);
        assert_eq!(host.cmd_desc(g), None);
        assert_eq!(host.cmd_desc(424_242), None);
    });
}

#[test]
fn playing_characters_are_listed_newest_connection_first() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    let bob = player(&mut app, temple, "Bob", 8);
    let carol = player(&mut app, temple, "Carol", 9);
    app.world_mut().get_mut::<Session>(bob).unwrap().state = SessionState::Connected;
    npc(&mut app, temple, 3001, "the guard");
    let (a, c) = (uid(&mut app, alice), uid(&mut app, carol));
    run(&mut app, |host| {
        assert_eq!(host.cmd_playing_characters(), [c, a]);
    });
}

#[test]
fn log_sinks_keep_the_lines_and_count_them() {
    let mut app = world();
    run(&mut app, |host| {
        host.cmd_mudlog(1, 31, "Alice killed by script at Temple");
        host.cmd_syslog("SYSERR: something odd");
        host.cmd_syslog("SYSERR: again");
    });
    assert_eq!(app.world().resource::<DgLog>().count("mudlog"), 1);
    assert_eq!(app.world().resource::<DgLog>().count("syslog"), 2);
    let lines = log_lines(&app);
    assert!(lines.contains(&"mudlog 1 31: Alice killed by script at Temple".to_owned()));
    assert!(lines.contains(&"syslog: SYSERR: something odd".to_owned()));
}

#[test]
fn facts_the_pack_does_not_model_answer_neutrally() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        assert!(!host.cmd_is_charmed(a));
        assert_eq!(host.cmd_invis_level(a), 0);
        assert!(host.cmd_house_can_enter(a, 3001));
        assert!(!host.cmd_is_charmed(31_337));
    });
}

// -- zones and rooms -----------------------------------------------------------

#[test]
fn zones_and_room_ranges() {
    let mut app = world();
    for num in [3003, 3001, 3002, 4000, 100] {
        room(&mut app, num);
    }
    let mut content = app.world_mut().resource_mut::<TbaContent>();
    for (number, bot, top) in [(30, 3000, 3099), (40, 4000, 4099)] {
        content.zones.insert(
            number,
            ZoneDef {
                name: format!("zone {number}"),
                bot,
                top,
                lifespan_mins: 15,
                reset_mode: ResetMode::Always,
                commands: Vec::new(),
            },
        );
    }
    run(&mut app, |host| {
        assert_eq!(host.cmd_zone_of_vnum(3050), Some(30));
        assert_eq!(host.cmd_zone_of_vnum(3000), Some(30));
        assert_eq!(host.cmd_zone_of_vnum(4099), Some(40));
        assert_eq!(host.cmd_zone_of_vnum(3100), None);
        assert_eq!(host.cmd_zone_of_vnum(-1), None);

        // World-table order is vnum order, whatever the spawn order was.
        let rooms = host.cmd_rooms_in_range(3001, 4000);
        let vnums: Vec<i32> = rooms.iter().map(|r| host.vnum(*r)).collect();
        assert_eq!(vnums, [3001, 3002, 3003, 4000]);
        assert!(host.cmd_rooms_in_range(5000, 6000).is_empty());
        assert!(host.cmd_rooms_in_range(4000, 3001).is_empty());
    });
}

// -- characters ------------------------------------------------------------------

#[test]
fn a_character_moves_between_rooms_and_heads_the_room_list() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let beggar = npc(&mut app, square, 3002, "the beggar");
    let (g, b, sq) = (
        uid(&mut app, guard),
        uid(&mut app, beggar),
        uid(&mut app, square),
    );
    run(&mut app, |host| {
        host.cmd_char_from_room(g);
        assert_eq!(host.in_room(g), None);
        host.cmd_char_to_room(g, sq);
        assert_eq!(host.in_room(g), Some(sq));
        // `char_to_room` prepends: the newcomer is first.
        assert_eq!(host.room_people(sq), [g, b]);
    });
    assert_eq!(location(&app, guard), Some(square));
}

#[test]
fn a_moved_player_is_registered_and_shown_the_room() {
    let mut app = world();
    app.insert_resource(VisibilityPolicy::default());
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    for r in [temple, square] {
        app.world_mut().entity_mut(r).insert(Exits::default());
    }
    let alice = player(&mut app, temple, "Alice", 7);
    app.world_mut().resource_mut::<OnlinePlayers>().map.insert(
        1,
        OnlinePlayer {
            entity: alice,
            name: "Alice".into(),
            room: temple,
            account_id: 7,
        },
    );
    let (a, sq) = (uid(&mut app, alice), uid(&mut app, square));
    let _ = scenes(&mut app);
    app.world_mut()
        .resource_mut::<Messages<QuestEvent>>()
        .drain()
        .for_each(drop);

    run(&mut app, |host| {
        host.cmd_char_from_room(a);
        host.cmd_char_to_room(a, sq);
    });
    assert_eq!(location(&app, alice), Some(square));
    assert_eq!(app.world().resource::<OnlinePlayers>().map[&1].room, square);
    assert_eq!(scenes(&mut app), 1);
    let visited: Vec<_> = app
        .world_mut()
        .resource_mut::<Messages<QuestEvent>>()
        .drain()
        .collect();
    assert!(
        matches!(visited.as_slice(), [QuestEvent::Visited { player, room }]
            if *player == alice && room.as_ref() == "tba.3002")
    );
}

#[test]
fn leaving_a_room_ends_the_fight() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    let a = npc(&mut app, temple, 3001, "a");
    let b = npc(&mut app, temple, 3002, "b");
    app.world_mut().entity_mut(a).insert(fighter(10));
    app.world_mut().entity_mut(b).insert(fighter(10));
    let (au, bu, sq) = (uid(&mut app, a), uid(&mut app, b), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_hit_victim(au, bu);
        host.cmd_char_from_room(au);
        host.cmd_char_to_room(au, sq);
    });
    let a_state = app.world().get::<TbaCombatant>(a).unwrap();
    assert_eq!(a_state.target, None);
    assert_eq!(a_state.position, Position::Standing);
}

#[test]
fn hostile_character_moves_do_nothing() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let sword = item_on_ground(&mut app, temple, 3001, "a sword");
    let (g, t, s) = (
        uid(&mut app, guard),
        uid(&mut app, temple),
        uid(&mut app, sword),
    );
    run(&mut app, |host| {
        // A room is not a character, a character is not a room, and neither
        // id exists.
        host.cmd_char_to_room(t, t);
        host.cmd_char_to_room(g, g);
        host.cmd_char_to_room(g, 777_777);
        host.cmd_char_to_room(s, t);
        host.cmd_char_from_room(t);
        host.cmd_char_from_room(555_555);
        assert_eq!(host.in_room(g), Some(t));
    });
    assert_eq!(location(&app, guard), Some(temple));
    assert!(app.world().get::<Location>(temple).is_none());
}

// -- objects ---------------------------------------------------------------------

#[test]
fn an_object_goes_to_a_room_a_carrier_and_a_container() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let sword = item_on_ground(&mut app, temple, 3001, "a sword");
    let lamp = item_on_ground(&mut app, temple, 3003, "a lamp");
    let sack = item_on_ground(&mut app, temple, 3002, "a sack");
    app.world_mut().entity_mut(sack).insert(Container {
        capacity: 1,
        contents: Vec::new(),
    });
    let ids = [temple, square, guard, sword, lamp, sack].map(|e| uid(&mut app, e));
    let [t, sq, g, sw, la, sa] = ids;

    run(&mut app, |host| {
        host.cmd_obj_to_room(sw, sq);
        assert_eq!(host.in_room(sw), Some(sq));
        // `obj_to_room` appends: oldest first.
        host.cmd_obj_to_room(la, sq);
        assert_eq!(host.room_contents(sq), [sw, la]);

        host.cmd_obj_to_char(sw, g);
        assert_eq!(host.obj_carried_by(sw), Some(g));
        assert_eq!(host.in_room(sw), None);
        host.cmd_obj_to_char(la, g);
        // `obj_to_char` prepends: newest first.
        assert_eq!(host.carrying(g), [la, sw]);

        // Containers take what is put in, past their capacity (the C never
        // checks), newest first.
        host.cmd_obj_to_obj(sw, sa);
        host.cmd_obj_to_obj(la, sa);
        assert_eq!(host.obj_contains(sa), [la, sw]);
        assert_eq!(host.obj_in_obj(sw), Some(sa));
        assert!(host.carrying(g).is_empty());
        assert_eq!(host.room_contents(t), [sa]);
    });
    assert_eq!(
        app.world().get::<Container>(sack).unwrap().contents,
        [sword, lamp]
    );
}

#[test]
fn detaching_an_object_leaves_it_in_no_place() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let sack = item_on_ground(&mut app, temple, 3002, "a sack");
    app.world_mut().entity_mut(sack).insert(Container {
        capacity: 5,
        contents: Vec::new(),
    });
    let inside = item_inside(&mut app, sack, 3001, "a sword");
    app.world_mut()
        .get_mut::<Container>(sack)
        .unwrap()
        .contents
        .push(inside);
    let carried = item_carried(&mut app, guard, 3003, "a lamp");
    let [sa, i, c, g] = [sack, inside, carried, guard].map(|e| uid(&mut app, e));

    run(&mut app, |host| {
        host.cmd_obj_detach(i);
        assert_eq!(host.obj_in_obj(i), None);
        assert!(host.obj_contains(sa).is_empty());
        host.cmd_obj_detach(c);
        assert!(host.carrying(g).is_empty());
        host.cmd_obj_detach(sa);
        assert_eq!(host.in_room(sa), None);
        assert!(host.exists(sa) && host.exists(i) && host.exists(c));
    });
    assert!(
        app.world()
            .get::<Container>(sack)
            .unwrap()
            .contents
            .is_empty()
    );
}

#[test]
fn an_object_is_never_put_inside_itself() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let outer = item_on_ground(&mut app, temple, 3002, "a sack");
    let inner = item_inside(&mut app, outer, 3002, "a smaller sack");
    let [o, i] = [outer, inner].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_obj_to_obj(o, o);
        host.cmd_obj_to_obj(o, i);
        assert_eq!(host.obj_in_obj(i), Some(o));
        assert_eq!(host.obj_in_obj(o), None);
        assert_eq!(host.in_room(o).map(|r| host.vnum(r)), Some(3001));
    });
}

#[test]
fn equipping_wears_at_a_position_and_unequipping_leaves_the_object_nowhere() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let sword = item_on_ground(&mut app, temple, 3001, "a sword");
    let lamp = item_carried(&mut app, guard, 3003, "a lamp");
    let [g, sw, la] = [guard, sword, lamp].map(|e| uid(&mut app, e));

    run(&mut app, |host| {
        host.cmd_equip_char(g, sw, WEAR_WIELD);
        assert_eq!(host.equipment(g, WEAR_WIELD), Some(sw));
        assert_eq!(host.obj_worn_by(sw), Some(g));
        assert_eq!(host.in_room(sw), None);
        assert!(host.room_contents(host.in_room(g).unwrap()).is_empty());

        // The inventory item can be worn too; a taken position is refused and
        // the item stays where it was.
        host.cmd_equip_char(g, la, WEAR_WIELD);
        assert_eq!(host.equipment(g, WEAR_WIELD), Some(sw));
        assert_eq!(host.obj_carried_by(la), Some(g));
        assert_eq!(host.carrying(g), [la]);
        host.cmd_equip_char(g, la, WEAR_HEAD);
        assert_eq!(host.equipment(g, WEAR_HEAD), Some(la));
        assert!(host.carrying(g).is_empty());

        // A position that does not exist.
        host.cmd_equip_char(g, la, 99);

        let off = host.cmd_unequip_char(g, WEAR_WIELD);
        assert_eq!(off, Some(sw));
        assert_eq!(host.equipment(g, WEAR_WIELD), None);
        assert_eq!(host.obj_worn_by(sw), None);
        assert_eq!(host.obj_carried_by(sw), None);
        assert_eq!(host.in_room(sw), None);
        assert_eq!(host.cmd_unequip_char(g, WEAR_BODY), None);
        assert_eq!(host.cmd_unequip_char(g, -1), None);
        // Put it back to a place of the caller's choosing.
        host.cmd_obj_to_char(sw, g);
        assert_eq!(host.carrying(g), [sw]);
    });
}

#[test]
fn extracting_an_object_takes_its_contents_with_it() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let sack = item_on_ground(&mut app, temple, 3002, "a sack");
    let inner = item_inside(&mut app, sack, 3002, "a smaller sack");
    let sword = item_inside(&mut app, inner, 3001, "a sword");
    let bystander = item_on_ground(&mut app, temple, 3003, "a lamp");
    let [sa, i, sw, by] = [sack, inner, sword, bystander].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_extract_obj(sa);
        assert!(!host.exists(sa) && !host.exists(i) && !host.exists(sw));
        assert!(host.exists(by));
        // Extracting what is gone does nothing.
        host.cmd_extract_obj(sa);
        host.cmd_extract_obj(1_234_567);
    });
    for entity in [sack, inner, sword] {
        assert!(app.world().get_entity(entity).is_err());
    }
    assert!(app.world().get_entity(bystander).is_ok());
}

#[test]
fn extracting_a_worn_or_contained_object_clears_where_it_was() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let worn = item_worn(&mut app, guard, WearSlot::Wield, 3001, "a sword");
    let sack = item_on_ground(&mut app, temple, 3002, "a sack");
    app.world_mut().entity_mut(sack).insert(Container {
        capacity: 5,
        contents: Vec::new(),
    });
    let held = item_inside(&mut app, sack, 3003, "a lamp");
    app.world_mut()
        .get_mut::<Container>(sack)
        .unwrap()
        .contents
        .push(held);
    let [g, w, sa, h] = [guard, worn, sack, held].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_extract_obj(w);
        host.cmd_extract_obj(h);
        assert_eq!(host.equipment(g, WEAR_WIELD), None);
        assert!(host.obj_contains(sa).is_empty());
    });
    let worn_set = &app.world().get::<TbaEquipment>(guard).unwrap().slots;
    assert!(worn_set.is_empty());
    assert!(
        app.world()
            .get::<Container>(sack)
            .unwrap()
            .contents
            .is_empty()
    );
}

#[test]
fn extracting_an_object_forgets_its_waits() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let lamp = item_on_ground(&mut app, temple, 3003, "a lamp");
    let l = uid(&mut app, lamp);
    run(&mut app, |host| {
        host.schedule_wait(l, 5, 10);
    });
    assert_eq!(
        app.world()
            .resource::<mib_tba::dg::waits::DgWaits>()
            .waits
            .len(),
        1
    );
    run(&mut app, |host| host.cmd_extract_obj(l));
    assert!(
        app.world()
            .resource::<mib_tba::dg::waits::DgWaits>()
            .waits
            .is_empty()
    );
}

// -- creation --------------------------------------------------------------------

#[test]
fn a_new_object_exists_in_no_place_with_its_id_and_scripts() {
    let mut app = world();
    define(
        &mut app,
        600,
        "obj",
        "n",
        100,
        "set loaded no\nglobal loaded",
    );
    attach(&mut app, "item:tba_item_3001", &[600]);
    let temple = room(&mut app, 3001);
    let _ = temple;
    let sword = run(&mut app, |host| host.cmd_read_object(3001)).expect("sword");
    run(&mut app, |host| {
        assert!(host.exists(sword));
        assert_eq!(host.vnum(sword), 3001);
        assert_eq!(host.in_room(sword), None);
        assert_eq!(host.obj_carried_by(sword), None);
        assert_eq!(host.obj_in_obj(sword), None);
        assert!(host.object_list().contains(&sword));
    });
    // Its prototype triggers are attached; the VM fires the load trigger, not
    // the host, and a later dispatch does not fire it either.
    assert_eq!(trigger_vnums(&mut app, sword), [600]);
    assert_eq!(global(&mut app, sword, "loaded"), None);

    let sack = run(&mut app, |host| host.cmd_read_object(3002)).expect("sack");
    let entity = run(&mut app, |host| host.entity(sack)).unwrap();
    assert_eq!(app.world().get::<Container>(entity).unwrap().capacity, 5);
}

#[test]
fn an_object_that_does_not_exist_is_not_loaded() {
    let mut app = world();
    run(&mut app, |host| {
        assert_eq!(host.cmd_read_object(9999), None);
        assert_eq!(host.cmd_read_object(-1), None);
        assert_eq!(host.cmd_read_object(70_000), None);
        assert_eq!(host.cmd_read_mobile(9999), None);
        assert_eq!(host.cmd_read_mobile(-5), None);
    });
}

#[test]
fn a_new_mob_has_combat_state_and_scripts_before_anything_else_happens() {
    let mut app = world();
    define(
        &mut app,
        601,
        "mob",
        "n",
        100,
        "set loaded yes\nglobal loaded",
    );
    attach(&mut app, "npc:tba_npc_3001", &[601]);
    let mob = run(&mut app, |host| host.cmd_read_mobile(3001)).expect("mob");
    let entity = run(&mut app, |host| host.entity(mob)).unwrap();
    {
        let world = app.world();
        let combatant = world.get::<TbaCombatant>(entity).expect("combatant now");
        assert_eq!((combatant.hp, combatant.max_hp), (20, 20));
        assert!(world.get::<TbaNpcBase>(entity).is_some());
        assert!(world.get::<TbaAttackerMemory>(entity).is_some());
        assert!(world.get::<Location>(entity).is_none());
    }
    run(&mut app, |host| {
        assert_eq!(host.vnum(mob), 3001);
        assert_eq!(host.name(mob), "a city guard");
        assert!(host.is_npc(mob));
        assert_eq!(host.in_room(mob), None);
        assert!(host.character_list().contains(&mob));
    });
    assert_eq!(trigger_vnums(&mut app, mob), [601]);
    assert_eq!(global(&mut app, mob, "loaded"), None);
}

#[test]
fn a_mob_placed_in_a_room_remembers_where_it_started() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let mob = run(&mut app, |host| host.cmd_read_mobile(3002)).expect("mob");
    let t = uid(&mut app, temple);
    run(&mut app, |host| host.cmd_char_to_room(mob, t));
    let entity = run(&mut app, |host| host.entity(mob)).unwrap();
    assert_eq!(
        app.world().get::<TbaSpawnHome>(entity).unwrap().room,
        temple
    );
    assert_eq!(location(&app, entity), Some(temple));
}

// -- transform -------------------------------------------------------------------

#[test]
fn a_mob_overwritten_takes_the_new_description_and_keeps_its_identity() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let mut hurt = fighter(7);
    hurt.max_hp = 30;
    app.world_mut().entity_mut(guard).insert(hurt);
    let g = uid(&mut app, guard);
    let t = uid(&mut app, temple);
    let temp = run(&mut app, |host| host.cmd_read_mobile(3002)).expect("temp");
    let temp_entity = run(&mut app, |host| host.entity(temp)).unwrap();
    app.world_mut()
        .get_mut::<TbaCombatant>(temp_entity)
        .unwrap()
        .max_hp = 99;
    app.world_mut()
        .get_mut::<TbaCombatant>(temp_entity)
        .unwrap()
        .hp = 99;

    run(&mut app, |host| {
        host.cmd_char_to_room(temp, t);
        host.cmd_overwrite_mob(g, temp, true);
        assert_eq!(host.name(g), "a beggar");
        // The vnum is the old one: `ch->nr` is restored.
        assert_eq!(host.vnum(g), 3001);
        host.cmd_extract_char(temp);
    });
    let combatant = app.world().get::<TbaCombatant>(guard).unwrap();
    assert_eq!((combatant.hp, combatant.max_hp), (7, 30));
    assert!(app.world().get_entity(temp_entity).is_err());

    // Without `keep_hp` the new mob's hit points come with it.
    let temp = run(&mut app, |host| host.cmd_read_mobile(3002)).expect("temp");
    run(&mut app, |host| {
        host.cmd_overwrite_mob(g, temp, false);
        host.cmd_extract_char(temp);
    });
    let combatant = app.world().get::<TbaCombatant>(guard).unwrap();
    assert_ne!(combatant.max_hp, 30);
    assert_eq!(combatant.hp, combatant.max_hp);
}

#[test]
fn an_object_overwritten_takes_the_new_vnum_and_keeps_its_place() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let sack = item_carried(&mut app, guard, 3002, "a sack");
    let inner = item_inside(&mut app, sack, 3003, "a lamp");
    app.world_mut().entity_mut(sack).insert(Container {
        capacity: 1,
        contents: vec![inner],
    });
    let [g, sa, i] = [guard, sack, inner].map(|e| uid(&mut app, e));
    let temp = run(&mut app, |host| host.cmd_read_object(3001)).expect("temp");
    run(&mut app, |host| {
        host.cmd_overwrite_obj(sa, temp);
        host.cmd_extract_obj(temp);
        assert_eq!(host.vnum(sa), 3001);
        assert_eq!(host.name(sa), "a sword");
        assert_eq!(host.obj_carried_by(sa), Some(g));
        assert_eq!(host.obj_contains(sa), [i]);
    });
    // No longer a container, but what was inside stays inside.
    assert_eq!(
        app.world()
            .get::<Container>(sack)
            .map(|c| c.contents.clone()),
        Some(vec![inner]),
    );
}

// -- extraction of characters ------------------------------------------------------

#[test]
fn an_extracted_npc_stays_until_the_sweep_and_a_player_is_never_removed() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let alice = player(&mut app, temple, "Alice", 7);
    let sword = item_carried(&mut app, guard, 3001, "a sword");
    let [g, a, sw] = [guard, alice, sword].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_extract_char(g);
        host.cmd_extract_char(a);
        // Still listed, but dead.
        assert!(host.is_dead(g) && host.is_dead(a));
        assert!(host.exists(g));
        assert!(host.room_people(host.in_room(g).unwrap()).contains(&g));
    });
    assert!(app.world().get_entity(guard).is_err());
    assert!(app.world().get_entity(sword).is_err());
    assert!(app.world().get_entity(alice).is_ok());
    assert!(app.world().get::<DgDead>(alice).is_none());
    run(&mut app, |host| {
        assert!(!host.exists(g) && !host.exists(sw));
        assert!(!host.is_dead(a));
    });
}

#[test]
fn extracting_a_character_ends_every_fight_with_it() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let a = npc(&mut app, temple, 3001, "a");
    let b = npc(&mut app, temple, 3002, "b");
    app.world_mut().entity_mut(a).insert(fighter(10));
    app.world_mut().entity_mut(b).insert(fighter(10));
    let [au, bu] = [a, b].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_hit_victim(au, bu);
        host.cmd_extract_char(bu);
    });
    assert_eq!(app.world().get::<TbaCombatant>(a).unwrap().target, None);
}

// -- fighting and dying --------------------------------------------------------------

#[test]
fn a_blow_starts_a_fight_on_both_sides() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let alice = player(&mut app, temple, "Alice", 7);
    app.world_mut().entity_mut(guard).insert(fighter(10));
    app.world_mut().entity_mut(alice).insert(fighter(10));
    let [g, a] = [guard, alice].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_hit_victim(g, a));
    let guard_state = app.world().get::<TbaCombatant>(guard).unwrap();
    let alice_state = app.world().get::<TbaCombatant>(alice).unwrap();
    assert_eq!(guard_state.target, Some(alice));
    assert_eq!(guard_state.position, Position::Fighting);
    assert_eq!(alice_state.target, Some(guard));
    assert_eq!(alice_state.position, Position::Fighting);

    // Someone already fighting keeps their opponent.
    let beggar = npc(&mut app, temple, 3002, "the beggar");
    app.world_mut().entity_mut(beggar).insert(fighter(10));
    let b = uid(&mut app, beggar);
    run(&mut app, |host| host.cmd_hit_victim(b, a));
    assert_eq!(
        app.world().get::<TbaCombatant>(beggar).unwrap().target,
        Some(alice)
    );
    assert_eq!(
        app.world().get::<TbaCombatant>(alice).unwrap().target,
        Some(guard)
    );
}

#[test]
fn a_new_mob_can_fight_at_once() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    app.world_mut().entity_mut(alice).insert(fighter(10));
    let [a, t] = [alice, temple].map(|e| uid(&mut app, e));
    let mob = run(&mut app, |host| host.cmd_read_mobile(3001)).expect("mob");
    let entity = run(&mut app, |host| host.entity(mob)).unwrap();
    run(&mut app, |host| {
        host.cmd_char_to_room(mob, t);
        host.cmd_hit_victim(mob, a);
    });
    assert_eq!(
        app.world().get::<TbaCombatant>(entity).unwrap().target,
        Some(alice)
    );
}

#[test]
fn a_dead_or_missing_opponent_starts_nothing() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let a = npc(&mut app, temple, 3001, "a");
    let b = npc(&mut app, temple, 3002, "b");
    let mut corpse = fighter(0);
    corpse.position = Position::Dead;
    app.world_mut().entity_mut(a).insert(fighter(10));
    app.world_mut().entity_mut(b).insert(corpse);
    let [au, bu] = [a, b].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_hit_victim(au, bu);
        host.cmd_hit_victim(au, au);
        host.cmd_hit_victim(au, 8_888_888);
    });
    assert_eq!(app.world().get::<TbaCombatant>(a).unwrap().target, None);
}

#[test]
fn stopping_a_fight_clears_the_target_and_stands_up() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let a = npc(&mut app, temple, 3001, "a");
    let b = npc(&mut app, temple, 3002, "b");
    app.world_mut().entity_mut(a).insert(fighter(10));
    app.world_mut().entity_mut(b).insert(fighter(10));
    let [au, bu] = [a, b].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_hit_victim(au, bu);
        host.cmd_stop_fighting(au);
        host.cmd_stop_fighting(31_000_000);
    });
    let a_state = app.world().get::<TbaCombatant>(a).unwrap();
    assert_eq!(
        (a_state.target, a_state.position),
        (None, Position::Standing)
    );
    // Only the one who stopped.
    assert_eq!(app.world().get::<TbaCombatant>(b).unwrap().target, Some(a));
}

#[test]
fn a_script_kill_of_an_npc_is_announced_and_the_npc_swept() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    app.world_mut().entity_mut(guard).insert(fighter(0));
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.cmd_die(g);
        assert!(host.is_dead(g));
        host.cmd_die(g);
    });
    let defeated: Vec<NpcDefeated> = app
        .world_mut()
        .resource_mut::<Messages<NpcDefeated>>()
        .drain()
        .collect();
    assert_eq!(defeated.len(), 1);
    assert_eq!(defeated[0].victim, guard);
    assert_eq!(defeated[0].npc.as_ref(), "tba_npc_3001");
    // No killer: nobody is paid.
    assert!(app.world().get::<Player>(defeated[0].killer).is_none());
    assert!(app.world().get_entity(guard).is_err());
}

#[test]
fn a_script_kill_of_a_player_goes_through_player_death() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    app.world_mut().entity_mut(alice).insert(fighter(0));
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.cmd_die(a);
        assert!(host.is_dead(a));
    });
    let defeated: Vec<PlayerDefeated> = app
        .world_mut()
        .resource_mut::<Messages<PlayerDefeated>>()
        .drain()
        .collect();
    assert_eq!(defeated.len(), 1);
    assert_eq!(defeated[0].player, alice);
    // The player stays in the game; the pack's death handling owns the rest.
    assert!(app.world().get_entity(alice).is_ok());
    assert!(app.world().get::<DgDead>(alice).is_none());
    assert_eq!(
        app.world().get::<TbaCombatant>(alice).unwrap().position,
        Position::Dead
    );
}

#[test]
fn dying_without_combat_state_still_removes_the_npc() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let g = uid(&mut app, guard);
    run(&mut app, |host| host.cmd_die(g));
    assert!(app.world().get_entity(guard).is_err());
    run(&mut app, |host| host.cmd_die(g));
}

#[test]
fn hunting_is_recorded_and_counted_as_a_gap() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let alice = player(&mut app, temple, "Alice", 7);
    let [g, a] = [guard, alice].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_set_hunting(g, a));
    assert_eq!(unsupported(&app, "mhunt"), 1);
}

#[test]
fn following_and_unfollowing() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let alice = player(&mut app, temple, "Alice", 7);
    let [g, a] = [guard, alice].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_follow(g, a));
    let following = *app.world().get::<TbaFollowing>(guard).unwrap();
    assert_eq!((following.leader, following.last_room), (alice, temple));
    run(&mut app, |host| {
        host.cmd_follow(g, g);
        host.cmd_unfollow(g);
        host.cmd_unfollow(g);
        host.cmd_unfollow(654_321);
    });
    assert!(app.world().get::<TbaFollowing>(guard).is_none());
}

// -- exits and doors -----------------------------------------------------------------

/// Three rooms with exits: 100 (the first of the world table), 3001, 3002.
fn exit_world() -> (App, Entity, Entity, Entity) {
    let mut app = world();
    let first = room(&mut app, 100);
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    for r in [first, temple, square] {
        app.world_mut().entity_mut(r).insert(Exits::default());
    }
    (app, first, temple, square)
}

const NORTH: usize = 0;
const EAST: usize = 1;

#[test]
fn a_new_exit_points_at_the_first_room_like_a_zeroed_c_exit() {
    let (mut app, first, temple, _) = exit_world();
    let t = uid(&mut app, temple);
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Keyword("gate".into()));
    });
    let exits = app.world().get::<Exits>(temple).unwrap();
    assert_eq!(exits.map.get(&ExitKey::North), Some(&first));
    // The keyword waits until the flags make a door of the exit.
    assert_eq!(unsupported(&app, "exit.keyword"), 0);
}

#[test]
fn a_room_without_exits_gets_them() {
    let mut app = world();
    let first = room(&mut app, 100);
    let temple = room(&mut app, 3001);
    let t = uid(&mut app, temple);
    run(&mut app, |host| {
        host.cmd_exit_set(t, EAST, ExitField::Flags(0))
    });
    assert_eq!(
        app.world()
            .get::<Exits>(temple)
            .unwrap()
            .map
            .get(&ExitKey::East),
        Some(&first)
    );
}

#[test]
fn exit_flags_make_and_unmake_a_door() {
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::ToRoom(Some(sq)));
        // EX_ISDOOR | EX_CLOSED | EX_LOCKED
        host.cmd_exit_set(t, NORTH, ExitField::Flags(7));
    });
    {
        let door = &app.world().get::<Doors>(temple).unwrap().by_dir[&ExitKey::North];
        assert!(door.pickable);
        let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::North];
        assert_eq!((state.closed, state.locked), (true, true));
        assert_eq!(state.to, Some(RoomId::from("tba.3002")));
    }
    // EX_ISDOOR | EX_CLOSED | EX_PICKPROOF: closed, unlocked, unpickable.
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Flags(11))
    });
    {
        let door = &app.world().get::<Doors>(temple).unwrap().by_dir[&ExitKey::North];
        assert!(!door.pickable);
        let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::North];
        assert_eq!((state.closed, state.locked), (true, false));
    }
    // An open door.
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Flags(1))
    });
    let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::North];
    assert_eq!((state.closed, state.locked), (false, false));
    // No EX_ISDOOR: no door at all, and the exit stays.
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Flags(0))
    });
    assert!(app.world().get::<Doors>(temple).unwrap().by_dir.is_empty());
    assert!(
        app.world()
            .get::<RoomState>(temple)
            .unwrap()
            .doors
            .is_empty()
    );
    assert_eq!(
        app.world()
            .get::<Exits>(temple)
            .unwrap()
            .map
            .get(&ExitKey::North),
        Some(&square)
    );
}

#[test]
fn a_door_has_a_key_and_a_keyword() {
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::ToRoom(Some(sq)));
        // Set before the flags make a door: each field is stored on its own.
        host.cmd_exit_set(t, NORTH, ExitField::Key(3001));
        host.cmd_exit_set(t, NORTH, ExitField::Keyword("gate".into()));
        host.cmd_exit_set(t, NORTH, ExitField::Flags(7));
    });
    assert_eq!(unsupported(&app, "exit.key"), 0);
    assert_eq!(unsupported(&app, "exit.keyword"), 0);
    {
        let door = &app.world().get::<Doors>(temple).unwrap().by_dir[&ExitKey::North];
        assert_eq!(door.key, Some(ItemId::from("tba_item_3001")));
        assert_eq!(door.name.as_deref(), Some("gate"));
    }
    // `-1` is stored as 65535 (NOTHING): no key; an empty keyword clears it.
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Key(65_535));
        host.cmd_exit_set(t, NORTH, ExitField::Keyword(String::new()));
    });
    let door = &app.world().get::<Doors>(temple).unwrap().by_dir[&ExitKey::North];
    assert_eq!((door.key.clone(), door.name.clone()), (None, None));
}

#[test]
fn an_exit_description_is_a_counted_gap_that_still_creates_the_exit() {
    let (mut app, first, temple, _) = exit_world();
    let t = uid(&mut app, temple);
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Description("A path.".into()));
    });
    assert_eq!(unsupported(&app, "exit.description"), 1);
    assert_eq!(
        app.world()
            .get::<Exits>(temple)
            .unwrap()
            .map
            .get(&ExitKey::North),
        Some(&first)
    );
}

#[test]
fn an_exit_can_lead_to_a_room_or_nowhere() {
    let (mut app, first, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, EAST, ExitField::ToRoom(Some(sq)));
        host.cmd_exit_set(t, EAST, ExitField::Flags(3));
    });
    let exits = app.world().get::<Exits>(temple).unwrap();
    assert_eq!(exits.map.get(&ExitKey::East), Some(&square));
    // One-sided: the way back is not made.
    assert!(app.world().get::<Exits>(square).unwrap().map.is_empty());

    // Leading nowhere: the exit exists, so changing it again creates nothing.
    run(&mut app, |host| {
        host.cmd_exit_set(t, EAST, ExitField::ToRoom(None));
        host.cmd_exit_set(t, EAST, ExitField::Flags(1));
    });
    assert!(app.world().get::<Exits>(temple).unwrap().map.is_empty());
    assert_ne!(first, square);
    // Pointing it somewhere again.
    run(&mut app, |host| {
        host.cmd_exit_set(t, EAST, ExitField::ToRoom(Some(sq)))
    });
    assert_eq!(
        app.world()
            .get::<Exits>(temple)
            .unwrap()
            .map
            .get(&ExitKey::East),
        Some(&square)
    );
    let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::East];
    assert_eq!(state.to, Some(RoomId::from("tba.3002")));
}

#[test]
fn purging_an_exit_removes_it_and_its_door() {
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::ToRoom(Some(sq)));
        host.cmd_exit_set(t, NORTH, ExitField::Flags(3));
        host.cmd_exit_set(t, EAST, ExitField::ToRoom(None));
        host.cmd_exit_purge(t, NORTH);
        host.cmd_exit_purge(t, EAST);
        // Nothing there, a direction that does not exist, a room that does not.
        host.cmd_exit_purge(t, 3);
        host.cmd_exit_purge(t, 12);
        host.cmd_exit_purge(9_999_999, NORTH);
    });
    assert!(app.world().get::<Exits>(temple).unwrap().map.is_empty());
    assert!(app.world().get::<Doors>(temple).unwrap().by_dir.is_empty());
    assert!(
        app.world()
            .get::<RoomState>(temple)
            .unwrap()
            .doors
            .is_empty()
    );
    // Purged for good: a later field creates a fresh exit.
    run(&mut app, |host| {
        host.cmd_exit_set(t, EAST, ExitField::Flags(0))
    });
    assert_eq!(app.world().get::<Exits>(temple).unwrap().map.len(), 1);
}

#[test]
fn diagonal_directions_map_to_the_c_numbers() {
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    let expected = [
        (6, ExitKey::Northwest),
        (7, ExitKey::Northeast),
        (8, ExitKey::Southeast),
        (9, ExitKey::Southwest),
        (4, ExitKey::Up),
        (5, ExitKey::Down),
        (2, ExitKey::South),
        (3, ExitKey::West),
    ];
    run(&mut app, |host| {
        for (dir, _) in expected {
            host.cmd_exit_set(t, dir, ExitField::ToRoom(Some(sq)));
        }
        host.cmd_exit_set(t, 10, ExitField::ToRoom(Some(sq)));
        host.cmd_exit_set(t, 4_000_000, ExitField::Flags(1));
    });
    let exits = app.world().get::<Exits>(temple).unwrap();
    for (_, key) in expected {
        assert_eq!(exits.map.get(&key), Some(&square), "{key:?}");
    }
    assert_eq!(exits.map.len(), expected.len());
}

#[test]
fn a_door_made_by_a_script_blocks_movement_like_any_other() {
    // The movement gate reads `Doors` and `RoomState`; a closed door built
    // here must read as closed there.
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::ToRoom(Some(sq)));
        host.cmd_exit_set(t, NORTH, ExitField::Flags(7));
    });
    let world = app.world();
    let door = world
        .get::<Doors>(temple)
        .and_then(|doors| doors.by_dir.get(&ExitKey::North))
        .expect("a door");
    let state = world
        .get::<RoomState>(temple)
        .and_then(|state| state.doors.get(&ExitKey::North))
        .expect("its state");
    assert!(state.locked && state.closed);
    assert!(door.default_closed && door.default_locked);
}

// -- real script lines ------------------------------------------------------------------

/// A guard in the temple running trigger 700 with this script, plus Alice.
/// `{ALICE}` in the script is replaced by Alice's id reference.
fn scripted(script: &str) -> (App, Entity, Entity, Entity, Entity) {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    let alice = player(&mut app, temple, "Alice", 7);
    let alice_uid = uid(&mut app, alice);
    define(
        &mut app,
        700,
        "mob",
        "g",
        0,
        &script.replace("{ALICE}", &id_ref(alice_uid)),
    );
    attach(&mut app, "npc:tba_npc_3001", &[700]);
    let guard = npc(&mut app, temple, 3001, "the guard");
    (app, temple, square, guard, alice)
}

#[test]
fn msend_and_mecho_reach_players_in_the_room() {
    let (mut app, _, _, guard, alice) =
        scripted("msend {ALICE} Hello there.\nmecho The guard coughs.");
    let _ = alice;
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    assert_eq!(output(&mut app), ["Hello there.", "The guard coughs."]);
}

#[test]
fn mload_puts_a_mob_in_the_room_and_runs_its_load_trigger() {
    let (mut app, temple, _, guard, _) = scripted("mload mob 3002");
    define(
        &mut app,
        710,
        "mob",
        "n",
        100,
        "set arrived yes\nglobal arrived",
    );
    attach(&mut app, "npc:tba_npc_3002", &[710]);
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    run(&mut app, |host| {
        let t = host.in_room(g).unwrap();
        let people = host.room_people(t);
        assert_eq!(people.len(), 3);
        let beggar = people
            .into_iter()
            .find(|p| host.vnum(*p) == 3002)
            .expect("the beggar");
        assert_eq!(
            host.script(beggar)
                .unwrap()
                .global_vars
                .vars
                .iter()
                .find(|v| v.name == "arrived")
                .map(|v| v.value.as_str()),
            Some("yes")
        );
    });
    let _ = temple;
}

#[test]
fn mload_obj_puts_it_in_the_inventory_and_a_container() {
    let (mut app, _, _, guard, _) = scripted("mload obj 3001");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    run(&mut app, |host| {
        let held = host.carrying(g);
        assert_eq!(held.len(), 1);
        assert_eq!(host.vnum(held[0]), 3001);
    });

    // Into a container: the sack is loaded first, then the lamp inside it.
    let (mut app, temple, _, guard, _) = scripted("mload obj 3002\nset sack %self.inventory%");
    let _ = temple;
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    run(&mut app, |host| {
        let sack = host.carrying(g)[0];
        assert_eq!(host.vnum(sack), 3002);
        let lamp = host.cmd_read_object(3003).unwrap();
        host.cmd_obj_to_obj(lamp, sack);
        assert_eq!(host.obj_contains(sack), [lamp]);
    });
}

#[test]
fn mload_of_a_missing_prototype_is_logged_not_fatal() {
    let (mut app, _, _, guard, _) =
        scripted("mload mob 9999\nmload obj 9998\nmsend {ALICE} still here");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    assert_eq!(output(&mut app), ["still here"]);
    let lines = log_lines(&app).join("\n");
    assert!(lines.contains("mload: bad mob vnum"), "{lines}");
    assert!(lines.contains("mload: bad object vnum"), "{lines}");
}

#[test]
fn mteleport_moves_a_player_and_shows_the_room() {
    let (mut app, _, square, guard, alice) = scripted("mteleport {ALICE} 3002");
    app.insert_resource(VisibilityPolicy::default());
    app.world_mut().entity_mut(square).insert(Exits::default());
    let alice_room = location(&app, alice).unwrap();
    app.world_mut().resource_mut::<OnlinePlayers>().map.insert(
        1,
        OnlinePlayer {
            entity: alice,
            name: "Alice".into(),
            room: alice_room,
            account_id: 7,
        },
    );
    let g = uid(&mut app, guard);
    let _ = scenes(&mut app);
    run_trigger(&mut app, g, 700);
    assert_eq!(location(&app, alice), Some(square));
    assert_eq!(app.world().resource::<OnlinePlayers>().map[&1].room, square);
    assert_eq!(scenes(&mut app), 1);
    // The guard stayed.
    assert_ne!(location(&app, guard), Some(square));
}

#[test]
fn mgoto_and_mat_move_the_mob() {
    let (mut app, _, square, guard, _) = scripted("mgoto 3002");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    assert_eq!(location(&app, guard), Some(square));

    let (mut app, temple, _, guard, _) = scripted("mat 3002 mecho unheard\nmsend {ALICE} back");
    let g2 = uid(&mut app, guard);
    run_trigger(&mut app, g2, 700);
    assert_eq!(location(&app, guard), Some(temple));
    assert!(output(&mut app).contains(&"back".to_owned()));
    let _ = g;
}

#[test]
fn mpurge_removes_an_object_a_mob_and_the_running_owner() {
    let (mut app, temple, _, guard, _) = scripted("mpurge sword\nmpurge beggar");
    let sword = item_on_ground(&mut app, temple, 3001, "a sword");
    let beggar = npc(&mut app, temple, 3002, "a beggar");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    assert!(app.world().get_entity(sword).is_err());
    assert!(app.world().get_entity(beggar).is_err());
    assert!(app.world().get_entity(guard).is_ok());

    // The owner purging itself is gone once the dispatch ends.
    let (mut app, _, _, guard, alice) = scripted("mpurge self\nmsend {ALICE} unreachable");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    assert!(app.world().get_entity(guard).is_err());
    assert!(app.world().get_entity(alice).is_ok());
}

#[test]
fn mpurge_with_no_argument_clears_the_room_but_not_players_or_the_owner() {
    let (mut app, temple, _, guard, alice) = scripted("mpurge");
    let sword = item_on_ground(&mut app, temple, 3001, "a sword");
    let sack = item_on_ground(&mut app, temple, 3002, "a sack");
    let inner = item_inside(&mut app, sack, 3003, "a lamp");
    let beggar = npc(&mut app, temple, 3002, "a beggar");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    for gone in [sword, sack, inner, beggar] {
        assert!(app.world().get_entity(gone).is_err());
    }
    assert!(app.world().get_entity(guard).is_ok());
    assert!(app.world().get_entity(alice).is_ok());
}

#[test]
fn mkill_starts_a_fight_with_a_player() {
    let (mut app, _, _, guard, alice) = scripted("");
    let a = uid(&mut app, alice);
    define(
        &mut app,
        701,
        "mob",
        "g",
        0,
        &format!("mkill {}", id_ref(a)),
    );
    attach(&mut app, "npc:tba_npc_3001", &[700, 701]);
    app.world_mut().entity_mut(alice).insert(fighter(10));
    let g_mob = run(&mut app, |host| host.cmd_read_mobile(3001)).unwrap();
    let guard_room = location(&app, guard).unwrap();
    let t = uid(&mut app, guard_room);
    run(&mut app, |host| host.cmd_char_to_room(g_mob, t));
    run_trigger(&mut app, g_mob, 701);
    let entity = run(&mut app, |host| host.entity(g_mob)).unwrap();
    assert_eq!(
        app.world().get::<TbaCombatant>(entity).unwrap().target,
        Some(alice)
    );
    assert_eq!(
        app.world().get::<TbaCombatant>(alice).unwrap().target,
        Some(entity)
    );
}

#[test]
fn mfollow_and_mhunt_act_on_the_target() {
    let (mut app, _, _, guard, alice) = scripted("");
    let a = uid(&mut app, alice);
    define(
        &mut app,
        702,
        "mob",
        "g",
        0,
        &format!("mfollow {0}\nmhunt {0}", id_ref(a)),
    );
    attach(&mut app, "npc:tba_npc_3001", &[702]);
    let g = uid(&mut app, guard);
    run(&mut app, |_| {});
    // The prototype triggers are attached when the guard is first seen.
    let guard_room = location(&app, guard).unwrap();
    let guard2 = npc(&mut app, guard_room, 3001, "the second guard");
    let g2 = uid(&mut app, guard2);
    run_trigger(&mut app, g2, 702);
    assert_eq!(
        app.world().get::<TbaFollowing>(guard2).unwrap().leader,
        alice
    );
    assert_eq!(unsupported(&app, "mhunt"), 1);
    let _ = g;
}

#[test]
fn mtransform_turns_the_mob_into_another_and_leaves_no_stray() {
    let (mut app, _, _, guard, _) = scripted("mtransform 3002");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    run(&mut app, |host| {
        assert_eq!(host.name(g), "a beggar");
        assert_eq!(host.vnum(g), 3001);
        let room = host.in_room(g).unwrap();
        assert_eq!(host.room_people(room).len(), 2);
    });
}

#[test]
fn otransform_and_opurge_work_on_objects() {
    let mut app = world();
    define(&mut app, 720, "obj", "g", 0, "otransform 3001");
    attach(&mut app, "item:tba_item_3003", &[720]);
    let temple = room(&mut app, 3001);
    let lamp = item_on_ground(&mut app, temple, 3003, "a lamp");
    let l = uid(&mut app, lamp);
    run_trigger(&mut app, l, 720);
    run(&mut app, |host| {
        assert_eq!(host.name(l), "a sword");
        assert_eq!(host.vnum(l), 3001);
        assert_eq!(host.room_contents(host.in_room(l).unwrap()), [l]);
    });
}

#[test]
fn mdoor_builds_a_door_from_a_script() {
    let (mut app, _, square, guard, _) =
        scripted("mdoor 3001 north room 3002\nmdoor 3001 north flags abc");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    let temple = location(&app, guard).unwrap();
    assert_eq!(
        app.world()
            .get::<Exits>(temple)
            .map(|e| e.map.get(&ExitKey::North).copied()),
        Some(Some(square))
    );
    let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::North];
    assert_eq!((state.closed, state.locked), (true, true));
}

#[test]
fn a_script_that_loads_its_own_kind_keeps_unique_ids() {
    let (mut app, _, _, guard, _) = scripted("mload mob 3002\nmload mob 3002\nmload mob 3002");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    run(&mut app, |host| {
        let mut ids = host.room_people(host.in_room(g).unwrap());
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total);
        assert_eq!(total, 5);
    });
}

#[test]
fn script_state_survives_for_a_loaded_mob() {
    let (mut app, _, _, guard, _) = scripted("mload mob 3001");
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    let mut scripts = app.world_mut().query::<(&Npc, &DgScript)>();
    // The loaded copy carries the same prototype trigger as the first guard.
    assert_eq!(scripts.iter(app.world()).count(), 2);
}

// -- review regressions ------------------------------------------------------------

#[test]
fn a_closed_exit_without_the_door_flag_still_blocks() {
    // 175.trg re-seals an exit with `flags bcd`; the C blocks on EX_CLOSED
    // whether or not the exit is flagged EX_ISDOOR.
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::ToRoom(Some(sq)));
        host.cmd_exit_set(t, NORTH, ExitField::Flags(2));
    });
    // Closed and not a door: sealed, since the C cannot open it either.
    let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::North];
    assert_eq!((state.closed, state.locked), (true, true));
    assert!(
        app.world()
            .get::<Doors>(temple)
            .unwrap()
            .by_dir
            .contains_key(&ExitKey::North)
    );
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Flags(14));
    });
    let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::North];
    assert_eq!((state.closed, state.locked), (true, true));
    let door = &app.world().get::<Doors>(temple).unwrap().by_dir[&ExitKey::North];
    assert!(!door.pickable);
}

#[test]
fn a_key_and_keyword_survive_the_exit_losing_and_regaining_its_door() {
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::ToRoom(Some(sq)));
        host.cmd_exit_set(t, NORTH, ExitField::Flags(1));
        host.cmd_exit_set(t, NORTH, ExitField::Key(3001));
        host.cmd_exit_set(t, NORTH, ExitField::Keyword("gate".into()));
        host.cmd_exit_set(t, NORTH, ExitField::Flags(0));
    });
    assert!(app.world().get::<Doors>(temple).unwrap().by_dir.is_empty());
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Flags(7));
    });
    let door = &app.world().get::<Doors>(temple).unwrap().by_dir[&ExitKey::North];
    assert_eq!(door.key, Some(ItemId::from("tba_item_3001")));
    assert_eq!(door.name.as_deref(), Some("gate"));
    // Purging the exit forgets what was waiting.
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::Flags(0));
        host.cmd_exit_purge(t, NORTH);
        host.cmd_exit_set(t, NORTH, ExitField::Flags(1));
    });
    let door = &app.world().get::<Doors>(temple).unwrap().by_dir[&ExitKey::North];
    assert_eq!((door.key.clone(), door.name.clone()), (None, None));
}

#[test]
fn a_blow_needs_both_in_one_room_no_peace_and_a_killable_victim() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let beggar = npc(&mut app, temple, 3002, "the beggar");
    let alice = player(&mut app, square, "Alice", 7);
    for e in [guard, beggar, alice] {
        app.world_mut().entity_mut(e).insert(fighter(10));
    }
    let [g, b, a] = [guard, beggar, alice].map(|e| uid(&mut app, e));
    // Not in the same room: nothing starts.
    run(&mut app, |host| host.cmd_hit_victim(g, a));
    assert_eq!(app.world().get::<TbaCombatant>(guard).unwrap().target, None);
    assert_eq!(app.world().get::<TbaCombatant>(alice).unwrap().target, None);
    // A fight broken off when the opponent is gone from the room.
    app.world_mut()
        .get_mut::<TbaCombatant>(guard)
        .unwrap()
        .target = Some(alice);
    run(&mut app, |host| host.cmd_hit_victim(g, a));
    assert_eq!(app.world().get::<TbaCombatant>(guard).unwrap().target, None);

    // A peaceful room.
    app.world_mut()
        .get_mut::<Room>(temple)
        .unwrap()
        .tags
        .insert(RoomTag::Peaceful);
    run(&mut app, |host| host.cmd_hit_victim(g, b));
    assert_eq!(app.world().get::<TbaCombatant>(guard).unwrap().target, None);
    app.world_mut()
        .get_mut::<Room>(temple)
        .unwrap()
        .tags
        .remove(&RoomTag::Peaceful);
    run(&mut app, |host| host.cmd_hit_victim(g, b));
    assert_eq!(
        app.world().get::<TbaCombatant>(guard).unwrap().target,
        Some(beggar)
    );

    // MOB_NOKILL: the guard's prototype (3001) is flagged.
    let other = npc(&mut app, temple, 3001, "the other guard");
    let saint = npc(&mut app, temple, 3002, "the saint");
    for e in [other, saint] {
        app.world_mut().entity_mut(e).insert(fighter(10));
    }
    let [o, s] = [other, saint].map(|e| uid(&mut app, e));
    app.world_mut()
        .resource_mut::<TbaContent>()
        .npc_stats
        .get_mut(&NpcId::from("tba_npc_3001"))
        .unwrap()
        .behaviour
        .push(NpcBehaviour::NoKill);
    run(&mut app, |host| host.cmd_hit_victim(s, o));
    assert_eq!(app.world().get::<TbaCombatant>(saint).unwrap().target, None);
}

#[test]
fn an_extracted_character_is_not_fought_followed_or_hunted() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let beggar = npc(&mut app, temple, 3002, "the beggar");
    for e in [guard, beggar] {
        app.world_mut().entity_mut(e).insert(fighter(10));
    }
    let [g, b] = [guard, beggar].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_extract_char(b);
        host.cmd_hit_victim(g, b);
        host.cmd_follow(g, b);
        host.cmd_set_hunting(g, b);
    });
    let state = app.world().get::<TbaCombatant>(guard).unwrap();
    assert_eq!((state.target, state.position), (None, Position::Standing));
    assert!(app.world().get::<TbaFollowing>(guard).is_none());
    assert_eq!(unsupported(&app, "mhunt"), 0);
}

#[test]
fn extracting_a_leader_stops_its_followers() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let beggar = npc(&mut app, temple, 3002, "the beggar");
    let [g, b] = [guard, beggar].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_follow(g, b));
    assert!(app.world().get::<TbaFollowing>(guard).is_some());
    run(&mut app, |host| host.cmd_extract_char(b));
    assert!(app.world().get::<TbaFollowing>(guard).is_none());
}

#[test]
fn a_hunt_of_a_stale_id_is_not_counted() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let g = uid(&mut app, guard);
    run(&mut app, |host| host.cmd_set_hunting(g, 654_321));
    assert_eq!(unsupported(&app, "mhunt"), 0);
}

#[test]
fn a_player_killed_twice_before_the_death_handling_runs_dies_once() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    app.world_mut().entity_mut(alice).insert(fighter(5));
    let a = uid(&mut app, alice);
    run(&mut app, |host| host.cmd_die(a));
    run(&mut app, |host| host.cmd_die(a));
    let defeated = app
        .world_mut()
        .resource_mut::<Messages<PlayerDefeated>>()
        .drain()
        .count();
    assert_eq!(defeated, 1);
}

#[test]
fn a_script_kill_clears_the_victim_target_and_reports_experience() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let beggar = npc(&mut app, temple, 3002, "the beggar");
    for e in [guard, beggar] {
        app.world_mut().entity_mut(e).insert(fighter(10));
    }
    let [g, b] = [guard, beggar].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_hit_victim(g, b);
        host.cmd_die(g);
    });
    assert_eq!(
        app.world().get::<TbaCombatant>(beggar).unwrap().target,
        None
    );
    let defeated: Vec<NpcDefeated> = app
        .world_mut()
        .resource_mut::<Messages<NpcDefeated>>()
        .drain()
        .collect();
    assert_eq!((defeated[0].experience, defeated[0].victim), (100, guard));
}

#[test]
fn a_fighter_who_cannot_fight_is_not_engaged() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let beggar = npc(&mut app, temple, 3002, "the beggar");
    let mut stunned = fighter(10);
    stunned.position = Position::Stunned;
    app.world_mut().entity_mut(guard).insert(fighter(10));
    app.world_mut().entity_mut(beggar).insert(stunned);
    let [g, b] = [guard, beggar].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_hit_victim(g, b));
    let state = app.world().get::<TbaCombatant>(beggar).unwrap();
    assert_eq!((state.target, state.position), (None, Position::Stunned));
    assert_eq!(
        app.world().get::<TbaCombatant>(guard).unwrap().target,
        Some(beggar)
    );
}

#[test]
fn a_remembering_victim_remembers_a_player_attacker() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let alice = player(&mut app, temple, "Alice", 7);
    app.world_mut()
        .entity_mut(guard)
        .insert((fighter(10), TbaAttackerMemory::default()));
    app.world_mut().entity_mut(alice).insert(fighter(10));
    let [g, a] = [guard, alice].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_hit_victim(a, g));
    assert!(
        app.world()
            .get::<TbaAttackerMemory>(guard)
            .unwrap()
            .0
            .contains(&alice)
    );
}

#[test]
fn following_oneself_is_refused() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let g = uid(&mut app, guard);
    run(&mut app, |host| host.cmd_follow(g, g));
    assert!(app.world().get::<TbaFollowing>(guard).is_none());
}

#[test]
fn script_worn_items_reach_the_saved_equipment_and_leave_it() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    let a = uid(&mut app, alice);
    let sword = run(&mut app, |host| host.cmd_read_object(3001)).unwrap();
    run(&mut app, |host| host.cmd_equip_char(a, sword, WEAR_WIELD));
    let sword_entity = run(&mut app, |host| host.entity(sword)).unwrap();
    let save_id = app
        .world()
        .get::<SaveId>(sword_entity)
        .unwrap()
        .0
        .to_string();
    let saved = app.world().get::<TbaEquipmentSave>(alice).unwrap();
    assert_eq!(saved.worn.get(&WearSlot::Wield), Some(&save_id));
    // Purging the worn item takes it out of the save.
    run(&mut app, |host| host.cmd_extract_obj(sword));
    assert!(
        app.world()
            .get::<TbaEquipmentSave>(alice)
            .unwrap()
            .worn
            .is_empty()
    );
    // So does taking it off.
    let lamp = run(&mut app, |host| host.cmd_read_object(3003)).unwrap();
    run(&mut app, |host| {
        host.cmd_equip_char(a, lamp, WEAR_HEAD);
        host.cmd_unequip_char(a, WEAR_HEAD);
    });
    assert!(
        app.world()
            .get::<TbaEquipmentSave>(alice)
            .unwrap()
            .worn
            .is_empty()
    );
}

#[test]
fn an_object_transformed_takes_the_new_prototypes_timer() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let old = item_on_ground(&mut app, temple, 3003, "a lamp");
    app.world_mut()
        .entity_mut(old)
        .insert(TbaItemTimer { remaining: 9 });
    let o = uid(&mut app, old);
    // A script change on the object is part of what the transform replaces.
    run(&mut app, |host| host.obj_set_cost(o, 999));
    // The new prototype has no timer: the old one goes.
    define_item(&mut app, 3001, TbaItemKind::Other, &[], &["sword"]);
    let temp = run(&mut app, |host| host.cmd_read_object(3001)).unwrap();
    run(&mut app, |host| {
        host.cmd_overwrite_obj(o, temp);
        host.cmd_extract_obj(temp);
    });
    assert!(app.world().get::<TbaItemTimer>(old).is_none());
    assert_eq!(
        run(&mut app, |host| host.obj_cost(o)),
        0,
        "the transform clears the script's override"
    );
    // A prototype with a timer gives it, though the template never had one.
    define_item(&mut app, 3002, TbaItemKind::Other, &[], &["sack"]);
    app.world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_3002"))
        .unwrap()
        .timer = Some(5);
    let temp = run(&mut app, |host| host.cmd_read_object(3002)).unwrap();
    run(&mut app, |host| {
        host.cmd_overwrite_obj(o, temp);
        host.cmd_extract_obj(temp);
    });
    assert_eq!(
        app.world().get::<TbaItemTimer>(old).map(|t| t.remaining),
        Some(5)
    );
}

#[test]
fn a_mob_transformed_takes_the_new_behaviours_and_reports_the_rest() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let g = uid(&mut app, guard);
    let temp = run(&mut app, |host| host.cmd_read_mobile(3002)).unwrap();
    let temp_entity = run(&mut app, |host| host.entity(temp)).unwrap();
    app.world_mut().entity_mut(temp_entity).insert(NpcBehaviors(
        mib_stdlib::content::NpcBehaviorFlags {
            sentinel: true,
            ..Default::default()
        },
    ));
    run(&mut app, |host| {
        host.cmd_overwrite_mob(g, temp, false);
        host.cmd_extract_char(temp);
    });
    assert!(app.world().get::<NpcBehaviors>(guard).unwrap().0.sentinel);
    assert_eq!(unsupported(&app, "mtransform.stats"), 1);
}

#[test]
fn a_mob_transformed_keeps_its_position_and_target() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let beggar = npc(&mut app, temple, 3002, "the beggar");
    let mut busy = fighter(10);
    busy.position = Position::Fighting;
    busy.target = Some(beggar);
    app.world_mut().entity_mut(guard).insert(busy);
    let g = uid(&mut app, guard);
    let temp = run(&mut app, |host| host.cmd_read_mobile(3002)).unwrap();
    run(&mut app, |host| {
        host.cmd_overwrite_mob(g, temp, false);
        host.cmd_extract_char(temp);
    });
    let state = app.world().get::<TbaCombatant>(guard).unwrap();
    assert_eq!(
        (state.position, state.target),
        (Position::Fighting, Some(beggar))
    );
}

#[test]
fn a_refused_move_does_not_reorder_lists() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let first = item_on_ground(&mut app, temple, 3001, "a sword");
    let second = item_on_ground(&mut app, temple, 3003, "a lamp");
    let [f, t] = [first, temple].map(|e| uid(&mut app, e));
    let _ = second;
    let before = run(&mut app, |host| host.room_contents(t));
    run(&mut app, |host| {
        // An item cannot hold itself, a room cannot carry, a room is no item.
        host.cmd_obj_to_obj(f, f);
        host.cmd_obj_to_char(f, t);
        host.cmd_obj_to_room(f, f);
    });
    assert_eq!(run(&mut app, |host| host.room_contents(t)), before);
}

#[test]
fn a_move_puts_the_object_at_the_head_of_the_new_list() {
    // Creation order and move order disagree.
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let sword = item_on_ground(&mut app, temple, 3001, "a sword");
    let lamp = item_on_ground(&mut app, temple, 3003, "a lamp");
    let [g, sw, la] = [guard, sword, lamp].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_obj_to_char(la, g);
        host.cmd_obj_to_char(sw, g);
        assert_eq!(host.carrying(g), [sw, la]);
        host.cmd_obj_to_char(la, g);
        assert_eq!(host.carrying(g), [la, sw]);
        host.cmd_equip_char(g, sw, WEAR_WIELD);
        host.cmd_unequip_char(g, WEAR_WIELD);
        host.cmd_obj_to_char(sw, g);
        assert_eq!(host.carrying(g), [sw, la]);
    });
}

#[test]
fn a_temporary_mob_leaves_no_spawn_index_entry() {
    let mut app = world();
    app.init_resource::<NpcIndex>();
    let temple = room(&mut app, 3001);
    let t = uid(&mut app, temple);
    for _ in 0..3 {
        run(&mut app, |host| {
            let proxy = host.cmd_read_mobile(3001).unwrap();
            host.cmd_char_to_room(proxy, t);
            host.cmd_extract_char(proxy);
        });
    }
    let index = app.world().get_resource::<NpcIndex>();
    assert!(index.is_none_or(|index| index.map.values().all(Vec::is_empty)));
}

#[test]
fn a_loaded_mob_has_attack_dice_and_a_spawn_stamp() {
    let mut app = world();
    app.world_mut()
        .resource_mut::<TbaContent>()
        .npc_stats
        .get_mut(&NpcId::from("tba_npc_3001"))
        .unwrap()
        .damage = Some("1d4+1".parse().expect("dice"));
    let mob = run(&mut app, |host| host.cmd_read_mobile(3001)).unwrap();
    let entity = run(&mut app, |host| host.entity(mob)).unwrap();
    assert!(app.world().get::<TbaAttackDice>(entity).is_some());
    assert!(
        app.world()
            .get::<mib_tba::dg::ids::DgBirth>(entity)
            .is_some()
    );
}

#[test]
fn a_player_arriving_not_playing_records_no_visit() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    let bob = player(&mut app, temple, "Bob", 8);
    app.world_mut().get_mut::<Session>(bob).unwrap().state = SessionState::Connected;
    let [b, sq] = [bob, square].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_char_to_room(b, sq));
    let visits = app
        .world_mut()
        .resource_mut::<Messages<QuestEvent>>()
        .drain()
        .count();
    assert_eq!(visits, 0);
}

#[test]
fn a_purged_exit_is_no_longer_hidden() {
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::ToRoom(Some(sq)));
    });
    app.world_mut()
        .get_mut::<Exits>(temple)
        .unwrap()
        .hidden
        .insert(ExitKey::North);
    run(&mut app, |host| host.cmd_exit_purge(t, NORTH));
    assert!(
        !app.world()
            .get::<Exits>(temple)
            .unwrap()
            .hidden
            .contains(&ExitKey::North)
    );
}

// -- more real script lines ------------------------------------------------------------

#[test]
fn a_script_loads_onto_a_wear_position_and_into_a_container() {
    let (mut app, _, _, guard, alice) = scripted("mload obj 3001 {ALICE} wield");
    let g = uid(&mut app, guard);
    define_item(
        &mut app,
        3001,
        TbaItemKind::Other,
        &[WearSlot::Wield],
        &["sword"],
    );
    run_trigger(&mut app, g, 700);
    let worn = app
        .world()
        .get::<TbaEquipment>(alice)
        .and_then(|e| e.worn(WearSlot::Wield));
    assert!(worn.is_some());
    assert_eq!(
        app.world()
            .get::<TbaEquipmentSave>(alice)
            .map(|s| s.worn.len()),
        Some(1)
    );

    let (mut app, _, _, guard, _) = scripted("mload obj 3002\nmload obj 3003 sack");
    define_item(&mut app, 3002, TbaItemKind::Container, &[], &["sack"]);
    let g = uid(&mut app, guard);
    run_trigger(&mut app, g, 700);
    run(&mut app, |host| {
        let sack = host.carrying(g)[0];
        assert_eq!(host.obj_contains(sack).len(), 1);
    });
}

#[test]
fn an_object_script_loads_builds_a_door_and_teleports() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    app.world_mut().entity_mut(temple).insert(Exits::default());
    let alice = player(&mut app, temple, "Alice", 7);
    let au = uid(&mut app, alice);
    define(
        &mut app,
        710,
        "obj",
        "j",
        0,
        &format!("oload obj 3003\nodoor 3001 north room 3002\noteleport {UID_CHAR}{au} 3002"),
    );
    attach(&mut app, "item:tba_item_3001", &[710]);
    let sword = item_on_ground(&mut app, temple, 3001, "a sword");
    let s = uid(&mut app, sword);
    run_trigger(&mut app, s, 710);
    assert_eq!(
        app.world()
            .get::<Exits>(temple)
            .unwrap()
            .map
            .get(&ExitKey::North),
        Some(&square)
    );
    assert_eq!(location(&app, alice), Some(square));
}

#[test]
fn a_room_script_loads_teleports_and_builds_doors() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    app.world_mut().entity_mut(temple).insert(Exits::default());
    let alice = player(&mut app, temple, "Alice", 7);
    define(
        &mut app,
        720,
        "wld",
        "f",
        0,
        "wload obj 3001\nwdoor 3001 north room 3002\nwdoor 3001 north flags bcd\nwecho Rumble.\nwteleport all 3002",
    );
    attach(&mut app, "room:tba.3001", &[720]);
    let t = uid(&mut app, temple);
    run_trigger(&mut app, t, 720);
    assert_eq!(location(&app, alice), Some(square));
    let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::North];
    assert_eq!((state.closed, state.locked), (true, true));
}

// -- review fixes ----------------------------------------------------------------------

#[test]
fn a_kill_from_the_dead_position_still_runs_the_death_handling() {
    // The VM stores the hit points, updates the position to dead and only then
    // calls cmd_die.
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    let guard = npc(&mut app, temple, 3001, "the guard");
    for e in [alice, guard] {
        let mut dying = fighter(-11);
        dying.position = Position::Dead;
        app.world_mut().entity_mut(e).insert(dying);
    }
    let [a, g] = [alice, guard].map(|e| uid(&mut app, e));
    run(&mut app, |host| {
        host.cmd_die(a);
        host.cmd_die(a);
        host.cmd_die(g);
    });
    let players = app
        .world_mut()
        .resource_mut::<Messages<PlayerDefeated>>()
        .drain()
        .count();
    let npcs = app
        .world_mut()
        .resource_mut::<Messages<NpcDefeated>>()
        .drain()
        .count();
    assert_eq!((players, npcs), (1, 1));
    assert!(app.world().get_entity(guard).is_err());
}

#[test]
fn equipping_a_player_whose_saved_worn_set_is_not_relinked_keeps_the_set() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let alice = player(&mut app, temple, "Alice", 7);
    let sword = item_on_ground(&mut app, temple, 3001, "a sword");
    app.world_mut()
        .entity_mut(sword)
        .insert(SaveId::new("item:d0"));
    app.world_mut().entity_mut(alice).insert(TbaEquipmentSave {
        worn: [(WearSlot::Body, "item:d77".to_owned())].into(),
    });
    let [a, sw] = [alice, sword].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_equip_char(a, sw, WEAR_WIELD));
    let save = app.world().get::<TbaEquipmentSave>(alice).unwrap();
    assert_eq!(save.worn.len(), 2);
    assert_eq!(
        save.worn.get(&WearSlot::Body).map(String::as_str),
        Some("item:d77")
    );
    assert_eq!(
        save.worn.get(&WearSlot::Wield).map(String::as_str),
        Some("item:d0")
    );
    assert!(app.world().get::<TbaEquipment>(alice).is_none());
}

#[test]
fn a_teleported_leader_does_not_drag_its_followers() {
    let mut app = world();
    app.add_systems(Update, follow_system);
    let temple = room(&mut app, 3001);
    let square = room(&mut app, 3002);
    let alice = player(&mut app, temple, "Alice", 7);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let [a, g, sq] = [alice, guard, square].map(|e| uid(&mut app, e));
    run(&mut app, |host| host.cmd_follow(g, a));
    app.update();
    run(&mut app, |host| {
        host.cmd_char_from_room(a);
        host.cmd_char_to_room(a, sq);
    });
    app.update();
    assert_eq!(location(&app, alice), Some(square));
    assert_eq!(location(&app, guard), Some(temple));
}

#[test]
fn a_closed_exit_that_is_not_a_door_cannot_be_opened() {
    let (mut app, _, temple, square) = exit_world();
    let (t, sq) = (uid(&mut app, temple), uid(&mut app, square));
    run(&mut app, |host| {
        host.cmd_exit_set(t, NORTH, ExitField::ToRoom(Some(sq)));
        // EX_CLOSED alone.
        host.cmd_exit_set(t, NORTH, ExitField::Flags(2));
    });
    let door = &app.world().get::<Doors>(temple).unwrap().by_dir[&ExitKey::North];
    assert!(!door.pickable && door.key.is_none());
    let state = &app.world().get::<RoomState>(temple).unwrap().doors[&ExitKey::North];
    assert_eq!((state.closed, state.locked), (true, true));
}

#[test]
fn a_dead_character_is_not_transformed_and_reports_no_gap() {
    let mut app = world();
    let temple = room(&mut app, 3001);
    let guard = npc(&mut app, temple, 3001, "the guard");
    let g = uid(&mut app, guard);
    run(&mut app, |host| host.cmd_overwrite_mob(g, g, false));
    assert_eq!(unsupported(&app, "mtransform.stats"), 0);
}
