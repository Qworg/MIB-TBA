//! Parser tests against committed fixture excerpts, one per record type.
//!
//! Each fixture is a hand-sized excerpt of the real tbaMUD grammar; the
//! assertions pin the fields the converter maps.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use tbaport::source::deferred::scan_triggers;
use tbaport::source::obj::parse_obj;
use tbaport::source::quest::parse_quests;
use tbaport::source::shop::parse_shops;
use tbaport::source::social::parse_socials;
use tbaport::source::text::{load_message_topics, load_text_topics};
use tbaport::source::tokens::{Source, ascii_flags, bit};
use tbaport::source::zone::parse_zone;
use tbaport::source::{parse_mobs, parse_objs, parse_rooms};

fn world() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tbamud/lib/world")
}

fn source(subdir: &str, name: &str) -> Source {
    Source::from_path(&world().join(subdir).join(name)).expect("fixture opens")
}

/// The fixture tbaMUD root (the parent of `lib/world`).
fn tbamud() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tbamud")
}

/// A path under the fixture `lib/` tree (`text`, `misc`).
fn lib_path(relative: &str) -> PathBuf {
    tbamud().join("lib").join(relative)
}

#[test]
fn rooms_parse_exits_doors_and_extra_descriptions() {
    let mut src = source("wld", "30.wld");
    let rooms = parse_rooms(&mut src).expect("rooms parse");
    assert_eq!(rooms.len(), 3);

    let first = &rooms[0];
    assert_eq!(first.vnum, 3000);
    assert_eq!(first.name, "A Small Room");
    assert!(first.description.contains("A tiny test room"));
    assert_eq!(first.sector, 0);
    // flags[0] == 156 = NOMOB | INDOORS | PEACEFUL | NOMAGIC
    assert!(bit(first.flags[0], 2));
    assert!(bit(first.flags[0], 3));
    assert_eq!(first.exits.len(), 1);
    let exit = &first.exits[0];
    assert_eq!(exit.dir, 0);
    assert_eq!(exit.exit_info_code, 1);
    assert_eq!(exit.key, 3000);
    assert_eq!(exit.to_room, 3001);
    assert_eq!(first.extra_descriptions.len(), 1);
    assert_eq!(first.extra_descriptions[0].keywords, "sign wall");
}

#[test]
fn description_only_exit_is_kept_as_raw() {
    let mut src = source("wld", "30.wld");
    let rooms = parse_rooms(&mut src).expect("rooms parse");
    let dead_end = rooms.iter().find(|room| room.vnum == 3002).unwrap();
    assert_eq!(dead_end.exits.len(), 1);
    assert_eq!(dead_end.exits[0].to_room, -1);
}

#[test]
fn mobs_parse_simple_and_enhanced_forms() {
    let mut src = source("mob", "30.mob");
    let mobs = parse_mobs(&mut src).expect("mobs parse");
    assert_eq!(mobs.len(), 2);

    let guard = &mobs[0];
    assert_eq!(guard.vnum, 3000);
    assert_eq!(guard.short_descr, "the test guard");
    assert_eq!(ascii_flags("26635"), guard.mob_flags[0]);
    assert_eq!(guard.alignment, 350);
    assert_eq!(guard.level, 5);
    assert_eq!(guard.hitroll_field, 3);
    assert_eq!(guard.ac_field, 2);
    assert_eq!(guard.hp.to_string(), "3d8+20");
    assert_eq!(guard.damage.to_string(), "2d6+3");
    assert_eq!(guard.gold, 20);
    assert_eq!(guard.experience, 100);
    assert_eq!(guard.default_position, 8);
    assert!(!guard.enhanced);

    let mage = &mobs[1];
    assert!(mage.enhanced);
    assert_eq!(
        mage.especs,
        vec!["Str: 18".to_owned(), "Int: 25".to_owned()]
    );
}

#[test]
fn objects_parse_affects_and_extra_descriptions() {
    let mut src = source("obj", "30.obj");
    let _header = src.next_nonblank();
    let sword = parse_obj(&mut src, 3000).expect("first object parses");
    assert_eq!(sword.vnum, 3000);
    assert_eq!(sword.obj_type, 5);
    assert_eq!(ascii_flags("a"), sword.wear_flags[0]);
    assert_eq!(sword.values, [0, 3, 6, 4]);
    assert_eq!(sword.weight, 4);
    assert_eq!(sword.cost, 50);
    assert_eq!(sword.applies.len(), 2);
    assert_eq!(sword.applies[0].location, 18);
    assert_eq!(sword.applies[0].modifier, 2);
    assert_eq!(sword.extra_descriptions.len(), 1);
    // The next record's header is handed back, unread.
    assert_eq!(src.peek().map(str::trim), Some("#3001"));

    let _header = src.next_nonblank();
    let bag = parse_obj(&mut src, 3001).expect("second object parses");
    assert_eq!(bag.obj_type, 15);
    assert_eq!(bag.values[0], 50);
    // The `$` terminator is handed back, unread.
    assert_eq!(src.peek().map(str::trim), Some("$"));

    // The whole file scans to exactly two objects.
    let mut whole = source("obj", "30.obj");
    assert_eq!(parse_objs(&mut whole).expect("objects parse").len(), 2);
}

#[test]
fn zone_parses_header_flags_and_commands() {
    let mut src = source("zon", "30.zon");
    let zone = parse_zone(&mut src).expect("zone parses");
    assert_eq!(zone.number, 30);
    assert_eq!(zone.name, "Fixture Zone");
    assert_eq!(zone.bot, 3000);
    assert_eq!(zone.top, 3002);
    assert_eq!(zone.lifespan, 15);
    assert_eq!(zone.reset_mode, 2);
    // zbuf1 == "d" -> ZONE_GRID bit 3.
    assert!(bit(zone.zone_flags[0], 3));

    let codes: Vec<char> = zone.commands.iter().map(|c| c.code).collect();
    assert_eq!(
        codes,
        vec!['O', 'M', 'T', 'T', 'T', 'T', 'V', 'G', 'E', 'P', 'D', 'R'],
        "comment lines are skipped and order is preserved"
    );
    let give = &zone.commands[7];
    assert_eq!(give.code, 'G');
    assert_eq!(give.if_flag, 1);
    let equip = &zone.commands[8];
    assert_eq!(equip.arg3, 16, "equipment position is the source WEAR_*");

    // `T <if> <attach> <trigger> <room>`: attach 2 is a world target, and the
    // room is `arg3`.
    let world_trigger = &zone.commands[5];
    assert_eq!(
        (world_trigger.arg1, world_trigger.arg2, world_trigger.arg3),
        (2, 3000, 9999)
    );
    // `V <if> <attach> <arg2> <arg3> <name> <value>`.
    let variable = &zone.commands[6];
    assert_eq!(variable.code, 'V');
    assert_eq!(
        (variable.arg1, variable.arg2, variable.arg3),
        (0, 0, 0),
        "the four numbers are read before the name"
    );
    assert_eq!(variable.sarg1, "zonearg");
    assert_eq!(variable.sarg2, "varval");
}

#[test]
fn triggers_scan_their_records() {
    let mut triggers = source("trg", "30.trg");
    let triggers = scan_triggers(&mut triggers).expect("triggers scan");
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].vnum, 3000);
    assert_eq!(triggers[0].attach, "mob");
    assert_eq!(triggers[0].name, "Fixture Trigger");
    assert_eq!(triggers[0].flags, "q");
    assert_eq!(triggers[0].types, vec!["Leave"], "letter q is bit 16");
    assert_eq!(triggers[0].narg, 100);
    assert_eq!(triggers[0].lines, 1);
    assert_eq!(
        triggers[0].bucket,
        tbaport::source::deferred::TriggerBucket::GatekeeperTransport
    );
}

#[test]
fn trigger_type_letters_expand_per_attach_family() {
    use tbaport::source::deferred::{TriggerBucket, classify, expand_types};

    // Same letter, different bit name per attach family.
    assert_eq!(expand_types(0, "cg"), vec!["Command", "Greet"]);
    assert_eq!(
        expand_types(2, "cfg"),
        vec!["Command", "Zone Reset", "Enter"]
    );
    // Object bit 3 (`d`) is `UNUSED1` and is dropped.
    assert!(expand_types(1, "d").is_empty());

    let types = |name: &str| vec![name.to_owned()];
    assert_eq!(
        classify(
            "mob",
            &types("Greet"),
            "Questmaster Greet",
            "if %actor.varexists(quest_x)%"
        ),
        TriggerBucket::QuestObjective
    );
    assert_eq!(
        classify("mob", &types("Speech"), "Mob Speech", "say hi"),
        TriggerBucket::InteractiveResponse
    );
    assert_eq!(
        classify("world", &types("Door"), "Room Door", "open door"),
        TriggerBucket::GatekeeperTransport
    );
    assert_eq!(
        classify("mob", &types("Random"), "Mob Random", "%echo% nothing"),
        TriggerBucket::AmbienceFlavour
    );
    assert_eq!(
        classify("mob", &types("Random"), "Mob Random", "kill %actor%"),
        TriggerBucket::CombatBehaviour
    );
}

#[test]
fn quests_parse_their_objective_and_rewards() {
    let mut quests = source("qst", "30.qst");
    let quests = parse_quests(&mut quests).expect("quests parse");
    assert_eq!(quests.len(), 1);
    let quest = &quests[0];
    assert_eq!(quest.vnum, 100);
    assert_eq!(quest.name, "Fixture Quest");
    assert_eq!(quest.quest_type, 3, "AQ_MOB_KILL");
    assert_eq!(quest.questmaster, 3000);
    assert_eq!(quest.target, 3001);
    assert_eq!(quest.values[6], 3, "QST_QUANTITY");
    assert_eq!(quest.gold_reward, 10);
    assert_eq!(quest.obj_reward, -1);
}

#[test]
fn socials_parse_the_new_format_sets() {
    let mut src = Source::from_path(&lib_path("misc/socials.new")).expect("socials fixture opens");
    let socials = parse_socials(&mut src).expect("socials parse");
    assert_eq!(socials.len(), 2);
    let wave = &socials[0];
    assert_eq!(wave.command, "wave");
    assert_eq!(wave.sort_as, "wave");
    assert_eq!(wave.hide, 0);
    assert_eq!(wave.min_char_position, 5);
    assert_eq!(
        wave.messages[0].as_deref(),
        Some("You wave."),
        "char_no_arg"
    );
    assert_eq!(
        wave.messages[12].as_deref(),
        Some("$n waves $p around."),
        "others_obj_found"
    );
    let ponder = &socials[1];
    assert_eq!(ponder.command, "ponder");
    assert_eq!(ponder.hide, 1);
    assert_eq!(ponder.min_char_position, 1);
    assert_eq!(ponder.messages[8], None, "`#` is Circle's NULL");
}

#[test]
fn text_files_become_help_topics_with_colour_stripped() {
    let text_dir = lib_path("text");
    let topics = load_text_topics(&text_dir).expect("text topics");
    assert_eq!(topics.len(), 2, "only the committed motd/news fixtures");
    let motd = topics
        .iter()
        .find(|(id, _)| id == "motd")
        .expect("motd topic");
    assert!(motd.1.body.contains("Welcome to"));
    let news = topics
        .iter()
        .find(|(id, _)| id == "news")
        .expect("news topic");
    assert!(
        news.1.body.contains("http://tbamud.com"),
        "`@C`/`@n` are resolved: {}",
        news.1.body
    );
    assert!(!news.1.body.contains('@'), "no colour codes survive");
}

#[test]
fn combat_messages_become_one_topic_per_attack_type() {
    let messages = load_message_topics(&lib_path("misc/messages")).expect("message topics");
    assert_eq!(messages.len(), 2);
    let (id, topic) = messages
        .iter()
        .find(|(id, _)| id == "combat-message-5")
        .expect("burning hands topic");
    assert_eq!(id, "combat-message-5");
    assert!(topic.title.contains("burning hands"));
    assert!(
        topic
            .body
            .contains("die/attacker: You have burned $N to death!")
    );
    let unused = messages
        .iter()
        .find(|(id, _)| id == "combat-message-202")
        .expect("unused type topic");
    assert!(unused.1.title.contains("!UNUSED!"));
}

#[test]
fn shops_parse_their_trade_records() {
    let mut src = source("shp", "30.shp");
    let shops = parse_shops(&mut src).expect("shops parse");
    assert_eq!(shops.len(), 1);
    let shop = &shops[0];
    assert_eq!(shop.record, 3000);
    assert_eq!(shop.keeper, 3000);
    assert_eq!(shop.produce, vec![3000]);
    assert_eq!(shop.profit_buy, 1.15);
    assert_eq!(shop.profit_sell, 0.15);
    let kinds: Vec<i32> = shop.buy_types.iter().map(|entry| entry.kind).collect();
    assert_eq!(kinds, vec![2, 3, 4, 10]);
    assert_eq!(shop.messages.len(), 7);
    assert_eq!(shop.messages[5], "%s That'll be %d coins, please.");
    assert_eq!(shop.rooms, vec![3001]);
    assert_eq!(
        (shop.open1, shop.close1, shop.open2, shop.close2),
        (0, 28, 0, 0)
    );
}

#[test]
fn trailing_t_lines_are_read_in_source_order() {
    let mut rooms = source("wld", "30.wld");
    let rooms = parse_rooms(&mut rooms).expect("rooms parse");
    assert_eq!(
        rooms[0].trigger_vnums,
        vec![3000, 9999, 3000],
        "a repeated or unknown trigger is kept as written"
    );
    assert!(rooms[1].trigger_vnums.is_empty());

    let mut objs = source("obj", "30.obj");
    let objs = parse_objs(&mut objs).expect("objects parse");
    assert!(objs[0].trigger_vnums.is_empty());
    assert_eq!(objs[1].trigger_vnums, vec![3000]);
}
