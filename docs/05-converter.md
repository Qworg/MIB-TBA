# 05 — The tbaMUD → MIB Converter (`tools/tbaport`)

> Status: **Implemented** (Phase 10).
> Companion documents: [03-tba-pack.md](03-tba-pack.md) (the TBA pack spec),
> the tbaMUD C loader under `/home/qworg/Development/tbamud/src/`.
>
> The authoritative grammar is the C code, not `doc/building.txt`. Line
> references below are to the tbaMUD checkout and were read while writing this
> document.

`tbaport` is a **build-time tool**: it reads the tbaMUD flat-file world and
emits MIB pack YAML plus a machine-readable drop report. It is independent of
the running server and links only the schema crates (`mib-stdlib`, `mib-tba`,
`mib-core`).

---

## 1. Scope and decisions

1. **Output is staged, never written into `content/`.** The default output is
   `target/tbaport/content`; the default report is
   `target/tbaport/drop-report.json`. Swapping the staged world into `content/`
   is a separate, explicit step.
2. **Ids.** Rooms `tba.<vnum>`, NPCs `tba_npc_<vnum>`, items `tba_item_<vnum>`,
   areas `tba_<zone>`. The room vnum is also carried in `RoomDef::num` so the
   zone `[bot, top]` bounds remain recoverable.
3. **Zone membership is by file, not by `[bot, top]`.** Every room in
   `wld/<N>.wld` is emitted under zone `N`; the bounds are used only to place
   `M` resets and to describe the zone. A room outside the bounds is never
   dropped.
4. **Zones are programs.** `M/O/G/E/P/D/R` map 1:1 onto `ZoneCommandKind`
   variants in source order. Each command is wrapped in `ZoneCommand`, which
   carries Circle's per-command `if_flag`; the interpreter honours it (a
   flagged command runs only when the command before it succeeded,
   `reset_zone`/`db.c:2868`). The raw flag is still recorded in the drop report
   (`zones[].command_flags[]`).
5. **`P` uses the container prototype named by `arg3`**, matching
   `reset_zone`'s `get_obj_num(ZCMD.arg3)`.
6. **`stdlib.npcs`** gets name + persona (long description + detailed
   description); **`tba.npc_stats`** carries the Diku numbers and AI flags.
7. **`stdlib.items`** gets name + description + container/tags; **`tba.items`**
   carries kind, slots, values, affects and the economy fields.
8. **DG triggers (`.trg`) are converted.** All 1,461 records are emitted into
`tba.triggers.<zone>.yaml`, one shard per zone (see §2.6 and §3.4). Each record
maps to a `TriggerDef` (`attach_type` + `flags`, with the expanded `types`), and
each owner's ordered trigger list is carried by `TriggersFile.attachments`.
Every script is **analysed** with `dg_vm::analyze`, and the result is written
into the drop report's `trigger_coverage` (lines by class, control and statement
histograms, `wait` forms, script commands, structural errors and the
supported/unsupported mob-command words). `tbaport triage` still writes the
standalone classification `reports/trigger-triage.json`. **Shops (`.shp`)** are
parsed and emitted as `tba.shops` (see §3.6); **quests (`.qst`)** are parsed
and emitted as `stdlib.quests` (see §3.7); **`lib/text`,
`lib/misc/messages`** become `stdlib.help` topics (see §3.8) and
**`lib/misc/socials.new`** becomes `tba.socials` (see §3.9).
9. **Determinism.** Zones are iterated in ascending numeric order and every
   section is a `BTreeMap`; converting the same source twice is byte-identical.
   Rooms are sharded one file per zone (TBA-E01, §6) and the committed drop
   report is emitted with the same command, so it stays diffable.
10. **Dangling references are dropped, never emitted.** Every reference the
    converter writes is resolved against the converted id set first. An exit to
    a missing room is removed; a door whose key names a missing item keeps the
    door but loses the key; a zone reset (`M/O/G/E/P/D/R`) that names a missing
    mob, item, container or room is removed; a shop's produce/room/keeper that
    names a missing record is removed. Each case is recorded in the drop
    report, so a partial (`--only Z`) or a source world with genuinely broken
    references still emits a pack the engine can load.
11. **Every drop is named.** A source field is either mapped to a schema field
    or listed in the report with a reason. `global_drops` carries a feature
    that has no per-record home (room coordinates, the combat-skill non-goals);
    `unmapped` carries a per-record drop; `dangling` carries a reference that
    resolved to nothing.

`unsafe_code` is denied; `unwrap_used`/`expect_used` are denied in library code
(tests may allow them).

---

## 2. Source grammar

### 2.1 Common tokenizer (`src/db.c`)

| Mechanism | C location | Behaviour |
|---|---|---|
| `get_line` | `db.c` (used throughout) | reads one `\n` line, strips delimiter |
| `fread_string` | `db.c:3124` | accumulate lines until one ends in `~`; joins with CRLF; `parse_at` converts `@<c>` colour codes |
| `asciiflag_conv` | `db.c:1209` | flag field is a decimal integer **or** letters: `a`..`z` = bits 0..25, `A`..`Z` = bits 26..51 |
| `asciiflag_conv_aff` | `db.c:1232` | same, every bit shifted up by one (`AFF_DONTUSE == 0`) |
| `dirs[]` | `constants.c:35` | `north east south west up down northwest northeast southeast southwest` |

The converter replaces `@<c>` colour codes with nothing (`@@` → literal `@`) and
joins string lines with `\n`. The leading indentation that `fread_string`
preserves is **kept** (the C loader keeps it too).

### 2.2 `.wld` — `parse_room` (`db.c:1306`), `setup_dir` (`db.c:1490`)

```text
#<vnum>
<name>~
<description>~
<zone#> <flags0> <flags1> <flags2> <flags3> <sector>
D<dir>
<exit description>~
<exit keywords>~
<exit_info_code> <key vnum> <to room vnum>
E
<keywords>~
<description>~
S
T <trigger vnum>          (zero or more, after S)
```

- The first numeric token is the owning zone number; the loader discards it and
  files the room through the zone table. `tbaport` ignores it too.
- `exit_info_code`: `0` plain, `1` door, `2` door+pickproof, `3`
  door+hidden, `4` door+pickproof+hidden (`setup_dir` maps these onto
  `EX_ISDOOR`/`EX_PICKPROOF`/`EX_HIDDEN`; `EX_CLOSED`/`EX_LOCKED` are **not**
  in the file).
- `key` of `-1`/`65535` and `to_room` of `-1`/`0`/`65535` are `NOWHERE`.
- `E` blocks append to a linked list; `tbaport` keeps file order.
- Trailing `T <vnum>` lines attach DG triggers; `attachments` keeps them in source order.

### 2.3 `.mob` — `parse_mobile` (`db.c:1890`)

```text
#<vnum>
<keyword list>~
<short descr>~
<long descr>~
<detailed descr>~
<f1> <f2> <f3> <f4> <f5> <f6> <f7> <f8> <alignment> <S|E>
<level> <hr> <ac> <hpdice> <damdice>
<gold> <experience>
<position> <default position> <sex>
[<e-spec line> ...]
E                          (only for the E form)
[T <trigger vnum> ...]
```

- 128-bit flag line: `f1..f4` are `MOB_*`, `f5..f8` are `AFF_*`. The legacy
  4-field form is also accepted.
- `parse_simple_mob` (`db.c:1682`): `GET_HITROLL = 20 - t[1]`,
  `GET_AC = 10 * t[2]`, HP = `t[3]d t[4] + t[5]`, damage =
  `t[6]d t[7] + t[8]`.
- `parse_enhanced_mob` (`db.c:1870`) adds `Keyword: value` e-specs;
  `interpret_espec` (`db.c:1718`) recognises `Str`/`Int`/… and save throws.

### 2.4 `.obj` — `parse_object` (`db.c:2087`)

```text
#<vnum>
<keyword list>~
<short descr>~
<description>~
<action description>~
<type> <extra0..3> <wear0..3> <affect0..3>     (13 fields; legacy 4 accepted)
<value0> <value1> <value2> <value3>
<weight> <cost> <rent> <level> <timer>          (3–5 fields)
A
<apply location> <modifier>                     (zero or more)
E
<keywords>~
<description>~
T <trigger vnum>                                (zero or more)
#<next vnum> | $
```

Object records have no end marker; the reader consumes up to the next `#`/`$`
and hands it back. The third numeric line may have 3, 4 or 5 fields (missing
`level`/`timer` default to 0). `A` blocks are `APPLY_*` affects.

### 2.5 `.zon` — `load_zones` (`db.c:2338`), `reset_zone` (`db.c:2859`)

```text
#<number>
<builders>~
<name>~
<bot> <top> <lifespan> <reset_mode> [zone flags ×4] [min] [max]
* comment
M <if> <mob> <max> <room>
O <if> <item> <max> <room>
G <if> <item> <max>
E <if> <item> <max> <wear position 0..17>
P <if> <item> <max> <container>
D <if> <room> <dir 0..9> <0 open|1 closed|2 locked>
R <if> <room> <item>
T <if> <attach 0 mob|1 obj|2 wld> <trigger> <room>
V <if> <a1> <a2> <a3> <sarg1> <sarg2>
S | $
```

`renum_zone_table` (`db.c:1623`) confirms the argument roles. `reset_mode`:
`0` never, `1` when deserted, `2` always.

### 2.6 Record readers and the world-global readers

| File | C source | Header scanned | Attachment |
| --- | --- | --- | --- |
| `.trg` | `parse_trigger` (`dg_db_scripts.c:30`) | `#vnum`, name, `<attach> <flags> <narg>`, arg list, body | `attach_type` `mob`/`obj`/`wld`; emitted as `tba.triggers`, classified into `reports/trigger-triage.json` |
| `.qst` | `parse_quest` (`quest.c:129`) | `#vnum`, five strings, three numeric lines, `S` | questmaster vnum from line 1 |
| `lib/misc/socials.new` | `boot_social_messages` (`db.c:195`), `save_socials` (`aedit.c:341`) | `~<command> <sort_as> <hide> <min_char_pos> <min_victim_pos> <min_level>`, thirteen message lines | command word |
| `lib/text/*` | `MOTD_FILE`/`NEWS_FILE`/… (`db.h:90`) | raw prose | file name |
| `lib/misc/messages` | `load_messages` (`msgedit.c:97`) | `*` name comment, `M`, type, twelve messages | attack type |

The `.trg` reader consumes the arg list and script body so the next record's
`#` is read at the right level. The `.qst`, `.shp`, `socials.new` and `messages`
readers (`source/quest.rs`, `source/shop.rs`, `source/social.rs`,
`source/text.rs`) are full record parsers, not scanners: see §3.6–§3.9 for the
grammars and their mappings. `CONFIG_NEW_SOCIALS` is `YES` in the source
(`config.c:271`), so `socials.new` is the active social table and the legacy
`lib/misc/socials` is recorded as a permanent drop rather than parsed.

---

## 3. Field mapping

### 3.1 Rooms

| Source | Target |
|---|---|
| `name` | `RoomDef::name` |
| `description` | `RoomDef::description` |
| owning `.wld` file number | `RoomDef::area` = `tba_<zone>`; key = `tba.<vnum>` |
| `vnum` | `RoomDef::num` |
| room flag `ROOM_DARK` (0) | `RoomTag::Dark` |
| `ROOM_DEATH` (1) | **permanent drop** (no `RoomTag`; `room_flag`) |
| `ROOM_NOMOB` (2) | `RoomTag::NoMob` |
| `ROOM_INDOORS` (3) | **absence** → `RoomTag::Outdoor` |
| `ROOM_PEACEFUL` (4) | `RoomTag::Peaceful` |
| `ROOM_SOUNDPROOF` (5) | `RoomTag::Soundproof` |
| `ROOM_NOTRACK` (6) | **permanent drop** (no `RoomTag`) |
| `ROOM_NOMAGIC` (7) | **permanent drop** (no `RoomTag`) |
| `ROOM_TUNNEL` (8) | `RoomTag::Tunnel` |
| `ROOM_PRIVATE` (9) | `RoomTag::Private` |
| `ROOM_GODROOM` (10) | `RoomTag::Godroom` |
| `ROOM_HOUSE` (11) .. `ROOM_WORLDMAP` (16) | **permanent drop** (house/OLC/map internals) |
| `sector` 0..9 | `Sector::{Inside,City,Field,Forest,Hills,Mountain,WaterSwim,WaterNoSwim,Flying,Underwater}` |
| `D` blocks | `BTreeMap<SmolStr, ExitDef>` keyed by `dirs[]` |
| `E` blocks | `RoomDef::extra_descriptions` (keywords split on whitespace) |
| `T` lines | the room's entry in `tba.triggers.attachments` (source order) |
| `coord` | **permanent drop, recorded once** in `global_drops` |

Every source flag either maps to a `RoomTag` or produces a permanent-drop
`unmapped` entry (`room_flag`). `RoomDef::coord` is always `None` because
tbaMUD's `struct room_data` carries no coordinates; the absence is recorded
once in the report's `global_drops` (`no_source_field` / `room coord`).

A `D` block whose `to_room` is `NOWHERE` (`-1`/`0`/`65535`) is a
**description-only exit**. `ExitDef::to` is non-optional, so it cannot be
emitted: the exit is dropped and recorded as `exit_description_only` with its
direction and source description. A real vnum that was not converted is a
dangling destination and is recorded as `dangling_exit` instead.

`ExitDef`:

| Source | Target |
|---|---|
| `to_room` | `ExitDef::to` = `tba.<to>`; `-1`/`0`/`65535` → dropped (description-only) |
| code 3/4 | `ExitDef::hidden` |
| code 1..4 | `ExitDef::door = Some(DoorDef)` |
| code 2/4 | `DoorDef::pickable = false` |
| `key` (>0 and ≠65535) | `DoorDef::key` = `tba_item_<key>` |
| `keywords` (non-empty) | `DoorDef::name` |
| `description` | report only (there is no MIB field for a direction description) |
| — | `DoorDef::closed`/`locked` default false; the initial state lives in the zone `D` reset |

### 3.2 NPCs

| Source | `stdlib.npcs` / `tba.npc_stats` |
|---|---|
| `short_descr` | `NpcDef::name` (first letter lowered after `a`/`an`/`the`, matching `db.c:1915`) |
| `long_descr` + `\n\n` + detailed descr | `NpcDef::persona` |
| `MOB_SENTINEL`, `MOB_SCAVENGER`, `MOB_WIMPY` | `NpcDef::behaviors.{sentinel,scavenger,wimpy}` |
| `MOB_AGGRESSIVE`/`AGGR_EVIL`/`AGGR_GOOD`/`AGGR_NEUTRAL`/`STAY_ZONE`/`MEMORY`/`HELPER`/`AWARE` | `TbaNpcStats::behaviour` (`NpcBehaviour`) |
| `level` | `level` |
| `hr` field | `hitroll = 20 - hr` |
| `ac` field | `armor_class = 10 * ac` |
| hp dice | `hp: Dice` |
| damage dice bonus | `damroll` |
| damage dice | `damage: Dice`; `npc_combatant_system` attaches it as `TbaAttackDice`, used by `resolve_rounds` |
| `alignment` | `alignment` |
| `gold` | `gold` |
| `experience` | `experience` |
| default position | `default_position: Position` |
| `—` | `thaco = 20` (monsters use a flat 20; see `fight.c:829`) |
| `MOB_NOCHARM`/`NO_SUMMON`/`NOSLEEP`/`NOBASH`/`NOBLIND`/`NOKILL` | `TbaNpcStats::behaviour` (`NpcBehaviour::No*`) |
| `MOB_SPEC` | **permanent drop** (`mob_flag`): special procedures are a non-goal (#31) |
| `AFF_*` on the mob | **permanent drop** (`mob_aff`): no affect-flag field |
| e-specs | **permanent drop** (`mob_espec`): no target field |
| sex | **permanent drop** (`mob_sex`): `TbaNpcStats` has no sex field |
| keyword list | **permanent drop** (`npc_keywords`): `stdlib.npcs` has no keyword list |
| `T` lines | the NPC's entry in `tba.triggers.attachments` (source order) |

`MOB_ISNPC` and `MOB_NOTDEADYET` are loader bookkeeping and are not reported.

The following Circle **combat skills** occur in the source class tables. They
have no schema field (they are class rules, not world data), but they are
**implemented as pack gameplay** in `mib_tba::skills` (GitHub issue #25). They
are recorded once in the report's `global_drops` (`combat_skill`) so the class
vocabulary stays traceable, with a reason that names the implementation rather
than a non-goal: `backstab`, `kick`, `bash`, `rescue`, `assist`, `consider`.
The per-class minimum levels live in `tba.classes.skill_levels`.

### 3.3 Items

| Source | `stdlib.items` / `tba.items` |
|---|---|
| `short_descr` | `ItemDef::name` |
| `description` (first letter capitalised, `db.c:2118`) | `ItemDef::description` |
| `cost` | `ItemDef::value` |
| type `ITEM_CONTAINER` (15), `value[0]` | `ItemDef::container.capacity` |
| type | `ItemTag::{LightSource,Weapon,Armor,Drink(17/23),Key,Food,Boat}` |
| type 0..23 | `TbaItemDef::kind` (`TbaItemKind`) |
| `ITEM_WEAR_*` bits | `TbaItemDef::slots`; `TAKE` is not a slot, `FINGER`/`NECK`/`WRIST` expand to both paired slots |
| `value[0..3]` | `TbaItemDef::values` |
| `A` blocks | `TbaItemDef::affects` (`APPLY_*` → `AffectLocation`) |
| `weight/cost/rent/level` | `weight/cost/rent/min_level` |
| `timer` | `timer` (`0` → `None`) |
| keyword list | `TbaItemDef::keywords` |
| `ITEM_*` extra flags | `TbaItemDef::extra_flags` (`ItemExtraFlag`) |
| `AFF_*` affect-flags | `TbaItemDef::affect_flags` (`ItemAffectFlag`); the `DONTUSE`/`UNUSED`/`FREE` placeholder bits are **permanent drops** |
| action description | **permanent drop** (`item_action_description`): `stdlib.items` has no action description |
| `E` extra descriptions | **permanent drop** (`item_extra_descriptions`): `stdlib.items` has no extra-description field |
| `T` lines | the item's entry in `tba.triggers.attachments` (source order) |
| `APPLY_CLASS/LEVEL/AGE/CHAR_WEIGHT/CHAR_HEIGHT/GOLD/EXP` | **permanent drop** (`affect_location`): no `AffectLocation` variant, and no runtime consumer |

`APPLY_*` → `AffectLocation`: `1..6` → Str/Dex/Int/Wis/Con/Cha, `12` →
MaxMana, `13` → MaxHp, `14` → MaxMove, `17` → ArmorClass, `18` → Hitroll,
`19` → Damroll, `20..24` → the five saves. `APPLY_NONE` is skipped.

### 3.4 Zone resets

| Code | `ZoneCommandKind` | Fields |
|---|---|---|
| `M` | `PlaceMob` | `mob=tba_npc_<a1>`, `max_in_world=a2`, `room=tba.<a3>` |
| `O` | `DropItem` | `item=tba_item_<a1>`, `max_in_world=a2`, `room=tba.<a3>`; `a3` = nowhere → dropped |
| `G` | `GiveItem` | `item`, `max_in_world` |
| `E` | `EquipItem` | `item`, `slot: WearSlot` from `a3` (0..17), `max_in_world` |
| `P` | `PutItem` | `item=tba_item_<a1>`, `container=tba_item_<a3>`, `max_in_world=a2` |
| `D` | `DoorState` | `room=tba.<a1>`, `exit=dirs[a2]`, `state` from `a3` (0 open/1 closed/2 locked) |
| `R` | `RemoveItem` | `room=tba.<a1>`, `item=tba_item_<a2>` |
| `T` | — | folded into the owner's `tba.triggers.attachments` list |
| `V` | — | reported as a trigger-variable drop (no schema home) |
| `*` | — | skipped (comment) |

Each mapped command is wrapped in `ZoneCommand { if_flag, command }`. The
`if_flag` comes from the source's first command number and is honoured by the
interpreter (a flagged command is skipped when the previous command failed).

`ResetMode`: `0` → `Never`, `1` → `WhenDesert`, `2` → `Always`. Zone flag
words are recorded as unmapped because `ZoneDef` has no flags field.

### 3.5 Areas

One `stdlib.areas` entry per converted zone: id `tba_<zone>`, name from the
`.zon` name line (e.g. `Northern Midgaard`).

### 3.6 Shops

One `tba.shops` entry per converted shopkeeper, keyed by the keeper's
`stdlib.npcs` id (`SHOP_KEEPER`, `shop.c:1203`), because that is what Circle's
`shop_keeper` matches against. The record's `#vnum` label is only a shop id.

| Source (`.shp`) | `tba.shops` |
|---|---|
| `profit_buy` | `buy_multiplier` (a non-positive value drops the shop) |
| `profit_sell` | `sell_multiplier` |
| trade-type list | `buy_kinds` (`TbaItemKind`) |
| produce list | `produce` (`stdlib.items` ids; a missing item is dangling) |
| room list | `rooms` (`stdlib.rooms` ids; a missing room is dangling) |
| `trade_with` bits | `trade_with` (`ShopTrade::Good/Evil/Neutral/MagicUser/Cleric/Thief/Warrior`) |
| `open1/close1/open2/close2` | `hours: ShopHours` |
| seven messages | `messages`, kept verbatim with `%s`/`%d` |
| per-type buy word | **permanent drop** (`shop_buy_word`) |
| `broke_temper` | **permanent drop** (`shop_broke_temper`): no temper reaction |
| `bitvector` | **permanent drop** (`shop_bitvector`): `SHOP_*` flags |
| `#vnum` ≠ keeper | **unmapped** (`shop_record`): the keeper vnum is the key |
| keeper `-1`, duplicate keeper, missing produce/room, non-positive `profit_buy` | **dangling** / **unmapped** as named |

The runtime enforces `hours` (`is_open`, `shop.c:140`), `rooms` (`ok_shop_room`,
`shop.c:939`), and `trade_with` (`is_ok_char`, `shop.c:108`), and renders
`messages[0..6]` for the deny/buy/sell replies.

---

### 3.7 Quests (`stdlib.quests`)

One `stdlib.quests` entry per non-empty `.qst` record, keyed `tba_quest_<vnum>`.
The source world has exactly one (vnum 100, "Kill the Mice!"); the other seven
`.qst` files are empty (`$~`).

| Source (`.qst`) | `stdlib.quests` |
|---|---|
| `name` | `QuestDef::name` |
| `info` (else `desc`) | `QuestDef::description` |
| `questmaster` | `QuestDef::giver` = `tba_npc_<vnum>` (a missing giver drops the quest, `dangling_quest_giver`) |
| `type` 0 `AQ_OBJ_FIND` | `QuestStepDef::Collect { item: tba_item_<target>, count: quantity }` |
| `type` 1 `AQ_ROOM_FIND` | `QuestStepDef::Visit { room: tba.<target> }` |
| `type` 3 `AQ_MOB_KILL` | `QuestStepDef::Kill { npc: tba_npc_<target>, count: quantity }` |
| `type` 2/4/5/6 (`MOB_FIND`/`MOB_SAVE`/`OBJ_RETURN`/`ROOM_CLEAR`) | **unmapped** (`quest_objective`): no `QuestStepDef` variant; the quest is dropped |
| `exp_reward` | `RewardSpec::xp` |
| `obj_reward` (a real item) | `RewardSpec::items` |
| `obj_reward` that resolves to no item (including the source's `65535`) | **dangling** (`dangling_quest_reward_item`) |
| `quantity` (`values[6]`) | the collect/kill `count` (minimum 1) |
| `gold_reward` | **permanent drop** (`quest_gold_reward`): `RewardSpec` has no gold field |
| `desc`/`done`/`quit` | **permanent drop** (`quest_text`): `QuestDef` has one description field |
| `flags` bit 0 (`AQ_REPEATABLE`) | **permanent drop** (`quest_flag`) |
| `prereq`/`prev_quest`/`next_quest` (≠ `-1`) | **permanent drop** (`quest_link`) |
| `points`/`penalty`/`minlevel`/`maxlevel`/`time`/`returnmob` | **permanent drop** (`quest_condition`) |

A step reference that names no converted object/room/NPC is a `dangling_*`
entry and drops the whole quest, because a quest missing its objective would
change the world rather than omit a field.

### 3.8 Help topics (`stdlib.help`)

The eleven `lib/text/` files and `lib/misc/messages` have no MIB home, so they
become authored `stdlib.help` topics.

| Source | Topic id | Audience |
|---|---|---|
| `lib/text/motd` | `motd` | public |
| `lib/text/imotd` | `imotd` | staff |
| `lib/text/news` | `news` | public |
| `lib/text/credits` | `credits` | public |
| `lib/text/handbook` | `handbook` | staff |
| `lib/text/policies` | `policies` | public |
| `lib/text/background` | `background` | public |
| `lib/text/info` | `info` | public |
| `lib/text/wizlist` | `wizlist` | staff |
| `lib/text/greetings` | `greetings` | public |
| `lib/text/immlist` | `immlist` | staff |
| `lib/misc/messages` attack type `N` | `combat-message-N` | public |

**Colour.** The text files carry Circle `@`-codes (`@RHELP COLORCODES@n`).
`HelpTopicDef` prose reaches the client through `sanitize_prose`
(`packs/stdlib/code/src/help.rs`), which strips ANSI/control characters but not
the literal `@R` sequence, so the converter resolves the codes with
`strip_colour_codes` at conversion time. `@@` becomes a literal `@`; every
other `@<c>` is dropped.

**Combat messages.** `load_messages` (`msgedit.c:97`) reads `M`-delimited
entries: the numeric attack type and twelve strings (die/miss/hit/god ×
attacker/victim/room; a line beginning with `#` is Circle's NULL). Several `M`
entries may share a type — those are random variants — and the converter groups
them under one topic body that labels all twelve slots. The `*`-comment name
(for example `burning hands`) titles the topic. The legacy `lib/text/help/`
database (`help.hlp` and friends) is *not* converted; it is recorded once as
`global_drops`/`out_of_scope` because the generated command catalog plus these
topics supersede it.

### 3.9 Socials (`tba.socials`)

One `tba.socials` entry per `lib/misc/socials.new` record, keyed by the command
word (491 records). `stdlib.verbs` cannot express a social — it is a static
effect list, while a social is a message table parameterised by actor, target,
body part and object — so TBA owns the section.

| Source | `tba.socials` |
|---|---|
| command word (`~<name>`) | map key |
| `sort_as` | `sort_as` |
| `hide` | `hide` |
| `min_char_position`/`min_victim_position`/`min_level_char` | the same fields |
| the thirteen message lines | the matching `char_*`/`others_*`/`vict_*` fields; a `#` line is `None` |

Two records with the same command word are a `duplicate_social` global drop
(the last wins). The `$`-codes are preserved verbatim; the runtime
(`code/src/socials.rs`) resolves `$n`/`$N`/`$e`/`$E`/`$m`/`$M`/`$s`/`$S`,
`$t`/`$T` (body part/object), `$p`/`$P`/`$o`/`$O` (object) and the `$u`/`$U`
case switches, and drops an unknown code as `perform_act`'s default branch does.
The source's stray `$i` (which `perform_act` also has no case for) is recorded
once as a `social_code` global drop. MIB models no sex, so the pronouns render
as neutral singular *they/them/their*. The legacy `lib/misc/socials` old-format
file is a `global_drops`/`out_of_scope` entry (`CONFIG_NEW_SOCIALS` is `YES`).

### 3.10 DG triggers (`tba.triggers`)

One `tba.triggers.<zone>.yaml` shard per zone; the pack loader merges them back
into the `TriggersFile` the engine loads. The `.trg` reader
(`source/deferred.rs`) emits one `TriggerDef` per record and one
order-preserving attachment list per owner.

| Source (`.trg` / owner `T` line) | `tba.triggers` |
|---|---|
| `#<vnum>` | `TriggerId` `tba_trig_<vnum>` |
| `name~` | `TriggerDef::name` |
| `<attach_type>` (`0`/`1`/`2`) | `TriggerDef::attach_type` (`mob`/`obj`/`wld`) |
| type field | `TriggerDef::flags` (the raw token) and `TriggerDef::types` (the expanded lowercase names from `expand_types`) |
| `narg` | `TriggerDef::narg` |
| arg list | `TriggerDef::arglist` |
| script body | `TriggerDef::script` (colour-stripped, verbatim) |
| owner `T <vnum>` lines, in order | the owner's `TriggersFile::attachments` list (`npc:<id>` / `item:<id>` / `room:<id>`) |
| zone-reset `T <if> …` | folded into the target prototype's list |
| zone-reset `V …` | **reported** as a trigger-variable drop (no schema home) |

The legacy `attach` field is gone: the owner's kind decides how the record's
type letters are read (`mapping/attach.rs`), and a record whose `attach_type`
differs from its owner is kept and named (`trigger_attach_type_mismatch`). An
owner `T` line that names no emitted trigger is a `dangling_trigger_ref` drop;
a zone-reset `T` that cannot resolve is a `dangling_zone_trigger` /
`dangling_zone_trigger_room` drop.

---

## 4. Drop-report schema

`serde_json`, pretty-printed, terminated by a newline. Every vector is sorted
before writing, so the report is deterministic. Schema `2`:

```jsonc
{
  "schema": 2,
  "tool": "tbaport 0.1.0",
  "source": "/home/qworg/Development/tbamud",
  "out": "target/tbaport/content",
  "mode": "full",              // or "only"
  "only": null,                // or the zone number
  "global_drops": [
    { "kind": "no_source_field", "name": "room coord",
      "reason": "tbaMUD `struct room_data` has no coordinates; `RoomDef::coord` is always `None`" },
    { "kind": "combat_skill", "name": "backstab",
      "reason": "Circle class ability `backstab`; implemented as tba pack gameplay (issue #25), not lost by conversion" }
  ],
  "totals": {
    "zones": 189, "rooms": 12700, "npcs": 3705, "items": 4765,
    "zone_commands": 18513, "deferred": 0,
    "dangling": 15, "unmapped": 17593, "global_drops": 10,
    "unmapped_by_kind": { "room_flag": 1726, "mob_sex": 3705, ... },
    "dangling_by_kind": { "dangling_door_key": 3, ... }
  },
  "zones": [
    {
      "number": 30,
      "name": "Northern Midgaard",
      "bot": 3000, "top": 3099,
      "area": "tba_30",
      "rooms": 58, "npcs": 46, "items": 63, "commands": 169,
      "source_files": { "wld": true, "mob": true, "obj": true,
                        "zon": true, "shp": true, "trg": true, "qst": false },
      "deferred": [],
      "triggers": [
        { "vnum": 3000, "name": "Fixture Trigger", "attach": "mob",
          "flags": "a", "types": ["speech"], "narg": 0,
          "bucket": "interactive_response", "lines": 12, "raw_lines": 15 }
      ],
      "trigger_variables": [],
      "dangling": [
        { "kind": "dangling_door_key",
          "from": "room tba.3040 exit west door.key",
          "target": "tba_item_3112" }
      ],
      "unmapped": [
        { "object": "npc tba_npc_3000", "kind": "mob_flag",
          "name": "SPEC", "detail": "special procedures are a non-goal (see #31)" },
        { "object": "item tba_item_3000", "kind": "affect_location",
          "name": "AGE", "detail": "modifier 5; permanent drop: AffectLocation has no such variant" }
      ],
      "command_flags": [
        { "index": 0, "code": "R", "if_flag": 0 },
        { "index": 3, "code": "E", "if_flag": 1 }
      ]
    }
  ]
}
```

Finding kinds:

- `global_drops[].kind`: `no_source_field` (a whole-world absence, e.g. room
  coordinates), `combat_skill` (a Circle class ability with no schema field,
  implemented as pack gameplay in `mib_tba::skills`, issue #25),
  `out_of_scope` (the legacy `lib/misc/socials` and `lib/text/help` files),
  `social_code` (a `$`-code `perform_act` cannot resolve), `duplicate_social`.
- `deferred` is empty: every remaining source feature has a schema home or a
  named `unmapped`/`dangling` entry. Triggers are emitted (issue #27) and quests
  convert (issue #16); the zone-reset `V` commands land in `trigger_variables`,
  so nothing writes a `deferred` record now.
- `dangling.kind`: `dangling_door_key`, `dangling_exit`, `dangling_reset_mob`,
  `dangling_reset_item`, `dangling_reset_container`, `dangling_reset_room`,
  `dangling_shop_keeper`, `dangling_shop_produce`, `dangling_shop_room`,
  `duplicate_shop_keeper`, `dangling_quest_giver`, `dangling_quest_target_item`,
  `dangling_quest_target_room`, `dangling_quest_target_npc`,
  `dangling_quest_reward_item`, plus the defensive `dangling_npc_stats_key` /
  `dangling_item_stats_key`.
  A dangling reference is **not** in the emitted YAML; the report is the only
  record of it.
- `unmapped.kind`: `room_flag`, `mob_flag`, `mob_aff`, `mob_espec`,
  `mob_sex`, `npc_keywords`, `position`, `sector`, `exit_direction`,
  `exit_description_only`, `item_affect_flag`, `item_action_description`,
  `item_extra_descriptions`, `item_type`, `affect_location`, `zone_flag`,
  `zone_command_dropped`, `reset_mode`, `shop_buy_word`, `shop_broke_temper`,
  `shop_bitvector`, `shop_record`, `shop_profit_buy`, `quest_objective`,
  `quest_gold_reward`, `quest_text`, `quest_flag`, `quest_link`,
  `quest_condition`.
- `trigger_coverage`: the `dg_vm::analyze` roll-up of every emitted script —
  `records`, `script_lines`, `by_attach_type`, `lines_by_class`, `controls`,
  `statements`, `wait_forms`, `script_commands`, `unknown_object_commands` /
  `unknown_world_commands`, `mud_commands` (the supported/unsupported words),
  `dynamic_commands`, `over_length_lines` and `structural_errors`.
- `trigger_triage`: the corpus classification (attach, expanded type,
  interactivity bucket, script size), per zone and as a whole.
- `trigger_attachments`: the owner/link roll-up — `owners`, `links`,
  `links_by_owner_kind`, `zone_reset_folded` / `zone_reset_already_present`,
  `unattached_triggers`, `attach_type_mismatches` and `variable_commands`.

`command_flags` remains the raw carrier for the Circle `if_flag`: one entry per
parsed command, in source order, keyed by its index in the reset program
(`T`/`V` and comments included in the index space but not in `commands`).

`DropReport::write` also emits a Markdown summary beside the JSON
(`reports/drop-report.summary.md`), grouping every `unmapped` and `dangling`
entry by reason, listing the global drops, and printing the trigger-corpus
coverage (lines by class, the control and statement histograms, and
"Supported words: 42; unsupported words: 94").

---

## 5. Module layout

```text
tools/tbaport/
├── Cargo.toml                 # schema deps + serde/serde_yaml/serde_json
├── src/
│   ├── lib.rs                 # public API re-exports
│   ├── main.rs                # thin binary
│   ├── cli.rs                 # argument parsing, summaries
│   ├── error.rs               # ConvertError / Result
│   ├── ids.rs                 # tba.<vnum>, tba_npc_<vnum>, ...
│   ├── source/
│   │   ├── mod.rs             # record loops + shared RawExtraDesc
│   │   ├── tokens.rs          # Source reader, fread_string, asciiflag_conv
│   │   ├── room.rs            # parse_room / setup_dir
│   │   ├── mob.rs             # parse_mobile / simple+enhanced
│   │   ├── obj.rs             # parse_object
│   │   ├── zone.rs            # load_zones
│   │   ├── quest.rs           # parse_quest (.qst → stdlib.quests)
│   │   ├── social.rs          # socials.new → tba.socials
│   │   ├── text.rs            # lib/text + lib/misc/messages → stdlib.help
│   │   └── deferred.rs        # .trg record reader
│   ├── mapping.rs             # raw records → mib-stdlib/mib-tba types
│   ├── emit.rs                # OutputBundle + YAML writers
│   └── report.rs              # DropReport types + JSON writer
└── tests/
    ├── parse.rs               # per-record fixture tests
    ├── convert.rs             # round-trip, determinism, zone 30 counts
    └── fixtures/tbamud/lib/{world/{wld,mob,obj,zon,shp,trg,qst}/30.*,
                             misc/{messages,socials.new},text/{motd,news}}
```

`mapping.rs` runs in two phases: read every selected zone into raw records,
collect the global room/NPC/item id sets, then map and detect dangling
references. `emit.rs` serialises `mib-stdlib`/`mib-tba` values directly, so the
output cannot drift from the schema the engine loads.

---

## 6. CLI

```text
tbaport convert --source DIR [--out DIR] [--report FILE] [--only ZONE] [--start-room VNUM] [--validate-only]
tbaport triage  --source DIR [--out FILE] [--only ZONE]

  --source DIR       tbaMUD checkout root (must contain lib/world)
  --out DIR|FILE     convert: staging output directory [default: target/tbaport/content]
                     triage:  JSON output path [default: reports/trigger-triage.json]
  --report FILE      drop-report path [default: target/tbaport/drop-report.json]
  --only ZONE        convert a single zone
  --start-room VNUM  tag this room vnum as `start_room` [default: 3001]
  --validate-only    parse/map and write the report, but emit no YAML
```

`tbaport convert` writes the `.trg` coverage into the drop report's
`trigger_coverage`; `tbaport triage` does no mapping, reads the `.trg` records,
classifies every one, marks the curated essential-interactivity set, and writes
the deterministic `reports/trigger-triage.json` (see `docs/design/triggers.md`).

Files written: `stdlib.areas.yaml`, `stdlib.rooms.<zone>.yaml` (one shard per
zone, TBA-E01), `stdlib.npcs.yaml`, `stdlib.items.yaml`,
`stdlib.quests.yaml`, `stdlib.help.yaml`, `tba.zones.yaml`,
`tba.npc_stats.yaml`, `tba.items.yaml`, `tba.shops.yaml`, `tba.socials.yaml`,
`tba.triggers.<zone>.yaml` (one shard per zone), plus the report and its
Markdown summary. `tba.classes.yaml`,
`tba.spells.yaml` and `stdlib.verbs.yaml` are **not** converter output: the
Diku classes live in `class.c` and the spell catalog and verb list were
transcribed by hand, so they stay hand-authored (a world swap must preserve
them; see the runbook). A committed report lives at
`reports/drop-report.json` (§8/runbook).

Each YAML file is prefixed with a fixed provenance comment. Library entry point:

```rust
let conversion = tbaport::convert(&tbaport::ConvertOptions {
    source_root: "/path/to/tbamud".into(),
    only: Some(30),
})?;
tbaport::write_bundle(Path::new("target/tbaport/content"), &conversion.bundle)?;
conversion.report.write(Path::new("target/tbaport/drop-report.json"))?;
```

---

## 7. Determinism and round-trip test plan

| Test | File | Assertion |
|---|---|---|
| Tokenizer | `source/tokens.rs` | `asciiflag_conv` letters/numbers, `_aff` shift, `@@` colour escape |
| Rooms | `tests/parse.rs` | 3 fixture rooms; exit code/key/destination; extra descriptions; description-only exit retained as raw |
| Mobs | `tests/parse.rs` | simple `S` and enhanced `E` forms; flag words, dice, stats, e-specs |
| Objects | `tests/parse.rs` | 13-field flags, holds terminator for the caller, affects, container capacity |
| Zones | `tests/parse.rs` | header/bounds/reset mode, comment skipping, command order, `if_flag`, `E` wear position |
| Triggers | `tests/parse.rs` | trigger records, their type letters, and the trailing `T` lines; quests are parsed as real records |
| Quests | `tests/parse.rs`, `tests/convert.rs` | the `.qst` header/objectives/rewards, and the mapped `QuestDef` (giver, kill step, count) |
| Socials | `tests/parse.rs`, `tests/convert.rs` | the new-format header and thirteen sets, `#`→`None`, and the mapped `SocialDef` |
| Help | `tests/parse.rs`, `tests/convert.rs` | text files become topics with `@`-codes stripped; `messages` groups by attack type |
| Social runtime | `code/tests/socials.rs` | target found/not-found, the room/victim lines, the `socials` listing, the sleeping-actor gate, `$`-substitution |
| Schema exactness | `tests/convert.rs` | every emitted file (including each room shard) deserialises into its real type and re-serialises to the exact bytes written (header aside) |
| Determinism | `tests/convert.rs` | two in-process conversions produce byte-identical YAML **and** byte-identical JSON report |
| Sharding | `tests/convert.rs` | rooms are emitted as `stdlib.rooms.<zone>.yaml` shards; the engine merges them back to the full `RoomsFile` |
| Real counts | `tests/convert.rs` | `--only 30` totals equal a strict `#<digits>` scan of `30.wld`/`30.mob`/`30.obj`, and are pinned at 58/46/63 |
| Registry validation | `tests/validate_pack.rs` | the **full 189-zone** conversion loads through the real `stdlib + tba` registry and `validate_all` returns no issues |
| Full-world determinism | manual | `convert` twice into different dirs; `diff -r` the YAML trees; reports differ only in the `out` field |

The full 191-file world converts in ~2 seconds, so the real-count test runs
against the sibling checkout when present (`TBAMUD_SOURCE` or
`/home/qworg/Development/tbamud`) and skips cleanly otherwise.

---

## 8. Known gaps (all recorded in the drop report)

Every gap below is a **named drop**: it appears in
`reports/drop-report.json` as a `global_drops` / `unmapped` / `dangling` entry,
and `reports/drop-report.summary.md` groups the surviving drops by reason.

- **Room coordinates.** tbaMUD's `struct room_data` has no coordinate field, so
  `RoomDef::coord` is always `None` (`global_drops`: `no_source_field` / `room
  coord`).
- **Room flags without a `RoomTag`.** `ROOM_DEATH`, `ROOM_NOTRACK`,
  `ROOM_NOMAGIC`, `ROOM_HOUSE`+, and the OLC/BFS/worldmap internals
  (`room_flag`).
- **Description-only exits.** An exit whose destination is `NOWHERE` has no
  `ExitDef::to` and is dropped (`exit_description_only`) with its direction and
  description.
- **NPC attack dice** are now modelled (`tba.npc_stats.damage` +
  `TbaAttackDice`). **NPC affect-flags, e-specs, sex and keyword lists** have no
  field and are named (`mob_aff`, `mob_espec`, `mob_sex`, `npc_keywords`).
- **`MOB_SPEC` special procedures** are a non-goal (`mob_flag`), tracked in #31.
- **Combat skills** `backstab`, `kick`, `bash`, `rescue`, `assist`, `consider`
  are class rules with no world-content field, so they stay in the
  `global_drops` list (`combat_skill`) — but they are **implemented** as pack
  gameplay in `mib_tba::skills` (issue #25), and the reason string says so. See
  `docs/runbook.md` §6/§8 for the commands and their remaining limits.
- **Item action descriptions and object `E` descriptions** (`stdlib.items` has
  no field). The keyword list and `ITEM_*`/`AFF_*` flags are now modelled.
- **`APPLY_CLASS/LEVEL/AGE/CHAR_WEIGHT/CHAR_HEIGHT/GOLD/EXP`** have no
  `AffectLocation` variant (`affect_location`).
- **Zone flags** (`CLOSED`, `NO_IMMORT`, `QUEST`, `GRID`, …) have no `ZoneDef`
  field (`zone_flag`).
- **Shops**: the keeper temper (`broke_temper`), the `SHOP_*` bitvector, the
  per-type buy word and a differing `#vnum` are named drops; the keeper `-1`,
  duplicate keeper, missing produce/room and a non-positive `profit_buy` drop
  the shop (`dangling` / `shop_profit_buy`). Opening hours, room restrictions
  and `trade_with` are now modelled and enforced.
- **Quests** now convert (issue #16). The objective types with a
  `QuestStepDef` are mapped; `MOB_FIND`/`MOB_SAVE`/`OBJ_RETURN`/`ROOM_CLEAR`
  and the gold reward, the extra text (`desc`/`done`/`quit`), the repeatable
  flag, the chain links and the level/time conditions are named drops. The
  source's one real quest (`tba_quest_100`) converts.
- **Text and combat messages** now convert to `stdlib.help` topics (issue
  #18). The legacy `lib/text/help/help.hlp` database is an `out_of_scope`
  global drop.
- **Socials** now convert to `tba.socials` (issue #17). The source's stray
  `$i` code and the legacy old-format `lib/misc/socials` are named global
  drops; pronouns render neutral (MIB models no sex).
- **DG triggers** convert (issue #27). All 1,461 records are emitted into
  `tba.triggers.<zone>.yaml`; each owner's ordered attachment list is in
  `TriggersFile.attachments`, and zone-reset `T` commands are folded in. What
  is not modelled is named: the `attach_type` mismatches (8), the 15 scripts
  with structural errors (30 control-flow errors, from `dg_vm::analyze`), the
  zone-reset `V` commands (none in this source) and any host primitive the
  runtime does not model. Every script's coverage is in the drop report's
  `trigger_coverage`, the classification in `trigger_triage` and the per-zone
  `triggers[]`, and the standalone `reports/trigger-triage.json`
  (`tbaport triage`).

None of these is silently discarded.
