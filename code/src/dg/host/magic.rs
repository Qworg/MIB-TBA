//! `MagicHost`: spells and affects.
//!
//! # Spell numbers
//!
//! Scripts and saved data name spells by the C's numbers (`SPELL_*` and
//! `SKILL_*` in `spells.h`), so [`SPELLS`] ports that table: for each spell the
//! pack carries, its number, the name `spello` gives it (what `find_skill_num`
//! matches against), the pack's id for it in `tba.spells`, and the `TAR_*` /
//! `MAG_*` bits and violence flag of `mag_assign_spells`. `SPELL_VENTRILOQUATE`
//! and `SPELL_GROUP_RECALL` have no `spello` in the C (their slots read
//! `!UNUSED!`), so they have no entry here even though the pack lists them.
//! A spell is defined for scripts when the C defines it and the pack's content
//! carries it; a skill when a class grants it or has a level for it.
//!
//! # Casting
//!
//! `call_magic` is ported up to the dispatch on the routines: the cast
//! triggers, the `ROOM_NOMAGIC` / `ROOM_PEACEFUL` / `MOB_NOKILL` refusals.
//! The effect is the pack's own: [`crate::spells::cast_spell_at_level`]
//! resolves the catalog entry (saving throw, damage, heal, buff) as the `cast`
//! command does, minus the mana, class and position checks a script cast
//! skips. The pack has no object-targeted spells, so a cast at an object is
//! logged and does nothing. A spell the pack lists but cannot run (utility, or
//! a detect that is not true sight) is logged as `magic_spell_inert`. An NPC the
//! spell kills runs its `death` triggers (as `raw_kill` does for the killer) and is
//! then marked `DgDead`, going at the end of the host run.
//!
//! # Affects
//!
//! A script affect (`dg_affect`) is a [`DgAffect`] marker on the character
//! plus, when it changes a number the pack models, a pinned `TbaBuffs` entry
//! ([`ActiveBuff::pinned`]) that makes it count in the recompute, runs out by
//! the round clock like any buff and is never replaced by a cast.
//! `affect_from_char` removes the markers of a spell number and the buffs they
//! made; for `SPELL_DG_AFFECT` it removes every pinned affect buff, so a saved
//! buff whose marker was lost still goes. A bit the pack has a component for
//! (`AFF_DETECT_INVIS`, `AFF_SENSE_LIFE`, `AFF_SNEAK`, `AFF_HIDE`) sets it
//! while the marker lives; hide and sneak have no clock of their own, so a
//! timed one ends when the marker is next pruned (any `dg_affect` or
//! `affect_from_char` on the character). Buffs take effect on players and on
//! NPCs alike: `recompute_equipment_affects` and
//! `recompute_npc_equipment_affects` fold them into the combatant.

use dg_vm::dispatch::{cast_mtrigger, cast_otrigger, cast_wtrigger};
use dg_vm::fields_char::CharHost;
use dg_vm::fields_room::RoomHost;
use dg_vm::magic::{MAG_GROUPS, MAX_SPELLS, MagicHost, SPELL_DG_AFFECT, TOP_SPELL_DEFINE};
use dg_vm::{BaseHost, Uid, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::Npc;

use super::EcsHost;
use super::chars::chardata::{DgAffect, TbaDgAffects};
use super::chars::{ROUND_PULSES, affect_location, class_key};
use crate::combat::{TbaCombatant, TbaHidden, TbaSneaking};
use crate::content::{AffectDef, Position, SpellKind, SpellTarget};
use crate::dg::script::DgDead;
use crate::items::MUD_HOUR_SECONDS;
use crate::spells::{ActiveBuff, PINNED_FOREVER, TbaBuffs, cast_spell_at_level};
use crate::stealth::{TRUE_SIGHT_DETECT, TbaTrueSight};
use mib_stdlib::prelude::SEE_INVIS;

const TAR_IGNORE: u32 = 1 << 0;
const TAR_CHAR_ROOM: u32 = 1 << 1;
const TAR_CHAR_WORLD: u32 = 1 << 2;
const TAR_FIGHT_VICT: u32 = 1 << 4;
const TAR_SELF_ONLY: u32 = 1 << 5;
const TAR_NOT_SELF: u32 = 1 << 6;
const TAR_OBJ_INV: u32 = 1 << 7;
const TAR_OBJ_ROOM: u32 = 1 << 8;
const TAR_OBJ_WORLD: u32 = 1 << 9;
const TAR_OBJ_EQUIP: u32 = 1 << 10;

const MAG_DAMAGE: u32 = 1 << 0;
const MAG_AFFECTS: u32 = 1 << 1;
const MAG_UNAFFECTS: u32 = 1 << 2;
const MAG_POINTS: u32 = 1 << 3;
const MAG_ALTER_OBJS: u32 = 1 << 4;
const MAG_AREAS: u32 = 1 << 7;
const MAG_SUMMONS: u32 = 1 << 8;
const MAG_CREATIONS: u32 = 1 << 9;
const MAG_MANUAL: u32 = 1 << 10;
const MAG_ROOMS: u32 = 1 << 11;

/// `ROOM_PEACEFUL`.
const ROOM_PEACEFUL: usize = 4;
/// `ROOM_NOMAGIC`.
const ROOM_NOMAGIC: usize = 7;
/// `MOB_NOKILL`.
const MOB_NOKILL: u32 = 18;

/// Pulses in one mud hour, the unit an affect's duration counts in.
const HOUR_PULSES: i64 = (MUD_HOUR_SECONDS * 10.0) as i64;

/// `AFF_DETECT_INVIS`, `AFF_SENSE_LIFE`, `AFF_SNEAK` and `AFF_HIDE`: the affect
/// bits the pack has a component for.
pub(super) const AFF_DETECT_INVIS: u32 = 4;
pub(super) const AFF_SENSE_LIFE: u32 = 6;
const AFF_SNEAK: u32 = 19;
const AFF_HIDE: u32 = 20;

/// One row of `spell_info[]` for a spell or skill the pack can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpellInfo {
    /// The `SPELL_*` / `SKILL_*` number.
    pub number: i32,
    /// The pack's id in `tba.spells` (a spell) or in a class's granted skills
    /// (a skill); empty for `SPELL_DG_AFFECT`.
    pub id: &'static str,
    /// `spell_info[].name`.
    pub name: &'static str,
    /// `spell_info[].targets`.
    pub targets: u32,
    /// `spell_info[].violent`.
    pub violent: bool,
    /// `spell_info[].routines`.
    pub routines: u32,
}

const fn spell(
    number: i32,
    id: &'static str,
    name: &'static str,
    targets: u32,
    violent: bool,
    routines: u32,
) -> SpellInfo {
    SpellInfo {
        number,
        id,
        name,
        targets,
        violent,
        routines,
    }
}

const fn skill(number: i32, id: &'static str, name: &'static str) -> SpellInfo {
    spell(number, id, name, 0, false, 0)
}

/// `spell_info[]` as `mag_assign_spells` fills it (`spell_parser.c`), for the
/// spells the pack carries, in `spells.h` order, then the skills.
const SPELLS: &[SpellInfo] = &[
    spell(1, "armor", "armor", TAR_CHAR_ROOM, false, MAG_AFFECTS),
    spell(2, "teleport", "teleport", TAR_CHAR_ROOM, false, MAG_MANUAL),
    spell(
        3,
        "bless",
        "bless",
        TAR_CHAR_ROOM | TAR_OBJ_INV,
        false,
        MAG_AFFECTS | MAG_ALTER_OBJS,
    ),
    spell(
        4,
        "blindness",
        "blindness",
        TAR_CHAR_ROOM | TAR_NOT_SELF,
        false,
        MAG_AFFECTS,
    ),
    spell(
        5,
        "burning_hands",
        "burning hands",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(
        6,
        "call_lightning",
        "call lightning",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(
        7,
        "charm",
        "charm person",
        TAR_CHAR_ROOM | TAR_NOT_SELF,
        true,
        MAG_MANUAL,
    ),
    spell(
        8,
        "chill_touch",
        "chill touch",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE | MAG_AFFECTS,
    ),
    spell(9, "clone", "clone", TAR_IGNORE, false, MAG_SUMMONS),
    spell(
        10,
        "color_spray",
        "color spray",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(
        11,
        "control_weather",
        "control weather",
        TAR_IGNORE,
        false,
        MAG_MANUAL,
    ),
    spell(
        12,
        "create_food",
        "create food",
        TAR_IGNORE,
        false,
        MAG_CREATIONS,
    ),
    spell(
        13,
        "create_water",
        "create water",
        TAR_OBJ_INV | TAR_OBJ_EQUIP,
        false,
        MAG_MANUAL,
    ),
    spell(
        14,
        "cure_blind",
        "cure blind",
        TAR_CHAR_ROOM,
        false,
        MAG_UNAFFECTS,
    ),
    spell(
        15,
        "cure_critic",
        "cure critic",
        TAR_CHAR_ROOM,
        false,
        MAG_POINTS,
    ),
    spell(
        16,
        "cure_light",
        "cure light",
        TAR_CHAR_ROOM,
        false,
        MAG_POINTS,
    ),
    spell(
        17,
        "curse",
        "curse",
        TAR_CHAR_ROOM | TAR_OBJ_INV,
        true,
        MAG_AFFECTS | MAG_ALTER_OBJS,
    ),
    spell(
        18,
        "detect_align",
        "detect alignment",
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    ),
    spell(
        19,
        "detect_invis",
        "detect invisibility",
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    ),
    spell(
        20,
        "detect_magic",
        "detect magic",
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    ),
    spell(
        21,
        "detect_poison",
        "detect poison",
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_MANUAL,
    ),
    spell(
        22,
        "dispel_evil",
        "dispel evil",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(23, "earthquake", "earthquake", TAR_IGNORE, true, MAG_AREAS),
    spell(
        24,
        "enchant_weapon",
        "enchant weapon",
        TAR_OBJ_INV,
        false,
        MAG_MANUAL,
    ),
    spell(
        25,
        "energy_drain",
        "energy drain",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE | MAG_MANUAL,
    ),
    spell(
        26,
        "fireball",
        "fireball",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(
        27,
        "harm",
        "harm",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(
        28,
        "heal",
        "heal",
        TAR_CHAR_ROOM,
        false,
        MAG_POINTS | MAG_UNAFFECTS,
    ),
    spell(
        29,
        "invisibility",
        "invisibility",
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_AFFECTS | MAG_ALTER_OBJS,
    ),
    spell(
        30,
        "lightning_bolt",
        "lightning bolt",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(
        31,
        "locate_object",
        "locate object",
        TAR_OBJ_WORLD,
        false,
        MAG_MANUAL,
    ),
    spell(
        32,
        "magic_missile",
        "magic missile",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(
        33,
        "poison",
        "poison",
        TAR_CHAR_ROOM | TAR_NOT_SELF | TAR_OBJ_INV,
        true,
        MAG_AFFECTS | MAG_ALTER_OBJS,
    ),
    spell(
        34,
        "protection_from_evil",
        "protection from evil",
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    ),
    spell(
        35,
        "remove_curse",
        "remove curse",
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_EQUIP,
        false,
        MAG_UNAFFECTS | MAG_ALTER_OBJS,
    ),
    spell(
        36,
        "sanctuary",
        "sanctuary",
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    ),
    spell(
        37,
        "shocking_grasp",
        "shocking grasp",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(38, "sleep", "sleep", TAR_CHAR_ROOM, true, MAG_AFFECTS),
    spell(
        39,
        "strength",
        "strength",
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    ),
    spell(
        40,
        "summon",
        "summon",
        TAR_CHAR_WORLD | TAR_NOT_SELF,
        false,
        MAG_MANUAL,
    ),
    spell(
        42,
        "word_of_recall",
        "word of recall",
        TAR_CHAR_ROOM,
        false,
        MAG_MANUAL,
    ),
    spell(
        43,
        "remove_poison",
        "remove poison",
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_UNAFFECTS | MAG_ALTER_OBJS,
    ),
    spell(
        44,
        "sense_life",
        "sense life",
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    ),
    spell(
        45,
        "animate_dead",
        "animate dead",
        TAR_OBJ_ROOM,
        false,
        MAG_SUMMONS,
    ),
    spell(
        46,
        "dispel_good",
        "dispel good",
        TAR_CHAR_ROOM | TAR_FIGHT_VICT,
        true,
        MAG_DAMAGE,
    ),
    spell(
        47,
        "group_armor",
        "group armor",
        TAR_IGNORE,
        false,
        MAG_GROUPS,
    ),
    spell(
        48,
        "group_heal",
        "group heal",
        TAR_IGNORE,
        false,
        MAG_GROUPS,
    ),
    spell(
        50,
        "infravision",
        "infravision",
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    ),
    spell(
        51,
        "waterwalk",
        "waterwalk",
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    ),
    spell(
        52,
        "identify",
        "identify",
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_MANUAL,
    ),
    spell(53, "fly", "fly", TAR_CHAR_ROOM, false, MAG_AFFECTS),
    spell(54, "darkness", "darkness", TAR_IGNORE, false, MAG_ROOMS),
    skill(131, "backstab", "backstab"),
    skill(132, "bash", "bash"),
    skill(133, "hide", "hide"),
    skill(134, "kick", "kick"),
    skill(135, "pick_lock", "pick lock"),
    skill(136, "whirlwind", "whirlwind"),
    skill(137, "rescue", "rescue"),
    skill(138, "sneak", "sneak"),
    skill(139, "steal", "steal"),
    skill(140, "track", "track"),
    skill(141, "bandage", "bandage"),
    // The framework's own affect: castable by nobody, defined for `stat`.
    spell(SPELL_DG_AFFECT, "", "Script-inflicted", TAR_IGNORE, true, 0),
];

/// The `spell_info[number]` row, if the C defines the slot and the pack's
/// table has it.
pub(super) fn spell_info(number: i32) -> Option<&'static SpellInfo> {
    SPELLS.iter().find(|info| info.number == number)
}

impl EcsHost<'_> {
    /// Whether the pack carries what the row describes.
    fn carries(&self, info: &SpellInfo) -> bool {
        if info.number == SPELL_DG_AFFECT {
            return true;
        }
        let Some(content) = self.content() else {
            return false;
        };
        if info.number <= MAX_SPELLS {
            return content.spells.contains_key(info.id);
        }
        content.classes.values().any(|class| {
            class
                .granted_skills
                .iter()
                .any(|id| class_key(id) == info.id)
        }) || content
            .skill_levels
            .values()
            .any(|levels| levels.keys().any(|id| class_key(id) == info.id))
    }

    /// The row for a spell number the pack carries.
    fn carried_info(&self, number: i32) -> Option<&'static SpellInfo> {
        spell_info(number).filter(|info| self.carries(info))
    }

    /// Drop the script affects that have worn off. Their buffs run out on
    /// the round clock by themselves; the bit components go with the marker.
    pub(super) fn expire_dg_affects(&mut self, entity: Entity) {
        let Some(mut marks) = self.world.get_mut::<TbaDgAffects>(entity) else {
            return;
        };
        let (live, dead): (Vec<DgAffect>, Vec<DgAffect>) = std::mem::take(&mut marks.0)
            .into_iter()
            .partition(|mark| mark.expires.is_none_or(|expires| expires > 0));
        marks.0 = live;
        for mark in &dead {
            self.release_bit(entity, mark);
        }
    }

    /// Remove the pinned buff that makes a marker count, if it is still there.
    /// A timed marker takes a timed buff, a permanent one a permanent buff.
    fn remove_buff_for(&mut self, entity: Entity, mark: &DgAffect) {
        let Some(affect) = &mark.numeric else {
            return;
        };
        if let Some(mut buffs) = self.world.get_mut::<TbaBuffs>(entity)
            && let Some(index) = buffs.buffs.iter().position(|buff| {
                buff.is_pinned()
                    && !buff.is_adjustment()
                    && &buff.affect == affect
                    && (buff.rounds() >= PINNED_FOREVER) == mark.expires.is_none()
            })
        {
            buffs.buffs.remove(index);
        }
    }

    /// Set the pack component an affect bit stands for. A component the
    /// character already has (a cast's true sight, the player's own sneak) is
    /// kept as it is; the result tells whether this call created it.
    fn grant_bit(&mut self, entity: Entity, bit: u32, rounds: u32) -> bool {
        let mut target = self.world.entity_mut(entity);
        match bit {
            AFF_DETECT_INVIS | AFF_SENSE_LIFE => {
                if target.contains::<TbaTrueSight>() {
                    return false;
                }
                target.insert(TbaTrueSight {
                    sight: SEE_INVIS,
                    detect: TRUE_SIGHT_DETECT,
                    rounds_left: rounds.max(1),
                });
                true
            }
            AFF_SNEAK => {
                let fresh = !target.contains::<TbaSneaking>();
                target.insert(TbaSneaking);
                fresh
            }
            AFF_HIDE => {
                let fresh = !target.contains::<TbaHidden>();
                target.insert(TbaHidden);
                fresh
            }
            _ => false,
        }
    }

    /// Take away the component a marker's bit set, unless another live marker
    /// still sets it (which then takes over the ownership) or the marker did
    /// not create it.
    fn release_bit(&mut self, entity: Entity, mark: &DgAffect) {
        let Some(bit) = mark.bit else {
            return;
        };
        let sights = [AFF_DETECT_INVIS, AFF_SENSE_LIFE];
        let shared =
            |other: u32| other == bit || (sights.contains(&bit) && sights.contains(&other));
        if let Some(mut marks) = self.world.get_mut::<TbaDgAffects>(entity)
            && let Some(holder) = marks
                .0
                .iter_mut()
                .find(|other| other.bit.is_some_and(shared) && other.expires.is_none_or(|e| e > 0))
        {
            holder.owns_bit |= mark.owns_bit;
            return;
        }
        if !mark.owns_bit {
            return;
        }
        let mut target = self.world.entity_mut(entity);
        match bit {
            AFF_DETECT_INVIS | AFF_SENSE_LIFE => {
                target.remove::<TbaTrueSight>();
            }
            AFF_SNEAK => {
                target.remove::<TbaSneaking>();
            }
            AFF_HIDE => {
                target.remove::<TbaHidden>();
            }
            _ => {}
        }
    }

    /// The characters a cast may touch: the victim and the caster, and
    /// everyone in the caster's room for a spell that hits the room.
    fn cast_candidates(&self, caster: Uid, victim: Option<Uid>, room_wide: bool) -> Vec<Entity> {
        let mut found: Vec<Entity> = Vec::new();
        let mut push = |uid: Uid, host: &Self| {
            if let Some(entity) = host.character(uid)
                && !found.contains(&entity)
            {
                found.push(entity);
            }
        };
        push(caster, self);
        if let Some(victim) = victim {
            push(victim, self);
        }
        if room_wide && let Some(room) = self.in_room(caster) {
            for uid in self.room_people(room) {
                push(uid, self);
            }
        }
        found
    }
}

impl MagicHost for EcsHost<'_> {
    /// The C's name for a spell the pack carries; `None` otherwise.
    fn magic_spell_name(&self, spellnum: i32) -> Option<String> {
        self.carried_info(spellnum).map(|info| info.name.to_owned())
    }

    fn magic_spell_targets(&self, spellnum: i32) -> u32 {
        self.carried_info(spellnum).map_or(0, |info| info.targets)
    }

    fn magic_spell_routines(&self, spellnum: i32) -> u32 {
        self.carried_info(spellnum).map_or(0, |info| info.routines)
    }

    /// `call_magic(..., CAST_SPELL)`: the cast triggers and the room and mob
    /// refusals as in the C, then the pack's own effect for the spell. A
    /// spell that needs a victim and has none does nothing, as `mag_damage`,
    /// `mag_affects` and `mag_points` do when `victim == NULL`.
    fn magic_call_magic(
        &mut self,
        caster: Uid,
        victim: Option<Uid>,
        obj: Option<Uid>,
        spellnum: i32,
        level: i32,
    ) {
        self.flush_reads();
        if !(1..=TOP_SPELL_DEFINE).contains(&spellnum) {
            return;
        }
        if cast_wtrigger(self, caster, victim, obj, spellnum) == 0
            || cast_otrigger(self, caster, obj, spellnum) == 0
            || cast_mtrigger(self, caster, victim, spellnum) == 0
        {
            return;
        }
        let Some(caster_entity) = self.character(caster) else {
            return;
        };
        let info = self.carried_info(spellnum);
        let room = self.in_room(caster);
        if let Some(room) = room {
            let room_entity = self.entity(room);
            if self.room_flagged(room, ROOM_NOMAGIC) {
                crate::dg::out::send_to_char(
                    self.world,
                    caster_entity,
                    "Your magic fizzles out and dies.",
                );
                if let Some(room_entity) = room_entity {
                    let name = self.name(caster);
                    let mut chars = name.chars();
                    let name: String = chars
                        .next()
                        .map(|first| first.to_uppercase().chain(chars).collect())
                        .unwrap_or_default();
                    let line = format!("{name}'s magic fizzles out and dies.");
                    crate::dg::out::send_to_room(self.world, room_entity, &line, &[caster_entity]);
                }
                return;
            }
            let violent = info.is_some_and(|info| info.violent || info.routines & MAG_DAMAGE != 0);
            if self.room_flagged(room, ROOM_PEACEFUL) && violent {
                crate::dg::out::send_to_char(
                    self.world,
                    caster_entity,
                    "A flash of white light fills the room, dispelling your violent magic!",
                );
                if let Some(room_entity) = room_entity {
                    crate::dg::out::send_to_room(
                        self.world,
                        room_entity,
                        "White light from no particular source suddenly fills the room, then vanishes.",
                        &[caster_entity],
                    );
                }
                return;
            }
        }
        let victim_entity = victim.and_then(|victim| self.character(victim));
        if let Some(victim) = victim.filter(|_| victim_entity.is_some())
            && self.is_npc(victim)
            && self.char_act_flagged(victim, MOB_NOKILL)
        {
            crate::dg::out::send_to_char(self.world, caster_entity, "This mob is protected.");
            return;
        }
        let Some(info) = info else {
            self.unsupported(
                "magic_spell_not_carried",
                &format!("spell {spellnum} is not one the pack carries"),
            );
            return;
        };
        let Some(def) = self.content().and_then(|c| c.spells.get(info.id)) else {
            // `SPELL_DG_AFFECT` and skills have no routines to run.
            return;
        };
        let (kind, target, affects) = (def.kind, def.target, def.affect.clone());
        let needs_victim = matches!(target, SpellTarget::Caster | SpellTarget::Victim);
        if needs_victim && victim_entity.is_none() {
            if obj.is_some() {
                self.unsupported(
                    "magic_obj_target",
                    "the pack has no spell that acts on an object",
                );
            }
            return;
        }
        if obj.is_some() && info.routines & MAG_ALTER_OBJS != 0 {
            self.unsupported(
                "magic_alter_objs",
                "the pack has no spell that alters an object",
            );
        }
        let sight = matches!(info.id, "detect_invis" | "sense_life");
        if kind == SpellKind::Utility || (kind == SpellKind::Detect && !sight) {
            self.unsupported(
                "magic_spell_inert",
                &format!("spell {spellnum} has no effect in the pack"),
            );
        }
        let id = info.id;
        let level = u32::try_from(level.max(1)).unwrap_or(1);
        let touched = self.cast_candidates(caster, victim, target == SpellTarget::Room);
        let alive_before: Vec<Entity> = touched
            .iter()
            .copied()
            .filter(|entity| {
                self.world
                    .get::<TbaCombatant>(*entity)
                    .is_none_or(|combatant| combatant.position != Position::Dead)
            })
            .collect();
        cast_spell_at_level(self.world, caster_entity, victim_entity, id, level);
        for entity in touched {
            let npc = self.world.get::<Npc>(entity).is_some();
            if npc
                && alive_before.contains(&entity)
                && self
                    .world
                    .get::<TbaCombatant>(entity)
                    .is_some_and(|combatant| combatant.position == Position::Dead)
            {
                // `raw_kill` runs `death_mtrigger(victim, killer)` before the
                // extraction. The pack's spell leaves a dead NPC in place; it
                // goes at the end of the host run like any extracted
                // character, so its death trigger has to run here.
                if let Some(dead) = self.uid(entity) {
                    dg_vm::dispatch::death_mtrigger(self, dead, Some(caster));
                }
                self.world.entity_mut(entity).insert(DgDead);
            }
            if let Some(affect) = &affects
                && self.world.get::<TbaBuffs>(entity).is_some_and(|buffs| {
                    buffs
                        .buffs
                        .iter()
                        .any(|buff| !buff.is_pinned() && &buff.affect == affect)
                })
            {
                self.rebuild(entity);
            }
        }
    }

    /// `affect_from_char(ch, spell)`: the script affects of that spell and
    /// the buffs they made (every pinned affect buff for `SPELL_DG_AFFECT`);
    /// for a spell the pack carries, also the cast buffs equal to its affect
    /// (the pack's buffs carry no spell tag).
    fn magic_remove_dg_affects(&mut self, ch: Uid, spell: i32) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        self.expire_dg_affects(entity);
        let removed: Vec<DgAffect> = match self.world.get_mut::<TbaDgAffects>(entity) {
            Some(mut marks) => {
                let (gone, kept): (Vec<DgAffect>, Vec<DgAffect>) = std::mem::take(&mut marks.0)
                    .into_iter()
                    .partition(|mark| mark.spell == spell);
                marks.0 = kept;
                gone
            }
            None => Vec::new(),
        };
        for mark in &removed {
            self.remove_buff_for(entity, mark);
            self.release_bit(entity, mark);
        }
        if spell == SPELL_DG_AFFECT {
            // A pinned buff no remaining marker accounts for belongs to a
            // script affect whose marker was lost.
            let owned: Vec<AffectDef> = self
                .world
                .get::<TbaDgAffects>(entity)
                .map(|marks| marks.0.iter().filter_map(|m| m.numeric.clone()).collect())
                .unwrap_or_default();
            if let Some(mut buffs) = self.world.get_mut::<TbaBuffs>(entity) {
                buffs.buffs.retain(|buff| {
                    !buff.is_pinned() || buff.is_adjustment() || owned.contains(&buff.affect)
                });
            }
        }
        if spell != SPELL_DG_AFFECT
            && let Some(info) = self.carried_info(spell)
            && let Some(affect) = self
                .content()
                .and_then(|content| content.spells.get(info.id))
                .and_then(|def| def.affect.clone())
            && let Some(mut buffs) = self.world.get_mut::<TbaBuffs>(entity)
        {
            buffs
                .buffs
                .retain(|buff| buff.is_pinned() || buff.affect != affect);
        }
        self.rebuild(entity);
    }

    /// `affect_to_char`: a marker for the affect, a buff for the number it
    /// changes when the pack models that number, and the bit it sets stored
    /// with the marker (hide, sneak and the two true-sight bits also set their
    /// pack component; any other bit is logged as unread). A
    /// duration of -1 is permanent; otherwise the affect lasts until the
    /// `duration + 1`th mud-hour update, counted from now.
    fn magic_add_dg_affect(
        &mut self,
        ch: Uid,
        spell: i32,
        location: i32,
        modifier: i8,
        duration: i16,
        bit: Option<u32>,
    ) {
        self.flush_reads();
        let Some(entity) = self.character(ch) else {
            return;
        };
        self.expire_dg_affects(entity);
        let permanent = duration < 0;
        let hours = i64::from(duration.max(0)) + 1;
        let expires = (!permanent).then_some(hours * HOUR_PULSES);
        let numeric = affect_location(location)
            .filter(|_| modifier != 0)
            .map(|location| AffectDef {
                location,
                modifier: i32::from(modifier),
            });
        if location != 0 && affect_location(location).is_none() {
            self.unsupported(
                "magic_affect_apply",
                &format!("the pack models no apply type {location}"),
            );
        }
        let mut owns_bit = false;
        if let Some(bit) = bit {
            if matches!(
                bit,
                AFF_DETECT_INVIS | AFF_SENSE_LIFE | AFF_SNEAK | AFF_HIDE
            ) {
                let rounds = if permanent {
                    PINNED_FOREVER
                } else {
                    u32::try_from(hours.saturating_mul(HOUR_PULSES) / ROUND_PULSES)
                        .unwrap_or(PINNED_FOREVER)
                };
                owns_bit = self.grant_bit(entity, bit, rounds);
            } else {
                self.unsupported(
                    "magic_affect_bit",
                    "an affect bit is recorded but no pack system reads it",
                );
            }
        }
        if let Some(affect) = &numeric {
            let rounds = if permanent {
                PINNED_FOREVER
            } else {
                u32::try_from(hours.saturating_mul(HOUR_PULSES) / ROUND_PULSES)
                    .unwrap_or(PINNED_FOREVER - 1)
                    .min(PINNED_FOREVER - 1)
            };
            let buff = ActiveBuff::pinned(affect.clone(), rounds, false);
            if let Some(mut buffs) = self.world.get_mut::<TbaBuffs>(entity) {
                buffs.buffs.push(buff);
            } else {
                self.world
                    .entity_mut(entity)
                    .insert(TbaBuffs { buffs: vec![buff] });
            }
        }
        let mark = DgAffect {
            spell,
            location,
            modifier,
            bit,
            expires,
            numeric,
            owns_bit,
        };
        if let Some(mut marks) = self.world.get_mut::<TbaDgAffects>(entity) {
            marks.0.push(mark);
        } else {
            self.world
                .entity_mut(entity)
                .insert(TbaDgAffects(vec![mark]));
        }
        self.rebuild(entity);
    }
}
