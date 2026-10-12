//! Issue #15 Phase 3 integration tests: variables, `%load%`/`%purge%`, and the
//! `stdlib.quests` bridge, driven from the **real** `content/tba.triggers.*`
//! and `content/stdlib.quests.yaml` records.
//!
//! Two flows:
//!
//! - **Tutorial Quest 1317** (`tba_trig_9/10/11`, zone 0): the greet and the
//!   "yes" speech run through the real event systems; the completion record runs
//!   through the real interpreter, persists
//!   `solved_tutorial_quest_zone_0` in `TbaTriggerVars`, and emits a
//!   `QuestEvent::FlagSet` that advances a `Flag` quest step.
//! - **Kill the Mice!** (`tba_quest_100`, zone 1): three native kills become
//!   `QuestEvent::Killed`, the stdlib `advance_quests` marks the quest ready,
//!   and `handle_turnin` emits the reward.
//!
//! Tests may unwrap/expect — a panic here is a test failure.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use mib_core::prelude::*;
use mib_protocol::command::ExitKey;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use mib_tba::triggers::ast::parse_script;
use mib_tba::triggers::content::{TriggerDef, TriggerId, TriggersFile};
use mib_tba::triggers::context::{ActorView, EntityView, TriggerContext};
use mib_tba::triggers::effects::{
    PendingFlag, PendingTriggerOps, PendingVarWrite, TbaTriggerVars, trigger_effects_system,
};
use mib_tba::triggers::eval::run;
use mib_tba::triggers::events::{TriggerLastRoom, trigger_command_system, trigger_greet_system};
use mib_tba::triggers::index::TbaTriggers;
use mib_tba::triggers::quests::trigger_quest_bridge_system;
use smol_str::SmolStr;

fn content_path(name: &str) -> String {
    format!("{}/../content/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn load_triggers(zone: u32) -> TriggersFile {
    let body = std::fs::read_to_string(content_path(&format!("tba.triggers.{zone}.yaml")))
        .expect("trigger shard");
    serde_yaml::from_str(&body).expect("triggers parse")
}

fn load_quests() -> QuestsFile {
    let body = std::fs::read_to_string(content_path("stdlib.quests.yaml")).expect("quest file");
    serde_yaml::from_str(&body).expect("quests parse")
}

fn run_update(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn run_fixed(app: &mut App) {
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

fn spawn_room(app: &mut App) -> Entity {
    app.world_mut().spawn_empty().id()
}

fn spawn_player(app: &mut App, room: Entity, name: &str) -> Entity {
    app.world_mut()
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
        .id()
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

fn command(app: &mut App, session: Entity, line: &str) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
            session,
            session_id: 1,
            input: PlayerInput::RawLine(line.to_owned()),
        });
}

/// Attach a real (unattached) zone-0 trigger to a local fixture NPC.
fn fixture(def: &TriggerDef) -> TriggerDef {
    let mut def = def.clone();
    def.attach = "npc:tba_npc_1317".to_owned();
    def
}

#[test]
fn tutorial_1317_persists_the_flag_and_advances_a_quest_step() {
    let file = load_triggers(0);
    let greet = file
        .triggers
        .get(&TriggerId::from_vnum(9))
        .map(fixture)
        .expect("tba_trig_9");
    let accept = file
        .triggers
        .get(&TriggerId::from_vnum(10))
        .map(fixture)
        .expect("tba_trig_10");
    let completion = file
        .triggers
        .get(&TriggerId::from_vnum(11))
        .cloned()
        .expect("tba_trig_11");

    let mut app = App::new();
    app.init_resource::<TbaRng>();
    app.init_resource::<TriggerLastRoom>();
    app.init_resource::<PendingTriggerOps>();
    app.add_message::<CommandReceived>();
    app.add_message::<OutputEvent>();
    app.add_message::<QuestEvent>();
    app.add_message::<NpcDefeated>();

    let mut defs = BTreeMap::new();
    for def in [greet, accept] {
        defs.insert(TriggerId::from_vnum(def.vnum), def);
    }
    app.insert_resource(TbaTriggers::build(defs));
    app.add_systems(
        Update,
        (trigger_command_system, trigger_effects_system).chain(),
    );
    app.add_systems(
        FixedUpdate,
        (trigger_greet_system, trigger_effects_system).chain(),
    );

    // The player walks south into the questmaster's room.
    let previous = spawn_room(&mut app);
    let room = spawn_room(&mut app);
    app.world_mut().entity_mut(previous).insert(Exits {
        map: [(ExitKey::South, room)].into_iter().collect(),
        hidden: BTreeSet::new(),
    });
    let actor = spawn_player(&mut app, previous, "Aria");
    let _npc = spawn_npc(&mut app, room, "the questmaster", "tba_npc_1317");

    // First sight records the room without greeting.
    run_fixed(&mut app);
    assert!(lines(&mut app).is_empty(), "no greet on first sight");

    app.world_mut().entity_mut(actor).insert(Location { room });
    run_fixed(&mut app);
    let greeted = lines(&mut app).join("\n");
    assert!(
        greeted.contains("Hello, Aria. Could you find me the magic eight ball?"),
        "the greet fired with substituted text: {greeted}"
    );

    // The player says "yes"; the speech trigger answers.
    command(&mut app, actor, "say yes");
    run_update(&mut app);
    let accepted = lines(&mut app).join("\n");
    assert!(
        accepted.contains("Perfect, Aria. I'll make this easy."),
        "the accept speech fired: {accepted}"
    );
    assert!(
        app.world().get::<TbaTriggerVars>(actor).is_none(),
        "the objective is not satisfied yet"
    );

    // The receive record runs the real script against an object-47 context.
    let program = parse_script(&completion.script);
    assert!(program.is_valid(), "{:?}", program.error);
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
        "the objective emits a quest flag: {:?}",
        outcome.flags
    );
    assert!(
        outcome.spawns.is_empty(),
        "the completion record loads nothing: {:?}",
        outcome.spawns
    );

    // Queue the persisted write and the flag, then drain them through the
    // real exclusive effect system.
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

    // A Flag-step quest bridges the interpreter's flag into stdlib progress.
    let flag_quest = QuestDef {
        name: "Tutorial 1317".to_owned(),
        description: String::new(),
        giver: NpcId::from("tba_npc_1317"),
        steps: vec![QuestStepDef::Flag {
            flag: SmolStr::new("solved_tutorial_quest_zone_0"),
        }],
        reward: RewardSpec {
            xp: 50,
            items: Vec::new(),
        },
        origin: QuestOrigin::Authored,
    };
    let quest_id = QuestId::from("tba_quest_tutorial_1317");
    let mut map = BTreeMap::new();
    map.insert(quest_id.clone(), flag_quest.clone());
    app.insert_resource(QuestIndex { map });
    app.world_mut().entity_mut(actor).insert(PlayerQuestLog {
        quests: [(quest_id.clone(), QuestProgress::fresh(&flag_quest))]
            .into_iter()
            .collect(),
    });
    app.add_systems(Update, advance_quests.after(trigger_effects_system));

    run_update(&mut app);

    let vars = app
        .world()
        .get::<TbaTriggerVars>(actor)
        .expect("TbaTriggerVars persisted");
    assert_eq!(
        vars.remote
            .get("solved_tutorial_quest_zone_0")
            .map(String::as_str),
        Some("1"),
        "the remote variable is persisted"
    );
    let log = app.world().get::<PlayerQuestLog>(actor).expect("quest log");
    assert!(
        log.quests
            .get(&quest_id)
            .is_some_and(|progress| progress.ready),
        "the flag advanced the quest step to ready"
    );
}

#[test]
fn kill_the_mice_progresses_and_rewards() {
    let quests = load_quests();
    let mice = quests
        .quests
        .get(&QuestId::from("tba_quest_100"))
        .expect("tba_quest_100")
        .clone();
    assert_eq!(mice.name, "Kill the Mice!");
    let quest_id = QuestId::from("tba_quest_100");
    let mut index = BTreeMap::new();
    index.insert(quest_id.clone(), mice.clone());

    let mut app = App::new();
    app.add_message::<NpcDefeated>();
    app.add_message::<QuestEvent>();
    app.add_message::<OutputEvent>();
    app.add_message::<RewardAwarded>();
    app.insert_resource(QuestIndex { map: index });
    app.init_resource::<PendingTurnin>();
    app.add_systems(
        Update,
        (trigger_quest_bridge_system, advance_quests).chain(),
    );
    app.add_systems(Update, turnin_system);

    let room = spawn_room(&mut app);
    let actor = spawn_player(&mut app, room, "Aria");
    let _giver = spawn_npc(&mut app, room, "the questmaster", "tba_npc_179");
    app.world_mut().entity_mut(actor).insert(PlayerQuestLog {
        quests: [(quest_id.clone(), QuestProgress::fresh(&mice))]
            .into_iter()
            .collect(),
    });

    for _ in 0..3 {
        let victim = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<Messages<NpcDefeated>>()
            .write(NpcDefeated {
                victim,
                killer: actor,
                npc: NpcId::from("tba_npc_194"),
                experience: 10,
                gold: 0,
                alignment: 0,
            });
    }
    run_update(&mut app);

    let log = app.world().get::<PlayerQuestLog>(actor).expect("quest log");
    let progress = log.quests.get(&quest_id).expect("active quest");
    assert_eq!(progress.steps.len(), 1);
    assert_eq!(progress.steps[0].current, 3, "three mice credited");
    assert!(progress.ready, "the quest is ready to turn in");

    // Turn in the ready quest in front of the giver.
    app.world_mut().insert_resource(PendingTurnin {
        session: Some(actor),
    });
    run_update(&mut app);

    let rewards: Vec<RewardAwarded> = app
        .world_mut()
        .resource_mut::<Messages<RewardAwarded>>()
        .drain()
        .collect();
    assert_eq!(rewards.len(), 1, "the turn-in emitted a reward");
    assert_eq!(rewards[0].player, actor);
    assert_eq!(rewards[0].spec.xp, mice.reward.xp);
    let log = app.world().get::<PlayerQuestLog>(actor).expect("quest log");
    assert!(
        !log.quests.contains_key(&quest_id),
        "the completed quest left the log"
    );
}

/// The pending turn-in request the test drives.
#[derive(Resource, Default)]
struct PendingTurnin {
    session: Option<Entity>,
}

/// A thin test system that calls the real stdlib turn-in handler.
#[allow(clippy::too_many_arguments)]
fn turnin_system(
    mut commands: Commands,
    mut pending: ResMut<PendingTurnin>,
    index: Res<QuestIndex>,
    players: Query<(&Player, &Location)>,
    npc_keys: Query<(Entity, &NpcKey, &Location)>,
    logs: Query<&PlayerQuestLog>,
    mut output: MessageWriter<OutputEvent>,
    mut rewards: MessageWriter<RewardAwarded>,
) {
    let Some(session) = pending.session.take() else {
        return;
    };
    handle_turnin(
        session,
        "tba_quest_100",
        &index,
        &players,
        &npc_keys,
        &logs,
        &mut commands,
        &mut output,
        &mut rewards,
    );
}
