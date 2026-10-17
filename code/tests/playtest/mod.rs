//! A playtest harness: boot the real `stdlib` + `tba` packs under the headless
//! bridge (no sockets) and drive a registered character's commands through the
//! engine router and every intake, the way a game binary does.
//!
//! Included by test files with `mod playtest;`. Tests may unwrap/expect.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dg_vm::BaseHost;
use mib_bridge::prelude::*;
use mib_content::prelude::{MibModule, SectionRegistry, registry_for};
use mib_core::prelude::*;
use mib_protocol::command::ExitKey;
use mib_stdlib::prelude::*;
use mib_tba::combat::TbaRound;
use mib_tba::dg::intake::movement::DgLastRoom;
use mib_tba::dg::script::DgProtos;
use mib_tba::dg::{DgLog, with_host};
use mib_tba::prelude::*;
use mib_tba::triggers::content::TriggerDef;

/// The engine's packs, in the sibling MIB checkout: `code/` → repo → `MIB/packs`.
fn engine_pack(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../MIB/packs")
        .join(name)
}

/// This repository is the pack; `code/` sits one level below the pack root.
fn this_pack() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The real content installed, without a bridge: `StdlibPlugin` + `TbaPlugin`
/// and this pack's roots, with `Startup` already run. For tests that need the
/// world and the DG runtime but no router.
pub fn content_app() -> App {
    let roots = [engine_pack("stdlib"), this_pack()];
    let mut app = App::new();
    app.add_plugins(StdlibPlugin);
    app.add_plugins(TbaPlugin);
    app.insert_resource(Modules::new(vec![
        Box::new(StdlibModule),
        Box::new(TbaModule),
    ]));
    app.insert_resource(ContentRegistry(registry()));
    let mut config = Config::default();
    config.game.packs = roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    app.insert_resource(config);
    app.world_mut().run_schedule(Startup);
    app
}

// -- synthetic world ---------------------------------------------------------

/// A room with this vnum.
pub fn spawn_room(app: &mut App, vnum: u32) -> Entity {
    app.world_mut()
        .spawn((
            Room {
                key: format!("tba.{vnum}").into(),
                name: format!("Room {vnum}"),
                description: FormattedText::default(),
                num: vnum,
                area: "test".into(),
                coord: (0, 0, 0),
                tags: std::collections::BTreeSet::new(),
                media: Vec::new(),
                sector: mib_stdlib::content::Sector::Field,
                extra_descriptions: Vec::new(),
            },
            RoomState::default(),
        ))
        .id()
}

/// A playing player in a room.
pub fn spawn_player(app: &mut App, room: Entity, name: &str) -> Entity {
    app.world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: name.to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room },
        ))
        .id()
}

/// An NPC made from a `tba_npc_<vnum>` prototype id.
pub fn spawn_npc(app: &mut App, room: Entity, npc_id: &str) -> Entity {
    app.world_mut()
        .spawn((
            Npc {
                name: npc_id.to_owned(),
                persona: String::new(),
            },
            NpcKey(NpcId::from(npc_id)),
            Location { room },
        ))
        .id()
}

/// An item made from a `tba_item_<vnum>` prototype id.
pub fn spawn_item(app: &mut App, room: Entity, item_id: &str) -> Entity {
    app.world_mut()
        .spawn((
            Item {
                def_id: ItemId::from(item_id),
                name: item_id.to_owned(),
                description: String::new(),
                value: None,
                tags: Vec::new(),
            },
            Location { room },
        ))
        .id()
}

/// The registry a `stdlib` + `tba` build links.
fn registry() -> SectionRegistry {
    let modules: [&dyn MibModule; 2] = [&StdlibModule, &TbaModule];
    registry_for(&modules)
}

/// The real composition: bridge + stdlib + tba, with this pack's roots.
fn compose(bevy: BridgeBevy) -> App {
    let roots = [engine_pack("stdlib"), this_pack()];
    let mut app = App::new();
    app.add_plugins(BridgePlugin::new(bevy));
    app.add_plugins(StdlibPlugin);
    app.add_plugins(TbaPlugin);
    // A headless app has no `TimePlugin`; the fixed clock drives the world.
    app.insert_resource(Time::<Fixed>::from_hz(10.0));
    app.insert_resource(Modules::new(vec![
        Box::new(StdlibModule),
        Box::new(TbaModule),
    ]));
    app.insert_resource(ContentRegistry(registry()));
    app.insert_resource(NpcProvider(ProviderId::Mock));
    let mut config = Config::default();
    config.game.packs = roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    app.insert_resource(config);
    app
}

/// A running game with one registered character.
pub struct Play {
    pub bridge: Arc<Bridge>,
    pub app: App,
    pub handle: SessionHandle,
    rx: tokio::sync::mpsc::Receiver<OutboundFrame>,
}

impl Play {
    /// Start the bridge and register a character named `name`.
    pub fn new(name: &str) -> Self {
        let (bridge, bevy, _feed) = Bridge::start().expect("bridge start");
        let app = compose(bevy);
        let (handle, rx) = bridge
            .block_on(
                bridge.new_session(FrontendKind::Telnet, FrontendCapabilities::telnet_default()),
            )
            .expect("open session");
        let mut play = Self {
            bridge,
            app,
            handle,
            rx,
        };
        // The account store is shared across bridges in one process, so the
        // username must be unique per test.
        static NEXT_ACCOUNT: AtomicU64 = AtomicU64::new(0);
        let user = format!(
            "tester{}x{}",
            std::process::id(),
            NEXT_ACCOUNT.fetch_add(1, Ordering::Relaxed)
        );
        play.say_input(PlayerInput::RawLine(user));
        play.say_input(PlayerInput::RawLine("password".to_owned()));
        play.say_input(PlayerInput::RawLine(name.to_owned()));
        // Drain the registration frames (MOTD, room, prompt).
        let _ = play.frames();
        // One fixed tick runs the login triggers and marks the player in-game
        // so the movement intake sees it.
        play.tick(0.1);
        let _ = play.frames();
        play
    }

    fn input(&mut self, input: PlayerInput) {
        self.bridge
            .block_on(self.handle.input(input))
            .expect("send input");
        self.app.update();
        self.app.world_mut().flush();
    }

    fn say_input(&mut self, input: PlayerInput) {
        self.bridge
            .block_on(self.handle.input(input))
            .expect("send input");
        // Account hashing runs on workers; pump until it settles.
        pump_account_jobs(&mut self.app);
    }

    /// Send a raw line as the client typed it: the engine's parser, the router
    /// and every intake.
    pub fn send(&mut self, line: &str) -> Vec<String> {
        self.input(PlayerInput::RawLine(line.to_owned()));
        self.text()
    }

    /// Send a structured command.
    pub fn send_command(&mut self, command: Command) -> Vec<String> {
        self.input(PlayerInput::Command(command));
        self.text()
    }

    /// Drain all frames delivered so far.
    pub fn frames(&mut self) -> Vec<OutboundFrame> {
        let mut frames = Vec::new();
        while let Ok(frame) = self.rx.try_recv() {
            frames.push(frame);
        }
        frames
    }

    /// The plain text of the frames delivered so far.
    pub fn text(&mut self) -> Vec<String> {
        self.frames()
            .into_iter()
            .filter_map(|frame| match frame {
                OutboundFrame::Output {
                    payload:
                        OutboundPayload::Text { body, .. } | OutboundPayload::Error { body, .. },
                } => Some(body.render_plain()),
                _ => None,
            })
            .collect()
    }

    /// Advance the fixed clock, run one `FixedUpdate`, then run `Update` so the
    /// bridge delivers anything the tick produced.
    pub fn tick(&mut self, seconds: f32) {
        let delta = std::time::Duration::from_secs_f32(seconds);
        self.app
            .world_mut()
            .resource_mut::<Time<Fixed>>()
            .advance_by(delta);
        self.app.world_mut().run_schedule(FixedUpdate);
        self.app.update();
        self.app.world_mut().flush();
    }

    /// The session entity the bridge drives.
    pub fn session(&self) -> Entity {
        // The bridge keeps the session entity; find it by the player's name.
        let mut query = self
            .app
            .world()
            .try_query::<(Entity, &Player)>()
            .expect("players query");
        query
            .iter(self.app.world())
            .map(|(entity, _)| entity)
            .next()
            .expect("a registered player")
    }

    /// The room the character stands in.
    pub fn room(&self) -> Entity {
        self.app
            .world()
            .get::<Location>(self.session())
            .expect("the character has a room")
            .room
    }

    /// The vnum of a room entity.
    pub fn vnum(&self, room: Entity) -> u32 {
        self.app.world().get::<Room>(room).expect("a room").num
    }

    /// The room entity for a `tba.<vnum>` key.
    pub fn room_by_key(&self, key: &str) -> Entity {
        let index = self.app.world().resource::<RoomIndex>();
        *index
            .map
            .get(&RoomId::from(key))
            .unwrap_or_else(|| panic!("room {key} exists"))
    }

    /// The first NPC in a room whose name matches `name` (case-insensitive).
    pub fn find_npc(&self, room: Entity, name: &str) -> Option<Entity> {
        let mut query = self.app.world().try_query::<(Entity, &Npc, &Location)>()?;
        query
            .iter(self.app.world())
            .find(|(_, npc, location)| location.room == room && npc.name.eq_ignore_ascii_case(name))
            .map(|(entity, _, _)| entity)
    }

    /// The recent script-log lines.
    pub fn log(&self) -> Vec<String> {
        self.app
            .world()
            .resource::<DgLog>()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Define a trigger prototype from inline text, as `dg_common::define`.
    pub fn define_trigger(
        &mut self,
        vnum: i32,
        attach: &str,
        flags: &str,
        narg: i32,
        arglist: &str,
        script: &str,
    ) {
        let def = TriggerDef {
            vnum,
            name: format!("trigger {vnum}"),
            attach_type: attach.to_owned(),
            flags: flags.to_owned(),
            types: Vec::new(),
            narg,
            arglist: arglist.to_owned(),
            script: script.to_owned(),
        };
        assert!(
            self.app.world_mut().resource_mut::<DgProtos>().add(&def),
            "trigger {vnum} loads"
        );
    }

    /// Set the triggers an owner (`npc:<id>`, `item:<id>`, `room:<key>`) runs.
    pub fn attach(&mut self, owner: &str, vnums: &[i32]) {
        self.app
            .world_mut()
            .resource_mut::<DgProtos>()
            .set_attachments(owner, vnums.to_vec());
    }

    /// Spawn an NPC from a `tba_npc_<vnum>` prototype id in a room.
    pub fn spawn_npc(&mut self, room: Entity, npc_id: &str) -> Entity {
        spawn_npc(&mut self.app, room, npc_id)
    }

    /// Spawn a room with this vnum.
    pub fn spawn_room(&mut self, vnum: u32) -> Entity {
        spawn_room(&mut self.app, vnum)
    }

    /// Link `from` to `to` along `key`.
    pub fn link(&mut self, from: Entity, key: ExitKey, to: Entity) {
        let world = self.app.world_mut();
        if world.get::<Exits>(from).is_none() {
            world.entity_mut(from).insert(Exits::default());
        }
        world
            .get_mut::<Exits>(from)
            .expect("exits")
            .map
            .insert(key, to);
    }

    /// Put the character in `room` without a walk (a script move).
    pub fn move_player(&mut self, room: Entity) {
        let session = self.session();
        self.app
            .world_mut()
            .entity_mut(session)
            .insert(Location { room });
        self.app
            .world_mut()
            .entity_mut(session)
            .insert(DgLastRoom(room));
    }

    /// Run one combat round (`TbaRound`), then `Update` so the bridge delivers
    /// what the round produced.
    pub fn round(&mut self) {
        self.app.world_mut().run_schedule(TbaRound);
        self.app.update();
        self.app.world_mut().flush();
    }

    /// The character's persisted remote variables.
    pub fn remote_vars(&self) -> std::collections::BTreeMap<String, String> {
        self.app
            .world()
            .get::<TbaTriggerVars>(self.session())
            .map(|vars| vars.remote.clone())
            .unwrap_or_default()
    }

    /// The room an entity stands in.
    pub fn location(&self, entity: Entity) -> Option<Entity> {
        self.app.world().get::<Location>(entity).map(|l| l.room)
    }

    /// The trigger vnums attached to an entity's script.
    pub fn trigger_vnums(&mut self, entity: Entity) -> Vec<i32> {
        with_host(self.app.world_mut(), |host| {
            let Some(uid) = host.uid(entity) else {
                return Vec::new();
            };
            host.script(uid)
                .map(|script| script.triggers.iter().map(|t| t.proto.vnum).collect())
                .unwrap_or_default()
        })
    }

    /// A global variable of an entity's script.
    pub fn entity_global(&mut self, entity: Entity, name: &str) -> Option<String> {
        with_host(self.app.world_mut(), |host| {
            let uid = host.uid(entity)?;
            let script = host.script(uid)?;
            script
                .global_vars
                .vars
                .iter()
                .find(|var| var.name == name)
                .map(|var| var.value.clone())
        })
    }
}
