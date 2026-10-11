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

/// Synthetic armor duration: the test asserts a buff lasts exactly the spell's
/// declared rounds without looping the catalog's real 24.
const ARMOR_ROUNDS: u32 = 3;
/// Circle's unsaved `chill touch` duration (`tbamud/src/magic.c:326`).
const CHILL_TOUCH_ROUNDS: u32 = 4;

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
            "detect_invis",
            "blink",
            "scaling",
            "hex",
            "fizzle",
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

/// A neutral spell block the other fixtures tweak. The synthetic `mana_min ==
/// mana_max` makes the cost flat, so the pre-#7 assertions keep their meaning;
/// the level-scaling tests use a spell with a real triple.
fn base(name: &str, mana: u32) -> SpellDef {
    SpellDef {
        name: name.to_owned(),
        mana_min: mana,
        mana_max: mana,
        mana_change: 0,
        kind: SpellKind::Detect,
        dice: None,
        level_bonus: LevelBonus::default(),
        affect: None,
        duration: DEFAULT_BUFF_ROUNDS,
        duration_bonus: LevelBonus::default(),
        target: SpellTarget::Victim,
        min_position: Position::Standing,
        save: SaveEffect::None,
        min_level: BTreeMap::new(),
        unsupported: None,
    }
}

fn damage(name: &str, mana: u32, dice: Dice) -> SpellDef {
    SpellDef {
        kind: SpellKind::Damage,
        dice: Some(dice),
        min_position: Position::Fighting,
        ..base(name, mana)
    }
}

fn heal(name: &str, mana: u32, dice: Dice) -> SpellDef {
    SpellDef {
        kind: SpellKind::Heal,
        dice: Some(dice),
        min_position: Position::Fighting,
        ..base(name, mana)
    }
}

fn buff(name: &str, mana: u32, affect: AffectDef, duration: u32) -> SpellDef {
    SpellDef {
        kind: SpellKind::Buff,
        affect: Some(affect),
        duration,
        ..base(name, mana)
    }
}

fn detect(name: &str, mana: u32) -> SpellDef {
    SpellDef {
        kind: SpellKind::Detect,
        target: SpellTarget::Caster,
        ..base(name, mana)
    }
}

fn utility(name: &str, mana: u32, reason: &str) -> SpellDef {
    SpellDef {
        kind: SpellKind::Utility,
        target: SpellTarget::Caster,
        unsupported: Some(reason.to_owned()),
        ..base(name, mana)
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
                duration: CHILL_TOUCH_ROUNDS,
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
                ARMOR_ROUNDS,
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
                DEFAULT_BUFF_ROUNDS,
            ),
        ),
        (SmolStr::new("detect_magic"), detect("detect magic", 10)),
        (
            SmolStr::new("detect_invis"),
            detect("detect invisibility", 10),
        ),
        (
            SmolStr::new("blink"),
            utility("blink", 5, "the blink routine is not modelled"),
        ),
        // A flat-rolling damage spell whose roll scales with the caster level
        // (Circle's `earthquake`, `magic.c:281`).
        (
            SmolStr::new("scaling"),
            SpellDef {
                level_bonus: LevelBonus {
                    per_level: 1,
                    divisor: 1,
                },
                ..damage("scaling", 0, Dice::constant(0))
            },
        ),
        // A hostile affect with a save that negates it (Circle's `blindness`,
        // `magic.c:353-370`).
        (
            SmolStr::new("hex"),
            SpellDef {
                kind: SpellKind::Debuff,
                affect: Some(AffectDef {
                    location: AffectLocation::Hitroll,
                    modifier: -4,
                }),
                duration: 2,
                save: SaveEffect::Negate,
                ..base("hex", 0)
            },
        ),
        // A utility spell whose Circle routine the engine cannot express.
        (
            SmolStr::new("fizzle"),
            utility("fizzle", 5, "the fizzle routine is not modelled"),
        ),
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
    app.init_resource::<VisibilityPolicy>();
    app.add_message::<OutputEvent>();
    app.add_message::<CommandReceived>();
    app.add_message::<PlayerDefeated>();
    app.add_message::<NpcDefeated>();
    app.add_systems(
        Update,
        (
            handle_cast_command,
            recompute_equipment_affects,
            sync_perception,
        )
            .chain(),
    );
    app.init_schedule(TbaRound);
    app.add_systems(TbaRound, (tick_buffs, tick_true_sight));
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

/// Set the caster level the `cast` handler reads for mana/damage scaling.
fn set_level(app: &mut App, entity: Entity, level: u32) {
    app.world_mut()
        .get_mut::<TbaProgress>(entity)
        .unwrap()
        .level = level;
}

/// Force a save outcome: a large negative `SAVING_SPELL` modifier makes the
/// `MAX(1, save)` floor win (save succeeds), a large positive one makes the
/// roll lose (save fails). With the `0xC0FFEE` app seed the first save roll is
/// 33, so both directions are deterministic.
fn set_spell_save(app: &mut App, entity: Entity, modifier: i32) {
    app.world_mut()
        .get_mut::<TbaCombatant>(entity)
        .unwrap()
        .saves
        .spell = modifier;
}

fn cast(app: &mut App, session: Entity, line: &str) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
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

#[test]
fn hidden_caster_can_still_target_itself_by_name() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    app.world_mut().entity_mut(player).insert(TbaHidden);

    // A hidden caster is invisible to others, but its own name still resolves
    // (the helper never returns the actor), so the self-target guard fires
    // rather than "you don't see Merlin".
    cast(&mut app, player, "cast 'magic missile' Merlin");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 50, "nothing was cast");
    let messages = drain_text(&mut app);
    assert!(
        messages
            .iter()
            .any(|line| line.contains("shouldn't cast that on yourself")),
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
fn buff_duration_comes_from_the_spell_and_expires_exactly() {
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
    let buffs = app.world().get::<TbaBuffs>(player).unwrap();
    assert_eq!(buffs.buffs.len(), 1, "one active buff recorded");
    assert_eq!(
        buffs.buffs[0].rounds_left, ARMOR_ROUNDS,
        "the countdown starts at the spell's duration"
    );

    // Still active after all but the last round...
    for _ in 0..ARMOR_ROUNDS - 1 {
        run_round(&mut app);
        run_game(&mut app);
    }
    assert_eq!(
        app.world().get::<TbaBuffs>(player).unwrap().buffs[0].rounds_left,
        1,
        "one round left after the others elapsed"
    );
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
fn active_buffs_round_trip_through_serde() {
    let store = TbaBuffs {
        buffs: vec![ActiveBuff {
            affect: AffectDef {
                location: AffectLocation::Str,
                modifier: -1,
            },
            rounds_left: 3,
        }],
    };
    let json = serde_json::to_string(&store).expect("serialise buffs");
    let back: TbaBuffs = serde_json::from_str(&json).expect("deserialise buffs");
    assert_eq!(back, store, "a restart keeps the effect and its rounds");
    assert!(json.contains("rounds_left"), "{json}");
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
    let buffs = app.world().get::<TbaBuffs>(goblin).unwrap();
    assert_eq!(buffs.buffs.len(), 1, "the strength drain was installed");
    assert_eq!(
        buffs.buffs[0].rounds_left, CHILL_TOUCH_ROUNDS,
        "the damage spell's affect uses its own duration"
    );
}

// ---------------------------------------------------------------------------
// Detect / utility
// ---------------------------------------------------------------------------

/// A true-sight divination (`detect invisibility`) grants the engine
/// `Perception` carrier for the spell's duration; a narrower one
/// (`detect magic`) does not. The grant rides the pack's round-ticked
/// `TbaTrueSight` and expires cleanly.
#[test]
fn detect_invis_grants_true_sight_and_expires() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let seer = spawn_player(&mut app, room, "Seer", "mage", 50);
    let dabbler = spawn_player(&mut app, room, "Dabbler", "mage", 50);

    cast(&mut app, seer, "cast 'detect invis'");
    cast(&mut app, dabbler, "cast 'detect magic'");
    run_game(&mut app);

    assert_eq!(
        app.world().get::<Perception>(seer),
        Some(&Perception {
            sight: SEE_INVIS,
            detect: u8::MAX,
        }),
        "detect invis mirrors TbaTrueSight into Perception"
    );
    assert!(
        app.world().get::<TbaTrueSight>(seer).is_some(),
        "the pack grant is present"
    );
    assert!(
        app.world().get::<Perception>(dabbler).is_none()
            && app.world().get::<TbaTrueSight>(dabbler).is_none(),
        "detect magic is not true sight"
    );

    // `affect_rounds` for a duration-only detect spell is `DEFAULT_BUFF_ROUNDS`.
    for _ in 0..DEFAULT_BUFF_ROUNDS {
        run_round(&mut app);
    }
    assert!(
        app.world().get::<TbaTrueSight>(seer).is_none(),
        "the grant expires"
    );
    run_game(&mut app);
    assert!(
        app.world().get::<Perception>(seer).is_none(),
        "the engine carrier is removed with the grant"
    );
}

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
        messages.iter().any(
            |line| line.contains("blink") && line.contains("the blink routine is not modelled")
        ),
        "{messages:?}"
    );
}

// ---------------------------------------------------------------------------
// Mana scaling (issue #7)
// ---------------------------------------------------------------------------

#[test]
fn mana_cost_scales_with_level_and_clamps_to_the_source_band() {
    // magic_missile's real triple (content/tba.spells.yaml,
    // spell_parser.c:877-878): 25 at level 1, -3 per level, floor 10.
    let spell = SpellDef {
        mana_min: 10,
        mana_max: 25,
        mana_change: 3,
        min_level: BTreeMap::from([(SmolStr::new("mage"), 1)]),
        ..base("magic missile", 0)
    };
    assert_eq!(mana_cost(&spell, "mage", 1), 25, "mana_max at min level");
    assert_eq!(mana_cost(&spell, "mage", 5), 13, "25 - 3*4");
    assert_eq!(mana_cost(&spell, "mage", 6), 10, "reaches the floor");
    assert_eq!(mana_cost(&spell, "mage", 30), 10, "clamps at mana_min");

    // A caster below the class min level is impossible in Circle, but the
    // pack's flat `granted_skills` gate allows it; it must pay `mana_max`.
    let later = SpellDef {
        min_level: BTreeMap::from([(SmolStr::new("mage"), 15)]),
        ..spell.clone()
    };
    assert_eq!(mana_cost(&later, "mage", 1), 25, "clamps at mana_max");
    assert_eq!(mana_cost(&later, "mage", 15), 25);
    assert_eq!(mana_cost(&later, "mage", 21), 10);
}

#[test]
fn a_cast_spends_the_level_scaled_cost() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    spawn_npc(&mut app, room, "goblin", 100);
    // Synthetic magic_missile is flat (10). Give the level-3 caster a real
    // 25 -> 10 triple so the handler demonstrably charges the scaled value.
    {
        let mut content = app.world_mut().resource_mut::<TbaContent>();
        let spell = content.spells.get_mut("magic_missile").unwrap();
        spell.mana_min = 10;
        spell.mana_max = 25;
        spell.mana_change = 3;
        spell.min_level = BTreeMap::from([(SmolStr::new("mage"), 1)]);
    }
    set_level(&mut app, player, 3);

    cast(&mut app, player, "cast 'magic missile' goblin");
    run_game(&mut app);

    // 25 - 3*(3-1) = 19.
    assert_eq!(combatant_of(&app, player).mana, 50 - 19);
}

// ---------------------------------------------------------------------------
// Damage/heal level scaling (issue #7)
// ---------------------------------------------------------------------------

#[test]
fn damage_scales_with_caster_level() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);
    set_level(&mut app, player, 5);

    // `scaling` is a constant 0 roll plus `+ level` (earthquake's term).
    cast(&mut app, player, "cast 'scaling' goblin");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, goblin).hp, 95, "0 + level 5");
}

#[test]
fn heal_scales_with_caster_level() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    {
        let mut vitals = app.world_mut().get_mut::<TbaCombatant>(player).unwrap();
        vitals.hp = 1;
        vitals.max_hp = 200;
    }
    // A heal with a `+ level / 4` term (cure light/critic, magic.c:805-812).
    {
        let mut content = app.world_mut().resource_mut::<TbaContent>();
        let spell = content.spells.get_mut("cure_light").unwrap();
        spell.dice = Some(Dice::constant(0));
        spell.level_bonus = LevelBonus {
            per_level: 1,
            divisor: 4,
        };
    }
    set_level(&mut app, player, 9);

    cast(&mut app, player, "cast 'cure light'");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).hp, 1 + 9 / 4, "0 + 9/4 = 2");
}

// ---------------------------------------------------------------------------
// Saving throws (issue #7)
// ---------------------------------------------------------------------------

#[test]
fn a_successful_save_halves_damage_but_a_failed_save_does_not() {
    // Force save success: the `-1000` modifier floors the throw at 1, below
    // the seed's roll of 33.
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);
    {
        let mut content = app.world_mut().resource_mut::<TbaContent>();
        let spell = content.spells.get_mut("magic_missile").unwrap();
        spell.dice = Some(Dice::constant(10));
        spell.save = SaveEffect::Half;
    }
    set_spell_save(&mut app, goblin, -1000);

    cast(&mut app, player, "cast 'magic missile' goblin");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, goblin).hp, 95, "10 halved to 5");
    assert!(
        drain_text(&mut app)
            .iter()
            .any(|line| line.contains("resist")),
        "the victim is told it resisted"
    );

    // Force save failure: `+1000` beats any roll, so full damage.
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);
    {
        let mut content = app.world_mut().resource_mut::<TbaContent>();
        let spell = content.spells.get_mut("magic_missile").unwrap();
        spell.dice = Some(Dice::constant(10));
        spell.save = SaveEffect::Half;
    }
    set_spell_save(&mut app, goblin, 1000);

    cast(&mut app, player, "cast 'magic missile' goblin");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, goblin).hp, 90, "no save, full 10");
    assert!(
        !drain_text(&mut app)
            .iter()
            .any(|line| line.contains("resist")),
        "nothing resisted"
    );
}

#[test]
fn a_successful_save_negates_a_debuff() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);
    set_spell_save(&mut app, goblin, -1000);

    cast(&mut app, player, "cast 'hex' goblin");
    run_game(&mut app);

    assert!(
        app.world().get::<TbaBuffs>(goblin).is_none(),
        "a negated debuff installs no affect"
    );

    // The same cast without the save lands the affect.
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);
    let goblin = spawn_npc(&mut app, room, "goblin", 100);
    set_spell_save(&mut app, goblin, 1000);

    cast(&mut app, player, "cast 'hex' goblin");
    run_game(&mut app);

    let buffs = app.world().get::<TbaBuffs>(goblin).unwrap();
    assert_eq!(buffs.buffs.len(), 1, "the hex landed");
}

// ---------------------------------------------------------------------------
// Unsupported spells (issue #7)
// ---------------------------------------------------------------------------

#[test]
fn an_unsupported_spell_spends_mana_and_reports_its_reason() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, "Merlin", "mage", 50);

    cast(&mut app, player, "cast 'fizzle'");
    run_game(&mut app);

    assert_eq!(combatant_of(&app, player).mana, 45, "the 5 mana was spent");
    let messages = drain_text(&mut app);
    assert!(
        messages
            .iter()
            .any(|line| line.contains("fizzle")
                && line.contains("the fizzle routine is not modelled")),
        "{messages:?}"
    );
}

#[test]
fn save_type_index_and_tables_match_the_source() {
    // magic_user SPELL level 1 is 60 (class.c, mage SAVING_SPELL row); the
    // modifier from the affect block shifts it.
    assert_eq!(saving_throws("magic_user", SaveType::Spell, 1), 60);
    assert_eq!(saving_throws("cleric", SaveType::Spell, 1), 75);
    assert_eq!(saving_throws("thief", SaveType::Spell, 1), 75);
    assert_eq!(saving_throws("warrior", SaveType::Spell, 1), 85);
    // An unknown class (an NPC) uses the warrior table.
    assert_eq!(saving_throws("goblin", SaveType::Spell, 1), 85);
}

#[test]
fn mag_savingthrow_honours_the_roll_and_the_modifier() {
    // Seed 0xC0FFEE's first 0..99 roll is 33: a low total saves, a high one
    // does not, and the `MAX(1, save)` floor keeps a huge negative modifier
    // from making the throw worse.
    let saves = TbaSaves {
        spell: -1000,
        ..TbaSaves::default()
    };
    let mut rng = TbaRng::from_seed(0xC0FFEE);
    assert!(mag_savingthrow(
        &mut rng,
        "warrior",
        1,
        &saves,
        SaveType::Spell
    ));

    let saves = TbaSaves {
        spell: 1000,
        ..TbaSaves::default()
    };
    let mut rng = TbaRng::from_seed(0xC0FFEE);
    assert!(!mag_savingthrow(
        &mut rng,
        "warrior",
        1,
        &saves,
        SaveType::Spell
    ));
}
