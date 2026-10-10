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
4. **Zones are programs.** `M/O/G/E/P/D/R` map 1:1 onto `ZoneCommand` variants
   in source order. `if_flag` has no field in `ZoneCommand`; command order is
   preserved and every command's `if_flag` is recorded in the drop report
   (`zones[].command_flags[]`) so the interpreter semantics are not lost.
5. **`P` uses the container prototype named by `arg3`**, matching
   `reset_zone`'s `get_obj_num(ZCMD.arg3)`.
6. **`stdlib.npcs`** gets name + persona (long description + detailed
   description); **`tba.npc_stats`** carries the Diku numbers and AI flags.
7. **`stdlib.items`** gets name + description + container/tags; **`tba.items`**
   carries kind, slots, values, affects and the economy fields.
8. **DG triggers (`.trg`) and quests (`.qst`) are deferred** (doc §10/§9).
   No sections are emitted for them; each record is written to the drop report
   with vnum and attachment point so a later phase can rebuild it. **Shops
   (`.shp`)** are no longer deferred: they are parsed and emitted as
   `tba.shops` (see §3.6).
9. **Determinism.** Zones are iterated in ascending numeric order and every
   section is a `BTreeMap`; converting the same source twice is byte-identical.
10. **Dangling references are dropped, never emitted.** Every reference the
    converter writes is resolved against the converted id set first. An exit to
    a missing room is removed; a door whose key names a missing item keeps the
    door but loses the key; a zone reset (`M/O/G/E/P/D/R`) that names a missing
    mob, item, container or room is removed. Each case is recorded in the drop
    report, so a partial (`--only Z`) or a source world with genuinely broken
    references still emits a pack the engine can load.

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
- Trailing `T <vnum>` lines attach DG triggers (deferred).

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

### 2.6 Deferred readers and the shop reader

| File | C source | Header scanned | Attachment |
| --- | --- | --- | --- |
| `.trg` | `parse_trigger` (`dg_db_scripts.c:30`) | `#vnum`, name, `<attach> <flags> <narg>`, arg list, body | `0` mob, `1` object, `2` world |
| `.qst` | `parse_quest` (`quest.c:129`) | `#vnum`, five strings, three numeric lines, `S` | questmaster vnum from line 1 |

The `.trg` reader consumes the arg list and script body so the next record's
`#` is read at the right level. The `.shp` reader (`source/shop.rs`, mirroring
`boot_the_shops` in `shop.c:1203`) is a full record parser, not a scanner: see
§3.6 for the grammar and its mapping into `tba.shops`.

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
| `ROOM_NOMOB` (2) | `RoomTag::NoMob` |
| `ROOM_INDOORS` (3) | **absence** → `RoomTag::Outdoor` |
| `ROOM_PEACEFUL` (4) | `RoomTag::Peaceful` |
| `ROOM_SOUNDPROOF` (5) | `RoomTag::Soundproof` |
| `ROOM_TUNNEL` (8) | `RoomTag::Tunnel` |
| `ROOM_PRIVATE` (9) | `RoomTag::Private` |
| `ROOM_GODROOM` (10) | `RoomTag::Godroom` |
| `DEATH`(1), `NOTRACK`(6), `NOMAGIC`(7), `HOUSE`+, internal bits | unmapped → report |
| `sector` 0..9 | `Sector::{Inside,City,Field,Forest,Hills,Mountain,WaterSwim,WaterNoSwim,Flying,Underwater}` |
| `D` blocks | `BTreeMap<SmolStr, ExitDef>` keyed by `dirs[]` |
| `E` blocks | `RoomDef::extra_descriptions` (keywords split on whitespace) |
| `T` lines | deferred trigger metadata |

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
| damage dice | unmapped → report (no attack-dice field) |
| `alignment` | `alignment` |
| `gold` | `gold` |
| `experience` | `experience` |
| default position | `default_position: Position` |
| `—` | `thaco = 20` (monsters use a flat 20; see `fight.c:829`) |
| `MOB_SPEC`, `NOCHARM`, `NOSUMMON`, `NOSLEEP`, `NOBASH`, `NOBLIND`, `NOKILL` | unmapped → report |
| `AFF_*`, e-specs, sex, keyword list | unmapped → report |

`MOB_ISNPC` and `MOB_NOTDEADYET` are loader bookkeeping and are not reported.

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
| `AFF_*` affect-flags | unmapped → report (no target field yet) |
| extra flags, `APPLY_CLASS/LEVEL/AGE/WEIGHT/HEIGHT/GOLD/EXP` | unmapped → report |
| keyword list, action description, `E` blocks | unmapped → report |

`APPLY_*` → `AffectLocation`: `1..6` → Str/Dex/Int/Wis/Con/Cha, `12` →
MaxMana, `13` → MaxHp, `14` → MaxMove, `17` → ArmorClass, `18` → Hitroll,
`19` → Damroll, `20..24` → the five saves. `APPLY_NONE` is skipped.

### 3.4 Zone resets

| Code | `ZoneCommand` | Fields |
|---|---|---|
| `M` | `PlaceMob` | `mob=tba_npc_<a1>`, `max_in_world=a2`, `room=tba.<a3>` |
| `O` | `DropItem` | `item=tba_item_<a1>`, `max_in_world=a2`, `room=tba.<a3>`; `a3` = nowhere → dropped |
| `G` | `GiveItem` | `item`, `max_in_world` |
| `E` | `EquipItem` | `item`, `slot: WearSlot` from `a3` (0..17), `max_in_world` |
| `P` | `PutItem` | `item=tba_item_<a1>`, `container=tba_item_<a3>`, `max_in_world=a2` |
| `D` | `DoorState` | `room=tba.<a1>`, `exit=dirs[a2]`, `state` from `a3` (0 open/1 closed/2 locked) |
| `R` | `RemoveItem` | `room=tba.<a1>`, `item=tba_item_<a2>` |
| `T`/`V` | — | deferred (trigger / trigger variable) |
| `*` | — | skipped (comment) |

`ResetMode`: `0` → `Never`, `1` → `WhenDesert`, `2` → `Always`. Zone flag
words are recorded as unmapped because `ZoneDef` has no flags field.

### 3.5 Areas

One `stdlib.areas` entry per converted zone: id `tba_<zone>`, name from the
`.zon` name line (e.g. `Northern Midgaard`).

---

## 4. Drop-report schema

`serde_json`, pretty-printed, terminated by a newline. Every vector is sorted
before writing, so the report is deterministic. Schema `1`:

```jsonc
{
  "schema": 1,
  "tool": "tbaport 0.1.0",
  "source": "/home/qworg/Development/tbamud",
  "out": "target/tbaport/content",
  "mode": "full",              // or "only"
  "only": null,                // or the zone number
  "totals": {
    "zones": 189, "rooms": 12700, "npcs": 3705, "items": 4765,
    "zone_commands": 18513, "deferred": 1797,
    "dangling": 3, "unmapped": 32258
  },
  "zones": [
    {
      "number": 30,
      "name": "Northern Midgaard",
      "bot": 3000, "top": 3099,
      "area": "tba_30",
      "rooms": 58, "npcs": 46, "items": 63, "commands": 166,
      "source_files": { "wld": true, "mob": true, "obj": true,
                        "zon": true, "shp": true, "trg": true, "qst": false },
      "deferred": [
        { "section": "shops", "vnum": 3000, "attach": "npc",
          "detail": "shopkeeper", "reason": "shops are deferred (doc §9)" },
        { "section": "triggers", "vnum": 3000, "attach": "mob",
          "detail": "Fixture Trigger", "reason": "DG scripts are deferred (doc §10)" }
      ],
      "dangling": [
        { "kind": "dangling_door_key",
          "from": "room tba.3040 exit west door.key",
          "target": "tba_item_3112" },
        { "kind": "dangling_exit", "from": "room tba.3002 exit down",
          "target": "tba.7026" },
        { "kind": "dangling_reset_item", "from": "zone 30 command 35 (G)",
          "target": "tba_item_3102" }
      ],
      "unmapped": [
        { "object": "npc tba_npc_3000", "kind": "mob_flag",
          "name": "NOCHARM", "detail": "" },
        { "object": "item tba_item_3000", "kind": "affect_location",
          "name": "AGE", "detail": "modifier 5" }
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

- `deferred.section`: `triggers`, `trigger_variables`, `quests`.
- `dangling.kind`: `dangling_door_key`, `dangling_exit`, `dangling_reset_mob`,
  `dangling_reset_item`, `dangling_reset_container`, `dangling_reset_room`,
  `dangling_shop_keeper`, `duplicate_shop_keeper`, `dangling_shop_produce`,
  plus the defensive `dangling_npc_stats_key` / `dangling_item_stats_key`.
  A dangling reference is **not** in the emitted YAML; the report is the only
  record of it.
- `unmapped.kind`: `room_flag`, `mob_flag`, `mob_aff`, `mob_espec`,
  `mob_damage_dice`, `mob_sex`, `npc_keywords`, `position`, `sector`,
  `exit_direction`, `exit_description_only`, `item_extra_flag`,
  `item_affect_flag`, `item_keywords`, `item_action_description`,
  `item_extra_descriptions`, `item_type`, `affect_location`, `zone_flag`,
  `zone_command_dropped`, `reset_mode`, `shop_buy_word`,
  `shop_room_restriction`, `shop_broke_temper`, `shop_bitvector`,
  `shop_trade_with`, `shop_hours`, `shop_record`, `shop_profit_buy`.

`command_flags` is the carrier for the Circle `if_flag`, which the real
`ZoneCommand` type deliberately does not model: one entry per parsed command,
in source order, keyed by its index in the reset program (`T`/`V` and comments
included in the index space but not in `commands`).

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
│   │   └── deferred.rs        # .shp / .trg / .qst scanners
│   ├── mapping.rs             # raw records → mib-stdlib/mib-tba types
│   ├── emit.rs                # OutputBundle + YAML writers
│   └── report.rs              # DropReport types + JSON writer
└── tests/
    ├── parse.rs               # per-record fixture tests
    ├── convert.rs             # round-trip, determinism, zone 30 counts
    └── fixtures/tbamud/lib/world/{wld,mob,obj,zon,shp,trg,qst}/30.*
```

`mapping.rs` runs in two phases: read every selected zone into raw records,
collect the global room/NPC/item id sets, then map and detect dangling
references. `emit.rs` serialises `mib-stdlib`/`mib-tba` values directly, so the
output cannot drift from the schema the engine loads.

---

## 6. CLI

```text
tbaport convert --source DIR [--out DIR] [--report FILE] [--only ZONE] [--validate-only]

  --source DIR       tbaMUD checkout root (must contain lib/world)
  --out DIR          staging output directory [default: target/tbaport/content]
  --report FILE      drop-report path [default: target/tbaport/drop-report.json]
  --only ZONE        convert a single zone
  --validate-only    parse/map and write the report, but emit no YAML
```

Files written: `stdlib.areas.yaml`, `stdlib.rooms.yaml`, `stdlib.npcs.yaml`,
`stdlib.items.yaml`, `tba.zones.yaml`, `tba.npc_stats.yaml`, `tba.items.yaml`.
`tba.classes.yaml` is **not** converter output: the four Diku classes live in
`class.c`, not in world files, and stay hand-authored.

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
| Deferred | `tests/parse.rs` | one shop, one trigger (attach `mob`), one quest (questmaster) |
| Schema exactness | `tests/convert.rs` | every emitted file deserialises into its real type and re-serialises to the exact bytes written (header aside) |
| Determinism | `tests/convert.rs` | two in-process conversions produce byte-identical YAML **and** byte-identical JSON report |
| Real counts | `tests/convert.rs` | `--only 30` totals equal a strict `#<digits>` scan of `30.wld`/`30.mob`/`30.obj`, and are pinned at 58/46/63 |
| Registry validation | `tests/validate_pack.rs` | the **full 189-zone** conversion loads through the real `stdlib + tba` registry and `validate_all` returns no issues |
| Full-world determinism | manual | `convert` twice into different dirs; `diff -r` the YAML trees; reports differ only in the `out` field |

The full 191-file world converts in ~2 seconds, so the real-count test runs
against the sibling checkout when present (`TBAMUD_SOURCE` or
`/home/qworg/Development/tbamud`) and skips cleanly otherwise.

---

## 8. Known gaps (all recorded in the drop report)

- **Zone `if_flag` has no `ZoneCommand` field.** Order is preserved and the raw
  flag is carried in `command_flags`.
- **`AFF_*` affect-flags on objects** (glow, hum, invisibility, anti-class
  bits) have no target field. They are named in the report.
- **Item extra flags** (`ITEM_GLOW` … `ITEM_QUEST`) likewise.
- **NPC/item keyword lists** have no `stdlib` field; `name` alone is emitted.
- **NPC sex and attack dice**; `TbaNpcStats` carries neither.
- **Item action descriptions and object extra descriptions.**
- **Room `D`-block general descriptions** (description-only exits).
- **`APPLY_CLASS/LEVEL/AGE/CHAR_WEIGHT/CHAR_HEIGHT/GOLD/EXP`** have no
  `AffectLocation`.
- **Zone flags** (`CLOSED`, `NO_IMMORT`, `QUEST`, `GRID`, …) have no field.
- **Shops** are emitted as `tba.shops`; the source's keeper temper, trade-with
  bits, room restriction, opening hours, buy words and any non-positive
  `profit_buy` have no target field and are reported as `unmapped`.
- **DG triggers and quests** remain deferred wholesale.

None of these is silently discarded: each produces a `deferred`, `dangling` or
`unmapped` entry.
