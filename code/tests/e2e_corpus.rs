//! Corpus smoke: fire every trigger record in the real content once, in-game,
//! and check that the host's unmodelled keys are documented.
//!
//! Tests may unwrap/expect.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod playtest;

use dg_vm::Script;
use dg_vm::driver::{Mode, script_driver};
use mib_core::prelude::*;
use mib_tba::dg::script::{DgProtos, DgScript, DgScriptInit};
use mib_tba::dg::{DgLog, with_host};
use mib_tba::prelude::*;

/// The host gaps this corpus exercises, each a documented unmodelled fact:
/// `magic_affect_bit` (an `AFF_*` bit the pack has no component for),
/// `char_sex` / `char_weight` / `char_pref_flags` (not modelled),
/// `char_npc_no_combat_state` / `char_set_stat_no_combat_state` (an NPC read
/// or written before its combat state exists), `char_update_pos_dead` (a
/// script writes hit points at or below zero), and `mtransform.stats` (a
/// transform copies statistics the pack does not model).
const UNMODELLED: &[&str] = &[
    "char_npc_no_combat_state",
    "char_pref_flags",
    "char_set_stat_no_combat_state",
    "char_sex",
    "char_update_pos_dead",
    "char_weight",
    "magic_affect_bit",
    "mtransform.stats",
];

/// Run every trigger record's body once against the real ECS host. The body
/// runs without the event variables a dispatched trigger would have (no
/// `%actor%`, `%object%`, ...), so this proves the corpus does not panic or
/// wedge in-game, and that every key it logs is documented above. `script_log`
/// is the error sink, not an unmodelled key.
#[test]
fn every_trigger_record_runs_in_game_without_panicking() {
    let mut app = playtest::content_app();
    let room = playtest::spawn_room(&mut app, 1);
    let _player = playtest::spawn_player(&mut app, room, "Tester");
    let _item = playtest::spawn_item(&mut app, room, "tba_item_47");

    let vnums: Vec<i32> = {
        let content = app.world().resource::<TbaContent>();
        content.triggers.values().map(|def| def.vnum).collect()
    };
    assert!(!vnums.is_empty(), "the real content has triggers");

    // Reuse one owner at a time so `with_host`'s world sweep stays constant.
    let mut owner: Option<Entity> = None;
    let mut ran = 0usize;
    for vnum in &vnums {
        let Some(proto) = app.world().resource::<DgProtos>().get(*vnum) else {
            continue;
        };
        if let Some(old) = owner.take() {
            app.world_mut().despawn(old);
        }
        let mut script = Script::default();
        let trigger_id = script.add_trigger(proto, -1);
        let entity = playtest::spawn_npc(&mut app, room, "tba_npc_1");
        app.world_mut()
            .entity_mut(entity)
            .insert((DgScript(script), DgScriptInit));
        owner = Some(entity);
        let ok = with_host(app.world_mut(), |host| {
            let Some(uid) = host.uid(entity) else {
                return false;
            };
            let _ = script_driver(host, uid, trigger_id, Mode::New);
            true
        });
        assert!(ok, "trigger {vnum} has a script id");
        ran += 1;
    }
    assert_eq!(ran, vnums.len(), "every record ran");

    let undocumented: Vec<String> = app
        .world()
        .resource::<DgLog>()
        .keys()
        .map(|(key, _)| key.to_owned())
        .filter(|key| key != "script_log" && !UNMODELLED.contains(&key.as_str()))
        .collect();
    assert!(
        undocumented.is_empty(),
        "undocumented unmodelled keys: {undocumented:?}"
    );
}
