//! The pet-shop room procedure (`SPECIAL(pet_shops)`,
//! `tbamud/src/spec_procs.c:614`).
//!
//! Circle's stock is the NPCs in the room immediately after the shop; the
//! price is `level * 300`. MIB stdlib owns the owned-NPC primitive
//! (`claim_follower`/`set_following`/`dismiss_follower`); this module owns the
//! shop stock, pricing and `list`/`buy` commands.

use bevy::ecs::message::MessageCursor;
use bevy::prelude::*;
use mib_core::prelude::{CommandReceived, PlayerInput};
use mib_stdlib::prelude::{
    LoadedContent, Location, Npc, NpcDef, NpcId, NpcKey, NpcsSection, Player, Room, claim_follower,
    set_following, spawn_npc,
};

use crate::combat::TbaGold;
use crate::dg::intake::commands::{DgClaimed, claim_id, is_claimed};
use crate::dg::out::{is_playing, send_to_char};
use crate::module::TbaContent;

/// The room vnums Circle assigns `pet_shops` to (`spec_assign.c:156-162`).
pub const PET_SHOP_ROOMS: [u32; 7] = [3031, 10738, 23281, 25722, 27155, 27616, 31523];

/// `PET_PRICE(pet)` (`spec_procs.c:613`).
fn pet_price(level: u32) -> u32 {
    level.saturating_mul(300)
}

fn stock_room(world: &mut World, shop: Entity) -> Option<Entity> {
    let vnum = world.get::<Room>(shop)?.num;
    let wanted = vnum.checked_add(1)?;
    let mut query = world.query::<(Entity, &Room)>();
    query
        .iter(world)
        .find(|(_, room)| room.num == wanted)
        .map(|(entity, _)| entity)
}

/// The NPCs currently in the stock room, with their level-derived price.
fn stock(world: &mut World, shop: Entity) -> Vec<(Entity, NpcId, String, u32)> {
    let Some(room) = stock_room(world, shop) else {
        return Vec::new();
    };
    let content = world
        .get_resource::<TbaContent>()
        .cloned()
        .unwrap_or_default();
    let npcs: Vec<(Entity, NpcId, String)> = {
        let mut query = world.query::<(Entity, &NpcKey, &Npc, &Location)>();
        query
            .iter(world)
            .filter(|(_, _, _, location)| location.room == room)
            .map(|(entity, key, npc, _)| (entity, key.0.clone(), npc.name.clone()))
            .collect()
    };
    npcs.into_iter()
        .map(|(entity, key, name)| {
            let level = content.npc_stats.get(&key).map_or(0, |stats| stats.level);
            (entity, key, name, pet_price(level))
        })
        .collect()
}

fn npc_def(loaded: Option<&LoadedContent>, id: &NpcId) -> Option<NpcDef> {
    let loaded = loaded?;
    for pack in loaded.0.iter().rev() {
        if let Some(section) = pack.section::<NpcsSection>()
            && let Some(def) = section.npcs.get(id)
        {
            return Some(def.clone());
        }
    }
    None
}

fn name_matches(needle: &str, name: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    !needle.is_empty() && name.to_lowercase().contains(&needle)
}

/// The exclusive implementation, run after [`handle_petshop_commands`] has
/// established that the room is a pet shop.
pub fn petshop_system(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
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
        let is_list = verb.eq_ignore_ascii_case("list");
        let is_buy = verb.eq_ignore_ascii_case("buy");
        if !is_list && !is_buy {
            continue;
        }
        let Some(room) = world
            .get::<Location>(msg.session)
            .map(|location| location.room)
        else {
            continue;
        };
        let Some(shop) = world.get::<Room>(room).map(|room| room.num) else {
            continue;
        };
        if !PET_SHOP_ROOMS.contains(&shop) {
            continue;
        }
        let stock = stock(world, room);
        if is_list {
            let mut text = "Available pets are:\r\n".to_owned();
            for (_, _, name, price) in &stock {
                text.push_str(&format!("{price:8} - {name}\r\n"));
            }
            send_to_char(world, msg.session, &text);
            claim_id(world, msg.id, msg.session);
            continue;
        }
        // `buy <pet> [name]`
        let (pet_word, pet_name) = match rest.find(char::is_whitespace) {
            Some(index) => (&rest[..index], rest[index + 1..].trim()),
            None => (rest, ""),
        };
        let Some((stock_entity, key, _, price)) = stock
            .iter()
            .find(|(_, _, name, _)| name_matches(pet_word, name))
            .cloned()
        else {
            send_to_char(world, msg.session, "There is no such pet!\r\n");
            claim_id(world, msg.id, msg.session);
            continue;
        };
        if world.get::<TbaGold>(msg.session).map_or(0, |gold| gold.0) < price {
            send_to_char(world, msg.session, "You don't have enough gold!\r\n");
            claim_id(world, msg.id, msg.session);
            continue;
        }
        let _ = stock_entity;
        let def = {
            let loaded = world.get_resource::<LoadedContent>();
            npc_def(loaded, &key)
        };
        let Some(def) = def else {
            send_to_char(world, msg.session, "There is no such pet!\r\n");
            claim_id(world, msg.id, msg.session);
            continue;
        };
        let Some(account) = world
            .get::<Player>(msg.session)
            .map(|player| player.account_id)
        else {
            continue;
        };
        let owner = world
            .get::<Player>(msg.session)
            .map(|player| player.name.clone())
            .unwrap_or_default();
        let pet = spawn_npc(world, Some(room), &key, &def);
        if !pet_name.is_empty()
            && let Some(mut npc) = world.get_mut::<Npc>(pet)
        {
            npc.name = format!("{} {}", npc.name, pet_name);
        }
        if !claim_follower(world, pet, account, &owner) {
            // A failed claim must not charge the player.
            send_to_char(world, msg.session, "There is no such pet!\r\n");
            claim_id(world, msg.id, msg.session);
            continue;
        }
        set_following(world, pet, true);
        if let Some(mut gold) = world.get_mut::<TbaGold>(msg.session) {
            gold.0 = gold.0.saturating_sub(price);
        }
        send_to_char(world, msg.session, "May you enjoy your pet.\r\n");
        claim_id(world, msg.id, msg.session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pet_price_is_level_times_300() {
        assert_eq!(pet_price(0), 0);
        assert_eq!(pet_price(1), 300);
        assert_eq!(pet_price(20), 6_000);
        assert_eq!(PET_SHOP_ROOMS.len(), 7);
    }
}
