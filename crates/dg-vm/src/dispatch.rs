//! `dg_triggers.c`: which triggers fire for which event.
//!
//! Ports every function of `dg_triggers.c`, plus `script_trigger_check` and
//! `check_time_triggers` from `dg_scripts.c`. The game calls these from its
//! own event points (movement, `get`, `say`, combat, the heartbeat…); each
//! doc comment says where the C calls the function and what the caller does
//! with the result.
//!
//! Every function keeps the C's candidate walk (who is examined, in which
//! list order), its guards, the point at which the percent roll is made, the
//! variables it sets (names, text, and the order the C sets them in) and what
//! it returns. A run is made with [`script_driver`] after the event's
//! variables have been added to the instance through the owner's script.
//!
//! # Deviations from the C
//!
//! - The C walks `t = t->next` through a trigger list that a running script
//!   can change (attach, detach, purge of its own owner). Here each walk takes
//!   a snapshot of [`crate::types::Script::trigger_ids`] and re-fetches every
//!   instance by id, so a trigger detached by an earlier run is skipped and the run state
//!   tested is always current. The walks that carry on after a run
//!   (`greet_mtrigger`, `command_mtrigger`, `timer_otrigger`, `cmd_otrig`,
//!   `leave_otrigger`) take the successor of the trigger just run from the
//!   live list, as `t->next` does, so a trigger an earlier run attached is
//!   reached (`greet_mtrigger` reads it before the run, as the C's `next_trig`
//!   does); the snapshot (refreshed before each trigger runs) serves only
//!   when the current trigger has left the list. Such a walk ends after `TRIGGER_WALK_LIMIT` steps. The memory lists
//!   the C walks with a saved `next` pointer are snapshots.
//! - The room walks of `greet_mtrigger`, `greet_memory_mtrigger`,
//!   `entry_memory_mtrigger` (the C reads `ch->next_in_room` after the run)
//!   and of `command_mtrigger`, `speech_mtrigger` (`ch_next` saved before
//!   the run) and `leave_otrigger` (`obj_next`) go through the live lists,
//!   so a mob or object a script moved carries the walk into its new list,
//!   as the C pointer does. Each such walk ends after `LIVE_WALK_LIMIT`
//!   steps, with one `script_log` line: two mobs that keep putting
//!   themselves back at the head of a list would loop for ever in the C.
//! - "The owner was purged" (`!obj` after `script_driver` NULLs `go_adress`)
//!   is `!host.exists(obj)` or [`crate::types::VmState::run_purged`] after
//!   the run, which the driver sets only when it left through its purge path
//!   (`dg_owner_purged` tested after a command sent to the command
//!   interpreter), the one place the C NULLs the pointer. That includes the
//!   flag a nested script left set by purging *its own* owner under such a
//!   command (an `oload mob` whose load trigger purges the new mob): the C
//!   then reports the live object as purged and its dispatcher refuses the
//!   action. A flag left under a `dg_cast` line is not tested, so the run
//!   ends normally. `run_purged` is cleared on every driver entry, so it is
//!   read only straight after a run.
//!   `has_obj_by_uid_in_lookup_table` is `!host.exists(obj)`, and a purged
//!   character is `DEAD(ch)`, [`crate::host::WorldHost::is_dead`].
//! - Where the C would read out of bounds, the nearest safe thing happens:
//!   an actor or owner with no room is skipped where the C indexes
//!   `world[NOWHERE]`; a door sub-command outside `cmd_door[]` is the empty
//!   string; `act_mtrigger` on text with no `\r` keeps all of it (the C
//!   dereferences the NULL from `strchr`).
//! - `word_check` copies the word list into a `MAX_INPUT_LENGTH` buffer; a
//!   longer list is cut at one byte less, as `strcpy` would overrun it.
//! - `is_substring` and `word_check` return `bool`, not `int`: they are
//!   predicates and nothing outside this module calls them.
//! - `wear_otrigger`'s `where` argument is unused in the C (it sets no
//!   variable) and is kept only for the signature.
//! - `act_mtrigger`'s `arg` argument is unused in the C and is kept only for
//!   the signature.

use crate::cstr::{is_space, matching_quote, skip_spaces, starts_with_ci, str_str};
use crate::driver::{Mode, script_driver, script_driver_default};
use crate::fields_char::CharStat;
use crate::host::DgHost;
use crate::lookup::NUM_WEARS;
use crate::magic::TOP_SPELL_DEFINE;
use crate::types::trig::{
    MTRIG_ACT, MTRIG_BRIBE, MTRIG_CAST, MTRIG_COMMAND, MTRIG_DAMAGE, MTRIG_DEATH, MTRIG_DOOR,
    MTRIG_ENTRY, MTRIG_FIGHT, MTRIG_GREET, MTRIG_GREET_ALL, MTRIG_HITPRCNT, MTRIG_LEAVE,
    MTRIG_LOAD, MTRIG_MEMORY, MTRIG_RANDOM, MTRIG_RECEIVE, MTRIG_SPEECH, MTRIG_TIME, OTRIG_CAST,
    OTRIG_COMMAND, OTRIG_CONSUME, OTRIG_DROP, OTRIG_GET, OTRIG_GIVE, OTRIG_LEAVE, OTRIG_LOAD,
    OTRIG_RANDOM, OTRIG_REMOVE, OTRIG_TIME, OTRIG_TIMER, OTRIG_WEAR, WTRIG_CAST, WTRIG_COMMAND,
    WTRIG_DOOR, WTRIG_DROP, WTRIG_ENTER, WTRIG_GLOBAL, WTRIG_LEAVE, WTRIG_LOGIN, WTRIG_RANDOM,
    WTRIG_RESET, WTRIG_SPEECH, WTRIG_TIME,
};
use crate::types::{
    DIRS, LVL_IMMORT, MAX_INPUT_LENGTH, MUDLOG_NRM, NUM_OF_DIRS, OCMD_DRINK, OCMD_EAT, OCMD_EQUIP,
    OCMD_INVEN, OCMD_QUAFF, OCMD_ROOM, POS_SLEEPING, PRF_NOHASSLE, REV_DIR, SCRIPT_ERROR_CODE, Uid,
    uid_text,
};

/// What dispatch needs from the world beyond the shared primitives. Position,
/// fighting, hit points, charm, visibility, the people / contents /
/// equipment / inventory lists, room zones, the playing characters, spell
/// names, memory lists and the room table are all read through [`crate::host::WorldHost`],
/// [`crate::fields_char::CharHost`] and [`crate::commands::CmdHost`].
pub trait DispatchHost {
    /// `free_proto_script(&mob_proto[GET_MOB_RNUM(ch)], MOB_TRIGGER)` /
    /// `free_proto_script(&obj_proto[GET_OBJ_RNUM(obj)], OBJ_TRIGGER)`: drop
    /// the script from the prototype of this mob or object, so nothing
    /// loaded from it afterwards has triggers. Does nothing for an entity
    /// with no prototype (`GET_MOB_RNUM(ch) == NOBODY`, a player). Called
    /// when a `load` trigger recursed past [`crate::types::MAX_SCRIPT_DEPTH`].
    fn disp_free_proto_script(&mut self, uid: Uid);
}

// ---------------------------------------------------------------------------
// Helpers shared by the trigger functions.
// ---------------------------------------------------------------------------

/// `cmd_door[]` (`act.movement.c`), indexed by `SCMD_OPEN`..`SCMD_PICK`.
const CMD_DOOR: [&str; 5] = ["open", "close", "unlock", "lock", "pick"];

/// `skill_name(num)` (`spell_parser.c`).
fn skill_name<H: DgHost + ?Sized>(host: &H, num: i32) -> String {
    if num > 0 && num <= TOP_SPELL_DEFINE {
        host.magic_spell_name(num)
            .unwrap_or_else(|| "!UNUSED!".to_owned())
    } else if num == -1 {
        "UNUSED".to_owned()
    } else {
        "UNDEFINED".to_owned()
    }
}

/// `is_empty(world[room].zone)` (`db.c`): no `CON_PLAYING` character stands in
/// a room of the zone, not counting an immortal with `PRF_NOHASSLE`
/// (`!IS_NPC && GET_LEVEL >= LVL_IMMORT && PRF_FLAGGED(PRF_NOHASSLE)`).
/// Gates the random and time triggers of anything without the `GLOBAL` bit.
fn zone_empty<H: DgHost + ?Sized>(host: &H, room: Uid) -> bool {
    let zone = host.room_zone_number(room);
    !host.cmd_playing_characters().into_iter().any(|ch| {
        let Some(here) = room_of(host, ch) else {
            return false;
        };
        host.room_zone_number(here) == zone
            && !(!host.is_npc(ch)
                && host.level(ch) >= LVL_IMMORT
                && host.char_pref_flagged(ch, PRF_NOHASSLE))
    })
}

/// `SCRIPT_CHECK(go, type)`.
fn script_check<H: DgHost + ?Sized>(host: &H, go: Uid, type_bit: u32) -> bool {
    host.script(go).is_some_and(|script| script.check(type_bit))
}

/// `AWAKE(ch)`.
fn awake<H: DgHost + ?Sized>(host: &H, ch: Uid) -> bool {
    host.char_stat(ch, CharStat::Pos) > POS_SLEEPING
}

/// `FIGHTING(ch) != NULL`.
fn fighting<H: DgHost + ?Sized>(host: &H, ch: Uid) -> bool {
    host.char_fighting(ch).is_some()
}

/// `AFF_FLAGGED(ch, AFF_CHARM)`.
fn charmed<H: DgHost + ?Sized>(host: &H, ch: Uid) -> bool {
    host.cmd_is_charmed(ch)
}

/// `rand_number(1, 100) <= narg`.
fn roll<H: DgHost + ?Sized>(host: &mut H, narg: i32) -> bool {
    host.rand_number(1, 100) <= i64::from(narg)
}

/// A snapshot of the owner's trigger ids, in list order.
fn trigger_ids<H: DgHost + ?Sized>(host: &H, go: Uid) -> Vec<u64> {
    host.script(go)
        .map(crate::types::Script::trigger_ids)
        .unwrap_or_default()
}

/// `TRIGGER_CHECK(t, type)`: the instance's `GET_TRIG_NARG` when it has the
/// type bit and is neither running nor waiting, else `None`.
fn checked<H: DgHost + ?Sized>(host: &H, go: Uid, id: u64, type_bit: u32) -> Option<i32> {
    host.script(go)
        .and_then(|script| script.trigger(id))
        .filter(|trig| trig.check(type_bit))
        .map(|trig| trig.proto.narg)
}

/// `GET_TRIG_NARG(t)`, or `0` for an instance that is gone.
fn trig_narg<H: DgHost + ?Sized>(host: &H, go: Uid, id: u64) -> i32 {
    host.script(go)
        .and_then(|script| script.trigger(id))
        .map_or(0, |trig| trig.proto.narg)
}

/// `GET_TRIG_ARG(t)`, empty for an instance that is gone.
fn trig_arg<H: DgHost + ?Sized>(host: &H, go: Uid, id: u64) -> String {
    host.script(go)
        .and_then(|script| script.trigger(id))
        .map(|trig| trig.proto.arglist.clone())
        .unwrap_or_default()
}

/// `GET_TRIG_VNUM(t)`.
fn trig_vnum<H: DgHost + ?Sized>(host: &H, go: Uid, id: u64) -> i32 {
    host.script(go)
        .and_then(|script| script.trigger(id))
        .map_or(0, |trig| trig.proto.vnum)
}

/// `IS_SET(GET_TRIG_TYPE(t), type)` and `!GET_TRIG_DEPTH(t)` for the
/// triggers the C tests without `TRIGGER_CHECK`: `(has the type bit, is not
/// running or waiting, narg)`.
fn trig_state<H: DgHost + ?Sized>(host: &H, go: Uid, id: u64, type_bit: u32) -> (bool, bool, i32) {
    host.script(go)
        .and_then(|script| script.trigger(id))
        .map_or((false, false, 0), |trig| {
            (
                trig.proto.trigger_type & type_bit != 0,
                !trig.is_busy(),
                trig.proto.narg,
            )
        })
}

/// `add_var(&GET_TRIG_VARS(t), name, value, 0)`.
fn set_var<H: DgHost + ?Sized>(host: &mut H, go: Uid, id: u64, name: &str, value: &str) {
    if let Some(trig) = host
        .script_mut(go)
        .and_then(|script| script.trigger_mut(id))
    {
        trig.vars.add_var(name, value, 0);
    }
}

/// `ADD_UID_VAR(buf, t, id, name, 0)`: the entity's id as `}<id>` text.
fn set_uid_var<H: DgHost + ?Sized>(host: &mut H, go: Uid, id: u64, name: &str, uid: Uid) {
    set_var(host, go, id, name, &uid_text(uid));
}

/// `script_driver(&go, t, type, TRIG_NEW)`.
fn run<H: DgHost + ?Sized>(host: &mut H, go: Uid, id: u64) -> i32 {
    script_driver(host, go, id, Mode::New)
}

/// The C's `!obj` after `script_driver(&obj, ...)`: the object no longer
/// exists, or the run left through its purge path (`dg_owner_purged` tested
/// after a command, which NULLs `go_adress`). Meant for the point straight
/// after a run.
fn object_gone<H: DgHost + ?Sized>(host: &mut H, obj: Uid) -> bool {
    !host.exists(obj) || host.vm_state().run_purged
}

/// Bound on the steps of a trigger-list walk (no real list is near it).
const TRIGGER_WALK_LIMIT: usize = 1_000;

/// A walk over an owner's trigger list as the C's `t = t->next` makes it: the
/// successor of the trigger just run is read from the list as it is now, so a
/// trigger an earlier run attached is reached, and one detached is not. The
/// snapshot (the list as the trigger about to run found it) serves only when
/// that trigger has left the list itself: the walk then goes to the first
/// later trigger of the snapshot that is still attached. Ends after [`TRIGGER_WALK_LIMIT`] steps (a script that
/// attaches a trigger each time it runs would never end in the C).
struct TriggerWalk {
    snapshot: Vec<u64>,
    started: bool,
    prev: Option<u64>,
    locked: Option<Option<u64>>,
    steps: usize,
}

impl TriggerWalk {
    fn new<H: DgHost + ?Sized>(host: &H, go: Uid) -> Self {
        Self {
            snapshot: trigger_ids(host, go),
            started: false,
            prev: None,
            locked: None,
            steps: 0,
        }
    }

    /// The trigger after the one last returned, in the live list.
    fn following<H: DgHost + ?Sized>(&self, host: &H, go: Uid) -> Option<u64> {
        let prev = self.prev?;
        let live = trigger_ids(host, go);
        if let Some(index) = live.iter().position(|id| *id == prev) {
            return live.get(index + 1).copied();
        }
        let index = self.snapshot.iter().position(|id| *id == prev)?;
        self.snapshot
            .iter()
            .skip(index + 1)
            .copied()
            .find(|id| live.contains(id))
    }

    /// `next_trig = t->next`, read before the current trigger runs; the next
    /// call to [`TriggerWalk::next`] goes there.
    fn lock_successor<H: DgHost + ?Sized>(&mut self, host: &H, go: Uid) {
        self.locked = Some(self.following(host, go));
    }

    /// The next trigger id to examine, `None` at the end of the list.
    fn next<H: DgHost + ?Sized>(&mut self, host: &mut H, go: Uid, what: &str) -> Option<u64> {
        let id = if self.started {
            match self.locked.take() {
                Some(locked) => locked,
                None => self.following(host, go),
            }
        } else {
            self.started = true;
            self.snapshot.first().copied()
        };
        self.prev = id;
        let id = id?;
        // The list as the trigger about to run found it, for the successor
        // lookup should that trigger detach itself.
        self.snapshot = trigger_ids(host, go);
        self.steps += 1;
        if self.steps > TRIGGER_WALK_LIMIT {
            host.script_log(&format!(
                "{what} walk stopped after {TRIGGER_WALK_LIMIT} triggers (a script keeps attaching triggers)."
            ));
            return None;
        }
        Some(id)
    }
}

/// The first trigger with this type bit that is not running and wins its
/// percent roll (`TRIGGER_CHECK(t, type) && rand_number(1, 100) <=
/// GET_TRIG_NARG(t)`). The roll is made only for a trigger that passes the
/// check, in list order.
fn find_rolled<H: DgHost + ?Sized>(host: &mut H, go: Uid, type_bit: u32) -> Option<u64> {
    for id in trigger_ids(host, go) {
        if let Some(narg) = checked(host, go, id, type_bit)
            && roll(host, narg)
        {
            return Some(id);
        }
    }
    None
}

/// The first trigger with this type bit that is not running and whose narg
/// satisfies `accept` (the bribe and time tests, which roll no dice).
fn find_narg<H: DgHost + ?Sized>(
    host: &H,
    go: Uid,
    type_bit: u32,
    accept: impl Fn(i32) -> bool,
) -> Option<u64> {
    trigger_ids(host, go)
        .into_iter()
        .find(|id| checked(host, go, *id, type_bit).is_some_and(&accept))
}

/// The text of a direction for the `direction` variable: `dirs[dir]`, or its
/// opposite when `reversed`, for `dir` in `0..DIR_COUNT`; otherwise `none`.
fn direction_text<H: DgHost + ?Sized>(host: &H, dir: i32, reversed: bool) -> &'static str {
    match usize::try_from(dir) {
        Ok(dir) if dir < if host.subst_diagonal_dirs() { 10 } else { 6 } && dir < NUM_OF_DIRS => {
            DIRS[if reversed { REV_DIR[dir] } else { dir }]
        }
        _ => "none",
    }
}

/// `cmd_door[subcmd]`.
fn door_text(subcmd: i32) -> &'static str {
    usize::try_from(subcmd)
        .ok()
        .and_then(|index| CMD_DOOR.get(index))
        .copied()
        .unwrap_or("")
}

/// The test the command triggers share: the trigger argument is `*` first, or
/// the typed command begins with it (`!strn_cmp(arg, cmd, strlen(arg))`).
fn command_matches(trig_arg: &str, cmd: &str) -> bool {
    trig_arg.starts_with('*') || starts_with_ci(cmd, trig_arg)
}

/// `mudlog(NRM, LVL_BUILDER, TRUE, "SYSERR: <what> Trigger #%d has no text
/// argument!", vnum)`.
fn log_no_text<H: DgHost + ?Sized>(host: &mut H, what: &str, vnum: i32) {
    host.cmd_mudlog(
        MUDLOG_NRM,
        LVL_IMMORT,
        &format!("SYSERR: {what} Trigger #{vnum} has no text argument!"),
    );
}

/// `(GET_TRIG_NARG(t) && word_check(str, arg)) || (!GET_TRIG_NARG(t) &&
/// is_substring(arg, str))`: the speech and act text test.
fn text_matches(narg: i32, arg: &str, text: &str) -> bool {
    (narg != 0 && word_check(text, arg)) || (narg == 0 && is_substring(arg, text))
}

/// The room a character stands in, if any (`IN_ROOM(ch) != NOWHERE`).
fn room_of<H: DgHost + ?Sized>(host: &H, ch: Uid) -> Option<Uid> {
    host.in_room(ch).filter(|room| host.exists(*room))
}

/// `ch->next_in_room` read from the live list, after whatever the last run
/// did to it: the entry after `ch` in the people of the room `ch` is in now
/// (a character a script moved continues in its new room's list, as the C's
/// pointer does). `None` at the end of the list, or for a character that is
/// gone or in no room, where the C would follow a dangling pointer.
fn next_in_room<H: DgHost + ?Sized>(host: &H, ch: Uid) -> Option<Uid> {
    let room = room_of(host, ch)?;
    let people = host.room_people(room);
    let index = people.iter().position(|other| *other == ch)?;
    people.get(index + 1).copied()
}

/// Bound on the steps of a live-list walk: a script that keeps moving the
/// walked character into a list it has not yet left would loop for ever in
/// the C; here the walk ends after this many characters.
const LIVE_WALK_LIMIT: usize = 100_000;

/// Counts one step of a live-list walk; `true` (after one `script_log` line)
/// when the walk has gone past [`LIVE_WALK_LIMIT`] and must end.
fn walk_exhausted<H: DgHost + ?Sized>(host: &mut H, steps: usize, what: &str, unit: &str) -> bool {
    if steps <= LIVE_WALK_LIMIT {
        return false;
    }
    host.script_log(&format!(
        "{what} walk stopped after {LIVE_WALK_LIMIT} {unit} (a script keeps changing the list being walked)."
    ));
    true
}

/// `obj->next_content` read from the live list the object is in now: the
/// entry after `obj` in the room contents, the carried items, or the
/// contents of its container. `None` at the end, for a worn object (the C
/// leaves `next_content` NULL there) and for an object that is gone.
fn next_content<H: DgHost + ?Sized>(host: &H, obj: Uid) -> Option<Uid> {
    if !host.exists(obj) {
        return None;
    }
    let list = if let Some(room) = host.in_room(obj) {
        host.room_contents(room)
    } else if let Some(ch) = host.obj_carried_by(obj) {
        host.carrying(ch)
    } else {
        let container = host.obj_in_obj(obj)?;
        host.obj_contains(container)
    };
    let index = list.iter().position(|other| *other == obj)?;
    list.get(index + 1).copied()
}

/// Remove one memory entry from a mob's `SCRIPT_MEM` list (the unlink loop
/// of `greet_memory_mtrigger` / `entry_memory_mtrigger`). Does nothing when
/// the entry is no longer in the list.
fn forget<H: DgHost + ?Sized>(host: &mut H, ch: Uid, mem: &crate::types::ScriptMemory) {
    let mut list = host.script_mem(ch);
    if let Some(index) = list.iter().position(|entry| entry == mem) {
        list.remove(index);
        host.set_script_mem(ch, list);
    }
}

// ---------------------------------------------------------------------------
// General functions used by several triggers.
// ---------------------------------------------------------------------------

/// `one_phrase(arg, first_arg)`: the first word of `arg`, or the text inside
/// the first pair of double quotes, and the rest of the string after it.
///
/// Leading whitespace is skipped with the interpreter's `skip_spaces`, which
/// stops at a tab: a word list that begins with a tab yields an empty phrase
/// and ends [`word_check`]. An unterminated quote yields the text up to but
/// not including the last byte (`matching_quote` points at that byte).
pub fn one_phrase(arg: &str) -> (String, &str) {
    let arg = skip_spaces(arg);
    if arg.is_empty() {
        return (String::new(), arg);
    }
    if arg.starts_with('"') {
        let p = matching_quote(arg, 0);
        // `p` can be mid-character when the quote is unterminated and the
        // text ends in a multi-byte character; the lossy conversion keeps
        // the cut safe.
        let phrase = if p > 1 {
            String::from_utf8_lossy(&arg.as_bytes()[1..p]).into_owned()
        } else {
            String::new()
        };
        return (phrase, arg.get(p + 1..).unwrap_or(""));
    }
    let end = arg
        .find(|c: char| is_space(c) || c == '"')
        .unwrap_or(arg.len());
    (arg[..end].to_owned(), &arg[end..])
}

/// `is_substring(sub, string)`: `sub` occurs in `string` (the first
/// occurrence `str_str` finds, ASCII case-insensitive) as a whole word: at
/// the start or after whitespace or punctuation, and at the end or before
/// whitespace or punctuation. Only the first occurrence is tried.
pub fn is_substring(sub: &str, string: &str) -> bool {
    let Some(start) = str_str(string, sub) else {
        return false;
    };
    let bytes = string.as_bytes();
    let boundary = |byte: u8| is_space(char::from(byte)) || byte.is_ascii_punctuation();
    let front = start == 0 || bytes.get(start - 1).is_some_and(|byte| boundary(*byte));
    let end = start + sub.len();
    let back = end == bytes.len() || bytes.get(end).is_some_and(|byte| boundary(*byte));
    front && back
}

/// `word_check(str, wordlist)`: `str` contains a word or phrase from
/// `wordlist`. Phrases are in double quotes. A list that begins with `*`
/// matches everything.
pub fn word_check(str: &str, wordlist: &str) -> bool {
    if wordlist.starts_with('*') {
        return true;
    }
    // `strcpy(words, wordlist)` into a MAX_INPUT_LENGTH buffer.
    let mut end = wordlist.len().min(MAX_INPUT_LENGTH - 1);
    while !wordlist.is_char_boundary(end) {
        end -= 1;
    }
    let (mut phrase, mut rest) = one_phrase(&wordlist[..end]);
    while !phrase.is_empty() {
        if is_substring(&phrase, str) {
            return true;
        }
        (phrase, rest) = one_phrase(rest);
    }
    false
}

// ---------------------------------------------------------------------------
// Mob triggers.
// ---------------------------------------------------------------------------

/// `random_mtrigger(ch)`: fires the first `random` trigger that wins its
/// roll. Called by [`script_trigger_check`] every `PULSE_DG_SCRIPT` pulses
/// for each character with a script that is in an occupied zone or carries
/// the `GLOBAL` bit. The result is ignored.
pub fn random_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid) {
    if !script_check(host, ch, MTRIG_RANDOM) || charmed(host, ch) {
        return;
    }
    if let Some(id) = find_rolled(host, ch, MTRIG_RANDOM) {
        run(host, ch, id);
    }
}

/// `bribe_mtrigger(ch, actor, amount)`: fires the first `bribe` trigger
/// whose narg is at most `amount` (no roll), with `amount` and `actor` set.
///
/// Called from `do_give` (`act.item.c`, `perform_give_gold`) after the coins
/// have already moved to the receiving mob; the result is ignored.
pub fn bribe_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid, actor: Uid, amount: i32) {
    if !script_check(host, ch, MTRIG_BRIBE) || charmed(host, ch) {
        return;
    }
    if let Some(id) = find_narg(host, ch, MTRIG_BRIBE, |narg| amount >= narg) {
        set_var(host, ch, id, "amount", &amount.to_string());
        set_uid_var(host, ch, id, "actor", actor);
        run(host, ch, id);
    }
}

/// `greet_memory_mtrigger(actor)`: for each awake, idle, uncharmed mob in the
/// actor's room that remembers the actor, run the remembered command or, if
/// no command was performed so far, the first `memory` trigger (whose roll
/// is made only after its other conditions hold); then forget the entry.
///
/// A remembered *command* is run and the entry **kept** (the `break` comes
/// before the delete), and `command_performed` is shared across the mobs of
/// the room, so after one mob has run a command the later mobs only forget.
///
/// Called right after [`greet_mtrigger`] in `do_simple_move` (`act.movement.c`)
/// when the greet triggers did not stop the move, from `nanny`
/// (`interpreter.c`) when a player enters the game, and from `spell_recall`,
/// `spell_teleport` and `spell_summon` (`spells.c`). The result is ignored.
pub fn greet_memory_mtrigger<H: DgHost + ?Sized>(host: &mut H, actor: Uid) {
    if !host.valid_dg_target(actor, true) {
        return;
    }
    let Some(room) = room_of(host, actor) else {
        return;
    };
    let mut command_performed = false;
    let mut cur = host.room_people(room).first().copied();
    let mut steps = 0;
    while let Some(ch) = cur {
        steps += 1;
        if walk_exhausted(host, steps, "greet_memory_mtrigger", "characters") {
            break;
        }
        cur = greet_memory_visit(host, actor, ch, &mut command_performed)
            .then(|| next_in_room(host, ch))
            .flatten();
    }
}

/// The body of the [`greet_memory_mtrigger`] walk for one mob; `false` when
/// the walk cannot go on (the mob is gone).
fn greet_memory_visit<H: DgHost + ?Sized>(
    host: &mut H,
    actor: Uid,
    ch: Uid,
    command_performed: &mut bool,
) -> bool {
    if !host.exists(ch) {
        return false;
    }
    if host.script_mem(ch).is_empty()
        || !awake(host, ch)
        || fighting(host, ch)
        || ch == actor
        || charmed(host, ch)
    {
        return true;
    }
    for mem in host.script_mem(ch) {
        let current = host.script_mem(ch);
        if current.is_empty() {
            break;
        }
        if mem.id != actor || !current.contains(&mem) {
            continue;
        }
        if let Some(cmd) = &mem.cmd {
            host.mud_command(ch, cmd);
            *command_performed = true;
            break;
        }
        if !*command_performed {
            for id in trigger_ids(host, ch) {
                let (has_type, idle, narg) = trig_state(host, ch, id, MTRIG_MEMORY);
                if has_type && host.can_see(ch, actor) && idle && roll(host, narg) {
                    set_uid_var(host, ch, id, "actor", actor);
                    run(host, ch, id);
                    break;
                }
            }
        }
        forget(host, ch, &mem);
    }
    true
}

/// `greet_mtrigger(actor, dir)`: runs **every** `greet` trigger (that the mob
/// can see the actor for, or `greet-all`) of every awake, idle, uncharmed mob
/// in the actor's room, setting `direction` to the opposite of `dir` (`none`
/// outside `0..DIR_COUNT`) and `actor`. Returns `0` if any run returned `0`,
/// else `1`.
///
/// Called from `do_simple_move` (`act.movement.c`) after the actor is in the
/// new room; a `0` return moves the actor back to the room it left and the
/// move fails. Also called with `dir` `-1` from `nanny` (`interpreter.c`)
/// and the teleport spells (`spells.c`), where the result is ignored.
pub fn greet_mtrigger<H: DgHost + ?Sized>(host: &mut H, actor: Uid, dir: i32) -> i32 {
    if !host.valid_dg_target(actor, true) {
        return 1;
    }
    let Some(room) = room_of(host, actor) else {
        return 1;
    };
    let mut final_ret = 1;
    let mut cur = host.room_people(room).first().copied();
    let mut steps = 0;
    while let Some(ch) = cur {
        steps += 1;
        if walk_exhausted(host, steps, "greet_mtrigger", "characters") {
            break;
        }
        if !host.exists(ch) {
            break;
        }
        if script_check(host, ch, MTRIG_GREET | MTRIG_GREET_ALL)
            && awake(host, ch)
            && !fighting(host, ch)
            && ch != actor
            && !charmed(host, ch)
        {
            let mut walk = TriggerWalk::new(host, ch);
            while let Some(id) = walk.next(host, ch, "greet_mtrigger") {
                // `next_trig = t->next`, read before the trigger runs.
                walk.lock_successor(host, ch);
                let (greet, idle, narg) = trig_state(host, ch, id, MTRIG_GREET);
                let (greet_all, _, _) = trig_state(host, ch, id, MTRIG_GREET_ALL);
                if ((greet && host.can_see(ch, actor)) || greet_all) && idle && roll(host, narg) {
                    let direction = direction_text(host, dir, true);
                    set_var(host, ch, id, "direction", direction);
                    set_uid_var(host, ch, id, "actor", actor);
                    if run(host, ch, id) == 0 {
                        final_ret = 0;
                    }
                }
            }
        }
        cur = next_in_room(host, ch);
    }
    final_ret
}

/// `entry_memory_mtrigger(ch)`: for each other character in the mob's room
/// that the mob remembers, run the remembered command or the first `memory`
/// trigger that wins its roll, then forget the entry (commands included).
///
/// Called from `do_simple_move` (`act.movement.c`) right after the mob
/// itself has entered a room, and from the teleport spells (`spells.c`). The
/// result is ignored.
pub fn entry_memory_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid) {
    if host.script_mem(ch).is_empty() || charmed(host, ch) {
        return;
    }
    let Some(room) = room_of(host, ch) else {
        return;
    };
    let mut cur = host.room_people(room).first().copied();
    let mut steps = 0;
    while let Some(actor) = cur {
        steps += 1;
        if walk_exhausted(host, steps, "entry_memory_mtrigger", "characters")
            || host.script_mem(ch).is_empty()
            || !host.exists(actor)
        {
            break;
        }
        if actor != ch {
            for mem in host.script_mem(ch) {
                let current = host.script_mem(ch);
                if current.is_empty() {
                    break;
                }
                if mem.id != actor || !current.contains(&mem) {
                    continue;
                }
                if let Some(cmd) = &mem.cmd {
                    host.mud_command(ch, cmd);
                } else if let Some(id) = find_rolled(host, ch, MTRIG_MEMORY) {
                    set_uid_var(host, ch, id, "actor", actor);
                    run(host, ch, id);
                }
                forget(host, ch, &mem);
            }
        }
        cur = next_in_room(host, actor);
    }
}

/// `entry_mtrigger(ch)`: runs the first `entry` trigger that wins its roll
/// and returns its result; `1` if none ran.
///
/// Called from `do_simple_move` (`act.movement.c`) after the mob is in the
/// new room, before `enter_wtrigger`; a `0` return puts the mob back in the
/// room it left and the move fails.
pub fn entry_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid) -> i32 {
    if !script_check(host, ch, MTRIG_ENTRY) || charmed(host, ch) {
        return 1;
    }
    match find_rolled(host, ch, MTRIG_ENTRY) {
        Some(id) => run(host, ch, id),
        None => 1,
    }
}

/// `command_mtrigger(actor, cmd, argument)`: offers a typed command to the
/// `command` triggers of the mobs in the actor's room. For each uncharmed mob
/// with such a trigger (the actor itself only when `CONFIG_SCRIPT_PLAYERS`),
/// each idle trigger whose argument is `*` or a prefix of `cmd` gets `actor`,
/// `arg` and `cmd` and is run; the first run that returns non-zero claims the
/// command and ends the walk with `1`. A run that returns `0` lets the walk
/// go on to the next trigger and mob. Returns `0` if none claimed it.
///
/// `cmd` is the first word as `any_one_arg` left it (lowercased), or the
/// single first character when that is not a letter (`'hi` gives `'`); the
/// argument is the rest of the line, not yet stripped of its leading spaces.
///
/// Called from `command_interpreter` (`interpreter.c`) for every command a
/// character types, after `command_wtrigger` and before `command_otrigger`;
/// a non-zero result means the command was handled and is not executed.
pub fn command_mtrigger<H: DgHost + ?Sized>(
    host: &mut H,
    actor: Uid,
    cmd: &str,
    argument: &str,
) -> i32 {
    if !host.valid_dg_target(actor, false) {
        return 0;
    }
    let Some(room) = room_of(host, actor) else {
        return 0;
    };
    // `skip_spaces(&argument)` / `skip_spaces(&cmd)` advance the caller's
    // pointers for good, so later triggers see the skipped text.
    let (mut cmd, mut argument) = (cmd, argument);
    let mut cur = host.room_people(room).first().copied();
    let mut steps = 0;
    while let Some(ch) = cur {
        steps += 1;
        if walk_exhausted(host, steps, "command_mtrigger", "characters") || !host.exists(ch) {
            break;
        }
        // `ch_next = ch->next_in_room`, read before the mob's triggers run.
        let saved = next_in_room(host, ch);
        cur = saved;
        if !script_check(host, ch, MTRIG_COMMAND)
            || charmed(host, ch)
            || !(actor != ch || host.script_players())
        {
            continue;
        }
        let mut walk = TriggerWalk::new(host, ch);
        while let Some(id) = walk.next(host, ch, "command_mtrigger") {
            if checked(host, ch, id, MTRIG_COMMAND).is_none() {
                continue;
            }
            let arg = trig_arg(host, ch, id);
            if arg.is_empty() {
                let vnum = trig_vnum(host, ch, id);
                log_no_text(host, "Command", vnum);
                continue;
            }
            if command_matches(&arg, cmd) {
                set_uid_var(host, ch, id, "actor", actor);
                argument = skip_spaces(argument);
                set_var(host, ch, id, "arg", argument);
                cmd = skip_spaces(cmd);
                set_var(host, ch, id, "cmd", cmd);
                if run(host, ch, id) != 0 {
                    return 1;
                }
            }
        }
    }
    0
}

/// `speech_mtrigger(actor, str)`: for each awake, uncharmed mob in the
/// actor's room with a `speech` trigger (the actor itself only when
/// `CONFIG_SCRIPT_PLAYERS`), runs the first idle trigger whose argument
/// matches the spoken text (`word_check` when narg is non-zero,
/// `is_substring` when it is zero), with `actor` and `speech` set.
///
/// Called from `do_say` (`act.comm.c`) last, after the speaker's own message,
/// and unconditionally: also for a bare `say`, with the `skip_spaces`'d
/// argument empty, so a trigger with a non-zero narg and an argument
/// starting with `*` still fires (with narg `0` the `*` is looked for as a
/// word, and an empty line has none). Followed by `speech_wtrigger`. The
/// result is ignored.
pub fn speech_mtrigger<H: DgHost + ?Sized>(host: &mut H, actor: Uid, str: &str) {
    let Some(room) = room_of(host, actor) else {
        return;
    };
    let mut cur = host.room_people(room).first().copied();
    let mut steps = 0;
    while let Some(ch) = cur {
        steps += 1;
        if walk_exhausted(host, steps, "speech_mtrigger", "characters") || !host.exists(ch) {
            break;
        }
        // `ch_next = ch->next_in_room`, read before the mob's triggers run.
        let saved = next_in_room(host, ch);
        cur = saved;
        if !script_check(host, ch, MTRIG_SPEECH)
            || !awake(host, ch)
            || charmed(host, ch)
            || !(actor != ch || host.script_players())
        {
            continue;
        }
        for id in trigger_ids(host, ch) {
            if checked(host, ch, id, MTRIG_SPEECH).is_none() {
                continue;
            }
            let arg = trig_arg(host, ch, id);
            if arg.is_empty() {
                let vnum = trig_vnum(host, ch, id);
                log_no_text(host, "Speech", vnum);
                continue;
            }
            if text_matches(trig_narg(host, ch, id), &arg, str) {
                set_uid_var(host, ch, id, "actor", actor);
                set_var(host, ch, id, "speech", str);
                run(host, ch, id);
                break;
            }
        }
    }
}

/// `act_mtrigger(ch, str, actor, victim, object, target, arg)`: the mob `ch`
/// was shown the line `str` by `act()`. If it has an uncharmed `act` trigger
/// and is not the actor, runs the first idle trigger whose argument matches
/// the line (as for speech), with `actor`, `victim`, `object`, `target` (each
/// only when given) and `arg` set. `arg` is the line without its leading
/// spaces and without everything from its first `\r` on. `arg` (the last
/// parameter) is unused, as in the C.
///
/// Called from `perform_act` (`comm.c`) for every NPC recipient other than
/// the actor while the global `dg_act_check`
/// ([`crate::types::VmState::act_check`]) is set (the last `act()` made,
/// nested ones included, was not `DG_NO_TRIG`), after the text is built, with
/// the line still ending in `\r\n`. The result is ignored.
#[allow(clippy::too_many_arguments)]
pub fn act_mtrigger<H: DgHost + ?Sized>(
    host: &mut H,
    ch: Uid,
    text: &str,
    actor: Option<Uid>,
    victim: Option<Uid>,
    object: Option<Uid>,
    target: Option<Uid>,
    _arg: Option<&str>,
) {
    if !script_check(host, ch, MTRIG_ACT) || charmed(host, ch) || actor == Some(ch) {
        return;
    }
    for id in trigger_ids(host, ch) {
        if checked(host, ch, id, MTRIG_ACT).is_none() {
            continue;
        }
        let arg = trig_arg(host, ch, id);
        if arg.is_empty() {
            let vnum = trig_vnum(host, ch, id);
            log_no_text(host, "Act", vnum);
            continue;
        }
        if text_matches(trig_narg(host, ch, id), &arg, text) {
            if let Some(actor) = actor {
                set_uid_var(host, ch, id, "actor", actor);
            }
            if let Some(victim) = victim {
                set_uid_var(host, ch, id, "victim", victim);
            }
            if let Some(object) = object {
                set_uid_var(host, ch, id, "object", object);
            }
            if let Some(target) = target {
                set_uid_var(host, ch, id, "target", target);
            }
            // `skip_spaces(&nstr)` moves the start; `*p = '\0'` cuts at the
            // first `\r` of the original text, which a skip can pass.
            let start = text.len() - skip_spaces(text).len();
            let end = match text.find('\r') {
                Some(cr) if start <= cr => cr,
                _ => text.len(),
            };
            set_var(host, ch, id, "arg", &text[start..end]);
            run(host, ch, id);
            break;
        }
    }
}

/// `fight_mtrigger(ch)`: a fighting, uncharmed mob runs the first `fight`
/// trigger that wins its roll, with `actor` set to its opponent.
///
/// Called from `hit` (`fight.c`) at the start of each blow, once both
/// combatants are known to exist. The result is ignored.
pub fn fight_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid) {
    if !script_check(host, ch, MTRIG_FIGHT) || !fighting(host, ch) || charmed(host, ch) {
        return;
    }
    if let Some(id) = find_rolled(host, ch, MTRIG_FIGHT) {
        match host.char_fighting(ch) {
            Some(actor) => set_uid_var(host, ch, id, "actor", actor),
            None => set_var(host, ch, id, "actor", "nobody"),
        }
        run(host, ch, id);
    }
}

/// `hitprcnt_mtrigger(ch)`: a fighting, uncharmed mob runs the first
/// `hitprcnt` trigger whose narg is at least its hit-point percentage
/// (`GET_HIT * 100 / GET_MAX_HIT`, integer division, only when the maximum is
/// non-zero), with `actor` set to its opponent. No roll.
///
/// Called from `hit` (`fight.c`) at the end of each blow, for the victim.
/// The result is ignored.
pub fn hitprcnt_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid) {
    if !script_check(host, ch, MTRIG_HITPRCNT) || !fighting(host, ch) || charmed(host, ch) {
        return;
    }
    let max_hit = host.char_stat(ch, CharStat::MaxHit);
    if max_hit == 0 {
        return;
    }
    let percent = host
        .char_stat(ch, CharStat::Hit)
        .wrapping_mul(100)
        .wrapping_div(max_hit);
    if let Some(id) = find_narg(host, ch, MTRIG_HITPRCNT, |narg| percent <= narg) {
        if let Some(actor) = host.char_fighting(ch) {
            set_uid_var(host, ch, id, "actor", actor);
        }
        run(host, ch, id);
    }
}

/// `receive_mtrigger(ch, actor, obj)`: runs the first `receive` trigger that
/// wins its roll, with `actor` and `object` set. Returns `0` if the actor or
/// the mob died, the object was purged, or the object is no longer carried by
/// the actor; otherwise the script's result. `1` if no trigger ran.
///
/// Called from `perform_give` (`act.item.c`) after `give_otrigger`, and from
/// `do_steal` (`act.other.c`) with the thief as `ch` and the victim as
/// `actor`; a `0` return cancels the transfer.
pub fn receive_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid, actor: Uid, obj: Uid) -> i32 {
    if !script_check(host, ch, MTRIG_RECEIVE) || charmed(host, ch) {
        return 1;
    }
    let Some(id) = find_rolled(host, ch, MTRIG_RECEIVE) else {
        return 1;
    };
    set_uid_var(host, ch, id, "actor", actor);
    set_uid_var(host, ch, id, "object", obj);
    let ret_val = run(host, ch, id);
    if host.is_dead(actor)
        || host.is_dead(ch)
        || !host.exists(obj)
        || host.obj_carried_by(obj) != Some(actor)
    {
        0
    } else {
        ret_val
    }
}

/// `death_mtrigger(ch, actor)`: runs the first `death` trigger that wins its
/// roll, with `actor` (the killer, when there is one) set, and returns its
/// result; `1` if none ran.
///
/// Called from `raw_kill` (`fight.c`) after the dying mob's affects are gone
/// and its position is set to standing, only when there is a killer; a
/// non-zero result lets `death_cry` run, a `0` suppresses it.
pub fn death_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid, actor: Option<Uid>) -> i32 {
    if !script_check(host, ch, MTRIG_DEATH) || charmed(host, ch) {
        return 1;
    }
    let Some(id) = find_rolled(host, ch, MTRIG_DEATH) else {
        return 1;
    };
    if let Some(actor) = actor {
        set_uid_var(host, ch, id, "actor", actor);
    }
    run(host, ch, id)
}

/// `load_mtrigger(ch)`: a mob was just loaded. Script `load` commands call
/// this for the mob they create.
///
/// Runs the first `load` trigger that wins its roll. If that run hit the
/// recursion cap ([`SCRIPT_ERROR_CODE`]) the mob's prototype loses its script
/// so the load chain ends here. Also called from `read_mobile` users: zone
/// resets (`db.c`), `do_load` (`act.wizard.c`) and `mag_summons` (`magic.c`).
/// The result is ignored.
pub fn load_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid) {
    if !script_check(host, ch, MTRIG_LOAD) {
        return;
    }
    let mut result = 0;
    if let Some(id) = find_rolled(host, ch, MTRIG_LOAD) {
        result = run(host, ch, id);
    }
    if result == SCRIPT_ERROR_CODE {
        host.disp_free_proto_script(ch);
    }
}

/// `cast_mtrigger(actor, ch, spellnum)`: runs the first `cast` trigger that
/// wins its roll on the spell's target, with `actor`, `spell` (the number)
/// and `spellname` set, and returns its result; `1` if there is no target or
/// no trigger ran.
///
/// Called from `call_magic` (`spell_parser.c`) after `cast_wtrigger` and
/// `cast_otrigger`; a `0` return cancels the spell.
pub fn cast_mtrigger<H: DgHost + ?Sized>(
    host: &mut H,
    actor: Uid,
    ch: Option<Uid>,
    spellnum: i32,
) -> i32 {
    let Some(ch) = ch else {
        return 1;
    };
    if !script_check(host, ch, MTRIG_CAST) || charmed(host, ch) {
        return 1;
    }
    let Some(id) = find_rolled(host, ch, MTRIG_CAST) else {
        return 1;
    };
    set_uid_var(host, ch, id, "actor", actor);
    set_var(host, ch, id, "spell", &spellnum.to_string());
    let name = skill_name(host, spellnum);
    set_var(host, ch, id, "spellname", &name);
    run(host, ch, id)
}

/// `damage_mtrigger(actor, victim, dam, attacktype)`: runs the first `damage`
/// trigger of the victim that wins its roll, with `actor`, `victim`,
/// `damdealt` and `attacktype` (the skill name) set, with `dam` as the
/// default return, and returns what the script returned (so a script can
/// change the damage with `return`). Returns `dam` when there is no victim or
/// no trigger ran.
///
/// Called from `damage` (`fight.c`) after the immortal-damage check; the
/// caller uses the result as the new damage, and `-1` aborts the attack
/// (`damage` returns 0).
pub fn damage_mtrigger<H: DgHost + ?Sized>(
    host: &mut H,
    actor: Uid,
    victim: Option<Uid>,
    dam: i32,
    attacktype: i32,
) -> i32 {
    let Some(victim) = victim else {
        return dam;
    };
    if !script_check(host, victim, MTRIG_DAMAGE) || charmed(host, victim) {
        return dam;
    }
    let Some(id) = find_rolled(host, victim, MTRIG_DAMAGE) else {
        return dam;
    };
    set_uid_var(host, victim, id, "actor", actor);
    set_uid_var(host, victim, id, "victim", victim);
    set_var(host, victim, id, "damdealt", &dam.to_string());
    let name = skill_name(host, attacktype);
    set_var(host, victim, id, "attacktype", &name);
    script_driver_default(host, victim, id, Mode::New, dam)
}

/// `leave_mtrigger(actor, dir)`: the first `leave` trigger (of a mob that is
/// awake, idle, uncharmed and can see the actor, in room order) that wins its
/// roll runs with `direction` (`dirs[dir]`, `none` outside `0..DIR_COUNT`) and
/// `actor` set, and its result is returned at once; `1` if none ran.
///
/// Called from `do_simple_move` (`act.movement.c`) before the actor leaves; a
/// `0` return, or a script that moved the actor, cancels the move.
pub fn leave_mtrigger<H: DgHost + ?Sized>(host: &mut H, actor: Uid, dir: i32) -> i32 {
    if !host.valid_dg_target(actor, true) {
        return 1;
    }
    let Some(room) = room_of(host, actor) else {
        return 1;
    };
    for ch in host.room_people(room) {
        if !host.exists(ch)
            || !script_check(host, ch, MTRIG_LEAVE)
            || !awake(host, ch)
            || fighting(host, ch)
            || ch == actor
            || charmed(host, ch)
        {
            continue;
        }
        for id in trigger_ids(host, ch) {
            let (leave, idle, narg) = trig_state(host, ch, id, MTRIG_LEAVE);
            if (leave && host.can_see(ch, actor)) && idle && roll(host, narg) {
                let direction = direction_text(host, dir, false);
                set_var(host, ch, id, "direction", direction);
                set_uid_var(host, ch, id, "actor", actor);
                return run(host, ch, id);
            }
        }
    }
    1
}

/// `door_mtrigger(actor, subcmd, dir)`: as [`leave_mtrigger`] for the `door`
/// trigger (no `valid_dg_target` test), setting `cmd` (`open`, `close`,
/// `unlock`, `lock` or `pick`), `direction` and `actor`.
///
/// Called from `do_doorcmd` (`act.movement.c`) before the door changes; a `0`
/// return stops the command.
pub fn door_mtrigger<H: DgHost + ?Sized>(host: &mut H, actor: Uid, subcmd: i32, dir: i32) -> i32 {
    let Some(room) = room_of(host, actor) else {
        return 1;
    };
    for ch in host.room_people(room) {
        if !host.exists(ch)
            || !script_check(host, ch, MTRIG_DOOR)
            || !awake(host, ch)
            || fighting(host, ch)
            || ch == actor
            || charmed(host, ch)
        {
            continue;
        }
        for id in trigger_ids(host, ch) {
            let (door, idle, narg) = trig_state(host, ch, id, MTRIG_DOOR);
            if door && host.can_see(ch, actor) && idle && roll(host, narg) {
                set_var(host, ch, id, "cmd", door_text(subcmd));
                let direction = direction_text(host, dir, false);
                set_var(host, ch, id, "direction", direction);
                set_uid_var(host, ch, id, "actor", actor);
                return run(host, ch, id);
            }
        }
    }
    1
}

/// `time_mtrigger(ch)`: runs the first idle `time` trigger whose narg equals
/// the current mud hour, with `time` set to the hour.
///
/// Called by [`check_time_triggers`] at the start of each mud hour for
/// characters in an occupied zone or with the `GLOBAL` bit. The result is
/// ignored.
pub fn time_mtrigger<H: DgHost + ?Sized>(host: &mut H, ch: Uid) {
    if !script_check(host, ch, MTRIG_TIME) || charmed(host, ch) {
        return;
    }
    let hours = host.mud_time().hours;
    if let Some(id) = find_narg(host, ch, MTRIG_TIME, |narg| hours == narg) {
        set_var(host, ch, id, "time", &hours.to_string());
        run(host, ch, id);
    }
}

// ---------------------------------------------------------------------------
// Object triggers.
// ---------------------------------------------------------------------------

/// `random_otrigger(obj)`: runs the first `random` trigger that wins its roll.
/// Called by [`script_trigger_check`] for every object with a script.
pub fn random_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid) {
    if !script_check(host, obj, OTRIG_RANDOM) {
        return;
    }
    if let Some(id) = find_rolled(host, obj, OTRIG_RANDOM) {
        run(host, obj, id);
    }
}

/// `timer_otrigger(obj)`: runs **every** idle `timer` trigger on the object
/// (no roll), stopping if a script purged the object.
///
/// Called from `point_update` (`limits.c`) when an object's timer counts down
/// to zero (`else if (GET_OBJ_TIMER(j) > 0) { GET_OBJ_TIMER(j)--; if
/// (!GET_OBJ_TIMER(j)) timer_otrigger(j); }`). The result is ignored and
/// nothing follows the call: the object stays unless a script purged it (the
/// decay of corpses is a separate branch of `point_update`).
pub fn timer_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid) {
    if !script_check(host, obj, OTRIG_TIMER) {
        return;
    }
    let mut walk = TriggerWalk::new(host, obj);
    while let Some(id) = walk.next(host, obj, "timer_otrigger") {
        if checked(host, obj, id, OTRIG_TIMER).is_some() {
            run(host, obj, id);
            if object_gone(host, obj) {
                return;
            }
        }
    }
}

/// `get_otrigger(obj, actor)`: runs the first `get` trigger that wins its
/// roll, with `actor` set. Returns `0` if the actor died or the object was
/// purged, else the script's result; `1` if none ran.
///
/// Called from `perform_get_from_room` and `perform_get_from_container`
/// (`act.item.c`) before the object moves; a `0` return leaves it where it
/// is.
pub fn get_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid, actor: Uid) -> i32 {
    if !script_check(host, obj, OTRIG_GET) {
        return 1;
    }
    let Some(id) = find_rolled(host, obj, OTRIG_GET) else {
        return 1;
    };
    set_uid_var(host, obj, id, "actor", actor);
    let ret_val = run(host, obj, id);
    if host.is_dead(actor) || object_gone(host, obj) {
        0
    } else {
        ret_val
    }
}

/// `cmd_otrig(obj, actor, cmd, argument, type)`: offers a command to one
/// object's `command` triggers. Each idle trigger whose narg has the `type`
/// bit (`OCMD_EQUIP`, `OCMD_INVEN` or `OCMD_ROOM`) and whose argument is `*`
/// or a prefix of `cmd` gets `actor`, `arg` and `cmd` and is run; a non-zero
/// result returns `1` at once. A zero result carries on to the object's
/// other triggers, unless the script purged the object (then `0`).
///
/// Called by [`command_otrigger`] for each object in reach.
pub fn cmd_otrig<H: DgHost + ?Sized>(
    host: &mut H,
    obj: Uid,
    actor: Uid,
    cmd: &str,
    argument: &str,
    type_: i32,
) -> i32 {
    if !script_check(host, obj, OTRIG_COMMAND) {
        return 0;
    }
    let (mut cmd, mut argument) = (cmd, argument);
    let mut walk = TriggerWalk::new(host, obj);
    while let Some(id) = walk.next(host, obj, "cmd_otrig") {
        if checked(host, obj, id, OTRIG_COMMAND).is_none() {
            continue;
        }
        let narg = trig_narg(host, obj, id);
        let arg = trig_arg(host, obj, id);
        if narg & type_ != 0 && arg.is_empty() {
            let vnum = trig_vnum(host, obj, id);
            log_no_text(host, "O-Command", vnum);
            continue;
        }
        if narg & type_ != 0 && command_matches(&arg, cmd) {
            set_uid_var(host, obj, id, "actor", actor);
            argument = skip_spaces(argument);
            set_var(host, obj, id, "arg", argument);
            cmd = skip_spaces(cmd);
            set_var(host, obj, id, "cmd", cmd);
            if run(host, obj, id) != 0 {
                return 1;
            }
            if object_gone(host, obj) {
                return 0;
            }
        }
    }
    0
}

/// `command_otrigger_list`: [`cmd_otrig`] over the objects that were in
/// `list` when the walk started and are still there when their turn comes
/// (still carried by the actor for `OCMD_INVEN`, still in `room` for
/// `OCMD_ROOM`); scripts may purge or move any of them.
fn command_otrigger_list<H: DgHost + ?Sized>(
    host: &mut H,
    list: Vec<Uid>,
    actor: Uid,
    cmd: &str,
    argument: &str,
    type_: i32,
    room: Option<Uid>,
) -> i32 {
    for obj in list {
        if !host.exists(obj) {
            continue;
        }
        if (type_ == OCMD_INVEN && host.obj_carried_by(obj) != Some(actor))
            || (type_ == OCMD_ROOM && (room.is_none() || host.in_room(obj) != room))
        {
            continue;
        }
        if cmd_otrig(host, obj, actor, cmd, argument, type_) != 0 {
            return 1;
        }
    }
    0
}

/// `command_otrigger(actor, cmd, argument)`: offers a typed command to the
/// `command` triggers of the actor's worn equipment (by position), then the
/// actor's inventory, then the objects lying in the actor's room, in list
/// order. Returns `1` as soon as one claims it, else `0`.
///
/// `cmd` is the first word as `any_one_arg` left it (lowercased), or the
/// single first character when that is not a letter (`'hi` gives `'`); the
/// argument is the rest of the line, not yet stripped of its leading spaces.
///
/// Called from `command_interpreter` (`interpreter.c`) last, after the world
/// and mob command triggers; a non-zero result means the command was handled
/// and is not executed.
pub fn command_otrigger<H: DgHost + ?Sized>(
    host: &mut H,
    actor: Uid,
    cmd: &str,
    argument: &str,
) -> i32 {
    if !host.valid_dg_target(actor, false) {
        return 0;
    }
    for pos in 0..NUM_WEARS {
        if let Some(worn) = host.equipment(actor, pos)
            && cmd_otrig(host, worn, actor, cmd, argument, OCMD_EQUIP) != 0
        {
            return 1;
        }
    }
    let carried = host.carrying(actor);
    if command_otrigger_list(host, carried, actor, cmd, argument, OCMD_INVEN, None) != 0 {
        return 1;
    }
    let Some(room) = room_of(host, actor) else {
        return 0;
    };
    let contents = host.room_contents(room);
    if command_otrigger_list(host, contents, actor, cmd, argument, OCMD_ROOM, Some(room)) != 0 {
        return 1;
    }
    0
}

/// `wear_otrigger(obj, actor, where)`: runs the first idle `wear` trigger (no
/// roll), with `actor` set, and returns `0` if the object was purged, else
/// the script's result; `1` if none ran.
///
/// Called from `perform_wear` (`act.item.c`) before the object is worn, from
/// `reset_zone` (`db.c`) for `E` commands, and from `save_char` (`players.c`)
/// when it re-equips the character after writing the file; a `0` return
/// keeps the object off (both of those put it in the carrier's inventory
/// with `obj_to_char`). `_where` is unused, as in the C.
pub fn wear_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid, actor: Uid, _where: i32) -> i32 {
    if !script_check(host, obj, OTRIG_WEAR) {
        return 1;
    }
    for id in trigger_ids(host, obj) {
        if checked(host, obj, id, OTRIG_WEAR).is_some() {
            set_uid_var(host, obj, id, "actor", actor);
            let ret_val = run(host, obj, id);
            return if !object_gone(host, obj) { ret_val } else { 0 };
        }
    }
    1
}

/// `remove_otrigger(obj, actor)`: as [`wear_otrigger`] for the `remove`
/// trigger, after a `valid_dg_target` test on the actor (`1` if that fails).
///
/// Called from `perform_remove` (`act.item.c`) before the object comes off (a
/// `0` return keeps it on), and from `make_corpse` (`fight.c`) and
/// `save_char` (`players.c`) for each worn item, where the result is ignored.
pub fn remove_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid, actor: Uid) -> i32 {
    if !script_check(host, obj, OTRIG_REMOVE) {
        return 1;
    }
    if !host.valid_dg_target(actor, false) {
        return 1;
    }
    for id in trigger_ids(host, obj) {
        if checked(host, obj, id, OTRIG_REMOVE).is_some() {
            set_uid_var(host, obj, id, "actor", actor);
            let ret_val = run(host, obj, id);
            return if !object_gone(host, obj) { ret_val } else { 0 };
        }
    }
    1
}

/// `drop_otrigger(obj, actor)`: runs the first `drop` trigger that wins its
/// roll, with `actor` set, and returns `0` if the object was purged, else the
/// script's result; `1` if none ran.
///
/// Called from `perform_drop` and `perform_put` (`act.item.c`) before the
/// object leaves the actor; a `0` return keeps it, and the caller also stops
/// if the object no longer exists.
pub fn drop_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid, actor: Uid) -> i32 {
    if !script_check(host, obj, OTRIG_DROP) {
        return 1;
    }
    let Some(id) = find_rolled(host, obj, OTRIG_DROP) else {
        return 1;
    };
    set_uid_var(host, obj, id, "actor", actor);
    let ret_val = run(host, obj, id);
    if !object_gone(host, obj) { ret_val } else { 0 }
}

/// `give_otrigger(obj, actor, victim)`: runs the first `give` trigger that
/// wins its roll, with `actor` (the giver) and `victim` set, and returns `0`
/// if the object was purged or is no longer carried by the giver, else the
/// script's result; `1` if none ran.
///
/// Called from `perform_give` (`act.item.c`) before `receive_mtrigger`, and
/// from `do_steal` (`act.other.c`) with the roles swapped; a `0` return
/// cancels the transfer.
pub fn give_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid, actor: Uid, victim: Uid) -> i32 {
    if !script_check(host, obj, OTRIG_GIVE) {
        return 1;
    }
    let Some(id) = find_rolled(host, obj, OTRIG_GIVE) else {
        return 1;
    };
    set_uid_var(host, obj, id, "actor", actor);
    set_uid_var(host, obj, id, "victim", victim);
    let ret_val = run(host, obj, id);
    if object_gone(host, obj) || host.obj_carried_by(obj) != Some(actor) {
        0
    } else {
        ret_val
    }
}

/// `load_otrigger(obj)`: an object was just loaded. Script `load` commands
/// call this for the object they create.
///
/// Runs the first `load` trigger that wins its roll; a run that hit the
/// recursion cap ([`SCRIPT_ERROR_CODE`]) removes the script from the object's
/// prototype. Also called from zone resets (`db.c`), `do_load`
/// (`act.wizard.c`) and `mag_creations` (`magic.c`). The result is ignored.
pub fn load_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid) {
    if !script_check(host, obj, OTRIG_LOAD) {
        return;
    }
    let mut result = 0;
    if let Some(id) = find_rolled(host, obj, OTRIG_LOAD) {
        result = run(host, obj, id);
    }
    if result == SCRIPT_ERROR_CODE {
        host.disp_free_proto_script(obj);
    }
}

/// `cast_otrigger(actor, obj, spellnum)`: runs the first `cast` trigger of the
/// spell's target object that wins its roll, with `actor`, `spell` and
/// `spellname` set, and returns its result; `1` if there is no object or no
/// trigger ran.
///
/// Called from `call_magic` (`spell_parser.c`) after `cast_wtrigger`; a `0`
/// return cancels the spell.
pub fn cast_otrigger<H: DgHost + ?Sized>(
    host: &mut H,
    actor: Uid,
    obj: Option<Uid>,
    spellnum: i32,
) -> i32 {
    let Some(obj) = obj else {
        return 1;
    };
    if !script_check(host, obj, OTRIG_CAST) {
        return 1;
    }
    let Some(id) = find_rolled(host, obj, OTRIG_CAST) else {
        return 1;
    };
    set_uid_var(host, obj, id, "actor", actor);
    set_var(host, obj, id, "spell", &spellnum.to_string());
    let name = skill_name(host, spellnum);
    set_var(host, obj, id, "spellname", &name);
    run(host, obj, id)
}

/// `leave_otrigger(room, actor, dir)`: for each object lying in `room` (list
/// order) with a `leave` trigger, runs **every** idle `leave` trigger that
/// wins its roll, with `direction` (`dirs[dir]`, `none` outside
/// `0..DIR_COUNT`) and `actor` set. Returns `0` if any run returned `0`,
/// else `1`. A purged object ends its own triggers; the walk of the room goes
/// on.
///
/// Called from `do_simple_move` (`act.movement.c`) after `leave_wtrigger`,
/// before the actor leaves; a `0` return, or a script that moved the actor,
/// cancels the move.
pub fn leave_otrigger<H: DgHost + ?Sized>(host: &mut H, room: Uid, actor: Uid, dir: i32) -> i32 {
    if !host.valid_dg_target(actor, true) {
        return 1;
    }
    let mut final_ret = 1;
    let mut cur = host.room_contents(room).first().copied();
    let mut steps = 0;
    while let Some(obj) = cur {
        steps += 1;
        if walk_exhausted(host, steps, "leave_otrigger", "objects") || !host.exists(obj) {
            break;
        }
        // `obj_next = obj->next_content`, read before the triggers run.
        let saved = next_content(host, obj);
        cur = saved;
        if !script_check(host, obj, OTRIG_LEAVE) {
            continue;
        }
        let mut walk = TriggerWalk::new(host, obj);
        while let Some(id) = walk.next(host, obj, "leave_otrigger") {
            let Some(narg) = checked(host, obj, id, OTRIG_LEAVE) else {
                continue;
            };
            if roll(host, narg) {
                let direction = direction_text(host, dir, false);
                set_var(host, obj, id, "direction", direction);
                set_uid_var(host, obj, id, "actor", actor);
                if run(host, obj, id) == 0 {
                    final_ret = 0;
                }
                if object_gone(host, obj) {
                    break;
                }
            }
        }
    }
    final_ret
}

/// `consume_otrigger(obj, actor, cmd)`: runs the first idle `consume` trigger
/// (no roll), with `actor` and `command` (`eat`, `drink` or `quaff` for
/// `OCMD_EAT`, `OCMD_DRINK`, `OCMD_QUAFF`) set, and returns `0` if the object
/// was purged, else the script's result; `1` if none ran.
///
/// Called from `do_eat` and `do_drink` (`act.item.c`) before anything is
/// consumed, and from `mag_objectmagic` (`spell_parser.c`) for a quaffed
/// potion; a `0` return stops the command.
pub fn consume_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid, actor: Uid, cmd: i32) -> i32 {
    if !script_check(host, obj, OTRIG_CONSUME) {
        return 1;
    }
    for id in trigger_ids(host, obj) {
        if checked(host, obj, id, OTRIG_CONSUME).is_some() {
            set_uid_var(host, obj, id, "actor", actor);
            match cmd {
                OCMD_EAT => set_var(host, obj, id, "command", "eat"),
                OCMD_DRINK => set_var(host, obj, id, "command", "drink"),
                OCMD_QUAFF => set_var(host, obj, id, "command", "quaff"),
                _ => {}
            }
            let ret_val = run(host, obj, id);
            return if !object_gone(host, obj) { ret_val } else { 0 };
        }
    }
    1
}

/// `time_otrigger(obj)`: as [`time_mtrigger`] for an object.
/// Called by [`check_time_triggers`] for every object with a script.
pub fn time_otrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid) {
    if !script_check(host, obj, OTRIG_TIME) {
        return;
    }
    let hours = host.mud_time().hours;
    if let Some(id) = find_narg(host, obj, OTRIG_TIME, |narg| hours == narg) {
        set_var(host, obj, id, "time", &hours.to_string());
        run(host, obj, id);
    }
}

// ---------------------------------------------------------------------------
// World triggers.
// ---------------------------------------------------------------------------

/// `reset_wtrigger(room)`: runs the first `reset` trigger that wins its roll.
///
/// Called from `reset_zone` (`db.c`) for every room of the zone, by vnum,
/// after the zone's reset commands have run. The result is ignored.
pub fn reset_wtrigger<H: DgHost + ?Sized>(host: &mut H, room: Uid) {
    if !script_check(host, room, WTRIG_RESET) {
        return;
    }
    if let Some(id) = find_rolled(host, room, WTRIG_RESET) {
        run(host, room, id);
    }
}

/// `random_wtrigger(room)`: runs the first `random` trigger that wins its
/// roll. Called by [`script_trigger_check`] for each room with a script that
/// is in an occupied zone or carries the `GLOBAL` bit.
pub fn random_wtrigger<H: DgHost + ?Sized>(host: &mut H, room: Uid) {
    if !script_check(host, room, WTRIG_RANDOM) {
        return;
    }
    if let Some(id) = find_rolled(host, room, WTRIG_RANDOM) {
        run(host, room, id);
    }
}

/// `enter_wtrigger(room, actor, dir)`: `actor` is entering `room` from
/// direction `dir` (`-1` when moved by a script). Returns `0` when a trigger
/// refuses the entry. Script `teleport` and `goto` commands call this.
///
/// Runs the first `enter` trigger that wins its roll, with `direction` (the
/// opposite of `dir`: the way back out; `none` outside `0..DIR_COUNT`) and
/// `actor` set, and returns its result; `1` if none ran. Also called from
/// `do_simple_move` (`act.movement.c`) after the actor is in the new room and
/// `entry_mtrigger` allowed it (a `0` return puts the actor back in the room
/// it left), and from `do_goto`, `do_teleport` and `do_trans`
/// (`act.wizard.c`) with `dir` `-1`, where the result is ignored.
pub fn enter_wtrigger<H: DgHost + ?Sized>(host: &mut H, room: Uid, actor: Uid, dir: i32) -> i32 {
    if !script_check(host, room, WTRIG_ENTER) {
        return 1;
    }
    let Some(id) = find_rolled(host, room, WTRIG_ENTER) else {
        return 1;
    };
    let direction = direction_text(host, dir, true);
    set_var(host, room, id, "direction", direction);
    set_uid_var(host, room, id, "actor", actor);
    run(host, room, id)
}

/// `command_wtrigger(actor, cmd, argument)`: offers a typed command to the
/// `command` triggers of the actor's room. The first idle trigger whose
/// argument is `*` or a prefix of `cmd` gets `actor`, `arg` and `cmd`, runs,
/// and its result is returned whether or not it is zero; `0` if none
/// matched.
///
/// `cmd` is the first word as `any_one_arg` left it (lowercased), or the
/// single first character when that is not a letter (`'hi` gives `'`); the
/// argument is the rest of the line, not yet stripped of its leading spaces.
///
/// Called from `command_interpreter` (`interpreter.c`) first, before the mob
/// and object command triggers; a non-zero result means the command was
/// handled and is not executed.
pub fn command_wtrigger<H: DgHost + ?Sized>(
    host: &mut H,
    actor: Uid,
    cmd: &str,
    argument: &str,
) -> i32 {
    let Some(room) = room_of(host, actor) else {
        return 0;
    };
    if !script_check(host, room, WTRIG_COMMAND) {
        return 0;
    }
    if !host.valid_dg_target(actor, false) {
        return 0;
    }
    for id in trigger_ids(host, room) {
        if checked(host, room, id, WTRIG_COMMAND).is_none() {
            continue;
        }
        let arg = trig_arg(host, room, id);
        if arg.is_empty() {
            let vnum = trig_vnum(host, room, id);
            log_no_text(host, "W-Command", vnum);
            continue;
        }
        if command_matches(&arg, cmd) {
            set_uid_var(host, room, id, "actor", actor);
            set_var(host, room, id, "arg", skip_spaces(argument));
            set_var(host, room, id, "cmd", skip_spaces(cmd));
            return run(host, room, id);
        }
    }
    0
}

/// `speech_wtrigger(actor, str)`: runs the first idle `speech` trigger of the
/// actor's room whose argument is `*` or matches the spoken text (as for
/// [`speech_mtrigger`]), with `actor` and `speech` set.
///
/// Called from `do_say` (`act.comm.c`) last, right after `speech_mtrigger`
/// and unconditionally: also for a bare `say`, with an empty argument. The
/// result is ignored.
pub fn speech_wtrigger<H: DgHost + ?Sized>(host: &mut H, actor: Uid, str: &str) {
    let Some(room) = room_of(host, actor) else {
        return;
    };
    if !script_check(host, room, WTRIG_SPEECH) {
        return;
    }
    for id in trigger_ids(host, room) {
        if checked(host, room, id, WTRIG_SPEECH).is_none() {
            continue;
        }
        let arg = trig_arg(host, room, id);
        if arg.is_empty() {
            let vnum = trig_vnum(host, room, id);
            log_no_text(host, "W-Speech", vnum);
            continue;
        }
        if arg.starts_with('*') || text_matches(trig_narg(host, room, id), &arg, str) {
            set_uid_var(host, room, id, "actor", actor);
            set_var(host, room, id, "speech", str);
            run(host, room, id);
            break;
        }
    }
}

/// `drop_wtrigger(obj, actor)`: runs the first `drop` trigger of the actor's
/// room that wins its roll, with `actor` and `object` set. Returns `0` if the
/// object was purged or is no longer carried by the actor, else the script's
/// result; `1` if none ran.
///
/// Called from `perform_drop` (`act.item.c`) after `drop_otrigger`, for a
/// plain drop (`SCMD_DROP`), and from `perform_drop_gold` (`act.item.c`) in
/// its non-donate branch, for the coins about to be dropped (the donate and
/// junk modes do not call it); a `0` return keeps the object, and for coins
/// the caller extracts the money object if it still exists. (Coins are
/// carried by nobody, so a `drop` trigger that ran always refuses them.)
pub fn drop_wtrigger<H: DgHost + ?Sized>(host: &mut H, obj: Uid, actor: Uid) -> i32 {
    let Some(room) = room_of(host, actor) else {
        return 1;
    };
    if !script_check(host, room, WTRIG_DROP) {
        return 1;
    }
    let Some(id) = find_rolled(host, room, WTRIG_DROP) else {
        return 1;
    };
    set_uid_var(host, room, id, "actor", actor);
    set_uid_var(host, room, id, "object", obj);
    let ret_val = run(host, room, id);
    if !host.exists(obj) || host.obj_carried_by(obj) != Some(actor) {
        0
    } else {
        ret_val
    }
}

/// `cast_wtrigger(actor, vict, obj, spellnum)`: runs the first `cast` trigger
/// of the caster's room that wins its roll, with `actor`, `victim` and
/// `object` (each only when given), `spell` and `spellname` set, and returns
/// its result; `1` if none ran.
///
/// Called first from `call_magic` (`spell_parser.c`); a `0` return cancels
/// the spell.
pub fn cast_wtrigger<H: DgHost + ?Sized>(
    host: &mut H,
    actor: Uid,
    vict: Option<Uid>,
    obj: Option<Uid>,
    spellnum: i32,
) -> i32 {
    let Some(room) = room_of(host, actor) else {
        return 1;
    };
    if !script_check(host, room, WTRIG_CAST) {
        return 1;
    }
    let Some(id) = find_rolled(host, room, WTRIG_CAST) else {
        return 1;
    };
    set_uid_var(host, room, id, "actor", actor);
    if let Some(vict) = vict {
        set_uid_var(host, room, id, "victim", vict);
    }
    if let Some(obj) = obj {
        set_uid_var(host, room, id, "object", obj);
    }
    set_var(host, room, id, "spell", &spellnum.to_string());
    let name = skill_name(host, spellnum);
    set_var(host, room, id, "spellname", &name);
    run(host, room, id)
}

/// `leave_wtrigger(room, actor, dir)`: runs the first `leave` trigger of
/// `room` that wins its roll, with `direction` (`dirs[dir]`, `none` outside
/// `0..DIR_COUNT`) and `actor` set, and returns its result; `1` if none ran
/// or the actor is not a valid target.
///
/// Called from `do_simple_move` (`act.movement.c`) after `leave_mtrigger`; a
/// `0` return, or a script that moved the actor, cancels the move.
pub fn leave_wtrigger<H: DgHost + ?Sized>(host: &mut H, room: Uid, actor: Uid, dir: i32) -> i32 {
    if !host.valid_dg_target(actor, true) {
        return 1;
    }
    if !script_check(host, room, WTRIG_LEAVE) {
        return 1;
    }
    let Some(id) = find_rolled(host, room, WTRIG_LEAVE) else {
        return 1;
    };
    let direction = direction_text(host, dir, false);
    set_var(host, room, id, "direction", direction);
    set_uid_var(host, room, id, "actor", actor);
    run(host, room, id)
}

/// `door_wtrigger(actor, subcmd, dir)`: runs the first `door` trigger of the
/// actor's room that wins its roll, with `cmd`, `direction` and `actor` set,
/// and returns its result; `1` if none ran.
///
/// Called from `do_doorcmd` (`act.movement.c`) after `door_mtrigger`; a `0`
/// return stops the command.
pub fn door_wtrigger<H: DgHost + ?Sized>(host: &mut H, actor: Uid, subcmd: i32, dir: i32) -> i32 {
    let Some(room) = room_of(host, actor) else {
        return 1;
    };
    if !script_check(host, room, WTRIG_DOOR) {
        return 1;
    }
    let Some(id) = find_rolled(host, room, WTRIG_DOOR) else {
        return 1;
    };
    set_var(host, room, id, "cmd", door_text(subcmd));
    let direction = direction_text(host, dir, false);
    set_var(host, room, id, "direction", direction);
    set_uid_var(host, room, id, "actor", actor);
    run(host, room, id)
}

/// `time_wtrigger(room)`: as [`time_mtrigger`] for a room.
/// Called by [`check_time_triggers`] for each room with a script that is in
/// an occupied zone or carries the `GLOBAL` bit.
pub fn time_wtrigger<H: DgHost + ?Sized>(host: &mut H, room: Uid) {
    if !script_check(host, room, WTRIG_TIME) {
        return;
    }
    let hours = host.mud_time().hours;
    if let Some(id) = find_narg(host, room, WTRIG_TIME, |narg| hours == narg) {
        set_var(host, room, id, "time", &hours.to_string());
        run(host, room, id);
    }
}

/// `login_wtrigger(room, actor)`: runs the first `login` trigger of `room`
/// that wins its roll, with `actor` set, and returns its result; `1` if none
/// ran.
///
/// Called from `enter_player_game` (`interpreter.c`) once the character is
/// loaded into its start room. The result is ignored.
pub fn login_wtrigger<H: DgHost + ?Sized>(host: &mut H, room: Uid, actor: Uid) -> i32 {
    if !script_check(host, room, WTRIG_LOGIN) {
        return 1;
    }
    let Some(id) = find_rolled(host, room, WTRIG_LOGIN) else {
        return 1;
    };
    set_uid_var(host, room, id, "actor", actor);
    run(host, room, id)
}

// ---------------------------------------------------------------------------
// The heartbeat sweeps (`dg_scripts.c`).
// ---------------------------------------------------------------------------

/// Every room, in world-table order (by vnum).
fn all_rooms<H: DgHost + ?Sized>(host: &H) -> Vec<Uid> {
    host.cmd_rooms_in_range(0, i32::MAX)
}

/// `script_trigger_check()`: the random-trigger sweep. Offers every character
/// with a `random` trigger (when its room's zone is occupied, or the script
/// has the `GLOBAL` bit; a character in no room only qualifies by `GLOBAL`),
/// every object with one, and every room with one (occupied zone or
/// `GLOBAL`) to its `random` trigger, in that order.
///
/// Called from `heartbeat` (`comm.c`) every `PULSE_DG_SCRIPT` pulses.
pub fn script_trigger_check<H: DgHost + ?Sized>(host: &mut H) {
    for ch in host.character_list() {
        if !host.exists(ch) {
            continue;
        }
        if script_check(host, ch, WTRIG_RANDOM)
            && (room_of(host, ch).is_some_and(|room| !zone_empty(host, room))
                || script_check(host, ch, WTRIG_GLOBAL))
        {
            random_mtrigger(host, ch);
        }
    }

    for obj in host.object_list() {
        if !host.exists(obj) {
            continue;
        }
        if script_check(host, obj, OTRIG_RANDOM) {
            random_otrigger(host, obj);
        }
    }

    for room in all_rooms(host) {
        if script_check(host, room, WTRIG_RANDOM)
            && (!zone_empty(host, room) || script_check(host, room, WTRIG_GLOBAL))
        {
            random_wtrigger(host, room);
        }
    }
}

/// `check_time_triggers()`: the time-trigger sweep, as [`script_trigger_check`]
/// for `time` triggers.
///
/// Called from `heartbeat` (`comm.c`) at the start of every mud hour, right
/// after `weather_and_time`.
pub fn check_time_triggers<H: DgHost + ?Sized>(host: &mut H) {
    for ch in host.character_list() {
        if !host.exists(ch) {
            continue;
        }
        if script_check(host, ch, WTRIG_TIME)
            && (room_of(host, ch).is_some_and(|room| !zone_empty(host, room))
                || script_check(host, ch, WTRIG_GLOBAL))
        {
            time_mtrigger(host, ch);
        }
    }

    for obj in host.object_list() {
        if !host.exists(obj) {
            continue;
        }
        if script_check(host, obj, OTRIG_TIME) {
            time_otrigger(host, obj);
        }
    }

    for room in all_rooms(host) {
        if script_check(host, room, WTRIG_TIME)
            && (!zone_empty(host, room) || script_check(host, room, WTRIG_GLOBAL))
        {
            time_wtrigger(host, room);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::host::{BaseHost, WorldHost};
    use crate::magic::TAR_CHAR_ROOM;
    use crate::mock::{MockAction, MockHost};
    use crate::types::{AttachType, MAX_SCRIPT_DEPTH, ScriptMemory};

    /// A script that leaves a mark in its owner's globals.
    const RAN: &str = "set ran 1\nglobal ran";

    /// A small world: the Temple (3001) with the player Alice (id 1,
    /// level 5, a Cleric, standing) and a guard mob (vnum 3024); the Altar
    /// (3002) beside it.
    struct W {
        h: MockHost,
        room: Uid,
        room2: Uid,
        alice: Uid,
        guard: Uid,
    }

    impl W {
        fn new() -> Self {
            let mut h = MockHost::new();
            let room = h.add_room(3001, "Temple");
            let room2 = h.add_room(3002, "Altar");
            let alice = h.add_player("Alice");
            let guard = h.add_mob(3024, "the guard");
            h.set_num(alice, "level", 5);
            h.set_num(alice, "class", 1);
            h.set_num(alice, "desc", 1);
            for ch in [alice, guard] {
                h.set_num(ch, "pos", 8);
                h.char_to_room(ch, room);
            }
            Self {
                h,
                room,
                room2,
                alice,
                guard,
            }
        }

        /// Another standing mob in the Temple; it goes to the head of the
        /// room's people.
        fn mob(&mut self, vnum: i32, name: &str) -> Uid {
            let mob = self.h.add_mob(vnum, name);
            self.h.set_num(mob, "pos", 8);
            self.h.char_to_room(mob, self.room);
            mob
        }

        fn obj(&mut self, vnum: i32, name: &str) -> Uid {
            self.h.add_obj(vnum, name)
        }

        /// A prototype of the owner's kind, attached; returns the instance id.
        fn trig(
            &mut self,
            owner: Uid,
            vnum: i32,
            types: &str,
            narg: i32,
            arg: &str,
            body: &str,
        ) -> u64 {
            let attach = AttachType::from(self.h.kind(owner).unwrap());
            let proto = self.h.add_proto(vnum, attach, types, narg, arg, body);
            self.h.attach(owner, &proto).unwrap()
        }

        /// A global the owner's script holds.
        fn g(&self, owner: Uid, name: &str) -> Option<String> {
            self.h
                .script(owner)?
                .global_vars
                .get(name)
                .map(|var| var.value.clone())
        }

        fn ran(&self, owner: Uid) -> bool {
            self.g(owner, "ran").is_some()
        }

        fn rolls(&mut self, rolls: &[i64]) {
            self.h.rolls = rolls.iter().copied().collect();
        }

        fn charm(&mut self, ch: Uid) {
            self.h.set_num(ch, "aff:22", 1);
        }

        fn sleep(&mut self, ch: Uid) {
            self.h.set_num(ch, "pos", 4);
        }

        fn blind(&mut self, ch: Uid, target: Uid) {
            self.h
                .entity_mut(ch)
                .unwrap()
                .lists
                .insert("cannot_see".to_owned(), vec![target]);
        }

        fn busy(&mut self, owner: Uid, id: u64) {
            self.h
                .script_mut(owner)
                .unwrap()
                .trigger_mut(id)
                .unwrap()
                .run
                .depth = 1;
        }

        fn say_log(&self) -> Vec<String> {
            self.h.mud_commands.iter().map(|(_, c)| c.clone()).collect()
        }
    }

    fn id(uid: Uid) -> String {
        uid_text(uid)
    }

    // ---- the text helpers ---------------------------------------------------

    #[test]
    fn one_phrase_takes_a_word_or_a_quoted_phrase() {
        assert_eq!(one_phrase("  hello world"), ("hello".to_owned(), " world"));
        assert_eq!(
            one_phrase("\"two words\" rest"),
            ("two words".to_owned(), " rest")
        );
        assert_eq!(one_phrase(""), (String::new(), ""));
        assert_eq!(one_phrase("   "), (String::new(), ""));
        // A word ends at a quote, which then opens the next phrase.
        assert_eq!(one_phrase("ab\"cd\""), ("ab".to_owned(), "\"cd\""));
        // An empty pair of quotes is an empty phrase.
        assert_eq!(one_phrase("\"\""), (String::new(), ""));
        // skip_spaces stops at a tab, and the word loop does not start there.
        assert_eq!(one_phrase("\tx"), (String::new(), "\tx"));
        // An unterminated quote loses its last byte (matching_quote returns
        // the last byte's index and the C NULs it).
        assert_eq!(one_phrase("\"open"), ("ope".to_owned(), ""));
        assert_eq!(one_phrase("\""), (String::new(), ""));
        // A backslash escapes the closing quote.
        assert_eq!(one_phrase("\"a\\\"b\" c"), ("a\\\"b".to_owned(), " c"));
        // An unterminated quote ending in a multi-byte character cannot panic.
        assert_eq!(one_phrase("\"\u{e9}"), ("\u{fffd}".to_owned(), ""));
    }

    #[test]
    fn is_substring_wants_a_whole_word_at_the_first_occurrence() {
        assert!(is_substring("hello", "say hello there"));
        assert!(is_substring("HELLO", "oh hello"));
        assert!(is_substring("hi", "hi"));
        // Punctuation is a boundary too.
        assert!(is_substring("hello", "Hello, world"));
        assert!(is_substring("world", "hello world!"));
        assert!(!is_substring("hell", "say hello there"));
        assert!(!is_substring("ello", "hello"));
        assert!(!is_substring("", "x"));
        assert!(!is_substring("a", ""));
        // Only the first occurrence is tried: "cat" in "concat" is not a
        // word, and the later word is never looked at.
        assert!(!is_substring("cat", "concat cat"));
        // str_str does not back up after a partial match.
        assert!(!is_substring("aab", "aaab"));
        // Non-ASCII bytes are neither space nor punctuation.
        assert!(!is_substring("caf", "caf\u{e9}"));
    }

    #[test]
    fn word_check_matches_any_word_or_phrase() {
        assert!(word_check("anything", "*"));
        assert!(word_check("", "* ignored"));
        assert!(word_check("I want to buy a sword", "buy sell"));
        assert!(word_check("I want to sell it", "buy sell"));
        assert!(!word_check("hello there", "buy sell"));
        assert!(word_check("I want to buy a sword", "\"buy a\""));
        assert!(!word_check("buy sword a", "\"buy a\""));
        assert!(!word_check("buy", ""));
        // An empty phrase ends the list.
        assert!(!word_check("x", "\"\" x"));
        // Whole words only.
        assert!(!word_check("swordsman", "sword"));
        // A list longer than the C buffer is cut, on a character boundary.
        let long = "\u{e9}".repeat(400);
        assert!(!word_check("zzz", &long));
        let tail = format!("{} zzz", "a ".repeat(300));
        assert!(!word_check("zzz", &tail));
    }

    // ---- mob triggers ----------------------------------------------------------

    #[test]
    fn random_mtrigger_runs_the_first_trigger_that_wins_its_roll() {
        let mut w = W::new();
        let t1 = w.trig(w.guard, 1, "b", 10, "", "set a 1\nglobal a");
        let t2 = w.trig(w.guard, 2, "b", 100, "", "set b 1\nglobal b");
        w.trig(w.guard, 3, "b", 100, "", "set c 1\nglobal c");
        // 20 > 10 loses the first; 5 <= 100 wins the second; the third is
        // never reached.
        w.rolls(&[20, 5, 1]);
        random_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.g(w.guard, "a"), None);
        assert_eq!(w.g(w.guard, "b").as_deref(), Some("1"));
        assert_eq!(w.g(w.guard, "c"), None);
        assert_eq!(w.h.rolls.len(), 1);

        // A busy trigger is skipped without a roll.
        w.h.rolls.clear();
        w.rolls(&[1, 1]);
        w.busy(w.guard, t1);
        w.busy(w.guard, t2);
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        random_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.g(w.guard, "c").as_deref(), Some("1"));
        assert_eq!(w.h.rolls.len(), 1);
    }

    #[test]
    fn random_mtrigger_skips_charmed_mobs_and_other_types() {
        let mut w = W::new();
        w.trig(w.guard, 1, "g", 100, "", RAN);
        w.rolls(&[1]);
        random_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
        w.trig(w.guard, 2, "b", 100, "", RAN);
        w.charm(w.guard);
        random_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
        assert_eq!(w.h.rolls.len(), 1);
    }

    #[test]
    fn bribe_compares_the_amount_with_narg_and_rolls_nothing() {
        let mut w = W::new();
        w.trig(
            w.guard,
            5,
            "m",
            10,
            "",
            "global amount\nglobal actor\nset ran 1\nglobal ran",
        );
        w.rolls(&[100]);
        bribe_mtrigger(&mut w.h, w.guard, w.alice, 9);
        assert!(!w.ran(w.guard));
        bribe_mtrigger(&mut w.h, w.guard, w.alice, 10);
        assert!(w.ran(w.guard));
        assert_eq!(w.g(w.guard, "amount").as_deref(), Some("10"));
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        assert_eq!(w.h.rolls.len(), 1);
        // A charmed mob takes the money and ignores it.
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        w.charm(w.guard);
        bribe_mtrigger(&mut w.h, w.guard, w.alice, 50);
        assert!(!w.ran(w.guard));
    }

    #[test]
    fn bribe_corpus_trigger_five_refunds_and_thanks() {
        // tbaMUD trigger 5, "Tutorial II Guard Bribe 10": `0 m 1`.
        const BRIBE: &str = "* A basic guard bribe trigger.\n\
            if %amount% >= 10\n\
              if %amount% > 10\n\
                eval change %amount% - 10\n\
                give %change% coin %actor.name%\n\
              end\n\
              say Thank you.\n\
              wait 1 sec\n\
              unlock gateway\n\
            else\n\
              say only %amount% coins, I require 10.\n\
              give %amount% coin %actor.name%\n\
            end";
        let mut w = W::new();
        let t = w.trig(w.guard, 5, "m", 1, "", BRIBE);
        bribe_mtrigger(&mut w.h, w.guard, w.alice, 5);
        assert_eq!(
            w.say_log(),
            ["say only 5 coins, I require 10.", "give 5 coin Alice"]
        );
        w.h.mud_commands.clear();
        bribe_mtrigger(&mut w.h, w.guard, w.alice, 13);
        assert_eq!(w.say_log(), ["give 3 coin Alice", "say Thank you."]);
        // The `wait` left the trigger waiting.
        assert_eq!(w.h.waits, vec![(w.guard, t, 10)]);
    }

    /// A greet script that records what it was told and returns `ret`.
    fn greet_script(ret: i32) -> String {
        format!("global direction\nglobal actor\nset ran 1\nglobal ran\nreturn {ret}")
    }

    #[test]
    fn greet_sets_the_reversed_direction_and_the_actor() {
        let mut w = W::new();
        w.trig(w.guard, 1, "g", 100, "", &greet_script(1));
        // Entering by walking north: the actor came from the south.
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert_eq!(w.g(w.guard, "direction").as_deref(), Some("south"));
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        greet_mtrigger(&mut w.h, w.alice, 4);
        assert_eq!(w.g(w.guard, "direction").as_deref(), Some("down"));
        greet_mtrigger(&mut w.h, w.alice, -1);
        assert_eq!(w.g(w.guard, "direction").as_deref(), Some("none"));
        // Diagonals exist only when DIR_COUNT says so.
        greet_mtrigger(&mut w.h, w.alice, 7);
        assert_eq!(w.g(w.guard, "direction").as_deref(), Some("none"));
        w.h.globals
            .nums
            .insert("config:diagonal_dirs".to_owned(), 1);
        greet_mtrigger(&mut w.h, w.alice, 7);
        assert_eq!(w.g(w.guard, "direction").as_deref(), Some("southwest"));
        greet_mtrigger(&mut w.h, w.alice, 10);
        assert_eq!(w.g(w.guard, "direction").as_deref(), Some("none"));
    }

    #[test]
    fn greet_runs_every_trigger_of_every_mob_and_ands_the_results() {
        let mut w = W::new();
        let late = w.mob(3025, "the clerk");
        // Room order is [late, guard, alice].
        w.trig(late, 1, "g", 100, "", &greet_script(1));
        w.trig(late, 2, "g", 100, "", "set second 1\nglobal second");
        w.trig(w.guard, 3, "g", 100, "", &greet_script(0));
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 0);
        assert!(w.ran(late) && w.ran(w.guard));
        assert_eq!(w.g(late, "second").as_deref(), Some("1"));
        // All pass: 1.
        let mut w = W::new();
        w.trig(w.guard, 1, "g", 100, "", &greet_script(1));
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
    }

    #[test]
    fn greet_guards() {
        let setup = || {
            let mut w = W::new();
            w.trig(w.guard, 1, "g", 100, "", &greet_script(0));
            w
        };
        // Asleep, fighting, charmed: skipped, and the move is allowed.
        let mut w = setup();
        w.sleep(w.guard);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert!(!w.ran(w.guard));
        let mut w = setup();
        let bob = w.mob(3030, "a rat");
        w.h.set_link(w.guard, "fighting", Some(bob));
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert!(!w.ran(w.guard));
        let mut w = setup();
        w.charm(w.guard);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert!(!w.ran(w.guard));
        // The actor's own triggers do not fire.
        let mut w = W::new();
        w.trig(w.guard, 1, "g", 100, "", &greet_script(0));
        assert_eq!(greet_mtrigger(&mut w.h, w.guard, 0), 1);
        assert!(!w.ran(w.guard));
        // A god who is not a valid target is let through untouched.
        let mut w = setup();
        w.h.set_num(w.alice, "dg_invalid", 1);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert!(!w.ran(w.guard));
        // The roll is made last.
        let mut w = W::new();
        w.trig(w.guard, 1, "g", 50, "", &greet_script(0));
        w.rolls(&[60]);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert!(!w.ran(w.guard));
        w.rolls(&[50]);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 0);
        // A trigger that is still running is not started again, and no die
        // is rolled for it.
        let mut w = W::new();
        let t = w.trig(w.guard, 1, "g", 50, "", &greet_script(0));
        w.busy(w.guard, t);
        w.rolls(&[1]);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert_eq!(w.h.rolls.len(), 1);
    }

    #[test]
    fn greet_needs_sight_except_for_greet_all() {
        let mut w = W::new();
        w.trig(w.guard, 1, "g", 100, "", &greet_script(1));
        w.blind(w.guard, w.alice);
        greet_mtrigger(&mut w.h, w.alice, 0);
        assert!(!w.ran(w.guard));
        // 'h' is MTRIG_GREET_ALL.
        w.trig(w.guard, 2, "h", 100, "", "set all 1\nglobal all");
        greet_mtrigger(&mut w.h, w.alice, 0);
        assert!(!w.ran(w.guard));
        assert_eq!(w.g(w.guard, "all").as_deref(), Some("1"));
    }

    #[test]
    fn corpus_questmaster_greeting_waits_two_seconds() {
        // tbaMUD trigger 138 ("Questmaster Greet"), `0 g 100`.
        let body = "if %actor.is_pc% && %actor.questpoints% < 50\n\
              wait 2 sec\n\
              if !%actor.varexists(on_quest_zone_1)%\n\
                say Welcome %actor.name%, are you interested in a simple quest?\n\
              end\n\
            end";
        let mut w = W::new();
        let t = w.trig(w.guard, 138, "g", 100, "", body);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 2), 1);
        assert_eq!(w.h.waits, vec![(w.guard, t, 20)]);
        assert!(w.h.mud_commands.is_empty());
    }

    #[test]
    fn greet_memory_runs_the_memory_script_and_forgets() {
        let mut w = W::new();
        let bob = w.h.add_player("Bob");
        w.h.char_to_room(bob, w.room);
        w.trig(
            w.guard,
            1,
            "o",
            100,
            "",
            "global actor\nset ran 1\nglobal ran",
        );
        w.h.set_script_mem(
            w.guard,
            vec![
                ScriptMemory { id: bob, cmd: None },
                ScriptMemory {
                    id: w.alice,
                    cmd: None,
                },
            ],
        );
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        // Only Alice's entry went.
        assert_eq!(
            w.h.script_mem(w.guard),
            vec![ScriptMemory { id: bob, cmd: None }]
        );
    }

    #[test]
    fn greet_memory_commands_are_kept_and_block_later_scripts() {
        let mut w = W::new();
        let clerk = w.mob(3025, "the clerk");
        // Room order [clerk, guard, alice]: the clerk's command comes first.
        w.h.set_script_mem(
            clerk,
            vec![ScriptMemory {
                id: w.alice,
                cmd: Some("say hi".to_owned()),
            }],
        );
        w.trig(w.guard, 1, "o", 100, "", RAN);
        w.h.set_script_mem(
            w.guard,
            vec![ScriptMemory {
                id: w.alice,
                cmd: None,
            }],
        );
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert_eq!(w.h.mud_commands, vec![(clerk, "say hi".to_owned())]);
        // The command entry stays (the C breaks before the delete).
        assert_eq!(w.h.script_mem(clerk).len(), 1);
        // The guard's script was skipped, but its memory was dropped.
        assert!(!w.ran(w.guard));
        assert!(w.h.script_mem(w.guard).is_empty());
    }

    #[test]
    fn greet_memory_guards() {
        let mem = |alice| {
            vec![ScriptMemory {
                id: alice,
                cmd: None,
            }]
        };
        // Asleep: untouched.
        let mut w = W::new();
        w.trig(w.guard, 1, "o", 100, "", RAN);
        w.h.set_script_mem(w.guard, mem(w.alice));
        w.sleep(w.guard);
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert!(!w.ran(w.guard));
        assert_eq!(w.h.script_mem(w.guard).len(), 1);
        // Cannot see the actor: no run, but the memory goes.
        let mut w = W::new();
        w.trig(w.guard, 1, "o", 100, "", RAN);
        w.h.set_script_mem(w.guard, mem(w.alice));
        w.blind(w.guard, w.alice);
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert!(!w.ran(w.guard));
        assert!(w.h.script_mem(w.guard).is_empty());
        // Charmed, and the actor's own memory: untouched.
        let mut w = W::new();
        w.h.set_script_mem(w.guard, mem(w.alice));
        w.charm(w.guard);
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert_eq!(w.h.script_mem(w.guard).len(), 1);
        let mut w = W::new();
        w.h.set_script_mem(w.guard, mem(w.guard));
        greet_memory_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.h.script_mem(w.guard).len(), 1);
        // The roll gates the script; the memory goes either way.
        let mut w = W::new();
        w.trig(w.guard, 1, "o", 30, "", RAN);
        w.h.set_script_mem(w.guard, mem(w.alice));
        w.rolls(&[31]);
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert!(!w.ran(w.guard));
        assert!(w.h.script_mem(w.guard).is_empty());
    }

    #[test]
    fn entry_memory_looks_for_remembered_people_in_the_room() {
        let mut w = W::new();
        let bob = w.h.add_player("Bob");
        w.h.char_to_room(bob, w.room);
        w.trig(
            w.guard,
            1,
            "o",
            100,
            "",
            "global actor\nset ran 1\nglobal ran",
        );
        w.h.set_script_mem(
            w.guard,
            vec![
                ScriptMemory {
                    id: w.alice,
                    cmd: Some("say hello again".to_owned()),
                },
                ScriptMemory { id: bob, cmd: None },
                ScriptMemory {
                    id: w.guard,
                    cmd: None,
                },
                ScriptMemory {
                    id: MOB_ID_FAR,
                    cmd: None,
                },
            ],
        );
        entry_memory_mtrigger(&mut w.h, w.guard);
        assert_eq!(
            w.h.mud_commands,
            vec![(w.guard, "say hello again".to_owned())]
        );
        assert_eq!(w.g(w.guard, "actor"), Some(id(bob)));
        // Both entries went, commands included; its own and a stranger's
        // stay.
        assert_eq!(
            w.h.script_mem(w.guard),
            vec![
                ScriptMemory {
                    id: w.guard,
                    cmd: None
                },
                ScriptMemory {
                    id: MOB_ID_FAR,
                    cmd: None
                },
            ]
        );
    }

    const MOB_ID_FAR: Uid = 19_999_999;

    #[test]
    fn entry_memory_rolls_for_the_script_and_skips_charmed_mobs() {
        let mut w = W::new();
        w.trig(w.guard, 1, "o", 50, "", RAN);
        let mem = vec![ScriptMemory {
            id: w.alice,
            cmd: None,
        }];
        w.h.set_script_mem(w.guard, mem.clone());
        w.rolls(&[51]);
        entry_memory_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
        assert!(w.h.script_mem(w.guard).is_empty());
        w.h.set_script_mem(w.guard, mem.clone());
        w.charm(w.guard);
        entry_memory_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.h.script_mem(w.guard), mem);
    }

    #[test]
    fn entry_returns_the_first_rolled_trigger_result() {
        let mut w = W::new();
        assert_eq!(entry_mtrigger(&mut w.h, w.guard), 1);
        w.trig(w.guard, 1, "i", 20, "", "return 0");
        w.trig(
            w.guard,
            2,
            "i",
            100,
            "",
            "set second 1\nglobal second\nreturn 0",
        );
        w.trig(w.guard, 3, "i", 100, "", "return 1");
        w.rolls(&[30, 1, 1]);
        assert_eq!(entry_mtrigger(&mut w.h, w.guard), 0);
        assert_eq!(w.g(w.guard, "second").as_deref(), Some("1"));
        assert_eq!(w.h.rolls.len(), 1);
        // Nothing wins: 1.
        w.rolls(&[99, 99, 99]);
        w.h.script_mut(w.guard).unwrap().triggers.truncate(1);
        assert_eq!(entry_mtrigger(&mut w.h, w.guard), 1);
        w.charm(w.guard);
        w.rolls(&[1]);
        assert_eq!(entry_mtrigger(&mut w.h, w.guard), 1);
    }

    #[test]
    fn command_mtrigger_matches_prefixes_and_sets_the_variables() {
        let mut w = W::new();
        w.trig(
            w.guard,
            1,
            "c",
            0,
            "kill",
            "global arg\nglobal cmd\nglobal actor\nset ran 1\nglobal ran",
        );
        // The trigger's argument must be a prefix of what was typed.
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "k", "guard"), 0);
        assert!(!w.ran(w.guard));
        assert_eq!(
            command_mtrigger(&mut w.h, w.alice, "KILL", "  guard now"),
            1
        );
        assert_eq!(w.g(w.guard, "arg").as_deref(), Some("guard now"));
        assert_eq!(w.g(w.guard, "cmd").as_deref(), Some("KILL"));
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        // A longer typed word still has the trigger argument as its prefix.
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "killer", ""), 1);
        // Another command does not.
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "look", ""), 0);
        assert!(!w.ran(w.guard));
    }

    #[test]
    fn command_mtrigger_star_matches_everything() {
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", RAN);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "dance", ""), 1);
        assert!(w.ran(w.guard));
    }

    #[test]
    fn command_mtrigger_a_zero_result_lets_the_walk_go_on() {
        let mut w = W::new();
        let clerk = w.mob(3025, "the clerk");
        // Room order [clerk, guard, alice].
        w.trig(clerk, 1, "c", 0, "*", "set first 1\nglobal first\nreturn 0");
        w.trig(w.guard, 2, "c", 0, "*", "set one 1\nglobal one\nreturn 0");
        w.trig(w.guard, 3, "c", 0, "*", "set two 1\nglobal two");
        w.trig(w.guard, 4, "c", 0, "*", "set three 1\nglobal three");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
        assert_eq!(w.g(clerk, "first").as_deref(), Some("1"));
        assert_eq!(w.g(w.guard, "one").as_deref(), Some("1"));
        assert_eq!(w.g(w.guard, "two").as_deref(), Some("1"));
        // The trigger after the one that claimed the command did not run.
        assert_eq!(w.g(w.guard, "three"), None);
        // If nobody claims it, 0.
        let mut w = W::new();
        w.trig(w.guard, 2, "c", 0, "*", "return 0");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 0);
    }

    #[test]
    fn command_mtrigger_skip_spaces_advances_the_callers_text() {
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", "return 0");
        w.trig(w.guard, 2, "c", 0, "kill", "global cmd");
        // The first match skips the blank in front of `cmd` for good, so the
        // second trigger's prefix test sees "kill".
        assert_eq!(command_mtrigger(&mut w.h, w.alice, " kill", ""), 1);
        assert_eq!(w.g(w.guard, "cmd").as_deref(), Some("kill"));
    }

    #[test]
    fn command_mtrigger_guards() {
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", RAN);
        // Not a valid target.
        w.h.set_num(w.alice, "dg_invalid", 1);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 0);
        w.h.set_num(w.alice, "dg_invalid", 0);
        // Charmed.
        w.charm(w.guard);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 0);
        assert!(!w.ran(w.guard));
        // A mob's own commands reach its triggers only with
        // CONFIG_SCRIPT_PLAYERS.
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", RAN);
        assert_eq!(command_mtrigger(&mut w.h, w.guard, "x", ""), 0);
        w.h.globals.nums.insert("script_players".to_owned(), 1);
        assert_eq!(command_mtrigger(&mut w.h, w.guard, "x", ""), 1);
        // A sleeping mob still sees commands (there is no AWAKE test).
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", RAN);
        w.sleep(w.guard);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
        // A running trigger is skipped.
        let mut w = W::new();
        let t = w.trig(w.guard, 1, "c", 0, "*", RAN);
        w.busy(w.guard, t);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 0);
    }

    #[test]
    fn command_mtrigger_logs_a_trigger_without_an_argument() {
        let mut w = W::new();
        w.trig(w.guard, 77, "c", 0, "", RAN);
        w.trig(w.guard, 78, "c", 0, "*", "set second 1\nglobal second");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
        assert_eq!(
            w.h.effects,
            ["mudlog 2 31: SYSERR: Command Trigger #77 has no text argument!"]
        );
        // It was skipped; the next one ran.
        assert!(!w.ran(w.guard));
        assert_eq!(w.g(w.guard, "second").as_deref(), Some("1"));
    }

    #[test]
    fn speech_word_check_and_substring_modes() {
        let mut w = W::new();
        // narg 1: a word list; narg 0: one substring.
        w.trig(
            w.guard,
            1,
            "d",
            1,
            "buy \"for sale\"",
            "global actor\nglobal speech\nset ran 1\nglobal ran",
        );
        speech_mtrigger(&mut w.h, w.alice, "what is for sale?");
        assert!(w.ran(w.guard));
        assert_eq!(w.g(w.guard, "speech").as_deref(), Some("what is for sale?"));
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));

        let mut w = W::new();
        w.trig(w.guard, 1, "d", 0, "hello there", RAN);
        speech_mtrigger(&mut w.h, w.alice, "hello");
        assert!(!w.ran(w.guard));
        speech_mtrigger(&mut w.h, w.alice, "oh, hello there!");
        assert!(w.ran(w.guard));
        // With narg 0 `*` is a literal.
        let mut w = W::new();
        w.trig(w.guard, 1, "d", 0, "*", RAN);
        speech_mtrigger(&mut w.h, w.alice, "anything");
        assert!(!w.ran(w.guard));
        // With a word list `*` matches all.
        let mut w = W::new();
        w.trig(w.guard, 1, "d", 1, "*", RAN);
        speech_mtrigger(&mut w.h, w.alice, "anything");
        assert!(w.ran(w.guard));
    }

    #[test]
    fn speech_stops_at_the_first_match_of_each_mob() {
        let mut w = W::new();
        let clerk = w.mob(3025, "the clerk");
        w.trig(w.guard, 1, "d", 1, "no", "set a 1\nglobal a");
        w.trig(w.guard, 2, "d", 1, "yes", "set b 1\nglobal b");
        w.trig(w.guard, 3, "d", 1, "yes", "set c 1\nglobal c");
        w.trig(clerk, 4, "d", 1, "yes", "set d 1\nglobal d");
        speech_mtrigger(&mut w.h, w.alice, "yes");
        assert_eq!(w.g(w.guard, "a"), None);
        assert_eq!(w.g(w.guard, "b").as_deref(), Some("1"));
        assert_eq!(w.g(w.guard, "c"), None);
        // The other mob is still examined.
        assert_eq!(w.g(clerk, "d").as_deref(), Some("1"));
    }

    #[test]
    fn speech_guards() {
        let mut w = W::new();
        w.trig(w.guard, 1, "d", 1, "yes", RAN);
        w.sleep(w.guard);
        speech_mtrigger(&mut w.h, w.alice, "yes");
        assert!(!w.ran(w.guard));
        w.h.set_num(w.guard, "pos", 8);
        w.charm(w.guard);
        speech_mtrigger(&mut w.h, w.alice, "yes");
        assert!(!w.ran(w.guard));
        w.h.set_num(w.guard, "aff:22", 0);
        speech_mtrigger(&mut w.h, w.guard, "yes");
        assert!(!w.ran(w.guard));
        w.h.globals.nums.insert("script_players".to_owned(), 1);
        speech_mtrigger(&mut w.h, w.guard, "yes");
        assert!(w.ran(w.guard));
        // An empty argument is logged and skipped.
        let mut w = W::new();
        w.trig(w.guard, 9, "d", 1, "", RAN);
        speech_mtrigger(&mut w.h, w.alice, "yes");
        assert!(!w.ran(w.guard));
        assert_eq!(
            w.h.effects,
            ["mudlog 2 31: SYSERR: Speech Trigger #9 has no text argument!"]
        );
    }

    #[test]
    fn corpus_questmaster_answers_yes() {
        // tbaMUD trigger 139, `0 d 1` with the argument `*`.
        let body = "if %actor% == %self%\n\
              halt\n\
            end\n\
            if %actor.varexists(on_quest_zone_1)%\n\
              halt\n\
            end\n\
            if %actor.questpoints% > 50\n\
              halt\n\
            end\n\
            eval word %speech.car%\n\
            eval rest %speech.cdr%\n\
            while %word%\n\
              if yes /= %word%\n\
                say Very well %actor.name%. Would you like to find an object or hunt a mobile?\n\
                halt\n\
              end\n\
              set word %rest.car%\n\
              set rest %rest.cdr%\n\
            done";
        let mut w = W::new();
        w.trig(w.guard, 139, "d", 1, "*", body);
        speech_mtrigger(&mut w.h, w.alice, "yes please");
        assert_eq!(
            w.say_log(),
            ["say Very well Alice. Would you like to find an object or hunt a mobile?"]
        );
        // The quest master's own speech is not heard by its trigger.
        w.h.mud_commands.clear();
        speech_mtrigger(&mut w.h, w.guard, "yes");
        assert!(w.h.mud_commands.is_empty());
    }

    #[test]
    fn act_sets_the_participants_and_trims_the_line() {
        let mut w = W::new();
        let sword = w.obj(5, "a sword");
        let bag = w.obj(6, "a bag");
        w.trig(
            w.guard,
            1,
            "e",
            0,
            "hands you",
            "global actor\nglobal victim\nglobal object\nglobal target\nglobal arg\nset ran 1\nglobal ran",
        );
        let bob = w.h.add_player("Bob");
        act_mtrigger(
            &mut w.h,
            w.guard,
            "   Alice hands you a sword.\r\n",
            Some(w.alice),
            Some(bob),
            Some(sword),
            Some(bag),
            Some("ignored"),
        );
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        assert_eq!(w.g(w.guard, "victim"), Some(id(bob)));
        assert_eq!(w.g(w.guard, "object"), Some(id(sword)));
        assert_eq!(w.g(w.guard, "target"), Some(id(bag)));
        assert_eq!(
            w.g(w.guard, "arg").as_deref(),
            Some("Alice hands you a sword.")
        );
    }

    #[test]
    fn act_variables_are_set_only_for_the_entities_given() {
        let mut w = W::new();
        w.trig(
            w.guard,
            1,
            "e",
            1,
            "waves smile",
            "global actor\nglobal victim\nglobal arg\nset ran 1\nglobal ran",
        );
        act_mtrigger(
            &mut w.h,
            w.guard,
            "Alice smiles.\r\n",
            Some(w.alice),
            None,
            None,
            None,
            None,
        );
        assert!(!w.ran(w.guard));
        act_mtrigger(
            &mut w.h,
            w.guard,
            "Alice smiles.\r\n",
            None,
            None,
            None,
            None,
            None,
        );
        assert!(!w.ran(w.guard));
        act_mtrigger(
            &mut w.h,
            w.guard,
            "The wind smile\r\n",
            None,
            None,
            None,
            None,
            None,
        );
        assert!(w.ran(w.guard));
        assert_eq!(w.g(w.guard, "actor"), None);
        assert_eq!(w.g(w.guard, "victim"), None);
        assert_eq!(w.g(w.guard, "arg").as_deref(), Some("The wind smile"));
    }

    #[test]
    fn act_arg_cut_follows_the_c_pointer_arithmetic() {
        let mut w = W::new();
        w.trig(w.guard, 1, "e", 0, "foo", "global arg");
        // The `\r` is found before the blanks are skipped; skipping runs
        // through `\r\n`, past the cut, so the rest of the text is the arg.
        act_mtrigger(
            &mut w.h,
            w.guard,
            "  \r\nfoo\r\n",
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(w.g(w.guard, "arg").as_deref(), Some("foo\r\n"));
        // No `\r` at all (the C would crash): the whole text.
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        act_mtrigger(&mut w.h, w.guard, " foo", None, None, None, None, None);
        assert_eq!(w.g(w.guard, "arg").as_deref(), Some("foo"));
    }

    #[test]
    fn act_guards_and_first_match() {
        let mut w = W::new();
        w.trig(w.guard, 1, "e", 0, "smiles", "set a 1\nglobal a");
        w.trig(w.guard, 2, "e", 0, "smiles", "set b 1\nglobal b");
        // The actor's own act does not trigger it.
        act_mtrigger(
            &mut w.h,
            w.guard,
            "x smiles.\r\n",
            Some(w.guard),
            None,
            None,
            None,
            None,
        );
        assert_eq!(w.g(w.guard, "a"), None);
        w.charm(w.guard);
        act_mtrigger(
            &mut w.h,
            w.guard,
            "x smiles.\r\n",
            Some(w.alice),
            None,
            None,
            None,
            None,
        );
        assert_eq!(w.g(w.guard, "a"), None);
        w.h.set_num(w.guard, "aff:22", 0);
        // A sleeping mob still sees it: there is no AWAKE test.
        w.sleep(w.guard);
        act_mtrigger(
            &mut w.h,
            w.guard,
            "x smiles.\r\n",
            Some(w.alice),
            None,
            None,
            None,
            None,
        );
        assert_eq!(w.g(w.guard, "a").as_deref(), Some("1"));
        assert_eq!(w.g(w.guard, "b"), None);
        // An empty argument is logged and skipped.
        let mut w = W::new();
        w.trig(w.guard, 3, "e", 0, "", RAN);
        w.trig(w.guard, 4, "e", 0, "smiles", "set b 1\nglobal b");
        act_mtrigger(
            &mut w.h,
            w.guard,
            "x smiles.\r\n",
            Some(w.alice),
            None,
            None,
            None,
            None,
        );
        assert_eq!(w.g(w.guard, "b").as_deref(), Some("1"));
        assert_eq!(
            w.h.effects,
            ["mudlog 2 31: SYSERR: Act Trigger #3 has no text argument!"]
        );
    }

    #[test]
    fn fight_sets_the_opponent_as_actor() {
        let mut w = W::new();
        w.trig(
            w.guard,
            1,
            "k",
            50,
            "",
            "global actor\nset ran 1\nglobal ran",
        );
        w.trig(w.guard, 2, "k", 100, "", "set second 1\nglobal second");
        // Not fighting: nothing, and no die.
        w.rolls(&[1]);
        fight_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
        assert_eq!(w.h.rolls.len(), 1);
        w.h.set_link(w.guard, "fighting", Some(w.alice));
        // First loses its roll, second wins; only one runs.
        w.rolls(&[60, 5]);
        fight_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
        assert_eq!(w.g(w.guard, "second").as_deref(), Some("1"));
        w.rolls(&[50]);
        fight_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        w.charm(w.guard);
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        fight_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
    }

    #[test]
    fn hitprcnt_tests_the_percentage_not_a_roll() {
        let mut w = W::new();
        w.trig(w.guard, 1, "l", 30, "", "set a 1\nglobal a");
        w.trig(w.guard, 2, "l", 50, "", "global actor\nset b 1\nglobal b");
        w.h.set_num(w.guard, "max_hit", 100);
        w.h.set_num(w.guard, "hit", 40);
        w.rolls(&[100]);
        // Not fighting: nothing.
        hitprcnt_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.g(w.guard, "b"), None);
        w.h.set_link(w.guard, "fighting", Some(w.alice));
        // 40% is above 30, at most 50.
        hitprcnt_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.g(w.guard, "a"), None);
        assert_eq!(w.g(w.guard, "b").as_deref(), Some("1"));
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        assert_eq!(w.h.rolls.len(), 1);
        // 51% is above both.
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        w.h.set_num(w.guard, "hit", 51);
        hitprcnt_mtrigger(&mut w.h, w.guard);
        assert!(w.g(w.guard, "b").is_none());
        // Integer division: 1/3 is 33%, within a narg of 33.
        let mut w = W::new();
        w.trig(w.guard, 1, "l", 33, "", RAN);
        w.h.set_num(w.guard, "max_hit", 3);
        w.h.set_num(w.guard, "hit", 1);
        w.h.set_link(w.guard, "fighting", Some(w.alice));
        hitprcnt_mtrigger(&mut w.h, w.guard);
        assert!(w.ran(w.guard));
        // No maximum: no trigger.
        let mut w = W::new();
        w.trig(w.guard, 1, "l", 100, "", RAN);
        w.h.set_link(w.guard, "fighting", Some(w.alice));
        hitprcnt_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
    }

    #[test]
    fn receive_reports_what_the_script_did() {
        let mut w = W::new();
        let ball = w.obj(99, "a ball");
        w.h.obj_to_char(ball, w.alice);
        w.trig(
            w.guard,
            1,
            "j",
            100,
            "",
            "global actor\nglobal object\nreturn 1",
        );
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 1);
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        assert_eq!(w.g(w.guard, "object"), Some(id(ball)));
    }

    #[test]
    fn receive_zero_from_the_script_refuses() {
        let mut w = W::new();
        let ball = w.obj(99, "a ball");
        w.h.obj_to_char(ball, w.alice);
        w.trig(w.guard, 1, "j", 100, "", "return 0");
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 0);
    }

    #[test]
    fn receive_refuses_when_the_world_changed_under_it() {
        let setup = |body: &str| {
            let mut w = W::new();
            let ball = w.obj(99, "a ball");
            w.h.obj_to_char(ball, w.alice);
            w.trig(w.guard, 1, "j", 100, "", body);
            (w, ball)
        };
        // Purged by the script.
        let (mut w, ball) = setup(&format!("%purge% {}", id(30_000_001)));
        assert_eq!(ball, 30_000_001);
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 0);
        assert!(!w.h.exists(ball));
        // No longer carried by the giver.
        let (mut w, ball) = setup("return 1");
        w.h.obj_to_char(ball, w.guard);
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 0);
        // The giver died.
        let (mut w, ball) = setup("return 1");
        w.h.extract_char(w.alice);
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 0);
        // The mob died.
        let (mut w, ball) = setup("return 1");
        w.h.extract_char(w.guard);
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 0);
    }

    #[test]
    fn receive_rolls_and_defaults_to_one() {
        let mut w = W::new();
        let ball = w.obj(99, "a ball");
        w.h.obj_to_char(ball, w.alice);
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 1);
        w.trig(w.guard, 1, "j", 40, "", "return 0");
        w.rolls(&[41]);
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 1);
        w.charm(w.guard);
        w.rolls(&[1]);
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 1);
        assert_eq!(w.h.rolls.len(), 1);
    }

    /// tbaMUD trigger 143 ("Questmaster Receive"), `0 j 100`, verbatim.
    const TRIG_143: &str = r#"* By Rumble of The Builder Academy    tbamud.com 9091
* Part of a timed quest to kill a mob or find an object. Trigs 138-144.
* Check if they are on the quest.
if !%actor.varexists(on_quest_zone_1)%
  say You are not even on a quest %actor.name%.
  * Stop the trig and prevent the item from being given to the mob.
  halt
end
*
wait 1 sec
* If they had the head or the token.
if %object.vnum% == 15 || %object.vnum% == 17
  rdelete on_quest_zone_1 %actor.id%
  say Well done, %actor.name%.
  * Give them 50 gold and experience. Delete the on quest variable and purge.
  nop %actor.exp(50)%
  nop %actor.gold(50)%
  %purge% %object%
  * Reward them with 1 questpoint. Cheap I know but these quests are not hard.
  nop %actor.questpoints(1)%
  say you have earned 50 gold, 50 xp, 1 questpoint %actor.name%.
else
  say I don't want that!
  * Don't accept the object to prevent overloading the mob with junk.
  return 0.
end"#;

    #[test]
    fn corpus_questmaster_refuses_when_not_on_a_quest() {
        let mut w = W::new();
        let ball = w.obj(99, "a ball");
        w.h.obj_to_char(ball, w.alice);
        let t = w.trig(w.guard, 143, "j", 100, "", TRIG_143);
        assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, ball), 1);
        assert_eq!(w.say_log(), ["say You are not even on a quest Alice."]);
        // `halt` stops the trigger before the `wait`.
        assert!(w.h.waits.is_empty());
        assert!(w.h.script(w.guard).unwrap().trigger(t).is_some());
    }

    #[test]
    fn corpus_questmaster_pays_for_the_head_and_refuses_junk() {
        for (vnum, name, rewarded) in [(15, "a head", true), (99, "a ball", false)] {
            let mut w = W::new();
            let item = w.obj(vnum, name);
            w.h.obj_to_char(item, w.alice);
            // Alice is on the quest.
            w.h.ensure_script(w.alice)
                .unwrap()
                .global_vars
                .add_var("on_quest_zone_1", "1", 0);
            let t = w.trig(w.guard, 143, "j", 100, "", TRIG_143);
            // The branch before the `wait 1 sec` is skipped; the script waits.
            assert_eq!(receive_mtrigger(&mut w.h, w.guard, w.alice, item), 1);
            assert!(w.h.mud_commands.is_empty());
            assert_eq!(w.h.waits, [(w.guard, t, 10)]);
            crate::driver::resume(&mut w.h, w.guard, t);
            if rewarded {
                assert_eq!(
                    w.say_log(),
                    [
                        "say Well done, Alice.",
                        "say you have earned 50 gold, 50 xp, 1 questpoint Alice."
                    ]
                );
                // `rdelete` took the quest variable, `%purge%` the head.
                assert!(
                    w.h.script(w.alice)
                        .unwrap()
                        .global_vars
                        .get("on_quest_zone_1")
                        .is_none()
                );
                assert!(!w.h.exists(item));
                assert_eq!(w.h.num(w.alice, "exp"), 50);
                assert_eq!(w.h.num(w.alice, "gold"), 50);
                assert_eq!(w.h.num(w.alice, "qp"), 1);
            } else {
                assert_eq!(w.say_log(), ["say I don't want that!"]);
                assert!(w.h.exists(item));
                assert_eq!(w.h.num(w.alice, "exp"), 0);
            }
        }
    }

    #[test]
    fn death_passes_the_killer_and_the_result() {
        let mut w = W::new();
        assert_eq!(death_mtrigger(&mut w.h, w.guard, Some(w.alice)), 1);
        w.trig(w.guard, 1, "f", 100, "", "global actor\nreturn 0");
        assert_eq!(death_mtrigger(&mut w.h, w.guard, Some(w.alice)), 0);
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        // No killer: no actor variable.
        assert_eq!(death_mtrigger(&mut w.h, w.guard, None), 0);
        assert_eq!(w.g(w.guard, "actor"), None);
        w.rolls(&[1]);
        w.charm(w.guard);
        assert_eq!(death_mtrigger(&mut w.h, w.guard, None), 1);
        assert_eq!(w.h.rolls.len(), 1);
    }

    #[test]
    fn load_runs_one_trigger_and_prunes_the_prototype_on_the_depth_error() {
        let mut w = W::new();
        w.trig(w.guard, 1, "n", 20, "", "set a 1\nglobal a");
        w.trig(w.guard, 2, "n", 100, "", RAN);
        w.trig(w.guard, 3, "n", 100, "", "set c 1\nglobal c");
        w.rolls(&[50, 50]);
        load_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.g(w.guard, "a"), None);
        assert!(w.ran(w.guard));
        assert_eq!(w.g(w.guard, "c"), None);
        assert!(w.h.effects.is_empty());
        // Past the recursion cap the script driver reports the error code and
        // the prototype loses its script.
        let mut w = W::new();
        w.trig(w.guard, 1, "n", 100, "", RAN);
        w.h.vm.depth = MAX_SCRIPT_DEPTH + 1;
        load_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.h.effects, [format!("free_proto_script {}", w.guard)]);
        // Mobs with no script, or no load trigger, are left alone.
        let mut w = W::new();
        load_mtrigger(&mut w.h, w.guard);
        assert!(w.h.effects.is_empty());
    }

    #[test]
    fn cast_names_the_spell_and_returns_the_script_result() {
        let mut w = W::new();
        w.h.globals
            .texts
            .insert("spell:5".to_owned(), "magic missile".to_owned());
        w.h.globals
            .texts
            .insert("spell:7".to_owned(), "fireball".to_owned());
        w.trig(
            w.guard,
            1,
            "p",
            50,
            "",
            "global actor\nglobal spell\nglobal spellname\nreturn 0",
        );
        assert_eq!(cast_mtrigger(&mut w.h, w.alice, Some(w.guard), 5), 0);
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        assert_eq!(w.g(w.guard, "spell").as_deref(), Some("5"));
        assert_eq!(w.g(w.guard, "spellname").as_deref(), Some("magic missile"));
        // A hole in the table, the top, and beyond it.
        cast_mtrigger(&mut w.h, w.alice, Some(w.guard), 6);
        assert_eq!(w.g(w.guard, "spellname").as_deref(), Some("!UNUSED!"));
        cast_mtrigger(&mut w.h, w.alice, Some(w.guard), TOP_SPELL_DEFINE);
        assert_eq!(w.g(w.guard, "spellname").as_deref(), Some("!UNUSED!"));
        cast_mtrigger(&mut w.h, w.alice, Some(w.guard), TOP_SPELL_DEFINE + 1);
        assert_eq!(w.g(w.guard, "spellname").as_deref(), Some("UNDEFINED"));
        cast_mtrigger(&mut w.h, w.alice, Some(w.guard), -1);
        assert_eq!(w.g(w.guard, "spellname").as_deref(), Some("UNUSED"));
        // No target, charmed target, no roll.
        assert_eq!(cast_mtrigger(&mut w.h, w.alice, None, 5), 1);
        w.charm(w.guard);
        assert_eq!(cast_mtrigger(&mut w.h, w.alice, Some(w.guard), 5), 1);
        w.h.set_num(w.guard, "aff:22", 0);
        w.rolls(&[51]);
        assert_eq!(cast_mtrigger(&mut w.h, w.alice, Some(w.guard), 5), 1);
    }

    #[test]
    fn damage_lets_the_script_change_the_amount() {
        let mut w = W::new();
        w.h.globals
            .texts
            .insert("spell:3".to_owned(), "burning hands".to_owned());
        // No script, no victim: unchanged.
        assert_eq!(damage_mtrigger(&mut w.h, w.alice, Some(w.guard), 12, 3), 12);
        assert_eq!(damage_mtrigger(&mut w.h, w.alice, None, 12, 3), 12);
        w.trig(
            w.guard,
            1,
            "s",
            100,
            "",
            "global actor\nglobal victim\nglobal damdealt\nglobal attacktype\nset ran 1\nglobal ran",
        );
        // The script does not `return`: the default is the damage.
        assert_eq!(damage_mtrigger(&mut w.h, w.alice, Some(w.guard), 12, 3), 12);
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        assert_eq!(w.g(w.guard, "victim"), Some(id(w.guard)));
        assert_eq!(w.g(w.guard, "damdealt").as_deref(), Some("12"));
        assert_eq!(w.g(w.guard, "attacktype").as_deref(), Some("burning hands"));
        let mut w = W::new();
        w.trig(w.guard, 1, "s", 100, "", "return 0");
        assert_eq!(damage_mtrigger(&mut w.h, w.alice, Some(w.guard), 12, 3), 0);
        let mut w = W::new();
        w.trig(w.guard, 1, "s", 100, "", "return -1");
        assert_eq!(damage_mtrigger(&mut w.h, w.alice, Some(w.guard), 12, 3), -1);
        // Charmed, or a lost roll, leaves the damage alone.
        w.charm(w.guard);
        assert_eq!(damage_mtrigger(&mut w.h, w.alice, Some(w.guard), 12, 3), 12);
        w.h.set_num(w.guard, "aff:22", 0);
        let mut w2 = W::new();
        w2.trig(w2.guard, 1, "s", 10, "", "return 0");
        w2.rolls(&[11]);
        assert_eq!(
            damage_mtrigger(&mut w2.h, w2.alice, Some(w2.guard), 12, 3),
            12
        );
    }

    /// tbaMUD trigger 3000 ("Mage Guildguard"), `0 q 100`, verbatim.
    const TRIG_3000: &str = r#"* Check the direction the player must go to enter the guild.
if %direction% == south
  * Stop them if they are not the appropriate class.
  if %actor.class% != Magic User
    return 0
    %send% %actor% The guard humiliates you, and blocks your way.
    %echoaround% %actor% The guard humiliates %actor.name%, and blocks %actor.hisher% way.
  end
end"#;

    #[test]
    fn corpus_guild_guard_blocks_the_wrong_class() {
        let mut w = W::new();
        let bob = w.h.add_player("Bob");
        w.h.set_num(bob, "desc", 1);
        w.h.set_num(bob, "pos", 8);
        w.h.char_to_room(bob, w.room);
        // Alice is a female Cleric.
        w.h.set_num(w.alice, "sex", 2);
        w.trig(w.guard, 3000, "q", 100, "", TRIG_3000);
        // Alice leaving south (direction 2): refused, told so, and the room
        // sees it (`%echoaround%` skips her).
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 2), 0);
        assert_eq!(
            w.h.effects,
            [
                format!(
                    "send {}: The guard humiliates you, and blocks your way.\n\r",
                    w.alice
                ),
                format!("send {bob}: The guard humiliates Alice, and blocks her way.\n\r"),
            ]
        );
        // Leaving north: not the guarded way.
        w.h.effects.clear();
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 1);
        assert!(w.h.effects.is_empty());
        // A Magic User walks through.
        w.h.set_num(w.alice, "class", 0);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 2), 1);
        assert!(w.h.effects.is_empty());
    }

    #[test]
    fn leave_uses_the_plain_direction_and_returns_the_first_run() {
        let mut w = W::new();
        let clerk = w.mob(3025, "the clerk");
        // Room order [clerk, guard, alice].
        w.trig(
            clerk,
            1,
            "q",
            100,
            "",
            "global direction\nglobal actor\nreturn 0",
        );
        w.trig(w.guard, 2, "q", 100, "", RAN);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 3), 0);
        assert_eq!(w.g(clerk, "direction").as_deref(), Some("west"));
        assert_eq!(w.g(clerk, "actor"), Some(id(w.alice)));
        // The walk ended with the first trigger that ran.
        assert!(!w.ran(w.guard));
        leave_mtrigger(&mut w.h, w.alice, -1);
        assert_eq!(w.g(clerk, "direction").as_deref(), Some("none"));
    }

    #[test]
    fn leave_guards_and_roll_order() {
        let mut w = W::new();
        let t = w.trig(w.guard, 1, "q", 50, "", "return 0");
        // Cannot see the actor.
        w.blind(w.guard, w.alice);
        w.rolls(&[1]);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 1);
        assert_eq!(w.h.rolls.len(), 1);
        w.h.entity_mut(w.guard).unwrap().lists.clear();
        // Busy.
        w.busy(w.guard, t);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 1);
        assert_eq!(w.h.rolls.len(), 1);
        w.h.script_mut(w.guard)
            .unwrap()
            .trigger_mut(t)
            .unwrap()
            .run
            .depth = 0;
        // Lost roll falls through to the end: 1.
        w.rolls(&[51]);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 1);
        w.rolls(&[50]);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 0);
        // Asleep, fighting, charmed, the actor itself, and invalid targets.
        w.sleep(w.guard);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 1);
        w.h.set_num(w.guard, "pos", 8);
        assert_eq!(leave_mtrigger(&mut w.h, w.guard, 0), 1);
        w.h.set_num(w.alice, "dg_invalid", 1);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 1);
    }

    #[test]
    fn door_sets_cmd_direction_and_actor() {
        let mut w = W::new();
        w.trig(
            w.guard,
            1,
            "r",
            100,
            "",
            "global cmd\nglobal direction\nglobal actor\nreturn 0",
        );
        assert_eq!(door_mtrigger(&mut w.h, w.alice, 2, 1), 0);
        assert_eq!(w.g(w.guard, "cmd").as_deref(), Some("unlock"));
        assert_eq!(w.g(w.guard, "direction").as_deref(), Some("east"));
        assert_eq!(w.g(w.guard, "actor"), Some(id(w.alice)));
        for (subcmd, name) in [
            (0, "open"),
            (1, "close"),
            (3, "lock"),
            (4, "pick"),
            (5, ""),
            (-1, ""),
        ] {
            door_mtrigger(&mut w.h, w.alice, subcmd, 9);
            assert_eq!(w.g(w.guard, "cmd").as_deref(), Some(name));
        }
        door_mtrigger(&mut w.h, w.alice, 0, 12);
        assert_eq!(w.g(w.guard, "direction").as_deref(), Some("none"));
        // No valid_dg_target test: an invalid actor still triggers it.
        w.h.set_num(w.alice, "dg_invalid", 1);
        assert_eq!(door_mtrigger(&mut w.h, w.alice, 0, 0), 0);
        // But sight, sleep, and the actor itself still matter.
        w.blind(w.guard, w.alice);
        assert_eq!(door_mtrigger(&mut w.h, w.alice, 0, 0), 1);
    }

    #[test]
    fn time_compares_the_hour_without_a_roll() {
        let mut w = W::new();
        w.trig(w.guard, 1, "t", 6, "", "global time\nset ran 1\nglobal ran");
        w.rolls(&[100]);
        w.h.time.hours = 7;
        time_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
        w.h.time.hours = 6;
        time_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.g(w.guard, "time").as_deref(), Some("6"));
        assert_eq!(w.h.rolls.len(), 1);
        w.h.script_mut(w.guard).unwrap().global_vars.clear();
        w.charm(w.guard);
        time_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
    }

    // ---- object triggers ---------------------------------------------------------

    #[test]
    fn object_random_and_time() {
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "b", 25, "", RAN);
        w.rolls(&[26]);
        random_otrigger(&mut w.h, orb);
        assert!(!w.ran(orb));
        w.rolls(&[25]);
        random_otrigger(&mut w.h, orb);
        assert!(w.ran(orb));

        let clock = w.obj(11, "a clock");
        w.trig(clock, 2, "t", 12, "", "global time");
        w.rolls(&[1]);
        w.h.time.hours = 12;
        time_otrigger(&mut w.h, clock);
        assert_eq!(w.g(clock, "time").as_deref(), Some("12"));
        assert_eq!(w.h.rolls.len(), 1);
        w.h.time.hours = 13;
        w.h.script_mut(clock).unwrap().global_vars.clear();
        time_otrigger(&mut w.h, clock);
        assert_eq!(w.g(clock, "time"), None);
    }

    #[test]
    fn timer_runs_every_timer_trigger_without_a_roll() {
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "f", 1, "", "set a 1\nglobal a");
        w.trig(orb, 2, "b", 100, "", "set skip 1\nglobal skip");
        w.trig(orb, 3, "f", 1, "", "set c 1\nglobal c");
        let busy = w.trig(orb, 4, "f", 1, "", "set d 1\nglobal d");
        w.busy(orb, busy);
        w.rolls(&[100]);
        timer_otrigger(&mut w.h, orb);
        assert_eq!(w.g(orb, "a").as_deref(), Some("1"));
        assert_eq!(w.g(orb, "skip"), None);
        assert_eq!(w.g(orb, "c").as_deref(), Some("1"));
        assert_eq!(w.g(orb, "d"), None);
        assert_eq!(w.h.rolls.len(), 1);
    }

    #[test]
    fn timer_stops_when_the_script_purges_its_object() {
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "f", 1, "", "%purge% self");
        let victim = w.obj(11, "a bead");
        w.trig(victim, 9, "f", 1, "", RAN);
        w.h.obj_to_room(orb, w.room);
        w.h.obj_to_room(victim, w.room);
        timer_otrigger(&mut w.h, orb);
        assert!(!w.h.exists(orb));
        assert!(w.h.exists(victim));
    }

    #[test]
    fn get_returns_the_script_result_unless_the_world_changed() {
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        assert_eq!(get_otrigger(&mut w.h, orb, w.alice), 1);
        w.trig(orb, 1, "g", 100, "", "global actor\nreturn 0");
        assert_eq!(get_otrigger(&mut w.h, orb, w.alice), 0);
        assert_eq!(w.g(orb, "actor"), Some(id(w.alice)));
        // A script result of 1 stands, unless the actor is dead.
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "g", 100, "", "return 1");
        assert_eq!(get_otrigger(&mut w.h, orb, w.alice), 1);
        w.h.extract_char(w.alice);
        assert_eq!(get_otrigger(&mut w.h, orb, w.alice), 0);
        // Purged by its own script.
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "g", 100, "", "%purge% self");
        assert_eq!(get_otrigger(&mut w.h, orb, w.alice), 0);
        // The roll.
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "g", 10, "", "return 0");
        w.rolls(&[11]);
        assert_eq!(get_otrigger(&mut w.h, orb, w.alice), 1);
    }

    /// An object command trigger that records itself and returns `ret`.
    fn cmd_script(name: &str, ret: i32) -> String {
        format!("set {name} 1\nglobal {name}\nglobal arg\nglobal cmd\nglobal actor\nreturn {ret}")
    }

    #[test]
    fn cmd_otrig_checks_the_where_flag_and_the_command() {
        let mut w = W::new();
        let ball = w.obj(47, "a ball");
        // The header of tbaMUD trigger 6 (`1 c 2`, `shake`) with a stand-in
        // body that records the variables; the real script is
        // `corpus_magic_eight_ball_answers_the_one_who_shakes_it`.
        w.trig(
            ball,
            6,
            "c",
            i32::from(OCMD_INVEN as i16),
            "shake",
            &cmd_script("ran", 1),
        );
        assert_eq!(
            cmd_otrig(&mut w.h, ball, w.alice, "shake", "  ball", OCMD_ROOM),
            0
        );
        assert!(!w.ran(ball));
        assert_eq!(
            cmd_otrig(&mut w.h, ball, w.alice, "shak", "ball", OCMD_INVEN),
            0
        );
        assert_eq!(
            cmd_otrig(&mut w.h, ball, w.alice, "Shake", "  ball", OCMD_INVEN),
            1
        );
        assert_eq!(w.g(ball, "arg").as_deref(), Some("ball"));
        assert_eq!(w.g(ball, "cmd").as_deref(), Some("Shake"));
        assert_eq!(w.g(ball, "actor"), Some(id(w.alice)));
        // No such object: nothing.
        assert_eq!(
            cmd_otrig(&mut w.h, 30_000_999, w.alice, "shake", "", OCMD_INVEN),
            0
        );
    }

    /// tbaMUD trigger 6 ("Obj Command Magic Eight Ball"), `1 c 2` `shake`,
    /// verbatim.
    const TRIG_6: &str = r#"* By Rumble of The Builder Academy    tbamud.com 9091
* Numeric Arg: 2 means in character's carried inventory.
* Command trigs do not work for level 33 and above.
* There are 20 possible answers that the Magic Eight Ball can give.
* Of these, nine are full positive, two are full negative, one is
* mostly positive, three are mostly negative, and five are abstentions.
*
* Check arguments if they match. /= checks abbreviations.
set argcheck 'ball 'eightball
if %argcheck.contains('%arg%)%
  * Echo text to everyone else in the room and the actor.
  %echoaround% %actor% %actor.name% shakes %self.shortdesc% vigorously.
  %send% %actor% You shake %self.shortdesc% vigorously.
  * Use a switch to choose a random response (1-20).
  switch %random.20%
    * Send the answer! %self% is the 8ball, or whatever the trig is attached to.
    * Only the actor sees the answer.
    * Case is what we are trying to match. Does %random.20% == 1?
    case 1
      %send% %actor% %self.shortdesc% reveals the answer: Outlook Good
      * We are done with this case so check the next one.
    break
    case 2
      %send% %actor% %self.shortdesc% reveals the answer: Outlook Not So Good
    break
    case 3
      %send% %actor% %self.shortdesc% reveals the answer: My Reply Is No
    break
    case 4
      %send% %actor% %self.shortdesc% reveals the answer: Don't Count On It
    break
    case 5
      %send% %actor% %self.shortdesc% reveals the answer: You May Rely On It
    break
    case 6
      %send% %actor% %self.shortdesc% reveals the answer: Ask Again Later
    break
    case 7
      %send% %actor% %self.shortdesc% reveals the answer: Most Likely
    break
    case 8
      %send% %actor% %self.shortdesc% reveals the answer: Cannot Predict Now
    break
    case 9
      %send% %actor% %self.shortdesc% reveals the answer: Yes
    break
    case 10
      %send% %actor% %self.shortdesc% reveals the answer: Yes, definitely
    break
    case 11
      %send% %actor% %self.shortdesc% reveals the answer: Better Not Tell You Now
    break
    case 12
      %send% %actor% %self.shortdesc% reveals the answer: It Is Certain
    break
    case 13
      %send% %actor% %self.shortdesc% reveals the answer: Very Doubtful
    break
    case 14
      %send% %actor% %self.shortdesc% reveals the answer: It Is Decidedly So
    break
    case 15
      %send% %actor% %self.shortdesc% reveals the answer: Concentrate And Ask Again
    break
    case 16
      %send% %actor% %self.shortdesc% reveals the answer: Signs Point To Yes
    break
    case 17
      %send% %actor% %self.shortdesc% reveals the answer: My Sources Say No
    break
    case 18
      %send% %actor% %self.shortdesc% reveals the answer: Without A Doubt
    break
    case 19
      %send% %actor% %self.shortdesc% reveals the answer: Reply Hazy, Try Again
    break
    case 20
      %send% %actor% %self.shortdesc% reveals the answer: As I See It, Yes
    break
    * Every switch should have a default. A catch-all if the cases do not match.
    default
      %send% %actor% %self.shortdesc% explodes since your question is unanswerable.
    break
    * Every switch must have a done! Just like every if needs an end!
  done
  * The actor didn't use the command shake with arg ball or eightball.
else
  * Return 0 allows the command to continue through to the MUD. The player will
  * get the Huh!?! response or the shake social if you have one.
  return 0
end"#;

    #[test]
    fn corpus_magic_eight_ball_answers_the_one_who_shakes_it() {
        let mut w = W::new();
        let bob = w.h.add_player("Bob");
        w.h.set_num(bob, "desc", 1);
        w.h.set_num(bob, "pos", 8);
        w.h.char_to_room(bob, w.room);
        let ball = w.obj(47, "a ball");
        w.h.obj_to_char(ball, w.alice);
        w.trig(ball, 6, "c", OCMD_INVEN, "shake", TRIG_6);
        let alice = w.alice;
        for arg in ["ball", "eightball"] {
            w.h.effects.clear();
            w.rolls(&[7]);
            assert_eq!(
                cmd_otrig(&mut w.h, ball, alice, "shake", arg, OCMD_INVEN),
                1
            );
            assert_eq!(
                w.h.effects,
                [
                    format!("send {bob}: Alice shakes a ball vigorously.\n\r"),
                    format!("send {alice}: You shake a ball vigorously.\n\r"),
                    format!("send {alice}: a ball reveals the answer: Most Likely\n\r"),
                ],
                "{arg}"
            );
        }
        // Any other argument lets the command through to the game.
        w.h.effects.clear();
        assert_eq!(
            cmd_otrig(&mut w.h, ball, alice, "shake", "tree", OCMD_INVEN),
            0
        );
        assert!(w.h.effects.is_empty());
        // Every one of the 20 rolls has its own answer.
        let mut answers = Vec::new();
        for roll in 1..=20 {
            w.h.effects.clear();
            w.rolls(&[roll]);
            cmd_otrig(&mut w.h, ball, alice, "shake", "ball", OCMD_INVEN);
            answers.push(w.h.effects.last().cloned().unwrap());
        }
        answers.sort();
        answers.dedup();
        assert_eq!(answers.len(), 20);
    }

    #[test]
    fn cmd_otrig_carries_on_after_a_zero_and_stops_after_a_purge() {
        let mut w = W::new();
        let ball = w.obj(47, "a ball");
        w.trig(ball, 1, "c", OCMD_ROOM, "*", &cmd_script("one", 0));
        w.trig(ball, 2, "c", OCMD_ROOM, "*", &cmd_script("two", 1));
        w.trig(ball, 3, "c", OCMD_ROOM, "*", &cmd_script("three", 1));
        assert_eq!(cmd_otrig(&mut w.h, ball, w.alice, "x", "", OCMD_ROOM), 1);
        assert!(w.g(ball, "one").is_some() && w.g(ball, "two").is_some());
        assert_eq!(w.g(ball, "three"), None);

        let mut w = W::new();
        let ball = w.obj(47, "a ball");
        w.trig(ball, 1, "c", OCMD_ROOM, "*", "return 0\n%purge% self");
        w.trig(ball, 2, "c", OCMD_ROOM, "*", "return 1");
        assert_eq!(cmd_otrig(&mut w.h, ball, w.alice, "x", "", OCMD_ROOM), 0);
        assert!(!w.h.exists(ball));
    }

    #[test]
    fn cmd_otrig_logs_an_empty_argument_only_for_a_matching_flag() {
        let mut w = W::new();
        let ball = w.obj(47, "a ball");
        w.trig(ball, 55, "c", OCMD_ROOM, "", RAN);
        assert_eq!(cmd_otrig(&mut w.h, ball, w.alice, "x", "", OCMD_INVEN), 0);
        assert!(w.h.effects.is_empty());
        assert_eq!(cmd_otrig(&mut w.h, ball, w.alice, "x", "", OCMD_ROOM), 0);
        assert_eq!(
            w.h.effects,
            ["mudlog 2 31: SYSERR: O-Command Trigger #55 has no text argument!"]
        );
        assert!(!w.ran(ball));
    }

    #[test]
    fn command_otrigger_walks_equipment_then_inventory_then_the_room() {
        let mut w = W::new();
        let worn = w.obj(1, "a ring");
        let held = w.obj(2, "a ball");
        let lying = w.obj(3, "a rock");
        w.h.equip_char(w.alice, worn, 2);
        w.h.obj_to_char(held, w.alice);
        w.h.obj_to_room(lying, w.room);
        w.trig(worn, 1, "c", OCMD_EQUIP, "*", &cmd_script("ran", 0));
        w.trig(held, 2, "c", OCMD_INVEN, "*", &cmd_script("ran", 0));
        w.trig(lying, 3, "c", OCMD_ROOM, "*", &cmd_script("ran", 1));
        // Each returns 0 until the room object claims it.
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", "y"), 1);
        assert!(w.ran(worn) && w.ran(held) && w.ran(lying));

        // The first to claim ends the walk.
        let mut w = W::new();
        let worn = w.obj(1, "a ring");
        let held = w.obj(2, "a ball");
        w.h.equip_char(w.alice, worn, 2);
        w.h.obj_to_char(held, w.alice);
        w.trig(worn, 1, "c", OCMD_EQUIP, "*", &cmd_script("ran", 1));
        w.trig(held, 2, "c", OCMD_INVEN, "*", &cmd_script("ran", 1));
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", "y"), 1);
        assert!(w.ran(worn));
        assert!(!w.ran(held));
    }

    #[test]
    fn command_otrigger_flags_select_where_an_object_counts() {
        let mut w = W::new();
        let worn = w.obj(1, "a ring");
        let held = w.obj(2, "a ball");
        let lying = w.obj(3, "a rock");
        w.h.equip_char(w.alice, worn, 17);
        w.h.obj_to_char(held, w.alice);
        w.h.obj_to_room(lying, w.room);
        // Each trigger is for a place its object is not in.
        w.trig(worn, 1, "c", OCMD_INVEN | OCMD_ROOM, "*", RAN);
        w.trig(held, 2, "c", OCMD_EQUIP | OCMD_ROOM, "*", RAN);
        w.trig(lying, 3, "c", OCMD_EQUIP | OCMD_INVEN, "*", RAN);
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", ""), 0);
        assert!(!w.ran(worn) && !w.ran(held) && !w.ran(lying));
        // The last wear position is reached.
        w.trig(worn, 4, "c", OCMD_EQUIP, "*", RAN);
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", ""), 1);
    }

    #[test]
    fn command_otrigger_skips_objects_scripts_purged_or_moved() {
        let mut w = W::new();
        let first = w.obj(1, "a ball");
        let second = w.obj(2, "a rock");
        let third = w.obj(3, "a coin");
        for o in [first, second, third] {
            w.h.obj_to_char(o, w.alice);
        }
        // Inventory order is [third, second, first] (obj_to_char pushes on
        // the front). Third purges second, then declines.
        w.trig(
            third,
            1,
            "c",
            OCMD_INVEN,
            "*",
            &format!("%purge% {}\nreturn 0", id(second)),
        );
        w.trig(second, 2, "c", OCMD_INVEN, "*", RAN);
        w.trig(
            first,
            3,
            "c",
            OCMD_INVEN,
            "*",
            "set ran 1\nglobal ran\nreturn 0",
        );
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", ""), 0);
        assert!(!w.h.exists(second));
        assert!(w.ran(first));

        // An object that left the actor before its turn is skipped
        // (OCMD_INVEN), as is one that left the room (OCMD_ROOM).
        let mut w = W::new();
        let ball = w.obj(1, "a ball");
        let rock = w.obj(2, "a rock");
        w.h.obj_to_char(ball, w.guard);
        w.h.obj_to_room(rock, w.room2);
        w.trig(ball, 1, "c", OCMD_INVEN, "*", RAN);
        w.trig(rock, 2, "c", OCMD_ROOM, "*", RAN);
        assert_eq!(
            command_otrigger_list(&mut w.h, vec![ball], w.alice, "x", "", OCMD_INVEN, None),
            0
        );
        assert_eq!(
            command_otrigger_list(
                &mut w.h,
                vec![rock],
                w.alice,
                "x",
                "",
                OCMD_ROOM,
                Some(w.room)
            ),
            0
        );
        assert!(!w.ran(ball) && !w.ran(rock));
        assert_eq!(
            command_otrigger_list(
                &mut w.h,
                vec![rock],
                w.alice,
                "x",
                "",
                OCMD_ROOM,
                Some(w.room2)
            ),
            1
        );
    }

    #[test]
    fn command_otrigger_needs_a_valid_target() {
        let mut w = W::new();
        let ball = w.obj(1, "a ball");
        w.h.obj_to_char(ball, w.alice);
        w.trig(ball, 1, "c", OCMD_INVEN, "*", "return 1");
        w.h.set_num(w.alice, "dg_invalid", 1);
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", ""), 0);
    }

    #[test]
    fn wear_returns_the_result_of_the_first_trigger_and_asks_no_roll() {
        let mut w = W::new();
        let ring = w.obj(1, "a ring");
        assert_eq!(wear_otrigger(&mut w.h, ring, w.alice, 3), 1);
        w.trig(ring, 1, "j", 1, "", "global actor\nreturn 0");
        w.trig(ring, 2, "j", 1, "", "set second 1\nglobal second");
        w.rolls(&[100]);
        assert_eq!(wear_otrigger(&mut w.h, ring, w.alice, 3), 0);
        assert_eq!(w.g(ring, "actor"), Some(id(w.alice)));
        assert_eq!(w.g(ring, "second"), None);
        assert_eq!(w.h.rolls.len(), 1);
        // Purged: 0 whatever the script returned.
        let mut w = W::new();
        let ring = w.obj(1, "a ring");
        w.trig(ring, 1, "j", 1, "", "%purge% self\nreturn 1");
        assert_eq!(wear_otrigger(&mut w.h, ring, w.alice, 3), 0);
        // Busy: skipped.
        let mut w = W::new();
        let ring = w.obj(1, "a ring");
        let t = w.trig(ring, 1, "j", 1, "", "return 0");
        w.busy(ring, t);
        assert_eq!(wear_otrigger(&mut w.h, ring, w.alice, 3), 1);
    }

    #[test]
    fn remove_tests_the_script_before_the_actor() {
        let mut w = W::new();
        let ring = w.obj(1, "a ring");
        assert_eq!(remove_otrigger(&mut w.h, ring, w.alice), 1);
        w.trig(ring, 1, "l", 1, "", "global actor\nreturn 0");
        assert_eq!(remove_otrigger(&mut w.h, ring, w.alice), 0);
        assert_eq!(w.g(ring, "actor"), Some(id(w.alice)));
        w.h.set_num(w.alice, "dg_invalid", 1);
        w.h.script_mut(ring).unwrap().global_vars.clear();
        assert_eq!(remove_otrigger(&mut w.h, ring, w.alice), 1);
        assert_eq!(w.g(ring, "actor"), None);
        // Purged.
        let mut w = W::new();
        let ring = w.obj(1, "a ring");
        w.trig(ring, 1, "l", 1, "", "%purge% self");
        assert_eq!(remove_otrigger(&mut w.h, ring, w.alice), 0);
    }

    #[test]
    fn drop_rolls_and_reports_a_purge() {
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.trig(rock, 1, "h", 30, "", "global actor\nreturn 0");
        w.rolls(&[31]);
        assert_eq!(drop_otrigger(&mut w.h, rock, w.alice), 1);
        w.rolls(&[30]);
        assert_eq!(drop_otrigger(&mut w.h, rock, w.alice), 0);
        assert_eq!(w.g(rock, "actor"), Some(id(w.alice)));
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.trig(rock, 1, "h", 100, "", "%purge% self\nreturn 1");
        assert_eq!(drop_otrigger(&mut w.h, rock, w.alice), 0);
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.trig(rock, 1, "h", 100, "", "return 1");
        assert_eq!(drop_otrigger(&mut w.h, rock, w.alice), 1);
    }

    #[test]
    fn give_wants_the_object_still_with_the_giver() {
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.h.obj_to_char(rock, w.alice);
        w.trig(
            rock,
            1,
            "i",
            100,
            "",
            "global actor\nglobal victim\nreturn 1",
        );
        assert_eq!(give_otrigger(&mut w.h, rock, w.alice, w.guard), 1);
        assert_eq!(w.g(rock, "actor"), Some(id(w.alice)));
        assert_eq!(w.g(rock, "victim"), Some(id(w.guard)));
        w.h.obj_to_char(rock, w.guard);
        assert_eq!(give_otrigger(&mut w.h, rock, w.alice, w.guard), 0);
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.h.obj_to_char(rock, w.alice);
        w.trig(rock, 1, "i", 100, "", "return 0");
        assert_eq!(give_otrigger(&mut w.h, rock, w.alice, w.guard), 0);
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.h.obj_to_char(rock, w.alice);
        w.trig(rock, 1, "i", 100, "", "%purge% self");
        assert_eq!(give_otrigger(&mut w.h, rock, w.alice, w.guard), 0);
    }

    #[test]
    fn object_load_prunes_the_prototype_on_the_depth_error() {
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.trig(rock, 1, "n", 50, "", RAN);
        w.rolls(&[51]);
        load_otrigger(&mut w.h, rock);
        assert!(!w.ran(rock));
        w.rolls(&[50]);
        load_otrigger(&mut w.h, rock);
        assert!(w.ran(rock));
        assert!(w.h.effects.is_empty());
        w.h.vm.depth = MAX_SCRIPT_DEPTH + 1;
        w.rolls(&[1]);
        load_otrigger(&mut w.h, rock);
        assert_eq!(w.h.effects, [format!("free_proto_script {rock}")]);
    }

    #[test]
    fn object_cast() {
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.h.globals
            .texts
            .insert("spell:2".to_owned(), "armor".to_owned());
        assert_eq!(cast_otrigger(&mut w.h, w.alice, None, 2), 1);
        w.trig(
            rock,
            1,
            "p",
            50,
            "",
            "global actor\nglobal spell\nglobal spellname\nreturn 0",
        );
        assert_eq!(cast_otrigger(&mut w.h, w.alice, Some(rock), 2), 0);
        assert_eq!(w.g(rock, "spell").as_deref(), Some("2"));
        assert_eq!(w.g(rock, "spellname").as_deref(), Some("armor"));
        assert_eq!(w.g(rock, "actor"), Some(id(w.alice)));
        w.rolls(&[51]);
        assert_eq!(cast_otrigger(&mut w.h, w.alice, Some(rock), 2), 1);
        // Charm is not a condition for objects; a missing script is 1.
        let other = w.obj(2, "a gem");
        assert_eq!(cast_otrigger(&mut w.h, w.alice, Some(other), 2), 1);
    }

    #[test]
    fn leave_otrigger_runs_every_trigger_of_every_object_in_the_room() {
        let mut w = W::new();
        let a = w.obj(1, "a rock");
        let b = w.obj(2, "a coin");
        let c = w.obj(3, "a gem");
        for o in [a, b, c] {
            w.h.obj_to_room(o, w.room);
        }
        w.trig(
            a,
            1,
            "q",
            100,
            "",
            "global direction\nglobal actor\nreturn 0",
        );
        w.trig(a, 2, "q", 100, "", "set second 1\nglobal second");
        w.trig(b, 3, "q", 100, "", "return 1");
        w.trig(c, 4, "b", 100, "", RAN);
        assert_eq!(leave_otrigger(&mut w.h, w.room, w.alice, 1), 0);
        assert_eq!(w.g(a, "direction").as_deref(), Some("east"));
        assert_eq!(w.g(a, "actor"), Some(id(w.alice)));
        assert_eq!(w.g(a, "second").as_deref(), Some("1"));
        assert!(!w.ran(c));
        // All 1: 1.
        w.h.script_mut(a).unwrap().triggers.clear();
        assert_eq!(leave_otrigger(&mut w.h, w.room, w.alice, -1), 1);
    }

    #[test]
    fn leave_otrigger_purge_ends_that_objects_triggers_only() {
        let mut w = W::new();
        let a = w.obj(1, "a rock");
        let b = w.obj(2, "a coin");
        w.h.obj_to_room(a, w.room);
        w.h.obj_to_room(b, w.room);
        w.trig(a, 1, "q", 100, "", "return 0\n%purge% self");
        w.trig(a, 2, "q", 100, "", RAN);
        w.trig(b, 3, "q", 100, "", RAN);
        assert_eq!(leave_otrigger(&mut w.h, w.room, w.alice, 0), 0);
        assert!(!w.h.exists(a));
        assert!(w.ran(b));
        // The roll is per trigger; a lost one runs nothing and is not an
        // error. Invalid targets are let through.
        let mut w = W::new();
        let a = w.obj(1, "a rock");
        w.h.obj_to_room(a, w.room);
        w.trig(a, 1, "q", 10, "", "return 0");
        w.rolls(&[11]);
        assert_eq!(leave_otrigger(&mut w.h, w.room, w.alice, 0), 1);
        w.rolls(&[10]);
        w.h.set_num(w.alice, "dg_invalid", 1);
        assert_eq!(leave_otrigger(&mut w.h, w.room, w.alice, 0), 1);
        assert_eq!(w.h.rolls.len(), 1);
    }

    #[test]
    fn consume_names_the_command() {
        let mut w = W::new();
        let pie = w.obj(1, "a pie");
        w.trig(pie, 1, "s", 1, "", "global command\nglobal actor\nreturn 0");
        for (cmd, name) in [
            (OCMD_EAT, "eat"),
            (OCMD_DRINK, "drink"),
            (OCMD_QUAFF, "quaff"),
        ] {
            w.h.script_mut(pie).unwrap().global_vars.clear();
            assert_eq!(consume_otrigger(&mut w.h, pie, w.alice, cmd), 0);
            assert_eq!(w.g(pie, "command").as_deref(), Some(name));
            assert_eq!(w.g(pie, "actor"), Some(id(w.alice)));
        }
        // An unknown command sets no variable.
        w.h.script_mut(pie).unwrap().global_vars.clear();
        consume_otrigger(&mut w.h, pie, w.alice, 9);
        assert_eq!(w.g(pie, "command"), None);
        // No roll; purged is 0 even for a script that returns 1.
        let mut w = W::new();
        let pie = w.obj(1, "a pie");
        w.trig(pie, 1, "s", 1, "", "%purge% self\nreturn 1");
        w.rolls(&[100]);
        assert_eq!(consume_otrigger(&mut w.h, pie, w.alice, OCMD_EAT), 0);
        assert_eq!(w.h.rolls.len(), 1);
        let mut w = W::new();
        let pie = w.obj(1, "a pie");
        assert_eq!(consume_otrigger(&mut w.h, pie, w.alice, OCMD_EAT), 1);
    }

    // ---- world triggers ------------------------------------------------------

    #[test]
    fn reset_random_and_time_for_rooms() {
        let mut w = W::new();
        w.trig(w.room, 1, "f", 40, "", "set r 1\nglobal r");
        w.rolls(&[41]);
        reset_wtrigger(&mut w.h, w.room);
        assert_eq!(w.g(w.room, "r"), None);
        w.rolls(&[40]);
        reset_wtrigger(&mut w.h, w.room);
        assert_eq!(w.g(w.room, "r").as_deref(), Some("1"));

        w.trig(w.room, 2, "b", 40, "", "set b 1\nglobal b");
        w.rolls(&[41]);
        random_wtrigger(&mut w.h, w.room);
        assert_eq!(w.g(w.room, "b"), None);
        w.rolls(&[40]);
        random_wtrigger(&mut w.h, w.room);
        assert_eq!(w.g(w.room, "b").as_deref(), Some("1"));

        w.trig(w.room, 3, "t", 3, "", "global time");
        w.rolls(&[1]);
        w.h.time.hours = 3;
        time_wtrigger(&mut w.h, w.room);
        assert_eq!(w.g(w.room, "time").as_deref(), Some("3"));
        assert_eq!(w.h.rolls.len(), 1);
    }

    #[test]
    fn enter_reverses_the_direction_and_returns_the_script_result() {
        let mut w = W::new();
        assert_eq!(enter_wtrigger(&mut w.h, w.room, w.alice, 0), 1);
        w.trig(
            w.room,
            1,
            "g",
            100,
            "",
            "global direction\nglobal actor\nreturn 0",
        );
        assert_eq!(enter_wtrigger(&mut w.h, w.room, w.alice, 1), 0);
        assert_eq!(w.g(w.room, "direction").as_deref(), Some("west"));
        assert_eq!(w.g(w.room, "actor"), Some(id(w.alice)));
        enter_wtrigger(&mut w.h, w.room, w.alice, -1);
        assert_eq!(w.g(w.room, "direction").as_deref(), Some("none"));
        // No AFF_CHARM test on the actor.
        w.charm(w.alice);
        w.rolls(&[1]);
        assert_eq!(enter_wtrigger(&mut w.h, w.room, w.alice, 0), 0);
        // Roll lost: 1, and the first-rolled trigger only.
        let mut w = W::new();
        w.trig(w.room, 1, "g", 10, "", "return 0");
        w.trig(w.room, 2, "g", 100, "", "return 1");
        w.rolls(&[50, 50]);
        assert_eq!(enter_wtrigger(&mut w.h, w.room, w.alice, 0), 1);
    }

    #[test]
    fn world_command_returns_the_first_matching_result_even_zero() {
        let mut w = W::new();
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "x", ""), 0);
        w.trig(w.room, 1, "c", 0, "dance", "set a 1\nglobal a\nreturn 0");
        w.trig(
            w.room,
            2,
            "c",
            0,
            "*",
            "global arg\nglobal cmd\nglobal actor\nreturn 1",
        );
        // The first matching trigger decides, even with 0.
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "dance", ""), 0);
        assert_eq!(w.g(w.room, "a").as_deref(), Some("1"));
        assert_eq!(w.g(w.room, "cmd"), None);
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "sing", "  loudly "), 1);
        assert_eq!(w.g(w.room, "arg").as_deref(), Some("loudly "));
        assert_eq!(w.g(w.room, "cmd").as_deref(), Some("sing"));
        assert_eq!(w.g(w.room, "actor"), Some(id(w.alice)));
        w.h.set_num(w.alice, "dg_invalid", 1);
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "sing", ""), 0);
    }

    #[test]
    fn world_command_logs_an_empty_argument_and_goes_on() {
        let mut w = W::new();
        w.trig(w.room, 66, "c", 0, "", RAN);
        w.trig(w.room, 67, "c", 0, "*", "return 1");
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "x", ""), 1);
        assert_eq!(
            w.h.effects,
            ["mudlog 2 31: SYSERR: W-Command Trigger #66 has no text argument!"]
        );
        assert!(!w.ran(w.room));
    }

    #[test]
    fn world_speech_takes_star_before_the_narg_rules() {
        let mut w = W::new();
        w.trig(w.room, 1, "d", 0, "*", "global speech\nglobal actor");
        speech_wtrigger(&mut w.h, w.alice, "anything at all");
        assert_eq!(w.g(w.room, "speech").as_deref(), Some("anything at all"));
        assert_eq!(w.g(w.room, "actor"), Some(id(w.alice)));

        let mut w = W::new();
        w.trig(w.room, 1, "d", 0, "open sesame", "set a 1\nglobal a");
        w.trig(w.room, 2, "d", 1, "sesame", "set b 1\nglobal b");
        w.trig(w.room, 3, "d", 1, "sesame", "set c 1\nglobal c");
        speech_wtrigger(&mut w.h, w.alice, "sesame");
        assert_eq!(w.g(w.room, "a"), None);
        assert_eq!(w.g(w.room, "b").as_deref(), Some("1"));
        // Only the first match runs.
        assert_eq!(w.g(w.room, "c"), None);
        speech_wtrigger(&mut w.h, w.alice, "say open sesame now");
        assert_eq!(w.g(w.room, "a").as_deref(), Some("1"));
        // The room does not care whether anyone is awake, or a valid target.
        let mut w = W::new();
        w.trig(w.room, 9, "d", 1, "", RAN);
        speech_wtrigger(&mut w.h, w.alice, "x");
        assert_eq!(
            w.h.effects,
            ["mudlog 2 31: SYSERR: W-Speech Trigger #9 has no text argument!"]
        );
    }

    #[test]
    fn world_drop_sets_actor_and_object() {
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.h.obj_to_char(rock, w.alice);
        assert_eq!(drop_wtrigger(&mut w.h, rock, w.alice), 1);
        w.trig(
            w.room,
            1,
            "h",
            100,
            "",
            "global actor\nglobal object\nreturn 1",
        );
        assert_eq!(drop_wtrigger(&mut w.h, rock, w.alice), 1);
        assert_eq!(w.g(w.room, "actor"), Some(id(w.alice)));
        assert_eq!(w.g(w.room, "object"), Some(id(rock)));
        // The script moved the object: refused.
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.h.obj_to_char(rock, w.alice);
        w.trig(
            w.room,
            1,
            "h",
            100,
            "",
            &format!("%purge% {}\nreturn 1", id(rock)),
        );
        assert_eq!(drop_wtrigger(&mut w.h, rock, w.alice), 0);
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.trig(w.room, 1, "h", 100, "", "return 1");
        // Not carried by the actor when the script is done.
        assert_eq!(drop_wtrigger(&mut w.h, rock, w.alice), 0);
        let mut w = W::new();
        let rock = w.obj(1, "a rock");
        w.h.obj_to_char(rock, w.alice);
        w.trig(w.room, 1, "h", 10, "", "return 0");
        w.rolls(&[11]);
        assert_eq!(drop_wtrigger(&mut w.h, rock, w.alice), 1);
    }

    #[test]
    fn world_cast_sets_what_it_is_given() {
        let mut w = W::new();
        w.h.globals
            .texts
            .insert("spell:4".to_owned(), "bless".to_owned());
        let rock = w.obj(1, "a rock");
        assert_eq!(cast_wtrigger(&mut w.h, w.alice, None, None, 4), 1);
        w.trig(
            w.room,
            1,
            "p",
            100,
            "",
            "global actor\nglobal victim\nglobal object\nglobal spell\nglobal spellname\nreturn 0",
        );
        assert_eq!(cast_wtrigger(&mut w.h, w.alice, None, None, 4), 0);
        assert_eq!(w.g(w.room, "victim"), None);
        assert_eq!(w.g(w.room, "object"), None);
        assert_eq!(w.g(w.room, "spellname").as_deref(), Some("bless"));
        assert_eq!(
            cast_wtrigger(&mut w.h, w.alice, Some(w.guard), Some(rock), 4),
            0
        );
        assert_eq!(w.g(w.room, "victim"), Some(id(w.guard)));
        assert_eq!(w.g(w.room, "object"), Some(id(rock)));
        assert_eq!(w.g(w.room, "spell").as_deref(), Some("4"));
        assert_eq!(w.g(w.room, "actor"), Some(id(w.alice)));
    }

    #[test]
    fn world_leave_tests_the_target_before_the_script() {
        let mut w = W::new();
        w.trig(
            w.room,
            1,
            "q",
            100,
            "",
            "global direction\nglobal actor\nreturn 0",
        );
        assert_eq!(leave_wtrigger(&mut w.h, w.room, w.alice, 3), 0);
        assert_eq!(w.g(w.room, "direction").as_deref(), Some("west"));
        assert_eq!(w.g(w.room, "actor"), Some(id(w.alice)));
        leave_wtrigger(&mut w.h, w.room, w.alice, 11);
        assert_eq!(w.g(w.room, "direction").as_deref(), Some("none"));
        w.h.set_num(w.alice, "dg_invalid", 1);
        assert_eq!(leave_wtrigger(&mut w.h, w.room, w.alice, 3), 1);
        // Another room's script is not this one's business.
        assert_eq!(leave_wtrigger(&mut w.h, w.room2, w.guard, 3), 1);
    }

    #[test]
    fn world_door_and_login() {
        let mut w = W::new();
        w.trig(
            w.room,
            1,
            "r",
            100,
            "",
            "global cmd\nglobal direction\nglobal actor\nreturn 0",
        );
        assert_eq!(door_wtrigger(&mut w.h, w.alice, 4, 5), 0);
        assert_eq!(w.g(w.room, "cmd").as_deref(), Some("pick"));
        assert_eq!(w.g(w.room, "direction").as_deref(), Some("down"));
        assert_eq!(w.g(w.room, "actor"), Some(id(w.alice)));
        door_wtrigger(&mut w.h, w.alice, 7, -1);
        assert_eq!(w.g(w.room, "cmd").as_deref(), Some(""));
        assert_eq!(w.g(w.room, "direction").as_deref(), Some("none"));
        // The room is the actor's, not an argument.
        w.h.char_to_room(w.alice, w.room2);
        assert_eq!(door_wtrigger(&mut w.h, w.alice, 0, 0), 1);

        let mut w = W::new();
        assert_eq!(login_wtrigger(&mut w.h, w.room, w.alice), 1);
        w.trig(w.room, 2, "s", 20, "", "global actor\nreturn 0");
        w.rolls(&[21]);
        assert_eq!(login_wtrigger(&mut w.h, w.room, w.alice), 1);
        w.rolls(&[20]);
        assert_eq!(login_wtrigger(&mut w.h, w.room, w.alice), 0);
        assert_eq!(w.g(w.room, "actor"), Some(id(w.alice)));
    }

    #[test]
    fn actors_with_no_room_are_skipped_not_crashed_on() {
        let mut w = W::new();
        w.trig(w.room, 1, "c", 0, "*", "return 1");
        w.h.char_from_room(w.alice);
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "x", ""), 0);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 0);
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", ""), 0);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 1);
        assert_eq!(door_mtrigger(&mut w.h, w.alice, 0, 0), 1);
        assert_eq!(door_wtrigger(&mut w.h, w.alice, 0, 0), 1);
        assert_eq!(cast_wtrigger(&mut w.h, w.alice, None, None, 1), 1);
        speech_mtrigger(&mut w.h, w.alice, "x");
        speech_wtrigger(&mut w.h, w.alice, "x");
        greet_memory_mtrigger(&mut w.h, w.alice);
        entry_memory_mtrigger(&mut w.h, w.alice);
        let rock = w.obj(1, "a rock");
        assert_eq!(drop_wtrigger(&mut w.h, rock, w.alice), 1);
    }

    // ---- the heartbeat sweeps -------------------------------------------------------

    /// A script that notes its owner in the wait queue, so the order of
    /// the sweep shows in `host.waits`.
    const WAITS: &str = "wait 1";

    #[test]
    fn random_sweep_visits_characters_then_objects_then_rooms() {
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        let t_orb = w.trig(orb, 1, "b", 100, "", WAITS);
        let t_room = w.trig(w.room, 2, "b", 100, "", WAITS);
        let t_guard = w.trig(w.guard, 3, "b", 100, "", WAITS);
        script_trigger_check(&mut w.h);
        let order: Vec<Uid> = w.h.waits.iter().map(|(owner, _, _)| *owner).collect();
        assert_eq!(order, [w.guard, orb, w.room]);
        let _ = (t_orb, t_room, t_guard);
    }

    #[test]
    fn random_sweep_needs_an_occupied_zone_or_the_global_bit() {
        let mut w = W::new();
        w.h.set_num(w.room, "zone", 30);
        w.h.set_num(w.alice, "desc", 0);
        let orb = w.obj(10, "an orb");
        w.trig(w.guard, 3, "b", 100, "", RAN);
        w.trig(w.room, 2, "b", 100, "", RAN);
        w.trig(orb, 1, "b", 100, "", RAN);
        script_trigger_check(&mut w.h);
        // The mob and the room are in an empty zone; the object has no zone
        // test.
        assert!(!w.ran(w.guard) && !w.ran(w.room));
        assert!(w.ran(orb));
        // The GLOBAL bit ('a') overrides.
        w.trig(w.guard, 4, "a", 100, "", "set g 1\nglobal g");
        w.trig(w.room, 5, "a", 100, "", "set g 1\nglobal g");
        script_trigger_check(&mut w.h);
        assert!(w.ran(w.guard) && w.ran(w.room));
        // An occupied zone needs no bit.
        let mut w = W::new();
        w.h.set_num(w.room, "zone", 30);
        w.trig(w.guard, 3, "b", 100, "", RAN);
        script_trigger_check(&mut w.h);
        assert!(w.ran(w.guard));
    }

    #[test]
    fn random_sweep_handles_characters_in_no_room() {
        let mut w = W::new();
        w.h.char_from_room(w.guard);
        w.trig(w.guard, 3, "b", 100, "", RAN);
        script_trigger_check(&mut w.h);
        // No room means no zone to ask: not without the GLOBAL bit.
        assert!(!w.ran(w.guard));
        w.trig(w.guard, 4, "a", 100, "", "set g 1\nglobal g");
        script_trigger_check(&mut w.h);
        assert!(w.ran(w.guard));
    }

    #[test]
    fn random_sweep_skips_what_an_earlier_script_purged() {
        let mut w = W::new();
        let first = w.obj(10, "an orb");
        let second = w.obj(11, "a bead");
        // object_list is newest first: [second, first].
        w.trig(second, 1, "b", 100, "", &format!("%purge% {}", id(first)));
        w.trig(first, 2, "b", 100, "", RAN);
        script_trigger_check(&mut w.h);
        assert!(!w.h.exists(first));
        assert!(w.h.exists(second));
    }

    #[test]
    fn time_sweep_matches_the_hour_in_every_list() {
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(w.guard, 3, "t", 5, "", WAITS);
        w.trig(orb, 1, "t", 5, "", WAITS);
        w.trig(w.room, 2, "t", 5, "", WAITS);
        let other = w.mob(3026, "a cat");
        w.trig(other, 4, "t", 6, "", WAITS);
        w.h.time.hours = 5;
        check_time_triggers(&mut w.h);
        let order: Vec<Uid> = w.h.waits.iter().map(|(owner, _, _)| *owner).collect();
        assert_eq!(order, [w.guard, orb, w.room]);
    }

    #[test]
    fn time_sweep_zone_rules() {
        let mut w = W::new();
        w.h.set_num(w.room, "zone", 7);
        w.h.set_num(w.alice, "desc", 0);
        w.trig(w.guard, 3, "t", 5, "", RAN);
        w.trig(w.room, 2, "t", 5, "", RAN);
        w.h.time.hours = 5;
        check_time_triggers(&mut w.h);
        assert!(!w.ran(w.guard) && !w.ran(w.room));
        w.trig(w.guard, 4, "a", 5, "", "set g 1\nglobal g");
        w.trig(w.room, 5, "a", 5, "", "set g 1\nglobal g");
        check_time_triggers(&mut w.h);
        assert!(w.ran(w.guard) && w.ran(w.room));
    }

    // ---- follow-ups from the review ------------------------------------------------

    /// Alice as a level-33 (`LVL_GRGOD`) god without nohassle: a valid target
    /// only when the C passes `DG_ALLOW_GODS`.
    fn grgod(w: &mut W) {
        w.h.set_num(w.alice, "level", 33);
    }

    fn mem_of(ch: Uid) -> Vec<ScriptMemory> {
        vec![ScriptMemory { id: ch, cmd: None }]
    }

    #[test]
    fn allow_gods_is_passed_where_the_c_passes_it() {
        // DG_ALLOW_GODS: greet, greet_memory and the three leave triggers.
        let mut w = W::new();
        grgod(&mut w);
        w.trig(w.guard, 1, "g", 100, "", &greet_script(0));
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 0);
        // A god with nohassle is not a target either way.
        w.h.set_nohassle(w.alice, true);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);

        let mut w = W::new();
        grgod(&mut w);
        w.trig(w.guard, 1, "o", 100, "", RAN);
        w.h.set_script_mem(w.guard, mem_of(w.alice));
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert!(w.ran(w.guard));

        let mut w = W::new();
        grgod(&mut w);
        w.trig(w.guard, 1, "q", 100, "", "return 0");
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 0);

        let mut w = W::new();
        grgod(&mut w);
        let rock = w.obj(1, "a rock");
        w.h.obj_to_room(rock, w.room);
        w.trig(rock, 1, "q", 100, "", "return 0");
        assert_eq!(leave_otrigger(&mut w.h, w.room, w.alice, 0), 0);

        let mut w = W::new();
        grgod(&mut w);
        w.trig(w.room, 1, "q", 100, "", "return 0");
        assert_eq!(leave_wtrigger(&mut w.h, w.room, w.alice, 0), 0);
    }

    #[test]
    fn allow_gods_is_not_passed_to_the_command_and_remove_triggers() {
        // valid_dg_target(actor, 0): a level-33 god is not a target.
        let mut w = W::new();
        grgod(&mut w);
        w.trig(w.guard, 1, "c", 0, "*", RAN);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 0);
        assert!(!w.ran(w.guard));

        let mut w = W::new();
        grgod(&mut w);
        let rock = w.obj(1, "a rock");
        w.h.obj_to_room(rock, w.room);
        w.trig(rock, 1, "c", OCMD_ROOM, "*", "return 1");
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", ""), 0);

        let mut w = W::new();
        grgod(&mut w);
        let ring = w.obj(1, "a ring");
        w.trig(ring, 1, "l", 100, "", "return 0");
        assert_eq!(remove_otrigger(&mut w.h, ring, w.alice), 1);
        w.h.set_num(w.alice, "level", 5);
        assert_eq!(remove_otrigger(&mut w.h, ring, w.alice), 0);

        let mut w = W::new();
        grgod(&mut w);
        w.trig(w.room, 1, "c", 0, "*", "return 1");
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "x", ""), 0);
        w.h.set_num(w.alice, "level", 5);
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "x", ""), 1);
    }

    #[test]
    fn greet_memory_wants_a_valid_target_and_a_mob_that_is_not_fighting() {
        let mut w = W::new();
        w.trig(w.guard, 1, "o", 100, "", RAN);
        w.h.set_script_mem(w.guard, mem_of(w.alice));
        w.h.set_num(w.alice, "dg_invalid", 1);
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert!(!w.ran(w.guard));
        assert_eq!(w.h.script_mem(w.guard).len(), 1);

        let mut w = W::new();
        w.trig(w.guard, 1, "o", 100, "", RAN);
        w.h.set_script_mem(w.guard, mem_of(w.alice));
        w.h.set_link(w.guard, "fighting", Some(w.alice));
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert!(!w.ran(w.guard));
        assert_eq!(w.h.script_mem(w.guard).len(), 1);
    }

    /// Mutations of a mob that make `leave` and `door` skip it, and the actor
    /// each is run with.
    fn mob_skip_cases(w: &mut W, case: usize) -> Uid {
        match case {
            0 => w.sleep(w.guard),
            1 => w.h.set_link(w.guard, "fighting", Some(w.alice)),
            2 => w.charm(w.guard),
            _ => return w.guard,
        }
        w.alice
    }

    #[test]
    fn leave_and_door_skip_sleeping_fighting_charmed_mobs_and_the_actor() {
        let script = "set ran 1\nglobal ran\nreturn 0";
        // Controls: both run.
        let mut w = W::new();
        w.trig(w.guard, 1, "q", 100, "", script);
        assert_eq!(leave_mtrigger(&mut w.h, w.alice, 0), 0);
        let mut w = W::new();
        w.trig(w.guard, 1, "r", 100, "", script);
        assert_eq!(door_mtrigger(&mut w.h, w.alice, 0, 0), 0);
        for case in 0..4 {
            let mut w = W::new();
            w.trig(w.guard, 1, "q", 100, "", script);
            let actor = mob_skip_cases(&mut w, case);
            assert_eq!(leave_mtrigger(&mut w.h, actor, 0), 1, "leave {case}");
            assert!(!w.ran(w.guard), "leave {case}");

            let mut w = W::new();
            w.trig(w.guard, 1, "r", 100, "", script);
            let actor = mob_skip_cases(&mut w, case);
            assert_eq!(door_mtrigger(&mut w.h, actor, 0, 0), 1, "door {case}");
            assert!(!w.ran(w.guard), "door {case}");
        }
    }

    #[test]
    fn speech_is_heard_by_a_fighting_mob_and_load_runs_on_a_charmed_one() {
        let mut w = W::new();
        w.trig(w.guard, 1, "d", 0, "hi", RAN);
        w.h.set_link(w.guard, "fighting", Some(w.alice));
        speech_mtrigger(&mut w.h, w.alice, "hi");
        assert!(w.ran(w.guard));

        let mut w = W::new();
        w.trig(w.guard, 1, "n", 100, "", RAN);
        w.charm(w.guard);
        load_mtrigger(&mut w.h, w.guard);
        assert!(w.ran(w.guard));
    }

    #[test]
    fn command_mtrigger_answers_one_whatever_non_zero_the_script_returned() {
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", "return 5");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
    }

    #[test]
    fn command_wtrigger_skips_leading_spaces_of_the_command_and_argument() {
        let mut w = W::new();
        w.trig(w.room, 1, "c", 0, "*", "global arg\nglobal cmd\nreturn 1");
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "  look", "  at me"), 1);
        assert_eq!(w.g(w.room, "cmd").as_deref(), Some("look"));
        assert_eq!(w.g(w.room, "arg").as_deref(), Some("at me"));
    }

    #[test]
    fn a_star_counts_only_as_the_first_character_of_the_argument() {
        assert!(!word_check("hello", "a*"));
        assert!(!word_check("hello", "x *"));
        assert!(word_check("hello", "*x"));
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "a*", RAN);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "bob", ""), 0);
        assert!(!w.ran(w.guard));
        let ball = w.obj(1, "a ball");
        w.h.obj_to_room(ball, w.room);
        w.trig(ball, 2, "c", OCMD_ROOM, "a*", RAN);
        assert_eq!(command_otrigger(&mut w.h, w.alice, "bob", ""), 0);
        assert!(!w.ran(ball));
        w.trig(w.room, 3, "c", 0, "a*", RAN);
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "bob", ""), 0);
        assert!(!w.ran(w.room));
    }

    #[test]
    fn time_wtrigger_wants_the_hour_to_equal_narg() {
        for (hours, fires) in [(3, false), (5, true), (7, false)] {
            let mut w = W::new();
            w.trig(w.room, 1, "t", 5, "", RAN);
            w.h.time.hours = hours;
            time_wtrigger(&mut w.h, w.room);
            assert_eq!(w.ran(w.room), fires, "hour {hours}");
        }
    }

    #[test]
    fn death_answers_one_when_the_roll_is_lost() {
        let mut w = W::new();
        w.trig(w.guard, 1, "f", 50, "", "return 0");
        w.rolls(&[51]);
        assert_eq!(death_mtrigger(&mut w.h, w.guard, Some(w.alice)), 1);
        w.rolls(&[50]);
        assert_eq!(death_mtrigger(&mut w.h, w.guard, Some(w.alice)), 0);
    }

    #[test]
    fn time_sweep_skips_a_character_in_no_room() {
        let mut w = W::new();
        w.trig(w.guard, 3, "t", 5, "", RAN);
        w.h.char_from_room(w.guard);
        w.h.time.hours = 5;
        check_time_triggers(&mut w.h);
        assert!(!w.ran(w.guard));
    }

    #[test]
    fn command_otrigger_walks_the_equipment_positions_upwards() {
        let mut w = W::new();
        let high = w.obj(1, "a cloak");
        let low = w.obj(2, "a ring");
        w.h.equip_char(w.alice, high, 9);
        w.h.equip_char(w.alice, low, 2);
        w.trig(high, 1, "c", OCMD_EQUIP, "*", &cmd_script("ran", 1));
        w.trig(low, 2, "c", OCMD_EQUIP, "*", &cmd_script("ran", 1));
        assert_eq!(command_otrigger(&mut w.h, w.alice, "x", ""), 1);
        assert!(w.ran(low));
        assert!(!w.ran(high));
    }

    #[test]
    fn skill_name_zero_is_undefined() {
        let mut w = W::new();
        w.h.globals
            .texts
            .insert("spell:0".to_owned(), "reserved".to_owned());
        w.h.globals
            .texts
            .insert("spell:7".to_owned(), "fireball".to_owned());
        w.trig(w.guard, 1, "p", 100, "", "global spellname");
        cast_mtrigger(&mut w.h, w.alice, Some(w.guard), 0);
        assert_eq!(w.g(w.guard, "spellname").as_deref(), Some("UNDEFINED"));
    }

    #[test]
    fn a_nested_script_purging_its_own_owner_makes_the_objects_dispatchers_report_it_gone() {
        // C: the rat's load trigger purges the rat and leaves
        // `dg_owner_purged` set; the orb's script sees it after `%load%`,
        // NULLs `go_adress`, and `get_otrigger` returns 0.
        let mut w = W::new();
        w.h.globals
            .texts
            .insert("mobproto:3100".to_owned(), "a rat".to_owned());
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "g", 100, "", "oload mob 3100\nreturn 1");
        w.h.add_proto(3100, AttachType::Mob, "n", 100, "", "mpurge %self%");
        w.h.globals.nums.insert("mobtrig:3100".to_owned(), 3100);
        w.h.obj_to_room(orb, w.room);
        assert_eq!(get_otrigger(&mut w.h, orb, w.alice), 0);
        assert!(w.h.exists(orb));
    }

    /// A world with an orb in the Temple and mob 3100, whose load trigger
    /// purges the new mob (`purging`) or is harmless.
    fn orb_world(purging: bool) -> (W, Uid) {
        let mut w = W::new();
        w.h.globals
            .texts
            .insert("mobproto:3100".to_owned(), "a rat".to_owned());
        let body = if purging { "mpurge %self%" } else { "return 1" };
        w.h.add_proto(3100, AttachType::Mob, "n", 100, "", body);
        w.h.globals.nums.insert("mobtrig:3100".to_owned(), 3100);
        let orb = w.obj(10, "an orb");
        w.h.obj_to_room(orb, w.room);
        (w, orb)
    }

    #[test]
    fn every_object_dispatcher_treats_a_nested_self_purge_as_the_object_gone() {
        // C: the script's `oload mob` leaves `dg_owner_purged` set by the
        // rat's own purge; the orb's driver tests it after the command, NULLs
        // `go_adress`, and the dispatcher's `!obj` test fires. The control
        // (the rat survives) takes the other branch.
        const NESTED: &str = "oload mob 3100\nreturn 1";
        for purging in [true, false] {
            let gone = purging;
            let expect =
                |gone_value: i32, kept_value: i32| if gone { gone_value } else { kept_value };

            // timer_otrigger stops after the first trigger.
            let (mut w, orb) = orb_world(purging);
            w.trig(orb, 1, "f", 1, "", NESTED);
            w.trig(orb, 2, "f", 1, "", RAN);
            timer_otrigger(&mut w.h, orb);
            assert_eq!(w.ran(orb), !gone, "timer purging={purging}");

            // cmd_otrig returns 0 without trying the next trigger.
            let (mut w, orb) = orb_world(purging);
            w.trig(orb, 1, "c", OCMD_ROOM, "*", "return 0\noload mob 3100");
            w.trig(orb, 2, "c", OCMD_ROOM, "*", "return 1");
            assert_eq!(
                cmd_otrig(&mut w.h, orb, w.alice, "x", "", OCMD_ROOM),
                expect(0, 1),
                "cmd purging={purging}"
            );

            let (mut w, orb) = orb_world(purging);
            w.trig(orb, 1, "j", 1, "", NESTED);
            assert_eq!(
                wear_otrigger(&mut w.h, orb, w.alice, 3),
                expect(0, 1),
                "wear purging={purging}"
            );

            let (mut w, orb) = orb_world(purging);
            w.trig(orb, 1, "l", 1, "", NESTED);
            assert_eq!(
                remove_otrigger(&mut w.h, orb, w.alice),
                expect(0, 1),
                "remove purging={purging}"
            );

            let (mut w, orb) = orb_world(purging);
            w.trig(orb, 1, "h", 100, "", NESTED);
            assert_eq!(
                drop_otrigger(&mut w.h, orb, w.alice),
                expect(0, 1),
                "drop purging={purging}"
            );

            let (mut w, orb) = orb_world(purging);
            w.h.obj_to_char(orb, w.alice);
            w.trig(orb, 1, "i", 100, "", NESTED);
            assert_eq!(
                give_otrigger(&mut w.h, orb, w.alice, w.guard),
                expect(0, 1),
                "give purging={purging}"
            );

            let (mut w, orb) = orb_world(purging);
            w.trig(orb, 1, "s", 1, "", NESTED);
            assert_eq!(
                consume_otrigger(&mut w.h, orb, w.alice, OCMD_EAT),
                expect(0, 1),
                "consume purging={purging}"
            );

            // leave_otrigger skips the object's remaining triggers and goes
            // on to the next object.
            let (mut w, orb) = orb_world(purging);
            let rock = w.obj(11, "a rock");
            w.h.obj_to_room(rock, w.room);
            w.trig(orb, 1, "q", 100, "", NESTED);
            w.trig(orb, 2, "q", 100, "", RAN);
            w.trig(rock, 3, "q", 100, "", RAN);
            leave_otrigger(&mut w.h, w.room, w.alice, 0);
            assert_eq!(w.ran(orb), !gone, "leave orb purging={purging}");
            assert!(w.ran(rock), "leave rock purging={purging}");
        }
    }

    #[test]
    fn a_flag_left_by_a_script_under_a_dg_cast_line_does_not_make_the_object_gone() {
        // C: only the interpreter branch tests `dg_owner_purged`; `dg_cast`
        // is an earlier arm of the chain, so the orb's run ends normally and
        // `get_otrigger` returns the script's `return 1`.
        for (purging, expected) in [(true, 1), (false, 1)] {
            let mut w = W::new();
            w.h.globals.nums.insert("cast_triggers".to_owned(), 1);
            w.h.add_spell("fireball", 26, TAR_CHAR_ROOM, 0);
            w.h.globals
                .texts
                .insert("mobproto:1".to_owned(), "caster".to_owned());
            let body = if purging {
                "mpurge %self%"
            } else {
                "set cast 1\nglobal cast"
            };
            w.trig(w.guard, 1, "p", 100, "", body);
            let orb = w.obj(10, "an orb");
            w.h.obj_to_room(orb, w.room);
            w.trig(orb, 2, "g", 100, "", "dg_cast 'fireball' guard\nreturn 1");
            assert_eq!(get_otrigger(&mut w.h, orb, w.alice), expected);
            assert_eq!(w.h.is_dead(w.guard), purging);
            if purging {
                assert!(w.h.vm.owner_purged);
            }
            assert!(!w.h.vm.run_purged);
        }
        // Two timer triggers, the first the dg_cast: the second still runs.
        let mut w = W::new();
        w.h.globals.nums.insert("cast_triggers".to_owned(), 1);
        w.h.add_spell("fireball", 26, TAR_CHAR_ROOM, 0);
        w.h.globals
            .texts
            .insert("mobproto:1".to_owned(), "caster".to_owned());
        w.trig(w.guard, 1, "p", 100, "", "mpurge %self%");
        let orb = w.obj(10, "an orb");
        w.h.obj_to_room(orb, w.room);
        w.trig(orb, 2, "f", 1, "", "dg_cast 'fireball' guard");
        w.trig(orb, 3, "f", 1, "", RAN);
        timer_otrigger(&mut w.h, orb);
        assert!(w.ran(orb));
    }

    #[test]
    fn an_object_is_gone_when_extracted_or_when_the_run_left_through_its_purge_path() {
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "g", 100, "", "return 1");
        assert_eq!(get_otrigger(&mut w.h, orb, w.alice), 1);
        assert!(!object_gone(&mut w.h, orb));
        // A run that left through its purge path makes a live object gone:
        // `go_adress` is NULLed. The raw flag alone does not.
        w.h.vm.owner_purged = true;
        assert!(!object_gone(&mut w.h, orb));
        w.h.vm.run_purged = true;
        assert!(object_gone(&mut w.h, orb));
        // An extracted object is gone.
        w.h.extract_obj(orb);
        assert!(object_gone(&mut w.h, orb));
    }

    #[test]
    fn zone_emptiness_ignores_nohassle_immortals_and_players_elsewhere() {
        let fires = |setup: &dyn Fn(&mut W)| {
            let mut w = W::new();
            w.trig(w.guard, 3, "b", 100, "", RAN);
            setup(&mut w);
            script_trigger_check(&mut w.h);
            w.ran(w.guard)
        };
        // A mortal counts, with or without nohassle.
        assert!(fires(&|_| {}));
        assert!(fires(&|w| w.h.set_nohassle(w.alice, true)));
        // An immortal counts unless nohassle is on.
        assert!(fires(&|w| w.h.set_num(w.alice, "level", 32)));
        assert!(!fires(&|w| {
            w.h.set_num(w.alice, "level", 32);
            w.h.set_nohassle(w.alice, true);
        }));
        // A player in another zone, or not playing, does not.
        assert!(!fires(&|w| {
            w.h.set_num(w.room2, "zone", 9);
            w.h.char_to_room(w.alice, w.room2);
        }));
        assert!(!fires(&|w| w.h.set_num(w.alice, "desc", 0)));
    }

    #[test]
    fn greet_walk_follows_a_mob_a_script_moved_into_another_room() {
        // C: `ch = ch->next_in_room` is read after the run, so the mob that
        // went to the Altar carries the walk on to the Altar's people.
        let mut w = W::new();
        let mover = w.mob(3030, "the mover");
        let far = w.h.add_mob(3031, "the far mob");
        w.h.set_num(far, "pos", 8);
        w.h.char_to_room(far, w.room2);
        w.trig(mover, 1, "g", 100, "", "mgoto 3002");
        w.trig(w.guard, 2, "g", 100, "", RAN);
        w.trig(far, 3, "h", 100, "", RAN);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
        assert_eq!(w.h.in_room(mover), Some(w.room2));
        assert!(!w.ran(w.guard));
        assert!(w.ran(far));
    }

    #[test]
    fn entry_memory_walk_follows_an_actor_a_script_moved_away() {
        let mut w = W::new();
        let bob = w.mob(3030, "bob");
        w.trig(w.guard, 1, "o", 100, "", "mteleport %actor% 3002");
        w.h.set_script_mem(
            w.guard,
            vec![
                ScriptMemory { id: bob, cmd: None },
                ScriptMemory {
                    id: w.alice,
                    cmd: None,
                },
            ],
        );
        // Room order [bob, guard, alice]. Bob is sent to the Altar, where
        // his `next_in_room` is the end of the list: Alice is never reached
        // and her entry stays.
        entry_memory_mtrigger(&mut w.h, w.guard);
        assert_eq!(w.h.in_room(bob), Some(w.room2));
        assert_eq!(w.h.in_room(w.alice), Some(w.room));
        assert_eq!(w.h.script_mem(w.guard), mem_of(w.alice));
    }

    // ---- more corpus records -------------------------------------------------------

    /// Alice with a descriptor, and Bob, another player in the Temple, so that
    /// `%send%` and `%echoaround%` leave effects.
    fn watched() -> (W, Uid) {
        let mut w = W::new();
        w.h.globals
            .texts
            .insert("commands".to_owned(), "north look enter".to_owned());
        w.h.set_num(w.alice, "desc", 1);
        let bob = w.h.add_player("Bob");
        w.h.set_num(bob, "desc", 1);
        w.h.set_num(bob, "pos", 8);
        w.h.char_to_room(bob, w.room);
        (w, bob)
    }

    #[test]
    fn corpus_guild_guards_stop_the_wrong_class_on_their_own_side() {
        // Triggers 3001 (Cleric, north), 3002 (Thief, east), 3003 (Warrior,
        // east), each `0 q 100`.
        let template = |dir: &str, class: &str| {
            format!(
                "* Check the direction the player must go to enter the guild.\n\
                 if %direction% == {dir}\n\
                 \x20 * Stop them if they are not the appropriate class.\n\
                 \x20 if %actor.class% != {class}\n\
                 \x20   return 0\n\
                 \x20   %send% %actor% The guard humiliates you, and blocks your way.\n\
                 \x20   %echoaround% %actor% The guard humiliates %actor.name%, and blocks %actor.hisher% way.\n\
                 \x20 end\n\
                 end"
            )
        };
        for (vnum, dir, class, dir_num) in [
            (3001, "north", "Cleric", 0),
            (3002, "east", "Thief", 1),
            (3003, "east", "Warrior", 1),
        ] {
            let (mut w, bob) = watched();
            w.trig(w.guard, vnum, "q", 100, "", &template(dir, class));
            // Alice is a Cleric (class 1).
            let passes = class == "Cleric";
            let expected = i32::from(passes);
            assert_eq!(
                leave_mtrigger(&mut w.h, w.alice, dir_num),
                expected,
                "{vnum}"
            );
            if passes {
                assert!(w.h.effects.is_empty(), "{vnum}");
            } else {
                assert_eq!(
                    w.h.effects,
                    [
                        format!(
                            "send {}: The guard humiliates you, and blocks your way.\n\r",
                            w.alice
                        ),
                        format!("send {bob}: The guard humiliates Alice, and blocks its way.\n\r"),
                    ],
                    "{vnum}"
                );
            }
            // The other way out is not guarded.
            w.h.effects.clear();
            assert_eq!(leave_mtrigger(&mut w.h, w.alice, 3), 1, "{vnum}");
            assert!(w.h.effects.is_empty(), "{vnum}");
        }
    }

    #[test]
    fn corpus_mob_command_example_lets_other_commands_continue() {
        // Trigger 62, "Mob Command Example", `0 c 100`, argument `l`. The
        // record's `%echoround% %actor% %actor.name% stares at the orb.`
        // line (a typo in the corpus, a mob command that does not exist) is
        // left out.
        let body = "* does not work for level 32 and above.\n\
            * Make sure the command is look, check for any abbrev of orb\n\
            if %cmd.mudcommand% == look && orb /= %arg%\n\
              %send% %actor% As you look at the orb a feeling of peace and serenity comes over you.\n\
            else\n\
              * If it doesn't match let the command continue.\n\
              return 0\n\
            end";
        let (mut w, _) = watched();
        w.trig(w.guard, 62, "c", 0, "l", body);
        // `look orb` is claimed: the script's default return of 1 stands.
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "look", "orb"), 1);
        assert_eq!(
            w.h.effects,
            [format!(
                "send {}: As you look at the orb a feeling of peace and serenity comes over you.\n\r",
                w.alice
            )]
        );
        // `look floor` and `north` are not: the script returns 0, and the
        // walk ends with 0.
        w.h.effects.clear();
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "look", "floor"), 0);
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "north", ""), 0);
        assert!(w.h.effects.is_empty());
    }

    #[test]
    fn corpus_object_get_drop_and_wear_triggers() {
        // Trigger 169, "Quest Token check if player is on quest", `1 g 100`.
        let get = "if !%actor.varexists(on_quest_zone_1)%\n\
              %send% %actor% You are not on a quest, don't steal other peoples quest items!\n\
              return 0\n\
            end";
        let (mut w, _) = watched();
        let token = w.obj(1, "a token");
        w.h.obj_to_room(token, w.room);
        w.trig(token, 169, "g", 100, "", get);
        assert_eq!(get_otrigger(&mut w.h, token, w.alice), 0);
        assert_eq!(
            w.h.effects,
            [format!(
                "send {}: You are not on a quest, don't steal other peoples quest items!\n\r",
                w.alice
            )]
        );
        // On the quest: the script says nothing and the get goes ahead.
        w.h.effects.clear();
        let marker = w.h.add_proto(900, AttachType::Mob, "b", 0, "", "nop 0");
        w.h.attach(w.alice, &marker).unwrap();
        w.h.script_mut(w.alice)
            .unwrap()
            .global_vars
            .add_var("on_quest_zone_1", "1", 0);
        assert_eq!(get_otrigger(&mut w.h, token, w.alice), 1);
        assert!(w.h.effects.is_empty());

        // Trigger 84, "Obj Drop Example", `1 h 100`.
        let drop = "if %actor.level% < 31\n\
              %send% %actor% You fail to drop %self.shortdesc%.\n\
              return 0\n\
            else\n\
              %send% %actor% %self.shortdesc% glows a bright yellow, then explodes.\n\
              %purge% %self%\n\
            end";
        let (mut w, _) = watched();
        let ball = w.obj(2, "a ball");
        w.h.obj_to_char(ball, w.alice);
        w.trig(ball, 84, "h", 100, "", drop);
        assert_eq!(drop_otrigger(&mut w.h, ball, w.alice), 0);
        assert_eq!(
            w.h.effects,
            [format!("send {}: You fail to drop a ball.\n\r", w.alice)]
        );
        // A level-31 immortal drops it and it explodes: purged, so 0.
        w.h.effects.clear();
        w.h.set_num(w.alice, "level", 31);
        assert_eq!(drop_otrigger(&mut w.h, ball, w.alice), 0);
        assert_eq!(
            w.h.effects[0],
            format!(
                "send {}: a ball glows a bright yellow, then explodes.\n\r",
                w.alice
            )
        );
        assert!(!w.h.exists(ball));
    }

    #[test]
    fn corpus_straitjacket_wear_attaches_the_remove_trigger() {
        // Triggers 1345 (`1 j 100`), 1346 (`1 c 7`, argument `untie`) and
        // 1347 (`1 l 7`).
        let (mut w, bob) = watched();
        let jacket = w.obj(1330, "a straitjacket");
        w.trig(jacket, 1345, "j", 100, "", "attach 1347 %self.id%");
        let remove = "%echoaround% %actor% %actor.name% struggles to get out of the straitjacket.\n\
            %send% %actor% You can't get out of it. Maybe you should untie it first.\n\
            return 0";
        w.h.add_proto(1347, AttachType::Obj, "l", 7, "", remove);
        let untie = "%echoaround% %actor% %actor.name% unties the straitjacket.\n\
            %send% %actor% You untie the jacket, maybe now you can remove it.\n\
            detach 1347 %self.id%";
        w.trig(jacket, 1346, "c", 7, "untie", untie);
        // Wearing it is allowed (no return) and attaches the remove trigger.
        assert_eq!(wear_otrigger(&mut w.h, jacket, w.alice, 3), 1);
        w.h.equip_char(w.alice, jacket, 3);
        assert!(script_check(&w.h, jacket, OTRIG_REMOVE));
        // It cannot come off.
        assert_eq!(remove_otrigger(&mut w.h, jacket, w.alice), 0);
        assert_eq!(
            w.h.effects,
            [
                format!("send {bob}: Alice struggles to get out of the straitjacket.\n\r"),
                format!(
                    "send {}: You can't get out of it. Maybe you should untie it first.\n\r",
                    w.alice
                ),
            ]
        );
        // `untie` (worn counts: the narg has OCMD_EQUIP) detaches it.
        w.h.effects.clear();
        assert_eq!(command_otrigger(&mut w.h, w.alice, "untie", ""), 1);
        assert!(!script_check(&w.h, jacket, OTRIG_REMOVE));
        assert_eq!(remove_otrigger(&mut w.h, jacket, w.alice), 1);
    }

    #[test]
    fn corpus_room_enter_drop_and_command_examples() {
        // Trigger 55, "Room Enter Example", `2 g 100`. The record's two
        // `%echoaround%` lines are left out.
        let enter = "if %actor.is_pc%\n\
              if %actor.sex% == male\n\
                return 0\n\
                %send% %actor% An invisible barrier blocks you. A voice rings out \"No men allowed.\"\n\
              elseif %actor.sex% == female\n\
                %echo% Ladies are always welcome!\n\
              else * must be neutral.\n\
                return 0\n\
                %send% %actor% An invisible barrier blocks you. A voice rings out \"No... things allowed.\"\n\
              end\n\
            end";
        let (mut w, _) = watched();
        w.trig(w.room, 55, "g", 100, "", enter);
        w.h.set_num(w.alice, "sex", 1);
        assert_eq!(enter_wtrigger(&mut w.h, w.room, w.alice, 0), 0);
        assert_eq!(
            w.h.effects,
            [format!(
                "send {}: An invisible barrier blocks you. A voice rings out \"No men allowed.\"\n\r",
                w.alice
            )]
        );
        w.h.effects.clear();
        w.h.set_num(w.alice, "sex", 2);
        assert_eq!(enter_wtrigger(&mut w.h, w.room, w.alice, 0), 1);
        assert_eq!(w.h.effects.len(), 2, "both players hear the echo");
        assert!(w.h.effects[0].contains("Ladies are always welcome!"));

        // Trigger 56, "Room Drop Example", `2 h 100`.
        let drop = "if %object.type% == TRASH\n\
              %send% %actor% Magical forces prevent you from dropping %object.shortdesc%.\n\
              %send% %actor% No Littering! Drop your trash somewhere else.\n\
              return 0\n\
            end";
        let (mut w, _) = watched();
        let can = w.obj(5, "a can");
        w.h.set_num(can, "type", 13);
        w.h.obj_to_char(can, w.alice);
        w.trig(w.room, 56, "h", 100, "", drop);
        assert_eq!(drop_wtrigger(&mut w.h, can, w.alice), 0);
        assert_eq!(w.h.effects.len(), 2);
        assert!(w.h.effects[0].contains("Magical forces prevent you from dropping a can."));
        // Not trash: the script is silent and its default return stands.
        w.h.effects.clear();
        w.h.set_num(can, "type", 9);
        assert_eq!(drop_wtrigger(&mut w.h, can, w.alice), 1);
        assert!(w.h.effects.is_empty());

        // Trigger 1452, "Room Command Example", `2 c 100`, argument `l`.
        let look = "if %cmd.mudcommand% == look && test /= %arg%\n\
              %echo% The trigger works!\n\
              %force% %actor% applaud\n\
            else\n\
              %send% %actor% Look at what?\n\
            end";
        let (mut w, _) = watched();
        w.trig(w.room, 1452, "c", 0, "l", look);
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "look", "te"), 1);
        // `do_wforce` hands `command_interpreter` what `one_argument` left,
        // with its leading space.
        assert_eq!(w.h.mud_commands, [(w.alice, " applaud".to_owned())]);
        w.h.effects.clear();
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "look", "x"), 1);
        assert_eq!(
            w.h.effects,
            [format!("send {}: Look at what?\n\r", w.alice)]
        );
        // `north` does not start with `l`.
        assert_eq!(command_wtrigger(&mut w.h, w.alice, "north", ""), 0);
    }

    // ---- review round 3 ----------------------------------------------------------

    #[test]
    fn command_mtrigger_walk_follows_the_next_mob_a_script_moved() {
        // C: `ch_next = ch->next_in_room` is saved before the triggers run.
        // Room order [first, guard, alice]. first sends the guard to the
        // Altar (head of [guard, far]) and declines; the saved pointer is
        // the guard, whose own next is now `far`, so far's trigger claims
        // the command.
        let mut w = W::new();
        let first = w.mob(3030, "the first mob");
        let far = w.h.add_mob(3031, "the far mob");
        w.h.set_num(far, "pos", 8);
        w.h.char_to_room(far, w.room2);
        w.trig(
            first,
            1,
            "c",
            0,
            "*",
            &format!("mteleport {} 3002\nreturn 0", id(w.guard)),
        );
        w.trig(far, 2, "c", 0, "*", "set ran 1\nglobal ran\nreturn 1");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
        assert_eq!(w.h.in_room(w.guard), Some(w.room2));
        assert!(w.ran(far));
    }

    #[test]
    fn speech_mtrigger_walk_follows_the_next_mob_a_script_moved() {
        let mut w = W::new();
        let first = w.mob(3030, "the first mob");
        let far = w.h.add_mob(3031, "the far mob");
        w.h.set_num(far, "pos", 8);
        w.h.char_to_room(far, w.room2);
        w.trig(
            first,
            1,
            "d",
            0,
            "hi",
            &format!("mteleport {} 3002", id(w.guard)),
        );
        w.trig(far, 2, "d", 0, "hi", RAN);
        speech_mtrigger(&mut w.h, w.alice, "hi");
        assert_eq!(w.h.in_room(w.guard), Some(w.room2));
        assert!(w.ran(far));
    }

    #[test]
    fn next_content_reads_the_list_the_object_is_in_now() {
        // leave_otrigger's saved `obj_next` continues in the list its object
        // is in when its turn comes (see also
        // `leave_otrigger_follows_an_object_an_earlier_run_moved`).
        let mut w = W::new();
        let a = w.obj(1, "a rock");
        let b = w.obj(2, "a coin");
        let c = w.obj(3, "a gem");
        for o in [a, b] {
            w.h.obj_to_room(o, w.room);
        }
        w.h.obj_to_room(c, w.room2);
        assert_eq!(next_content(&w.h, a), Some(b));
        assert_eq!(next_content(&w.h, b), None);
        w.h.obj_to_room(b, w.room2);
        assert_eq!(next_content(&w.h, a), None);
        assert_eq!(next_content(&w.h, c), Some(b));
        w.h.extract_obj(b);
        assert_eq!(next_content(&w.h, b), None);
    }

    #[test]
    fn leave_otrigger_follows_an_object_an_earlier_run_moved() {
        // Room contents [a, b]; Alice carries x. `a` forces Alice to run a
        // command, which puts `b` at the head of her inventory ([b, x])
        // before `b`'s turn. `obj_next` of `a` is `b`; `b`'s is read from the
        // list `b` is in by then, so the walk goes on to `x`.
        let mut w = W::new();
        let (a, b, x) = (w.obj(1, "a rock"), w.obj(2, "a coin"), w.obj(3, "a gem"));
        for o in [a, b] {
            w.h.obj_to_room(o, w.room);
        }
        w.h.obj_to_char(x, w.alice);
        w.trig(a, 1, "q", 100, "", "oforce %actor% hop");
        w.trig(b, 2, "q", 100, "", RAN);
        w.trig(x, 3, "q", 100, "", RAN);
        let alice = w.alice;
        w.h.hook(
            Some(alice),
            "hop",
            MockAction::ObjToChar { obj: b, ch: alice },
        );
        assert_eq!(leave_otrigger(&mut w.h, w.room, alice, 0), 1);
        assert!(w.ran(b));
        assert!(w.ran(x));
    }

    #[test]
    fn greet_returning_zero_does_not_end_the_walk() {
        // Room [late, guard, alice]: late's first trigger returns 0, yet its
        // second trigger and the guard's still run.
        let mut w = W::new();
        let late = w.mob(3025, "the clerk");
        w.trig(late, 1, "g", 100, "", &greet_script(0));
        w.trig(late, 2, "g", 100, "", "set second 1\nglobal second");
        w.trig(w.guard, 3, "g", 100, "", RAN);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 0);
        assert_eq!(w.g(late, "second").as_deref(), Some("1"));
        assert!(w.ran(w.guard));
    }

    #[test]
    fn greet_memory_skips_a_waiting_trigger_and_runs_only_one() {
        // A trigger that is still running is skipped, memory forgotten.
        let mut w = W::new();
        let t = w.trig(w.guard, 1, "o", 100, "", RAN);
        w.busy(w.guard, t);
        w.h.set_script_mem(w.guard, mem_of(w.alice));
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert!(!w.ran(w.guard));
        assert!(w.h.script_mem(w.guard).is_empty());
        // Only the first memory trigger that qualifies runs.
        let mut w = W::new();
        w.trig(w.guard, 1, "o", 100, "", RAN);
        w.trig(w.guard, 2, "o", 100, "", "set b 1\nglobal b");
        w.h.set_script_mem(w.guard, mem_of(w.alice));
        greet_memory_mtrigger(&mut w.h, w.alice);
        assert!(w.ran(w.guard));
        assert_eq!(w.g(w.guard, "b"), None);
    }

    #[test]
    fn door_mtrigger_skips_a_waiting_trigger() {
        let mut w = W::new();
        let t = w.trig(w.guard, 1, "r", 100, "", "return 0");
        w.busy(w.guard, t);
        assert_eq!(door_mtrigger(&mut w.h, w.alice, 0, 0), 1);
    }

    #[test]
    fn hitprcnt_skips_a_charmed_mob() {
        let mut w = W::new();
        w.trig(w.guard, 1, "l", 100, "", RAN);
        w.h.set_num(w.guard, "max_hit", 100);
        w.h.set_num(w.guard, "hit", 10);
        w.h.set_link(w.guard, "fighting", Some(w.alice));
        w.charm(w.guard);
        hitprcnt_mtrigger(&mut w.h, w.guard);
        assert!(!w.ran(w.guard));
    }

    #[test]
    fn time_otrigger_wants_the_hour_to_equal_narg() {
        let mut w = W::new();
        let clock = w.obj(11, "a clock");
        w.trig(clock, 2, "t", 5, "", RAN);
        w.h.time.hours = 3;
        time_otrigger(&mut w.h, clock);
        assert!(!w.ran(clock));
        w.h.time.hours = 7;
        time_otrigger(&mut w.h, clock);
        assert!(!w.ran(clock));
        w.h.time.hours = 5;
        time_otrigger(&mut w.h, clock);
        assert!(w.ran(clock));
    }

    #[test]
    fn zone_emptiness_edges() {
        let fires = |setup: &dyn Fn(&mut W)| {
            let mut w = W::new();
            w.trig(w.guard, 3, "b", 100, "", RAN);
            setup(&mut w);
            script_trigger_check(&mut w.h);
            w.ran(w.guard)
        };
        // LVL_IMMORT itself (31) with nohassle does not count; 30 does.
        assert!(!fires(&|w| {
            w.h.set_num(w.alice, "level", 31);
            w.h.set_nohassle(w.alice, true);
        }));
        assert!(fires(&|w| {
            w.h.set_num(w.alice, "level", 30);
            w.h.set_nohassle(w.alice, true);
        }));
        // A playing character in no room is skipped.
        assert!(!fires(&|w| w.h.char_from_room(w.alice)));
        // A mob that is "playing" with nohassle-level is still not an
        // immortal player: an NPC never gets the nohassle exemption.
        assert!(fires(&|w| {
            w.h.set_num(w.guard, "desc", 1);
            w.h.set_num(w.guard, "level", 40);
            w.h.set_nohassle(w.guard, true);
            w.h.set_num(w.alice, "desc", 0);
        }));
    }

    #[test]
    fn greet_walk_goes_on_past_a_mob_that_purged_itself() {
        // The purged mob stays in the list until the sweep, and the walk
        // goes on to the next one.
        let mut w = W::new();
        let late = w.mob(3025, "the clerk");
        w.trig(late, 1, "g", 100, "", "mpurge %self%");
        w.trig(w.guard, 2, "g", 100, "", RAN);
        greet_mtrigger(&mut w.h, w.alice, 0);
        assert!(w.h.is_dead(late));
        assert!(w.ran(w.guard));
    }

    #[test]
    fn load_prunes_the_prototype_only_on_the_error_code() {
        let mut w = W::new();
        w.trig(w.guard, 1, "n", 100, "", "return 0");
        load_mtrigger(&mut w.h, w.guard);
        assert!(w.h.effects.is_empty());
    }

    #[test]
    fn cmd_otrig_strips_the_commands_leading_spaces_for_the_variable() {
        let mut w = W::new();
        let ball = w.obj(47, "a ball");
        w.trig(ball, 1, "c", OCMD_INVEN, "*", &cmd_script("ran", 1));
        assert_eq!(
            cmd_otrig(&mut w.h, ball, w.alice, "  shake", "x", OCMD_INVEN),
            1
        );
        assert_eq!(w.g(ball, "cmd").as_deref(), Some("shake"));
    }

    #[test]
    fn a_runaway_live_walk_is_logged() {
        // Two mobs that each put themselves back at the head of the room
        // would alternate for ever in the C.
        let mut w = W::new();
        let other = w.mob(3030, "the other");
        w.trig(w.guard, 1, "g", 100, "", "mgoto 3001");
        w.trig(other, 2, "g", 100, "", "mgoto 3001");
        greet_mtrigger(&mut w.h, w.alice, 0);
        assert!(
            w.h.logs
                .iter()
                .any(|line| line.contains("greet_mtrigger walk stopped"))
        );
    }

    #[test]
    fn command_mtrigger_reaches_a_trigger_an_earlier_run_attached() {
        // C: after the first run `t->next` is trigger 99, which claims it.
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", "attach 99 %self.id%\nreturn 0");
        w.h.add_proto(99, AttachType::Mob, "c", 0, "*", "return 1");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
    }

    #[test]
    fn command_mtrigger_walk_survives_a_trigger_detaching_itself() {
        // C: `command_mtrigger` reads `t->next` after the run, from a trigger
        // that has been freed (undefined); the port goes to the successor
        // the trigger had when it started, here trigger 99, attached by A.
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", "attach 99 %self.id%\nreturn 0");
        w.trig(w.guard, 2, "c", 0, "*", "detach 2 %self.id%\nreturn 0");
        w.h.add_proto(99, AttachType::Mob, "c", 0, "*", "return 1");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
        // A trigger after the one that detached itself still runs.
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", "detach 1 %self.id%\nreturn 0");
        w.trig(w.guard, 2, "c", 0, "*", "return 1");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
        // A trigger that detaches itself and its successor: the walk goes on
        // to the first later trigger still attached.
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", "return 0");
        w.trig(
            w.guard,
            2,
            "c",
            0,
            "*",
            "detach 2 %self.id%\ndetach 3 %self.id%\nreturn 0",
        );
        w.trig(w.guard, 3, "c", 0, "*", "return 0");
        w.trig(w.guard, 4, "c", 0, "*", "return 1");
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 1);
    }

    #[test]
    fn cmd_otrig_reaches_a_trigger_an_earlier_run_attached() {
        let mut w = W::new();
        let ball = w.obj(47, "a ball");
        w.trig(
            ball,
            1,
            "c",
            OCMD_ROOM,
            "*",
            "attach 99 %self.id%\nreturn 0",
        );
        w.h.add_proto(99, AttachType::Obj, "c", OCMD_ROOM, "*", "return 1");
        assert_eq!(cmd_otrig(&mut w.h, ball, w.alice, "x", "", OCMD_ROOM), 1);
    }

    #[test]
    fn timer_otrigger_reaches_a_trigger_an_earlier_run_attached() {
        let mut w = W::new();
        let orb = w.obj(10, "an orb");
        w.trig(orb, 1, "f", 1, "", "attach 99 %self.id%");
        w.h.add_proto(99, AttachType::Obj, "f", 1, "", RAN);
        timer_otrigger(&mut w.h, orb);
        assert!(w.ran(orb));
    }

    #[test]
    fn leave_otrigger_reaches_a_trigger_an_earlier_run_attached() {
        // C: the attached trigger is `t->next` after the first run; it
        // returns 0, so the walk's result is 0.
        let mut w = W::new();
        let a = w.obj(1, "a rock");
        w.h.obj_to_room(a, w.room);
        w.trig(a, 1, "q", 100, "", "attach 99 %self.id%\nreturn 1");
        w.h.add_proto(99, AttachType::Obj, "q", 100, "", "return 0");
        assert_eq!(leave_otrigger(&mut w.h, w.room, w.alice, 0), 0);
    }

    #[test]
    fn greet_mtrigger_reads_the_next_trigger_just_before_each_run() {
        // [A, B]: A attaches 99 (the tail) before B's turn starts, so B's
        // `next_trig` is 99, which returns 0. B attaches 100 during its own
        // run, after B's `next_trig` was read, but 99's `next_trig` (read
        // when 99's turn starts) is 100, so 100 runs too.
        let mut w = W::new();
        w.trig(w.guard, 1, "g", 100, "", "attach 99 %self.id%\nreturn 1");
        w.trig(w.guard, 2, "g", 100, "", "attach 100 %self.id%\nreturn 1");
        w.h.add_proto(99, AttachType::Mob, "g", 100, "", "return 0");
        w.h.add_proto(100, AttachType::Mob, "g", 100, "", RAN);
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 0);
        assert!(w.ran(w.guard));
        // A trigger attached by the last run is not examined.
        let mut w = W::new();
        w.trig(w.guard, 1, "g", 100, "", "attach 99 %self.id%\nreturn 1");
        w.h.add_proto(99, AttachType::Mob, "g", 100, "", "return 0");
        assert_eq!(greet_mtrigger(&mut w.h, w.alice, 0), 1);
    }

    #[test]
    fn a_trigger_that_keeps_attaching_itself_ends_the_walk() {
        let mut w = W::new();
        w.trig(w.guard, 1, "c", 0, "*", "attach 1 %self.id%\nreturn 0");
        w.h.add_proto(
            1,
            AttachType::Mob,
            "c",
            0,
            "*",
            "attach 1 %self.id%\nreturn 0",
        );
        assert_eq!(command_mtrigger(&mut w.h, w.alice, "x", ""), 0);
        assert!(
            w.h.logs
                .iter()
                .any(|l| l.contains("command_mtrigger walk stopped")
                    && !l.starts_with("SCRIPT ERROR"))
        );
    }

    #[test]
    fn next_content_follows_carried_and_contained_objects() {
        let mut w = W::new();
        let a = w.obj(1, "a rock");
        let b = w.obj(2, "a coin");
        w.h.obj_to_char(a, w.alice);
        w.h.obj_to_char(b, w.alice);
        let carried = w.h.carrying(w.alice);
        let first = carried[0];
        assert_eq!(next_content(&w.h, first), Some(carried[1]));
        assert_eq!(next_content(&w.h, carried[1]), None);
        let bag = w.obj(3, "a bag");
        let x = w.obj(4, "a gem");
        let y = w.obj(5, "a pearl");
        w.h.obj_to_obj(x, bag);
        w.h.obj_to_obj(y, bag);
        let inside = w.h.obj_contains(bag);
        assert_eq!(next_content(&w.h, inside[0]), Some(inside[1]));
        assert_eq!(next_content(&w.h, inside[1]), None);
    }

    #[test]
    fn greet_memory_stops_at_the_first_remembered_command() {
        // C: `command_performed = 1; break;` ends the walk of this mob's
        // memory, so the second command entry for the actor never runs.
        let mut w = W::new();
        w.h.set_script_mem(
            w.guard,
            vec![
                ScriptMemory {
                    id: w.alice,
                    cmd: Some("bow".to_owned()),
                },
                ScriptMemory {
                    id: w.alice,
                    cmd: Some("wave".to_owned()),
                },
            ],
        );
        let alice = w.alice;
        greet_memory_mtrigger(&mut w.h, alice);
        assert_eq!(w.say_log(), vec!["bow".to_owned()]);
    }

    #[test]
    fn cmd_otrig_any_non_zero_result_claims_the_command() {
        let mut w = W::new();
        let ball = w.obj(47, "a ball");
        w.trig(ball, 1, "c", OCMD_ROOM, "*", "return 2");
        assert_eq!(cmd_otrig(&mut w.h, ball, w.alice, "x", "", OCMD_ROOM), 1);
    }

    #[test]
    fn skill_name_only_minus_one_is_unused() {
        let w = W::new();
        assert_eq!(skill_name(&w.h, -1), "UNUSED");
        assert_eq!(skill_name(&w.h, -2), "UNDEFINED");
    }

    #[test]
    fn word_check_cuts_the_list_one_byte_short_of_the_buffer() {
        // 'a ' * 255 is 510 bytes, then 'zz': bytes 510..512 hold the word.
        // strcpy into char[MAX_INPUT_LENGTH] keeps 511 bytes, so the word
        // is cut to 'z' and still matches 'z' but not 'zz'.
        let list = format!("{}zz", "a ".repeat(255));
        assert_eq!(list.len(), 512);
        assert!(word_check("z", &list));
        assert!(!word_check("zz", &list));
    }
}
