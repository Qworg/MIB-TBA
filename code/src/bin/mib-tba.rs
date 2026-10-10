//! `mib-tba` — the TBA game's own server binary.
//!
//! MIB's `mib-server` is built with a fixed module list (`stdlib + classic-rpg`)
//! and MIB does not depend on this repository, so the TBA game ships its own
//! server instead: it composes the reusable [`mib_host::ServerBuilder`] with
//! the `stdlib + tba` catalog. Running this binary *is* choosing the game
//! (README "Running"; doc §2).
//!
//! ```text
//! mib-tba [--config FILE] [--packs DIR]... [run|validate-packs]
//! ```
//!
//! With no `--packs`, the default world is the engine's `packs/stdlib` beside
//! this checkout plus this repository's own pack root; an explicit `--packs`
//! (repeatable) replaces that, so an operator can point at a deployed world.

use std::error::Error;
use std::path::PathBuf;

use mib_host::prelude::{ModuleCatalog, ServerBuilder};
use mib_stdlib::prelude::StdlibModule;
use mib_tba::prelude::TbaModule;

/// The code modules this binary supplies: TBA is an independent genre, so no
/// `classic-rpg`. Order is the engine's convention — a later module wins on a
/// duplicate section id.
fn module_catalog() -> ModuleCatalog {
    ModuleCatalog::new().with(StdlibModule).with(TbaModule)
}

/// `[<repo>/../MIB/packs/stdlib, <repo>]`: the engine's stdlib pack beside this
/// checkout, then this repository (which *is* the `tba` pack root).
fn default_packs() -> Vec<PathBuf> {
    let code_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = code_dir
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| code_dir.clone());
    let engine_stdlib = code_dir.join("../../MIB/packs/stdlib");
    vec![engine_stdlib, repo_root]
}

#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    packs: Vec<PathBuf>,
    command: Option<String>,
}

/// A deliberately tiny parser (the binary has three flags; a CLI crate would be
/// more dependency than interface).
fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--config" => {
                args.config = Some(PathBuf::from(
                    argv.next().ok_or("--config needs a file path")?,
                ));
            }
            "--packs" => {
                args.packs.push(PathBuf::from(
                    argv.next().ok_or("--packs needs a directory")?,
                ));
            }
            "run" | "validate-packs" => {
                if args.command.is_some() {
                    return Err("only one command may be given".into());
                }
                args.command = Some(arg);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(args)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args().map_err(|message| {
        format!("{message}\nusage: mib-tba [--config FILE] [--packs DIR]... [run|validate-packs]")
    })?;

    let mut cfg = mib_core::config::load(args.config.as_deref())?;
    let packs = if args.packs.is_empty() {
        default_packs()
    } else {
        args.packs
    };
    cfg.game.packs = packs
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();

    if args.command.as_deref() == Some("validate-packs") {
        mib_host::validate_packs_with_catalog(&cfg, packs, None, &module_catalog())?;
        return Ok(());
    }

    Ok(ServerBuilder::new(cfg)
        .with_catalog(module_catalog())
        .build()?
        .run()?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn catalog_has_no_duplicates_and_supplies_the_tba_game() {
        let catalog = module_catalog();
        assert_eq!(catalog.duplicate_names(), Vec::<&str>::new());
        assert!(catalog.contains("stdlib"));
        assert!(catalog.contains("tba"));
        assert!(
            !catalog.contains("classic-rpg"),
            "TBA is an independent genre and must not link classic-rpg"
        );
    }

    #[test]
    fn default_packs_end_at_the_repo_root_and_include_engine_stdlib() {
        let packs = default_packs();
        assert_eq!(packs.len(), 2);
        assert!(packs[0].ends_with("MIB/packs/stdlib"), "{:?}", packs[0]);
        assert!(packs[1].join("pack.toml").is_file(), "{:?}", packs[1]);
    }
}
