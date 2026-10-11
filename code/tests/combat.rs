//! TBA combat + aggro integration tests (TBA-E05 / E08 / E31).
//!
//! These drive the real systems against a small ECS app rather than the full
//! pack boot: `StdlibPlugin`'s spatial index is exercised through
//! `maintain_room_occupants`, while the TBA systems are wired exactly as
//! [`TbaPlugin`] wires them. The required semantics are: player init, aggro
//! gating (alignment + memory), helper, the `attack`/`flee` commands, round
//! damage/defeat, and Sentinel pursuit.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn warrior() -> TbaClassDef {
    TbaClassDef {
        name: "Warrior".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 10, 0),
        mana_dice: None,
        // Empty tables fall back to Circle's level-1 THAC0 (`20`); this suite
        // exercises combat wiring, not the tables (those are asserted in
        // `progression.rs` and `pack.rs`).
        xp_table: Vec::new(),
        thaco_table: Vec::new(),
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: Vec::new(),
    }
}

fn magic_user() -> TbaClassDef {
    TbaClassDef {
        name: "Magic User".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 4, 0),
        mana_dice: Some(Dice::new(1, 4, 0)),
        xp_table: Vec::new(),
        thaco_table: Vec::new(),
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: Vec::new(),
    }
}

fn stats(behaviour: Vec<NpcBehaviour>) -> TbaNpcStats {
    TbaNpcStats {
        hp: Dice::constant(50),
        level: 1,
        hitroll: 0,
        damroll: 0,
        armor_class: 100,
        thaco: 20,
        alignment: 0,
        gold: 0,
        experience: 0,
        default_position: Position::Standing,
        behaviour,
        damage: None,
    }
}

/// `tba.classes` with both a warrior and a magic user.
fn classes() -> BTreeMap<SmolStr, TbaClassDef> {
    BTreeMap::from([
        (SmolStr::new("warrior"), warrior()),
        (SmolStr::new("magic_user"), magic_user()),
    ])
}

fn content_with(mobs: &[(&str, Vec<NpcBehaviour>)]) -> TbaContent {
    let npc_stats = mobs
        .iter()
        .map(|(id, behaviour)| (NpcId::from(*id), stats(behaviour.clone())))
        .collect();
    TbaContent {
        classes: classes(),
        npc_stats,
        items: BTreeMap::new(),
        zones: BTreeMap::new(),
        spells: BTreeMap::new(),
        ..TbaContent::default()
    }
}

fn content() -> TbaContent {
    content_with(&[])
}

/// A warrior whose Circle XP table levels at 100 / 300 / 600.
fn reward_warrior() -> TbaClassDef {
    TbaClassDef {
        name: "Warrior".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 10, 0),
        mana_dice: None,
        xp_table: vec![0, 1, 100, 300, 600],
        thaco_table: vec![100, 20, 20, 19, 18],
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: Vec::new(),
    }
}

/// Content where `goblin` yields the given reward and `warrior` uses the
/// reward table above.
fn reward_content(experience: u32, gold: u32) -> TbaContent {
    let mut goblin = stats(Vec::new());
    goblin.experience = experience;
    goblin.gold = gold;
    TbaContent {
        classes: BTreeMap::from([(SmolStr::new("warrior"), reward_warrior())]),
        npc_stats: BTreeMap::from([(NpcId::from("goblin"), goblin)]),
        ..TbaContent::default()
    }
}

/// The TBA systems wired as `TbaPlugin` wires them, on a bare app.
fn test_app(content: TbaContent) -> App {
    let mut app = App::new();
    app.insert_resource(content);
    app.insert_resource(TbaRng::from_seed(0xC0FFEE));
    app.init_resource::<RoomOccupants>();
    app.insert_resource(Time::<Fixed>::from_hz(10.0));
    app.add_message::<OutputEvent>();
    app.add_message::<PlayerDefeated>();
    app.add_message::<NpcDefeated>();
    app.add_message::<CommandReceived>();
    app.add_message::<CharacterCreated>();
    app.add_systems(
        Update,
        (
            player_combatant_system,
            handle_combat_commands,
            award_kill_rewards,
            apply_death_penalty,
        )
            .chain(),
    );
    app.add_systems(
        FixedUpdate,
        (
            maintain_room_occupants,
            npc_combatant_system,
            auto_engage_system,
            helper_system,
            pursuit_system,
        )
            .chain(),
    );
    app.init_schedule(TbaRound);
    app.add_systems(TbaRound, resolve_rounds);
    app
}

fn run_game(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn run_fixed(app: &mut App) {
    app.world_mut().run_schedule(FixedUpdate);
    app.world_mut().flush();
}

fn run_round(app: &mut App) {
    app.world_mut().run_schedule(TbaRound);
    app.world_mut().flush();
}

fn spawn_room(world: &mut World) -> Entity {
    world.spawn_empty().id()
}

fn combatant(hp: i32, thaco: i32, damroll: i32) -> TbaCombatant {
    TbaCombatant {
        hp,
        max_hp: hp,
        mana: 0,
        max_mana: 0,
        move_points: 80,
        max_move_points: 80,
        hitroll: 0,
        damroll,
        armor_class: 100,
        thaco,
        attributes: TbaAttributes::neutral(),
        saves: TbaSaves::default(),
        position: Position::Standing,
        target: None,
    }
}

fn spawn_player(world: &mut World, room: Entity, alignment: i32) -> Entity {
    world
        .spawn((
            Player {
                account_id: 1,
                name: "Hero".to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room },
            TbaAlignment(alignment),
            combatant(30, 20, 0),
        ))
        .id()
}

fn spawn_npc(world: &mut World, room: Entity, id: &str) -> Entity {
    world
        .spawn((
            NpcKey(NpcId::from(id)),
            Npc {
                name: id.to_owned(),
                persona: String::new(),
            },
            Location { room },
        ))
        .id()
}

fn combatant_of(app: &App, entity: Entity) -> TbaCombatant {
    app.world()
        .get::<TbaCombatant>(entity)
        .cloned()
        .expect("combatant present")
}

fn target_of(app: &App, entity: Entity) -> Option<Entity> {
    app.world()
        .get::<TbaCombatant>(entity)
        .and_then(|c| c.target)
}

fn position_of(app: &App, entity: Entity) -> Position {
    app.world().get::<TbaCombatant>(entity).unwrap().position
}

fn drain_defeats(app: &mut App) -> Vec<Entity> {
    app.world_mut()
        .resource_mut::<Messages<PlayerDefeated>>()
        .drain()
        .map(|event| event.player)
        .collect()
}

fn send_command(app: &mut App, session: Entity, command: Command) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
            session,
            session_id: 1,
            input: PlayerInput::Command(command),
        });
}

// ---------------------------------------------------------------------------
// Player initialisation
// ---------------------------------------------------------------------------

#[test]
fn character_created_inserts_progress_vitals_and_alignment() {
    let mut app = test_app(content());
    let session = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<Messages<CharacterCreated>>()
        .write(CharacterCreated {
            session,
            account_id: 1,
            name: "Hero".to_owned(),
            archetype: Some("Magic User".to_owned()),
        });
    run_game(&mut app);

    let progress = app
        .world()
        .get::<TbaProgress>(session)
        .cloned()
        .expect("progress inserted");
    assert_eq!(progress.class, SmolStr::new("magic_user"));
    assert_eq!(progress.level, 1);
    let vitals = app.world().get::<TbaCombatant>(session).expect("vitals");
    assert!(vitals.is_alive());
    assert_eq!(vitals.max_mana, 3, "1d4 average at level 1 rounds to 3");
    assert_eq!(app.world().get::<TbaAlignment>(session).unwrap().0, 0);
    assert_eq!(
        app.world().get::<TbaGold>(session).unwrap().0,
        0,
        "a fresh character starts with an empty purse"
    );
}

#[test]
fn player_init_unknown_class_defaults_to_warrior_and_is_idempotent() {
    let mut app = test_app(content());
    let session = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<Messages<CharacterCreated>>()
        .write(CharacterCreated {
            session,
            account_id: 1,
            name: "Hero".to_owned(),
            archetype: Some("Assassin".to_owned()),
        });
    run_game(&mut app);
    assert_eq!(
        app.world().get::<TbaProgress>(session).unwrap().class,
        SmolStr::new("warrior")
    );

    // A second event must not reset current HP.
    app.world_mut().get_mut::<TbaCombatant>(session).unwrap().hp = 3;
    app.world_mut()
        .resource_mut::<Messages<CharacterCreated>>()
        .write(CharacterCreated {
            session,
            account_id: 1,
            name: "Hero".to_owned(),
            archetype: Some("Magic User".to_owned()),
        });
    run_game(&mut app);
    assert_eq!(app.world().get::<TbaCombatant>(session).unwrap().hp, 3);
    assert_eq!(
        app.world().get::<TbaProgress>(session).unwrap().class,
        SmolStr::new("warrior")
    );
}

// ---------------------------------------------------------------------------
// Auto-engage
// ---------------------------------------------------------------------------

#[test]
fn aggressive_npc_engages_but_passive_npc_does_not() {
    let mut app = test_app(content_with(&[
        ("orc", vec![NpcBehaviour::Aggressive]),
        ("rabbit", Vec::new()),
    ]));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    let orc = spawn_npc(app.world_mut(), room, "orc");
    let rabbit = spawn_npc(app.world_mut(), room, "rabbit");
    run_fixed(&mut app);

    assert_eq!(target_of(&app, orc), Some(player));
    assert_eq!(position_of(&app, orc), Position::Fighting);
    assert_eq!(target_of(&app, rabbit), None);
    assert_eq!(position_of(&app, rabbit), Position::Standing);
}

#[test]
fn aggro_flags_gate_on_player_alignment() {
    let mut app = test_app(content_with(&[
        ("holy_hater", vec![NpcBehaviour::AggrGood]),
        ("demon", vec![NpcBehaviour::AggrEvil]),
        ("elemental", vec![NpcBehaviour::AggrNeutral]),
    ]));
    let room = spawn_room(app.world_mut());
    let good = spawn_player(app.world_mut(), room, 500);
    let holy_hater = spawn_npc(app.world_mut(), room, "holy_hater");
    let demon = spawn_npc(app.world_mut(), room, "demon");
    let elemental = spawn_npc(app.world_mut(), room, "elemental");
    run_fixed(&mut app);

    assert_eq!(
        target_of(&app, holy_hater),
        Some(good),
        "AggrGood hates good"
    );
    assert_eq!(target_of(&app, demon), None, "AggrEvil ignores good");
    assert_eq!(target_of(&app, elemental), None, "AggrNeutral ignores good");
}

#[test]
fn evil_player_draws_the_evil_hater() {
    let mut app = test_app(content_with(&[
        ("holy_hater", vec![NpcBehaviour::AggrGood]),
        ("demon", vec![NpcBehaviour::AggrEvil]),
    ]));
    let room = spawn_room(app.world_mut());
    let evil = spawn_player(app.world_mut(), room, -500);
    let demon = spawn_npc(app.world_mut(), room, "demon");
    let holy_hater = spawn_npc(app.world_mut(), room, "holy_hater");
    run_fixed(&mut app);

    assert_eq!(target_of(&app, demon), Some(evil));
    assert_eq!(target_of(&app, holy_hater), None);
}

#[test]
fn memory_mob_re_engages_a_remembered_player() {
    let mut app = test_app(content_with(&[("haunt", vec![NpcBehaviour::Memory])]));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    let haunt = spawn_npc(app.world_mut(), room, "haunt");
    // Initialise the memory component, then remember the player.
    run_fixed(&mut app);
    assert_eq!(target_of(&app, haunt), None, "passive until attacked");
    app.world_mut()
        .get_mut::<TbaAttackerMemory>(haunt)
        .unwrap()
        .0
        .insert(player);
    run_fixed(&mut app);
    assert_eq!(target_of(&app, haunt), Some(player));
}

#[test]
fn non_memory_mob_does_not_re_engage_from_the_set() {
    let mut app = test_app(content_with(&[("statue", vec![NpcBehaviour::Sentinel])]));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    let statue = spawn_npc(app.world_mut(), room, "statue");
    run_fixed(&mut app);
    app.world_mut()
        .get_mut::<TbaAttackerMemory>(statue)
        .unwrap()
        .0
        .insert(player);
    run_fixed(&mut app);
    assert_eq!(target_of(&app, statue), None);
}

#[test]
fn helper_npc_joins_an_allys_fight() {
    let mut app = test_app(content_with(&[
        ("captain", vec![NpcBehaviour::Aggressive]),
        ("guardsman", vec![NpcBehaviour::Helper]),
    ]));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    let captain = spawn_npc(app.world_mut(), room, "captain");
    let guardsman = spawn_npc(app.world_mut(), room, "guardsman");
    run_fixed(&mut app);

    assert_eq!(target_of(&app, captain), Some(player));
    assert_eq!(target_of(&app, guardsman), Some(player), "helper joins");
    assert_eq!(position_of(&app, guardsman), Position::Fighting);
}

// ---------------------------------------------------------------------------
// Player commands
// ---------------------------------------------------------------------------

#[test]
fn attack_command_sets_target_position_and_memory() {
    let mut app = test_app(content_with(&[("goblin", Vec::new())]));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    let goblin = spawn_npc(app.world_mut(), room, "goblin");
    run_fixed(&mut app);

    send_command(
        &mut app,
        player,
        Command::Attack {
            target: "gob".to_owned(),
        },
    );
    run_game(&mut app);

    assert_eq!(target_of(&app, player), Some(goblin));
    assert_eq!(position_of(&app, player), Position::Fighting);
    assert!(
        app.world()
            .get::<TbaAttackerMemory>(goblin)
            .unwrap()
            .0
            .contains(&player),
        "attacking a mob records the attacker"
    );
}

#[test]
fn attack_command_rejects_self_and_unknown_targets() {
    let mut app = test_app(content());
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    run_game(&mut app);

    let before = drain_text(&mut app);
    send_command(
        &mut app,
        player,
        Command::Attack {
            target: "hero".to_owned(),
        },
    );
    run_game(&mut app);
    assert_eq!(target_of(&app, player), None);
    let denied: Vec<String> = drain_text(&mut app);
    assert!(
        denied.iter().any(|line| line.contains("don't see")),
        "unknown target denied: {denied:?} (before: {before:?})"
    );
}

#[test]
fn flee_clears_the_target_and_moves_through_an_exit() {
    let mut app = test_app(content());
    let world = app.world_mut();
    let room = spawn_room(world);
    let destination = spawn_room(world);
    let player = spawn_player(world, room, 0);
    let enemy = spawn_npc(world, room, "orc");
    world.entity_mut(room).insert(Exits {
        map: HashMap::from([(
            ExitKey::from_word("north").expect("north is a direction"),
            destination,
        )]),
        hidden: BTreeSet::new(),
    });
    world.get_mut::<TbaCombatant>(player).unwrap().target = Some(enemy);
    world.get_mut::<TbaCombatant>(player).unwrap().position = Position::Fighting;

    send_command(&mut app, player, Command::Flee);
    run_game(&mut app);

    assert_eq!(target_of(&app, player), None);
    assert_eq!(position_of(&app, player), Position::Standing);
    assert_eq!(
        app.world().get::<Location>(player).unwrap().room,
        destination,
        "flee used the only exit"
    );
}

fn drain_text(app: &mut App) -> Vec<String> {
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

// ---------------------------------------------------------------------------
// Round resolution
// ---------------------------------------------------------------------------

#[test]
fn resolve_rounds_deals_damage_without_defeating_a_healthy_target() {
    let mut app = test_app(content());
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    let dummy = spawn_npc(app.world_mut(), room, "dummy");
    app.world_mut()
        .entity_mut(dummy)
        .insert(combatant(100, 100, 0));
    app.world_mut()
        .get_mut::<TbaCombatant>(player)
        .unwrap()
        .target = Some(dummy);
    // Guarantee the swing lands: THAC0 -100 vs AC 100.
    app.world_mut()
        .get_mut::<TbaCombatant>(player)
        .unwrap()
        .thaco = -100;

    run_round(&mut app);

    let hp = combatant_of(&app, dummy).hp;
    assert!(hp < 100 && hp > 0, "took damage but survived: {hp}");
    assert!(drain_defeats(&mut app).is_empty());
    assert!(app.world().get_entity(dummy).is_ok());
}

#[test]
fn strength_bonus_flips_a_borderline_swing_in_resolve_rounds() {
    // circle's `compute_thaco` subtracts `str_app[].tohit` from THAC0
    // (`tbamud/src/fight.c:830`). The first d20 from the shared seed is 14;
    // with THAC0 26 against AC 100 the needed roll is 16, so a neutral (13)
    // attacker misses while strength 20 (str_app +3) lands on 17.
    fn swing(strength: i32) -> bool {
        let mut app = test_app(content());
        let room = spawn_room(app.world_mut());
        let player = spawn_player(app.world_mut(), room, 0);
        let dummy = spawn_npc(app.world_mut(), room, "dummy");
        app.world_mut()
            .entity_mut(dummy)
            .insert(combatant(100, 100, 0));
        {
            let mut att = app.world_mut().get_mut::<TbaCombatant>(player).unwrap();
            att.target = Some(dummy);
            att.thaco = 26;
            att.attributes.strength = strength;
        }
        run_round(&mut app);
        combatant_of(&app, dummy).hp < 100
    }

    assert!(!swing(13), "a 13-strength attacker misses the 16 threshold");
    assert!(
        swing(20),
        "str_app's +3 lifts the 14 roll over the threshold"
    );
}

#[test]
fn lethal_hit_on_a_player_emits_player_defeated() {
    let mut app = test_app(content());
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    let attacker = spawn_npc(app.world_mut(), room, "executioner");
    app.world_mut()
        .entity_mut(attacker)
        .insert(combatant(50, -100, 100));
    app.world_mut()
        .get_mut::<TbaCombatant>(attacker)
        .unwrap()
        .target = Some(player);
    assert!(combatant_of(&app, player).is_alive());

    run_round(&mut app);

    assert_eq!(drain_defeats(&mut app), vec![player]);
    assert!(!combatant_of(&app, player).is_alive());
    assert_eq!(combatant_of(&app, player).position, Position::Dead);
}

#[test]
fn lethal_hit_on_an_npc_despawns_it() {
    let mut app = test_app(content());
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    let victim = spawn_npc(app.world_mut(), room, "victim");
    app.world_mut()
        .entity_mut(victim)
        .insert(combatant(1, 100, 0));
    app.world_mut()
        .get_mut::<TbaCombatant>(player)
        .unwrap()
        .target = Some(victim);
    app.world_mut()
        .get_mut::<TbaCombatant>(player)
        .unwrap()
        .thaco = -100;
    app.world_mut()
        .get_mut::<TbaCombatant>(player)
        .unwrap()
        .damroll = 100;

    run_round(&mut app);

    assert!(
        app.world().get_entity(victim).is_err(),
        "a defeated NPC is despawned"
    );
    assert!(drain_defeats(&mut app).is_empty(), "not a player");
}

// ---------------------------------------------------------------------------
// Sentinel / pursuit
// ---------------------------------------------------------------------------

#[test]
fn sentinel_holds_its_room_while_a_plain_mob_follows() {
    let mut app = test_app(content_with(&[
        ("guard", vec![NpcBehaviour::Sentinel]),
        ("wolf", Vec::new()),
    ]));
    let world = app.world_mut();
    let home = spawn_room(world);
    let chase = spawn_room(world);
    let player = spawn_player(world, chase, 0);
    let guard = spawn_npc(world, home, "guard");
    let wolf = spawn_npc(world, home, "wolf");
    world.entity_mut(home).insert(Exits {
        map: HashMap::from([(
            ExitKey::from_word("north").expect("north is a direction"),
            chase,
        )]),
        hidden: BTreeSet::new(),
    });
    run_fixed(&mut app);
    app.world_mut()
        .get_mut::<TbaCombatant>(guard)
        .unwrap()
        .target = Some(player);
    app.world_mut()
        .get_mut::<TbaCombatant>(wolf)
        .unwrap()
        .target = Some(player);

    run_fixed(&mut app);

    assert_eq!(
        app.world().get::<Location>(guard).unwrap().room,
        home,
        "sentinel never leaves home"
    );
    assert_eq!(target_of(&app, guard), None, "sentinel gives up the chase");
    assert_eq!(
        app.world().get::<Location>(wolf).unwrap().room,
        chase,
        "an ordinary mob follows one room"
    );
    assert_eq!(target_of(&app, wolf), Some(player));
}

// ---------------------------------------------------------------------------
// Kill rewards (issue #2)
// ---------------------------------------------------------------------------

/// Spawn a 1-HP goblin, point the (always-hit, hard-hitting) player at it, and
/// run one round plus the award pass.
fn kill_goblin(app: &mut App, player: Entity, room: Entity) {
    let npc = spawn_npc(app.world_mut(), room, "goblin");
    app.world_mut().entity_mut(npc).insert(combatant(1, 20, 0));
    {
        let mut attacker = app.world_mut().get_mut::<TbaCombatant>(player).unwrap();
        attacker.thaco = 0;
        attacker.damroll = 5;
        attacker.target = Some(npc);
    }
    run_round(app);
    run_game(app);
    assert!(
        app.world().get::<NpcKey>(npc).is_none(),
        "the goblin is despawned by the killing round"
    );
}

#[test]
fn killing_an_npc_awards_experience_gold_and_levels() {
    let mut app = test_app(reward_content(100, 7));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    app.world_mut()
        .entity_mut(player)
        .insert((TbaProgress::new(SmolStr::new("warrior")), TbaGold(0)));

    kill_goblin(&mut app, player, room);

    let progress = app.world().get::<TbaProgress>(player).unwrap();
    assert_eq!(progress.xp, 100, "the prototype's experience is granted");
    assert_eq!(progress.level, 2, "100 XP is the level-2 threshold");
    assert_eq!(
        app.world().get::<TbaGold>(player).unwrap().0,
        7,
        "the prototype's gold is granted"
    );
}

#[test]
fn experience_can_be_regained_after_death_without_deleveling() {
    let mut app = test_app(reward_content(100, 0));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    app.world_mut()
        .entity_mut(player)
        .insert((TbaProgress::new(SmolStr::new("warrior")), TbaGold(0)));

    // Two kills: level 2 at 100 XP, then 200 XP (still level 2).
    kill_goblin(&mut app, player, room);
    kill_goblin(&mut app, player, room);
    assert_eq!(app.world().get::<TbaProgress>(player).unwrap().level, 2);

    // Die: lose 25% of the XP earned within the level, stay level 2.
    app.world_mut()
        .resource_mut::<Messages<PlayerDefeated>>()
        .write(PlayerDefeated { player });
    run_game(&mut app);
    let after_death = app.world().get::<TbaProgress>(player).unwrap().clone();
    assert_eq!(after_death.level, 2, "death never de-levels");
    assert_eq!(after_death.xp, 175, "25% of the 100 XP above the threshold");

    // Kill again: the lost XP is regained.
    kill_goblin(&mut app, player, room);
    let progress = app.world().get::<TbaProgress>(player).unwrap();
    assert_eq!(progress.xp, 275, "XP is regained after death");
    assert_eq!(progress.level, 2);
}

/// `reward_content` with the goblin's moral alignment overridden.
fn aligned_reward_content(alignment: i32) -> TbaContent {
    let mut content = reward_content(0, 0);
    content
        .npc_stats
        .get_mut(&NpcId::from("goblin"))
        .expect("goblin stats")
        .alignment = alignment;
    content
}

#[test]
fn killing_a_good_mob_moves_the_killer_evil() {
    let mut app = test_app(aligned_reward_content(1000));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    app.world_mut()
        .entity_mut(player)
        .insert((TbaProgress::new(SmolStr::new("warrior")), TbaGold(0)));

    kill_goblin(&mut app, player, room);

    assert_eq!(
        app.world().get::<TbaAlignment>(player).unwrap().0,
        -62,
        "Circle moves the killer 1/16 of the way toward -victim"
    );
}

#[test]
fn killing_an_evil_mob_moves_the_killer_good() {
    let mut app = test_app(aligned_reward_content(-1000));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    app.world_mut()
        .entity_mut(player)
        .insert((TbaProgress::new(SmolStr::new("warrior")), TbaGold(0)));

    kill_goblin(&mut app, player, room);

    assert_eq!(app.world().get::<TbaAlignment>(player).unwrap().0, 62);
}

#[test]
fn killing_a_neutral_mob_leaves_a_neutral_killer_unchanged() {
    let mut app = test_app(aligned_reward_content(0));
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    app.world_mut()
        .entity_mut(player)
        .insert((TbaProgress::new(SmolStr::new("warrior")), TbaGold(0)));

    kill_goblin(&mut app, player, room);

    assert_eq!(app.world().get::<TbaAlignment>(player).unwrap().0, 0);
}

#[test]
fn the_updated_alignment_is_what_aggro_reads() {
    // A demon that hates evil joins the world from the start; after enough
    // good kills the hero's alignment crosses `EVIL_ALIGNMENT` and the demon
    // engages. This proves the reward pass writes the component the aggro
    // check reads, not a copy.
    let mut content = aligned_reward_content(1000);
    content
        .npc_stats
        .insert(NpcId::from("demon"), stats(vec![NpcBehaviour::AggrEvil]));
    let mut app = test_app(content);
    let room = spawn_room(app.world_mut());
    let player = spawn_player(app.world_mut(), room, 0);
    app.world_mut()
        .entity_mut(player)
        .insert((TbaProgress::new(SmolStr::new("warrior")), TbaGold(0)));
    let demon = spawn_npc(app.world_mut(), room, "demon");

    // Neutral at first: the evil-hater ignores the hero.
    run_fixed(&mut app);
    assert_eq!(target_of(&app, demon), None);

    // Seven +1000 kills take alignment from 0 to -361 (< -350).
    for _ in 0..7 {
        kill_goblin(&mut app, player, room);
    }
    assert_eq!(app.world().get::<TbaAlignment>(player).unwrap().0, -361);

    run_fixed(&mut app);
    assert_eq!(
        target_of(&app, demon),
        Some(player),
        "the demon now hates the hero's updated alignment"
    );
}
