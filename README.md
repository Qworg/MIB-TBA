# MIB-TBA

The **TBA content pack** for [MIB](https://github.com/Qworg/MIB): the
[Builder Academy](https://github.com/tbamud/tbamud) (CircleMUD / tbaMUD) world
and its Diku/DikuMUD game rules, packaged as `packs/tba`.

**This repository is the pack.** Its root holds `pack.toml`, `content/` and the
`mib-tba` code crate — the same layout MIB expects at `packs/<id>/`:

```text
pack.toml            pack manifest (id "tba", game "tba", depends_on ["stdlib"])
content/             the world and the tba.* sections
  stdlib.areas.yaml  ┐
  stdlib.rooms.yaml  │ world content, carried in the genre-agnostic stdlib sections
  stdlib.npcs.yaml   │
  stdlib.items.yaml  ┘
  tba.zones.yaml     ┐
  tba.npc_stats.yaml │ Diku domain rules owned by mib-tba
  tba.items.yaml     │
  tba.classes.yaml   ┘
code/                the `mib-tba` crate (packs/tba/code half)
docs/                port planning: docs/implplan.md
```

## How MIB consumes it

MIB mounts this repository as a **git submodule at `packs/tba`**:

```sh
cd /path/to/MIB
git submodule add -b <branch> https://github.com/Qworg/MIB-TBA.git packs/tba
```

MIB's workspace glob `members = ["crates/*", "packs/*/code"]` then picks up
`packs/tba/code` with no MIB layout change, and `mib-server` links `TbaModule`.

## Building and testing

The pack compiles **inside MIB**, not here: `code/Cargo.toml` inherits MIB's
workspace (`version.workspace`, `[lints] workspace`), so there is deliberately no
`Cargo.toml` at this repository's root. Building through MIB is what keeps the
pack a single workspace member with one dependency graph.

```sh
cd /path/to/MIB        # with the submodule checked out
cargo test -p mib-tba                            # the crate's unit tests
cargo test -p mib-server --test tba              # pack load / validate / install
cargo run -p mib-server -- validate-packs packs/stdlib packs/tba
```

Because the pack lives behind a submodule, CI and any Docker build that reads
`packs/` must check out submodules (`actions/checkout` with
`submodules: recursive`; `git clone --recurse-submodules`).

## Status

- **Phase 9.1 — pack skeleton and `tba.*` data models.** Done: `TbaModule`, the
  four `tba.*` sections, the `TbaContent` resource, and a three-room scaffold
  world. `validate-packs` is green.
- Remaining phases (zone reset interpreter, Circle combat, aggro, converter) are
  planned in [`docs/implplan.md`](docs/implplan.md).

The Diku game rules are an **independent genre**: this pack depends on
`mib-stdlib`, *not* `mib-classic-rpg`. Circle round-based combat is not
classic-rpg's 3×3 tactical grid, so the two coexist rather than stack.
