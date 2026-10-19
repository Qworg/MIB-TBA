//! Issue #19 integration tests for the in-world MUD clock and its `time`
//! command.
//!
//! The clock itself is the engine's D-42 `TimeOfDay`; the pack only renders it.
//! These tests drive the raw-line `handle_time_command` system against a small
//! app and assert Circle's exact `do_time` rendering.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;

fn test_app(clock: TimeOfDay) -> App {
    let mut app = App::new();
    app.insert_resource(clock);
    app.add_message::<OutputEvent>();
    app.add_message::<CommandReceived>();
    app.add_systems(Update, handle_time_command);
    app
}

fn spawn_player(app: &mut App, room: Entity) -> Entity {
    app.world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: "Hero".to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room },
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

fn drain_text(app: &mut App) -> Vec<String> {
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

#[test]
fn time_command_reports_the_in_world_clock() {
    let mut app = test_app(TimeOfDay {
        hour: 14,
        ..TimeOfDay::default()
    });
    let room = app.world_mut().spawn_empty().id();
    let player = spawn_player(&mut app, room);

    send_line(&mut app, player, "time");
    app.update();

    let text = drain_text(&mut app).join("\n");
    assert!(text.contains("It is 2 o'clock pm"), "{text}");
    assert!(text.contains("on the Day of the Bull"), "{text}");
    assert!(
        text.contains("The 1st Day of the Month of Winter, Year 0."),
        "{text}"
    );
}

#[test]
fn time_command_uses_midnight_and_the_calendar() {
    // Day index 1*35 + 1 = day 2 of month 1, year 3 of Circle's calendar.
    let mut app = test_app(TimeOfDay {
        hour: 0,
        day_index: 3 * 35 * 17 + 36,
        ..TimeOfDay::default()
    });
    let room = app.world_mut().spawn_empty().id();
    let player = spawn_player(&mut app, room);

    send_line(&mut app, player, "time");
    app.update();

    let text = drain_text(&mut app).join("\n");
    assert!(text.contains("It is 12 o'clock am"), "{text}");
    // day 1 + 1 = the 2nd; month 1 -> the Winter Wolf.
    assert!(
        text.contains("The 2nd Day of the Month of the Winter Wolf, Year 3."),
        "{text}"
    );
}

#[test]
fn non_time_lines_are_ignored() {
    let mut app = test_app(TimeOfDay::default());
    let room = app.world_mut().spawn_empty().id();
    let player = spawn_player(&mut app, room);

    send_line(&mut app, player, "look");
    send_line(&mut app, player, "timekeeper");
    app.update();

    assert!(
        drain_text(&mut app).is_empty(),
        "only an exact `time` verb is answered"
    );
}
