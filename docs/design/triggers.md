# DG Script Triggers (TBA-E20) — corpus, converter schema, and runtime

Status: **built**. This document classifies the tbaMUD DG trigger corpus,
describes how the converter emits it (`tba.triggers`), and how the game runs it
(the `crates/dg-vm` port hosted by `code/src/dg/`). `docs/03-tba-pack.md` §10 is
the pack-level summary; §6 below is the detailed design.

Machine-readable companions: [`reports/trigger-triage.json`](../../reports/trigger-triage.json),
regenerated with `tbaport triage` (§8), and the trigger section of
[`reports/drop-report.json`](../../reports/drop-report.json) (`trigger_triage`,
per-zone `triggers[]`, `trigger_coverage` and the attachment roll-up). The host
contract is documented in [`crates/dg-vm/HOST.md`](../../crates/dg-vm/HOST.md).

---

## 1. Summary

- **Corpus:** 1,461 trigger records, 113 zones, 26,340 script lines
  (26,305 non-blank).
- **Attach:** mob 795, world 384, object 282.
- **Interactivity buckets:** `interactive_response` 902,
  `ambience_flavour` 206, `combat_behaviour` 166, `gatekeeper_transport` 111,
  `quest_objective` 76.
- **Essential set:** **133** records (9.1% of the corpus) carry the
  player-visible scripted behaviour a demo can notice. It is concentrated in
  the Builder Academy tutorial (zone 0, 40), the Sanctus quest area (zone 1,
  57), and the newbie/Midgaard spine (zones 5/28/30/54/74/...).
- **Built:** an interpreter-led runtime, as recommended. The interpreter became
  the complete `dg-vm` port of every `dg_*.c` (`crates/dg-vm`), hosted by
  `code/src/dg/`; `stdlib.quests` still carries the structured
  objectives/rewards through a kill bridge. All 1,461 records are emitted and
  dispatched. Pure re-authoring could not express the conditional dialogue and
  gate behaviour.

---

## 2. Source and method

The corpus is `lib/world/trg/*.trg` in the tbaMUD checkout. Each record is:

```text
#<vnum>
<name>~
<attach_type> <type_letters> <narg>
<arglist>~
<script body>~
```

The reader mirrors `parse_trigger` (`src/dg_db_scripts.c:30`):

- line 52: `sscanf(line, "%d %s %d", &attach_type, flags, t)` — so
  `attach_type` is `0` mob, `1` object, `2` world (`dg_scripts.h`).
- line 54: `trigger_type = asciiflag_conv(flags)` — the type letters become a
  bitvector.
- line 55: `narg = (k == 3) ? t[0] : 0`.

`fread_string` in this tbaMUD tree only strips a **trailing** `~` (`src/db.c`,
the `fread_string` body: *"now only removes trailing ~'s -- Welcor"*). The
Rust reader matches that, which matters: five trigger bodies contain a bare
`~` inside a line (e.g. `say small bit of fun~ I am all that.`), and a naive
first-`~` reader truncates them.

The ticket's intake numbers are the **single-letter** record counts
(`c 373, g 289, b 195, …`, 1,391 records with exactly one type letter). This
triage expands the 70 multi-letter records too (`ab` ×19, `gh` ×16, `bg` ×11,
…, `ghjl`), so the per-type totals below run slightly higher. The spec's
"27,801 script lines" was an estimate; the authoritative C-faithful count is
26,340.

---

## 3. The DG trigger model and the type-letter mapping

### 3.1 Attach type

`attach_type` (`dg_db_scripts.c:53`, `dg_scripts.h`) is:

| value | attach |
|---:|---|
| 0 | mob (NPC) |
| 1 | object (item) |
| 2 | world (room) |

Triggers attach through `T <vnum>` lines in `.mob`/`.obj`/`.wld` records and
through zone-reset `T <attach> <vnum>` commands (see
`docs/05-converter.md` §2).

### 3.2 Type letters

`asciiflag_conv` (`src/db.c:1209`) maps lowercase `a`..`z` to bits `0..25`:

```c
for (p = flag; *p; p++) {
    if (islower(*p))  flags |= 1 << (*p - 'a');
    else if (isupper(*p)) flags |= 1 << (26 + (*p - 'A'));
}
```

The bit **names depend on the attach type**, from three tables in
`src/constants.c`:

- `trig_types[]` — mob — `src/constants.c:859` (21 entries, `a`..`t` plus a
  sentinel).
- `otrig_types[]` — object — `src/constants.c:884`.
- `wtrig_types[]` — world — `src/constants.c:909`.

The bit values themselves are the `MTRIG_*`/`OTRIG_*`/`WTRIG_*` defines in
`src/dg_scripts.h:54-109`, and the runtime test is
`TRIGGER_CHECK` (`src/dg_scripts.h:446`).

| bit | letter | mob (`trig_types`) | object (`otrig_types`) | world (`wtrig_types`) |
|---:|---|---|---|---|
| 0 | a | Global | Global | Global |
| 1 | b | Random | Random | Random |
| 2 | c | Command | Command | Command |
| 3 | d | Speech | *UNUSED1* | Speech |
| 4 | e | Act | *UNUSED2* | *UNUSED1* |
| 5 | f | Death | Timer | Zone Reset |
| 6 | g | Greet | Get | Enter |
| 7 | h | Greet-All | Drop | Drop |
| 8 | i | Entry | Give | *UNUSED2* |
| 9 | j | Receive | Wear | *UNUSED3* |
| 10 | k | Fight | *UNUSED3* | *UNUSED4* |
| 11 | l | HitPrcnt | Remove | *UNUSED5* |
| 12 | m | Bribe | *UNUSED4* | *UNUSED6* |
| 13 | n | Load | Load | *UNUSED7* |
| 14 | o | Memory | *UNUSED5* | *UNUSED8* |
| 15 | p | Cast | Cast | Cast |
| 16 | q | Leave | Leave | Leave |
| 17 | r | Door | *UNUSED6* | Door |
| 18 | s | Damage | Consume | Login |
| 19 | t | Time | Time | Time |

`UNUSEDn` slots are dropped from the classification (they are reserved bits,
not behaviour). Uppercase letters would name bits ≥ 26, which no table
defines; the corpus uses none.

Consequences worth calling out:

- `g` is the most common letter but means **Greet** on a mob, **Get** on an
  object, and **Enter** on a room. Type expansion must be per attach.
- `f` means **Death** (mob), **Timer** (object) or **Zone Reset** (room).
- `d` is **Speech** on mobs/rooms but reserved on objects.

---

## 4. Corpus classification (all 1,461)

### 4.1 By attach

| attach | records |
|---|---:|
| mob | 795 |
| world | 384 |
| object | 282 |
| **total** | **1,461** |

### 4.2 By expanded type (occurrences)

Each multi-letter record contributes to every type it names.

| type | mob | object | world | total |
|---|---:|---:|---:|---:|
| Command | 72 | 171 | 133 | 376 |
| Random | 143 | 22 | 64 | 229 |
| Greet | 197 | 0 | 0 | 197 |
| Speech | 86 | 0 | 34 | 120 |
| Enter | 0 | 0 | 108 | 108 |
| Leave | 50 | 3 | 16 | 69 |
| Fight | 66 | 0 | 0 | 66 |
| Death | 46 | 0 | 0 | 46 |
| Receive | 42 | 0 | 0 | 42 |
| Load | 30 | 9 | 0 | 39 |
| Greet-All | 36 | 0 | 0 | 36 |
| Global | 19 | 2 | 7 | 28 |
| Act | 25 | 0 | 0 | 25 |
| Wear | 0 | 22 | 0 | 22 |
| Drop | 0 | 9 | 10 | 19 |
| Get | 0 | 18 | 0 | 18 |
| Timer | 0 | 14 | 0 | 14 |
| Entry | 11 | 0 | 0 | 11 |
| Bribe | 10 | 0 | 0 | 10 |
| Zone Reset | 0 | 0 | 10 | 10 |
| Remove | 0 | 9 | 0 | 9 |
| HitPrcnt | 8 | 0 | 0 | 8 |
| Cast | 2 | 1 | 4 | 7 |
| Memory | 6 | 0 | 0 | 6 |
| Consume | 0 | 6 | 0 | 6 |
| Door | 3 | 0 | 2 | 5 |
| Give | 0 | 4 | 0 | 4 |
| Login | 0 | 0 | 3 | 3 |
| Time | 0 | 0 | 2 | 2 |

### 4.3 By zone

113 zones contain triggers. The densest are the tutorial/example zones
(0 "Builder Academy", 1 "Sanctus", 13 "TBA Examples", 27 "Memlin Caverns",
19 "Spider Swamp", 118 "The Dollhouse"), each near 100 records.

| zone | all | interactive + quest + gate | essential |
|---:|---:|---:|---:|
| 0 | 100 | 74 | 40 |
| 1 | 100 | 71 | 57 |
| 13 | 100 | 74 | 0 |
| 27 | 99 | 71 | 0 |
| 19 | 96 | 72 | 0 |
| 118 | 94 | 78 | 0 |
| 14 | 66 | 46 | 0 |
| 201 | 61 | 49 | 0 |
| 12 | 47 | 39 | 0 |
| 3 | 46 | 29 | 1 |
| 241 | 41 | 30 | 0 |
| 11 | 36 | 32 | 0 |
| 117 | 32 | 32 | 0 |
| 103 | 30 | 21 | 0 |
| 83 | 25 | 17 | 0 |
| 2 | 22 | 16 | 0 |
| 130 | 20 | 17 | 0 |
| 30 | 19 | 11 | 7 |
| 104 | 19 | 15 | 0 |
| 236 | 18 | 13 | 0 |

Full per-zone counts are in the triage JSON (`corpus.by_zone`).

### 4.4 Interactivity taxonomy

Every record gets exactly one **primary** bucket, assigned in this precedence
order (the same function is `source::deferred::classify`, and the triage JSON
stores the result):

1. **`quest_objective`** — the body names a quest state: `%object.vnum%` /
   `%obj.vnum%`, `%actor.varexists(…quest|solved|complet…)`, `questpoint(s)`,
   or the name is quest-like (`quest|reward|complet|deliver|fetch|bring|slay`)
   and the body loads/purges/sends or probes a variable.
2. **`gatekeeper_transport`** — a movement-gating type (mob `Door`/`Entry`/
   `Leave`, world `Door`/`Enter`/`Leave`, object `Leave`) or a transport
   directive (`%teleport%`, `%goto%`, `%door%`, or a body line beginning
   `open`/`close`/`lock`/`unlock`), and the record is **not** primarily
   interactive.
3. **`interactive_response`** — a player-input type: mob
   `Speech`/`Command`/`Act`/`Greet`/`Greet-All`/`Receive`/`Bribe`/`Cast`;
   object `Command`/`Get`/`Drop`/`Give`/`Wear`/`Remove`/`Consume`; world
   `Command`/`Speech`/`Enter`/`Drop`/`Cast`/`Give`.
4. **`combat_behaviour`** — mob `Fight`/`HitPrcnt`/`Death`/`Damage`/`Memory`,
   object `Timer`, world `Zone Reset`.
5. **`ambience_flavour`** — everything else. A `Random`/`Global`/`Load`/
   `Time`/`Login` record is promoted to `combat_behaviour` only if its body
   contains a combat token (`kill`, `mkill`, `dg_cast`, `damage`, `hit`,
   `attack`).

Bucket totals, and their split by attach:

| attach | quest | interactive | gatekeeper | combat | ambience | total |
|---|---:|---:|---:|---:|---:|---:|
| mob | 64 | 391 | 81 | 135 | 124 | 795 |
| object | 8 | 226 | 3 | 20 | 25 | 282 |
| world | 4 | 285 | 27 | 11 | 57 | 384 |
| **total** | **76** | **902** | **111** | **166** | **206** | **1,461** |

The taxonomy is deliberately rough: a quest turn-in is also an interactive
`Receive`, and a guard who blocks a direction is both `Leave` and a
gatekeeper. The precedence above picks the *primary* meaning; the full type
list is preserved per record in the JSON so a later phase can re-bucket
without re-parsing.

---

## 5. The essential interactivity set

### 5.1 Selection rule

The **demo spine** is the set of zones the demo playtest exercises. It is
defined in `crate::triage::DEMO_ZONES`:

- every zone reachable on foot from the start room `tba.3001` (the Temple of
  Midgaard) in the converted exit graph — a BFS over the 12,700 room exits
  reaches 1,865 rooms in 33 zones (30, 31, 32, 36, 9, 15, 25, 26, 33, 35, 40,
  41, 50–56, 60–65, 70–73, 79, 120, 150, 186); and
- the newbie/tutorial/quest hubs reached by portal or script rather than a
  walkable exit — 0 (Builder Academy), 1 (Sanctus), 5 (Newbie Farm), 28
  (Mudschool), 74 (Newbie Graveyard), 234 (Newbie School). Sanctus is the only
  shipped `.qst` area and contains the object/mob "portal to Midgaard"
  triggers, which is how the start region links to it.

A record is **essential** when its home zone or any attach owner's zone is in
the demo spine **and**:

- its bucket is `quest_objective` or `gatekeeper_transport`; or
- it is a mob `interactive_response` (the ticket's
  SPEECH/COMMAND/ACT/GREET/GREET-ALL/RECEIVE set, plus `Bribe`/`Cast`/`Door`/
  `Entry`/`Leave`).

Essentials are ranked by a fixed demo-impact score (`crate::triage::score`):
bucket weight (quest 1000, gatekeeper 600, interactive 400), +200 for the
start zone 30, +120 for the Academy zone 0, +5 per non-blank script line
(capped at 40), +40 for Greet/Speech/Receive/Command/Act, +20 for
Bribe/Cast/Door/Entry/Leave, and keyword bonuses (`questmaster`, `tutorial`,
`newbie`, `guide`, `guild`, `guard`, `portal`, `quest`). Ties break on
`(zone, vnum)`, so the ranking is reproducible.

### 5.2 Result

**133 of 1,461 records (9.1%)** are essential:

| tier | bucket | count |
|---|---|---:|
| 0 | quest_objective | 22 |
| 1 | gatekeeper_transport | 25 |
| 2 | interactive_response (mob) | 86 |
| **total** | | **133** |

By attach: mob 122, object 6, world 5. By home zone: 0→40, 1→57, 3→1, 5→7,
9→3, 28→1, 30→7, 50→1, 54→4, 63→1, 64→1, 74→10.

### 5.3 What remains

The other **1,328 records (90.9%)** are outside the demo spine:

| bucket | non-essential | note |
|---|---:|---|
| interactive_response | 816 | input handlers in non-demo zones, or object/room puzzles |
| ambience_flavour | 206 | pure `Random`/`Time` flavour |
| combat_behaviour | 166 | fight/death/damage behaviour |
| gatekeeper_transport | 86 | gates in non-demo zones |
| quest_objective | 54 | quest chains in non-demo zones |

These are all emitted and dispatched too — the essential flag is a demo-impact
ranking, not a coverage boundary (#27 is resolved: all 1,461 records convert).
None is silently dropped: every record is itemised in `drop-report.json` with
its bucket.

### 5.4 Curated table

`rank` is the demo-impact rank (1..133). Empty owner means the record exists
in the trigger file but has no `T` attachment in this source world (several
Builder Academy examples are deliberately unattached).

<!-- BEGIN GENERATED TABLE -->
#### Tier 0 — quest/objective (22)

| rank | vnum | zone | attach | types | owner | purpose |
|---:|---:|---:|---|---|---|---|
| 1 | 139 | 1 | mob | Speech | npc 3 | Questmaster Quest Assignment - 3 |
| 2 | 138 | 1 | mob | Greet | npc 3 | Questmaster Greet - 3 |
| 3 | 143 | 1 | mob | Receive | npc 3 | Questmaster Receive - 3 |
| 4 | 190 | 1 | mob | Receive | npc 25 | Mob Quest Tutorial Example Quest completion - 25 |
| 5 | 11 | 0 | mob | Receive | — | Tutorial Quest 1317 - Completion |
| 6 | 9 | 0 | mob | Greet | — | Tutorial Quest 1317 - Starter |
| 7 | 192 | 1 | mob | Greet | npc 25 | Mob Quest Tutorial Example Quest starter - 25 |
| 8 | 191 | 1 | mob | Speech | npc 25 | Mob Quest Tutorial Example Quest accepted - 25 |
| 9 | 10 | 0 | mob | Speech | — | Tutorial Quest 1317 - Accept |
| 10 | 12 | 0 | world | Enter | — | Tutorial Quest 1441 - Load 8ball |
| 11 | 3 | 0 | mob | Receive | npc 14 | Mob Tutorial Example Completion - 14 |
| 12 | 168 | 1 | object | Command | item 44 | Questpoint Setter - 44 |
| 13 | 167 | 1 | mob | Command | npc 210 | Mob Questshop Example |
| 14 | 140 | 1 | object | Command | item 16 | Quest Timer - 16 |
| 15 | 141 | 1 | object | Timer | item 15, item 16, item 17 | Quest 10 min Purge - 15, 16, 17 |
| 16 | 189 | 1 | world | Enter | room 22 | Mob Quest Example Load 8ball - 22 |
| 17 | 142 | 1 | object | Random | item 16 | Quest Timer Random - 16 |
| 18 | 144 | 1 | mob | Load | npc 15 | Quest Mob Loads Head - 15 |
| 19 | 169 | 1 | object | Get | item 15 | Quest Token check if player is on quest - 15 |
| 20 | 27 | 0 | mob | Random | — | Actor Variables Example |
| 21 | 69 | 0 | mob | Receive | npc 179 | Mob Receive Example |
| 22 | 152 | 1 | mob | Receive | npc 207 | Angel Receives Treats - 207 |

#### Tier 1 — gatekeepers/transport (25)

| rank | vnum | zone | attach | types | owner | purpose |
|---:|---:|---:|---|---|---|---|
| 24 | 3000 | 30 | mob | Leave | npc 3024 | Mage Guildguard - 3024 |
| 25 | 3001 | 30 | mob | Leave | npc 3025 | Cleric Guildguard - 3025 |
| 26 | 3002 | 30 | mob | Leave | npc 3026 | Thief Guildguard - 3026 |
| 27 | 3003 | 30 | mob | Leave | npc 3027 | Warrior Guildguard - 3027 |
| 36 | 43 | 0 | mob | Global+Random | npc 109 | Mob Wait Until Example |
| 37 | 174 | 1 | mob | Random | npc 30 | Underground Spy M30 T174 |
| 38 | 136 | 1 | mob | Leave | npc 177 | Thief Guildguard - 177 |
| 39 | 133 | 1 | mob | Leave | npc 127 | Warrior Guildguard - 127 |
| 40 | 134 | 1 | mob | Leave | npc 173 | Mage Guildguard - 173 |
| 41 | 135 | 1 | mob | Leave | npc 174 | Cleric Guildguard - 174 |
| 42 | 5400 | 54 | mob | Leave | npc 5459 | Thief Guildguard - 5459 |
| 43 | 5401 | 54 | mob | Leave | npc 5456 | Mage Guildguard - 5456 |
| 44 | 5402 | 54 | mob | Leave | npc 5457 | Cleric Guildguard - 5457 |
| 45 | 5403 | 54 | mob | Leave | npc 5458 | Warrior Guildguard - 5458 |
| 47 | 68 | 0 | mob | Entry | — | Mob Entry Example |
| 48 | 59 | 0 | world | Door | room 164 | Room Door Example |
| 49 | 188 | 1 | mob | Leave | npc 26 | TBA Magic Eight Ball Check - 26 |
| 50 | 58 | 0 | world | Leave | room 142 | Room Leave Example |
| 51 | 77 | 0 | mob | Leave | — | Mob Leave Example |
| 52 | 89 | 0 | object | Leave | item 89 | Shackles O89 |
| 57 | 78 | 0 | mob | Door | npc 200 | Mob Door Example |
| 58 | 65 | 0 | mob | Death | npc 146 | Mob Death Example |
| 61 | 5002 | 50 | mob | Leave | npc 5005 | Brass Dragon Guard - 5005 |
| 62 | 54 | 0 | world | Zone Reset | — | Room Global Zone Reset Example |
| 72 | 7402 | 74 | mob | Random+Entry | npc 7402 | Girl Skip - 7402 |

#### Tier 2 — other interactive responses (86)

| rank | vnum | zone | attach | types | owner | purpose |
|---:|---:|---:|---|---|---|---|
| 23 | 1 | 0 | mob | Greet | npc 14 | Mob Tutorial Example Quest Offer - M14 |
| 28 | 3016 | 30 | mob | Greet | npc 3012 | Kind Soul Gives Newbie Equipment |
| 29 | 3013 | 30 | mob | Act | — | Newbie Tour Guide Loader |
| 30 | 5 | 0 | mob | Bribe | npc 1312, npc 24 | Tutorial II Guard Bribe 10 - 24 |
| 31 | 3012 | 30 | mob | Act | — | Newbie Tour Guide |
| 32 | 94 | 0 | mob | Greet | npc 13 | Kind Soul 13 - Give Newbie Equipment |
| 33 | 4 | 0 | mob | Greet | npc 1312, npc 24 | Tutorial II Guard Greet - 24 |
| 34 | 7 | 0 | mob | Act | npc 1312, npc 24 | Tutorial II Guard Closes Gate - 24 |
| 35 | 8 | 0 | mob | Act | npc 1312, npc 24 | Tutorial II Guard Closes Gate 2 - 24 |
| 46 | 103 | 1 | mob | Greet | npc 196 | Mob Greet Newbie Guide - 196 |
| 53 | 22 | 0 | mob | Greet | — | IF Example |
| 54 | 33 | 0 | mob | Speech | — | Mob Quote Using Arrays |
| 55 | 63 | 0 | mob | Speech | — | Mob Speech and Expressions Example |
| 56 | 98 | 0 | mob | Act | npc 98 | Mob Act - 98 Teleporter Give |
| 59 | 46 | 0 | mob | Speech | npc 25500 | Parrot Array Example |
| 60 | 32 | 0 | mob | Greet | — | Mob Checks Player Inventory |
| 63 | 66 | 0 | mob | Greet | — | Mob Greet Example |
| 64 | 194 | 1 | mob | Act | npc 17 | TBA Greeting - 17 |
| 65 | 38 | 0 | mob | Receive | — | Mob Receive and Assemble Example |
| 66 | 72 | 0 | mob | Bribe | npc 31 | Mob Bribe Example |
| 67 | 153 | 1 | mob | Speech | npc 207 | Angel Follows Masters Commands - 207 |
| 68 | 166 | 1 | mob | Greet | npc 135 | Cast spells on Greet - M135 |
| 69 | 6300 | 63 | mob | Greet | npc 6318 | Near Death Trap Mahatma - 6318 |
| 70 | 47 | 0 | mob | Greet | — | Mob Greet Steal |
| 71 | 102 | 1 | mob | Command | — | Mob Command - portal to Midgaard |
| 73 | 197 | 1 | mob | Greet | npc 22 | TBA Hallway - 22 |
| 74 | 62 | 0 | mob | Command | npc 215 | Mob Command Example |
| 75 | 64 | 0 | mob | Act | npc 112 | Mob Action Example |
| 76 | 21 | 0 | mob | Greet | — | Transform Example |
| 77 | 76 | 0 | mob | Cast | npc 128 | Mob Cast Example |
| 78 | 20 | 0 | mob | Speech | — | Rumble's Spy |
| 79 | 67 | 0 | mob | Greet-All | — | Mob Greet-All Example |
| 80 | 196 | 1 | mob | Command | npc 17 | TBA Capital Letters Test - 17 |
| 81 | 74 | 0 | mob | Greet | — | Mob Memory Example Part 1 |
| 82 | 198 | 1 | mob | Greet | npc 21 | TBA Give Suggestions - 21 |
| 83 | 0 | 0 | mob | Greet | — | Non-attachable trigger |
| 84 | 112 | 1 | mob | Greet | npc 184 | Mob Greet Morgan - 184 |
| 85 | 110 | 1 | mob | Greet | npc 147 | Mob Greet Logan - 110 |
| 86 | 106 | 1 | mob | Greet | npc 197 | Mob Greet Carpenter - 197 |
| 87 | 121 | 1 | mob | Greet | npc 188 | Mob Greet Butcher - 199 |
| 88 | 122 | 1 | mob | Greet | npc 111 | Mob Greet Rowan - 111 |
| 89 | 123 | 1 | mob | Greet | npc 124 | Mob Greet Fiona - 124 |
| 90 | 155 | 1 | mob | Greet | npc 207 | Check for treats - 207 |
| 91 | 109 | 1 | mob | Greet | npc 108 | Mob Greet Sarge - 109 |
| 92 | 160 | 1 | mob | Greet | npc 191 | Puppy plays - 191 |
| 93 | 105 | 1 | mob | Greet | npc 140 | Mob Greet Hannibal - 140 |
| 94 | 113 | 1 | mob | Greet | npc 182 | Mob Greet Ingrid - 182 |
| 95 | 116 | 1 | mob | Greet | npc 109 | Mob Greet Hazel - 109 |
| 96 | 117 | 1 | mob | Greet | npc 158 | Mob Greet Carla - 158 |
| 97 | 119 | 1 | mob | Greet | npc 102 | Mob Greet Liam - 119 |
| 98 | 120 | 1 | mob | Greet | npc 187 | Mob Greet Baker - 187 |
| 99 | 125 | 1 | mob | Greet | npc 186 | Mob Greet Healer - 186 |
| 100 | 126 | 1 | mob | Greet | npc 185 | Mob Greet Sareth - 185 |
| 101 | 108 | 1 | mob | Greet | npc 104 | Mob Greet Rhian - 104 |
| 102 | 111 | 1 | mob | Greet | npc 107 | Mob Greet Branwen - 111 |
| 103 | 118 | 1 | mob | Greet | npc 101 | Mob Greet Ian - 101 |
| 104 | 124 | 1 | mob | Greet | npc 106 | Mob Greet Lugdach - 106 |
| 105 | 302 | 3 | mob | Greet | npc 172 | Mob Greet direction |
| 106 | 127 | 1 | mob | Act | npc 156 | Mob Act - 156 speaker greet |
| 107 | 555 | 5 | mob | Greet | npc 555 | Bat Warning - 555 |
| 108 | 6400 | 64 | mob | Greet | npc 6406 | Near Death Trap - 6406 |
| 109 | 130 | 1 | mob | Bribe | npc 165 | Mob Bribe Beggar - 165 |
| 110 | 526 | 5 | mob | Greet | npc 526 | Helen Welcome - 526 |
| 111 | 129 | 1 | mob | Greet | npc 165 | Mob Greet Beggar - 165 |
| 112 | 500 | 5 | mob | Greet | npc 500 | Bunny - 500 |
| 113 | 584 | 5 | mob | Speech | npc 584 | Arthur Welcome - 584 |
| 114 | 107 | 1 | mob | Greet | npc 103 | Mob Greet Shiro - 103 |
| 115 | 114 | 1 | mob | Greet | npc 110 | Mob Greet Corwin - 110 |
| 116 | 115 | 1 | mob | Greet | npc 119 | Mob Greet Banker - 119 |
| 117 | 504 | 5 | mob | Speech | npc 504 | Rooster teach kill - 504 |
| 118 | 7406 | 74 | mob | Random+Greet | npc 7406 | Mother - 7406 |
| 119 | 7412 | 74 | mob | Random+Greet | npc 7412 | Pickpocket - 7412 |
| 120 | 520 | 5 | mob | Greet | npc 520 | Farmer Welcome - 520 |
| 121 | 7409 | 74 | mob | Random+Greet | npc 7409 | Stern - 7409 |
| 122 | 128 | 1 | mob | Act | npc 156 | Mob Act - 156 speaker goodbye |
| 123 | 901 | 9 | mob | Greet | — | snarl with glee |
| 124 | 902 | 9 | mob | Greet | — | glare |
| 125 | 907 | 9 | mob | Greet | — | Flash Light |
| 126 | 7403 | 74 | mob | Random+Greet | npc 7403 | Squat - 7403 |
| 127 | 7404 | 74 | mob | Random+Greet | npc 7404 | Dwarf - 7404 |
| 128 | 7405 | 74 | mob | Random+Greet | npc 7405 | Angray Man - 7405 |
| 129 | 502 | 5 | mob | Speech | npc 502 | Mouse teach scan - 502 |
| 130 | 2800 | 28 | mob | Greet | — | new trigger |
| 131 | 7408 | 74 | mob | Random+Greet | npc 7408 | Woman - 7408 |
| 132 | 7411 | 74 | mob | Random+Greet | npc 7411 | Ancient - 7411 |
| 133 | 7417 | 74 | mob | Random+Greet | npc 7417 | Mourner sob - 7417 |

<!-- END GENERATED TABLE -->

---

## 6. Native design (built)

### 6.1 Decision: interpreter-led, now the whole VM

The design chose the **hybrid, interpreter-led** path: keep the scripts as
scripts and evaluate them against engine events rather than re-authoring them
as `stdlib.quests`/`stdlib.verbs`/LLM dialogue, which cannot express the
conditional text, waits and gates the corpus uses; `stdlib.quests` keeps the
structured objectives and rewards. Option (b)'s "bounded subset" grew into the
**complete** `dg-vm`, a line-faithful, engine-agnostic port of every `dg_*.c`.
`code/src/dg/` is the game's host side; `code/src/triggers/` keeps only the
content schema and the kill-to-`QuestEvent` bridge. `docs/03-tba-pack.md` §10
is the pack-level summary of the same system.

### 6.2 Content schema (`tba.triggers`)

The converter ([`tools/tbaport`], `docs/05-converter.md`) emits one shard per
zone, `tba.triggers.<zone>.yaml` (like `stdlib.rooms`, so a zone re-converts
alone). It writes **all** 1,461 records, not just the demo set — `attach_type`,
the raw `flags` letters and the lowercased expanded `types`, `narg`, `arglist`,
the verbatim (colour-stripped) `script`, and the ordered `attachments` map:

```yaml
# tba.triggers.1.yaml — generated, hand-editable
triggers:
  tba_trig_139:
    vnum: 139
    name: Questmaster Quest Assignment - 3
    attach_type: mob               # the kind the record was authored for
    flags: d                       # raw .trg letters
    types: [speech]                # lowercased expanded letters (per attach)
    narg: 100
    arglist: ""                    # match text for speech/command
    script: |-                     # the DG body, verbatim (colour-stripped)
      if %actor.is_pc% && !%actor.varexists(quest_mice)%
        say Can you help me, %actor.name%?
attachments:                       # owner -> trigger ids, in source order
  npc:tba_npc_3:
  - tba_trig_139
```

`attach_type` is the kind the record was authored for; `types` is expanded from
it **per the record's `attach_type`** via the `trig_types[]`/`otrig_types[]`/
`wtrig_types[]` tables (§3.2). `attachments` is the owner's `proto_script`
order from its `T` lines, followed by any zone-reset `T <attach> <vnum>`
commands folded into prototypes; a trigger listed twice is attached twice.
Owners are `npc:<id>`, `item:<id>` or `room:<id>`, and the pack loader merges
per-zone shards and rejects a duplicate owner key. `code/src/triggers/content.rs`
is the serde schema and validation; the runtime parses each `script` once at
world build (and on hot reload) into the VM's `Program`.

Coverage comes from `dg_vm::analyze`, run by the converter over every emitted
script and written to `reports/drop-report.json` (`trigger_coverage`): line
classes, control keywords, statements, `wait` forms, script commands,
unknown/dynamic commands, field references and structural errors. The report's
supported-mob-command list comes from the game's `cmd_info[]` table through
`mib_tba::dg::mobcmd::supported_mob_words()`. Regeneration is in
`docs/runbook.md` §5.

### 6.3 The `dg-vm` crate and its host contract

`crates/dg-vm` is deliberately dependency-free (empty `[dependencies]`) and
engine-agnostic: it reaches the world only through the `DgHost` trait, so it
builds and tests in seconds without the engine and the converter uses its
analyzer without linking the game (969 tests). `crates/dg-vm/src/lib.rs` carries
the model and call graph; the layering is:

- `types` (ids, variables, `Script`/`TrigInstance`s, trigger-type bits,
  `MAX_SCRIPT_DEPTH`), `cstr` (the C string helpers), `lookup` (text name →
  entity, shared by every module);
- `script` (a body as classified lines, `Program`), `driver` (`script_driver`:
  control flow and built-in statements), `expr` (`eval_expr`/`eval_op`), `subst`
  (`%…%`), `text` (string functions), `fields_char`/`fields_obj`/`fields_room`
  (the field tables);
- `commands` (the `m*`/`o*`/`w*` script commands), `magic` (`dg_cast`/
  `dg_affect`), `dispatch` (`dg_triggers.c`: which trigger fires for which
  event), `analyze` (static coverage), `mock` (an in-memory host for tests).

A script command can fire further triggers, so the graph is re-entrant through
the host and bounded by `types::MAX_SCRIPT_DEPTH`.

`DgHost` unions nine traits — `BaseHost`, `WorldHost`, `SubstHost`, `CharHost`,
`ObjHost`, `RoomHost`, `CmdHost`, `MagicHost`, `DispatchHost` — **134 methods**,
each a typed, data-level fact naming the C expression it stands for. Every
method's C expression, unmodelled return value and VM callers, plus the numbers
a host interprets (directions, wear positions, flags, levels, spells), are in
`crates/dg-vm/HOST.md`. A primitive handed a dead `Uid` does nothing and
returns the neutral value; a fact the host does not model is reported through
`BaseHost::unsupported` (§6.5).

### 6.4 The game runtime (`code/src/dg/`)

`code/src/dg/` is the ECS side of the contract:

| module | responsibility |
|---|---|
| `mod.rs` | `register`/`register_runtime`, the `DgVm` run state, and `with_host`. |
| `host/` | `EcsHost`: the nine `DgHost` traits against the world, one file per trait. |
| `intake/` | the systems that observe gameplay and call `dg_vm::dispatch` (`commands`, `movement`, `items`, `combat`, `world`, `acts`). |
| `mobcmd` | the NPC command interpreter (`cmd_info[]`) behind `mud_command`, and the `mudcommand` text function. |
| `ids` | `Uid` ↔ `Entity` mapping and the `DgOrder`/`DgBirth` stamps that reproduce the C's linked-list order. |
| `script` | `DgScript`/`DgMemory` per entity; `DgProtos` and `attach_proto_scripts` built from `tba.triggers`. |
| `waits` | the pulse clock and the `wait` queue (the random/time heartbeats). |
| `vars` | `TbaTriggerVars`, the persisted player `remote`/`global` variables. |
| `chardata` | facts the pack has no component for (title, practices, quest points, conditions, script affects, NPC purse), so a script's write is visible to the next read. |
| `out` | text delivery to the players a script addresses. |

**`with_host`** is the single entry point for trigger activity. Each call
ensures ids, gives players their scripts, attaches prototype triggers, fires the
`load` trigger of every mob and object it sees for the first time, and runs the
caller's closure with an `EcsHost`. Only when the outermost call ends with the
VM idle does it sweep extracted characters and mirror player variables back to
`TbaTriggerVars`; a nested call does only the id pass, matching the C's depth
behaviour.

**Intake** reproduces `command_interpreter`'s fixed sequence around the engine
router (`intake/mod.rs`, `DgIntake`):

1. **`CommandGate`** (pre-route) — world, then mob, then object `command`
   triggers; the first non-zero return claims the command.
2. **`PreCommit`** (pre-route) — the triggers that can refuse an action before
   it happens (`leave`, `door`, `get`, `drop`, `give`, `receive`, `bribe`,
   `wear`, `remove`, `consume`, `cast`); a refusal claims the command.
3. the engine routes the command and the pack's handlers run, honouring
   `is_claimed`.
4. **`PostRoute`** — the reactions (`speech`, `act`, `enter`, `greet`, `entry`,
   `memory`).

Combat (`fight`/`hitprcnt`/`damage`/`death`/`cast`), movement and login, item
actions, zone/load/timer and the periodic `random`/`global`/`time` heartbeats
hang off the same dispatch functions. `docs/runbook.md` §6 lists the systems and
their schedules.

### 6.5 What stays unmodelled

The VM implements the language; the remaining gaps are facts the **game host**
does not model. Each is reported through `BaseHost::unsupported` with a short
stable key and counted in `DgLog` (a bounded ring plus a per-key count) — never
silently dropped. `code/tests/e2e_corpus.rs` runs every record once in-game and
allow-lists the eight keys the corpus exercises:
`char_npc_no_combat_state`, `char_pref_flags`, `char_set_stat_no_combat_state`,
`char_sex`, `char_update_pos_dead`, `char_weight`, `magic_affect_bit`,
`mtransform.stats`. `script_log` is the VM's error sink, not an unmodelled key.

The full set the host reports (from `code/src/dg/host/*`, `code/src/dg/intake/*`,
`code/src/dg/mobcmd*`, and the pack handlers they call: `chardata.rs`,
`consume.rs`, `zones/mod.rs`):

| group | keys |
|---|---|
| character | `char_sex` / `char_set_sex`, `char_weight`, `char_str_add` / `char_set_str_add`, `char_wait_state` / `char_set_wait_state`, `char_pref_flags`, `char_act_flag_inert`, `char_quest_completed`, `char_set_class`, `char_set_level_npc`, `char_npc_no_combat_state`, `char_set_stat_no_combat_state`, `char_update_pos_dead` |
| object | `obj.alias`, `obj.val.effect`, `obj.cost.effect`, `obj.rent.effect`, `obj.weight.effect`, `obj.apply.effect` |
| magic | `magic_spell_not_carried`, `magic_obj_target`, `magic_alter_objs`, `magic_spell_inert`, `magic_affect_apply`, `magic_affect_bit` |
| world / items | `mhunt`, `mtransform.stats`, `exit.description`, `carry_limit`, `put_curse_container`, `food_bites`, `food_poison`, `drink_level`, `drink_poison`, `quaff_wait_state`, `death_cry`, `reset_wear_otrigger` |
| NPC interpreter | `mobcmd.aff_flying`, `mobcmd.aff_waterwalk`, `mobcmd.indoors`, `mobcmd.container_closed`, `mobcmd.container_door`, `mobcmd.gemote`, `mobcmd.force_player`, and `mobcmd.<command>` for every `cmd_info[]` row whose handler is `Unmodelled` |

A write/read pair (`char_str_add` / `char_set_str_add`, `char_sex` /
`char_set_sex`, `char_wait_state` / `char_set_wait_state`) logs the read key and
the write key separately; a script that only reads logs the read key. The
`obj.*.effect` and `char_act_flag_inert` keys mean the write is recorded in the
per-instance override but no pack system reads it (the pack reads the
prototype), so the value is visible to the next script but not to the pack's own
systems. A `mobcmd.*` key is a command word the interpreter recognises but does
not model; a word it does not recognise is `Huh?!?`, not an unmodelled key.

---

## 7. Phased plan for #15 (complete)

All phases shipped on `feat/dg-vm`; the sizes below were the original
one-contributor estimates.

- **Phase 0 — triage + design.** This document, `reports/trigger-triage.json`,
  the converter classification, and the `tbaport triage` subcommand.
- **Phase 1 — `tba.triggers` content + parser.** Every record emitted as
  `tba.triggers.<zone>.yaml` and parsed into `Program` at build/reload.
- **Phase 2 — evaluation core.** Context, `%…%` substitution, text and control.
- **Phase 3 — state + quests.** Variables, `remote`/`global` persistence, and
  the `stdlib.quests` kill bridge (`NpcDefeated` → `QuestEvent::Killed`).
- **Phase 4 — time + movement.** `wait`, doors, gates and teleport.
- **Phase 5 — combat + hardening.** Combat and periodic events, `dg_cast`
  mapping, the nested-trigger depth cap, and the corpus smoke test.

The **full DG VM** was then built as `crates/dg-vm` (#29) and hosted by
`code/src/dg/`, superseding the bounded subset the phases assumed. What
remains is not interpreter coverage but the host facts in §6.5; the
corpus-level quirks are the 8 `attach_type` mismatches (kept, named drops) and
the 432 records no owner attaches. `docs/runbook.md` §8 tracks the runtime-side
gaps.

---

## 8. Regenerating the triage

```sh
# Standalone triage (reports/trigger-triage.json)
cargo run -p tbaport -- triage --source /path/to/tbamud --out reports/trigger-triage.json

# Full conversion, whose drop report embeds the same classification
cargo run -p tbaport -- convert --source /path/to/tbamud \
    --out target/tbaport/content --report reports/drop-report.json
```

`tbaport triage` writes records sorted by `(zone, vnum)` and rolls up counts in
`BTreeMap`s, so it is byte-deterministic. The conversion report and the
standalone triage share `source::deferred::classify`; a test pins the full
corpus totals so a parser or classifier regression is loud.
