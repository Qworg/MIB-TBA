//! TBA-E09 end-to-end: the death penalty's gold pile reaches the corpse.
//!
//! In the real game [`apply_death_penalty`](mib_tba::prelude::apply_death_penalty)
//! is ordered `.before(handle_player_death)` inside `BridgeSet::Game` (see
//! `mib_tba::module` and `tests/module.rs`'s graph test). This suite wires the
//! same two systems with an explicit order edge and drives them through a real
//! `Update`, then checks the stdlib corpse pass collected the pile this system
//! spawned.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use mib_core::prelude::*;
use mib_protocol::prelude::FormattedText;
use mib_stdlib::content::{RoomTag, Sector};
use mib_stdlib::prelude::*;
use mib_tba::prelude::*;
use smol_str::SmolStr;

fn test_room(key: &str, tags: Vec<RoomTag>) -> Room {
    Room {
        key: key.into(),
        name: key.into(),
        description: FormattedText::plain("A test room."),
        num: 0,
        area: "test".into(),
        coord: (0, 0, 0),
        tags: tags.into_iter().collect(),
        media: Vec::new(),
        sector: Sector::Inside,
        extra_descriptions: Vec::new(),
    }
}

fn no_exits() -> Exits {
    Exits {
        map: HashMap::new(),
        hidden: BTreeSet::new(),
    }
}

#[test]
fn death_penalty_gold_moves_into_the_corpse() {
    let mut app = App::new();
    app.add_message::<PlayerDefeated>();
    app.add_message::<OutputEvent>();

    let start = app
        .world_mut()
        .spawn((test_room("sanctuary", vec![RoomTag::StartRoom]), no_exits()))
        .id();
    let death_room = app
        .world_mut()
        .spawn((test_room("dungeon", Vec::new()), no_exits()))
        .id();
    app.insert_resource(RoomIndex {
        start: Some(start),
        ..Default::default()
    });
    app.insert_resource(TbaContent {
        classes: BTreeMap::from([(SmolStr::new("warrior"), default_class())]),
        ..TbaContent::default()
    });

    let player = app
        .world_mut()
        .spawn((
            Player {
                account_id: 7,
                name: "Hero".into(),
            },
            Location { room: death_room },
            TbaProgress {
                class: SmolStr::new("warrior"),
                level: 2,
                xp: 2400,
            },
            TbaGold(41),
        ))
        .id();

    // The real composition orders `apply_death_penalty` before the stdlib
    // corpse pass; `.chain()` is that same edge for this isolated app.
    app.add_systems(Update, (apply_death_penalty, handle_player_death).chain());

    app.world_mut().write_message(PlayerDefeated { player });
    app.update();

    // 1. XP penalty: level-2 warrior threshold is 2000, so 400 earned this
    //    level costs 100.
    assert_eq!(app.world().get::<TbaProgress>(player).unwrap().xp, 2300);

    // 2. The purse is emptied and the pile now lives in the corpse.
    assert_eq!(app.world().get::<TbaGold>(player).unwrap().0, 0);

    let gold: Vec<Entity> = app
        .world_mut()
        .query::<(Entity, &Item)>()
        .iter(app.world())
        .filter(|(_, item)| item.def_id == ItemId::from(GOLD_ITEM_ID))
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(gold.len(), 1, "exactly one gold pile");
    let pile = gold[0];
    assert_eq!(
        app.world().get::<Item>(pile).unwrap().value,
        Some(41),
        "the pile carries the amount"
    );
    let corpse = app
        .world()
        .get::<ContainedBy>(pile)
        .expect("the stdlib sweep moved the pile into the corpse");
    let corpse_item = app
        .world()
        .get::<Item>(corpse.0)
        .expect("corpse is an item");
    assert!(
        corpse_item.name.contains("the corpse of Hero"),
        "corpse names the dead player: {}",
        corpse_item.name
    );

    // 3. The player respawned at the sanctuary.
    assert_eq!(app.world().get::<Location>(player).unwrap().room, start);
}
