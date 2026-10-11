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

/// A fresh temp pack root (content/ + pack.toml) namespaced by pid.
fn temp_pack() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tbaport-validate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn full_conversion_validates_clean_through_the_pack_registry() {
    let Some(root) = real_tbamud() else {
        eprintln!("skipping: tbaMUD source not checked out (set TBAMUD_SOURCE)");
        return;
    };

    let pack_root = temp_pack();
    std::fs::create_dir_all(pack_root.join("content")).expect("mkdir content");
    std::fs::write(
        pack_root.join("pack.toml"),
        "id = \"tba\"\nversion = \"1\"\ntitle = \"The Builder Academy\"\ngame = \"tba\"\ndepends_on = [\"stdlib\"]\n",
    )
    .expect("write pack.toml");

    let conversion = convert(&ConvertOptions {
        source_root: root,
        only: None,
        start_room: Some(DEFAULT_START_ROOM),
    })
    .expect("full conversion");
    for (name, yaml) in render(&conversion.bundle).expect("render bundle") {
        std::fs::write(pack_root.join("content").join(name), yaml).expect("write section");
    }

    let registry = registry_for(&[
        &StdlibModule as &dyn MibModule,
        &TbaModule as &dyn MibModule,
    ]);
    let assets = load_packs(&[engine_stdlib(), pack_root.clone()], &registry)
        .expect("emitted pack loads alongside the engine stdlib");
    let issues = validate_all(&assets, &registry);
    for issue in &issues {
        eprintln!("ISSUE: {issue:?}");
    }
    assert!(
        issues.is_empty(),
        "emitted pack must validate clean: {} issue(s)",
        issues.len()
    );

    let _ = std::fs::remove_dir_all(&pack_root);
}
