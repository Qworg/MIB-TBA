# MIB Implementation Plan — TBA Content Pack (`packs/tba/`)

> Status: **Proposed** — plan complete, no workstream started. Every claim below is
> cited to `path:line` in `/home/qworg/Development/MIB` (target) or
> `/home/qworg/Development/tbamud` (source) as of 2026-10-04.
> Duration: **~38–42 eng-weeks** (Phases 8–13), gated on Phase 8.
> Prerequisites: MIB Phases 0–7 complete; `mib-stdlib` landed as a pack
> (`packs/stdlib/`); `mib-classic-rpg` landed as a pack (`packs/classic-rpg/`).
> Crates: `packs/tba/code` (new, `mib-tba`), plus edits to `mib-stdlib`,
> `mib-classic-rpg`, `mib-content`, `mib-persist`, `mib-server`.
> Design reference: `docs/reimpl_design.md` §8 (crate layout), §13 (scope rules),
> D12 (layering); `docs/deferred.md` D-01/D-02/D-04/D-05/D-06/D-12/D-14`15.
> Source PR: none — this is the port itself.

---

## 1. Objective

Turn the playable world of **tbaMUD** (`~/Development/tbamud`, a CircleMUD 3.64
derivative) into a MIB content pack so that a MIB server can boot and serve that
world under MIB's own engine, protocol, persistence, and AI, without the tbaMUD
C code.

The deliverable is **one pack**: `packs/tba/` — a `pack.toml` plus `content/`,
optionally with a code half `packs/tba/code/` (`mib-tba`) if any tba-specific
selector cannot be expressed with the existing `stdlib.*` / `rpg.*` schema.

The pack stack is:

```text
mib-core                 engine assembly (app, tick, bridge, config)
  └── mib-stdlib         genre-agnostic gameplay + stdlib.* schemas   packs/stdlib
        └── mib-classic-rpg   RPG genre + rpg.* schemas                packs/classic-rpg
              └── mib-tba        TBA world selectors (only if needed)  packs/tba/code
                    pack "tba"   content/ = the ported world          packs/tba
```

`packs/tba/pack.toml` declares `depends_on = ["stdlib", "classic-rpg"]` and
`game = "classic-rpg"` — the `packs/tutorial` shape (`packs/tutorial/pack.toml:4-11`),
which is the only shipped pack that is both a real world *and* a genre consumer.
The user-facing statement "the stack is MIB-StdLib-TBA" is the `stdlib → tba`
edge; `classic-rpg` is present because the world ships combat/stats/equipment
data (`rpg.npc_stats`, `rpg.items`) and therefore must pass the genre gate
(`packs/classic-rpg/code/src/content.rs:96-111`).

**Explicitly not the objective:** reimplementing tbaMUD. The port keeps its
*world* (rooms, mobs, objects, shops, help, prose) and re-expresses or drops its
*behaviour* (DG triggers, C spec-procs, class/spell tables). §4 is the decision
register for every behaviour.

---

## 2. Entry state

What exists today, verified by reading the code, not the docs.

**MIB engine (target).**

- 13 registered content sections: 9 × `stdlib.*` (`packs/stdlib/code/src/content.rs:757-768`)
  and 4 × `rpg.*` (`packs/classic-rpg/code/src/content.rs:363-369`).
- Pack lifecycle is complete: manifest (`crates/mib-content/src/manifest.rs:9-45`),
  dependency-ordered load (`crates/mib-content/src/pack.rs:166-230`), module
  `[code]` check (`crates/mib-content/src/module.rs:151-183`), per-pack
  referential validation with field-path diagnostics
  (`crates/mib-content/src/validation.rs`, `crates/mib-host/src/offline.rs:93-142`),
  hot reload with an all-or-nothing safety contract (`packs/stdlib/code/src/reload.rs:138-198`).
- A working `validate-packs` CLI (`crates/mib-server/src/main.rs:33-39`).
- Playable gameplay: movement/doors/items/containers (`packs/stdlib/code/src/commands.rs`,
  `doors.rs`, `items.rs`), channels, 4-kind quests, autonomy, dialogue,
  3×3 grid combat, equipment, XP. 38 rooms of shipped content total
  (`packs/tutorial` 28 + `packs/smoke` 10).
- Workspace size: `mib-stdlib` 17,607 LOC, `mib-classic-rpg` 6,530 LOC,
  `mib-content` 1,034 LOC, engine+plumbing ~46k LOC.

**tbaMUD world (source).** All world data is git-tracked ASCII; 12,733 rooms,
29,327 exits, 3,705 mob prototypes, 4,765 object prototypes, 189 zones, 18,514
zone-reset commands, 334 shops, 1,461 DG triggers (27,801 script lines), 722
help entries, 491 socials, 2,395 room extra-descriptions. Format spec is
`doc/building.txt`; loaders are `src/db.c:1306/1890/2087/2314`, `src/shop.c:1203`,
`src/quest.c`, `src/dg_db_scripts.c`.

**The gap, in one line.** Roughly 70–75% of the source *bytes* are portable as
data; essentially 100% of the source *behaviour* is engine code
(`castle.c` 812 LOC of hand-coded quest, 13,119 LOC of DG scripting, hard-coded
class/spell tables). MIB today has **no** spawner repopulation, **no** player
death/respawn, **no** shops/economy, **no** skills/casting, **no** time/weather,
**no** help or emote commands, and **no** persistence of items or room/door
state — all of which the source world uses.

---

## 3. Background you need

### 3.1 The source: what a tbaMUD record is

| Record | Where | Shape | Port weight |
|---|---|---|---|
| Room | `lib/world/wld/<zone>.wld` | vnum, name, desc, 16-bit flags ×4, sector 0–9, 0–6 `D` exits, 0–∞ `E` extra-descs, `T` triggers | 12,733 records |
| Mobile | `lib/world/mob/<zone>.mob` | vnum, keywords, 3 descriptions, 18 flag bits, affect bits, alignment, level, hitroll, AC, `xdY+Z` hp, `xdY+Z` damage, gold, exp, positions, 13 `espec` stats, `T` triggers | 3,705 |
| Object | `lib/world/obj/<zone>.obj` | vnum, keywords, 3+action desc, 24 types, 17 extra flags, 15 wear positions, `value[0..3]`, weight/cost/rent/**min_level**/**timer**, `E`/`A`/`T` | 4,765 |
| Zone | `lib/world/zon/<zone>.zon` | builder, name, `bot top lifespan resetmode`, 18,514 reset commands (`M O G E P D R T`) with an **if-flag chain** | 189 |
| Shop | `lib/world/shp/<zone>.shp` | stock list, buy-type filters, 2 profit floats, 7 `~` messages, temper, flags, keeper vnum, `with_who`, room list, open hours | 330 loaded |
| Trigger | `lib/world/trg/<zone>.trg` | attach type (mob/obj/room), type bitvector, narg, arg, free-form DG script | 1,461 |
| Text | `lib/text/help/help.hlp`, `lib/misc/socials.new`, `lib/misc/messages` | 722 help entries (350 `@R<topic>@n` xrefs), 491 socials, 55 combat-message blocks | 1.3 MB |

**Extraction harness exists in the source and should be used.** tbaMUD ships an
IMP-only per-zone export that writes all seven record types for one zone into
`lib/world/export/`, optionally renumbered into a target zone range
(`src/genolc.c:532 do_export_zone`, registered `src/interpreter.c:147`). Boot
tbaMUD once, run `export <vnum>` for each zone, and the converter reads
canonical, self-contained, renumberable bundles instead of re-implementing seven
loaders. The loaders remain the authority for edge cases
(`fread_string` `src/db.c:3124`, `asciiflag_conv` `src/db.c:1209`).

**The docs are stale in at least four places** — the shop version tag
(`src/shop.h:59` accepts the substring `"v3.0"`, `building.txt:1439` says
tbaMUD), mob `THAC0` vs `hitroll` (`building.txt:298` vs `parse_simple_mob`),
the E-spec keyword list (`building.txt:4.6` omits `SavingPara/Rod/Petri/Breath/Spell`,
used by 101 mobs), and the object `min_level`/`timer` fields (undocumented).
**Write the converter against `src/db.c`, not `doc/building.txt`.**

### 3.2 The target: what MIB can express

Complete field surface, verified in code — this is the contract the converter
must emit into.

| Section | File | Fields |
|---|---|---|
| `stdlib.areas` | `stdlib.areas.yaml` | `name` |
| `stdlib.rooms` | `stdlib.rooms.yaml` | `area`\*, `name`\*, `description`\*, `exits{direction:{to*,hidden,door}}`, `spawns[{npc\|item}]`, `tags[safe\|no_recall\|outdoor\|start_room]`, `num`, `coord{x,y,z}` |
| `stdlib.rooms.door` | — | `locked`, `closed`, `key`, `name`, `pickable`, `difficulty` |
| `stdlib.npcs` | `stdlib.npcs.yaml` | `name`\*, `persona`\*, `dialogue`, `tags[quest_giver\|merchant\|hostile]` |
| `stdlib.items` | `stdlib.items.yaml` | `name`\*, `description`\*, `value`, `container{capacity}` |
| `stdlib.spawners` | `stdlib.spawners.yaml` | `what`\*, `max_alive`\*, `period_secs`\* |
| `stdlib.channels` | `stdlib.channels.yaml` | `name`\*, `description`, `default_subscribed`, `min_level` |
| `stdlib.quests` | `stdlib.quests.yaml` | `name`\*, `giver`\*, `steps[kill{npc,count}\|visit{room}\|collect{item,count}\|flag{flag}]`, `reward{xp,items}`, `origin` |
| `stdlib.autonomy` | `stdlib.autonomy.yaml` | `think_interval_secs`, `priority` |
| `stdlib.verbs` | `stdlib.verbs.yaml` | `description`, `effects[teleport\|say\|give\|set_flag\|spawn]` |
| `rpg.npc_stats` | `rpg.npc_stats.yaml` | `hp`\*, `attack`\*, `defense`\*, `level`\*, `hostile`, `faction`, `loot[]` |
| `rpg.items` | `rpg.items.yaml` | `kind`\* (`weapon\|armor\|consumable\|misc`), `slot`, `damage`, `armor` |
| `rpg.skills` | `rpg.skills.yaml` | `name`\*, `cost`\*, `description` — **inert, read by nothing** |
| `rpg.archetypes` | `rpg.archetypes.yaml` | `name`\*, `description`, `starting_items[]` — **inert, read by nothing** |

`*` = required. All structs are `#[serde(deny_unknown_fields)]`.

**Roughly half the authored surface produces no gameplay.** Verified dead keys:
`stdlib.items.value` (no economy), `RoomTag::{safe,no_recall,outdoor}` (only
`start_room` is read, `world.rs:335`), `NpcTag::*` (hostility comes from
`rpg.npc_stats.hostile`, `combat/systems.rs:94-121`), `NpcDef.dialogue`,
`SpawnerDef.period_secs`, `ChannelDef.min_level`, `DoorDef.difficulty`,
`VerbDef.description`, `EffectDef::Give` (prints a message, grants nothing,
`verbs.rs:187-198`), the whole of `rpg.skills` and `rpg.archetypes`. A plan that
budgets on "13 sections cover the world" is wrong; the effective surface is
**9 sections**.

### 3.3 The expressiveness delta — the real work

Grade: **(a)** fully data-driven · **(b)** data-driven with knobs · **(c)** engine
code, no content hook.

| tbaMUD feature | MIB today | Grade | Consequence |
|---|---|---|---|
| Room topology, 6 directions + `n/s/e/w/u/d` | `stdlib.rooms.exits`, 12 `ExitKey`s | (a) | **Fits.** `D0–D5` map 1:1. Zero diagonal exits in the source world. |
| Back-exits | **never auto-reversed** (`world.rs:355`) | (c) | 29,327 exits → converter must synthesise reciprocal exits and flag asymmetric pairs. |
| Extra-descriptions (signage, maps, plaques) | **no field** | (c) | 2,395 records. New `edesc` on rooms required. |
| Room flags (16: DARK, NO_MOB, PEACEFUL, SOUNDPROOF, NO_TRACK, NO_MAGIC, TUNNEL, PRIVATE, HOUSE, GODROOM, …) | `RoomTag` has 4, 1 live | (b) | Only `start_room` is read. 12,700 rooms lose all flag semantics — 2,488 DARK rooms, 509 PEACEFUL, 1,054 NO_MOB. |
| Sector/terrain (10: INSIDE/CITY/FIELD/FOREST/HILLS/MOUNTAIN/WATER_SWIM/WATER_NOSWIM/FLYING/UNDERWATER) | **no field** | (c) | Movement cost/light/boat gating gone. Data-only unless movement rules are wanted. |
| Doors + keys + locked/pickproof | `DoorDef` | (b) | Fits; `difficulty` inert, `pick` is a deterministic stub (`doors.rs:371-373`). |
| Mob stats (level, hitroll, AC, dice hp, dice damage, gold, exp, alignment, positions, 8 attributes + 5 saves) | `rpg.npc_stats` has hp/attack/defense/level | (b) | Severe narrowing. Dice expressions, hitroll/AC split, alignment, positions, saves all need schema. |
| Mob behaviour flags (AGGRESSIVE, MEMORY, HELPER, SCAVENGER, WIMPY, SENTINEL, STAY_ZONE, AWARE, …) | `hostile` only | (c) | Leashing, scavenging, wimpy-flee, group-aggro, backstab-immunity gone. |
| Object types (24) | `kind` 4 values | (c) | Weapons/armor map; 21 other types (potions, wands, scrolls, food, drinkcon, containers, keys, notes, boats, fountains, furniture, money) have no engine support. |
| Object values `value[0..3]`, wear positions (15), affects (APPLY_* 0–24) | `slot` (6), `damage`, `armor` | (b) | 6 wear slots vs 15 positions; `A` blocks (2,406 records) unmappable as-is. |
| Object economy fields (cost, rent, weight, min_level, timer) | `value` (inert) | (c) | No currency, no weight, no level gate. |
| Zone resets / repopulation (18,514 commands, if-flag chain, 189 zones, lifespan+resetmode) | `SpawnerDef.period_secs` **parsed but never read**; spawners spawn `max_alive` **locationless** NPCs once (`world.rs:392-411`) | (c) | **The largest gameplay gap.** Rooms empty out permanently and stay empty. |
| Shops (330, 1,431 stock lines, buy-type filters, 7 messages, profit, hours) | **absent** (D-02 deferred) | (c) | `NpcTag::Merchant` inert. |
| DG triggers (1,461 / 27,801 lines, `%var%` VM, 35 event types) | 5-effect verb DSL, no args (`verbs.rs:147-150`) | (c) | Dominant cost if ported; biggest quality loss if dropped. |
| Quest engine (7 `AQ_*` types, chains, timers) | 4 step types | (b) | Source has **1** quest; triggers carry the quest feel. |
| Classes (4), skills/spells (130), saving throws, titles, practice | `rpg.skills`/`archetypes` inert; no mana; `CombatAction::Skill` unreachable (`systems.rs:406`); `create-character <name> [archetype]` discards the archetype (`route.rs:1184`) | (c) | Class identity gone unless built. |
| Help (722 entries, `@R<topic>@n` xrefs) | **no `help` command at all** | (c) | 534 KB of player-facing text unreachable. |
| Socials/emotes (491) | `say` only, fixed format (`commands.rs:208-236`) | (c) | No emote verb. |
| Player death/respawn/corpse | **absent** — `end_combat` despawns NPCs only (`systems.rs:926-935`); HP lives in `Combatant` | (c) | A defeated player silently resets to max HP. |
| Item / door / room persistence | `DocKind::{Item,RoomState}` exist in `mib-persist/src/snapshot.rs:33,37`; **nothing registers them** | (c) | Doors reset on restart; ground items vanish at 300 s (`config.rs:117`). |
| Time/weather/day-night | **absent** | (c) | 2 `WTRIG_TIME` room triggers become inert. |
| Name-based target resolution | item names matched by **substring**, NPCs by **exact** (`items.rs:16-19`, `commands.rs:184`) | (c) | With 4,765 objects and 64% duplicate *room* names, `get sword` is ambiguous and there is no disambiguation prompt. |

### 3.4 Four structural constraints that shape the whole plan

These are engine facts, not preferences. Each one would invalidate a naive
"just write YAML" approach.

**C1 — One world = one pack, because exits validate per-pack.**
`RoomsSection::validate` builds `room_ids` from `defs.rooms` only
(`packs/stdlib/code/src/content.rs:444`), while `area_ids`/`npc_ids`/`item_ids`
go through `ctx.section()`, which falls back across packs
(`crates/mib-content/src/validation.rs:53-58`). **An exit to a room in another
pack fails validation** even though instantiation unions the room map
(`world.rs:145-219`). Consequence: the 12,733-room world cannot be sharded into
zone packs. Either it is one pack, or the engine gains cross-pack room
resolution, or shards are cut only at topological boundaries with no
cross-shard exits (practically impossible in a Diku world).

**C2 — One file per section, no splitting.**
`load_pack` iterates the registry and computes `content_dir.join(format!("{id}.yaml"))`
(`crates/mib-content/src/pack.rs:91-117`). There is no directory scan, no glob,
no multi-document YAML, no includes. 12,733 rooms + 29,327 exits would be a
single `stdlib.rooms.yaml` of roughly 150k lines, parsed non-streaming as
`serde_yaml::Value` then again as typed structs (`pack.rs:100-110`) — 2× peak
memory, and re-parsed on every 1 s hot-reload poll (`reload.rs:104-115`,
`config.rs:112-114`). No templating, no merge keys (`<<:` fails
`deny_unknown_fields`; `docs/design/deferred/D-09-content-templating.md:60-70`).

**C3 — The 12 `ExitKey` set covers the source.** `ExitKey` is closed at 12
directions with aliases (`crates/mib-protocol/src/command.rs:14-66`); tbaMUD
uses `D0–D5` and the world has **0** diagonal exits. This is the one place the
target is comfortably larger than the source. (`E` extra-desc exits —
"climb", "enter portal" — are the exception; tbaMUD's `E` blocks are
extra-descriptions, not exits, so there is no loss here after all.)

**C4 — Validation failure empties the world, loudly in logs and silently in
gameplay.** Load error, any validation issue, or an unsatisfied `[code]` leaves
**zero rooms** and does not exit non-zero (`world.rs:229-296`). With a 150k-line
file and `deny_unknown_fields` everywhere, one typo takes the world offline with
only a log line. And **no CI job invokes `validate-packs`** — a broken pack is
ungated unless a test loads it.

---

## 4. Scope decision register

This is the heart of the plan. Every source subsystem gets an explicit
disposition. `Port` = becomes content; `Adapt` = becomes content via a new
schema field/section; `Build` = new engine code; `Drop` = deliberately not
ported, with the loss named.

### 4.1 Portable now (mechanical, no engine change)

| Source | Target | Notes |
|---|---|---|
| Room vnum → id, name, description | `stdlib.rooms` | Namespace ids `tba.<vnum>`; rooms are the only place vnums survive. |
| `D0–D5` exits, door flag 1/2, key vnum | `stdlib.rooms.exits` + `door` | Synthesise reciprocal exits (C3.3); flag asymmetric pairs in the converter report. |
| Mob name/short/long/detailed desc | `stdlib.npcs.name` + `persona` | `persona` is **mandatory** (`content.rs:222`) and feeds both the LLM and `look <npc>`. Long+detailed desc concatenated is the honest persona. |
| Object name/short/long desc | `stdlib.items` | 4,765 prototypes. |
| Mob hp/level, and attack/defense derived from hitroll/AC | `rpg.npc_stats` | Lossy mapping — documented in §4.3. |
| Weapon damage → `damage`, armor AC → `armor`, wear position → `slot` | `rpg.items` | 6 slots: head/body/hands/feet/main_hand/off_hand. Map 19 source wear positions onto these, dropping finger/neck/wrist/waist/legs/shield/about. |
| Room `spawns` (initial population) | `stdlib.rooms.spawns` | Static boot population only. |
| Kill/visit/collect quests implied by triggers | `stdlib.quests` | Hand-converted, high-value only. |
| Socials | `stdlib.verbs` (see W13.3) or a new `stdlib.socials` | 491 socials with target-slot semantics. |
| Combat message blocks | constants or a new `stdlib.messages` | 55 blocks; low priority. |

### 4.2 Requires a decision now (recorded; revisit at the stated trigger)

| ID | Decision | Rationale | Trigger to revisit |
|---|---|---|---|
| **D-T1** | **World scope = a curated subset, not all 189 zones.** Only 45 of 189 zones are built to 100 rooms; 3 zones have ≤6 rooms; 144 are partial. Start with the 45 complete zones (~4,500 rooms) plus Midgaard (zone 30) as the tutorial slice. | 12,733 rooms × ~930 desc chars/room is ~11 MB of prose; the tail is stubs and duplicate filler ("In the Mist" ×98). | A zone is requested by name, or the 45 are green and the owner wants the remainder. |
| **D-T2** | **DG triggers are NOT ported in Phase 1.** The 1,461 triggers become: (a) `stdlib.quests` where they encode kill/fetch/visit objectives, (b) `stdlib.verbs` where they encode a fixed response, (c) dropped where they are ambience. | A DG VM is ~13k LOC of C plus a 299-command interpreter dependency. The verb DSL has no arguments (`verbs.rs:147-150`) and only 5 effects. Porting the VM is a separate multi-phase project, larger than the pack. | A curated trigger set (>50 triggers) is identified whose loss is demo-visible, or a player-facing requirement names trigger behaviour. |
| **D-T3** | **Shops and economy are out of Phase 1 scope** (matches MIB D-02). | No currency system exists; `ItemDef.value` is inert. Building shops means building economy (prices, gold, banking, `with_who` gates) — a phase of its own. | D-02's trigger ("an economy/shop design lands") or the world feels hollow without 330 shops. |
| **D-T4** | **Classes/spells/skills are out of Phase 1 scope** (matches MIB D-04). Port the *content* (4 classes, 130 spell names, titles) as inert `rpg.*` data so nothing is lost on disk, but do not build casting. | MIB has no mana (`DerivedStats` = attack/defense/hp_bonus only), `CombatAction::Skill` is unreachable, and `rpg.skills`/`archetypes` are read by nothing. Every magic item references spell vnums, so identical numbering must be preserved for a later phase. | D-04's trigger, or magic items prove worthless in play. |
| **D-T5** | **Houses and estates are dropped** (1,131 LOC, 12 HOUSE rooms, zero shipped house data). | `lib/house/` holds only a `00` placeholder; the subsystem is configured but untested in practice. | A player request names player housing. |
| **D-T6** | **Guild guards / `guild_info[]` are dropped.** | Already dead code in the source: 0 assignments, and `spec_assign.c:60` records that guildguards were migrated to triggers. | — (permanent). |
| **D-T7** | **Protocol layer is dropped** (MSP/MXP/MSDP/MSSP/ATCP/MCCP, 3,621 LOC). | ~0 content references; MIB has its own telnet + GMCP frontend. | A specific client capability is requested. |
| **D-T8** | **IBT (ideas/bugs/typos), player prefs, ASCII automap are dropped** in Phase 1. | Infra, not world; MIB has admin surfaces and no map renderer. | Player-facing demand. |

### 4.3 Behaviour that must be built (the Build list)

Ordered by hit rate against the source world.

1. **Spawner placement + repopulation** (`W8.2`) — make `period_secs` live, attach
   spawners to rooms, respawn on death, honour `max_alive`. Without this, 18,514
   reset commands become dead data and the world empties permanently.
2. **Zone/reset semantics** (`W9.5`) — model the `M/O/G/E/P/D/R` command set with
   its if-flag chain as a `stdlib.resets` section, or fold it into spawners plus
   a per-zone reset sweep.
3. **Room flags, sector, extra-descriptions** (`W8.3`) — 2,395 signage records and
   2,488 DARK rooms are player-visible.
4. **Player death/respawn** (`W8.5`) — no corpse/graveyard, but a defeated player
   must not silently resurrect.
5. **Item + door/room-state persistence** (`W8.4`) — register the existing
   `DocKind::{Item,RoomState}`; door state currently resets on every restart.
6. **Extended NPC stats + behaviour flags** (`W9.2`) — alignment, hitroll/AC,
   dice, MEMORY/HELPER/SCAVENGER/WIMPY/SENTINEL/STAY_ZONE.
7. **Extended object model** (`W9.3`) — 24 types, `value[0..3]`, `A` affects,
   weight/cost/level/timer, and behaviour for at least containers/keys/notes/food/drink.
8. **Dice expressions** (`W9.1`) — `xdY+Z` is the source's native stat format.
9. **Help + socials commands** (`W13.3`) — 722 help entries and 491 socials are
   written content that is currently unreachable.
10. **Item-name disambiguation** (`W8.6`) — 4,765 objects make substring matching
    unusable without an ordinal/`N.item` selector.

---

## 5. Workstreams

Six phases, `W8.x`–`W13.x`. Phases are contracts: exit criteria are binary and
demoable.

### Phase 8 — Engine prerequisites (blocking, in `MIB`)

Everything here lands **before** any bulk conversion, because C1–C4 make a naive
port impossible.

- [ ] **W8.1 — Section file sharding.** `mib-content`. Accept
  `content/<section-id>.*.yaml` (e.g. `stdlib.rooms.0.yaml`, `.1`, …) and merge
  per-entry in filename order, so a 12,733-room section is many files. Keep
  single-file behaviour as the degenerate one-file case. This is the enabling
  change for C2 and is the first thing built.
- [ ] **W8.2 — Spawners become real.** `mib-stdlib`. Room-linked placement,
  `period_secs`-driven repopulation, death-aware accounting against
  `max_alive`, stable `SaveId` allocation across respawns
  (`world.rs:392-411`, `world.rs:504-522`, `reload.rs:477-479`).
- [ ] **W8.3 — Room model depth.** New `RoomDef` fields: `flags[]` (closed enum
  for DARK/NO_MOB/PEACEFUL/SOUNDPROOF/NO_TRACK/NO_MAGIC/TUNNEL/PRIVATE/GODROOM/HOUSE),
  `sector` (closed enum of 10), and `edescs: [{keywords, description}]`. Wire
  DARK into scene emission, PEACEFUL into `start_combat`, NO_MAGIC as a hook.
- [ ] **W8.4 — Item and room-state persistence.** Register `DocKind::Item` and
  `DocKind::RoomState` (`packs/stdlib/code/src/lib.rs:228-247`); persist door
  open/locked/closed and ground-item identity. Fixes "doors reset on restart".
- [ ] **W8.5 — Player death/respawn.** A defeated player enters a state; respawn
  at the start room (or a configured temple); no XP penalty in Phase 1; no
  corpse (record as a deliberate narrow scope).
- [ ] **W8.6 — Target disambiguation.** `get 2.sword`, `get sword.2`, or a
  numbered prompt when a fragment matches >1 item. Same for NPCs.
- [ ] **W8.7 — Operational hardening.** Make boot failure exit non-zero under a
  `--strict` flag; add a CI job running
  `cargo run -p mib-server -- validate-packs --packs-dir packs` (currently
  **no** CI step invokes it); add a pack-load test for `packs/tba`.
- [ ] **W8.8 — Scale guard.** `auto_engage` iterates all NPCs every fixed tick
  (`systems.rs:97-113`) — `O(all_npcs)`. Add a room-local index and a
  tick-duration soak at target entity counts (`mib_tick_duration_seconds`
  histogram already exists, `mib-core/app.rs:150`).

**Exit:** a synthetic 5,000-room pack (generated, not ported) boots, validates,
survives hot reload, and holds `mib_tick_duration_seconds` p99 within the
existing SLO under a 20-session soak.

### Phase 9 — Data-model extensions (in `MIB`)

tba-specific selectors that the existing schema cannot carry.

- [ ] **W9.1 — Dice expressions.** A `Dice` serde type (`"2d6+3"`) reused by mob
  hp/damage and object `value`.
- [ ] **W9.2 — NPC stats + behaviour.** Extend `rpg.npc_stats` (or add a
  `tba.npc_stats` in `packs/tba/code` if the fields are not generally useful):
  `ac`, `hitroll`, `damage: Dice`, `gold`, `alignment`, `position`, `attributes`,
  `saves`, and a `behaviour[]` flag set (MEMORY, HELPER, SCAVENGER, WIMPY,
  SENTINEL, STAY_ZONE, AWARE, AGGRESSIVE). Wire each to a system.
- [ ] **W9.3 — Object model.** Extend `rpg.items` (or `tba.items`) with
  `weight`, `cost`, `min_level`, `timer`, `values[4]`, `affects[]`
  (APPLY_* closed enum), and an expanded `kind` covering CONTAINER, KEY, NOTE,
  FOOD, DRINKCON, LIGHT, WAND/STAFF/SCROLL/POTION (stored, not castable until
  D-T4), MONEY, BOAT, FOUNTAIN, FURNITURE, TRASH, TREASURE, WORN.
- [ ] **W9.4 — Make archetypes live.** `create-character <name> [archetype]`
  currently discards the archetype (`route.rs:1184`); `starting_items` is read by
  nothing. Wire both, so the 4 source classes map to 4 archetypes. This is
  small and unblocks class identity without D-04.
- [ ] **W9.5 — Zone reset model.** A `stdlib.resets` section (or spawner
  extension) that expresses the `M/O/G/E/P/D/R` command set and its if-flag
  chain, plus zone `lifespan`/`resetmode`. Reuses W8.2's spawn machinery.

**Exit:** every field in §4.1 and every item in §4.3 has a schema home and at
least one system reading it; `docs/content-guide.md` §5 is regenerated from the
code (it currently documents 5 of 9 `stdlib.*` sections and omits `hidden`,
`container`, `hostile`/`faction`/`loot`, `name`/`pickable`/`difficulty`).

### Phase 10 — Conversion harness (in `MIB-TBA`)

Per the `scripts/port_tutorial.py` precedent: a committed, re-runnable translator
that emits an honest report of every source feature with no MIB schema.

- [ ] **W10.1 — Source readers** for all seven record types, written against
  `src/db.c` (`fread_string` `:3124`, `asciiflag_conv` `:1209`), not
  `doc/building.txt`. Handle the `espec` extension and the numeric/keyword
  buy-type split.
- [ ] **W10.2 — ID mapping.** `tba.<zone>.<vnum>` namespacing, deterministic
  ordering, `requires_mib`/`version` pinned. Preserve spell vnums in an inert
  table for D-T4.
- [ ] **W10.3 — Emitters** per section, plus the file-sharding split from W8.1.
  Every dropped tag, unmappable wear position, asymmetric exit, and spec-proc
  vnum is logged in a machine-readable report.
- [ ] **W10.4 — Round-trip and invariant harness.** Re-read the emitted pack,
  assert counts against the source, assert every exit is reciprocal or explicitly
  flagged, assert no orphaned reference.
- [ ] **W10.5 — `export`-based extraction proof.** Boot tbaMUD, run `export <vnum>`
  for one zone, and prove the converter consumes the bundle.

**Exit:** running the converter over zone 30 (Midgaard) twice produces
byte-identical output, and the emitted pack validates clean with
`validate-packs`.

### Phase 11 — Vertical slice (in `MIB`)

- [ ] **W11.1 — Midgaard (zone 30) end-to-end.** ~100 rooms, its mobs, objects,
  doors, and its shop-free interactions, playable over telnet: account →
  character → movement → look → get/drop → kill → respawn.
- [ ] **W11.2 — One guild/shop slice** as a design probe for D-T3/D-T4: enough to
  prove whether shops are worth a phase, without building the economy.
- [ ] **W11.3 — Signage and text slice.** Room `E` extra-descriptions, a handful
  of help entries (with `@R<topic>@n` resolved by the renderer), a handful of
  socials.

**Exit:** a fresh player walks Midgaard on telnet and completes a kill→loot→respawn
loop; the pack's room/exit/npc/object counts match the source for that zone.

### Phase 12 — Bulk conversion (in `MIB`)

- [ ] **W12.1 — Zone-by-zone waves** over the curated set (D-T1), each wave
  validated and smoke-tested before the next. Waves are cut by zone, so a broken
  zone is a single revert.
- [ ] **W12.2 — Orphaned-prototype triage.** 124 orphaned mobs + 616 orphaned
  objects. Cross-check against shops (shop stock is not a zone command) before
  purging; keep orphans that shops reference.
- [ ] **W12.3 — Scale soak at real content size** (re-run W8.8's soak against the
  real pack).

**Exit:** the curated world is fully playable; `validate-packs` is green in CI;
the demo loop passes on telnet and in the web client.

### Phase 13 — Behaviour re-authoring and launch

- [ ] **W13.1 — Trigger triage.** Classify all 1,461 triggers as
  quest / verb / ambience / drop, and convert the first two classes. Publish the
  classification so D-T2's revisit trigger is measurable.
- [ ] **W13.2 — Spec-proc disposition.** The 74 vnum→behaviour assignments
  (`spec_assign.c`, `castle.c`) get an explicit drop or a conversion; `castle.c`
  (812 LOC of live bespoke quest) is the headline decision.
- [ ] **W13.3 — Help + socials commands.** `help <topic>` with the xref resolver,
  `emote`/social verbs.
- [ ] **W13.4 — Verification.** Full arc over telnet + web client; tick/latency
  dashboards; runbook entry for the `tba` pack.

**Exit:** launch review passes with the pack as the default `game.packs` world.

---

## 6. Interfaces introduced this phase

| Interface | Home | Consumers |
|---|---|---|
| Sharded section files (`stdlib.rooms.*.yaml`) | `crates/mib-content` | every pack |
| Room `flags[]`, `sector`, `edescs[]` | `mib-stdlib` (`content.rs`, `world.rs`) | scene emission, combat, movement |
| Live `SpawnerDef` (`period_secs`, room link) | `mib-stdlib` (`world.rs`, a new reset system) | `stdlib.resets` |
| `Dice` type | `mib-stdlib` (shared) | `rpg.npc_stats`, `rpg.items` |
| Extended `rpg.npc_stats` / `rpg.items` | `mib-classic-rpg` (`content.rs`, `combat/model.rs`, `equipment.rs`) | combat, equipment |
| `stdlib.resets` (or spawner extension) | `mib-stdlib` | zone lifecycle |
| `stdlib.help` (new) | `mib-stdlib` | a `help` command |
| `stdlib.socials` (new) or verb-generated | `mib-stdlib` | an `emote`/social command |
| `packs/tba/` pack | the port | `mib-server` default config |
| `tools/tbaport/` converter | `MIB-TBA` | regeneration |

---

## 7. Testing

- [ ] **Pack contract test** `crates/mib-server/tests/tba.rs`, modelled on
  `crates/mib-server/tests/tutorial.rs:29-195`: pinned count constants with a
  provenance comment, `load_packs([stdlib, classic-rpg, tba])` +
  `assert!(validate_all(...).is_empty())`, the `boot()` harness
  (`StdlibPlugin + ClassicRpgPlugin + Modules::new([...]) + ContentRegistry(full_registry())`),
  `run_schedule(Startup)`, and assertions on `RoomIndex` counts.
- [ ] **Negative fixtures** with exact field-path assertions, per
  `packs/stdlib/code/tests/content.rs:92-157` (temp dir namespaced by pid+thread).
- [ ] **Converter unit tests** per record type, using committed source excerpts
  under `tools/tbaport/tests/fixtures/`, including the four doc-vs-code drift
  cases (§3.1).
- [ ] **Determinism test:** convert twice, `diff -r` byte-identical.
- [ ] **Scale soak** (W8.8 / W12.3) with `mib_tick_duration_seconds` p99 inside
  the SLO and `mib_persist_sweep_misses_total` at 0 (the R6 check,
  `docs/runbook.md:126`).
- [ ] **End-to-end arc** over real telnet, per
  `crates/mib-server/tests/tutorial_arc.rs` (real `CARGO_BIN_EXE_mib-server`,
  mock provider, `STEP_TIMEOUT`).
- [ ] **CI gate:** add the `validate-packs --packs-dir packs` job that does not
  exist today (W8.7).

---

## 8. Exit criteria

- [ ] `packs/tba/` loads and validates clean in CI; `cargo test --workspace` green.
- [ ] The curated world (D-T1) is walkable end-to-end on telnet and the web client.
- [ ] Rooms have flags/sector/extra-descriptions; doors persist across restart;
  items persist across restart.
- [ ] Spawners repopulate on `period_secs`; zones reset per `lifespan`/`resetmode`.
- [ ] A player can die and respawn.
- [ ] `help <topic>` works with the 350 `@R<topic>@n` xrefs resolved; emote/socials work.
- [ ] Tick p99 within SLO at real content size; `mib_persist_sweep_misses_total` = 0.
- [ ] The converter is re-runnable and deterministic, with a committed report of
  every dropped source feature.
- [ ] `docs/content-guide.md` §5 regenerated from code.

---

## 9. Risks

| Risk | Local mitigation |
|---|---|
| **One 150k-line `stdlib.rooms.yaml` is a single point of failure (C2)** and one typo empties the world (C4). | W8.1 sharding lands first; W8.7 makes failure loud and CI-gated. |
| **Cross-pack exits fail validation (C1)** blocks zone sharding. | Keep the world one pack; revisit engine cross-pack resolution only if the single pack becomes unwieldy. |
| **Scope creep from 12,733 rooms.** | D-T1 curation; waves cut by zone (W12.1). |
| **DG triggers are the source's gameplay (27,801 lines)** and D-T2 drops them. | W13.1 classification makes the loss measurable; the trigger VM is a separately-costed follow-on. |
| **Silent data loss** — closed enums (`RoomTag`, `NpcTag`) drop source tags, `deny_unknown_fields` rejects invented fields. | Converter emits a machine-readable drop report (W10.3); every drop is listed in the plan's register. |
| **Doc-vs-code drift in the source** (4 known places). | Converter written against `src/db.c`; fixtures pin each drift case. |
| **Perf collapse at scale** (`auto_engage` is `O(all_npcs)`). | W8.8 room-local index + soak before bulk conversion. |
| **Name ambiguity** (4,765 objects, 64% duplicate room names). | W8.6 disambiguation; ids remain the converter's key. |
| **Hot reload renumbers GMCP `num`** on every room-touching edit. | Freeze `num`/`coord` from the source vnum at conversion time; document reload semantics. |
| **No CI pack gate** means a broken pack ships. | W8.7. |

---

## 10. Effort

Rough order-of-magnitude; `1 eng-week ≈ 30 h`.

| Phase | Workstreams | Est. (eng-weeks) |
|---|---|---|
| 8 — Engine prerequisites | W8.1–W8.8 | 7 |
| 9 — Data-model extensions | W9.1–W9.5 | 7 |
| 10 — Conversion harness | W10.1–W10.5 | 5 |
| 11 — Vertical slice | W11.1–W11.3 | 4 |
| 12 — Bulk conversion (curated set) | W12.1–W12.3 | 8 |
| 13 — Behaviour re-authoring + launch | W13.1–W13.4 | 7.5 |
| **Total** | | **~38.5** |

Sensitivity: the D-T1 world scope dominates Phase 12 (roughly linear in zones);
D-T2 (triggers) is the largest optional add-on (~+8–12 weeks for a trigger VM);
D-T3/D-T4 (shops, magic) are each ~+6–10 weeks.

---

## 11. Sequencing

```text
Phase 8 (engine prerequisites) ──┬─> Phase 9 (data model) ──┬─> Phase 10 (harness) ──> Phase 11 (slice) ──> Phase 12 (bulk) ──> Phase 13 (launch)
                                  │                          │
     C1/C2/C4 unblocked           │   schema homes exist     │   converter emits into them
```

Phase 8 is strictly serial and gates everything. Phase 9 can begin the moment
W8.1/W8.3 land (schema work parallelises with spawner/persistence work). Phase 10
can begin against zone 30 before Phase 9 completes for fields Midgaard does not
use. Phases 11–13 are serial.

---

## Deviation log

| Date | Entry |
|---|---|
| 2026-10-04 | **Plan created.** Six parallel research agents examined both codebases; the load-bearing findings (C1 same-pack exit validation at `content.rs:444`, C2 one-file-per-section at `pack.rs:91-117`, C4 empty-world-on-failure at `world.rs:229-296`, and the 9-section effective content surface) were re-verified by direct read before this document was written. Two agent counts disagreed marginally (12,700 vs 12,733 rooms); the code-verified `grep -c '^#'` value (12,733) is used, and the converter's own census is the authority. |
