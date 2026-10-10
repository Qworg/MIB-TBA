//! TBA pack tests: this repository *is* the `packs/tba` pack.
//!
//! Covers the converted tbaMUD world that Phase 10 lands in `content/`:
//!
//! 1. the `tba.*` sections register, and the full pack loads and validates
//!    cleanly alongside the engine's `packs/stdlib`;
//! 2. the world instantiates through the real `StdlibPlugin` boot path, with
//!    the `start_room` tagged (the Temple of Midgaard, vnum 3001);
//! 3. `TbaModule::install` populates the `TbaContent` resource from the YAML;
//! 4. zone 30 (Northern Midgaard) keeps a readable per-zone regression pin;
//! 5. a dangling `tba.zones` command reference is a validation issue naming
//!    the exact field path.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use mib_content::prelude::{MibModule, SectionRegistry, load_packs, registry_for, validate_all};
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    ContentRegistry, ItemId, Modules, NpcId, RoomId, RoomIndex, StdlibModule, StdlibPlugin,
};
use mib_tba::prelude::{
    ShopDef, ShopsSection, SpellKind, SpellTarget, SpellsSection, TbaClassesSection, TbaContent,
    TbaItemsSection, TbaModule, TbaNpcStatsSection, TbaPlugin, ZonesSection,
};

/// Exact counts from the full tbaMUD conversion now in `content/` (Phase 10).
const TBA_AREAS: usize = 189;
const TBA_ROOMS: usize = 12_700;
const TBA_NPCS: usize = 3_705;
/// `stdlib.items` / `tba.items` count the 4,765 source objects **plus** the
/// converter's synthetic `tba_gold` prototype (TBA-E09).
const TBA_ITEMS: usize = 4_766;
const TBA_ZONES: usize = 189;
const TBA_CLASSES: usize = 4;
const TBA_SPELLS: usize = 54;
const TBA_SHOPS: usize = 327;

/// Zone 30, Northern Midgaard — the converted heartland the Phase 9.1 scaffold
/// was pinned to. Its per-zone counts keep a targeted regression readable
/// without scanning the whole 12,700-room pack.
const ZONE_30_ROOMS: usize = 58;
const ZONE_30_NPCS: usize = 46;
const ZONE_30_ITEMS: usize = 63;
const ZONE_30_COMMANDS: usize = 169;

/// The engine's packs, in the sibling MIB checkout: `code/` → repo → `MIB/packs`.
fn engine_pack(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../MIB/packs")
        .join(name)
}

/// This repository is the pack; `code/` sits one level below the pack root, so
/// the pack root is `code/..` — the directory holding `pack.toml`.
fn this_pack() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The registry a `stdlib` + `tba` build links. Not
/// `mib_classic_rpg::full_registry()`: TBA is an independent game and ships no
/// `rpg.*` sections.
fn registry() -> SectionRegistry {
    let modules: [&dyn MibModule; 2] = [&StdlibModule, &TbaModule];
    registry_for(&modules)
}

/// Compose the real plugins with the `stdlib` + `tba` registry and the
/// requested roots, as a game binary does, then run `Startup` once.
///
/// This pack declares `[code] module = "tba"`, so the composition must declare
/// the code it links — without `Modules` the world is refused
/// (`PackError::MissingModule`) instead of silently instantiating nothing.
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

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mib-tba-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn tba_pack_loads_and_validates_with_the_expected_sections() {
    let assets = load_packs(&[engine_pack("stdlib"), this_pack()], &registry())
        .expect("tba pack loads with the engine's stdlib");
    assert!(
        validate_all(&assets, &registry()).is_empty(),
        "validates cleanly"
    );

    let tba = assets
        .iter()
        .find(|asset| asset.id().as_str() == "tba")
        .expect("tba asset present");

    assert_eq!(
        tba.section::<ZonesSection>().expect("zones").zones.len(),
        TBA_ZONES
    );
    assert_eq!(
        tba.section::<TbaNpcStatsSection>()
            .expect("npc_stats")
            .npc_stats
            .len(),
        TBA_NPCS
    );
    assert_eq!(
        tba.section::<TbaItemsSection>().expect("items").items.len(),
        TBA_ITEMS
    );
    assert_eq!(
        tba.section::<TbaClassesSection>()
            .expect("classes")
            .classes
            .len(),
        TBA_CLASSES
    );
    assert_eq!(
        tba.section::<SpellsSection>().expect("spells").spells.len(),
        TBA_SPELLS
    );
    assert_eq!(
        tba.section::<ShopsSection>().expect("shops").shops.len(),
        TBA_SHOPS
    );
    // Zone 30's weaponsmith is a real converted shop (30.shp): it buys only
    // weapons and stocks the six 3020-series blades and clubs.
    let weaponsmith: &ShopDef = tba
        .section::<ShopsSection>()
        .expect("shops")
        .shops
        .get(&NpcId::from("tba_npc_3003"))
        .expect("zone-30 weaponsmith shop");
    assert_eq!(weaponsmith.buy_multiplier, 1.3);
    assert_eq!(weaponsmith.sell_multiplier, 0.4);
    assert_eq!(weaponsmith.produce.len(), 6);
    assert_eq!(
        weaponsmith.produce.first(),
        Some(&ItemId::from("tba_item_3020"))
    );

    // The world itself lives in the `stdlib.*` sections this pack ships.
    let rooms = tba
        .section::<mib_stdlib::prelude::RoomsSection>()
        .expect("rooms");
    assert_eq!(rooms.rooms.len(), TBA_ROOMS);
    let areas = tba
        .section::<mib_stdlib::prelude::AreasSection>()
        .expect("areas");
    assert_eq!(areas.areas.len(), TBA_AREAS);
    let items = tba
        .section::<mib_stdlib::prelude::ItemsSection>()
        .expect("stdlib items");
    assert_eq!(items.items.len(), TBA_ITEMS);
}

/// Zone 30 (Northern Midgaard) is the converted heartland. Pin its name,
/// bounds, reset program and per-zone counts so a regression in the middle of
/// a 12,700-room pack is still readable.
#[test]
fn zone_30_is_northern_midgaard() {
    use mib_tba::prelude::ZoneCommand;

    let assets =
        load_packs(&[engine_pack("stdlib"), this_pack()], &registry()).expect("tba pack loads");
    let tba = assets
        .iter()
        .find(|asset| asset.id().as_str() == "tba")
        .expect("tba asset present");

    let zone = tba
        .section::<ZonesSection>()
        .expect("zones")
        .zones
        .get(&30)
        .expect("zone 30");
    assert_eq!(zone.name, "Northern Midgaard");
    assert_eq!(zone.bot, 3000);
    assert_eq!(zone.top, 3099);
    assert_eq!(zone.commands.len(), ZONE_30_COMMANDS);
    // The full 30.zon reset program, by command kind. A silent mapping
    // regression would shrink one of these buckets.
    let kinds = |pred: fn(&ZoneCommand) -> bool| zone.commands.iter().filter(|c| pred(c)).count();
    assert_eq!(kinds(|c| matches!(c, ZoneCommand::PlaceMob { .. })), 57);
    assert_eq!(kinds(|c| matches!(c, ZoneCommand::GiveItem { .. })), 62);
    assert_eq!(kinds(|c| matches!(c, ZoneCommand::EquipItem { .. })), 24);

    let areas = tba
        .section::<mib_stdlib::prelude::AreasSection>()
        .expect("areas");
    assert_eq!(
        areas
            .areas
            .get(&mib_stdlib::prelude::AreaId::from("tba_30"))
            .expect("area tba_30")
            .name,
        "Northern Midgaard"
    );

    let rooms = tba
        .section::<mib_stdlib::prelude::RoomsSection>()
        .expect("rooms");
    assert_eq!(
        rooms
            .rooms
            .values()
            .filter(|room| room.area.as_ref() == "tba_30")
            .count(),
        ZONE_30_ROOMS
    );

    let stats = tba.section::<TbaNpcStatsSection>().expect("npc_stats");
    assert_eq!(
        stats
            .npc_stats
            .keys()
            .filter(|id| vnum_in_zone(id.as_ref(), "tba_npc_"))
            .count(),
        ZONE_30_NPCS
    );
    let items = tba
        .section::<mib_stdlib::prelude::ItemsSection>()
        .expect("stdlib items");
    assert_eq!(
        items
            .items
            .keys()
            .filter(|id| vnum_in_zone(id.as_ref(), "tba_item_"))
            .count(),
        ZONE_30_ITEMS
    );

    // Known zone-30 records resolve to their converted ids with real values.
    assert_eq!(
        items
            .items
            .get(&ItemId::from("tba_item_3030"))
            .expect("a torch")
            .name,
        "a torch"
    );
    let guard = stats
        .npc_stats
        .get(&NpcId::from("tba_npc_3060"))
        .expect("the cityguard");
    assert_eq!(guard.hp.to_string(), "2d2+100");
    assert_eq!(guard.level, 10);
    assert_eq!(guard.gold, 100);
}

/// True when a converted `tba_npc_<vnum>` / `tba_item_<vnum>` id names a vnum
/// inside zone 30 (`[3000, 3099]`).
fn vnum_in_zone(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .and_then(|vnum| vnum.parse::<u32>().ok())
        .is_some_and(|vnum| (3000..=3099).contains(&vnum))
}

#[test]
fn tba_class_tables_are_the_circle_tables() {
    // Guards the hand-authored `tba.classes.yaml` against a transcription
    // slip: these are exact rows from tbaMUD `src/class.c`.
    let assets =
        load_packs(&[engine_pack("stdlib"), this_pack()], &registry()).expect("tba pack loads");
    let tba = assets
        .iter()
        .find(|asset| asset.id().as_str() == "tba")
        .expect("tba asset present");
    let classes = tba.section::<TbaClassesSection>().expect("classes");

    let mage = classes.classes.get("magic_user").expect("magic_user");
    assert_eq!(mage.thaco_table[0], 100, "level-0 THAC0 sentinel");
    assert_eq!(mage.thaco_table[1], 20, "magic-user level 1 THAC0");
    assert_eq!(mage.thaco_table[4], 19, "magic-user level 4 THAC0");
    assert_eq!(mage.thaco_table.len(), 35, "levels 0..=34 (LVL_IMPL)");
    assert_eq!(mage.xp_table[0], 0, "level-0 XP sentinel");
    assert_eq!(mage.xp_table[2], 2500, "magic-user level-2 XP threshold");
    assert_eq!(mage.xp_table[3], 5000, "magic-user level-3 XP threshold");
    assert_eq!(mage.xp_table.len(), 32, "levels 0..=31 (LVL_IMMORT)");

    let warrior = classes.classes.get("warrior").expect("warrior");
    assert_eq!(warrior.thaco_table[5], 16, "warrior level 5 THAC0");
    assert_eq!(warrior.thaco_table[34], 1, "warrior level-34 THAC0");
    assert_eq!(warrior.xp_table[2], 2000, "warrior level-2 XP threshold");
}

#[test]
fn tba_spell_catalog_is_the_circle_catalog() {
    // Guards the hand-authored `tba.spells.yaml` against a transcription
    // slip: ids, mana and target types from tbaMUD's `spells.h` and
    // `spell_parser.c` `mag_assign_spells`.
    let assets =
        load_packs(&[engine_pack("stdlib"), this_pack()], &registry()).expect("tba pack loads");
    let tba = assets
        .iter()
        .find(|asset| asset.id().as_str() == "tba")
        .expect("tba asset present");
    let spells = tba.section::<SpellsSection>().expect("spells");

    let missile = spells.spells.get("magic_missile").expect("magic_missile");
    assert_eq!(missile.name, "magic missile");
    assert_eq!(missile.mana, 10, "spell_parser.c mana_min");
    assert_eq!(missile.kind, SpellKind::Damage);
    assert_eq!(missile.target, SpellTarget::Victim);
    assert_eq!(missile.dice.expect("damage dice").to_string(), "1d8+1");

    let heal = spells.spells.get("cure_light").expect("cure_light");
    assert_eq!(heal.kind, SpellKind::Heal);
    assert_eq!(heal.mana, 10);

    let armor = spells.spells.get("armor").expect("armor");
    assert_eq!(armor.kind, SpellKind::Buff);
    assert!(armor.affect.is_some(), "armor installs APPLY_AC");

    let detect = spells.spells.get("detect_invis").expect("detect_invis");
    assert_eq!(detect.kind, SpellKind::Detect);
    assert_eq!(detect.target, SpellTarget::Caster);

    // Every `granted_skills` name that is a spell must resolve in the catalog;
    // the fighter/thief entries (kick, bash, sneak, ...) are skills, not
    // spells, so they are exempt.
    let classes = tba.section::<TbaClassesSection>().expect("classes");
    let spell_ids: std::collections::BTreeSet<String> =
        spells.spells.keys().map(|id| id.to_string()).collect();
    for (class_id, class) in &classes.classes {
        for skill in &class.granted_skills {
            if matches!(
                skill.as_str(),
                "kick" | "bash" | "rescue" | "sneak" | "hide" | "pick_lock" | "backstab"
            ) {
                continue;
            }
            assert!(
                spell_ids.contains(skill.as_str()),
                "class {class_id} grants unknown spell `{skill}`"
            );
        }
    }
}

#[test]
fn tba_world_instantiates_and_installs_the_content_resource() {
    let app = boot();

    let index = app.world().resource::<RoomIndex>();
    assert_eq!(index.map.len(), TBA_ROOMS, "every tba room instantiates");
    let start = index.start.expect("a start_room is tagged");
    let temple = *index
        .map
        .get(&RoomId::from("tba.3001"))
        .expect("the Temple of Midgaard exists");
    assert_eq!(start, temple, "the Temple of Midgaard is the start room");

    // `TbaModule::install` ran and turned the YAML into a resource.
    let content = app.world().resource::<TbaContent>();
    assert_eq!(content.zones.len(), TBA_ZONES);
    assert!(
        content.zones.contains_key(&30),
        "zone 30 is installed: {:?}",
        content.zones.keys()
    );
    assert_eq!(content.npc_stats.len(), TBA_NPCS);
    assert!(
        content.npc_stats.contains_key(&NpcId::from("tba_npc_3060")),
        "the cityguard's Diku stats are installed"
    );
    assert_eq!(content.items.len(), TBA_ITEMS);
    assert_eq!(content.shops.len(), TBA_SHOPS);
    assert_eq!(content.classes.len(), TBA_CLASSES);
    assert_eq!(content.spells.len(), TBA_SPELLS);
    assert!(
        content.classes.contains_key("magic_user"),
        "the four Diku classes are installed: {:?}",
        content.classes.keys()
    );
}

#[test]
fn dangling_zone_command_reference_names_the_field_path() {
    let dir = temp_dir("dangling-zone-ref");
    std::fs::create_dir_all(dir.join("content")).expect("mkdir content");
    std::fs::write(
        dir.join("pack.toml"),
        "id = \"tba-bad\"\nversion = \"1\"\ntitle = \"bad\"\ndepends_on = [\"stdlib\"]\n",
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("content/tba.zones.yaml"),
        "zones:\n  5:\n    name: Broken\n    bot: 1\n    top: 2\n    commands:\n    - type: give_item\n      item: nonexistent_item\n",
    )
    .expect("write zones");

    let assets = load_packs(&[engine_pack("stdlib"), dir.clone()], &registry()).expect("loads");
    let issues = validate_all(&assets, &registry());
    let issue = issues
        .iter()
        .find(|issue| issue.field.as_deref() == Some("zones.5.commands[0].item"))
        .unwrap_or_else(|| panic!("expected a zones.5.commands[0].item issue, got {issues:?}"));
    assert!(
        issue.message.contains("nonexistent_item"),
        "issue names the bad target: {}",
        issue.message
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spell_missing_dice_names_the_field_path() {
    // The real validation path (`load_packs` + `validate_all`) rejects a
    // damage spell with no dice, not just the pure helper's unit test.
    let dir = temp_dir("bad-spell");
    std::fs::create_dir_all(dir.join("content")).expect("mkdir content");
    std::fs::write(
        dir.join("pack.toml"),
        "id = \"tba-bad\"\nversion = \"1\"\ntitle = \"bad\"\ndepends_on = [\"stdlib\"]\n",
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("content/tba.spells.yaml"),
        "spells:\n  fireball:\n    name: fireball\n    mana: 30\n    kind: damage\n    target: victim\n",
    )
    .expect("write spells");

    let assets = load_packs(&[engine_pack("stdlib"), dir.clone()], &registry()).expect("loads");
    let issues = validate_all(&assets, &registry());
    let issue = issues
        .iter()
        .find(|issue| issue.field.as_deref() == Some("spells.fireball.dice"))
        .unwrap_or_else(|| panic!("expected a spells.fireball.dice issue, got {issues:?}"));
    assert!(
        issue.message.contains("dice"),
        "issue explains the missing roll: {}",
        issue.message
    );

    let _ = std::fs::remove_dir_all(&dir);
}
