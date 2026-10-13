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
    assert_eq!(
        first.report.totals.dangling, 2,
        "the unknown `T 9999` line and the `T` command naming room 9999"
    );
    assert_eq!(
        first.report.totals.deferred, 0,
        "zone-reset T and V are no longer deferred"
    );
    assert_eq!(first.report.totals.triggers_converted, 1);

    // Every record is emitted with its own attach_type and raw type letters;
    // the owners that list it are in `attachments`.
    let emitted = first
        .bundle
        .triggers
        .triggers
        .get(&mib_tba::triggers::content::TriggerId::from_vnum(3000))
        .expect("fixture trigger emitted");
    assert!(
        first.bundle.triggers.attachments["item:tba_item_3001"]
            .contains(&mib_tba::triggers::content::TriggerId::from_vnum(3000)),
        "the bag lists the trigger"
    );
    assert_eq!(emitted.attach_type, "mob");
    assert_eq!(emitted.flags, "q");
    assert_eq!(emitted.types, vec!["leave"]);
    assert!(emitted.script.contains("Hello from a DG trigger."));
    assert_eq!(first.report.trigger_coverage.records, 1);
    assert_eq!(
        first.report.trigger_coverage.script_lines,
        emitted
            .script
            .split(['\n', '\r'])
            .filter(|line| !line.is_empty())
            .count()
    );
    assert_eq!(
        first.report.trigger_coverage.by_attach_type.get("mob"),
        Some(&1)
    );
    // `say Hello from a DG trigger.` on a mob is a line for the game's
    // command interpreter, and no word is supported yet.
    let coverage = &first.report.trigger_coverage;
    assert_eq!(coverage.lines_by_class.get("mud_command"), Some(&1));
    assert_eq!(coverage.mud_commands.lines, 1);
    assert_eq!(coverage.mud_commands.unsupported.get("say"), Some(&1));
    assert!(coverage.mud_commands.supported.is_empty());
    assert_eq!(coverage.structural_errors.count, 0);
    assert!(coverage.unknown_object_commands.is_empty());

    // The trigger layer is classified as well as emitted. The fixture trigger
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

/// The coverage counts every emitted record, its script lines (a line ends at
/// `\n` or `\r`, empty lines are dropped, as `parse_trigger` does) and its
/// attach type.
#[test]
fn trigger_coverage_counts_the_emitted_corpus() {
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
    let coverage = &conversion.report.trigger_coverage;
    assert_eq!(
        coverage.records, 1461,
        "coverage spans every emitted record"
    );
    let lines: usize = conversion
        .bundle
        .triggers
        .triggers
        .values()
        .map(|def| {
            def.script
                .split(['\n', '\r'])
                .filter(|line| !line.is_empty())
                .count()
        })
        .sum();
    assert_eq!(coverage.script_lines, lines);
    assert_eq!(
        coverage.by_attach_type,
        std::collections::BTreeMap::from([
            ("mob".to_owned(), 795),
            ("obj".to_owned(), 282),
            ("wld".to_owned(), 384)
        ]),
        "the 795/282/384 split of the triage"
    );
}

fn ids(vnums: &[i32]) -> Vec<mib_tba::triggers::content::TriggerId> {
    vnums
        .iter()
        .map(|vnum| mib_tba::triggers::content::TriggerId::from_vnum(*vnum))
        .collect()
}

#[test]
fn fixture_attachments_follow_the_c_rules() {
    let conversion = convert(&ConvertOptions {
        source_root: fixture_root(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("fixture converts");
    let lists = &conversion.bundle.triggers.attachments;

    // Mob 3001 has a `T 3000` line. Mob 3000 has none, and gets the trigger
    // from the zone's `M` + `T` reset commands; the second identical `T`
    // command finds it already there and adds nothing.
    assert_eq!(lists["npc:tba_npc_3001"], ids(&[3000]));
    assert_eq!(lists["npc:tba_npc_3000"], ids(&[3000]));
    // The bag's `T 3000` names a mob trigger: kept, and reported.
    assert_eq!(lists["item:tba_item_3001"], ids(&[3000]));
    // Room 3000 lists `T 3000`, `T 9999`, `T 3000`: the unknown trigger is
    // skipped (`dg_read_trigger`), the repeat is attached twice.
    assert_eq!(lists["room:tba.3000"], ids(&[3000, 3000]));
    assert_eq!(lists.len(), 4);

    let summary = &conversion.report.trigger_attachments;
    assert_eq!(summary.owners, 4);
    assert_eq!(summary.links, 5);
    assert_eq!(summary.links_by_owner_kind["npc"], 2);
    assert_eq!(summary.links_by_owner_kind["item"], 1);
    assert_eq!(summary.links_by_owner_kind["room"], 2);
    assert_eq!(summary.zone_reset_folded, 1);
    assert_eq!(summary.zone_reset_already_present, 1);
    assert_eq!(summary.unattached_triggers, 0);
    assert_eq!(
        summary.attach_type_mismatches, 3,
        "the bag and both links in room 3000"
    );
    assert_eq!(summary.variable_commands, 1);

    // The folded command and the repeat are itemised, with the zone-file
    // line, `if_flag` and owner; the `T` lines are fixture lines 9 and 10.
    let links = &conversion.report.zones[0].zone_trigger_links;
    let summary_links: Vec<_> = links
        .iter()
        .map(|link| {
            (
                link.command,
                link.owner.as_str(),
                link.trigger,
                link.outcome.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary_links,
        [
            (2, "npc tba_npc_3000", 3000, "folded"),
            (3, "npc tba_npc_3000", 3000, "already_present"),
        ]
    );
    assert!(
        links
            .iter()
            .all(|link| link.zone == 30 && link.if_flag == 0)
    );

    // `T 0 1 ...` follows a mob load, so there is no object to attach to;
    // `T 0 2 ... 9999` names a room that does not exist (`renum_zone_table`
    // disables it).
    let zone = &conversion.report.zones[0];
    assert!(
        zone.unmapped
            .iter()
            .any(|entry| entry.kind == "zone_trigger_no_target")
    );
    assert!(
        zone.dangling
            .iter()
            .any(|entry| entry.kind == "dangling_zone_trigger_room")
    );
    assert!(
        zone.dangling
            .iter()
            .any(|entry| entry.kind == "dangling_trigger_ref")
    );

    // The `V` command is a named drop carrying everything needed to apply it.
    let [variable] = zone.trigger_variables.as_slice() else {
        panic!("one V command: {:?}", zone.trigger_variables);
    };
    assert_eq!(variable.kind, "zone_trigger_variable");
    assert_eq!(variable.zone, 30);
    assert_eq!(variable.attach, "mob");
    assert_eq!(variable.target.as_deref(), Some("npc tba_npc_3000"));
    // `arg2` is the builder's context and `arg3` the room vnum: the zone line
    // is `V 0 0 7 3001 zonearg varval`.
    assert_eq!(variable.context, 7);
    assert_eq!(variable.room, 3001);
    assert_eq!(variable.name, "zonearg");
    assert_eq!(variable.value, "varval");
}

/// Every `.trg` record is emitted, with its own `attach_type` and raw type
/// letters, whether or not the triage calls it essential.
#[test]
fn full_conversion_emits_every_trigger() {
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
    let triggers = &conversion.bundle.triggers.triggers;
    assert_eq!(triggers.len(), 1461);
    assert_eq!(conversion.report.totals.triggers_converted, 1461);
    assert_eq!(conversion.report.trigger_coverage.records, 1461);
    assert_eq!(
        conversion.report.totals.deferred, 0,
        "nothing trigger-related is deferred any more"
    );

    let mut by_attach_type = std::collections::BTreeMap::new();
    for def in triggers.values() {
        *by_attach_type.entry(def.attach_type.as_str()).or_insert(0) += 1;
        assert!(
            !def.flags.is_empty(),
            "trigger {} keeps its letters",
            def.vnum
        );
    }
    assert_eq!(
        by_attach_type,
        std::collections::BTreeMap::from([("mob", 795), ("obj", 282), ("wld", 384)]),
        "the 795/282/384 split of the triage"
    );

    // Trigger 0 is `0 g 100` (mob, Greet) in 0.trg.
    let first = &triggers[&mib_tba::triggers::content::TriggerId::from_vnum(0)];
    assert_eq!(
        (first.attach_type.as_str(), first.flags.as_str()),
        ("mob", "g")
    );
    let ab = triggers
        .values()
        .find(|def| def.flags == "ab")
        .expect("a two-letter record");
    assert_eq!(ab.types.len(), 2);
}

/// Owner lists reproduce the `T` lines of the `.mob`/`.obj`/`.wld` record in
/// file order. The expected lists were read off the source files by hand:
/// `grep -A5 '^#3$' mob/0.mob` and so on.
#[test]
fn attachment_lists_reproduce_the_source_t_line_order() {
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
    let lists = &conversion.bundle.triggers.attachments;

    let expected: &[(&str, &[i32])] = &[
        // mobs (mob/0.mob, mob/1.mob): note the non-ascending orders
        ("npc:tba_npc_3", &[138, 139, 143]),
        ("npc:tba_npc_14", &[3, 1]),
        ("npc:tba_npc_24", &[4, 5, 7, 8]),
        ("npc:tba_npc_25", &[192, 191, 190]),
        ("npc:tba_npc_31", &[173, 72]),
        // objects (obj/0.obj, obj/1.obj)
        ("item:tba_item_15", &[141, 169]),
        ("item:tba_item_16", &[140, 141, 142]),
        ("item:tba_item_17", &[141]),
        ("item:tba_item_89", &[89, 180]),
        // rooms (wld/0.wld, wld/653.wld)
        ("room:tba.94", &[172, 171]),
        ("room:tba.97", &[170, 171, 172]),
        // a trigger listed twice by one room is attached twice
        ("room:tba.65306", &[65302, 65302]),
        // the only zone-reset `T` (345.zon: `T 1 2 34509 34537`), folded in
        ("room:tba.34537", &[34509]),
    ];
    for (owner, vnums) in expected {
        assert_eq!(lists.get(*owner), Some(&ids(vnums)), "owner {owner}");
    }

    // A trigger attached to several owners is in each list.
    let id = mib_tba::triggers::content::TriggerId::from_vnum(141);
    let holders: Vec<&str> = lists
        .iter()
        .filter(|(_, list)| list.contains(&id))
        .map(|(owner, _)| owner.as_str())
        .collect();
    assert_eq!(
        holders,
        ["item:tba_item_15", "item:tba_item_16", "item:tba_item_17"]
    );
    let id = mib_tba::triggers::content::TriggerId::from_vnum(171);
    assert!(lists["room:tba.94"].contains(&id) && lists["room:tba.97"].contains(&id));

    // Pinned totals, cross-checked against a scan of the source: 799 mob, 224
    // object and 681 room `T` lines, plus the one folded reset command.
    let summary = &conversion.report.trigger_attachments;
    assert_eq!(summary.owners, 1295);
    assert_eq!(summary.links, 1705);
    assert_eq!(summary.links_by_owner_kind["npc"], 799);
    assert_eq!(summary.links_by_owner_kind["item"], 224);
    assert_eq!(summary.links_by_owner_kind["room"], 682);
    assert_eq!(summary.zone_reset_folded, 1);
    assert_eq!(summary.zone_reset_already_present, 0);
    assert_eq!(summary.unattached_triggers, 432);
    assert_eq!(summary.unattached.len(), 432);
    assert!(
        summary.unattached.windows(2).all(|pair| pair[0] < pair[1]),
        "unattached vnums are ascending numerically"
    );
    assert_eq!(conversion.report.schema, 3);
    assert_eq!(summary.variable_commands, 0, "no zone file has a V command");
    assert_eq!(summary.attach_type_mismatches, 8);

    // Trigger 180 is a world trigger that object 89 lists (obj/0.obj).
    let zone_0 = &conversion.report.zones[0];
    assert!(zone_0.unmapped.iter().any(|entry| {
        entry.kind == "trigger_attach_type_mismatch"
            && entry.object == "item tba_item_89"
            && entry.name == "trigger 180"
    }));
}

#[test]
fn full_conversion_is_byte_deterministic() {
    let Some(root) = real_tbamud() else {
        eprintln!("skipping: tbaMUD source not checked out (set TBAMUD_SOURCE)");
        return;
    };
    let options = ConvertOptions {
        source_root: root,
        only: None,
        start_room: Some(DEFAULT_START_ROOM),
    };
    let first = convert(&options).expect("first conversion");
    let second = convert(&options).expect("second conversion");
    assert_eq!(
        render(&first.bundle).unwrap(),
        render(&second.bundle).unwrap(),
        "YAML must be byte-identical"
    );
    assert_eq!(
        serde_json::to_string(&first.report).unwrap(),
        serde_json::to_string(&second.report).unwrap(),
        "drop report must be byte-identical"
    );

    // Each owner key is in exactly one trigger shard (the pack loader rejects
    // a repeated key), and the shards together carry every trigger.
    let mut owner_shards = std::collections::BTreeMap::<String, usize>::new();
    let mut trigger_count = 0;
    for (name, yaml) in render(&first.bundle).unwrap() {
        if !name.starts_with("tba.triggers.") {
            continue;
        }
        let body = yaml.split_once('\n').expect("header line").1;
        let shard: TriggersFile = serde_yaml::from_str(body).expect("shard parses");
        trigger_count += shard.triggers.len();
        for owner in shard.attachments.keys() {
            *owner_shards.entry(owner.clone()).or_default() += 1;
        }
    }
    assert_eq!(trigger_count, 1461);
    assert_eq!(owner_shards.len(), 1295);
    assert!(owner_shards.values().all(|count| *count == 1));
}

/// Copy the fixture tree into a scratch directory, replace zone 30's reset
/// file with `zon`, and optionally strip every `T` line from the world files
/// (so only reset commands attach triggers). The caller removes the directory.
fn scratch_world(tag: &str, zon: &str, strip_t_lines: bool) -> PathBuf {
    fn copy(from: &Path, to: &Path, strip: bool) {
        std::fs::create_dir_all(to).expect("mkdir");
        for entry in std::fs::read_dir(from).expect("read_dir") {
            let entry = entry.expect("entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("type").is_dir() {
                copy(&entry.path(), &target, strip);
            } else if strip
                && matches!(
                    entry.path().extension().and_then(|e| e.to_str()),
                    Some("mob" | "obj" | "wld")
                )
            {
                let text = std::fs::read_to_string(entry.path()).expect("read");
                let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with("T ")).collect();
                std::fs::write(&target, kept.join("\n") + "\n").expect("write");
            } else {
                std::fs::copy(entry.path(), &target).expect("copy");
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("tbaport-scratch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy(&fixture_root(), &dir, strip_t_lines);
    std::fs::write(dir.join("lib/world/zon/30.zon"), zon).expect("write zon");
    dir
}

const ZONE_HEAD: &str = "#30\ntbaport fixture~\nFixture Zone~\n3000 3002 15 2 d 0 0 0 1 33\n";

fn convert_zone_30(tag: &str, commands: &str, strip_t_lines: bool) -> tbaport::Conversion {
    let root = scratch_world(tag, &format!("{ZONE_HEAD}{commands}S\n$\n"), strip_t_lines);
    let conversion = convert(&ConvertOptions {
        source_root: root.clone(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("scratch world converts");
    let _ = std::fs::remove_dir_all(&root);
    conversion
}

/// `renum_zone_table` turns a command naming a missing vnum into `*`, which
/// `reset_zone` handles with `last_cmd = 0` only: `tmob` still points at the
/// last mob that loaded, so a later `T` attaches to it.
#[test]
fn a_disabled_load_leaves_the_reset_target_in_place() {
    let conversion = convert_zone_30(
        "disabled-load",
        "M 0 3000 1 3001\nM 0 9999 1 3001\nT 0 0 3000 3001\n",
        false,
    );
    let lists = &conversion.bundle.triggers.attachments;
    assert_eq!(lists["npc:tba_npc_3000"], ids(&[3000]));
    assert!(
        !conversion.report.zones[0]
            .dangling
            .iter()
            .any(|entry| entry.kind == "dangling_zone_trigger_target")
    );
}

/// A command with a non-zero `if_flag` is skipped while `last_cmd` is clear,
/// which the disabled command before it left it.
#[test]
fn a_dependent_trigger_after_a_disabled_command_is_skipped() {
    let conversion = convert_zone_30(
        "if-flag-skip",
        "M 0 3000 1 3001\nM 0 9999 1 3001\nT 1 0 3000 3001\n",
        false,
    );
    assert!(
        !conversion
            .bundle
            .triggers
            .attachments
            .contains_key("npc:tba_npc_3000")
    );
    let zone = &conversion.report.zones[0];
    assert!(zone.unmapped.iter().any(|entry| {
        entry.kind == "zone_trigger_skipped" && entry.object == "zone 30 command 2 (T)"
    }));
    assert!(zone.zone_trigger_links.is_empty());
}

/// A reset `T` whose room is missing is disabled at boot, so the trigger is
/// attached nowhere.
#[test]
fn a_disabled_reset_trigger_is_unattached() {
    let conversion = convert_zone_30("unattached", "M 0 3000 1 3001\nT 0 0 3000 9999\n", true);
    assert!(conversion.bundle.triggers.attachments.is_empty());
    assert_eq!(
        conversion.report.trigger_attachments.unattached,
        [3000],
        "the trigger is reported unattached"
    );
}

/// A reset `T` naming an existing room folds into that owner's list.
#[test]
fn a_reset_trigger_folds_into_its_owner() {
    let conversion = convert_zone_30("folded", "M 0 3000 1 3001\nT 0 0 3000 3001\n", true);
    assert_eq!(
        conversion.bundle.triggers.attachments["npc:tba_npc_3000"],
        ids(&[3000])
    );
}

/// A zone may span more than 100 vnums; the owner list goes to its zone's
/// shard, not to the shard of `vnum / 100`.
#[test]
fn owner_lists_shard_by_the_owners_zone_file() {
    let root = scratch_world("shard-zone", "", false);
    let wld = root.join("lib/world/wld/30.wld");
    let text = std::fs::read_to_string(&wld).expect("read wld");
    // Room 3150 (was 3002) gains a `T` line, as the last record of the file.
    let text = text
        .replace("#3002\n", "#3150\n")
        .replace("S\n$", "S\nT 3000\n$");
    std::fs::write(&wld, text).expect("write wld");
    std::fs::write(
        root.join("lib/world/zon/30.zon"),
        "#30\ntbaport fixture~\nFixture Zone~\n3000 3199 15 2 d 0 0 0 1 33\nS\n$\n",
    )
    .expect("write zon");
    let conversion = convert(&ConvertOptions {
        source_root: root.clone(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("scratch world converts");
    let _ = std::fs::remove_dir_all(&root);

    let files = render(&conversion.bundle).expect("render");
    let shard_of = |owner: &str| -> Vec<&str> {
        files
            .iter()
            .filter(|(name, yaml)| {
                name.starts_with("tba.triggers.")
                    && yaml.lines().any(|line| line == format!("  {owner}:"))
            })
            .map(|(name, _)| name.as_str())
            .collect()
    };
    // Banding room 3150 by `vnum / 100` would put its list in
    // `tba.triggers.31.yaml`, a shard for a zone that does not exist.
    assert_eq!(shard_of("room:tba.3150"), ["tba.triggers.30.yaml"]);
    assert_eq!(shard_of("room:tba.3000"), ["tba.triggers.30.yaml"]);
    assert!(!files.iter().any(|(name, _)| name == "tba.triggers.31.yaml"));
}

/// `V` reads like `sscanf("%79s %79[^\f\n\r\t\v]")`: the value keeps trailing
/// blanks and both fields stop at 79 bytes.
#[test]
fn variable_command_strings_follow_the_sscanf_fields() {
    let long = "x".repeat(100);
    let commands =
        format!("M 0 3000 1 3001\nV 0 0 5 3001 name value   \nV 0 0 5 3001 other {long}\n");
    let conversion = convert_zone_30("v-strings", &commands, false);
    let variables = &conversion.report.zones[0].trigger_variables;
    let value_of = |name: &str| {
        variables
            .iter()
            .find(|variable| variable.name == name)
            .map(|variable| variable.value.clone())
    };
    assert_eq!(value_of("name").as_deref(), Some("value   "));
    assert_eq!(value_of("other"), Some("x".repeat(79)));
}

/// The owner label of the `T` commands folded in a scratch conversion.
fn folded_owners(conversion: &tbaport::Conversion) -> Vec<String> {
    conversion.report.zones[0]
        .zone_trigger_links
        .iter()
        .map(|link| link.owner.clone())
        .collect()
}

/// `renum_zone_table` leaves an `O` whose room is `NOWHERE` alone; with
/// `CIRCLE_UNSIGNED_INDEX 1` that is 65535, and `reset_zone` still loads the
/// object and makes it `tobj`.
#[test]
fn an_o_command_at_nowhere_still_loads_and_takes_the_trigger() {
    let conversion = convert_zone_30("o-nowhere", "O 0 3000 1 65535\nT 1 1 3000 3001\n", true);
    assert_eq!(folded_owners(&conversion), ["item tba_item_3000"]);
    assert_eq!(
        conversion.bundle.triggers.attachments["item:tba_item_3000"],
        ids(&[3000])
    );
}

/// A `-1` room is an `int` that `real_room` cannot find, so the `O` is
/// disabled at boot and `tobj` stays on the earlier object.
#[test]
fn an_o_command_at_minus_one_is_disabled() {
    let conversion = convert_zone_30(
        "o-minus-one",
        "O 0 3000 1 3001\nO 0 3001 1 -1\nT 0 1 3000 3001\n",
        true,
    );
    assert_eq!(folded_owners(&conversion), ["item tba_item_3000"]);
    assert!(
        !conversion
            .bundle
            .triggers
            .attachments
            .contains_key("item:tba_item_3001")
    );
}

/// `G` and `E` with no mob loaded earlier are rewritten to `*` before they
/// touch `tobj`; an `E` with a wear position past `NUM_WEARS` also leaves it.
#[test]
fn g_and_e_that_cannot_load_leave_the_target_alone() {
    let before_any_mob = convert_zone_30(
        "g-no-mob",
        "O 0 3000 1 3001\nG 0 3001 1 -1\nT 0 1 3000 3001\n",
        true,
    );
    assert_eq!(folded_owners(&before_any_mob), ["item tba_item_3000"]);
    let bad_position = convert_zone_30(
        "e-bad-pos",
        "M 0 3000 1 3001\nO 0 3000 1 3001\nE 0 3001 1 18\nT 0 1 3000 3001\n",
        true,
    );
    assert_eq!(folded_owners(&bad_position), ["item tba_item_3000"]);
    let good_position = convert_zone_30(
        "e-good-pos",
        "M 0 3000 1 3001\nO 0 3000 1 3001\nE 0 3001 1 17\nT 0 1 3000 3001\n",
        true,
    );
    assert_eq!(folded_owners(&good_position), ["item tba_item_3001"]);
}

/// A reset `T` naming a trigger that does not exist is disabled at boot.
#[test]
fn a_zone_trigger_naming_a_missing_trigger_is_dangling() {
    let conversion = convert_zone_30("zone-t-missing", "M 0 3000 1 3001\nT 0 0 9999 3001\n", true);
    let zone = &conversion.report.zones[0];
    assert!(zone.dangling.iter().any(|entry| {
        entry.kind == "dangling_zone_trigger" && entry.from == "zone 30 command 1 (T)"
    }));
    assert!(conversion.bundle.triggers.attachments.is_empty());
}

/// A `V` carries a `fate` telling a command that never runs from one that
/// runs without a target.
#[test]
fn variable_drops_say_whether_the_command_runs() {
    let conversion = convert_zone_30(
        "v-fates",
        "M 0 9999 1 3001\nV 1 0 4 3001 skipped a\nM 0 3000 1 3001\nV 0 0 4 9999 disabled b\n\
         V 0 3 4 3001 untargeted c\nV 0 0 4 3001 runs d\n",
        false,
    );
    let fate = |name: &str| {
        let drop = conversion.report.zones[0]
            .trigger_variables
            .iter()
            .find(|variable| variable.name == name)
            .expect("variable reported");
        (drop.fate.clone(), drop.target.clone())
    };
    assert_eq!(fate("skipped"), ("skipped".to_owned(), None));
    assert_eq!(fate("disabled"), ("disabled".to_owned(), None));
    assert_eq!(fate("untargeted"), ("runs".to_owned(), None));
    assert_eq!(
        fate("runs"),
        ("runs".to_owned(), Some("npc tba_npc_3000".to_owned()))
    );
}

/// A first command with a non-zero `if_flag` is skipped because `last_cmd`
/// starts clear, not because a command was disabled.
#[test]
fn the_skipped_message_names_the_last_cmd_gate() {
    let conversion = convert_zone_30("first-skipped", "T 1 2 3000 3001\n", true);
    let entry = conversion.report.zones[0]
        .unmapped
        .iter()
        .find(|entry| entry.kind == "zone_trigger_skipped")
        .expect("skipped command reported");
    assert!(
        entry.detail.contains("last_cmd is clear"),
        "{}",
        entry.detail
    );
}

/// Each skipped `T` line is its own report entry, so two lines naming the
/// same missing trigger count twice (`dg_read_trigger` logs each).
#[test]
fn repeated_dangling_t_lines_are_counted_separately() {
    let root = scratch_world("dangling-twice", "", false);
    let wld = root.join("lib/world/wld/30.wld");
    let text = std::fs::read_to_string(&wld).expect("read wld");
    assert_eq!(text.matches("T 9999\n").count(), 1);
    std::fs::write(&wld, text.replace("T 9999\n", "T 9999\nT 9999\n")).expect("write wld");
    std::fs::write(
        root.join("lib/world/zon/30.zon"),
        format!("{ZONE_HEAD}S\n$\n"),
    )
    .expect("write zon");
    let conversion = convert(&ConvertOptions {
        source_root: root.clone(),
        only: Some(30),
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("scratch world converts");
    let _ = std::fs::remove_dir_all(&root);
    let lines: Vec<&str> = conversion.report.zones[0]
        .dangling
        .iter()
        .filter(|entry| entry.kind == "dangling_trigger_ref")
        .map(|entry| entry.from.as_str())
        .collect();
    assert_eq!(lines, ["room tba.3000 T line 2", "room tba.3000 T line 3"]);
}

/// The corpus totals of `dg_vm::analyze` reach the report. The expected
/// numbers were counted from the `.trg` bodies with a separate line scan.
#[test]
fn trigger_coverage_reports_the_analysis_totals() {
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
    let coverage = &conversion.report.trigger_coverage;
    // A line scan of the emitted scripts: the first word of each non-empty
    // line (a line ends at `\n` or `\r`).
    let mut scanned = std::collections::BTreeMap::<String, usize>::new();
    let mut lines = 0usize;
    for def in conversion.bundle.triggers.triggers.values() {
        for line in def.script.split(['\n', '\r']).filter(|l| !l.is_empty()) {
            lines += 1;
            let word = if line.trim_start().starts_with('*') {
                "*"
            } else {
                line.split_whitespace().next().unwrap_or("")
            };
            *scanned.entry(word.to_ascii_lowercase()).or_default() += 1;
        }
    }
    assert_eq!(coverage.script_lines, lines);
    assert_eq!(
        coverage.lines_by_class.values().sum::<usize>(),
        coverage.script_lines
    );
    assert_eq!(coverage.lines_by_class["comment"], scanned["*"]);
    assert_eq!(coverage.controls["if"], scanned["if"]);
    assert_eq!(coverage.statements["wait"], scanned["wait"]);
    assert_eq!(coverage.statements["set"], scanned["set"]);
    assert_eq!(coverage.statements["eval"], scanned["eval"]);
    assert_eq!(coverage.statements["dg_cast"], scanned["dg_cast"]);
    assert_eq!(coverage.statements["dg_affect"], scanned["dg_affect"]);

    let errors = &coverage.structural_errors;
    assert_eq!(
        errors.by_vnum.keys().copied().collect::<Vec<_>>(),
        vec![
            1292, 1324, 1338, 1385, 1939, 1959, 2737, 4601, 8315, 13005, 21101, 34302, 55513
        ]
    );
    assert_eq!(errors.scripts, 13);
    assert_eq!(
        errors.count,
        errors.by_vnum.values().map(Vec::len).sum::<usize>()
    );
    assert_eq!(
        errors.by_vnum[&1338],
        vec!["line 1: 'if' without 'end' (error 2)"]
    );

    // Every unknown command names the scripts that use it, sorted.
    let unknown: usize = coverage
        .unknown_object_commands
        .values()
        .chain(coverage.unknown_world_commands.values())
        .map(|u| u.count)
        .sum();
    assert_eq!(unknown, coverage.lines_by_class["unknown_command"]);
    for word in coverage
        .unknown_object_commands
        .values()
        .chain(coverage.unknown_world_commands.values())
    {
        assert!(!word.vnums.is_empty());
        assert!(word.vnums.windows(2).all(|pair| pair[0] < pair[1]));
    }

    // The game's word list is not wired in: every mob mud command is
    // unsupported.
    let mud = &coverage.mud_commands;
    assert_eq!(mud.lines, coverage.lines_by_class["mud_command"]);
    assert!(mud.supported.is_empty());
    assert_eq!(mud.unsupported.values().sum::<usize>(), mud.lines);

    assert_eq!(
        coverage.dynamic_commands,
        coverage.lines_by_class["dynamic_command"]
    );
    let fields = &coverage.field_references;
    assert!(fields.text_functions.contains_key("strlen"));
    assert!(fields.character.contains_key("name"));
    assert!(fields.object.contains_key("shortdesc"));
    assert!(fields.room.contains_key("vnum"));
    assert!(fields.unknown_or_global.contains_key("tba_trial_vnum"));
    assert!(!coverage.unknown_variable_heads.contains_key("actor"));

    let summary = conversion.report.summary();
    for heading in [
        "### Lines by class",
        "### Structural errors",
        "### Unknown object commands",
        "### Unknown world commands",
        "### Mob commands for the game",
        "### Substitution fields",
    ] {
        assert!(summary.contains(heading), "{heading}");
    }
    assert!(summary.contains("| 1338 | line 1: 'if' without 'end' (error 2) |"));
}
