# MIB-TBA runbook

How to build, test, validate, run, and regenerate the `tba` game pack — the
[Builder Academy](https://github.com/tbamud/tbamud) (CircleMUD / tbaMUD) world
and its Diku rules, running on the MIB engine.

The pack's specification is [`docs/03-tba-pack.md`](03-tba-pack.md); the
converter design is [`docs/05-converter.md`](05-converter.md). This runbook is
the operational companion.

---

## 1. What this repository is

This repository **is** the pack directory MIB loads. Its root holds
`pack.toml`, the `content/` sections, and `code/` (the `mib-tba` crate). MIB is
the shell (rooms, movement, bridge, networking, persistence); this is one game
built on it. MIB does **not** depend on this repository; a MIB operator chooses
which game to run.

Two halves live side by side:

| Half | Location | Owner |
|---|---|---|
| Engine | `../MIB` (sibling checkout) | MIB |
| Game | this repository | MIB-TBA |

`Cargo.toml` consumes the MIB crates as **path dependencies**, so both
checkouts must be present:

```text
~/Development/MIB        the engine
~/Development/MIB-TBA    this game
```

Uncommitted engine work is picked up at build time. If MIB is mid-change and
does not build, this will not either.

### Prerequisites

- Rust (this workspace is `edition = 2024`, `rust-version = 1.96.0`); see the
  engine's `rust-toolchain.toml` for the pinned toolchain.
- The sibling `MIB` checkout.
- For **regenerating** the world only: a tbaMUD source checkout (default
  `/home/qworg/Development/tbamud`).

---

## 2. Build and test

```sh
cd MIB-TBA

cargo build -p mib-tba              # the game runtime
cargo build -p tbaport             # the converter (build-time tool)

cargo test -p mib-tba              # unit + combat/death/equipment/shops/spells/rewards/pack
cargo test -p tbaport             # 22 tests: parsers, determinism, full-conversion validation
```

`cargo test -p mib-tba --test pack` is the pack contract test: it loads
`content/` through the real `stdlib + tba` registry, boots the world, and
asserts the pinned counts (189 areas / 12,700 rooms / 3,705 NPCs / 4,766 items /
327 shops), that `tba.3001` is the start room, and that `TbaModule::install`
populates `TbaContent`.

Lint/format gates (the workspace denies `unwrap_used`/`expect_used` and
`unsafe_code`):

```sh
cargo clippy -p mib-tba --lib --tests
cargo clippy -p tbaport --all-targets
cargo fmt --all -- --check
```

All of the above run in CI on every push and pull request
(`.github/workflows/ci.yml`): it checks out the MIB engine and tbaMUD as
siblings of this repository, then runs fmt, clippy with `-D warnings`, both
test suites (the converter suite includes the full-conversion registry
validation), and `validate-packs`.

---

## 3. Validate the pack (offline, no database)

The game's own binary can validate `content/` without starting the bridge,
telnet, the gateway, or the AI provider:

```sh
cargo run -p mib-tba -- validate-packs
# OK   stdlib
# OK   tba
# 2 pack(s) validated
```

Exit code is non-zero on any load or validation error. This is the CI gate to
run after any content or schema change.

### Offline operator subcommands

`mib-tba` also mirrors the engine's offline surface as thin wrappers over
`mib_host`, so a MIB-TBA deployment is migrated, backed up and restored with the
game binary. None of these start the bridge, telnet, gateway or AI provider:

```text
mib-tba migrate
mib-tba snapshot --now
mib-tba backup run | list | prune [--dry-run]
mib-tba restore-test [ID] [--from DIR] [--boot]
mib-tba wal-sync | wal-status
mib-tba restore-base [ID] [--from DIR] --into DIR [--to-time TS] [--to-lsn LSN]
```

`--config`/`--packs` stay global (they may appear before or after the command).
The semantics and failure modes are the engine's `mib-host` surface (D-35);
`restore-test --boot` and `[persist.backup] verify_boot` still fail loudly as
unimplemented rather than silently skipping the check. `restore-test` and
`restore-base` take the manifest id positionally (or as `--id`, the
`mib-server` spelling).

---

## 4. Run the game server

This repository ships its own server so the operator selects the game by
running it (MIB's own `mib-server` links `stdlib + classic-rpg` only).

```sh
cargo run -p mib-tba -- run
```

Binary flags:

```text
mib-tba [--config FILE] [--packs DIR]... [run|validate-packs]
```

- `--config FILE` — a MIB TOML config (see the engine's
  [`docs/config-reference.md`](../MIB/docs/config-reference.md)).
- `--packs DIR` — repeatable; overrides the default pack roots.
- `run` (default) — boot the full server: bridge, telnet, gateway, persistence.
- `validate-packs` — offline pack validation only.

**Default pack roots** when neither `--packs` nor a config overrides them:

```text
<MIB>/packs/stdlib     the engine's genre-agnostic base layer
<this repo>            the tba pack root (pack.toml lives here)
```

A full `run` is the engine's normal boot path: it needs whatever MIB needs
(database for persistence, an AI provider for dialogue, telnet/gateway config).
See the engine runbook [`../MIB/docs/runbook.md`](../MIB/docs/runbook.md). Use
`validate-packs` when you only want to check content.

Shut down cleanly with `SIGINT`/`SIGTERM`; the engine flushes persistence on the
way out.

---

## 5. Regenerate the world (converter)

`tbaport` reads tbaMUD world files and emits schema-exact MIB pack YAML plus a
machine-readable drop report. It depends on the schema crates (`mib-stdlib`,
`mib-tba`) and serialises their real types, so the output cannot drift from the
schemas by hand.

```sh
# Full world, staged (never writes content/ directly), with the committed report:
cargo run -p tbaport -- convert \
  --source /home/qworg/Development/tbamud \
  --out target/tbaport/content \
  --report reports/drop-report.json

# A single zone (fast iteration), or a parse-only report:
cargo run -p tbaport -- convert --source ../tbamud --out target/tbaport/z30 --only 30
cargo run -p tbaport -- convert --source ../tbamud --validate-only

# Regenerate the DG trigger triage (issue #15):
cargo run -p tbaport -- triage --source ../tbamud --out reports/trigger-triage.json

# Pick the room new characters spawn in (Circle CONFIG_MORTAL_START):
cargo run -p tbaport -- convert --source ../tbamud --out target/tbaport/content --start-room 3001
```

The committed drop report lives at **`reports/drop-report.json`** with a
human-readable **`reports/drop-report.summary.md`** beside it (the same run
writes both). Both are deterministic and diffable: every vector is sorted, and
the summary groups the surviving drops by reason. `docs/05-converter.md` §8 is
the narrative disposition table.

CLI:

```text
tbaport convert --source DIR [--out DIR] [--report FILE] [--only ZONE] [--start-room VNUM] [--validate-only]
tbaport triage  --source DIR [--out FILE] [--only ZONE]

--source DIR       tbaMUD checkout root (must contain lib/world)
--out DIR|FILE     convert: staging output directory      [default: target/tbaport/content]
                   triage:  JSON output path               [default: reports/trigger-triage.json]
--report FILE      drop-report path              [default: target/tbaport/drop-report.json]
--only ZONE        convert a single zone
--start-room VNUM  tag this room vnum as `start_room`  [default: 3001]
--validate-only    parse/map and write the report, but emit no YAML
```

The converter is **deterministic** — sorted zone/vnum iteration; converting the
same source twice is byte-identical. It reports (rather than silently drops)
every source feature with no schema home, with reason codes such as
`dangling_door_key`, `dangling_shop_produce`, and `deferred` (e.g. DG triggers).

`cargo test -p tbaport` includes a test that converts all 189 zones and loads
the result through the real `stdlib + tba` registry, asserting `validate_all` is
clean. Treat a failure there as a release blocker.

### Swapping a regenerated world into `content/`

`content/` mixes converted world data with hand-authored game rules. A swap
must replace the converted files and preserve the hand-authored ones.

| Converted (regenerate) | Hand-authored (preserve) |
|---|---|
| `stdlib.areas.yaml` | `tba.classes.yaml` |
| `stdlib.rooms.<zone>.yaml` (one shard per zone) | `tba.spells.yaml` |
| `stdlib.npcs.yaml` | `stdlib.verbs.yaml` |
| `stdlib.items.yaml` | |
| `stdlib.quests.yaml` | |
| `stdlib.help.yaml` | |
| `tba.zones.yaml` | |
| `tba.npc_stats.yaml` | |
| `tba.items.yaml` | |
| `tba.shops.yaml` | |
| `tba.socials.yaml` | |
| `tba.triggers.<zone>.yaml` (one shard per zone) | |

Procedure:

1. Convert to a staging dir with the desired `--start-room` (and
   `--report reports/drop-report.json` to refresh the committed report).
2. Remove the old `content/stdlib.rooms.yaml` and copy the converted files over
   `content/` (the room shards **replace** the single file; keeping both would
   make the engine report a duplicate key).
3. Leave the three hand-authored files untouched.
4. Update the pinned counts in `code/tests/pack.rs` if the world changed.
5. Run: `cargo test -p tbaport && cargo test -p mib-tba && cargo run -p mib-tba -- validate-packs`.

The converter always emits a synthetic `tba_gold` prototype (kind `money`) into
`stdlib.items`/`tba.items`; it backs the death-penalty gold drop and is the
reason the item count is one higher than the source object count.

---

## 6. Runtime systems (what runs when)

| System | Schedule | Notes |
|---|---|---|
| `zone_reset_system` | `FixedUpdate` | Ages zones; runs the `M/O/G/E/P/D/R` interpreter when `lifespan_mins` elapses. `when_desert` checks player occupancy by room `num` in `[bot, top]`. |
| `tick_item_timers`, `apply_night_darkness` | `FixedUpdate` (after `maintain_room_occupants`) | The shared `TbaObjectClock` ages item timers and advances the MUD calendar (75 real s/hour, 35-day months, 17-month years). `apply_night_darkness` mirrors Circle's `room_is_dark` onto authored `outdoor` rooms' runtime `RoomState.flags[Dark]`; authored `dark` rooms are never touched. The clock is runtime-only (a restart begins at year 0/day 0/hour 0). |
| `auto_engage_system`, `npc_combatant_system`, `helper_system`, `pursuit_system` | `FixedUpdate` (after `maintain_room_occupants`) | Room-local aggro; alignment ±350 gating; Memory/Sentinel/StayZone/Helper. A player the observer cannot perceive through the engine `Concealment`/`Perception` seam (fed by the pack's `TbaHidden`/`TbaSneaking` sync; see §8) is skipped (`CAN_SEE`-style stealth). |
| `resolve_rounds`, `tick_buffs` | `TbaRound` (every 3 s on the frame clock) | Combat rounds. Deliberately **not** on `tick_hz`, so combat never forces the engine tick rate. |
| `handle_combat_commands`, `handle_skill_commands`, `handle_equipment_commands`, `handle_cast_command`, `handle_shop_commands`, `handle_social_commands`, `handle_time_command`, `handle_party_commands`, `recompute_equipment_affects`, `sync_wielded_weapon` | `Update`, `BridgeSet::Game`, after `route_commands` | Genre commands. `cast`/`list`/`buy`/`sell`/`value`, `social <name>`/`socials`, `time`, `group`/`gtell`(`;`)/`follow`/`unfollow`, and the issue #25 skills `backstab`/`kick`/`bash`/`rescue`/`assist`/`consider`/`sneak`/`hide`/`pick_lock` have no fixed `Command` variant and are parsed from `PlayerInput::RawLine`, declared in `stdlib.verbs.yaml` so the stdlib router claims them. `social` resolves `$n`/`$N`/`$e`/`$E`/`$m`/`$M`/`$s`/`$S` (and `$t`/`$p`/case switches) against the actor/target and broadcasts the room and victim lines. `mib_tba::skills` gates each ability on `tba.classes` `granted_skills` + `skill_levels`, `sneak`/`hide` set the runtime-only `TbaSneaking`/`TbaHidden` (synced into the engine `Concealment`), `pick_lock` writes the room's runtime `RoomState` door state, and `find_target_in_room` resolves a target only when the engine `visible_to`/`perceive` predicate admits it for the actor. |
| `intercept_who` | `Update`, `CommandSet::PreRoute` (before `route_commands`) | The pack's only remaining visibility surface. `handle_who` is global and lists every player, so this marks `who` handled and reproduces the stdlib line with the engine `perceive` predicate applied (`Concealment` on the listed player, `Perception` on the asker). `look`/`Scene`/GMCP are filtered by the engine seam, not here. |
| `sync_concealment`, `sync_perception` | `Update`, `BridgeSet::Game` (after the genre command chain) | Mirror the pack's private `TbaHidden`/`TbaSneaking` into the engine `Concealment` and the pack's `TbaTrueSight` into the engine `Perception`, removing each carrier when its pack state clears. This is how the pack drives the engine's observer-aware projection. |
| `tick_true_sight` | `TbaRound` | Ages the pack's `TbaTrueSight` true-sight grant one combat round; `sync_perception` then drops the engine `Perception`. |
| `follow_system` | `FixedUpdate` (after `maintain_room_occupants`) | Drags a follower into the leader's new room only when the follower was standing in the leader's previous room, so a follower who wandered off is never teleported. |
| `trigger_command_system` | `Update`, `BridgeSet::Game`, after `route_commands` | Issue #15: the `speech`/`command`/`act` DG trigger intake. It reads the same `CommandReceived` stream the router does, matches the trigger `arglist`, and evaluates the supported subset (`say`/`emote`/`echo`/`%send%`/`%echoaround%`, `if`/`switch`/`while`, conditions, `set`/`eval`/`unset`/`remote`/`global`/`local`/`rdelete`, `%load%`/`%purge%`) with `%…%` substitution. It only queues effects; the drain, the timer system and `apply_trigger_casts` below apply them. A construct outside the supported subset (#28) is counted in the runtime drop log. |
| `trigger_leave_system` | `Update`, `CommandSet::PreRoute` (before `route_commands`) | Issue #15 Phase 4: runs `leave` triggers before movement commits. A record that `return 0`s marks the command handled so the move never applies (DG's `leave_wtrigger`/`leave_mtrigger` gate). |
| `trigger_timer_system` | `FixedUpdate`, before the effect drain | Issue #15 Phase 4: resumes every due `wait` (`wait N sec|tick`, `wait until HH:MM`) from `TriggerTimers`. Timers are runtime-only and do not survive a restart (DG parity). |
| `trigger_periodic_system` | `FixedUpdate`, after `trigger_greet_system` | Issue #15 Phase 5: the periodic `random`/`global`/`timer`/`time` heartbeats. |
| `trigger_round_system` | `TbaRound`, after `resolve_rounds` | Issue #15 Phase 5: enqueues `fight`/`hitprcnt` records once per fighting owner per round. |
| `trigger_damage_system`, `trigger_death_system`, `trigger_death_reset_system` | `Update` around `run_due_round`; `trigger_death_system` also on `TbaRound` | Issue #15 Phase 5: `damage` fires once per hit-point loss and `death` once per defeated NPC. The `TbaRound` instance of the death reader runs before the round's despawn applies; the `Update` instance covers spell/skill kills, and `TriggerDeathSeen` (cleared each frame by the reset system) keeps the two from firing twice. |
| `trigger_queue_system` | `Update` (`BridgeSet::Game`, after the drain) and `FixedUpdate` | Issue #15 Phase 5: drains the serialized `TriggerQueue` and dispatches each event in order, so nested and per-round firings never interleave. |
| `apply_trigger_casts` | `Update` (`BridgeSet::Game`, between the intake and the drain) and `FixedUpdate` | Issue #15 Phase 5: resolves a queued `dg_cast` against `tba.spells`. A spell the catalog does not carry is a `dg_cast_spell` named drop. `dg_cast` spends no mana and has no class gate. |
| `trigger_effects_system` | `Update` (`BridgeSet::Game`) and `FixedUpdate`, after the trigger intake | Issue #15 Phase 3: the exclusive drain for `%load%`/`%purge%` (routing through the stdlib `spawn_item_on_ground`/`spawn_item_carried_by_entity`/`spawn_npc_from_content` helpers, which take `&mut World`), for `remote`/`global` variable writes (`TbaTriggerVars`), and for the Phase 4/5 world effects (`%teleport%`, `%door%`, `%force%`, `%damage%`). It also forwards a satisfied objective's flag to the stdlib as `QuestEvent::FlagSet` and accumulates runtime named drops in `TriggerDropLog`. |
| `trigger_quest_bridge_system` | `Update`, after `award_kill_rewards` | Issue #15 Phase 3: translates each TBA native kill (`NpcDefeated`) into a stdlib `QuestEvent::Killed`, so `stdlib.quests` `Kill` steps advance. The prototype id rides on the message because the victim is despawned the same frame. `stdlib.quests` owns objective persistence, the journal, and rewards. |
| `trigger_greet_system` | `FixedUpdate`, after `maintain_room_occupants` | Issue #15: the `greet`/`greet-all`/`entry` DG trigger intake on a player room change (`Changed<Location>`), with `%direction%` derived from the exit graph. |
| `apply_death_penalty` | `Update`, `.before(handle_player_death)` | 25% of current-level XP (floored); converts `TbaGold` to a carried `tba_gold` item so the stdlib corpse sweep lands it in the corpse. |
| `player_combatant_system` | `Update`, `BridgeSet::Game` | On `CharacterCreated`, inserts `TbaProgress` + `derive_vitals` from `tba.classes`. |

Persistent player state (`SaveRegistry`, `DocKind::Player`): `TbaProgress`,
`TbaCombatant`, `TbaGold`, `TbaEquipmentSave` (equipment is relinked to live item
entities on restore), `TbaTriggerVars` (DG `remote`/`global` quest variables)
and `TbaBuffs` (active spell buffs and their remaining rounds). Parties, the
follow state and trigger `wait` timers are runtime-only components/resources;
they do not survive a restart.

Progression uses Circle's real tables: per-class `xp_table` / `thaco_table` in
`content/tba.classes.yaml`, transcribed from `tbamud/src/class.c`
(`level_exp`, `thaco`).

---

## 7. Configuration

- Pack roots: `game.packs` in the MIB config, or `--packs` on the game binary.
- Everything else (tick/frame rate, networking, persistence, AI provider,
  gateway, logging) is the standard MIB config; see the engine's
  `docs/config-reference.md`.

No TBA-specific environment variables exist. The pack's tunables are content
(`content/*.yaml`).

---

## 8. Known gaps and deliberate deferrals

These are recorded, not silent:

- **DG triggers (TBA-E20)** are ported for the essential 133-record set
  (#15, Phases 1–5). It is emitted into `tba.triggers.<zone>.yaml` and
  evaluated natively: text/control, variables, `%load%`/`%purge%`, the
  `stdlib.quests` bridge, `wait`/timers, doors/gates/teleport, and the combat
  and periodic events. Three things stay deferred: the other 1,328 triggers
  (#27); the unsupported constructs and the event types with no intake —
  `receive`, `wear`, `drop`, `get`, `bribe`, `zone reset`, `remove`,
  `consume`, `memory`, `give`, `login` — which include 12 essential records
  that are emitted but never fire (#28); and the full DG VM (#29). Every
  trigger is
  itemised in [`reports/drop-report.json`](../reports/drop-report.json)
  (grouped in [`reports/drop-report.summary.md`](../reports/drop-report.summary.md))
  and classified (attach, expanded type, interactivity bucket, script size) in
  the triage JSON [`reports/trigger-triage.json`](../reports/trigger-triage.json).
  The classification, the curated 133-trigger essential set, and the native
  design live in [`docs/design/triggers.md`](design/triggers.md); the parser's
  per-construct coverage and runtime named drops are both in the drop report.
- **Quests** (`.qst`) convert to `stdlib.quests`; a `MOB_FIND`/`MOB_SAVE`/
  `OBJ_RETURN`/`ROOM_CLEAR` objective and the reward/condition fields with no
  `QuestDef` home are named drops (`docs/05-converter.md` §3.7).
- **Text and combat messages** convert to `stdlib.help` topics; the legacy
  `lib/text/help/help.hlp` database is an `out_of_scope` global drop.
- **Socials** (`socials.new`) convert to `tba.socials`; the runtime serves
  `social <name> [target] [body part]` and `socials`. MIB models no sex, so the
  `$e`/`$m`/`$s` pronouns render as neutral singular *they/them/their*, and the
  source's stray `$i` code is a named global drop.
- **26 of 54 spells** are `Utility`: they cost mana and report the catalog's
  `unsupported` reason ("You cast <spell>, but <reason>."); their Circle
  routines are not modelled. Buffs take their duration from the spell's
  source `duration` and persist across a restart (`TbaBuffs`).
- **Affect locations**: the six attributes and the five saves are applied to
  `TbaCombatant` from worn items and buffs. The `APPLY_CLASS`/`LEVEL`/`AGE`/
  `CHAR_WEIGHT`/`CHAR_HEIGHT`/`GOLD`/`EXP` locations have no `AffectLocation`
  variant and are permanent `affect_location` drops.
- **Multi-room pursuit** is a single adjacent step, not a pathfinder.
- **Weather** (pressure, sky, the `SUN_RISE`/`SUN_SET` phases) is deferred (#30);
  only the binary day/night predicate that `apply_night_darkness` needs is
  modelled. The clock itself is runtime-only (Circle persists MUD time to a
  `time` file; MIB has no generic resource-persistence slot for it).
- **Mail and the friends list** are MIB engine features (D-06/D-07), separate
  from the Circle group model, and are deferred (#33).
- The stdlib `look` darkness check reads `Room::tags` rather than
  `Room::effective_flags`, so a night-written `RoomState.flags[Dark]` override
  persists and is the runtime authority but is not yet what `look` consults.
- **Combat skills** are implemented as pack gameplay (issue #25):
  `backstab` (thief, level 3, piercing weapon), `kick` (warrior, level 1),
  `bash` (warrior, level 12), `rescue` (warrior, level 3), `assist` and
  `consider` (universal), `sneak` (thief, level 1), `hide` (thief, level 5) and
  `pick_lock` (thief, level 2). The per-class minimum levels are
  `tba.classes.skill_levels` (transcribed from `init_spell_levels()`,
  `tbamud/src/class.c:1652-1664`). The remaining honest limits:

  - **Lockpicking** is the pack's `pick_lock <direction>` verb
    (`mib_tba::skills`); the bare stdlib `pick` command
    ([`mib_stdlib::doors::handle_pick`]) is a W4.3 stub and stays router-owned,
    so it cannot be overridden. `pick_lock` reads Circle's `do_gen_door`/`pick`
    (`tbamud/src/act.movement.c:589-608`, `ok_pick`; the success branch at
    `:560-565`), gates on the thief's `granted_skills`/`skill_levels`, requires
    a pickable, locked, closed door, and on success writes the runtime
    `RoomState` door state (`closed = true`, `locked = false`) the way the zone
    engine's `D` reset does (`code/src/zones/mod.rs::set_door_state`).
  - **Stealth is runtime-only, and drives the engine seam.** `TbaHidden`/
    `TbaSneaking` are never persisted (MIB does not persist Circle's
    affect-flag bitvector). `mib_tba::stealth::sync_concealment` mirrors them
    into the engine `Concealment` carrier while either is set and removes the
    carrier when neither is; the `stealth` value is the concealer's level
    (floored at `1`), so a default observer (`detect == 0`) cannot see them and
    an observer needs `detect >= level` to pierce the concealment. `TbaHidden`
    is cleared by `attack`, by the offensive skills and by a hostile `cast`
    (Circle's `appear()`). Circle gates only `AFF_HIDE` on `CAN_SEE`, so the
    `sneak` aggro gate is a pack extension.
  - **True sight is a spell-granted engine `Perception`.** `detect invisibility`
    and `sense life` grant the pack's round-ticked `TbaTrueSight`, which
    `mib_tba::stealth::sync_perception` mirrors into the engine `Perception`
    (`sight = SEE_INVIS`, `detect = u8::MAX`); the grant expires after the
    spell's `affect_rounds`. A detector then perceives concealed occupants
    through the same engine predicate as the projection, and can aggro/target
    them.
  - **`look`/`Scene`/GMCP respect stealth through the engine seam.** MIB's
    observer-aware projection (`build_room_view`/`emit_scene`/`find_look_target`)
    filters a concealed occupant from another actor's `look`, `Scene`, and both
    GMCP enumerations using the engine `Concealment`/`Perception` carriers, so
    the pack no longer reimplements or intercepts `look`. This also closes the
    former **arrival-scene** residual: `execute_move` → `emit_scene` now filters
    the destination's occupants for the moving observer.
  - **`who` is the one surface the pack still filters.** `handle_who` is global
    and not observer-aware, so `mib_tba::visibility::intercept_who` runs in
    `CommandSet::PreRoute`, marks `who` handled, and reproduces the stdlib line
    with the engine `perceive` predicate applied (see §6).
  - MIB has no `practice` system (#32), so a granted skill is treated as fully
    learned (`SKILL_LEARNED` = 100); Circle's DEX `dex_app_skill` bonus is not
    modelled, so `pick_lock` uses 100 as the skill level.
  - `kick`/`bash` deduct `SKILL_MOVE_COST` (10) movement and refuse at zero.
    This tbaMUD revision's `do_kick`/`do_bash` no longer contains the `GET_MOVE`
    check; the pack reinstates the classic Circle cost.
- **`MOB_SPEC` special procedures** are a non-goal (#31); the flag is a named
  `mob_flag` drop in the report. NPC e-specs (`mob_espec`) are tracked there
  too.
- The converter's drop report lists every unmapped source flag and field, with
  its disposition documented in `docs/05-converter.md` §8.

---

## 9. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Build fails in MIB code | The sibling `../MIB` checkout is mid-change; build MIB or pin a `git` dep. |
| `validate-packs` names a field path | A content reference is dangling (e.g. `rooms.X.exits.north.door.key`). Fix the content or the converter rule. |
| No spawn point / `RoomIndex.start` is `None` | No room is tagged `start_room`. Regenerate with `--start-room <vnum>` (default 3001). |
| `cast`/`list`/`buy`/`sell`/`value`/`social`/`socials`/`time`/`group`/`gtell`/`follow`/`unfollow`/`backstab`/`kick`/`bash`/`rescue`/`assist`/`consider`/`sneak`/`hide`/`pick_lock` rejected as unknown | The verb is not declared in `content/stdlib.verbs.yaml`; declare it there (effect-free). |
| Equipment lost across restart | The item has no stable `SaveId`, so it cannot be relinked; check the warning log. |
| `run` fails before the world loads | Engine config (database, AI provider). Use `validate-packs` to separate content from infrastructure. |

---

## 10. Releasing / pinning

For a released game, replace the `path` deps in `Cargo.toml` with `git` deps
pinned to an engine revision. That is what "targets MIB vX" means here, and it
insulates the game from engine churn.
