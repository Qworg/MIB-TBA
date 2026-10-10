//! TBA parties/groups and `follow` (issue #20).
//!
//! CircleMUD models a group as a small shared membership list with a leader
//! (`group_data` in `tbamud/src/structs.h`): `group <player>` adds a same-room
//! player, `group` lists the members, `group leave`/`disband` removes, `gtell`
//! group-says to every member, and `follow` drags a follower after its leader.
//! This module reproduces that surface on top of MIB's raw-line command idiom
//! (`bridge` genre commands run after `route_commands`; see `socials.rs`).
//!
//! ## Model: a resource, not a component
//!
//! [`TbaParties`] is a **resource** holding [`TbaParty`] values. The choice is
//! deliberate: a party is a small shared graph, so membership has to stay
//! consistent across every member entity on every join/leave/leader-transfer.
//! A per-player `TbaParty` component would require N point-edits per mutation
//! and a second authority for the leader; one resource makes membership a
//! single atomic edit and gives the reward pass one lookup. `follow` **is** a
//! per-entity component ([`TbaFollowing`]) because it is private to the
//! follower (who to chase, where the leader was last seen).
//!
//! ## XP and gold
//!
//! [`award_kill_rewards`](crate::combat::award_kill_rewards) consults
//! [`TbaParties`] and splits the defeated NPC's experience among the killer and
//! the party members **present in the killer's room**; see the split rule in
//! `combat/rewards.rs`. Gold stays with the killer (Circle's group code splits
//! only XP; `group_gain`, `tbamud/src/fight.c:353`).
//!
//! ## Scope
//!
//! Mail and the friends list are MIB engine features (D-06/D-07) and are **out
//! of scope** here: they are separate persistence-backed subsystems, not part
//! of the Circle group model. They are recorded as deferred by name rather than
//! stubbed.

use std::collections::{BTreeSet, HashMap};

use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{Location, Player, Session, SessionState};

use crate::combat::commands::{deny, text};

/// A party: a leader and the full member set (the leader is always a member).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TbaParty {
    /// The member every join/leave decision is checked against.
    pub leader: Entity,
    /// All members, including the leader.
    pub members: BTreeSet<Entity>,
}

/// Every live party. See the module docs for why this is a resource.
#[derive(Resource, Debug, Default)]
pub struct TbaParties {
    parties: Vec<TbaParty>,
}

/// What happened when a member left a party.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaveOutcome {
    /// The last member left, so the party was removed.
    Disbanded,
    /// Members remain; `new_leader` is `Some` when leadership transferred.
    Left {
        new_leader: Option<Entity>,
        remaining: Vec<Entity>,
    },
}

impl TbaParties {
    /// The live parties, in creation order.
    pub fn parties(&self) -> &[TbaParty] {
        &self.parties
    }

    /// Is there no party at all?
    pub fn is_empty(&self) -> bool {
        self.parties.is_empty()
    }

    /// The party `entity` belongs to, if any.
    pub fn party_of(&self, entity: Entity) -> Option<&TbaParty> {
        self.parties
            .iter()
            .find(|party| party.members.contains(&entity))
    }

    /// Index of `entity`'s party, if any.
    pub fn index_of(&self, entity: Entity) -> Option<usize> {
        self.parties
            .iter()
            .position(|party| party.members.contains(&entity))
    }

    /// Is `entity` in any party?
    pub fn in_party(&self, entity: Entity) -> bool {
        self.party_of(entity).is_some()
    }

    /// Are `a` and `b` members of the same party?
    pub fn same_party(&self, a: Entity, b: Entity) -> bool {
        match (self.index_of(a), self.index_of(b)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// The members of `entity`'s party, or `None` when it has none.
    pub fn members_of(&self, entity: Entity) -> Option<Vec<Entity>> {
        self.party_of(entity)
            .map(|party| party.members.iter().copied().collect())
    }

    /// Create a party led by `leader`, containing only `leader`. Returns
    /// `false` (a no-op) when `leader` is already in a party.
    pub fn create(&mut self, leader: Entity) -> bool {
        if self.in_party(leader) {
            return false;
        }
        self.parties.push(TbaParty {
            leader,
            members: BTreeSet::from([leader]),
        });
        true
    }

    /// Add `member` to `leader`'s party. `leader` must lead a party and
    /// `member` must not already be in one; otherwise this is a no-op and
    /// returns `false`.
    pub fn add_member(&mut self, leader: Entity, member: Entity) -> bool {
        if self.in_party(member) {
            return false;
        }
        let Some(party) = self.parties.iter_mut().find(|party| party.leader == leader) else {
            return false;
        };
        party.members.insert(member);
        true
    }

    /// Add `member` to the party that `host` belongs to (Circle's `group
    /// join`). `host` may be any member; the party's leader is unchanged.
    pub fn join(&mut self, member: Entity, host: Entity) -> bool {
        if self.in_party(member) {
            return false;
        }
        let Some(index) = self.index_of(host) else {
            return false;
        };
        self.parties[index].members.insert(member);
        true
    }

    /// Remove `entity` from its party, transferring leadership to the lowest
    /// member id when the leader leaves and deleting the party when empty.
    pub fn remove(&mut self, entity: Entity) -> Option<LeaveOutcome> {
        let index = self.index_of(entity)?;
        let party = &mut self.parties[index];
        party.members.remove(&entity);
        if party.members.is_empty() {
            self.parties.remove(index);
            return Some(LeaveOutcome::Disbanded);
        }
        let mut new_leader = None;
        if party.leader == entity
            && let Some(&next) = party.members.iter().next()
        {
            party.leader = next;
            new_leader = Some(next);
        }
        Some(LeaveOutcome::Left {
            new_leader,
            remaining: party.members.iter().copied().collect(),
        })
    }

    /// Delete the party led by `leader`, returning its members. `None` when
    /// `leader` does not lead a party.
    pub fn disband(&mut self, leader: Entity) -> Option<Vec<Entity>> {
        let index = self
            .parties
            .iter()
            .position(|party| party.leader == leader)?;
        let party = self.parties.remove(index);
        Some(party.members.into_iter().collect())
    }
}

/// `follow <target>` state on the follower entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component)]
pub struct TbaFollowing {
    /// The entity being followed.
    pub leader: Entity,
    /// The room the leader was last known to occupy; a follower is only moved
    /// when it is standing where the leader just was.
    pub last_room: Entity,
}

/// One parsed party/follow command, before the target is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartyCommand {
    /// `group` — list the actor's group.
    List,
    /// `group <player>` (or `group new` when `target` is empty).
    Form { target: String },
    /// `group leave`.
    Leave,
    /// `group disband`.
    Disband,
    /// `gtell <msg>` / `; <msg>`.
    Tell { message: String },
    /// `follow [<player>]`; an empty target asks/answers who is followed.
    Follow { target: String },
    /// `unfollow`.
    Unfollow,
}

/// Parse a raw line as a party/follow command, or `None` when it is not one.
///
/// `;` is the trivial group-say shorthand (Circle's group-tell), so
/// `; hello` is `gtell hello`.
pub fn parse_party_line(line: &str) -> Option<PartyCommand> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix(';') {
        return Some(PartyCommand::Tell {
            message: rest.trim().to_owned(),
        });
    }
    let (verb, rest) = split_first_word(line);
    match verb.to_ascii_lowercase().as_str() {
        "gtell" | "gt" => Some(PartyCommand::Tell {
            message: rest.trim().to_owned(),
        }),
        "unfollow" => Some(PartyCommand::Unfollow),
        "follow" => Some(PartyCommand::Follow {
            target: rest.trim().to_owned(),
        }),
        "group" => {
            let rest = rest.trim();
            if rest.is_empty() {
                return Some(PartyCommand::List);
            }
            let (sub, _arg) = split_first_word(rest);
            match sub.to_ascii_lowercase().as_str() {
                "list" => Some(PartyCommand::List),
                "leave" => Some(PartyCommand::Leave),
                "disband" => Some(PartyCommand::Disband),
                "new" => Some(PartyCommand::Form {
                    target: String::new(),
                }),
                _ => Some(PartyCommand::Form {
                    target: rest.to_owned(),
                }),
            }
        }
        _ => None,
    }
}

fn split_first_word(line: &str) -> (&str, &str) {
    match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index + 1..].trim()),
        None => (line, ""),
    }
}

/// The display name of a party member (or `someone` when it is off-line).
fn player_name(entity: Entity, players: &Query<(Entity, &Player, &Location)>) -> String {
    players
        .iter()
        .find(|(candidate, _, _)| *candidate == entity)
        .map(|(_, player, _)| player.name.clone())
        .unwrap_or_else(|| "someone".to_owned())
}

/// Find a same-room player whose name contains `needle` (case-insensitive),
/// excluding `actor`.
fn find_player_in_room(
    actor: Entity,
    room: Entity,
    needle: &str,
    players: &Query<(Entity, &Player, &Location)>,
) -> Option<(Entity, String)> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    players
        .iter()
        .find(|(entity, player, location)| {
            *entity != actor
                && location.room == room
                && player.name.to_lowercase().contains(&needle)
        })
        .map(|(entity, player, _)| (entity, player.name.clone()))
}

/// Would making `actor` follow `target` close a follow cycle?
fn would_loop(chain: &HashMap<Entity, Entity>, actor: Entity, target: Entity) -> bool {
    let mut cursor = target;
    for _ in 0..=chain.len() {
        match chain.get(&cursor) {
            Some(&next) if next == actor => return true,
            Some(&next) => cursor = next,
            None => return false,
        }
    }
    // A chain longer than the map means it already contains a cycle.
    true
}

/// `BridgeSet::Game`: translate the party and `follow` raw lines into state.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_party_commands(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    players: Query<(Entity, &Player, &Location)>,
    mut parties: ResMut<TbaParties>,
    mut followers: Query<(Entity, &mut TbaFollowing)>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    for msg in reader.read() {
        let session = msg.session;
        let playing = sessions
            .get(session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        let PlayerInput::RawLine(line) = &msg.input else {
            continue;
        };
        let Some(command) = parse_party_line(line) else {
            continue;
        };
        let Some(room) = players
            .get(session)
            .ok()
            .map(|(_, _, location)| location.room)
        else {
            continue;
        };
        let actor_name = player_name(session, &players);
        match command {
            PartyCommand::List => {
                match parties.party_of(session) {
                    None => {
                        output.write(text(session, "You are not a member of a group."));
                    }
                    Some(party) => {
                        let mut listing = String::from("Your group consists of:\n");
                        let members: Vec<Entity> = party.members.iter().copied().collect();
                        let leader = party.leader;
                        for member in members {
                            let name = player_name(member, &players);
                            let leader_mark = if member == leader { " (leader)" } else { "" };
                            listing.push_str(&format!("  {name}{leader_mark}\n"));
                        }
                        output.write(text(session, listing.trim_end().to_owned()));
                    }
                }
                write_prompt(&mut output, session);
            }
            PartyCommand::Form { target } if target.is_empty() => {
                if parties.create(session) {
                    output.write(text(session, "You have formed a group."));
                } else {
                    deny(&mut output, session, "You are already in a group.");
                }
                write_prompt(&mut output, session);
            }
            PartyCommand::Form { target } => {
                let Some((target_entity, target_name)) =
                    find_player_in_room(session, room, &target, &players)
                else {
                    deny(&mut output, session, "There is no such player here.");
                    continue;
                };
                if target_entity == session {
                    deny(&mut output, session, "You can't group with yourself.");
                    continue;
                }
                let actor_party = parties.index_of(session);
                let target_party = parties.index_of(target_entity);
                match (actor_party, target_party) {
                    (Some(a), Some(t)) if a == t => {
                        deny(
                            &mut output,
                            session,
                            format!("You and {target_name} are already in the same group."),
                        );
                    }
                    (Some(a), _) => {
                        if parties.parties()[a].leader != session {
                            deny(
                                &mut output,
                                session,
                                "Only the group's leader can add members.",
                            );
                        } else if target_party.is_some() {
                            deny(
                                &mut output,
                                session,
                                format!("{target_name} is already in a group."),
                            );
                        } else {
                            let existing: Vec<Entity> =
                                parties.members_of(session).unwrap_or_default();
                            parties.add_member(session, target_entity);
                            output.write(text(session, format!("{target_name} joins your group.")));
                            output.write(text(
                                target_entity,
                                format!("You have joined {actor_name}'s group."),
                            ));
                            for member in existing {
                                if member != session {
                                    output.write(text(
                                        member,
                                        format!("{target_name} joins the group."),
                                    ));
                                }
                            }
                        }
                    }
                    (None, Some(t)) => {
                        let leader = parties.parties()[t].leader;
                        let existing: Vec<Entity> = parties.members_of(leader).unwrap_or_default();
                        parties.join(session, leader);
                        output.write(text(
                            session,
                            format!("You have joined {target_name}'s group."),
                        ));
                        for member in existing {
                            if member != target_entity {
                                output.write(text(
                                    member,
                                    format!("{actor_name} has joined the group."),
                                ));
                            }
                        }
                    }
                    (None, None) => {
                        parties.create(session);
                        parties.join(target_entity, session);
                        output.write(text(session, "You have formed a group."));
                        output.write(text(
                            target_entity,
                            format!("You have joined {actor_name}'s group."),
                        ));
                    }
                }
                write_prompt(&mut output, session);
            }
            PartyCommand::Leave => match parties.remove(session) {
                None => {
                    deny(&mut output, session, "But you aren't part of a group!");
                }
                Some(LeaveOutcome::Disbanded) => {
                    output.write(text(session, "You have left the group."));
                }
                Some(LeaveOutcome::Left {
                    new_leader,
                    remaining,
                }) => {
                    for member in remaining {
                        output.write(text(member, format!("{actor_name} has left the group.")));
                    }
                    if let Some(new_leader) = new_leader {
                        let name = player_name(new_leader, &players);
                        for member in parties.members_of(new_leader).unwrap_or_default() {
                            output.write(text(
                                member,
                                format!("{name} has assumed leadership of the group."),
                            ));
                        }
                    }
                    output.write(text(session, "You have left the group."));
                }
            },
            PartyCommand::Disband => {
                let is_leader = parties
                    .party_of(session)
                    .map(|party| party.leader == session)
                    .unwrap_or(false);
                if !is_leader {
                    deny(
                        &mut output,
                        session,
                        "Only the group's leader can disband the group.",
                    );
                } else {
                    let members = parties.disband(session).unwrap_or_default();
                    for member in members {
                        output.write(text(member, "The group has been disbanded."));
                    }
                }
                write_prompt(&mut output, session);
            }
            PartyCommand::Tell { message } => {
                let Some(party) = parties.party_of(session) else {
                    deny(&mut output, session, "But you are not a member of a group!");
                    continue;
                };
                if message.trim().is_empty() {
                    deny(
                        &mut output,
                        session,
                        "Yes, but WHAT do you want to group-say?",
                    );
                    continue;
                }
                let members: Vec<Entity> = party.members.iter().copied().collect();
                for member in members {
                    if member != session {
                        output.write(text(
                            member,
                            format!("[Group] {actor_name} says, '{message}'"),
                        ));
                    }
                }
                output.write(text(session, format!("You group-say, '{message}'")));
                write_prompt(&mut output, session);
            }
            PartyCommand::Follow { target } if target.is_empty() => {
                match followers.get(session) {
                    Ok((_, following)) => {
                        let name = player_name(following.leader, &players);
                        output.write(text(session, format!("You are following {name}.")));
                    }
                    Err(_) => {
                        output.write(text(session, "Whom do you wish to follow?"));
                    }
                }
                write_prompt(&mut output, session);
            }
            PartyCommand::Follow { target } => {
                let Some((target_entity, target_name)) =
                    find_player_in_room(session, room, &target, &players)
                else {
                    deny(&mut output, session, "There is no such player here.");
                    continue;
                };
                if target_entity == session {
                    if followers.get(session).is_ok() {
                        commands.entity(session).remove::<TbaFollowing>();
                        output.write(text(session, "You are no longer following anyone."));
                    } else {
                        deny(&mut output, session, "You are already following yourself.");
                    }
                    write_prompt(&mut output, session);
                    continue;
                }
                let chain: HashMap<Entity, Entity> = followers
                    .iter()
                    .map(|(entity, following)| (entity, following.leader))
                    .collect();
                if would_loop(&chain, session, target_entity) {
                    deny(
                        &mut output,
                        session,
                        "Sorry, but following in loops is not allowed.",
                    );
                    continue;
                }
                match followers.get_mut(session) {
                    Ok((_, mut following)) => {
                        following.leader = target_entity;
                        following.last_room = room;
                    }
                    Err(_) => {
                        commands.entity(session).insert(TbaFollowing {
                            leader: target_entity,
                            last_room: room,
                        });
                    }
                }
                output.write(text(
                    session,
                    format!("You are now following {target_name}."),
                ));
                write_prompt(&mut output, session);
            }
            PartyCommand::Unfollow => {
                if followers.get(session).is_ok() {
                    commands.entity(session).remove::<TbaFollowing>();
                    output.write(text(session, "You are no longer following anyone."));
                } else {
                    deny(&mut output, session, "You are not following anyone.");
                }
                write_prompt(&mut output, session);
            }
        }
    }
}

/// `FixedUpdate`: move followers that were standing in the leader's previous
/// room into the leader's new room.
///
/// Conservative by design: a follower is only dragged when it is exactly where
/// the leader just was (`follower.room == following.last_room`). A follower
/// that wandered off is left where it is, and the anchor is updated to the
/// leader's new room so a later move cannot teleport it across the world.
#[allow(clippy::type_complexity)]
pub fn follow_system(
    mut locations: ParamSet<(
        Query<(Entity, &Location), Changed<Location>>,
        Query<(&mut Location, &mut TbaFollowing)>,
    )>,
) {
    let changed: Vec<(Entity, Entity)> = locations
        .p0()
        .iter()
        .map(|(entity, location)| (entity, location.room))
        .collect();
    let mut followers = locations.p1();
    for (leader, room) in changed {
        for (mut location, mut following) in &mut followers {
            if following.leader != leader {
                continue;
            }
            if location.room == following.last_room && location.room != room {
                location.room = room;
            }
            following.last_room = room;
        }
    }
}

/// `FixedUpdate`: drop despawned players from every party.
///
/// A finished session despawns its `Player` entity; without this, the party
/// resource would keep a phantom member (listed as `someone`, and a target for
/// `gtell` writes to a dead session). Leadership follows the same transfer rule
/// as an explicit `group leave`.
pub fn cleanup_parties_on_logout(
    mut removed: RemovedComponents<Player>,
    mut parties: ResMut<TbaParties>,
) {
    let gone: Vec<Entity> = removed.read().collect();
    for entity in gone {
        let _ = parties.remove(entity);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parse_group_forms_listing_and_leave() {
        assert_eq!(parse_party_line("group"), Some(PartyCommand::List));
        assert_eq!(parse_party_line("group list"), Some(PartyCommand::List));
        assert_eq!(
            parse_party_line("group Bob"),
            Some(PartyCommand::Form {
                target: "Bob".to_owned()
            })
        );
        assert_eq!(
            parse_party_line("GROUP new"),
            Some(PartyCommand::Form {
                target: String::new()
            })
        );
        assert_eq!(parse_party_line("group leave"), Some(PartyCommand::Leave));
        assert_eq!(
            parse_party_line("group disband"),
            Some(PartyCommand::Disband)
        );
    }

    #[test]
    fn parse_gtell_including_semicolon_shorthand() {
        assert_eq!(
            parse_party_line("gtell hello there"),
            Some(PartyCommand::Tell {
                message: "hello there".to_owned()
            })
        );
        assert_eq!(
            parse_party_line("; hello there"),
            Some(PartyCommand::Tell {
                message: "hello there".to_owned()
            })
        );
        assert_eq!(
            parse_party_line("gt"),
            Some(PartyCommand::Tell {
                message: String::new()
            })
        );
    }

    #[test]
    fn parse_follow_and_unfollow() {
        assert_eq!(
            parse_party_line("follow Alice"),
            Some(PartyCommand::Follow {
                target: "Alice".to_owned()
            })
        );
        assert_eq!(
            parse_party_line("follow"),
            Some(PartyCommand::Follow {
                target: String::new()
            })
        );
        assert_eq!(parse_party_line("unfollow"), Some(PartyCommand::Unfollow));
        assert_eq!(parse_party_line("say hi"), None);
        assert_eq!(parse_party_line("groupx"), None);
    }

    #[test]
    fn membership_is_a_single_source_of_truth() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();
        let mut parties = TbaParties::default();
        assert!(parties.is_empty());

        assert!(parties.create(a));
        assert!(!parties.create(a), "cannot re-create for a member");
        assert!(parties.add_member(a, b));
        assert!(!parties.add_member(a, b), "cannot add twice");
        assert!(parties.same_party(a, b));
        assert!(!parties.in_party(c));
        let members = parties.members_of(a).expect("a has a party");
        assert!(
            members.len() == 2 && members.contains(&a) && members.contains(&b),
            "membership is exactly {{a, b}}: {members:?}"
        );

        // A non-leader cannot add, and cannot join a second party.
        assert!(!parties.add_member(b, c));
        assert!(parties.join(c, b), "any member can host a join");
        assert!(parties.same_party(a, c));
    }

    #[test]
    fn leaving_transfers_leadership_and_disbands_when_empty() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let mut parties = TbaParties::default();
        parties.create(a);
        parties.add_member(a, b);

        let outcome = parties.remove(a).expect("a is in the party");
        assert_eq!(
            outcome,
            LeaveOutcome::Left {
                new_leader: Some(b),
                remaining: vec![b],
            }
        );
        assert_eq!(parties.party_of(b).map(|p| p.leader), Some(b));
        assert_eq!(parties.remove(b), Some(LeaveOutcome::Disbanded));
        assert!(parties.is_empty());
        assert_eq!(parties.remove(a), None, "not in a party");
    }

    #[test]
    fn disband_removes_the_whole_party() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let mut parties = TbaParties::default();
        parties.create(a);
        parties.add_member(a, b);
        let mut members = parties.disband(a).expect("a leads");
        members.sort_unstable();
        assert_eq!(members.len(), 2, "{members:?}");
        assert!(members.contains(&a) && members.contains(&b), "{members:?}");
        assert!(parties.is_empty());
        assert_eq!(parties.disband(a), None);
    }

    #[test]
    fn follow_loop_detection_walks_the_chain() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();
        let outsider = world.spawn_empty().id();
        let chain = HashMap::from([(b, a), (c, b)]);
        // c -> b -> a already; making a follow c would close the loop.
        assert!(would_loop(&chain, a, c));
        assert!(would_loop(&chain, a, b), "b follows a");
        assert!(!would_loop(&chain, a, outsider), "no chain from outsider");
        assert!(!would_loop(&HashMap::new(), a, b));
    }
}
