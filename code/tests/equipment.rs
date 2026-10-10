//! TBA-E28 equipment integration tests.
//!
//! These drive the real equipment systems against a small ECS app (the same
//! shape `tests/combat.rs` uses): `handle_equipment_commands` mutates the
//! runtime map and the persisted shadow, `recompute_equipment_affects` rebuilds
//! the combatant from class/level plus worn affects, and `sync_wielded_weapon`
//! owns the `WieldedWeapon` seam. `restore_equipment_from_save` is included so
//! the SaveId → Entity relink is exercised end to end.
//!
//! Tests may unwrap/expect — a panic here is a test failure, not a fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use mib_core::prelude::*;
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn warrior() -> TbaClassDef {
    TbaClassDef {
        name: "Warrior".to_owned(),
        description: String::new(),
        hp_dice: Dice::new(1, 10, 0),
        mana_dice: None,
        // Empty tables fall back to Circle's level-1 THAC0 (`20`); the tables
        // themselves are asserted in `progression.rs` and `pack.rs`.
        xp_table: Vec::new(),
        thaco_table: Vec::new(),
        prime_attribute: None,
        starting_items: Vec::new(),
        granted_skills: Vec::new(),
    }
}

fn item_def(kind: TbaItemKind, slots: Vec<WearSlot>, affects: Vec<AffectDef>) -> TbaItemDef {
    TbaItemDef {
        kind,
        slots,
        values: [0; 4],
        affects,
        keywords: Vec::new(),
        extra_flags: Vec::new(),
        affect_flags: Vec::new(),
        weight: 0,
        cost: 0,
        rent: 0,
        min_level: 0,
        timer: None,
    }
}

fn weapon_def(dice: [i32; 3]) -> TbaItemDef {
    let mut def = item_def(TbaItemKind::Weapon, vec![WearSlot::Wield], Vec::new());
    def.values = [0, dice[0], dice[1], dice[2]];
    def
}

fn apply(location: AffectLocation, modifier: i32) -> AffectDef {
    AffectDef { location, modifier }
}

fn content_with(items: Vec<(&str, TbaItemDef)>) -> TbaContent {
    let classes = BTreeMap::from([(SmolStr::new("warrior"), warrior())]);
    let items: BTreeMap<ItemId, TbaItemDef> = items
        .into_iter()
        .map(|(id, def)| (ItemId::from(id), def))
        .collect();
    TbaContent {
        classes,
        items,
        ..TbaContent::default()
    }
}

/// The equipment systems wired like the plugin, with the relink pass included.
fn test_app(content: TbaContent) -> App {
    let mut app = App::new();
    app.insert_resource(content);
    app.add_message::<OutputEvent>();
    app.add_message::<CommandReceived>();
    app.add_systems(
        Update,
        (
            handle_equipment_commands,
            restore_equipment_from_save,
            recompute_equipment_affects,
            sync_wielded_weapon,
        )
            .chain(),
    );
    app
}

fn run_game(app: &mut App) {
    app.world_mut().run_schedule(Update);
    app.world_mut().flush();
}

fn spawn_player(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            Player {
                account_id: 1,
                name: "Hero".to_owned(),
            },
            Session {
                state: SessionState::Playing,
            },
            TbaProgress::new("warrior"),
            derive_vitals(&warrior(), 1),
            TbaEquipment::default(),
        ))
        .id()
}

fn spawn_item(app: &mut App, owner: Entity, save_id: &str, def_id: &str, name: &str) -> Entity {
    app.world_mut()
        .spawn((
            Item {
                def_id: ItemId::from(def_id),
                name: name.to_owned(),
                description: String::new(),
                value: None,
                tags: Vec::new(),
            },
            CarriedBy(owner),
            SaveId::new(save_id),
        ))
        .id()
}

fn send_command(app: &mut App, session: Entity, command: Command) {
    app.world_mut()
        .resource_mut::<Messages<CommandReceived>>()
        .write(CommandReceived {
            session,
            session_id: 1,
            input: PlayerInput::Command(command),
        });
}

fn drain_output(app: &mut App) -> Vec<String> {
    app.world_mut()
        .resource_mut::<Messages<OutputEvent>>()
        .drain()
        .filter_map(|event| match event.payload {
            OutboundPayload::Text { body, .. } | OutboundPayload::Error { body, .. } => {
                Some(body.render_plain())
            }
            _ => None,
        })
        .collect()
}

fn combatant_of(app: &App, entity: Entity) -> TbaCombatant {
    app.world()
        .get::<TbaCombatant>(entity)
        .cloned()
        .expect("combatant present")
}

fn equipment_of(app: &App, entity: Entity) -> TbaEquipment {
    let equipment = app
        .world()
        .get::<TbaEquipment>(entity)
        .expect("equipment present");
    TbaEquipment {
        slots: equipment.slots.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn equipping_a_weapon_sets_the_wield_slot_and_wielded_weapon_dice() {
    let mut app = test_app(content_with(vec![("sword", weapon_def([2, 6, 3]))]));
    let player = spawn_player(&mut app);
    let sword = spawn_item(
        &mut app,
        player,
        "item:c:inn:sword:0",
        "sword",
        "a rusty sword",
    );

    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "rusty sword".to_owned(),
        },
    );
    run_game(&mut app);

    assert_eq!(
        equipment_of(&app, player).slots.get(&WearSlot::Wield),
        Some(&sword),
        "the weapon occupies the wield slot"
    );
    let wielded = app
        .world()
        .get::<WieldedWeapon>(player)
        .expect("WieldedWeapon is set for an equipped weapon");
    assert_eq!(wielded.0, ItemId::from("sword"));
    let dice = weapon_dice(Some(wielded), Some(app.world().resource::<TbaContent>()));
    assert_eq!(
        dice,
        Dice::new(2, 6, 3),
        "weapon_dice reads the item values"
    );

    let messages = drain_output(&mut app);
    assert!(
        messages.iter().any(|line| line.contains("You equip")),
        "{messages:?}"
    );
}

#[test]
fn armor_affects_change_armor_class_and_hitroll_then_revert_on_unequip() {
    let armor = item_def(
        TbaItemKind::Armor,
        vec![WearSlot::Body],
        vec![
            apply(AffectLocation::ArmorClass, -12),
            apply(AffectLocation::Hitroll, 2),
            apply(AffectLocation::Damroll, 3),
        ],
    );
    let mut app = test_app(content_with(vec![("plate", armor)]));
    let player = spawn_player(&mut app);
    let base = combatant_of(&app, player);
    let plate = spawn_item(&mut app, player, "item:plate", "plate", "a steel plate");
    assert_eq!(base.armor_class, NAKED_ARMOR_CLASS);

    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "steel plate".to_owned(),
        },
    );
    run_game(&mut app);

    assert_eq!(
        equipment_of(&app, player).slots.get(&WearSlot::Body),
        Some(&plate)
    );
    let equipped = combatant_of(&app, player);
    assert_eq!(equipped.armor_class, base.armor_class - 12);
    assert_eq!(equipped.hitroll, base.hitroll + 2);
    assert_eq!(equipped.damroll, base.damroll + 3);

    send_command(
        &mut app,
        player,
        Command::Unequip {
            slot: "body".to_owned(),
        },
    );
    run_game(&mut app);

    let reverted = combatant_of(&app, player);
    assert_eq!(
        reverted, base,
        "unequip reverts every affect to the class base"
    );
    assert!(
        equipment_of(&app, player).slots.is_empty(),
        "the slot is cleared"
    );
}

#[test]
fn attribute_and_save_affects_accumulate_then_revert_without_stacking() {
    let hat = item_def(
        TbaItemKind::Armor,
        vec![WearSlot::Head],
        vec![
            apply(AffectLocation::Str, 5),
            apply(AffectLocation::Dex, 2),
            apply(AffectLocation::SaveSpell, 2),
            apply(AffectLocation::SavePara, -1),
        ],
    );
    let cloak = item_def(
        TbaItemKind::Armor,
        vec![WearSlot::About],
        vec![
            apply(AffectLocation::Str, 1),
            apply(AffectLocation::SaveSpell, 3),
            apply(AffectLocation::Hitroll, 2),
        ],
    );
    let mut app = test_app(content_with(vec![("hat", hat), ("cloak", cloak)]));
    let player = spawn_player(&mut app);
    let base = combatant_of(&app, player);
    assert_eq!(base.attributes, TbaAttributes::neutral());
    assert_eq!(base.saves, TbaSaves::default());

    spawn_item(&mut app, player, "item:hat", "hat", "a wool hat");
    spawn_item(&mut app, player, "item:cloak", "cloak", "a grey cloak");
    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "wool hat".to_owned(),
        },
    );
    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "grey cloak".to_owned(),
        },
    );
    run_game(&mut app);

    let equipped = combatant_of(&app, player);
    assert_eq!(equipped.attributes.strength, BASE_ATTRIBUTE + 6);
    assert_eq!(equipped.attributes.dexterity, BASE_ATTRIBUTE + 2);
    assert_eq!(equipped.saves.spell, 5, "the two save affects accumulate");
    assert_eq!(equipped.saves.para, -1);
    assert_eq!(equipped.hitroll, 2);
    assert_eq!(
        equipped.attributes.to_hit_bonus(),
        3,
        "str 19 sits on str_app's +3 to-hit row"
    );

    // Repeated recomputes rebuild from the neutral base, so nothing stacks.
    for _ in 0..3 {
        run_game(&mut app);
    }
    assert_eq!(combatant_of(&app, player), equipped);

    send_command(
        &mut app,
        player,
        Command::Unequip {
            slot: "head".to_owned(),
        },
    );
    send_command(
        &mut app,
        player,
        Command::Unequip {
            slot: "about".to_owned(),
        },
    );
    run_game(&mut app);
    assert_eq!(
        combatant_of(&app, player),
        base,
        "unequipping reverts the attributes and saves to the base"
    );
}

#[test]
fn unequipping_a_weapon_clears_wielded_weapon() {
    let mut app = test_app(content_with(vec![("sword", weapon_def([1, 8, 1]))]));
    let player = spawn_player(&mut app);
    let _sword = spawn_item(&mut app, player, "item:sword", "sword", "a broadsword");

    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "broadsword".to_owned(),
        },
    );
    run_game(&mut app);
    assert!(app.world().get::<WieldedWeapon>(player).is_some());

    send_command(
        &mut app,
        player,
        Command::Unequip {
            slot: "main hand".to_owned(),
        },
    );
    run_game(&mut app);

    assert!(
        app.world().get::<WieldedWeapon>(player).is_none(),
        "the wielded seam is cleared"
    );
    let dice = weapon_dice(None, Some(app.world().resource::<TbaContent>()));
    assert_eq!(dice, Dice::new(1, 4, 0), "combat falls back to unarmed");
}

#[test]
fn equip_denies_unknown_not_carried_and_slotless_items() {
    let mut app = test_app(content_with(vec![
        ("sword", weapon_def([1, 6, 0])),
        (
            "rock",
            item_def(TbaItemKind::Treasure, Vec::new(), Vec::new()),
        ),
    ]));
    let player = spawn_player(&mut app);
    let other = app.world_mut().spawn_empty().id();
    // Carried by someone else, plus a slotless carried item.
    spawn_item(&mut app, other, "item:sword", "sword", "a loaned sword");
    spawn_item(&mut app, player, "item:rock", "rock", "a plain rock");

    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "banana".to_owned(),
        },
    );
    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "loaned sword".to_owned(),
        },
    );
    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "rock".to_owned(),
        },
    );
    run_game(&mut app);

    let messages = drain_output(&mut app);
    assert!(
        messages
            .iter()
            .filter(|line| line.contains("You aren't carrying that."))
            .count()
            >= 2,
        "unknown and not-carried both deny: {messages:?}"
    );
    assert!(
        messages.iter().any(|line| line.contains("You can't equip")),
        "a slotless item is denied: {messages:?}"
    );
    assert!(equipment_of(&app, player).slots.is_empty());
}

#[test]
fn paired_slots_fill_both_sides() {
    let ring = item_def(
        TbaItemKind::Armor,
        vec![WearSlot::FingerR],
        vec![apply(AffectLocation::Hitroll, 1)],
    );
    let mut app = test_app(content_with(vec![("ring", ring)]));
    let player = spawn_player(&mut app);
    let first = spawn_item(&mut app, player, "item:ring:1", "ring", "a copper ring");
    let second = spawn_item(&mut app, player, "item:ring:2", "ring", "a silver ring");

    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "copper".to_owned(),
        },
    );
    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "silver".to_owned(),
        },
    );
    run_game(&mut app);

    let slots = equipment_of(&app, player).slots;
    assert_eq!(slots.get(&WearSlot::FingerR), Some(&first));
    assert_eq!(
        slots.get(&WearSlot::FingerL),
        Some(&second),
        "the second ring fills the paired slot"
    );
}

#[test]
fn equipment_lists_worn_items_by_slot() {
    let mut app = test_app(content_with(vec![
        ("sword", weapon_def([1, 6, 0])),
        (
            "cap",
            item_def(TbaItemKind::Armor, vec![WearSlot::Head], Vec::new()),
        ),
    ]));
    let player = spawn_player(&mut app);
    spawn_item(&mut app, player, "item:sword", "sword", "a short sword");
    spawn_item(&mut app, player, "item:cap", "cap", "a leather cap");

    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "short sword".to_owned(),
        },
    );
    send_command(
        &mut app,
        player,
        Command::Equip {
            item: "leather cap".to_owned(),
        },
    );
    send_command(&mut app, player, Command::Equipment);
    run_game(&mut app);

    let messages = drain_output(&mut app);
    let listing = messages
        .iter()
        .find(|line| line.contains("You are wearing:"))
        .expect("an equipment listing");
    assert!(listing.contains("head: a leather cap"), "{listing}");
    assert!(listing.contains("wield: a short sword"), "{listing}");
}

#[test]
fn save_round_trips_and_relinks_live_entities() {
    let mut app = test_app(content_with(vec![("sword", weapon_def([1, 8, 0]))]));
    let player = spawn_player(&mut app);
    let sword = spawn_item(
        &mut app,
        player,
        "item:c:inn:sword:0",
        "sword",
        "a keeper sword",
    );

    let mut save = TbaEquipmentSave::default();
    save.worn
        .insert(WearSlot::Wield, "item:c:inn:sword:0".to_owned());
    save.worn
        .insert(WearSlot::Body, "item:c:inn:gone:0".to_owned());
    let json = serde_json::to_string(&save).unwrap();
    let restored: TbaEquipmentSave = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, save, "the persisted shadow round-trips");

    // Simulate a login: the bundle applies the save before the runtime map
    // exists, and one item's save id no longer resolves.
    app.world_mut().entity_mut(player).remove::<TbaEquipment>();
    app.world_mut().entity_mut(player).insert(restored);
    run_game(&mut app);
    // The relink inserts `TbaEquipment` through commands; give the follow-up
    // systems a frame to observe it.
    run_game(&mut app);

    let slots = equipment_of(&app, player).slots;
    assert_eq!(
        slots.get(&WearSlot::Wield),
        Some(&sword),
        "the live save id reconnects to its entity"
    );
    assert!(
        !slots.contains_key(&WearSlot::Body),
        "a dangling save id is dropped, never dangling"
    );
    assert!(
        app.world().get::<WieldedWeapon>(player).is_some(),
        "the relink re-inserts WieldedWeapon for the wield slot"
    );
}
