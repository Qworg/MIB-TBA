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
    ContentRegistry, CreditsSection, HelpSection, ItemId, Modules, NpcId, QuestId, QuestStepDef,
    RoomId, RoomIndex, StdlibModule, StdlibPlugin,
};
use mib_tba::prelude::{
    SaveEffect, ShopDef, ShopsSection, SocialsSection, SpellKind, SpellTarget, SpellsSection,
    TbaClassesSection, TbaContent, TbaItemsSection, TbaModule, TbaNpcStatsSection, TbaPlugin,
    TbaSocialDef, TriggersSection, ZonesSection,
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
/// Issue #17: every `socials.new` record converts.
const TBA_SOCIALS: usize = 491;
/// Issue #18: the eleven `lib/text` files plus one topic per combat attack type.
const TBA_HELP_TOPICS: usize = 49;
/// Issue #16: the only non-empty `.qst` record in the source world.
const TBA_QUESTS: usize = 1;
/// Issue #15: every DG trigger record emitted into `tba.triggers`.
const TBA_TRIGGERS: usize = 1461;

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
    assert_eq!(
        tba.section::<SocialsSection>()
            .expect("socials")
            .socials
            .len(),
        TBA_SOCIALS
    );
    // Issue #15: the essential DG scripts land in `tba.triggers`, keyed by a
    // stable `tba_trig_<vnum>` id; the owners' ordered lists are `attachments`.
    let triggers = tba.section::<TriggersSection>().expect("triggers");
    assert_eq!(triggers.triggers.len(), TBA_TRIGGERS);
    let guildguard = triggers
        .triggers
        .get(&mib_tba::prelude::TriggerId::from_vnum(3000))
        .expect("zone-30 guild guard trigger");
    assert!(
        triggers.attachments["npc:tba_npc_3024"]
            .contains(&mib_tba::prelude::TriggerId::from_vnum(3000)),
        "the guild guard carries the trigger"
    );
    assert_eq!(guildguard.types, vec!["leave"]);
    assert!(guildguard.script.contains("%direction%"));
    // Issue #17: a spot-check that the new-format sets survive, including
    // Circle's `$`-codes and the `#`-as-None mapping.
    let smile: &TbaSocialDef = tba
        .section::<SocialsSection>()
        .expect("socials")
        .socials
        .get("smile")
        .expect("the smile social");
    assert_eq!(smile.char_no_arg.as_deref(), Some("You smile happily."));
    assert!(
        smile
            .others_found
            .as_deref()
            .is_some_and(|line| line.contains("$N")),
        "substitution codes are preserved"
    );

    // Issue #18: the text files and combat messages are authored topics.
    let help = tba.section::<HelpSection>().expect("help");
    assert_eq!(help.topics.len(), TBA_HELP_TOPICS);
    assert_eq!(
        help.topics.get("motd").map(|topic| topic.title.as_str()),
        Some("Message of the Day")
    );
    // The CircleMUD licence requires the `CIRCLEMUD` entry to be kept intact
    // and shown for `help circlemud`.
    assert!(
        help.topics.contains_key("circlemud"),
        "the CIRCLEMUD help entry is preserved"
    );

    // The CircleMUD/DikuMUD licences require a `credits` command, which the
    // engine aggregates across every loaded pack (MIB #22); this pack supplies
    // its lineage as its own `stdlib.credits` section.
    let credits = tba.section::<CreditsSection>().expect("credits");
    assert!(
        credits
            .credits
            .iter()
            .any(|entry| entry.name == "CircleMUD credits"),
        "the pack credits its upstream lineage: {:?}",
        credits.credits.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    // Issue #16: the single quest maps its objective and giver.
    let quests = tba
        .section::<mib_stdlib::prelude::QuestsSection>()
        .expect("quests");
    assert_eq!(quests.quests.len(), TBA_QUESTS);
    let mice = quests
        .quests
        .get(&QuestId::from("tba_quest_100"))
        .expect("Kill the Mice!");
    assert_eq!(mice.giver, NpcId::from("tba_npc_179"));
    assert!(matches!(mice.steps.as_slice(), [QuestStepDef::Kill { .. }]));
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
    // The weaponsmith's source room restriction and default hours survive
    // the conversion and are carried on `tba.shops`.
    assert_eq!(weaponsmith.rooms, vec![RoomId::from("tba.3011")]);
    assert_eq!(
        weaponsmith.hours,
        mib_tba::prelude::ShopHours::default(),
        "0/28/0/0 is the always-open default"
    );
    assert!(weaponsmith.trade_with.is_empty());

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
    use mib_tba::prelude::ZoneCommandKind;

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
    let kinds = |pred: fn(&ZoneCommandKind) -> bool| {
        zone.commands.iter().filter(|c| pred(&c.command)).count()
    };
    assert_eq!(kinds(|c| matches!(c, ZoneCommandKind::PlaceMob { .. })), 57);
    assert_eq!(kinds(|c| matches!(c, ZoneCommandKind::GiveItem { .. })), 62);
    assert_eq!(
        kinds(|c| matches!(c, ZoneCommandKind::EquipItem { .. })),
        24
    );

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
    assert_eq!(
        guard.damage.expect("the mob's attack dice").to_string(),
        "1d2+1",
        "npc_stats carries the source damage dice (#11/#12)"
    );
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
    assert_eq!((missile.mana_min, missile.mana_max), (10, 25));
    assert_eq!(missile.mana_change, 3, "spell_parser.c:877-878");
    assert_eq!(
        missile.min_level.get("magic_user"),
        Some(&1),
        "class.c:1592 magic-user learns it at level 1"
    );
    assert_eq!(missile.kind, SpellKind::Damage);
    assert_eq!(missile.save, SaveEffect::Half, "magic.c:290 dam /= 2");
    assert_eq!(missile.target, SpellTarget::Victim);
    assert_eq!(missile.dice.expect("damage dice").to_string(), "1d8+1");

    let heal = spells.spells.get("cure_light").expect("cure_light");
    assert_eq!(heal.kind, SpellKind::Heal);
    assert_eq!(heal.mana_min, 10);
    assert_eq!(heal.level_bonus.bonus(7), 1, "+ level / 4, magic.c:808");

    let armor = spells.spells.get("armor").expect("armor");
    assert_eq!(armor.kind, SpellKind::Buff);
    assert!(armor.affect.is_some(), "armor installs APPLY_AC");
    assert_eq!(armor.duration, 24, "magic.c:336 armor af[0].duration");

    let bless = spells.spells.get("bless").expect("bless");
    assert_eq!(bless.kind, SpellKind::Buff);
    assert_eq!(bless.duration, 6, "magic.c:344 bless af[0].duration");

    let chill = spells.spells.get("chill_touch").expect("chill_touch");
    assert_eq!(
        chill.duration, 4,
        "magic.c:327 chill touch's unsaved duration"
    );

    let strength = spells.spells.get("strength").expect("strength");
    assert_eq!(
        strength.duration, 4,
        "magic.c:498 strength floor (level/2 + 4)"
    );
    assert_eq!(strength.duration_bonus.bonus(6), 3, "strength + level/2");

    let poison = spells.spells.get("poison").expect("poison");
    assert_eq!(poison.kind, SpellKind::Debuff);
    assert_eq!(poison.save, SaveEffect::Negate, "magic.c:446");
    assert_eq!(poison.duration_bonus.bonus(9), 9, "poison duration = level");

    let detect = spells.spells.get("detect_invis").expect("detect_invis");
    assert_eq!(detect.kind, SpellKind::Detect);
    assert_eq!(detect.target, SpellTarget::Caster);

    // Issue #7: no `Utility` no-op may stay silent. Every catalog entry is
    // either a modelled kind or a `Utility` carrying an `unsupported` reason.
    let mut implemented = 0;
    let mut flagged = 0;
    for (id, spell) in &spells.spells {
        match spell.kind {
            SpellKind::Utility => {
                assert!(
                    spell
                        .unsupported
                        .as_deref()
                        .is_some_and(|reason| !reason.trim().is_empty()),
                    "utility spell `{id}` must carry an `unsupported` reason"
                );
                flagged += 1;
            }
            _ => {
                assert!(
                    spell.unsupported.is_none(),
                    "modelled spell `{id}` must not claim to be unsupported"
                );
                implemented += 1;
            }
        }
    }
    // #39 is converting the `Utility` set to real kinds; these counts move as
    // each group lands. Seven spells landed in the transport, charm and party
    // groups.
    assert_eq!(flagged, 19, "Circle routines still unimplemented (#39)");
    assert_eq!(
        implemented, 35,
        "modeled kinds: damage/heal/buff/debuff/detect/transport/charm"
    );
    assert_eq!(implemented + flagged, TBA_SPELLS);

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
    assert_eq!(content.socials.len(), TBA_SOCIALS);
    assert_eq!(content.classes.len(), TBA_CLASSES);
    assert_eq!(content.spells.len(), TBA_SPELLS);
    assert_eq!(content.triggers.len(), TBA_TRIGGERS);
    assert!(
        content.trigger_attachments["npc:tba_npc_3024"]
            .contains(&mib_tba::prelude::TriggerId::from_vnum(3000)),
        "the guild guard's trigger list is installed"
    );
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
        "spells:\n  fireball:\n    name: fireball\n    mana_min: 30\n    mana_max: 40\n    mana_change: 2\n    kind: damage\n    target: victim\n",
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

#[test]
fn dangling_trigger_attach_names_the_field_path() {
    // A `tba.triggers` attachment owner that names no emitted NPC is a validation issue
    // at the exact field path, not a silent no-op (issue #15).
    let dir = temp_dir("dangling-trigger");
    std::fs::create_dir_all(dir.join("content")).expect("mkdir content");
    std::fs::write(
        dir.join("pack.toml"),
        "id = \"tba-bad\"\nversion = \"1\"\ntitle = \"bad\"\ndepends_on = [\"stdlib\"]\n",
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("content/tba.triggers.yaml"),
        "triggers:\n  tba_trig_7:\n    vnum: 7\n    name: broken\n    types:\n    - greet\n    script: |\n      say hi\nattachments:\n  npc:tba_npc_9999:\n  - tba_trig_7\n",
    )
    .expect("write triggers");

    let assets = load_packs(&[engine_pack("stdlib"), dir.clone()], &registry()).expect("loads");
    let issues = validate_all(&assets, &registry());
    let issue = issues
        .iter()
        .find(|issue| issue.field.as_deref() == Some("attachments.npc:tba_npc_9999"))
        .unwrap_or_else(|| {
            panic!("expected an attachments.npc:tba_npc_9999 issue, got {issues:?}")
        });
    assert!(
        issue.message.contains("tba_npc_9999"),
        "issue names the bad target: {}",
        issue.message
    );

    let _ = std::fs::remove_dir_all(&dir);
}
