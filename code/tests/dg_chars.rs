//! Character facts, substitution specials and magic: `CharHost`, `SubstHost`
//! and `MagicHost` for the ECS host.
//!
//! Expectations come from tbaMUD's C (`limits.c`, `spell_parser.c`, `class.c`,
//! `dg_variables.c`) and `crates/dg-vm/HOST.md`, never from what the code
//! returns. Tests do not lean on other streams' stubs: room flags, script
//! commands are not exercised.
//!
//! Tests may unwrap/expect.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dg_common;

use std::path::Path;
use std::sync::Arc;

use bevy::ecs::system::RunSystemOnce;
use dg_common::*;
use dg_vm::fields_char::{CharAbil, CharFact, CharHost, CharStat, char_field};
use dg_vm::host::Cx;
use dg_vm::magic::{MagicHost, SPELL_DG_AFFECT, find_skill_num};
use dg_vm::script::Program;
use dg_vm::subst::SubstHost;
use dg_vm::types::{AttachType, TrigInstance, TrigProto};
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::combat::{
    TbaAlignment, TbaCombatant, TbaGold, TbaNpcBase, TbaProgress, default_class, derive_vitals,
};
use mib_tba::dg::DgLog;
use mib_tba::equipment::recompute_equipment_affects;
use mib_tba::party::TbaFollowing;
use mib_tba::prelude::*;
use mib_tba::spells::{TbaBuffs, tick_buffs};

// -- fixtures ---------------------------------------------------------------

/// An app whose content carries the pack's real classes and spells.
fn pack_app() -> App {
    let mut app = app();
    let content = Path::new(env!("CARGO_MANIFEST_DIR")).join("../content");
    let spells: SpellsFile = serde_yaml::from_str(
        &std::fs::read_to_string(content.join("tba.spells.yaml")).expect("spells file"),
    )
    .expect("spells parse");
    let classes: TbaClassesFile = serde_yaml::from_str(
        &std::fs::read_to_string(content.join("tba.classes.yaml")).expect("classes file"),
    )
    .expect("classes parse");
    let mut tba = app.world_mut().resource_mut::<TbaContent>();
    tba.spells = spells.spells;
    tba.classes = classes.classes;
    tba.skill_levels = classes.skill_levels;
    app
}

/// A player with the combat state a played character has.
fn hero(app: &mut App, room: Entity, name: &str, id: u64, class: &str, level: u32) -> Entity {
    let entity = player(app, room, name, id);
    let content = app.world().resource::<TbaContent>();
    let def = content
        .classes
        .get(class)
        .cloned()
        .unwrap_or_else(default_class);
    let mut combatant = derive_vitals(&def, level);
    combatant.hp = combatant.max_hp;
    combatant.mana = combatant.max_mana;
    combatant.move_points = combatant.max_move_points;
    let mut progress = TbaProgress::new(class);
    progress.level = level;
    app.world_mut()
        .entity_mut(entity)
        .insert((combatant, progress, TbaGold(0), TbaAlignment(0)));
    entity
}

/// An NPC with prototype statistics and a combat block.
fn mob(app: &mut App, room: Entity, vnum: u32, name: &str, level: u32) -> Entity {
    define_npc_level(app, vnum, level);
    let entity = npc(app, room, vnum, name);
    let combatant = derive_vitals(&default_class(), level);
    app.world_mut()
        .entity_mut(entity)
        .insert((combatant.clone(), TbaNpcBase(combatant)));
    entity
}

fn stats_of(app: &mut App, vnum: u32) -> Mut<'_, TbaNpcStats> {
    let key = NpcId::from(format!("tba_npc_{vnum}"));
    let content = app.world_mut().resource_mut::<TbaContent>();
    Mut::map_unchanged(content, |content| {
        content.npc_stats.get_mut(&key).expect("npc stats")
    })
}

/// The frame's derived-stat rebuild.
fn recompute(app: &mut App) {
    app.world_mut()
        .run_system_once(recompute_equipment_affects)
        .expect("recompute");
}

fn count(app: &App, key: &str) -> u64 {
    app.world().resource::<DgLog>().count(key)
}

/// `%<id>.<field>(<sub>)%` through the VM's field table.
fn field(app: &mut App, owner: Uid, c: Uid, name: &str, sub: &str) -> Option<String> {
    let proto = Arc::new(TrigProto {
        vnum: 1,
        name: "fields".to_owned(),
        attach_type: AttachType::Mob,
        trigger_type: 0,
        narg: 0,
        arglist: String::new(),
        program: Arc::new(Program::parse("")),
    });
    let mut trig = TrigInstance::new(1, proto);
    run(app, |host| {
        let mut cx = Cx {
            host,
            owner,
            attach: AttachType::Mob,
            trig: &mut trig,
        };
        char_field(&mut cx, c, name, sub)
    })
}

// -- stats: reads -------------------------------------------------------------

#[test]
fn a_players_stats_come_from_its_components() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    {
        let mut combatant = app.world_mut().get_mut::<TbaCombatant>(alice).unwrap();
        combatant.hp = 11;
        combatant.mana = 3;
        combatant.move_points = 20;
        combatant.hitroll = 2;
        combatant.damroll = 4;
        combatant.saves.para = -1;
        combatant.saves.rod = -2;
        combatant.saves.petri = -3;
        combatant.saves.breath = -4;
        combatant.saves.spell = -5;
        combatant.position = Position::Resting;
    }
    app.world_mut().entity_mut(alice).insert(TbaAlignment(-350));
    let want = app.world().get::<TbaCombatant>(alice).unwrap().clone();
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        assert_eq!(host.char_stat(a, CharStat::Hit), 11);
        assert_eq!(host.char_stat(a, CharStat::MaxHit), want.max_hp);
        assert_eq!(host.char_stat(a, CharStat::Mana), 3);
        assert_eq!(host.char_stat(a, CharStat::MaxMana), want.max_mana);
        assert_eq!(host.char_stat(a, CharStat::Move), 20);
        assert_eq!(host.char_stat(a, CharStat::MaxMove), want.max_move_points);
        assert_eq!(host.char_stat(a, CharStat::Hitroll), 2);
        assert_eq!(host.char_stat(a, CharStat::Damroll), 4);
        assert_eq!(host.char_stat(a, CharStat::SavePara), -1);
        assert_eq!(host.char_stat(a, CharStat::SaveRod), -2);
        assert_eq!(host.char_stat(a, CharStat::SavePetri), -3);
        assert_eq!(host.char_stat(a, CharStat::SaveBreath), -4);
        assert_eq!(host.char_stat(a, CharStat::SaveSpell), -5);
        assert_eq!(host.char_stat(a, CharStat::Align), -350);
        // POS_RESTING; CLASS_WARRIOR.
        assert_eq!(host.char_stat(a, CharStat::Pos), 5);
        assert_eq!(host.char_stat(a, CharStat::Class), 3);
        assert_eq!(host.level(a), 5);
        // Not modelled: neutral.
        assert_eq!(host.char_stat(a, CharStat::Sex), 0);
        assert_eq!(host.char_stat(a, CharStat::WaitState), 0);
        // init_char: not hungry, not thirsty, not drunk, nothing practised.
        assert_eq!(host.char_stat(a, CharStat::Hunger), 24);
        assert_eq!(host.char_stat(a, CharStat::Thirst), 24);
        assert_eq!(host.char_stat(a, CharStat::Drunk), 0);
        assert_eq!(host.char_stat(a, CharStat::Practices), 0);
        assert_eq!(host.char_stat(a, CharStat::QuestPoints), 0);
        assert_eq!(host.char_fact(a, CharFact::Weight), 0);
    });
}

#[test]
fn class_numbers_are_the_cs() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    for (index, class) in ["magic_user", "cleric", "thief", "warrior"]
        .into_iter()
        .enumerate()
    {
        let entity = hero(&mut app, temple, class, 10 + index as u64, class, 1);
        let id = uid(&mut app, entity);
        // CLASS_MAGIC_USER 0, CLASS_CLERIC 1, CLASS_THIEF 2, CLASS_WARRIOR 3.
        run(&mut app, |host| {
            assert_eq!(host.char_stat(id, CharStat::Class), index as i32);
        });
    }
}

#[test]
fn an_npcs_stats_come_from_its_prototype() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3024, "the guard", 12);
    {
        let mut stats = stats_of(&mut app, 3024);
        stats.alignment = 500;
        stats.hitroll = 3;
        stats.damroll = 5;
        stats.gold = 120;
        stats.experience = 900;
        stats.default_position = Position::Sleeping;
    }
    let want = app.world().get::<TbaCombatant>(guard).unwrap().clone();
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        assert_eq!(host.level(g), 12);
        assert_eq!(host.char_stat(g, CharStat::Align), 500);
        assert_eq!(host.char_stat(g, CharStat::MaxHit), want.max_hp);
        assert_eq!(host.char_stat(g, CharStat::Hit), want.hp);
        assert_eq!(host.char_fact(g, CharFact::Gold), 120);
        assert_eq!(host.char_fact(g, CharFact::Exp), 900);
        // GET_CLASS of a mob is 0 (CLASS_MAGIC_USER) from the loader.
        assert_eq!(host.char_stat(g, CharStat::Class), 0);
        // The pack's NPC combat block starts a mob standing (POS_STANDING).
        assert_eq!(host.char_stat(g, CharStat::Pos), 8);
    });
}

#[test]
fn a_despawned_or_unknown_character_reads_as_neutral() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 1, "g", 3);
    let g = uid(&mut app, guard);
    app.world_mut().despawn(guard);
    let sword = item_on_ground(&mut app, temple, 2, "a sword");
    let s = uid(&mut app, sword);
    run(&mut app, |host| {
        for id in [g, s, 424_242] {
            assert_eq!(host.char_stat(id, CharStat::Hit), 0);
            assert_eq!(host.char_fact(id, CharFact::Gold), 0);
            assert_eq!(host.char_abil(id, CharAbil::Str), 0);
            assert_eq!(host.char_real_abil(id, CharAbil::Str), 0);
            assert_eq!(host.char_armor_class(id), 0);
            assert_eq!(host.char_fighting(id), None);
            assert_eq!(host.char_master(id), None);
            assert_eq!(host.char_first_follower(id), None);
            assert!(!host.char_affected_by_spell(id, 1));
            assert_eq!(host.char_skill(id, 134), 0);
            assert!(!host.char_act_flagged(id, 3));
            assert!(!host.char_pref_flagged(id, 1));
            assert_eq!(host.char_title(id), None);
            host.char_set_stat(id, CharStat::Hit, 5);
            host.char_set_level(id, 5);
            host.char_set_real_abil(id, CharAbil::Str, 5);
            host.char_affect_total(id);
            host.char_update_pos(id);
            host.char_gain_exp(id, 10);
            host.char_increase_gold(id, 10);
            host.char_set_skill(id, 134, 5);
            host.char_set_act_flag(id, 3, true);
            host.char_set_short_descr(id, "x");
            host.char_set_title(id, "x");
            host.magic_add_dg_affect(id, SPELL_DG_AFFECT, 1, 1, 1, None);
            host.magic_remove_dg_affects(id, SPELL_DG_AFFECT);
            host.magic_call_magic(id, None, None, 32, 10);
        }
    });
}

// -- stats: writes ------------------------------------------------------------

#[test]
fn the_vital_pools_position_and_alignment_are_written_as_given() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.char_set_stat(a, CharStat::Hit, 7);
        host.char_set_stat(a, CharStat::Mana, 2);
        host.char_set_stat(a, CharStat::Move, 9);
        host.char_set_stat(a, CharStat::Align, 1000);
        host.char_set_stat(a, CharStat::Pos, 6);
        // A position outside the C's table is ignored.
        host.char_set_stat(a, CharStat::Pos, 99);
    });
    recompute(&mut app);
    run(&mut app, |host| {
        assert_eq!(host.char_stat(a, CharStat::Hit), 7);
        assert_eq!(host.char_stat(a, CharStat::Mana), 2);
        assert_eq!(host.char_stat(a, CharStat::Move), 9);
        assert_eq!(host.char_stat(a, CharStat::Align), 1000);
        assert_eq!(host.char_stat(a, CharStat::Pos), 6);
    });
}

#[test]
fn a_write_to_a_derived_stat_survives_the_frames_rebuild() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    let cases = [
        (CharStat::MaxHit, 321),
        (CharStat::MaxMana, 44),
        (CharStat::MaxMove, 250),
        (CharStat::Hitroll, 9),
        (CharStat::Damroll, 8),
        (CharStat::SavePara, -7),
        (CharStat::SaveRod, -6),
        (CharStat::SavePetri, -5),
        (CharStat::SaveBreath, -4),
        (CharStat::SaveSpell, -3),
    ];
    run(&mut app, |host| {
        for (stat, value) in cases {
            host.char_set_stat(a, stat, value);
            assert_eq!(host.char_stat(a, stat), value, "{stat:?} reads back");
        }
    });
    for _ in 0..2 {
        recompute(&mut app);
        run(&mut app, |host| {
            for (stat, value) in cases {
                assert_eq!(host.char_stat(a, stat), value, "{stat:?} after a rebuild");
            }
        });
    }
    // A second write replaces the first rather than adding to it.
    run(&mut app, |host| {
        host.char_set_stat(a, CharStat::MaxHit, 100)
    });
    recompute(&mut app);
    run(&mut app, |host| {
        assert_eq!(host.char_stat(a, CharStat::MaxHit), 100);
        host.char_set_stat(a, CharStat::MaxHit, 321);
    });
    recompute(&mut app);
    run(&mut app, |host| {
        assert_eq!(host.char_stat(a, CharStat::MaxHit), 321);
    });
}

#[test]
fn an_npcs_derived_write_lands_in_its_combat_block_and_its_base() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3024, "the guard", 12);
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.char_set_stat(g, CharStat::MaxHit, 777);
        host.char_set_stat(g, CharStat::Damroll, 6);
        assert_eq!(host.char_stat(g, CharStat::MaxHit), 777);
    });
    assert_eq!(app.world().get::<TbaCombatant>(guard).unwrap().max_hp, 777);
    assert_eq!(app.world().get::<TbaNpcBase>(guard).unwrap().0.max_hp, 777);
    assert_eq!(app.world().get::<TbaNpcBase>(guard).unwrap().0.damroll, 6);
}

#[test]
fn player_only_attributes_are_kept_per_player_and_shared_by_every_npc() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let bob = hero(&mut app, temple, "Bob", 8, "warrior", 5);
    let one = mob(&mut app, temple, 1, "one", 1);
    let two = mob(&mut app, temple, 2, "two", 1);
    let (a, b, m1, m2) = (
        uid(&mut app, alice),
        uid(&mut app, bob),
        uid(&mut app, one),
        uid(&mut app, two),
    );
    let specials = [
        CharStat::Practices,
        CharStat::QuestPoints,
        CharStat::Hunger,
        CharStat::Thirst,
        CharStat::Drunk,
    ];
    run(&mut app, |host| {
        for (n, stat) in specials.into_iter().enumerate() {
            host.char_set_stat(a, stat, 10 + n as i32);
            host.char_set_stat(m1, stat, 20 + n as i32);
        }
        for (n, stat) in specials.into_iter().enumerate() {
            assert_eq!(host.char_stat(a, stat), 10 + n as i32);
            assert_eq!(host.char_stat(m1, stat), 20 + n as i32);
            // Another player is untouched; another mob shares the record.
            assert_ne!(host.char_stat(b, stat), 10 + n as i32);
            assert_eq!(host.char_stat(m2, stat), 20 + n as i32);
        }
        // The shared record starts at zero, even for hunger.
    });
    let mut fresh = pack_app();
    let temple = room(&mut fresh, 1);
    let m = mob(&mut fresh, temple, 1, "one", 1);
    let m = uid(&mut fresh, m);
    run(&mut fresh, |host| {
        assert_eq!(host.char_stat(m, CharStat::Hunger), 0);
        assert_eq!(host.char_stat(m, CharStat::Thirst), 0);
    });
}

#[test]
fn sex_and_the_wait_state_are_unmodelled_and_counted() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.char_set_stat(a, CharStat::Sex, 2);
        host.char_set_stat(a, CharStat::WaitState, 5);
        assert_eq!(host.char_stat(a, CharStat::Sex), 0);
        assert_eq!(host.char_stat(a, CharStat::WaitState), 0);
    });
    assert_eq!(count(&app, "char_set_sex"), 1);
    assert_eq!(count(&app, "char_set_wait_state"), 1);
}

#[test]
fn the_class_is_set_by_its_number() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.char_set_stat(a, CharStat::Class, 1);
        assert_eq!(host.char_stat(a, CharStat::Class), 1);
        host.char_set_stat(a, CharStat::Class, 9);
        assert_eq!(host.char_stat(a, CharStat::Class), 1, "unchanged");
    });
    assert_eq!(
        app.world()
            .get::<TbaProgress>(alice)
            .unwrap()
            .class
            .as_str(),
        "cleric"
    );
    assert_eq!(count(&app, "char_set_class"), 1);
}

#[test]
fn the_level_is_set_for_players_and_logged_for_npcs() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let guard = mob(&mut app, temple, 3, "g", 4);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    run(&mut app, |host| {
        host.char_set_level(a, 12);
        assert_eq!(host.level(a), 12);
        host.char_set_level(g, 40);
        assert_eq!(host.level(g), 4, "a mob's level is its prototype's");
    });
    assert_eq!(count(&app, "char_set_level_npc"), 1);
    let before = app.world().get::<TbaCombatant>(alice).unwrap().max_hp;
    recompute(&mut app);
    let after = app.world().get::<TbaCombatant>(alice).unwrap().max_hp;
    assert!(after > before, "the next rebuild uses the new level");
}

// -- abilities, armour class, position -------------------------------------

#[test]
fn abilities_read_the_affected_value_and_the_real_one() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    let all = [
        CharAbil::Str,
        CharAbil::Int,
        CharAbil::Wis,
        CharAbil::Dex,
        CharAbil::Con,
        CharAbil::Cha,
    ];
    run(&mut app, |host| {
        for abil in all {
            assert_eq!(host.char_abil(a, abil), 13);
            assert_eq!(host.char_real_abil(a, abil), 13);
        }
        assert_eq!(host.char_abil(a, CharAbil::StrAdd), 0);
        assert_eq!(host.char_real_abil(a, CharAbil::StrAdd), 0);
        // APPLY_STR +2, a day long.
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 1, 2, 24, None);
    });
    recompute(&mut app);
    run(&mut app, |host| {
        assert_eq!(host.char_abil(a, CharAbil::Str), 15);
        assert_eq!(host.char_real_abil(a, CharAbil::Str), 13, "without affects");
        host.char_set_real_abil(a, CharAbil::Str, 16);
        host.char_set_real_abil(a, CharAbil::Wis, 9);
    });
    recompute(&mut app);
    run(&mut app, |host| {
        assert_eq!(host.char_real_abil(a, CharAbil::Str), 16);
        assert_eq!(host.char_abil(a, CharAbil::Str), 18, "affect on top");
        assert_eq!(host.char_real_abil(a, CharAbil::Wis), 9);
        assert_eq!(host.char_abil(a, CharAbil::Wis), 9);
        host.char_set_real_abil(a, CharAbil::StrAdd, 50);
    });
    assert_eq!(count(&app, "char_set_str_add"), 1);
}

#[test]
fn an_npcs_real_ability_write_moves_the_affected_one_with_it() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "g", 4);
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        assert_eq!(host.char_real_abil(g, CharAbil::Dex), 13);
        host.char_set_real_abil(g, CharAbil::Dex, 18);
        assert_eq!(host.char_real_abil(g, CharAbil::Dex), 18);
        assert_eq!(host.char_abil(g, CharAbil::Dex), 18);
    });
}

#[test]
fn armor_class_adds_the_dexterity_bonus_while_awake() {
    // compute_armor_class (class.c/utils): GET_AC + dex_app[GET_DEX].defensive * 10
    // when AWAKE, floored at -100. dex_app[16].defensive is -2.
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    {
        let mut combatant = app.world_mut().get_mut::<TbaCombatant>(alice).unwrap();
        combatant.armor_class = 40;
        combatant.attributes.dexterity = 16;
    }
    run(&mut app, |host| assert_eq!(host.char_armor_class(a), 20));
    app.world_mut()
        .get_mut::<TbaCombatant>(alice)
        .unwrap()
        .position = Position::Sleeping;
    run(&mut app, |host| assert_eq!(host.char_armor_class(a), 40));
    {
        let mut combatant = app.world_mut().get_mut::<TbaCombatant>(alice).unwrap();
        combatant.position = Position::Standing;
        combatant.armor_class = -95;
    }
    run(&mut app, |host| assert_eq!(host.char_armor_class(a), -100));
}

#[test]
fn update_pos_follows_the_hit_points() {
    // update_pos (fight.c): > 0 and not stunned or worse stays; > 0 from
    // stunned or worse stands. The pack has no stunned, incapacitated or
    // mortally wounded state a character can be left in (its rules clamp hit
    // points at zero and never bleed a downed character), so every count at
    // or below zero is POS_DEAD, as its own damage does at zero.
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    let expect = [
        (5, Position::Sitting, 6),
        (0, Position::Sitting, 0),
        (-1, Position::Sitting, 0),
        (-3, Position::Sitting, 0),
        (-6, Position::Sitting, 0),
        (-11, Position::Sitting, 0),
        (5, Position::Dead, 8),
        (5, Position::Stunned, 8),
    ];
    for (hp, from, pos) in expect {
        {
            let mut combatant = app.world_mut().get_mut::<TbaCombatant>(alice).unwrap();
            combatant.hp = hp;
            combatant.position = from;
        }
        run(&mut app, |host| {
            host.char_update_pos(a);
            assert_eq!(host.char_stat(a, CharStat::Pos), pos, "hp {hp}");
        });
    }
}

#[test]
fn affect_total_rebuilds_now() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 19, 3, -1, None);
        // The affect is already in effect: no frame has run.
        assert_eq!(host.char_stat(a, CharStat::Damroll), 3);
        host.char_affect_total(a);
        assert_eq!(host.char_stat(a, CharStat::Damroll), 3);
    });
}

// -- gold and experience ---------------------------------------------------

#[test]
fn gold_is_clamped_like_increase_gold() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.char_increase_gold(a, 50);
        assert_eq!(host.char_fact(a, CharFact::Gold), 50);
        host.char_increase_gold(a, -20);
        assert_eq!(host.char_fact(a, CharFact::Gold), 30);
        host.char_increase_gold(a, -1_000);
        assert_eq!(host.char_fact(a, CharFact::Gold), 0, "never below zero");
        host.char_increase_gold(a, i32::MAX);
        assert_eq!(host.char_fact(a, CharFact::Gold), 2_140_000_000, "MAX_GOLD");
        host.char_increase_gold(a, i32::MAX);
        assert_eq!(host.char_fact(a, CharFact::Gold), 2_140_000_000);
        host.char_increase_gold(a, i32::MIN);
        assert_eq!(host.char_fact(a, CharFact::Gold), 0);
    });
    assert_eq!(app.world().get::<TbaGold>(alice).unwrap().0, 0);
}

#[test]
fn an_npcs_gold_starts_from_its_prototype() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "g", 4);
    let other = mob(&mut app, temple, 4, "h", 4);
    stats_of(&mut app, 3).gold = 40;
    stats_of(&mut app, 4).gold = 40;
    let (g, h) = (uid(&mut app, guard), uid(&mut app, other));
    run(&mut app, |host| {
        assert_eq!(host.char_fact(g, CharFact::Gold), 40);
        host.char_increase_gold(g, 15);
        assert_eq!(host.char_fact(g, CharFact::Gold), 55);
        assert_eq!(host.char_fact(h, CharFact::Gold), 40, "per mob");
        host.char_increase_gold(g, -100);
        assert_eq!(host.char_fact(g, CharFact::Gold), 0);
    });
}

#[test]
fn experience_gains_are_capped_and_level_the_player() {
    // gain_exp: gain capped at 100000 per call; the level rises while the
    // class's level_exp for the next level is reached (warrior: 2000 for
    // level 2, 4000 for 3).
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 1);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.char_gain_exp(a, 1999);
        assert_eq!(host.level(a), 1);
        assert_eq!(host.char_fact(a, CharFact::Exp), 1999);
        host.char_gain_exp(a, 1);
        assert_eq!(host.level(a), 2);
        host.char_gain_exp(a, 6000);
        // 8000 total: level 4 needs 8000.
        assert_eq!(host.level(a), 4);
        host.char_gain_exp(a, i32::MAX);
        assert_eq!(host.char_fact(a, CharFact::Exp), 8000 + 100_000);
        // 108000 total: level 7 needs 64000, level 8 needs 125000.
        assert_eq!(host.level(a), 7);
        // Losses are capped at 500000 per call and floor at zero.
        host.char_gain_exp(a, -50);
        assert_eq!(host.char_fact(a, CharFact::Exp), 108_000 - 50);
        host.char_gain_exp(a, i32::MIN);
        assert_eq!(host.char_fact(a, CharFact::Exp), 0);
        assert_eq!(host.level(a), 7, "a loss never lowers the level");
    });
    let text = output(&mut app).join("\n");
    assert!(text.contains("You rise a level!"), "{text}");
}

#[test]
fn experience_levels_several_at_once_and_leaves_immortals_alone() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 1);
    let god = hero(&mut app, temple, "God", 8, "warrior", 31);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, god));
    run(&mut app, |host| {
        host.char_gain_exp(a, 8000);
        assert_eq!(host.level(a), 4);
        host.char_gain_exp(g, 5000);
        assert_eq!(host.char_fact(g, CharFact::Exp), 0, "level >= LVL_IMMORT");
        assert_eq!(host.level(g), 31);
    });
    assert!(output(&mut app).join("\n").contains("You rise 3 levels!"));
}

#[test]
fn an_npcs_experience_just_changes() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "g", 4);
    stats_of(&mut app, 3).experience = 100;
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.char_gain_exp(g, 250_000);
        assert_eq!(
            host.char_fact(g, CharFact::Exp),
            250_100,
            "no cap for a mob"
        );
        host.char_gain_exp(g, -300_000);
        assert_eq!(host.char_fact(g, CharFact::Exp), -49_900, "no floor either");
    });
}

// -- fighting and following ------------------------------------------------

#[test]
fn fighting_reads_the_combat_target_while_it_lives() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let guard = mob(&mut app, temple, 3, "g", 4);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    run(&mut app, |host| assert_eq!(host.char_fighting(a), None));
    app.world_mut()
        .get_mut::<TbaCombatant>(alice)
        .unwrap()
        .target = Some(guard);
    run(&mut app, |host| assert_eq!(host.char_fighting(a), Some(g)));
    app.world_mut()
        .get_mut::<TbaCombatant>(guard)
        .unwrap()
        .position = Position::Dead;
    run(&mut app, |host| assert_eq!(host.char_fighting(a), None));
    app.world_mut()
        .get_mut::<TbaCombatant>(guard)
        .unwrap()
        .position = Position::Standing;
    app.world_mut().despawn(guard);
    run(&mut app, |host| assert_eq!(host.char_fighting(a), None));
}

#[test]
fn master_and_followers_follow_the_party_model() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let leader = hero(&mut app, temple, "Leader", 7, "warrior", 5);
    let first = hero(&mut app, temple, "First", 8, "warrior", 5);
    let second = hero(&mut app, temple, "Second", 9, "warrior", 5);
    let (l, f, s) = (
        uid(&mut app, leader),
        uid(&mut app, first),
        uid(&mut app, second),
    );
    run(&mut app, |host| {
        assert_eq!(host.char_master(f), None);
        assert_eq!(host.char_first_follower(l), None);
    });
    app.world_mut().entity_mut(first).insert(TbaFollowing {
        leader,
        last_room: temple,
    });
    app.world_mut().clear_trackers();
    app.world_mut().entity_mut(second).insert(TbaFollowing {
        leader,
        last_room: temple,
    });
    run(&mut app, |host| {
        assert_eq!(host.char_master(f), Some(l));
        assert_eq!(host.char_master(s), Some(l));
        // add_follower puts the new follower at the head of the list.
        assert_eq!(host.char_first_follower(l), Some(s));
        assert_eq!(host.char_first_follower(f), None);
    });
    app.world_mut().despawn(second);
    run(&mut app, |host| {
        assert_eq!(host.char_first_follower(l), Some(f));
    });
}

// -- skills ---------------------------------------------------------------

#[test]
fn a_class_skill_is_fully_learned_and_others_are_not() {
    // SKILL_KICK 134 and SKILL_BASH 132 (warrior 1 and 12 in skill_levels),
    // SPELL_MAGIC_MISSILE 32 and SPELL_FIREBALL 26 (magic user grants).
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let warrior = hero(&mut app, temple, "Wally", 7, "warrior", 5);
    let mage = hero(&mut app, temple, "Merlin", 8, "magic_user", 5);
    let (w, m) = (uid(&mut app, warrior), uid(&mut app, mage));
    run(&mut app, |host| {
        assert_eq!(host.char_skill(w, 134), 100);
        assert_eq!(host.char_skill(w, 132), 0, "bash needs level 12");
        assert_eq!(host.char_skill(w, 138), 0, "sneak is a thief skill");
        assert_eq!(host.char_skill(w, 32), 0);
        assert_eq!(host.char_skill(m, 32), 100);
        assert_eq!(host.char_skill(m, 26), 100);
        assert_eq!(host.char_skill(m, 134), 0);
        assert_eq!(host.char_skill(m, 0), 0);
        assert_eq!(host.char_skill(m, -5), 0);
        assert_eq!(host.char_skill(m, 100_000), 0);
        host.char_set_level(w, 12);
        assert_eq!(host.char_skill(w, 132), 100);
    });
}

#[test]
fn set_skill_wins_over_the_grant_and_npcs_share_one_record() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let warrior = hero(&mut app, temple, "Wally", 7, "warrior", 5);
    let one = mob(&mut app, temple, 1, "one", 1);
    let two = mob(&mut app, temple, 2, "two", 1);
    let (w, m1, m2) = (
        uid(&mut app, warrior),
        uid(&mut app, one),
        uid(&mut app, two),
    );
    run(&mut app, |host| {
        host.char_set_skill(w, 134, 40);
        host.char_set_skill(w, 138, 77);
        assert_eq!(host.char_skill(w, 134), 40);
        assert_eq!(host.char_skill(w, 138), 77);
        assert_eq!(host.char_skill(m1, 134), 0);
        host.char_set_skill(m1, 134, 9);
        assert_eq!(host.char_skill(m2, 134), 9, "dummy_mob is shared");
        assert_eq!(host.char_skill(w, 134), 40);
    });
}

// -- act, preference and affect flags ---------------------------------------

#[test]
fn npc_act_flags_come_from_the_prototype_and_the_engine() {
    // MOB_SENTINEL 1, MOB_AGGRESSIVE 5, MOB_ISNPC 3, MOB_NOKILL 18, MOB_WIMPY 7.
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "g", 4);
    stats_of(&mut app, 3).behaviour = vec![NpcBehaviour::Aggressive, NpcBehaviour::NoKill];
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        assert!(host.char_act_flagged(g, 3), "MOB_ISNPC");
        assert!(host.char_act_flagged(g, 5));
        assert!(host.char_act_flagged(g, 18));
        assert!(!host.char_act_flagged(g, 1));
        assert!(!host.char_act_flagged(g, 7));
    });
}

#[test]
fn set_act_flag_is_read_back_and_drives_the_engine_flags() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "g", 4);
    stats_of(&mut app, 3).behaviour = vec![NpcBehaviour::Aggressive];
    app.world_mut()
        .entity_mut(guard)
        .insert(NpcBehaviors::default());
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.char_set_act_flag(g, 1, true);
        assert!(host.char_act_flagged(g, 1));
        host.char_set_act_flag(g, 5, false);
        assert!(!host.char_act_flagged(g, 5), "cleared over the prototype");
        host.char_set_act_flag(g, 2, true);
        host.char_set_act_flag(g, 7, true);
        assert!(host.char_act_flagged(g, 2));
        host.char_set_act_flag(g, 7, false);
        assert!(!host.char_act_flagged(g, 7));
    });
    let flags = app.world().get::<NpcBehaviors>(guard).unwrap();
    assert!(flags.0.sentinel, "MOB_SENTINEL reaches the engine");
    assert!(flags.0.scavenger, "MOB_SCAVENGER reaches the engine");
    assert!(!flags.0.wimpy);
    // Bit 5 reads nothing, and neither do the sentinel flag of a mob that does
    // not wander (the engine's wander system is its only reader) or the
    // wimpy flag (set and cleared).
    assert_eq!(count(&app, "char_act_flag_inert"), 4);

    // Setting and clearing an unrelated bit is stored but inert.
    run(&mut app, |host| {
        host.char_set_act_flag(g, 16, true);
        assert!(host.char_act_flagged(g, 16));
    });
    assert_eq!(count(&app, "char_act_flag_inert"), 5);
}

#[test]
fn a_player_has_only_the_act_flags_a_script_set_and_no_preferences() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        // Bit 3 is MOB_ISNPC only for an NPC; bit 5 is a plain player bit.
        assert!(!host.char_act_flagged(a, 3));
        assert!(!host.char_act_flagged(a, 5));
        host.char_set_act_flag(a, 5, true);
        assert!(host.char_act_flagged(a, 5));
        host.char_set_act_flag(a, 5, false);
        assert!(!host.char_act_flagged(a, 5));
        for bit in 0..40 {
            assert!(!host.char_pref_flagged(a, bit));
        }
    });
}

// -- names, titles, quests ----------------------------------------------------

#[test]
fn title_and_short_description() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let guard = mob(&mut app, temple, 3, "the guard", 4);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    run(&mut app, |host| {
        assert_eq!(host.char_title(a), None, "GET_TITLE is NULL until set");
        host.char_set_title(a, "the Brave");
        assert_eq!(host.char_title(a).as_deref(), Some("the Brave"));
        host.char_set_title(a, "");
        assert_eq!(host.char_title(a).as_deref(), Some(""));

        assert_eq!(host.name(g), "the guard");
        host.char_set_short_descr(g, "a tired guard");
        assert_eq!(host.name(g), "a tired guard");
        host.char_set_short_descr(a, "ignored");
        assert_eq!(host.name(a), "Alice", "a player has no short description");
    });
}

#[test]
fn the_pack_has_no_autoquests() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        assert_eq!(host.char_quest(a), -1);
        assert!(!host.char_quest_exists(10_000));
        assert!(!host.char_quest_completed(a, 10_000));
    });
}

// -- through the field table ----------------------------------------------------

#[test]
fn the_field_table_reads_and_writes_through_the_host() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let guard = mob(&mut app, temple, 3, "the guard", 4);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    let want_hp = app.world().get::<TbaCombatant>(alice).unwrap().hp;
    let f = |app: &mut App, name: &str, sub: &str| field(app, g, a, name, sub);
    assert_eq!(f(&mut app, "hitp", "").unwrap(), want_hp.to_string());
    assert_eq!(f(&mut app, "gold", "").unwrap(), "0");
    assert_eq!(f(&mut app, "gold", "25").unwrap(), "25");
    assert_eq!(f(&mut app, "gold", "-5").unwrap(), "20");
    assert_eq!(f(&mut app, "level", "").unwrap(), "5");
    assert_eq!(f(&mut app, "exp", "").unwrap(), "0");
    assert_eq!(f(&mut app, "skill", "kick").unwrap(), "100");
    assert_eq!(f(&mut app, "skill", "magic missile").unwrap(), "0");
    assert_eq!(f(&mut app, "skillset", "kick 30").unwrap(), "");
    assert_eq!(f(&mut app, "skill", "kick").unwrap(), "30");
    assert_eq!(f(&mut app, "affect", "armor").unwrap(), "0");
    assert_eq!(f(&mut app, "maxhitp", "").unwrap(), {
        app.world()
            .get::<TbaCombatant>(alice)
            .unwrap()
            .max_hp
            .to_string()
    });
    // damroll(n) sets and returns the value; the rebuild keeps it.
    assert_eq!(f(&mut app, "damroll", "11").unwrap(), "11");
    recompute(&mut app);
    assert_eq!(f(&mut app, "damroll", "").unwrap(), "11");
    assert_eq!(f(&mut app, "prac", "3").unwrap(), "3");
    assert_eq!(f(&mut app, "title", "").unwrap(), "(null)");
    assert_eq!(f(&mut app, "title", "the Wise").unwrap(), "the Wise");
}

// -- SubstHost ---------------------------------------------------------------------

#[test]
fn substitution_specials_are_the_cs_defaults() {
    // happy_data is {0, 0, 0, 0} until the hh command runs; CONFIG_DIAGONAL_DIRS
    // is NO.
    let mut app = pack_app();
    run(&mut app, |host| {
        let happy = host.subst_happy_hour();
        assert_eq!(
            (
                happy.qp_rate,
                happy.exp_rate,
                happy.gold_rate,
                happy.ticks_left
            ),
            (0, 0, 0, 0)
        );
        assert!(!happy.is_active());
        assert!(!host.subst_diagonal_dirs());
    });
}

// -- MagicHost: the spell table ---------------------------------------------------

#[test]
fn spell_names_use_the_cs_numbers() {
    let mut app = pack_app();
    run(&mut app, |host| {
        // spells.h numbers, spell_parser.c names.
        for (number, name) in [
            (1, "armor"),
            (7, "charm person"),
            (19, "detect invisibility"),
            (26, "fireball"),
            (32, "magic missile"),
            (34, "protection from evil"),
            (42, "word of recall"),
            (53, "fly"),
            (54, "darkness"),
            (SPELL_DG_AFFECT, "Script-inflicted"),
        ] {
            assert_eq!(host.magic_spell_name(number).as_deref(), Some(name));
        }
        // No spello() in the C for these: unused slots.
        assert_eq!(host.magic_spell_name(41), None, "ventriloquate");
        assert_eq!(host.magic_spell_name(49), None, "group recall");
        assert_eq!(host.magic_spell_name(0), None);
        assert_eq!(host.magic_spell_name(55), None);
        assert_eq!(host.magic_spell_name(299), None);
        assert_eq!(host.magic_spell_name(-3), None);
        // Skills the classes grant are named; ones the pack lacks are not.
        assert_eq!(host.magic_spell_name(134).as_deref(), Some("kick"));
        assert_eq!(host.magic_spell_name(135).as_deref(), Some("pick lock"));
        assert_eq!(host.magic_spell_name(141), None, "bandage");
    });
}

#[test]
fn spell_names_are_undefined_without_content() {
    let mut app = app();
    run(&mut app, |host| {
        assert_eq!(host.magic_spell_name(32), None);
        assert_eq!(host.magic_spell_targets(32), 0);
        assert_eq!(host.magic_spell_routines(32), 0);
        assert_eq!(
            host.magic_spell_name(SPELL_DG_AFFECT).as_deref(),
            Some("Script-inflicted")
        );
    });
}

#[test]
fn find_skill_num_matches_abbreviations_as_the_c_does() {
    let mut app = pack_app();
    run(&mut app, |host| {
        assert_eq!(find_skill_num(host, "magic missile"), 32);
        assert_eq!(find_skill_num(host, "magic mis"), 32);
        assert_eq!(find_skill_num(host, "m m"), 32);
        assert_eq!(find_skill_num(host, "fireb"), 26);
        // The lowest number an abbreviation fits wins: "armor" before
        // "animate dead"'s 45, "detect a" is alignment, "detect i" invisibility.
        assert_eq!(find_skill_num(host, "a"), 1);
        assert_eq!(find_skill_num(host, "detect a"), 18);
        assert_eq!(find_skill_num(host, "detect inv"), 19);
        assert_eq!(find_skill_num(host, "detect m"), 20);
        assert_eq!(find_skill_num(host, "script"), SPELL_DG_AFFECT);
        assert_eq!(find_skill_num(host, "kick"), 134);
        assert_eq!(find_skill_num(host, "pick l"), 135);
        assert_eq!(find_skill_num(host, "ventriloquate"), -1);
        assert_eq!(find_skill_num(host, "nonsense"), -1);
    });
}

#[test]
fn spell_targets_and_routines_are_the_cs_bits() {
    // TAR_*: IGNORE 1, CHAR_ROOM 2, CHAR_WORLD 4, FIGHT_VICT 16, SELF_ONLY 32,
    // NOT_SELF 64, OBJ_INV 128, OBJ_ROOM 256, OBJ_WORLD 512, OBJ_EQUIP 1024.
    // MAG_*: DAMAGE 1, AFFECTS 2, UNAFFECTS 4, POINTS 8, ALTER_OBJS 16,
    // GROUPS 32, MASSES 64, AREAS 128, SUMMONS 256, CREATIONS 512, MANUAL 1024,
    // ROOMS 2048.
    let mut app = pack_app();
    run(&mut app, |host| {
        let cases: [(i32, u32, u32); 12] = [
            (1, 2, 2),
            (3, 2 | 128, 2 | 16),
            (4, 2 | 64, 2),
            (5, 2 | 16, 1),
            (8, 2 | 16, 1 | 2),
            (23, 1, 128),
            (28, 2, 8 | 4),
            (31, 512, 1024),
            (40, 4 | 64, 1024),
            (47, 1, 32),
            (54, 1, 2048),
            (SPELL_DG_AFFECT, 1, 0),
        ];
        for (number, targets, routines) in cases {
            assert_eq!(
                host.magic_spell_targets(number),
                targets,
                "targets {number}"
            );
            assert_eq!(
                host.magic_spell_routines(number),
                routines,
                "routines {number}"
            );
        }
        assert_eq!(host.magic_spell_targets(41), 0);
        assert_eq!(host.magic_spell_routines(41), 0);
        assert_eq!(host.magic_spell_targets(-1), 0);
        // MAG_GROUPS is the bit the VM reads to refuse a group spell.
        assert_ne!(host.magic_spell_routines(48) & (1 << 5), 0);
    });
}

// -- MagicHost: casting -------------------------------------------------------------

#[test]
fn a_cast_damage_spell_hurts_its_victim() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let troll = mob(&mut app, temple, 3, "a troll", 20);
    let (a, t) = (uid(&mut app, alice), uid(&mut app, troll));
    let before = app.world().get::<TbaCombatant>(troll).unwrap().hp;
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(t), None, 32, 10)
    });
    let after = app.world().get::<TbaCombatant>(troll).unwrap().hp;
    assert!(
        after < before,
        "magic missile did damage: {before} -> {after}"
    );
    // The caster needs no mana for a script cast.
    let mana = app.world().get::<TbaCombatant>(alice).unwrap();
    assert_eq!(mana.mana, mana.max_mana);
}

#[test]
fn a_cast_buff_counts_on_a_player_and_is_seen_by_affected_by_spell() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "cleric", 10);
    let bob = hero(&mut app, temple, "Bob", 8, "warrior", 10);
    let (a, b) = (uid(&mut app, alice), uid(&mut app, bob));
    recompute(&mut app);
    let ac_before = app.world().get::<TbaCombatant>(bob).unwrap().armor_class;
    run(&mut app, |host| {
        assert!(!host.char_affected_by_spell(b, 1));
        host.magic_call_magic(a, Some(b), None, 1, 10);
        assert!(host.char_affected_by_spell(b, 1));
        assert!(!host.char_affected_by_spell(a, 1));
    });
    recompute(&mut app);
    let ac_after = app.world().get::<TbaCombatant>(bob).unwrap().armor_class;
    assert!(ac_after < ac_before, "armor improves the armour class");
    // affect_from_char(ch, SPELL_ARMOR) takes it off again.
    run(&mut app, |host| {
        host.magic_remove_dg_affects(b, 1);
        assert!(!host.char_affected_by_spell(b, 1));
    });
    recompute(&mut app);
    assert_eq!(
        app.world().get::<TbaCombatant>(bob).unwrap().armor_class,
        ac_before
    );
}

#[test]
fn a_cast_with_no_victim_does_nothing_for_a_victim_spell() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let a = uid(&mut app, alice);
    let before = app.world().get::<TbaCombatant>(alice).unwrap().clone();
    run(&mut app, |host| {
        host.magic_call_magic(a, None, None, 32, 10);
        host.magic_call_magic(a, None, None, 1, 10);
        host.magic_call_magic(a, Some(424_242), None, 32, 10);
        // Out of the C's range: refused before anything else.
        host.magic_call_magic(a, Some(a), None, 0, 10);
        host.magic_call_magic(a, Some(a), None, 300, 10);
        host.magic_call_magic(a, Some(a), None, -1, 10);
    });
    assert_eq!(*app.world().get::<TbaCombatant>(alice).unwrap(), before);
}

#[test]
fn a_protected_mob_refuses_the_spell() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let guard = mob(&mut app, temple, 3, "the guard", 20);
    stats_of(&mut app, 3).behaviour = vec![NpcBehaviour::NoKill];
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    let before = app.world().get::<TbaCombatant>(guard).unwrap().hp;
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(g), None, 32, 10)
    });
    assert_eq!(app.world().get::<TbaCombatant>(guard).unwrap().hp, before);
    assert!(
        output(&mut app)
            .iter()
            .any(|line| line.contains("This mob is protected."))
    );
}

#[test]
fn casting_what_the_pack_cannot_do_is_counted() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let a = uid(&mut app, alice);
    let sword = item_on_ground(&mut app, temple, 2, "a sword");
    let s = uid(&mut app, sword);
    run(&mut app, |host| {
        // Ventriloquate has no spello in the C; the pack carries it as a name.
        host.magic_call_magic(a, Some(a), None, 41, 10);
        // A victim spell at an object has no pack effect.
        host.magic_call_magic(a, None, Some(s), 1, 10);
        // Bless alters objects.
        host.magic_call_magic(a, Some(a), Some(s), 3, 10);
    });
    assert_eq!(count(&app, "magic_spell_not_carried"), 1);
    assert_eq!(count(&app, "magic_obj_target"), 1);
    assert_eq!(count(&app, "magic_alter_objs"), 1);
}

// -- MagicHost: script affects ------------------------------------------------------

#[test]
fn a_script_affect_counts_until_it_is_removed_by_its_marker() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    recompute(&mut app);
    let base_hit = app.world().get::<TbaCombatant>(alice).unwrap().hitroll;
    let base_ac = app.world().get::<TbaCombatant>(alice).unwrap().armor_class;
    run(&mut app, |host| {
        // APPLY_HITROLL 18, APPLY_AC 17; permanent (-1), then timed.
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 18, 4, -1, None);
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 17, -10, 5, None);
        assert!(host.char_affected_by_spell(a, SPELL_DG_AFFECT));
    });
    recompute(&mut app);
    assert_eq!(
        app.world().get::<TbaCombatant>(alice).unwrap().hitroll,
        base_hit + 4
    );
    assert_eq!(
        app.world().get::<TbaCombatant>(alice).unwrap().armor_class,
        base_ac - 10
    );
    run(&mut app, |host| {
        host.magic_remove_dg_affects(a, SPELL_DG_AFFECT);
        assert!(!host.char_affected_by_spell(a, SPELL_DG_AFFECT));
    });
    recompute(&mut app);
    assert_eq!(
        app.world().get::<TbaCombatant>(alice).unwrap().hitroll,
        base_hit
    );
    assert_eq!(
        app.world().get::<TbaCombatant>(alice).unwrap().armor_class,
        base_ac
    );
    assert!(app.world().get::<TbaBuffs>(alice).unwrap().buffs.is_empty());
}

#[test]
fn removing_a_spell_number_leaves_other_affects_alone() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 18, 4, -1, None);
        host.magic_add_dg_affect(a, 1, 19, 2, -1, None);
        host.magic_remove_dg_affects(a, SPELL_DG_AFFECT);
        assert!(!host.char_affected_by_spell(a, SPELL_DG_AFFECT));
        assert!(host.char_affected_by_spell(a, 1));
        host.char_affect_total(a);
        assert_eq!(host.char_stat(a, CharStat::Damroll), 2);
        assert_eq!(host.char_stat(a, CharStat::Hitroll), 0);
    });
}

#[test]
fn a_script_affect_wears_off_after_its_duration() {
    // affect_update runs once a mud hour (75 s, 750 pulses) and removes an
    // affect when its duration is already 0, so duration 0 lasts to the first
    // update and duration 1 to the second.
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 19, 5, 0, None);
        host.magic_add_dg_affect(a, 2, 18, 1, -1, None);
    });
    tick_pulses(&mut app, 700);
    run(&mut app, |host| {
        assert!(host.char_affected_by_spell(a, SPELL_DG_AFFECT))
    });
    tick_pulses(&mut app, 100);
    run(&mut app, |host| {
        assert!(!host.char_affected_by_spell(a, SPELL_DG_AFFECT), "worn off");
        assert!(host.char_affected_by_spell(a, 2), "permanent stays");
        // The buff runs out on the round clock: 25 rounds an hour.
        host.char_affect_total(a);
        host.magic_add_dg_affect(a, 3, 0, 0, -1, None);
    });
    for _ in 0..25 {
        app.world_mut().run_system_once(tick_buffs).expect("tick");
    }
    recompute(&mut app);
    assert_eq!(app.world().get::<TbaCombatant>(alice).unwrap().damroll, 0);
    // The buff's own round countdown matches the duration: 25 rounds an hour.
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, 4, 18, 1, 2, None)
    });
    let rounds = app
        .world()
        .get::<TbaBuffs>(alice)
        .unwrap()
        .buffs
        .iter()
        .filter(|buff| buff.rounds() < 1_000)
        .map(|buff| buff.rounds())
        .max()
        .unwrap();
    assert_eq!(rounds, 3 * 25);
    app.world_mut().run_system_once(tick_buffs).expect("tick");
    assert!(
        app.world()
            .get::<TbaBuffs>(alice)
            .unwrap()
            .buffs
            .iter()
            .any(|b| b.rounds() == 74)
    );
}

#[test]
fn a_bit_affect_is_found_by_its_spell_and_logged_as_inert() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        // AFF_SANCTUARY is bit 8; APPLY_NONE.
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, 3, Some(8));
        assert!(host.char_affected_by_spell(a, SPELL_DG_AFFECT));
        host.magic_remove_dg_affects(a, SPELL_DG_AFFECT);
        assert!(!host.char_affected_by_spell(a, SPELL_DG_AFFECT));
        // APPLY_LEVEL has no pack field: kept as a marker, counted.
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 8, 2, 3, None);
        assert!(host.char_affected_by_spell(a, SPELL_DG_AFFECT));
    });
    assert_eq!(count(&app, "magic_affect_bit"), 1);
    assert_eq!(count(&app, "magic_affect_apply"), 1);
    assert!(
        app.world()
            .get::<TbaBuffs>(alice)
            .is_none_or(|b| b.buffs.is_empty())
    );
}

#[test]
fn affects_on_an_npc_change_its_statistics() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "g", 4);
    let g = uid(&mut app, guard);
    let before = app.world().get::<TbaCombatant>(guard).unwrap().clone();
    run(&mut app, |host| {
        // APPLY_DAMROLL +6, permanent: the buff counts for an NPC too.
        host.magic_add_dg_affect(g, SPELL_DG_AFFECT, 19, 6, -1, None);
        assert!(host.char_affected_by_spell(g, SPELL_DG_AFFECT));
        host.char_affect_total(g);
    });
    assert_ne!(
        *app.world().get::<TbaCombatant>(guard).unwrap(),
        before,
        "the NPC's buff is folded into its combatant"
    );
    run(&mut app, |host| {
        host.magic_remove_dg_affects(g, SPELL_DG_AFFECT);
        assert!(!host.char_affected_by_spell(g, SPELL_DG_AFFECT));
        host.char_affect_total(g);
    });
    assert_eq!(*app.world().get::<TbaCombatant>(guard).unwrap(), before);
}

#[test]
fn true_sight_spells_read_as_affected() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        assert!(!host.char_affected_by_spell(a, 19));
        host.magic_call_magic(a, Some(a), None, 19, 10);
        assert!(host.char_affected_by_spell(a, 19), "detect invisibility");
    });
}

// -- review fixes --------------------------------------------------------------

use bevy::ecs::message::Messages;

use mib_tba::equipment::recompute_npc_equipment_affects;

fn spell_affect(app: &App, id: &str) -> AffectDef {
    app.world()
        .resource::<TbaContent>()
        .spells
        .get(id)
        .and_then(|def| def.affect.clone())
        .expect("spell affect")
}

#[test]
fn damage_to_zero_or_below_is_death_after_a_recompute_too() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.char_set_stat(a, CharStat::Hit, -5);
        host.char_update_pos(a);
    });
    recompute(&mut app);
    let combatant = app.world().get::<TbaCombatant>(alice).unwrap();
    assert_eq!(combatant.hp, 0);
    assert_eq!(combatant.position, Position::Dead);
}

#[test]
fn an_equipped_npcs_stat_write_survives_the_rebuild_without_double_counting() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "the guard", 4);
    define_item(
        &mut app,
        50,
        TbaItemKind::Weapon,
        &[WearSlot::Wield],
        &["sword"],
    );
    app.world_mut()
        .resource_mut::<TbaContent>()
        .items
        .get_mut(&ItemId::from("tba_item_50"))
        .unwrap()
        .affects = vec![AffectDef {
        location: AffectLocation::Hitroll,
        modifier: 2,
    }];
    item_worn(&mut app, guard, WearSlot::Wield, 50, "a sword");
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.char_affect_total(g);
        assert_eq!(host.char_stat(g, CharStat::Hitroll), 2);
        host.char_set_stat(g, CharStat::Hitroll, 7);
        assert_eq!(host.char_stat(g, CharStat::Hitroll), 7);
        host.char_set_real_abil(g, CharAbil::Str, 16);
        assert_eq!(host.char_abil(g, CharAbil::Str), 16);
        assert_eq!(host.char_real_abil(g, CharAbil::Str), 16);
    });
    app.world_mut()
        .run_system_once(recompute_npc_equipment_affects)
        .expect("npc recompute");
    let combatant = app.world().get::<TbaCombatant>(guard).unwrap();
    assert_eq!(combatant.hitroll, 7, "the frame's rebuild agrees");
    assert_eq!(combatant.attributes.strength, 16);
}

#[test]
fn a_cast_does_not_wipe_permanent_writes_or_script_affects() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let cleric = hero(&mut app, temple, "Cleric", 8, "cleric", 20);
    let (a, c) = (uid(&mut app, alice), uid(&mut app, cleric));
    let strength = spell_affect(&app, "strength");
    let bless = spell_affect(&app, "bless");
    run(&mut app, |host| {
        host.char_set_real_abil(a, CharAbil::Str, 16);
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 18, 5, -1, None);
        host.magic_call_magic(c, Some(a), None, 39, 10);
        host.magic_call_magic(c, Some(a), None, 3, 10);
        // The cast is in force at once.
        assert_eq!(host.char_abil(a, CharAbil::Str), 16 + strength.modifier);
        assert_eq!(host.char_stat(a, CharStat::Hitroll), 5 + bless.modifier);
        assert_eq!(host.char_real_abil(a, CharAbil::Str), 16);
        assert!(host.char_affected_by_spell(a, SPELL_DG_AFFECT));
        assert!(host.char_affected_by_spell(a, 39));
    });
    recompute(&mut app);
    let combatant = app.world().get::<TbaCombatant>(alice).unwrap();
    assert_eq!(combatant.attributes.strength, 16 + strength.modifier);
    assert_eq!(combatant.hitroll, 5 + bless.modifier);
    // Taking the script affect off leaves the permanent write and the casts.
    run(&mut app, |host| {
        host.magic_remove_dg_affects(a, SPELL_DG_AFFECT);
        assert_eq!(host.char_stat(a, CharStat::Hitroll), bless.modifier);
        assert_eq!(host.char_real_abil(a, CharAbil::Str), 16);
    });
}

#[test]
fn a_pack_spell_is_not_confused_with_an_equal_script_affect() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        // strength is +1 str, bless +2 hitroll.
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 1, 1, 5, None);
        host.char_set_stat(a, CharStat::Hitroll, 2);
        assert!(!host.char_affected_by_spell(a, 39));
        assert!(!host.char_affected_by_spell(a, 3));
    });
}

#[test]
fn overlapping_script_affects_expire_independently() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    // Duration 0 lasts to the first mud-hour update (25 rounds), 1 to the second.
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 1, 2, 0, None)
    });
    tick_pulses(&mut app, 300);
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 1, 2, 1, None)
    });
    for _ in 0..25 {
        app.world_mut().run_system_once(tick_buffs).expect("tick");
    }
    tick_pulses(&mut app, 500);
    run(&mut app, |host| {
        // The first marker is dead; this prunes it.
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 18, 1, 5, None);
        assert_eq!(host.char_abil(a, CharAbil::Str), 13 + 2, "the second holds");
    });
    let buffs = app.world().get::<TbaBuffs>(alice).unwrap();
    assert_eq!(
        buffs
            .buffs
            .iter()
            .filter(|b| b.affect.location == AffectLocation::Str)
            .count(),
        1
    );
}

#[test]
fn a_lethal_script_cast_leaves_an_npc_for_the_end_of_the_pulse() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let rat = mob(&mut app, temple, 3, "a rat", 1);
    app.world_mut().get_mut::<TbaCombatant>(rat).unwrap().hp = 1;
    let (a, r) = (uid(&mut app, alice), uid(&mut app, rat));
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(r), None, 32, 30);
        assert!(host.is_dead(r), "marked, not yet extracted");
        assert_eq!(host.name(r), "a rat");
        assert_eq!(host.in_room(r), host.in_room(a));
    });
    assert!(
        app.world().get_entity(rat).is_err(),
        "swept when the host run ends, not mid-script"
    );
}

#[test]
fn spells_without_a_pack_effect_are_counted() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(a), None, 36, 10); // sanctuary
        host.magic_call_magic(a, Some(a), None, 20, 10); // detect magic
        host.magic_call_magic(a, Some(a), None, 19, 10); // detect invisibility: true sight
    });
    assert_eq!(count(&app, "magic_spell_inert"), 2);
    assert!(
        app.world()
            .get::<mib_tba::stealth::TbaTrueSight>(alice)
            .is_some()
    );
}

#[test]
fn a_cast_on_an_npc_folds_its_buff_into_the_combatant() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "cleric", 10);
    let guard = mob(&mut app, temple, 3, "the guard", 4);
    let (a, g) = (uid(&mut app, alice), uid(&mut app, guard));
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(g), None, 39, 10);
        assert!(host.char_affected_by_spell(g, 39));
    });
    assert_eq!(count(&app, "magic_npc_affect"), 0);
}

#[test]
fn a_script_cast_by_an_npc_writes_nothing_to_its_unconnected_caster() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 10);
    let troll = mob(&mut app, temple, 3, "a troll", 10);
    let (a, t) = (uid(&mut app, alice), uid(&mut app, troll));
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .clear();
    run(&mut app, |host| {
        host.magic_call_magic(t, Some(a), None, 32, 10); // magic missile
        host.magic_call_magic(t, Some(a), None, 39, 10); // strength
        host.magic_call_magic(t, Some(a), None, 36, 10); // sanctuary
    });
    let to_caster = app
        .world()
        .resource::<Messages<OutputEvent>>()
        .iter_current_update_messages()
        .filter(|event| event.session == troll)
        .count();
    assert_eq!(to_caster, 0);
}

#[test]
fn a_cast_trigger_that_returns_zero_refuses_the_spell() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    define(&mut app, 900, "mob", "p", 100, "return 0");
    attach(&mut app, "npc:tba_npc_3", &[900]);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let troll = mob(&mut app, temple, 3, "a troll", 20);
    let (a, t) = (uid(&mut app, alice), uid(&mut app, troll));
    let before = app.world().get::<TbaCombatant>(troll).unwrap().hp;
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(t), None, 32, 10)
    });
    assert_eq!(app.world().get::<TbaCombatant>(troll).unwrap().hp, before);
}

#[test]
fn a_cast_trigger_on_the_item_cast_through_or_the_room_refuses_the_spell() {
    // An object's cast trigger is asked for the object the spell is cast
    // through; a room's for the caster's room.
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let vault = room(&mut app, 2);
    define(&mut app, 901, "obj", "p", 100, "return 0");
    attach(&mut app, "item:tba_item_60", &[901]);
    define(&mut app, 902, "wld", "p", 100, "return 0");
    attach(&mut app, "room:tba.2", &[902]);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let bob = hero(&mut app, vault, "Bob", 8, "magic_user", 10);
    let wand = item_carried(&mut app, alice, 60, "a wand");
    let troll = mob(&mut app, temple, 3, "a troll", 20);
    let ogre = mob(&mut app, vault, 4, "an ogre", 20);
    let (a, b, w) = (
        uid(&mut app, alice),
        uid(&mut app, bob),
        uid(&mut app, wand),
    );
    let (t, o) = (uid(&mut app, troll), uid(&mut app, ogre));
    let (troll_hp, ogre_hp) = (
        app.world().get::<TbaCombatant>(troll).unwrap().hp,
        app.world().get::<TbaCombatant>(ogre).unwrap().hp,
    );
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(t), Some(w), 32, 10);
        host.magic_call_magic(b, Some(o), None, 32, 10);
    });
    assert_eq!(app.world().get::<TbaCombatant>(troll).unwrap().hp, troll_hp);
    assert_eq!(app.world().get::<TbaCombatant>(ogre).unwrap().hp, ogre_hp);
    // Without the wand the temple's spell goes through.
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(t), None, 32, 10)
    });
    assert!(app.world().get::<TbaCombatant>(troll).unwrap().hp < troll_hp);
}

#[test]
fn room_and_caster_target_spells_land_where_the_pack_says() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 20);
    let bob = hero(&mut app, temple, "Bob", 8, "warrior", 20);
    let (a, b) = (uid(&mut app, alice), uid(&mut app, bob));
    let (alice_hp, bob_hp) = (
        app.world().get::<TbaCombatant>(alice).unwrap().hp,
        app.world().get::<TbaCombatant>(bob).unwrap().hp,
    );
    run(&mut app, |host| {
        // earthquake hits the room but not the caster
        host.magic_call_magic(a, None, None, 23, 30)
    });
    assert_eq!(app.world().get::<TbaCombatant>(alice).unwrap().hp, alice_hp);
    assert!(app.world().get::<TbaCombatant>(bob).unwrap().hp < bob_hp);
    // detect invisibility is a self spell: it lands on the victim named, as
    // `mag_affects(victim)` does in the C.
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(b), None, 19, 10)
    });
    let sight = |entity| {
        app.world()
            .get::<mib_tba::stealth::TbaTrueSight>(entity)
            .is_some()
    };
    assert!(sight(bob));
    assert!(!sight(alice));
}

#[test]
fn every_dg_affect_apply_number_reaches_its_statistic() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    let stats = [
        (12, CharStat::MaxMana),
        (13, CharStat::MaxHit),
        (14, CharStat::MaxMove),
        (20, CharStat::SavePara),
        (21, CharStat::SaveRod),
        (22, CharStat::SavePetri),
        (23, CharStat::SaveBreath),
        (24, CharStat::SaveSpell),
    ];
    for (apply, stat) in &stats {
        let before = run(&mut app, |host| host.char_stat(a, *stat));
        run(&mut app, |host| {
            host.magic_add_dg_affect(a, SPELL_DG_AFFECT, *apply, 3, -1, None)
        });
        let after = run(&mut app, |host| host.char_stat(a, *stat));
        assert_eq!(after, before + 3, "apply {apply}");
    }
    let abils = [
        (1, CharAbil::Str),
        (2, CharAbil::Dex),
        (3, CharAbil::Int),
        (4, CharAbil::Wis),
        (5, CharAbil::Con),
        (6, CharAbil::Cha),
    ];
    for (apply, abil) in abils {
        let before = run(&mut app, |host| host.char_abil(a, abil));
        run(&mut app, |host| {
            host.magic_add_dg_affect(a, SPELL_DG_AFFECT, apply, 2, -1, None)
        });
        let after = run(&mut app, |host| host.char_abil(a, abil));
        assert_eq!(after, before + 2, "apply {apply}");
    }
    let damroll = run(&mut app, |host| host.char_stat(a, CharStat::Damroll));
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 19, 4, -1, None);
        assert_eq!(host.char_stat(a, CharStat::Damroll), damroll + 4);
    });
}

#[test]
fn an_affect_bit_the_pack_has_a_component_for_sets_it_for_the_marker() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "thief", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, -1, Some(20)); // hide
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, -1, Some(19)); // sneak
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, -1, Some(4)); // detect invis
    });
    assert!(app.world().get::<TbaHidden>(alice).is_some());
    assert!(app.world().get::<TbaSneaking>(alice).is_some());
    assert!(
        app.world()
            .get::<mib_tba::stealth::TbaTrueSight>(alice)
            .is_some()
    );
    assert_eq!(count(&app, "magic_affect_bit"), 0);
    run(&mut app, |host| {
        host.magic_remove_dg_affects(a, SPELL_DG_AFFECT)
    });
    assert!(app.world().get::<TbaHidden>(alice).is_none());
    assert!(app.world().get::<TbaSneaking>(alice).is_none());
    assert!(
        app.world()
            .get::<mib_tba::stealth::TbaTrueSight>(alice)
            .is_none()
    );
}

#[test]
fn quest_state_comes_from_the_quest_index_and_log() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    let mut index = QuestIndex::default();
    let def: QuestDef = serde_yaml::from_str(
        "name: Kill the Mice!\ndescription: d\ngiver: tba_npc_179\nsteps: []\nreward: {xp: 0, items: []}\n",
    )
    .expect("quest def");
    index
        .map
        .insert(QuestId::from("tba_quest_100"), def.clone());
    app.world_mut().insert_resource(index);
    run(&mut app, |host| {
        assert!(host.char_quest_exists(100));
        assert!(!host.char_quest_exists(101));
        assert_eq!(host.char_quest(a), -1);
    });
    let mut log = PlayerQuestLog::default();
    log.quests
        .insert(QuestId::from("tba_quest_100"), QuestProgress::fresh(&def));
    app.world_mut().entity_mut(alice).insert(log);
    run(&mut app, |host| assert_eq!(host.char_quest(a), 100));
}

#[test]
fn reads_of_unmodelled_facts_are_counted_at_the_next_mutable_call() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.char_set_title(a, "the Brave"); // creates the counter
        host.char_stat(a, CharStat::Sex);
        host.char_stat(a, CharStat::Sex);
        host.char_fact(a, CharFact::Weight);
        host.char_abil(a, CharAbil::StrAdd);
        host.char_pref_flagged(a, 3);
        host.char_quest_completed(a, 100);
        host.char_set_title(a, "the Bold"); // reports them
    });
    // One log line per key, carrying the number of reads.
    let lines: Vec<&str> = app.world().resource::<DgLog>().lines().collect();
    let reads = |key: &str, n: u32| {
        lines
            .iter()
            .filter(|l| {
                l.contains(&format!("unsupported {key}:")) && l.ends_with(&format!("(x{n})"))
            })
            .count()
    };
    assert_eq!(count(&app, "char_sex"), 1);
    assert_eq!(reads("char_sex", 2), 1);
    assert_eq!(reads("char_weight", 1), 1);
    assert_eq!(reads("char_str_add", 1), 1);
    assert_eq!(reads("char_pref_flags", 1), 1);
    assert_eq!(reads("char_quest_completed", 1), 1);
}

#[test]
fn many_unmodelled_reads_do_not_evict_script_errors() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.script_log("real error");
        host.char_set_title(a, "the Brave");
        for _ in 0..1500 {
            host.char_stat(a, CharStat::Sex);
        }
        host.char_set_title(a, "the Bold");
    });
    let log = app.world().resource::<DgLog>();
    assert!(log.lines().any(|line| line.contains("real error")));
    assert!(log.lines().any(|line| line.ends_with("(x1500)")));
}

#[test]
fn a_changed_npc_purse_is_what_the_kill_pays() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "the guard", 4);
    stats_of(&mut app, 3).gold = 40;
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.char_increase_gold(g, -40);
        host.char_increase_gold(g, 5);
        assert_eq!(host.char_fact(g, CharFact::Gold), 5);
    });
    let (_, gold) = {
        let content = app.world().resource::<TbaContent>();
        let key = app.world().get::<NpcKey>(guard);
        let purse = app.world().get::<mib_tba::dg::chardata::TbaNpcPurse>(guard);
        mib_tba::combat::npc_reward(content, key, purse)
    };
    assert_eq!(
        gold, 5,
        "the kill pays the changed purse, not the prototype"
    );
}

#[test]
fn gain_exp_stops_at_the_level_below_immortal() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 30);
    let a = uid(&mut app, alice);
    run(&mut app, |host| host.char_gain_exp(a, 100_000));
    assert_eq!(app.world().get::<TbaProgress>(alice).unwrap().level, 30);
}

#[test]
fn a_sentinel_flag_reaches_a_wandering_npc() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "g", 4);
    let mut behaviors = NpcBehaviors::default();
    behaviors.0.wander = true;
    app.world_mut().entity_mut(guard).insert(behaviors);
    let g = uid(&mut app, guard);
    run(&mut app, |host| host.char_set_act_flag(g, 1, true));
    assert!(app.world().get::<NpcBehaviors>(guard).unwrap().0.sentinel);
    assert_eq!(count(&app, "char_act_flag_inert"), 0);
}

#[test]
fn reaching_the_maximum_gold_tells_the_player() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| host.char_increase_gold(a, i32::MAX));
    assert_eq!(app.world().get::<TbaGold>(alice).unwrap().0, 2_140_000_000);
}

// -- review regressions ------------------------------------------------------

#[test]
fn an_npc_killed_by_a_script_cast_runs_its_death_trigger() {
    // raw_kill: death_mtrigger(victim, killer) before the extraction.
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    define(&mut app, 903, "mob", "f", 100, "nop %actor.gold(77)%");
    attach(&mut app, "npc:tba_npc_3", &[903]);
    let alice = hero(&mut app, temple, "Alice", 7, "magic_user", 10);
    let rat = mob(&mut app, temple, 3, "a rat", 1);
    app.world_mut().get_mut::<TbaCombatant>(rat).unwrap().hp = 1;
    let (a, r) = (uid(&mut app, alice), uid(&mut app, rat));
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(r), None, 32, 30)
    });
    assert!(app.world().get_entity(rat).is_err());
    assert_eq!(app.world().get::<TbaGold>(alice).unwrap().0, 77);
}

#[test]
fn a_timed_hide_or_sneak_ends_with_its_duration_without_help() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, 1, Some(19));
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, 1, Some(20));
    });
    assert!(app.world().get::<TbaSneaking>(alice).is_some());
    tick_pulses(&mut app, 1600);
    // Any mutable call sweeps it.
    run(&mut app, |host| host.char_set_title(a, "the Quiet"));
    assert!(app.world().get::<TbaSneaking>(alice).is_none());
    assert!(app.world().get::<TbaHidden>(alice).is_none());
}

#[test]
fn a_script_affect_keeps_a_components_it_did_not_create() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "thief", 5);
    let a = uid(&mut app, alice);
    app.world_mut().entity_mut(alice).insert(TbaSneaking);
    run(&mut app, |host| {
        host.magic_call_magic(a, Some(a), None, 19, 10); // detect invisibility
        assert!(host.char_affected_by_spell(a, 19));
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, -1, Some(6)); // sense life
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, -1, Some(19)); // sneak
        host.magic_remove_dg_affects(a, SPELL_DG_AFFECT);
    });
    assert!(
        app.world()
            .get::<mib_tba::stealth::TbaTrueSight>(alice)
            .is_some(),
        "the spell's sight survives"
    );
    assert!(
        app.world().get::<TbaSneaking>(alice).is_some(),
        "the player's own sneak survives"
    );
}

#[test]
fn true_sight_from_a_script_is_not_the_spell() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, -1, Some(4));
        assert!(host.char_affected_by_spell(a, SPELL_DG_AFFECT));
        assert!(!host.char_affected_by_spell(a, 19));
        assert!(!host.char_affected_by_spell(a, 25));
    });
}

#[test]
fn a_write_to_an_npc_without_combat_state_gives_it_one() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    define_npc_level(&mut app, 3, 4);
    let guard = npc(&mut app, temple, 3, "the guard");
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.char_set_title(g, "x"); // creates the read counter
        assert_eq!(host.char_stat(g, CharStat::Hit), 0); // neutral, counted
        host.char_set_stat(g, CharStat::Hit, 5);
        assert_eq!(host.char_stat(g, CharStat::Hit), 5);
        assert!(host.char_stat(g, CharStat::MaxHit) > 0);
        host.char_set_title(g, "y"); // reports the read
    });
    assert_eq!(count(&app, "char_set_stat_no_combat_state"), 0);
    assert_eq!(count(&app, "char_npc_no_combat_state"), 1);
}

#[test]
fn a_write_to_a_character_with_no_combat_state_and_no_prototype_is_logged() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let ghost = npc(&mut app, temple, 999, "a ghost");
    let g = uid(&mut app, ghost);
    run(&mut app, |host| host.char_set_stat(g, CharStat::Hit, 5));
    assert_eq!(count(&app, "char_set_stat_no_combat_state"), 1);
}

#[test]
fn update_pos_that_kills_is_counted() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.char_set_stat(a, CharStat::Hit, -3);
        host.char_update_pos(a);
    });
    assert_eq!(count(&app, "char_update_pos_dead"), 1);
}

#[test]
fn a_player_hit_by_a_script_debuff_is_told() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let troll = mob(&mut app, temple, 3, "a troll", 10);
    let (a, t) = (uid(&mut app, alice), uid(&mut app, troll));
    output(&mut app);
    run(&mut app, |host| {
        for _ in 0..60 {
            host.magic_call_magic(t, Some(a), None, 33, 30); // poison
        }
    });
    let landed = app
        .world()
        .get::<TbaBuffs>(alice)
        .is_some_and(|buffs| !buffs.buffs.is_empty());
    assert!(landed, "sixty casts land at least once");
    let text = output(&mut app).join("\n");
    assert!(text.contains("You are afflicted by"), "{text}");
}

#[test]
fn experience_caps_hold_at_the_edges() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    // A class table longer than the immortal threshold.
    app.world_mut()
        .resource_mut::<TbaContent>()
        .classes
        .get_mut("warrior")
        .unwrap()
        .xp_table
        .extend([9_000_000, 10_000_000, 11_000_000]);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 29);
    app.world_mut().get_mut::<TbaProgress>(alice).unwrap().xp = 20_000_000;
    let a = uid(&mut app, alice);
    run(&mut app, |host| host.char_gain_exp(a, 1));
    assert_eq!(app.world().get::<TbaProgress>(alice).unwrap().level, 30);
    // The loss cap is 500000 per call.
    run(&mut app, |host| {
        host.char_gain_exp(a, i32::MIN);
        assert_eq!(host.char_fact(a, CharFact::Exp), 20_000_001 - 500_000);
    });
}

#[test]
fn reaching_the_maximum_gold_says_so() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    output(&mut app);
    run(&mut app, |host| host.char_increase_gold(a, i32::MAX));
    let text = output(&mut app).join("\n");
    assert!(
        text.contains("You have reached the maximum gold!"),
        "{text}"
    );
}

#[test]
fn an_npc_keeps_its_experience_when_its_gold_changes() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let guard = mob(&mut app, temple, 3, "the guard", 4);
    let g = uid(&mut app, guard);
    run(&mut app, |host| {
        host.char_gain_exp(g, 40);
        host.char_increase_gold(g, 5);
        assert_eq!(host.char_fact(g, CharFact::Exp), 40);
        assert_eq!(host.char_fact(g, CharFact::Gold), 5);
    });
}

#[test]
fn an_unknown_class_is_class_undefined() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "bard", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        assert_eq!(host.char_stat(a, CharStat::Class), -1);
    });
}

#[test]
fn behaviour_bits_map_to_their_mob_flag_numbers() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let rat = mob(&mut app, temple, 3, "a rat", 1);
    stats_of(&mut app, 3).behaviour = vec![NpcBehaviour::Memory];
    let r = uid(&mut app, rat);
    run(&mut app, |host| {
        assert!(host.char_act_flagged(r, 11), "MOB_MEMORY");
        assert!(!host.char_act_flagged(r, 12), "MOB_HELPER");
    });
    stats_of(&mut app, 3).behaviour = vec![NpcBehaviour::Helper];
    run(&mut app, |host| {
        assert!(host.char_act_flagged(r, 12));
        assert!(!host.char_act_flagged(r, 11));
    });
}

#[test]
fn the_lowest_quest_vnum_answers_when_several_are_logged() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    let def: QuestDef = serde_yaml::from_str(
        "name: Q\ndescription: d\ngiver: tba_npc_179\nsteps: []\nreward: {xp: 0, items: []}\n",
    )
    .expect("quest def");
    let mut log = PlayerQuestLog::default();
    for vnum in [105, 100, 110] {
        log.quests.insert(
            QuestId::from(format!("tba_quest_{vnum}").as_str()),
            QuestProgress::fresh(&def),
        );
    }
    app.world_mut().entity_mut(alice).insert(log);
    run(&mut app, |host| assert_eq!(host.char_quest(a), 100));
}

#[test]
fn dg_affect_off_removes_a_pinned_buff_whose_marker_was_lost() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "warrior", 5);
    let a = uid(&mut app, alice);
    let buff = mib_tba::spells::ActiveBuff::pinned(
        AffectDef {
            location: AffectLocation::Damroll,
            modifier: 3,
        },
        10,
        false,
    );
    app.world_mut()
        .entity_mut(alice)
        .insert(TbaBuffs { buffs: vec![buff] });
    run(&mut app, |host| {
        host.magic_remove_dg_affects(a, SPELL_DG_AFFECT)
    });
    assert!(app.world().get::<TbaBuffs>(alice).unwrap().buffs.is_empty());
}

#[test]
fn two_script_affects_sharing_a_bit_release_it_together() {
    let mut app = pack_app();
    let temple = room(&mut app, 1);
    let alice = hero(&mut app, temple, "Alice", 7, "thief", 5);
    let a = uid(&mut app, alice);
    run(&mut app, |host| {
        host.magic_add_dg_affect(a, SPELL_DG_AFFECT, 0, 0, 1, Some(20));
        host.magic_add_dg_affect(a, 5, 0, 0, -1, Some(20));
    });
    tick_pulses(&mut app, 1600);
    run(&mut app, |host| host.char_set_title(a, "x"));
    assert!(
        app.world().get::<TbaHidden>(alice).is_some(),
        "the permanent one still holds it"
    );
    run(&mut app, |host| host.magic_remove_dg_affects(a, 5));
    assert!(app.world().get::<TbaHidden>(alice).is_none());
}
