//! DG intake for combat, magic and the world clock: `fight`, `hitprcnt`,
//! `damage`, `death`, `cast`, `reset` and `timer` triggers.
//!
//! Expectations come from tbaMUD's C (`fight.c` `hit` / `damage` / `raw_kill`,
//! `spell_parser.c` `call_magic`, `db.c` `reset_zone`, `limits.c`
//! `point_update`) and `dg-vm`'s dispatcher notes, never from what the code
//! returns. A trigger proves it ran, and with what, by storing its event
//! variables as globals on its owner.
//!
//! Tests may unwrap/expect.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dg_common;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dg_common::*;
use dg_vm::BaseHost;
use dg_vm::types::uid_text;
use mib_content::prelude::{load_packs, registry_for};
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::dg::intake::DgIntake;
use mib_tba::dg::intake::combat::{RefusedCasts, cast_allowed, spell_number};
use mib_tba::items::{ExpiredItemTimers, TbaItemTimer, tick_item_timers};
use mib_tba::prelude::*;
use mib_tba::zones::ZoneResets;
use smol_str::SmolStr;

// -- fixtures ----------------------------------------------------------------

/// The runtime with the pack's combat round, the cast handler behind the DG
/// gate, and the messages those write.
fn combat_app() -> App {
    let mut app = app();
    app.init_resource::<VisibilityPolicy>();
    app.init_resource::<HandledCommands>();
    app.add_message::<CommandReceived>();
    app.add_message::<PlayerDefeated>();
    app.add_message::<NpcDefeated>();
    app.add_message::<TbaDamage>();
    app.init_schedule(TbaRound);
    app.add_systems(TbaRound, resolve_rounds);
    mib_tba::dg::intake::combat::register(&mut app);
    // The engine's router marks every non-genre command handled before the
    // pack's handlers see it, `cast` lines included.
    app.add_systems(
        Update,
        (
            (|mut reader: MessageReader<CommandReceived>, mut handled: ResMut<HandledCommands>| {
                for msg in reader.read() {
                    handled.mark_handled(msg.id);
                }
            })
            .after(DgIntake::PreCommit)
            .before(handle_cast_command),
            handle_cast_command.after(DgIntake::PreCommit),
        ),
    );
    app
}

fn round(app: &mut App) {
    app.world_mut().run_schedule(TbaRound);
    app.world_mut().flush();
}

/// Give an entity combat vitals that hit every time (`hits`) or never, for
/// `damage` points a blow.
fn arm(app: &mut App, entity: Entity, hp: i32, damage: i32, hits: bool) {
    let combatant = TbaCombatant {
        hp,
        max_hp: hp,
        thaco: if hits { -100 } else { 100 },
        ..fallback_combatant()
    };
    app.world_mut()
        .entity_mut(entity)
        .insert((combatant, TbaAttackDice(Dice::constant(damage))));
}

fn fight(app: &mut App, attacker: Entity, target: Entity) {
    app.world_mut()
        .get_mut::<TbaCombatant>(attacker)
        .expect("armed")
        .target = Some(target);
}

fn hp(app: &App, entity: Entity) -> i32 {
    app.world()
        .get::<TbaCombatant>(entity)
        .expect("combatant")
        .hp
}

fn damage_events(app: &mut App) -> Vec<(i32, bool)> {
    app.world_mut()
        .resource_mut::<Messages<TbaDamage>>()
        .drain()
        .map(|event| (event.damage, event.lethal))
        .collect()
}

/// Attach a trigger that exists as a prototype to an entity already in the
/// world.
fn attach_now(app: &mut App, entity: Entity, vnum: i32) {
    let id = uid(app, entity);
    run(app, |host| {
        let proto = host.trig_proto(vnum).expect("trigger defined");
        host.ensure_script(id)
            .expect("script")
            .add_trigger(proto, -1);
    });
}

/// A script that counts its runs in the global `n` and records `actor`.
const COUNT: &str = "global actor\nif %n%\n  eval n %n% + 1\nelse\n  set n 1\nend\nglobal n";

// -- fight and hitprcnt ------------------------------------------------------

#[test]
fn fight_runs_once_per_round_for_a_fighting_npc_with_its_opponent() {
    let mut app = combat_app();
    define(&mut app, 1, "mob", "k", 100, COUNT);
    attach(&mut app, "npc:tba_npc_10", &[1]);
    let hall = room(&mut app, 1);
    let guard = npc(&mut app, hall, 10, "the guard");
    let thief = npc(&mut app, hall, 11, "the thief");
    let bystander = npc(&mut app, hall, 12, "the bystander");
    for entity in [guard, thief, bystander] {
        arm(&mut app, entity, 100, 1, true);
    }
    fight(&mut app, guard, thief);
    fight(&mut app, thief, guard);
    let g = uid(&mut app, guard);
    let t = uid(&mut app, thief);

    round(&mut app);
    assert_eq!(global(&mut app, g, "n").as_deref(), Some("1"));
    assert_eq!(global(&mut app, g, "actor"), Some(uid_text(t)));
    round(&mut app);
    assert_eq!(global(&mut app, g, "n").as_deref(), Some("2"));
}

#[test]
fn fight_does_not_run_for_an_npc_that_is_not_fighting() {
    let mut app = combat_app();
    define(&mut app, 1, "mob", "k", 100, COUNT);
    attach(&mut app, "npc:tba_npc_10", &[1]);
    let hall = room(&mut app, 1);
    let guard = npc(&mut app, hall, 10, "the guard");
    let thief = npc(&mut app, hall, 11, "the thief");
    arm(&mut app, guard, 100, 1, true);
    arm(&mut app, thief, 100, 1, true);
    // The thief attacks the guard, which has no target of its own.
    fight(&mut app, thief, guard);
    let g = uid(&mut app, guard);

    round(&mut app);
    assert_eq!(global(&mut app, g, "n"), None);
    // `damage` starts the victim fighting its attacker (`set_fighting`), so
    // its `hitprcnt` and, from the next round, `fight` triggers apply.
    assert_eq!(
        app.world().get::<TbaCombatant>(guard).unwrap().target,
        Some(thief)
    );
}

#[test]
fn a_victim_already_fighting_keeps_its_opponent() {
    let mut app = combat_app();
    let hall = room(&mut app, 1);
    let guard = npc(&mut app, hall, 10, "the guard");
    let thief = npc(&mut app, hall, 11, "the thief");
    let rat = npc(&mut app, hall, 12, "the rat");
    for entity in [guard, thief, rat] {
        arm(&mut app, entity, 100, 1, true);
    }
    fight(&mut app, thief, guard);
    fight(&mut app, guard, rat);
    round(&mut app);
    assert_eq!(
        app.world().get::<TbaCombatant>(guard).unwrap().target,
        Some(rat)
    );
}

#[test]
fn a_fight_between_characters_in_different_rooms_is_called_off() {
    // `perform_violence` stops a fight whose sides are apart; `hit` abandons
    // a blow whose sides a `fight` script separated.
    let mut app = combat_app();
    let hall = room(&mut app, 1);
    let cellar = room(&mut app, 2);
    let guard = npc(&mut app, hall, 10, "the guard");
    let thief = npc(&mut app, cellar, 11, "the thief");
    arm(&mut app, guard, 100, 5, true);
    arm(&mut app, thief, 100, 5, true);
    fight(&mut app, thief, guard);
    round(&mut app);
    assert_eq!(hp(&app, guard), 100);
    // The blow is abandoned; the mob keeps its target for `pursuit_system`.
    assert_eq!(hp(&app, thief), 100);
    assert!(damage_events(&mut app).is_empty());
}

#[test]
fn a_called_off_fight_stands_the_fighter_down() {
    // `stop_fighting` leaves the character standing, so the AI can engage
    // again; a mob keeps its target for `pursuit_system` to follow or drop.
    let mut app = combat_app();
    let hall = room(&mut app, 1);
    let cellar = room(&mut app, 2);
    let orc = npc(&mut app, hall, 10, "the orc");
    let hero = npc(&mut app, cellar, 11, "the hero");
    arm(&mut app, orc, 100, 5, true);
    arm(&mut app, hero, 100, 5, true);
    // The hero is a stand-in for a player: a character with no `Npc`.
    app.world_mut().entity_mut(hero).remove::<Npc>();
    fight(&mut app, orc, hero);
    fight(&mut app, hero, orc);
    app.world_mut()
        .get_mut::<TbaCombatant>(orc)
        .unwrap()
        .position = Position::Fighting;
    app.world_mut()
        .get_mut::<TbaCombatant>(hero)
        .unwrap()
        .position = Position::Fighting;
    round(&mut app);
    let orc_state = app.world().get::<TbaCombatant>(orc).unwrap();
    assert_eq!(orc_state.target, Some(hero), "the mob's pursuit decides");
    assert_eq!(orc_state.position, Position::Fighting);
    let hero_state = app.world().get::<TbaCombatant>(hero).unwrap();
    assert_eq!(hero_state.target, None);
    assert_eq!(hero_state.position, Position::Standing);
}

#[test]
fn hitprcnt_runs_for_the_victim_once_its_hit_points_fall_to_the_threshold() {
    let mut app = combat_app();
    // Runs when hit points are at most 50% of the maximum.
    define(&mut app, 1, "mob", "l", 50, COUNT);
    attach(&mut app, "npc:tba_npc_10", &[1]);
    let hall = room(&mut app, 1);
    let guard = npc(&mut app, hall, 10, "the guard");
    let thief = npc(&mut app, hall, 11, "the thief");
    arm(&mut app, guard, 100, 1, true);
    arm(&mut app, thief, 100, 25, true);
    fight(&mut app, guard, thief);
    fight(&mut app, thief, guard);
    let g = uid(&mut app, guard);
    let t = uid(&mut app, thief);

    round(&mut app);
    assert_eq!(hp(&app, guard), 75);
    assert_eq!(global(&mut app, g, "n"), None, "75% is above 50");
    round(&mut app);
    assert_eq!(hp(&app, guard), 50);
    assert_eq!(
        global(&mut app, g, "n").as_deref(),
        Some("1"),
        "50% runs it"
    );
    assert_eq!(global(&mut app, g, "actor"), Some(uid_text(t)));
    round(&mut app);
    assert_eq!(hp(&app, guard), 25);
    assert_eq!(global(&mut app, g, "n").as_deref(), Some("2"));
}

// -- damage ------------------------------------------------------------------

/// A goblin that hits (or misses) for 5 and a guard it fights, the guard
/// carrying the `damage` trigger `script`.
fn damage_scene(script: &str, hits: bool) -> (App, Entity, Entity) {
    let mut app = combat_app();
    define(&mut app, 1, "mob", "s", 100, script);
    attach(&mut app, "npc:tba_npc_10", &[1]);
    let hall = room(&mut app, 1);
    let guard = npc(&mut app, hall, 10, "the guard");
    let goblin = npc(&mut app, hall, 11, "the goblin");
    arm(&mut app, guard, 50, 1, true);
    arm(&mut app, goblin, 50, 5, hits);
    fight(&mut app, goblin, guard);
    (app, guard, goblin)
}

#[test]
fn a_damage_trigger_sees_the_blow_before_it_lands() {
    let (mut app, guard, goblin) = damage_scene(
        "global damdealt\nglobal actor\nglobal victim\nglobal attacktype",
        true,
    );
    let g = uid(&mut app, guard);
    let b = uid(&mut app, goblin);

    round(&mut app);
    assert_eq!(global(&mut app, g, "damdealt").as_deref(), Some("5"));
    assert_eq!(global(&mut app, g, "actor"), Some(uid_text(b)));
    assert_eq!(global(&mut app, g, "victim"), Some(uid_text(g)));
    // The pack's blow has no weapon attack type: the C's `TYPE_HIT` slot is
    // past the last spell, which `skill_name` calls UNDEFINED.
    assert_eq!(
        global(&mut app, g, "attacktype").as_deref(),
        Some("UNDEFINED")
    );
    assert_eq!(
        hp(&app, guard),
        45,
        "a script that does not return keeps the damage"
    );
    assert_eq!(damage_events(&mut app), [(5, false)]);
}

#[test]
fn a_damage_trigger_can_change_the_damage() {
    let (mut app, guard, _) = damage_scene("return 2", true);
    round(&mut app);
    assert_eq!(hp(&app, guard), 48);
    assert_eq!(damage_events(&mut app), [(2, false)]);
}

#[test]
fn a_damage_trigger_returning_minus_one_cancels_the_blow() {
    let (mut app, guard, _) = damage_scene("global damdealt\nreturn -1", true);
    let g = uid(&mut app, guard);
    round(&mut app);
    assert_eq!(global(&mut app, g, "damdealt").as_deref(), Some("5"));
    assert_eq!(hp(&app, guard), 50);
    assert!(damage_events(&mut app).is_empty());
}

#[test]
fn a_miss_is_a_blow_of_zero_damage_the_trigger_can_still_raise() {
    // `hit` calls `damage(ch, victim, 0, ..)` on a miss, and the trigger's
    // return value replaces the damage whatever it was.
    let (mut app, guard, _) = damage_scene("global damdealt\nreturn 3", false);
    let g = uid(&mut app, guard);
    round(&mut app);
    assert_eq!(global(&mut app, g, "damdealt").as_deref(), Some("0"));
    assert_eq!(hp(&app, guard), 47);

    let (mut app, guard, _) = damage_scene("global damdealt", false);
    let g = uid(&mut app, guard);
    round(&mut app);
    assert_eq!(global(&mut app, g, "damdealt").as_deref(), Some("0"));
    assert_eq!(hp(&app, guard), 50);
    assert!(damage_events(&mut app).is_empty());
}

#[test]
fn a_negative_damage_other_than_minus_one_does_no_damage() {
    // `damage` clamps what is left to at least 0.
    let (mut app, guard, _) = damage_scene("return -4", true);
    round(&mut app);
    assert_eq!(hp(&app, guard), 50);
}

// -- death -------------------------------------------------------------------

#[test]
fn a_death_trigger_runs_with_the_killer_before_the_npc_is_removed() {
    let mut app = combat_app();
    // The trigger tells its killer it ran: a variable the NPC's own script
    // would lose when the NPC is removed.
    define(
        &mut app,
        1,
        "mob",
        "f",
        100,
        "global actor\nset died 1\nremote died %actor.id%\nreturn 0",
    );
    // `remote` needs a script on the target; this one never runs.
    define(&mut app, 2, "mob", "c", 100, "*");
    attach(&mut app, "npc:tba_npc_10", &[1]);
    attach(&mut app, "npc:tba_npc_11", &[2]);
    let hall = room(&mut app, 1);
    let guard = npc(&mut app, hall, 10, "the guard");
    let goblin = npc(&mut app, hall, 11, "the goblin");
    arm(&mut app, guard, 5, 1, true);
    arm(&mut app, goblin, 50, 5, true);
    fight(&mut app, goblin, guard);
    let b = uid(&mut app, goblin);

    round(&mut app);
    assert_eq!(
        global(&mut app, b, "died").as_deref(),
        Some("1"),
        "the trigger ran for the kill"
    );
    // A `0` return only suppresses the death cry, which the pack lacks; the
    // victim is removed and rewarded for all the same.
    assert!(app.world().get_entity(guard).is_err(), "the guard is gone");
    let defeated: Vec<_> = app
        .world_mut()
        .resource_mut::<Messages<NpcDefeated>>()
        .drain()
        .map(|event| (event.victim, event.killer))
        .collect();
    assert_eq!(defeated, [(guard, goblin)]);
    assert_eq!(damage_events(&mut app), [(5, true)]);
    // Only a script that returned `0` is a death cry the pack could not stop.
    assert_eq!(
        app.world()
            .get_resource::<mib_tba::dg::DgLog>()
            .map_or(0, |log| log.count("death_cry")),
        1
    );
}

#[test]
fn an_ordinary_kill_records_no_unsupported_death_cry() {
    let mut app = combat_app();
    let hall = room(&mut app, 1);
    let guard = npc(&mut app, hall, 10, "the guard");
    let goblin = npc(&mut app, hall, 11, "the goblin");
    arm(&mut app, guard, 5, 1, true);
    arm(&mut app, goblin, 50, 5, true);
    fight(&mut app, goblin, guard);
    round(&mut app);
    assert!(app.world().get_entity(guard).is_err());
    assert_eq!(
        app.world()
            .get_resource::<mib_tba::dg::DgLog>()
            .map_or(0, |log| log.count("death_cry")),
        0
    );
}

#[test]
fn a_dead_character_does_not_strike_back_in_the_same_round() {
    // `perform_violence` handles one fighter at a time: whichever of two
    // fighters who would kill each other acts first kills the other before it
    // can strike. (Resolving both blows from the same state would kill both.)
    let mut app = combat_app();
    let hall = room(&mut app, 1);
    let guard = npc(&mut app, hall, 10, "the guard");
    let goblin = npc(&mut app, hall, 11, "the goblin");
    arm(&mut app, guard, 5, 5, true);
    arm(&mut app, goblin, 5, 5, true);
    fight(&mut app, goblin, guard);
    fight(&mut app, guard, goblin);
    round(&mut app);
    let survivors = [guard, goblin]
        .into_iter()
        .filter(|entity| app.world().get_entity(*entity).is_ok())
        .count();
    assert_eq!(survivors, 1);
}

#[test]
fn a_players_death_runs_only_the_players_own_death_trigger() {
    let mut app = combat_app();
    define(
        &mut app,
        1,
        "mob",
        "f",
        100,
        "global actor\nset died 1\nglobal died",
    );
    // Room and object triggers of any kind exist for other events only: the C
    // calls `death_mtrigger` on the dying character alone.
    define(
        &mut app,
        2,
        "wld",
        "f",
        100,
        "set reset_ran 1\nglobal reset_ran",
    );
    attach(&mut app, "room:tba.1", &[2]);
    let hall = room(&mut app, 1);
    let alice = player(&mut app, hall, "Alice", 5);
    let goblin = npc(&mut app, hall, 11, "the goblin");
    app.world_mut().entity_mut(alice).insert((
        TbaCombatant {
            hp: 3,
            max_hp: 3,
            ..fallback_combatant()
        },
        TbaEquipment::default(),
    ));
    arm(&mut app, goblin, 50, 5, true);
    fight(&mut app, goblin, alice);
    attach_now(&mut app, alice, 1);
    let a = uid(&mut app, alice);
    let h = uid(&mut app, hall);
    let b = uid(&mut app, goblin);

    round(&mut app);
    assert_eq!(global(&mut app, a, "died").as_deref(), Some("1"));
    assert_eq!(global(&mut app, a, "actor"), Some(uid_text(b)));
    assert_eq!(global(&mut app, h, "reset_ran"), None);
    let defeated: Vec<_> = app
        .world_mut()
        .resource_mut::<Messages<PlayerDefeated>>()
        .drain()
        .map(|event| event.player)
        .collect();
    assert_eq!(defeated, [alice]);
    assert!(
        app.world().get_entity(alice).is_ok(),
        "a player is not removed"
    );
}

// -- cast --------------------------------------------------------------------

fn spell(name: &str, mana: u32, target: SpellTarget) -> SpellDef {
    SpellDef {
        name: name.to_owned(),
        mana_min: mana,
        mana_max: mana,
        mana_change: 0,
        kind: SpellKind::Damage,
        dice: Some(Dice::constant(7)),
        level_bonus: LevelBonus::default(),
        affect: None,
        duration: 1,
        duration_bonus: LevelBonus::default(),
        target,
        min_position: Position::Standing,
        save: SaveEffect::None,
        min_level: BTreeMap::new(),
        cures: None,
        unsupported: None,
    }
}

fn mage_content() -> TbaContent {
    let class = TbaClassDef {
        name: "Mage".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 4, 0),
        mana_dice: Some(Dice::new(20, 10, 0)),
        xp_table: Vec::new(),
        thaco_table: Vec::new(),
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: vec![SmolStr::new("magic_missile")],
    };
    TbaContent {
        classes: BTreeMap::from([(SmolStr::new("mage"), class)]),
        spells: BTreeMap::from([(
            SmolStr::new("magic_missile"),
            spell("magic missile", 10, SpellTarget::Victim),
        )]),
        ..TbaContent::default()
    }
}

/// A mage in a hall with a goblin, the cast line typed.
fn cast_scene() -> (App, Entity, Entity, Entity) {
    let mut app = combat_app();
    app.world_mut().insert_resource(mage_content());
    let hall = room(&mut app, 1);
    let mage = player(&mut app, hall, "Merlin", 5);
    let goblin = npc(&mut app, hall, 11, "goblin");
    arm(&mut app, goblin, 40, 1, true);
    let mut vitals = TbaCombatant {
        hp: 20,
        max_hp: 20,
        ..fallback_combatant()
    };
    vitals.mana = 50;
    vitals.max_mana = 200;
    app.world_mut().entity_mut(mage).insert((
        vitals,
        TbaProgress::new("mage"),
        TbaEquipment::default(),
    ));
    (app, hall, mage, goblin)
}

fn type_cast(app: &mut App, mage: Entity, id: u64) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId(id),
            session: mage,
            session_id: 1,
            input: PlayerInput::RawLine("cast 'magic missile' goblin".to_owned()),
        });
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn mana(app: &App, entity: Entity) -> i32 {
    app.world().get::<TbaCombatant>(entity).unwrap().mana
}

const CAST_VARS: &str = "global actor\nglobal victim\nglobal spell\nglobal spellname";

#[test]
fn spell_numbers_are_the_spells_h_slots() {
    assert_eq!(spell_number("magic_missile"), Some(32));
}

#[test]
fn a_cast_with_no_cast_trigger_takes_effect() {
    let (mut app, _, mage, goblin) = cast_scene();
    type_cast(&mut app, mage, 1);
    assert_eq!(hp(&app, goblin), 33);
    assert_eq!(mana(&app, mage), 40);
}

#[test]
fn a_mob_cast_trigger_that_returns_zero_stops_the_spell() {
    let (mut app, _, mage, goblin) = cast_scene();
    define(
        &mut app,
        1,
        "mob",
        "p",
        100,
        &format!("{CAST_VARS}\nreturn 0"),
    );
    attach(&mut app, "npc:tba_npc_11", &[1]);
    let g = uid(&mut app, goblin);
    let m = uid(&mut app, mage);

    type_cast(&mut app, mage, 1);
    assert_eq!(global(&mut app, g, "spell").as_deref(), Some("32"));
    assert_eq!(global(&mut app, g, "actor"), Some(uid_text(m)));
    assert_eq!(hp(&app, goblin), 40, "the spell did not land");
    assert_eq!(mana(&app, mage), 50, "a refused cast costs no mana");
    assert!(
        app.world()
            .resource::<RefusedCasts>()
            .contains(CommandId(1)),
        "the refusal took the command"
    );
    let said = output(&mut app);
    assert!(
        said.iter()
            .any(|line| line.contains("You cast magic missile")),
        "the words are said: {said:?}"
    );
}

#[test]
fn a_cast_the_caster_can_no_longer_afford_never_reaches_the_triggers() {
    // `do_cast` checks mana before `call_magic`: with mana for one cast, the
    // second of two lines in one frame runs no `cast` trigger.
    let (mut app, _, mage, goblin) = cast_scene();
    app.world_mut().get_mut::<TbaCombatant>(mage).unwrap().mana = 10;
    define(
        &mut app,
        1,
        "mob",
        "p",
        100,
        &format!("{CAST_VARS}\n{COUNT}"),
    );
    attach(&mut app, "npc:tba_npc_11", &[1]);
    let g = uid(&mut app, goblin);
    for id in [1, 2] {
        app.world_mut()
            .resource_mut::<Messages<CommandReceived>>()
            .write(CommandReceived {
                id: CommandId(id),
                session: mage,
                session_id: 1,
                input: PlayerInput::RawLine("cast 'magic missile' goblin".to_owned()),
            });
    }
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
    assert_eq!(global(&mut app, g, "n").as_deref(), Some("1"));
}

#[test]
fn a_spell_kill_runs_the_death_trigger_with_the_victim_still_present() {
    let (mut app, _, mage, goblin) = cast_scene();
    app.world_mut().get_mut::<TbaCombatant>(goblin).unwrap().hp = 5;
    define(
        &mut app,
        1,
        "mob",
        "f",
        100,
        "global actor\nset died 1\nremote died %actor.id%\nreturn 0",
    );
    attach(&mut app, "npc:tba_npc_11", &[1]);
    let m = uid(&mut app, mage);
    let _ = uid(&mut app, goblin);

    type_cast(&mut app, mage, 1);
    assert_eq!(
        global(&mut app, m, "died").as_deref(),
        Some("1"),
        "the death trigger ran for the spell's kill"
    );
    assert!(
        app.world().get_entity(goblin).is_err(),
        "then it is removed"
    );
}

#[test]
fn a_room_cast_trigger_that_returns_zero_stops_the_spell() {
    let (mut app, hall, mage, goblin) = cast_scene();
    define(
        &mut app,
        1,
        "wld",
        "p",
        100,
        &format!("{CAST_VARS}\nreturn 0"),
    );
    attach(&mut app, "room:tba.1", &[1]);
    let r = uid(&mut app, hall);
    let g = uid(&mut app, goblin);

    type_cast(&mut app, mage, 1);
    assert_eq!(global(&mut app, r, "spell").as_deref(), Some("32"));
    assert_eq!(global(&mut app, r, "victim"), Some(uid_text(g)));
    assert_eq!(hp(&app, goblin), 40);
    assert_eq!(mana(&app, mage), 50);
}

#[test]
fn a_cast_trigger_that_returns_one_lets_the_spell_through() {
    let (mut app, hall, mage, goblin) = cast_scene();
    define(
        &mut app,
        1,
        "mob",
        "p",
        100,
        &format!("{CAST_VARS}\nreturn 1"),
    );
    define(
        &mut app,
        2,
        "wld",
        "p",
        100,
        "set seen 1\nglobal seen\nreturn 1",
    );
    attach(&mut app, "npc:tba_npc_11", &[1]);
    attach(&mut app, "room:tba.1", &[2]);
    let g = uid(&mut app, goblin);
    let r = uid(&mut app, hall);

    type_cast(&mut app, mage, 1);
    assert_eq!(global(&mut app, r, "seen").as_deref(), Some("1"));
    assert_eq!(global(&mut app, g, "spell").as_deref(), Some("32"));
    assert_eq!(hp(&app, goblin), 33);
    assert_eq!(mana(&app, mage), 40);
    assert!(
        !app.world()
            .resource::<RefusedCasts>()
            .contains(CommandId(1))
    );
}

#[test]
fn a_line_the_handler_would_refuse_never_reaches_the_triggers() {
    let (mut app, _, mage, goblin) = cast_scene();
    define(
        &mut app,
        1,
        "mob",
        "p",
        100,
        "set seen 1\nglobal seen\nreturn 0",
    );
    attach(&mut app, "npc:tba_npc_11", &[1]);
    let g = uid(&mut app, goblin);
    // Not enough mana: `do_cast` refuses before `call_magic`.
    app.world_mut().get_mut::<TbaCombatant>(mage).unwrap().mana = 3;
    type_cast(&mut app, mage, 1);
    assert_eq!(global(&mut app, g, "seen"), None);
    assert_eq!(mana(&app, mage), 3);
    assert!(
        !app.world()
            .resource::<RefusedCasts>()
            .contains(CommandId(1))
    );
}

#[test]
fn an_object_cast_trigger_can_stop_a_spell_aimed_at_the_object() {
    // The pack's `cast` command has no object target; `cast_allowed` is the
    // same three calls `call_magic` makes for any caster.
    let (mut app, hall, mage, _) = cast_scene();
    define(
        &mut app,
        1,
        "obj",
        "p",
        100,
        &format!("{CAST_VARS}\nreturn 0"),
    );
    define(
        &mut app,
        2,
        "obj",
        "p",
        100,
        "set seen 1\nglobal seen\nreturn 1",
    );
    attach(&mut app, "item:tba_item_20", &[1]);
    attach(&mut app, "item:tba_item_21", &[2]);
    let blocking = item_on_ground(&mut app, hall, 20, "a ward");
    let open = item_on_ground(&mut app, hall, 21, "a stone");
    let m = uid(&mut app, mage);
    let blocking_id = uid(&mut app, blocking);
    let open_id = uid(&mut app, open);

    let refused = run(&mut app, |host| {
        cast_allowed(host, m, None, Some(blocking_id), 32)
    });
    assert!(!refused);
    assert_eq!(
        global(&mut app, blocking_id, "spell").as_deref(),
        Some("32")
    );
    assert_eq!(global(&mut app, blocking_id, "actor"), Some(uid_text(m)));

    let allowed = run(&mut app, |host| {
        cast_allowed(host, m, None, Some(open_id), 32)
    });
    assert!(allowed);
    assert_eq!(global(&mut app, open_id, "seen").as_deref(), Some("1"));
}

#[test]
fn the_cast_triggers_run_room_then_object_then_character() {
    // `call_magic` stops at the first refusal, so a refusing room trigger
    // keeps the later ones from running.
    let (mut app, hall, mage, goblin) = cast_scene();
    define(&mut app, 1, "wld", "p", 100, "return 0");
    define(&mut app, 2, "mob", "p", 100, "set seen 1\nglobal seen");
    attach(&mut app, "room:tba.1", &[1]);
    attach(&mut app, "npc:tba_npc_11", &[2]);
    let m = uid(&mut app, mage);
    let g = uid(&mut app, goblin);
    let _ = hall;
    let allowed = run(&mut app, |host| cast_allowed(host, m, Some(g), None, 32));
    assert!(!allowed);
    assert_eq!(global(&mut app, g, "seen"), None);
}

// -- zone reset --------------------------------------------------------------

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mib-tba-dg-intake-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A pack with one NPC prototype, so a zone reset can place it.
fn loaded_content(dir: &Path) -> LoadedContent {
    std::fs::create_dir_all(dir.join("content")).expect("mkdir content");
    std::fs::write(
        dir.join("pack.toml"),
        "id = \"dg-intake-test\"\nversion = \"1\"\ntitle = \"dg intake test\"\n",
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("content/stdlib.npcs.yaml"),
        "npcs:\n  test_mob:\n    name: a test mob\n    persona: A test mob.\n    dialogue: null\n    tags: []\n    behaviors:\n      sentinel: false\n      scavenger: false\n      wander: false\n      wimpy: false\n",
    )
    .expect("write npcs");
    let registry = registry_for(&[&StdlibModule]);
    LoadedContent(load_packs(&[dir.to_path_buf()], &registry).expect("pack loads"))
}

fn zone(bot: u32, top: u32, lifespan_mins: u32) -> ZoneDef {
    ZoneDef {
        name: format!("zone {bot}"),
        bot,
        top,
        lifespan_mins,
        reset_mode: ResetMode::Always,
        commands: vec![ZoneCommand::always(ZoneCommandKind::PlaceMob {
            mob: NpcId::from("test_mob"),
            room: RoomId::from(format!("tba.{bot}")),
            max_in_world: 0,
        })],
        flags: Vec::new(),
    }
}

#[test]
fn a_zone_reset_fires_reset_triggers_for_that_zones_rooms_and_load_for_its_mobs() {
    let dir = temp_dir("reset");
    let mut app = app();
    mib_tba::dg::intake::world::register(&mut app);
    app.add_systems(
        FixedUpdate,
        zone_reset_system.before(mib_tba::dg::intake::world::zone_reset_triggers),
    );
    app.insert_resource(loaded_content(&dir));

    // Zone 1 holds rooms 1..=99 and is due; zone 2 holds 100..=199 and is not.
    let first = room(&mut app, 10);
    let second = room(&mut app, 99);
    let other = room(&mut app, 150);
    let content = TbaContent {
        zones: BTreeMap::from([(1, zone(10, 99, 1)), (2, zone(150, 199, 60))]),
        ..TbaContent::default()
    };
    app.insert_resource(content.clone());
    app.insert_resource(RoomIndex {
        map: HashMap::from([
            (RoomId::from("tba.10"), first),
            (RoomId::from("tba.150"), other),
        ]),
        ..Default::default()
    });
    let mut tracker = ZoneTracker::from_zones(&content.zones);
    tracker.zones[0].age_mins = 100.0;
    app.insert_resource(tracker);

    // `reset` counts its runs per room; the mob's `load` marks it.
    let count = "if %n%\n  eval n %n% + 1\nelse\n  set n 1\nend\nglobal n";
    define(&mut app, 1, "wld", "f", 100, count);
    define(&mut app, 2, "mob", "n", 100, "set loaded 1\nglobal loaded");
    attach(&mut app, "room:tba.10", &[1]);
    attach(&mut app, "room:tba.99", &[1]);
    attach(&mut app, "room:tba.150", &[1]);
    attach(&mut app, "npc:test_mob", &[2]);
    let a = uid(&mut app, first);
    let b = uid(&mut app, second);
    let c = uid(&mut app, other);

    tick_pulses(&mut app, 1);
    assert_eq!(global(&mut app, a, "n").as_deref(), Some("1"));
    assert_eq!(global(&mut app, b, "n").as_deref(), Some("1"));
    assert_eq!(global(&mut app, c, "n"), None, "zone 2 did not reset");

    let mob = app
        .world_mut()
        .query_filtered::<Entity, With<NpcKey>>()
        .iter(app.world())
        .next()
        .expect("the reset placed the mob");
    let m = uid(&mut app, mob);
    assert_eq!(global(&mut app, m, "loaded").as_deref(), Some("1"));

    // The reset list is consumed: the next tick resets nothing.
    assert!(app.world().resource::<ZoneResets>().zones.is_empty());
    tick_pulses(&mut app, 1);
    assert_eq!(global(&mut app, a, "n").as_deref(), Some("1"));
    let _ = std::fs::remove_dir_all(&dir);
}

// -- object timers -----------------------------------------------------------

fn timer_app() -> App {
    let mut app = app();
    mib_tba::dg::intake::world::register(&mut app);
    app.add_systems(
        FixedUpdate,
        tick_item_timers.before(mib_tba::dg::intake::world::item_timer_triggers),
    );
    app
}

/// Advance the world clock one MUD hour.
///
/// Object timers are aged from the shared world clock (D-42), not from the
/// frame cadence, so the test moves the clock itself.
fn next_hour(app: &mut App) {
    app.world_mut().resource_mut::<TimeOfDay>().world_epoch_secs += WORLD_SECONDS_PER_HOUR;
    tick(app, Duration::from_millis(1));
}

#[test]
fn a_timer_trigger_runs_when_the_timer_runs_out_and_the_object_stays() {
    let mut app = timer_app();
    let count = "if %n%\n  eval n %n% + 1\nelse\n  set n 1\nend\nglobal n";
    define(&mut app, 1, "obj", "f", 100, count);
    attach(&mut app, "item:tba_item_20", &[1]);
    let hall = room(&mut app, 1);
    let ice = item_on_ground(&mut app, hall, 20, "an ice cream");
    app.world_mut()
        .entity_mut(ice)
        .insert(TbaItemTimer { remaining: 2 });
    let i = uid(&mut app, ice);

    next_hour(&mut app);
    assert_eq!(global(&mut app, i, "n"), None, "one hour left");
    assert!(app.world().get_entity(ice).is_ok());

    next_hour(&mut app);
    assert_eq!(
        global(&mut app, i, "n").as_deref(),
        Some("1"),
        "ran at zero"
    );
    // `point_update` does nothing after `timer_otrigger`: the object stays
    // (only a script removes it), its timer spent: zero, which is no timer
    // and is what a save keeps.
    assert!(app.world().get_entity(ice).is_ok());
    assert_eq!(
        app.world().get::<TbaItemTimer>(ice).map(|t| t.remaining),
        Some(0)
    );

    next_hour(&mut app);
    next_hour(&mut app);
    assert_eq!(global(&mut app, i, "n").as_deref(), Some("1"), "only once");
    assert!(app.world().resource::<ExpiredItemTimers>().items.is_empty());
}

#[test]
fn an_item_with_no_timer_trigger_still_decays_at_zero() {
    let mut app = timer_app();
    define(&mut app, 1, "obj", "g", 100, "set x 1");
    attach(&mut app, "item:tba_item_20", &[1]);
    let hall = room(&mut app, 1);
    let plain = item_on_ground(&mut app, hall, 20, "a torch");
    app.world_mut()
        .entity_mut(plain)
        .insert(TbaItemTimer { remaining: 1 });
    let _ = uid(&mut app, plain);

    next_hour(&mut app);
    assert!(app.world().get_entity(plain).is_err(), "the pack's decay");
}

#[test]
fn an_item_a_timer_script_gave_a_new_timer_keeps_it() {
    let mut app = timer_app();
    define(&mut app, 1, "obj", "f", 100, "set n 1\nglobal n");
    attach(&mut app, "item:tba_item_20", &[1]);
    let hall = room(&mut app, 1);
    let thing = item_on_ground(&mut app, hall, 20, "a candle");
    app.world_mut()
        .entity_mut(thing)
        .insert(TbaItemTimer { remaining: 1 });
    let _ = uid(&mut app, thing);

    // Stand in for a script that re-arms the item between the tick and the
    // intake that runs it.
    app.add_systems(
        FixedUpdate,
        (|mut timers: Query<&mut TbaItemTimer>| {
            for mut timer in &mut timers {
                if timer.remaining == 0 {
                    timer.remaining = 3;
                }
            }
        })
        .after(tick_item_timers)
        .before(mib_tba::dg::intake::world::item_timer_triggers),
    );
    next_hour(&mut app);
    assert_eq!(
        app.world().get::<TbaItemTimer>(thing).map(|t| t.remaining),
        Some(3)
    );
}

#[test]
fn an_item_expiring_before_its_scripts_attach_still_gets_its_timer_trigger() {
    let mut app = timer_app();
    define(&mut app, 1, "obj", "f", 100, "set n 1\nglobal n");
    define(&mut app, 2, "obj", "g", 100, "set x 1");
    attach(&mut app, "item:tba_item_20", &[1]);
    attach(&mut app, "item:tba_item_21", &[2]);
    let hall = room(&mut app, 1);
    // Neither item has been through `with_host` yet: no script attached.
    let timed = item_on_ground(&mut app, hall, 20, "an ice cream");
    let plain = item_on_ground(&mut app, hall, 21, "a torch");
    for item in [timed, plain] {
        app.world_mut()
            .entity_mut(item)
            .insert(TbaItemTimer { remaining: 1 });
    }

    next_hour(&mut app);
    assert!(app.world().get_entity(timed).is_ok(), "the trigger decides");
    let i = uid(&mut app, timed);
    assert_eq!(global(&mut app, i, "n").as_deref(), Some("1"));
    assert!(app.world().get_entity(plain).is_err(), "no timer trigger");
}
