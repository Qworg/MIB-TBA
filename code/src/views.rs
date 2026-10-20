//! Domain views — the queries a gameplay system reads together, bundled.
//!
//! Bevy gives a system sixteen parameters. The pack's command handlers are
//! *join points* (the router, the content catalog, class gating, combat state,
//! kill events, DG claim state and output all meet in one), so they run close
//! to that ceiling: the widest were 15, 15, 13, 12, 12 and 11.
//!
//! The width is not fifteen distinct dependencies. It is a handful of query
//! *shapes* re-declared in each handler — `Query<&Session>` ten times,
//! `Query<(Entity, &Player, &Location)>` six, `Query<&mut TbaCombatant>` four —
//! so a Bevy parameter (a borrow declaration) is spent restating the same
//! shape rather than naming a new dependency.
//!
//! A view groups the shapes a system actually uses together. A system that
//! reads *k* of a view's queries spends one parameter instead of *k*, and
//! reaches the data by name (`view.players.get(e)`) rather than by position in
//! a tuple. A view is opt-in per field: a system that needs one query from a
//! view still spends one parameter, so nothing is lost by adopting one.
//!
//! The rule of thumb: **a new system uses the view when it needs two or more
//! of its queries.** Below that, one plain query is clearer than a bundle.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use mib_stdlib::prelude::{CarriedBy, Item, Location, Npc, NpcKey, Player, SaveId, Session};

use crate::combat::TbaProgress;

/// The acting character: whether the session is playing, who the player is,
/// where they stand, and their class and level.
///
/// This is the "who is doing this" half of a command handler. It is the most
/// widely shared group of queries in the pack: ten of the command handlers
/// read the session state and six read the player and their room.
#[derive(SystemParam)]
pub struct ActorView<'w, 's> {
    /// Whether a session is in the game (`Session::state`).
    pub sessions: Query<'w, 's, &'static Session>,
    /// The player entity, their name and their room.
    pub players: Query<'w, 's, (Entity, &'static Player, &'static Location)>,
    /// The character's class and level.
    pub progress: Query<'w, 's, &'static TbaProgress>,
}

/// The non-player characters in a room: their identity, their room and their
/// prototype.
///
/// A combat or spell handler needs all three to resolve a target and to look
/// up a prototype's behaviour; keeping them together means the target
/// resolver and the handler read the same view.
#[derive(SystemParam)]
pub struct NpcView<'w, 's> {
    /// The NPC entity, its display name and its room.
    pub npcs: Query<'w, 's, (Entity, &'static Npc, &'static Location)>,
    /// The prototype an NPC was instantiated from.
    pub keys: Query<'w, 's, &'static NpcKey>,
}

/// The items in play: the ones a character carries, the prototypes they come
/// from, and their durable ids.
///
/// The equipment handler reads all three to resolve a worn item, find its
/// prototype and locate its saved document; the social and shop handlers read
/// the carried set.
#[derive(SystemParam)]
pub struct ItemView<'w, 's> {
    /// The carried item entity, its prototype and its holder.
    pub carried: Query<'w, 's, (Entity, &'static Item, &'static CarriedBy)>,
    /// The item prototypes, keyed by the content id.
    pub prototypes: Query<'w, 's, &'static Item>,
    /// The durable id of each persistent entity.
    pub save_ids: Query<'w, 's, &'static SaveId>,
}
