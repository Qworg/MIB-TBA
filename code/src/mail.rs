//! The postmaster's mail commands (`SPECIAL(postmaster)`, `tbamud/src/mail.c`).
//!
//! The durable mailbox is `mib_stdlib::mail` (D-06): a `Mailbox` player
//! component plus the offline `PlayerBundles` fallback. This module owns only
//! the Circle front end:
//!
//! - `check` reports whether mail is waiting;
//! - `receive` lists the messages and marks them read (the C creates mail
//!   paper objects; the pack has no note-item type, so it shows the message);
//! - `mail <name>` starts a two-line compose (subject, then body), charges
//!   `STAMP_PRICE` and calls the stdlib service.
//!
//! Every command only works while a postmaster is in the room, matching the C
//! spec proc.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::ecs::message::MessageCursor;
use bevy::prelude::*;
use mib_core::prelude::{CommandReceived, PlayerInput};
use mib_stdlib::prelude::CharacterCreated;
use mib_stdlib::prelude::{
    DirtySet, Location, MailConfig, MailDef, Mailbox, PendingBundleRestore, Player, PlayerBundles,
    resolve_recipient, seed_mail, send_mail,
};

use crate::combat::{TbaGold, TbaProgress};
use crate::content::SpecProc;
use crate::dg::intake::commands::{DgClaimed, claim_id, is_claimed};
use crate::dg::out::{is_playing, send_to_char};
use crate::spec::TbaSpecProc;

/// Circle's minimum writer level (`MIN_MAIL_LEVEL`, `mail.h:19`).
const MIN_MAIL_LEVEL: u32 = 1;
/// Circle's postage (`STAMP_PRICE`, `mail.h:22`).
const STAMP_PRICE: u32 = 150;
/// `LVL_IMMORT`, which sends free (`mail.c:441`).
const LVL_IMMORT: u32 = 31;

/// The compose state for one player, keyed by the player entity.
#[derive(Resource, Debug, Default)]
pub struct MailCompose(HashMap<Entity, Compose>);

#[derive(Debug, Clone)]
struct Compose {
    recipient: String,
    subject: Option<String>,
    lines: Vec<String>,
}

/// A received command the postmaster owns.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MailCommand {
    Mail(String),
    Check,
    Receive,
}

/// Parse `mail <name>`, `check` and `receive`.
fn parse_mail_line(line: &str) -> Option<MailCommand> {
    let line = line.trim();
    let (verb, rest) = match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index + 1..].trim()),
        None => (line, ""),
    };
    if verb.eq_ignore_ascii_case("check") {
        Some(MailCommand::Check)
    } else if verb.eq_ignore_ascii_case("receive") {
        Some(MailCommand::Receive)
    } else if verb.eq_ignore_ascii_case("mail") {
        Some(MailCommand::Mail(rest.to_owned()))
    } else {
        None
    }
}

fn raw_line(input: &PlayerInput) -> Option<&str> {
    match input {
        PlayerInput::RawLine(line) => Some(line),
        _ => None,
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn player_level(world: &World, entity: Entity) -> u32 {
    world
        .get::<TbaProgress>(entity)
        .map_or(1, |progress| progress.level)
}

/// Whether a postmaster stands in `room`.
fn postmaster_in(world: &mut World, room: Entity) -> bool {
    let mut query = world.query::<(&TbaSpecProc, &Location)>();
    query
        .iter(world)
        .any(|(proc, location)| proc.0 == SpecProc::Postmaster && location.room == room)
}

/// Give every player a mailbox so the online stdlib path can deliver to it.
pub fn ensure_mailboxes(world: &mut World) {
    let players: Vec<Entity> = {
        let mut query = world.query_filtered::<Entity, (With<Player>, Without<Mailbox>)>();
        query.iter(world).collect()
    };
    for player in players {
        world.entity_mut(player).insert(Mailbox::default());
    }
}

/// The `Update` front end: `mail`/`check`/`receive` at a postmaster.
pub fn handle_mail_commands(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
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
        let Some(line) = raw_line(&msg.input) else {
            continue;
        };
        let Some(command) = parse_mail_line(line) else {
            continue;
        };
        let Some(room) = world
            .get::<Location>(msg.session)
            .map(|location| location.room)
        else {
            continue;
        };
        if !postmaster_in(world, room) {
            continue;
        }
        // The player may have been created before `ensure_mailboxes` ran.
        if world.get::<Mailbox>(msg.session).is_none() {
            world.entity_mut(msg.session).insert(Mailbox::default());
        }
        match command {
            MailCommand::Check => {
                let (waiting, unread) = maintain_mailbox(world, msg.session);
                let text = if waiting {
                    format!("The postmaster tells you, 'You have {unread} message(s) waiting.'\r\n")
                } else {
                    "The postmaster tells you, 'Sorry, you don't have any mail waiting.'\r\n"
                        .to_owned()
                };
                send_to_char(world, msg.session, &text);
                claim_id(world, msg.id, msg.session);
            }
            MailCommand::Receive => {
                let text = receive_mailbox(world, msg.session);
                send_to_char(world, msg.session, &text);
                claim_id(world, msg.id, msg.session);
            }
            MailCommand::Mail(recipient) => {
                if recipient.is_empty() {
                    send_to_char(
                        world,
                        msg.session,
                        "The postmaster tells you, 'You need to specify an addressee!'\r\n",
                    );
                    claim_id(world, msg.id, msg.session);
                    continue;
                }
                let level = player_level(world, msg.session);
                if level < MIN_MAIL_LEVEL {
                    send_to_char(
                        world,
                        msg.session,
                        &format!(
                            "The postmaster tells you, 'Sorry, you have to be level {MIN_MAIL_LEVEL} to send mail!'\r\n"
                        ),
                    );
                    claim_id(world, msg.id, msg.session);
                    continue;
                }
                if level < LVL_IMMORT && gold_of(world, msg.session) < STAMP_PRICE {
                    send_to_char(
                        world,
                        msg.session,
                        &format!(
                            "The postmaster tells you, 'A stamp costs {STAMP_PRICE} coins.'\r\n\
                             The postmaster tells you, '...which I see you can't afford.'\r\n"
                        ),
                    );
                    claim_id(world, msg.id, msg.session);
                    continue;
                }
                if !recipient_exists(world, &recipient) {
                    send_to_char(
                        world,
                        msg.session,
                        "The postmaster tells you, 'No one by that name is registered here!'\r\n",
                    );
                    claim_id(world, msg.id, msg.session);
                    continue;
                }
                if level < LVL_IMMORT {
                    spend_gold(world, msg.session, STAMP_PRICE);
                    send_to_char(
                        world,
                        msg.session,
                        &format!(
                            "The postmaster tells you, 'I'll take {STAMP_PRICE} coins for the stamp.'\r\n"
                        ),
                    );
                }
                world
                    .get_resource_or_insert_with(MailCompose::default)
                    .0
                    .insert(
                        msg.session,
                        Compose {
                            recipient,
                            subject: None,
                            lines: Vec::new(),
                        },
                    );
                send_to_char(world, msg.session, "Subject: ");
                claim_id(world, msg.id, msg.session);
            }
        }
    }
}

/// `PreRoute`: while a player is composing mail, consume the line.
///
/// Runs before the DG command gate so a body line is never offered to a
/// trigger or the router.
pub fn compose_mail_input(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
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
            .get_resource_mut::<MailCompose>()
            .and_then(|mut compose| compose.0.remove(&msg.session))
        else {
            continue;
        };
        let Some(line) = raw_line(&msg.input) else {
            // A structured command aborts the editor rather than silently
            // becoming body text.
            send_to_char(world, msg.session, "Mail aborted.\r\n");
            claim_id(world, msg.id, msg.session);
            continue;
        };
        let line = line.trim_end();
        if line.eq_ignore_ascii_case("/c") || line.eq_ignore_ascii_case("abort") {
            send_to_char(world, msg.session, "Mail aborted.\r\n");
            claim_id(world, msg.id, msg.session);
            continue;
        }
        if line.eq_ignore_ascii_case("/h") || line.eq_ignore_ascii_case("help") {
            send_to_char(
                world,
                msg.session,
                "Write your message line by line. /s saves and sends, /c aborts, /h shows this help.\r\n",
            );
            world
                .get_resource_or_insert_with(MailCompose::default)
                .0
                .insert(msg.session, compose);
            claim_id(world, msg.id, msg.session);
            continue;
        }
        if line.eq_ignore_ascii_case("/s") || line == "@" {
            let subject = compose.subject.clone().unwrap_or_default();
            let body = compose.lines.join("\n");
            deliver(world, msg.session, &compose.recipient, &subject, &body);
            claim_id(world, msg.id, msg.session);
            continue;
        }
        if compose.subject.is_none() {
            if line.trim().is_empty() {
                send_to_char(world, msg.session, "Subject: ");
            } else {
                compose.subject = Some(line.trim().to_owned());
                send_to_char(
                    world,
                    msg.session,
                    "Write your message. (/s saves /h for help).\r\n",
                );
            }
            world
                .get_resource_or_insert_with(MailCompose::default)
                .0
                .insert(msg.session, compose);
            claim_id(world, msg.id, msg.session);
            continue;
        }
        compose.lines.push(line.to_owned());
        world
            .get_resource_or_insert_with(MailCompose::default)
            .0
            .insert(msg.session, compose);
        claim_id(world, msg.id, msg.session);
    }
}

/// Deliver through the stdlib mailbox service. A failure re-inserts nothing:
/// the compose buffer is already gone, matching Circle's abort on a send
/// error (the sender is told).
fn deliver(world: &mut World, session: Entity, recipient: &str, subject: &str, body: &str) {
    use bevy::ecs::system::SystemState;

    type MailQuery<'w, 's> = Query<
        'w,
        's,
        (
            Entity,
            &'static Player,
            &'static mut Mailbox,
            Option<&'static PendingBundleRestore>,
        ),
    >;
    type Params<'w, 's> = (
        MailQuery<'w, 's>,
        ResMut<'w, PlayerBundles>,
        ResMut<'w, DirtySet>,
        Option<Res<'w, MailConfig>>,
    );

    let Some(player) = world.get::<Player>(session) else {
        return;
    };
    let from = player.name.clone();
    world.init_resource::<PlayerBundles>();
    world.init_resource::<DirtySet>();
    let mut state: SystemState<Params<'_, '_>> = SystemState::new(world);
    let result = {
        let Ok((mut online, mut bundles, mut dirty, config)) = state.get_mut(world) else {
            return;
        };
        let config = config
            .as_deref()
            .map(|config| config.0.clone())
            .unwrap_or_else(MailDef::default);
        send_mail(
            &config,
            &mut online,
            &mut bundles,
            &mut dirty,
            &from,
            recipient,
            subject,
            body,
            now_secs(),
        )
    };
    match result {
        Ok(_) => send_to_char(world, session, "Message sent!\r\n"),
        Err(error) => send_to_char(
            world,
            session,
            &format!("Unable to send your message: {error}\r\n"),
        ),
    }
}

/// `check`/`receive`: apply expiry and return the unread count.
fn maintain_mailbox(world: &mut World, session: Entity) -> (bool, usize) {
    let config = world
        .get_resource::<MailConfig>()
        .map(|config| config.0.clone())
        .unwrap_or_default();
    let Some(mut mailbox) = world.get_mut::<Mailbox>(session) else {
        return (false, 0);
    };
    mailbox.maintain(now_secs(), &config);
    let unread = mailbox.unread();
    (!mailbox.messages.is_empty(), unread)
}

/// List the mailbox and mark every message read, the closest the pack gets to
/// Circle's `receive` (which turns each message into a mail-paper item).
fn receive_mailbox(world: &mut World, session: Entity) -> String {
    let config = world
        .get_resource::<MailConfig>()
        .map(|config| config.0.clone())
        .unwrap_or_default();
    let now = now_secs();
    let Some(mut mailbox) = world.get_mut::<Mailbox>(session) else {
        return "The postmaster tells you, 'Sorry, you don't have any mail waiting.'\r\n"
            .to_owned();
    };
    mailbox.maintain(now, &config);
    if mailbox.messages.is_empty() {
        return "The postmaster tells you, 'Sorry, you don't have any mail waiting.'\r\n"
            .to_owned();
    }
    let mut out = String::new();
    for (index, message) in mailbox.messages.iter().enumerate() {
        out.push_str(&format!(
            "{}. From {}: {} ({})\r\n",
            index + 1,
            message.from,
            message.subject,
            message.body
        ));
    }
    for message in &mut mailbox.messages {
        message.read = true;
    }
    out.push_str("The postmaster hands you your mail.\r\n");
    out
}

fn gold_of(world: &World, entity: Entity) -> u32 {
    world.get::<TbaGold>(entity).map_or(0, |gold| gold.0)
}

fn spend_gold(world: &mut World, entity: Entity, amount: u32) {
    if let Some(mut gold) = world.get_mut::<TbaGold>(entity) {
        gold.0 = gold.0.saturating_sub(amount);
    }
}

/// Whether a character name resolves online or in the persisted bundles.
fn recipient_exists(world: &mut World, name: &str) -> bool {
    let online = {
        let mut query = world.query::<&Player>();
        query
            .iter(world)
            .any(|player| player.name.eq_ignore_ascii_case(name.trim()))
    };
    if online {
        return true;
    }
    world
        .get_resource::<PlayerBundles>()
        .is_some_and(|bundles| resolve_recipient(bundles, name).is_some())
}

/// `CharacterCreated`: apply the `stdlib.mail` seed messages once.
pub fn seed_player_mail(world: &mut World, mut cursor: Local<MessageCursor<CharacterCreated>>) {
    use bevy::ecs::system::SystemState;

    type MailQuery<'w, 's> = Query<
        'w,
        's,
        (
            Entity,
            &'static Player,
            &'static mut Mailbox,
            Option<&'static PendingBundleRestore>,
        ),
    >;
    type Params<'w, 's> = (
        MailQuery<'w, 's>,
        ResMut<'w, PlayerBundles>,
        ResMut<'w, DirtySet>,
        Option<Res<'w, MailConfig>>,
    );

    let created: Vec<Entity> = {
        let Some(messages) = world.get_resource::<Messages<CharacterCreated>>() else {
            return;
        };
        cursor.read(messages).map(|event| event.session).collect()
    };
    if created.is_empty() {
        return;
    }
    world.init_resource::<PlayerBundles>();
    world.init_resource::<DirtySet>();
    let mut state: SystemState<Params<'_, '_>> = SystemState::new(world);
    for session in created {
        let Some(name) = world
            .get::<Player>(session)
            .map(|player| player.name.clone())
        else {
            continue;
        };
        let Ok((mut online, mut bundles, mut dirty, config)) = state.get_mut(world) else {
            return;
        };
        let config = config
            .as_deref()
            .map(|config| config.0.clone())
            .unwrap_or_else(MailDef::default);
        if let Err(error) = seed_mail(
            &config,
            &mut online,
            &mut bundles,
            &mut dirty,
            &name,
            now_secs(),
        ) {
            tracing::warn!(%name, %error, "could not seed the character's mailbox");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use mib_stdlib::prelude::{MailDef, PlayerBundles};

    #[test]
    fn postmaster_line_parser_matches_the_circle_verbs() {
        assert_eq!(parse_mail_line("check"), Some(MailCommand::Check));
        assert_eq!(parse_mail_line("receive"), Some(MailCommand::Receive));
        assert_eq!(
            parse_mail_line("mail Alice"),
            Some(MailCommand::Mail("Alice".to_owned()))
        );
        assert_eq!(
            parse_mail_line("  MAIL   Bob  "),
            Some(MailCommand::Mail("Bob".to_owned()))
        );
        assert_eq!(parse_mail_line("look"), None);
    }

    #[test]
    fn deliver_writes_to_an_online_recipients_mailbox() {
        let mut world = World::new();
        world.insert_resource(PlayerBundles::default());
        world.insert_resource(DirtySet::default());
        world.insert_resource(MailConfig(MailDef::default()));
        world.init_resource::<Messages<mib_core::prelude::OutputEvent>>();
        let sender = world
            .spawn(Player {
                account_id: 1,
                name: "Alice".to_owned(),
            })
            .id();
        let recipient = world
            .spawn((
                Player {
                    account_id: 2,
                    name: "Bob".to_owned(),
                },
                Mailbox::default(),
            ))
            .id();

        deliver(&mut world, sender, "Bob", "Hello", "The body.");

        let mailbox = world.get::<Mailbox>(recipient).expect("mailbox");
        assert_eq!(mailbox.messages.len(), 1);
        let message = &mailbox.messages[0];
        assert_eq!(message.from, "Alice");
        assert_eq!(message.subject, "Hello");
        assert_eq!(message.body, "The body.");
        assert!(!message.read);
    }

    #[test]
    fn receive_lists_and_marks_messages_read() {
        let mut world = World::new();
        world.insert_resource(MailConfig(MailDef::default()));
        world.init_resource::<Messages<mib_core::prelude::OutputEvent>>();
        let player = world
            .spawn((
                Player {
                    account_id: 2,
                    name: "Bob".to_owned(),
                },
                Mailbox {
                    messages: vec![mib_stdlib::prelude::MailMessage {
                        id: mib_stdlib::prelude::MailId(1),
                        from: "Alice".into(),
                        subject: "Hi".into(),
                        body: "Body".to_owned(),
                        sent_at: now_secs(),
                        read: false,
                    }],
                    next_id: 2,
                },
            ))
            .id();

        let output = receive_mailbox(&mut world, player);
        assert!(output.contains("From Alice: Hi"), "{output}");
        let mailbox = world.get::<Mailbox>(player).expect("mailbox");
        assert!(mailbox.messages[0].read);
    }
}
