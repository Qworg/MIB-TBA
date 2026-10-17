//! `mib-tba` — the TBA game's own server binary.
//!
//! MIB's `mib-server` is built with a fixed module list (`stdlib + classic-rpg`)
//! and MIB does not depend on this repository, so the TBA game ships its own
//! server instead: it composes the reusable [`mib_host::ServerBuilder`] with
//! the `stdlib + tba` catalog. Running this binary *is* choosing the game
//! (README "Running"; doc §2).
//!
//! ```text
//! mib-tba [--config FILE] [--packs DIR]... [COMMAND]
//! ```
//!
//! With no `--packs`, the default world is the engine's `packs/stdlib` beside
//! this checkout plus this repository's own pack root; an explicit `--packs`
//! (repeatable) replaces that, so an operator can point at a deployed world.
//!
//! Besides `run` and `validate-packs`, the offline operator surface is ported
//! from `mib-server` as thin wrappers over `mib_host`: `migrate`, `snapshot`,
//! `backup run|list|prune`, `restore-test`, `wal-sync`, `wal-status` and
//! `restore-base`. They never touch the bridge or gateway, so a down server can
//! still be backed up, migrated, or restored (D-35). The parser is hand-rolled
//! (the engine's clap dependency would be more interface than this binary
//! needs) but mirrors `mib-server`'s argument names so an operator's muscle
//! memory carries between the two binaries.

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

/// The login banner shown at session open.
///
/// The CircleMUD and DikuMUD licences require their creators to be named in the
/// login sequence; the session MOTD (`game.motd`) is this pack's surface for
/// that. The full credits and the `CIRCLEMUD` help entry are served by the
/// converted `credits` and `circlemud` help topics (see `NOTICE`).
fn login_banner() -> String {
    "MIB — The Builder Academy (tbaMUD)\n\
     Based on CircleMUD by Jeremy Elson and DikuMUD by Sebastian Hammer, \
     Michael Seifert, Hans Henrik Staerfeldt, Tom Madsen and Katja Nyboe.\n\
     Type 'help circlemud' or 'help credits' for the full credits."
        .to_owned()
}

/// One offline operation. `Run` is the default when the operator gives no
/// command (matching `mib-server`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    /// Start the server.
    Run,
    /// Load and validate the content packs, then exit.
    ValidatePacks,
    /// Apply the `mib-persist` migrations and exit (W5.5).
    Migrate,
    /// Ask the running server for a full snapshot sweep (W5.5).
    Snapshot { now: bool },
    /// Take a base backup (D-35).
    BackupRun,
    /// List backup manifests, newest first.
    BackupList,
    /// Prune backups past the retention window.
    BackupPrune { dry_run: bool },
    /// Restore-test a backup in a scratch database.
    RestoreTest {
        id: Option<String>,
        from: Option<PathBuf>,
        boot: bool,
    },
    /// Advance the WAL archive with `pg_receivewal`.
    WalSync,
    /// Report WAL archive health (non-zero exit when behind).
    WalStatus,
    /// Materialize a base backup + WAL into a data directory for PITR.
    RestoreBase {
        id: Option<String>,
        from: Option<PathBuf>,
        into: PathBuf,
        to_time: Option<String>,
        to_lsn: Option<String>,
    },
}

/// The parsed command line. `command: None` means `run`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    config: Option<PathBuf>,
    packs: Vec<PathBuf>,
    command: Option<Command>,
}

/// Parse `std::env::args().skip(1)`.
fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

/// Parse an argument list. Split out from [`parse_args`] so the parser is unit
/// testable without mutating the process environment.
///
/// `--config` and `--packs` are global and may appear anywhere on the line (as
/// in `mib-server`); everything else is the command and its arguments.
fn parse_args_from<I, S>(argv: I) -> Result<Args, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let tokens: Vec<String> = argv.into_iter().map(Into::into).collect();
    let mut config = None;
    let mut packs = Vec::new();
    let mut rest = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--config" => {
                let value = tokens.get(index + 1).ok_or("--config needs a file path")?;
                config = Some(PathBuf::from(value));
                index += 2;
            }
            "--packs" => {
                let value = tokens.get(index + 1).ok_or("--packs needs a directory")?;
                packs.push(PathBuf::from(value));
                index += 2;
            }
            _ => {
                rest.push(tokens[index].clone());
                index += 1;
            }
        }
    }
    Ok(Args {
        config,
        packs,
        command: parse_command(&rest)?,
    })
}

/// Dispatch the non-global tokens to one [`Command`].
fn parse_command(rest: &[String]) -> Result<Option<Command>, String> {
    let Some((verb, args)) = rest.split_first() else {
        return Ok(None);
    };
    let command = match verb.as_str() {
        "run" => {
            no_extra_args(args, "run")?;
            Command::Run
        }
        "validate-packs" => {
            no_extra_args(args, "validate-packs")?;
            Command::ValidatePacks
        }
        "migrate" => {
            no_extra_args(args, "migrate")?;
            Command::Migrate
        }
        "snapshot" => {
            let now = parse_snapshot(args)?;
            Command::Snapshot { now }
        }
        "backup" => parse_backup(args)?,
        "restore-test" => parse_restore_test(args)?,
        "wal-sync" => {
            no_extra_args(args, "wal-sync")?;
            Command::WalSync
        }
        "wal-status" => {
            no_extra_args(args, "wal-status")?;
            Command::WalStatus
        }
        "restore-base" => parse_restore_base(args)?,
        other => return Err(format!("unknown command `{other}`")),
    };
    Ok(Some(command))
}

/// Reject any trailing tokens (these subcommands take none).
fn no_extra_args(args: &[String], command: &str) -> Result<(), String> {
    match args.first() {
        None => Ok(()),
        Some(token) => Err(format!("unexpected argument `{token}` for `{command}`")),
    }
}

/// `snapshot [--now]`. The engine's `snapshot` takes only the boolean `--now`;
/// a trailing token is accepted and ignored so a caller appending a timestamp
/// (the operator's habit from other tooling) is not rejected.
fn parse_snapshot(args: &[String]) -> Result<bool, String> {
    let mut now = false;
    for arg in args {
        match arg.as_str() {
            "--now" => now = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}` for `snapshot`"));
            }
            // A positional timestamp is not used by `mib-host::snapshot`; it is
            // accepted for forward compatibility and ignored.
            _ => {}
        }
    }
    Ok(now)
}

/// `backup run|list|prune [--dry-run]`. The engine additionally nests
/// `wal-sync`, `wal-status` and `restore-base` under `backup`; those stay
/// top-level here (the issue's operator surface) but are otherwise identical.
fn parse_backup(args: &[String]) -> Result<Command, String> {
    let Some((action, rest)) = args.split_first() else {
        return Err("backup needs an action: run, list or prune".into());
    };
    match action.as_str() {
        "run" => {
            no_extra_args(rest, "backup run")?;
            Ok(Command::BackupRun)
        }
        "list" => {
            no_extra_args(rest, "backup list")?;
            Ok(Command::BackupList)
        }
        "prune" => {
            let mut dry_run = false;
            for arg in rest {
                match arg.as_str() {
                    "--dry-run" => dry_run = true,
                    other => {
                        return Err(format!("unknown argument `{other}` for `backup prune`"));
                    }
                }
            }
            Ok(Command::BackupPrune { dry_run })
        }
        other => Err(format!(
            "unknown backup action `{other}` (expected run, list or prune)"
        )),
    }
}

/// `restore-test [<id>] [--from DIR] [--boot]`. `--id` is accepted as an alias
/// for the positional id so a `mib-server` habit still works.
fn parse_restore_test(args: &[String]) -> Result<Command, String> {
    let mut id = None;
    let mut from = None;
    let mut boot = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--from" => {
                from = Some(PathBuf::from(require_value(args, &mut index, "--from")?));
            }
            "--id" => {
                set_id(&mut id, require_value(args, &mut index, "--id")?, "--id")?;
            }
            "--boot" => boot = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}` for `restore-test`"));
            }
            other => set_id(&mut id, other.to_owned(), "restore-test")?,
        }
        index += 1;
    }
    Ok(Command::RestoreTest { id, from, boot })
}

/// `restore-base [<id>] [--from DIR] --into DIR [--to-time TS] [--to-lsn LSN]`.
fn parse_restore_base(args: &[String]) -> Result<Command, String> {
    let mut id = None;
    let mut from = None;
    let mut into = None;
    let mut to_time = None;
    let mut to_lsn = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--from" => {
                from = Some(PathBuf::from(require_value(args, &mut index, "--from")?));
            }
            "--into" => {
                into = Some(PathBuf::from(require_value(args, &mut index, "--into")?));
            }
            "--to-time" => {
                to_time = Some(require_value(args, &mut index, "--to-time")?);
            }
            "--to-lsn" => {
                to_lsn = Some(require_value(args, &mut index, "--to-lsn")?);
            }
            "--id" => {
                set_id(&mut id, require_value(args, &mut index, "--id")?, "--id")?;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}` for `restore-base`"));
            }
            other => set_id(&mut id, other.to_owned(), "restore-base")?,
        }
        index += 1;
    }
    let Some(into) = into else {
        return Err("restore-base needs --into <DIR>".into());
    };
    Ok(Command::RestoreBase {
        id,
        from,
        into,
        to_time,
        to_lsn,
    })
}

/// Consume the value after a `--flag`, advancing the cursor past it.
fn require_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

/// Store an id at most once; a second one (positional or `--id`) is an error.
fn set_id(slot: &mut Option<String>, value: String, source: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{source} takes at most one id"));
    }
    *slot = Some(value);
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args().map_err(|message| {
        format!(
            "{message}\nusage: mib-tba [--config FILE] [--packs DIR]... \
             [run|validate-packs|migrate|snapshot|backup|restore-test|wal-sync|wal-status|restore-base]"
        )
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

    // The CircleMUD/DikuMUD licences require their creators to be named in the
    // login sequence; an operator's own `game.motd` wins.
    if cfg
        .game
        .motd
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        cfg.game.motd = Some(login_banner());
    }

    match args.command.unwrap_or(Command::Run) {
        Command::Run => Ok(ServerBuilder::new(cfg)
            .with_catalog(module_catalog())
            .build()?
            .run()?),
        Command::ValidatePacks => {
            mib_host::validate_packs_with_catalog(&cfg, packs, None, &module_catalog())?;
            Ok(())
        }
        Command::Migrate => mib_host::migrate(&cfg).map_err(Into::into),
        Command::Snapshot { now } => mib_host::snapshot(&cfg, now).map_err(Into::into),
        Command::BackupRun => mib_host::backup_run(&cfg).map_err(Into::into),
        Command::BackupList => mib_host::backup_list(&cfg).map_err(Into::into),
        Command::BackupPrune { dry_run } => {
            mib_host::backup_prune(&cfg, dry_run).map_err(Into::into)
        }
        Command::RestoreTest { id, from, boot } => {
            mib_host::restore_test(&cfg, id, from, boot).map_err(Into::into)
        }
        Command::WalSync => mib_host::backup_wal_sync(&cfg).map_err(Into::into),
        Command::WalStatus => mib_host::backup_wal_status(&cfg).map_err(Into::into),
        Command::RestoreBase {
            id,
            from,
            into,
            to_time,
            to_lsn,
        } => mib_host::restore_base(&cfg, id, from, into, to_time, to_lsn).map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn parse(argv: &[&str]) -> Result<Args, String> {
        parse_args_from(argv.iter().copied())
    }

    #[test]
    fn the_login_banner_names_the_upstream_creators() {
        let banner = login_banner();
        for name in [
            "Jeremy Elson",
            "Sebastian Hammer",
            "Michael Seifert",
            "Hans Henrik Staerfeldt",
            "Tom Madsen",
            "Katja Nyboe",
        ] {
            assert!(banner.contains(name), "the banner names {name}");
        }
    }

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

    #[test]
    fn no_command_means_run() {
        assert_eq!(parse(&[]).expect("empty").command, None);
    }

    #[test]
    fn global_flags_are_extracted_from_anywhere() {
        let args = parse(&[
            "run", "--config", "mib.toml", "--packs", "a", "--packs", "b",
        ])
        .expect("parse");
        assert_eq!(args.config, Some(PathBuf::from("mib.toml")));
        assert_eq!(args.packs, vec![PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(args.command, Some(Command::Run));
    }

    #[test]
    fn existing_commands_still_parse() {
        assert_eq!(parse(&["run"]).expect("run").command, Some(Command::Run));
        assert_eq!(
            parse(&["validate-packs"]).expect("validate").command,
            Some(Command::ValidatePacks)
        );
    }

    #[test]
    fn offline_subcommands_parse() {
        assert_eq!(
            parse(&["migrate"]).expect("migrate").command,
            Some(Command::Migrate)
        );
        assert_eq!(
            parse(&["snapshot", "--now"]).expect("snapshot").command,
            Some(Command::Snapshot { now: true })
        );
        assert_eq!(
            parse(&["snapshot"]).expect("snapshot").command,
            Some(Command::Snapshot { now: false })
        );
        assert_eq!(
            parse(&["wal-sync"]).expect("wal-sync").command,
            Some(Command::WalSync)
        );
        assert_eq!(
            parse(&["wal-status"]).expect("wal-status").command,
            Some(Command::WalStatus)
        );
    }

    #[test]
    fn backup_actions_parse() {
        assert_eq!(
            parse(&["backup", "run"]).expect("run").command,
            Some(Command::BackupRun)
        );
        assert_eq!(
            parse(&["backup", "list"]).expect("list").command,
            Some(Command::BackupList)
        );
        assert_eq!(
            parse(&["backup", "prune"]).expect("prune").command,
            Some(Command::BackupPrune { dry_run: false })
        );
        assert_eq!(
            parse(&["backup", "prune", "--dry-run"])
                .expect("prune")
                .command,
            Some(Command::BackupPrune { dry_run: true })
        );
    }

    #[test]
    fn restore_test_accepts_positional_id_from_and_boot() {
        assert_eq!(
            parse(&["restore-test", "42", "--from", "/tmp/b", "--boot"])
                .expect("parse")
                .command,
            Some(Command::RestoreTest {
                id: Some("42".to_owned()),
                from: Some(PathBuf::from("/tmp/b")),
                boot: true,
            })
        );
        // `--id` mirrors `mib-server`.
        assert_eq!(
            parse(&["restore-test", "--id", "7"])
                .expect("parse")
                .command,
            Some(Command::RestoreTest {
                id: Some("7".to_owned()),
                from: None,
                boot: false,
            })
        );
        assert_eq!(
            parse(&["restore-test"]).expect("parse").command,
            Some(Command::RestoreTest {
                id: None,
                from: None,
                boot: false,
            })
        );
    }

    #[test]
    fn restore_base_requires_into_and_parses_recovery_targets() {
        assert_eq!(
            parse(&[
                "restore-base",
                "3",
                "--from",
                "/tmp/b",
                "--into",
                "/tmp/data",
                "--to-time",
                "2024-01-01T00:00:00Z",
                "--to-lsn",
                "0/16B6C50",
            ])
            .expect("parse")
            .command,
            Some(Command::RestoreBase {
                id: Some("3".to_owned()),
                from: Some(PathBuf::from("/tmp/b")),
                into: PathBuf::from("/tmp/data"),
                to_time: Some("2024-01-01T00:00:00Z".to_owned()),
                to_lsn: Some("0/16B6C50".to_owned()),
            })
        );
        let error = parse(&["restore-base", "3"]).expect_err("--into is required");
        assert!(error.contains("--into"), "{error}");
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(parse(&["bogus"]).is_err());
        assert!(parse(&["run", "extra"]).is_err());
        assert!(parse(&["backup"]).is_err());
        assert!(parse(&["backup", "bogus"]).is_err());
        assert!(parse(&["restore-test", "1", "2"]).is_err());
        assert!(parse(&["--config"]).is_err());
        assert!(parse(&["--packs"]).is_err());
        assert!(parse(&["snapshot", "--bogus"]).is_err());
    }
}
