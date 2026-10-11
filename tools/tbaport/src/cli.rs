//! Command-line interface.
//!
//! ```text
//! tbaport convert --source DIR [--out DIR] [--report FILE] [--only <zone>] [--start-room <vnum>] [--validate-only]
//! ```
//!
//! Defaults keep the staging contract from the design: output goes to
//! `target/tbaport/content` and the report to `target/tbaport/drop-report.json`,
//! never into `content/`.

use std::path::PathBuf;

use crate::emit::write_bundle;
use crate::error::{ConvertError, Result};
use crate::mapping::{Conversion, ConvertOptions, convert};
use crate::report::DEFAULT_START_ROOM;

const USAGE: &str = "\
tbaport — deterministic tbaMUD -> MIB content converter

USAGE:
    tbaport convert --source DIR [--out DIR] [--report FILE] [--only ZONE] [--start-room VNUM] [--validate-only]

OPTIONS:
    --source DIR       tbaMUD checkout root (must contain lib/world)
    --out DIR          staging output directory [default: target/tbaport/content]
    --report FILE      drop-report path [default: target/tbaport/drop-report.json]
    --only ZONE        convert a single zone
    --start-room VNUM  tag this room vnum as `start_room` [default: 3001]
    --validate-only    parse/map and write the report, but emit no YAML
    -h, --help         print this help
";

/// Parse arguments, run the conversion, and print a summary.
pub fn run(args: &[String]) -> Result<()> {
    let mut iter = args.iter();
    let command = match iter.next() {
        Some(command) => command.as_str(),
        None => {
            print!("{USAGE}");
            return Ok(());
        }
    };

    match command {
        "convert" => run_convert(iter),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(ConvertError::Usage(format!(
            "unknown command `{other}` (try `tbaport --help`)"
        ))),
    }
}

fn run_convert<'a>(mut iter: impl Iterator<Item = &'a String>) -> Result<()> {
    let mut source: Option<PathBuf> = None;
    let mut out = PathBuf::from("target/tbaport/content");
    let mut report_path = PathBuf::from("target/tbaport/drop-report.json");
    let mut only: Option<u32> = None;
    let mut start_room: Option<u32> = Some(DEFAULT_START_ROOM);
    let mut validate_only = false;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--source" => source = Some(PathBuf::from(next_value(&mut iter, "--source")?)),
            "--out" => out = PathBuf::from(next_value(&mut iter, "--out")?),
            "--report" => report_path = PathBuf::from(next_value(&mut iter, "--report")?),
            "--only" => {
                let value = next_value(&mut iter, "--only")?;
                only = Some(value.parse::<u32>().map_err(|_| {
                    ConvertError::Usage(format!("--only expects a zone number, got `{value}`"))
                })?);
            }
            "--start-room" => {
                let value = next_value(&mut iter, "--start-room")?;
                start_room = Some(value.parse::<u32>().map_err(|_| {
                    ConvertError::Usage(format!("--start-room expects a room vnum, got `{value}`"))
                })?);
            }
            "--validate-only" => validate_only = true,
            other => {
                return Err(ConvertError::Usage(format!(
                    "unknown argument `{other}` (try `tbaport --help`)"
                )));
            }
        }
    }

    let source = source.ok_or_else(|| ConvertError::Usage("--source is required".to_owned()))?;
    let options = ConvertOptions {
        source_root: source,
        only,
        start_room,
    };

    let Conversion { bundle, mut report } = convert(&options)?;
    report.out = out.display().to_string();
    if let Some(requested) = start_room
        && report.start_room.is_none()
    {
        eprintln!(
            "tbaport: warning: start room tba.{requested} was not found; no room is tagged `start_room`"
        );
    }

    if validate_only {
        println!("validate-only: parsed and mapped; no YAML written");
    } else {
        let written = write_bundle(&out, &bundle)?;
        println!("wrote {} files to {}", written.len(), out.display());
    }

    report.write(&report_path)?;
    let totals = &report.totals;
    println!(
        "totals: zones={} rooms={} npcs={} items={} commands={} deferred={} dangling={} unmapped={}",
        totals.zones,
        totals.rooms,
        totals.npcs,
        totals.items,
        totals.zone_commands,
        totals.deferred,
        totals.dangling,
        totals.unmapped,
    );
    println!("report: {}", report_path.display());
    Ok(())
}

fn next_value<'a>(iter: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String> {
    iter.next()
        .cloned()
        .ok_or_else(|| ConvertError::Usage(format!("{flag} requires a value")))
}

/// Entry point for the binary; returns a process exit code.
pub fn run_from_env() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("tbaport: {error}");
            2
        }
    }
}
