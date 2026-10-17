# MIB Implementation Plan — Part 3: TBA Pack (`packs/tba/*`)

> Status: **Draft Specification**  
> Companion documents:  
> - [README.md](file:///home/qworg/Development/MIB/docs/tba-impl/README.md) — Master Architecture & Traceability  
> - [01-engine.md](file:///home/qworg/Development/MIB/docs/tba-impl/01-engine.md) — Engine Prerequisites (`crates/*`)  
> - [02-stdlib.md](file:///home/qworg/Development/MIB/docs/tba-impl/02-stdlib.md) — Standard Library Mechanics (`packs/stdlib/*`)  
> Target Directory: [`packs/tba/`](file:///home/qworg/Development/MIB/packs/tba) (`pack.toml`, `code/`, `content/`)  

---

## 1. Architectural Foundations: Pack & Module Layout

The TBA port lives in its own dedicated pack: `packs/tba/`.
Per the **D12/D13 contract**, the pack contains its own Rust code crate:
```text
packs/tba/
├── pack.toml                  # Pack manifest
├── content/                   # Converted YAML data
│   ├── stdlib.rooms.0.yaml    # Sharded rooms (using TBA-E01)
│   ├── stdlib.rooms.1.yaml
│   ├── stdlib.npcs.yaml       # Base mob prototypes
│   ├── tba.zones.yaml         # Zone reset scripts (TBA-E07)
│   ├── tba.npc_stats.yaml     # Diku mob stats & THAC0 (TBA-E27)
│   ├── tba.items.yaml         # 24 item types & 15 wear slots (TBA-E28)
│   ├── tba.classes.yaml       # 4 Diku classes (TBA-E18/E19)
│   ├── tba.triggers.<zone>.yaml # Converted DG scripts (one shard per zone)
│   └── tba.messages.yaml      # Combat messages (TBA-E30)
└── code/                      # The `mib-tba` crate
    ├── Cargo.toml
    └── src/
        ├── lib.rs             # TbaModule registration & plugin assembly
        ├── combat/            # Native Circle round-based combat & aggro
        ├── zones/             # Stateful zone reset interpreter
        ├── items/             # 15-slot wear positions & affect calculations
        ├── triggers/          # `tba.triggers` schema and the quests bridge
        ├── dg/                # DG runtime: dg-vm host, intake, script state
        └── content/           # `tba.*` serde section schemas
```

### 1.1 Pack Manifest (`packs/tba/pack.toml`)
```toml
id = "tba"
version = "1"
title = "The Builder Academy (CircleMUD / tbaMUD)"
game = "tba"
depends_on = ["stdlib"]

[code]
module = "tba"
```
*(Notice: NO dependency on `classic-rpg`).*

### 1.2 Module Implementation (`packs/tba/code/src/lib.rs`)
```rust
use mib_content::prelude::*;
use mib_core::prelude::*;

pub struct TbaModule;

impl MibModule for TbaModule {
    fn name(&self) -> &'static str {
        "tba"
    }

    fn register_sections(&self, registry: &mut SectionRegistry) {
        registry.register::<content::ZonesSection>();
        registry.register::<content::TbaNpcStatsSection>();
        registry.register::<content::TbaItemsSection>();
        registry.register::<content::TbaClassesSection>();
    }

    fn build(&self, app: &mut App) {
        app.add_plugins(TbaPlugin);
    }
}

pub struct TbaPlugin;

impl Plugin for TbaPlugin {
    fn build(&self, app: &mut App) {
        // Register persistent components with SaveRegistry
        app.add_systems(Startup, zones::setup_zones);
        app.add_systems(FixedUpdate, (
            zones::zone_reset_system,
            combat::auto_engage_system,
            combat::combat_round_system,
        ).chain());
    }
}
```

---

## 2. Registering with `ServerBuilder`

In [`crates/mib-server/src/main.rs`](file:///home/qworg/Development/MIB/crates/mib-server/src/main.rs):
Add `mib-tba` to workspace dependencies, and link `TbaModule` alongside `StdlibModule`:
```rust
let server = ServerBuilder::new(cfg)
    .with_module(mib_stdlib::prelude::StdlibModule)
    .with_module(mib_tba::prelude::TbaModule)
    .build()?;
```

---

## 3. TBA-E07 — Zone Reset Engine

### 3.1 Problem Statement & Background
tbaMUD contains 189 zones governed by 18,514 reset commands. A zone reset is **not** a declarative spawn table: it is an imperative program where equipment, container items, and door locks depend on whether the preceding mob spawn succeeded.

### 3.2 Data Model (`tba.zones.yaml`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneDef {
    pub id: u32,                  // e.g. 30 (Midgaard)
    pub name: String,
    pub bot: u32,                 // Bottom room vnum (3000)
    pub top: u32,                 // Top room vnum (3099)
    pub lifespan_mins: u32,       // Reset timer (default 15 mins)
    pub reset_mode: ResetMode,
    pub commands: Vec<ZoneCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetMode {
    Never,
    WhenDesert, // Reset only if no players are inside the zone bounds
    Always,     // Reset even if players are present
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ZoneCommand {
    /// M: Place mob in room (max in world)
    PlaceMob { mob: NpcId, max_in_world: u32, room: RoomId },
    /// O: Drop item in room (max in world)
    DropItem { item: ItemId, max_in_world: u32, room: RoomId },
    /// G: Give item to last spawned mob
    GiveItem { item: ItemId, max_in_world: u32 },
    /// E: Equip item on last spawned mob in wear slot
    EquipItem { item: ItemId, max_in_world: u32, slot: WearSlot },
    /// P: Put item into container item
    PutItem { item: ItemId, max_in_world: u32, container: ItemId },
    /// D: Set door state on room exit
    DoorState { room: RoomId, exit: ExitKey, state: DoorState },
    /// R: Remove item from room
    RemoveItem { room: RoomId, item: ItemId },
}
```

### 3.3 Execution Runtime
A resource `ZoneTracker` tracks the age of each zone:
```rust
#[derive(Debug, Resource)]
pub struct ZoneTracker {
    pub zones: Vec<ZoneState>,
}

pub struct ZoneState {
    pub def: ZoneDef,
    pub age_mins: f32,
}
```

#### The Reset Interpreter Loop
Runs on `FixedUpdate`:
1. Increment `age_mins` by fixed tick elapsed time.
2. If `age_mins >= lifespan_mins`:
   - If `reset_mode == WhenDesert`: Check `RoomOccupants` for all rooms between `bot` and `top`. If any player is present, defer reset.
   - If ready to reset, execute commands sequentially:
     - Track `last_mob_entity: Option<Entity>`.
     - `PlaceMob`: Check total living instances of `mob` in world. If $< max$, spawn mob via stdlib's `spawn_npc`, set `last_mob_entity = Some(entity)`. Otherwise set `None`.
     - `GiveItem`: Only runs if `last_mob_entity.is_some()`. Spawns item, attaches `CarriedBy(last_mob)`.
     - `EquipItem`: Only runs if `last_mob_entity.is_some()`. Spawns item, equips into mob's `TbaEquipment` slot.
     - `DoorState`: Sets door open/closed/locked on room's `DoorState`.
   - Reset `age_mins = 0.0`.

---

## 4. TBA-E05/E31 — Diku Combat System & Auto-Engage

### 4.1 Combat Model Architecture
Do **not** use `classic-rpg`'s 3×3 grid. Implement classic CircleMUD round-based combat:

```rust
#[derive(Debug, Component)]
pub struct TbaCombatant {
    pub hp: i32,
    pub max_hp: i32,
    pub mana: i32,
    pub max_mana: i32,
    pub move_points: i32,
    pub max_move_points: i32,
    pub hitroll: i32,
    pub damroll: i32,
    pub armor_class: i32, // 100 down to -100 (in tenths: 10 AC = 100)
    pub thaco: i32,       // Base THAC0 from level/class
    pub position: Position,
    pub target: Option<Entity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Position {
    Dead,
    MortallyWounded,
    Incapacitated,
    Stunned,
    Sleeping,
    Resting,
    Sitting,
    Fighting,
    Standing,
}
```

### 4.2 Auto-Engage Aggro System (TBA-E05)
Using stdlib's `RoomOccupants` spatial index:
```rust
pub fn auto_engage_system(
    room_occupants: Res<RoomOccupants>,
    players_query: Query<(Entity, &Location), With<Player>>,
    mut mobs_query: Query<(Entity, &mut TbaCombatant, &TbaNpcStats)>,
    mut commands: Commands,
) {
    // Only query rooms containing active players:
    for (player_entity, player_loc) in &players_query {
        let occupants = room_occupants.occupants(player_loc.0);
        for &occupant in occupants {
            if let Ok((mob_entity, mut combatant, stats)) = mobs_query.get_mut(occupant) {
                if combatant.target.is_some() || combatant.position != Position::Standing {
                    continue;
                }
                // Check aggro rules:
                if stats.aggressive 
                    || (stats.aggr_evil && is_evil(player_entity))
                    || (stats.aggr_good && is_good(player_entity))
                    || stats.memory.contains(&player_entity)
                {
                    initiate_combat(&mut commands, mob_entity, player_entity);
                    break;
                }
            }
        }
    }
}
```

### 4.3 Combat Round Resolution
Round timer executes every 3 seconds:
1. Hit check formula:
   $$\text{roll} = d20 + \text{hitroll}$$
   $$\text{needed} = \text{THAC0} - (\text{target AC} / 10)$$
   $$\text{hit} = \text{roll} \ge \text{needed}$$
2. Damage formula:
   $$\text{damage} = \text{roll\_weapon\_dice}() + \text{damroll}$$
3. Apply damage to target HP.
4. If target HP $\le 0$, update position to `Dead`, emit `PlayerDefeated` (for players) or reward XP/gold and despawn (for mobs).

---

## 5. TBA-E08 — Diku NPC Behavior & Memory

Extend `TbaNpcStats` with Diku AI flags:
- `SENTINEL`: Mob does not pursue players into another room or wander.
- `STAY_ZONE`: Mob cannot be leashed outside its zone `[bot, top]`.
- `MEMORY`: Mob records `AttackerMemory(HashSet<Entity>)`. If a player who previously attacked the mob enters the room, the mob re-engages immediately.
- `HELPER`: If any friendly mob in the same room enters combat, all mobs with `HELPER` join the fight on the same side.

---

## 6. TBA-E09 — Death Penalties & Corpse Handling

When player HP $\le 0$ during combat:
1. Calculate XP penalty (Circle standard: loss of $25\%$ of current level's XP).
2. Emit stdlib's `PlayerDefeated(player_entity)` to trigger corpse creation and temple respawn.
3. Drop carried gold on the corpse.

---

## 7. TBA-E28 — Extended Object & Equipment Model

### 7.1 Wear Positions (15 Slots)
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WearSlot {
    Light,
    FingerR,
    FingerL,
    Neck1,
    Neck2,
    Body,
    Head,
    Legs,
    Feet,
    Hands,
    Arms,
    Shield,
    About,
    Waist,
    WristR,
    WristL,
    Wield,
    Hold,
}
```

### 7.2 Equipment Component
```rust
#[derive(Debug, Component, Default, Serialize, Deserialize)]
pub struct TbaEquipment {
    pub slots: HashMap<WearSlot, Entity>,
}
```
**Persistence**: Register `TbaEquipment` with [`SaveRegistry`](file:///home/qworg/Development/MIB/packs/stdlib/code/src/persist.rs#L123) under `DocKind::Player` so worn items survive server restart.

### 7.3 Affect Blocks
Items apply `Affect` bonuses when worn:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffectDef {
    pub location: AffectLocation,
    pub modifier: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffectLocation {
    Str, Dex, Int, Wis, Con, Cha,
    Hitroll, Damroll, ArmorClass,
    MaxHp, MaxMana, MaxMove,
    SavePara, SaveRod, SavePetri, SaveBreath, SaveSpell,
}
```
When an item is equipped via `wear <item>` or zone reset `E`, recompute derived stats on `TbaCombatant`.

---

## 8. TBA-E18 — Skills, Spells, and Classes (D-04)

### 8.1 Four Character Classes
Define in `tba.classes.yaml`:
- **Warrior**: High HP ($1d10$), high THAC0 progression, heavy armor.
- **Thief**: Backstab, sneak, hide, pick locks.
- **Cleric**: Healing spells, blessings, medium armor.
- **Magic-User**: Attack spells (Magic Missile, Fireball), low HP, no armor.

### 8.2 Spells & Spellcasting Command
Register player command: `cast '<spell name>' [target]`.
1. Check mana cost against `TbaCombatant.mana`.
2. Check position: must be standing or fighting.
3. Check target: validate target in room.
4. Execute spell effect (healing, damage, buff) and deduct mana.

### 8.3 Skills & Class Abilities (issue #25)
Each class's `granted_skills` also names the Circle **skills** it learns, and
`tba.classes.skill_levels` carries `init_spell_levels()`'s
(`tbamud/src/class.c:1652-1664`) minimum level per class:

- Warrior: `kick` (1), `rescue` (3), `bash` (12).
- Thief: `sneak` (1), `pick_lock` (2), `backstab` (3), `hide` (5).

`mib_tba::skills` serves the commands `backstab`, `kick`, `bash`, `rescue`,
`assist [ally]`, `consider`, `sneak`, `hide` and `pick_lock <direction>`, each
parsed from `PlayerInput::RawLine` and gated on the class skill and its minimum
level. `assist` and `consider` are universal (`do_assist`/`do_consider` have no
class gate). `sneak`/`hide` set the runtime-only `TbaSneaking`/`TbaHidden`
components, which `mib_tba::stealth` syncs into the engine `Concealment`
carrier; the observer-aware projection (`look`/`Scene`/GMCP) and the pack's own
target resolution then filter through the engine `Concealment`/`Perception`
seam (a `detect invisibility`/`sense life` cast grants the matching engine
`Perception`). `pick_lock` reads Circle's
`do_gen_door`/`pick` (`tbamud/src/act.movement.c:589-608`), requires a pickable,
locked door, and writes the room's runtime `RoomState` exactly like the zone
engine's `D` reset. The bare stdlib `pick` command (`handle_pick`) is
router-owned and cannot be overridden. See `docs/runbook.md` §6/§8 for the
system wiring and the remaining limits (there is no practice system).

---

## 9. TBA-E17 — Shops & Economy (D-02)

Define `tba.shops.yaml`:
- Shopkeeper NPC ID.
- Selling multiplier (e.g. $1.20$), buying multiplier (e.g. $0.80$).
- Accepted item types bitmask (e.g. weapons and armor only).
- Commands: `list`, `buy <item>`, `sell <item>`, `value <item>`.

---

## 10. TBA-E20 — Trigger VM (DG Scripts) (D-T2)

The DG runtime is built. `tools/tbaport` emits all **1,461** trigger records into
`tba.triggers.<zone>.yaml` (one shard per zone), and `code/src/dg/` hosts the
engine-agnostic `crates/dg-vm` interpreter on the ECS world.

### 10.1 Content schema

`tba.triggers` (`code/src/triggers/content.rs`) has two parts:

- `triggers` — `TriggerDef`, keyed `tba_trig_<vnum>`, with `vnum`, `name`,
  `attach_type` (`mob`/`obj`/`wld`), `flags` (the record's raw type token),
  `types` (expanded lowercase names such as `speech`/`greet-all`), `narg`,
  `arglist` and `script`. The legacy single `attach` field is gone.
- `attachments` — each owner (`npc:<id>`, `item:<id>`, `room:<id>`) mapped to
  its ordered trigger list in `proto_script` order: the record's `T` lines,
  then any zone-reset `T` commands folded in. A trigger listed twice attaches
  twice, as in the C.

Zone-reset `V` commands have no schema home and are reported as drops.

### 10.2 Runtime

`crates/dg-vm` is a line-faithful port of tbaMUD's `dg_*.c` — triggers
(`dg_triggers.c`), the driver and expression evaluator, the `%…%` substitution
tables, the `m*`/`o*`/`w*` script commands and `dg_cast`. It is engine-agnostic:
it reaches the game only through the `DgHost` traits, which `code/src/dg/host`
implements over the ECS. `code/src/dg/` adds the game side: entity ids and list
order, per-entity `DgScript`/`DgProtos` state, the pulse clock and `wait` queue,
text delivery and the persisted `TbaTriggerVars`.

Every event family has intake (`code/src/dg/intake/`): commands and speech;
movement (`leave`, `door`, `entry`, `enter`, `greet`, `memory`, `login`); items
(`get`, `drop`, `give`, `receive`, `bribe`, `wear`, `remove`, `consume`, `cast`);
combat (`fight`, `hitprcnt`, `damage`, `death`); the zone reset and item timers;
and the pulse heartbeat (`random`, `timer`, `time`). All trigger activity funnels
through `dg::with_host`, which attaches prototype triggers in attachment order
and fires `load`.

### 10.3 Coverage

The converter analyses every emitted script with `dg_vm::analyze` and writes the
result into the drop report's `trigger_coverage`: lines by class, control and
statement histograms, `wait` forms, script commands, structural errors and the
mob-command words. The supported mob words come from the game's `cmd_info[]`
table (`mib_tba::dg::mobcmd::supported_mob_words`, 42 supported / 94 unsupported
in the tbaMUD corpus). `tbaport triage` still writes the standalone
classification `reports/trigger-triage.json`.
