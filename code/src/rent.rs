//! The receptionist/cryogenicist (`SPECIAL(receptionist)`,
//! `SPECIAL(cryogenicist)`, `tbamud/src/objsave.c`).
//!
//! MIB stdlib owns the escrow primitive (`storage::park_player_items` /
//! `restore_player_items`); this module owns Circle's policy: the rent formula
//! (`MIN_RENT_COST + sum(item.rent)` times the factor), the 35-item cap, the
//! `offer`/`rent` commands and stripping the worn set when items are parked.

use bevy::ecs::message::MessageCursor;
use bevy::prelude::*;
use mib_core::prelude::{CommandReceived, PlayerInput};
use mib_stdlib::prelude::{
    CarriedBy, ContainedBy, Item, Location, Player, park_player_items, restore_player_items,
};

use crate::combat::{TbaGold, WieldedWeapon};
use crate::content::TbaItemDef;
use crate::dg::intake::commands::{DgClaimed, claim_id, is_claimed};
use crate::dg::out::{is_playing, send_to_char};
use crate::equipment::{TbaEquipment, TbaEquipmentSave};
use crate::module::TbaContent;
use crate::spec::TbaSpecProc;

/// Circle's `MIN_RENT_COST` (`config.c` default 50).
const MIN_RENT_COST: u32 = 50;
/// Circle's `MAX_OBJ_SAVE` (`config.c` default 35).
const MAX_OBJ_SAVE: usize = 35;
/// `RENT_FACTOR` / `CRYO_FACTOR` (`objsave.c:27-28`).
const RENT_FACTOR: u32 = 1;
const CRYO_FACTOR: u32 = 4;

/// One receptionist command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RentCommand {
    Offer,
    Rent,
}

fn parse_rent_line(line: &str) -> Option<RentCommand> {
    let line = line.trim();
    let verb = line.split_whitespace().next().unwrap_or("");
    if verb.eq_ignore_ascii_case("offer") {
        Some(RentCommand::Offer)
    } else if verb.eq_ignore_ascii_case("rent") {
        Some(RentCommand::Rent)
    } else {
        None
    }
}

/// A receptionist is in the room; return its proc if so.
fn receptionist_in(world: &mut World, room: Entity) -> Option<SpecProcKind> {
    let mut query = world.query::<(&TbaSpecProc, &Location)>();
    query.iter(world).find_map(|(proc, location)| {
        (location.room == room).then_some(match proc.0 {
            crate::content::SpecProc::Receptionist => Some(SpecProcKind::Rent),
            crate::content::SpecProc::Cryogenicist => Some(SpecProcKind::Cryo),
            _ => None,
        })?
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecProcKind {
    Rent,
    Cryo,
}

/// Every item the player carries, including container contents and the worn
/// set (TBA equipment items stay `CarriedBy`). A container is counted once and
/// its contents follow, matching Circle's recursive `Crash_report_rent`.
fn owned_items(world: &mut World, player: Entity) -> Vec<Entity> {
    let mut query = world.query::<(Entity, Option<&CarriedBy>, Option<&ContainedBy>)>();
    let rows: Vec<(Entity, Option<Entity>, Option<Entity>)> = query
        .iter(world)
        .map(|(entity, carried, contained)| {
            (
                entity,
                carried.map(|carried| carried.0),
                contained.map(|contained| contained.0),
            )
        })
        .collect();
    let mut owned: Vec<Entity> = rows
        .iter()
        .filter(|(_, carried, _)| *carried == Some(player))
        .map(|(entity, _, _)| *entity)
        .collect();
    let mut index = 0;
    while index < owned.len() {
        let parent = owned[index];
        for (entity, _, contained) in &rows {
            if *contained == Some(parent) && !owned.contains(entity) {
                owned.push(*entity);
            }
        }
        index += 1;
    }
    owned
}

/// Circle's `Crash_offer_rent`: the minimum fee plus each item's rent, times
/// the factor. Returns `(cost, item_count)`.
fn rent_cost(world: &mut World, player: Entity, factor: u32) -> (u32, usize) {
    let items = owned_items(world, player);
    let content = world
        .get_resource::<TbaContent>()
        .cloned()
        .unwrap_or_default();
    let mut cost = MIN_RENT_COST.saturating_mul(factor);
    for item in &items {
        let Some(def) = world.get::<Item>(*item).map(|item| item.def_id.clone()) else {
            continue;
        };
        let rent = content
            .items
            .get(&def)
            .map_or(0, |def: &TbaItemDef| def.rent);
        cost = cost.saturating_add(rent.saturating_mul(factor));
    }
    (cost, items.len())
}

fn spend_gold(world: &mut World, player: Entity, amount: u32) -> bool {
    let Some(mut gold) = world.get_mut::<TbaGold>(player) else {
        return false;
    };
    if gold.0 < amount {
        return false;
    }
    gold.0 -= amount;
    true
}

fn strip_worn(world: &mut World, player: Entity) {
    world
        .entity_mut(player)
        .remove::<(TbaEquipment, TbaEquipmentSave, WieldedWeapon)>();
}

/// `Update`, after the router: `offer`/`rent` at a receptionist.
pub fn handle_rent_commands(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
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
        let Some(command) = parse_rent_line(line) else {
            continue;
        };
        let Some(room) = world
            .get::<Location>(msg.session)
            .map(|location| location.room)
        else {
            continue;
        };
        let Some(kind) = receptionist_in(world, room) else {
            continue;
        };
        let factor = match kind {
            SpecProcKind::Rent => RENT_FACTOR,
            SpecProcKind::Cryo => CRYO_FACTOR,
        };
        let (cost, count) = rent_cost(world, msg.session, factor);
        if count == 0 {
            send_to_char(
                world,
                msg.session,
                "The receptionist tells you, 'But you are not carrying anything!  Just quit!'\r\n",
            );
            claim_id(world, msg.id, msg.session);
            continue;
        }
        if count > MAX_OBJ_SAVE {
            send_to_char(
                world,
                msg.session,
                &format!(
                    "The receptionist tells you, 'Sorry, but I cannot store more than {MAX_OBJ_SAVE} items.'\r\n"
                ),
            );
            claim_id(world, msg.id, msg.session);
            continue;
        }
        let mode = match kind {
            SpecProcKind::Rent => "per day",
            SpecProcKind::Cryo => "to be frozen",
        };
        if command == RentCommand::Offer {
            send_to_char(
                world,
                msg.session,
                &format!(
                    "The receptionist tells you, 'That will cost you {cost} gold coins {mode}.'\r\n"
                ),
            );
            claim_id(world, msg.id, msg.session);
            continue;
        }
        if !spend_gold(world, msg.session, cost) {
            send_to_char(
                world,
                msg.session,
                "The receptionist tells you, '...which I see you can't afford.'\r\n",
            );
            claim_id(world, msg.id, msg.session);
            continue;
        }
        let Some(account) = world
            .get::<Player>(msg.session)
            .map(|player| player.account_id)
        else {
            continue;
        };
        let storage = match kind {
            SpecProcKind::Rent => "rent",
            SpecProcKind::Cryo => "cryo",
        };
        let parked = park_player_items(world, account, storage);
        if parked.is_empty() {
            send_to_char(
                world,
                msg.session,
                "The receptionist tells you, 'I was unable to store your belongings. Please try again.'\r\n",
            );
            claim_id(world, msg.id, msg.session);
            continue;
        }
        strip_worn(world, msg.session);
        send_to_char(
            world,
            msg.session,
            "The receptionist stores your belongings and helps you into your private chamber.\r\n",
        );
        claim_id(world, msg.id, msg.session);
    }
}

/// `Update`: when a player enters, hand back anything parked under their
/// account. `Added<Player>` runs once per login.
pub fn restore_rented_items(players: Query<&Player, Added<Player>>, mut commands: Commands) {
    for player in &players {
        let account = player.account_id;
        commands.queue(move |world: &mut World| {
            let _ = restore_player_items(world, account, "rent");
            let _ = restore_player_items(world, account, "cryo");
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::content::TbaItemKind;
    use mib_stdlib::prelude::ItemId;

    #[test]
    fn rent_parser_matches_circle_verbs() {
        assert_eq!(parse_rent_line("offer"), Some(RentCommand::Offer));
        assert_eq!(parse_rent_line("RENT"), Some(RentCommand::Rent));
        assert_eq!(parse_rent_line("rent now"), Some(RentCommand::Rent));
        assert_eq!(parse_rent_line("look"), None);
    }

    #[test]
    fn rent_cost_is_the_fee_plus_each_items_rent() {
        let mut world = World::new();
        let id = ItemId::from("tba_item_1");
        let mut content = crate::module::TbaContent::default();
        content.items.insert(
            id.clone(),
            TbaItemDef {
                keywords: Vec::new(),
                extra_flags: Vec::new(),
                affect_flags: Vec::new(),
                kind: TbaItemKind::Other,
                slots: Vec::new(),
                values: [0; 4],
                affects: Vec::new(),
                weight: 1,
                cost: 0,
                rent: 10,
                min_level: 0,
                timer: None,
            },
        );
        world.insert_resource(content);
        let player = world
            .spawn(Player {
                account_id: 1,
                name: "Alice".to_owned(),
            })
            .id();
        world.spawn((
            Item {
                def_id: id,
                name: "a trinket".to_owned(),
                description: String::new(),
                value: None,
                tags: Vec::new(),
            },
            CarriedBy(player),
        ));

        assert_eq!(rent_cost(&mut world, player, RENT_FACTOR), (60, 1));
        assert_eq!(rent_cost(&mut world, player, CRYO_FACTOR), (240, 1));
    }
}
