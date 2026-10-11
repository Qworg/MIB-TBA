//! TBA-E17 — the shopkeeper economy: `list`, `buy`, `sell`, `value`.
//!
//! CircleMUD's shop commands have no structured protocol variants:
//! [`Command`](mib_protocol::prelude::Command) stops at the movement/combat
//! grammar and knows nothing about trade. So [`handle_shop_commands`] parses
//! [`PlayerInput::RawLine`] itself, running in
//! [`BridgeSet::Game`](mib_core::prelude::BridgeSet) after the stdlib router
//! (the pack declares `list`/`buy`/`sell`/`value` in `content/stdlib.verbs.yaml`
//! so the router claims them instead of reporting an unknown verb).
//!
//! The trade terms are [`TbaContent::shops`](crate::module::TbaContent::shops),
//! keyed by the shopkeeper's `stdlib.npcs` id. A command only acts when a
//! shopkeeper with an entry is standing in the actor's room; the keeper is
//! matched through its runtime [`NpcKey`], the same id the converter emits.
//!
//! Pricing is Circle's (`src/shop.c`): the object's `cost` multiplied by the
//! keeper's `profit_buy` when the player buys and `profit_sell` when the
//! player sells, rounded to the nearest coin. The item's `TbaItemDef` supplies
//! the cost and the `kind` the shop checks against its `buy_kinds` bitmask.

use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{
    CarriedBy, Item, ItemDef, ItemId, ItemsSection, LoadedContent, Location, NpcKey, Player, Room,
    Session, SessionState, spawn_item_carried_by_entity,
};

use crate::combat::TbaGold;
use crate::combat::commands::{broadcast, deny, text};
use crate::combat::model::{EVIL_ALIGNMENT, GOOD_ALIGNMENT, TbaAlignment, TbaProgress};
use crate::content::{ShopDef, ShopTrade};
use crate::items::TbaObjectClock;
use crate::module::TbaContent;

/// Circle's closed-shop replies (`shop.h:144-146`), selected the way
/// `is_open` (`shop.c:140`) selects them.
const MSG_NOT_OPEN_YET: &str = "Come back later!";
const MSG_NOT_REOPEN_YET: &str = "Sorry, we have closed, but come back later.";
const MSG_CLOSED_FOR_DAY: &str = "Sorry, come back tomorrow.";
/// Circle's refusal replies (`shop.h:149-150`).
const MSG_NO_SELL_ALIGN: &str = "Get out of here before I call the guards!";
const MSG_NO_SELL_CLASS: &str = "We don't serve your kind here!";

// -- line parsing -----------------------------------------------------------

/// One parsed shop command, before the shop or item is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShopCommand {
    /// `list` — the shop's `produce` with the keeper's buy price.
    List,
    /// `buy <item>` — purchase one of the shop's `produce`.
    Buy(String),
    /// `sell <item>` — sell a carried item the shop accepts.
    Sell(String),
    /// `value <item>` — ask what the shop would pay for a carried item.
    Value(String),
}

/// Parse a raw line as a shop command, or `None` when it is not one.
///
/// The verb is case-insensitive; the argument is the whole trimmed remainder
/// so a multi-word item name (`buy a long sword`) stays intact.
pub fn parse_shop_line(line: &str) -> Option<ShopCommand> {
    let line = line.trim();
    let (verb, rest) = match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index + 1..].trim()),
        None => (line, ""),
    };
    let command = if verb.eq_ignore_ascii_case("list") {
        ShopCommand::List
    } else if verb.eq_ignore_ascii_case("buy") {
        ShopCommand::Buy(rest.to_owned())
    } else if verb.eq_ignore_ascii_case("sell") {
        ShopCommand::Sell(rest.to_owned())
    } else if verb.eq_ignore_ascii_case("value") {
        ShopCommand::Value(rest.to_owned())
    } else {
        return None;
    };
    Some(command)
}

// -- shared helpers ---------------------------------------------------------

/// Case-insensitive `contains` of the typed fragment against a display name,
/// matching the stdlib item core's lookup convention.
fn name_matches(needle: &str, name: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    !needle.is_empty() && name.to_lowercase().contains(&needle)
}

/// Resolve an `stdlib.items` prototype from the loaded pack content.
fn item_def(loaded: Option<&LoadedContent>, id: &ItemId) -> Option<ItemDef> {
    let loaded = loaded?;
    for pack in loaded.0.iter().rev() {
        if let Some(section) = pack.section::<ItemsSection>()
            && let Some(def) = section.items.get(id)
        {
            return Some(def.clone());
        }
    }
    None
}

/// Circle's `round(cost * multiplier)`, saturating on a nonsensical multiplier
/// rather than wrapping. A NaN multiplier prices at zero.
fn shop_price(cost: u32, multiplier: f32) -> u32 {
    let scaled = cost as f32 * multiplier;
    if scaled.is_nan() {
        return 0;
    }
    scaled.round().max(0.0) as u32
}

/// Substitute Circle's two shop-message specifiers: `%s` with the actor's
/// name and `%d` with the coin amount. `%%` is a literal percent; any other
/// `%x` is left untouched, matching `printf`'s behaviour for an unknown
/// conversion rather than silently dropping the text.
fn shop_message(message: &str, player: &str, amount: u32) -> String {
    let mut out = String::with_capacity(message.len() + player.len());
    let mut chars = message.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push_str(player),
            Some('d') => out.push_str(&amount.to_string()),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Render a shop message by index, falling back to `fallback` when the source
/// record carried no message there.
fn shop_says(shop: &ShopDef, index: usize, player: &str, amount: u32, fallback: &str) -> String {
    match shop.messages.get(index) {
        Some(message) if !message.trim().is_empty() => shop_message(message, player, amount),
        _ => fallback.to_owned(),
    }
}

/// Why the shop is closed at `hour`, or `None` when it is open. Mirrors
/// Circle's `is_open` (`shop.c:140-160`) so the reply matches the reason.
fn closed_reason(shop: &ShopDef, hour: u32) -> Option<&'static str> {
    let hours = &shop.hours;
    if hours.open1 > hour {
        return Some(MSG_NOT_OPEN_YET);
    }
    if hours.close1 < hour {
        if hours.open2 > hour {
            return Some(MSG_NOT_REOPEN_YET);
        }
        if hours.close2 < hour {
            return Some(MSG_CLOSED_FOR_DAY);
        }
    }
    None
}

/// Circle's `is_ok_char` (`shop.c:108`): the class/alignment the keeper refuses
/// to serve, if any.
fn trade_refused(
    shop: &ShopDef,
    alignment: Option<&TbaAlignment>,
    progress: Option<&TbaProgress>,
) -> Option<&'static str> {
    let alignment = alignment.map_or(0, |alignment| alignment.0);
    let good = alignment >= GOOD_ALIGNMENT;
    let evil = alignment <= EVIL_ALIGNMENT;
    let neutral = !good && !evil;
    if (good && shop.trade_with.contains(&ShopTrade::Good))
        || (evil && shop.trade_with.contains(&ShopTrade::Evil))
        || (neutral && shop.trade_with.contains(&ShopTrade::Neutral))
    {
        return Some(MSG_NO_SELL_ALIGN);
    }
    let class = progress.map(|progress| progress.class.as_str())?;
    let refused = match class {
        "magic_user" => ShopTrade::MagicUser,
        "cleric" => ShopTrade::Cleric,
        "thief" => ShopTrade::Thief,
        "warrior" => ShopTrade::Warrior,
        _ => return None,
    };
    shop.trade_with
        .contains(&refused)
        .then_some(MSG_NO_SELL_CLASS)
}

/// The acting player's display name, for room broadcasts.
fn actor_name(players: &Query<(Entity, &Player, &Location)>, session: Entity) -> String {
    players
        .iter()
        .find(|(entity, _, _)| *entity == session)
        .map(|(_, player, _)| player.name.clone())
        .unwrap_or_else(|| "Someone".to_owned())
}

/// Find a carried item by a case-insensitive name fragment, returning the
/// entity, its prototype id and its display name.
fn find_carried(
    session: Entity,
    needle: &str,
    items: &Query<(Entity, &Item, &CarriedBy)>,
) -> Option<(Entity, ItemId, String)> {
    items
        .iter()
        .find(|(_, item, carried)| carried.0 == session && name_matches(needle, &item.name))
        .map(|(entity, item, _)| (entity, item.def_id.clone(), item.name.clone()))
}

/// The room an entity is in, when it is a player.
fn player_room(players: &Query<(Entity, &Player, &Location)>, session: Entity) -> Option<Entity> {
    players
        .iter()
        .find(|(entity, _, _)| *entity == session)
        .map(|(_, _, location)| location.room)
}

// -- command handlers -------------------------------------------------------

/// `list`: the shop's stock with the keeper's buy price.
fn shop_list(
    session: Entity,
    shop: &ShopDef,
    content: &TbaContent,
    loaded: Option<&LoadedContent>,
    output: &mut MessageWriter<OutputEvent>,
) {
    let mut lines = Vec::new();
    for id in &shop.produce {
        let Some(def) = item_def(loaded, id) else {
            continue;
        };
        let cost = content.items.get(id).map(|item| item.cost).unwrap_or(0);
        let price = shop_price(cost, shop.buy_multiplier);
        lines.push(format!("{} for {price} coins", def.name));
    }
    let body = if lines.is_empty() {
        "You see nothing for sale.".to_owned()
    } else {
        format!("You can buy:\n{}", lines.join("\n"))
    };
    output.write(text(session, body));
    write_prompt(output, session);
}

/// `value`: what the shop would pay for a carried item.
fn shop_value(
    session: Entity,
    shop: &ShopDef,
    content: &TbaContent,
    items: &Query<(Entity, &Item, &CarriedBy)>,
    needle: &str,
    output: &mut MessageWriter<OutputEvent>,
) {
    if needle.trim().is_empty() {
        deny(output, session, "Value what?");
        return;
    }
    let Some((_, def_id, name)) = find_carried(session, needle, items) else {
        deny(output, session, "You don't have that.");
        return;
    };
    let Some(tba) = content.items.get(&def_id) else {
        deny(output, session, "The shopkeeper isn't interested in that.");
        return;
    };
    if !shop.buy_kinds.contains(&tba.kind) {
        deny(
            output,
            session,
            format!("The shopkeeper doesn't buy {name}."),
        );
        return;
    }
    let price = shop_price(tba.cost, shop.sell_multiplier);
    output.write(text(
        session,
        format!("The shopkeeper will give you {price} coins for {name}."),
    ));
    write_prompt(output, session);
}

/// `buy`: purchase one of the shop's `produce`, carried by the buyer.
#[allow(clippy::too_many_arguments)]
fn shop_buy(
    session: Entity,
    shop: &ShopDef,
    content: &TbaContent,
    loaded: Option<&LoadedContent>,
    needle: &str,
    players: &Query<(Entity, &Player, &Location)>,
    purses: &mut Query<&mut TbaGold>,
    commands: &mut Commands,
    output: &mut MessageWriter<OutputEvent>,
) {
    if needle.trim().is_empty() {
        deny(output, session, "Buy what?");
        return;
    }
    let actor = actor_name(players, session);
    let Some((item_id, def)) = shop.produce.iter().find_map(|id| {
        let def = item_def(loaded, id)?;
        name_matches(needle, &def.name).then(|| (id.clone(), def))
    }) else {
        deny(
            output,
            session,
            shop_says(shop, 0, &actor, 0, "They don't sell that."),
        );
        return;
    };
    let cost = content
        .items
        .get(&item_id)
        .map(|item| item.cost)
        .unwrap_or(0);
    let price = shop_price(cost, shop.buy_multiplier);

    let Ok(mut gold) = purses.get_mut(session) else {
        deny(output, session, "You have no purse.");
        return;
    };
    if gold.0 < price {
        deny(
            output,
            session,
            shop_says(shop, 4, &actor, price, "You can't afford it!"),
        );
        return;
    }
    gold.0 -= price;

    // The spawn needs the world (the stdlib item core), so it is queued rather
    // than built by hand: the prototype, its save id and its placement all come
    // from the one seam every other spawn uses.
    let holder = session;
    let spawn_id = item_id.clone();
    let spawn_def = def.clone();
    commands.queue(move |world: &mut World| {
        spawn_item_carried_by_entity(world, holder, &spawn_id, &spawn_def);
    });

    let name = def.name.clone();
    output.write(text(
        session,
        shop_says(
            shop,
            5,
            &actor,
            price,
            &format!("You buy {name} for {price} coins."),
        ),
    ));
    if let Some(room) = player_room(players, session) {
        broadcast(
            players,
            room,
            session,
            output,
            &format!("{actor} buys {name}."),
        );
    }
    write_prompt(output, session);
}

/// `sell`: sell a carried item the shop accepts, for the keeper's sell price.
#[allow(clippy::too_many_arguments)]
fn shop_sell(
    session: Entity,
    shop: &ShopDef,
    content: &TbaContent,
    items: &Query<(Entity, &Item, &CarriedBy)>,
    needle: &str,
    players: &Query<(Entity, &Player, &Location)>,
    purses: &mut Query<&mut TbaGold>,
    commands: &mut Commands,
    output: &mut MessageWriter<OutputEvent>,
) {
    if needle.trim().is_empty() {
        deny(output, session, "Sell what?");
        return;
    }
    let actor = actor_name(players, session);
    let Some((entity, def_id, name)) = find_carried(session, needle, items) else {
        deny(
            output,
            session,
            shop_says(shop, 1, &actor, 0, "You don't have that."),
        );
        return;
    };
    let Some(tba) = content.items.get(&def_id) else {
        deny(
            output,
            session,
            shop_says(
                shop,
                2,
                &actor,
                0,
                "The shopkeeper isn't interested in that.",
            ),
        );
        return;
    };
    if !shop.buy_kinds.contains(&tba.kind) {
        deny(
            output,
            session,
            shop_says(
                shop,
                2,
                &actor,
                0,
                &format!("The shopkeeper doesn't buy {name}."),
            ),
        );
        return;
    }
    let price = shop_price(tba.cost, shop.sell_multiplier);

    let Ok(mut gold) = purses.get_mut(session) else {
        deny(output, session, "You have no purse.");
        return;
    };
    gold.0 = gold.0.saturating_add(price);
    commands.entity(entity).despawn();

    output.write(text(
        session,
        shop_says(
            shop,
            6,
            &actor,
            price,
            &format!("You sell {name} for {price} coins."),
        ),
    ));
    if let Some(room) = player_room(players, session) {
        broadcast(
            players,
            room,
            session,
            output,
            &format!("{actor} sells {name}."),
        );
    }
    write_prompt(output, session);
}

/// `BridgeSet::Game`: translate `list`/`buy`/`sell`/`value` into trade.
///
/// Only a room containing a shopkeeper NPC whose id names a `tba.shops` entry
/// can trade; any other room denies. The command is resolved before the shop
/// so an unrelated raw line costs nothing.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_shop_commands(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    players: Query<(Entity, &Player, &Location)>,
    alignments: Query<&TbaAlignment>,
    progress: Query<&TbaProgress>,
    npcs: Query<(Entity, &NpcKey, &Location)>,
    rooms: Query<&Room>,
    content: Option<Res<TbaContent>>,
    loaded: Option<Res<LoadedContent>>,
    clock: Option<Res<TbaObjectClock>>,
    mut purses: Query<&mut TbaGold>,
    items: Query<(Entity, &Item, &CarriedBy)>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);
    let loaded = loaded.as_deref();
    let hour = clock.map_or(0, |clock| clock.hour_of_day);

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
        let Some(command) = parse_shop_line(line) else {
            continue;
        };
        let Some(room) = player_room(&players, session) else {
            continue;
        };

        // The shop is wherever its keeper stands, matched by the `NpcKey` the
        // zone reset installed.
        let shop = npcs.iter().find_map(|(_, key, location)| {
            (location.room == room)
                .then(|| content.shops.get(&key.0))
                .flatten()
        });
        let Some(shop) = shop else {
            deny(&mut output, session, "There is no shop here.");
            continue;
        };

        // `SHOP_ROOM`: an empty list means the keeper trades wherever it is;
        // otherwise the player must be in one of the listed rooms
        // (`ok_shop_room`, `shop.c:939`).
        if !shop.rooms.is_empty()
            && !rooms.get(room).ok().is_some_and(|room| {
                shop.rooms
                    .iter()
                    .any(|allowed| allowed.as_ref() == room.key.as_str())
            })
        {
            deny(&mut output, session, "There is no shop here.");
            continue;
        }

        // Circle's `is_open`: a closed keeper explains which close sent you away.
        if let Some(reason) = closed_reason(shop, hour) {
            output.write(text(session, reason));
            write_prompt(&mut output, session);
            continue;
        }

        // `SHOP_TRADE_WITH`: refuse before any trade command is evaluated.
        if let Some(reason) = trade_refused(
            shop,
            alignments.get(session).ok(),
            progress.get(session).ok(),
        ) {
            let actor = actor_name(&players, session);
            deny(&mut output, session, format!("{actor} {reason}"));
            continue;
        }

        match command {
            ShopCommand::List => shop_list(session, shop, content, loaded, &mut output),
            ShopCommand::Value(needle) => {
                shop_value(session, shop, content, &items, &needle, &mut output);
            }
            ShopCommand::Buy(needle) => shop_buy(
                session,
                shop,
                content,
                loaded,
                &needle,
                &players,
                &mut purses,
                &mut commands,
                &mut output,
            ),
            ShopCommand::Sell(needle) => shop_sell(
                session,
                shop,
                content,
                &items,
                &needle,
                &players,
                &mut purses,
                &mut commands,
                &mut output,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parses_each_shop_verb_and_keeps_the_full_argument() {
        assert_eq!(parse_shop_line("list"), Some(ShopCommand::List));
        assert_eq!(
            parse_shop_line("BUY a long sword"),
            Some(ShopCommand::Buy("a long sword".to_owned()))
        );
        assert_eq!(
            parse_shop_line("sell dagger"),
            Some(ShopCommand::Sell("dagger".to_owned()))
        );
        assert_eq!(
            parse_shop_line("  value  brass key  "),
            Some(ShopCommand::Value("brass key".to_owned()))
        );
        assert_eq!(
            parse_shop_line("buy"),
            Some(ShopCommand::Buy(String::new()))
        );
        assert_eq!(parse_shop_line("attack goblin"), None);
        assert_eq!(parse_shop_line("listing"), None);
    }

    #[test]
    fn prices_round_to_the_nearest_coin() {
        // Circle's 50-cost item at profit_buy 1.15 is 57.5 -> 58.
        assert_eq!(shop_price(50, 1.15), 58);
        // profit_sell 0.5 of 50 is 25.
        assert_eq!(shop_price(50, 0.5), 25);
        // Rounding is half-up, and zero/nonsense multipliers are safe.
        assert_eq!(shop_price(3, 0.5), 2);
        assert_eq!(shop_price(100, 0.0), 0);
        assert_eq!(shop_price(100, f32::NAN), 0);
        assert_eq!(shop_price(100, -1.0), 0);
    }

    fn shop() -> ShopDef {
        ShopDef {
            buy_multiplier: 1.0,
            sell_multiplier: 1.0,
            buy_kinds: Vec::new(),
            produce: Vec::new(),
            rooms: Vec::new(),
            trade_with: Vec::new(),
            hours: crate::content::ShopHours::default(),
            messages: Vec::new(),
        }
    }

    #[test]
    fn shop_messages_substitute_name_and_coins() {
        let message = shop_message("%s That'll be %d coins, please.", "Aria", 58);
        assert_eq!(message, "Aria That'll be 58 coins, please.");
        // Literal percent and unknown conversions survive.
        assert_eq!(shop_message("100%% of %s", "Aria", 0), "100% of Aria");
        assert_eq!(shop_message("a %q b", "Aria", 0), "a %q b");

        // A missing message falls back; a present one is used.
        let mut shop = shop();
        assert_eq!(shop_says(&shop, 5, "Aria", 10, "fallback"), "fallback");
        shop.messages = vec![String::new(); 7];
        shop.messages[5] = "%s pays %d.".to_owned();
        assert_eq!(shop_says(&shop, 5, "Aria", 10, "fallback"), "Aria pays 10.");
        assert_eq!(shop_says(&shop, 6, "Aria", 10, "fallback"), "fallback");
    }

    #[test]
    fn closed_reason_matches_circles_is_open() {
        use crate::content::ShopHours;

        // Default = always open.
        let mut shop = shop();
        assert_eq!(closed_reason(&shop, 0), None);
        assert_eq!(closed_reason(&shop, 23), None);

        // Before opening.
        shop.hours = ShopHours {
            open1: 9,
            close1: 17,
            open2: 0,
            close2: 0,
        };
        assert_eq!(closed_reason(&shop, 8), Some(MSG_NOT_OPEN_YET));
        assert_eq!(closed_reason(&shop, 9), None);
        assert_eq!(closed_reason(&shop, 17), None);
        assert_eq!(closed_reason(&shop, 18), Some(MSG_CLOSED_FOR_DAY));

        // Two shifts: the gap reports "not reopened yet".
        shop.hours = ShopHours {
            open1: 8,
            close1: 12,
            open2: 14,
            close2: 20,
        };
        assert_eq!(closed_reason(&shop, 13), Some(MSG_NOT_REOPEN_YET));
        assert_eq!(closed_reason(&shop, 14), None);
        assert_eq!(closed_reason(&shop, 21), Some(MSG_CLOSED_FOR_DAY));
    }

    #[test]
    fn trade_refused_checks_alignment_then_class() {
        use crate::combat::model::{TbaAlignment, TbaProgress};

        let mut shop = shop();
        // No restrictions: everyone is served.
        assert_eq!(trade_refused(&shop, None, None), None);

        shop.trade_with = vec![ShopTrade::Good];
        let good = TbaAlignment(400);
        assert_eq!(
            trade_refused(&shop, Some(&good), None),
            Some(MSG_NO_SELL_ALIGN)
        );
        assert_eq!(trade_refused(&shop, Some(&TbaAlignment(0)), None), None);

        shop.trade_with = vec![ShopTrade::Warrior];
        let warrior = TbaProgress::new("warrior");
        assert_eq!(
            trade_refused(&shop, None, Some(&warrior)),
            Some(MSG_NO_SELL_CLASS)
        );
        let mage = TbaProgress::new("magic_user");
        assert_eq!(trade_refused(&shop, None, Some(&mage)), None);
    }
}
