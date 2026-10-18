//! Mapping the parsed tbaMUD records onto the real `mib-stdlib` / `mib-tba`
//! content types, and building the drop report.
//!
//! Everything the source sets that has no target field is recorded in the
//! report rather than dropped silently. Flag tables are transcribed from
//! `src/structs.h`; the numbers in comments are the source constants.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dg_vm::analyze::{Corpus, analyze};
use dg_vm::types::AttachType;
use mib_content::prelude::SmolStr;
use mib_stdlib::content::{
    AreaDef, ContainerDef, DoorDef, ExitDef, ExtraDescDef, ItemDef, ItemId, ItemTag,
    NpcBehaviorFlags, NpcDef, NpcId, QuestDef, QuestId, QuestOrigin, QuestStepDef, RewardSpec,
    RoomDef, RoomId, RoomTag, Sector,
};
use mib_tba::combat::{
    GOLD_ITEM_DESCRIPTION, GOLD_ITEM_ID, GOLD_ITEM_NAME, TbaAttributes, TbaSaves,
};
use mib_tba::content::{
    AffectDef, AffectLocation, DoorReset, ItemAffectFlag, ItemExtraFlag, NpcBehaviour, Position,
    ResetMode, ShopDef, ShopHours, ShopTrade, SocialDef, TbaItemDef, TbaItemKind, TbaNpcStats,
    WearSlot, ZoneCommand, ZoneCommandKind, ZoneDef,
};
use mib_tba::triggers::content::{TriggerDef, TriggerId, TriggersFile};

mod attach;

use crate::emit::OutputBundle;
use crate::error::{ConvertError, Result};
use crate::ids;
use crate::report::{
    CommandFlag, DanglingRef, DropReport, GlobalDrop, REPORT_SCHEMA, SourceFiles, Totals,
    TriggerAttachments, TriggerCoverage, TriggerTriage, TriggerTriageRecord, UnmappedFlag,
    ZoneReport,
};
use crate::source::{
    deferred::{TriggerRecord, scan_triggers},
    mob::RawMob,
    obj::RawObj,
    parse_mobs, parse_objs, parse_rooms,
    quest::{RawQuest, parse_quests},
    room::RawRoom,
    shop::{RawShop, parse_shops},
    social::{RawSocial, parse_socials},
    text::{load_circle_help, load_message_topics, load_text_topics},
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

/// Object extra-flag names. Kept as the ordered source table so the emitted
/// [`ItemExtraFlag`] order matches `structs.h:412`.
#[allow(dead_code)]
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
    triggers: Vec<TriggerRecord>,
    quests: Vec<RawQuest>,
    files: SourceFiles,
}

/// A resolved attachment target for one trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AttachCandidate {
    Npc(i32),
    Item(i32),
    Room(i32),
}

impl AttachCandidate {
    /// The content id (`tba_npc_3`, `tba_item_16`, `tba.22`).
    fn id(self) -> String {
        match self {
            AttachCandidate::Npc(vnum) => ids::npc_id(vnum).to_string(),
            AttachCandidate::Item(vnum) => ids::item_id(vnum).to_string(),
            AttachCandidate::Room(vnum) => ids::room_id(vnum).to_string(),
        }
    }

    /// The `tba.triggers` attach string (`npc:<id>`, …).
    fn attach(self) -> String {
        match self {
            AttachCandidate::Npc(_) => format!("npc:{}", self.id()),
            AttachCandidate::Item(_) => format!("item:{}", self.id()),
            AttachCandidate::Room(_) => format!("room:{}", self.id()),
        }
    }

    /// The triage-style owner label (`npc tba_npc_3`, …).
    fn label(self) -> String {
        match self {
            AttachCandidate::Npc(_) => format!("npc {}", self.id()),
            AttachCandidate::Item(_) => format!("item {}", self.id()),
            AttachCandidate::Room(_) => format!("room {}", self.id()),
        }
    }
}

/// One thing that attaches a trigger, from the `.wld`/`.mob`/`.obj` `T` lines
/// or a zone-reset `T` command.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TriggerOwner {
    /// The triage label (`npc tba_npc_3`, `zone tba_30 reset`).
    label: String,
    /// The zone used for the demo/essential check (the triage's `owner_zones`).
    zone: u32,
}

/// Collect every trigger attachment for the triage. The `zone` field is the
/// triage's `owner_zones`, so the curated essential set cannot drift. Which
/// entity a trigger really lands on is [`attach`]'s job.
fn collect_trigger_owners(sources: &[ZoneSource]) -> BTreeMap<i32, Vec<TriggerOwner>> {
    let mut owners: BTreeMap<i32, Vec<TriggerOwner>> = BTreeMap::new();
    for source in sources {
        for mob in &source.mobs {
            for trigger in &mob.trigger_vnums {
                owners.entry(*trigger).or_default().push(TriggerOwner {
                    label: AttachCandidate::Npc(mob.vnum).label(),
                    zone: crate::triage::vnum_zone(mob.vnum),
                });
            }
        }
        for obj in &source.objs {
            for trigger in &obj.trigger_vnums {
                owners.entry(*trigger).or_default().push(TriggerOwner {
                    label: AttachCandidate::Item(obj.vnum).label(),
                    zone: crate::triage::vnum_zone(obj.vnum),
                });
            }
        }
        for room in &source.rooms {
            for trigger in &room.trigger_vnums {
                owners.entry(*trigger).or_default().push(TriggerOwner {
                    label: AttachCandidate::Room(room.vnum).label(),
                    zone: crate::triage::vnum_zone(room.vnum),
                });
            }
        }
        // Zone-reset `T` commands count as owners under the reset zone,
        // whether or not their target resolves.
        for command in source.zone.commands.iter().filter(|c| c.code == 'T') {
            owners.entry(command.arg2).or_default().push(TriggerOwner {
                label: format!("zone tba_{} reset", source.number),
                zone: source.number,
            });
        }
    }
    for list in owners.values_mut() {
        list.sort();
        list.dedup();
    }
    owners
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
    let trigger_kinds = trigger_attach_types(&sources);

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
            triggers: Vec::new(),
            trigger_variables: Vec::new(),
            zone_trigger_links: Vec::new(),
        };

        bundle.areas.areas.insert(
            ids::area_id(source.number),
            AreaDef {
                name: source.zone.name.clone(),
                weather_zone: None,
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
        map_shops(source, &npc_ids, &item_ids, &room_ids, &mut bundle, &mut zr);

        zr.rooms = source.rooms.len();
        zr.npcs = source.mobs.len();
        zr.items = source.objs.len();
        zr.commands = zone_def_kept(&bundle, source.number);

        map_triggers(source, &mut bundle);
        attach::attach_prototype_lines(source, &trigger_kinds, &mut bundle, &mut zr);
        add_trigger_records(&mut zr, &source.triggers);
        for raw in &source.quests {
            if let Some((quest_id, quest)) = map_quest(raw, &room_ids, &npc_ids, &item_ids, &mut zr)
            {
                bundle.quests.quests.insert(quest_id, quest);
            }
        }

        zones.push(zr);
    }

    // Zone-reset `T` commands fold into the prototype lists only after every
    // zone's own `T` lines are in, so a folded trigger always follows the
    // target's own lines even when the target lives in another zone.
    let entities = attach::Entities {
        rooms: &room_ids,
        npcs: &npc_ids,
        items: &item_ids,
        triggers: &trigger_kinds,
        zones: sources
            .iter()
            .flat_map(|source| {
                let owners = source
                    .mobs
                    .iter()
                    .map(|mob| AttachCandidate::Npc(mob.vnum))
                    .chain(
                        source
                            .objs
                            .iter()
                            .map(|obj| AttachCandidate::Item(obj.vnum)),
                    )
                    .chain(
                        source
                            .rooms
                            .iter()
                            .map(|room| AttachCandidate::Room(room.vnum)),
                    );
                owners.map(|owner| (owner, source.number))
            })
            .collect(),
    };
    for (source, zr) in sources.iter().zip(zones.iter_mut()) {
        attach::fold_reset_scripts(source, &entities, &mut bundle, zr);
    }

    // The death penalty's gold prototype is a genre rule, not a world object:
    // no tbaMUD source file defines it, so the converter always adds it. A
    // converted world that fully replaces `content/` therefore still has the
    // prototype TBA-E09 drops.
    insert_gold_prototype(&mut bundle);

    // Login prose and socials are world-global, not per-zone; load them once.
    let mut drops = global_drops();
    load_help(&options.source_root, &mut bundle)?;
    load_socials(&options.source_root, &mut bundle, &mut drops)?;

    // Confirm the tag landed: the requested room must exist and actually carry
    // the tag. A missing room is a no-op recorded as `None` (the CLI also
    // warns) rather than a silent success or a hard failure, so `--only`
    // conversions of zones that do not hold the start room still work.
    let start_room = options
        .start_room
        .filter(|vnum| bundle.rooms.rooms.contains_key(&ids::room_id(*vnum as i32)));

    let trigger_coverage = trigger_coverage(&bundle.triggers);
    let trigger_attachments = summarise_attachments(&bundle.triggers);

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
        global_drops: drops,
        trigger_triage: TriggerTriage::default(),
        trigger_coverage,
        trigger_attachments,
        totals: Totals::default(),
        zones,
    };
    report.finalise();

    Ok(Conversion { bundle, report })
}

/// Build the standalone DG trigger triage (`reports/trigger-triage.json`).
///
/// This is the regenerable source for the Phase-2 (TBA-E20) scope: every
/// `.trg` record, its classification, its attachment owners, and the curated
/// essential-interactivity ranking documented in `docs/design/triggers.md`.
/// It performs no mapping and emits no content.
pub fn build_triage(options: &ConvertOptions) -> Result<crate::triage::TriageReport> {
    let world = options.source_root.join("lib/world");
    let numbers = selected_zone_numbers(&world, options.only)?;

    let mut sources = Vec::with_capacity(numbers.len());
    for number in numbers {
        sources.push(load_zone(&world, number)?);
    }

    // Owner map: trigger vnum -> every thing that attaches it, from the
    // `.mob`/`.obj`/`.wld` `T` lines and the zone-reset `T` commands.
    // Attachment is what makes a trigger demo-visible.
    let owners = collect_trigger_owners(&sources);

    let mut records: Vec<crate::triage::TriageRecord> = Vec::new();
    for source in &sources {
        for trigger in &source.triggers {
            let owner_entries: Vec<(String, u32)> = owners
                .get(&trigger.vnum)
                .map(|list| list.iter().map(|o| (o.label.clone(), o.zone)).collect())
                .unwrap_or_default();
            let mut owner_ids: Vec<String> =
                owner_entries.iter().map(|(id, _)| id.clone()).collect();
            let mut owner_zones: Vec<u32> = owner_entries.iter().map(|(_, z)| *z).collect();
            owner_zones.sort_unstable();
            owner_zones.dedup();
            let in_demo = crate::triage::is_demo_zone(source.number)
                || owner_zones.iter().any(|z| crate::triage::is_demo_zone(*z));
            let essential = in_demo
                && crate::triage::is_essential(trigger.bucket, &trigger.attach, &trigger.types);
            let score = crate::triage::score(
                trigger.bucket,
                source.number,
                &owner_zones,
                &trigger.types,
                trigger.lines,
                &trigger.name,
            );
            owner_ids.sort();
            owner_ids.dedup();
            records.push(crate::triage::TriageRecord {
                vnum: trigger.vnum,
                zone: source.number,
                source_file: format!("{}.trg", source.number),
                attach: trigger.attach.clone(),
                name: trigger.name.clone(),
                flags: trigger.flags.clone(),
                types: trigger.types.clone(),
                narg: trigger.narg,
                bucket: trigger.bucket.as_str().to_owned(),
                lines: trigger.lines,
                raw_lines: trigger.raw_lines,
                essential,
                tier: if essential {
                    crate::triage::tier(trigger.bucket)
                } else {
                    3
                },
                score,
                rank: None,
                owners: owner_ids,
            });
        }
    }
    records.sort_by_key(|record| (record.zone, record.vnum));

    // Rank the essential set by demo impact, with `(zone, vnum)` tie-breaks.
    let mut essential_indices: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.essential)
        .map(|(index, _)| index)
        .collect();
    essential_indices.sort_by(|a, b| {
        let left = &records[*a];
        let right = &records[*b];
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.zone.cmp(&right.zone))
            .then_with(|| left.vnum.cmp(&right.vnum))
    });
    let mut rank = 0usize;
    for index in essential_indices {
        rank += 1;
        records[index].rank = Some(rank);
    }

    // Roll-ups.
    let mut corpus = crate::triage::Corpus::default();
    let mut by_attach: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_bucket: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_zone: BTreeMap<u32, usize> = BTreeMap::new();
    let mut essential_buckets: BTreeMap<String, usize> = BTreeMap::new();
    let mut essential_attach: BTreeMap<String, usize> = BTreeMap::new();
    let mut essential_zones: BTreeSet<u32> = BTreeSet::new();
    for record in &records {
        *by_attach.entry(record.attach.clone()).or_default() += 1;
        *by_bucket.entry(record.bucket.clone()).or_default() += 1;
        *by_zone.entry(record.zone).or_default() += 1;
        for name in &record.types {
            *by_type.entry(name.clone()).or_default() += 1;
        }
        corpus.script_lines += record.raw_lines;
        corpus.nonblank_script_lines += record.lines;
        if record.essential {
            *essential_buckets.entry(record.bucket.clone()).or_default() += 1;
            *essential_attach.entry(record.attach.clone()).or_default() += 1;
            essential_zones.insert(record.zone);
        }
    }
    corpus.records = records.len();
    corpus.zones_with_triggers = by_zone.len();
    corpus.by_attach = by_attach;
    corpus.by_type = by_type;
    corpus.by_bucket = by_bucket;
    corpus.by_zone = by_zone;

    let mut demo_zones: Vec<u32> = crate::triage::DEMO_ZONES.to_vec();
    demo_zones.sort_unstable();
    demo_zones.dedup();

    Ok(crate::triage::TriageReport {
        schema: crate::triage::TRIAGE_SCHEMA,
        tool: format!("tbaport {}", env!("CARGO_PKG_VERSION")),
        source: options.source_root.display().to_string(),
        corpus,
        essential: crate::triage::Essential {
            count: records.iter().filter(|record| record.essential).count(),
            zones: essential_zones.into_iter().collect(),
            demo_zones,
            by_attach: essential_attach,
            by_bucket: essential_buckets,
        },
        records,
    })
}

/// Whole-world features with no schema home, recorded once per report.
///
/// These are permanent, named drops: a room coordinate Field has no tbaMUD
/// source, and the Circle combat skills are **not** a schema loss at all —
/// they are class rules implemented as pack gameplay in `mib_tba::skills`
/// (GitHub issue #25). They stay in the report so the class table's skill
/// vocabulary remains traceable, but the reason names the pack implementation
/// rather than a "non-goal".
fn global_drops() -> Vec<GlobalDrop> {
    let mut drops = vec![GlobalDrop {
        kind: "no_source_field".to_owned(),
        name: "room coord".to_owned(),
        reason: "tbaMUD `struct room_data` has no coordinates; `RoomDef::coord` is \
                     always `None`"
            .to_owned(),
    }];
    for skill in ["assist", "backstab", "bash", "consider", "kick", "rescue"] {
        drops.push(GlobalDrop {
            kind: "combat_skill".to_owned(),
            name: skill.to_owned(),
            reason: format!(
                "Circle class ability `{skill}`; implemented as tba pack gameplay \
                 (issue #25), not lost by conversion"
            ),
        });
    }
    drops.push(GlobalDrop {
        kind: "out_of_scope".to_owned(),
        name: "lib/misc/socials".to_owned(),
        reason: "legacy old-format social file; `CONFIG_NEW_SOCIALS` is YES, so the \
                 run-time reads `lib/misc/socials.new` (issue #17)"
            .to_owned(),
    });
    drops.push(GlobalDrop {
        kind: "out_of_scope".to_owned(),
        name: "lib/text/credits".to_owned(),
        reason: "the credit text is served by the engine's `credits` command from the \
                 pack's hand-authored `stdlib.credits` section (MIB #22), not as a help \
                 topic: a topic id equal to a command name is unreachable"
            .to_owned(),
    });
    drops.push(GlobalDrop {
        kind: "out_of_scope".to_owned(),
        name: "lib/text/help".to_owned(),
        reason: "legacy `help.hlp` database; the generated command catalog plus the \
                 converted `lib/text` and `lib/misc/messages` topics supersede it, \
                 except its `CIRCLEMUD` entry, which is preserved as the `circlemud` \
                 topic for the CircleMUD licence (issue #18)"
            .to_owned(),
    });
    drops
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
            keywords: Vec::new(),
            extra_flags: Vec::new(),
            affect_flags: Vec::new(),
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
    let (quests, qst) = optional_parse(&world.join("qst"), filename_number, "qst", parse_quests)?;
    files.qst = qst;

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

fn optional_scan<T>(
    dir: &Path,
    number: u32,
    ext: &str,
    scanner: fn(&mut Source) -> Result<Vec<T>>,
) -> Result<Option<Vec<T>>> {
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
        ambient_phases: BTreeMap::new(),
        ambient_weather: BTreeMap::new(),
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
    // Bits with a target: 0..10 above. Everything else is a permanent drop:
    // `RoomTag` has no corresponding variant.
    let mapped = [0u32, 2, 3, 4, 5, 8, 9, 10];
    for (index, name) in ROOM_FLAG_NAMES.iter().enumerate() {
        let index = index as u32;
        if bit(f0, index) && !mapped.contains(&index) {
            push_unmapped(
                zr,
                object,
                "room_flag",
                name,
                "permanent drop: RoomDef/RoomTag has no matching flag",
            );
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

    let especs = map_especs(&raw.especs, &object, zr);

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
    if bit(f0, 13) {
        behaviour.push(NpcBehaviour::NoCharm);
    }
    if bit(f0, 14) {
        behaviour.push(NpcBehaviour::NoSummon);
    }
    if bit(f0, 15) {
        behaviour.push(NpcBehaviour::NoSleep);
    }
    if bit(f0, 16) {
        behaviour.push(NpcBehaviour::NoBash);
    }
    if bit(f0, 17) {
        behaviour.push(NpcBehaviour::NoBlind);
    }
    if bit(f0, 18) {
        behaviour.push(NpcBehaviour::NoKill);
    }

    // Every meaningful `MOB_*` flag now has a home in `behaviour`. `MOB_SPEC`
    // (0) is a special procedure, which is a documented non-goal (#31);
    // `MOB_ISNPC` (3) and `MOB_NOTDEADYET` (19) are loader bookkeeping the
    // loader sets itself and are not reported.
    if bit(f0, 0) {
        push_unmapped(
            zr,
            &object,
            "mob_flag",
            MOB_FLAG_NAMES[0],
            "permanent drop: `MOB_SPEC` is a legacy marker; the procedure is assigned in C (`spec_assign.c`/`castle.c`), not the world file (see #31)",
        );
    }

    for (index, name) in AFF_FLAG_NAMES.iter().enumerate() {
        if bit(raw.aff_flags[0], index as u32) {
            push_unmapped(
                zr,
                &object,
                "mob_aff",
                name,
                "permanent drop: TbaNpcStats has no affect-flag field",
            );
        }
    }

    push_unmapped(
        zr,
        &object,
        "mob_sex",
        &raw.sex.to_string(),
        "permanent drop: TbaNpcStats has no sex field",
    );
    push_unmapped(
        zr,
        &object,
        "npc_keywords",
        &raw.name,
        "permanent drop: stdlib.npcs has no keyword list",
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
        damage: Some(raw.damage),
        attributes: especs.attributes,
        saves: especs.saves,
    };

    (npc, stats)
}

/// Decoded enhanced-mob e-specs (`interpret_espec`, `tbamud/src/db.c:1775`).
struct Especs {
    attributes: TbaAttributes,
    saves: TbaSaves,
}

/// Decode the `Keyword: value` lines of an enhanced (`E`) mob.
///
/// `interpret_espec` starts from [`TbaAttributes::mob_default`] (the simple-mob
/// loader's 11, `db.c:1682`) and zero saves, then clamps each value to its
/// source range. The six attributes and five saving throws have schema fields
/// ([`TbaNpcStats::attributes`] / [`TbaNpcStats::saves`]); the other two
/// keywords do not and are named drops.
fn map_especs(lines: &[String], object: &str, zr: &mut ZoneReport) -> Especs {
    let mut especs = Especs {
        attributes: TbaAttributes::mob_default(),
        saves: TbaSaves::default(),
    };
    for line in lines {
        let line = line.trim();
        let Some((keyword, value)) = line.split_once(':') else {
            push_unmapped(
                zr,
                object,
                "mob_espec",
                line,
                "permanent drop: not a `Keyword: value` e-spec; `interpret_espec` ignores it",
            );
            continue;
        };
        let value = espec_atoi(value);
        match keyword.trim().to_ascii_lowercase().as_str() {
            "str" => especs.attributes.strength = value.clamp(3, 25),
            "int" => especs.attributes.intelligence = value.clamp(3, 25),
            "wis" => especs.attributes.wisdom = value.clamp(3, 25),
            "dex" => especs.attributes.dexterity = value.clamp(3, 25),
            "con" => especs.attributes.constitution = value.clamp(3, 25),
            "cha" => especs.attributes.charisma = value.clamp(3, 25),
            "savingpara" => especs.saves.para = value.clamp(0, 100),
            "savingrod" => especs.saves.rod = value.clamp(0, 100),
            "savingpetri" => especs.saves.petri = value.clamp(0, 100),
            "savingbreath" => especs.saves.breath = value.clamp(0, 100),
            "savingspell" => especs.saves.spell = value.clamp(0, 100),
            "stradd" => push_unmapped(
                zr,
                object,
                "mob_espec",
                line,
                "permanent drop: exceptional strength (`str_add`) is not modelled",
            ),
            "barehandattack" => push_unmapped(
                zr,
                object,
                "mob_espec",
                line,
                "permanent drop: bare-hand attack is a display-only message type; damage comes from the `damage` dice",
            ),
            _ => push_unmapped(
                zr,
                object,
                "mob_espec",
                line,
                "permanent drop: unknown e-spec keyword; `interpret_espec` ignores it",
            ),
        }
    }
    especs
}

/// C `atoi` on an e-spec value: optional sign and leading digits, else 0.
fn espec_atoi(value: &str) -> i32 {
    let value = value.trim_start();
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let end = digits
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(digits.len(), |(index, _)| index);
    if end == 0 {
        return 0;
    }
    let magnitude = digits[..end].parse::<i64>().unwrap_or(i64::MAX);
    let signed = if negative { -magnitude } else { magnitude };
    signed.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
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
    let object = format!("item tba_item_{}", raw.vnum);

    let kind = map_item_kind(raw.obj_type, &object, zr);
    let slots = map_wear_slots(raw.wear_flags[0]);
    let affects = map_applies(raw, &object, zr);
    let extra_flags = map_item_extra_flags(raw.extra_flags[0]);
    let affect_flags = map_item_affect_flags(raw.affect_flags[0], &object, zr);
    let keywords = raw
        .name
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();

    // Extra flags, affect flags and the keyword list now have `tba.items`
    // fields. The action description and the `E` blocks still have no target.
    if !raw.action_description.trim().is_empty() {
        push_unmapped(
            zr,
            &object,
            "item_action_description",
            &raw.action_description,
            "permanent drop: stdlib.items has no action description",
        );
    }
    if !raw.extra_descriptions.is_empty() {
        push_unmapped(
            zr,
            &object,
            "item_extra_descriptions",
            &raw.extra_descriptions.len().to_string(),
            "permanent drop: stdlib.items has no extra-description field",
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
        keywords,
        extra_flags,
        affect_flags,
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

/// Decode the source `GET_OBJ_EXTRA` bits into the modelled [`ItemExtraFlag`]
/// order. The bit index is the `ITEM_EXTRA_NAMES` order (`structs.h:412`).
fn map_item_extra_flags(flags: u64) -> Vec<ItemExtraFlag> {
    use crate::source::tokens::bit;
    const FLAGS: [ItemExtraFlag; 18] = [
        ItemExtraFlag::Glow,
        ItemExtraFlag::Hum,
        ItemExtraFlag::NoRent,
        ItemExtraFlag::NoDonate,
        ItemExtraFlag::NoInvis,
        ItemExtraFlag::Invisible,
        ItemExtraFlag::Magic,
        ItemExtraFlag::NoDrop,
        ItemExtraFlag::Bless,
        ItemExtraFlag::AntiGood,
        ItemExtraFlag::AntiEvil,
        ItemExtraFlag::AntiNeutral,
        ItemExtraFlag::AntiMagicUser,
        ItemExtraFlag::AntiCleric,
        ItemExtraFlag::AntiThief,
        ItemExtraFlag::AntiWarrior,
        ItemExtraFlag::NoSell,
        ItemExtraFlag::Quest,
    ];
    FLAGS
        .into_iter()
        .enumerate()
        .filter(|(index, _)| bit(flags, *index as u32))
        .map(|(_, flag)| flag)
        .collect()
}

/// Decode the source `GET_OBJ_AFFECT` bits into the modelled
/// [`ItemAffectFlag`] order. The three placeholder bits (`DONTUSE`, `UNUSED`,
/// `FREE`) have no enum variant and are reported.
fn map_item_affect_flags(flags: u64, object: &str, zr: &mut ZoneReport) -> Vec<ItemAffectFlag> {
    use crate::source::tokens::bit;
    let mut out = Vec::new();
    for (index, name) in AFF_FLAG_NAMES.iter().enumerate() {
        if !bit(flags, index as u32) {
            continue;
        }
        let flag = match index {
            1 => ItemAffectFlag::Blind,
            2 => ItemAffectFlag::Invisible,
            3 => ItemAffectFlag::DetectAlign,
            4 => ItemAffectFlag::DetectInvis,
            5 => ItemAffectFlag::DetectMagic,
            6 => ItemAffectFlag::SenseLife,
            7 => ItemAffectFlag::Waterwalk,
            8 => ItemAffectFlag::Sanctuary,
            10 => ItemAffectFlag::Curse,
            11 => ItemAffectFlag::Infravision,
            12 => ItemAffectFlag::Poison,
            13 => ItemAffectFlag::ProtectEvil,
            14 => ItemAffectFlag::ProtectGood,
            15 => ItemAffectFlag::Sleep,
            16 => ItemAffectFlag::NoTrack,
            17 => ItemAffectFlag::Flying,
            18 => ItemAffectFlag::Scuba,
            19 => ItemAffectFlag::Sneak,
            20 => ItemAffectFlag::Hide,
            22 => ItemAffectFlag::Charm,
            _ => {
                push_unmapped(
                    zr,
                    object,
                    "item_affect_flag",
                    name,
                    "permanent drop: placeholder AFF_* bit has no enum variant",
                );
                continue;
            }
        };
        out.push(flag);
    }
    out
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
                &format!(
                    "modifier {}; permanent drop: AffectLocation has no such variant",
                    apply.modifier
                ),
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

/// Wrap a mapped command body with Circle's source `if_flag`.
fn zone_command(if_flag: i32, command: ZoneCommandKind) -> ZoneCommand {
    ZoneCommand {
        if_flag: if_flag != 0,
        command,
    }
}

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
                commands.push(zone_command(
                    command.if_flag,
                    ZoneCommandKind::PlaceMob {
                        mob,
                        room,
                        max_in_world: command.arg2.max(0) as u32,
                    },
                ));
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
                commands.push(zone_command(
                    command.if_flag,
                    ZoneCommandKind::DropItem {
                        item,
                        room,
                        max_in_world: command.arg2.max(0) as u32,
                    },
                ));
            }
            'G' => {
                let item = ids::item_id(command.arg1);
                if !check_item(&from, &item, items, zr, "dangling_reset_item") {
                    continue;
                }
                commands.push(zone_command(
                    command.if_flag,
                    ZoneCommandKind::GiveItem {
                        item,
                        max_in_world: command.arg2.max(0) as u32,
                    },
                ));
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
                commands.push(zone_command(
                    command.if_flag,
                    ZoneCommandKind::EquipItem {
                        item,
                        slot,
                        max_in_world: command.arg2.max(0) as u32,
                    },
                ));
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
                commands.push(zone_command(
                    command.if_flag,
                    ZoneCommandKind::PutItem {
                        item,
                        container,
                        max_in_world: command.arg2.max(0) as u32,
                    },
                ));
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
                commands.push(zone_command(
                    command.if_flag,
                    ZoneCommandKind::DoorState {
                        room,
                        exit: DIRS[command.arg2 as usize].to_owned(),
                        state,
                    },
                ));
            }
            'R' => {
                let room = ids::room_id(command.arg1);
                let item = ids::item_id(command.arg2);
                let room_ok = check(&from, &room, rooms, "dangling_reset_room", "room", zr);
                let item_ok = check_item(&from, &item, items, zr, "dangling_reset_item");
                if !(room_ok && item_ok) {
                    continue;
                }
                commands.push(zone_command(
                    command.if_flag,
                    ZoneCommandKind::RemoveItem { room, item },
                ));
            }
            // `T` and `V` attach scripts to a mob, object or room rather than
            // reset the world; `attach::fold_reset_scripts` handles them.
            'T' | 'V' => {}
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
                "permanent drop: ZoneDef has no zone-flags field",
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
    room_ids: &BTreeSet<RoomId>,
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

        // `SHOP_ROOM`: an empty list means the keeper trades wherever it
        // stands. A listed vnum with no converted room cannot be represented
        // (it would fail `tba.shops` validation), so it is dropped and named.
        let mut rooms = Vec::with_capacity(raw.rooms.len());
        for vnum in &raw.rooms {
            let room = ids::room_id(*vnum);
            if room_ids.contains(&room) {
                rooms.push(room);
            } else {
                push_dangling(zr, "dangling_shop_room", &label, room.as_ref());
            }
        }

        let buy_kinds = raw
            .buy_types
            .iter()
            .map(|entry| map_item_kind(entry.kind, &label, zr))
            .collect();
        let trade_with = map_shop_trade_with(raw.trade_with, &label, zr);

        for word in raw.buy_words() {
            push_unmapped(
                zr,
                &label,
                "shop_buy_word",
                word,
                "permanent drop: tba.shops has no per-type buy keyword",
            );
        }
        if raw.broke_temper != 0 {
            push_unmapped(
                zr,
                &label,
                "shop_broke_temper",
                &raw.broke_temper.to_string(),
                "permanent drop: no keeper-temper reaction is modelled",
            );
        }
        if raw.bitvector != 0 {
            push_unmapped(
                zr,
                &label,
                "shop_bitvector",
                &raw.bitvector.to_string(),
                "permanent drop: SHOP_* bitvector has no target field",
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
                rooms,
                trade_with,
                hours: ShopHours {
                    open1: raw.open1.max(0) as u32,
                    close1: raw.close1.max(0) as u32,
                    open2: raw.open2.max(0) as u32,
                    close2: raw.close2.max(0) as u32,
                },
                messages: raw.messages.clone(),
            },
        );
    }
}

/// Decode the `SHOP_TRADE_WITH` bits (`shop.h:78`) into the modelled
/// [`ShopTrade`] order. An unknown high bit is reported.
fn map_shop_trade_with(bits: i32, label: &str, zr: &mut ZoneReport) -> Vec<ShopTrade> {
    const TRADES: [ShopTrade; 7] = [
        ShopTrade::Good,
        ShopTrade::Evil,
        ShopTrade::Neutral,
        ShopTrade::MagicUser,
        ShopTrade::Cleric,
        ShopTrade::Thief,
        ShopTrade::Warrior,
    ];
    let mut out = Vec::new();
    for (index, trade) in TRADES.into_iter().enumerate() {
        if bits & (1 << index) != 0 {
            out.push(trade);
        }
    }
    if bits & !0x7f != 0 {
        push_unmapped(
            zr,
            label,
            "shop_trade_with",
            &bits.to_string(),
            "permanent drop: SHOP_TRADE_WITH bits above bit 6 are not modelled",
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Quests
// ---------------------------------------------------------------------------

/// Circle's `AQ_*` objective names (`quest.h:14`).
fn quest_type_name(value: i32) -> &'static str {
    match value {
        -1 => "UNDEFINED",
        0 => "OBJ_FIND",
        1 => "ROOM_FIND",
        2 => "MOB_FIND",
        3 => "MOB_KILL",
        4 => "MOB_SAVE",
        5 => "OBJ_RETURN",
        6 => "ROOM_CLEAR",
        _ => "UNKNOWN",
    }
}

/// Map one `.qst` record into `stdlib.quests`.
///
/// The objective types with a direct `QuestStepDef` are converted (fetch,
/// visit, kill); a type with no variant drops the **whole** quest through a
/// named `unmapped` entry, because a quest missing its objective would change
/// the world rather than merely omit a field. Giver and step references are
/// resolved against the converted id sets, so a dangling one drops the quest
/// and is reported. Source fields `stdlib.quests` cannot carry (`gold_reward`,
/// `desc`/`done`/`quit`, the chain links and the level/time conditions) are
/// recorded as `unmapped`.
fn map_quest(
    raw: &RawQuest,
    room_ids: &BTreeSet<RoomId>,
    npc_ids: &BTreeSet<NpcId>,
    item_ids: &BTreeSet<ItemId>,
    zr: &mut ZoneReport,
) -> Option<(QuestId, QuestDef)> {
    let label = format!("quest {}", raw.vnum);
    let giver = ids::npc_id(raw.questmaster);
    if raw.questmaster < 0 || !npc_ids.contains(&giver) {
        push_dangling(zr, "dangling_quest_giver", &label, giver.as_ref());
        return None;
    }

    let quantity = raw.values[6].max(1) as u32;
    let step = match raw.quest_type {
        // AQ_OBJ_FIND: retrieve an object.
        0 => {
            let item = ids::item_id(raw.target);
            if !item_ids.contains(&item) {
                push_dangling(zr, "dangling_quest_target_item", &label, item.as_ref());
                return None;
            }
            QuestStepDef::Collect {
                item,
                count: quantity,
            }
        }
        // AQ_ROOM_FIND: reach a room.
        1 => {
            let room = ids::room_id(raw.target);
            if !room_ids.contains(&room) {
                push_dangling(zr, "dangling_quest_target_room", &label, room.as_ref());
                return None;
            }
            QuestStepDef::Visit { room }
        }
        // AQ_MOB_KILL: kill a mob.
        3 => {
            let npc = ids::npc_id(raw.target);
            if !npc_ids.contains(&npc) {
                push_dangling(zr, "dangling_quest_target_npc", &label, npc.as_ref());
                return None;
            }
            QuestStepDef::Kill {
                npc,
                count: quantity,
            }
        }
        other => {
            push_unmapped(
                zr,
                &label,
                "quest_objective",
                quest_type_name(other),
                "no QuestStepDef variant for this AQ_* objective; the quest is dropped",
            );
            return None;
        }
    };

    let mut reward = RewardSpec::default();
    if raw.exp_reward > 0 {
        reward.xp = raw.exp_reward as u64;
    }
    if raw.obj_reward > 0 {
        let item = ids::item_id(raw.obj_reward);
        if item_ids.contains(&item) {
            reward.items.push(item);
        } else {
            push_dangling(zr, "dangling_quest_reward_item", &label, item.as_ref());
        }
    }
    if raw.gold_reward > 0 {
        push_unmapped(
            zr,
            &label,
            "quest_gold_reward",
            &raw.gold_reward.to_string(),
            "RewardSpec has no gold field (issue #16)",
        );
    }

    // `info` is the long description shown at join; `desc` is the short form.
    let description = if raw.info.trim().is_empty() {
        raw.desc.trim().to_owned()
    } else {
        raw.info.trim().to_owned()
    };
    if raw.info.trim().is_empty() && !raw.desc.trim().is_empty() {
        // `desc` stood in for `description`; nothing dropped.
    } else if !raw.desc.trim().is_empty() {
        push_unmapped(zr, &label, "quest_text", "desc", raw.desc.trim());
    }
    for (name, text) in [("done", &raw.done), ("quit", &raw.quit)] {
        if !text.trim().is_empty() {
            push_unmapped(zr, &label, "quest_text", name, text.trim());
        }
    }

    record_quest_conditions(raw, &label, zr);

    Some((
        ids::quest_id(raw.vnum),
        QuestDef {
            name: raw.name.clone(),
            description,
            giver,
            steps: vec![step],
            reward,
            origin: QuestOrigin::Authored,
        },
    ))
}

/// Record the quest fields the MIB schema has no home for.
fn record_quest_conditions(raw: &RawQuest, label: &str, zr: &mut ZoneReport) {
    if raw.flags & 1 != 0 {
        push_unmapped(
            zr,
            label,
            "quest_flag",
            "REPEATABLE",
            "QuestDef has no repeatable flag",
        );
    }
    for (name, value) in [
        ("prereq", raw.prereq),
        ("prev_quest", raw.prev_quest),
        ("next_quest", raw.next_quest),
    ] {
        if value >= 0 {
            push_unmapped(zr, label, "quest_link", name, &value.to_string());
        }
    }
    let conditions = [
        ("points", raw.values[0]),
        ("penalty", raw.values[1]),
        ("min_level", raw.values[2]),
        ("max_level", raw.values[3]),
        ("time_limit", raw.values[4]),
        ("return_mob", raw.values[5]),
    ];
    for (name, value) in conditions {
        if value != 0 && value != -1 {
            push_unmapped(zr, label, "quest_condition", name, &value.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// Help topics + socials (world-global)
// ---------------------------------------------------------------------------

/// Load `lib/text/*`, the `CIRCLEMUD` entry of `lib/text/help/help.hlp`, and
/// `lib/misc/messages` into `stdlib.help` topics.
fn load_help(source_root: &Path, bundle: &mut OutputBundle) -> Result<()> {
    let text_dir = source_root.join("lib/text");
    for (id, topic) in load_text_topics(&text_dir)? {
        bundle.help.topics.insert(SmolStr::new(id), topic);
    }
    // The CircleMUD licence requires the `CIRCLEMUD` entry to be maintained
    // intact and shown for `help circlemud`, so it is kept even though the rest
    // of the legacy `help.hlp` database is dropped.
    if let Some((id, topic)) = load_circle_help(&text_dir)? {
        bundle.help.topics.insert(SmolStr::new(id), topic);
    }
    for (id, topic) in load_message_topics(&source_root.join("lib/misc/messages"))? {
        bundle.help.topics.insert(SmolStr::new(id), topic);
    }
    Ok(())
}

/// Codes `mib_tba::socials::substitute` resolves (`perform_act`'s cases).
const SUPPORTED_SOCIAL_CODES: &str = "nNeEmMsStTpPoOaAuUF$";

/// Load `lib/misc/socials.new` into `tba.socials`, recording duplicate command
/// words and any `$`-code the engine cannot resolve as global drops.
fn load_socials(
    source_root: &Path,
    bundle: &mut OutputBundle,
    drops: &mut Vec<GlobalDrop>,
) -> Result<()> {
    let path = source_root.join("lib/misc/socials.new");
    if !path.is_file() {
        return Ok(());
    }
    let mut src = Source::from_path(&path)?;
    let raw_socials: Vec<RawSocial> = parse_socials(&mut src)?;
    let mut unsupported: BTreeSet<char> = BTreeSet::new();
    for raw in &raw_socials {
        for message in raw.messages.iter().flatten() {
            collect_unsupported_codes(message, &mut unsupported);
        }
        let name = SmolStr::new(raw.command.clone());
        if bundle
            .socials
            .socials
            .insert(name.clone(), map_social(raw))
            .is_some()
        {
            drops.push(GlobalDrop {
                kind: "duplicate_social".to_owned(),
                name: name.to_string(),
                reason: "two socials share a command word; the last one wins".to_owned(),
            });
        }
    }
    for code in unsupported {
        drops.push(GlobalDrop {
            kind: "social_code".to_owned(),
            name: format!("${code}"),
            reason: format!(
                "tbaMUD `perform_act` has no `${code}` case; the code is preserved in \
                 `tba.socials` and expands to nothing at run time"
            ),
        });
    }
    Ok(())
}

fn collect_unsupported_codes(message: &str, out: &mut BTreeSet<char>) {
    let mut chars = message.chars();
    while let Some(c) = chars.next() {
        if c == '$'
            && let Some(code) = chars.next()
            && !SUPPORTED_SOCIAL_CODES.contains(code)
        {
            out.insert(code);
        }
    }
}

fn map_social(raw: &RawSocial) -> SocialDef {
    let message = |index: usize| raw.messages.get(index).cloned().flatten();
    SocialDef {
        sort_as: SmolStr::new(raw.sort_as.clone()),
        hide: raw.hide != 0,
        min_char_position: raw.min_char_position,
        min_victim_position: raw.min_victim_position,
        min_level_char: raw.min_level_char,
        char_no_arg: message(0),
        others_no_arg: message(1),
        char_found: message(2),
        others_found: message(3),
        vict_found: message(4),
        not_found: message(5),
        char_auto: message(6),
        others_auto: message(7),
        char_body_found: message(8),
        others_body_found: message(9),
        vict_body_found: message(10),
        char_obj_found: message(11),
        others_obj_found: message(12),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Record the structured triage record of every `.trg` record in the zone, so
/// the per-zone `triggers[]` itemises the whole corpus. Every record is also
/// emitted into `tba.triggers` ([`map_triggers`]); the triage is a priority
/// ranking, not a filter.
fn add_trigger_records(zr: &mut ZoneReport, records: &[TriggerRecord]) {
    for record in records {
        zr.triggers.push(TriggerTriageRecord {
            vnum: record.vnum,
            name: record.name.clone(),
            attach: record.attach.clone(),
            flags: record.flags.clone(),
            types: record.types.clone(),
            narg: record.narg,
            bucket: record.bucket.as_str().to_owned(),
            lines: record.lines,
            raw_lines: record.raw_lines,
        });
    }
}

/// Every loaded trigger vnum with its `attach_type` (0 mob, 1 object, 2
/// world). `dg_read_trigger` skips a `T` line whose vnum is not in this set.
fn trigger_attach_types(sources: &[ZoneSource]) -> BTreeMap<i32, i32> {
    sources
        .iter()
        .flat_map(|source| &source.triggers)
        .map(|trigger| (trigger.vnum, trigger.attach_type))
        .collect()
}

/// The `attach_type` field spelling in `tba.triggers`.
fn attach_type_name(attach_type: i32) -> &'static str {
    match attach_type {
        0 => "mob",
        1 => "obj",
        2 => "wld",
        _ => "",
    }
}

/// Emit every trigger record of one zone into `tba.triggers`.
///
/// Which triggers an entity carries, and in what order, is the attachment
/// lists ([`attach`]).
fn map_triggers(source: &ZoneSource, bundle: &mut OutputBundle) {
    for trigger in &source.triggers {
        let def = TriggerDef {
            vnum: trigger.vnum,
            name: trigger.name.clone(),
            attach_type: attach_type_name(trigger.attach_type).to_owned(),
            flags: trigger.flags.clone(),
            types: trigger
                .types
                .iter()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
            narg: trigger.narg,
            arglist: trigger.arglist.clone(),
            script: trigger.body.clone(),
        };
        bundle
            .triggers
            .triggers
            .insert(TriggerId::from_vnum(trigger.vnum), def);
    }
}

/// The corpus coverage of every emitted script, from `dg_vm::analyze`.
///
/// Each script is analysed as the owner type its record was authored for. The
/// game's supported mob command words come from the NPC command interpreter's
/// `cmd_info[]` table (`mib_tba::dg::mobcmd`), so a mob line is unsupported
/// only when the interpreter does not model its first word (or does not
/// recognize it at all).
fn trigger_coverage(triggers: &TriggersFile) -> TriggerCoverage {
    let mut corpus = Corpus::default();
    let mut by_attach_type: BTreeMap<String, usize> = BTreeMap::new();
    for def in triggers.triggers.values() {
        *by_attach_type.entry(def.attach_type.clone()).or_default() += 1;
        // A record of no known owner type is analysed as a room script so its
        // lines are in the totals; its record count stays under the empty
        // attach type.
        let attach = match def.attach_type.as_str() {
            "mob" => AttachType::Mob,
            "obj" => AttachType::Obj,
            _ => AttachType::Wld,
        };
        corpus.add(def.vnum, &analyze(&def.script, attach));
    }
    let supported = mib_tba::dg::mobcmd::supported_mob_words();
    let mut coverage = TriggerCoverage::from_corpus(&corpus, by_attach_type, &supported);
    coverage.records = triggers.triggers.len();
    coverage
}

/// The attachment roll-up of the emitted lists. The mismatch, variable and
/// zone-reset counts come from the zone reports ([`DropReport::finalise`]).
fn summarise_attachments(triggers: &TriggersFile) -> TriggerAttachments {
    let mut summary = TriggerAttachments {
        owners: triggers.attachments.len(),
        ..TriggerAttachments::default()
    };
    let mut attached: BTreeSet<&TriggerId> = BTreeSet::new();
    for (owner, list) in &triggers.attachments {
        summary.links += list.len();
        *summary
            .links_by_owner_kind
            .entry(attach::owner_kind(owner).to_owned())
            .or_default() += list.len();
        attached.extend(list);
    }
    summary.unattached = triggers
        .triggers
        .iter()
        .filter(|(id, _)| !attached.contains(id))
        .map(|(_, def)| def.vnum)
        .collect();
    summary.unattached.sort_unstable();
    summary.unattached_triggers = summary.unattached.len();
    summary
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_record_of_unknown_attach_type_is_still_counted_in_the_lines() {
        let mut triggers = TriggersFile::default();
        for (vnum, attach_type, script) in [(1, "mob", "say hi"), (2, "", "wecho x\nfoo")] {
            triggers.triggers.insert(
                TriggerId::from_vnum(vnum),
                TriggerDef {
                    vnum,
                    attach_type: attach_type.to_owned(),
                    script: script.to_owned(),
                    ..TriggerDef::default()
                },
            );
        }
        let coverage = trigger_coverage(&triggers);
        assert_eq!(coverage.records, 2);
        assert_eq!(coverage.script_lines, 3);
        assert_eq!(coverage.by_attach_type[""], 1);
        assert_eq!(coverage.unknown_world_commands["foo"].vnums, [2]);
        assert_eq!(coverage.script_commands["wecho"], 1);
    }

    fn zone_report() -> ZoneReport {
        ZoneReport {
            number: 1,
            name: String::new(),
            bot: 0,
            top: 0,
            area: String::new(),
            rooms: 0,
            npcs: 0,
            items: 0,
            commands: 0,
            source_files: SourceFiles::default(),
            deferred: Vec::new(),
            dangling: Vec::new(),
            unmapped: Vec::new(),
            command_flags: Vec::new(),
            triggers: Vec::new(),
            trigger_variables: Vec::new(),
            zone_trigger_links: Vec::new(),
        }
    }

    #[test]
    fn especs_map_attributes_and_saves_and_name_the_unmapped_keys() {
        let mut zr = zone_report();
        let lines = [
            "Str: 18".to_owned(),
            "Int: 25".to_owned(),
            "SavingSpell: 2".to_owned(),
            "BareHandAttack: 12".to_owned(),
            "StrAdd: 50".to_owned(),
            "Unknown: 1".to_owned(),
            "no colon".to_owned(),
        ];
        let especs = map_especs(&lines, "npc tba_npc_1", &mut zr);

        assert_eq!(especs.attributes.strength, 18);
        assert_eq!(especs.attributes.intelligence, 25);
        assert_eq!(
            especs.attributes.dexterity, 11,
            "an absent e-spec keeps the loader's mob baseline"
        );
        assert_eq!(especs.saves.spell, 2);
        assert_eq!(especs.saves.para, 0);

        let dropped: Vec<_> = zr
            .unmapped
            .iter()
            .map(|drop| (drop.kind.as_str(), drop.name.as_str()))
            .collect();
        assert_eq!(
            dropped,
            [
                ("mob_espec", "BareHandAttack: 12"),
                ("mob_espec", "StrAdd: 50"),
                ("mob_espec", "Unknown: 1"),
                ("mob_espec", "no colon"),
            ]
        );
    }

    #[test]
    fn espec_values_are_clamped_and_parsed_like_atoi() {
        let mut zr = zone_report();
        let lines = [
            "Str: 99".to_owned(),
            "Dex: abc".to_owned(),
            "Cha: +7".to_owned(),
            "Con: -4".to_owned(),
            "SavingPara: 101".to_owned(),
        ];
        let especs = map_especs(&lines, "npc tba_npc_1", &mut zr);

        assert_eq!(especs.attributes.strength, 25, "Str clamps to 25");
        assert_eq!(
            especs.attributes.dexterity, 3,
            "atoi(abc) is 0, clamps to 3"
        );
        assert_eq!(especs.attributes.charisma, 7, "a leading + is accepted");
        assert_eq!(
            especs.attributes.constitution, 3,
            "a negative value clamps to the floor"
        );
        assert_eq!(especs.saves.para, 100);
        assert!(zr.unmapped.is_empty());
    }

    #[test]
    fn item_extra_flags_map_to_their_enum_order() {
        // ITEM_GLOW | ITEM_HUM | ITEM_NODROP = bits 0, 1, 7.
        let flags = map_item_extra_flags((1 << 0) | (1 << 1) | (1 << 7));
        assert_eq!(
            flags,
            vec![
                ItemExtraFlag::Glow,
                ItemExtraFlag::Hum,
                ItemExtraFlag::NoDrop
            ]
        );
        assert!(map_item_extra_flags(0).is_empty());
    }

    #[test]
    fn item_affect_flags_map_and_report_placeholders() {
        let mut zr = zone_report();
        // AFF_INVISIBLE (bit 2) | AFF_FLYING (bit 17).
        let flags = map_item_affect_flags((1 << 2) | (1 << 17), "item tba_item_1", &mut zr);
        assert_eq!(
            flags,
            vec![ItemAffectFlag::Invisible, ItemAffectFlag::Flying]
        );
        assert!(zr.unmapped.is_empty());

        // AFF_DONTUSE (bit 0) is a placeholder and is reported.
        let mut zr = zone_report();
        let flags = map_item_affect_flags(1 << 0, "item tba_item_1", &mut zr);
        assert!(flags.is_empty());
        assert_eq!(zr.unmapped.len(), 1);
        assert_eq!(zr.unmapped[0].kind, "item_affect_flag");
    }

    #[test]
    fn shop_trade_with_bits_map_and_report_unknown_bits() {
        let mut zr = zone_report();
        // TRADE_NOGOOD | TRADE_NOEVIL = bits 0, 1.
        let trades = map_shop_trade_with(0b000_0011, "shop tba_npc_1", &mut zr);
        assert_eq!(trades, vec![ShopTrade::Good, ShopTrade::Evil]);
        assert!(zr.unmapped.is_empty());

        // Bit 7 is not a `SHOP_TRADE_WITH` bit and is reported.
        let mut zr = zone_report();
        let trades = map_shop_trade_with(1 << 7, "shop tba_npc_1", &mut zr);
        assert!(trades.is_empty());
        assert_eq!(zr.unmapped.len(), 1);
        assert_eq!(zr.unmapped[0].kind, "shop_trade_with");
    }
}
