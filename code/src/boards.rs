//! Bulletin-board commands (`SPECIAL(gen_board)`, `tbamud/src/boards.c`).
//!
//! The durable post store is `mib_stdlib::boards` (`BoardPosts` on the board
//! item's document). This module owns Circle's command surface:
//! `look/examine <board>`, `read <n> <board>`, `write <header>` and
//! `remove <n> <board>`. A board is an item whose id appears in
//! `stdlib.boards`; it must be in the room or carried.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::ecs::message::MessageCursor;
use bevy::prelude::*;
use mib_core::prelude::{Command, CommandReceived, PlayerInput};
use mib_stdlib::prelude::{BoardConfig, BoardDef, BoardPosts, CarriedBy, Item, Location, Player};

use crate::combat::TbaProgress;
use crate::dg::intake::commands::{DgClaimed, claim_id, is_claimed};
use crate::dg::out::{is_playing, send_to_char};

/// The players currently composing a board post.
#[derive(Resource, Debug, Default)]
pub struct BoardCompose(HashMap<Entity, Compose>);

#[derive(Debug, Clone)]
struct Compose {
    board: Entity,
    subject: String,
    lines: Vec<String>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn name_matches(needle: &str, name: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    !needle.is_empty() && name.to_lowercase().contains(&needle)
}

/// The board items the player can reach: the room's and their own carried
/// items that carry a `BoardPosts` store.
fn boards_for(world: &mut World, player: Entity) -> Vec<(Entity, String)> {
    let Some(room) = world.get::<Location>(player).map(|location| location.room) else {
        return Vec::new();
    };
    let mut query = world.query::<(
        Entity,
        &Item,
        Option<&Location>,
        Option<&CarriedBy>,
        Option<&BoardPosts>,
    )>();
    query
        .iter(world)
        .filter(|(_, _, location, carried, posts)| {
            posts.is_some()
                && (location.is_some_and(|location| location.room == room)
                    || carried.is_some_and(|carried| carried.0 == player))
        })
        .map(|(entity, item, _, _, _)| (entity, item.name.clone()))
        .collect()
}

/// Resolve the board named by `needle`, or the only board when it is empty.
fn find_board(world: &mut World, player: Entity, needle: &str) -> Option<Entity> {
    let boards = boards_for(world, player);
    if needle.trim().is_empty() {
        return boards.first().map(|(entity, _)| *entity);
    }
    boards
        .into_iter()
        .find(|(_, name)| name_matches(needle, name))
        .map(|(entity, _)| entity)
}

fn board_def(world: &World, board: Entity) -> BoardDef {
    let id = world.get::<Item>(board).map(|item| item.def_id.clone());
    id.and_then(|id| {
        world
            .get_resource::<BoardConfig>()
            .and_then(|config| config.0.get(&id).cloned())
    })
    .unwrap_or_default()
}

/// Render Circle's board listing (`board_show_board`).
fn show_board(world: &mut World, board: Entity) -> String {
    let def = board_def(world, board);
    let title = def
        .title
        .clone()
        .or_else(|| world.get::<Item>(board).map(|item| item.name.clone()))
        .unwrap_or_else(|| "a bulletin board".to_owned());
    let posts = world.get::<BoardPosts>(board).cloned().unwrap_or_default();
    let mut out = format!("This is {title}.  Usage: READ/REMOVE <messg #>, WRITE <header>.\r\n");
    if posts.posts.is_empty() {
        out.push_str("The board is empty.\r\n");
        return out;
    }
    out.push_str(&format!(
        "There are {} messages on the board.\r\n",
        posts.posts.len()
    ));
    for post in &posts.posts {
        out.push_str(&format!(
            "{:3} : {} :: {}\r\n",
            post.id, post.author, post.subject
        ));
    }
    out
}

fn read_post(world: &mut World, number: u64, board: Entity) -> String {
    let posts = world.get::<BoardPosts>(board).cloned().unwrap_or_default();
    match posts.find(number) {
        Some(post) => format!(
            "Post {} by {}: {}\r\n{}\r\n",
            post.id, post.author, post.subject, post.body
        ),
        None => format!("There is no message {number} on the board.\r\n"),
    }
}

fn push_post(
    world: &mut World,
    player: Entity,
    board: Entity,
    subject: &str,
    body: &str,
) -> String {
    let Some(name) = world
        .get::<Player>(player)
        .map(|player| player.name.clone())
    else {
        return "Only players may write on the board.\r\n".to_owned();
    };
    let def = board_def(world, board);
    let Some(mut posts) = world.get_mut::<BoardPosts>(board) else {
        return "That is not a bulletin board.\r\n".to_owned();
    };
    match posts.push(&def, &name, subject, body, now_secs()) {
        Ok(id) => format!("You post message {id} on the board.\r\n"),
        Err(error) => format!("The board rejects your message: {error}\r\n"),
    }
}

fn is_moderator(world: &World, player: Entity) -> bool {
    world
        .get::<TbaProgress>(player)
        .is_some_and(|progress| progress.level >= 31)
}

fn split_number_and_name(rest: &str) -> (Option<u64>, &str) {
    let rest = rest.trim();
    let (first, remaining) = match rest.find(char::is_whitespace) {
        Some(index) => (&rest[..index], rest[index + 1..].trim()),
        None => (rest, ""),
    };
    match first.parse::<u64>() {
        Ok(number) => (Some(number), remaining),
        Err(_) => (None, rest),
    }
}

/// `Update`, after the router: `read`/`write`/`remove` on a board.
pub fn handle_board_commands(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    let messages: Vec<CommandReceived> = {
        let Some(messages) = world.get_resource::<Messages<CommandReceived>>() else {
            return;
        };
        cursor.read(messages).cloned().collect()
    };
    for msg in messages {
        if !is_playing(world, msg.session) || is_claimed(world.get_resource::<DgClaimed>(), msg.id)
        {
            continue;
        }
        let Some(line) = (match &msg.input {
            PlayerInput::RawLine(line) => Some(line.as_str()),
            _ => None,
        }) else {
            continue;
        };
        let line = line.trim();
        let (verb, rest) = match line.find(char::is_whitespace) {
            Some(index) => (&line[..index], line[index + 1..].trim()),
            None => (line, ""),
        };
        if verb.eq_ignore_ascii_case("read") {
            let (number, name) = split_number_and_name(rest);
            let Some(number) = number else {
                continue;
            };
            let Some(board) = find_board(world, msg.session, name) else {
                continue;
            };
            let text = read_post(world, number, board);
            send_to_char(world, msg.session, &text);
            claim_id(world, msg.id, msg.session);
        } else if verb.eq_ignore_ascii_case("write") {
            let Some(board) = find_board(world, msg.session, "") else {
                continue;
            };
            if rest.is_empty() {
                send_to_char(world, msg.session, "We must have a headline!\r\n");
                claim_id(world, msg.id, msg.session);
                continue;
            }
            world
                .get_resource_or_insert_with(BoardCompose::default)
                .0
                .insert(
                    msg.session,
                    Compose {
                        board,
                        subject: rest.to_owned(),
                        lines: Vec::new(),
                    },
                );
            send_to_char(
                world,
                msg.session,
                "Write your message. (/s saves /h for help).\r\n",
            );
            claim_id(world, msg.id, msg.session);
        } else if verb.eq_ignore_ascii_case("remove") {
            let (number, name) = split_number_and_name(rest);
            let Some(number) = number else {
                continue;
            };
            let Some(board) = find_board(world, msg.session, name) else {
                continue;
            };
            let Some(name) = world
                .get::<Player>(msg.session)
                .map(|player| player.name.clone())
            else {
                continue;
            };
            let moderator = is_moderator(world, msg.session);
            let removed = world
                .get_mut::<BoardPosts>(board)
                .map(|mut posts| posts.remove(number, &name, moderator));
            let text = match removed {
                Some(Ok(post)) => format!("Message {} removed.\r\n", post.id),
                Some(Err(error)) => format!("{error}\r\n"),
                None => "That is not a bulletin board.\r\n".to_owned(),
            };
            send_to_char(world, msg.session, &text);
            claim_id(world, msg.id, msg.session);
        }
    }
}

/// `PreRoute`: `look`/`examine <board>` shows the posts instead of the item.
pub fn intercept_board_look(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    let messages: Vec<CommandReceived> = {
        let Some(messages) = world.get_resource::<Messages<CommandReceived>>() else {
            return;
        };
        cursor.read(messages).cloned().collect()
    };
    for msg in messages {
        if !is_playing(world, msg.session) || is_claimed(world.get_resource::<DgClaimed>(), msg.id)
        {
            continue;
        }
        let target = match &msg.input {
            PlayerInput::Command(Command::Look { target }) => target.clone(),
            PlayerInput::RawLine(line) => {
                let line = line.trim();
                let (verb, rest) = match line.find(char::is_whitespace) {
                    Some(index) => (&line[..index], line[index + 1..].trim()),
                    None => (line, ""),
                };
                if verb.eq_ignore_ascii_case("look") || verb.eq_ignore_ascii_case("examine") {
                    Some(rest.to_owned())
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(target) = target.filter(|target| !target.is_empty()) else {
            continue;
        };
        let Some(board) = find_board(world, msg.session, &target) else {
            continue;
        };
        let text = show_board(world, board);
        send_to_char(world, msg.session, &text);
        claim_id(world, msg.id, msg.session);
    }
}

/// `PreRoute`: consume a board post body before the DG gate.
pub fn compose_board_input(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    let messages: Vec<CommandReceived> = {
        let Some(messages) = world.get_resource::<Messages<CommandReceived>>() else {
            return;
        };
        cursor.read(messages).cloned().collect()
    };
    for msg in messages {
        if !is_playing(world, msg.session) || is_claimed(world.get_resource::<DgClaimed>(), msg.id)
        {
            continue;
        }
        let Some(mut compose) = world
            .get_resource_mut::<BoardCompose>()
            .and_then(|mut compose| compose.0.remove(&msg.session))
        else {
            continue;
        };
        let Some(line) = (match &msg.input {
            PlayerInput::RawLine(line) => Some(line.trim_end()),
            _ => None,
        }) else {
            send_to_char(world, msg.session, "Post aborted.\r\n");
            claim_id(world, msg.id, msg.session);
            continue;
        };
        if line.eq_ignore_ascii_case("/c") {
            send_to_char(world, msg.session, "Post aborted.\r\n");
            claim_id(world, msg.id, msg.session);
            continue;
        }
        if line.eq_ignore_ascii_case("/h") {
            send_to_char(
                world,
                msg.session,
                "Write the post line by line. /s saves, /c aborts, /h shows this help.\r\n",
            );
            world
                .get_resource_or_insert_with(BoardCompose::default)
                .0
                .insert(msg.session, compose);
            claim_id(world, msg.id, msg.session);
            continue;
        }
        if line.eq_ignore_ascii_case("/s") || line == "@" {
            let body = compose.lines.join("\n");
            let text = push_post(world, msg.session, compose.board, &compose.subject, &body);
            send_to_char(world, msg.session, &text);
            claim_id(world, msg.id, msg.session);
            continue;
        }
        compose.lines.push(line.to_owned());
        world
            .get_resource_or_insert_with(BoardCompose::default)
            .0
            .insert(msg.session, compose);
        claim_id(world, msg.id, msg.session);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use mib_stdlib::prelude::{BoardDef, ItemId};

    fn fixture() -> (World, Entity, Entity) {
        let mut world = World::new();
        let mut boards = BoardConfig::default();
        let id = ItemId::from("tba_item_3099");
        boards.0.insert(
            id.clone(),
            BoardDef {
                title: Some("The Mortal Board".to_owned()),
                max_posts: 2,
                ..BoardDef::default()
            },
        );
        world.insert_resource(boards);
        let room = world.spawn_empty().id();
        let board = world
            .spawn((
                Item {
                    def_id: id,
                    name: "a bulletin board".to_owned(),
                    description: "A board.".to_owned(),
                    value: None,
                    tags: Vec::new(),
                },
                BoardPosts::default(),
                Location { room },
            ))
            .id();
        let player = world
            .spawn((
                Player {
                    account_id: 1,
                    name: "Alice".to_owned(),
                },
                Location { room },
            ))
            .id();
        (world, player, board)
    }

    #[test]
    fn a_board_is_found_in_the_room_and_posts_round_trip() {
        let (mut world, player, board) = fixture();
        assert_eq!(find_board(&mut world, player, "bulletin"), Some(board));

        let message = push_post(&mut world, player, board, "Hello", "First post");
        assert!(message.contains("message 1"), "{message}");
        let listing = show_board(&mut world, board);
        assert!(listing.contains("There are 1 messages"), "{listing}");
        assert!(listing.contains("Alice :: Hello"), "{listing}");
        let read = read_post(&mut world, 1, board);
        assert!(read.contains("First post"), "{read}");
    }

    #[test]
    fn a_full_board_drops_the_oldest_post() {
        let (mut world, player, board) = fixture();
        push_post(&mut world, player, board, "One", "1");
        push_post(&mut world, player, board, "Two", "2");
        push_post(&mut world, player, board, "Three", "3");
        let posts = world.get::<BoardPosts>(board).expect("posts");
        assert_eq!(posts.posts.len(), 2);
        assert_eq!(posts.posts[0].subject, "Two");
        assert_eq!(posts.posts[1].subject, "Three");
    }
}
