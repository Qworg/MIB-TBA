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
    CarriedBy, Item, ItemDef, ItemId, ItemsSection, LoadedContent, Location, NpcId, NpcKey, Player,
    Room, SessionState, spawn_item_carried_by_entity,
};

use crate::combat::TbaGold;
use crate::combat::commands::{broadcast, deny, text};
use crate::combat::model::{EVIL_ALIGNMENT, GOOD_ALIGNMENT, TbaAlignment, TbaProgress};
use crate::content::{ShopDef, ShopTrade};
use crate::dg::intake::commands::{DgClaimed, is_claimed};
use crate::module::TbaContent;
use crate::views::ActorView;
use mib_stdlib::prelude::TimeOfDay;

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

/// A shop interaction the plan resolved.
///
/// The plan answers *what* would happen — which shop, which item, at what
/// price, or which refusal to say — and the act carries it out. The plan is a
/// plain value: it names no query, so the act needs none to decide anything.
#[derive(Debug, Clone)]
enum ShopPlan {
    /// A refusal, with the line the player sees and whether it is an error
    /// (Circle's `send_to_char` versus the pack's `deny`).
    Refused { line: String, error: bool },
    /// `list`: the keeper's stock and its buy prices.
    List { stock: Vec<(String, u32)> },
    /// `buy <item>`: the prototype, its definition and the price.
    Buy {
        shop: NpcId,
        item: ItemId,
        def: ItemDef,
        price: u32,
    },
    /// `sell <item>`: the carried item, its name and the price.
    Sell {
        shop: NpcId,
        entity: Entity,
        name: String,
        price: u32,
    },
    /// `value <item>`: the carried item's name and what it is worth.
    Value { name: String, price: u32 },
}

/// Resolve a shop command into the interaction it would perform.
///
/// Everything the command *decides* — the shop, whether it trades here and
/// now, the item, the price, and any refusal — is settled here. The act
/// receives a plain value.
#[allow(clippy::too_many_arguments)]
fn plan_shop(
    session: Entity,
    command: &ShopCommand,
    content: &TbaContent,
    loaded: Option<&LoadedContent>,
    hour: u32,
    players: &Query<(Entity, &Player, &Location)>,
    progress: &Query<&TbaProgress>,
    alignments: &Query<&TbaAlignment>,
    npcs: &Query<(Entity, &NpcKey, &Location)>,
    rooms: &Query<&Room>,
    items: &Query<(Entity, &Item, &CarriedBy)>,
) -> Option<ShopPlan> {
    let room = player_room(players, session)?;

    // The shop is wherever its keeper stands, matched by the `NpcKey` the
    // zone reset installed.
    let shop = npcs.iter().find_map(|(_, key, location)| {
        (location.room == room)
            .then(|| content.shops.get(&key.0).map(|def| (key.0.clone(), def)))
            .flatten()
    });
    let Some((shop_id, shop)) = shop else {
        return Some(ShopPlan::Refused {
            line: "There is no shop here.".to_owned(),
            error: true,
        });
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
        return Some(ShopPlan::Refused {
            line: "There is no shop here.".to_owned(),
            error: true,
        });
    }

    // Circle's `is_open`: a closed keeper explains which close sent you away.
    if let Some(reason) = closed_reason(shop, hour) {
        return Some(ShopPlan::Refused {
            line: reason.to_owned(),
            error: false,
        });
    }

    // `SHOP_TRADE_WITH`: refuse before any trade command is evaluated.
    if let Some(reason) = trade_refused(
        shop,
        alignments.get(session).ok(),
        progress.get(session).ok(),
    ) {
        let actor = actor_name(players, session);
        return Some(ShopPlan::Refused {
            line: format!("{actor} {reason}"),
            error: true,
        });
    }

    let actor = actor_name(players, session);
    match command {
        ShopCommand::List => {
            let mut stock = Vec::new();
            for id in &shop.produce {
                let Some(def) = item_def(loaded, id) else {
                    continue;
                };
                let cost = content.items.get(id).map(|item| item.cost).unwrap_or(0);
                let price = shop_price(cost, shop.buy_multiplier);
                stock.push((def.name.clone(), price));
            }
            Some(ShopPlan::List { stock })
        }
        ShopCommand::Value(needle) => {
            if needle.trim().is_empty() {
                return Some(ShopPlan::Refused {
                    line: "Value what?".to_owned(),
                    error: true,
                });
            }
            let Some((_entity, def_id, name)) = find_carried(session, needle, items) else {
                return Some(ShopPlan::Refused {
                    line: "You don't have that.".to_owned(),
                    error: true,
                });
            };
            let Some(tba) = content.items.get(&def_id) else {
                return Some(ShopPlan::Refused {
                    line: "The shopkeeper isn't interested in that.".to_owned(),
                    error: true,
                });
            };
            if !shop.buy_kinds.contains(&tba.kind) {
                return Some(ShopPlan::Refused {
                    line: format!("The shopkeeper doesn't buy {name}."),
                    error: true,
                });
            }
            let price = shop_price(tba.cost, shop.sell_multiplier);
            Some(ShopPlan::Value { name, price })
        }
        ShopCommand::Buy(needle) => {
            if needle.trim().is_empty() {
                return Some(ShopPlan::Refused {
                    line: "Buy what?".to_owned(),
                    error: true,
                });
            }
            let Some((item_id, def)) = shop.produce.iter().find_map(|id| {
                let def = item_def(loaded, id)?;
                name_matches(needle, &def.name).then(|| (id.clone(), def))
            }) else {
                return Some(ShopPlan::Refused {
                    line: shop_says(shop, 0, &actor, 0, "They don't sell that."),
                    error: true,
                });
            };
            let cost = content
                .items
                .get(&item_id)
                .map(|item| item.cost)
                .unwrap_or(0);
            let price = shop_price(cost, shop.buy_multiplier);
            Some(ShopPlan::Buy {
                shop: shop_id,
                item: item_id,
                def,
                price,
            })
        }
        ShopCommand::Sell(needle) => {
            if needle.trim().is_empty() {
                return Some(ShopPlan::Refused {
                    line: "Sell what?".to_owned(),
                    error: true,
                });
            }
            let Some((entity, def_id, name)) = find_carried(session, needle, items) else {
                return Some(ShopPlan::Refused {
                    line: shop_says(shop, 1, &actor, 0, "You don't have that."),
                    error: true,
                });
            };
            let Some(tba) = content.items.get(&def_id) else {
                return Some(ShopPlan::Refused {
                    line: shop_says(
                        shop,
                        2,
                        &actor,
                        0,
                        "The shopkeeper isn't interested in that.",
                    ),
                    error: true,
                });
            };
            if !shop.buy_kinds.contains(&tba.kind) {
                return Some(ShopPlan::Refused {
                    line: shop_says(
                        shop,
                        2,
                        &actor,
                        0,
                        &format!("The shopkeeper doesn't buy {name}."),
                    ),
                    error: true,
                });
            }
            let price = shop_price(tba.cost, shop.sell_multiplier);
            Some(ShopPlan::Sell {
                shop: shop_id,
                entity,
                name,
                price,
            })
        }
    }
}

/// Carry out a resolved shop interaction.
fn apply_shop_plan(
    session: Entity,
    plan: &ShopPlan,
    content: &TbaContent,
    players: &Query<(Entity, &Player, &Location)>,
    purses: &mut Query<&mut TbaGold>,
    commands: &mut Commands,
    output: &mut MessageWriter<OutputEvent>,
) {
    match plan {
        ShopPlan::Refused { line, error } => {
            if *error {
                deny(output, session, line.clone());
            } else {
                output.write(text(session, line.clone()));
                write_prompt(output, session);
            }
        }
        ShopPlan::List { stock } => {
            let body = if stock.is_empty() {
                "You see nothing for sale.".to_owned()
            } else {
                let lines: Vec<String> = stock
                    .iter()
                    .map(|(name, price)| format!("{name} for {price} coins"))
                    .collect();
                format!("You can buy:\n{}", lines.join("\n"))
            };
            output.write(text(session, body));
            write_prompt(output, session);
        }
        ShopPlan::Buy {
            shop,
            item,
            def,
            price,
        } => {
            let actor = actor_name(players, session);
            let Ok(mut gold) = purses.get_mut(session) else {
                deny(output, session, "You have no purse.");
                return;
            };
            let keeper = content.shops.get(shop);
            if gold.0 < *price {
                let line = keeper.map_or_else(
                    || "You can't afford it!".to_owned(),
                    |keeper| shop_says(keeper, 4, &actor, *price, "You can't afford it!"),
                );
                deny(output, session, line);
                return;
            }
            gold.0 -= *price;

            // The spawn needs the world (the stdlib item core), so it is queued
            // rather than built by hand: the prototype, its save id and its
            // placement all come from the one seam every other spawn uses.
            let holder = session;
            let spawn_id = item.clone();
            let spawn_def = def.clone();
            commands.queue(move |world: &mut World| {
                spawn_item_carried_by_entity(world, holder, &spawn_id, &spawn_def);
            });

            let name = def.name.clone();
            let line = keeper.map_or_else(
                || format!("You buy {name} for {price} coins."),
                |keeper| {
                    shop_says(
                        keeper,
                        5,
                        &actor,
                        *price,
                        &format!("You buy {name} for {price} coins."),
                    )
                },
            );
            output.write(text(session, line));
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
        ShopPlan::Sell {
            shop,
            entity,
            name,
            price,
        } => {
            let actor = actor_name(players, session);
            let Ok(mut gold) = purses.get_mut(session) else {
                deny(output, session, "You have no purse.");
                return;
            };
            gold.0 = gold.0.saturating_add(*price);
            commands.entity(*entity).despawn();

            let line = content.shops.get(shop).map_or_else(
                || format!("You sell {name} for {price} coins."),
                |keeper| {
                    shop_says(
                        keeper,
                        6,
                        &actor,
                        *price,
                        &format!("You sell {name} for {price} coins."),
                    )
                },
            );
            output.write(text(session, line));
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
        ShopPlan::Value { name, price } => {
            output.write(text(
                session,
                format!("The shopkeeper will give you {price} coins for {name}."),
            ));
            write_prompt(output, session);
        }
    }
}

/// `BridgeSet::Game`: translate `list`/`buy`/`sell`/`value` into trade.
///
/// Only a room containing a shopkeeper NPC whose id names a `tba.shops` entry
/// can trade; any other room denies. The command is resolved before the shop
/// so an unrelated raw line costs nothing.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_shop_commands(
    mut reader: MessageReader<CommandReceived>,
    claimed: Option<Res<DgClaimed>>,
    actors: ActorView<'_, '_>,
    alignments: Query<&TbaAlignment>,
    npcs: Query<(Entity, &NpcKey, &Location)>,
    rooms: Query<&Room>,
    content: Option<Res<TbaContent>>,
    loaded: Option<Res<LoadedContent>>,
    clock: Option<Res<TimeOfDay>>,
    mut purses: Query<&mut TbaGold>,
    items: Query<(Entity, &Item, &CarriedBy)>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);
    let loaded = loaded.as_deref();
    let hour = clock.map_or(0, |clock| u32::from(clock.hour));
    // The view is this system's own access; the helpers below keep taking the
    // individual queries they need.
    let ActorView {
        sessions,
        players,
        progress,
        carried: _,
    } = &actors;

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
        let Some(command) = parse_shop_line(line) else {
            continue;
        };
        // The plan settles every decision: the shop, whether it trades here
        // and now, the item, the price, and any refusal.
        let Some(plan) = plan_shop(
            session,
            &command,
            content,
            loaded,
            hour,
            players,
            progress,
            &alignments,
            &npcs,
            &rooms,
            &items,
        ) else {
            continue;
        };
        apply_shop_plan(
            session,
            &plan,
            content,
            players,
            &mut purses,
            &mut commands,
            &mut output,
        );
    }
}
