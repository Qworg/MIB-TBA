//! End-to-end conversion tests: fixture round-trip, determinism, and a
//! room/NPC/object count assertion for zone 30 (Midgaard) against the real
//! tbaMUD source when it is checked out.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use mib_content::prelude::SmolStr;
use mib_stdlib::content::{
    AreasFile, HelpFile, ItemId, ItemsFile, NpcId, NpcsFile, QuestId, QuestStepDef, QuestsFile,
    RoomId, RoomsFile,
};
use mib_tba::content::{
    ShopsFile, SocialsFile, TbaItemKind, TbaItemsFile, TbaNpcStatsFile, ZonesFile,
};
use mib_tba::triggers::content::TriggersFile;
use tbaport::{ConvertOptions, DEFAULT_START_ROOM, convert, emit::render};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tbamud")
}

/// The sibling tbaMUD checkout, or `None` when it is not present.
fn real_tbamud() -> Option<PathBuf> {
    let candidate = std::env::var("TBAMUD_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/qworg/Development/tbamud"));
    candidate
        .join("lib/world/zon/30.zon")
        .is_file()
        .then_some(candidate)
}

fn count_records(path: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    // A record header is a whole line `#<digits>`. Loose `#`-prefix matching
    // would also count room names/descriptions that begin with `#`.
    text.lines()
        .filter(|line| {
            line.strip_prefix('#')
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        })
        .count()
}

#[test]
fn fixture_bundle_is_schema_exact_and_deterministic() {
    let options = ConvertOptions {
        source_root: fixture_root(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    };

    let first = convert(&options).expect("fixture converts");
    assert_eq!(first.report.totals.rooms, 3);
    assert_eq!(first.report.totals.npcs, 2);
    assert_eq!(first.report.totals.items, 2);
    assert_eq!(first.report.totals.zone_commands, 7);
    assert_eq!(first.report.totals.dangling, 0);
    assert_eq!(
        first.report.totals.deferred, 2,
        "the zone-reset T and V remain deferred; the essential trigger is emitted"
    );
    assert_eq!(first.report.totals.triggers_converted, 1);

    // The fixture trigger is essential (demo zone + gatekeeper), so it lands
    // in `tba.triggers` with its resolved attach and verbatim script.
    let emitted = first
        .bundle
        .triggers
        .triggers
        .get(&mib_tba::triggers::content::TriggerId::from_vnum(3000))
        .expect("fixture trigger emitted");
    assert_eq!(emitted.attach, "npc:tba_npc_3000");
    assert_eq!(emitted.types, vec!["leave"]);
    assert!(emitted.script.contains("Hello from a DG trigger."));
    assert_eq!(first.report.trigger_coverage.records, 1);
    assert_eq!(first.report.trigger_coverage.parsed, 1);

    // The trigger layer is classified, not just deferred. The fixture trigger
    // (`0 q 100`, mob) is a `GatekeeperTransport`.
    let triage = &first.report.trigger_triage;
    assert_eq!(triage.records, 1);
    assert_eq!(triage.by_attach.get("mob"), Some(&1));
    assert_eq!(triage.by_type.get("Leave"), Some(&1));
    assert_eq!(triage.by_bucket.get("gatekeeper_transport"), Some(&1));
    let trigger = &first.report.zones[0].triggers[0];
    assert_eq!(trigger.vnum, 3000);
    assert_eq!(trigger.flags, "q");
    assert_eq!(trigger.types, vec!["Leave"]);

    // Every emitted file parses back into its real type and re-serialises to
    // exactly the bytes that were written (modulo the provenance header).
    // Rooms are sharded one file per zone (TBA-E01); every shard is a real
    // `RoomsFile`.
    let rendered = render(&first.bundle).expect("renders");
    let mut room_shards = 0;
    for (name, yaml) in &rendered {
        let body = yaml
            .split_once('\n')
            .map(|(_, rest)| rest)
            .expect("header line");
        if let Some(zone) = name
            .strip_prefix("stdlib.rooms.")
            .and_then(|rest| rest.strip_suffix(".yaml"))
        {
            assert!(
                zone.parse::<u32>().is_ok(),
                "room shard `{name}` names a zone number"
            );
            roundtrip::<RoomsFile>(body);
            room_shards += 1;
            continue;
        }
        if let Some(zone) = name
            .strip_prefix("tba.triggers.")
            .and_then(|rest| rest.strip_suffix(".yaml"))
        {
            assert!(
                zone.parse::<u32>().is_ok(),
                "trigger shard `{name}` names a zone number"
            );
            roundtrip::<TriggersFile>(body);
            continue;
        }
        match name.as_str() {
            "stdlib.areas.yaml" => roundtrip::<AreasFile>(body),
            "stdlib.npcs.yaml" => roundtrip::<NpcsFile>(body),
            "stdlib.items.yaml" => roundtrip::<ItemsFile>(body),
            "stdlib.quests.yaml" => roundtrip::<QuestsFile>(body),
            "stdlib.help.yaml" => roundtrip::<HelpFile>(body),
            "tba.zones.yaml" => roundtrip::<ZonesFile>(body),
            "tba.npc_stats.yaml" => roundtrip::<TbaNpcStatsFile>(body),
            "tba.items.yaml" => roundtrip::<TbaItemsFile>(body),
            "tba.shops.yaml" => roundtrip::<ShopsFile>(body),
            "tba.socials.yaml" => roundtrip::<SocialsFile>(body),
            other => panic!("unexpected emitted file {other}"),
        }
    }
    assert!(room_shards >= 1, "rooms are sharded by zone");

    // Two runs over the same source are byte-identical, YAML and report.
    let second = convert(&options).expect("fixture converts again");
    let first_yaml = render(&first.bundle).unwrap();
    let second_yaml = render(&second.bundle).unwrap();
    assert_eq!(first_yaml, second_yaml, "YAML must be byte-identical");
    assert_eq!(
        serde_json::to_string(&first.report).unwrap(),
        serde_json::to_string(&second.report).unwrap(),
        "drop report must be byte-identical"
    );
}

#[test]
fn fixture_content_maps_the_expected_fields() {
    let options = ConvertOptions {
        source_root: fixture_root(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    };
    let conversion = convert(&options).expect("fixture converts");

    let guard = conversion
        .bundle
        .npc_stats
        .npc_stats
        .get(&NpcId::from("tba_npc_3000"))
        .expect("guard stats");
    assert_eq!(guard.hp.to_string(), "3d8+20");
    assert_eq!(guard.hitroll, 20 - 3);
    assert_eq!(guard.thaco, 20);
    assert_eq!(guard.armor_class, 20);
    assert_eq!(guard.gold, 20);
    assert_eq!(guard.experience, 100);
    assert_eq!(
        guard.damage,
        Some(mib_core::prelude::Dice::new(2, 6, 3)),
        "the source damage dice are carried"
    );

    let sword = conversion
        .bundle
        .tba_items
        .items
        .get(&ItemId::from("tba_item_3000"))
        .expect("sword def");
    assert_eq!(sword.affects.len(), 2);
    assert_eq!(sword.cost, 50);
    assert_eq!(
        sword.keywords,
        vec!["sword".to_owned(), "long".to_owned()],
        "the source keyword list is preserved"
    );
    // `a` == ITEM_WEAR_TAKE only; TAKE is not a slot.
    assert!(sword.slots.is_empty());

    let bag = conversion
        .bundle
        .items
        .items
        .get(&ItemId::from("tba_item_3001"))
        .expect("bag def");
    assert_eq!(bag.container.as_ref().map(|c| c.capacity), Some(50));

    let zone = conversion.bundle.zones.zones.get(&30).expect("zone 30");
    assert_eq!(zone.name, "Fixture Zone");
    assert_eq!(zone.commands.len(), 7);

    // Issue #16: the fixture quest maps onto a real `QuestDef`.
    let quest = conversion
        .bundle
        .quests
        .quests
        .get(&QuestId::from("tba_quest_100"))
        .expect("fixture quest");
    assert_eq!(quest.name, "Fixture Quest");
    assert_eq!(quest.giver, NpcId::from("tba_npc_3000"));
    assert_eq!(
        quest.reward,
        mib_stdlib::content::RewardSpec::default(),
        "gold is recorded as a drop; -1 obj_reward is NOTHING"
    );
    match quest.steps.as_slice() {
        [QuestStepDef::Kill { npc, count }] => {
            assert_eq!(npc, &NpcId::from("tba_npc_3001"));
            assert_eq!(*count, 3, "QST_QUANTITY");
        }
        other => panic!("expected one kill step, got {other:?}"),
    }

    // Issue #18: the fixture `lib/text` + `lib/misc/messages` become topics.
    let topics = &conversion.bundle.help.topics;
    assert!(topics.contains_key(&SmolStr::new("motd")), "motd topic");
    assert!(topics.contains_key(&SmolStr::new("news")), "news topic");
    assert!(
        topics.contains_key(&SmolStr::new("combat-message-5")),
        "combat messages become topics"
    );

    // Issue #17: the fixture socials parse into `tba.socials`.
    let socials = &conversion.bundle.socials.socials;
    let wave = socials
        .get(&SmolStr::new("wave"))
        .expect("fixture wave social");
    assert_eq!(wave.sort_as, "wave");
    assert_eq!(wave.char_no_arg.as_deref(), Some("You wave."));
    assert_eq!(
        wave.others_obj_found.as_deref(),
        Some("$n waves $p around.")
    );
    let ponder = socials
        .get(&SmolStr::new("ponder"))
        .expect("fixture ponder social");
    assert!(ponder.hide);
    assert_eq!(ponder.char_body_found, None, "`#` maps to None");

    let shop = conversion
        .bundle
        .shops
        .shops
        .get(&NpcId::from("tba_npc_3000"))
        .expect("fixture shop");
    assert_eq!(shop.produce, vec![ItemId::from("tba_item_3000")]);
    assert_eq!(shop.buy_multiplier, 1.15);
    assert_eq!(shop.sell_multiplier, 0.15);
    assert_eq!(
        shop.rooms,
        vec![RoomId::from("tba.3001")],
        "the source room restriction is carried"
    );
    assert_eq!(
        shop.trade_with,
        vec![mib_tba::content::ShopTrade::Evil],
        "trade_with bit 1 is TRADE_NOEVIL"
    );
    assert_eq!(
        shop.hours,
        mib_tba::content::ShopHours::default(),
        "0/28/0/0 is always open"
    );
    assert_eq!(
        shop.buy_kinds,
        vec![
            TbaItemKind::Scroll,
            TbaItemKind::Wand,
            TbaItemKind::Staff,
            TbaItemKind::Potion
        ]
    );
}

/// TBA-E09's gold prototype is a genre rule, not a source object, so the
/// converter must add it to every converted world.
#[test]
fn gold_prototype_is_always_emitted() {
    let conversion = convert(&ConvertOptions {
        source_root: fixture_root(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("fixture converts");

    let id = ItemId::from("tba_gold");
    let item = conversion
        .bundle
        .items
        .items
        .get(&id)
        .expect("gold is in stdlib.items");
    assert_eq!(item.name, "a pile of gold coins");
    assert_eq!(item.value, Some(1));

    let tba = conversion
        .bundle
        .tba_items
        .items
        .get(&id)
        .expect("gold is in tba.items");
    assert_eq!(tba.kind, TbaItemKind::Money);

    // It survives render + reparse like every other item.
    let rendered = render(&conversion.bundle).expect("renders");
    let (_, yaml) = rendered
        .iter()
        .find(|(name, _)| *name == "stdlib.items.yaml")
        .expect("stdlib items file");
    let body = yaml.split_once('\n').map(|(_, rest)| rest).expect("header");
    let parsed: ItemsFile = serde_yaml::from_str(body).expect("reparses");
    assert!(
        parsed.items.contains_key(&id),
        "gold survives the round-trip"
    );
}

#[test]
fn dangling_references_are_dropped_and_reported() {
    // Zone 31's fixture deliberately names a missing door key (item 9999), a
    // missing exit destination (room 9998) and missing reset targets.
    let options = ConvertOptions {
        source_root: fixture_root(),
        only: Some(31),
        start_room: None,
    };
    let conversion = convert(&options).expect("zone 31 converts");

    // The door survives, but its key is gone; the missing destination exit is
    // removed entirely.
    let room = conversion
        .bundle
        .rooms
        .rooms
        .get(&RoomId::from("tba.3100"))
        .expect("test room");
    assert_eq!(room.exits.len(), 1, "the dangling east exit is dropped");
    let north = room.exits.get("north").expect("north exit");
    assert_eq!(north.to, RoomId::from("tba.3101"));
    let door = north.door.as_ref().expect("north is a door");
    assert!(
        door.key.is_none(),
        "a dangling door key must not reach YAML"
    );

    // The two dangling resets are dropped; the valid door reset is kept.
    let zone = conversion.bundle.zones.zones.get(&31).expect("zone 31");
    assert_eq!(zone.commands.len(), 1);

    let kinds: Vec<&str> = conversion.report.zones[0]
        .dangling
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    assert!(kinds.contains(&"dangling_door_key"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"dangling_exit"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"dangling_reset_item"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"dangling_reset_mob"), "kinds: {kinds:?}");

    // No emitted section names any of the missing ids.
    let yaml = render(&conversion.bundle).unwrap();
    for (_, body) in &yaml {
        assert!(!body.contains("tba_item_9999"), "dangling item id leaked");
        assert!(!body.contains("tba_npc_9999"), "dangling npc id leaked");
        assert!(!body.contains("tba.9998"), "dangling room id leaked");
    }
}

#[test]
fn shop_references_are_dropped_and_reported() {
    // Zone 32's fixture has an existing keeper (3200) whose shop names a
    // missing produce item (9999), and a second shop whose keeper (9999) was
    // never converted.
    let conversion = convert(&ConvertOptions {
        source_root: fixture_root(),
        only: Some(32),
        start_room: None,
    })
    .expect("zone 32 converts");

    assert_eq!(
        conversion.bundle.shops.shops.len(),
        1,
        "only the valid-keeper shop survives"
    );
    let shop = conversion
        .bundle
        .shops
        .shops
        .get(&NpcId::from("tba_npc_3200"))
        .expect("keeper 3200");
    assert_eq!(
        shop.produce,
        vec![ItemId::from("tba_item_3200")],
        "the missing produce id is dropped"
    );

    let kinds: Vec<&str> = conversion.report.zones[0]
        .dangling
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    assert!(kinds.contains(&"dangling_shop_produce"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"dangling_shop_keeper"), "kinds: {kinds:?}");

    let yaml = render(&conversion.bundle).unwrap();
    let (_, shops_yaml) = yaml
        .iter()
        .find(|(name, _)| *name == "tba.shops.yaml")
        .expect("shops file");
    assert!(
        !shops_yaml.contains("tba_item_9999"),
        "dangling produce leaked"
    );
    assert!(
        !shops_yaml.contains("tba_npc_9999"),
        "dangling keeper leaked"
    );
}

#[test]
fn fixture_triage_marks_the_essential_set() {
    let options = ConvertOptions {
        source_root: fixture_root(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    };
    let triage = tbaport::build_triage(&options).expect("fixture triage");
    assert_eq!(triage.corpus.records, 1);
    assert_eq!(triage.corpus.zones_with_triggers, 1);
    let record = &triage.records[0];
    assert!(record.essential, "a demo-zone gatekeeper is essential");
    assert_eq!(record.rank, Some(1));
    assert_eq!(record.tier, 1);
    assert!(!record.owners.is_empty(), "the fixture trigger is attached");
}

#[test]
fn zone_30_counts_match_the_real_source() {
    let Some(root) = real_tbamud() else {
        eprintln!("skipping: tbaMUD source not checked out (set TBAMUD_SOURCE)");
        return;
    };
    let options = ConvertOptions {
        source_root: root.clone(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    };
    let conversion = convert(&options).expect("zone 30 converts");

    let world = root.join("lib/world");
    assert_eq!(
        conversion.report.totals.rooms,
        count_records(&world.join("wld/30.wld")),
        "room count must match 30.wld"
    );
    assert_eq!(
        conversion.report.totals.npcs,
        count_records(&world.join("mob/30.mob")),
        "npc count must match 30.mob"
    );
    assert_eq!(
        conversion.report.totals.items,
        count_records(&world.join("obj/30.obj")),
        "item count must match 30.obj"
    );

    // Midgaard's known totals, pinned so a silent parse regression is loud.
    assert_eq!(conversion.report.totals.rooms, 58);
    assert_eq!(conversion.report.totals.npcs, 46);
    assert_eq!(conversion.report.totals.items, 63);

    // The default start room (3001) lives in Midgaard and must be tagged.
    assert_eq!(conversion.report.start_room, Some(DEFAULT_START_ROOM));
    let temple = conversion
        .bundle
        .rooms
        .rooms
        .get(&RoomId::from("tba.3001"))
        .expect("Temple of Midgaard");
    assert!(
        temple
            .tags
            .contains(&mib_stdlib::content::RoomTag::StartRoom)
    );
}

/// `--start-room` defaults to 3001 (Circle's `CONFIG_MORTAL_START`): the room
/// that vnum names is tagged `start_room`, exactly once, so new characters
/// have a spawn point. An absent vnum is a no-op recorded as `None`.
#[test]
fn full_trigger_triage_is_pinned() {
    let Some(root) = real_tbamud() else {
        eprintln!("skipping: tbaMUD source not checked out (set TBAMUD_SOURCE)");
        return;
    };
    let options = ConvertOptions {
        source_root: root,
        only: None,
        start_room: Some(DEFAULT_START_ROOM),
    };
    let triage = tbaport::build_triage(&options).expect("full triage");
    assert_eq!(triage.corpus.records, 1461);
    assert_eq!(triage.corpus.zones_with_triggers, 113);
    assert_eq!(
        triage.corpus.by_attach,
        std::collections::BTreeMap::from([
            ("mob".to_owned(), 795),
            ("object".to_owned(), 282),
            ("world".to_owned(), 384),
        ])
    );
    assert_eq!(
        triage.corpus.by_bucket,
        std::collections::BTreeMap::from([
            ("ambience_flavour".to_owned(), 206),
            ("combat_behaviour".to_owned(), 166),
            ("gatekeeper_transport".to_owned(), 111),
            ("interactive_response".to_owned(), 902),
            ("quest_objective".to_owned(), 76),
        ])
    );
    assert_eq!(triage.essential.count, 133);
    assert_eq!(
        triage.essential.by_bucket,
        std::collections::BTreeMap::from([
            ("gatekeeper_transport".to_owned(), 25),
            ("interactive_response".to_owned(), 86),
            ("quest_objective".to_owned(), 22),
        ])
    );
}

#[test]
fn default_start_room_is_tagged_once() {
    use mib_stdlib::content::RoomTag;

    assert_eq!(DEFAULT_START_ROOM, 3001, "Circle's CONFIG_MORTAL_START");

    // The fixture's zone 30 has a room 3001.
    let conversion = convert(&ConvertOptions {
        source_root: fixture_root(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("fixture converts");
    assert_eq!(conversion.report.start_room, Some(3001));
    assert!(
        conversion
            .bundle
            .rooms
            .rooms
            .get(&RoomId::from("tba.3001"))
            .expect("room 3001")
            .tags
            .contains(&RoomTag::StartRoom)
    );
    assert_eq!(
        conversion
            .bundle
            .rooms
            .rooms
            .values()
            .filter(|room| room.tags.contains(&RoomTag::StartRoom))
            .count(),
        1,
        "exactly one room is tagged"
    );

    // A requested room the converted selection does not contain is a clean
    // no-op, reported as `None` rather than a hard failure.
    let absent = convert(&ConvertOptions {
        source_root: fixture_root(),
        only: Some(31),
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("zone 31 converts despite the absent start room");
    assert_eq!(absent.report.start_room, None);
    assert!(
        absent
            .bundle
            .rooms
            .rooms
            .values()
            .all(|room| !room.tags.contains(&RoomTag::StartRoom)),
        "no room is tagged when the requested start room is absent"
    );
}

fn roundtrip<T>(body: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let parsed: T = serde_yaml::from_str(body).expect("emitted YAML deserialises");
    let again = serde_yaml::to_string(&parsed).expect("re-serialises");
    assert_eq!(body, again, "round-trip must be byte-stable");
}

#[test]
fn every_essential_trigger_parses() {
    let Some(root) = real_tbamud() else {
        eprintln!("skipping: tbaMUD source not checked out (set TBAMUD_SOURCE)");
        return;
    };
    let conversion = convert(&ConvertOptions {
        source_root: root,
        only: None,
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("full conversion");
    let mut failures = Vec::new();
    for (id, def) in &conversion.bundle.triggers.triggers {
        let program = mib_tba::triggers::ast::parse_script(&def.script);
        if let Some(error) = &program.error {
            failures.push(format!("{id} ({}): {error}", def.name));
        }
    }
    assert!(
        failures.is_empty(),
        "every essential trigger must parse: {failures:#?}"
    );
    assert_eq!(
        conversion.report.trigger_coverage.parse_failures, 0,
        "coverage agrees"
    );
}
