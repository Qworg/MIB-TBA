//! The script command interpreters: `dg_mobcmd.c`, `dg_objcmd.c`,
//! `dg_wldcmd.c`, and the message formatter in `dg_comm.c`.
//!
//! # Layout
//!
//! | file | ports |
//! |---|---|
//! | `mod.rs` | the three dispatch tables and interpreters (`script_command_interpreter`, `obj_command_interpreter`, `wld_command_interpreter`) and the [`CmdHost`] trait |
//! | `comm.rs` | `dg_comm.c`: `sub_write`, `any_one_name`, `send_to_zone`, `send_to_range`, and the slice of `act()` that `act_to_room` and `send_char_pos` use |
//! | `misc.rs` | `dg_misc.c`: `script_damage` and `send_char_pos` |
//! | `shared.rs` | what the three command files have in common: the log prefixes, `door`, `zoneecho`, `recho`, `load`, the room-finding functions |
//! | `mob.rs`, `obj.rs`, `wld.rs` | the per-type commands |
//!
//! `mdoor`/`odoor`/`wdoor`, `mzoneecho`/`ozoneecho`/`wzoneecho`,
//! `mrecho`/`orecho`/`wrecho` and `mload`/`oload`/`wload` are each one
//! function parameterised by the owner's [`AttachType`](crate::types::AttachType); the C copies differ
//! only in log wording and in how targets are looked up, and both are kept.
//!
//! # Command-word matching
//!
//! The mob table is matched on the whole word, case-insensitively
//! (`str_cmp`). The object and world tables are matched with
//! `strncmp(entry, word, strlen(word))` against entries that **end in a
//! space** (`"oecho "`), in table order: so a prefix of a command selects the
//! first entry it begins, the full name selects its own entry (`oecho` begins
//! `"oecho "` but `oechoaround` does not begin `"oecho "`), and `wlog`, the one
//! world entry without a trailing space, can only be reached by typing
//! `wlog` in full (`wlo` and `wl` select `wload`). A word longer than an entry
//! never matches it, since the entry's terminator differs from the word's next
//! byte.
//!
//! # Quirks kept
//!
//! - `mload`/`oload`/`wload` read the room target from the text *after* the
//!   vnum with its leading space, so `isdigit(*target)` is asked of a space and
//!   a room target is always refused; a trailing space counts as a target.
//! - `wload` sets `lastloaded` for an object only when no target is given, and
//!   says `mload: bad mob vnum`; `mzoneecho` and `mrecho` skip the charm test.
//! - `mremember` keeps the command text with the space that followed the name;
//!   `mforce`, `mat`, `oforce`, `wforce` pass their command on the same way, and
//!   test it for emptiness *before* skipping that space, so a trailing space is
//!   a command.
//! - `mdoor` and its copies create a missing exit with every field zero, which
//!   leaves it pointing at room number 0 until `room` is set.
//! - `oset` is `osetval` (a prefix of `"osetval "`), `o` is `oasound`, and
//!   `reserved` in any case is nothing.
//! - `find_target_room` counts a `N.name` prefix down through the character
//!   search and carries what is left into the object search.
//! - `sub_write` reads a name with `any_one_name`, which stops at punctuation:
//!   an id (`}123`) is therefore never a name there.
//!
//! # Deviations from the C
//!
//! - The C logs through `script_vlog` with the owner's name spliced into the
//!   format string, so a `%` in a name is interpreted as a conversion. Here the
//!   name is data.
//! - Where the C would read or write out of bounds, crash, or overflow a
//!   buffer, the code takes the nearest safe path and says so where it
//!   happens: a character or object in no room (`world[NOWHERE]`), the `mjunk`
//!   word shorter than four bytes (`arg[3]`, `arg+4`), the empty command word
//!   that selects the NULL `RESERVED` table entry, a trailing `$` in `act`,
//!   the fixed-size `sprintf` buffers of the zone echoes, `wmove` of an object
//!   that is not lying in a room, and the unbounded recursion of `oat`/`wat`
//!   (nesting is capped at `obj::MAX_NEST` levels, 256).
//! - The `SYSERR` lines written when an object or mob is placed in `NOWHERE`
//!   (`obj_to_room`, `char_to_room`) and when `wmove` finds an object that is
//!   not in a room are approximations of the C text: the C prints a pointer
//!   and the room number, which the VM does not have.
//! - `GET_LEVEL(ch->desc->original)` of a descriptor that is not switched
//!   dereferences NULL in the C; the VM uses the character's own level
//!   instead ([`CmdHost::cmd_desc`] reports only the descriptor and its
//!   `original`).
//! - `act()` to a room follows `to->next_in_room` live, as the C does, so an
//!   act trigger that moves a recipient carries the walk into the list it is
//!   in now. A recipient that is in no room any more ends the walk, as in the
//!   C (`char_from_room` leaves its `next_in_room` NULL). The `all` forms of
//!   `oforce`, `wforce`, `mteleport`, `oteleport` and `wteleport` save
//!   `next_ch` before their body and follow it the same way
//!   (`comm::for_each_occupant`). Both walks are cut off after 100000
//!   steps, which the C does not do.
//! - `dg_act_check`, the static of `comm.c` that every `act()` overwrites, is
//!   [`crate::types::VmState::act_check`], so that the `act()` of the game
//!   and this one share it: an `act` trigger that makes the game say
//!   something switches it off for the rest of the outer walk.
//! - `sub_write` takes the room to work in as a parameter rather than reading
//!   `IN_ROOM(ch)`, so `do_masound`'s temporary reassignment of the mob's room
//!   needs no host support.
//! - `mjunk` reproduces the C's clobbered buffer exactly (see
//!   [`mob`]): `find_all_dots` rewrites `all.name` in place and the loop then
//!   reads `arg[3]` and `arg+4` of it.
//! - `do_oat`'s search of `object_list` for the duplicate and of the lookup
//!   table for the owner are [`crate::host::BaseHost::exists`] tests (ids are
//!   never reused).
//! - `send_to_char` is [`CmdHost::cmd_send`]: a character with no descriptor
//!   receives nothing, as in the C. `sub_write` never offers its text to an
//!   NPC's `act` triggers; `act()` does, through
//!   [`crate::dispatch::act_mtrigger`].
//! - Vnum arguments are wrapped to 16 bits (`room_vnum`, `mob_vnum` and
//!   `obj_vnum` are `ush_int`) where the C passes them on; `NOBODY` and
//!   `NOTHING` print as 65535 in `mob_log` / `obj_log`.
//! - The `*_vis` lookups' `get_number` strips `N.` from the caller's buffer;
//!   later lookups and log lines in the same command see the bare name.
//! - `update_pos`, `die`, `hit`, `char_to_room` and the other `handler.c`
//!   routines are host primitives; their internals are not ported here.
//! - `sh_int` hit points: `script_damage` truncates to 16 bits as the C's
//!   `GET_HIT(vict) -= dam` does.

pub mod comm;
pub mod misc;
pub mod mob;
pub mod obj;
pub mod shared;
pub mod wld;

#[cfg(test)]
mod script_tests;
#[cfg(test)]
mod support;

use crate::cstr::{any_one_arg, skip_spaces};
use crate::host::{Cx, DgHost};
use crate::types::Uid;

pub use comm::{TO_CHAR, TO_ROOM, sub_write};
pub use misc::{script_damage, send_char_pos};

/// One field of a room exit (`struct room_direction_data`) a script can set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitField {
    /// `general_description`: the text exactly as stored (the VM has already
    /// appended `"\r\n"`).
    Description(String),
    /// `exit_info`, already truncated to the C's `sh_int`.
    Flags(i32),
    /// `key`, an `obj_vnum`: already narrowed to 16 bits by the VM.
    Key(i32),
    /// `keyword`.
    Keyword(String),
    /// `to_room`: `None` is `NOWHERE`.
    ToRoom(Option<Uid>),
}

/// The world effects script commands have.
///
/// Every method is one fact read or one effect, named for the C expression it
/// stands for. Characters are named by [`Uid`]; a method handed an id that no
/// longer exists does nothing and returns the neutral value.
pub trait CmdHost {
    // ---- text delivery -------------------------------------------------

    /// `CONFIG_HUH`: what a command a character may not use answers.
    fn cmd_huh(&self) -> String;

    /// `send_to_char(ch, "%s", text)`: queue `text` for the character's
    /// connection. Does nothing for a character with no descriptor.
    fn cmd_send(&mut self, ch: Uid, text: &str);

    /// `ch->desc`: `None` when `ch->desc` is `NULL`, else `Some` of
    /// `ch->desc->original`, the player behind a switched mob (`None` inside
    /// when the descriptor is not switched). The VM reads descriptor
    /// presence from the outer `Option` and applies the policy for an
    /// unswitched descriptor itself.
    fn cmd_desc(&self, ch: Uid) -> Option<Option<Uid>>;

    /// `mudlog(log_type, level, TRUE, "%s", text)`: `log_type` is `OFF` 0,
    /// `BRF` 1, `NRM` 2 or `CMP` 3 ([`crate::types::MUDLOG_BRF`] and
    /// friends); `level` is the lowest immortal level that sees the line.
    fn cmd_mudlog(&mut self, log_type: i32, level: i32, text: &str);

    /// `log("%s", text)`: a line for the server log only (the `SYSERR` lines
    /// of `act` and the object and room handlers). Not shown to immortals.
    fn cmd_syslog(&mut self, text: &str);

    /// The characters of `descriptor_list` whose `connected` is `CON_PLAYING`
    /// (`i->character` for each `!i->connected`), in list order.
    fn cmd_playing_characters(&self) -> Vec<Uid>;

    // ---- character facts -----------------------------------------------

    /// `AFF_FLAGGED(ch, AFF_CHARM)`.
    fn cmd_is_charmed(&self, ch: Uid) -> bool;

    /// `GET_INVIS_LEV(ch)`.
    fn cmd_invis_level(&self, ch: Uid) -> i32;

    /// `die(ch, NULL)`.
    fn cmd_die(&mut self, ch: Uid);

    // ---- combat and following ------------------------------------------

    /// `stop_fighting(ch)`.
    fn cmd_stop_fighting(&mut self, ch: Uid);

    /// `hit(ch, victim, TYPE_UNDEFINED)`.
    fn cmd_hit_victim(&mut self, ch: Uid, victim: Uid);

    /// `HUNTING(ch) = victim`.
    fn cmd_set_hunting(&mut self, ch: Uid, victim: Uid);

    /// The "stop following someone else first" block of `do_mfollow`: remove
    /// `ch`'s entry from `ch->master->followers` and set `ch->master = NULL`.
    /// It is not `stop_follower`: no affect is removed and nothing is shown.
    fn cmd_unfollow(&mut self, ch: Uid);

    /// The tail of `do_mfollow`: `ch->master = leader` and a new follower
    /// entry for `ch` at the head of `leader->followers`.
    fn cmd_follow(&mut self, ch: Uid, leader: Uid);

    // ---- rooms ---------------------------------------------------------

    /// `do_mdoor` "purge": free the exit if there is one
    /// (`rm->dir_option[dir] = NULL`).
    fn cmd_exit_purge(&mut self, room: Uid, dir: usize);

    /// Set one field of an exit. When the room has no exit that way, first
    /// `CREATE`s one: every field zero, which makes `to_room` room number 0
    /// (the first room in the world table), not `NOWHERE`.
    fn cmd_exit_set(&mut self, room: Uid, dir: usize, field: ExitField);

    /// `House_can_enter(ch, room_vnum)`.
    fn cmd_house_can_enter(&self, ch: Uid, room_vnum: i32) -> bool;

    /// `real_zone_by_thing(vnum)`: the zone whose vnum range holds `vnum`,
    /// `None` for `NOWHERE`. The zone is named by its number
    /// (`zone_table[zone].number`), the value `RoomHost::room_zone_number`
    /// reports, rather than by its table index; the two are in one-to-one
    /// correspondence, so comparing them selects the same rooms.
    fn cmd_zone_of_vnum(&self, vnum: i32) -> Option<i32>;

    /// The rooms whose vnum is in `start..=finish`, in world-table order.
    fn cmd_rooms_in_range(&self, start: i32, finish: i32) -> Vec<Uid>;

    // ---- moving and removing things ------------------------------------

    /// `char_from_room(ch)`.
    fn cmd_char_from_room(&mut self, ch: Uid);

    /// `char_to_room(ch, room)`.
    fn cmd_char_to_room(&mut self, ch: Uid, room: Uid);

    /// `obj_to_room(obj, room)`.
    fn cmd_obj_to_room(&mut self, obj: Uid, room: Uid);

    /// `obj_to_char(obj, ch)`.
    fn cmd_obj_to_char(&mut self, obj: Uid, ch: Uid);

    /// `obj_to_obj(obj, container)`.
    fn cmd_obj_to_obj(&mut self, obj: Uid, container: Uid);

    /// Whichever of `obj_from_room`, `obj_from_char` or `obj_from_obj`
    /// applies to where the object is. Not for a worn object
    /// ([`CmdHost::cmd_unequip_char`]).
    fn cmd_obj_detach(&mut self, obj: Uid);

    /// `equip_char(ch, obj, pos)`.
    fn cmd_equip_char(&mut self, ch: Uid, obj: Uid, pos: i32);

    /// `unequip_char(ch, pos)`: the object that was worn there, now carried
    /// by nobody.
    fn cmd_unequip_char(&mut self, ch: Uid, pos: i32) -> Option<Uid>;

    /// `extract_char(ch)`: deferred, as [`crate::host`] describes.
    fn cmd_extract_char(&mut self, ch: Uid);

    /// `extract_obj(obj)`: immediate, and takes the contents with it.
    fn cmd_extract_obj(&mut self, obj: Uid);

    /// `read_mobile(vnum, VIRTUAL)`: a new mob, in no room; `None` for a vnum
    /// that does not exist.
    fn cmd_read_mobile(&mut self, vnum: i32) -> Option<Uid>;

    /// `read_object(vnum, VIRTUAL)`: a new object, in no place; `None` for a
    /// vnum that does not exist.
    fn cmd_read_object(&mut self, vnum: i32) -> Option<Uid>;

    /// The `memcpy(ch, &tmpmob, sizeof(*ch))` of `do_mtransform`: copy the
    /// prototype data of mob `from` (a temporary mob from
    /// [`CmdHost::cmd_read_mobile`]) over `ch` while keeping `ch`'s identity
    /// (script id, affects, carried items, scripts, memory, events, list
    /// links, followers, master, group, was-in-room, gold, position, carrying
    /// weight and count, fighting and hunting) and, when `keep_hp`, its hit
    /// points, maximum hit points and experience. `ch->nr` is restored to its
    /// old value afterwards (`ch->nr = this_rnum`), so the vnum of `ch` is
    /// NOT changed by a transform.
    fn cmd_overwrite_mob(&mut self, ch: Uid, from: Uid, keep_hp: bool);

    /// The `memcpy(obj, &tmpobj, sizeof(*obj))` of `do_otransform`: copy the
    /// prototype data of object `from` (a temporary object from
    /// [`CmdHost::cmd_read_object`]) over `obj` while keeping its place
    /// (room, carrier, wearer and wear position, container), contents, script
    /// id, scripts and list links. Unlike mobs, `item_number` is copied, so
    /// the vnum of `obj` becomes that of `from`.
    fn cmd_overwrite_obj(&mut self, obj: Uid, from: Uid);
}

/// `script_command_interpreter(ch, cmd)` (`interpreter.c`): run `cmd` if its
/// first word is one of the mob script commands (`mecho`, `mload`, …).
/// Returns `false` when it is not, and the driver then hands the line to
/// [`crate::host::BaseHost::mud_command`].
///
/// Only a whole word matches (`str_cmp`), so `mech` is not `mecho`.
pub fn script_command_interpreter<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) -> bool {
    let arg = skip_spaces(cmd);
    if arg.is_empty() {
        return false;
    }
    let (first, line) = any_one_arg(arg);
    match mob::MOB_SCRIPT_COMMANDS
        .iter()
        .find(|(name, _)| crate::cstr::str_eq(&first, name))
    {
        Some((_, command)) => {
            mob::run(cx.host, cx.owner, *command, line);
            true
        }
        None => false,
    }
}

/// `obj_command_interpreter(obj, argument)` (`dg_objcmd.c`).
pub fn obj_command_interpreter<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    obj::interpret(cx.host, cx.owner, cmd);
}

/// `wld_command_interpreter(room, argument)` (`dg_wldcmd.c`).
pub fn wld_command_interpreter<H: DgHost + ?Sized>(cx: &mut Cx<'_, H>, cmd: &str) {
    wld::interpret(cx.host, cx.owner, cmd);
}

/// The index of the first table entry that `strncmp(entry, word,
/// strlen(word))` accepts. `word` is already lower-cased by `any_one_arg`; the
/// comparison is case-sensitive. An entry shorter than the word never
/// matches. An empty word accepts the first entry.
pub(crate) fn strncmp_lookup(table: &[&str], word: &str) -> Option<usize> {
    let word = word.as_bytes();
    table.iter().position(|entry| {
        let entry = entry.as_bytes();
        entry.len() >= word.len() && entry[..word.len()] == *word
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::support::*;
    use super::*;
    use crate::host::WorldHost;

    #[test]
    fn strncmp_lookup_matches_prefixes_of_space_terminated_entries() {
        let table = ["RESERVED", "oecho ", "oechoaround ", "olog "];
        assert_eq!(strncmp_lookup(&table, "oecho"), Some(1));
        assert_eq!(strncmp_lookup(&table, "oec"), Some(1));
        assert_eq!(strncmp_lookup(&table, "oechoa"), Some(2));
        assert_eq!(strncmp_lookup(&table, "oechoaround"), Some(2));
        assert_eq!(strncmp_lookup(&table, "oechoaroundx"), None);
        assert_eq!(strncmp_lookup(&table, "o"), Some(1));
        assert_eq!(strncmp_lookup(&table, "olog"), Some(3));
        assert_eq!(strncmp_lookup(&table, "ologs"), None);
        assert_eq!(strncmp_lookup(&table, "reserved"), None);
        assert_eq!(strncmp_lookup(&table, ""), Some(0));
        assert_eq!(strncmp_lookup(&table, "\u{e9}"), None);
        // An entry with no trailing space is reached only by its full name.
        let table = ["wload ", "wlog"];
        assert_eq!(strncmp_lookup(&table, "wl"), Some(0));
        assert_eq!(strncmp_lookup(&table, "wlog"), Some(1));
        assert_eq!(strncmp_lookup(&table, "wlogs"), None);
    }

    #[test]
    fn the_mob_table_matches_whole_words_only() {
        let mut w = World::new();
        let mob = w.mob;
        assert!(w.run_mob(mob, " mlog hello"));
        assert_eq!(w.log(), "Mob (the guard, VNum 3000):: hello");
        // A prefix, a longer word, a bare space and the empty line are not commands.
        assert!(!w.run_mob(mob, "mlo hello"));
        assert!(!w.run_mob(mob, "mlogs hello"));
        assert!(!w.run_mob(mob, "   "));
        assert!(!w.run_mob(mob, ""));
        assert!(!w.run_mob(mob, "say hello"));
        assert_eq!(w.host.logs.len(), 1);
        // Case-insensitive, leading space skipped.
        assert!(w.run_mob(mob, "   MLOG shout"));
        assert_eq!(w.log(), "Mob (the guard, VNum 3000):: shout");
        // A leading tab is not skipped, so the first word is empty.
        assert!(!w.run_mob(mob, "\tmlog x"));
        assert_eq!(w.host.logs.len(), 2);
    }

    #[test]
    fn the_mob_table_lists_every_command_in_order() {
        let names: Vec<&str> = mob::MOB_SCRIPT_COMMANDS.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "masound",
                "mkill",
                "mjunk",
                "mdamage",
                "mdoor",
                "mecho",
                "mrecho",
                "mechoaround",
                "msend",
                "mload",
                "mpurge",
                "mgoto",
                "mat",
                "mteleport",
                "mforce",
                "mhunt",
                "mremember",
                "mforget",
                "mtransform",
                "mzoneecho",
                "mfollow",
                "mlog"
            ]
        );
    }

    #[test]
    fn the_obj_table_matches_prefixes_in_table_order() {
        let mut w = World::new();
        let obj = w.obj;
        let prefix = "Obj (a lamp, VNum 7000):: ";
        w.run_obj(obj, "olog full");
        assert_eq!(w.log(), format!("{prefix}full"));
        // `olo` begins "oload " before it begins "olog ".
        w.run_obj(obj, "olo abbreviated");
        assert_eq!(w.log(), format!("{prefix}oload: bad syntax"));
        // A word that is no prefix of any entry is logged.
        w.run_obj(obj, "ologx nope");
        assert_eq!(w.log(), format!("{prefix}Unknown object cmd: 'ologx nope'"));
        // An empty line is ignored; so is one of spaces.
        let before = w.host.logs.len();
        w.run_obj(obj, "");
        w.run_obj(obj, "    ");
        assert_eq!(w.host.logs.len(), before);
        // `o` begins "oasound " first, `oec` "oecho " before "oechoaround ".
        w.run_obj(obj, "o");
        assert_eq!(w.log(), format!("{prefix}oasound called with no args"));
        w.run_obj(obj, "oec");
        assert_eq!(w.log(), format!("{prefix}oecho called with no args"));
        w.run_obj(obj, "oecho");
        assert_eq!(w.log(), format!("{prefix}oecho called with no args"));
        w.run_obj(obj, "oechoa");
        assert_eq!(w.log(), format!("{prefix}osend called with no args"));
        w.run_obj(obj, "OECHOAROUND");
        assert_eq!(w.log(), format!("{prefix}osend called with no args"));
        // The RESERVED entry is matched case-sensitively, so no word reaches
        // it; an empty first word (a leading tab) does, and is reported.
        w.run_obj(obj, "reserved");
        assert_eq!(w.log(), format!("{prefix}Unknown object cmd: 'reserved'"));
        w.run_obj(obj, "\tolog x");
        assert_eq!(w.log(), format!("{prefix}Unknown object cmd: '\tolog x'"));
        // Non-ASCII words never match.
        w.run_obj(obj, "\u{e9}cho");
        assert!(w.log().contains("Unknown object cmd"));
    }

    #[test]
    fn the_obj_table_lists_every_command_in_order() {
        let names: Vec<&str> = obj::OBJ_CMD_INFO.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "RESERVED",
                "oasound ",
                "oat ",
                "odoor ",
                "odamage ",
                "oecho ",
                "oechoaround ",
                "oforce ",
                "oload ",
                "opurge ",
                "orecho ",
                "osend ",
                "osetval ",
                "oteleport ",
                "otimer ",
                "otransform ",
                "ozoneecho ",
                "omove ",
                "olog "
            ]
        );
    }

    #[test]
    fn oecho_and_oechoaround_are_different_entries() {
        let mut w = World::new();
        let (obj, p1, p2) = (w.obj, w.player, w.player2);
        // `oechoaround bob hi` goes to everyone in Bob's room but Bob.
        w.run_obj(obj, "oechoaround bob hi");
        assert_eq!(w.sent_to(p1), ["hi\n\r"]);
        assert!(w.sent_to(p2).is_empty());
        w.clear();
        // `oecho` goes to the whole room.
        w.run_obj(obj, "oecho all");
        assert_eq!(w.sent_to(p1), ["all\n\r"]);
        assert_eq!(w.sent_to(p2), ["all\n\r"]);
    }

    #[test]
    fn the_wld_table_reaches_wlog_only_in_full() {
        let mut w = World::new();
        let room = w.room;
        w.run_wld(room, "wlog full");
        assert_eq!(w.log(), "Room 3001 :: full");
        let n = w.host.logs.len();
        // `wlo`/`wl` select wload (which complains of its syntax), not wlog.
        w.run_wld(room, "wlo nope");
        assert_eq!(w.host.logs.len(), n + 1);
        assert_eq!(w.log(), "Room 3001 :: wload: bad syntax");
        w.run_wld(room, "wl");
        assert_eq!(w.log(), "Room 3001 :: wload: bad syntax");
        // "wlogx" matches nothing.
        w.run_wld(room, "wlogx");
        assert_eq!(w.log(), "Room 3001 :: Unknown world cmd: 'wlogx'");
        // wecho and wechoaround.
        w.run_wld(room, "wecho");
        assert_eq!(w.log(), "Room 3001 :: wecho called with no args");
        w.run_wld(room, "wechoaround");
        assert_eq!(w.log(), "Room 3001 :: wsend called with no args");
        w.run_wld(room, "w");
        assert_eq!(w.log(), "Room 3001 :: wasound called with no argument");
        w.run_wld(room, "reserved");
        assert_eq!(w.log(), "Room 3001 :: Unknown world cmd: 'reserved'");
        w.run_wld(room, "\twecho x");
        assert!(w.log().contains("Unknown world cmd"));
        let n = w.host.logs.len();
        w.run_wld(room, "   ");
        assert_eq!(w.host.logs.len(), n);
    }

    #[test]
    fn the_wld_table_lists_every_command_in_order() {
        let names: Vec<&str> = wld::WLD_CMD_INFO.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "RESERVED",
                "wasound ",
                "wdoor ",
                "wecho ",
                "wechoaround ",
                "wforce ",
                "wload ",
                "wpurge ",
                "wrecho ",
                "wsend ",
                "wteleport ",
                "wzoneecho ",
                "wdamage ",
                "wat ",
                "wmove ",
                "wlog"
            ]
        );
    }

    /// Lines as they reach the interpreters from the `.trg` corpus (after
    /// `%…%` substitution): the `%load%` abbreviation `o`, `oset` for
    /// `osetval`, escaped decoration in `%send%`, ids as targets.
    #[test]
    fn lines_from_the_trigger_corpus() {
        let mut w = World::new();
        let (mob, lamp, room, alice, bob) = (w.mob, w.obj, w.room, w.player, w.player2);
        let id = |ch: Uid| crate::types::uid_text(ch);

        // trg 0..: the guard that humiliates.
        assert!(w.run_mob(
            mob,
            &format!(
                "mechoaround {} The guard humiliates Alice, and blocks her way.",
                id(alice)
            )
        ));
        assert_eq!(
            w.sent_to(bob),
            ["The guard humiliates Alice, and blocks her way.\n\r"]
        );
        assert!(w.run_mob(
            mob,
            &format!("msend {} The guard humiliates you.", id(alice))
        ));
        assert_eq!(w.sent_to(alice), ["The guard humiliates you.\n\r"]);
        // The backslash escapes of the ASCII-art `%send%` lines.
        w.clear();
        assert!(w.run_mob(
            mob,
            &format!("msend {} \\@w\\@0     \\|=\\|'    '\\|=\\|\\@n", id(alice))
        ));
        assert_eq!(w.sent_to(alice), ["@w@0     |=|'    '|=|@n\n\r"]);

        // %force% %actor% look, %damage% %actor% 10 and -10, %teleport%.
        assert!(w.run_mob(mob, &format!("mforce {} look", id(alice))));
        assert_eq!(w.host.mud_commands, [(alice, " look".to_owned())]);
        assert!(w.run_mob(mob, &format!("mdamage {} 10", id(alice))));
        assert!(w.run_mob(mob, &format!("mdamage {} -10", id(alice))));
        assert_eq!(w.host.num(alice, "hit"), 20);
        assert!(w.run_mob(mob, &format!("mteleport {} 3002", id(alice))));
        assert_eq!(w.host.in_room(alice), Some(w.room2));

        // %door% 55577 west description You see trees.
        assert!(w.run_mob(mob, "mdoor 3001 west description You see trees."));
        assert_eq!(w.host.text(room, "exit:3:desc"), "You see trees.\r\n");
        // %transform% -1, mgoto <vnum>, mkill by name.
        assert!(w.run_mob(mob, "mgoto 3002"));
        assert_eq!(w.host.in_room(mob), Some(w.room2));
        assert!(w.run_mob(mob, "mgoto 3001"));

        // %load% o 24129 %actor% inv: `o` abbreviates `obj`, `inv` is no place.
        w.host
            .globals
            .texts
            .insert("objproto:24129".to_owned(), "a pin".to_owned());
        assert!(w.run_mob(mob, &format!("mload o 24129 {} inv", id(bob))));
        assert_eq!(w.host.list(bob, "inventory").len(), 1);

        // oset 0 4 is osetval 0 4; otimer 20; oecho with colour codes.
        w.run_obj(lamp, "oset 0 4");
        w.run_obj(lamp, "otimer 20");
        assert_eq!(w.host.num(lamp, "val0"), 4);
        assert_eq!(w.host.num(lamp, "timer"), 20);
        w.host.char_to_room(alice, room);
        w.clear();
        w.run_obj(lamp, "oecho @o@gNo score!@n");
        assert_eq!(w.sent_to(bob), ["@o@gNo score!@n\n\r"]);
        w.clear();
        w.run_obj(
            lamp,
            &format!("oechoaround {} Bob scores a @o@g3@n.", id(alice)),
        );
        assert_eq!(w.sent_to(bob), ["Bob scores a @o@g3@n.\n\r"]);
        assert!(w.host.logs.is_empty());

        // wsend %actor% ..., wechoaround.
        w.run_wld(
            room,
            &format!("wsend {} You hack through the thicket!", id(bob)),
        );
        assert_eq!(
            w.sent_to(bob).last().unwrap(),
            "You hack through the thicket!\n\r"
        );
        w.run_wld(
            room,
            &format!("wechoaround {} Bob hacks down the thicket.", id(bob)),
        );
        assert!(w.host.logs.is_empty());
    }

    #[test]
    fn exit_field_is_plain_data() {
        assert_eq!(ExitField::Key(3), ExitField::Key(3));
        assert_ne!(ExitField::ToRoom(None), ExitField::ToRoom(Some(1)));
    }
}
