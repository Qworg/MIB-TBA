//! The `tba.*` pack sections — CircleMUD / tbaMUD domain data.
//!
//! Seven sections, all owned by `mib-tba` (the `[code] module = "tba"` half of
//! `packs/tba/`):
//!
//! - [`ZonesSection`] (`tba.zones`) — the stateful zone-reset programs (TBA-E07)
//! - [`TbaNpcStatsSection`] (`tba.npc_stats`) — Diku mob stats and AI flags (TBA-E27)
//! - [`TbaItemsSection`] (`tba.items`) — 24 item types, 18 wear positions,
//!   affect blocks (TBA-E28)
//! - [`TbaClassesSection`] (`tba.classes`) — the four Diku classes (TBA-E18/E19)
//! - [`SpellsSection`] (`tba.spells`) — Circle's spell catalog, read by the
//!   `cast` command (TBA-E18)
//! - [`ShopsSection`] (`tba.shops`) — shopkeeper trade terms, read by the
//!   `list`/`buy`/`sell`/`value` commands (TBA-E17)
//! - [`SocialsSection`] (`tba.socials`) — Circle's 491 social emotes, read by
//!   the `social`/`socials` commands (issue #17)
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

use crate::combat::{TbaAttributes, TbaSaves};

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
    /// Cannot be charmed (`MOB_NOCHARM`).
    NoCharm,
    /// Cannot be summoned (`MOB_NOSUMMON`).
    NoSummon,
    /// Cannot be put to sleep (`MOB_NOSLEEP`).
    NoSleep,
    /// Cannot be bashed (`MOB_NOBASH`).
    NoBash,
    /// Cannot be blinded (`MOB_NOBLIND`).
    NoBlind,
    /// Kills a character outright instead of reducing them to 0 HP
    /// (`MOB_NOKILL`).
    NoKill,
}

/// Circle's `GET_SEX`: 0 neutral, 1 male, 2 female (`structs.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TbaSex {
    /// Circle's "it" (`SEX_NEUTRAL`).
    #[default]
    Neutral,
    Male,
    Female,
}

impl TbaSex {
    /// The absent value, so a neutral reference serialises to nothing.
    pub fn is_neutral(&self) -> bool {
        *self == Self::Neutral
    }
}

/// A mobile's hard-coded special procedure.
///
/// `MOB_SPEC` only marks that something *may* be assigned; the actual
/// function pointer lives in the C tables (`spec_assign.c::assign_mobiles`,
/// `castle.c::assign_kings_castle`). The converter transcribes those tables
/// into this enum so a mob's behaviour is data, not a flag guess. Not every
/// `MOB_SPEC`-flagged mob has one, and most assigned mobs do not carry the
/// flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecProc {
    /// `SPECIAL(guild)`: the `practice` command (`spec_procs.c:147`).
    Guild,
    /// `SPECIAL(mayor)`: the Midgaard mayor's scheduled walk (`spec_procs.c:223`).
    Mayor,
    /// `SPECIAL(postmaster)`: send/check/receive mail (`mail.c:390`).
    Postmaster,
    /// `SPECIAL(receptionist)`: rent storage (`objsave.c:1197`).
    Receptionist,
    /// `SPECIAL(cryogenicist)`: free cryo storage (`objsave.c:1202`).
    Cryogenicist,
    /// `SPECIAL(CastleGuard)`: banzaii the staff's attackers (`castle.c:648`).
    CastleGuard,
    /// `SPECIAL(king_welmar)`: the King's schedule, spells and banzaii
    /// (`castle.c:366`).
    KingWelmar,
    /// `SPECIAL(peter)`: the captain inspects the guard (`castle.c:669`).
    Peter,
    /// `SPECIAL(training_master)`: sparring and the warrior practice gate
    /// (`castle.c:483`).
    TrainingMaster,
    /// `SPECIAL(James)`: the butler picks up trash and complains (`castle.c:611`).
    James,
    /// `SPECIAL(cleaning)`: the cleaning woman picks up trash (`castle.c:642`).
    Cleaning,
    /// `SPECIAL(tim)`: the King's twin (`castle.c:573`).
    Tim,
    /// `SPECIAL(tom)`: the King's twin (`castle.c:568`).
    Tom,
    /// `SPECIAL(DicknDavid)`: the treasury guards (`castle.c:657`).
    DicknDavid,
    /// `SPECIAL(jerry)`: the castle gambler (`castle.c:733`).
    Jerry,
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
pub enum ZoneCommandKind {
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

/// A reset command plus Circle's per-command `if_flag`.
///
/// `if_flag` mirrors `ZCMD.if_flag` in `reset_zone` (`db.c`): a command whose
/// flag is set runs only when the command before it succeeded (`last_cmd` in
/// C). A plain command (`if_flag: false`) always runs. The flag is folded into
/// a wrapper rather than repeated on every variant so a reset program stays a
/// single homogeneous `Vec`.
///
/// The wrapper is `#[serde(flatten)]`, so the emitted YAML keeps the
/// `type: <variant>` shape and gains an `if_flag` key alongside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneCommand {
    /// Circle's `if_flag`: run only when the previous command succeeded.
    #[serde(default)]
    pub if_flag: bool,
    /// The command body.
    #[serde(flatten)]
    pub command: ZoneCommandKind,
}

impl ZoneCommand {
    /// A command that always runs (`if_flag: false`).
    pub fn always(command: ZoneCommandKind) -> Self {
        Self {
            if_flag: false,
            command,
        }
    }
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
    /// The source `ZONE_*` flags. The engine acts on none of them; they are
    /// preserved so a future system (or a builder's audit) can read them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<ZoneFlag>,
    #[serde(default)]
    pub commands: Vec<ZoneCommand>,
}

fn default_lifespan_mins() -> u32 {
    15
}

/// Circle's `ZONE_*` flags (`structs.h`), in bit order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneFlag {
    Closed,
    NoImmort,
    Quest,
    Grid,
    NoBuild,
    NoAstral,
    Worldmap,
}

/// Circle's `SHOP_*` feature bits (`SHOP_NUMITEMS` .. `SHOP_NOCOMPLAIN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShopFlag {
    NumItems,
    Fix,
    Cash,
    Broke,
    Notell,
    NoTradeWith,
    NoComplain,
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
                match &command.command {
                    ZoneCommandKind::PlaceMob { mob, room, .. } => {
                        require_npc(ctx, &format!("{field}.mob"), mob, &npcs);
                        require_room(ctx, &format!("{field}.room"), room, &rooms);
                    }
                    ZoneCommandKind::DropItem { item, room, .. } => {
                        require_item(ctx, &format!("{field}.item"), item, &items);
                        require_room(ctx, &format!("{field}.room"), room, &rooms);
                    }
                    ZoneCommandKind::GiveItem { item, .. } => {
                        require_item(ctx, &format!("{field}.item"), item, &items);
                    }
                    ZoneCommandKind::EquipItem { item, .. } => {
                        require_item(ctx, &format!("{field}.item"), item, &items);
                    }
                    ZoneCommandKind::PutItem {
                        item, container, ..
                    } => {
                        require_item(ctx, &format!("{field}.item"), item, &items);
                        require_item(ctx, &format!("{field}.container"), container, &items);
                    }
                    ZoneCommandKind::DoorState { room, exit, .. } => {
                        require_room(ctx, &format!("{field}.room"), room, &rooms);
                        if ExitKey::from_word(exit).is_none() {
                            ctx.error_at(
                                format!("{field}.exit"),
                                format!("unknown exit direction `{exit}`"),
                            );
                        }
                    }
                    ZoneCommandKind::RemoveItem { room, item } => {
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
    /// The source `.mob` damage dice (`#d#+#`). Circle NPCs deal this instead
    /// of an unarmed roll; [`npc_combatant_from_stats`] carries it into the
    /// runtime [`TbaAttackDice`](crate::combat::TbaAttackDice). `None` falls
    /// back to unarmed damage.
    #[serde(default)]
    pub damage: Option<Dice>,
    /// Enhanced-mob e-spec prime attributes (`Str`/`Int`/…,
    /// `interpret_espec`, `tbamud/src/db.c:1718`).
    ///
    /// The simple-mob loader sets every attribute to **11** before reading the
    /// e-spec section (`db.c:1682-1687`), so an absent block means
    /// [`TbaAttributes::mob_default`], not the player-side baseline of 13.
    #[serde(
        default = "TbaAttributes::mob_default",
        skip_serializing_if = "TbaAttributes::is_mob_default"
    )]
    pub attributes: TbaAttributes,
    /// Enhanced-mob saving-throw modifiers (`SavingPara`/`SavingRod`/…,
    /// `interpret_espec`).
    ///
    /// These are `apply_saving_throw[]` modifiers layered on the warrior base
    /// table by [`mag_savingthrow`](crate::combat::mag_savingthrow), the same
    /// way a player's [`TbaSaves`](crate::combat::TbaSaves) are.
    #[serde(default, skip_serializing_if = "TbaSaves::is_default")]
    pub saves: TbaSaves,
    /// The hard-coded C procedure assigned to this mob, if any.
    ///
    /// This is transcribed from `spec_assign.c`/`castle.c`, not derived from
    /// `MOB_SPEC`; see [`SpecProc`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_proc: Option<SpecProc>,
    /// The source `GET_SEX`.
    #[serde(default, skip_serializing_if = "TbaSex::is_neutral")]
    pub sex: TbaSex,
    /// The source `.mob` keyword list (`ch->player.name`), which is what a
    /// player types to address the mobile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Circle's `AFF_FLAGS` bits on the mobile. Circle uses one `AFF_*`
    /// vocabulary for characters and objects, so this shares
    /// [`ItemAffectFlag`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aff_flags: Vec<ItemAffectFlag>,
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
///
/// `Ord` is derived so the type can key the shop buy-word map deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

/// Object extra flags (`ITEM_*` in `structs.h:412`).
///
/// These are the source's `GET_OBJ_EXTRA` bits. They are carried as data even
/// where the engine does not yet act on them, so a later drop/sell/decay pass
/// can honour them without a re-conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemExtraFlag {
    Glow,
    Hum,
    NoRent,
    NoDonate,
    NoInvis,
    Invisible,
    Magic,
    NoDrop,
    Bless,
    AntiGood,
    AntiEvil,
    AntiNeutral,
    AntiMagicUser,
    AntiCleric,
    AntiThief,
    AntiWarrior,
    NoSell,
    Quest,
}

/// Object affect flags (`AFF_*` in `structs.h:272`), the source's
/// `GET_OBJ_AFFECT` bits. The loader's affect flags shift every bit up by one
/// (`AFF_DONTUSE == 0`); the three placeholder bits (`DONTUSE`, `UNUSED`,
/// `FREE`) are not modelled and are reported instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemAffectFlag {
    Blind,
    Invisible,
    DetectAlign,
    DetectInvis,
    DetectMagic,
    SenseLife,
    Waterwalk,
    Sanctuary,
    Curse,
    Infravision,
    Poison,
    ProtectEvil,
    ProtectGood,
    Sleep,
    NoTrack,
    Flying,
    Scuba,
    Sneak,
    Hide,
    Charm,
}

/// Diku item data layered on top of `stdlib.items`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaItemDef {
    /// The source `.obj` keyword list (the first string of the record).
    /// `stdlib.items` carries only a display `name`; the keywords are what a
    /// player may type to target the item, so they are preserved here.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// The source `GET_OBJ_EXTRA` bits (`ITEM_*`).
    #[serde(default)]
    pub extra_flags: Vec<ItemExtraFlag>,
    /// The source `GET_OBJ_AFFECT` bits (`AFF_*`).
    #[serde(default)]
    pub affect_flags: Vec<ItemAffectFlag>,
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
    /// Circle's `action_description` (what `look <item>` shows for a readable
    /// or writable object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_description: Option<String>,
    /// The source `E` extra descriptions (`ex_description`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_descriptions: Vec<TbaExtraDesc>,
}

/// One `E` extra description on an item or room prototype.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaExtraDesc {
    /// The keyword list a player types to reach this description.
    pub keywords: Vec<String>,
    pub description: String,
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

/// One character class. `granted_skills` names spells/skills; a class may cast
/// the spells it names (`crate::spells`) and use the class skills it names
/// (`crate::skills`, issue #25).
///
/// The progression tables are transcribed verbatim from CircleMUD/tbaMUD's
/// `src/class.c` — [`thaco`](https://github.com/tbamud/tbamud) for
/// `thaco_table` and `level_exp` for `xp_table`. Both are indexed by level and
/// keep the source's level-0 sentinel at index 0 (`100` THAC0 and `0` XP
/// respectively), so `table[L]` is level `L`'s value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaClassDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Hit points gained per level, e.g. `1d10` for a warrior.
    pub hp_dice: Dice,
    /// Mana gained per level for a casting class; `None` for a non-caster.
    #[serde(default)]
    pub mana_dice: Option<Dice>,
    /// Total XP required to reach each level, exactly `level_exp` from
    /// `src/class.c`. Index 0 is the level-0 sentinel (`0`); index `L` is the
    /// total XP for level `L`, up to the table's last (immortal) entry.
    #[serde(default)]
    pub xp_table: Vec<u64>,
    /// THAC0 at each level, exactly the `thaco` chart from `src/class.c`.
    /// Index 0 is the level-0 sentinel (`100`); index `L` is the THAC0 at
    /// level `L`, up to the table's last (implementor) entry.
    #[serde(default)]
    pub thaco_table: Vec<i32>,
    /// Prime attribute; `None` for a class with no attribute requirement.
    #[serde(default)]
    pub prime_attribute: Option<Attribute>,
    #[serde(default)]
    pub starting_items: Vec<ItemId>,
    #[serde(default)]
    pub granted_skills: Vec<SmolStr>,
}

/// `tba.classes` — keyed by the class id (e.g. `warrior`).
///
/// `skill_levels` is CircleMUD's `init_spell_levels()`
/// (`tbamud/src/class.c:1652-1664`): the minimum level at which each class may
/// use a **skill** (it is the same `spell_info[].min_level[]` table that
/// `tba.spells` carries per spell, but the converter-authored spell catalog
/// only holds true spells). It is kept at the file level, not on each class,
/// because Circle registers it from a separate function — `spell_level(skill,
/// class, level)` — rather than in the class table itself. A skill named in a
/// class's `granted_skills` with no entry here is usable from level 1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TbaClassesFile {
    pub classes: BTreeMap<SmolStr, TbaClassDef>,
    #[serde(default)]
    pub skill_levels: BTreeMap<SmolStr, BTreeMap<SmolStr, u32>>,
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
            // Both tables are authoritative game rules, not optional tuning:
            // an empty or sentinel-less table would silently fall back to a
            // placeholder instead of Circle's progression.
            if class.xp_table.len() < 2 || class.xp_table.first() != Some(&0) {
                ctx.error_at(
                    format!("classes.{class_id}.xp_table"),
                    "xp_table must start with the level-0 sentinel (0) and define level 1",
                );
            }
            if class.thaco_table.len() < 2 || class.thaco_table.first() != Some(&100) {
                ctx.error_at(
                    format!("classes.{class_id}.thaco_table"),
                    "thaco_table must start with the level-0 sentinel (100) and define level 1",
                );
            }
        }
        // A skill level is only meaningful for a skill the class actually
        // learns; a stray key would silently never gate anything.
        for (class_id, levels) in &defs.skill_levels {
            let Some(class) = defs.classes.get(class_id) else {
                ctx.error_at(
                    format!("skill_levels.{class_id}"),
                    "class is not defined in `classes`",
                );
                continue;
            };
            for (skill, level) in levels {
                if *level < 1 {
                    ctx.error_at(
                        format!("skill_levels.{class_id}.{skill}"),
                        "skill level must be at least 1",
                    );
                }
                if !class.granted_skills.iter().any(|granted| granted == skill) {
                    ctx.error_at(
                        format!("skill_levels.{class_id}.{skill}"),
                        format!("`{skill}` is not in `{class_id}`'s granted_skills"),
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// tba.spells — Circle's spell catalog (TBA-E18)
// ---------------------------------------------------------------------------

/// What a spell does when cast.
///
/// Data-only: the `cast` handler in [`crate::spells`] implements each kind
/// generically from the spell's [`dice`](SpellDef::dice) and
/// [`affect`](SpellDef::affect), so the catalog can carry spells whose Circle
/// routine is not modelled yet as [`SpellKind::Utility`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellKind {
    /// A hostile spell that rolls [`SpellDef::dice`] against its victim.
    Damage,
    /// A restorative spell that rolls [`SpellDef::dice`] up to the target's
    /// maximum hit points.
    Heal,
    /// A spell that installs [`SpellDef::affect`] for a number of combat
    /// rounds.
    Buff,
    /// A hostile affect: a [`SpellDef::affect`] installed on a victim.
    /// Circle's `MAG_AFFECTS` curses (`blindness`, `curse`, `poison`). A
    /// `Debuff` requires an explicit victim and usually carries a save.
    Debuff,
    /// A divination: emits the message the data supports; no combat effect.
    Detect,
    /// A spell whose Circle routine is not modelled yet. It is castable (mana
    /// is spent) and reports its [`SpellDef::unsupported`] reason rather than
    /// inventing an effect.
    Utility,
}

/// A damage/heal roll's per-level term, Circle's `+ level` (`earthquake`,
/// `tbamud/src/magic.c:281`) and `+ level / 4` (`cure light`/`cure critic`,
/// `tbamud/src/magic.c:801-822`).
///
/// The bonus is `per_level * level / divisor` with C integer truncation
/// toward zero. The neutral value (`per_level` 0) adds nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelBonus {
    /// The amount added per caster level.
    #[serde(default)]
    pub per_level: i32,
    /// The divisor applied after multiplying by the level (at least 1).
    #[serde(default = "default_level_divisor")]
    pub divisor: u32,
}

/// The serde default for [`LevelBonus::divisor`]: no division.
fn default_level_divisor() -> u32 {
    1
}

impl Default for LevelBonus {
    fn default() -> Self {
        Self {
            per_level: 0,
            divisor: default_level_divisor(),
        }
    }
}

impl LevelBonus {
    /// The bonus for `level`, truncated the way Circle's C integer division
    /// does. A zero divisor is treated as 1 rather than panicking.
    pub fn bonus(&self, level: u32) -> i32 {
        let divisor = i32::try_from(self.divisor.max(1)).unwrap_or(i32::MAX);
        self.per_level.saturating_mul(level as i32) / divisor
    }
}

/// What a successful saving throw does to a spell, Circle's `mag_savingthrow`
/// branches (`tbamud/src/magic.c:36-53`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveEffect {
    /// No save (`mag_affects` spells such as `armor`/`bless`).
    #[default]
    None,
    /// A save halves the damage (`dam /= 2`, `tbamud/src/magic.c:290`).
    Half,
    /// A save negates the spell (`send_to_char(ch, CONFIG_NOEFFECT); return`,
    /// `tbamud/src/magic.c:375` and friends).
    Negate,
}

/// Who a spell is aimed at, folded down from Circle's `TAR_*` target bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellTarget {
    /// The caster (`TAR_SELF_ONLY`).
    ///
    /// Rust reserves `Self` as an enum variant name, so the Rust name is
    /// `Caster` while the authored/serde name stays `self`.
    #[serde(rename = "self")]
    Caster,
    /// Another character in the room (`TAR_CHAR_ROOM`, or `TAR_CHAR_WORLD`
    /// which this phase resolves in the caster's room only).
    Victim,
    /// The caster's room (`TAR_IGNORE` area spells such as `earthquake`).
    Room,
    /// No character target (`TAR_IGNORE`, or object-only target bits).
    None,
}

/// The duration, in combat rounds, a spell's affect lasts when the catalog
/// does not carry a source pulse count.
///
/// Circle's `mag_affects` sets an explicit `af[].duration` per spell; where a
/// spell has no source value (or no affect at all) this documented default
/// applies. It is 6 because the most common short buff in the source,
/// `SPELL_BLESS` (`tbamud/src/magic.c:344`), lasts 6 pulses.
pub const DEFAULT_SPELL_DURATION: u32 = 6;

/// The serde default for [`SpellDef::duration`].
fn default_spell_duration() -> u32 {
    DEFAULT_SPELL_DURATION
}

/// One spell, from tbaMUD's `spell_info[]`.
///
/// Circle stores a level-scaled mana cost: `mana_max` at the level the caster's
/// class first learns the spell, falling by `mana_change` per level to the
/// floor `mana_min` (`mag_manacost`, `tbamud/src/spell_parser.c:58-62`). The
/// three numbers are the `spello()` arguments (`tbamud/src/spell_parser.c:676`)
/// and [`min_level`](Self::min_level) is the per-class learning level from
/// `init_spell_levels()` (`tbamud/src/class.c:1589-1665`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellDef {
    /// Display name, for example `magic missile`.
    pub name: String,
    /// The settled floor of the mana cost, Circle's `mana_min`.
    pub mana_min: u32,
    /// The cost at the level the class first learns the spell, Circle's
    /// `mana_max`.
    pub mana_max: u32,
    /// The reduction per level, Circle's `mana_change`.
    pub mana_change: u32,
    pub kind: SpellKind,
    /// Damage/heal roll. Required for [`SpellKind::Damage`] and
    /// [`SpellKind::Heal`]; also applied alongside a `Damage` spell's affect
    /// (for example `chill touch`'s strength drain).
    #[serde(default)]
    pub dice: Option<Dice>,
    /// The per-level damage/heal term, e.g. `earthquake`'s `+ level` or
    /// `cure light`'s `+ level / 4`. Defaults to no term.
    #[serde(default)]
    pub level_bonus: LevelBonus,
    /// Affect installed for [`SpellKind::Buff`]/[`SpellKind::Debuff`], or
    /// carried alongside a damage roll.
    #[serde(default)]
    pub affect: Option<AffectDef>,
    /// Combat rounds the affect lasts (Circle's `af[].duration` from
    /// `mag_affects` in `tbamud/src/magic.c`), counted down by
    /// [`crate::spells::tick_buffs`]. Defaults to [`DEFAULT_SPELL_DURATION`]
    /// for a spell whose source carries no pulse count.
    #[serde(default = "default_spell_duration")]
    pub duration: u32,
    /// A per-caster-level term added to [`duration`](Self::duration), for the
    /// source affects whose pulse count grows with level (`strength`'s
    /// `(GET_LEVEL(ch) / 2) + 4`, `curse`'s `1 + (GET_LEVEL(ch) / 2)`, and
    /// `poison`'s `GET_LEVEL(ch)`, `tbamud/src/magic.c:380-500`). Defaults to
    /// no term.
    #[serde(default)]
    pub duration_bonus: LevelBonus,
    pub target: SpellTarget,
    /// Circle's minimum caster position (usually `standing`; combat spells are
    /// `fighting`). The caster must be at least this high on the [`Position`]
    /// ladder.
    #[serde(default)]
    pub min_position: Position,
    /// What a successful save does ([`SaveEffect::None`] for a spell the
    /// source never rolls a save for). `cast` always uses `SAVING_SPELL`.
    #[serde(default)]
    pub save: SaveEffect,
    /// The level at which each class first learns the spell, Circle's
    /// `spell_info[].min_level[]` (`init_spell_levels`, `tbamud/src/class.c`).
    /// Keyed by the `tba.classes` id. A class absent from the map pays
    /// `mana_max` (the level-1 fallback), which is the documented behaviour for
    /// a class the source never assigned the spell to.
    #[serde(default)]
    pub min_level: BTreeMap<SmolStr, u32>,
    /// The Circle routine this spell needs but the engine cannot express, with
    /// the reason. [`SpellKind::Utility`] spells must carry one; the cast
    /// spends mana and reports it.
    #[serde(default)]
    pub unsupported: Option<String>,
}

/// `tba.spells` — keyed by the snake_case spell id (for example
/// `magic_missile`), which is the name [`TbaClassDef::granted_skills`] uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpellsFile {
    pub spells: BTreeMap<SmolStr, SpellDef>,
}

/// `tba.spells`.
pub struct SpellsSection;

impl PackSection for SpellsSection {
    const ID: &'static str = "tba.spells";
    type Defs = SpellsFile;

    fn validate(defs: &Self::Defs, ctx: &mut ValidationCtx<'_>) {
        for (spell_id, spell) in &defs.spells {
            for (field, message) in spell_issues(spell_id, spell) {
                ctx.error_at(field, message);
            }
        }
    }
}

/// Structural validation for one spell, independent of the ECS.
///
/// Returns `(field, message)` pairs so both [`SpellsSection::validate`] and its
/// unit test exercise the same rules. A `Damage`/`Heal` spell without dice or
/// a `Buff` without an affect would otherwise silently do nothing on cast.
fn spell_issues(spell_id: &SmolStr, spell: &SpellDef) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    if spell.name.trim().is_empty() {
        issues.push((
            format!("spells.{spell_id}.name"),
            "spell name must not be empty".to_owned(),
        ));
    }
    if spell.mana_min > spell.mana_max {
        issues.push((
            format!("spells.{spell_id}.mana_min"),
            "mana_min must not exceed mana_max".to_owned(),
        ));
    }
    for (class, level) in &spell.min_level {
        if *level == 0 {
            issues.push((
                format!("spells.{spell_id}.min_level.{class}"),
                "a spell level must be at least 1 (class.c:1589)".to_owned(),
            ));
        }
    }
    match spell.kind {
        SpellKind::Damage | SpellKind::Heal => {
            if spell.dice.is_none() {
                issues.push((
                    format!("spells.{spell_id}.dice"),
                    "a damage or heal spell must define its dice roll".to_owned(),
                ));
            }
        }
        SpellKind::Buff | SpellKind::Debuff => {
            if spell.affect.is_none() {
                issues.push((
                    format!("spells.{spell_id}.affect"),
                    "a buff or debuff spell must define its apply affect".to_owned(),
                ));
            }
        }
        SpellKind::Detect => {}
        SpellKind::Utility => {
            if spell
                .unsupported
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            {
                issues.push((
                    format!("spells.{spell_id}.unsupported"),
                    "a utility spell must carry an `unsupported` reason (issue #7)".to_owned(),
                ));
            }
        }
    }
    // Only an unmodelled `Utility` routine may claim to be unsupported; a
    // modelled kind with a reason would hide a working effect.
    if spell.kind != SpellKind::Utility && spell.unsupported.is_some() {
        issues.push((
            format!("spells.{spell_id}.unsupported"),
            "only a utility spell may carry an `unsupported` reason".to_owned(),
        ));
    }
    issues
}

// ---------------------------------------------------------------------------
// tba.shops — CircleMUD shopkeeper economy (TBA-E17)
// ---------------------------------------------------------------------------

/// The seven `SHOP_TRADE_WITH` bits (`shop.h:78`): each names a class or
/// alignment the keeper refuses to serve (`is_ok_char`, `shop.c:108`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShopTrade {
    Good,
    Evil,
    Neutral,
    MagicUser,
    Cleric,
    Thief,
    Warrior,
}

/// Circle's shop opening hours (`SHOP_OPEN1/CLOSE1/OPEN2/CLOSE2`), in MUD
/// hours 0..=23. A shop may open twice a day; the default (`0/28/0/0`) means
/// "always open", the source convention for an unset schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopHours {
    pub open1: u32,
    pub close1: u32,
    pub open2: u32,
    pub close2: u32,
}

impl Default for ShopHours {
    fn default() -> Self {
        Self {
            open1: 0,
            close1: 28,
            open2: 0,
            close2: 0,
        }
    }
}

impl ShopHours {
    /// Whether the shop is open at `hour`, exactly Circle's `is_open`
    /// (`shop.c:140-160`): closed before the first opening, and closed during
    /// the gap between the first close and the second opening.
    pub fn is_open(&self, hour: u32) -> bool {
        if self.open1 > hour {
            return false;
        }
        if self.close1 < hour && (self.open2 > hour || self.close2 < hour) {
            return false;
        }
        true
    }
}

/// One shopkeeper's trade terms, from a `.shp` record.
///
/// Circle stores a shop as the keeper mob, the item prototypes it produces,
/// the item **types** it will buy, and a `profit_buy`/`profit_sell` pair
/// applied to an object's `cost` (`src/shop.c`, `shopping_buy` and
/// `shopping_sell`):
///
/// ```text
/// buy price  = round(cost * profit_buy)
/// sell price = round(cost * profit_sell)
/// ```
///
/// `messages` carries Circle's seven `read_shop_message` strings in source
/// order (message 0..6: no-such-item, player-lacks-item, do-not-buy,
/// keeper-lacks-cash, player-lacks-cash, buy, sell), kept verbatim including
/// their `%s`/`%d` specifiers so a later pass can render them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopDef {
    /// Circle's `profit_buy` (`SHOP_BUYPROFIT`): the multiplier applied to an
    /// item's `cost` when a player buys it. Always positive.
    pub buy_multiplier: f32,
    /// Circle's `profit_sell` (`SHOP_SELLPROFIT`): the multiplier applied to
    /// an item's `cost` when a player sells it. `0.0` means the keeper pays
    /// nothing; negative values are rejected.
    pub sell_multiplier: f32,
    /// The `ITEM_*` types the shop will purchase (`SHOP_BUYTYPE`).
    #[serde(default)]
    pub buy_kinds: Vec<TbaItemKind>,
    /// The `stdlib.items` prototypes the shop stocks (`SHOP_PRODUCT`).
    #[serde(default)]
    pub produce: Vec<ItemId>,
    /// The rooms the shop operates in (`SHOP_ROOM`), in source order. Empty
    /// means the keeper acts wherever it stands.
    #[serde(default)]
    pub rooms: Vec<RoomId>,
    /// The classes/alignments the keeper refuses to serve (`SHOP_TRADE_WITH`).
    #[serde(default)]
    pub trade_with: Vec<ShopTrade>,
    /// The keeper's opening hours. Defaults to always open.
    #[serde(default)]
    pub hours: ShopHours,
    /// Circle's seven shop messages, when the source carries them.
    #[serde(default)]
    pub messages: Vec<String>,
    /// Circle's `broke_temper` (`SHOP_BROKE_TEMPER`): the keeper-temper
    /// message index.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub broke_temper: u32,
    /// Circle's `SHOP_*` feature bits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bitvector: Vec<ShopFlag>,
    /// Per-type buy keywords (`SHOP_BUYWORD`), keyed by the item kind they
    /// apply to. A keeper that buys by name rather than by type uses these.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub buy_words: BTreeMap<TbaItemKind, String>,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// `tba.shops` — keyed by the shopkeeper's `stdlib.npcs` id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopsFile {
    pub shops: BTreeMap<NpcId, ShopDef>,
}

/// `tba.shops`.
pub struct ShopsSection;

impl PackSection for ShopsSection {
    const ID: &'static str = "tba.shops";
    type Defs = ShopsFile;

    fn validate(defs: &Self::Defs, ctx: &mut ValidationCtx<'_>) {
        let npcs = npc_ids(ctx);
        let items = item_ids(ctx);
        let rooms = room_ids(ctx);
        for (npc_id, shop) in &defs.shops {
            for (field, message) in shop_issues(npc_id, shop, &npcs, &items, &rooms) {
                ctx.error_at(field, message);
            }
        }
    }
}

/// Structural validation for one shop, independent of the ECS.
///
/// Returns `(field, message)` pairs so both [`ShopsSection::validate`] and its
/// unit test exercise the same rules. A shop whose keeper is not an NPC, whose
/// multipliers are nonsensical, or whose stock names an item that does not
/// exist would otherwise load and misbehave at trade time.
fn shop_issues(
    npc_id: &NpcId,
    shop: &ShopDef,
    npcs: &BTreeSet<NpcId>,
    items: &BTreeSet<ItemId>,
    rooms: &BTreeSet<RoomId>,
) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    let field = format!("shops.{npc_id}");
    if !npcs.contains(npc_id) {
        issues.push((
            field.clone(),
            format!("unknown npc `{npc_id}` (shopkeeper must be a stdlib.npcs entry)"),
        ));
    }
    // A multiplier must be finite and on the right side of zero; `<=` (not a
    // negated `>`) plus an explicit finiteness check also rejects NaN.
    if !shop.buy_multiplier.is_finite() || shop.buy_multiplier <= 0.0 {
        issues.push((
            format!("{field}.buy_multiplier"),
            format!(
                "buy_multiplier must be positive, got {}",
                shop.buy_multiplier
            ),
        ));
    }
    if !shop.sell_multiplier.is_finite() || shop.sell_multiplier < 0.0 {
        issues.push((
            format!("{field}.sell_multiplier"),
            format!(
                "sell_multiplier must not be negative, got {}",
                shop.sell_multiplier
            ),
        ));
    }
    for (index, item) in shop.produce.iter().enumerate() {
        if !items.contains(item) {
            issues.push((
                format!("{field}.produce[{index}]"),
                format!("unknown item `{item}` (not defined in stdlib.items)"),
            ));
        }
    }
    for (index, room) in shop.rooms.iter().enumerate() {
        if !rooms.contains(room) {
            issues.push((
                format!("{field}.rooms[{index}]"),
                format!("unknown room `{room}` (not defined in stdlib.rooms)"),
            ));
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// tba.socials — CircleMUD social emotes (issue #17)
// ---------------------------------------------------------------------------

/// One Circle social from `lib/misc/socials.new` (the active file: the source
/// checkout has `use_new_socials = YES`, `config.c:271`).
///
/// Every string keeps the source's `$`-codes verbatim; the runtime
/// ([`crate::socials`]) substitutes them against the actor, the target and the
/// optional body part/object. A source string written as `#` (Circle's NULL) is
/// `None`.
///
/// The map key in [`SocialsFile`] is the social word (Circle's `command`), so it
/// is not repeated on the struct — the same shape as `tba.shops` keying on the
/// keeper id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocialDef {
    /// Circle's `sort_as`: the command-list sort key (usually the same word).
    #[serde(default)]
    pub sort_as: SmolStr,
    /// Circle's `hide`: when set, a room that cannot see the actor does not see
    /// the action (`act(..., hide, ...)`).
    #[serde(default)]
    pub hide: bool,
    /// Circle's `min_char_position`: the actor's minimum [`Position`] (the
    /// numeric `POS_*` value; `-1` disables the gate).
    #[serde(default)]
    pub min_char_position: i32,
    /// Circle's `min_victim_position`: the target's minimum [`Position`]
    /// (`-1` disables the gate).
    #[serde(default)]
    pub min_victim_position: i32,
    /// Circle's `min_level_char`: the actor's minimum level.
    #[serde(default)]
    pub min_level_char: i32,
    /// No argument: shown to the actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_no_arg: Option<String>,
    /// No argument: shown to the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub others_no_arg: Option<String>,
    /// A target was found: shown to the actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_found: Option<String>,
    /// A target was found: shown to the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub others_found: Option<String>,
    /// A target was found: shown to the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vict_found: Option<String>,
    /// No target was found: shown to the actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_found: Option<String>,
    /// The target was the actor: shown to the actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_auto: Option<String>,
    /// The target was the actor: shown to the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub others_auto: Option<String>,
    /// A body part was named: shown to the actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_body_found: Option<String>,
    /// A body part was named: shown to the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub others_body_found: Option<String>,
    /// A body part was named: shown to the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vict_body_found: Option<String>,
    /// An object was targeted: shown to the actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_obj_found: Option<String>,
    /// An object was targeted: shown to the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub others_obj_found: Option<String>,
}

impl SocialDef {
    /// True when the social carries no message at all (an inert entry). The
    /// validator rejects these rather than shipping a command that does
    /// nothing.
    pub fn is_empty(&self) -> bool {
        self.message_strings()
            .iter()
            .all(|message| message.is_none())
    }

    /// Every message string, in source order.
    pub fn message_strings(&self) -> [Option<&String>; 13] {
        [
            self.char_no_arg.as_ref(),
            self.others_no_arg.as_ref(),
            self.char_found.as_ref(),
            self.others_found.as_ref(),
            self.vict_found.as_ref(),
            self.not_found.as_ref(),
            self.char_auto.as_ref(),
            self.others_auto.as_ref(),
            self.char_body_found.as_ref(),
            self.others_body_found.as_ref(),
            self.vict_body_found.as_ref(),
            self.char_obj_found.as_ref(),
            self.others_obj_found.as_ref(),
        ]
    }
}

/// `tba.socials` — keyed by the social word.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocialsFile {
    pub socials: BTreeMap<SmolStr, SocialDef>,
}

/// `tba.socials`.
pub struct SocialsSection;

impl PackSection for SocialsSection {
    const ID: &'static str = "tba.socials";
    type Defs = SocialsFile;

    fn validate(defs: &Self::Defs, ctx: &mut ValidationCtx<'_>) {
        for (name, social) in &defs.socials {
            let field = format!("socials.{name}");
            if name.trim().is_empty() {
                ctx.error_at(field.clone(), "social name must not be empty");
            }
            if social.is_empty() {
                ctx.error_at(
                    field.clone(),
                    "social carries no message strings and could never render",
                );
            }
            if social.sort_as.trim().is_empty() {
                ctx.error_at(
                    format!("{field}.sort_as"),
                    "sort_as must not be empty (defaults to the social name)",
                );
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
    registry.register::<SpellsSection>();
    registry.register::<ShopsSection>();
    registry.register::<SocialsSection>();
    crate::triggers::content::register(registry);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn spell(kind: SpellKind) -> SpellDef {
        SpellDef {
            name: "test spell".to_owned(),
            mana_min: 1,
            mana_max: 1,
            mana_change: 0,
            kind,
            dice: None,
            level_bonus: LevelBonus::default(),
            affect: None,
            duration: DEFAULT_SPELL_DURATION,
            duration_bonus: LevelBonus::default(),
            target: SpellTarget::Victim,
            min_position: Position::Standing,
            save: SaveEffect::None,
            min_level: BTreeMap::new(),
            unsupported: None,
        }
    }

    #[test]
    fn spec_proc_serialises_as_snake_case_and_is_absent_when_unassigned() {
        let base = TbaNpcStats {
            hp: Dice::constant(1),
            level: 1,
            hitroll: 0,
            damroll: 0,
            armor_class: 100,
            thaco: 20,
            alignment: 0,
            gold: 0,
            experience: 0,
            default_position: Position::Standing,
            behaviour: Vec::new(),
            damage: None,
            attributes: TbaAttributes::mob_default(),
            saves: TbaSaves::default(),
            spec_proc: None,
            sex: TbaSex::Neutral,
            keywords: Vec::new(),
            aff_flags: Vec::new(),
        };
        let yaml = serde_yaml::to_string(&base).expect("stats serialise");
        assert!(!yaml.contains("spec_proc"), "an unassigned mob omits it");

        let assigned = TbaNpcStats {
            spec_proc: Some(SpecProc::KingWelmar),
            ..base
        };
        let yaml = serde_yaml::to_string(&assigned).expect("assigned stats serialise");
        assert!(yaml.contains("spec_proc: king_welmar"), "{yaml}");
        let decoded: TbaNpcStats = serde_yaml::from_str(&yaml).expect("stats round-trip");
        assert_eq!(decoded.spec_proc, Some(SpecProc::KingWelmar));
    }

    #[test]
    fn damage_and_heal_spells_must_define_dice() {
        let id = SmolStr::new("test_spell");
        let issues = spell_issues(&id, &spell(SpellKind::Damage));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "spells.test_spell.dice");
        assert_eq!(spell_issues(&id, &spell(SpellKind::Heal)).len(), 1);

        let mut with_dice = spell(SpellKind::Damage);
        with_dice.dice = Some(Dice::new(1, 6, 0));
        assert!(spell_issues(&id, &with_dice).is_empty());
    }

    #[test]
    fn buff_spells_must_define_an_affect_and_names_must_not_be_empty() {
        let id = SmolStr::new("test_spell");
        let issues = spell_issues(&id, &spell(SpellKind::Buff));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "spells.test_spell.affect");

        let mut buff = spell(SpellKind::Buff);
        buff.affect = Some(AffectDef {
            location: AffectLocation::ArmorClass,
            modifier: -10,
        });
        assert!(spell_issues(&id, &buff).is_empty());

        let mut unnamed = spell(SpellKind::Utility);
        unnamed.name = "   ".to_owned();
        unnamed.unsupported = Some("not modelled".to_owned());
        let issues = spell_issues(&id, &unnamed);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "spells.test_spell.name");
    }

    #[test]
    fn utility_spells_must_carry_an_unsupported_reason() {
        let id = SmolStr::new("test_spell");
        let utility = spell(SpellKind::Utility);
        let issues = spell_issues(&id, &utility);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "spells.test_spell.unsupported");

        let mut reasoned = utility.clone();
        reasoned.unsupported = Some("MAG_SUMMONS is not modelled".to_owned());
        assert!(spell_issues(&id, &reasoned).is_empty());

        // A modelled kind must not pretend to be unsupported.
        let mut liar = spell(SpellKind::Detect);
        liar.unsupported = Some("nope".to_owned());
        let issues = spell_issues(&id, &liar);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "spells.test_spell.unsupported");
    }

    #[test]
    fn level_bonus_truncates_like_the_c_source() {
        assert_eq!(LevelBonus::default().bonus(100), 0);
        let earthquake = LevelBonus {
            per_level: 1,
            divisor: 1,
        };
        assert_eq!(earthquake.bonus(7), 7);
        let cure = LevelBonus {
            per_level: 1,
            divisor: 4,
        };
        assert_eq!(cure.bonus(7), 1, "7 / 4 truncates to 1");
        assert_eq!(cure.bonus(0), 0);
    }

    fn shop() -> ShopDef {
        ShopDef {
            buy_multiplier: 1.15,
            sell_multiplier: 0.5,
            buy_kinds: vec![TbaItemKind::Weapon],
            produce: vec![ItemId::from("sword")],
            rooms: Vec::new(),
            trade_with: Vec::new(),
            hours: ShopHours::default(),
            messages: Vec::new(),
            broke_temper: 0,
            bitvector: Vec::new(),
            buy_words: BTreeMap::new(),
        }
    }

    #[test]
    fn shop_validation_checks_keeper_multipliers_and_stock() {
        let npc = NpcId::from("tba_npc_3000");
        let npcs = BTreeSet::from([npc.clone()]);
        let items = BTreeSet::from([ItemId::from("sword")]);
        let rooms = BTreeSet::from([RoomId::from("tba.3001")]);
        assert!(shop_issues(&npc, &shop(), &npcs, &items, &rooms).is_empty());

        // An unknown keeper names the shop key itself.
        let stranger = NpcId::from("tba_npc_9999");
        let issues = shop_issues(&stranger, &shop(), &npcs, &items, &rooms);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "shops.tba_npc_9999");

        // A non-positive / negative / NaN multiplier is rejected.
        let mut bad_buy = shop();
        bad_buy.buy_multiplier = 0.0;
        assert_eq!(shop_issues(&npc, &bad_buy, &npcs, &items, &rooms).len(), 1);
        bad_buy.buy_multiplier = f32::NAN;
        assert_eq!(shop_issues(&npc, &bad_buy, &npcs, &items, &rooms).len(), 1);

        let mut bad_sell = shop();
        bad_sell.sell_multiplier = -0.1;
        let issues = shop_issues(&npc, &bad_sell, &npcs, &items, &rooms);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "shops.tba_npc_3000.sell_multiplier");
        // Zero is a valid "pays nothing" multiplier.
        bad_sell.sell_multiplier = 0.0;
        assert!(shop_issues(&npc, &bad_sell, &npcs, &items, &rooms).is_empty());

        // Unknown stock is reported at its exact index.
        let mut bad_stock = shop();
        bad_stock.produce = vec![ItemId::from("sword"), ItemId::from("ghost")];
        let issues = shop_issues(&npc, &bad_stock, &npcs, &items, &rooms);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "shops.tba_npc_3000.produce[1]");

        // Unknown room restrictions are reported at their exact index.
        let mut bad_rooms = shop();
        bad_rooms.rooms = vec![RoomId::from("tba.3001"), RoomId::from("tba.9999")];
        let issues = shop_issues(&npc, &bad_rooms, &npcs, &items, &rooms);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].0, "shops.tba_npc_3000.rooms[1]");
    }

    #[test]
    fn shop_hours_match_circles_is_open() {
        // The source default is always open.
        assert!(ShopHours::default().is_open(0));
        assert!(ShopHours::default().is_open(23));

        // Open 9..17, closed evening and overnight.
        let daytime = ShopHours {
            open1: 9,
            close1: 17,
            open2: 0,
            close2: 0,
        };
        assert!(!daytime.is_open(8), "before the first opening");
        assert!(daytime.is_open(9));
        assert!(daytime.is_open(17), "close hour is inclusive");
        assert!(!daytime.is_open(18), "after the first close");
        assert!(!daytime.is_open(23), "closed for the day");

        // A lunch-close shop reopens in the afternoon.
        let lunch = ShopHours {
            open1: 8,
            close1: 12,
            open2: 14,
            close2: 20,
        };
        assert!(lunch.is_open(8));
        assert!(!lunch.is_open(13), "between the two shifts");
        assert!(lunch.is_open(14));
        assert!(lunch.is_open(20));
        assert!(!lunch.is_open(21));
    }
}
