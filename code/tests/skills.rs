//! Issue #25 integration tests for the Circle class abilities and stealth.
//!
//! These drive the real [`handle_skill_commands`] system against a small ECS
//! app (the same shape `tests/combat.rs` uses). They cover the required
//! semantics: the class gate, the level gate, damage (kick + backstab
//! multiplier), movement cost and position gates, bash knockdown, rescue/assist
//! retargeting, `consider` output, and hide/sneak aggro suppression with
//! hidden cleared on attack.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const SEED: u64 = 0xC0FFEE;

fn class(id: &str, granted: &[&str]) -> TbaClassDef {
    TbaClassDef {
        name: id.to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 10, 0),
        mana_dice: None,
        xp_table: Vec::new(),
        thaco_table: Vec::new(),
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: granted.iter().map(|skill| SmolStr::new(*skill)).collect(),
    }
}

fn stats(level: u32, behaviour: Vec<NpcBehaviour>) -> TbaNpcStats {
    TbaNpcStats {
        hp: Dice::constant(50),
        level,
        hitroll: 0,
        damroll: 0,
        // A very high AC makes the melee to-hit roll always land, so a test
        // asserts the damage number rather than the d20.
        armor_class: 1000,
        thaco: 20,
        alignment: 0,
        gold: 0,
        experience: 0,
        default_position: Position::Standing,
        behaviour,
        damage: None,
        attributes: TbaAttributes::mob_default(),
        saves: TbaSaves::default(),
        spec_proc: None,
    }
}

fn dagger() -> TbaItemDef {
    TbaItemDef {
        kind: TbaItemKind::Weapon,
        slots: Vec::new(),
        // `[unused, dice_count, dice_sides, damage_type]`; `11` is
        // `TYPE_PIERCE - TYPE_HIT`, and `1d1` makes the roll deterministic.
        values: [0, 1, 1, 11],
        affects: Vec::new(),
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
    let mut content = TbaContent::default();
    content.classes.insert(
        SmolStr::new("warrior"),
        class("warrior", &["kick", "rescue", "bash"]),
    );
    content.classes.insert(
        SmolStr::new("thief"),
        class("thief", &["sneak", "hide", "pick_lock", "backstab"]),
    );
    content
        .classes
        .insert(SmolStr::new("mage"), class("mage", &[]));
    content.skill_levels.insert(
        SmolStr::new("warrior"),
        BTreeMap::from([
            (SmolStr::new("kick"), 1),
            (SmolStr::new("rescue"), 3),
            (SmolStr::new("bash"), 12),
        ]),
    );
    content.skill_levels.insert(
        SmolStr::new("thief"),
        BTreeMap::from([
            (SmolStr::new("sneak"), 1),
            (SmolStr::new("hide"), 5),
            (SmolStr::new("backstab"), 3),
            (SmolStr::new("pick_lock"), 2),
        ]),
    );
    content
        .npc_stats
        .insert(NpcId::from("goblin"), stats(5, Vec::new()));
    content
        .npc_stats
        .insert(NpcId::from("dragon"), stats(20, Vec::new()));
    content
        .npc_stats
        .insert(NpcId::from("orc"), stats(5, vec![NpcBehaviour::Aggressive]));
    content.items.insert(ItemId::from("dagger"), dagger());
    content
}

fn test_app(content: TbaContent) -> App {
    let mut app = App::new();
    app.insert_resource(content);
    app.insert_resource(TbaRng::from_seed(SEED));
    app.init_resource::<RoomOccupants>();
    app.init_resource::<VisibilityPolicy>();
    app.add_message::<OutputEvent>();
    app.add_message::<CommandReceived>();
    app.add_message::<PlayerDefeated>();
    app.add_message::<NpcDefeated>();
    app.add_message::<TbaDamage>();
    app.add_systems(
        Update,
        (
            // Drive the engine `Concealment` carrier from the pack's private
            // state both before the consumers (a direct `TbaHidden` insert)
            // and after the command handlers (a `hide` this frame).
            sync_concealment,
            maintain_room_occupants,
            handle_combat_commands,
            handle_skill_commands,
            sync_concealment,
        )
            .chain(),
    );
    app.add_systems(
        FixedUpdate,
        (maintain_room_occupants, auto_engage_system).chain(),
    );
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

fn spawn_room(app: &mut App) -> Entity {
    app.world_mut().spawn_empty().id()
}

fn combatant(hp: i32, position: Position, moves: i32) -> TbaCombatant {
    TbaCombatant {
        hp,
        max_hp: hp,
        mana: 0,
        max_mana: 0,
        move_points: moves,
        max_move_points: moves,
        hitroll: 0,
        damroll: 0,
        armor_class: 1000,
        thaco: 20,
        attributes: TbaAttributes::neutral(),
        saves: TbaSaves::default(),
        position,
        target: None,
    }
}

fn spawn_player(
    app: &mut App,
    room: Entity,
    name: &str,
    class_id: &str,
    level: u32,
    position: Position,
) -> Entity {
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
            TbaProgress {
                class: SmolStr::new(class_id),
                level,
                xp: 0,
            },
            TbaAlignment(0),
            combatant(30, position, 80),
        ))
        .id()
}

fn spawn_npc(app: &mut App, room: Entity, id: &str) -> Entity {
    app.world_mut()
        .spawn((
            NpcKey(NpcId::from(id)),
            Npc {
                name: id.to_owned(),
                persona: String::new(),
            },
            Location { room },
            combatant(50, Position::Standing, 80),
        ))
        .id()
}

fn send_line(app: &mut App, session: Entity, line: &str) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
            session,
            session_id: 1,
            input: PlayerInput::RawLine(line.to_owned()),
        });
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

fn hp_of(app: &App, entity: Entity) -> i32 {
    app.world().get::<TbaCombatant>(entity).unwrap().hp
}

fn position_of(app: &App, entity: Entity) -> Position {
    app.world().get::<TbaCombatant>(entity).unwrap().position
}

fn target_of(app: &App, entity: Entity) -> Option<Entity> {
    app.world()
        .get::<TbaCombatant>(entity)
        .and_then(|combatant| combatant.target)
}

fn has_hidden(app: &App, entity: Entity) -> bool {
    app.world().get::<TbaHidden>(entity).is_some()
}

fn has_sneaking(app: &App, entity: Entity) -> bool {
    app.world().get::<TbaSneaking>(entity).is_some()
}

/// A room with one authored, locked door at `direction`. The runtime
/// [`RoomState`] starts closed and locked, mirroring a converted door.
fn spawn_room_with_door(app: &mut App, direction: ExitKey, pickable: bool) -> Entity {
    let room = spawn_room(app);
    let mut doors = Doors::default();
    doors.by_dir.insert(
        direction,
        Door {
            key: None,
            name: Some("wooden door".to_owned()),
            pickable,
            difficulty: 0,
            default_closed: true,
            default_locked: true,
        },
    );
    let mut state = RoomState::default();
    state.doors.insert(
        direction,
        DoorState {
            closed: true,
            locked: true,
            to: None,
        },
    );
    app.world_mut().entity_mut(room).insert((doors, state));
    room
}

fn door_state(app: &App, room: Entity, direction: ExitKey) -> DoorState {
    app.world()
        .get::<RoomState>(room)
        .and_then(|state| state.doors.get(&direction).cloned())
        .expect("the room has a seeded door state")
}

/// A seed whose first `range(1, 101)` is the always-failure 101.
fn failing_seed() -> u64 {
    (0..10_000)
        .find(|seed| {
            let mut rng = TbaRng::from_seed(*seed);
            rng.range(1, 101) == 101
        })
        .expect("some seed fails the 1-in-101 roll")
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

#[test]
fn unknown_class_cannot_use_a_class_skill() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let mage = spawn_player(&mut app, room, "Mage", "mage", 20, Position::Standing);
    let goblin = spawn_npc(&mut app, room, "goblin");

    send_line(&mut app, mage, "kick goblin");
    run_game(&mut app);

    let text = drain_text(&mut app);
    assert!(
        text.iter().any(|line| line.contains("no idea how")),
        "a mage has no kick skill: {text:?}"
    );
    assert_eq!(hp_of(&app, goblin), 50, "the goblin was not touched");
}

#[test]
fn level_gate_blocks_bash_below_twelve() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let warrior = spawn_player(&mut app, room, "Hero", "warrior", 1, Position::Standing);
    let goblin = spawn_npc(&mut app, room, "goblin");
    app.world_mut()
        .entity_mut(warrior)
        .insert(WieldedWeapon(ItemId::from("dagger")));

    send_line(&mut app, warrior, "bash goblin");
    run_game(&mut app);

    let text = drain_text(&mut app);
    assert!(
        text.iter()
            .any(|line| line.contains("do not know of that skill")),
        "bash is a level-12 skill: {text:?}"
    );
    assert_eq!(position_of(&app, goblin), Position::Standing);
}

// ---------------------------------------------------------------------------
// kick
// ---------------------------------------------------------------------------

#[test]
fn kick_deals_half_level_damage() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let warrior = spawn_player(&mut app, room, "Hero", "warrior", 10, Position::Standing);
    let goblin = spawn_npc(&mut app, room, "goblin");

    send_line(&mut app, warrior, "kick goblin");
    run_game(&mut app);

    assert_eq!(hp_of(&app, goblin), 45, "level 10 kick deals 5");
    let text = drain_text(&mut app);
    assert!(
        text.iter().any(|line| line.contains("kick")),
        "the kick is announced: {text:?}"
    );
    assert_eq!(target_of(&app, warrior), Some(goblin));
}

#[test]
fn kick_requires_a_standing_position() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let warrior = spawn_player(&mut app, room, "Hero", "warrior", 10, Position::Sitting);
    let goblin = spawn_npc(&mut app, room, "goblin");

    send_line(&mut app, warrior, "kick goblin");
    run_game(&mut app);

    assert_eq!(hp_of(&app, goblin), 50);
    let text = drain_text(&mut app);
    assert!(
        text.iter().any(|line| line.contains("proper position")),
        "{text:?}"
    );
}

#[test]
fn kick_requires_move_points() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let warrior = spawn_player(&mut app, room, "Hero", "warrior", 10, Position::Standing);
    app.world_mut()
        .get_mut::<TbaCombatant>(warrior)
        .unwrap()
        .move_points = 0;
    let goblin = spawn_npc(&mut app, room, "goblin");

    send_line(&mut app, warrior, "kick goblin");
    run_game(&mut app);

    assert_eq!(hp_of(&app, goblin), 50);
    let text = drain_text(&mut app);
    assert!(
        text.iter().any(|line| line.contains("too exhausted")),
        "{text:?}"
    );
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

#[test]
fn bash_deals_one_and_knocks_the_victim_down() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let warrior = spawn_player(&mut app, room, "Hero", "warrior", 12, Position::Standing);
    app.world_mut()
        .entity_mut(warrior)
        .insert(WieldedWeapon(ItemId::from("dagger")));
    let goblin = spawn_npc(&mut app, room, "goblin");

    send_line(&mut app, warrior, "bash goblin");
    run_game(&mut app);

    assert_eq!(hp_of(&app, goblin), 49, "bash deals a single point");
    assert_eq!(
        position_of(&app, goblin),
        Position::Sitting,
        "Circle drops a bashed victim to POS_SITTING"
    );
}

#[test]
fn bash_requires_a_wielded_weapon() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let warrior = spawn_player(&mut app, room, "Hero", "warrior", 12, Position::Standing);
    let goblin = spawn_npc(&mut app, room, "goblin");

    send_line(&mut app, warrior, "bash goblin");
    run_game(&mut app);

    assert_eq!(position_of(&app, goblin), Position::Standing);
    let text = drain_text(&mut app);
    assert!(
        text.iter().any(|line| line.contains("wield a weapon")),
        "{text:?}"
    );
}

// ---------------------------------------------------------------------------
// backstab
// ---------------------------------------------------------------------------

#[test]
fn backstab_applies_the_opening_multiplier_and_breaks_stealth() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 3, Position::Standing);
    app.world_mut()
        .entity_mut(thief)
        .insert(WieldedWeapon(ItemId::from("dagger")));
    app.world_mut().entity_mut(thief).insert(TbaHidden);
    let goblin = spawn_npc(&mut app, room, "goblin");

    send_line(&mut app, thief, "backstab goblin");
    run_game(&mut app);

    // `1d1 + 11` weapon damage, doubled by `backstab_mult(3) == 2`.
    assert_eq!(hp_of(&app, goblin), 50 - 24);
    assert!(!has_hidden(&app, thief), "attacking breaks AFF_HIDE");
    assert_eq!(target_of(&app, thief), Some(goblin));
}

#[test]
fn backstab_refuses_a_fighting_victim() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 3, Position::Standing);
    app.world_mut()
        .entity_mut(thief)
        .insert(WieldedWeapon(ItemId::from("dagger")));
    let goblin = spawn_npc(&mut app, room, "goblin");
    app.world_mut()
        .get_mut::<TbaCombatant>(goblin)
        .unwrap()
        .target = Some(thief);

    send_line(&mut app, thief, "backstab goblin");
    run_game(&mut app);

    assert_eq!(hp_of(&app, goblin), 50);
    let text = drain_text(&mut app);
    assert!(
        text.iter().any(|line| line.contains("too alert")),
        "{text:?}"
    );
}

// ---------------------------------------------------------------------------
// rescue / assist
// ---------------------------------------------------------------------------

#[test]
fn rescue_retargets_the_attackers_onto_the_rescuer() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let rescuer = spawn_player(&mut app, room, "Knight", "warrior", 3, Position::Standing);
    let ally = spawn_player(&mut app, room, "Ally", "mage", 3, Position::Standing);
    let attacker = spawn_npc(&mut app, room, "goblin");
    app.world_mut()
        .get_mut::<TbaCombatant>(ally)
        .unwrap()
        .target = Some(attacker);
    app.world_mut()
        .get_mut::<TbaCombatant>(attacker)
        .unwrap()
        .target = Some(ally);

    send_line(&mut app, rescuer, "rescue Ally");
    run_game(&mut app);

    assert_eq!(target_of(&app, attacker), Some(rescuer));
    assert_eq!(target_of(&app, rescuer), Some(attacker));
    assert_eq!(target_of(&app, ally), None, "the ally is freed");
}

#[test]
fn rescue_needs_someone_fighting_the_ally() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let rescuer = spawn_player(&mut app, room, "Knight", "warrior", 3, Position::Standing);
    let ally = spawn_player(&mut app, room, "Ally", "mage", 3, Position::Standing);

    send_line(&mut app, rescuer, "rescue Ally");
    run_game(&mut app);

    let text = drain_text(&mut app);
    assert!(
        text.iter().any(|line| line.contains("nobody is fighting")),
        "{text:?}"
    );
    let _ = ally;
}

#[test]
fn assist_targets_the_allies_attacker() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let assistant = spawn_player(&mut app, room, "Knight", "warrior", 3, Position::Standing);
    let ally = spawn_player(&mut app, room, "Ally", "mage", 3, Position::Standing);
    let attacker = spawn_npc(&mut app, room, "goblin");
    app.world_mut()
        .get_mut::<TbaCombatant>(ally)
        .unwrap()
        .target = Some(attacker);

    send_line(&mut app, assistant, "assist Ally");
    run_game(&mut app);

    assert_eq!(target_of(&app, assistant), Some(attacker));
    assert_eq!(position_of(&app, assistant), Position::Fighting);
}

// ---------------------------------------------------------------------------
// consider
// ---------------------------------------------------------------------------

#[test]
fn consider_prints_the_circle_comparison() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let warrior = spawn_player(&mut app, room, "Hero", "warrior", 5, Position::Standing);
    let _goblin = spawn_npc(&mut app, room, "goblin");
    let _dragon = spawn_npc(&mut app, room, "dragon");

    send_line(&mut app, warrior, "consider goblin");
    run_game(&mut app);
    let even = drain_text(&mut app);
    assert!(
        even.iter().any(|line| line == "The perfect match!"),
        "{even:?}"
    );

    send_line(&mut app, warrior, "consider dragon");
    run_game(&mut app);
    let hopeless = drain_text(&mut app);
    assert!(
        hopeless.iter().any(|line| line == "You ARE mad!"),
        "{hopeless:?}"
    );
}

// ---------------------------------------------------------------------------
// hide / sneak
// ---------------------------------------------------------------------------

#[test]
fn hide_and_sneak_suppress_auto_engage() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 5, Position::Standing);
    let orc = spawn_npc(&mut app, room, "orc");

    send_line(&mut app, thief, "hide");
    run_game(&mut app);
    assert!(has_hidden(&app, thief), "hide sets TbaHidden");
    run_fixed(&mut app);
    assert_eq!(target_of(&app, orc), None, "a hidden player is not aggroed");

    // Clear it: the same mob now engages. One `Update` lets the sync drop the
    // engine `Concealment` before the next aggro pass.
    app.world_mut().entity_mut(thief).remove::<TbaHidden>();
    run_game(&mut app);
    run_fixed(&mut app);
    assert_eq!(target_of(&app, orc), Some(thief));
}

#[test]
fn sneak_suppresses_auto_engage() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 5, Position::Standing);
    let orc = spawn_npc(&mut app, room, "orc");

    send_line(&mut app, thief, "sneak");
    run_game(&mut app);
    assert!(has_sneaking(&app, thief), "sneak sets TbaSneaking");
    run_fixed(&mut app);
    assert_eq!(
        target_of(&app, orc),
        None,
        "a sneaking player is not aggroed"
    );
}

#[test]
fn attacking_clears_hidden() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let warrior = spawn_player(&mut app, room, "Hero", "warrior", 5, Position::Standing);
    let _goblin = spawn_npc(&mut app, room, "goblin");
    app.world_mut().entity_mut(warrior).insert(TbaHidden);

    send_command(
        &mut app,
        warrior,
        Command::Attack {
            target: "goblin".to_owned(),
        },
    );
    run_game(&mut app);

    assert!(!has_hidden(&app, warrior), "attacking clears TbaHidden");
}

#[test]
fn unknown_class_cannot_hide() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let mage = spawn_player(&mut app, room, "Mage", "mage", 20, Position::Standing);

    send_line(&mut app, mage, "hide");
    run_game(&mut app);

    assert!(!has_hidden(&app, mage));
    let text = drain_text(&mut app);
    assert!(
        text.iter()
            .any(|line| line.contains("no idea how to do that")),
        "{text:?}"
    );
}

// ---------------------------------------------------------------------------
// pick_lock
// ---------------------------------------------------------------------------

#[test]
fn pick_lock_opens_a_pickable_lock() {
    let mut app = test_app(content());
    let room = spawn_room_with_door(&mut app, ExitKey::North, true);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 2, Position::Standing);

    send_line(&mut app, thief, "pick_lock north");
    run_game(&mut app);

    let door = door_state(&app, room, ExitKey::North);
    assert!(!door.locked, "the lock yields");
    assert!(door.closed, "the door stays closed");
    let text = drain_text(&mut app);
    assert!(
        text.iter()
            .any(|line| line.contains("lock quickly yields to your skills")),
        "{text:?}"
    );
}

#[test]
fn pick_lock_roll_failure_leaves_the_lock_locked() {
    let mut app = test_app(content());
    app.insert_resource(TbaRng::from_seed(failing_seed()));
    let room = spawn_room_with_door(&mut app, ExitKey::North, true);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 2, Position::Standing);

    send_line(&mut app, thief, "pick_lock north");
    run_game(&mut app);

    let door = door_state(&app, room, ExitKey::North);
    assert!(door.locked, "a failed roll leaves the lock");
    let text = drain_text(&mut app);
    assert!(
        text.iter()
            .any(|line| line.contains("failed to pick the lock")),
        "{text:?}"
    );
}

#[test]
fn pick_lock_refuses_a_pickproof_door() {
    let mut app = test_app(content());
    let room = spawn_room_with_door(&mut app, ExitKey::North, false);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 2, Position::Standing);

    send_line(&mut app, thief, "pick_lock north");
    run_game(&mut app);

    assert!(door_state(&app, room, ExitKey::North).locked);
    let text = drain_text(&mut app);
    assert!(
        text.iter()
            .any(|line| line.contains("resists your attempts")),
        "{text:?}"
    );
}

#[test]
fn pick_lock_is_gated_by_class_and_level() {
    // A mage has no `pick_lock` grant: unknown skill, door untouched.
    let mut app = test_app(content());
    let room = spawn_room_with_door(&mut app, ExitKey::North, true);
    let mage = spawn_player(&mut app, room, "Mage", "mage", 20, Position::Standing);
    send_line(&mut app, mage, "pick_lock north");
    run_game(&mut app);
    assert!(door_state(&app, room, ExitKey::North).locked);
    let text = drain_text(&mut app);
    assert!(
        text.iter()
            .any(|line| line.contains("no idea how to do that")),
        "{text:?}"
    );

    // A level-1 thief is below the level-2 gate.
    let mut app = test_app(content());
    let room = spawn_room_with_door(&mut app, ExitKey::North, true);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 1, Position::Standing);
    send_line(&mut app, thief, "pick_lock north");
    run_game(&mut app);
    assert!(door_state(&app, room, ExitKey::North).locked);
    let text = drain_text(&mut app);
    assert!(
        text.iter()
            .any(|line| line.contains("do not know of that skill")),
        "{text:?}"
    );
}

// ---------------------------------------------------------------------------
// hidden targeting
// ---------------------------------------------------------------------------

#[test]
fn hidden_player_is_not_targetable_by_another() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let hero = spawn_player(&mut app, room, "Hero", "warrior", 5, Position::Standing);
    let shadow = spawn_player(&mut app, room, "Shadow", "thief", 5, Position::Standing);
    app.world_mut().entity_mut(shadow).insert(TbaHidden);

    send_command(
        &mut app,
        hero,
        Command::Attack {
            target: "Shadow".to_owned(),
        },
    );
    run_game(&mut app);

    assert_eq!(
        target_of(&app, hero),
        None,
        "a hidden player is not targetable"
    );
    let text = drain_text(&mut app);
    assert!(
        text.iter()
            .any(|line| line.contains("don't see Shadow here")),
        "{text:?}"
    );
}

#[test]
fn sneaking_player_is_not_targetable_by_another() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let hero = spawn_player(&mut app, room, "Hero", "warrior", 5, Position::Standing);
    let shadow = spawn_player(&mut app, room, "Shadow", "thief", 5, Position::Standing);
    app.world_mut().entity_mut(shadow).insert(TbaSneaking);

    send_command(
        &mut app,
        hero,
        Command::Attack {
            target: "Shadow".to_owned(),
        },
    );
    run_game(&mut app);

    assert_eq!(
        target_of(&app, hero),
        None,
        "a sneaking player is not targetable"
    );
}

#[test]
fn detector_can_target_a_concealed_player() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let hero = spawn_player(&mut app, room, "Hero", "warrior", 5, Position::Standing);
    let shadow = spawn_player(&mut app, room, "Shadow", "thief", 5, Position::Standing);
    app.world_mut().entity_mut(shadow).insert(TbaHidden);
    // True sight on the attacker: the engine seam admits the concealed target.
    app.world_mut().entity_mut(hero).insert(Perception {
        sight: SEE_INVIS,
        detect: u8::MAX,
    });

    send_command(
        &mut app,
        hero,
        Command::Attack {
            target: "Shadow".to_owned(),
        },
    );
    run_game(&mut app);

    assert_eq!(
        target_of(&app, hero),
        Some(shadow),
        "a detector can target a concealed player"
    );
}

#[test]
fn hidden_player_can_still_target_an_npc() {
    let mut app = test_app(content());
    let room = spawn_room(&mut app);
    let thief = spawn_player(&mut app, room, "Shadow", "thief", 5, Position::Standing);
    let goblin = spawn_npc(&mut app, room, "goblin");
    app.world_mut().entity_mut(thief).insert(TbaHidden);

    send_command(
        &mut app,
        thief,
        Command::Attack {
            target: "goblin".to_owned(),
        },
    );
    run_game(&mut app);

    assert_eq!(
        target_of(&app, thief),
        Some(goblin),
        "being hidden does not blind the actor's own targeting"
    );
    assert!(!has_hidden(&app, thief), "attacking clears TbaHidden");
}
