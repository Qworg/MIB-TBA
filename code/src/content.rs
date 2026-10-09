//! The `tba.*` pack sections — CircleMUD / tbaMUD domain data.
//!
//! Four sections, all owned by `mib-tba` (the `[code] module = "tba"` half of
//! `packs/tba/`):
//!
//! - [`ZonesSection`] (`tba.zones`) — the stateful zone-reset programs (TBA-E07)
//! - [`TbaNpcStatsSection`] (`tba.npc_stats`) — Diku mob stats and AI flags (TBA-E27)
//! - [`TbaItemsSection`] (`tba.items`) — 24 item types, 18 wear positions,
//!   affect blocks (TBA-E28)
//! - [`TbaClassesSection`] (`tba.classes`) — the four Diku classes (TBA-E18/E19)
//!
//! Everything here is genre data layered on top of `stdlib.*`: rooms, NPC
//! prototypes and item prototypes come from `stdlib.rooms` / `stdlib.npcs` /
//! `stdlib.items`, and the `tba.*` sections key into them by id. Validation
//! therefore resolves those base ids through [`ValidationCtx::section`], which
//! falls back across the loaded pack set.

use std::collections::{BTreeMap, BTreeSet};

use mib_content::prelude::{PackSection, SectionRegistry, ValidationCtx};
use mib_core::prelude::Dice;
use mib_protocol::command::ExitKey;
use mib_stdlib::prelude::{ItemId, ItemsSection, NpcId, NpcsSection, RoomId, RoomsSection};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// Shared domain types
// ---------------------------------------------------------------------------

/// A Diku attribute. Used by class definitions and by affect blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attribute {
    Str,
    Dex,
    Int,
    Wis,
    Con,
    Cha,
}

/// The 18 CircleMUD wear positions.
///
/// The prose in `docs/tba-impl/03-tba-pack.md` §7.1 calls these "15 slots" but
/// then lists 18; the list is authoritative and matches `ITEM_WEAR_*` in the
/// source `structs.h`. `Take` is deliberately absent — takeability is a
/// stdlib-level property, not a wear position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

/// The `APPLY_*` locations an item affect can modify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffectLocation {
    Str,
    Dex,
    Int,
    Wis,
    Con,
    Cha,
    Hitroll,
    Damroll,
    ArmorClass,
    MaxHp,
    MaxMana,
    MaxMove,
    SavePara,
    SaveRod,
    SavePetri,
    SaveBreath,
    SaveSpell,
}

/// A single `A`-block affect: `apply <location> by <modifier>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AffectDef {
    pub location: AffectLocation,
    pub modifier: i32,
}

/// A Diku position, ordered from worst to best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    Dead,
    MortallyWounded,
    Incapacitated,
    Stunned,
    Sleeping,
    Resting,
    Sitting,
    Fighting,
    #[default]
    Standing,
}

/// The mob AI flags the source world actually sets (TBA-E08).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NpcBehaviour {
    /// Attacks any visible player.
    Aggressive,
    /// Attacks players of evil alignment.
    AggrEvil,
    /// Attacks players of good alignment.
    AggrGood,
    /// Attacks players of neutral alignment.
    AggrNeutral,
    /// Never pursues out of the room it was loaded in.
    Sentinel,
    /// Never leaves its zone's `[bot, top]` bounds.
    StayZone,
    /// Remembers who attacked it and re-engages on sight.
    Memory,
    /// Joins a fight when a friendly mob in the room is fighting.
    Helper,
    /// Picks up valuable objects from the floor.
    Scavenger,
    /// Flees below 20% hit points.
    Wimpy,
    /// Cannot be backstabbed.
    Aware,
}

// ---------------------------------------------------------------------------
// tba.zones — the stateful zone reset program (TBA-E07)
// ---------------------------------------------------------------------------

/// What a zone does when its lifespan elapses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetMode {
    /// Never resets.
    Never,
    /// Resets only when no player is inside `[bot, top]`.
    WhenDesert,
    /// Resets on the timer regardless of occupancy.
    #[default]
    Always,
}

/// The door state a `D` reset command sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoorReset {
    Open,
    Closed,
    Locked,
}

/// One reset command. These are a program, not a table: `GiveItem` /
/// `EquipItem` / `PutItem` act on the mob placed by the preceding `PlaceMob`,
/// and are skipped when that placement did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ZoneCommand {
    /// `M` — place a mob in a room, up to `max_in_world` copies world-wide.
    PlaceMob {
        mob: NpcId,
        room: RoomId,
        #[serde(default)]
        max_in_world: u32,
    },
    /// `O` — drop an item in a room.
    DropItem {
        item: ItemId,
        room: RoomId,
        #[serde(default)]
        max_in_world: u32,
    },
    /// `G` — give an item to the last placed mob.
    GiveItem {
        item: ItemId,
        #[serde(default)]
        max_in_world: u32,
    },
    /// `E` — equip the last placed mob from an item.
    EquipItem {
        item: ItemId,
        slot: WearSlot,
        #[serde(default)]
        max_in_world: u32,
    },
    /// `P` — put an item inside a container item.
    PutItem {
        item: ItemId,
        container: ItemId,
        #[serde(default)]
        max_in_world: u32,
    },
    /// `D` — set a door's state on a room exit. `exit` is a direction word
    /// (`north`, `n`, …) validated with [`ExitKey::from_word`].
    DoorState {
        room: RoomId,
        exit: String,
        state: DoorReset,
    },
    /// `R` — remove an item from a room.
    RemoveItem { room: RoomId, item: ItemId },
}

/// One zone: bounds, reset cadence, and its reset program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneDef {
    pub name: String,
    /// Bottom room vnum of the zone range.
    pub bot: u32,
    /// Top room vnum of the zone range.
    pub top: u32,
    #[serde(default = "default_lifespan_mins")]
    pub lifespan_mins: u32,
    #[serde(default)]
    pub reset_mode: ResetMode,
    #[serde(default)]
    pub commands: Vec<ZoneCommand>,
}

fn default_lifespan_mins() -> u32 {
    15
}

/// `tba.zones` — keyed by zone vnum.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZonesFile {
    pub zones: BTreeMap<u32, ZoneDef>,
}

/// `tba.zones`.
pub struct ZonesSection;

impl PackSection for ZonesSection {
    const ID: &'static str = "tba.zones";
    type Defs = ZonesFile;

    fn validate(defs: &Self::Defs, ctx: &mut ValidationCtx<'_>) {
        // Resolve the base id sets up front so the mutable issue accumulator is
        // free while the loops report (the stdlib `RoomsSection` idiom). A
        // missing base section yields an empty set, so every reference is
        // still named rather than silently accepted.
        let npcs = npc_ids(ctx);
        let items = item_ids(ctx);
        let rooms = room_ids(ctx);

        for (vnum, zone) in &defs.zones {
            if zone.top < zone.bot {
                ctx.error_at(
                    format!("zones.{vnum}.top"),
                    format!("zone top {} is below bot {}", zone.top, zone.bot),
                );
            }

            for (index, command) in zone.commands.iter().enumerate() {
                let field = format!("zones.{vnum}.commands[{index}]");
                match command {
                    ZoneCommand::PlaceMob { mob, room, .. } => {
                        require_npc(ctx, &format!("{field}.mob"), mob, &npcs);
                        require_room(ctx, &format!("{field}.room"), room, &rooms);
                    }
                    ZoneCommand::DropItem { item, room, .. } => {
                        require_item(ctx, &format!("{field}.item"), item, &items);
                        require_room(ctx, &format!("{field}.room"), room, &rooms);
                    }
                    ZoneCommand::GiveItem { item, .. } => {
                        require_item(ctx, &format!("{field}.item"), item, &items);
                    }
                    ZoneCommand::EquipItem { item, .. } => {
                        require_item(ctx, &format!("{field}.item"), item, &items);
                    }
                    ZoneCommand::PutItem {
                        item, container, ..
                    } => {
                        require_item(ctx, &format!("{field}.item"), item, &items);
                        require_item(ctx, &format!("{field}.container"), container, &items);
                    }
                    ZoneCommand::DoorState { room, exit, .. } => {
                        require_room(ctx, &format!("{field}.room"), room, &rooms);
                        if ExitKey::from_word(exit).is_none() {
                            ctx.error_at(
                                format!("{field}.exit"),
                                format!("unknown exit direction `{exit}`"),
                            );
                        }
                    }
                    ZoneCommand::RemoveItem { room, item } => {
                        require_room(ctx, &format!("{field}.room"), room, &rooms);
                        require_item(ctx, &format!("{field}.item"), item, &items);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// tba.npc_stats — Diku mob stats + AI flags (TBA-E27 / E08)
// ---------------------------------------------------------------------------

/// Diku mob statistics. Everything the source `.mob` record carries that
/// `stdlib.npcs` does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaNpcStats {
    /// Hit points as a dice expression (`3d8+12`), the source's native format.
    pub hp: Dice,
    pub level: u32,
    #[serde(default)]
    pub hitroll: i32,
    #[serde(default)]
    pub damroll: i32,
    /// Circle armour class, 100 (worst) down to -100 (best).
    #[serde(default)]
    pub armor_class: i32,
    #[serde(default)]
    pub thaco: i32,
    #[serde(default)]
    pub alignment: i32,
    #[serde(default)]
    pub gold: u32,
    #[serde(default)]
    pub experience: u32,
    #[serde(default)]
    pub default_position: Position,
    #[serde(default)]
    pub behaviour: Vec<NpcBehaviour>,
}

/// `tba.npc_stats` — keyed by the `stdlib.npcs` id it describes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaNpcStatsFile {
    pub npc_stats: BTreeMap<NpcId, TbaNpcStats>,
}

/// `tba.npc_stats`.
pub struct TbaNpcStatsSection;

impl PackSection for TbaNpcStatsSection {
    const ID: &'static str = "tba.npc_stats";
    type Defs = TbaNpcStatsFile;

    fn validate(defs: &Self::Defs, ctx: &mut ValidationCtx<'_>) {
        let npcs = npc_ids(ctx);
        for npc_id in defs.npc_stats.keys() {
            if !npcs.contains(npc_id) {
                ctx.error_at(
                    format!("npc_stats.{npc_id}"),
                    format!("unknown npc `{npc_id}` (not defined in stdlib.npcs)"),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// tba.items — 24 types, 18 wear positions, affects (TBA-E28)
// ---------------------------------------------------------------------------

/// The 24 CircleMUD item types (`ITEM_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TbaItemKind {
    Undefined,
    Light,
    Scroll,
    Wand,
    Staff,
    Weapon,
    Furniture,
    Free,
    Treasure,
    Armor,
    Potion,
    Worn,
    Other,
    Trash,
    Free2,
    Container,
    Note,
    DrinkContainer,
    Key,
    Food,
    Money,
    Pen,
    Boat,
    Fountain,
}

/// Diku item data layered on top of `stdlib.items`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaItemDef {
    pub kind: TbaItemKind,
    /// Wear positions this item occupies. Empty means not wearable.
    #[serde(default)]
    pub slots: Vec<WearSlot>,
    /// The four type-dependent value fields (`value[0..3]`).
    #[serde(default)]
    pub values: [i32; 4],
    #[serde(default)]
    pub affects: Vec<AffectDef>,
    #[serde(default)]
    pub weight: u32,
    #[serde(default)]
    pub cost: u32,
    #[serde(default)]
    pub rent: u32,
    #[serde(default)]
    pub min_level: u32,
    #[serde(default)]
    pub timer: Option<u32>,
}

/// `tba.items` — keyed by the `stdlib.items` id it describes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaItemsFile {
    pub items: BTreeMap<ItemId, TbaItemDef>,
}

/// `tba.items`.
pub struct TbaItemsSection;

impl PackSection for TbaItemsSection {
    const ID: &'static str = "tba.items";
    type Defs = TbaItemsFile;

    fn validate(defs: &Self::Defs, ctx: &mut ValidationCtx<'_>) {
        let items = item_ids(ctx);
        for item_id in defs.items.keys() {
            if !items.contains(item_id) {
                ctx.error_at(
                    format!("items.{item_id}"),
                    format!("unknown item `{item_id}` (not defined in stdlib.items)"),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// tba.classes — the four Diku classes (TBA-E18 / E19)
// ---------------------------------------------------------------------------

/// One character class. `granted_skills` names spells/skills, which live in the
/// deferred magic system (D-04); they are carried as data now so the class
/// tables survive conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaClassDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Hit points gained per level, e.g. `1d10` for a warrior.
    pub hp_dice: Dice,
    /// Prime attribute; `None` for a class with no attribute requirement.
    #[serde(default)]
    pub prime_attribute: Option<Attribute>,
    #[serde(default)]
    pub starting_items: Vec<ItemId>,
    #[serde(default)]
    pub granted_skills: Vec<SmolStr>,
}

/// `tba.classes` — keyed by the class id (e.g. `warrior`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaClassesFile {
    pub classes: BTreeMap<SmolStr, TbaClassDef>,
}

/// `tba.classes`.
pub struct TbaClassesSection;

impl PackSection for TbaClassesSection {
    const ID: &'static str = "tba.classes";
    type Defs = TbaClassesFile;

    fn validate(defs: &Self::Defs, ctx: &mut ValidationCtx<'_>) {
        let items = item_ids(ctx);
        for (class_id, class) in &defs.classes {
            if class.name.trim().is_empty() {
                ctx.error_at(
                    format!("classes.{class_id}.name"),
                    "class name must not be empty",
                );
            }
            for (index, item) in class.starting_items.iter().enumerate() {
                if !items.contains(item) {
                    ctx.error_at(
                        format!("classes.{class_id}.starting_items[{index}]"),
                        format!("unknown item `{item}` (not defined in stdlib.items)"),
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register every `tba.*` section. Called from `TbaModule::register_sections`.
pub fn register(registry: &mut SectionRegistry) {
    registry.register::<ZonesSection>();
    registry.register::<TbaNpcStatsSection>();
    registry.register::<TbaItemsSection>();
    registry.register::<TbaClassesSection>();
}

// ---------------------------------------------------------------------------
// Validation helpers + empty fallbacks
// ---------------------------------------------------------------------------

/// Owned id sets for the base sections this pack layers on. Empty when the
/// base section is absent, so a `tba.*` reference is still reported as
/// unknown rather than silently accepted.
fn npc_ids(ctx: &ValidationCtx<'_>) -> BTreeSet<NpcId> {
    ctx.section::<NpcsSection>()
        .map(|npcs| npcs.npcs.keys().cloned().collect())
        .unwrap_or_default()
}

fn item_ids(ctx: &ValidationCtx<'_>) -> BTreeSet<ItemId> {
    ctx.section::<ItemsSection>()
        .map(|items| items.items.keys().cloned().collect())
        .unwrap_or_default()
}

fn room_ids(ctx: &ValidationCtx<'_>) -> BTreeSet<RoomId> {
    ctx.section::<RoomsSection>()
        .map(|rooms| rooms.rooms.keys().cloned().collect())
        .unwrap_or_default()
}

fn require_npc(ctx: &mut ValidationCtx<'_>, field: &str, id: &NpcId, npcs: &BTreeSet<NpcId>) {
    if !npcs.contains(id) {
        ctx.error_at(field.to_owned(), format!("unknown npc `{id}`"));
    }
}

fn require_item(ctx: &mut ValidationCtx<'_>, field: &str, id: &ItemId, items: &BTreeSet<ItemId>) {
    if !items.contains(id) {
        ctx.error_at(field.to_owned(), format!("unknown item `{id}`"));
    }
}

fn require_room(ctx: &mut ValidationCtx<'_>, field: &str, id: &RoomId, rooms: &BTreeSet<RoomId>) {
    if !rooms.contains(id) {
        ctx.error_at(field.to_owned(), format!("unknown room `{id}`"));
    }
}
