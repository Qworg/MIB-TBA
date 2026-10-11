//! `.zon` reader — mirrors `load_zones` (`db.c:2338`).
//!
//! One zone file holds exactly one zone. Commands run to a `S` (or `$`)
//! terminator; `*` lines are comments and are not commands. The command
//! grammar follows the `sscanf` in `load_zones`: `M O G E P D T V` take four
//! numbers (if-flag + three args), `R` takes three, and `V` additionally takes
//! two string args.

use crate::error::Result;
use crate::source::tokens::{Source, ascii_flags, parse_int};

/// One zone and its reset program.
#[derive(Debug, Clone)]
pub struct RawZone {
    pub number: u32,
    pub builders: String,
    pub name: String,
    pub bot: i32,
    pub top: i32,
    pub lifespan: u32,
    pub reset_mode: i32,
    pub zone_flags: [u64; 4],
    pub commands: Vec<RawCommand>,
}

/// One reset command.
#[derive(Debug, Clone, Copy)]
pub struct RawCommand {
    pub code: char,
    pub if_flag: i32,
    pub arg1: i32,
    pub arg2: i32,
    pub arg3: i32,
    /// 1-based source line, for diagnostics.
    pub line: usize,
}

/// Parse a `.zon` file.
pub fn parse_zone(src: &mut Source) -> Result<RawZone> {
    let header = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error("empty zone file"))?;
    let number: i32 = header
        .trim()
        .strip_prefix('#')
        .and_then(|rest| rest.trim().parse().ok())
        .ok_or_else(|| src.parse_error(format!("zone: expected `#<number>`, got `{header}`")))?;
    let number = number.max(0) as u32;

    let builders = strip_tilde(
        &src.next_nonblank()
            .ok_or_else(|| src.parse_error(format!("zone {number}: missing builders line")))?,
    );
    let name = strip_tilde(
        &src.next_nonblank()
            .ok_or_else(|| src.parse_error(format!("zone {number}: missing name line")))?,
    );

    let numeric = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("zone {number}: missing numeric line")))?;
    let tokens: Vec<&str> = numeric.split_whitespace().collect();
    if tokens.len() < 4 {
        return Err(src.parse_error(format!(
            "zone {number}: expected `<bot> <top> <lifespan> <reset_mode>`, got `{numeric}`"
        )));
    }
    let bot = parse_int(src, "zone bot", tokens[0])?;
    let top = parse_int(src, "zone top", tokens[1])?;
    let lifespan = parse_int(src, "zone lifespan", tokens[2])?.max(0) as u32;
    let reset_mode = parse_int(src, "zone reset mode", tokens[3])?;

    let mut zone_flags = [0u64; 4];
    if tokens.len() >= 10 {
        for (slot, token) in zone_flags.iter_mut().zip(&tokens[4..8]) {
            *slot = ascii_flags(token);
        }
    }

    if top < bot {
        return Err(src.parse_error(format!("zone {number}: bottom ({bot}) > top ({top})")));
    }

    let mut commands = Vec::new();
    loop {
        let line_no = src.line_no();
        let line = src
            .next_nonblank()
            .ok_or_else(|| src.parse_error(format!("zone {number}: missing S terminator")))?;
        let trimmed = line.trim_start();
        let code = match trimmed.chars().next() {
            Some(c) => c,
            None => continue,
        };
        let rest = trimmed[1..].trim();
        match code {
            '*' => continue,
            'S' | '$' => break,
            'V' => {
                let args = numbers(rest);
                commands.push(RawCommand {
                    code,
                    if_flag: args.first().copied().unwrap_or(0),
                    arg1: args.get(1).copied().unwrap_or(0),
                    arg2: args.get(2).copied().unwrap_or(0),
                    arg3: args.get(3).copied().unwrap_or(0),
                    line: line_no,
                });
            }
            'R' => {
                let args = numbers(rest);
                if args.len() < 3 {
                    return Err(src.parse_error(format!(
                        "zone {number}: R command expected three numbers, got `{rest}`"
                    )));
                }
                commands.push(RawCommand {
                    code,
                    if_flag: args[0],
                    arg1: args[1],
                    arg2: args[2],
                    arg3: 0,
                    line: line_no,
                });
            }
            'M' | 'O' | 'G' | 'E' | 'P' | 'D' | 'T' => {
                let args = numbers(rest);
                if args.len() < 4 {
                    return Err(src.parse_error(format!(
                        "zone {number}: {code} command expected four numbers, got `{rest}`"
                    )));
                }
                commands.push(RawCommand {
                    code,
                    if_flag: args[0],
                    arg1: args[1],
                    arg2: args[2],
                    arg3: args[3],
                    line: line_no,
                });
            }
            _ => {
                return Err(
                    src.parse_error(format!("zone {number}: unknown reset command `{code}`"))
                );
            }
        }
    }

    Ok(RawZone {
        number,
        builders,
        name,
        bot,
        top,
        lifespan,
        reset_mode,
        zone_flags,
        commands,
    })
}

fn strip_tilde(line: &str) -> String {
    line.trim_end_matches('~').trim().to_owned()
}

/// Extract every leading signed integer token from a command tail. Stops at
/// the first token that is not an integer (the trailing `(comment)` or the
/// string args of a `V` command).
fn numbers(rest: &str) -> Vec<i32> {
    let mut out = Vec::new();
    for token in rest.split_whitespace() {
        match token.parse::<i32>() {
            Ok(value) => out.push(value),
            Err(_) => break,
        }
    }
    out
}
