//! The character field table of `find_replacement` (`dg_variables.c`).
//!
//! [`char_field`] is the `if (c) { … }` block: `%actor.gold(5)%`,
//! `%self.hitp%`, `%mob.eq(head)%`. The C switches on the first letter of the
//! field and compares with `str_cmp`; here the field is lowercased once and
//! matched whole, which selects the same branch (every name is filed under
//! its own first letter in the C).
//!
//! Also ported from `dg_variables.c`: `skill_percent` and `char_has_item`
//! (with [`crate::fields_obj::item_in_list_c`]
//! and [`crate::fields_obj::get_object_in_equip_c`]). From `spell_parser.c`:
//! `find_skill_num` lives in [`crate::magic`], over the host's spell-name
//! table.
//!
//! # Fidelity notes
//!
//! - A field with a subfield *changes* the character, then returns the new
//!   value. The C stores into narrow fields (`sh_int` hit points and mana,
//!   `sbyte` abilities, damroll, hitroll and conditions) with C's implicit
//!   truncation; `narrow` reproduces it, so `%self.hitp(40000)%` wraps as
//!   it does in the game. Arithmetic on `int` fields wraps.
//! - `level(<n>)` sets the level but leaves the C's result sentinel
//!   untouched, as does `stradd` below 18 strength and `questpoints`/`qp`/
//!   `qpnts` on an NPC. Those return `None`, so the caller falls through to
//!   the character's own variables and the "unknown char field" log, exactly
//!   as the C does.
//! - `is_killer(on)` and `is_thief(on)` set `PLR_KILLER`/`PLR_THIEF` without
//!   an NPC check, so on a mob they set `MOB_SPEC`/`MOB_SENTINEL` (bits 0 and
//!   1 of the same array). Reading them is guarded by `PLR_FLAGGED`.
//! - `npcflag` runs `sprintbitarray` over `action_bits` and then the
//!   flawed [`str_str`]; on a player the same bits are read as player flags.
//!   `sprintbitarray` stops at the end of the name table: bit 20 prints
//!   `UNDEFINED `, higher bits are never looked at. When no bit prints,
//!   `sprintbitarray` leaves `NOBITS ` in the buffer, and that is what the
//!   search runs over (a flagless player matches `NOBITS`, `bit`, `s`, ...).
//! - `skill` with an empty or blank subfield resolves to spell 1, because
//!   the C's `find_skill_num` falls out of its word loop with nothing left in
//!   the name (`ok && !*first2`). Fill words are kept (`any_one_arg`), so
//!   `skill(the)` is an unknown skill. A slot the host leaves undefined is
//!   named `!UNUSED!`, as in the C.
//! - `title` on a player with no title prints `(null)`, which is what the
//!   C's `snprintf("%s", NULL)` gives with glibc.
//! - Deviations where the C would index out of bounds: a sex or position
//!   outside its name table prints `UNDEFINED` (as `sprinttype` does)
//!   rather than reading past the array.
//! - `vnum`, `inventory(<vnum>)` compare vnums as the C's unsigned 16-bit
//!   `IDXTYPE`: a prototype-less entity (host vnum any negative number) is
//!   65535; a vnum above 65535, which the C cannot hold, is passed through.
//!   `has_item(<vnum>)` compares the same way, both sides narrowed to 16 bits.
//! - Player-only attributes on an NPC go to the host's shared `dummy_mob`
//!   record (see [`CharHost`]); the mock keeps one on its globals.
//! - There is no `race` field in this tree's table (tbaMUD has no races);
//!   none is invented.

use crate::cstr::{
    atoi, is_abbrev, is_number, one_word, skip_spaces, sprinttype, str_str, strn_cmp,
};
use crate::fields_obj::{c_vnum, get_object_in_equip_c, item_in_list_c};
use crate::host::{Cx, DgHost};
use crate::lookup::{NUM_WEARS, find_eq_pos_script};
use crate::magic::find_skill_num;
use crate::types::{
    AttachType, LVL_GRGOD, LVL_IMMORT, PULSE_VIOLENCE, ROOM_ID_BASE, Uid, uid_text,
};

/// `PULSE_VIOLENCE / 2`: pulses per unit of `wait(<n>)`.
const WAIT_UNIT: i32 = (PULSE_VIOLENCE / 2) as i32;
/// `POS_SLEEPING` and `POS_STANDING`: the range `pos(<name>)` can set.
const POS_SLEEPING: i32 = 4;
const POS_STANDING: i32 = 8;
/// `MOB_ISNPC`, `MOB_NOTDEADYET` and `PLR_NOTDEADYET` (`structs.h`).
const MOB_ISNPC: u32 = 3;
const MOB_NOTDEADYET: u32 = 19;
const PLR_NOTDEADYET: u32 = 16;
/// `PLR_KILLER` and `PLR_THIEF`.
const PLR_KILLER: u32 = 0;
const PLR_THIEF: u32 = 1;

/// `pc_class_types` (`class.c`).
pub const PC_CLASS_TYPES: &[&str] = &["Magic User", "Cleric", "Thief", "Warrior"];
/// `genders` (`constants.c`).
pub const GENDERS: &[&str] = &["neutral", "male", "female"];
/// `position_types` (`constants.c`).
pub const POSITION_TYPES: &[&str] = &[
    "Dead",
    "Mortally wounded",
    "Incapacitated",
    "Stunned",
    "Sleeping",
    "Resting",
    "Sitting",
    "Fighting",
    "Standing",
];
/// `action_bits` (`constants.c`), the NPC act flag names.
pub const ACTION_BITS: &[&str] = &[
    "SPEC",
    "SENTINEL",
    "SCAVENGER",
    "ISNPC",
    "AWARE",
    "AGGR",
    "STAY-ZONE",
    "WIMPY",
    "AGGR_EVIL",
    "AGGR_GOOD",
    "AGGR_NEUTRAL",
    "MEMORY",
    "HELPER",
    "NO_CHARM",
    "NO_SUMMN",
    "NO_SLEEP",
    "NO_BASH",
    "NO_BLIND",
    "NO_KILL",
    "DEAD",
];
/// `preference_bits` (`constants.c`).
pub const PREFERENCE_BITS: &[&str] = &[
    "BRIEF",
    "COMPACT",
    "NO_SHOUT",
    "NO_TELL",
    "D_HP",
    "D_MANA",
    "D_MOVE",
    "AUTOEX",
    "NO_HASS",
    "QUEST",
    "SUMN",
    "NO_REP",
    "LIGHT",
    "C1",
    "C2",
    "NO_WIZ",
    "L1",
    "L2",
    "NO_AUC",
    "NO_GOS",
    "NO_GTZ",
    "RMFLG",
    "D_AUTO",
    "CLS",
    "BLDWLK",
    "AFK",
    "AUTOLOOT",
    "AUTOGOLD",
    "AUTOSPLIT",
    "AUTOSAC",
    "AUTOASSIST",
    "AUTOMAP",
    "AUTOKEY",
    "AUTODOOR",
    "ZONERESETS",
];

/// A numeric character attribute. [`CharHost::char_stat`] reads it and
/// [`CharHost::char_set_stat`] stores it; each variant is one C macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharStat {
    /// `GET_ALIGNMENT(ch)`.
    Align,
    /// `GET_DAMROLL(ch)` (`sbyte`).
    Damroll,
    /// `GET_HITROLL(ch)` (`sbyte`).
    Hitroll,
    /// `GET_HIT(ch)` (`sh_int`).
    Hit,
    /// `GET_MAX_HIT(ch)` (`sh_int`).
    MaxHit,
    /// `GET_MANA(ch)` (`sh_int`).
    Mana,
    /// `GET_MAX_MANA(ch)` (`sh_int`).
    MaxMana,
    /// `GET_MOVE(ch)` (`sh_int`).
    Move,
    /// `GET_MAX_MOVE(ch)` (`sh_int`).
    MaxMove,
    /// `GET_SEX(ch)`.
    Sex,
    /// `GET_CLASS(ch)`.
    Class,
    /// `GET_POS(ch)`.
    Pos,
    /// `GET_PRACTICES(ch)`.
    Practices,
    /// `GET_QUESTPOINTS(ch)`.
    QuestPoints,
    /// `GET_WAIT_STATE(ch)` (`ch->wait`).
    WaitState,
    /// `GET_COND(ch, DRUNK)` (`sbyte`).
    Drunk,
    /// `GET_COND(ch, HUNGER)` (`sbyte`).
    Hunger,
    /// `GET_COND(ch, THIRST)` (`sbyte`).
    Thirst,
    /// `GET_SAVE(ch, SAVING_BREATH)` (`sh_int`).
    SaveBreath,
    /// `GET_SAVE(ch, SAVING_PARA)` (`sh_int`).
    SavePara,
    /// `GET_SAVE(ch, SAVING_PETRI)` (`sh_int`).
    SavePetri,
    /// `GET_SAVE(ch, SAVING_ROD)` (`sh_int`).
    SaveRod,
    /// `GET_SAVE(ch, SAVING_SPELL)` (`sh_int`).
    SaveSpell,
}

/// A character attribute the field table only reads. Gold and experience are
/// changed through [`CharHost::char_increase_gold`] and
/// [`CharHost::char_gain_exp`]; weight is never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharFact {
    /// `GET_GOLD(ch)`.
    Gold,
    /// `GET_EXP(ch)`.
    Exp,
    /// `GET_WEIGHT(ch)` (`ubyte`).
    Weight,
}

/// An ability score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharAbil {
    Str,
    /// `str_add`.
    StrAdd,
    Int,
    Wis,
    Dex,
    Con,
    Cha,
}

/// The character attributes the field table reads and writes.
///
/// Every method takes a character id that may no longer exist; the neutral
/// value is `0`, `false`, `None` or empty, and a write to nothing is ignored.
/// Player-only attributes (practices, conditions, quest points, skills) on an
/// NPC: the C logs and reads or writes one `dummy_mob` record shared by every
/// mob (`CHECK_PLAYER_SPECIAL`), so a write through one mob is seen through
/// another. A faithful host keeps that one shared record for NPCs.
///
/// Two act bits are facts `WorldHost` already provides, and the VM reads them
/// there and never asks this trait for them, so a host need not store them in
/// the act array: `MOB_ISNPC` (bit 3, `WorldHost::is_npc`) and `DEAD(ch)`
/// (`MOB_NOTDEADYET`, bit 19, for an NPC; `PLR_NOTDEADYET`, bit 16, for a
/// player; `WorldHost::is_dead`).
pub trait CharHost {
    /// The value of one numeric attribute (see [`CharStat`]).
    fn char_stat(&self, ch: Uid, stat: CharStat) -> i32;

    /// A read-only attribute (see [`CharFact`]).
    fn char_fact(&self, ch: Uid, fact: CharFact) -> i32;

    /// Store one numeric attribute: `GET_xxx(ch) = value`. The VM has already
    /// narrowed `value` to the C field's width.
    fn char_set_stat(&mut self, ch: Uid, stat: CharStat, value: i32);

    /// `GET_LEVEL(ch) = level`. Reads go through `WorldHost::level`.
    fn char_set_level(&mut self, ch: Uid, level: i32);

    /// `ch->aff_abils.<abil>`: the ability including affects (`GET_STR(ch)`).
    fn char_abil(&self, ch: Uid, abil: CharAbil) -> i32;

    /// `ch->real_abils.<abil>`: the ability without affects.
    fn char_real_abil(&self, ch: Uid, abil: CharAbil) -> i32;

    /// `ch->real_abils.<abil> = value`.
    fn char_set_real_abil(&mut self, ch: Uid, abil: CharAbil, value: i32);

    /// `affect_total(ch)`: recompute `aff_abils` from `real_abils` and affects.
    fn char_affect_total(&mut self, ch: Uid);

    /// `compute_armor_class(ch)`.
    fn char_armor_class(&self, ch: Uid) -> i32;

    /// `update_pos(ch)`: fix `GET_POS` from `GET_HIT`.
    fn char_update_pos(&mut self, ch: Uid);

    /// `gain_exp(ch, gain)`.
    fn char_gain_exp(&mut self, ch: Uid, gain: i32);

    /// `increase_gold(ch, amt)`.
    fn char_increase_gold(&mut self, ch: Uid, amt: i32);

    /// `FIGHTING(ch)`.
    fn char_fighting(&self, ch: Uid) -> Option<Uid>;

    /// `ch->master`.
    fn char_master(&self, ch: Uid) -> Option<Uid>;

    /// `ch->followers->follower`: the head of the follower list.
    fn char_first_follower(&self, ch: Uid) -> Option<Uid>;

    /// `affected_by_spell(ch, spell)`.
    fn char_affected_by_spell(&self, ch: Uid, spell: i32) -> bool;

    /// `GET_SKILL(ch, skill)`.
    fn char_skill(&self, ch: Uid, skill: i32) -> i32;

    /// `SET_SKILL(ch, skill, value)`.
    fn char_set_skill(&mut self, ch: Uid, skill: i32, value: i32);

    /// `IS_SET_AR(MOB_FLAGS(ch), bit)`: a bit of the act array (`MOB_*` for
    /// an NPC, `PLR_*` for a player). The caller adds any `IS_NPC` test.
    fn char_act_flagged(&self, ch: Uid, bit: u32) -> bool;

    /// `SET_BIT_AR`/`REMOVE_BIT_AR(PLR_FLAGS(ch), bit)`.
    fn char_set_act_flag(&mut self, ch: Uid, bit: u32, on: bool);

    /// `IS_SET_AR(PRF_FLAGS(ch), bit)`. The caller adds the `IS_NPC` test.
    fn char_pref_flagged(&self, ch: Uid, bit: u32) -> bool;

    /// `ch->player.short_descr = strdup(text)`. Reads go through
    /// `WorldHost::name`.
    fn char_set_short_descr(&mut self, ch: Uid, text: &str);

    /// `GET_TITLE(ch)`; `None` when the pointer is NULL.
    fn char_title(&self, ch: Uid) -> Option<String>;

    /// Replace `GET_TITLE(ch)`.
    fn char_set_title(&mut self, ch: Uid, title: &str);

    /// `GET_QUEST(ch)`; `-1` when `GET_QUEST(ch) == NOTHING` (this tree's
    /// unsigned `NOTHING` is read as -1 by the VM; a host may also return the
    /// raw 65535, which no quest has).
    fn char_quest(&self, ch: Uid) -> i32;

    /// `real_quest(vnum) != NOTHING`.
    fn char_quest_exists(&self, vnum: i32) -> bool;

    /// `is_complete(ch, vnum)`.
    fn char_quest_completed(&self, ch: Uid, vnum: i32) -> bool;
}

/// Which C integer type a field is stored in.
#[derive(Clone, Copy)]
enum Width {
    I8,
    I16,
    I32,
}

/// The value C stores when an `int` result is assigned to a field of this
/// width (two's complement truncation).
fn narrow(width: Width, value: i32) -> i32 {
    match width {
        Width::I8 => i32::from(value as i8),
        Width::I16 => i32::from(value as i16),
        Width::I32 => value,
    }
}

/// `skill_percent(ch, skill)` (`dg_variables.c`).
pub fn skill_percent<H: DgHost + ?Sized>(host: &H, ch: Uid, skill: &str) -> String {
    let skillnum = find_skill_num(host, skill);
    if skillnum <= 0 {
        return "unknown skill".to_owned();
    }
    host.char_skill(ch, skillnum).to_string()
}

/// `char_has_item(item, ch)` (`dg_variables.c`): worn, or carried (searching
/// containers), by id, vnum or keyword.
pub fn char_has_item<H: DgHost + ?Sized>(host: &H, item: &str, ch: Uid) -> bool {
    if get_object_in_equip_c(host, ch, item).is_some() {
        return true;
    }
    item_in_list_c(host, item, &host.carrying(ch)) != 0
}

/// `"1"` or `"0"`.
fn flag(on: bool) -> Option<String> {
    Some(if on { "1" } else { "0" }.to_owned())
}

/// A number as text.
fn number(value: i32) -> Option<String> {
    Some(value.to_string())
}

/// A reference to an entity, or empty for none.
fn uid_or_empty(uid: Option<Uid>) -> Option<String> {
    Some(uid.map(uid_text).unwrap_or_default())
}

/// The common shape `if (subfield) stat = f(stat, atoi(subfield)); print stat`.
fn adjust<H: DgHost + ?Sized>(
    host: &mut H,
    c: Uid,
    stat: CharStat,
    sub: &str,
    width: Width,
    f: impl Fn(i32, i32) -> i32,
) -> Option<String> {
    if !sub.is_empty() {
        let value = narrow(width, f(host.char_stat(c, stat), atoi(sub)));
        host.char_set_stat(c, stat, value);
    }
    number(host.char_stat(c, stat))
}

/// An ability field: add, narrow to `sbyte`, clamp to `lo..=hi`, then
/// `affect_total`; prints the effective (`aff_abils`) value.
fn adjust_abil<H: DgHost + ?Sized>(
    host: &mut H,
    c: Uid,
    abil: CharAbil,
    sub: &str,
    lo: i32,
    hi: i32,
) -> Option<String> {
    if !sub.is_empty() {
        let mut value = narrow(
            Width::I8,
            host.char_real_abil(c, abil).wrapping_add(atoi(sub)),
        );
        if value > hi {
            value = hi;
        }
        if value < lo {
            value = lo;
        }
        host.char_set_real_abil(c, abil, value);
        host.char_affect_total(c);
    }
    number(host.char_abil(c, abil))
}

/// The ability cap of `str`, `int`, … : 25 for NPCs and `LVL_GRGOD` and up,
/// else 18.
fn abil_max<H: DgHost + ?Sized>(host: &H, c: Uid) -> i32 {
    if host.is_npc(c) || host.level(c) >= LVL_GRGOD {
        25
    } else {
        18
    }
}

/// A name from a table, `UNDEFINED` past its end (see the module notes).
fn table_name(index: i32, names: &[&str]) -> Option<String> {
    Some(sprinttype(index, names))
}

/// The `if (c) { … }` branch of `find_replacement`: the value of
/// `field(subfield)` on this character.
///
/// Returns `None` when the table leaves the result untouched (the C's
/// `*str == '\x1'` sentinel); the caller then applies the C's fallback (the
/// entity's own script variables, then an "unknown field" log). That is the
/// case for a field the table does not name, and also for the few it does
/// name that write nothing on some path.
pub fn char_field<H: DgHost + ?Sized>(
    cx: &mut Cx<'_, H>,
    c: Uid,
    field: &str,
    subfield: &str,
) -> Option<String> {
    let attach = cx.attach;
    let owner = cx.owner;
    let host: &mut H = &mut *cx.host;
    let sub = subfield;
    let has_sub = !sub.is_empty();
    let lowered = field.to_ascii_lowercase();
    match lowered.as_str() {
        "affect" => {
            if has_sub {
                let spell = find_skill_num(host, sub);
                flag(host.char_affected_by_spell(c, spell))
            } else {
                Some("0".to_owned())
            }
        }
        "alias" => Some(host.keywords(c)),
        "align" => adjust(host, c, CharStat::Align, sub, Width::I32, |_, add| {
            add.clamp(-1000, 1000)
        }),
        "armor" => number(host.char_armor_class(c)),
        "canbeseen" => {
            if attach == AttachType::Mob && !host.can_see(owner, c) {
                Some("0".to_owned())
            } else {
                Some("1".to_owned())
            }
        }
        "cha" => {
            let max = abil_max(host, c);
            adjust_abil(host, c, CharAbil::Cha, sub, 3, max)
        }
        "class" => {
            if has_sub {
                match PC_CLASS_TYPES
                    .iter()
                    .position(|class| is_abbrev(sub, class))
                    .and_then(|index| i32::try_from(index).ok())
                {
                    Some(class) => {
                        host.char_set_stat(c, CharStat::Class, class);
                        Some("1".to_owned())
                    }
                    None => Some("0".to_owned()),
                }
            } else {
                table_name(host.char_stat(c, CharStat::Class), PC_CLASS_TYPES)
            }
        }
        "con" => {
            let max = abil_max(host, c);
            adjust_abil(host, c, CharAbil::Con, sub, 3, max)
        }
        "damroll" => adjust(host, c, CharStat::Damroll, sub, Width::I8, |cur, add| {
            cur.wrapping_add(add).max(1)
        }),
        "dex" => {
            let max = abil_max(host, c);
            adjust_abil(host, c, CharAbil::Dex, sub, 3, max)
        }
        "drunk" => adjust(host, c, CharStat::Drunk, sub, Width::I8, |_, add| {
            add.clamp(-1, 24)
        }),
        "eq" => {
            if !has_sub {
                Some(String::new())
            } else if sub.starts_with('*') {
                let any = (0..NUM_WEARS).any(|pos| host.equipment(c, pos).is_some());
                Some(if any { "1" } else { "" }.to_owned())
            } else {
                match find_eq_pos_script(sub).and_then(|pos| host.equipment(c, pos)) {
                    Some(obj) => Some(uid_text(obj)),
                    None => Some(String::new()),
                }
            }
        }
        "exp" => {
            if has_sub {
                host.char_gain_exp(c, atoi(sub).min(1000));
            }
            number(host.char_fact(c, CharFact::Exp))
        }
        "fighting" => uid_or_empty(host.char_fighting(c)),
        "follower" => uid_or_empty(host.char_first_follower(c)),
        "gold" => {
            if has_sub {
                host.char_increase_gold(c, atoi(sub));
            }
            number(host.char_fact(c, CharFact::Gold))
        }
        "has_item" => {
            if has_sub {
                number(i32::from(char_has_item(host, sub, c)))
            } else {
                Some(String::new())
            }
        }
        "hasattached" => {
            if !has_sub || !host.is_npc(c) {
                Some(String::new())
            } else {
                let vnum = atoi(sub);
                flag(host.script(c).is_some_and(|sc| sc.is_attached(vnum)))
            }
        }
        "heshe" => Some(pronoun(host, c, ["it", "he", "she"])),
        "himher" => Some(pronoun(host, c, ["it", "him", "her"])),
        "hisher" => Some(pronoun(host, c, ["its", "his", "her"])),
        "hitp" => {
            if has_sub {
                let value = narrow(
                    Width::I16,
                    host.char_stat(c, CharStat::Hit).wrapping_add(atoi(sub)),
                );
                host.char_set_stat(c, CharStat::Hit, value);
                host.char_update_pos(c);
            }
            number(host.char_stat(c, CharStat::Hit))
        }
        "hitroll" => adjust(host, c, CharStat::Hitroll, sub, Width::I8, |cur, add| {
            cur.wrapping_add(add).max(1)
        }),
        "hunger" => adjust(host, c, CharStat::Hunger, sub, Width::I8, |_, add| {
            add.clamp(-1, 24)
        }),
        "id" => Some(c.to_string()),
        "is_pc" => flag(!host.is_npc(c)),
        "int" => {
            let max = abil_max(host, c);
            adjust_abil(host, c, CharAbil::Int, sub, 3, max)
        }
        "inventory" => {
            let carrying = host.carrying(c);
            if has_sub {
                let vnum = atoi(sub);
                let found = carrying
                    .iter()
                    .copied()
                    .find(|obj| c_vnum(host.vnum(*obj)) == vnum);
                uid_or_empty(found)
            } else {
                uid_or_empty(carrying.first().copied())
            }
        }
        "is_killer" => plr_flag(host, c, sub, PLR_KILLER),
        "is_thief" => plr_flag(host, c, sub, PLR_THIEF),
        "level" => {
            if has_sub {
                let level = atoi(sub).clamp(0, LVL_IMMORT - 1);
                host.char_set_level(c, level);
                None
            } else {
                number(host.level(c))
            }
        }
        "mana" => adjust(host, c, CharStat::Mana, sub, Width::I16, |cur, add| {
            cur.wrapping_add(add)
        }),
        "master" => uid_or_empty(host.char_master(c)),
        "maxhitp" => adjust(host, c, CharStat::MaxHit, sub, Width::I16, |cur, add| {
            cur.wrapping_add(add).max(1)
        }),
        "maxmana" => adjust(host, c, CharStat::MaxMana, sub, Width::I16, |cur, add| {
            cur.wrapping_add(add).max(1)
        }),
        "maxmove" => adjust(host, c, CharStat::MaxMove, sub, Width::I16, |cur, add| {
            cur.wrapping_add(add).max(1)
        }),
        "move" => adjust(host, c, CharStat::Move, sub, Width::I16, |cur, add| {
            cur.wrapping_add(add)
        }),
        "name" => Some(host.name(c)),
        "next_in_room" => {
            let next = host.in_room(c).and_then(|room| {
                let people = host.room_people(room);
                let at = people.iter().position(|who| *who == c)?;
                people.get(at + 1).copied()
            });
            uid_or_empty(next)
        }
        "npcflag" => {
            if has_sub {
                let mut buf = String::new();
                let npc = host.is_npc(c);
                for (bit, name) in (0u32..).zip(ACTION_BITS.iter()) {
                    // MOB_ISNPC and the NOTDEADYET bit (MOB_NOTDEADYET for an
                    // NPC, PLR_NOTDEADYET for a player, both the `DEAD(ch)`
                    // fact) are not kept in the act array.
                    let set = if bit == MOB_ISNPC {
                        npc
                    } else if bit == if npc { MOB_NOTDEADYET } else { PLR_NOTDEADYET } {
                        host.is_dead(c)
                    } else {
                        host.char_act_flagged(c, bit)
                    };
                    if set {
                        buf.push_str(name);
                        buf.push(' ');
                    }
                }
                // The name table's "\n" terminator is bit 20: a set bit there
                // prints UNDEFINED, and the C stops after it.
                if host.char_act_flagged(c, 20) {
                    buf.push_str("UNDEFINED ");
                }
                // sprintbitarray's fallback when nothing was printed.
                if buf.is_empty() {
                    buf.push_str("NOBITS ");
                }
                flag(str_str(&buf, sub).is_some())
            } else {
                Some("0".to_owned())
            }
        }
        "pos" => {
            if has_sub {
                for pos in POS_SLEEPING..=POS_STANDING {
                    let name = usize::try_from(pos)
                        .ok()
                        .and_then(|index| POSITION_TYPES.get(index))
                        .copied()
                        .unwrap_or("");
                    if strn_cmp(sub, name, sub.len()) == 0 {
                        host.char_set_stat(c, CharStat::Pos, pos);
                        break;
                    }
                }
            }
            table_name(host.char_stat(c, CharStat::Pos), POSITION_TYPES)
        }
        "prac" => adjust(host, c, CharStat::Practices, sub, Width::I32, |cur, add| {
            cur.wrapping_add(add).max(0)
        }),
        "pref" => {
            if has_sub {
                let pref = PREFERENCE_BITS.iter().position(|name| *name == sub);
                let on = !host.is_npc(c)
                    && pref
                        .and_then(|bit| u32::try_from(bit).ok())
                        .is_some_and(|bit| host.char_pref_flagged(c, bit));
                flag(on)
            } else {
                Some("0".to_owned())
            }
        }
        "questpoints" | "qp" | "qpnts" if !host.is_npc(c) => adjust(
            host,
            c,
            CharStat::QuestPoints,
            sub,
            Width::I32,
            |cur, add| cur.wrapping_add(add),
        ),
        "quest" => {
            if !host.is_npc(c) {
                let quest = host.char_quest(c);
                if quest != -1 && host.char_quest_exists(quest) {
                    return number(quest);
                }
            }
            Some("0".to_owned())
        }
        "questdone" => {
            if !host.is_npc(c) && has_sub {
                flag(host.char_quest_completed(c, atoi(sub)))
            } else {
                Some("0".to_owned())
            }
        }
        "room" => {
            // ACTOR_ROOM_IS_UID: the room's id, or the void's when nowhere.
            let room = host.in_room(c).unwrap_or(ROOM_ID_BASE);
            Some(uid_text(room))
        }
        "saving_breath" => save(host, c, CharStat::SaveBreath, sub),
        "saving_para" => save(host, c, CharStat::SavePara, sub),
        "saving_petri" => save(host, c, CharStat::SavePetri, sub),
        "saving_rod" => save(host, c, CharStat::SaveRod, sub),
        "saving_spell" => save(host, c, CharStat::SaveSpell, sub),
        "sex" => table_name(host.char_stat(c, CharStat::Sex), GENDERS),
        "skill" => Some(skill_percent(host, c, sub)),
        "skillset" => {
            if !host.is_npc(c) && has_sub {
                let (skillname, rest) = one_word(sub);
                let amount = skip_spaces(rest);
                if !amount.is_empty() && is_number(amount) {
                    let skillnum = find_skill_num(host, &skillname);
                    if skillnum > 0 {
                        host.char_set_skill(c, skillnum, atoi(amount).clamp(0, 100));
                    }
                }
            }
            // So the caller knows 'skillset' was recognised.
            Some(String::new())
        }
        "str" => {
            let max = abil_max(host, c);
            adjust_abil(host, c, CharAbil::Str, sub, 3, max)
        }
        "stradd" => {
            if host.char_abil(c, CharAbil::Str) >= 18 {
                adjust_abil(host, c, CharAbil::StrAdd, sub, 0, 100)
            } else {
                None
            }
        }
        "thirst" => adjust(host, c, CharStat::Thirst, sub, Width::I8, |_, add| {
            add.clamp(-1, 24)
        }),
        "title" => {
            if !host.is_npc(c) && has_sub && host.valid_dg_target(c, true) {
                host.char_set_title(c, sub);
            }
            if host.is_npc(c) {
                Some(String::new())
            } else {
                Some(host.char_title(c).unwrap_or_else(|| "(null)".to_owned()))
            }
        }
        "varexists" => {
            let found = host.script(c).is_some_and(|sc| {
                sc.global_vars
                    .vars
                    .iter()
                    .any(|var| var.name.eq_ignore_ascii_case(sub))
            });
            flag(found)
        }
        "vnum" => {
            if has_sub {
                let same = host.is_npc(c) && c_vnum(host.vnum(c)) == atoi(sub);
                number(i32::from(same))
            } else if host.is_npc(c) {
                number(c_vnum(host.vnum(c)))
            } else {
                Some("-1".to_owned())
            }
        }
        "weight" => number(host.char_fact(c, CharFact::Weight)),
        "wis" => {
            let max = abil_max(host, c);
            adjust_abil(host, c, CharAbil::Wis, sub, 3, max)
        }
        "wait" => {
            if has_sub {
                let wait = atoi(sub).wrapping_mul(WAIT_UNIT);
                host.char_set_stat(c, CharStat::WaitState, wait);
            }
            number(host.char_stat(c, CharStat::WaitState))
        }
        _ => None,
    }
}

/// `HSSH`/`HMHR`/`HSHR`: `words` is the neutral, male and female form.
fn pronoun<H: DgHost + ?Sized>(host: &H, c: Uid, words: [&str; 3]) -> String {
    let sex = host.char_stat(c, CharStat::Sex);
    let word = match sex {
        0 => words[0],
        1 => words[1],
        _ => words[2],
    };
    word.to_owned()
}

/// The `saving_*` fields: add to a `sh_int`.
fn save<H: DgHost + ?Sized>(host: &mut H, c: Uid, stat: CharStat, sub: &str) -> Option<String> {
    adjust(host, c, stat, sub, Width::I16, |cur, add| {
        cur.wrapping_add(add)
    })
}

/// `is_killer`/`is_thief`: `on`/`off` set or clear the bit (with no NPC
/// check); the value is `PLR_FLAGGED`.
fn plr_flag<H: DgHost + ?Sized>(host: &mut H, c: Uid, sub: &str, bit: u32) -> Option<String> {
    if crate::cstr::str_eq(sub, "on") {
        host.char_set_act_flag(c, bit, true);
    } else if crate::cstr::str_eq(sub, "off") {
        host.char_set_act_flag(c, bit, false);
    }
    flag(!host.is_npc(c) && host.char_act_flagged(c, bit))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::host::{BaseHost, WorldHost};
    use crate::mock::MockHost;
    use crate::types::{TrigInstance, UID_CHAR};

    struct Fx {
        host: MockHost,
        trig: TrigInstance,
        mob: Uid,
        pc: Uid,
    }

    fn fx() -> Fx {
        let mut host = MockHost::new();
        let room = host.add_room(3001, "Temple");
        let mob = host.add_mob(3000, "the guard");
        let pc = host.add_player("Bob");
        host.char_to_room(mob, room);
        host.char_to_room(pc, room);
        host.set_num(pc, "level", 5);
        host.set_num(mob, "level", 10);
        let proto = host.add_proto(1, AttachType::Mob, "g", 100, "", "say hi");
        Fx {
            host,
            trig: TrigInstance::new(1, proto),
            mob,
            pc,
        }
    }

    impl Fx {
        fn get(&mut self, c: Uid, field: &str, sub: &str) -> Option<String> {
            let owner = self.mob;
            let mut cx = Cx {
                host: &mut self.host,
                owner,
                attach: AttachType::Mob,
                trig: &mut self.trig,
            };
            char_field(&mut cx, c, field, sub)
        }

        fn s(&mut self, c: Uid, field: &str, sub: &str) -> String {
            self.get(c, field, sub).expect("field recognised")
        }
    }

    fn spells(host: &mut MockHost) {
        host.globals
            .texts
            .insert("spell:1".to_owned(), "armor".to_owned());
        host.globals
            .texts
            .insert("spell:2".to_owned(), "magic missile".to_owned());
        host.globals
            .texts
            .insert("spell:3".to_owned(), "detect invisibility".to_owned());
    }

    #[test]
    fn names_and_identity() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        assert_eq!(f.s(mob, "name", ""), "the guard");
        assert_eq!(f.s(pc, "NAME", ""), "Bob");
        assert_eq!(f.s(pc, "alias", ""), "Bob");
        assert_eq!(f.s(mob, "id", ""), mob.to_string());
        assert_eq!(f.s(pc, "is_pc", ""), "1");
        assert_eq!(f.s(mob, "is_pc", ""), "0");
        assert_eq!(f.s(mob, "vnum", ""), "3000");
        assert_eq!(f.s(pc, "vnum", ""), "-1");
        assert_eq!(f.s(mob, "vnum", "3000"), "1");
        assert_eq!(f.s(mob, "vnum", "3001"), "0");
        // A subfield on a player is always false, even for -1.
        assert_eq!(f.s(pc, "vnum", "-1"), "0");
        assert_eq!(f.s(pc, "level", ""), "5");
        assert_eq!(f.s(mob, "weight", ""), "0");
    }

    #[test]
    fn unknown_and_unnamed_fields_are_none() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        assert_eq!(f.get(mob, "nosuch", ""), None);
        assert_eq!(f.get(mob, "", ""), None);
        assert_eq!(f.get(mob, "ça", ""), None);
        // The `global` case is intercepted by subst; for a player the switch
        // has no such name.
        assert_eq!(f.get(pc, "global", "x"), None);
        // Quest points are guarded by !IS_NPC and have no other branch.
        assert_eq!(f.get(mob, "questpoints", ""), None);
        assert_eq!(f.get(mob, "qp", "5"), None);
        assert_eq!(f.get(mob, "qpnts", ""), None);
        // level(<n>) writes and leaves the sentinel.
        assert_eq!(f.get(pc, "level", "7"), None);
        assert_eq!(f.host.num(pc, "level"), 7);
    }

    #[test]
    fn level_is_bounded_when_set() {
        let mut f = fx();
        let pc = f.pc;
        f.get(pc, "level", "99");
        assert_eq!(f.host.num(pc, "level"), 30);
        f.get(pc, "level", "-4");
        assert_eq!(f.host.num(pc, "level"), 0);
        f.get(pc, "level", "31");
        assert_eq!(f.host.num(pc, "level"), 30);
        f.get(pc, "level", "abc");
        assert_eq!(f.host.num(pc, "level"), 0);
    }

    #[test]
    fn alignment_clamps() {
        let mut f = fx();
        let pc = f.pc;
        assert_eq!(f.s(pc, "align", ""), "0");
        assert_eq!(f.s(pc, "align", "5000"), "1000");
        assert_eq!(f.s(pc, "align", "-5000"), "-1000");
        // It sets, it does not add.
        assert_eq!(f.s(pc, "align", "40"), "40");
        assert_eq!(f.s(pc, "align", "10"), "10");
    }

    #[test]
    fn points_pools_wrap_like_shorts() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "hit", 20);
        f.host.set_num(pc, "pos", 8);
        assert_eq!(f.s(pc, "hitp", ""), "20");
        assert_eq!(f.s(pc, "hitp", "-5"), "15");
        // 15 + 40000 = 40015, which as a sh_int is 40015 - 65536.
        assert_eq!(f.s(pc, "hitp", "40000"), "-25521");
        assert_eq!(f.s(pc, "mana", "7"), "7");
        assert_eq!(f.s(pc, "mana", "-10"), "-3");
        assert_eq!(f.s(pc, "move", "100"), "100");
        // An overflowing atoi wraps in int, then truncates to short.
        // atoi wraps to 1215752191; +100 = 1215752291, low 16 bits 59491 = -6045.
        assert_eq!(f.s(pc, "move", "99999999999"), "-6045");
    }

    #[test]
    fn hitp_updates_position() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "hit", 10);
        f.host.set_num(pc, "pos", 8);
        // hit 10 - 12 = -2 and pos > STUNNED(3): update_pos falls to STUNNED.
        assert_eq!(f.s(pc, "hitp", "-12"), "-2");
        assert_eq!(f.host.num(pc, "pos"), 3);
        assert_eq!(f.s(pc, "hitp", "-5"), "-7");
        assert_eq!(f.host.num(pc, "pos"), 1);
        assert_eq!(f.s(pc, "hitp", "-10"), "-17");
        assert_eq!(f.host.num(pc, "pos"), 0);
        assert_eq!(f.s(pc, "hitp", "30"), "13");
        assert_eq!(f.host.num(pc, "pos"), 8);
        assert_eq!(f.host.num(pc, "hit"), 13);
        // A plain read does not touch the position.
        f.host.set_num(pc, "pos", 5);
        f.s(pc, "hitp", "");
        assert_eq!(f.host.num(pc, "pos"), 5);
    }

    #[test]
    fn maximums_floor_at_one() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "max_hit", 50);
        f.host.set_num(pc, "max_mana", 10);
        f.host.set_num(pc, "max_move", 10);
        assert_eq!(f.s(pc, "maxhitp", "-100"), "1");
        assert_eq!(f.s(pc, "maxhitp", "9"), "10");
        assert_eq!(f.s(pc, "maxmana", "-10"), "1");
        assert_eq!(f.s(pc, "maxmana", ""), "1");
        assert_eq!(f.s(pc, "maxmove", "5"), "15");
        assert_eq!(f.s(pc, "maxmove", "-99"), "1");
        // 10 + 40000 wraps as a short.
        assert_eq!(f.s(pc, "maxhitp", "40000"), "-25526");
    }

    #[test]
    fn rolls_floor_at_one_and_wrap_as_bytes() {
        let mut f = fx();
        let pc = f.pc;
        assert_eq!(f.s(pc, "damroll", ""), "0");
        assert_eq!(f.s(pc, "damroll", "-5"), "1");
        assert_eq!(f.s(pc, "damroll", "10"), "11");
        // 11 + 120 = 131, an sbyte -125.
        assert_eq!(f.s(pc, "damroll", "120"), "-125");
        assert_eq!(f.s(pc, "hitroll", "-9"), "1");
        assert_eq!(f.s(pc, "hitroll", "4"), "5");
    }

    #[test]
    fn conditions_are_set_and_clamped() {
        let mut f = fx();
        let pc = f.pc;
        for field in ["drunk", "hunger", "thirst"] {
            assert_eq!(f.s(pc, field, ""), "0");
            assert_eq!(f.s(pc, field, "10"), "10");
            assert_eq!(f.s(pc, field, "10"), "10");
            assert_eq!(f.s(pc, field, "99"), "24");
            assert_eq!(f.s(pc, field, "-7"), "-1");
        }
        assert_eq!(f.host.num(pc, "cond:0"), -1);
    }

    #[test]
    fn abilities_clamp_by_level_and_recompute() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        f.host.set_num(pc, "rabil:str", 14);
        f.host.set_num(pc, "abil:str", 14);
        assert_eq!(f.s(pc, "str", ""), "14");
        assert_eq!(f.s(pc, "str", "2"), "16");
        assert_eq!(f.s(pc, "str", "50"), "18");
        assert_eq!(f.s(pc, "str", "-50"), "3");
        assert!(f.host.effects.iter().any(|e| e.starts_with("affect_total")));
        // A mob caps at 25.
        assert_eq!(f.s(mob, "dex", "100"), "25");
        // A greater god also caps at 25.
        f.host.set_num(pc, "level", 33);
        assert_eq!(f.s(pc, "int", "100"), "25");
        f.host.set_num(pc, "level", 32);
        assert_eq!(f.s(pc, "wis", "100"), "18");
        f.host.set_num(pc, "rabil:con", 18);
        f.host.set_num(pc, "rabil:cha", 17);
        assert_eq!(f.s(pc, "con", "-1"), "17");
        assert_eq!(f.s(pc, "cha", "1"), "18");
        // Sbyte truncation: 18 + 120 = 138 -> -118 -> floor 3.
        assert_eq!(f.s(pc, "cha", "120"), "3");
    }

    #[test]
    fn stradd_needs_eighteen_strength() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "abil:str", 17);
        f.host.set_num(pc, "rabil:str", 17);
        assert_eq!(f.get(pc, "stradd", ""), None);
        assert_eq!(f.get(pc, "stradd", "10"), None);
        assert_eq!(f.host.num(pc, "rabil:stradd"), 0);
        f.host.set_num(pc, "abil:str", 18);
        f.host.set_num(pc, "rabil:str", 18);
        assert_eq!(f.s(pc, "stradd", ""), "0");
        assert_eq!(f.s(pc, "stradd", "60"), "60");
        assert_eq!(f.s(pc, "stradd", "60"), "100");
        // 100 - 120 = -20, floored at 0.
        assert_eq!(f.s(pc, "stradd", "-120"), "0");
    }

    #[test]
    fn gold_and_exp_go_through_the_game() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        f.host.set_num(pc, "gold", 100);
        assert_eq!(f.s(pc, "gold", ""), "100");
        assert_eq!(f.s(pc, "gold", "50"), "150");
        assert_eq!(f.s(pc, "gold", "-1000"), "0");
        assert_eq!(f.s(pc, "gold", "2147483647"), "2140000000");
        f.host.set_num(mob, "exp", 10);
        assert_eq!(f.s(mob, "exp", ""), "10");
        // Capped at 1000 per call; an NPC just adds.
        assert_eq!(f.s(mob, "exp", "5000"), "1010");
        assert_eq!(f.s(mob, "exp", "-4"), "1006");
        assert!(f.host.effects.iter().any(|e| e.starts_with("gain_exp")));
    }

    #[test]
    fn practices_and_quest_points_are_full_ints() {
        // spells_to_learn and questpoints are `int` in structs.h.
        let mut f = fx();
        let pc = f.pc;
        assert_eq!(f.s(pc, "prac", "40000"), "40000");
        assert_eq!(f.s(pc, "prac", "30000"), "70000");
        assert_eq!(f.s(pc, "questpoints", "100000"), "100000");
        assert_eq!(f.s(pc, "qp", "-1"), "99999");
    }

    #[test]
    fn practices_quest_points_and_saves() {
        let mut f = fx();
        let pc = f.pc;
        assert_eq!(f.s(pc, "prac", "3"), "3");
        assert_eq!(f.s(pc, "prac", "-10"), "0");
        assert_eq!(f.s(pc, "questpoints", "5"), "5");
        assert_eq!(f.s(pc, "qp", "-8"), "-3");
        assert_eq!(f.s(pc, "QPNTS", ""), "-3");
        for field in [
            "saving_breath",
            "saving_para",
            "saving_petri",
            "saving_rod",
            "saving_spell",
        ] {
            assert_eq!(f.s(pc, field, ""), "0");
            assert_eq!(f.s(pc, field, "-4"), "-4");
            assert_eq!(f.s(pc, field, "4"), "0");
        }
        // Each save is its own slot.
        f.s(pc, "saving_rod", "9");
        assert_eq!(f.s(pc, "saving_spell", ""), "0");
    }

    #[test]
    fn wait_state_scales_by_ten() {
        let mut f = fx();
        let pc = f.pc;
        assert_eq!(f.s(pc, "wait", ""), "0");
        assert_eq!(f.s(pc, "wait", "3"), "30");
        assert_eq!(f.s(pc, "wait", "-1"), "-10");
        assert_eq!(f.s(pc, "wait", "x"), "0");
    }

    #[test]
    fn armor_and_weight_read() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "ac", -35);
        f.host.set_num(pc, "weight", 150);
        assert_eq!(f.s(pc, "armor", ""), "-35");
        assert_eq!(f.s(pc, "weight", ""), "150");
    }

    #[test]
    fn sex_class_pos_tables() {
        let mut f = fx();
        let pc = f.pc;
        assert_eq!(f.s(pc, "sex", ""), "neutral");
        f.host.set_num(pc, "sex", 1);
        assert_eq!(f.s(pc, "sex", ""), "male");
        assert_eq!(f.s(pc, "heshe", ""), "he");
        assert_eq!(f.s(pc, "himher", ""), "him");
        assert_eq!(f.s(pc, "hisher", ""), "his");
        f.host.set_num(pc, "sex", 2);
        assert_eq!(f.s(pc, "sex", ""), "female");
        assert_eq!(f.s(pc, "heshe", ""), "she");
        assert_eq!(f.s(pc, "himher", ""), "her");
        assert_eq!(f.s(pc, "hisher", ""), "her");
        f.host.set_num(pc, "sex", 0);
        assert_eq!(f.s(pc, "heshe", ""), "it");
        assert_eq!(f.s(pc, "himher", ""), "it");
        assert_eq!(f.s(pc, "hisher", ""), "its");
        f.host.set_num(pc, "sex", 9);
        assert_eq!(f.s(pc, "sex", ""), "UNDEFINED");
        assert_eq!(f.s(pc, "heshe", ""), "she");

        assert_eq!(f.s(pc, "class", ""), "Magic User");
        assert_eq!(f.s(pc, "class", "war"), "1");
        assert_eq!(f.host.num(pc, "class"), 3);
        assert_eq!(f.s(pc, "class", ""), "Warrior");
        assert_eq!(f.s(pc, "class", "CLER"), "1");
        assert_eq!(f.s(pc, "class", ""), "Cleric");
        assert_eq!(f.s(pc, "class", "paladin"), "0");
        assert_eq!(f.s(pc, "class", ""), "Cleric");
        f.host.set_num(pc, "class", 7);
        assert_eq!(f.s(pc, "class", ""), "UNDEFINED");

        f.host.set_num(pc, "pos", 8);
        assert_eq!(f.s(pc, "pos", ""), "Standing");
        assert_eq!(f.s(pc, "pos", "sle"), "Sleeping");
        assert_eq!(f.host.num(pc, "pos"), 4);
        assert_eq!(f.s(pc, "pos", "SIT"), "Sitting");
        assert_eq!(f.s(pc, "pos", "f"), "Fighting");
        assert_eq!(f.s(pc, "pos", "re"), "Resting");
        // "S" is a prefix of Sleeping first.
        assert_eq!(f.s(pc, "pos", "s"), "Sleeping");
        // Dead is not settable; no match leaves it alone.
        assert_eq!(f.s(pc, "pos", "dead"), "Sleeping");
        assert_eq!(f.s(pc, "pos", "zzz"), "Sleeping");
        f.host.set_num(pc, "pos", 0);
        assert_eq!(f.s(pc, "pos", ""), "Dead");
        f.host.set_num(pc, "pos", 40);
        assert_eq!(f.s(pc, "pos", ""), "UNDEFINED");
    }

    #[test]
    fn links_between_characters() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        let other = f.host.add_mob(3002, "a rat");
        assert_eq!(f.s(mob, "fighting", ""), "");
        assert_eq!(f.s(mob, "master", ""), "");
        assert_eq!(f.s(mob, "follower", ""), "");
        f.host.set_link(mob, "fighting", Some(pc));
        f.host.set_link(mob, "master", Some(other));
        f.host
            .entity_mut(mob)
            .unwrap()
            .lists
            .insert("followers".to_owned(), vec![pc, other]);
        assert_eq!(f.s(mob, "fighting", ""), format!("{UID_CHAR}{pc}"));
        assert_eq!(f.s(mob, "master", ""), format!("{UID_CHAR}{other}"));
        assert_eq!(f.s(mob, "follower", ""), format!("{UID_CHAR}{pc}"));
    }

    #[test]
    fn next_in_room_and_room() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        // The room's people are newest first: pc, mob.
        assert_eq!(f.s(pc, "next_in_room", ""), format!("{UID_CHAR}{mob}"));
        assert_eq!(f.s(mob, "next_in_room", ""), "");
        assert_eq!(
            f.s(pc, "room", ""),
            format!("{UID_CHAR}{}", ROOM_ID_BASE + 3001)
        );
        let nowhere = f.host.add_mob(1, "x");
        assert_eq!(
            f.s(nowhere, "room", ""),
            format!("{UID_CHAR}{ROOM_ID_BASE}")
        );
        assert_eq!(f.s(nowhere, "next_in_room", ""), "");
    }

    #[test]
    fn equipment() {
        let mut f = fx();
        let mob = f.mob;
        let sword = f.host.add_obj(100, "sword");
        let cap = f.host.add_obj(101, "cap");
        assert_eq!(f.s(mob, "eq", ""), "");
        assert_eq!(f.s(mob, "eq", "*"), "");
        assert_eq!(f.s(mob, "eq", "wield"), "");
        f.host.equip_char(mob, sword, 16);
        f.host.equip_char(mob, cap, 6);
        assert_eq!(f.s(mob, "eq", "*"), "1");
        assert_eq!(f.s(mob, "eq", "*anything"), "1");
        assert_eq!(f.s(mob, "eq", "wield"), format!("{UID_CHAR}{sword}"));
        assert_eq!(f.s(mob, "eq", "head"), format!("{UID_CHAR}{cap}"));
        assert_eq!(f.s(mob, "eq", "6"), format!("{UID_CHAR}{cap}"));
        assert_eq!(f.s(mob, "eq", "feet"), "");
        assert_eq!(f.s(mob, "eq", "nonsense"), "");
        assert_eq!(f.s(mob, "eq", "99"), "");
    }

    #[test]
    fn has_item_finds_an_object_worn_only_in_the_last_position() {
        // get_object_in_equip: `for (j = 0; j < NUM_WEARS; j++)`, WEAR_HOLD
        // (17) included.
        let mut f = fx();
        let mob = f.mob;
        let held = f.host.add_obj(104, "torch");
        f.host.equip_char(mob, held, 17);
        assert_eq!(f.s(mob, "has_item", "104"), "1");
        assert_eq!(f.s(mob, "has_item", "105"), "0");
        // A prototype-less object is 65535, and a script's number narrows
        // to 16 bits: 68536 == 65535 + 1 is 0, 131071 is 65535.
        let corpse = f.host.add_obj(-2, "corpse");
        f.host.equip_char(mob, corpse, 3);
        assert_eq!(f.s(mob, "has_item", "65535"), "1");
        assert_eq!(f.s(mob, "has_item", "131071"), "1");
    }

    #[test]
    fn inventory_and_has_item() {
        let mut f = fx();
        let mob = f.mob;
        let a = f.host.add_obj(100, "red apple");
        let b = f.host.add_obj(101, "bag");
        let c = f.host.add_obj(102, "gem");
        f.host.set_num(b, "type", 15);
        let worn = f.host.add_obj(103, "hat");
        assert_eq!(f.s(mob, "inventory", ""), "");
        assert_eq!(f.s(mob, "inventory", "100"), "");
        f.host.obj_to_char(a, mob);
        f.host.obj_to_char(b, mob);
        f.host.obj_to_obj(c, b);
        f.host.equip_char(mob, worn, 6);
        // Head of the list is the newest.
        assert_eq!(f.s(mob, "inventory", ""), format!("{UID_CHAR}{b}"));
        assert_eq!(f.s(mob, "inventory", "100"), format!("{UID_CHAR}{a}"));
        // Only the top level is searched, and only by vnum.
        assert_eq!(f.s(mob, "inventory", "102"), "");
        assert_eq!(f.s(mob, "inventory", "103"), "");
        assert_eq!(f.s(mob, "inventory", "apple"), "");
        assert_eq!(f.s(mob, "has_item", ""), "");
        assert_eq!(f.s(mob, "has_item", "100"), "1");
        assert_eq!(f.s(mob, "has_item", "102"), "1");
        assert_eq!(f.s(mob, "has_item", "gem"), "1");
        assert_eq!(f.s(mob, "has_item", "hat"), "1");
        assert_eq!(f.s(mob, "has_item", "103"), "1");
        assert_eq!(f.s(mob, "has_item", &format!("{UID_CHAR}{c}")), "1");
        assert_eq!(f.s(mob, "has_item", "999"), "0");
        assert_eq!(f.s(mob, "has_item", "sword"), "0");
        assert!(char_has_item(&f.host, "apple", mob));
    }

    #[test]
    fn can_be_seen_uses_the_running_mob() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        assert_eq!(f.s(pc, "canbeseen", ""), "1");
        f.host
            .entity_mut(mob)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![pc]);
        assert_eq!(f.s(pc, "canbeseen", ""), "0");
        // Only mob triggers consult CAN_SEE.
        let owner = mob;
        let mut cx = Cx {
            host: &mut f.host,
            owner,
            attach: AttachType::Obj,
            trig: &mut f.trig,
        };
        assert_eq!(
            char_field(&mut cx, pc, "canbeseen", ""),
            Some("1".to_owned())
        );
    }

    #[test]
    fn affect_and_skill_names() {
        let mut f = fx();
        spells(&mut f.host);
        let pc = f.pc;
        assert_eq!(f.s(pc, "affect", ""), "0");
        assert_eq!(f.s(pc, "affect", "armor"), "0");
        f.host.set_num(pc, "spell:1", 1);
        assert_eq!(f.s(pc, "affect", "armor"), "1");
        assert_eq!(f.s(pc, "affect", "arm"), "1");
        f.host.set_num(pc, "spell:3", 1);
        assert_eq!(f.s(pc, "affect", "detect inv"), "1");
        assert_eq!(f.s(pc, "affect", "d i"), "1");
        assert_eq!(f.s(pc, "affect", "nothing"), "0");
    }

    #[test]
    fn find_skill_num_matches_whole_and_by_word() {
        let mut f = fx();
        spells(&mut f.host);
        let h = &f.host;
        assert_eq!(find_skill_num(h, "armor"), 1);
        assert_eq!(find_skill_num(h, "ARM"), 1);
        assert_eq!(find_skill_num(h, "magic missile"), 2);
        assert_eq!(find_skill_num(h, "mag mis"), 2);
        assert_eq!(find_skill_num(h, "m m"), 2);
        assert_eq!(find_skill_num(h, "missile"), -1);
        assert_eq!(find_skill_num(h, "magic missile extra"), -1);
        assert_eq!(find_skill_num(h, "det inv"), 3);
        assert_eq!(find_skill_num(h, "zork"), -1);
        // Nothing left in the name: the word loop is skipped, so spell 1.
        assert_eq!(find_skill_num(h, ""), 1);
        assert_eq!(find_skill_num(h, "   "), 1);
    }

    #[test]
    fn skill_and_skillset() {
        let mut f = fx();
        spells(&mut f.host);
        let (mob, pc) = (f.mob, f.pc);
        assert_eq!(f.s(pc, "skill", "zork"), "unknown skill");
        assert_eq!(f.s(pc, "skill", "magic missile"), "0");
        assert_eq!(f.s(pc, "skillset", "\"magic missile\" 55"), "");
        assert_eq!(f.s(pc, "skill", "magic missile"), "55");
        assert_eq!(f.s(pc, "skillset", "\"magic missile\" 250"), "");
        assert_eq!(f.s(pc, "skill", "mag"), "100");
        assert_eq!(f.s(pc, "skillset", "\"magic missile\" -9"), "");
        assert_eq!(f.s(pc, "skill", "mag"), "0");
        // Not a number, unknown skill, no amount, and NPCs change nothing.
        f.s(pc, "skillset", "armor lots");
        f.s(pc, "skillset", "zork 50");
        f.s(pc, "skillset", "armor");
        assert_eq!(f.host.num(pc, "skill:1"), 0);
        assert_eq!(f.s(mob, "skillset", "armor 50"), "");
        assert_eq!(f.host.num(mob, "skill:1"), 0);
        assert_eq!(f.s(pc, "skillset", ""), "");
        // The empty skill is spell 1.
        f.host.set_num(pc, "skill:1", 33);
        assert_eq!(f.s(pc, "skill", ""), "33");
    }

    #[test]
    fn act_and_pref_flags() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        assert_eq!(f.s(pc, "is_killer", ""), "0");
        assert_eq!(f.s(pc, "is_killer", "on"), "1");
        assert_eq!(f.s(pc, "is_killer", "ON"), "1");
        assert_eq!(f.s(pc, "is_thief", ""), "0");
        assert_eq!(f.s(pc, "is_thief", "on"), "1");
        assert_eq!(f.s(pc, "is_killer", "off"), "0");
        assert_eq!(f.s(pc, "is_thief", "bogus"), "1");
        assert_eq!(f.s(pc, "is_thief", "off"), "0");
        // On a mob the bits are MOB_SPEC / MOB_SENTINEL, and PLR_FLAGGED is
        // false for an NPC.
        assert_eq!(f.s(mob, "is_killer", "on"), "0");
        assert_eq!(f.host.num(mob, "act") & 1, 1);
        assert_eq!(f.s(mob, "is_thief", "on"), "0");
        assert_eq!(f.host.num(mob, "act") & 2, 2);

        assert_eq!(f.s(pc, "pref", ""), "0");
        assert_eq!(f.s(pc, "pref", "BRIEF"), "0");
        f.host.set_num(pc, "pref", 1 | (1 << 26));
        assert_eq!(f.s(pc, "pref", "BRIEF"), "1");
        assert_eq!(f.s(pc, "pref", "AUTOLOOT"), "1");
        assert_eq!(f.s(pc, "pref", "COMPACT"), "0");
        // Exact, case-sensitive names only.
        assert_eq!(f.s(pc, "pref", "brief"), "0");
        assert_eq!(f.s(pc, "pref", "BOGUS"), "0");
        f.host.set_num(mob, "pref", 1);
        assert_eq!(f.s(mob, "pref", "BRIEF"), "0");
    }

    #[test]
    fn npcflag_searches_the_flag_names() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        // SENTINEL (1) is stored; ISNPC (3) comes from is_npc.
        f.host.set_num(mob, "act", 1 << 1);
        assert_eq!(f.s(mob, "npcflag", ""), "0");
        assert_eq!(f.s(mob, "npcflag", "SENTINEL"), "1");
        assert_eq!(f.s(mob, "npcflag", "sentinel"), "1");
        assert_eq!(f.s(mob, "npcflag", "ISNPC"), "1");
        assert_eq!(f.s(mob, "npcflag", "AGGR"), "0");
        // A substring of a set name matches too.
        assert_eq!(f.s(mob, "npcflag", "SENT"), "1");
        // So does text spanning two names.
        assert_eq!(f.s(mob, "npcflag", "SENTINEL ISNPC"), "1");
        f.host.set_num(mob, "act", 1 << 8);
        assert_eq!(f.s(mob, "npcflag", "AGGR"), "1");
        assert_eq!(f.s(mob, "npcflag", "AGGR_EVIL"), "1");
        // Bit 19 is the last named flag, MOB_NOTDEADYET, which is the
        // DEAD(ch) fact: an extracted mob has it, an act bit does not make it.
        f.host.set_num(mob, "act", 1 << 19);
        assert_eq!(f.s(mob, "npcflag", "DEAD"), "0");
        assert!(!f.host.is_dead(mob));
        f.host.set_num(mob, "act", 0);
        f.host.extract_char(mob);
        assert_eq!(f.s(mob, "npcflag", "DEAD"), "1");
        assert_eq!(f.s(mob, "npcflag", "ISNPC DEAD"), "1");
        f.host.set_num(mob, "dead", 0);
        f.host.set_num(mob, "act", 1 << 20);
        assert_eq!(f.s(mob, "npcflag", "UNDEFINED"), "1");
        f.host.set_num(mob, "act", 1 << 21);
        assert_eq!(f.s(mob, "npcflag", "UNDEFINED"), "0");
        // A mob always has ISNPC (bit 3), so it never reaches the fallback.
        assert_eq!(f.s(mob, "npcflag", "NOBITS"), "0");
        // A player with no printable bit: sprintbitarray leaves "NOBITS ".
        f.host.set_num(pc, "act", 0);
        assert_eq!(f.s(pc, "npcflag", "NOBITS"), "1");
        assert_eq!(f.s(pc, "npcflag", "bit"), "1");
        assert_eq!(f.s(pc, "npcflag", "SENTINEL"), "0");
        f.host.set_num(pc, "act", 1 << 21);
        assert_eq!(f.s(pc, "npcflag", "NOBITS"), "1");
        // The same bits on a player read through the NPC names.
        f.host.set_num(pc, "act", 1);
        assert_eq!(f.s(pc, "npcflag", "SPEC"), "1");
    }

    #[test]
    fn title_rules() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        assert_eq!(f.s(pc, "title", ""), "(null)");
        assert_eq!(f.s(pc, "title", "the Brave"), "the Brave");
        assert_eq!(f.s(pc, "title", ""), "the Brave");
        assert_eq!(f.s(mob, "title", "nope"), "");
        assert_eq!(f.host.text(mob, "title"), "");
        // Not settable on a character scripts may not target.
        f.host.set_num(pc, "dg_invalid", 1);
        assert_eq!(f.s(pc, "title", "other"), "the Brave");
    }

    #[test]
    fn attached_triggers_and_variables() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        assert_eq!(f.s(mob, "hasattached", ""), "");
        assert_eq!(f.s(mob, "hasattached", "7"), "0");
        let proto = f.host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        f.host.attach(mob, &proto);
        assert_eq!(f.s(mob, "hasattached", "7"), "1");
        assert_eq!(f.s(mob, "hasattached", "8"), "0");
        assert_eq!(f.s(mob, "hasattached", "7abc"), "1");
        // Players are never reported.
        f.host.attach(pc, &proto);
        assert_eq!(f.s(pc, "hasattached", "7"), "");

        assert_eq!(f.s(pc, "varexists", "mood"), "0");
        f.host
            .ensure_script(pc)
            .unwrap()
            .global_vars
            .add_var("Mood", "calm", 0);
        assert_eq!(f.s(pc, "varexists", "mood"), "1");
        assert_eq!(f.s(pc, "varexists", "MOOD"), "1");
        assert_eq!(f.s(pc, "varexists", "other"), "0");
        assert_eq!(f.s(pc, "varexists", ""), "0");
        assert_eq!(f.s(mob, "varexists", "mood"), "0");
        // A trigger-local variable is not a global.
        f.trig.vars.add_var("local", "1", 0);
        assert_eq!(f.s(pc, "varexists", "local"), "0");
    }

    #[test]
    fn quests() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        f.host.globals.nums.insert("quest:5".to_owned(), 1);
        assert_eq!(f.s(pc, "quest", ""), "0");
        f.host.set_num(pc, "quest", 9);
        assert_eq!(f.s(pc, "quest", ""), "0");
        f.host.set_num(pc, "quest", 5);
        assert_eq!(f.s(pc, "quest", ""), "5");
        f.host.set_num(mob, "quest", 5);
        assert_eq!(f.s(mob, "quest", ""), "0");
        assert_eq!(f.s(pc, "questdone", "5"), "0");
        f.host.set_num(pc, "quest_done:5", 1);
        assert_eq!(f.s(pc, "questdone", "5"), "1");
        assert_eq!(f.s(pc, "questdone", ""), "0");
        assert_eq!(f.s(pc, "questdone", "6"), "0");
        f.host.set_num(mob, "quest_done:5", 1);
        assert_eq!(f.s(mob, "questdone", "5"), "0");
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut f = fx();
        spells(&mut f.host);
        let (mob, pc) = (f.mob, f.pc);
        let long = "9".repeat(10_000);
        let weird = [
            "",
            "é",
            "é é",
            "\u{0}",
            "-",
            "}",
            long.as_str(),
            "\t\t",
            "日本語",
        ];
        let fields = [
            "affect",
            "align",
            "armor",
            "cha",
            "class",
            "damroll",
            "drunk",
            "eq",
            "exp",
            "gold",
            "has_item",
            "hasattached",
            "hitp",
            "inventory",
            "is_killer",
            "level",
            "maxhitp",
            "move",
            "npcflag",
            "pos",
            "pref",
            "qp",
            "questdone",
            "saving_rod",
            "skill",
            "skillset",
            "stradd",
            "str",
            "title",
            "varexists",
            "vnum",
            "wait",
            "é",
            "日本語",
        ];
        for c in [mob, pc, 424_242] {
            for field in fields {
                for sub in weird {
                    let _ = f.get(c, field, sub);
                }
            }
        }
        // The overflow cases wrap rather than panic.
        f.host.set_num(pc, "gold", 5);
        f.s(pc, "wait", "2147483647");
        f.s(pc, "prac", "2147483647");
        f.s(pc, "prac", "2147483647");
        f.s(pc, "qp", "2147483647");
        f.s(pc, "qp", "2147483647");
    }

    #[test]
    fn works_on_a_missing_character() {
        let mut f = fx();
        assert_eq!(f.s(999_999, "hitp", "5"), "0");
        assert_eq!(f.s(999_999, "name", ""), "");
        assert_eq!(f.s(999_999, "inventory", ""), "");
        assert_eq!(
            f.s(999_999, "room", ""),
            format!("{UID_CHAR}{ROOM_ID_BASE}")
        );
    }

    #[test]
    fn skills_above_unused_spell_slots_are_found() {
        let mut f = fx();
        let pc = f.pc;
        // Slots 2..130 undefined, as in the C's "!UNUSED!" gap.
        f.host
            .globals
            .texts
            .insert("spell:1".into(), "armor".into());
        f.host
            .globals
            .texts
            .insert("spell:131".into(), "backstab".into());
        f.host.set_num(pc, "skill:131", 77);
        assert_eq!(f.s(pc, "skill", "backstab"), "77");
        assert_eq!(f.s(pc, "skill", "bogus"), "unknown skill");
    }

    #[test]
    fn mock_mob_has_isnpc_flag() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        // The ISNPC name is printed for an NPC, whose is_npc supplies bit 3.
        assert_eq!(f.s(mob, "npcflag", "ISNPC"), "1");
        assert_eq!(f.s(pc, "npcflag", "ISNPC"), "0");
    }

    #[test]
    fn corpus_style_usage() {
        // `%actor.eq(*)%`, `%actor.has_item(1234)%` and `%self.pos%` style
        // reads from the shipped scripts.
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        f.host.set_num(mob, "pos", 8);
        assert_eq!(f.s(mob, "pos", ""), "Standing");
        assert_eq!(f.s(pc, "eq", "*"), "");
        assert_eq!(f.s(pc, "has_item", "1234"), "0");
        assert_eq!(f.s(pc, "gold", ""), "0");
    }

    #[test]
    fn npcflag_reads_isnpc_through_is_npc() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        // The mock's act word has no bit 3 stored; IS_NPC is its own fact.
        assert_eq!(f.host.num(mob, "act") & 8, 0);
        assert_eq!(f.s(mob, "npcflag", "ISNPC"), "1");
        // sprintbitarray prints "ISNPC " only: NOBITS must not appear.
        assert_eq!(f.s(mob, "npcflag", "NOBITS"), "0");
        assert_eq!(f.s(pc, "npcflag", "ISNPC"), "0");
        assert_eq!(f.s(pc, "npcflag", "NOBITS"), "1");
    }

    #[test]
    fn vnums_compare_as_unsigned_sixteen_bit() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        let nowhere = f.host.add_obj(-1, "corpse");
        f.host.obj_to_char(nowhere, pc);
        // GET_OBJ_VNUM of a prototype-less object is NOTHING, 65535.
        assert_eq!(f.s(pc, "inventory", "65535"), uid_text(nowhere));
        assert_eq!(f.s(pc, "inventory", "-1"), "");
        let ghost = f.host.add_mob(-1, "ghost");
        assert_eq!(f.s(ghost, "vnum", ""), "65535");
        assert_eq!(f.s(ghost, "vnum", "65535"), "1");
        assert_eq!(f.s(ghost, "vnum", "-1"), "0");
        assert_eq!(f.s(mob, "vnum", "3000"), "1");
        assert_eq!(f.s(mob, "vnum", "abc"), "0");
        assert_eq!(f.s(pc, "vnum", ""), "-1");
    }

    #[test]
    fn npcs_share_one_dummy_player_record() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        let other = f.host.add_mob(3001, "the cook");
        // CHECK_PLAYER_SPECIAL: every mob reads and writes dummy_mob.
        assert_eq!(f.s(mob, "thirst", "-1"), "-1");
        assert_eq!(f.s(other, "thirst", ""), "-1");
        assert_eq!(f.s(mob, "prac", "5"), "5");
        assert_eq!(f.s(other, "prac", ""), "5");
        assert_eq!(f.s(pc, "prac", ""), "0");
        assert_eq!(f.s(pc, "thirst", ""), "0");
    }

    #[test]
    fn skill_names_use_the_unused_slot_name() {
        let mut f = fx();
        let pc = f.pc;
        spells(&mut f.host);
        // Slot 200 defined above a gap at 4..=199, the last stretch of the
        // scan up to TOP_SPELL_DEFINE (299).
        f.host
            .globals
            .texts
            .insert("spell:200".into(), "zap".into());
        f.host.set_num(pc, "skill:200", 9);
        assert_eq!(f.s(pc, "skill", "zap"), "9");
        // is_abbrev("!", "!UNUSED!") matches slot 4, whose skill is 0.
        assert_eq!(f.s(pc, "skill", "!"), "0");
        assert_eq!(f.s(pc, "skill", "!unused!"), "0");
        // Fill words are kept by any_one_arg.
        assert_eq!(f.s(pc, "skill", "the"), "unknown skill");
        // An empty name resolves to spell 1.
        f.host.set_num(pc, "skill:1", 42);
        assert_eq!(f.s(pc, "skill", " "), "42");
    }

    #[test]
    fn alias_is_the_keyword_list() {
        let mut f = fx();
        let mob = f.mob;
        f.host.set_text(mob, "alias", "guard city man");
        assert_eq!(f.s(mob, "alias", ""), "guard city man");
        assert_eq!(f.s(mob, "name", ""), "the guard");
    }

    #[test]
    fn eq_position_names_ignore_case() {
        let mut f = fx();
        let mob = f.mob;
        let sword = f.host.add_obj(100, "sword");
        f.host.equip_char(mob, sword, 16);
        assert_eq!(f.s(mob, "eq", "WIELD"), uid_text(sword));
        assert_eq!(f.s(mob, "eq", "Wield"), uid_text(sword));
    }

    #[test]
    fn stradd_needs_effective_strength() {
        let mut f = fx();
        let pc = f.pc;
        // GET_STR is the effective value: real 17 with an affect at 18 passes.
        f.host.set_num(pc, "abil:str", 18);
        f.host.set_num(pc, "rabil:str", 17);
        assert!(f.get(pc, "stradd", "").is_some());
        // Real 18 with an affect lowering it to 17 does not.
        f.host.set_num(pc, "abil:str", 17);
        f.host.set_num(pc, "rabil:str", 18);
        assert_eq!(f.get(pc, "stradd", ""), None);
    }

    #[test]
    fn canbeseen_ignores_the_viewer_for_world_triggers() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        f.host
            .entity_mut(mob)
            .unwrap()
            .lists
            .insert("cannot_see".to_owned(), vec![pc]);
        let owner = mob;
        let mut cx = Cx {
            host: &mut f.host,
            owner,
            attach: AttachType::Wld,
            trig: &mut f.trig,
        };
        assert_eq!(
            char_field(&mut cx, pc, "canbeseen", ""),
            Some("1".to_owned())
        );
    }

    #[test]
    fn exp_is_capped_and_floored_on_players() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "exp", 100);
        // MIN(atoi, 1000) then gain_exp: +1000.
        assert_eq!(f.s(pc, "exp", "5000"), "1100");
        // A loss cannot take a player below 0.
        assert_eq!(f.s(pc, "exp", "-5000"), "0");
    }

    #[test]
    fn class_can_be_set_on_an_npc() {
        let mut f = fx();
        let mob = f.mob;
        assert_eq!(f.s(mob, "class", "thief"), "1");
        assert_eq!(f.host.num(mob, "class"), 2);
        assert_eq!(f.s(mob, "class", ""), "Thief");
    }

    #[test]
    fn high_preference_bits_are_read() {
        let mut f = fx();
        let pc = f.pc;
        // ZONERESETS is bit 34 of the table.
        assert_eq!(f.s(pc, "pref", "ZONERESETS"), "0");
        f.host.set_num(pc, "pref", 1 << 34);
        assert_eq!(f.s(pc, "pref", "ZONERESETS"), "1");
        assert_eq!(f.s(pc, "pref", "AUTODOOR"), "0");
    }

    #[test]
    fn skillset_on_an_npc_does_nothing_at_all() {
        let mut f = fx();
        spells(&mut f.host);
        let mob = f.mob;
        // `if (!IS_NPC(c) && subfield && *subfield)`: no write to the mob
        // and none to the shared dummy record either.
        assert_eq!(f.s(mob, "skillset", "armor 50"), "");
        assert_eq!(f.host.num(mob, "skill:1"), 0);
        assert!(!f.host.globals.nums.contains_key("dummy:skill:1"));
        assert_eq!(f.s(mob, "skill", "armor"), "0");
    }

    #[test]
    fn skillset_needs_a_numeric_amount_even_for_a_known_skill() {
        let mut f = fx();
        spells(&mut f.host);
        let pc = f.pc;
        f.host.set_num(pc, "skill:1", 33);
        // `is_number("lots")` is false: the skill keeps its value.
        f.s(pc, "skillset", "armor lots");
        assert_eq!(f.host.num(pc, "skill:1"), 33);
        f.s(pc, "skillset", "armor 12x");
        assert_eq!(f.host.num(pc, "skill:1"), 33);
        f.s(pc, "skillset", "armor 12");
        assert_eq!(f.host.num(pc, "skill:1"), 12);
    }

    #[test]
    fn vnum_with_a_subfield_is_false_for_a_player_even_at_nothing() {
        let mut f = fx();
        let pc = f.pc;
        // `IS_NPC(c) ? (GET_MOB_VNUM(c) == atoi(subfield)) : 0`: a player's
        // NOBODY is 65535, but the IS_NPC guard answers first.
        assert_eq!(f.s(pc, "vnum", "65535"), "0");
        assert_eq!(f.s(pc, "vnum", "-1"), "0");
    }

    #[test]
    fn a_vnum_above_sixteen_bits_is_passed_through() {
        let mut f = fx();
        let pc = f.pc;
        let big = f.host.add_mob(70_000, "the giant");
        assert_eq!(f.s(big, "vnum", ""), "70000");
        assert_eq!(f.s(big, "vnum", "70000"), "1");
        assert_eq!(f.s(big, "vnum", "65535"), "0");
        let relic = f.host.add_obj(70_000, "a relic");
        f.host.obj_to_char(relic, pc);
        assert_eq!(f.s(pc, "inventory", "70000"), uid_text(relic));
        assert_eq!(f.s(pc, "inventory", "65535"), "");
    }

    #[test]
    fn a_non_numeric_vnum_subfield_is_zero() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        // atoi("abc") is 0: it names vnum 0, nothing else.
        assert_eq!(f.s(mob, "vnum", "abc"), "0");
        let zero = f.host.add_mob(0, "the zero");
        assert_eq!(f.s(zero, "vnum", "abc"), "1");
        assert_eq!(f.s(zero, "vnum", "12abc"), "0");
        let thing = f.host.add_obj(0, "a thing");
        f.host.obj_to_char(thing, pc);
        assert_eq!(f.s(pc, "inventory", "abc"), uid_text(thing));
        assert_eq!(f.s(pc, "inventory", "7"), "");
    }

    #[test]
    fn has_item_compares_vnums_as_sixteen_bit_idxtype() {
        // item_in_list and get_object_in_equip compare `obj_vnum ovnum =
        // atoi(item)` with GET_OBJ_VNUM, both ush_int: a prototype-less
        // object (host vnum -1) is 65535, and 68556 is 3020.
        let mut f = fx();
        let pc = f.pc;
        let corpse = f.host.add_obj(-1, "a corpse");
        f.host.obj_to_char(corpse, pc);
        assert_eq!(f.s(pc, "inventory", "65535"), uid_text(corpse));
        assert_eq!(f.s(pc, "has_item", "65535"), "1");
        let lamp = f.host.add_obj(3020, "a lamp");
        f.host.obj_to_char(lamp, pc);
        assert_eq!(f.s(pc, "has_item", "68556"), "1");
        assert_eq!(f.s(pc, "has_item", "3021"), "0");
        // Worn objects compare the same way.
        let ring = f.host.add_obj(-1, "a ring");
        f.host.equip_char(pc, ring, 1);
        f.host.extract_obj(corpse);
        assert_eq!(f.s(pc, "has_item", "65535"), "1");
        // Looking it up by id or name works.
        assert_eq!(f.s(pc, "has_item", &uid_text(lamp)), "1");
        assert_eq!(f.s(pc, "has_item", "lamp"), "1");
    }

    #[test]
    fn pos_can_be_set_up_to_standing() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "pos", 4);
        // POS_SLEEPING..=POS_STANDING, each by a prefix of its name.
        assert_eq!(f.s(pc, "pos", "standing"), "Standing");
        assert_eq!(f.host.num(pc, "pos"), 8);
        assert_eq!(f.s(pc, "pos", "sit"), "Sitting");
        assert_eq!(f.s(pc, "pos", "REST"), "Resting");
        assert_eq!(f.s(pc, "pos", "fight"), "Fighting");
        assert_eq!(f.s(pc, "pos", "s"), "Sleeping");
        // Positions below sleeping cannot be set: the position stays.
        assert_eq!(f.s(pc, "pos", "dead"), "Sleeping");
        assert_eq!(f.s(pc, "pos", "stun"), "Sleeping");
        assert_eq!(f.s(pc, "pos", "nonsense"), "Sleeping");
    }

    #[test]
    fn mana_and_move_pools_wrap_as_shorts() {
        let mut f = fx();
        let pc = f.pc;
        // 100 + 40000 = 40100, which as a sh_int is 40100 - 65536.
        f.host.set_num(pc, "mana", 100);
        assert_eq!(f.s(pc, "mana", "40000"), "-25436");
        for (field, key) in [
            ("maxmana", "max_mana"),
            ("maxmove", "max_move"),
            ("maxhitp", "max_hit"),
        ] {
            f.host.set_num(pc, key, 100);
            assert_eq!(f.s(pc, field, "40000"), "-25436", "{field}");
        }
        f.host.set_num(pc, "move", 100);
        assert_eq!(f.s(pc, "move", "40000"), "-25436");
    }

    #[test]
    fn hitroll_and_saves_wrap_as_their_c_types() {
        let mut f = fx();
        let pc = f.pc;
        // sbyte: 100 + 100 = 200 -> -56.
        f.host.set_num(pc, "hitroll", 100);
        assert_eq!(f.s(pc, "hitroll", "100"), "-56");
        // sh_int saves: 30000 + 10000 = 40000 -> -25536.
        f.host.set_num(pc, "save:1", 30_000);
        assert_eq!(f.s(pc, "saving_rod", "10000"), "-25536");
        f.host.set_num(pc, "save:4", 30_000);
        assert_eq!(f.s(pc, "saving_spell", "10000"), "-25536");
    }

    #[test]
    fn int_wis_and_dex_floor_at_three() {
        let mut f = fx();
        let pc = f.pc;
        for (field, key) in [("int", "int"), ("wis", "wis"), ("dex", "dex")] {
            f.host.set_num(pc, &format!("rabil:{key}"), 10);
            f.host.set_num(pc, &format!("abil:{key}"), 10);
            assert_eq!(f.s(pc, field, "-20"), "3", "{field}");
            assert_eq!(f.s(pc, field, "-1"), "3", "{field}");
            assert_eq!(f.s(pc, field, "1"), "4", "{field}");
        }
    }

    #[test]
    fn an_ability_without_a_subfield_prints_the_effective_value() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "rabil:int", 10);
        f.host.set_num(pc, "abil:int", 15);
        // GET_INT is aff_abils; nothing is written without a subfield.
        assert_eq!(f.s(pc, "int", ""), "15");
        assert_eq!(f.host.num(pc, "rabil:int"), 10);
    }

    #[test]
    fn eq_star_looks_at_every_slot() {
        let mut f = fx();
        let pc = f.pc;
        let torch = f.host.add_obj(5, "a torch");
        f.host.equip_char(pc, torch, 17);
        assert_eq!(f.s(pc, "eq", "*"), "1");
        let pc2 = f.host.add_player("Amy");
        let ring = f.host.add_obj(6, "a ring");
        f.host.equip_char(pc2, ring, 0);
        assert_eq!(f.s(pc2, "eq", "*"), "1");
    }

    #[test]
    fn hasattached_counts_the_owners_checked_out_trigger() {
        let mut f = fx();
        let mob = f.mob;
        let proto = f.host.add_proto(7, AttachType::Mob, "g", 100, "", "say hi");
        let id = f.host.attach(mob, &proto).unwrap();
        // The running instance is out of the list, its slot stays.
        let _running = f.host.script_mut(mob).unwrap().check_out(id).unwrap();
        assert_eq!(f.s(mob, "hasattached", "7"), "1");
        assert_eq!(f.s(mob, "hasattached", "8"), "0");
    }

    #[test]
    fn exp_with_a_zero_gain_changes_nothing() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "exp", -50);
        // gain_exp(c, 0): neither `gain > 0` nor `gain < 0` runs.
        assert_eq!(f.s(pc, "exp", "0"), "-50");
        assert_eq!(f.s(pc, "exp", "abc"), "-50");
    }

    #[test]
    fn exp_loss_is_capped_per_call() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "exp", 700_000);
        // gain = MAX(-CONFIG_MAX_EXP_LOSS, -600000) = -500000.
        assert_eq!(f.s(pc, "exp", "-600000"), "200000");
    }

    #[test]
    fn dead_is_the_is_dead_fact_in_npcflag() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        assert_eq!(f.s(mob, "npcflag", "DEAD"), "0");
        f.host.extract_char(mob);
        assert!(f.host.is_dead(mob));
        // The buffer is "ISNPC DEAD ".
        assert_eq!(f.s(mob, "npcflag", "DEAD"), "1");
        assert_eq!(f.s(mob, "npcflag", "ISNPC DEAD"), "1");
        // For a player the same dead bit is PLR_NOTDEADYET (16), which the
        // NPC name table prints as NO_BASH; nothing else is set.
        assert_eq!(f.s(pc, "npcflag", "NO_BASH"), "0");
        assert_eq!(f.s(pc, "npcflag", "NOBITS"), "1");
        f.host.extract_char(pc);
        assert_eq!(f.s(pc, "npcflag", "NO_BASH"), "1");
        assert_eq!(f.s(pc, "npcflag", "NOBITS"), "0");
        assert_eq!(f.s(pc, "npcflag", "DEAD"), "0");
        // A real NPC act bit 16 is NO_BASH, and a bit 19 on a player is
        // a bit of the array like any other: neither is the dead fact.
        let other = f.host.add_mob(1, "a rat");
        f.host.set_num(other, "act", 1 << 16);
        assert_eq!(f.s(other, "npcflag", "NO_BASH"), "1");
        let amy = f.host.add_player("Amy");
        f.host.set_num(amy, "act", 1 << 19);
        assert_eq!(f.s(amy, "npcflag", "DEAD"), "1");
        assert!(!f.host.is_dead(amy));
    }

    #[test]
    fn the_mock_keeps_dead_only_in_the_shared_key() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        f.host.extract_char(mob);
        f.host.extract_char(pc);
        // Neither dead bit is stored in the act array, and the act array
        // never answers for them.
        assert_eq!(f.host.num(mob, "act"), 0);
        assert_eq!(f.host.num(pc, "act"), 0);
        assert!(!f.host.char_act_flagged(mob, 19));
        assert!(!f.host.char_act_flagged(pc, 16));
        f.host.char_set_act_flag(mob, 19, true);
        assert!(f.host.is_dead(mob));
        assert_eq!(f.host.num(mob, "act"), 1 << 19);
        f.host.set_num(mob, "dead", 0);
        assert!(!f.host.is_dead(mob));
    }

    #[test]
    fn nohassle_is_preference_bit_8_in_pref_and_in_valid_dg_target() {
        let mut f = fx();
        let pc = f.pc;
        f.host.set_num(pc, "level", 34);
        // PRF_NOHASSLE is preference bit 8, "NO_HASS".
        assert_eq!(f.s(pc, "pref", "NO_HASS"), "0");
        assert!(f.host.valid_dg_target(pc, true));
        f.host.set_nohassle(pc, true);
        assert_eq!(f.s(pc, "pref", "NO_HASS"), "1");
        assert!(!f.host.valid_dg_target(pc, true));
        f.host.set_nohassle(pc, false);
        assert_eq!(f.s(pc, "pref", "NO_HASS"), "0");
        assert!(f.host.valid_dg_target(pc, true));
        // The one fact is the bit itself, however it got set.
        f.host.set_num(pc, "pref", 1 << 8);
        assert_eq!(f.s(pc, "pref", "NO_HASS"), "1");
        assert!(!f.host.valid_dg_target(pc, true));
    }

    #[test]
    fn hostile_input_leaves_every_stat_inside_its_c_bounds() {
        let mut f = fx();
        let (mob, pc) = (f.mob, f.pc);
        let weird = [
            "2147483647",
            "-2147483648",
            "99999999999",
            "-99999999999",
            "3",
            "é",
            "\t",
            "",
        ];
        for c in [mob, pc] {
            for sub in weird {
                for field in [
                    "align", "drunk", "hunger", "thirst", "str", "int", "wis", "dex",
                ] {
                    let _ = f.get(c, field, sub);
                }
            }
            assert!((-1000..=1000).contains(&f.host.char_stat(c, CharStat::Align)));
            for stat in [CharStat::Drunk, CharStat::Hunger, CharStat::Thirst] {
                assert!((-1..=24).contains(&f.host.char_stat(c, stat)));
            }
            let max = if c == mob { 25 } else { 18 };
            for abil in [CharAbil::Str, CharAbil::Int, CharAbil::Wis, CharAbil::Dex] {
                assert!(
                    (3..=max).contains(&f.host.char_real_abil(c, abil)),
                    "{abil:?}"
                );
            }
        }
    }

    #[test]
    fn a_missing_character_reads_neutral_and_writes_nothing() {
        let mut f = fx();
        let ghost = 999_999;
        for (field, sub, want) in [
            ("gold", "5", "0"),
            ("exp", "5", "0"),
            ("align", "5", "0"),
            ("str", "2", "0"),
            ("level", "", "0"),
            ("hasattached", "7", ""),
            ("varexists", "x", "0"),
            ("eq", "*", ""),
            ("fighting", "", ""),
            ("master", "", ""),
            ("npcflag", "SPEC", "0"),
            ("title", "x", "(null)"),
        ] {
            assert_eq!(f.s(ghost, field, sub), want, "{field}({sub})");
        }
        assert!(f.host.effects.iter().all(|e| !e.contains("999999")));
        assert!(!f.host.entities.contains_key(&ghost));
        assert!(f.host.globals.nums.is_empty());
    }
}
