//! Acceptance check for the converter: the emitted pack must **validate clean
//! through the real MIB pack registry** (`stdlib + tba`), not merely round-trip
//! its own serde types.
//!
//! This is the Phase 10 exit criterion ("the emitted pack validates clean with
//! `validate-packs`"). It is gated on the sibling tbaMUD checkout being present
//! and runs the full 189-zone conversion, so it is the slowest test in the
//! crate; the count/determinism tests cover zone 30 on the committed fixtures.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use mib_content::prelude::{MibModule, load_packs, registry_for, validate_all};
use mib_stdlib::prelude::StdlibModule;
use mib_tba::prelude::TbaModule;
use mib_tba::triggers::content::TriggerId;
use tbaport::emit::OutputBundle;
use tbaport::{ConvertOptions, DEFAULT_START_ROOM, convert, emit::render};

/// The sibling tbaMUD checkout, or `None` when it is not present.
fn real_tbamud() -> Option<PathBuf> {
    let candidate = std::env::var("TBAMUD_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/qworg/Development/tbamud"));
    candidate
        .join("lib/world/zon/30.zon")
        .is_file()
        .then_some(candidate)
}

/// The engine's stdlib pack: `tools/tbaport` → repo → `MIB/packs/stdlib`.
fn engine_stdlib() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../MIB/packs/stdlib")
}

/// A fresh temp pack root (content/ + pack.toml) namespaced by pid and test.
fn temp_pack(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tbaport-validate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Write `bundle` as a pack next to the engine stdlib, load it through the
/// registry and return every validation issue as text.
fn validation_issues(bundle: &OutputBundle, tag: &str) -> Vec<String> {
    let pack_root = temp_pack(tag);
    std::fs::create_dir_all(pack_root.join("content")).expect("mkdir content");
    std::fs::write(
        pack_root.join("pack.toml"),
        "id = \"tba\"\nversion = \"1\"\ntitle = \"The Builder Academy\"\ngame = \"tba\"\ndepends_on = [\"stdlib\"]\n",
    )
    .expect("write pack.toml");
    for (name, yaml) in render(bundle).expect("render bundle") {
        std::fs::write(pack_root.join("content").join(name), yaml).expect("write section");
    }

    let registry = registry_for(&[
        &StdlibModule as &dyn MibModule,
        &TbaModule as &dyn MibModule,
    ]);
    let assets = load_packs(&[engine_stdlib(), pack_root.clone()], &registry)
        .expect("emitted pack loads alongside the engine stdlib");
    let issues = validate_all(&assets, &registry)
        .iter()
        .map(|issue| format!("{issue:?}"))
        .collect();
    let _ = std::fs::remove_dir_all(&pack_root);
    issues
}

fn full_conversion() -> Option<tbaport::Conversion> {
    let Some(root) = real_tbamud() else {
        eprintln!("skipping: tbaMUD source not checked out (set TBAMUD_SOURCE)");
        return None;
    };
    Some(
        convert(&ConvertOptions {
            source_root: root,
            only: None,
            start_room: Some(DEFAULT_START_ROOM),
        })
        .expect("full conversion"),
    )
}

#[test]
fn full_conversion_validates_clean_through_the_pack_registry() {
    let Some(conversion) = full_conversion() else {
        return;
    };
    let issues = validation_issues(&conversion.bundle, "clean");
    for issue in &issues {
        eprintln!("ISSUE: {issue}");
    }
    assert!(
        issues.is_empty(),
        "emitted pack must validate clean: {} issue(s)",
        issues.len()
    );
}

/// The owner lists are checked against the merged section: an unknown owner
/// and an unknown trigger id are both rejected, while the real cross-zone
/// lists (trigger 72 in zone 0 on mob 31, say) pass.
#[test]
fn attachment_validation_rejects_unknown_owners_and_triggers() {
    let Some(mut conversion) = full_conversion() else {
        return;
    };
    let lists = &mut conversion.bundle.triggers.attachments;
    lists.insert(
        "npc:tba_npc_99999999".to_owned(),
        vec![TriggerId::from_vnum(3000)],
    );
    lists.insert(
        "item:tba_item_16".to_owned(),
        vec![TriggerId::from_vnum(88888888)],
    );
    let issues = validation_issues(&conversion.bundle, "bad");
    assert_eq!(issues.len(), 2, "issues: {issues:#?}");
    assert!(issues.iter().any(|issue| issue.contains("unknown npc")));
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("unknown trigger `tba_trig_88888888`"))
    );
}
