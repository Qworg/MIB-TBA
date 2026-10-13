# dg-vm host contract

This is the complete list of primitives a game implements to host the VM: nine traits, `BaseHost`, `WorldHost`, `SubstHost`, `CharHost`, `ObjHost`, `RoomHost`, `CmdHost`, `MagicHost` and `DispatchHost`, which `host::DgHost` unions. The tables are generated from the trait definitions (every `fn` inside every `pub trait`), so a method missing here is a bug in this file.

Rules (see `src/host.rs`): a primitive is data-level and typed, it reads or changes one fact (`GET_GOLD(ch)`, `obj_to_char(obj, ch)`), and it documents the C expression it stands for. Every fact has exactly one primitive across all nine traits. Every method of a sub-trait carries its module prefix (`subst_`, `char_`, `obj_`, `room_`, `cmd_`, `magic_`, `disp_`). An entity is named by a `Uid`; a primitive handed an id that no longer exists does nothing and returns the neutral value, and never panics. A host that does not model a fact returns the value in the "Unmodelled" column (and may report it through `BaseHost::unsupported`).

Columns: **Method** is the signature; **C expression** is what it stands for; **Unmodelled** is what a host that does not model it returns; **Called by** lists the VM modules that call it (non-test code; `commands::*` are the files of `src/commands/`).

## BaseHost

Defined in `host.rs`. 17 methods. What the interpreter core needs: script storage, existence, randomness, time, the wait queue, the shared run state, logging, and the one escape hatch into the game's command interpreter.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn exists(&self, uid: Uid) -> bool;` | Whether this id names a live entity (`find_char`/`find_obj`/ `find_room` returning non-NULL). See the module notes on extraction. | `false` | `commands::mob`, `commands::obj`, `commands::wld`, `dispatch`, `driver` |
| `fn script(&self, uid: Uid) -> Option<&Script>;` | `SCRIPT(go)`: the entity's script, if it has one. | `None` | `commands::comm`, `commands::mob`, `dispatch`, `driver`, `fields_char`, `fields_obj`, `fields_room`, `host`, `subst` |
| `fn script_mut(&mut self, uid: Uid) -> Option<&mut Script>;` | `SCRIPT(go)` for mutation, if it has one. | `None` | `commands::shared`, `dispatch`, `driver` |
| `fn ensure_script(&mut self, uid: Uid) -> Option<&mut Script>;` | The entity's script, created empty if absent (the C's `if (!SCRIPT(x)) CREATE(SCRIPT(x), …)`). `None` when the entity does not exist. | `None` | `driver` |
| `fn remove_script(&mut self, uid: Uid) -> Option<Script>;` | `extract_script`'s `SCRIPT(go) = NULL`: take the entity's script away, globals and all. The caller cancels the triggers' waits first. | `None` | `driver` |
| `fn vm_state(&mut self) -> &mut VmState;` | The C globals and statics the interpreter shares across nested runs (see `VmState`). The host stores one for the life of the world. | Not optional: store one `VmState` (`Default`) for the life of the world and hand out the same one every call | `commands::comm`, `commands::mob`, `commands::obj`, `driver`, `expr`, `subst` |
| `fn script_players(&self) -> bool;` | `CONFIG_SCRIPT_PLAYERS`: whether triggers may be attached to players. | `false` | `dispatch`, `driver` |
| `fn pulse(&self) -> i64;` | The global `pulse` counter (tenths of a second), which `wait until` reads to find the time of day. | `0` | `driver` |
| `fn rand_number(&mut self, from: i64, to: i64) -> i64;` | `rand_number(from, to)`, inclusive. `from > to` is swapped, as in the C. | Any value in `from..=to` | `dispatch`, `subst` |
| `fn mud_time(&self) -> MudTime;` | `time_info`. | `MudTime::default()` (all zero) | `dispatch`, `driver`, `subst` |
| `fn trig_proto(&self, vnum: i32) -> Option<Arc<TrigProto>>;` | `read_trigger(real_trigger(vnum))`: the prototype for `attach`. | `None` | `driver` |
| `fn schedule_wait(&mut self, owner: Uid, trigger_id: u64, pulses: i64);` | Queue a `wait`: after `pulses` (tenths of a second) the game calls `crate::driver::resume` for this owner and trigger instance. Replaces `event_create(trig_wait_event, …)`; like it, fewer than 1 pulse is 1. | Do nothing (a `wait` never resumes); the game must call `driver::resume` | `driver` |
| `fn cancel_wait(&mut self, owner: Uid, trigger_id: u64);` | `event_cancel(GET_TRIG_WAIT(trig))`: forget a queued wait, if any. Called when a trigger is detached or its script extracted, so a stale wait can never resume a later trigger that reuses the id. | Do nothing. | `driver` |
| `fn mud_command(&mut self, ch: Uid, line: &str);` | `command_interpreter(ch, line)`: have a character — any character, a player included — perform an ordinary game command. This is the fallback for a mob script line that is not a script command (`say`, `give`, `north`, a social…), and what `force`, `at` and a remembered command run. The command may itself fire triggers. | Do nothing. | `commands::mob`, `commands::obj`, `commands::wld`, `dispatch`, `driver` |
| `fn mud_command_name(&self, prefix: &str) -> Option<String>;` | The `mudcommand` text function: the full name of the first game command that `prefix` abbreviates (the first `cmd_info` entry with `strncmp(command, prefix, strlen(prefix)) == 0`), or `None`. | `None` | `text` |
| `fn script_log(&mut self, message: &str);` | `script_log`: a script error a builder should see. | Do nothing. (builders lose the script errors) | `commands::shared`, `dispatch`, `driver`, `expr`, `fields_room`, `host`, `magic`, `subst` |
| `fn unsupported(&mut self, what: &str, detail: &str);` | Something DG does that this host does not model. `what` is a short stable key (`"char.pref"`, `"mhunt"`); `detail` is free text. | Do nothing. | (host use only) |

`unsupported` is for the host's own use (a host that does not model a primitive reports it there); no VM module calls it.

## WorldHost

Defined in `host.rs`. 25 methods. The world graph every module shares: what exists, what it is called, where it is. The `Vec`s are in the order the C walks the corresponding linked list (head first); script behaviour depends on that order.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn kind(&self, uid: Uid) -> Option<Kind>;` | What kind of entity this id names, or `None` if it names nothing live. | `None` | `driver`, `lookup` |
| `fn is_npc(&self, ch: Uid) -> bool;` | `IS_NPC(ch)`. | `false` | `commands::comm`, `commands::misc`, `commands::mob`, `commands::obj`, `commands::shared`, `commands::wld`, `dispatch`, `driver`, `fields_char`, `lookup`, `subst` |
| `fn valid_dg_target(&self, ch: Uid, allow_gods: bool) -> bool;` | `valid_dg_target(ch, allow_gods ? DG_ALLOW_GODS : 0)` (`dg_misc.c`): whether scripts may target this character. NPCs and ordinary playing characters always are. | `is_npc(ch) \|\| level(ch) < LVL_IMMORT` | `commands::mob`, `commands::obj`, `commands::wld`, `dispatch`, `fields_char`, `lookup`, `subst` |
| `fn name(&self, uid: Uid) -> String;` | `GET_NAME(ch)` / `obj->short_description` / `room->name`. | empty string | `commands::comm`, `commands::misc`, `commands::shared`, `driver`, `expr`, `fields_char`, `fields_obj`, `fields_room`, `host`, `lookup`, `magic`, `subst`, `types` |
| `fn keywords(&self, uid: Uid) -> String;` | The keyword list names are matched against: `ch->player.name` (for a player, their name) / `obj->name`. | empty string | `commands::mob`, `fields_char`, `fields_obj`, `lookup` |
| `fn vnum(&self, uid: Uid) -> i32;` | `GET_MOB_VNUM(ch)` / `GET_OBJ_VNUM(obj)` / `room->number`. An entity with no prototype (a player, an object made without one) answers `-1`: the C's `NOBODY` / `NOTHING` is `(IDXTYPE)~0`, 65535, and the VM maps any negative vnum to it (`crate::lookup::c_vnum`). Vnums are 16 bits in the reference build (`CIRCLE_UNSIGNED_INDEX`), so a host never reports one above 65534. | `-1` | `commands::obj`, `commands::shared`, `commands::wld`, `dispatch`, `driver`, `expr`, `fields_char`, `fields_obj`, `fields_room`, `host`, `lookup`, `subst` |
| `fn in_room(&self, uid: Uid) -> Option<Uid>;` | `IN_ROOM(ch)` / `IN_ROOM(obj)`: the room a character is in, or the room an object lies in (`None` when it is carried, worn or contained). | `None` | `commands::comm`, `commands::misc`, `commands::mob`, `commands::obj`, `commands::shared`, `commands::wld`, `dispatch`, `driver`, `fields_char`, `fields_obj`, `lookup`, `subst` |
| `fn room_people(&self, room: Uid) -> Vec<Uid>;` | `room->people`. | empty | `commands::comm`, `commands::mob`, `commands::obj`, `commands::shared`, `commands::wld`, `dispatch`, `fields_char`, `fields_room`, `lookup`, `subst` |
| `fn room_contents(&self, room: Uid) -> Vec<Uid>;` | `room->contents`. | empty | `commands::comm`, `commands::mob`, `commands::obj`, `commands::wld`, `dispatch`, `driver`, `fields_obj`, `fields_room`, `lookup`, `subst` |
| `fn carrying(&self, ch: Uid) -> Vec<Uid>;` | `ch->carrying`. | empty | `commands::comm`, `commands::mob`, `dispatch`, `driver`, `fields_char`, `fields_obj`, `lookup`, `subst` |
| `fn equipment(&self, ch: Uid, pos: i32) -> Option<Uid>;` | `GET_EQ(ch, pos)`, `pos` in `0..NUM_WEARS` (see `crate::lookup`). | `None` | `commands::mob`, `commands::obj`, `commands::shared`, `dispatch`, `fields_char`, `lookup` |
| `fn obj_contains(&self, obj: Uid) -> Vec<Uid>;` | `obj->contains`. | empty | `dispatch`, `fields_obj`, `lookup` |
| `fn obj_carried_by(&self, obj: Uid) -> Option<Uid>;` | `obj->carried_by`. | `None` | `commands::obj`, `commands::shared`, `dispatch`, `fields_obj`, `lookup` |
| `fn obj_worn_by(&self, obj: Uid) -> Option<Uid>;` | `obj->worn_by`. | `None` | `commands::obj`, `commands::shared`, `fields_obj`, `lookup` |
| `fn obj_in_obj(&self, obj: Uid) -> Option<Uid>;` | `obj->in_obj`. | `None` | `commands::obj`, `commands::shared`, `dispatch`, `fields_obj`, `lookup` |
| `fn character_list(&self) -> Vec<Uid>;` | `character_list`. | empty | `commands::mob`, `dispatch`, `lookup` |
| `fn object_list(&self) -> Vec<Uid>;` | `object_list`. | empty | `dispatch`, `lookup` |
| `fn real_room(&self, vnum: i32) -> Option<Uid>;` | `real_room(vnum)`: the room with this vnum. | `None` | `commands::obj`, `commands::shared`, `commands::wld`, `fields_room`, `lookup`, `subst` |
| `fn can_see(&self, ch: Uid, target: Uid) -> bool;` | `CAN_SEE(ch, target)` for a character target, `CAN_SEE_OBJ(ch, target)` for an object. | `true` (everything is visible) | `commands::comm`, `commands::mob`, `dispatch`, `fields_char`, `lookup`, `subst` |
| `fn level(&self, ch: Uid) -> i32;` | `GET_LEVEL(ch)`. | `0` | `commands::comm`, `commands::misc`, `commands::mob`, `commands::shared`, `dispatch`, `fields_char`, `magic` |
| `fn is_dead(&self, ch: Uid) -> bool;` | `DEAD(ch)`: extracted this pulse and awaiting the sweep. | `false` | `dispatch`, `fields_char` |
| `fn obj_type(&self, obj: Uid) -> i32;` | `GET_OBJ_TYPE(obj)`: the `ITEM_*` number (`crate::types::ITEM_CONTAINER` is 15). `0` for something that is not an object. | `0` | `commands::shared`, `fields_obj`, `lookup` |
| `fn wear_flags(&self, obj: Uid) -> u32;` | `GET_OBJ_WEAR(obj)` as a bitvector: bit `n` is `ITEM_WEAR_*` number `n` (`0` take, `1` finger, … `14` hold; see `crate::lookup::can_wear_on_pos`). | `0` | `commands::shared`, `lookup` |
| `fn script_mem(&self, ch: Uid) -> Vec<ScriptMemory>;` | `SCRIPT_MEM(ch)`: who this mob remembers, head first. | empty | `commands::mob`, `dispatch` |
| `fn set_script_mem(&mut self, ch: Uid, memory: Vec<ScriptMemory>);` | Replace `SCRIPT_MEM(ch)`. | Do nothing. (mobs cannot remember) | `commands::mob`, `dispatch` |

`valid_dg_target` is the C function (`dg_misc.c`) rather than a stored fact; it combines `IS_NPC`, `ch->desc && STATE(ch->desc) != CON_PLAYING`, `GET_LEVEL`, `LVL_GRGOD` and `PRF_NOHASSLE`, in this order, and a host implements it as the C does (the `PRF_NOHASSLE` test is the same bit `CharHost::char_pref_flagged(ch, PRF_NOHASSLE)` reads).

`vnum`: an entity with no prototype (a player, an object made without one) answers a negative number, conventionally `-1`. The C's `NOBODY` / `NOTHING` is `(IDXTYPE)~0`, 65535 in the reference build (`CIRCLE_UNSIGNED_INDEX 1`), and the VM maps every negative vnum to it (`lookup::c_vnum`). Vnums a host reports are in `0..=65534`; the VM narrows every vnum a script supplies to 16 bits (`lookup::idx_vnum`) before comparing or calling `real_room`.

`equipment(ch, pos)`: `pos` is a wear position, see "Wear positions" below. `wear_flags` bit `n` is `ITEM_WEAR_*` number `n`, see "Item wear flags".

## SubstHost

Defined in `subst.rs`. 2 methods. Two facts only `%…%` substitution reads.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn subst_happy_hour(&self) -> HappyHour;` | `happy_data`: the happy-hour rates and the ticks left. All zero when there is no happy hour. | `HappyHour::default()` (all zero) | `subst` |
| `fn subst_diagonal_dirs(&self) -> bool;` | `CONFIG_DIAGONAL_DIRS`: whether diagonal directions exist (`DIR_COUNT` is 10 when set, 6 otherwise). `false` by default. | `false` | `commands::shared`, `dispatch`, `subst` |

## CharHost

Defined in `fields_char.rs`. 27 methods. Character attributes. Player-only attributes (practices, conditions, quest points, skills) on an NPC read and write the one `dummy_mob` record the C shares between all NPCs (`CHECK_PLAYER_SPECIAL`); a faithful host keeps that one shared record.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn char_stat(&self, ch: Uid, stat: CharStat) -> i32;` | The value of one numeric attribute (see `CharStat`). | `0` | `commands::comm`, `commands::misc`, `commands::shared`, `dispatch`, `fields_char` |
| `fn char_fact(&self, ch: Uid, fact: CharFact) -> i32;` | A read-only attribute (see `CharFact`). | `0` | `fields_char` |
| `fn char_set_stat(&mut self, ch: Uid, stat: CharStat, value: i32);` | Store one numeric attribute: `GET_xxx(ch) = value`. The VM has already narrowed `value` to the C field's width. | Do nothing. | `commands::misc`, `fields_char` |
| `fn char_set_level(&mut self, ch: Uid, level: i32);` | `GET_LEVEL(ch) = level`. Reads go through `WorldHost::level`. | Do nothing. | `fields_char` |
| `fn char_abil(&self, ch: Uid, abil: CharAbil) -> i32;` | `ch->aff_abils.<abil>`: the ability including affects (`GET_STR(ch)`). | `0` | `fields_char` |
| `fn char_real_abil(&self, ch: Uid, abil: CharAbil) -> i32;` | `ch->real_abils.<abil>`: the ability without affects. | `0` | `fields_char` |
| `fn char_set_real_abil(&mut self, ch: Uid, abil: CharAbil, value: i32);` | `ch->real_abils.<abil> = value`. | Do nothing. | `fields_char` |
| `fn char_affect_total(&mut self, ch: Uid);` | `affect_total(ch)`: recompute `aff_abils` from `real_abils` and affects. | Do nothing. | `fields_char` |
| `fn char_armor_class(&self, ch: Uid) -> i32;` | `compute_armor_class(ch)`. | `0` | `fields_char` |
| `fn char_update_pos(&mut self, ch: Uid);` | `update_pos(ch)`: fix `GET_POS` from `GET_HIT`. | Do nothing. | `commands::misc`, `fields_char` |
| `fn char_gain_exp(&mut self, ch: Uid, gain: i32);` | `gain_exp(ch, gain)`. | Do nothing. | `fields_char` |
| `fn char_increase_gold(&mut self, ch: Uid, amt: i32);` | `increase_gold(ch, amt)`. | Do nothing. | `fields_char` |
| `fn char_fighting(&self, ch: Uid) -> Option<Uid>;` | `FIGHTING(ch)`. | `None` | `commands::mob`, `dispatch`, `fields_char` |
| `fn char_master(&self, ch: Uid) -> Option<Uid>;` | `ch->master`. | `None` | `commands::mob`, `fields_char` |
| `fn char_first_follower(&self, ch: Uid) -> Option<Uid>;` | `ch->followers->follower`: the head of the follower list. | `None` | `fields_char` |
| `fn char_affected_by_spell(&self, ch: Uid, spell: i32) -> bool;` | `affected_by_spell(ch, spell)`. | `false` | `fields_char` |
| `fn char_skill(&self, ch: Uid, skill: i32) -> i32;` | `GET_SKILL(ch, skill)`. | `0` | `fields_char` |
| `fn char_set_skill(&mut self, ch: Uid, skill: i32, value: i32);` | `SET_SKILL(ch, skill, value)`. | Do nothing. | `fields_char` |
| `fn char_act_flagged(&self, ch: Uid, bit: u32) -> bool;` | `IS_SET_AR(MOB_FLAGS(ch), bit)`: a bit of the act array (`MOB_*` for an NPC, `PLR_*` for a player). The caller adds any `IS_NPC` test. | `false` | `commands::comm`, `fields_char` |
| `fn char_set_act_flag(&mut self, ch: Uid, bit: u32, on: bool);` | `SET_BIT_AR`/`REMOVE_BIT_AR(PLR_FLAGS(ch), bit)`. | Do nothing. | `fields_char` |
| `fn char_pref_flagged(&self, ch: Uid, bit: u32) -> bool;` | `IS_SET_AR(PRF_FLAGS(ch), bit)`. The caller adds the `IS_NPC` test. | `false` | `commands::mob`, `commands::shared`, `dispatch`, `fields_char` |
| `fn char_set_short_descr(&mut self, ch: Uid, text: &str);` | `ch->player.short_descr = strdup(text)`. Reads go through `WorldHost::name`. | Do nothing. | `magic` |
| `fn char_title(&self, ch: Uid) -> Option<String>;` | `GET_TITLE(ch)`; `None` when the pointer is NULL. | `None` | `fields_char` |
| `fn char_set_title(&mut self, ch: Uid, title: &str);` | Replace `GET_TITLE(ch)`. | Do nothing. | `fields_char` |
| `fn char_quest(&self, ch: Uid) -> i32;` | `GET_QUEST(ch)`; `-1` when `GET_QUEST(ch) == NOTHING` (this tree's unsigned `NOTHING` is read as -1 by the VM; a host may also return the raw 65535, which no quest has). | `-1` (`NOTHING`) | `fields_char` |
| `fn char_quest_exists(&self, vnum: i32) -> bool;` | `real_quest(vnum) != NOTHING`. | `false` | `fields_char` |
| `fn char_quest_completed(&self, ch: Uid, vnum: i32) -> bool;` | `is_complete(ch, vnum)`. | `false` | `fields_char` |

`char_act_flagged` / `char_pref_flagged` take a bit number of the act / preference bit array (names in `fields_char::ACTION_BITS` / `PREFERENCE_BITS`; the numbers the VM itself uses are in "Flag numbers"). `MOB_ISNPC` and `DEAD` (`MOB_NOTDEADYET` / `PLR_NOTDEADYET`) are facts `WorldHost::is_npc` and `is_dead` already provide: the VM never asks the act array for them, so a host need not store them there.

## ObjHost

Defined in `fields_obj.rs`. 17 methods. Object attributes the `%obj.field%` table reads and `otransform`/`oset`/`osetval`/`otimer` write.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn obj_val(&self, obj: Uid, idx: usize) -> i32;` | `GET_OBJ_VAL(obj, idx)`, `idx` in `0..4`. | `0` | `fields_obj` |
| `fn obj_set_val(&mut self, obj: Uid, idx: usize, value: i32);` | `GET_OBJ_VAL(obj, idx) = value`, `idx` in `0..4`. | Do nothing. | `commands::obj` |
| `fn obj_timer(&self, obj: Uid) -> i32;` | `GET_OBJ_TIMER(obj)`. | `0` | `fields_obj` |
| `fn obj_set_timer(&mut self, obj: Uid, value: i32);` | `GET_OBJ_TIMER(obj) = value`. | Do nothing. | `commands::obj` |
| `fn obj_cost(&self, obj: Uid) -> i32;` | `GET_OBJ_COST(obj)`. | `0` | `fields_obj` |
| `fn obj_set_cost(&mut self, obj: Uid, value: i32);` | `GET_OBJ_COST(obj) = value`. | Do nothing. | `fields_obj` |
| `fn obj_rent(&self, obj: Uid) -> i32;` | `GET_OBJ_RENT(obj)` (`cost_per_day`). | `0` | `fields_obj` |
| `fn obj_set_rent(&mut self, obj: Uid, value: i32);` | `GET_OBJ_RENT(obj) = value`. | Do nothing. | `fields_obj` |
| `fn obj_weight(&self, obj: Uid) -> i32;` | `GET_OBJ_WEIGHT(obj)`. | `0` | `fields_obj` |
| `fn obj_set_weight(&mut self, obj: Uid, value: i32);` | `GET_OBJ_WEIGHT(obj) = value` (the field only: no carried-weight bookkeeping, as in the C). | Do nothing. | `fields_obj` |
| `fn obj_extra_flagged(&self, obj: Uid, bit: usize) -> bool;` | `IS_SET_AR(GET_OBJ_EXTRA(obj), bit)`. | `false` | `fields_obj` |
| `fn obj_affect_flagged(&self, obj: Uid, bit: usize) -> bool;` | `IS_SET_AR(GET_OBJ_AFFECT(obj), bit)`. | `false` | `fields_obj` |
| `fn obj_apply(&self, obj: Uid, slot: usize) -> (u8, i8);` | `(obj->affected[slot].location, obj->affected[slot].modifier)`, `slot` in `0..MAX_OBJ_AFFECT`. | `(0, 0)` | `fields_obj` |
| `fn obj_set_apply(&mut self, obj: Uid, slot: usize, location: u8, modifier: i8);` | Assign `obj->affected[slot].location` and `.modifier`. | Do nothing. | `fields_obj` |
| `fn obj_set_alias(&mut self, obj: Uid, name: &str);` | `obj->name = strdup(name)` (`oset_alias`, after its length check). | Do nothing. | `fields_obj` |
| `fn obj_set_short_desc(&mut self, obj: Uid, text: &str);` | `obj->short_description = strdup(text)` (`oset_short_description`). | Do nothing. | `fields_obj` |
| `fn obj_set_long_desc(&mut self, obj: Uid, text: &str);` | `obj->description = strdup(text)` (`oset_long_description`). | Do nothing. | `fields_obj` |

## RoomHost

Defined in `fields_room.rs`. 6 methods. Room attributes the `%room.field%` table reads. Exits are written through `CmdHost::cmd_exit_set` / `cmd_exit_purge`.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn room_exit(&self, room: Uid, dir: usize) -> Option<RoomExit>;` | `R_EXIT(room, dir)`, `dir` in `0..NUM_OF_DIRS` (the field table only asks for `0..6`, `NORTH`..`DOWN`): `None` when the room has no exit that way. | `None` | `commands::shared`, `fields_room`, `subst` |
| `fn room_sector(&self, room: Uid) -> i32;` | `r->sector_type`. | `0` | `fields_room` |
| `fn room_flagged(&self, room: Uid, flag: usize) -> bool;` | `IS_SET_AR(ROOM_FLAGS(room), flag)`. | `false` | `commands::shared`, `fields_room` |
| `fn room_zone_number(&self, room: Uid) -> i32;` | `zone_table[r->zone].number`. | `0` | `commands::comm`, `dispatch`, `fields_room` |
| `fn room_zone_name(&self, room: Uid) -> String;` | `zone_table[r->zone].name`. | empty string | `fields_room` |
| `fn room_sky(&self) -> i32;` | `weather_info.sky` (`SKY_CLOUDLESS`..`SKY_LIGHTNING`). | `0` | `fields_room` |

`room_exit` returns `RoomExit { to_room: Option<Uid>, key: i32, info: u32 }`: `to_room` is `None` for an exit that leads to `NOWHERE` (the exit still exists); `key` is the C value (65535 for "no key"); `info` is `exit_info` (bit 0 `EX_ISDOOR`, 1 `EX_CLOSED`, 2 `EX_LOCKED`, 3 `EX_PICKPROOF`). `dir` is `0..NUM_OF_DIRS` (see "Directions").

## CmdHost

Defined in `commands/mod.rs`. 33 methods. The world effects of the script commands (`mload`, `mpurge`, `mteleport`, `mdoor`, …). Moves and extractions follow the C handlers; `cmd_extract_char` is deferred (`is_dead` true until the host sweeps), `cmd_extract_obj` is immediate.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn cmd_huh(&self) -> String;` | `CONFIG_HUH`: what a command a character may not use answers. | `"Huh?!?\r\n"` | `commands::mob` |
| `fn cmd_send(&mut self, ch: Uid, text: &str);` | `send_to_char(ch, "%s", text)`: queue `text` for the character's connection. Does nothing for a character with no descriptor. | Do nothing. | `commands::comm`, `commands::misc`, `commands::mob`, `commands::shared` |
| `fn cmd_desc(&self, ch: Uid) -> Option<Option<Uid>>;` | `ch->desc`: `None` when `ch->desc` is `NULL`, else `Some` of `ch->desc->original`, the player behind a switched mob (`None` inside when the descriptor is not switched). The VM reads descriptor presence from the outer `Option` and applies the policy for an unswitched descriptor itself. | `None` (no descriptor) | `commands::mob`, `commands::shared` |
| `fn cmd_mudlog(&mut self, log_type: i32, level: i32, text: &str);` | `mudlog(log_type, level, TRUE, "%s", text)`: `log_type` is `OFF` 0, `BRF` 1, `NRM` 2 or `CMP` 3 (`crate::types::MUDLOG_BRF` and friends); `level` is the lowest immortal level that sees the line. | Do nothing. | `commands::misc`, `dispatch` |
| `fn cmd_syslog(&mut self, text: &str);` | `log("%s", text)`: a line for the server log only (the `SYSERR` lines of `act` and the object and room handlers). Not shown to immortals. | Do nothing. | `commands::comm`, `commands::mob`, `commands::shared`, `commands::wld` |
| `fn cmd_playing_characters(&self) -> Vec<Uid>;` | The characters of `descriptor_list` whose `connected` is `CON_PLAYING` (`i->character` for each `!i->connected`), in list order. | empty | `commands::comm`, `commands::mob`, `dispatch` |
| `fn cmd_is_charmed(&self, ch: Uid) -> bool;` | `AFF_FLAGGED(ch, AFF_CHARM)`. | `false` | `commands::mob`, `dispatch` |
| `fn cmd_invis_level(&self, ch: Uid) -> i32;` | `GET_INVIS_LEV(ch)`. | `0` | `commands::misc` |
| `fn cmd_die(&mut self, ch: Uid);` | `die(ch, NULL)`. | Do nothing. | `commands::misc` |
| `fn cmd_stop_fighting(&mut self, ch: Uid);` | `stop_fighting(ch)`. | Do nothing. | `commands::mob` |
| `fn cmd_hit_victim(&mut self, ch: Uid, victim: Uid);` | `hit(ch, victim, TYPE_UNDEFINED)`. | Do nothing. | `commands::mob` |
| `fn cmd_set_hunting(&mut self, ch: Uid, victim: Uid);` | `HUNTING(ch) = victim`. | Do nothing. | `commands::mob` |
| `fn cmd_unfollow(&mut self, ch: Uid);` | The "stop following someone else first" block of `do_mfollow`: remove `ch`'s entry from `ch->master->followers` and set `ch->master = NULL`. It is not `stop_follower`: no affect is removed and nothing is shown. | Do nothing. | `commands::mob` |
| `fn cmd_follow(&mut self, ch: Uid, leader: Uid);` | The tail of `do_mfollow`: `ch->master = leader` and a new follower entry for `ch` at the head of `leader->followers`. | Do nothing. | `commands::mob` |
| `fn cmd_exit_purge(&mut self, room: Uid, dir: usize);` | `do_mdoor` "purge": free the exit if there is one (`rm->dir_option[dir] = NULL`). | Do nothing. | `commands::shared` |
| `fn cmd_exit_set(&mut self, room: Uid, dir: usize, field: ExitField);` | Set one field of an exit. When the room has no exit that way, first `CREATE`s one: every field zero, which makes `to_room` room number 0 (the first room in the world table), not `NOWHERE`. | Do nothing. | `commands::shared` |
| `fn cmd_house_can_enter(&self, ch: Uid, room_vnum: i32) -> bool;` | `House_can_enter(ch, room_vnum)`. | `true` | `commands::shared` |
| `fn cmd_zone_of_vnum(&self, vnum: i32) -> Option<i32>;` | `real_zone_by_thing(vnum)`: the zone whose vnum range holds `vnum`, `None` for `NOWHERE`. The zone is named by its number (`zone_table[zone].number`), the value `RoomHost::room_zone_number` reports, rather than by its table index; the two are in one-to-one correspondence, so comparing them selects the same rooms. | `None` | `commands::shared` |
| `fn cmd_rooms_in_range(&self, start: i32, finish: i32) -> Vec<Uid>;` | The rooms whose vnum is in `start..=finish`, in world-table order. | empty | `commands::comm`, `dispatch` |
| `fn cmd_char_from_room(&mut self, ch: Uid);` | `char_from_room(ch)`. | Do nothing. | `commands::mob`, `commands::shared` |
| `fn cmd_char_to_room(&mut self, ch: Uid, room: Uid);` | `char_to_room(ch, room)`. | Do nothing. | `commands::mob`, `commands::shared`, `magic` |
| `fn cmd_obj_to_room(&mut self, obj: Uid, room: Uid);` | `obj_to_room(obj, room)`. | Do nothing. | `commands::obj`, `commands::shared`, `commands::wld` |
| `fn cmd_obj_to_char(&mut self, obj: Uid, ch: Uid);` | `obj_to_char(obj, ch)`. | Do nothing. | `commands::shared` |
| `fn cmd_obj_to_obj(&mut self, obj: Uid, container: Uid);` | `obj_to_obj(obj, container)`. | Do nothing. | `commands::shared` |
| `fn cmd_obj_detach(&mut self, obj: Uid);` | Whichever of `obj_from_room`, `obj_from_char` or `obj_from_obj` applies to where the object is. Not for a worn object (`CmdHost::cmd_unequip_char`). | Do nothing. | `commands::obj`, `commands::wld` |
| `fn cmd_equip_char(&mut self, ch: Uid, obj: Uid, pos: i32);` | `equip_char(ch, obj, pos)`. | Do nothing. | `commands::mob`, `commands::obj`, `commands::shared` |
| `fn cmd_unequip_char(&mut self, ch: Uid, pos: i32) -> Option<Uid>;` | `unequip_char(ch, pos)`: the object that was worn there, now carried by nobody. | `None` | `commands::mob`, `commands::obj` |
| `fn cmd_extract_char(&mut self, ch: Uid);` | `extract_char(ch)`: deferred, as `crate::host` describes. | Do nothing. | `commands::mob`, `commands::obj`, `commands::wld`, `magic` |
| `fn cmd_extract_obj(&mut self, obj: Uid);` | `extract_obj(obj)`: immediate, and takes the contents with it. | Do nothing. | `commands::mob`, `commands::obj`, `commands::wld` |
| `fn cmd_read_mobile(&mut self, vnum: i32) -> Option<Uid>;` | `read_mobile(vnum, VIRTUAL)`: a new mob, in no room; `None` for a vnum that does not exist. | `None` | `commands::mob`, `commands::shared`, `magic` |
| `fn cmd_read_object(&mut self, vnum: i32) -> Option<Uid>;` | `read_object(vnum, VIRTUAL)`: a new object, in no place; `None` for a vnum that does not exist. | `None` | `commands::obj`, `commands::shared` |
| `fn cmd_overwrite_mob(&mut self, ch: Uid, from: Uid, keep_hp: bool);` | The `memcpy(ch, &tmpmob, sizeof(*ch))` of `do_mtransform`: copy the prototype data of mob `from` (a temporary mob from `CmdHost::cmd_read_mobile`) over `ch` while keeping `ch`'s identity (script id, affects, carried items, scripts, memory, events, list links, followers, master, group, was-in-room, gold, position, carrying weight and count, fighting and hunting) and, when `keep_hp`, its hit points, maximum hit points and experience. `ch->nr` is restored to its old value afterwards (`ch->nr = this_rnum`), so the vnum of `ch` is NOT changed by a transform. | Do nothing. | `commands::mob` |
| `fn cmd_overwrite_obj(&mut self, obj: Uid, from: Uid);` | The `memcpy(obj, &tmpobj, sizeof(*obj))` of `do_otransform`: copy the prototype data of object `from` (a temporary object from `CmdHost::cmd_read_object`) over `obj` while keeping its place (room, carrier, wearer and wear position, container), contents, script id, scripts and list links. Unlike mobs, `item_number` is copied, so the vnum of `obj` becomes that of `from`. | Do nothing. | `commands::obj` |

`cmd_exit_set` takes `ExitField::{Description(String), Flags(i32), Key(i32), Keyword(String), ToRoom(Option<Uid>)}` and, when the room has no exit that way, first creates one with every field zero (`to_room` room number 0, not `NOWHERE`), as the C `CREATE`s it. `cmd_mudlog` types: `OFF` 0, `BRF` 1, `NRM` 2, `CMP` 3 (`types::MUDLOG_*`).

## MagicHost

Defined in `magic.rs`. 6 methods. Spells and affects for `dg_cast` and `dg_affect`.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn magic_spell_name(&self, spellnum: i32) -> Option<String>;` | `spell_info[spellnum].name` for `spellnum` in `1..=TOP_SPELL_DEFINE`; `None` for a slot the host does not define (read as `"!UNUSED!"`, the C's name for an unused slot). | `None` | `dispatch`, `magic` |
| `fn magic_spell_targets(&self, spellnum: i32) -> u32;` | `spell_info[spellnum].targets`: the `TAR_*` bits. `0` for a spell the host does not know. | `0` | `magic` |
| `fn magic_spell_routines(&self, spellnum: i32) -> u32;` | `spell_info[spellnum].routines`: the `MAG_*` bits. `0` for a spell the host does not know. | `0` | `magic` |
| `fn magic_call_magic(&mut self, caster: Uid, victim: Option<Uid>, obj: Option<Uid>, spellnum: i32, level: i32);` | `call_magic(caster, victim, obj, spellnum, level, CAST_SPELL)`: the whole of the C function, including the `cast_wtrigger`, `cast_otrigger` and `cast_mtrigger` calls that may refuse the spell, and the `ROOM_NOMAGIC` / `ROOM_PEACEFUL` / `MOB_NOKILL` refusals. | Do nothing. | `magic` |
| `fn magic_remove_dg_affects(&mut self, ch: Uid, spell: i32);` | `affect_from_char(ch, spell)` (`spell` is `SPELL_DG_AFFECT`): remove every affect of that spell from the character. | Do nothing. | `magic` |
| `fn magic_add_dg_affect(&mut self, ch: Uid, spell: i32, location: i32, modifier: i8, duration: i16, bit: Option<u32>);` | `new_affect(&af); af.spell = spell; af.duration = duration; af.modifier = modifier; af.location = location;` then, when `bit` is given, `SET_BIT_AR(af.bitvector, bit)`; `affect_to_char(ch, &af)`. `location` is `APPLY_NONE` (`0`) for a bit affect. The VM has already narrowed `modifier` to the C's `sbyte` and `duration` to its `sh_int`. | Do nothing. | `magic` |

Spell numbers run `1..=TOP_SPELL_DEFINE` (299); `SPELL_DG_AFFECT` is 298. `magic_spell_targets` bits are the `TAR_*` constants of `magic.rs`; `magic_spell_routines` bits are the `MAG_*` bits of `spells.h` (the VM reads only `MAG_GROUPS`, bit 5). For an object or room caster the VM makes a proxy mob with `CmdHost::cmd_read_mobile(DG_CASTER_PROXY)` (vnum 1), `cmd_char_to_room` and `cmd_extract_char`.

## DispatchHost

Defined in `dispatch.rs`. 1 method. What `dg_triggers.c` needs beyond the shared primitives. Position, fighting, hit points, charm, visibility, the people / contents / equipment lists, room zones, the playing characters, spell names and memory lists are read through `WorldHost`, `CharHost`, `RoomHost`, `MagicHost` and `CmdHost`.

| Method | C expression | Unmodelled | Called by |
|---|---|---|---|
| `fn disp_free_proto_script(&mut self, uid: Uid);` | `free_proto_script(&mob_proto[GET_MOB_RNUM(ch)], MOB_TRIGGER)` / `free_proto_script(&obj_proto[GET_OBJ_RNUM(obj)], OBJ_TRIGGER)`: drop the script from the prototype of this mob or object, so nothing loaded from it afterwards has triggers. Does nothing for an entity with no prototype (`GET_MOB_RNUM(ch) == NOBODY`, a player). Called when a `load` trigger recursed past `crate::types::MAX_SCRIPT_DEPTH`. | Do nothing. | `dispatch` |

## Selectors and enums

### `CharStat`

`CharHost::char_stat` / `char_set_stat`: one numeric attribute. The VM narrows a value to the C field's width before `char_set_stat` (the width is in the C column).

| Variant | C expression |
|---|---|
| `CharStat::Align` | `GET_ALIGNMENT(ch)`. |
| `CharStat::Damroll` | `GET_DAMROLL(ch)` (`sbyte`). |
| `CharStat::Hitroll` | `GET_HITROLL(ch)` (`sbyte`). |
| `CharStat::Hit` | `GET_HIT(ch)` (`sh_int`). |
| `CharStat::MaxHit` | `GET_MAX_HIT(ch)` (`sh_int`). |
| `CharStat::Mana` | `GET_MANA(ch)` (`sh_int`). |
| `CharStat::MaxMana` | `GET_MAX_MANA(ch)` (`sh_int`). |
| `CharStat::Move` | `GET_MOVE(ch)` (`sh_int`). |
| `CharStat::MaxMove` | `GET_MAX_MOVE(ch)` (`sh_int`). |
| `CharStat::Sex` | `GET_SEX(ch)`. |
| `CharStat::Class` | `GET_CLASS(ch)`. |
| `CharStat::Pos` | `GET_POS(ch)`. |
| `CharStat::Practices` | `GET_PRACTICES(ch)`. |
| `CharStat::QuestPoints` | `GET_QUESTPOINTS(ch)`. |
| `CharStat::WaitState` | `GET_WAIT_STATE(ch)` (`ch->wait`). |
| `CharStat::Drunk` | `GET_COND(ch, DRUNK)` (`sbyte`). |
| `CharStat::Hunger` | `GET_COND(ch, HUNGER)` (`sbyte`). |
| `CharStat::Thirst` | `GET_COND(ch, THIRST)` (`sbyte`). |
| `CharStat::SaveBreath` | `GET_SAVE(ch, SAVING_BREATH)` (`sh_int`). |
| `CharStat::SavePara` | `GET_SAVE(ch, SAVING_PARA)` (`sh_int`). |
| `CharStat::SavePetri` | `GET_SAVE(ch, SAVING_PETRI)` (`sh_int`). |
| `CharStat::SaveRod` | `GET_SAVE(ch, SAVING_ROD)` (`sh_int`). |
| `CharStat::SaveSpell` | `GET_SAVE(ch, SAVING_SPELL)` (`sh_int`). |

### `CharFact`

`CharHost::char_fact`: read-only attributes (change gold and experience with `char_increase_gold` / `char_gain_exp`).

| Variant | C expression |
|---|---|
| `CharFact::Gold` | `GET_GOLD(ch)`. |
| `CharFact::Exp` | `GET_EXP(ch)`. |
| `CharFact::Weight` | `GET_WEIGHT(ch)` (`ubyte`). |

### `CharAbil`

`CharHost::char_abil` / `char_real_abil` / `char_set_real_abil`: `aff_abils` (with affects) and `real_abils` (without); `StrAdd` is `str_add`.

| Variant | C expression |
|---|---|
| `CharAbil::Str` | `GET_STR` |
| `CharAbil::StrAdd` | `str_add`. |
| `CharAbil::Int` | `GET_INT` |
| `CharAbil::Wis` | `GET_WIS` |
| `CharAbil::Dex` | `GET_DEX` |
| `CharAbil::Con` | `GET_CON` |
| `CharAbil::Cha` | `GET_CHA` |

### Other argument types

| Type | Defined in | Shape |
|---|---|---|
| `Uid` | `types.rs` | `i64`. Players `1..MOB_ID_BASE` (10 000 000); mobs from `MOB_ID_BASE`; a room is `ROOM_ID_BASE` (20 000 000) + vnum; objects from `OBJ_ID_BASE` (30 000 000). Text form `}` + decimal (`UID_CHAR`). `types::uid_kind` says what an id's range implies |
| `Kind` | `types.rs` | `Char` (player or mob), `Obj`, `Room` |
| `MudTime` | `types.rs` | `time_info`: `hours`, `day`, `month`, `year` (all `i32`) |
| `HappyHour` | `subst.rs` | `qp_rate`, `exp_rate`, `gold_rate`, `ticks_left` (`happy_data`) |
| `RoomExit` | `fields_room.rs` | `to_room: Option<Uid>`, `key: i32`, `info: u32` |
| `ExitField` | `commands/mod.rs` | `Description(String)`, `Flags(i32)`, `Key(i32)`, `Keyword(String)`, `ToRoom(Option<Uid>)` |
| `ScriptMemory` | `types.rs` | `id: Uid`, `cmd: Option<String>` (`None` is the `memory` trigger) |
| `Script`, `TrigProto`, `TrigInstance`, `VmState` | `types.rs` | VM-owned data the host stores: scripts by entity, prototypes by vnum, the one `VmState` |

## Numbers a host interprets

### Directions (`types::DIRS`, `NUM_OF_DIRS` = 10)

`0` north, `1` east, `2` south, `3` west, `4` up, `5` down, `6` northwest, `7` northeast, `8` southeast, `9` southwest (`REV_DIR` gives the opposite). `DIR_COUNT` is 6, or 10 when `SubstHost::subst_diagonal_dirs` is true. The `%room.<dir>%` fields use `0..6`.

### Wear positions (`lookup::WEAR_*`, `NUM_WEARS` = 18)

`0` light, `1` right finger, `2` left finger, `3` neck 1, `4` neck 2, `5` body, `6` head, `7` legs, `8` feet, `9` hands, `10` arms, `11` shield, `12` about, `13` waist, `14` right wrist, `15` left wrist, `16` wield, `17` hold.

### Item wear flags (`WorldHost::wear_flags`, bit `n`)

`0` take, `1` finger, `2` neck, `3` body, `4` head, `5` legs, `6` feet, `7` hands, `8` arms, `9` shield, `10` about, `11` waist, `12` wrist, `13` wield, `14` hold (`lookup::can_wear_on_pos` maps a position to its flag).

### Positions (`types::POS_*`, `CharStat::Pos`)

`0` dead, `1` mortally wounded, `2` incapacitated, `3` stunned, `4` sleeping, `5` resting, `6` sitting, `7` fighting, `8` standing (`fields_char::POSITION_TYPES` has the names).

### Flag numbers

Bit numbers into the C bit arrays. The VM names these (all in `types.rs`): act array `PLR_KILLER` 0, `PLR_THIEF` 1, `PLR_WRITING` 4; preference array `PRF_NOHASSLE` 8, `PRF_COLOR_1` 13, `PRF_COLOR_2` 14; affect array `AFF_CHARM` 22 (`cmd_is_charmed`); room flags `ROOM_INDOORS` 3, `ROOM_PRIVATE` 9, `ROOM_GODROOM` 10, `ROOM_HOUSE` 11. Everything else is passed through from a script's flag name, by its index in the name tables: `fields_char::ACTION_BITS` (act), `PREFERENCE_BITS` (preferences), `fields_obj::EXTRA_BITS` (`obj_extra_flagged`), `AFFECTED_BITS` (`obj_affect_flagged`), `APPLY_TYPES` (`obj_apply` locations), `fields_room::ROOM_BITS` (`room_flagged`), `EXIT_BITS`, `SECTOR_TYPES` (`room_sector`), `SKY_LOOK` (`room_sky`: 0 cloudless … 3 lightning), `fields_obj::ITEM_TYPES` (`obj_type`; `ITEM_CONTAINER` = 15). `obj_val` / `obj_set_val` take `idx` in `0..4`; `obj_apply` slots are `0..MAX_OBJ_AFFECT`.

### Levels

`LVL_IMMORT` 31, `LVL_GOD` 32, `LVL_GRGOD` 33, `LVL_IMPL` 34 (`types.rs`).

### Mud log types

`MUDLOG_OFF` 0, `MUDLOG_BRF` 1, `MUDLOG_NRM` 2, `MUDLOG_CMP` 3 (`cmd_mudlog`).

### Spell constants (`magic.rs`)

`TOP_SPELL_DEFINE` 299, `SPELL_DG_AFFECT` 298, `DG_CASTER_PROXY` 1, `DG_SPELL_LEVEL` 25, `TAR_*` target bits and `MAG_*` routine bits.

### VmState (`BaseHost::vm_state`)

`depth` (`script_driver`'s static), `owner_purged` (`dg_owner_purged`), `act_check` (`comm.c`'s `dg_act_check`), and the `var_subst` / `eval_expr` nesting counters. The host stores one `VmState` and returns it on every call. The host's own half of `act_check`: its `act()` must set `vm_state().act_check = !IS_SET(type, DG_NO_TRIG)` on every call with non-empty text (nested calls too, so a `say` made by an act trigger switches it off for the rest of the outer walk), and its `perform_act` must test that shared field, not the line's own flag, in `IS_NPC(to) && dg_act_check && to != ch` before calling `dispatch::act_mtrigger`; the crate keeps no global state of its own.

## Totals

| Trait | Methods |
|---|---|
| `BaseHost` | 17 |
| `WorldHost` | 25 |
| `SubstHost` | 2 |
| `CharHost` | 27 |
| `ObjHost` | 17 |
| `RoomHost` | 6 |
| `CmdHost` | 33 |
| `MagicHost` | 6 |
| `DispatchHost` | 1 |
| **Total** | **134** |
