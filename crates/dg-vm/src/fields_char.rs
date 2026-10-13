//! The character field table of `find_replacement` (`dg_variables.c`).
//!
//! [`char_field`] is the `if (c) { … }` block: `%actor.gold(5)%`,
//! `%self.hitp%`, `%mob.eq(head)%`. The C switches on the first letter of the
//! field and compares with `str_cmp`; here the field is lowercased once and
//! matched whole, which selects the same branch (every name is filed under
//! its own first letter in the C).
//!
//! Also ported from `dg_variables.c`: `skill_percent` and `char_has_item`
//! (with [`crate::lookup::item_in_list`] and
//! [`crate::lookup::get_object_in_equip`]). From `spell_parser.c`:
//! `find_skill_num`, over the host's spell-name table.
//!
//! # Fidelity notes
//!
//! - A field with a subfield *changes* the character, then returns the new
//!   value. The C stores into narrow fields (`sh_int` hit points and mana,
//!   `sbyte` abilities, damroll, hitroll and conditions) with C's implicit
//!   truncation; [`narrow`] reproduces it, so `%self.hitp(40000)%` wraps as
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
//!   `UNDEFINED `, higher bits are never looked at.
//! - `skill` with an empty or all-fill subfield resolves to spell 1, because
//!   the C's `find_skill_num` falls out of its word loop with nothing left in
//!   the name (`ok && !*first2`).
//! - `title` on a player with no title prints `(null)`, which is what the
//!   C's `snprintf("%s", NULL)` gives with glibc.
//! - Deviations where the C would index out of bounds: a sex, position or
//!   class outside its name table prints `UNDEFINED` (as `sprinttype` does)
//!   rather than reading past the array.
//! - There is no `race` field in this tree's table (tbaMUD has no races);
//!   none is invented.

use crate::cstr::{
    any_one_arg, atoi, is_abbrev, is_number, one_word, skip_spaces, sprinttype, str_str, strn_cmp,
};
use crate::host::{Cx, DgHost};
use crate::lookup::{NUM_WEARS, find_eq_pos_script, get_object_in_equip, item_in_list};
use crate::types::{AttachType, ROOM_ID_BASE, Uid, uid_text};

/// `LVL_IMMORT`.
const LVL_IMMORT: i32 = 31;
/// `LVL_GRGOD`.
const LVL_GRGOD: i32 = 33;
/// `PULSE_VIOLENCE / 2`: pulses per unit of `wait(<n>)` (`PULSE_VIOLENCE` is
/// two seconds of ten passes each).
const WAIT_UNIT: i32 = 10;
/// `POS_SLEEPING` and `POS_STANDING`: the range `pos(<name>)` can set.
const POS_SLEEPING: i32 = 4;
const POS_STANDING: i32 = 8;
/// `PLR_KILLER` and `PLR_THIEF`.
const PLR_KILLER: u32 = 0;
const PLR_THIEF: u32 = 1;
/// The upper bound on the spell numbers [`find_skill_num`] scans: the host
/// ends its table with `None` well before this.
const MAX_SPELL_SCAN: i32 = 1000;

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
    /// `GET_GOLD(ch)`. Read only; written through
    /// [`CharHost::char_increase_gold`].
    Gold,
    /// `GET_EXP(ch)`. Read only; written through [`CharHost::char_gain_exp`].
    Exp,
    /// `GET_SEX(ch)`.
    Sex,
    /// `GET_CLASS(ch)`.
    Class,
    /// `GET_POS(ch)`.
    Pos,
    /// `GET_LEVEL(ch)`.
    Level,
    /// `GET_WEIGHT(ch)` (`ubyte`). Read only.
    Weight,
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
/// Player-only attributes (practices, conditions, quest points, skills,
/// preferences) on an NPC are the host's to define: the C logs and uses a
/// shared dummy record.
pub trait CharHost {
    /// The value of one numeric attribute (see [`CharStat`]).
    fn char_stat(&self, ch: Uid, stat: CharStat) -> i32;

    /// Store one numeric attribute: `GET_xxx(ch) = value`. The VM has already
    /// narrowed `value` to the C field's width.
    fn char_set_stat(&mut self, ch: Uid, stat: CharStat, value: i32);

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

    /// `spell_info[spell].name`, for `spell` from 1 up; `None` past
    /// `TOP_SPELL_DEFINE`. Every number up to the top has a name (the C names
    /// unused slots `!UNUSED!`), so the table is contiguous.
    fn char_spell_name(&self, spell: i32) -> Option<String>;

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

    /// `GET_TITLE(ch)`; `None` when the pointer is NULL.
    fn char_title(&self, ch: Uid) -> Option<String>;

    /// Replace `GET_TITLE(ch)`.
    fn char_set_title(&mut self, ch: Uid, title: &str);

    /// `GET_QUEST(ch)`; `-1` (`NOTHING`) when on no quest.
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

/// `find_skill_num(name)` (`spell_parser.c`): the spell whose name `name`
/// abbreviates, whole or word by word; `-1` when none.
pub fn find_skill_num<H: CharHost + ?Sized>(host: &H, name: &str) -> i32 {
    for index in 1..=MAX_SPELL_SCAN {
        let Some(spell) = host.char_spell_name(index) else {
            break;
        };
        if is_abbrev(name, &spell) {
            return index;
        }
        let mut ok = true;
        let (mut first, mut temp) = any_one_arg(&spell);
        let (mut first2, mut temp2) = any_one_arg(name);
        while !first.is_empty() && !first2.is_empty() && ok {
            if !is_abbrev(&first2, &first) {
                ok = false;
            }
            (first, temp) = any_one_arg(temp);
            (first2, temp2) = any_one_arg(temp2);
        }
        if ok && first2.is_empty() {
            return index;
        }
    }
    -1
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
    if get_object_in_equip(host, ch, item).is_some() {
        return true;
    }
    item_in_list(host, item, &host.carrying(ch)) != 0
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
            number(host.char_stat(c, CharStat::Exp))
        }
        "fighting" => uid_or_empty(host.char_fighting(c)),
        "follower" => uid_or_empty(host.char_first_follower(c)),
        "gold" => {
            if has_sub {
                host.char_increase_gold(c, atoi(sub));
            }
            number(host.char_stat(c, CharStat::Gold))
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
                let found = carrying.iter().copied().find(|obj| host.vnum(*obj) == vnum);
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
                host.char_set_stat(c, CharStat::Level, level);
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
                for (bit, name) in (0u32..).zip(ACTION_BITS.iter()) {
                    if host.char_act_flagged(c, bit) {
                        buf.push_str(name);
                        buf.push(' ');
                    }
                }
                // The name table's "\n" terminator is bit 20: a set bit there
                // prints UNDEFINED, and the C stops after it.
                if host.char_act_flagged(c, 20) {
                    buf.push_str("UNDEFINED ");
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
            let quest = host.char_quest(c);
            if !host.is_npc(c) && quest != -1 && host.char_quest_exists(quest) {
                number(quest)
            } else {
                Some("0".to_owned())
            }
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
                let same = host.is_npc(c) && host.vnum(c) == atoi(sub);
                number(i32::from(same))
            } else if host.is_npc(c) {
                number(host.vnum(c))
            } else {
                Some("-1".to_owned())
            }
        }
        "weight" => number(host.char_stat(c, CharStat::Weight)),
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
    use crate::host::BaseHost;
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
        let wrapped = narrow(Width::I16, 100i32.wrapping_add(atoi("99999999999")));
        assert_eq!(f.s(pc, "move", "99999999999"), wrapped.to_string());
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
        // ISNPC (3) and SENTINEL (1).
        f.host.set_num(mob, "act", (1 << 3) | (1 << 1));
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
        // Bit 19 is the last named flag; bit 20 prints UNDEFINED; 21+ nothing.
        f.host.set_num(mob, "act", 1 << 19);
        assert_eq!(f.s(mob, "npcflag", "DEAD"), "1");
        f.host.set_num(mob, "act", 1 << 20);
        assert_eq!(f.s(mob, "npcflag", "UNDEFINED"), "1");
        f.host.set_num(mob, "act", 1 << 21);
        assert_eq!(f.s(mob, "npcflag", "UNDEFINED"), "0");
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
}
