//! TBA-E17 shop / economy integration tests.
//!
//! These drive the real [`handle_shop_commands`] system against a small ECS app
//! (the same shape `tests/spells.rs` uses), with a shopkeeper NPC, a player
//! purse of [`TbaGold`], and the `stdlib.items` prototypes resolved through a
//! [`LoadedContent`] resource. They cover the required semantics: `list` prices
//! the stock, `buy` deducts gold and spawns a carried item, an unaffordable
//! `buy` changes nothing, `sell` adds gold and removes the item, `value`/`sell`
//! of a kind the shop does not buy is denied, and every command outside a shop
//! room is denied.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use mib_content::prelude::{PackAsset, PackManifest, PackSection};
use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const SWORD: &str = "tba_item_3000";
const VEST: &str = "tba_item_3001";
const GEM: &str = "tba_item_4000";
const SHOPKEEPER: &str = "tba_npc_3000";

fn tba_item(kind: TbaItemKind, cost: u32) -> TbaItemDef {
    TbaItemDef {
        kind,
        slots: Vec::new(),
        values: [0; 4],
        affects: Vec::new(),
        keywords: Vec::new(),
        extra_flags: Vec::new(),
        affect_flags: Vec::new(),
        weight: 1,
        cost,
        rent: 0,
        min_level: 0,
        timer: None,
    }
}

fn content() -> TbaContent {
    let items = BTreeMap::from([
        (ItemId::from(SWORD), tba_item(TbaItemKind::Weapon, 50)),
        (ItemId::from(VEST), tba_item(TbaItemKind::Armor, 20)),
        (ItemId::from(GEM), tba_item(TbaItemKind::Treasure, 100)),
    ]);
    let shops = BTreeMap::from([(
        NpcId::from(SHOPKEEPER),
        ShopDef {
            buy_multiplier: 1.15,
            sell_multiplier: 0.5,
            // Buys weapons and armour only; the gem is deliberately unwanted.
            buy_kinds: vec![TbaItemKind::Weapon, TbaItemKind::Armor],
            produce: vec![ItemId::from(SWORD), ItemId::from(VEST)],
            rooms: Vec::new(),
            trade_with: Vec::new(),
            hours: ShopHours::default(),
            messages: Vec::new(),
        },
    )]);
    TbaContent {
        items,
        shops,
        ..TbaContent::default()
    }
}

fn stdlib_item(name: &str) -> ItemDef {
    ItemDef {
        name: name.to_owned(),
        description: format!("A test {name}."),
        value: Some(1),
        container: None,
        tags: Vec::new(),
    }
}

/// A one-pack `LoadedContent` carrying the `stdlib.items` prototypes the shop
/// system resolves names and spawn defs from.
fn loaded() -> LoadedContent {
    let items = BTreeMap::from([
        (ItemId::from(SWORD), stdlib_item("a long sword")),
        (ItemId::from(VEST), stdlib_item("a leather vest")),
        (ItemId::from(GEM), stdlib_item("a ruby gem")),
    ]);
    let sections: BTreeMap<&'static str, Box<dyn std::any::Any + Send + Sync>> =
        BTreeMap::from([(
            ItemsSection::ID,
            Box::new(ItemsFile { items }) as Box<dyn std::any::Any + Send + Sync>,
        )]);
    LoadedContent(vec![PackAsset {
        manifest: PackManifest {
            id: "shop-test".into(),
            version: "1".to_owned(),
            title: "shop test".to_owned(),
            game: None,
            depends_on: Vec::new(),
            content_root: "content".to_owned(),
            requires_mib: None,
            code: None,
        },
        sections,
        section_files: BTreeMap::new(),
        root: PathBuf::from("/shop-test"),
    }])
}

fn test_app() -> App {
    let mut app = App::new();
    app.insert_resource(content());
    app.insert_resource(loaded());
    app.add_message::<OutputEvent>();
    app.add_message::<CommandReceived>();
    app.add_systems(Update, handle_shop_commands);
    app
}

fn spawn_room(app: &mut App) -> Entity {
    app.world_mut().spawn_empty().id()
}

fn spawn_player(app: &mut App, room: Entity, gold: u32) -> Entity {
    app.world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: "Shopper".to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            Location { room },
            TbaGold(gold),
        ))
        .id()
}

fn spawn_shopkeeper(app: &mut App, room: Entity) -> Entity {
    app.world_mut()
        .spawn((
            Npc {
                name: "the shopkeeper".to_owned(),
                persona: String::new(),
            },
            Location { room },
            NpcKey(NpcId::from(SHOPKEEPER)),
        ))
        .id()
}

fn spawn_carried(app: &mut App, holder: Entity, item: &str) -> Entity {
    spawn_item_carried_from_content(app.world_mut(), holder, &ItemId::from(item))
        .expect("item def resolves and the holder is live")
}

fn command(app: &mut App, session: Entity, line: &str) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            id: CommandId::default(),
            session,
            session_id: 1,
            input: PlayerInput::RawLine(line.to_owned()),
        });
}

fn run(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn text(app: &mut App) -> String {
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .filter_map(|event| match event.payload {
            OutboundPayload::Text { body, .. } | OutboundPayload::Error { body, .. } => {
                Some(body.render_plain())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn gold_of(app: &App, entity: Entity) -> u32 {
    app.world()
        .get::<TbaGold>(entity)
        .map(|gold| gold.0)
        .unwrap_or(0)
}

fn carried_ids(app: &mut App, holder: Entity) -> Vec<ItemId> {
    app.world_mut()
        .query::<(&Item, &CarriedBy)>()
        .iter(app.world())
        .filter(|(_, carried)| carried.0 == holder)
        .map(|(item, _)| item.def_id.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

#[test]
fn list_shows_the_stock_and_the_buy_prices() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 0);
    spawn_shopkeeper(&mut app, room);

    command(&mut app, player, "list");
    run(&mut app);

    let shown = text(&mut app);
    // 50 * 1.15 = 57.5 -> 58; 20 * 1.15 = 23.
    assert!(shown.contains("a long sword"), "{shown}");
    assert!(shown.contains("58 coins"), "{shown}");
    assert!(shown.contains("a leather vest"), "{shown}");
    assert!(shown.contains("23 coins"), "{shown}");
}

#[test]
fn buy_deducts_gold_and_spawns_a_carried_item() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 100);
    // A witness in the same room receives the broadcast line.
    let _witness = spawn_player(&mut app, room, 0);
    spawn_shopkeeper(&mut app, room);

    command(&mut app, player, "buy a long sword");
    run(&mut app);

    assert_eq!(gold_of(&app, player), 42, "100 - round(50 * 1.15)");
    assert_eq!(carried_ids(&mut app, player), vec![ItemId::from(SWORD)]);
    let shown = text(&mut app);
    assert!(
        shown.contains("You buy a long sword for 58 coins"),
        "{shown}"
    );
    assert!(shown.contains("Shopper buys a long sword"), "{shown}");
}

#[test]
fn buy_with_insufficient_gold_is_denied_and_changes_nothing() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 10);
    spawn_shopkeeper(&mut app, room);

    command(&mut app, player, "buy a long sword");
    run(&mut app);

    assert_eq!(gold_of(&app, player), 10, "purse is untouched");
    assert!(carried_ids(&mut app, player).is_empty(), "nothing spawned");
    let shown = text(&mut app);
    assert!(shown.contains("afford"), "{shown}");
}

#[test]
fn buy_of_an_unstocked_item_is_denied() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 1000);
    spawn_shopkeeper(&mut app, room);

    command(&mut app, player, "buy a ruby gem");
    run(&mut app);

    assert_eq!(gold_of(&app, player), 1000);
    assert!(carried_ids(&mut app, player).is_empty());
    let shown = text(&mut app);
    assert!(shown.contains("don't sell"), "{shown}");
}

#[test]
fn value_quotes_the_sell_price() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 0);
    spawn_shopkeeper(&mut app, room);
    spawn_carried(&mut app, player, SWORD);

    command(&mut app, player, "value long sword");
    run(&mut app);

    // round(50 * 0.5) = 25.
    let shown = text(&mut app);
    assert!(shown.contains("25 coins for a long sword"), "{shown}");
    assert_eq!(gold_of(&app, player), 0, "value does not pay out");
}

#[test]
fn sell_adds_gold_and_removes_the_item() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 0);
    spawn_shopkeeper(&mut app, room);
    spawn_carried(&mut app, player, SWORD);

    command(&mut app, player, "sell sword");
    run(&mut app);

    assert_eq!(gold_of(&app, player), 25, "round(50 * 0.5)");
    assert!(
        carried_ids(&mut app, player).is_empty(),
        "the sword left the inventory"
    );
    let shown = text(&mut app);
    assert!(
        shown.contains("You sell a long sword for 25 coins"),
        "{shown}"
    );
}

#[test]
fn sell_and_value_of_an_unwanted_kind_are_denied() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 0);
    spawn_shopkeeper(&mut app, room);
    spawn_carried(&mut app, player, GEM);

    command(&mut app, player, "value ruby gem");
    run(&mut app);
    command(&mut app, player, "sell ruby gem");
    run(&mut app);

    let shown = text(&mut app);
    assert!(shown.contains("doesn't buy"), "{shown}");
    assert_eq!(gold_of(&app, player), 0, "no sale, no gold");
    assert_eq!(
        carried_ids(&mut app, player),
        vec![ItemId::from(GEM)],
        "the unwanted gem is untouched"
    );
}

#[test]
fn commands_outside_a_shop_are_denied() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 100);

    for line in [
        "list",
        "buy a long sword",
        "sell a long sword",
        "value a long sword",
    ] {
        command(&mut app, player, line);
        run(&mut app);
    }

    let shown = text(&mut app);
    assert_eq!(
        shown.matches("There is no shop here.").count(),
        4,
        "every trade verb is denied in a shopless room: {shown}"
    );
    assert_eq!(gold_of(&app, player), 100);
    assert!(carried_ids(&mut app, player).is_empty());
}

#[test]
fn closed_shop_refuses_trade_and_names_the_close() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 100);
    spawn_shopkeeper(&mut app, room);
    {
        let mut content = app.world_mut().resource_mut::<TbaContent>();
        let shop = content
            .shops
            .get_mut(&NpcId::from(SHOPKEEPER))
            .expect("shop is installed");
        shop.hours = ShopHours {
            open1: 9,
            close1: 17,
            open2: 0,
            close2: 0,
        };
    }
    app.insert_resource(TimeOfDay {
        hour: 20,
        ..TimeOfDay::default()
    });

    command(&mut app, player, "buy a long sword");
    run(&mut app);

    assert_eq!(gold_of(&app, player), 100, "no trade after hours");
    assert!(carried_ids(&mut app, player).is_empty());
    assert!(
        text(&mut app).contains("come back tomorrow"),
        "closed reply"
    );
}

#[test]
fn room_restriction_refuses_trade() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 100);
    spawn_shopkeeper(&mut app, room);
    {
        let mut content = app.world_mut().resource_mut::<TbaContent>();
        let shop = content
            .shops
            .get_mut(&NpcId::from(SHOPKEEPER))
            .expect("shop is installed");
        // The player's (empty) room cannot match a restricted vnum.
        shop.rooms = vec![RoomId::from("tba.9999")];
    }

    command(&mut app, player, "list");
    run(&mut app);

    assert!(text(&mut app).contains("There is no shop here."));
}

#[test]
fn trade_restriction_refuses_a_refused_class() {
    let mut app = test_app();
    let room = spawn_room(&mut app);
    let player = spawn_player(&mut app, room, 100);
    app.world_mut()
        .entity_mut(player)
        .insert(TbaProgress::new("warrior"));
    spawn_shopkeeper(&mut app, room);
    {
        let mut content = app.world_mut().resource_mut::<TbaContent>();
        let shop = content
            .shops
            .get_mut(&NpcId::from(SHOPKEEPER))
            .expect("shop is installed");
        shop.trade_with = vec![ShopTrade::Warrior];
    }

    command(&mut app, player, "buy a long sword");
    run(&mut app);

    assert_eq!(gold_of(&app, player), 100, "refused class cannot buy");
    let shown = text(&mut app);
    assert!(shown.contains("We don't serve your kind here!"), "{shown}");
}
