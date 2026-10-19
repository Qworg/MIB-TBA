//! The in-world MUD clock (issue #19): the `time` command and the day/night
//! room-darkness bridge.
//!
//! The clock itself is the engine's D-42 world clock
//! ([`TimeOfDay`](mib_stdlib::prelude::TimeOfDay), derived from the
//! wall-anchored `WorldClock` and authored in `content/stdlib.time.yaml` at
//! Circle's 75-real-seconds-per-MUD-hour cadence). This module only reads it:
//!
//! * [`handle_time_command`] answers the Circle `time` command from the clock,
//!   reproducing `do_time` (`tbamud/src/act.informative.c:1078`).
//! * [`apply_night_darkness`] writes the runtime `RoomState.flags` `Dark`
//!   override for authored **outdoor** rooms at Circle's sunset/night
//!   (`room_is_dark`, `tbamud/src/utils.c:911`). It never touches a room whose
//!   authored tags already include `Dark`, so a permanently dark room stays
//!   dark at noon.
//!
//! ## Weather
//!
//! Weather is the engine's D-42 per-zone simulation (`content/stdlib.weather.yaml`
//! plus `weather_zone` on each area); the `weather` verb and the outdoor
//! ambient line are projected by the stdlib, so the pack adds nothing here.
//! Circle's pressure/sky/temperature model is not reproduced digit-for-digit —
//! it is a weighted state per zone instead (recorded in `docs/runbook.md` §8).
//!
//! ## Known limit
//!
//! * The stdlib `look` darkness check reads [`Room::tags`] directly rather than
//!   [`Room::effective_flags`](mib_stdlib::prelude::Room::effective_flags), so
//!   the night override is the persisted runtime authority but is not yet
//!   consulted by that particular code path (the stdlib is the engine checkout
//!   and is out of scope here). The override still round-trips through
//!   `DocKind::RoomState` and is what a future `look` should read.
//!
//! ## Persistence
//!
//! The **room override** persists (`RoomState` is a `DocKind::RoomState`
//! document). The **clock** needs no persistence: it is a pure function of wall
//! time, so a restart resumes at the correct in-world instant instead of
//! resetting (this replaces the pack's former runtime-only clock).

use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{DayPhase, Room, RoomState, RoomTag, Session, SessionState, TimeOfDay};

use crate::combat::commands::text;
use crate::dg::intake::commands::{DgClaimed, is_claimed};

/// Circle's `weekdays` (`tbamud/src/constants.c:827`), indexed by
/// `((35 * month) + day) % 7`.
pub const WEEKDAYS: [&str; 7] = [
    "the Day of the Moon",
    "the Day of the Bull",
    "the Day of the Deception",
    "the Day of Thunder",
    "the Day of Freedom",
    "the Day of the Great Gods",
    "the Day of the Sun",
];

/// Circle's `month_name` (`tbamud/src/constants.c:838`), indexed by month 0..=16.
pub const MONTH_NAMES: [&str; 17] = [
    "Month of Winter",
    "Month of the Winter Wolf",
    "Month of the Frost Giant",
    "Month of the Old Forces",
    "Month of the Grand Struggle",
    "Month of the Spring",
    "Month of Nature",
    "Month of Futility",
    "Month of the Dragon",
    "Month of the Sun",
    "Month of the Heat",
    "Month of the Battle",
    "Month of the Dark Shades",
    "Month of the Shadows",
    "Month of the Long Shadows",
    "Month of the Ancient Darkness",
    "Month of the Great Evil",
];

/// Circle's ordinal suffix for a day-of-month (`do_time`): `1st`, `2nd`,
/// `3rd`, everything else `th`, with the 11th–13th exception.
pub fn ordinal_suffix(day: u32) -> &'static str {
    if (day % 100) / 10 == 1 {
        return "th";
    }
    match day % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

/// Render the two-line Circle time report for `clock`.
pub fn time_report(time: &TimeOfDay) -> String {
    let hour = u32::from(time.hour);
    let hour12 = if hour.is_multiple_of(12) {
        12
    } else {
        hour % 12
    };
    let meridiem = if hour >= 12 { "pm" } else { "am" };
    let (day0, month, year) = circle_calendar(time);
    let day = day0 + 1;
    let weekday = WEEKDAYS[((35 * month + day) % 7) as usize];
    let month_name = MONTH_NAMES[(month % 17) as usize];
    format!(
        "It is {hour12} o'clock {meridiem}, on {weekday}.\n\
         The {day}{} Day of the {month_name}, Year {year}.",
        ordinal_suffix(day),
    )
}

/// Circle's `time_info` from the engine clock's day index: 35-day months and 17
/// months per year.
///
/// All three are **0-based**, exactly as Circle stores them; `do_time` prints
/// `day + 1`. The engine clock (D-42) is the single authority for world time,
/// so these are derived rather than kept in a second clock.
pub fn circle_calendar(time: &TimeOfDay) -> (u32, u32, u32) {
    let days = time.day_index;
    let month = (days / 35) % 17;
    let year = days / (35 * 17);
    (days % 35, month, year)
}

/// `Update` in `BridgeSet::Game`: answer a raw `time` line from the clock.
pub fn handle_time_command(
    mut reader: MessageReader<CommandReceived>,
    claimed: Option<Res<DgClaimed>>,
    sessions: Query<&Session>,
    clock: Option<Res<TimeOfDay>>,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TimeOfDay::default();
    let clock = clock.as_deref().unwrap_or(&empty);
    for msg in reader.read() {
        let session = msg.session;
        let playing = sessions
            .get(session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        if is_claimed(claimed.as_deref(), msg.id) {
            continue;
        }
        let PlayerInput::RawLine(line) = &msg.input else {
            continue;
        };
        if !is_time_line(line) {
            continue;
        }
        output.write(text(session, time_report(clock)));
        write_prompt(&mut output, session);
    }
}

/// Is `line` the `time` command? Circle's `do_time` ignores arguments.
fn is_time_line(line: &str) -> bool {
    let line = line.trim();
    let verb = line.split_whitespace().next().unwrap_or("");
    verb.eq_ignore_ascii_case("time")
}

/// `FixedUpdate` after the clock advance: mirror night onto authored outdoor
/// rooms' runtime [`RoomState`] `Dark` flag.
///
/// Circle's `room_is_dark` makes an outdoor room dark at `SUN_SET`/`SUN_DARK`
/// unless it already carries `ROOM_DARK`. MIB keeps the authored tags in
/// [`Room::tags`] and the runtime override in [`RoomState::flags`], and
/// [`Room::effective_flags`](mib_stdlib::prelude::Room::effective_flags)
/// combines them; this system writes only the override, and only for rooms
/// tagged [`RoomTag::Outdoor`], so:
///
/// * an authored `Dark` room is left alone (permanently dark), and
/// * at night an outdoor room gets `flags[Dark] = true`; during the day the
///   entry is **removed** (not set `false`) so the map stays an override set,
///   not a full flag snapshot.
///
/// The write is skipped when the value already matches, so an unchanged night
/// does not dirty every room's save document every tick.
pub fn apply_night_darkness(
    clock: Option<Res<TimeOfDay>>,
    mut rooms: Query<(&Room, &mut RoomState)>,
) {
    let Some(clock) = clock else {
        return;
    };
    // Circle's `SUN_DARK` window is hours 22..=4, which is exactly the engine
    // clock's `Night` phase for the phase hours this world authors
    // (`docs`/`content/stdlib.time.yaml`: dawn 5, day 6, dusk 21, night 22).
    let night = clock.phase == DayPhase::Night;
    for (room, mut state) in &mut rooms {
        if room.tags.contains(&RoomTag::Dark) || !room.tags.contains(&RoomTag::Outdoor) {
            continue;
        }
        let current = state.flags.get(&RoomTag::Dark).copied();
        if night {
            if current != Some(true) {
                state.flags.insert(RoomTag::Dark, true);
            }
        } else if current.is_some() {
            state.flags.remove(&RoomTag::Dark);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use mib_stdlib::content::Sector;

    use super::*;

    fn room(tags: &[RoomTag]) -> Room {
        Room {
            key: "test".into(),
            name: "A Test Room".to_owned(),
            description: FormattedText::default(),
            num: 1,
            area: "test".into(),
            coord: (0, 0, 0),
            tags: tags.iter().copied().collect(),
            media: Vec::new(),
            sector: Sector::Field,
            extra_descriptions: Vec::new(),
            outdoor: false,
            weather_zone: None,
            ambient: None,
            ambient_stamp: None,
            ambient_phases: Default::default(),
            ambient_weather: Default::default(),
            time: None,
            weather_state: None,
        }
    }

    #[test]
    fn ordinal_suffix_matches_circle() {
        assert_eq!(ordinal_suffix(1), "st");
        assert_eq!(ordinal_suffix(2), "nd");
        assert_eq!(ordinal_suffix(3), "rd");
        assert_eq!(ordinal_suffix(4), "th");
        assert_eq!(ordinal_suffix(11), "th");
        assert_eq!(ordinal_suffix(12), "th");
        assert_eq!(ordinal_suffix(13), "th");
        assert_eq!(ordinal_suffix(21), "st");
        assert_eq!(ordinal_suffix(22), "nd");
        assert_eq!(ordinal_suffix(23), "rd");
    }

    #[test]
    fn time_report_renders_circles_two_lines() {
        let clock = TimeOfDay {
            hour: 14,
            day_index: 0,
            ..TimeOfDay::default()
        };
        let report = time_report(&clock);
        assert!(report.contains("It is 2 o'clock pm"), "{report}");
        assert!(report.contains("on the Day of the Bull"), "{report}");
        assert!(
            report.contains("The 1st Day of the Month of Winter, Year 0."),
            "{report}"
        );

        let midnight = TimeOfDay {
            hour: 0,
            ..TimeOfDay::default()
        };
        assert!(
            time_report(&midnight).contains("It is 12 o'clock am"),
            "{}",
            time_report(&midnight)
        );
    }

    #[test]
    fn night_darkness_only_touches_outdoor_rooms() {
        let mut app = App::new();
        app.insert_resource(TimeOfDay {
            hour: 23,
            phase: DayPhase::Night,
            ..TimeOfDay::default()
        });
        app.add_systems(Update, apply_night_darkness);
        let outdoor = app.world_mut().spawn(room(&[RoomTag::Outdoor])).id();
        let indoor = app.world_mut().spawn(room(&[])).id();
        let authored_dark = app
            .world_mut()
            .spawn(room(&[RoomTag::Outdoor, RoomTag::Dark]))
            .id();
        app.world_mut()
            .entity_mut(outdoor)
            .insert(RoomState::default());
        app.world_mut()
            .entity_mut(indoor)
            .insert(RoomState::default());
        app.world_mut()
            .entity_mut(authored_dark)
            .insert(RoomState::default());

        app.update();
        assert_eq!(
            app.world()
                .get::<RoomState>(outdoor)
                .unwrap()
                .flags
                .get(&RoomTag::Dark),
            Some(&true),
            "an outdoor room is dark at night"
        );
        assert!(
            app.world()
                .get::<RoomState>(indoor)
                .unwrap()
                .flags
                .is_empty(),
            "an indoor room is never overridden"
        );
        assert!(
            app.world()
                .get::<RoomState>(authored_dark)
                .unwrap()
                .flags
                .is_empty(),
            "an authored Dark room is left alone"
        );

        // Day clears the override (rather than storing an explicit `false`).
        app.world_mut().resource_mut::<TimeOfDay>().phase = DayPhase::Day;
        app.update();
        assert!(
            app.world()
                .get::<RoomState>(outdoor)
                .unwrap()
                .flags
                .is_empty(),
            "the day override is removed"
        );
    }
}
