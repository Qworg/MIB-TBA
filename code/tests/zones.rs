//! TBA zone-reset `E` (equip) integration tests (issue #4).
//!
//! These drive the real [`zone_reset_system`] over a synthetic world: a
//! `PlaceMob` spawns an NPC and an `EquipItem` reset puts a weapon in its
//! [`WearSlot::Wield`]. The NPC's runtime [`TbaEquipment`] map, the worn-item
//! affect recompute, and the [`WieldedWeapon`] seam are then asserted through
//! the same systems the plugin wires.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use mib_content::prelude::{load_packs, registry_for};
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    CarriedBy, Item, ItemId, LoadedContent, NpcId, NpcKey, RoomId, RoomIndex, StdlibModule,
};
use mib_tba::prelude::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mib-tba-zones-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Write a one-pack world with a single NPC and item prototype, then load it
/// through the real content pipeline so `find_npc_def`/`find_item_def` work.
fn loaded_content(dir: &Path) -> LoadedContent {
    std::fs::create_dir_all(dir.join("content")).expect("mkdir content");
    std::fs::write(
        dir.join("pack.toml"),
        "id = \"tba-zones-test\"\nversion = \"1\"\ntitle = \"zones test\"\n",
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("content/stdlib.npcs.yaml"),
        "npcs:\n  test_mob:\n    name: a test mob\n    persona: A test mob.\n    dialogue: null\n    tags: []\n    behaviors:\n      sentinel: false\n      scavenger: false\n      wander: false\n      wimpy: false\n  other_mob:\n    name: another test mob\n    persona: Another test mob.\n    dialogue: null\n    tags: []\n    behaviors:\n      sentinel: false\n      scavenger: false\n      wander: false\n      wimpy: false\n",
    )
    .expect("write npcs");
    std::fs::write(
        dir.join("content/stdlib.items.yaml"),
        "items:\n  test_sword:\n    name: a test sword\n    description: A test sword.\n    value: 0\n    container: null\n    tags: []\n",
    )
    .expect("write items");
    let registry = registry_for(&[&StdlibModule]);
    let assets = load_packs(&[dir.to_path_buf()], &registry).expect("pack loads");
    LoadedContent(assets)
}

fn npc_stats() -> TbaNpcStats {
    TbaNpcStats {
        hp: Dice::constant(20),
        level: 3,
        hitroll: 1,
        damroll: 2,
        armor_class: 50,
        thaco: 18,
        alignment: -100,
        gold: 0,
        experience: 0,
        default_position: Position::Standing,
        behaviour: Vec::new(),
        damage: None,
        attributes: TbaAttributes::mob_default(),
        saves: TbaSaves::default(),
        spec_proc: None,
    }
}

/// A `2d6+3` sword that also carries `damroll +4` and `armor_class -5` affects,
/// so the equip path is visible in both the worn map and the vitals.
fn sword_def() -> TbaItemDef {
    TbaItemDef {
        kind: TbaItemKind::Weapon,
        slots: vec![WearSlot::Wield],
        values: [0, 2, 6, 3],
        affects: vec![
            AffectDef {
                location: AffectLocation::Damroll,
                modifier: 4,
            },
            AffectDef {
                location: AffectLocation::ArmorClass,
                modifier: -5,
            },
        ],
        keywords: Vec::new(),
        extra_flags: Vec::new(),
        affect_flags: Vec::new(),
        weight: 0,
        cost: 0,
        rent: 0,
        min_level: 0,
        timer: None,
    }
}

fn content() -> TbaContent {
    TbaContent {
        npc_stats: BTreeMap::from([(NpcId::from("test_mob"), npc_stats())]),
        items: BTreeMap::from([(ItemId::from("test_sword"), sword_def())]),
        zones: BTreeMap::from([(
            5,
            ZoneDef {
                name: "Test Zone".into(),
                bot: 1,
                top: 99,
                lifespan_mins: 1,
                reset_mode: ResetMode::Always,
                commands: vec![
                    ZoneCommand::always(ZoneCommandKind::PlaceMob {
                        mob: NpcId::from("test_mob"),
                        room: RoomId::from("test_room"),
                        max_in_world: 0,
                    }),
                    ZoneCommand::always(ZoneCommandKind::EquipItem {
                        item: ItemId::from("test_sword"),
                        slot: WearSlot::Wield,
                        max_in_world: 0,
                    }),
                ],
            },
        )]),
        ..TbaContent::default()
    }
}

// ---------------------------------------------------------------------------
// Test app
// ---------------------------------------------------------------------------

fn test_app(dir: &Path) -> App {
    let mut app = App::new();
    let content = content();
    app.insert_resource(content.clone());
    app.insert_resource(loaded_content(dir));
    app.insert_resource(Time::<Fixed>::from_hz(10.0));
    app.insert_resource(TbaRng::from_seed(7));
    app.init_resource::<TbaObjectClock>();

    // A single empty entity stands in for the room; only `RoomIndex` matters
    // to `spawn_npc_from_content`.
    let room = app.world_mut().spawn_empty().id();
    app.insert_resource(RoomIndex {
        map: HashMap::from([(RoomId::from("test_room"), room)]),
        ..Default::default()
    });

    // The reset only fires once its age reaches `lifespan_mins`.
    let mut tracker = ZoneTracker::from_zones(&content.zones);
    tracker.zones[0].age_mins = 100.0;
    app.insert_resource(tracker);

    app.add_systems(
        FixedUpdate,
        (
            zone_reset_system,
            npc_combatant_system,
            recompute_npc_equipment_affects,
            sync_wielded_weapon,
        )
            .chain(),
    );
    app
}

fn run_fixed(app: &mut App) {
    app.world_mut().run_schedule(FixedUpdate);
    app.world_mut().flush();
}

fn only_npc(app: &mut App) -> Entity {
    app.world_mut()
        .query::<(Entity, &NpcKey)>()
        .iter(app.world())
        .map(|(entity, _)| entity)
        .next()
        .expect("the reset spawned the mob")
}

fn equipment_of(app: &App, entity: Entity) -> TbaEquipment {
    let equipment = app
        .world()
        .get::<TbaEquipment>(entity)
        .expect("equipment present");
    TbaEquipment {
        slots: equipment.slots.clone(),
    }
}

fn combatant_of(app: &App, entity: Entity) -> TbaCombatant {
    app.world()
        .get::<TbaCombatant>(entity)
        .cloned()
        .expect("combatant present")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn equip_reset_puts_the_item_in_the_mobs_slot() {
    let dir = temp_dir("slot");
    let mut app = test_app(&dir);
    run_fixed(&mut app);

    let npc = only_npc(&mut app);
    let slots = equipment_of(&app, npc).slots;
    let sword = slots.get(&WearSlot::Wield).expect("wield slot filled");
    let item = app.world().get::<Item>(*sword).expect("item entity");
    assert_eq!(item.def_id, ItemId::from("test_sword"));
    assert_eq!(
        app.world()
            .get::<CarriedBy>(*sword)
            .map(|carried| carried.0),
        Some(npc),
        "the worn item is carried so it is a normal, sweepable entity"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn equip_reset_applies_worn_affects_to_npc_vitals() {
    let dir = temp_dir("affects");
    let mut app = test_app(&dir);
    run_fixed(&mut app);

    let npc = only_npc(&mut app);
    let vitals = combatant_of(&app, npc);
    assert_eq!(vitals.damroll, 6, "base damroll 2 + the sword's +4");
    assert_eq!(vitals.armor_class, 45, "base AC 50 + the sword's -5");
    assert_eq!(vitals.hitroll, 1, "an unaffected field keeps the base");
    assert_eq!(vitals.thaco, 18, "THAC0 is not an affect field");
    assert_eq!(vitals.hp, vitals.max_hp, "the recompute never heals");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn equip_reset_syncs_the_npcs_weapon_dice() {
    let dir = temp_dir("dice");
    let mut app = test_app(&dir);
    run_fixed(&mut app);

    let npc = only_npc(&mut app);
    let wielded = app
        .world()
        .get::<WieldedWeapon>(npc)
        .expect("an equipped weapon sets WieldedWeapon");
    assert_eq!(wielded.0, ItemId::from("test_sword"));
    let content = app.world().resource::<TbaContent>();
    assert_eq!(
        weapon_dice(Some(wielded), Some(content)),
        Dice::new(2, 6, 3),
        "combat reads the reset-spawned weapon's dice"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unequipped_mob_keeps_its_unmodified_vitals() {
    // Control: with the `E` command removed, the mob's base vitals stand.
    let dir = temp_dir("control");
    let mut app = test_app(&dir);
    let mut content = app.world().resource::<TbaContent>().clone();
    content
        .zones
        .get_mut(&5)
        .expect("zone 5")
        .commands
        .retain(|command| !matches!(command.command, ZoneCommandKind::EquipItem { .. }));
    app.insert_resource(content.clone());
    let mut tracker = ZoneTracker::from_zones(&content.zones);
    tracker.zones[0].age_mins = 100.0;
    app.insert_resource(tracker);
    run_fixed(&mut app);

    let npc = only_npc(&mut app);
    assert_eq!(combatant_of(&app, npc).damroll, 2, "no weapon, no bonus");
    assert_eq!(combatant_of(&app, npc).armor_class, 50);
    assert!(app.world().get::<WieldedWeapon>(npc).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_reset_does_not_stack_affects() {
    let dir = temp_dir("nostack");
    let mut app = test_app(&dir);
    // Run the reset twice; the mob is despawned/respawned only when the
    // previous one is gone, but `max_in_world = 0` means the second pass adds
    // another mob. Either way, each mob's vitals are computed from its own
    // captured base, never added to.
    run_fixed(&mut app);
    run_fixed(&mut app);

    let mut query = app.world_mut().query::<(Entity, &TbaNpcBase)>();
    let npcs: Vec<Entity> = query.iter(app.world()).map(|(entity, _)| entity).collect();
    assert!(!npcs.is_empty());
    for npc in npcs {
        let vitals = combatant_of(&app, npc);
        assert_eq!(vitals.damroll, 6, "each mob is base 2 + one sword's +4");
        assert_eq!(vitals.armor_class, 45);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A zone for the `if_flag` tests: a successful unlimited placement, then a
/// capped placement that fails, then a conditional placement that should be
/// skipped because its predecessor failed.
fn flag_content(other_if_flag: bool) -> TbaContent {
    TbaContent {
        npc_stats: BTreeMap::from([
            (NpcId::from("test_mob"), npc_stats()),
            (NpcId::from("other_mob"), npc_stats()),
        ]),
        zones: BTreeMap::from([(
            5,
            ZoneDef {
                name: "Flag Zone".into(),
                bot: 1,
                top: 99,
                lifespan_mins: 1,
                reset_mode: ResetMode::Always,
                commands: vec![
                    ZoneCommand::always(ZoneCommandKind::PlaceMob {
                        mob: NpcId::from("test_mob"),
                        room: RoomId::from("test_room"),
                        max_in_world: 0,
                    }),
                    // The mob already exists, so a cap of 1 fails.
                    ZoneCommand::always(ZoneCommandKind::PlaceMob {
                        mob: NpcId::from("test_mob"),
                        room: RoomId::from("test_room"),
                        max_in_world: 1,
                    }),
                    ZoneCommand {
                        if_flag: other_if_flag,
                        command: ZoneCommandKind::PlaceMob {
                            mob: NpcId::from("other_mob"),
                            room: RoomId::from("test_room"),
                            max_in_world: 0,
                        },
                    },
                ],
            },
        )]),
        ..TbaContent::default()
    }
}

fn flag_app(dir: &Path, other_if_flag: bool) -> App {
    let mut app = App::new();
    let content = flag_content(other_if_flag);
    app.insert_resource(content.clone());
    app.insert_resource(loaded_content(dir));
    app.insert_resource(Time::<Fixed>::from_hz(10.0));
    app.insert_resource(TbaRng::from_seed(7));
    app.init_resource::<TbaObjectClock>();
    let room = app.world_mut().spawn_empty().id();
    app.insert_resource(RoomIndex {
        map: HashMap::from([(RoomId::from("test_room"), room)]),
        ..Default::default()
    });
    let mut tracker = ZoneTracker::from_zones(&content.zones);
    tracker.zones[0].age_mins = 100.0;
    app.insert_resource(tracker);
    app.add_systems(FixedUpdate, zone_reset_system);
    app
}

fn npc_count(app: &mut App, id: &str) -> usize {
    app.world_mut()
        .query::<&NpcKey>()
        .iter(app.world())
        .filter(|key| key.0 == NpcId::from(id))
        .count()
}

#[test]
fn if_flag_command_is_skipped_when_its_predecessor_failed() {
    let dir = temp_dir("if-flag");
    let mut app = flag_app(&dir, true);
    run_fixed(&mut app);

    assert_eq!(npc_count(&mut app, "test_mob"), 1, "first placement ran");
    assert_eq!(
        npc_count(&mut app, "other_mob"),
        0,
        "the if_flag command is skipped after a failure"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn if_flag_command_runs_when_its_predecessor_succeeded() {
    // Control: the same program with `if_flag: false` places the second mob,
    // proving the skip above is the flag and not the cap.
    let dir = temp_dir("if-flag-control");
    let mut app = flag_app(&dir, false);
    run_fixed(&mut app);

    assert_eq!(npc_count(&mut app, "test_mob"), 1);
    assert_eq!(
        npc_count(&mut app, "other_mob"),
        1,
        "an unflagged command runs regardless"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn npc_attack_dice_come_from_the_stat_block() {
    let dir = temp_dir("attack-dice");
    let mut app = test_app(&dir);
    {
        let mut content = app.world_mut().resource_mut::<TbaContent>();
        content
            .npc_stats
            .get_mut(&NpcId::from("test_mob"))
            .expect("stats")
            .damage = Some(Dice::new(3, 7, 2));
    }
    run_fixed(&mut app);

    let npc = only_npc(&mut app);
    let attack = app
        .world()
        .get::<TbaAttackDice>(npc)
        .expect("npc_stats.damage becomes TbaAttackDice");
    assert_eq!(attack.0, Dice::new(3, 7, 2));

    let content = app.world().resource::<TbaContent>();
    assert_eq!(
        combat_dice(None, Some(content), Some(attack)),
        Dice::new(3, 7, 2),
        "combat reads the mob's own damage dice"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
