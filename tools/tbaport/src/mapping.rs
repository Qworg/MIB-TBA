//! Mapping the parsed tbaMUD records onto the real `mib-stdlib` / `mib-tba`
//! content types, and building the drop report.
//!
//! Everything the source sets that has no target field is recorded in the
//! report rather than dropped silently. Flag tables are transcribed from
//! `src/structs.h`; the numbers in comments are the source constants.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use mib_stdlib::content::{
    AreaDef, ContainerDef, DoorDef, ExitDef, ExtraDescDef, ItemDef, ItemId, ItemTag,
    NpcBehaviorFlags, NpcDef, NpcId, RoomDef, RoomId, RoomTag, Sector,
};
use mib_tba::combat::{GOLD_ITEM_DESCRIPTION, GOLD_ITEM_ID, GOLD_ITEM_NAME};
use mib_tba::content::{
    AffectDef, AffectLocation, DoorReset, NpcBehaviour, Position, ResetMode, ShopDef, TbaItemDef,
    TbaItemKind, TbaNpcStats, WearSlot, ZoneCommand, ZoneDef,
};

use crate::emit::OutputBundle;
use crate::error::{ConvertError, Result};
use crate::ids;
use crate::report::{
    CommandFlag, DanglingRef, DeferredEntry, DropReport, REPORT_SCHEMA, SourceFiles, Totals,
    UnmappedFlag, ZoneReport,
};
use crate::source::{
    deferred::{DeferredRecord, scan_quests, scan_triggers},
    mob::RawMob,
    obj::RawObj,
    parse_mobs, parse_objs, parse_rooms,
    room::RawRoom,
    shop::{RawShop, parse_shops},
    tokens::Source,
    zone::{RawZone, parse_zone},
};

/// Direction words, in the order of `dirs[]` in `constants.c:35`.
const DIRS: [&str; 10] = [
    "north",
    "east",
    "south",
    "west",
    "up",
    "down",
    "northwest",
    "northeast",
    "southeast",
    "southwest",
];

/// Room flag names (`structs.h:73`).
const ROOM_FLAG_NAMES: [&str; 17] = [
    "DARK",
    "DEATH",
    "NOMOB",
    "INDOORS",
    "PEACEFUL",
    "SOUNDPROOF",
    "NOTRACK",
    "NOMAGIC",
    "TUNNEL",
    "PRIVATE",
    "GODROOM",
    "HOUSE",
    "HOUSE_CRASH",
    "ATRIUM",
    "OLC",
    "BFS_MARK",
    "WORLDMAP",
];

/// Mob flag names (`structs.h:207`).
const MOB_FLAG_NAMES: [&str; 20] = [
    "SPEC",
    "SENTINEL",
    "SCAVENGER",
    "ISNPC",
    "AWARE",
    "AGGRESSIVE",
    "STAY_ZONE",
    "WIMPY",
    "AGGR_EVIL",
    "AGGR_GOOD",
    "AGGR_NEUTRAL",
    "MEMORY",
    "HELPER",
    "NOCHARM",
    "NOSUMMON",
    "NOSLEEP",
    "NOBASH",
    "NOBLIND",
    "NOKILL",
    "NOTDEADYET",
];

/// Affected-bit names (`structs.h:272`).
const AFF_FLAG_NAMES: [&str; 23] = [
    "DONTUSE",
    "BLIND",
    "INVISIBLE",
    "DETECT_ALIGN",
    "DETECT_INVIS",
    "DETECT_MAGIC",
    "SENSE_LIFE",
    "WATERWALK",
    "SANCTUARY",
    "UNUSED",
    "CURSE",
    "INFRAVISION",
    "POISON",
    "PROTECT_EVIL",
    "PROTECT_GOOD",
    "SLEEP",
    "NOTRACK",
    "FLYING",
    "SCUBA",
    "SNEAK",
    "HIDE",
    "FREE",
    "CHARM",
];

/// Object extra-flag names (`structs.h:412`).
const ITEM_EXTRA_NAMES: [&str; 18] = [
    "GLOW",
    "HUM",
    "NORENT",
    "NODONATE",
    "NOINVIS",
    "INVISIBLE",
    "MAGIC",
    "NODROP",
    "BLESS",
    "ANTI_GOOD",
    "ANTI_EVIL",
    "ANTI_NEUTRAL",
    "ANTI_MAGIC_USER",
    "ANTI_CLERIC",
    "ANTI_THIEF",
    "ANTI_WARRIOR",
    "NOSELL",
    "QUEST",
];

/// `APPLY_*` names (`structs.h:434`).
const APPLY_NAMES: [&str; 25] = [
    "NONE",
    "STR",
    "DEX",
    "INT",
    "WIS",
    "CON",
    "CHA",
    "CLASS",
    "LEVEL",
    "AGE",
    "CHAR_WEIGHT",
    "CHAR_HEIGHT",
    "MANA",
    "HIT",
    "MOVE",
    "GOLD",
    "EXP",
    "AC",
    "HITROLL",
    "DAMROLL",
    "SAVING_PARA",
    "SAVING_ROD",
    "SAVING_PETRI",
    "SAVING_BREATH",
    "SAVING_SPELL",
];

/// Zone flag names (`constants.c:92`).
const ZONE_FLAG_NAMES: [&str; 7] = [
    "CLOSED",
    "NO_IMMORT",
    "QUEST",
    "GRID",
    "NOBUILD",
    "!ASTRAL",
    "WORLDMAP",
];

/// Options for one conversion run.
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    /// tbaMUD checkout root (contains `lib/world`).
    pub source_root: PathBuf,
    /// Convert only this zone number.
    pub only: Option<u32>,
    /// Tag the room with this vnum as `start_room` (Circle's
    /// `CONFIG_MORTAL_START`, default 3001). `None` leaves every room untagged.
    pub start_room: Option<u32>,
}

/// The result of a run: the content bundle plus its drop report.
#[derive(Debug, Clone)]
pub struct Conversion {
    pub bundle: OutputBundle,
    pub report: DropReport,
}

/// A zone and all the raw records read from its file set.
struct ZoneSource {
    /// Zone number as written in the `.zon` header (canonical).
    number: u32,
    zone: RawZone,
    rooms: Vec<RawRoom>,
    mobs: Vec<RawMob>,
    objs: Vec<RawObj>,
    shops: Vec<RawShop>,
    triggers: Vec<DeferredRecord>,
    quests: Vec<DeferredRecord>,
    files: SourceFiles,
}

/// Convert a tbaMUD world into MIB content plus a drop report.
pub fn convert(options: &ConvertOptions) -> Result<Conversion> {
    let world = options.source_root.join("lib/world");
    let numbers = selected_zone_numbers(&world, options.only)?;

    // Phase 1: read everything. The mapping pass needs the global id sets to
    // resolve (and report) cross-zone references.
    let mut sources = Vec::with_capacity(numbers.len());
    for number in numbers {
        sources.push(load_zone(&world, number)?);
    }

    let room_ids: BTreeSet<RoomId> = sources
        .iter()
        .flat_map(|s| s.rooms.iter().map(|r| ids::room_id(r.vnum)))
        .collect();
    let npc_ids: BTreeSet<NpcId> = sources
        .iter()
        .flat_map(|s| s.mobs.iter().map(|m| ids::npc_id(m.vnum)))
        .collect();
    let item_ids: BTreeSet<ItemId> = sources
        .iter()
        .flat_map(|s| s.objs.iter().map(|o| ids::item_id(o.vnum)))
        .collect();

    // Phase 2: map.
    let mut bundle = OutputBundle::empty();
    let mut zones = Vec::with_capacity(sources.len());
    for source in &sources {
        let mut zr = ZoneReport {
            number: source.number,
            name: source.zone.name.clone(),
            bot: source.zone.bot,
            top: source.zone.top,
            area: ids::area_id(source.number).to_string(),
            rooms: 0,
            npcs: 0,
            items: 0,
            commands: 0,
            source_files: source.files.clone(),
            deferred: Vec::new(),
            dangling: Vec::new(),
            unmapped: Vec::new(),
            command_flags: Vec::new(),
        };

        bundle.areas.areas.insert(
            ids::area_id(source.number),
            AreaDef {
                name: source.zone.name.clone(),
            },
        );

        for raw in &source.rooms {
            let def = map_room(raw, &room_ids, &item_ids, options.start_room, &mut zr);
            bundle.rooms.rooms.insert(ids::room_id(raw.vnum), def);
        }
        for raw in &source.mobs {
            let id = ids::npc_id(raw.vnum);
            let (npc, stats) = map_npc(raw, &mut zr);
            bundle.npcs.npcs.insert(id.clone(), npc);
            // `tba.npc_stats` keys must name a `stdlib.npcs` entry. Both are
            // built from this record, so the guard only fires on an id
            // collision; it exists so a refactor cannot emit a dangling key.
            if bundle.npcs.npcs.contains_key(&id) {
                bundle.npc_stats.npc_stats.insert(id, stats);
            } else {
                push_dangling(
                    &mut zr,
                    "dangling_npc_stats_key",
                    &format!("npc {}", raw.vnum),
                    id.as_ref(),
                );
            }
        }
        for raw in &source.objs {
            let id = ids::item_id(raw.vnum);
            let (item, tba) = map_item(raw, &mut zr);
            bundle.items.items.insert(id.clone(), item);
            // Same invariant as `tba.npc_stats`: a `tba.items` key must name a
            // `stdlib.items` entry.
            if bundle.items.items.contains_key(&id) {
                bundle.tba_items.items.insert(id, tba);
            } else {
                push_dangling(
                    &mut zr,
                    "dangling_item_stats_key",
                    &format!("item {}", raw.vnum),
                    id.as_ref(),
                );
            }
        }

        let zone_def = map_zone(&source.zone, &room_ids, &npc_ids, &item_ids, &mut zr);
        bundle.zones.zones.insert(source.number, zone_def);
        map_shops(source, &npc_ids, &item_ids, &mut bundle, &mut zr);

        zr.rooms = source.rooms.len();
        zr.npcs = source.mobs.len();
        zr.items = source.objs.len();
        zr.commands = zone_def_kept(&bundle, source.number);

        add_deferred(
            &mut zr,
            "triggers",
            &source.triggers,
            "DG scripts are deferred (doc §10)",
        );
        add_deferred(
            &mut zr,
            "quests",
            &source.quests,
            "quests are deferred (doc §9)",
        );

        zones.push(zr);
    }

    // The death penalty's gold prototype is a genre rule, not a world object:
    // no tbaMUD source file defines it, so the converter always adds it. A
    // converted world that fully replaces `content/` therefore still has the
    // prototype TBA-E09 drops.
    insert_gold_prototype(&mut bundle);

    // Confirm the tag landed: the requested room must exist and actually carry
    // the tag. A missing room is a no-op recorded as `None` (the CLI also
    // warns) rather than a silent success or a hard failure, so `--only`
    // conversions of zones that do not hold the start room still work.
    let start_room = options
        .start_room
        .filter(|vnum| bundle.rooms.rooms.contains_key(&ids::room_id(*vnum as i32)));

    let mut report = DropReport {
        schema: REPORT_SCHEMA,
        tool: format!("tbaport {}", env!("CARGO_PKG_VERSION")),
        source: options.source_root.display().to_string(),
        out: String::new(),
        mode: if options.only.is_some() {
            "only".to_owned()
        } else {
            "full".to_owned()
        },
        only: options.only,
        start_room,
        totals: Totals::default(),
        zones,
    };
    report.finalise();

    Ok(Conversion { bundle, report })
}

/// Insert the fixed `tba_gold` prototype (TBA-E09) into both item sections.
///
/// It uses `or_insert`, so a world that genuinely defines an item with this id
/// keeps its own definition; the runtime death penalty uses the same
/// [`GOLD_ITEM_ID`] and strings.
fn insert_gold_prototype(bundle: &mut OutputBundle) {
    let id = ItemId::from(GOLD_ITEM_ID);
    bundle
        .items
        .items
        .entry(id.clone())
        .or_insert_with(|| ItemDef {
            name: GOLD_ITEM_NAME.to_owned(),
            description: GOLD_ITEM_DESCRIPTION.to_owned(),
            value: Some(1),
            container: None,
            tags: Vec::new(),
        });
    bundle
        .tba_items
        .items
        .entry(id)
        .or_insert_with(|| TbaItemDef {
            kind: TbaItemKind::Money,
            slots: Vec::new(),
            values: [0; 4],
            affects: Vec::new(),
            weight: 1,
            cost: 1,
            rent: 0,
            min_level: 0,
            timer: None,
        });
}

/// Number of commands kept for a zone after mapping.
fn zone_def_kept(bundle: &OutputBundle, number: u32) -> usize {
    bundle
        .zones
        .zones
        .get(&number)
        .map_or(0, |zone| zone.commands.len())
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

fn selected_zone_numbers(world: &Path, only: Option<u32>) -> Result<Vec<u32>> {
    if let Some(number) = only {
        return Ok(vec![number]);
    }
    let dir = world.join("zon");
    let entries = std::fs::read_dir(&dir).map_err(|source| ConvertError::Io {
        path: dir.clone(),
        source,
    })?;
    let mut numbers = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ConvertError::Io {
            path: dir.clone(),
            source,
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".zon")
            && let Ok(number) = stem.parse::<u32>()
        {
            numbers.push(number);
        }
    }
    numbers.sort_unstable();
    numbers.dedup();
    Ok(numbers)
}

fn load_zone(world: &Path, filename_number: u32) -> Result<ZoneSource> {
    let zon_path = world.join("zon").join(format!("{filename_number}.zon"));
    let mut zone_src = Source::from_path(&zon_path)?;
    let zone = parse_zone(&mut zone_src)?;

    let mut files = SourceFiles {
        zon: true,
        ..SourceFiles::default()
    };

    let (rooms, wld) = optional_parse(&world.join("wld"), filename_number, "wld", parse_rooms)?;
    files.wld = wld;
    let (mobs, mob) = optional_parse(&world.join("mob"), filename_number, "mob", parse_mobs)?;
    files.mob = mob;
    let (objs, obj) = optional_parse(&world.join("obj"), filename_number, "obj", parse_objs)?;
    files.obj = obj;

    let (shops, shp) = optional_parse(&world.join("shp"), filename_number, "shp", parse_shops)?;
    files.shp = shp;
    let triggers = optional_scan(&world.join("trg"), filename_number, "trg", scan_triggers)?;
    files.trg = triggers.is_some();
    let quests = optional_scan(&world.join("qst"), filename_number, "qst", scan_quests)?;
    files.qst = quests.is_some();

    Ok(ZoneSource {
        number: zone.number,
        zone,
        rooms: rooms.unwrap_or_default(),
        mobs: mobs.unwrap_or_default(),
        objs: objs.unwrap_or_default(),
        shops: shops.unwrap_or_default(),
        triggers: triggers.unwrap_or_default(),
        quests: quests.unwrap_or_default(),
        files,
    })
}

fn optional_parse<T>(
    dir: &Path,
    number: u32,
    ext: &str,
    parser: fn(&mut Source) -> Result<T>,
) -> Result<(Option<T>, bool)> {
    let path = dir.join(format!("{number}.{ext}"));
    if !path.is_file() {
        return Ok((None, false));
    }
    let mut src = Source::from_path(&path)?;
    Ok((Some(parser(&mut src)?), true))
}

fn optional_scan(
    dir: &Path,
    number: u32,
    ext: &str,
    scanner: fn(&mut Source) -> Result<Vec<DeferredRecord>>,
) -> Result<Option<Vec<DeferredRecord>>> {
    let path = dir.join(format!("{number}.{ext}"));
    if !path.is_file() {
        return Ok(None);
    }
    let mut src = Source::from_path(&path)?;
    Ok(Some(scanner(&mut src)?))
}

// ---------------------------------------------------------------------------
// Rooms
// ---------------------------------------------------------------------------

fn map_room(
    raw: &RawRoom,
    rooms: &BTreeSet<RoomId>,
    items: &BTreeSet<ItemId>,
    start_room: Option<u32>,
    zr: &mut ZoneReport,
) -> RoomDef {
    let object = format!("room tba.{}", raw.vnum);
    let mut tags = map_room_flags(raw.flags, &object, zr);
    if start_room == Some(raw.vnum.max(0) as u32) {
        tags.push(RoomTag::StartRoom);
        tags.sort();
        tags.dedup();
    }
    let sector = map_sector(raw.sector, &object, zr);
    let mut exits = BTreeMap::new();

    for exit in &raw.exits {
        if !(0..10).contains(&exit.dir) {
            push_unmapped(
                zr,
                &object,
                "exit_direction",
                "out_of_range",
                &exit.dir.to_string(),
            );
            continue;
        }
        if exit.to_room <= 0 || exit.to_room == 65535 {
            push_unmapped(
                zr,
                &object,
                "exit_description_only",
                DIRS[exit.dir as usize],
                &exit.description,
            );
            continue;
        }

        let to = ids::room_id(exit.to_room);
        if !rooms.contains(&to) {
            // A dangling exit destination is dropped: the emitted pack must
            // never name a room that does not exist.
            push_dangling(
                zr,
                "dangling_exit",
                &format!("{object} exit {}", DIRS[exit.dir as usize]),
                to.as_ref(),
            );
            continue;
        }

        let code = exit.exit_info_code;
        let has_door = (1..=4).contains(&code);
        let hidden = code == 3 || code == 4;
        let pickproof = code == 2 || code == 4;
        // Only a real door emits a key; the loader stores a key on a plain
        // exit but nothing ever reads it. Validate only what we emit.
        let key = if has_door && exit.key > 0 && exit.key != 65535 {
            let key_id = ids::item_id(exit.key);
            if items.contains(&key_id) {
                Some(key_id)
            } else {
                // Keep the door, drop the key: a dangling `key` would fail
                // `stdlib.rooms` validation.
                push_dangling(
                    zr,
                    "dangling_door_key",
                    &format!("{object} exit {} door.key", DIRS[exit.dir as usize]),
                    key_id.as_ref(),
                );
                None
            }
        } else {
            None
        };
        let door = if has_door {
            Some(DoorDef {
                locked: false,
                closed: false,
                key,
                name: if exit.keywords.trim().is_empty() {
                    None
                } else {
                    Some(exit.keywords.clone())
                },
                pickable: !pickproof,
                difficulty: 0,
            })
        } else {
            None
        };
        exits.insert(DIRS[exit.dir as usize].into(), ExitDef { to, hidden, door });
    }

    let extra_descriptions = raw
        .extra_descriptions
        .iter()
        .map(|extra| ExtraDescDef {
            keywords: extra
                .keywords
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            description: extra.description.clone(),
        })
        .collect();

    RoomDef {
        area: ids::area_id(zr.number),
        name: raw.name.clone(),
        description: raw.description.clone(),
        exits,
        spawns: Vec::new(),
        tags,
        num: Some(raw.vnum.max(0) as u32),
        coord: None,
        sector,
        extra_descriptions,
    }
}

fn map_room_flags(flags: [u64; 4], object: &str, zr: &mut ZoneReport) -> Vec<RoomTag> {
    use crate::source::tokens::bit;
    let f0 = flags[0];
    let mut tags = Vec::new();
    if bit(f0, 0) {
        tags.push(RoomTag::Dark);
    }
    // ROOM_NOMOB, ROOM_INDOORS (mapped as its absence), ROOM_PEACEFUL,
    // ROOM_SOUNDPROOF, ROOM_TUNNEL, ROOM_PRIVATE, ROOM_GODROOM.
    if bit(f0, 2) {
        tags.push(RoomTag::NoMob);
    }
    if !bit(f0, 3) {
        // MIB has only the positive tag; a room the source does not mark
        // INDOORS is outdoor.
        tags.push(RoomTag::Outdoor);
    }
    if bit(f0, 4) {
        tags.push(RoomTag::Peaceful);
    }
    if bit(f0, 5) {
        tags.push(RoomTag::Soundproof);
    }
    if bit(f0, 8) {
        tags.push(RoomTag::Tunnel);
    }
    if bit(f0, 9) {
        tags.push(RoomTag::Private);
    }
    if bit(f0, 10) {
        tags.push(RoomTag::Godroom);
    }
    // Bits with a target: 0..10 above. Everything else is recorded.
    let mapped = [0u32, 2, 3, 4, 5, 8, 9, 10];
    for (index, name) in ROOM_FLAG_NAMES.iter().enumerate() {
        let index = index as u32;
        if bit(f0, index) && !mapped.contains(&index) {
            push_unmapped(zr, object, "room_flag", name, "");
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

fn map_sector(sector: i32, object: &str, zr: &mut ZoneReport) -> Sector {
    match sector {
        0 => Sector::Inside,
        1 => Sector::City,
        2 => Sector::Field,
        3 => Sector::Forest,
        4 => Sector::Hills,
        5 => Sector::Mountain,
        6 => Sector::WaterSwim,
        7 => Sector::WaterNoSwim,
        8 => Sector::Flying,
        9 => Sector::Underwater,
        other => {
            push_unmapped(zr, object, "sector", "out_of_range", &other.to_string());
            Sector::Inside
        }
    }
}

// ---------------------------------------------------------------------------
// NPCs
// ---------------------------------------------------------------------------

fn map_npc(raw: &RawMob, zr: &mut ZoneReport) -> (NpcDef, TbaNpcStats) {
    use crate::source::tokens::bit;
    let object = format!("npc tba_npc_{}", raw.vnum);

    let f0 = raw.mob_flags[0];
    let behaviors = NpcBehaviorFlags {
        sentinel: bit(f0, 1),
        scavenger: bit(f0, 2),
        wander: false,
        wimpy: bit(f0, 7),
    };
    // Fixed canonical order (the `NpcBehaviour` declaration order), so the
    // emitted list is deterministic without needing `Ord`.
    let mut behaviour = Vec::new();
    if bit(f0, 5) {
        behaviour.push(NpcBehaviour::Aggressive);
    }
    if bit(f0, 8) {
        behaviour.push(NpcBehaviour::AggrEvil);
    }
    if bit(f0, 9) {
        behaviour.push(NpcBehaviour::AggrGood);
    }
    if bit(f0, 10) {
        behaviour.push(NpcBehaviour::AggrNeutral);
    }
    if bit(f0, 1) {
        behaviour.push(NpcBehaviour::Sentinel);
    }
    if bit(f0, 6) {
        behaviour.push(NpcBehaviour::StayZone);
    }
    if bit(f0, 11) {
        behaviour.push(NpcBehaviour::Memory);
    }
    if bit(f0, 12) {
        behaviour.push(NpcBehaviour::Helper);
    }
    if bit(f0, 2) {
        behaviour.push(NpcBehaviour::Scavenger);
    }
    if bit(f0, 7) {
        behaviour.push(NpcBehaviour::Wimpy);
    }
    if bit(f0, 4) {
        behaviour.push(NpcBehaviour::Aware);
    }

    // Flags with no target field. MOB_ISNPC (3) and MOB_NOTDEADYET (19) are
    // runtime bookkeeping the loader sets itself and are not reported.
    let mapped: [u32; 11] = [1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    for (index, name) in MOB_FLAG_NAMES.iter().enumerate() {
        let index = index as u32;
        if bit(f0, index) && !mapped.contains(&index) && index != 3 && index != 19 {
            push_unmapped(zr, &object, "mob_flag", name, "");
        }
    }

    for (index, name) in AFF_FLAG_NAMES.iter().enumerate() {
        if bit(raw.aff_flags[0], index as u32) {
            push_unmapped(zr, &object, "mob_aff", name, "");
        }
    }

    for espec in &raw.especs {
        push_unmapped(zr, &object, "mob_espec", espec.trim(), "");
    }
    push_unmapped(
        zr,
        &object,
        "mob_damage_dice",
        &raw.damage.to_string(),
        "TbaNpcStats carries damroll but not attack dice",
    );
    push_unmapped(
        zr,
        &object,
        "mob_sex",
        &raw.sex.to_string(),
        "TbaNpcStats has no sex field",
    );
    push_unmapped(
        zr,
        &object,
        "npc_keywords",
        &raw.name,
        "stdlib.npcs has no keyword list",
    );

    // MOB_AWARE (bit 4) is already mapped above.

    let persona = join_persona(&raw.long_descr, &raw.description);
    let npc = NpcDef {
        name: normalize_short_description(&raw.short_descr),
        persona,
        dialogue: None,
        tags: Vec::new(),
        behaviors,
    };

    let stats = TbaNpcStats {
        hp: raw.hp,
        level: raw.level.max(0) as u32,
        hitroll: 20 - raw.hitroll_field,
        damroll: raw.damage.bonus,
        armor_class: 10 * raw.ac_field,
        thaco: 20,
        alignment: raw.alignment,
        gold: raw.gold,
        experience: raw.experience,
        default_position: map_position(raw.default_position, &object, zr),
        behaviour,
    };

    (npc, stats)
}

fn map_position(value: i32, object: &str, zr: &mut ZoneReport) -> Position {
    match value {
        0 => Position::Dead,
        1 => Position::MortallyWounded,
        2 => Position::Incapacitated,
        3 => Position::Stunned,
        4 => Position::Sleeping,
        5 => Position::Resting,
        6 => Position::Sitting,
        7 => Position::Fighting,
        8 => Position::Standing,
        other => {
            push_unmapped(zr, object, "position", "out_of_range", &other.to_string());
            Position::Standing
        }
    }
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

fn map_item(raw: &RawObj, zr: &mut ZoneReport) -> (ItemDef, TbaItemDef) {
    use crate::source::tokens::bit;
    let object = format!("item tba_item_{}", raw.vnum);

    let kind = map_item_kind(raw.obj_type, &object, zr);
    let slots = map_wear_slots(raw.wear_flags[0]);
    let affects = map_applies(raw, &object, zr);

    for (index, name) in ITEM_EXTRA_NAMES.iter().enumerate() {
        if bit(raw.extra_flags[0], index as u32) {
            push_unmapped(zr, &object, "item_extra_flag", name, "");
        }
    }
    for (index, name) in AFF_FLAG_NAMES.iter().enumerate() {
        if bit(raw.affect_flags[0], index as u32) {
            push_unmapped(
                zr,
                &object,
                "item_affect_flag",
                name,
                "AFF_* affect-flags have no target field yet",
            );
        }
    }
    push_unmapped(
        zr,
        &object,
        "item_keywords",
        &raw.name,
        "stdlib.items has no keyword list",
    );
    if !raw.action_description.trim().is_empty() {
        push_unmapped(
            zr,
            &object,
            "item_action_description",
            &raw.action_description,
            "stdlib.items has no action description",
        );
    }
    if !raw.extra_descriptions.is_empty() {
        push_unmapped(
            zr,
            &object,
            "item_extra_descriptions",
            &raw.extra_descriptions.len().to_string(),
            "stdlib.items has no extra descriptions field",
        );
    }

    let cost = raw.cost.max(0) as u32;
    let item = ItemDef {
        name: normalize_short_description(&raw.short_description),
        description: if raw.description.trim().is_empty() {
            raw.short_description.clone()
        } else {
            capitalize_first(&raw.description)
        },
        value: Some(cost),
        container: if raw.obj_type == 15 {
            Some(ContainerDef {
                capacity: raw.values[0].max(0) as u32,
            })
        } else {
            None
        },
        tags: map_item_tags(raw.obj_type),
    };

    let tba = TbaItemDef {
        kind,
        slots,
        values: raw.values,
        affects,
        weight: raw.weight.max(0) as u32,
        cost,
        rent: raw.rent.max(0) as u32,
        min_level: raw.level.max(0) as u32,
        timer: if raw.timer > 0 {
            Some(raw.timer as u32)
        } else {
            None
        },
    };

    (item, tba)
}

fn map_item_kind(value: i32, object: &str, zr: &mut ZoneReport) -> TbaItemKind {
    match value {
        0 => TbaItemKind::Undefined,
        1 => TbaItemKind::Light,
        2 => TbaItemKind::Scroll,
        3 => TbaItemKind::Wand,
        4 => TbaItemKind::Staff,
        5 => TbaItemKind::Weapon,
        6 => TbaItemKind::Furniture,
        7 => TbaItemKind::Free,
        8 => TbaItemKind::Treasure,
        9 => TbaItemKind::Armor,
        10 => TbaItemKind::Potion,
        11 => TbaItemKind::Worn,
        12 => TbaItemKind::Other,
        13 => TbaItemKind::Trash,
        14 => TbaItemKind::Free2,
        15 => TbaItemKind::Container,
        16 => TbaItemKind::Note,
        17 => TbaItemKind::DrinkContainer,
        18 => TbaItemKind::Key,
        19 => TbaItemKind::Food,
        20 => TbaItemKind::Money,
        21 => TbaItemKind::Pen,
        22 => TbaItemKind::Boat,
        23 => TbaItemKind::Fountain,
        other => {
            push_unmapped(zr, object, "item_type", "out_of_range", &other.to_string());
            TbaItemKind::Undefined
        }
    }
}

fn map_item_tags(obj_type: i32) -> Vec<ItemTag> {
    match obj_type {
        1 => vec![ItemTag::LightSource],
        5 => vec![ItemTag::Weapon],
        9 => vec![ItemTag::Armor],
        17 | 23 => vec![ItemTag::Drink],
        18 => vec![ItemTag::Key],
        19 => vec![ItemTag::Food],
        22 => vec![ItemTag::Boat],
        _ => Vec::new(),
    }
}

/// `ITEM_WEAR_*` bits (`structs.h:393`) → equipment slots (`structs.h:342`).
///
/// `TAKE` is deliberately not a wear position. The single `FINGER`, `NECK`
/// and `WRIST` flags expand to both paired slots.
fn map_wear_slots(flags: u64) -> Vec<WearSlot> {
    use crate::source::tokens::bit;
    let mut slots = Vec::new();
    let mut push = |slot: WearSlot| slots.push(slot);
    if bit(flags, 1) {
        push(WearSlot::FingerR);
        push(WearSlot::FingerL);
    }
    if bit(flags, 2) {
        push(WearSlot::Neck1);
        push(WearSlot::Neck2);
    }
    if bit(flags, 3) {
        push(WearSlot::Body);
    }
    if bit(flags, 4) {
        push(WearSlot::Head);
    }
    if bit(flags, 5) {
        push(WearSlot::Legs);
    }
    if bit(flags, 6) {
        push(WearSlot::Feet);
    }
    if bit(flags, 7) {
        push(WearSlot::Hands);
    }
    if bit(flags, 8) {
        push(WearSlot::Arms);
    }
    if bit(flags, 9) {
        push(WearSlot::Shield);
    }
    if bit(flags, 10) {
        push(WearSlot::About);
    }
    if bit(flags, 11) {
        push(WearSlot::Waist);
    }
    if bit(flags, 12) {
        push(WearSlot::WristR);
        push(WearSlot::WristL);
    }
    if bit(flags, 13) {
        push(WearSlot::Wield);
    }
    if bit(flags, 14) {
        push(WearSlot::Hold);
    }
    slots
}

fn map_applies(raw: &RawObj, object: &str, zr: &mut ZoneReport) -> Vec<AffectDef> {
    let mut affects = Vec::new();
    for apply in &raw.applies {
        if apply.location == 0 {
            continue;
        }
        if let Some(location) = map_apply_location(apply.location) {
            affects.push(AffectDef {
                location,
                modifier: apply.modifier,
            });
        } else {
            let name = APPLY_NAMES
                .get(apply.location as usize)
                .copied()
                .unwrap_or("UNKNOWN");
            push_unmapped(
                zr,
                object,
                "affect_location",
                name,
                &format!("modifier {}", apply.modifier),
            );
        }
    }
    affects
}

fn map_apply_location(value: i32) -> Option<AffectLocation> {
    Some(match value {
        1 => AffectLocation::Str,
        2 => AffectLocation::Dex,
        3 => AffectLocation::Int,
        4 => AffectLocation::Wis,
        5 => AffectLocation::Con,
        6 => AffectLocation::Cha,
        12 => AffectLocation::MaxMana,
        13 => AffectLocation::MaxHp,
        14 => AffectLocation::MaxMove,
        17 => AffectLocation::ArmorClass,
        18 => AffectLocation::Hitroll,
        19 => AffectLocation::Damroll,
        20 => AffectLocation::SavePara,
        21 => AffectLocation::SaveRod,
        22 => AffectLocation::SavePetri,
        23 => AffectLocation::SaveBreath,
        24 => AffectLocation::SaveSpell,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Zone resets
// ---------------------------------------------------------------------------

fn map_zone(
    raw: &RawZone,
    rooms: &BTreeSet<RoomId>,
    npcs: &BTreeSet<NpcId>,
    items: &BTreeSet<ItemId>,
    zr: &mut ZoneReport,
) -> ZoneDef {
    let mut commands = Vec::new();

    for (index, command) in raw.commands.iter().enumerate() {
        let from = format!("zone {} command {index} ({})", raw.number, command.code);
        zr.command_flags.push(CommandFlag {
            index,
            code: command.code.to_string(),
            if_flag: command.if_flag,
        });
        match command.code {
            'M' => {
                let mob = ids::npc_id(command.arg1);
                let room = ids::room_id(command.arg3);
                let mob_ok = if npcs.contains(&mob) {
                    true
                } else {
                    push_dangling(zr, "dangling_reset_mob", &from, mob.as_ref());
                    false
                };
                let room_ok = check(&from, &room, rooms, "dangling_reset_room", "room", zr);
                if !(mob_ok && room_ok) {
                    continue;
                }
                commands.push(ZoneCommand::PlaceMob {
                    mob,
                    room,
                    max_in_world: command.arg2.max(0) as u32,
                });
            }
            'O' => {
                if command.arg3 <= 0 || command.arg3 == 65535 {
                    push_unmapped(
                        zr,
                        &from,
                        "zone_command_dropped",
                        "O",
                        "drop target room is NOWHERE",
                    );
                    continue;
                }
                let item = ids::item_id(command.arg1);
                let room = ids::room_id(command.arg3);
                let item_ok = check_item(&from, &item, items, zr, "dangling_reset_item");
                let room_ok = check(&from, &room, rooms, "dangling_reset_room", "room", zr);
                if !(item_ok && room_ok) {
                    continue;
                }
                commands.push(ZoneCommand::DropItem {
                    item,
                    room,
                    max_in_world: command.arg2.max(0) as u32,
                });
            }
            'G' => {
                let item = ids::item_id(command.arg1);
                if !check_item(&from, &item, items, zr, "dangling_reset_item") {
                    continue;
                }
                commands.push(ZoneCommand::GiveItem {
                    item,
                    max_in_world: command.arg2.max(0) as u32,
                });
            }
            'E' => {
                let slot = match map_wear_slot(command.arg3) {
                    Some(slot) => slot,
                    None => {
                        push_unmapped(
                            zr,
                            &from,
                            "zone_command_dropped",
                            "E",
                            &format!("equipment position {} out of range", command.arg3),
                        );
                        continue;
                    }
                };
                let item = ids::item_id(command.arg1);
                if !check_item(&from, &item, items, zr, "dangling_reset_item") {
                    continue;
                }
                commands.push(ZoneCommand::EquipItem {
                    item,
                    slot,
                    max_in_world: command.arg2.max(0) as u32,
                });
            }
            'P' => {
                let item = ids::item_id(command.arg1);
                let container = ids::item_id(command.arg3);
                let item_ok = check_item(&from, &item, items, zr, "dangling_reset_item");
                let container_ok =
                    check_item(&from, &container, items, zr, "dangling_reset_container");
                if !(item_ok && container_ok) {
                    continue;
                }
                commands.push(ZoneCommand::PutItem {
                    item,
                    container,
                    max_in_world: command.arg2.max(0) as u32,
                });
            }
            'D' => {
                if !(0..10).contains(&command.arg2) {
                    push_unmapped(
                        zr,
                        &from,
                        "zone_command_dropped",
                        "D",
                        &format!("exit direction {} out of range", command.arg2),
                    );
                    continue;
                }
                let state = match command.arg3 {
                    0 => DoorReset::Open,
                    1 => DoorReset::Closed,
                    2 => DoorReset::Locked,
                    other => {
                        push_unmapped(
                            zr,
                            &from,
                            "zone_command_dropped",
                            "D",
                            &format!("door state {other} out of range"),
                        );
                        continue;
                    }
                };
                let room = ids::room_id(command.arg1);
                if !check(&from, &room, rooms, "dangling_reset_room", "room", zr) {
                    continue;
                }
                commands.push(ZoneCommand::DoorState {
                    room,
                    exit: DIRS[command.arg2 as usize].to_owned(),
                    state,
                });
            }
            'R' => {
                let room = ids::room_id(command.arg1);
                let item = ids::item_id(command.arg2);
                let room_ok = check(&from, &room, rooms, "dangling_reset_room", "room", zr);
                let item_ok = check_item(&from, &item, items, zr, "dangling_reset_item");
                if !(room_ok && item_ok) {
                    continue;
                }
                commands.push(ZoneCommand::RemoveItem { room, item });
            }
            'T' => {
                let attach = match command.arg1 {
                    0 => "mob",
                    1 => "object",
                    2 => "world",
                    _ => "unknown",
                };
                add_deferred(
                    zr,
                    "triggers",
                    &[DeferredRecord {
                        vnum: command.arg2,
                        attach: attach.to_owned(),
                        detail: format!("zone {} reset T", raw.number),
                    }],
                    "DG scripts are deferred (doc §10)",
                );
            }
            'V' => {
                add_deferred(
                    zr,
                    "trigger_variables",
                    &[DeferredRecord {
                        vnum: command.arg1,
                        attach: "world".to_owned(),
                        detail: format!("zone {} reset V", raw.number),
                    }],
                    "DG trigger variables are deferred (doc §10)",
                );
            }
            other => push_unmapped(
                zr,
                &from,
                "zone_command_dropped",
                &other.to_string(),
                "unknown reset command",
            ),
        }
    }

    for (index, name) in ZONE_FLAG_NAMES.iter().enumerate() {
        if crate::source::tokens::bit(raw.zone_flags[0], index as u32) {
            push_unmapped(
                zr,
                &format!("zone {}", raw.number),
                "zone_flag",
                name,
                "ZoneDef has no zone-flags field",
            );
        }
    }

    ZoneDef {
        name: raw.name.clone(),
        bot: raw.bot.max(0) as u32,
        top: raw.top.max(0) as u32,
        lifespan_mins: raw.lifespan,
        reset_mode: map_reset_mode(raw.reset_mode, &format!("zone {}", raw.number), zr),
        commands,
    }
}

fn check(
    from: &str,
    target: &RoomId,
    rooms: &BTreeSet<RoomId>,
    kind: &str,
    label: &str,
    zr: &mut ZoneReport,
) -> bool {
    if rooms.contains(target) {
        true
    } else {
        push_dangling(zr, kind, &format!("{from} {label}"), target.as_ref());
        false
    }
}

fn check_item(
    from: &str,
    target: &ItemId,
    items: &BTreeSet<ItemId>,
    zr: &mut ZoneReport,
    kind: &str,
) -> bool {
    if items.contains(target) {
        true
    } else {
        push_dangling(zr, kind, from, target.as_ref());
        false
    }
}

fn map_wear_slot(value: i32) -> Option<WearSlot> {
    Some(match value {
        0 => WearSlot::Light,
        1 => WearSlot::FingerR,
        2 => WearSlot::FingerL,
        3 => WearSlot::Neck1,
        4 => WearSlot::Neck2,
        5 => WearSlot::Body,
        6 => WearSlot::Head,
        7 => WearSlot::Legs,
        8 => WearSlot::Feet,
        9 => WearSlot::Hands,
        10 => WearSlot::Arms,
        11 => WearSlot::Shield,
        12 => WearSlot::About,
        13 => WearSlot::Waist,
        14 => WearSlot::WristR,
        15 => WearSlot::WristL,
        16 => WearSlot::Wield,
        17 => WearSlot::Hold,
        _ => return None,
    })
}

fn map_reset_mode(value: i32, object: &str, zr: &mut ZoneReport) -> ResetMode {
    match value {
        0 => ResetMode::Never,
        1 => ResetMode::WhenDesert,
        2 => ResetMode::Always,
        other => {
            push_unmapped(zr, object, "reset_mode", "out_of_range", &other.to_string());
            ResetMode::Always
        }
    }
}

// ---------------------------------------------------------------------------
// Shops
// ---------------------------------------------------------------------------

/// Map a zone's `.shp` records into `tba.shops`.
///
/// The shop's key is the **keeper mob vnum** (`SHOP_KEEPER`), because that is
/// what Circle's `shop_keeper` matches the NPC against; the record's `#vnum`
/// label is only a shop id and is reported when it differs. A keeper that is
/// not a converted NPC, a duplicate keeper (Circle resolves the first record),
/// and a produce vnum with no converted item are all dropped through the same
/// `dangling_*` seam as the rest of the converter. Every other `.shp` field
/// that has no `tba.shops` target is named in the report.
fn map_shops(
    source: &ZoneSource,
    npc_ids: &BTreeSet<NpcId>,
    item_ids: &BTreeSet<ItemId>,
    bundle: &mut OutputBundle,
    zr: &mut ZoneReport,
) {
    for raw in &source.shops {
        let keeper = ids::npc_id(raw.keeper);
        let label = format!("shop tba_npc_{}", raw.keeper);
        let from = format!("shop #{}", raw.record);

        if raw.keeper < 0 || !npc_ids.contains(&keeper) {
            push_dangling(zr, "dangling_shop_keeper", &from, keeper.as_ref());
            continue;
        }
        if bundle.shops.shops.contains_key(&keeper) {
            push_dangling(zr, "duplicate_shop_keeper", &from, keeper.as_ref());
            continue;
        }
        // `tba.shops` requires a positive buy multiplier; a zero-profit shop
        // cannot be represented, so it is dropped and named rather than
        // emitted as content the registry would reject (shop 34300 in the
        // tbaMUD world is such a stub). A non-finite value is rejected too.
        if !raw.profit_buy.is_finite() || raw.profit_buy <= 0.0 {
            push_unmapped(
                zr,
                &label,
                "shop_profit_buy",
                &raw.profit_buy.to_string(),
                "profit_buy is not positive; the shop is dropped",
            );
            continue;
        }

        let mut produce = Vec::with_capacity(raw.produce.len());
        for vnum in &raw.produce {
            let item = ids::item_id(*vnum);
            if item_ids.contains(&item) {
                produce.push(item);
            } else {
                push_dangling(zr, "dangling_shop_produce", &label, item.as_ref());
            }
        }

        let buy_kinds = raw
            .buy_types
            .iter()
            .map(|entry| map_item_kind(entry.kind, &label, zr))
            .collect();

        for word in raw.buy_words() {
            push_unmapped(
                zr,
                &label,
                "shop_buy_word",
                word,
                "tba.shops has no per-type buy keyword",
            );
        }
        if !raw.rooms.is_empty() {
            push_unmapped(
                zr,
                &label,
                "shop_room_restriction",
                &format!("{:?}", raw.rooms),
                "a shop here acts wherever its keeper stands",
            );
        }
        if raw.broke_temper != 0 {
            push_unmapped(
                zr,
                &label,
                "shop_broke_temper",
                &raw.broke_temper.to_string(),
                "no keeper-temper reaction is modelled",
            );
        }
        if raw.bitvector != 0 {
            push_unmapped(
                zr,
                &label,
                "shop_bitvector",
                &raw.bitvector.to_string(),
                "SHOP_* bitvector has no target field",
            );
        }
        if raw.trade_with != 0 {
            push_unmapped(
                zr,
                &label,
                "shop_trade_with",
                &raw.trade_with.to_string(),
                "trade-with class/alignment bits have no target field",
            );
        }
        if (raw.open1, raw.close1, raw.open2, raw.close2) != (0, 28, 0, 0) {
            push_unmapped(
                zr,
                &label,
                "shop_hours",
                &format!("{}/{}/{}/{}", raw.open1, raw.close1, raw.open2, raw.close2),
                "a shop here is always open",
            );
        }
        if raw.record != raw.keeper {
            push_unmapped(
                zr,
                &label,
                "shop_record",
                &raw.record.to_string(),
                "the #vnum is a shop label; the keeper vnum is the key",
            );
        }

        bundle.shops.shops.insert(
            keeper,
            ShopDef {
                buy_multiplier: raw.profit_buy,
                sell_multiplier: raw.profit_sell,
                buy_kinds,
                produce,
                messages: raw.messages.clone(),
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn add_deferred(zr: &mut ZoneReport, section: &str, records: &[DeferredRecord], reason: &str) {
    for record in records {
        zr.deferred.push(DeferredEntry {
            section: section.to_owned(),
            vnum: record.vnum,
            attach: record.attach.clone(),
            detail: record.detail.clone(),
            reason: reason.to_owned(),
        });
    }
}

fn push_dangling(zr: &mut ZoneReport, kind: &str, from: &str, target: &str) {
    zr.dangling.push(DanglingRef {
        kind: kind.to_owned(),
        from: from.to_owned(),
        target: target.to_owned(),
    });
}

fn push_unmapped(zr: &mut ZoneReport, object: &str, kind: &str, name: &str, detail: &str) {
    zr.unmapped.push(UnmappedFlag {
        object: object.to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
        detail: detail.to_owned(),
    });
}

/// Replicate the loader's `*tmpptr = LOWER(*tmpptr)` when the first word is a
/// leading article (`db.c:1915`, `db.c:2112`).
fn normalize_short_description(input: &str) -> String {
    let first = input.chars().next();
    let article = input
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(article.as_str(), "a" | "an" | "the")
        && first.is_some_and(|c| c.is_ascii_uppercase())
    {
        let mut chars = input.chars();
        let mut out = String::with_capacity(input.len());
        if let Some(c) = chars.next() {
            out.push(c.to_ascii_lowercase());
        }
        out.extend(chars);
        out
    } else {
        input.to_owned()
    }
}

/// Replicate `CAP()` (`db.c:2118`): uppercase only the first character.
fn capitalize_first(input: &str) -> String {
    let mut chars = input.chars();
    let mut out = String::with_capacity(input.len());
    if let Some(c) = chars.next() {
        out.extend(c.to_uppercase());
    }
    out.extend(chars);
    out
}

fn join_persona(long: &str, detailed: &str) -> String {
    match (long.trim().is_empty(), detailed.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => long.trim_end().to_owned(),
        (true, false) => detailed.trim_end().to_owned(),
        (false, false) => format!("{}\n\n{}", long.trim_end(), detailed.trim_end()),
    }
}
