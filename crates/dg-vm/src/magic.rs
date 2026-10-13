//! `dg_cast` and `dg_affect` (`dg_misc.c`): [`do_dg_cast`] and
//! [`do_dg_affect`].
//!
//! Everything about spells themselves (`find_skill_num`, `spell_info[]`,
//! `call_magic`, the affect list) is the host's, reached through
//! [`MagicHost`]. The VM keeps the argument parsing, target resolution,
//! caster selection and the property tables.
//!
//! # Deviations from the C
//!
//! - `strtok` is emulated on the quote characters only; the C's scratch
//!   buffers (`orig_cmd`, `buf2`) have no length checks and are not
//!   reproduced.
//! - The proxy caster is loaded, placed at the head of the room's people,
//!   used and extracted through three host calls rather than by touching the
//!   room's list directly.

use crate::cstr::{atoi, half_chop, one_argument, skip_spaces, str_cmp, str_eq};
use crate::host::{Cx, DgHost};
use crate::lookup::{dg_room_of_obj, get_char, get_obj};
use crate::types::{AttachType, Uid};

/// `DG_CASTER_PROXY` (`dg_scripts.h`): the vnum of the mob that casts for
/// objects and rooms.
pub const DG_CASTER_PROXY: i32 = 1;

/// `DG_SPELL_LEVEL`: the level spells cast by objects and rooms use.
pub const DG_SPELL_LEVEL: i32 = 25;

/// `MAX_SPELLS` (`spells.h`): spell numbers run `1..=MAX_SPELLS`.
pub const MAX_SPELLS: i32 = 130;

/// `SPELL_DG_AFFECT` (`spells.h`): the spell number every `dg_affect` affect
/// carries, so `off` can remove exactly those.
pub const SPELL_DG_AFFECT: i32 = 298;

/// `spell_info[n].targets` bits (`spells.h`).
pub const TAR_IGNORE: u32 = 1 << 0;
pub const TAR_CHAR_ROOM: u32 = 1 << 1;
pub const TAR_CHAR_WORLD: u32 = 1 << 2;
pub const TAR_OBJ_INV: u32 = 1 << 7;
pub const TAR_OBJ_ROOM: u32 = 1 << 8;
pub const TAR_OBJ_WORLD: u32 = 1 << 9;
pub const TAR_OBJ_EQUIP: u32 = 1 << 10;

/// `spell_info[n].routines` bit `MAG_GROUPS` (`spells.h`): group spells are
/// refused.
pub const MAG_GROUPS: u32 = 1 << 5;

/// `apply_types[]` (`constants.c`), without the `"\n"` terminator. The index
/// is the `APPLY_*` number.
pub const APPLY_TYPES: &[&str] = &[
    "NONE",
    "STR",
    "DEX",
    "INT",
    "WIS",
    "CON",
    "CHA",
    "CLASS",
    "LEVEL",
    "AGE",
    "CHAR_WEIGHT",
    "CHAR_HEIGHT",
    "MAXMANA",
    "MAXHIT",
    "MAXMOVE",
    "GOLD",
    "EXP",
    "ARMOR",
    "HITROLL",
    "DAMROLL",
    "SAVING_PARA",
    "SAVING_ROD",
    "SAVING_PETRI",
    "SAVING_BREATH",
    "SAVING_SPELL",
];

/// `affected_bits[]` (`constants.c`), without the `"\n"` terminator. The
/// index is the `AFF_*` bit number; entry 0 is the empty string the C keeps
/// there.
pub const AFFECTED_BITS: &[&str] = &[
    "",
    "BLIND",
    "INVIS",
    "DET-ALIGN",
    "DET-INVIS",
    "DET-MAGIC",
    "SENSE-LIFE",
    "WATWALK",
    "SANCT",
    "GROUP",
    "CURSE",
    "INFRA",
    "POISON",
    "PROT-EVIL",
    "PROT-GOOD",
    "SLEEP",
    "NO_TRACK",
    "FLY",
    "SCUBA",
    "SNEAK",
    "HIDE",
    "UNUSED",
    "CHARM",
];

/// The spell and affect primitives.
pub trait MagicHost {
    /// `find_skill_num(name)`: the spell number `name` names, or `-1`.
    /// The VM range-checks the result against `1..=MAX_SPELLS`.
    fn magic_find_skill_num(&self, name: &str) -> i32;

    /// `spell_info[spellnum].targets`: the `TAR_*` bits. `0` for a spell the
    /// host does not know.
    fn magic_spell_targets(&self, spellnum: i32) -> u32;

    /// `spell_info[spellnum].routines`: the `MAG_*` bits. `0` for a spell the
    /// host does not know.
    fn magic_spell_routines(&self, spellnum: i32) -> u32;

    /// `call_magic(caster, victim, obj, spellnum, level, CAST_SPELL)`.
    fn magic_call_magic(
        &mut self,
        caster: Uid,
        victim: Option<Uid>,
        obj: Option<Uid>,
        spellnum: i32,
        level: i32,
    );

    /// The temporary caster for an object or room:
    /// `caster = read_mobile(DG_CASTER_PROXY, VIRTUAL)`, then, if `short_descr`
    /// is given, `caster->player.short_descr = strdup(short_descr)`, then
    /// `caster->next_in_room = room->people; room->people = caster;
    /// caster->in_room = real_room(room->number)`. `None` when the mob cannot
    /// be loaded.
    fn magic_load_caster_proxy(&mut self, room: Uid, short_descr: Option<&str>) -> Option<Uid>;

    /// `extract_char(caster)` for the proxy made by
    /// [`Self::magic_load_caster_proxy`].
    fn magic_extract_caster_proxy(&mut self, proxy: Uid);

    /// `affect_from_char(ch, SPELL_DG_AFFECT)`: remove every `dg_affect`
    /// affect from the character.
    fn magic_remove_dg_affects(&mut self, ch: Uid);

    /// `new_affect(&af); af.spell = SPELL_DG_AFFECT; af.duration = duration;
    /// af.modifier = modifier; af.location = location;` then, when `bit` is
    /// given, `SET_BIT_AR(af.bitvector, bit)`; `affect_to_char(ch, &af)`.
    /// `location` is `APPLY_NONE` (`0`) for a bit affect.
    fn magic_add_dg_affect(
        &mut self,
        ch: Uid,
        location: i32,
        modifier: i32,
        duration: i32,
        bit: Option<u32>,
    );
}

/// What `strtok(cmd, "'")` / `strtok(NULL, "'")` / `strtok(NULL, "\0")` give
/// for a `dg_cast` line.
enum CastParts<'a> {
    /// The first `strtok` found nothing.
    NoName,
    /// There is no second token: no quote opens a spell name.
    NoQuoted,
    /// The spell name, and the text after its closing quote (if any).
    Parts(&'a str, Option<&'a str>),
}

/// Emulate the three `strtok` calls of `do_dg_cast`. The delimiter is the
/// single quote; the third call has an empty delimiter set, so it takes the
/// rest of the line whole.
fn cast_parts(cmd: &str) -> CastParts<'_> {
    let skip_quotes = |from: usize| {
        cmd[from..]
            .find(|c| c != '\'')
            .map_or(cmd.len(), |offset| from + offset)
    };
    let token_end = |from: usize| cmd[from..].find('\'').map_or(cmd.len(), |o| from + o);

    let start = skip_quotes(0);
    if start == cmd.len() {
        return CastParts::NoName;
    }
    let end = token_end(start);
    if end == cmd.len() {
        return CastParts::NoQuoted;
    }
    let start = skip_quotes(end + 1);
    if start == cmd.len() {
        return CastParts::NoQuoted;
    }
    let end = token_end(start);
    let spell = &cmd[start..end];
    let rest = cmd.get(end + 1..).filter(|rest| !rest.is_empty());
    CastParts::Parts(spell, rest)
}

/// `do_dg_cast(go, sc, trig, type, cmd)`: `cmd` is the whole substituted
/// `dg_cast '<spell>' [target]` line.
///
/// A mob casts as itself at its own level. An object or room casts through a
/// temporary [`DG_CASTER_PROXY`] mob, named after the object or "The gods",
/// at [`DG_SPELL_LEVEL`]. The target must exist unless the spell ignores
/// targets; only the first word of the target is used. Group spells are
/// refused.
pub fn do_dg_cast<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let owner = cx.owner;

    // The caster, or the room a proxy caster will stand in.
    let (caster, caster_room) = match cx.attach {
        AttachType::Mob => (Some(owner), None),
        AttachType::Wld => (None, Some(owner)),
        AttachType::Obj => match dg_room_of_obj(&*cx.host, owner) {
            Some(room) => (None, Some(room)),
            None => {
                cx.host
                    .script_log("dg_do_cast: unknown room for object-caster!");
                return;
            }
        },
    };

    let (spell, rest) = match cast_parts(cmd) {
        CastParts::NoName => {
            cx.log("dg_cast needs spell name.");
            return;
        }
        CastParts::NoQuoted => {
            cx.log("dg_cast needs spell name in `'s.");
            return;
        }
        CastParts::Parts(spell, rest) => (spell, rest),
    };

    let spellnum = cx.host.magic_find_skill_num(spell);
    if !(1..=MAX_SPELLS).contains(&spellnum) {
        cx.log(&format!("dg_cast: invalid spell name ({cmd})"));
        return;
    }

    // Find the target: the first word after the spell name.
    let target_name = rest.map(|rest| one_argument(rest).0);
    let target_name = target_name.as_deref().map(skip_spaces).unwrap_or("");
    let targets = cx.host.magic_spell_targets(spellnum);
    let mut victim = None;
    let mut target_obj = None;
    if targets & TAR_IGNORE == 0 && !target_name.is_empty() {
        let mut found = false;
        if targets & (TAR_CHAR_ROOM | TAR_CHAR_WORLD) != 0 {
            victim = get_char(&*cx.host, target_name);
            found = victim.is_some();
        }
        if !found && targets & (TAR_OBJ_INV | TAR_OBJ_EQUIP | TAR_OBJ_ROOM | TAR_OBJ_WORLD) != 0 {
            target_obj = get_obj(&*cx.host, target_name);
            found = target_obj.is_some();
        }
        if !found {
            cx.log(&format!("dg_cast: target not found ({cmd})"));
            return;
        }
    }

    if cx.host.magic_spell_routines(spellnum) & MAG_GROUPS != 0 {
        cx.log(&format!("dg_cast: group spells not permitted ({cmd})"));
        return;
    }

    if let Some(caster) = caster {
        let level = cx.host.level(caster);
        cx.host
            .magic_call_magic(caster, victim, target_obj, spellnum, level);
        return;
    }

    let Some(room) = caster_room else {
        return;
    };
    // The proxy is named for the object, or "the gods" for a room.
    let short_descr = match cx.attach {
        AttachType::Obj => Some(cx.host.name(owner)),
        _ => Some("The gods".to_owned()),
    };
    let Some(proxy) = cx
        .host
        .magic_load_caster_proxy(room, short_descr.as_deref())
    else {
        cx.host.script_log("dg_cast: Cannot load the caster mob!");
        return;
    };
    cx.host
        .magic_call_magic(proxy, victim, target_obj, spellnum, DG_SPELL_LEVEL);
    cx.host.magic_extract_caster_proxy(proxy);
}

/// Which table a `dg_affect` property was found in.
enum Property {
    /// `APPLY_TYPE`: index into [`APPLY_TYPES`].
    Apply(usize),
    /// `AFFECT_TYPE`: index into [`AFFECTED_BITS`].
    Affect(usize),
}

/// Find `property` in `apply_types[]`, then `affected_bits[]`.
fn find_property(property: &str) -> Option<Property> {
    if let Some(index) = APPLY_TYPES.iter().position(|name| str_eq(name, property)) {
        return Some(Property::Apply(index));
    }
    AFFECTED_BITS
        .iter()
        .position(|name| str_cmp(name, property) == 0)
        .map(Property::Affect)
}

/// `do_dg_affect(go, sc, trig, type, cmd)`: `cmd` is the whole substituted
/// `dg_affect <target> <property> <value> <duration>` line.
///
/// `property` is an `apply_types[]` name (the affect adds `value` to that
/// attribute) or an `affected_bits[]` name (it sets that bit). A `value` of
/// `off` removes the target's `dg_affect` affects instead. The duration must
/// be positive even then; the affect lasts `duration - 1` further ticks.
pub fn do_dg_affect<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    let (_junk, rest) = half_chop(cmd);
    let (charname, rest) = half_chop(rest);
    let (property, rest) = half_chop(rest);
    let (value_p, duration_p) = half_chop(rest);

    if charname.is_empty() || property.is_empty() || value_p.is_empty() || duration_p.is_empty() {
        cx.log("dg_affect usage: <target> <property> <value> <duration>");
        return;
    }

    let value = atoi(&value_p);
    let duration = atoi(duration_p);
    if duration <= 0 {
        cx.log("dg_affect: need positive duration!");
        cx.host.script_log(&format!(
            "Line was: dg_affect {charname} {property} {value_p} {duration_p} ({duration})"
        ));
        return;
    }

    let Some(found) = find_property(&property) else {
        cx.log(&format!("dg_affect: unknown property '{property}'!"));
        return;
    };

    let Some(ch) = get_char(&*cx.host, &charname) else {
        cx.log("dg_affect: cannot locate target!");
        return;
    };

    if str_eq(&value_p, "off") {
        cx.host.magic_remove_dg_affects(ch);
        return;
    }

    let duration = duration.wrapping_sub(1);
    match found {
        Property::Apply(index) => {
            let location = i32::try_from(index).unwrap_or(0);
            cx.host
                .magic_add_dg_affect(ch, location, value, duration, None);
        }
        Property::Affect(index) => {
            let bit = u32::try_from(index).unwrap_or(0);
            cx.host
                .magic_add_dg_affect(ch, 0, value, duration, Some(bit));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::host::WorldHost;
    use crate::mock::MockHost;
    use crate::types::{TrigInstance, uid_text};

    const MAGIC_MISSILE: i32 = 32;
    const SANCTUARY: i32 = 36;
    const ARMOR: i32 = 1;
    const IDENTIFY: i32 = 20;
    const GROUP_HEAL: i32 = 64;

    struct World {
        host: MockHost,
        room: Uid,
        mob: Uid,
        bob: Uid,
        sword: Uid,
    }

    fn world() -> World {
        let mut host = MockHost::new();
        let room = host.add_room(3001, "Temple");
        let mob = host.add_mob(100, "guard");
        host.set_num(mob, "level", 17);
        let bob = host.add_player("bob");
        let sword = host.add_obj(200, "sword");
        host.char_to_room(mob, room);
        host.char_to_room(bob, room);
        host.obj_to_room(sword, room);
        host.add_spell(
            "magic missile",
            MAGIC_MISSILE,
            TAR_CHAR_ROOM | TAR_NOT_SELF,
            0,
        );
        host.add_spell("sanctuary", SANCTUARY, TAR_CHAR_ROOM | TAR_SELF_ONLY, 0);
        host.add_spell("armor", ARMOR, TAR_CHAR_ROOM, 0);
        host.add_spell(
            "identify",
            IDENTIFY,
            TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
            0,
        );
        host.add_spell("group heal", GROUP_HEAL, TAR_IGNORE, MAG_GROUPS);
        host.add_spell("earthquake", 23, TAR_IGNORE, 0);
        World {
            host,
            room,
            mob,
            bob,
            sword,
        }
    }

    const TAR_NOT_SELF: u32 = 1 << 6;
    const TAR_SELF_ONLY: u32 = 1 << 5;

    /// Run `f` with a context owned by `owner`.
    fn run(w: &mut World, owner: Uid, attach: AttachType, f: impl FnOnce(&mut Cx<'_, MockHost>)) {
        let proto = w.host.add_proto(9, attach, "g", 100, "", "say hi");
        let mut trig = TrigInstance::new(1, proto);
        let mut cx = Cx {
            host: &mut w.host,
            owner,
            attach,
            trig: &mut trig,
        };
        f(&mut cx);
    }

    fn cast_as_mob(w: &mut World, cmd: &str) {
        let mob = w.mob;
        run(w, mob, AttachType::Mob, |cx| do_dg_cast(cx, cmd));
    }

    #[test]
    fn strtok_emulation() {
        let parts = |cmd| match cast_parts(cmd) {
            CastParts::NoName => "noname".to_owned(),
            CastParts::NoQuoted => "noquoted".to_owned(),
            CastParts::Parts(spell, rest) => format!("{spell}|{rest:?}"),
        };
        assert_eq!(parts("dg_cast 'fireball' bob"), "fireball|Some(\" bob\")");
        assert_eq!(parts("dg_cast 'fireball'"), "fireball|None");
        assert_eq!(parts("dg_cast 'fireball' "), "fireball|Some(\" \")");
        assert_eq!(parts("dg_cast 'fireball"), "fireball|None");
        assert_eq!(parts("dg_cast fireball"), "noquoted");
        assert_eq!(parts("dg_cast"), "noquoted");
        assert_eq!(parts(""), "noname");
        assert_eq!(parts("'''"), "noname");
        // Leading quotes are skipped, as strtok does.
        assert_eq!(parts("'a' b"), " b|None");
        assert_eq!(parts("dg_cast ''x' y"), "x|Some(\" y\")");
        assert_eq!(parts("dg_cast '' "), " |None");
        assert_eq!(parts("dg_cast '\u{e9}' \u{e9}"), "\u{e9}|Some(\" \u{e9}\")");
        // The target text is taken whole, quotes and all.
        assert_eq!(parts("x 'a' b'c"), "a|Some(\" b'c\")");
    }

    #[test]
    fn a_mob_casts_as_itself_at_its_own_level() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast 'magic missile' bob");
        assert_eq!(
            w.host.effects,
            vec![format!(
                "call_magic spell=32 level=17 caster={} victim={} obj=- room={}",
                w.mob, w.bob, w.room
            )]
        );
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn the_target_is_the_first_word_lowercased_with_fill_words_skipped() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast 'magic missile'  at  BOB and more");
        assert_eq!(w.host.effects.len(), 1);
        assert!(w.host.effects[0].contains(&format!("victim={}", w.bob)));
        // By id.
        let mut w = world();
        let id = uid_text(w.bob);
        cast_as_mob(&mut w, &format!("dg_cast 'magic missile' {id}"));
        assert!(w.host.effects[0].contains(&format!("victim={}", w.bob)));
    }

    #[test]
    fn a_spell_that_ignores_targets_needs_none() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast 'earthquake'");
        cast_as_mob(&mut w, "dg_cast 'earthquake' nobody-here");
        assert_eq!(w.host.effects.len(), 2);
        assert!(w.host.effects[1].contains("victim=- obj=-"));
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn a_targeted_spell_with_no_target_text_is_cast_untargeted() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast 'armor'");
        cast_as_mob(&mut w, "dg_cast 'armor' the");
        assert_eq!(w.host.effects.len(), 2);
        assert!(w.host.effects.iter().all(|e| e.contains("victim=- obj=-")));
    }

    #[test]
    fn an_object_target_is_tried_when_no_character_matches() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast 'identify' sword");
        assert!(w.host.effects[0].contains(&format!("victim=- obj={}", w.sword)));
        // A character wins over an object.
        let mut w = world();
        let bob = w.bob;
        w.host.set_text(w.sword, "alias", "bob");
        cast_as_mob(&mut w, "dg_cast 'identify' bob");
        assert!(w.host.effects[0].contains(&format!("victim={bob} obj=-")));
    }

    #[test]
    fn an_object_is_not_a_target_of_a_character_only_spell() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast 'armor' sword");
        assert!(w.host.effects.is_empty());
        assert_eq!(
            w.host.logs,
            vec!["Trigger: trigger 9, VNum 9. dg_cast: target not found (dg_cast 'armor' sword)"]
        );
    }

    #[test]
    fn target_not_found_is_logged() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast 'magic missile' nobody");
        assert!(w.host.effects.is_empty());
        assert_eq!(
            w.host.logs,
            vec![
                "Trigger: trigger 9, VNum 9. dg_cast: target not found (dg_cast 'magic missile' nobody)"
            ]
        );
    }

    #[test]
    fn a_target_scripts_may_not_touch_is_not_found() {
        let mut w = world();
        w.host.set_num(w.bob, "dg_invalid", 1);
        cast_as_mob(&mut w, "dg_cast 'magic missile' bob");
        assert!(w.host.effects.is_empty());
        assert_eq!(w.host.logs.len(), 1);
    }

    #[test]
    fn group_spells_are_refused() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast 'group heal'");
        assert!(w.host.effects.is_empty());
        assert_eq!(
            w.host.logs,
            vec![
                "Trigger: trigger 9, VNum 9. dg_cast: group spells not permitted (dg_cast 'group heal')"
            ]
        );
    }

    #[test]
    fn target_lookup_comes_before_the_group_check() {
        let mut w = world();
        w.host.add_spell("mass heal", 65, TAR_CHAR_ROOM, MAG_GROUPS);
        cast_as_mob(&mut w, "dg_cast 'mass heal' nobody");
        assert!(w.host.logs[0].contains("target not found"));
    }

    #[test]
    fn spell_name_errors() {
        let mut w = world();
        cast_as_mob(&mut w, "dg_cast");
        cast_as_mob(&mut w, "dg_cast 'unknown spell' bob");
        cast_as_mob(&mut w, "dg_cast 'reserved' bob");
        cast_as_mob(&mut w, "");
        cast_as_mob(&mut w, "'''");
        assert!(w.host.effects.is_empty());
        assert_eq!(
            w.host.logs,
            vec![
                "Trigger: trigger 9, VNum 9. dg_cast needs spell name in `'s.",
                "Trigger: trigger 9, VNum 9. dg_cast: invalid spell name (dg_cast 'unknown spell' bob)",
                "Trigger: trigger 9, VNum 9. dg_cast: invalid spell name (dg_cast 'reserved' bob)",
                "Trigger: trigger 9, VNum 9. dg_cast needs spell name.",
                "Trigger: trigger 9, VNum 9. dg_cast needs spell name.",
            ]
        );
    }

    #[test]
    fn spell_numbers_outside_the_range_are_invalid() {
        let mut w = world();
        w.host.add_spell("zero", 0, TAR_IGNORE, 0);
        w.host.add_spell("toohigh", MAX_SPELLS + 1, TAR_IGNORE, 0);
        w.host.add_spell("top", MAX_SPELLS, TAR_IGNORE, 0);
        cast_as_mob(&mut w, "dg_cast 'zero'");
        cast_as_mob(&mut w, "dg_cast 'toohigh'");
        assert!(w.host.effects.is_empty());
        cast_as_mob(&mut w, "dg_cast 'top'");
        assert_eq!(w.host.effects.len(), 1);
    }

    #[test]
    fn an_object_casts_through_a_proxy_named_for_it() {
        let mut w = world();
        let (sword, room, bob) = (w.sword, w.room, w.bob);
        run(&mut w, sword, AttachType::Obj, |cx| {
            do_dg_cast(cx, "dg_cast 'magic missile' bob");
        });
        // The proxy stood at the head of the room's people during the call
        // and was extracted afterwards.
        assert_eq!(w.host.effects.len(), 2);
        let proxy = w.host.character_list[0];
        assert_eq!(
            w.host.effects[0],
            format!(
                "call_magic spell=32 level=25 caster={proxy} victim={bob} obj=- room={}",
                room
            )
        );
        assert_eq!(w.host.effects[1], format!("extract_char {proxy}"));
        assert_eq!(w.host.name(proxy), "sword");
        assert_eq!(w.host.vnum(proxy), DG_CASTER_PROXY);
        assert!(w.host.is_dead(proxy));
        assert_eq!(w.host.list(room, "people")[0], proxy);
    }

    #[test]
    fn an_object_in_a_mobs_hands_casts_in_that_mobs_room() {
        let mut w = world();
        w.host.obj_to_char(w.sword, w.mob);
        let sword = w.sword;
        run(&mut w, sword, AttachType::Obj, |cx| {
            do_dg_cast(cx, "dg_cast 'earthquake'");
        });
        assert!(w.host.effects[0].ends_with(&format!("room={}", w.room)));
    }

    #[test]
    fn an_object_with_no_room_cannot_cast() {
        let mut w = world();
        let loose = w.host.add_obj(201, "loose");
        run(&mut w, loose, AttachType::Obj, |cx| {
            do_dg_cast(cx, "dg_cast 'earthquake'");
        });
        assert!(w.host.effects.is_empty());
        assert_eq!(
            w.host.logs,
            vec!["dg_do_cast: unknown room for object-caster!"]
        );
    }

    #[test]
    fn a_room_casts_as_the_gods() {
        let mut w = world();
        let (room, bob) = (w.room, w.bob);
        run(&mut w, room, AttachType::Wld, |cx| {
            do_dg_cast(cx, "dg_cast 'magic missile' bob");
        });
        let proxy = w.host.character_list[0];
        assert_eq!(
            w.host.effects[0],
            format!(
                "call_magic spell=32 level=25 caster={proxy} victim={bob} obj=- room={}",
                room
            )
        );
        assert_eq!(w.host.name(proxy), "The gods");
    }

    #[test]
    fn a_proxy_that_cannot_load_is_logged() {
        let mut w = world();
        w.host.globals.nums.insert("no_caster_proxy".to_owned(), 1);
        let room = w.room;
        run(&mut w, room, AttachType::Wld, |cx| {
            do_dg_cast(cx, "dg_cast 'earthquake'");
        });
        assert!(w.host.effects.is_empty());
        assert_eq!(w.host.logs, vec!["dg_cast: Cannot load the caster mob!"]);
    }

    #[test]
    fn the_proxy_is_not_loaded_when_the_cast_is_refused() {
        let mut w = world();
        let room = w.room;
        let before = w.host.character_list.len();
        run(&mut w, room, AttachType::Wld, |cx| {
            do_dg_cast(cx, "dg_cast 'magic missile' nobody");
        });
        assert_eq!(w.host.character_list.len(), before);
    }

    fn affect(w: &mut World, cmd: &str) {
        let mob = w.mob;
        run(w, mob, AttachType::Mob, |cx| do_dg_affect(cx, cmd));
    }

    #[test]
    fn name_tables_match_the_c() {
        assert_eq!(APPLY_TYPES.len(), 25);
        assert_eq!(APPLY_TYPES[17], "ARMOR");
        assert_eq!(APPLY_TYPES[24], "SAVING_SPELL");
        assert_eq!(AFFECTED_BITS.len(), 23);
        assert_eq!(AFFECTED_BITS[0], "");
        assert_eq!(AFFECTED_BITS[8], "SANCT");
        assert_eq!(AFFECTED_BITS[22], "CHARM");
    }

    #[test]
    fn an_apply_affect() {
        let mut w = world();
        affect(&mut w, "dg_affect bob armor -20 5");
        assert_eq!(
            w.host.effects,
            vec![format!(
                "affect_to_char {} location=17 modifier=-20 duration=4 bit=-",
                w.bob
            )]
        );
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn a_bit_affect_takes_the_bit_number_and_the_value_verbatim() {
        let mut w = world();
        affect(&mut w, "dg_affect bob SANCT 1 10");
        affect(&mut w, "dg_affect bob det-invis 0 1");
        assert_eq!(
            w.host.effects,
            vec![
                format!(
                    "affect_to_char {} location=0 modifier=1 duration=9 bit=8",
                    w.bob
                ),
                format!(
                    "affect_to_char {} location=0 modifier=0 duration=0 bit=4",
                    w.bob
                ),
            ]
        );
    }

    #[test]
    fn apply_names_win_over_affect_names() {
        let mut w = world();
        // "NONE" is an apply (index 0); the empty-named bit 0 is unreachable.
        affect(&mut w, "dg_affect bob none 3 2");
        assert_eq!(
            w.host.effects,
            vec![format!(
                "affect_to_char {} location=0 modifier=3 duration=1 bit=-",
                w.bob
            )]
        );
    }

    #[test]
    fn off_removes_the_dg_affects() {
        let mut w = world();
        affect(&mut w, "dg_affect bob str 3 4");
        affect(&mut w, "dg_affect bob str OFF 1");
        assert_eq!(w.host.effects.len(), 2);
        assert_eq!(w.host.effects[1], format!("affect_from_char {}", w.bob));
        assert_eq!(w.host.num(w.bob, "dg_affects"), 0);
    }

    #[test]
    fn off_still_needs_a_positive_duration_and_a_known_property() {
        let mut w = world();
        affect(&mut w, "dg_affect bob str off 0");
        affect(&mut w, "dg_affect bob bogus off 5");
        assert!(w.host.effects.is_empty());
        assert_eq!(w.host.logs.len(), 3);
    }

    #[test]
    fn usage_errors() {
        let mut w = world();
        affect(&mut w, "dg_affect");
        affect(&mut w, "dg_affect bob");
        affect(&mut w, "dg_affect bob str");
        affect(&mut w, "dg_affect bob str 3");
        assert!(w.host.effects.is_empty());
        let usage =
            "Trigger: trigger 9, VNum 9. dg_affect usage: <target> <property> <value> <duration>";
        assert_eq!(w.host.logs, vec![usage; 4]);
    }

    #[test]
    fn a_non_positive_duration_is_refused_with_the_line() {
        let mut w = world();
        affect(&mut w, "dg_affect bob str 3 0");
        affect(&mut w, "dg_affect bob str 3 -4");
        affect(&mut w, "dg_affect bob str 3 soon");
        assert!(w.host.effects.is_empty());
        assert_eq!(
            w.host.logs[0],
            "Trigger: trigger 9, VNum 9. dg_affect: need positive duration!"
        );
        assert_eq!(w.host.logs[1], "Line was: dg_affect bob str 3 0 (0)");
        assert_eq!(w.host.logs[3], "Line was: dg_affect bob str 3 -4 (-4)");
        assert_eq!(w.host.logs[5], "Line was: dg_affect bob str 3 soon (0)");
    }

    #[test]
    fn the_duration_is_the_leading_integer_of_the_rest_of_the_line() {
        let mut w = world();
        affect(&mut w, "dg_affect bob str 3 7 ticks and more");
        assert!(w.host.effects[0].contains("duration=6"));
    }

    #[test]
    fn unknown_property_and_target() {
        let mut w = world();
        affect(&mut w, "dg_affect bob flurble 1 5");
        affect(&mut w, "dg_affect nobody str 1 5");
        assert!(w.host.effects.is_empty());
        assert_eq!(
            w.host.logs,
            vec![
                "Trigger: trigger 9, VNum 9. dg_affect: unknown property 'flurble'!",
                "Trigger: trigger 9, VNum 9. dg_affect: cannot locate target!",
            ]
        );
    }

    #[test]
    fn the_target_may_be_an_id_and_the_value_is_atoi() {
        let mut w = world();
        let id = uid_text(w.bob);
        affect(&mut w, &format!("dg_affect {id} dex 12abc 3"));
        assert_eq!(
            w.host.effects,
            vec![format!(
                "affect_to_char {} location=2 modifier=12 duration=2 bit=-",
                w.bob
            )]
        );
    }

    #[test]
    fn property_names_with_odd_case_and_padding() {
        let mut w = world();
        affect(&mut w, "dg_affect   bob    Saving_Spell   -2    9");
        assert!(w.host.effects[0].contains("location=24 modifier=-2 duration=8"));
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let mut w = world();
        for cmd in [
            "",
            "'",
            "''",
            "dg_cast '",
            "dg_cast ' ' ",
            "dg_cast '\u{e9}",
            "dg_cast 'magic missile' \u{e9}\u{e9}",
            &format!("dg_cast 'magic missile' {}", "x".repeat(5000)),
        ] {
            cast_as_mob(&mut w, cmd);
        }
        for cmd in [
            "",
            "dg_affect \u{e9} \u{e9} \u{e9} \u{e9}",
            "dg_affect bob str 99999999999999999999 99999999999999999999",
            "dg_affect bob str 1 2147483647",
            "dg_affect bob str 1 -2147483648",
        ] {
            affect(&mut w, cmd);
        }
    }
}
