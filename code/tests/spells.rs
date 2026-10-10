//! TBA-E18 spell / `cast` command integration tests.
//!
//! These drive the real [`handle_cast_command`] system against a small ECS app
//! (the same shape `tests/combat.rs` uses), with the affect recompute chained
//! after it so buffs take effect the frame they are cast. They cover the
//! required semantics: unknown spell, insufficient mana (no deduction), a class
//! that does not know the spell, damage + mana, lethal damage
//! (`PlayerDefeated`/despawn), heal capping at max, buff change + expiry, and
//! the position/target gates.
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

fn mage() -> TbaClassDef {
    TbaClassDef {
        name: "Mage".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 4, 0),
        // Large enough that the test's chosen current mana survives the
        // per-frame affect recompute (which clamps to the derived maximum).
        mana_dice: Some(Dice::new(20, 10, 0)),
        xp_table: Vec::new(),
        thaco_table: Vec::new(),
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: [
            "magic_missile",
            "fireball",
            "chill_touch",
            "cure_light",
            "armor",
            "bless",
            "detect_magic",
            "blink",
        ]
        .into_iter()
        .map(SmolStr::new)
        .collect(),
    }
}

fn warrior() -> TbaClassDef {
    TbaClassDef {
        name: "Warrior".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 10, 0),
        // Synthetic: a real warrior has no mana, but the class-gate test needs
        // a purse large enough that a wrongly-successful cast would show.
        mana_dice: Some(Dice::new(20, 10, 0)),
        xp_table: Vec::new(),
        thaco_table: Vec::new(),
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: Vec::new(),
    }
}

fn damage(name: &str, mana: u32, dice: Dice) -> SpellDef {
    SpellDef {
        name: name.to_owned(),
        mana,
        kind: SpellKind::Damage,
        dice: Some(dice),
        affect: None,
        target: SpellTarget::Victim,
        min_position: Position::Fighting,
    }
}

fn heal(name: &str, mana: u32, dice: Dice) -> SpellDef {
    SpellDef {
        name: name.to_owned(),
        mana,
        kind: SpellKind::Heal,
        dice: Some(dice),
        affect: None,
        target: SpellTarget::Victim,
        min_position: Position::Fighting,
    }
}

fn buff(name: &str, mana: u32, affect: AffectDef) -> SpellDef {
    SpellDef {
        name: name.to_owned(),
        mana,
        kind: SpellKind::Buff,
        dice: None,
        affect: Some(affect),
        target: SpellTarget::Victim,
        min_position: Position::Standing,
    }
}

fn detect(name: &str, mana: u32) -> SpellDef {
    SpellDef {
        name: name.to_owned(),
        mana,
        kind: SpellKind::Detect,
        dice: None,
        affect: None,
        target: SpellTarget::Caster,
        min_position: Position::Standing,
    }
}

fn utility(name: &str, mana: u32) -> SpellDef {
    SpellDef {
        name: name.to_owned(),
        mana,
        kind: SpellKind::Utility,
        dice: None,
        affect: None,
        target: SpellTarget::Caster,
        min_position: Position::Standing,
    }
}

fn content() -> TbaContent {
    let spells = BTreeMap::from([
        (
            SmolStr::new("magic_missile"),
            damage("magic missile", 10, Dice::new(1, 8, 1)),
        ),
        (
            SmolStr::new("fireball"),
            damage("fireball", 30, Dice::new(4, 6, 0)),
        ),
        (
            SmolStr::new("chill_touch"),
            SpellDef {
                affect: Some(AffectDef {
                    location: AffectLocation::Str,
                    modifier: -1,
                }),
                ..damage("chill touch", 10, Dice::new(1, 8, 1))
            },
        ),
        (
            SmolStr::new("cure_light"),
            heal("cure light", 10, Dice::new(1, 8, 1)),
        ),
        (
            SmolStr::new("armor"),
            buff(
                "armor",
                15,
                AffectDef {
                    location: AffectLocation::ArmorClass,
                    modifier: -20,
                },
            ),
        ),
        (
            SmolStr::new("bless"),
            buff(
                "bless",
                5,
                AffectDef {
                    location: AffectLocation::Hitroll,
                    modifier: 2,
                },
            ),
        ),
        (SmolStr::new("detect_magic"), detect("detect magic", 10)),
        (SmolStr::new("blink"), utility("blink", 5)),
    ]);
    let classes = BTreeMap::from([
        (SmolStr::new("mage"), mage()),
        (SmolStr::new("warrior"), warrior()),
    ]);
    TbaContent {
        classes,
        spells,
        ..TbaContent::default()
    }
}

/// The cast system wired like `TbaPlugin`: the affect recompute follows so a
/// buff is visible in the frame it lands, and buffs tick on `TbaRound`.
fn test_app() -> App {
    let mut app = App::new();
    app.insert_resource(content());
    app.insert_resource(TbaRng::from_seed(0xC0FFEE));
    app.add_message::<OutputEvent>();
    app.add_message::<CommandReceived>();
    app.add_message::<PlayerDefeated>();
    app.add_systems(
        Update,
        (handle_cast_command, recompute_equipment_affects).chain(),
    );
    app.init_schedule(TbaRound);
    app.add_systems(TbaRound, tick_buffs);
    app
}

fn run_game(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn run_round(app: &mut App) {
    app.world_mut().run_schedule(TbaRound);
    app.world_mut().flush();
}

fn spawn_room(app: &mut App) -> Entity {
    app.world_mut().spawn_empty().id()
}

fn spawn_player(app: &mut App, room: Entity, name: &str, class: &str, mana: i32) -> Entity {
    let class_def = if class == "mage" { mage() } else { warrior() };
    let mut combatant = derive_vitals(&class_def, 1);
    combatant.mana = mana;
    combatant.max_mana = mana.max(combatant.max_mana);
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
            TbaProgress::new(class),
            combatant,
            TbaEquipment::default(),
        ))
        .id()
}

fn spawn_npc(app: &mut App, room: Entity, id: &str, hp: i32) -> Entity {
    let mut combatant = fallback_combatant();
    combatant.hp = hp;
    combatant.max_hp = hp;
    app.world_mut()
        .spawn((
            Npc {
                name: id.to_owned(),
                persona: String::new(),
            },
            Location { room },
            combatant,
        ))
        .id()
}

fn cast(app: &mut App, session: Entity, line: &str) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            session,
            session_id: 1,
            input: PlayerInput::RawLine(line.to_owned()),
        });
}

fn combatant_of(app: &App, entity: Entity) -> TbaCombatant {
    app.world()
        .get::<TbaCombatant>(entity)
        .cloned()
        .expect("combatant present")
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

fn drain_defeats(app: &mut App) -> Vec<Entity> {
    app.world_mut()
        .resource_mut::<Messages<PlayerDefeated>>()
        .drain()
        .map(|event| event.player)
        .collect()
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

#[test]
fn unknown_spell_is_denied_and_costs_no_mana() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);

    cast(&mut app, player, "cast 'banana'");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 50, "no mana spent");
    let messages = drain_text(&mut app);
    assert!(
        messages.iter().any(|line| line.contains("Cast what")),
        "{messages:?}"
    );
}

#[test]
fn insufficient_mana_is_denied_without_deducting() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 3);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);

    cast(&mut app, player, "cast 'magic missile' goblin");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 3, "mana is untouched");
    assert_eq!(combatant_of(&app, goblin).hp, 100, "no damage dealt");
    let messages = drain_text(&mut app);
    assert!(
        messages
            .iter()
            .any(|line| line.contains("haven't the energy")),
        "{messages:?}"
    );
}

#[test]
fn a_class_that_does_not_know_the_spell_is_denied_by_name() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Conan", "warrior", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);

    cast(&mut app, player, "cast 'magic missile' goblin");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 50);
    assert_eq!(combatant_of(&app, goblin).hp, 100);
    let messages = drain_text(&mut app);
    assert!(
        messages
            .iter()
            .any(|line| line.contains("do not know") && line.contains("magic missile")),
        "{messages:?}"
    );
}

#[test]
fn position_below_min_is_denied() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);
    app.world_mut()
        .get_mut::<TbaCombatant>(player)
        .unwrap()
        .position = Position::Resting;

    cast(&mut app, player, "cast 'magic missile' goblin");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 50);
    assert_eq!(combatant_of(&app, goblin).hp, 100);
    let messages = drain_text(&mut app);
    assert!(
        messages.iter().any(|line| line.contains("concentrate")),
        "{messages:?}"
    );
}

#[test]
fn hostile_target_gates_name_self_and_unknown_victims() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);

    // No argument at all.
    cast(&mut app, player, "cast 'magic missile'");
    run_game(&mut app);
    // Itself.
    cast(&mut app, player, "cast 'magic missile' Merlin");
    run_game(&mut app);
    // A target that is not in the room.
    cast(&mut app, player, "cast 'magic missile' dragon");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 50, "nothing was cast");
    let messages = drain_text(&mut app);
    assert!(
        messages
            .iter()
            .any(|line| line.contains("Upon whom should the spell")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|line| line.contains("shouldn't cast that on yourself")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|line| line.contains("don't see dragon")),
        "{messages:?}"
    );
}

// ---------------------------------------------------------------------------
// Damage / heal
// ---------------------------------------------------------------------------

#[test]
fn damage_spell_lowers_target_hp_and_deducts_mana() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);

    // The unquoted form exercises the longest-known-spell split.
    cast(&mut app, player, "cast magic missile goblin");
    run_game(&mut app);

    let damage = 100 - combatant_of(&app, goblin).hp;
    assert!(
        (2..=9).contains(&damage),
        "1d8+1 rolled {damage}, expected 2..=9"
    );
    assert_eq!(combatant_of(&app, player).mana, 40, "10 mana spent");
    let messages = drain_text(&mut app);
    assert!(
        messages.iter().any(|line| line.contains("magic missile")),
        "{messages:?}"
    );
}

#[test]
fn lethal_damage_spell_despawns_an_npc() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 1);

    cast(&mut app, player, "cast 'magic missile' goblin");
    run_game(&mut app);

    assert!(
        app.world().get_entity(goblin).is_err(),
        "a defeated NPC is despawned"
    );
    assert!(drain_defeats(&mut app).is_empty(), "not a player");
}

#[test]
fn lethal_damage_spell_defeats_a_player() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let caster = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let victim = spawn_player(&mut app, room, "Victim", "mage", 0);
    app.world_mut().get_mut::<TbaCombatant>(victim).unwrap().hp = 1;

    cast(&mut app, caster, "cast 'magic missile' Victim");
    run_game(&mut app);

    assert_eq!(drain_defeats(&mut app), vec![victim]);
    let vitals = combatant_of(&app, victim);
    assert_eq!(vitals.hp, 0);
    assert_eq!(vitals.position, Position::Dead);
    assert_eq!(combatant_of(&app, caster).mana, 40);
}

#[test]
fn heal_restores_hp_capped_at_max_and_deducts_mana() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    {
        let mut vitals = app.world_mut().get_mut::<TbaCombatant>(player).unwrap();
        vitals.hp = 1;
        vitals.max_hp = 30;
    }

    // No target: a non-violent character spell defaults to the caster.
    cast(&mut app, player, "cast 'cure light'");
    run_game(&mut app);

    let vitals = combatant_of(&app, player);
    assert!(vitals.hp > 1, "healed: {}", vitals.hp);
    assert!(vitals.hp <= vitals.max_hp, "capped at max: {}", vitals.hp);
    assert_eq!(vitals.mana, 40, "10 mana spent");
}

// ---------------------------------------------------------------------------
// Buffs
// ---------------------------------------------------------------------------

#[test]
fn buff_changes_a_stat_and_expires_after_its_rounds() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let base = combatant_of(&app, player);
    assert_eq!(base.armor_class, NAKED_ARMOR_CLASS);

    cast(&mut app, player, "cast 'armor'");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 35, "15 mana spent");
    assert_eq!(
        combatant_of(&app, player).armor_class,
        base.armor_class - 20,
        "the buff affect reached the recompute"
    );
    assert_eq!(
        app.world().get::<TbaBuffs>(player).unwrap().buffs.len(),
        1,
        "one active buff recorded"
    );

    // Still active after all but the last round...
    for _ in 0..DEFAULT_BUFF_ROUNDS - 1 {
        run_round(&mut app);
        run_game(&mut app);
    }
    assert_eq!(
        combatant_of(&app, player).armor_class,
        base.armor_class - 20,
        "the buff has not expired yet"
    );

    // ...and gone after the final round, without a second remove path.
    run_round(&mut app);
    run_game(&mut app);
    assert_eq!(
        combatant_of(&app, player).armor_class,
        base.armor_class,
        "the recompute rebuilt the base once the buff expired"
    );
    assert!(
        app.world()
            .get::<TbaBuffs>(player)
            .unwrap()
            .buffs
            .is_empty(),
        "the expired buff is dropped"
    );
}

#[test]
fn damaging_spell_with_an_affect_records_the_buff() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);

    cast(&mut app, player, "cast 'chill touch' goblin");
    run_game(&mut app);

    assert!(combatant_of(&app, goblin).hp < 100, "damage landed");
    assert_eq!(
        app.world().get::<TbaBuffs>(goblin).unwrap().buffs.len(),
        1,
        "the strength drain was installed alongside the damage"
    );
}

// ---------------------------------------------------------------------------
// Detect / utility
// ---------------------------------------------------------------------------

#[test]
fn detect_and_utility_spells_spend_mana_and_report() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);

    cast(&mut app, player, "cast 'detect magic'");
    cast(&mut app, player, "cast 'blink'");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 35, "10 + 5 mana spent");
    let messages = drain_text(&mut app);
    assert!(
        messages.iter().any(|line| line.contains("detect magic")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|line| line.contains("blink") && line.contains("nothing happens")),
        "{messages:?}"
    );
}
