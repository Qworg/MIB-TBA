//! What the item intake shares: the C's argument parsing and object lookup,
//! the engine's own item matching (mirrored, so a trigger sees the object the
//! engine will act on), and text delivery.

use dg_vm::{Uid, WorldHost};
use mib_core::prelude::*;
use mib_stdlib::prelude::{
    CarriedBy, Concealment, ContainedBy, Container, Item, Location, Perceiver, Perception, Player,
    perceive,
};

use crate::dg::host::EcsHost;
use crate::dg::out;

/// The words `one_argument` skips (`fill[]` in `interpreter.c`).
const FILL_WORDS: [&str; 7] = ["in", "from", "with", "the", "on", "at", "to"];

/// `one_argument`: the next word, lowercased, skipping fill words, and what
/// follows it.
pub fn one_argument(input: &str) -> (String, &str) {
    let mut rest = input;
    loop {
        let trimmed = rest.trim_start();
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        let (word, after) = trimmed.split_at(end);
        rest = after;
        let lower = word.to_lowercase();
        if !FILL_WORDS.contains(&lower.as_str()) {
            return (lower, rest);
        }
    }
}

/// `is_number`: an optional minus sign and then only digits.
pub fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// `atoi` of a string `is_number` accepted, saturating instead of overflowing.
pub fn atoi(text: &str) -> i32 {
    match text.parse::<i64>() {
        Ok(value) => i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX }),
        Err(_) => {
            if text.starts_with('-') {
                i32::MIN
            } else {
                i32::MAX
            }
        }
    }
}

/// The count `get_number` reads off a `N.name` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Number {
    /// The Nth match; `0` matches nothing.
    Nth(usize),
    /// `last.name`: the final match in the list.
    Last,
}

/// `get_number`: split `N.name` or `last.name` into the count and the name. A
/// prefix that is not a number is consumed and counts as `0`, which finds
/// nothing, as in the C.
pub fn split_number(name: &str) -> (Number, &str) {
    let Some((prefix, rest)) = name.split_once('.') else {
        return (Number::Nth(1), name);
    };
    if prefix.eq_ignore_ascii_case("last") {
        return (Number::Last, rest);
    }
    if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_digit()) {
        return (Number::Nth(0), rest);
    }
    (Number::Nth(prefix.parse().unwrap_or(usize::MAX)), rest)
}

/// The name half of a typed `N.name`: what the C leaves in the argument buffer
/// once `get_number` has cut the count off, and so what its messages repeat.
pub fn bare(typed: &str) -> &str {
    split_number(typed).1
}

/// How `find_all_dots` read an argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dots {
    /// `all`.
    All,
    /// `all.name`, with the name.
    AllDot(String),
    /// A single thing.
    Indiv,
}

/// `find_all_dots`.
pub fn find_all_dots(arg: &str) -> Dots {
    if arg == "all" {
        Dots::All
    } else if let Some(name) = arg.strip_prefix("all.") {
        Dots::AllDot(name.to_owned())
    } else {
        Dots::Indiv
    }
}

/// `isname`: the whole string is an abbreviation of one word of the list, or
/// equals the list.
pub fn is_name(needle: &str, namelist: &str) -> bool {
    if needle.is_empty() || namelist.is_empty() {
        return false;
    }
    if needle == namelist {
        return true;
    }
    let needle = needle.to_lowercase();
    namelist.split_whitespace().any(|word| {
        let word = word.to_lowercase();
        if !word.starts_with(&needle) {
            return false;
        }
        // Abbreviated numbers do not match.
        !needle.starts_with(|c: char| c.is_ascii_digit()) || needle == word
    })
}

/// The engine's rule: a case-insensitive `contains` of the fragment against a
/// short description.
pub fn name_matches(needle: &str, name: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    !needle.is_empty() && name.to_lowercase().contains(&needle)
}

/// Whether a typed fragment names a thing: a keyword abbreviation as the C
/// reads it, or a piece of the short description as the pack's own commands
/// read it.
pub fn names_thing(needle: &str, short: &str, keywords: &str) -> bool {
    is_name(needle, keywords) || name_matches(needle, short)
}

/// Pick from `list` by `number`, the Nth entry for which `matches` holds.
pub fn pick<T: Copy>(
    list: impl IntoIterator<Item = T>,
    number: Number,
    mut matches: impl FnMut(T) -> bool,
) -> Option<T> {
    match number {
        Number::Nth(0) => None,
        Number::Nth(n) => list.into_iter().filter(|item| matches(*item)).nth(n - 1),
        Number::Last => list.into_iter().filter(|item| matches(*item)).last(),
    }
}

/// `AN`: the article for a typed word.
pub fn article(word: &str) -> &'static str {
    if word.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
        "an"
    } else {
        "a"
    }
}

/// `money_desc`: how a heap of coins looks.
pub fn money_desc(amount: i32) -> &'static str {
    const TABLE: [(i32, &str); 14] = [
        (1, "a gold coin"),
        (10, "a tiny pile of gold coins"),
        (20, "a handful of gold coins"),
        (75, "a little pile of gold coins"),
        (200, "a small pile of gold coins"),
        (1000, "a pile of gold coins"),
        (5000, "a big pile of gold coins"),
        (10000, "a large heap of gold coins"),
        (20000, "a huge mound of gold coins"),
        (75000, "an enormous mound of gold coins"),
        (150_000, "a small mountain of gold coins"),
        (250_000, "a mountain of gold coins"),
        (500_000, "a huge mountain of gold coins"),
        (1_000_000, "an enormous mountain of gold coins"),
    ];
    TABLE.iter().find(|(limit, _)| amount <= *limit).map_or(
        "an absolutely colossal mountain of gold coins",
        |(_, text)| text,
    )
}

// -- the objects a character can name -----------------------------------------

/// The objects a character carries and does not wear (`ch->carrying`), newest
/// first.
pub fn carrying(host: &EcsHost<'_>, ch: Entity) -> Vec<Uid> {
    host.uid(ch)
        .map(|uid| host.carrying(uid))
        .unwrap_or_default()
}

/// `get_obj_in_list_vis` over `list` with a typed name that may carry a
/// `N.` prefix.
pub fn find_obj(host: &EcsHost<'_>, list: &[Uid], typed: &str) -> Option<Uid> {
    let (number, name) = split_number(typed);
    pick(list.iter().copied(), number, |uid| {
        names_thing(name, &host.name(uid), &host.keywords(uid))
    })
}

/// The objects after `after` in `list` that `name` (no count) matches, in
/// list order: the C's walk with `obj->next_content`.
pub fn matches_after(host: &EcsHost<'_>, list: &[Uid], after: Uid, name: &str) -> Vec<Uid> {
    list.iter()
        .copied()
        .skip_while(|uid| *uid != after)
        .skip(1)
        .filter(|uid| names_thing(name, &host.name(*uid), &host.keywords(*uid)))
        .collect()
}

/// A character in the room, as `get_char_room_vis` finds one: `self`/`me`
/// first, then the room's people newest first by keyword, seen by `ch`.
pub fn find_char_in_room(host: &EcsHost<'_>, ch: Entity, typed: &str) -> Option<Entity> {
    let name = typed.trim().to_lowercase();
    if name == "self" || name == "me" {
        return Some(ch);
    }
    let (number, name) = split_number(&name);
    let room = host.world.get::<Location>(ch)?.room;
    let (ch_uid, room_uid) = (host.uid(ch)?, host.uid(room)?);
    let people = host.room_people(room_uid);
    // `0.name` means a player by that name.
    let (number, players_only) = match number {
        Number::Nth(0) => (Number::Nth(1), true),
        other => (other, false),
    };
    let uid = pick(people.iter().copied(), number, |uid| {
        (!players_only
            || host
                .entity(uid)
                .is_some_and(|entity| host.world.get::<Player>(entity).is_some()))
            && names_thing(name, &host.name(uid), &host.keywords(uid))
            && host.can_see(ch_uid, uid)
    })?;
    host.entity(uid)
}

// -- the engine's item lookup, mirrored ------------------------------------------

/// One row of the engine's `ItemQuery`.
struct Row {
    entity: Entity,
    name: String,
    room: Option<Entity>,
    carried_by: Option<Entity>,
    contained: bool,
    container: bool,
}

/// The engine's item commands (`get`, `drop`, `put`) and the pack's `equip`
/// find things by walking a query over every item in the order the ECS
/// yields them. This view walks the same query in the same order and applies
/// the same rules (`mib_stdlib::items`), so what a trigger is told about is
/// what the command then moves.
pub struct EngineView {
    rows: Vec<Row>,
}

impl EngineView {
    /// Snapshot every item.
    pub fn new(world: &mut World) -> Self {
        let mut query = world.query::<(
            Entity,
            &Item,
            Option<&Location>,
            Option<&CarriedBy>,
            Option<&ContainedBy>,
        )>();
        let mut rows: Vec<Row> = query
            .iter(world)
            .map(|(entity, item, location, carried, contained)| Row {
                entity,
                name: item.name.clone(),
                room: location.map(|location| location.room),
                carried_by: carried.map(|carried| carried.0),
                contained: contained.is_some(),
                container: false,
            })
            .collect();
        for row in &mut rows {
            row.container = world.get::<Container>(row.entity).is_some();
        }
        Self { rows }
    }

    /// `find_carried`: a carried item (worn or not) the typed name matches.
    pub fn carried(&self, actor: Entity, typed: &str) -> Option<Entity> {
        self.nth(typed, |row| row.carried_by == Some(actor))
    }

    /// `ch->carrying`: a carried item the actor does not wear, which is all
    /// the C's `get_obj_in_list_vis(ch, name, ch->carrying)` looks at.
    pub fn carried_unworn(&self, actor: Entity, typed: &str, worn: &[Entity]) -> Option<Entity> {
        self.nth(typed, |row| {
            row.carried_by == Some(actor) && !worn.contains(&row.entity)
        })
    }

    /// `find_ground`: an item lying in `room`.
    pub fn ground(&self, room: Entity, typed: &str) -> Option<Entity> {
        self.nth(typed, |row| {
            row.carried_by.is_none() && !row.contained && row.room == Some(room)
        })
    }

    /// `find_container`: a container carried by `actor` or lying in `room`.
    pub fn container(&self, actor: Entity, room: Entity, typed: &str) -> Option<Entity> {
        self.nth(typed, |row| {
            !row.contained
                && row.container
                && (row.carried_by == Some(actor)
                    || (row.carried_by.is_none() && row.room == Some(room)))
        })
    }

    /// The matching entry of a container's contents, in its own order.
    pub fn in_contents(&self, contents: &[Entity], typed: &str) -> Option<Entity> {
        let selector = TargetSelector::parse(typed);
        contents
            .iter()
            .copied()
            .filter(|entity| {
                self.rows
                    .iter()
                    .find(|row| row.entity == *entity)
                    .is_some_and(|row| name_matches(&selector.keyword, &row.name))
            })
            .nth(selector.ordinal.saturating_sub(1))
    }

    fn nth(&self, typed: &str, mut eligible: impl FnMut(&Row) -> bool) -> Option<Entity> {
        let selector = TargetSelector::parse(typed);
        self.rows
            .iter()
            .filter(|row| eligible(row) && name_matches(&selector.keyword, &row.name))
            .nth(selector.ordinal.saturating_sub(1))
            .map(|row| row.entity)
    }
}

// -- text ---------------------------------------------------------------------

/// `CAP`: the first letter in upper case.
pub fn cap(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `act(..., TO_CHAR)`: a line to the actor.
pub fn tell(world: &mut World, ch: Entity, text: &str) {
    out::send_to_char(world, ch, &cap(text));
}

/// `act(..., TO_ROOM)` with `hide_invisible` TRUE: a line to everyone in the
/// actor's room but the actor and `also`, who can see the actor (the pack's
/// perception rule over `Concealment` and `Perception`, as `WorldHost::can_see`).
pub fn tell_room(world: &mut World, ch: Entity, text: &str, also: &[Entity]) {
    let Some(room) = world.get::<Location>(ch).map(|location| location.room) else {
        return;
    };
    let concealment = world.get::<Concealment>(ch).copied().unwrap_or_default();
    let mut players =
        world.query_filtered::<(Entity, &Location, Option<&Perception>), With<Player>>();
    let listeners: Vec<Entity> = players
        .iter(world)
        .filter(|(entity, location, _)| {
            location.room == room && *entity != ch && !also.contains(entity)
        })
        .filter(|(_, _, perception)| {
            perceive(
                &Perceiver::from_perception(*perception, false),
                &concealment,
                false,
            )
        })
        .map(|(entity, _, _)| entity)
        .collect();
    let text = cap(text);
    for listener in listeners {
        out::send_to_char(world, listener, &text);
    }
}

/// The prompt that ends every command.
pub fn prompt(world: &mut World, session: Entity) {
    if world.get::<Player>(session).is_none() {
        return;
    }
    if let Some(mut output) = world.get_resource_mut::<Messages<OutputEvent>>() {
        output.write(OutputEvent {
            session,
            payload: OutboundPayload::text(TextChannel::Prompt, "> "),
        });
    }
}
