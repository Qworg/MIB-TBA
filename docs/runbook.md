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

cargo test -p mib-tba              # 93 tests: unit + combat/death/equipment/shops/spells/pack
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
# Full world, staged (never writes content/ directly):
cargo run -p tbaport -- convert \
  --source /home/qworg/Development/tbamud \
  --out target/tbaport/content \
  --report target/tbaport/drop-report.json

# A single zone (fast iteration), or a parse-only report:
cargo run -p tbaport -- convert --source ../tbamud --out target/tbaport/z30 --only 30
cargo run -p tbaport -- convert --source ../tbamud --validate-only

# Pick the room new characters spawn in (Circle CONFIG_MORTAL_START):
cargo run -p tbaport -- convert --source ../tbamud --out target/tbaport/content --start-room 3001
```

CLI:

```text
tbaport convert --source DIR [--out DIR] [--report FILE] [--only ZONE] [--start-room VNUM] [--validate-only]

--source DIR       tbaMUD checkout root (must contain lib/world)
--out DIR          staging output directory      [default: target/tbaport/content]
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
| `stdlib.rooms.yaml` | `tba.spells.yaml` |
| `stdlib.npcs.yaml` | `stdlib.verbs.yaml` |
| `stdlib.items.yaml` | |
| `tba.zones.yaml` | |
| `tba.npc_stats.yaml` | |
| `tba.items.yaml` | |
| `tba.shops.yaml` | |

Procedure:

1. Convert to a staging dir with the desired `--start-room`.
2. Copy the eight converted files over `content/`.
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
| `auto_engage_system`, `npc_combatant_system`, `helper_system`, `pursuit_system` | `FixedUpdate` (after `maintain_room_occupants`) | Room-local aggro; alignment ±350 gating; Memory/Sentinel/StayZone/Helper. |
| `resolve_rounds`, `tick_buffs` | `TbaRound` (every 3 s on the frame clock) | Combat rounds. Deliberately **not** on `tick_hz`, so combat never forces the engine tick rate. |
| `handle_combat_commands`, `handle_equipment_commands`, `handle_cast_command`, `handle_shop_commands`, `recompute_equipment_affects`, `sync_wielded_weapon` | `Update`, `BridgeSet::Game`, after `route_commands` | Genre commands. `cast`/`list`/`buy`/`sell`/`value` have no fixed `Command` variant and are parsed from `PlayerInput::RawLine`, declared in `stdlib.verbs.yaml` so the stdlib router claims them. |
| `apply_death_penalty` | `Update`, `.before(handle_player_death)` | 25% of current-level XP (floored); converts `TbaGold` to a carried `tba_gold` item so the stdlib corpse sweep lands it in the corpse. |
| `player_combatant_system` | `Update`, `BridgeSet::Game` | On `CharacterCreated`, inserts `TbaProgress` + `derive_vitals` from `tba.classes`. |

Persistent player state (`SaveRegistry`, `DocKind::Player`): `TbaProgress`,
`TbaCombatant`, `TbaGold`, `TbaEquipmentSave` (equipment is relinked to live item
entities on restore). Buffs are transient.

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

- **DG triggers (TBA-E20)** are not ported — the spec itself defers them. Every
  trigger is itemised in `target/tbaport/drop-report.json` for a later phase.
- **30 of 54 spells** are `Utility`: they cost mana and report "nothing happens
  yet"; their Circle routines (flags, summons, object/room, group, debuffs) are
  not modelled. Buffs are transient (not persisted) with a fixed round duration.
- **Affect locations with no combatant field** (the six attributes and the five
  saves) are carried on items and persisted, but not applied.
- **Multi-room pursuit** is a single adjacent step, not a pathfinder.
- **Wimpy/Aware** NPC flags and the full shop message set are carried/ignored.
- The converter's drop report lists every unmapped source flag and field.

---

## 9. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Build fails in MIB code | The sibling `../MIB` checkout is mid-change; build MIB or pin a `git` dep. |
| `validate-packs` names a field path | A content reference is dangling (e.g. `rooms.X.exits.north.door.key`). Fix the content or the converter rule. |
| No spawn point / `RoomIndex.start` is `None` | No room is tagged `start_room`. Regenerate with `--start-room <vnum>` (default 3001). |
| `cast`/`list`/`buy`/`sell`/`value` rejected as unknown | The verb is not declared in `content/stdlib.verbs.yaml`. |
| Equipment lost across restart | The item has no stable `SaveId`, so it cannot be relinked; check the warning log. |
| `run` fails before the world loads | Engine config (database, AI provider). Use `validate-packs` to separate content from infrastructure. |

---

## 10. Releasing / pinning

For a released game, replace the `path` deps in `Cargo.toml` with `git` deps
pinned to an engine revision. That is what "targets MIB vX" means here, and it
insulates the game from engine churn.
