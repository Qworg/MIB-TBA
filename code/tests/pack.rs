//! TBA pack tests: this repository *is* the `packs/tba` pack (Phase 9.1).
//!
//! Covers the architectural scaffold this phase delivers:
//!
//! 1. the `tba.*` sections register, and the pack loads and validates cleanly
//!    alongside the engine's `packs/stdlib`;
//! 2. the world instantiates through the real `StdlibPlugin` boot path, with
//!    the `start_room` tagged;
//! 3. `TbaModule::install` populates the `TbaContent` resource from the YAML;
//! 4. a dangling `tba.zones` command reference is a validation issue naming
//!    the exact field path.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use mib_content::prelude::{MibModule, SectionRegistry, load_packs, registry_for, validate_all};
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    ContentRegistry, Modules, NpcId, RoomId, RoomIndex, StdlibModule, StdlibPlugin,
};
use mib_tba::prelude::{
    TbaClassesSection, TbaContent, TbaItemsSection, TbaModule, TbaNpcStatsSection, TbaPlugin,
    ZonesSection,
};

/// Exact counts from the Phase 9.1 scaffold in `content/`. The Phase 10
/// converter replaces that content; these constants are updated with it.
const TBA_AREAS: usize = 1;
const TBA_ROOMS: usize = 3;
const TBA_NPCS: usize = 1;
const TBA_ITEMS: usize = 2;
const TBA_ZONES: usize = 1;
const TBA_CLASSES: usize = 4;

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

#[test]
fn tba_world_instantiates_and_installs_the_content_resource() {
    let app = boot();

    let index = app.world().resource::<RoomIndex>();
    assert_eq!(index.map.len(), TBA_ROOMS, "every tba room instantiates");
    let start = index.start.expect("a start_room is tagged");
    let temple = *index
        .map
        .get(&RoomId::from("tba.temple"))
        .expect("the temple exists");
    assert_eq!(start, temple, "the temple is the start room");

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
        content
            .npc_stats
            .contains_key(&NpcId::from("tba_cityguard")),
        "the cityguard's Diku stats are installed"
    );
    assert_eq!(content.items.len(), TBA_ITEMS);
    assert_eq!(content.classes.len(), TBA_CLASSES);
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
