# MIB-TBA

A **game** for [MIB](https://github.com/Qworg/MIB): the
[Builder Academy](https://github.com/tbamud/tbamud) (CircleMUD / tbaMUD) world
and its Diku game rules, packaged as the `tba` pack.

MIB is the shell — rooms, movement, the bridge, networking, persistence, the
gateway. This repository is one game built on it. There can be others
(`classic-rpg` is the other one today), and a MIB operator chooses which game to
run. Nothing here is vendored into MIB, and MIB does not depend on this
repository.

## Layout

The repository *is* the pack directory MIB loads, so its root is the
`pack.toml` root:

```text
pack.toml            pack manifest (id "tba", game "tba", depends_on ["stdlib"])
content/             the world and the tba.* sections
  stdlib.areas.yaml  ┐
  stdlib.rooms.yaml  │ world content, in the genre-agnostic stdlib sections
  stdlib.npcs.yaml   │
  stdlib.items.yaml  ┘
  tba.zones.yaml     ┐
  tba.npc_stats.yaml │ Diku domain rules, owned by the mib-tba crate
  tba.items.yaml     │
  tba.classes.yaml   ┘
code/                the `mib-tba` crate (the pack's code half)
Cargo.toml           this game's own workspace
docs/                port planning: docs/implplan.md
```

## Building

This is a normal standalone Cargo workspace. It depends on the MIB crates as
**path dependencies**, so the two repositories are expected side by side:

```text
~/Development/MIB        the engine
~/Development/MIB-TBA    this game
```

```sh
cd MIB-TBA
cargo test -p mib-tba            # the crate's unit tests
cargo test -p mib-tba --test pack  # pack load / validate / instantiate
```

For a released game, swap the `path` deps in `Cargo.toml` for `git` deps pinned
to an engine revision — that is what "targets MIB vX" means here.

### A consequence of path deps

Building here compiles MIB **from its working tree**, so uncommitted work in a
sibling MIB checkout is picked up, for better and worse. If MIB is mid-change and
will not build, this will not either; pin a `git` dep to insulate the game from
engine churn.

## Running

Not yet wired. MIB currently links a game's code at **compile time**
(`mib-server` is built with the module list baked in), and runtime dynamic game
loading is deferred engine work (MIB `docs/deferred.md` D-37). So running this
game needs one of:

- a small binary in this repository that composes MIB's reusable
  `mib_host::ServerBuilder` with `StdlibModule + TbaModule` — the game ships its
  own server, which matches "the operator chooses the game"; or
- an engine-side mechanism for selecting a game without a rebuild.

That choice is open; see `docs/implplan.md`.

## Status

- **Phase 9.1 — pack skeleton and `tba.*` data models.** Done: `TbaModule`, the
  four `tba.*` sections, the `TbaContent` resource, and a three-room scaffold
  world.
- Remaining phases — zone reset interpreter, Circle combat, aggro, the converter
  — are planned in [`docs/implplan.md`](docs/implplan.md).

TBA is an **independent game**, not a layer on `classic-rpg`: it depends on
`mib-stdlib` only. Circle round-based combat is not classic-rpg's 3×3 tactical
grid, so the two games coexist rather than stack.
