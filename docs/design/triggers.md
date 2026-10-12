# DG Script Triggers (TBA-E20) — triage and Phase-2 native design

Status: **triage + design only**. This document classifies the tbaMUD DG
trigger corpus and specifies how the game re-authors it natively. No runtime
trigger behaviour is added by this change; `docs/03-tba-pack.md` §10 remains
the phased contract, and this document fills in its Phase-2 scope.

Machine-readable companion: [`reports/trigger-triage.json`](../../reports/trigger-triage.json),
regenerated with `tbaport triage` (§8). The per-zone classification also ships
inside [`reports/drop-report.json`](../../reports/drop-report.json)
(`trigger_triage` plus each zone's `triggers[]`).

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
- **Recommendation:** a **hybrid, interpreter-led** Phase 2 — a *minimal
  pack-native DG interpreter* (option **b**) is the backbone for the essential
  interactive/gatekeeping/ambience triggers, with the **structured
  objectives/rewards** carried by `stdlib.quests` (option **a**'s strong half).
  Pure re-authoring (a) cannot express the conditional dialogue and gate
  behaviour; the full DG VM (spec Phase 3) stays separately costed.

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
`docs/05-converter.md` §4).

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

The **demo spine** is the set of zones a Phase-2 playtest exercises. It is
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

The other **1,328 records (90.9%)** are not demo-visible for Phase 2:

| bucket | non-essential | note |
|---|---:|---|
| interactive_response | 816 | input handlers in non-demo zones, or object/room puzzles |
| ambience_flavour | 206 | pure `Random`/`Time` flavour |
| combat_behaviour | 166 | fight/death/damage behaviour |
| gatekeeper_transport | 86 | gates in non-demo zones |
| quest_objective | 54 | quest chains in non-demo zones |

These stay deferred (tracked in #27). None is silently dropped: every record is itemised in
`drop-report.json` with its bucket, and the essential flag distinguishes the
Phase-2 scope from the rest.

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

## 6. Phase-2 native design

### 6.1 The options

**(a) Re-author the essential set as `stdlib.quests` + `stdlib.verbs` +
dialogue.**

`stdlib.quests` is a good fit for structured objectives: it already models
`Kill`/`Visit`/`Collect`/`Flag` steps, per-player progress, rewards, and a
turn-in flow, and it is driven by `QuestEvent`s (`packs/stdlib/code/src/quests.rs`).
But it cannot express *presentation*: "greet the player, wait a second, branch
on a variable, teleport them". `stdlib.verbs` is static-effect-only — its
`VerbEffect` list is `teleport`/`say`/`give`/`set_flag`/`set_room_flag`/`spawn`
with no argument grammar and no conditionals (`packs/stdlib/code/src/verbs.rs`).
And the available dialogue system (`stdlib::dialogue`) is an **LLM** path
(`packs/stdlib/code/src/dialogue.rs`), not the `DialogueGraph` the spec names:
it builds a prompt and streams a generated reply. That is non-deterministic and
unsuitable for reproducing scripted quest text and exact gate logic.

**(b) Minimal pack-native DG interpreter for a language subset.**

Keep the scripts as scripts. A `mib-tba` system parses a bounded subset of the
DG language and evaluates it against engine events. The subset that covers the
corpus is small and well known:

- text: `say`, `emote`, `echo`/`%echo%`, `%echoaround%`, `%send%`, `%zoneecho%`,
  `%at%`;
- control: `if`/`elseif`/`else`/`end`, `switch`/`case`/`break`/`default`/`done`,
  `return`, `nop`, `halt`, `while` (iteration-capped);
- state: `set`, `eval`, `unset`, `remote`, `global`, `local`, and the
  `%actor%`/`%self%`/`%object%`/`%victim%`/`%target%` variable namespaces;
- effects: `%load%`, `%purge%`, `%force%`, `%teleport%`, `%door%`, `%damage%`,
  `dg_cast`, `give`, `drop`, `kill`;
- timing: `wait <n> [sec|s|tick]`, `wait until <hh:mm>`.

The runtime cost is a parser, a context/substitution engine, a small evaluator,
and a timer queue. That is more code than (a), but it preserves the original
behaviour and text exactly and covers the gatekeepers, dialogue chains and
objective loops uniformly.

**(c) Hybrid, interpreter-led.**

Use the interpreter (b) for interactive responses, gatekeepers, ambience and
combat triggers, and `stdlib.quests` (a's strong half) for the *structured*
kill/fetch/visit objectives and rewards. The interpreter emits `QuestEvent`s
(through the existing `Flags` component) when a script satisfies a step, so the
quest log/journal/turn-in work keeps the stdlib implementation instead of being
re-built inside the VM.

### 6.2 Recommendation: (c)

The hybrid is the only option that both reproduces the scripted interactions
and reuses the working quest machinery. It is essentially (b) plus a thin
bridge to (a), so it does not add a second interpretation engine — it adds a
data path.

- The essential set is **22 quest objectives, 25 gatekeepers, 86 dialogue/
  interaction** triggers. Only the 22 are a natural `stdlib.quests` fit; the
  other 111 need conditional text, movement and timers that quests/verbs
  cannot express. Pure (a) would leave most of the demo silently inert.
- The interpreter, not LLM dialogue, owns scripted text. That keeps the demo
  deterministic and source-faithful; the LLM dialogue path stays for
  free-form NPC conversation and is orthogonal.
- `stdlib.quests` keeps objective/reward persistence and journal UI out of the
  VM.

### 6.3 Content schema

The converter emits a pack-owned section, `tba.triggers` (per-zone shards like
`stdlib.rooms`, so a single zone can be re-converted). The schema is the DG
record plus its attachment, *not* a re-authoring:

```yaml
# tba.triggers.1.yaml (generated; hand-editable)
triggers:
  tba_trig_139:
    vnum: 139
    name: Questmaster Quest Assignment - 3
    attach: npc:tba_npc_3          # or room:tba.22 / item:tba_item_16
    types: [speech]                # lowercased expanded types
    narg: 100
    arglist: ""                    # DG arglist (match text for speech/command)
    script: |-                     # the DG body, verbatim (colour-stripped)
      if %actor.is_pc% && !%actor.varexists(quest_mice)%
        say Can you help me, %actor.name%?
        ...
```

The runtime parses `script` once at world build (and on hot reload) into an AST
and reports the constructs it does not understand as **named drops** in the
conversion report, so coverage is explicit rather than silent.

### 6.4 Runtime systems (`code/src/triggers/`)

| system | responsibility |
|---|---|
| `content` | serde types for `tba.triggers`; section registration beside `tba.zones`. |
| `index` | `TriggerIndex` resource: attach entity → `Vec<TriggerId>` by type; rebuilt at build/reload like `RoomIndex`. |
| `events` | `TriggerEvent` intake (speech, command, act, greet, entry, leave, receive, death, fight, damage, door, load, timer, time) assembled from existing seams (`CommandReceived`, `RoomOccupants` diffs, `NpcDefeated`/`PlayerDefeated`, `TbaRound`, the clock). |
| `context` | `TriggerContext` (actor, self, object, victim, target, direction, room) and the `%substitution%` resolver. |
| `eval` | the AST interpreter: conditions, `if`/`switch`/`while`, variables, text commands, effect directives, depth guard, iteration cap. |
| `timers` | `TriggerTimers`: `wait` queue keyed on the engine clock; survives no reboot (DG parity). |
| `quests` | bridge that turns a satisfied step into `QuestEvent::FlagSet`/`Collected` via the stdlib `Flags` component. |
| `persist` | `TbaTriggerVars` (`DocKind::Player`) for `remote`/`global` variables, so quest state survives reconnect. |

### 6.5 The hard DG features

| feature | Phase-2 coverage |
|---|---|
| **variables** | `set`/`eval`/`unset`, trigger-local frame, `remote`/`global` persisted in `TbaTriggerVars`, `local` scoped to one record. Built-in pseudo-arrays supported: `%actor.inventory%`, `%self.contents%`, `%random.char%`, `%random.<n>%`, `%random.99%`; user arrays deferred. |
| **`%actor%` context** | `TriggerContext` with `actor`/`self`/`object`/`victim`/`target`, `%direction%`, `%cmd%`, `%speech%`, `%object.vnum%`, etc. |
| **timers/`wait`** | `TriggerTimers`; `wait N sec` suspends the record's program counter; `wait until hh:mm` hooks the in-world clock (`mib_tba::clock`). |
| **`%load%`** | `%load% obj <vnum> [victim] [wear]` / `%load% mob <vnum>` route to the existing spawner/world spawn path. |
| **nested triggers** | A depth guard plus a serialized pending queue, mirroring DG's `GET_TRIG_DEPTH`/`TRIGGER_CHECK` (`dg_scripts.h:419,446`); `%attach%` deferred, `detach` supported (session-scoped). |
| **`while`/`switch`** | supported, with a per-record iteration cap to prevent a bad script wedging a tick. |
| **combat** (Phase 5) | `fight`/`hitprcnt` fire once per round from `TbaRound`; `damage`/`death`/`cast`/`load`/`door` fire from the combat/spawn/door seams; `%damage%`, `mkill`, and `detach` apply; `dg_cast` maps to `tba.spells`. |

**Explicitly not covered** (tracked in #28; the full VM is #29):
complete expression precedence and every `%…%` substitution; `dg_cast` of a
spell the catalog does not carry (named-dropped); `%transform%`,
`dg_affect`/full affect application, `mfollow`, `mremember`, `context` scope,
`%attach%`, OLC/`rdelete`, user-defined arrays (`set_array`), and `remote` to
offline players. Each is recorded as a named drop when a script uses it.

**Event types with no intake.** `TriggerKind` (`code/src/triggers/events.rs`)
dispatches `speech`, `command`, `act`, `greet`/`greet-all`, `entry`/`enter`,
`leave`, `fight`, `hitprcnt`, `damage`, `death`, `cast`, `load`, `door`,
`random`, `global`, `timer` and `time`. The remaining corpus types have no
variant, so a record of one of these types is emitted and parsed but never
fires (corpus counts from `reports/trigger-triage.json`): `receive` (42),
`wear` (22), `drop` (19), `get` (18), `bribe` (10), `zone reset` (10),
`remove` (9), `consume` (6), `memory` (6), `give` (4) and `login` (3). Twelve
of those records are in the essential set (`receive` 7, `bribe` 3, `get` 1,
`zone reset` 1).

---

## 7. Phased plan for #15

Sizes are honest engineering estimates for one contributor, excluding review.

### Phase 0 — triage + design (this change) — done

Deliverables: this document, `reports/trigger-triage.json`, the converter
classification, and the regenerating `tbaport triage` subcommand.

Acceptance:
- `docs/design/triggers.md` cites the letter mapping and counts to source.
- `reports/trigger-triage.json` is deterministic (byte-equal on re-run) and its
  pinned corpus totals match `tbaport triage`.
- `reports/drop-report.json` carries `trigger_triage` and per-zone `triggers[]`.
- `cargo test -p tbaport`, `cargo fmt --check`, and `cargo clippy -D warnings`
  are green; no pack runtime behaviour changes.

### Phase 1 — `tba.triggers` content + parser (no evaluation) — done (est. ~2–3 days)

Emit the **essential** scripts as `tba.triggers.<zone>.yaml`; parse them into an
AST at build/reload; keep the rest deferred and itemised.

Acceptance:
- The section loads, validates, and round-trips through the pack registry.
- Every essential record parses; every parse failure is a named, counted drop.
- A coverage report states the per-construct and per-record coverage.
- No behaviours fire yet; existing packs stay green.

### Phase 2 — evaluation core: context, substitutions, text, control — done (est. ~1 week)

`say`/`emote`/`echo`/`%echoaround%`/`%send%`, `if`/`elseif`/`else`/`end`,
`switch`/`case`, `return`/`nop`, substitutions, and the `speech`/`command`/
`greet` event intake.

Acceptance:
- Unit tests for substitution and condition evaluation.
- The tutorial greet/speech records produce the expected lines against a
  scripted harness; unknown substitutions resolve to a named drop.
- A malformed/looping script cannot panic or wedge a tick (iteration cap).

### Phase 3 — state + quests: variables, `%load%`/`%purge%`, bridge — done (est. ~1 week)

`set`/`eval`, `remote`/`global` with `TbaTriggerVars` persistence, and the
`stdlib.quests` bridge emitting `QuestEvent`s.

Acceptance:
- The zone-1 "Kill the Mice!" quest and the zone-0 Tutorial Quest 1317 chain
  complete end-to-end (accept → objective → turn-in → reward).
- Quest variables survive reconnect; `%load% obj`/`%purge%` behave once per
  step.
- Quest journal shows the steps and the ready-to-turn-in state.

### Phase 4 — time + movement: `wait`, doors, gates, teleport — done (est. ~1 week)

`TriggerTimers` and the movement/gate/door events.

Acceptance:
- Guild guards gate directions; `wait` sequences preserve order; a teleport
  trigger lands the player in the right room.
- Timers are cancelled when their owner is purged; no timer leaks across reset.

### Phase 5 — combat triggers + hardening — done

`fight`/`hitprcnt`/`death`/`damage`/`cast`/`load`/`door`/`random`/`global`/
`timer`/`time`, `dg_cast` mapping, the nested-trigger depth cap and serialized
pending queue, telemetry, and the curated-zone playtest.

Acceptance:
- Combat triggers fire once per round (never per damage point) with a depth cap.
- The curated demo checklist passes in zones 0/1/30/74.
- Final corpus coverage published: essential records firing vs named-dropped.
- The 133 essential records parse and dispatch without panicking (fuzz test).

### Deferred — full DG VM

The spec's Phase 3 (a clean Rust parser for the whole DG AST in
`code/src/triggers/`) remains separately costed (tracked in #29). Phase 1's parser is designed
so the AST is the same shape the full VM can extend, but no commitment is made
here beyond the subset in §6.5.

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
