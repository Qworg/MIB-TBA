//! Shared harness for the DG runtime tests: a minimal app with the resources
//! and systems the runtime needs, and helpers to build a small world, define
//! trigger prototypes from inline script text, run the VM against it and
//! advance the fixed clock.
//!
//! Included by test files with `mod dg_common;`. Tests may unwrap/expect.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use dg_vm::driver::{Mode, script_driver};
use dg_vm::{BaseHost, Uid};
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::dg::host::EcsHost;
use mib_tba::dg::script::DgProtos;
use mib_tba::dg::{DgLog, with_host};
use mib_tba::prelude::*;
use mib_tba::triggers::content::TriggerDef;

/// An app with the DG runtime (resources and pulse system, no intake), the
/// pack's clock and RNG, and a 10 Hz fixed clock that only moves when a test
/// advances it.
pub fn app() -> App {
    let mut app = App::new();
    app.init_resource::<TbaRng>();
    app.init_resource::<TbaObjectClock>();
    app.init_resource::<TbaContent>();
    app.insert_resource(Time::<Fixed>::from_hz(10.0));
    app.add_message::<OutputEvent>();
    mib_tba::dg::register_runtime(&mut app);
    app
}

/// Define a trigger prototype from inline text. `attach` is `mob`, `obj` or
/// `wld`; `flags` the record's type letters.
pub fn define(app: &mut App, vnum: i32, attach: &str, flags: &str, narg: i32, script: &str) {
    let def = TriggerDef {
        vnum,
        name: format!("trigger {vnum}"),
        attach_type: attach.to_owned(),
        flags: flags.to_owned(),
        types: Vec::new(),
        narg,
        arglist: String::new(),
        script: script.to_owned(),
    };
    assert!(
        app.world_mut().resource_mut::<DgProtos>().add(&def),
        "trigger {vnum} loads"
    );
}

/// Set the triggers an owner (`npc:<id>`, `item:<id>`, `room:<key>`) carries.
pub fn attach(app: &mut App, owner: &str, vnums: &[i32]) {
    app.world_mut()
        .resource_mut::<DgProtos>()
        .set_attachments(owner, vnums.to_vec());
}

/// A room with this vnum.
pub fn room(app: &mut App, vnum: u32) -> Entity {
    app.world_mut()
        .spawn((
            Room {
                key: format!("tba.{vnum}").into(),
                name: format!("Room {vnum}"),
                description: FormattedText::default(),
                num: vnum,
                area: "test".into(),
                coord: (0, 0, 0),
                tags: BTreeSet::new(),
                media: Vec::new(),
                sector: mib_stdlib::content::Sector::Field,
                extra_descriptions: Vec::new(),
                outdoor: false,
                weather_zone: None,
                ambient: None,
                ambient_stamp: None,
                ambient_phases: Default::default(),
                ambient_weather: Default::default(),
                time: None,
                weather_state: None,
            },
            RoomState::default(),
        ))
        .id()
}

/// A playing player with this account id.
pub fn player(app: &mut App, room: Entity, name: &str, account_id: u64) -> Entity {
    app.world_mut()
        .spawn((
            Player {
                account_id,
                name: name.to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room },
            TbaProgress::new("warrior"),
        ))
        .id()
}

/// An NPC made from `tba_npc_<vnum>`.
pub fn npc(app: &mut App, room: Entity, vnum: u32, name: &str) -> Entity {
    app.world_mut()
        .spawn((
            Npc {
                name: name.to_owned(),
                persona: String::new(),
            },
            NpcKey(NpcId::from(format!("tba_npc_{vnum}"))),
            Location { room },
        ))
        .id()
}

fn item_bundle(vnum: u32, name: &str) -> Item {
    Item {
        def_id: ItemId::from(format!("tba_item_{vnum}")),
        name: name.to_owned(),
        description: String::new(),
        value: None,
        tags: Vec::new(),
    }
}

/// An item lying in a room.
pub fn item_on_ground(app: &mut App, room: Entity, vnum: u32, name: &str) -> Entity {
    app.world_mut()
        .spawn((
            item_bundle(vnum, name),
            Location { room },
            GroundItem { spawned_at: 0.0 },
        ))
        .id()
}

/// An item in someone's inventory.
pub fn item_carried(app: &mut App, holder: Entity, vnum: u32, name: &str) -> Entity {
    app.world_mut()
        .spawn((item_bundle(vnum, name), CarriedBy(holder)))
        .id()
}

/// An item worn in `slot`: `CarriedBy` the wearer and in its equipment map.
pub fn item_worn(app: &mut App, holder: Entity, slot: WearSlot, vnum: u32, name: &str) -> Entity {
    let item = item_carried(app, holder, vnum, name);
    let world = app.world_mut();
    if world.get::<TbaEquipment>(holder).is_none() {
        world.entity_mut(holder).insert(TbaEquipment::default());
    }
    world
        .get_mut::<TbaEquipment>(holder)
        .expect("equipment")
        .slots
        .insert(slot, item);
    item
}

/// An item inside a container.
pub fn item_inside(app: &mut App, container: Entity, vnum: u32, name: &str) -> Entity {
    app.world_mut()
        .spawn((item_bundle(vnum, name), ContainedBy(container)))
        .id()
}

/// Give `tba_item_<vnum>` a prototype definition.
pub fn define_item(
    app: &mut App,
    vnum: u32,
    kind: TbaItemKind,
    slots: &[WearSlot],
    keywords: &[&str],
) {
    app.world_mut().resource_mut::<TbaContent>().items.insert(
        ItemId::from(format!("tba_item_{vnum}")),
        TbaItemDef {
            keywords: keywords.iter().map(|word| (*word).to_owned()).collect(),
            extra_flags: Vec::new(),
            affect_flags: Vec::new(),
            kind,
            slots: slots.to_vec(),
            values: [0; 4],
            affects: Vec::new(),
            weight: 1,
            cost: 0,
            rent: 0,
            min_level: 0,
            timer: None,
        },
    );
}

/// Give `tba_npc_<vnum>` mob statistics of this level.
pub fn define_npc_level(app: &mut App, vnum: u32, level: u32) {
    app.world_mut()
        .resource_mut::<TbaContent>()
        .npc_stats
        .insert(
            NpcId::from(format!("tba_npc_{vnum}")),
            TbaNpcStats {
                hp: "1d1+0".parse().expect("dice"),
                level,
                hitroll: 0,
                damroll: 0,
                armor_class: 0,
                thaco: 0,
                alignment: 0,
                gold: 0,
                experience: 0,
                default_position: Position::Standing,
                behaviour: Vec::new(),
                damage: None,
                attributes: TbaAttributes::mob_default(),
                saves: TbaSaves::default(),
                spec_proc: None,
            },
        );
}

/// Run `f` against the world as a DG host.
pub fn run<R>(app: &mut App, f: impl FnOnce(&mut EcsHost<'_>) -> R) -> R {
    with_host(app.world_mut(), f)
}

/// The script id of an entity, after making sure it has one.
pub fn uid(app: &mut App, entity: Entity) -> Uid {
    run(app, |host| host.uid(entity)).expect("entity has a script id")
}

/// Advance the fixed clock and run one `FixedUpdate`, as the engine does.
pub fn tick(app: &mut App, delta: Duration) {
    app.world_mut()
        .resource_mut::<Time<Fixed>>()
        .advance_by(delta);
    app.world_mut().run_schedule(FixedUpdate);
    app.world_mut().flush();
}

/// [`tick`] by whole pulses (a tenth of a second each).
pub fn tick_pulses(app: &mut App, pulses: u64) {
    tick(app, Duration::from_millis(100 * pulses));
}

/// Run the owner's trigger with this vnum from the start, as an event
/// dispatcher would, and return its return value.
pub fn run_trigger(app: &mut App, owner: Uid, vnum: i32) -> i32 {
    run(app, |host| {
        let id = host
            .script(owner)
            .and_then(|script| {
                script
                    .triggers
                    .iter()
                    .find(|trigger| trigger.proto.vnum == vnum)
                    .map(|trigger| trigger.id)
            })
            .expect("trigger attached");
        script_driver(host, owner, id, Mode::New)
    })
}

/// A global variable of an entity's script (context any).
pub fn global(app: &mut App, owner: Uid, name: &str) -> Option<String> {
    run(app, |host| {
        host.script(owner)
            .and_then(|script| script.global_vars.get(name))
            .map(|var| var.value.clone())
    })
}

/// The vnums of an entity's attached triggers, in order.
pub fn trigger_vnums(app: &mut App, owner: Uid) -> Vec<i32> {
    run(app, |host| {
        host.script(owner)
            .map(|script| {
                script
                    .triggers
                    .iter()
                    .map(|trigger| trigger.proto.vnum)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Drain the text the game sent to sessions.
pub fn output(app: &mut App) -> Vec<String> {
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .filter_map(|event| match event.payload {
            OutboundPayload::Text { body, .. } | OutboundPayload::Error { body, .. } => {
                Some(body.render_plain())
            }
            _ => None,
        })
        .collect()
}

/// The recent script log lines, oldest first.
pub fn log_lines(app: &App) -> Vec<String> {
    app.world()
        .resource::<DgLog>()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Drain the quest events written so far as `(player, flag)` pairs.
pub fn quest_flags(app: &mut App) -> Vec<(Entity, String)> {
    app.world_mut()
        .resource_mut::<Messages<QuestEvent>>()
        .drain()
        .filter_map(|event| match event {
            QuestEvent::FlagSet { player, flag } => Some((player, flag.to_string())),
            _ => None,
        })
        .collect()
}
