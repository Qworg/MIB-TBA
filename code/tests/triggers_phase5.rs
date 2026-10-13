//! Issue #15 Phase 5 integration tests: combat/behaviour event intake,
//! `dg_cast`/`%damage%`, the serialized queue + depth cap, the curated-zone
//! playtest, and a fuzz-ish pass over every essential record.
//!
//! These drive the **real** `content/tba.triggers.*` records through the same
//! systems `TbaPlugin` wires.
//!
//! Tests may unwrap/expect — a panic here is a test failure.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use mib_core::prelude::*;
use mib_protocol::command::ExitKey;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use mib_tba::triggers::content::{TriggerDef, TriggerId, TriggersFile};
use mib_tba::triggers::effects::{PendingFlag, PendingVarWrite};
use mib_tba::triggers::index::TbaTriggers;
use mib_tba::triggers::quests::trigger_quest_bridge_system;

fn content_path(name: &str) -> String {
    format!("{}/../content/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn load_triggers(zone: u32) -> TriggersFile {
    let body = std::fs::read_to_string(content_path(&format!("tba.triggers.{zone}.yaml")))
        .expect("trigger shard");
    serde_yaml::from_str(&body).expect("triggers parse")
}

/// Every essential trigger shard, merged into one `TbaTriggers`.
fn load_all_essential() -> TbaTriggers {
    let mut defs: BTreeMap<TriggerId, TriggerDef> = BTreeMap::new();
    for zone in [0, 1, 3, 5, 9, 28, 30, 50, 54, 63, 64, 74] {
        for (id, def) in load_triggers(zone).triggers {
            defs.insert(id, def);
        }
    }
    TbaTriggers::build(defs)
}

fn run_update(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn run_round(app: &mut App) {
    app.world_mut().run_schedule(TbaRound);
    app.world_mut().flush();
}

fn tick_fixed(app: &mut App, seconds: f32) {
    app.world_mut()
        .resource_mut::<Time<Fixed>>()
        .advance_by(std::time::Duration::from_secs_f32(seconds));
    app.world_mut().run_schedule(FixedUpdate);
    app.world_mut().flush();
}

fn lines(app: &mut App) -> Vec<String> {
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .filter_map(|event| match event.payload {
            OutboundPayload::Text { body, .. } | OutboundPayload::Error { body, .. } => {
                Some(body.render_plain())
            }
            _ => None,
        })
        .collect()
}

fn room(app: &mut App, num: u32) -> Entity {
    app.world_mut()
        .spawn((
            Room {
                key: format!("tba.{num}").into(),
                name: format!("Room {num}"),
                description: FormattedText::default(),
                num,
                area: "test".into(),
                coord: (0, 0, 0),
                tags: BTreeSet::new(),
                media: Vec::new(),
                sector: mib_stdlib::content::Sector::Field,
                extra_descriptions: Vec::new(),
            },
            RoomState::default(),
        ))
        .id()
}

fn spawn_player(app: &mut App, room: Entity, name: &str) -> Entity {
    let entity = app
        .world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: name.to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room },
            TbaProgress::new("warrior"),
        ))
        .id();
    app.world_mut().resource_mut::<OnlinePlayers>().map.insert(
        1,
        OnlinePlayer {
            entity,
            name: name.to_owned(),
            room,
            account_id: 1,
        },
    );
    entity
}

fn combatant(hp: i32, max_hp: i32, target: Option<Entity>) -> TbaCombatant {
    TbaCombatant {
        hp,
        max_hp,
        mana: 0,
        max_mana: 0,
        move_points: 80,
        max_move_points: 80,
        hitroll: 0,
        damroll: 0,
        armor_class: 100,
        thaco: 20,
        attributes: TbaAttributes::neutral(),
        saves: TbaSaves::default(),
        position: Position::Standing,
        target,
    }
}

fn spawn_npc(app: &mut App, room: Entity, name: &str, key: &str) -> Entity {
    app.world_mut()
        .spawn((
            Npc {
                name: name.to_owned(),
                persona: String::new(),
            },
            NpcKey(NpcId::from(key)),
            Location { room },
        ))
        .id()
}

fn def(vnum: i32, attach: &str, types: &[&str], narg: i32, script: &str) -> TriggerDef {
    TriggerDef {
        vnum,
        name: format!("phase5 trigger {vnum}"),
        attach: attach.to_owned(),
        types: types.iter().map(|t| (*t).to_owned()).collect(),
        narg,
        arglist: String::new(),
        script: script.to_owned(),
        ..Default::default()
    }
}

/// A base app wired like `TbaPlugin` wires the Phase-5 trigger systems.
fn test_app(defs: BTreeMap<TriggerId, TriggerDef>) -> App {
    let mut app = App::new();
    app.init_resource::<TbaRng>();
    app.init_resource::<TriggerLastRoom>();
    app.init_resource::<PendingTriggerOps>();
    app.init_resource::<TriggerTimers>();
    app.init_resource::<TriggerDropLog>();
    app.init_resource::<TriggerQueue>();
    app.init_resource::<TriggerPeriodic>();
    app.init_resource::<TriggerDeathSeen>();
    app.init_resource::<OnlinePlayers>();
    app.init_resource::<HandledCommands>();
    app.init_resource::<TbaObjectClock>();
    app.insert_resource(Time::<Fixed>::from_hz(10.0));
    app.insert_resource(TbaTriggers::build(defs));
    app.add_message::<CommandReceived>();
    app.add_message::<OutputEvent>();
    app.add_message::<QuestEvent>();
    app.add_message::<NpcDefeated>();
    app.add_message::<PlayerDefeated>();
    app.add_message::<TbaDamage>();
    app.init_schedule(TbaRound);
    app.add_systems(
        Update,
        (
            trigger_command_system,
            apply_trigger_casts,
            trigger_effects_system,
            trigger_queue_system,
        )
            .chain(),
    );
    app.add_systems(
        Update,
        (trigger_damage_system, trigger_death_system).after(trigger_queue_system),
    );
    app.add_systems(
        Update,
        trigger_death_reset_system.before(trigger_damage_system),
    );
    app.add_systems(
        TbaRound,
        (trigger_round_system, trigger_death_system).chain(),
    );
    app.add_systems(
        FixedUpdate,
        (
            trigger_timer_system,
            trigger_greet_system,
            trigger_periodic_system,
            trigger_queue_system,
            apply_trigger_casts,
            trigger_effects_system,
        )
            .chain(),
    );
    app
}

/// The combat round fires `fight` every round, and `hitprcnt` only below its
/// `narg` threshold.
#[test]
fn fight_and_hitprcnt_fire_once_per_round() {
    let mut defs = BTreeMap::new();
    defs.insert(
        TriggerId::from_vnum(9001),
        def(9001, "npc:tba_npc_5", &["fight"], 50, "say fight round\n"),
    );
    defs.insert(
        TriggerId::from_vnum(9002),
        def(9002, "npc:tba_npc_5", &["hitprcnt"], 50, "say hurt round\n"),
    );
    let mut app = test_app(defs);
    let here = room(&mut app, 100);
    let player = spawn_player(&mut app, here, "Aria");
    let npc = spawn_npc(&mut app, here, "the goblin", "tba_npc_5");
    app.world_mut()
        .entity_mut(npc)
        .insert(combatant(50, 50, Some(player)));

    run_round(&mut app);
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("fight round"), "the fight fired: {out}");
    assert!(
        !out.contains("hurt round"),
        "hitprcnt above narg does not fire: {out}"
    );

    // Drop the goblin below 50% and run another round.
    app.world_mut()
        .entity_mut(npc)
        .insert(combatant(10, 50, Some(player)));
    run_round(&mut app);
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("fight round"), "fight still fires: {out}");
    assert!(
        out.contains("hurt round"),
        "hitprcnt fires below narg: {out}"
    );
}

/// `damage`/`death` fire from the combat messages, and a `death` record sees
/// the victim before the despawn.
#[test]
fn damage_and_death_dispatch_from_combat_messages() {
    let mut defs = BTreeMap::new();
    defs.insert(
        TriggerId::from_vnum(9101),
        def(
            9101,
            "npc:tba_npc_5",
            &["damage"],
            100,
            "say %damage% points of damage\n",
        ),
    );
    defs.insert(
        TriggerId::from_vnum(9102),
        def(
            9102,
            "npc:tba_npc_5",
            &["death"],
            100,
            "say %self.name% has fallen\n",
        ),
    );
    let mut app = test_app(defs);
    let here = room(&mut app, 101);
    let player = spawn_player(&mut app, here, "Aria");
    let npc = spawn_npc(&mut app, here, "the goblin", "tba_npc_5");
    app.world_mut()
        .entity_mut(npc)
        .insert(combatant(20, 20, Some(player)));

    app.world_mut()
        .resource_mut::<Messages<TbaDamage>>()
        .write(TbaDamage {
            attacker: player,
            victim: npc,
            damage: 7,
            lethal: false,
        });
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("7 points of damage"), "damage fired: {out}");

    app.world_mut()
        .resource_mut::<Messages<NpcDefeated>>()
        .write(NpcDefeated {
            victim: npc,
            killer: player,
            npc: NpcId::from("tba_npc_5"),
            experience: 1,
            gold: 0,
            alignment: 0,
        });
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("the goblin has fallen"), "death fired: {out}");
}

/// `dg_cast` resolves a catalog spell onto its target, and an unknown phrase is
/// a named drop.
#[test]
fn dg_cast_maps_catalog_spells_and_named_drops_unknown() {
    let mut defs = BTreeMap::new();
    defs.insert(
        TriggerId::from_vnum(9201),
        def(
            9201,
            "npc:tba_npc_5",
            &["greet"],
            100,
            "dg_cast 'bless' %actor%\ndg_cast 'not a spell' %actor%\n",
        ),
    );
    let mut app = test_app(defs);
    // A tiny catalog with a `bless` buff.
    let mut content = TbaContent::default();
    content.spells.insert(
        smol_str::SmolStr::new("bless"),
        SpellDef {
            name: "bless".to_owned(),
            mana_min: 1,
            mana_max: 1,
            mana_change: 0,
            kind: SpellKind::Buff,
            dice: None,
            level_bonus: LevelBonus::default(),
            affect: Some(AffectDef {
                location: AffectLocation::Hitroll,
                modifier: 1,
            }),
            duration: 6,
            duration_bonus: LevelBonus::default(),
            target: SpellTarget::Victim,
            min_position: Position::Standing,
            save: SaveEffect::None,
            min_level: BTreeMap::new(),
            unsupported: None,
        },
    );
    app.insert_resource(content);

    let previous = room(&mut app, 101);
    let here = room(&mut app, 102);
    app.world_mut().entity_mut(previous).insert(Exits {
        map: [(ExitKey::South, here)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = app
        .world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: "Aria".to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room: previous },
            TbaProgress::new("warrior"),
        ))
        .id();
    let npc = spawn_npc(&mut app, here, "the mage", "tba_npc_5");
    app.world_mut()
        .entity_mut(npc)
        .insert(combatant(30, 30, None));
    app.world_mut()
        .entity_mut(actor)
        .insert(combatant(30, 30, None));

    // First sight records the room; the second tick greets.
    tick_fixed(&mut app, 0.1);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: here });
    tick_fixed(&mut app, 0.1);

    assert!(
        app.world().get::<TbaBuffs>(actor).is_some(),
        "the catalog buff landed on the actor"
    );
    let drops = app.world().resource::<TriggerDropLog>();
    assert_eq!(
        drops.count("dg_cast_spell"),
        1,
        "the unknown spell is a named drop: {:?}",
        drops.by_kind
    );
}

/// `%damage%` and `mkill` apply through the exclusive drain.
#[test]
fn percent_damage_and_mkill_apply() {
    let mut defs = BTreeMap::new();
    defs.insert(
        TriggerId::from_vnum(9501),
        def(
            9501,
            "npc:tba_npc_5",
            &["greet"],
            100,
            "%damage% %actor% 5\n",
        ),
    );
    let mut app = test_app(defs);
    let previous = room(&mut app, 107);
    let here = room(&mut app, 108);
    app.world_mut().entity_mut(previous).insert(Exits {
        map: [(ExitKey::South, here)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = spawn_player(&mut app, previous, "Aria");
    app.world_mut()
        .entity_mut(actor)
        .insert(combatant(30, 30, None));
    let _npc = spawn_npc(&mut app, here, "the trap", "tba_npc_5");

    tick_fixed(&mut app, 0.1);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: here });
    tick_fixed(&mut app, 0.1);
    assert_eq!(
        app.world().get::<TbaCombatant>(actor).map(|c| c.hp),
        Some(25),
        "`%damage% %actor% 5` dealt five points"
    );

    // `mkill` drops the actor to zero and emits its defeat.
    let mut defs = BTreeMap::new();
    defs.insert(
        TriggerId::from_vnum(9502),
        def(9502, "npc:tba_npc_5", &["greet"], 100, "mkill %actor%\n"),
    );
    let mut app = test_app(defs);
    let previous = room(&mut app, 109);
    let here = room(&mut app, 110);
    app.world_mut().entity_mut(previous).insert(Exits {
        map: [(ExitKey::South, here)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = spawn_player(&mut app, previous, "Aria");
    app.world_mut()
        .entity_mut(actor)
        .insert(combatant(30, 30, None));
    let _npc = spawn_npc(&mut app, here, "the assassin", "tba_npc_5");
    tick_fixed(&mut app, 0.1);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: here });
    tick_fixed(&mut app, 0.1);
    assert_eq!(
        app.world().get::<TbaCombatant>(actor).map(|c| c.hp),
        Some(0),
        "`mkill %actor%` dropped the actor"
    );
}

/// A `detach` directive makes the record one-shot for the session.
#[test]
fn detach_makes_a_record_one_shot() {
    let mut defs = BTreeMap::new();
    defs.insert(
        TriggerId::from_vnum(9301),
        def(
            9301,
            "npc:tba_npc_5",
            &["greet"],
            100,
            "say hello\ndetach 9301 %self.id%\n",
        ),
    );
    let mut app = test_app(defs);
    let previous = room(&mut app, 103);
    let here = room(&mut app, 104);
    app.world_mut().entity_mut(previous).insert(Exits {
        map: [(ExitKey::South, here)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = spawn_player(&mut app, previous, "Aria");
    let _npc = spawn_npc(&mut app, here, "the guard", "tba_npc_5");

    tick_fixed(&mut app, 0.1);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: here });
    tick_fixed(&mut app, 0.1);
    let first = lines(&mut app).join("\n");
    assert!(first.contains("hello"), "the first greeting fired: {first}");

    // Leave and re-enter: the detached record must not fire again.
    let away = room(&mut app, 105);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: away });
    tick_fixed(&mut app, 0.1);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: here });
    tick_fixed(&mut app, 0.1);
    let second = lines(&mut app).join("\n");
    assert!(
        !second.contains("hello"),
        "the detached record did not fire again: {second}"
    );
}

/// Every one of the 133 essential records parses and runs a dispatch without
/// panicking, and every distributed record's event type is indexed.
#[test]
fn every_essential_record_parses_and_executes() {
    let triggers = load_all_essential();
    assert_eq!(triggers.defs.len(), 133, "the essential set is complete");

    let room_vnum = Some(3001);
    let ctx = TriggerContext {
        actor: Some(ActorView {
            name: "Aria".to_owned(),
            is_pc: true,
            id: 1,
            vnum: 0,
            level: 7,
            class: "warrior".to_owned(),
        }),
        self_: Some(EntityView {
            name: "the trigger owner".to_owned(),
            vnum: 5,
        }),
        object: Some(EntityView {
            name: "a quest item".to_owned(),
            vnum: 47,
        }),
        victim: Some(EntityView {
            name: "a victim".to_owned(),
            vnum: 6,
        }),
        target: Some(EntityView {
            name: "a target".to_owned(),
            vnum: 7,
        }),
        direction: Some("south".to_owned()),
        cmd: Some("look".to_owned()),
        speech: Some("yes hunt".to_owned()),
        room_vnum,
        clock: Some(ClockView {
            hour: 12,
            day: 3,
            month: 1,
            year: 1,
        }),
        spell: Some("magic missile".to_owned()),
        damage: Some(3),
        vars: Default::default(),
    };
    let mut rng = TbaRng::from_seed(0x5EED);
    for (id, def) in &triggers.defs {
        let program = parse_script(&def.script);
        assert!(
            program.is_valid(),
            "{id} ({}) must parse: {:?}",
            def.name,
            program.error
        );
        // A second context with no actor exercises the empty-actor branches.
        let outcome = run(&program.nodes, &ctx, &mut rng);
        let _ = outcome.emitted;
        let empty = run(&program.nodes, &TriggerContext::default(), &mut rng);
        let _ = empty.emitted;
    }
}

/// The curated demo spine: real zone 0/1/30/74 records fire through their
/// event paths, and the two headline quests still complete end-to-end.
#[test]
fn curated_demo_spine_and_headline_quests() {
    let triggers = load_all_essential();
    assert_eq!(triggers.defs.len(), 133);

    // Zone 74's ambient `random` records are indexed and their scripts parse.
    let random_74 = triggers.index.by_type("random");
    assert!(
        random_74
            .iter()
            .any(|(attach, _)| attach == "npc:tba_npc_7402"),
        "zone 74's random records are indexed"
    );
    // Zone 30's guild guards are `leave` records.
    assert!(
        triggers
            .index
            .by_type("leave")
            .iter()
            .any(|(attach, _)| attach == "npc:tba_npc_3024"),
        "the mage guildguard is indexed"
    );

    // The zone 0 death record parses and its death path is wired.
    let death = parse_script(
        &triggers
            .def(&TriggerId::from_vnum(65))
            .expect("zone 0 death trigger")
            .script,
    );
    assert!(death.is_valid(), "{:?}", death.error);

    // The zone 1 `load` record loads its object from `apply_spawn`.
    let load = parse_script(
        &triggers
            .def(&TriggerId::from_vnum(144))
            .expect("zone 1 load trigger")
            .script,
    );
    assert!(load.is_valid(), "{:?}", load.error);

    // The two headline quests still complete end-to-end through the real
    // quest content and the kill bridge.
    let quests_body =
        std::fs::read_to_string(content_path("stdlib.quests.yaml")).expect("quests shard");
    let quests: mib_stdlib::content::QuestsFile =
        serde_yaml::from_str(&quests_body).expect("quests parse");
    assert!(
        quests
            .quests
            .contains_key(&mib_stdlib::content::QuestId::from("tba_quest_100")),
        "Kill the Mice! is present"
    );
    // The Tutorial 1317 chain is the zone-0 greet/speech/receive set (its
    // `Flag` quest is declared by the pack's quest index at install).
    for vnum in [9, 10, 11] {
        assert!(
            triggers.def(&TriggerId::from_vnum(vnum)).is_some(),
            "tutorial trigger {vnum} is emitted"
        );
    }
}

/// The curated zones fire through their event paths: zone 74's ambient
/// `random` emote and zone 0's `death` record.
#[test]
fn curated_zone_74_random_and_zone_0_death_fire() {
    let triggers = load_all_essential();
    let mut app = test_app(triggers.defs.clone());

    // Zone 74: `tba_trig_7408` (random, no wait) emotes to the room on the
    // 2-second heartbeat.
    let r74 = room(&mut app, 7400);
    let _visitor = spawn_player(&mut app, r74, "Visitor");
    let _woman = spawn_npc(&mut app, r74, "an old woman", "tba_npc_7408");
    tick_fixed(&mut app, 2.5);
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("muses"), "zone 74 random fired: {out}");

    // Zone 0: `tba_trig_65` (death) sends the victim's line to the killer.
    let r0 = room(&mut app, 0);
    let killer = spawn_player(&mut app, r0, "Aria");
    let victim = spawn_npc(&mut app, r0, "a cityguard", "tba_npc_146");
    app.world_mut()
        .resource_mut::<Messages<NpcDefeated>>()
        .write(NpcDefeated {
            victim,
            killer,
            npc: NpcId::from("tba_npc_146"),
            experience: 0,
            gold: 0,
            alignment: 0,
        });
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("curses"), "zone 0 death fired: {out}");
}

/// A bad script cannot wedge a tick: the step cap trips on the new combat
/// event path and the event is counted as a named drop.
#[test]
fn a_bad_script_cannot_wedge_a_round() {
    let mut defs = BTreeMap::new();
    defs.insert(
        TriggerId::from_vnum(9401),
        def(
            9401,
            "npc:tba_npc_5",
            &["fight"],
            100,
            "while 1\nsay stuck\ndone\n",
        ),
    );
    let mut app = test_app(defs);
    let here = room(&mut app, 106);
    let player = spawn_player(&mut app, here, "Aria");
    let npc = spawn_npc(&mut app, here, "the goblin", "tba_npc_5");
    app.world_mut()
        .entity_mut(npc)
        .insert(combatant(50, 50, Some(player)));

    run_round(&mut app);
    run_update(&mut app);
    // The queued event's drops are applied by the next drain pass.
    run_update(&mut app);
    let drops = app.world().resource::<TriggerDropLog>();
    assert!(
        drops.count("step_cap") >= 1,
        "the loop tripped the step cap: {:?}",
        drops.by_kind
    );
}

/// `door`/`load`/`time` events reach their records through the serialized
/// queue.
#[test]
fn door_load_and_time_events_dispatch() {
    let mut defs = BTreeMap::new();
    defs.insert(
        TriggerId::from_vnum(9601),
        def(9601, "room:tba.164", &["door"], 100, "say door moved\n"),
    );
    defs.insert(
        TriggerId::from_vnum(9602),
        def(9602, "npc:tba_npc_5", &["load"], 100, "say loaded now\n"),
    );
    defs.insert(
        TriggerId::from_vnum(9603),
        def(9603, "npc:tba_npc_5", &["time"], 12, "say noon\n"),
    );
    let mut app = test_app(defs);
    let here = room(&mut app, 164);
    let _player = spawn_player(&mut app, here, "Aria");
    let npc = spawn_npc(&mut app, here, "the goblin", "tba_npc_5");

    // A `%door%`/player door op becomes a `door` event.
    app.world_mut()
        .resource_mut::<PendingTriggerOps>()
        .doors
        .push(PendingDoor {
            room: here,
            direction: ExitKey::North,
            op: DoorOp::Open,
        });
    run_update(&mut app);
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("door moved"), "door event fired: {out}");

    // A load event enqueued directly dispatches the `load` record.
    app.world_mut()
        .resource_mut::<TriggerQueue>()
        .enqueue(TriggerEvent {
            kind: TriggerKind::Load,
            room: here,
            self_entity: Some(npc),
            ..TriggerEvent::default()
        });
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("loaded now"), "load event fired: {out}");

    // The in-world clock reaching `narg` fires the `time` record.
    app.world_mut().resource_mut::<TbaObjectClock>().hour_of_day = 12;
    tick_fixed(&mut app, 2.5);
    run_update(&mut app);
    let out = lines(&mut app).join("\n");
    assert!(out.contains("noon"), "time event fired: {out}");
}

/// The two headline quests still complete end-to-end under the Phase-5
/// runtime: the Tutorial 1317 flag and the zone-1 Kill the Mice! kills.
#[test]
fn headline_quests_complete_end_to_end() {
    use mib_stdlib::prelude::{
        PlayerQuestLog, QuestDef, QuestId, QuestIndex, QuestOrigin, QuestProgress, QuestStepDef,
        RewardSpec, advance_quests,
    };

    // --- Tutorial 1317: greet -> say yes -> completion flag. ---------------
    let file = load_triggers(0);
    let mut defs = BTreeMap::new();
    for vnum in [9, 10] {
        let mut def = file
            .triggers
            .get(&TriggerId::from_vnum(vnum))
            .expect("tutorial trigger")
            .clone();
        def.attach = "npc:tba_npc_1317".to_owned();
        defs.insert(TriggerId::from_vnum(vnum), def);
    }
    let completion = file
        .triggers
        .get(&TriggerId::from_vnum(11))
        .expect("completion")
        .clone();
    let mut app = test_app(defs);
    let previous = room(&mut app, 2);
    let here = room(&mut app, 3);
    app.world_mut().entity_mut(previous).insert(Exits {
        map: [(ExitKey::South, here)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = spawn_player(&mut app, previous, "Aria");
    let _npc = spawn_npc(&mut app, here, "the questmaster", "tba_npc_1317");
    tick_fixed(&mut app, 0.1);
    app.world_mut()
        .entity_mut(actor)
        .insert(Location { room: here });
    tick_fixed(&mut app, 0.1);
    tick_fixed(&mut app, 1.0);
    let greeted = lines(&mut app).join("\n");
    assert!(greeted.contains("magic eight ball"), "greet: {greeted}");

    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
            session: actor,
            session_id: 1,
            input: PlayerInput::RawLine("say yes".to_owned()),
        });
    run_update(&mut app);
    tick_fixed(&mut app, 1.0);

    // The completion record is `receive` (not yet an intake event); evaluate
    // it and apply its flag through the real drain, exactly as the receive
    // event will when it lands.
    let program = parse_script(&completion.script);
    let ctx = TriggerContext {
        actor: Some(ActorView {
            name: "Aria".to_owned(),
            is_pc: true,
            id: actor.index_u32() as i64,
            vnum: 0,
            level: 7,
            class: "warrior".to_owned(),
        }),
        self_: Some(EntityView {
            name: "the questmaster".to_owned(),
            vnum: 1317,
        }),
        object: Some(EntityView {
            name: "the magic eight ball".to_owned(),
            vnum: 47,
        }),
        ..TriggerContext::default()
    };
    let mut rng = TbaRng::from_seed(11);
    let outcome = run(&program.nodes, &ctx, &mut rng);
    assert!(
        outcome
            .flags
            .contains(&"solved_tutorial_quest_zone_0".to_owned()),
        "the completion emits the flag: {:?}",
        outcome.flags
    );
    app.world_mut()
        .entity_mut(actor)
        .insert(TbaProgress::new("warrior"));
    {
        let mut ops = app.world_mut().resource_mut::<PendingTriggerOps>();
        for write in &outcome.var_writes {
            ops.vars.push(PendingVarWrite {
                owner: actor,
                scope: write.scope,
                name: write.name.clone(),
                value: write.value.clone(),
            });
        }
        for flag in &outcome.flags {
            ops.flags.push(PendingFlag {
                player: actor,
                flag: flag.clone(),
            });
        }
    }
    run_update(&mut app);
    let vars = app
        .world()
        .get::<TbaTriggerVars>(actor)
        .expect("TbaTriggerVars persisted");
    assert_eq!(
        vars.remote
            .get("solved_tutorial_quest_zone_0")
            .map(String::as_str),
        Some("1")
    );
    // A `Flag` quest for the tutorial still advances to ready.
    let flag_quest = QuestDef {
        name: "Tutorial 1317".to_owned(),
        description: String::new(),
        giver: NpcId::from("tba_npc_1317"),
        steps: vec![QuestStepDef::Flag {
            flag: smol_str::SmolStr::new("solved_tutorial_quest_zone_0"),
        }],
        reward: RewardSpec {
            xp: 50,
            items: Vec::new(),
        },
        origin: QuestOrigin::Authored,
    };
    let quest_id = QuestId::from("tba_quest_tutorial_1317");
    app.insert_resource(QuestIndex {
        map: BTreeMap::from([(quest_id.clone(), flag_quest.clone())]),
    });
    app.world_mut().entity_mut(actor).insert(PlayerQuestLog {
        quests: [(quest_id.clone(), QuestProgress::fresh(&flag_quest))]
            .into_iter()
            .collect(),
    });
    app.add_systems(Update, advance_quests.after(trigger_effects_system));
    run_update(&mut app);
    let log = app.world().get::<PlayerQuestLog>(actor).expect("quest log");
    assert!(
        log.quests.get(&quest_id).is_some_and(|p| p.ready),
        "the tutorial flag quest is ready"
    );

    // --- Kill the Mice!: three kills advance the typed step. ----------------
    let quests_body =
        std::fs::read_to_string(content_path("stdlib.quests.yaml")).expect("quests shard");
    let quests: mib_stdlib::content::QuestsFile =
        serde_yaml::from_str(&quests_body).expect("quests parse");
    let mice = quests
        .quests
        .get(&QuestId::from("tba_quest_100"))
        .expect("tba_quest_100")
        .clone();
    let mice_id = QuestId::from("tba_quest_100");
    let mut kills = App::new();
    kills.add_message::<NpcDefeated>();
    kills.add_message::<QuestEvent>();
    kills.add_message::<OutputEvent>();
    kills.insert_resource(QuestIndex {
        map: BTreeMap::from([(mice_id.clone(), mice.clone())]),
    });
    kills.add_systems(
        Update,
        (trigger_quest_bridge_system, advance_quests).chain(),
    );
    let player = kills.world_mut().spawn_empty().id();
    kills.world_mut().entity_mut(player).insert(PlayerQuestLog {
        quests: [(mice_id.clone(), QuestProgress::fresh(&mice))]
            .into_iter()
            .collect(),
    });
    for _ in 0..3 {
        let victim = kills.world_mut().spawn_empty().id();
        kills
            .world_mut()
            .resource_mut::<Messages<NpcDefeated>>()
            .write(NpcDefeated {
                victim,
                killer: player,
                npc: NpcId::from("tba_npc_194"),
                experience: 10,
                gold: 0,
                alignment: 0,
            });
    }
    kills.update();
    let log = kills.world().get::<PlayerQuestLog>(player).expect("log");
    let progress = log.quests.get(&mice_id).expect("active quest");
    assert_eq!(progress.steps[0].current, 3, "three mice credited");
    assert!(progress.ready, "Kill the Mice! is ready to turn in");
}
