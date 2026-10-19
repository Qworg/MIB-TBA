//! The bank/ATM object procedures (`SPECIAL(bank)`, `tbamud/src/spec_procs.c`).
//!
//! `TbaGold` is the pack's carried purse; `TbaBank` is Circle's separate
//! `GET_BANK_GOLD`. Both are player components and persist through the player
//! bundle. The bank/ATM is an item in the room or inventory whose vnum is in
//! [`BANK_ITEMS`] (`spec_assign.c:140-148`).

use bevy::ecs::message::MessageCursor;
use bevy::prelude::*;
use mib_core::prelude::{CommandReceived, PlayerInput};
use mib_stdlib::prelude::{CarriedBy, Item, Location, Player};

use crate::combat::TbaGold;
use crate::dg::intake::commands::{DgClaimed, claim_id, is_claimed};
use crate::dg::out::{is_playing, send_to_char};

/// The object vnums Circle assigns `bank` to (`spec_assign.c:140-148`):
/// bank, ATM, cashcard and their copies.
pub const BANK_ITEMS: [&str; 9] = [
    "tba_item_115",
    "tba_item_334",
    "tba_item_336",
    "tba_item_3034",
    "tba_item_3036",
    "tba_item_3907",
    "tba_item_10640",
    "tba_item_10751",
    "tba_item_25758",
];

/// Circle's `GET_BANK_GOLD`, persisted with the player.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Component, serde::Serialize, serde::Deserialize,
)]
pub struct TbaBank(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BankCommand {
    Balance,
    Deposit(u32),
    Withdraw(u32),
}

/// C `atoi` on the amount, so `deposit 12coins` reads `12` like the C.
fn atoi(text: &str) -> u32 {
    let text = text.trim_start();
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

fn parse_bank_line(line: &str) -> Option<BankCommand> {
    let line = line.trim();
    let (verb, rest) = match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index + 1..].trim()),
        None => (line, ""),
    };
    if verb.eq_ignore_ascii_case("balance") {
        Some(BankCommand::Balance)
    } else if verb.eq_ignore_ascii_case("deposit") {
        Some(BankCommand::Deposit(atoi(rest)))
    } else if verb.eq_ignore_ascii_case("withdraw") {
        Some(BankCommand::Withdraw(atoi(rest)))
    } else {
        None
    }
}

fn bank_here(world: &mut World, player: Entity) -> bool {
    let Some(room) = world.get::<Location>(player).map(|location| location.room) else {
        return false;
    };
    let mut query = world.query::<(&Item, Option<&Location>, Option<&CarriedBy>)>();
    query.iter(world).any(|(item, location, carried)| {
        BANK_ITEMS.contains(&item.def_id.as_ref())
            && (location.is_some_and(|location| location.room == room)
                || carried.is_some_and(|carried| carried.0 == player))
    })
}

/// `Update`, after the router: `balance`, `deposit`, `withdraw` at a bank.
pub fn handle_bank_commands(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
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
        let Some(command) = parse_bank_line(line) else {
            continue;
        };
        if !bank_here(world, msg.session) {
            continue;
        }
        if world.get::<TbaBank>(msg.session).is_none() {
            world.entity_mut(msg.session).insert(TbaBank::default());
        }
        match command {
            BankCommand::Balance => {
                let bank = world.get::<TbaBank>(msg.session).map_or(0, |bank| bank.0);
                let text = if bank > 0 {
                    format!("Your current balance is {bank} coins.\r\n")
                } else {
                    "You currently have no money deposited.\r\n".to_owned()
                };
                send_to_char(world, msg.session, &text);
            }
            BankCommand::Deposit(amount) => {
                if amount == 0 {
                    send_to_char(world, msg.session, "How much do you want to deposit?\r\n");
                } else if world.get::<TbaGold>(msg.session).map_or(0, |gold| gold.0) < amount {
                    send_to_char(world, msg.session, "You don't have that many coins!\r\n");
                } else {
                    if let Some(mut gold) = world.get_mut::<TbaGold>(msg.session) {
                        gold.0 -= amount;
                    }
                    if let Some(mut bank) = world.get_mut::<TbaBank>(msg.session) {
                        bank.0 = bank.0.saturating_add(amount);
                    }
                    send_to_char(
                        world,
                        msg.session,
                        &format!("You deposit {amount} coins.\r\n"),
                    );
                }
            }
            BankCommand::Withdraw(amount) => {
                if amount == 0 {
                    send_to_char(world, msg.session, "How much do you want to withdraw?\r\n");
                } else if world.get::<TbaBank>(msg.session).map_or(0, |bank| bank.0) < amount {
                    send_to_char(
                        world,
                        msg.session,
                        "You don't have that many coins deposited!\r\n",
                    );
                } else {
                    if let Some(mut bank) = world.get_mut::<TbaBank>(msg.session) {
                        bank.0 -= amount;
                    }
                    if world.get::<TbaGold>(msg.session).is_none() {
                        world.entity_mut(msg.session).insert(TbaGold(0));
                    }
                    if let Some(mut gold) = world.get_mut::<TbaGold>(msg.session) {
                        gold.0 = gold.0.saturating_add(amount);
                    }
                    send_to_char(
                        world,
                        msg.session,
                        &format!("You withdraw {amount} coins.\r\n"),
                    );
                }
            }
        }
        claim_id(world, msg.id, msg.session);
    }
}

/// `CharacterCreated`: a bank balance starts at zero like Circle's
/// `GET_BANK_GOLD`. The component is persisted, so this is the only seed.
pub fn ensure_bank_account(
    mut created: MessageReader<mib_stdlib::prelude::CharacterCreated>,
    players: Query<Entity, (With<Player>, Without<TbaBank>)>,
    mut commands: Commands,
) {
    for event in created.read() {
        if players.contains(event.session) {
            commands.entity(event.session).insert(TbaBank::default());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn bank_parser_uses_atoi_and_circle_verbs() {
        assert_eq!(parse_bank_line("balance"), Some(BankCommand::Balance));
        assert_eq!(
            parse_bank_line("deposit 120"),
            Some(BankCommand::Deposit(120))
        );
        assert_eq!(
            parse_bank_line("withdraw 12coins"),
            Some(BankCommand::Withdraw(12))
        );
        assert_eq!(parse_bank_line("deposit"), Some(BankCommand::Deposit(0)));
        assert_eq!(parse_bank_line("look"), None);
    }
}
