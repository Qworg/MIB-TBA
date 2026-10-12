//! Issue #20 integration tests for TBA parties/groups, `gtell`, `follow`, and
//! party XP sharing.
//!
//! These drive the real systems against a small ECS app: `handle_party_commands`
//! for the raw-line commands, `follow_system` for the movement bridge, and
//! `award_kill_rewards` with a hand-written [`NpcDefeated`] for the reward
//! split. They cover form/list/leave/disband, an outsider rejection, `gtell`
//! delivery, the XP split among present members only, and the gold rule.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use smol_str::SmolStr;

fn warrior() -> TbaClassDef {
    TbaClassDef {
        name: "Warrior".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 10, 0),
        mana_dice: None,
        xp_table: vec![0, 1, 100, 300, 600],
        thaco_table: vec![100, 20, 20, 19, 18],
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: Vec::new(),
    }
}

fn content() -> TbaContent {
    TbaContent {
        classes: BTreeMap::from([(SmolStr::new("warrior"), warrior())]),
        ..TbaContent::default()
    }
}

fn test_app() -> App {
    let mut app = App::new();
    app.insert_resource(content());
    app.init_resource::<TbaParties>();
    app.add_message::<OutputEvent>();
    app.add_message::<CommandReceived>();
    app.add_message::<NpcDefeated>();
    app.add_systems(
        Update,
        (handle_party_commands, follow_system, award_kill_rewards).chain(),
    );
    app.add_systems(FixedUpdate, cleanup_parties_on_logout);
    app
}

fn spawn_room(app: &mut App) -> Entity {
    app.world_mut().spawn_empty().id()
}

fn spawn_player(app: &mut App, room: Entity, name: &str) -> Entity {
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
            TbaProgress::new(SmolStr::new("warrior")),
            TbaGold(0),
            TbaAlignment(0),
        ))
        .id()
}

fn send_line(app: &mut App, session: Entity, line: &str) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
            session,
            session_id: 1,
            input: PlayerInput::RawLine(line.to_owned()),
        });
}

/// Drain the frame's output once; `text_for` then slices it per session.
fn drain_text(app: &mut App) -> Vec<(Entity, String)> {
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .filter_map(|event| match event.payload {
            OutboundPayload::Text { body, .. } | OutboundPayload::Error { body, .. } => {
                Some((event.session, body.render_plain()))
            }
            _ => None,
        })
        .collect()
}

fn text_for(events: &[(Entity, String)], session: Entity) -> String {
    events
        .iter()
        .filter(|(target, _)| *target == session)
        .map(|(_, body)| body.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn xp_of(app: &App, entity: Entity) -> u64 {
    app.world().get::<TbaProgress>(entity).unwrap().xp
}

fn gold_of(app: &App, entity: Entity) -> u32 {
    app.world().get::<TbaGold>(entity).unwrap().0
}

fn parties(app: &App) -> &TbaParties {
    app.world().resource::<TbaParties>()
}

#[test]
fn group_forms_lists_and_leaves() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");

    send_line(&mut app, alice, "group Bob");
    app.update();
    assert!(
        parties(&app).same_party(alice, bob),
        "Alice and Bob are grouped"
    );
    assert_eq!(parties(&app).party_of(alice).unwrap().leader, alice);

    send_line(&mut app, alice, "group");
    app.update();
    let events = drain_text(&mut app);
    let listing = text_for(&events, alice);
    assert!(listing.contains("Your group consists of:"), "{listing}");
    assert!(listing.contains("Alice (leader)"), "{listing}");
    assert!(listing.contains("Bob"), "{listing}");

    send_line(&mut app, bob, "group leave");
    app.update();
    assert!(parties(&app).in_party(alice));
    assert!(!parties(&app).in_party(bob));
    assert_eq!(
        parties(&app).members_of(alice).unwrap().len(),
        1,
        "only the leader remains"
    );

    send_line(&mut app, bob, "group leave");
    app.update();
    let events = drain_text(&mut app);
    assert!(
        text_for(&events, bob).contains("aren't part of a group"),
        "leaving twice is refused"
    );
}

#[test]
fn a_non_leader_cannot_add_and_a_grouped_player_cannot_be_stolen() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    let carol = spawn_player(&mut app, room, "Carol");
    let dave = spawn_player(&mut app, room, "Dave");

    send_line(&mut app, alice, "group Bob");
    app.update();

    // Bob is not the leader: he cannot extend the party.
    send_line(&mut app, bob, "group Carol");
    app.update();
    assert!(
        !parties(&app).in_party(carol),
        "a non-leader cannot add members"
    );
    let events = drain_text(&mut app);
    assert!(
        text_for(&events, bob).contains("Only the group's leader"),
        "the refusal names the leader rule"
    );

    // Carol founds her own party with Dave; Alice cannot steal Carol into hers.
    send_line(&mut app, carol, "group Dave");
    app.update();
    assert!(parties(&app).same_party(carol, dave));

    send_line(&mut app, alice, "group Carol");
    app.update();
    assert!(
        !parties(&app).same_party(alice, carol),
        "Carol stays in her own group"
    );
    let events = drain_text(&mut app);
    assert!(
        text_for(&events, alice).contains("already in a group"),
        "the outsider is rejected"
    );

    // A player can join an existing group by naming one of its members.
    send_line(&mut app, dave, "group leave");
    app.update();
    send_line(&mut app, dave, "group Carol");
    app.update();
    assert!(
        parties(&app).same_party(carol, dave),
        "joining an existing group is allowed"
    );
}

#[test]
fn disband_removes_everyone_but_only_the_leader_may_do_it() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");

    send_line(&mut app, alice, "group Bob");
    app.update();

    send_line(&mut app, bob, "group disband");
    app.update();
    assert!(
        parties(&app).same_party(alice, bob),
        "a member cannot disband"
    );
    let events = drain_text(&mut app);
    assert!(text_for(&events, bob).contains("Only the group's leader"));

    send_line(&mut app, alice, "group disband");
    app.update();
    assert!(parties(&app).is_empty(), "the leader disbands the group");
}

#[test]
fn gtell_reaches_party_members_and_the_semicolon_shorthand_works() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    let carol = spawn_player(&mut app, room, "Carol");

    send_line(&mut app, alice, "group Bob");
    app.update();
    let _ = drain_text(&mut app);

    send_line(&mut app, alice, "gtell hello party");
    app.update();
    let events = drain_text(&mut app);
    assert!(
        text_for(&events, bob).contains("[Group] Alice says, 'hello party'"),
        "the member hears the group tell"
    );
    assert!(
        text_for(&events, alice).contains("You group-say, 'hello party'"),
        "the speaker sees their own tell"
    );
    assert!(
        text_for(&events, carol).is_empty(),
        "an outsider does not hear the tell"
    );

    send_line(&mut app, alice, "; shorthand");
    app.update();
    let events = drain_text(&mut app);
    assert!(
        text_for(&events, bob).contains("[Group] Alice says, 'shorthand'"),
        "the ; shorthand is group-say"
    );
}

#[test]
fn gtell_without_a_group_or_message_is_refused() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");

    send_line(&mut app, alice, "gtell hello");
    app.update();
    let events = drain_text(&mut app);
    assert!(
        text_for(&events, alice).contains("not a member of a group"),
        "an ungrouped tell is refused"
    );
}

#[test]
fn experience_is_split_among_present_party_members_and_gold_stays_with_the_killer() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let elsewhere = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");
    let carol = spawn_player(&mut app, room, "Carol");
    let dave = spawn_player(&mut app, room, "Dave");

    // Alice leads a party of Alice + Bob + Dave; Carol is an outsider.
    send_line(&mut app, alice, "group Bob");
    send_line(&mut app, alice, "group Dave");
    app.update();
    assert_eq!(parties(&app).members_of(alice).unwrap().len(), 3);

    // Dave wanders to another room, so he is in the party but not present.
    app.world_mut()
        .entity_mut(dave)
        .insert(Location { room: elsewhere });

    let victim = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<Messages<NpcDefeated>>()
        .write(NpcDefeated {
            victim,
            killer: alice,
            npc: NpcId::from("tba_npc_194"),
            experience: 90,
            gold: 9,
            alignment: 0,
        });
    app.update();

    assert_eq!(xp_of(&app, alice), 45, "the killer receives one share");
    assert_eq!(
        xp_of(&app, bob),
        45,
        "the present member receives one share"
    );
    assert_eq!(xp_of(&app, dave), 0, "an absent member receives nothing");
    assert_eq!(xp_of(&app, carol), 0, "an outsider receives nothing");
    assert_eq!(gold_of(&app, alice), 9, "gold stays with the killer");
    assert_eq!(gold_of(&app, bob), 0, "gold is not split");
    assert_eq!(gold_of(&app, dave), 0);
    assert_eq!(gold_of(&app, carol), 0);
}

#[test]
fn a_solo_kill_still_awards_the_full_experience() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");
    let victim = app.world_mut().spawn_empty().id();

    app.world_mut()
        .resource_mut::<Messages<NpcDefeated>>()
        .write(NpcDefeated {
            victim,
            killer: alice,
            npc: NpcId::from("tba_npc_194"),
            experience: 42,
            gold: 5,
            alignment: 0,
        });
    app.update();

    assert_eq!(
        xp_of(&app, alice),
        42,
        "a solo killer keeps the full reward"
    );
    assert_eq!(gold_of(&app, alice), 5);
}

#[test]
fn follow_moves_a_follower_into_the_leaders_new_room() {
    let mut app = test_app();
    let room1 = spawn_room(&mut app);
    let room2 = spawn_room(&mut app);
    let room3 = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room1, "Alice");
    let bob = spawn_player(&mut app, room1, "Bob");

    send_line(&mut app, bob, "follow Alice");
    app.update();
    assert_eq!(
        app.world().get::<TbaFollowing>(bob),
        Some(&TbaFollowing {
            leader: alice,
            last_room: room1,
        }),
        "Bob now follows Alice"
    );

    // Alice walks to another room; Bob was where she was and follows.
    app.world_mut()
        .entity_mut(alice)
        .insert(Location { room: room2 });
    app.update();
    assert_eq!(
        app.world().get::<Location>(bob).unwrap().room,
        room2,
        "the follower was dragged along"
    );

    // A follower who wandered off is not teleported: Bob leaves the room Alice
    // just vacated, then Alice moves again into a third room.
    app.world_mut()
        .entity_mut(bob)
        .insert(Location { room: room1 });
    app.world_mut()
        .entity_mut(alice)
        .insert(Location { room: room3 });
    app.update();
    assert_eq!(
        app.world().get::<Location>(bob).unwrap().room,
        room1,
        "an out-of-position follower is left alone"
    );

    send_line(&mut app, bob, "unfollow");
    app.update();
    assert!(
        app.world().get::<TbaFollowing>(bob).is_none(),
        "unfollow clears the state"
    );
}

#[test]
fn follow_rejects_loops() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");

    send_line(&mut app, alice, "follow Bob");
    app.update();
    send_line(&mut app, bob, "follow Alice");
    app.update();

    assert!(
        app.world().get::<TbaFollowing>(bob).is_none(),
        "the loop-forming follow is refused"
    );
    let events = drain_text(&mut app);
    assert!(
        text_for(&events, bob).contains("loops"),
        "the refusal names the loop"
    );
}

#[test]
fn a_despawned_player_is_dropped_from_the_party() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let alice = spawn_player(&mut app, room, "Alice");
    let bob = spawn_player(&mut app, room, "Bob");

    send_line(&mut app, alice, "group Bob");
    app.update();
    assert!(parties(&app).same_party(alice, bob));

    app.world_mut().entity_mut(bob).despawn();
    app.world_mut().run_schedule(FixedUpdate);
    app.world_mut().flush();
    assert!(
        !parties(&app).members_of(alice).unwrap().contains(&bob),
        "a despawned player leaves the party"
    );
    assert_eq!(parties(&app).party_of(alice).unwrap().leader, alice);
}
