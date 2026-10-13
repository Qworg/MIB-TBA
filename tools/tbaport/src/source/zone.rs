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
#[derive(Debug, Clone)]
pub struct RawCommand {
    pub code: char,
    pub if_flag: i32,
    pub arg1: i32,
    pub arg2: i32,
    pub arg3: i32,
    /// `V` only: the variable name (`ZCMD.sarg1`, one `%s` word).
    pub sarg1: String,
    /// `V` only: the variable value (`ZCMD.sarg2`, the rest of the line up to
    /// a tab or other control whitespace).
    pub sarg2: String,
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
        // Only leading blanks go: a `V` value keeps its trailing blanks, as
        // the C's `%[^\f\n\r\t\v]` does.
        let rest = trimmed[1..].trim_start();
        match code {
            '*' => continue,
            'S' | '$' => break,
            'V' => {
                // `sscanf(" %d %d %d %d %s %[^\f\n\r\t\v]")` (`db.c:2461`): a
                // missing field is a fatal format error in the C.
                let (args, tail) = leading_ints(rest, 4);
                let (name, value) = split_variable(tail);
                if args.len() < 4 || name.is_empty() || value.is_empty() {
                    return Err(src.parse_error(format!(
                        "zone {number}: V command expected four numbers, a name and a value, got `{rest}`"
                    )));
                }
                commands.push(RawCommand {
                    code,
                    if_flag: args[0],
                    arg1: args[1],
                    arg2: args[2],
                    arg3: args[3],
                    sarg1: name.to_owned(),
                    sarg2: value.to_owned(),
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
                    sarg1: String::new(),
                    sarg2: String::new(),
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
                    sarg1: String::new(),
                    sarg2: String::new(),
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

/// Take up to `count` leading integer tokens off `rest`, returning them and
/// the unread remainder (so a `V` command's name token is never read as a
/// number).
fn leading_ints(rest: &str, count: usize) -> (Vec<i32>, &str) {
    let mut ints = Vec::with_capacity(count);
    let mut tail = rest;
    while ints.len() < count {
        let trimmed = tail.trim_start();
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        match trimmed[..end].parse::<i32>() {
            Ok(value) => {
                ints.push(value);
                tail = &trimmed[end..];
            }
            Err(_) => break,
        }
    }
    (ints, tail)
}

/// Longest `V` name or value the C reads (`ZCMD_STR_FIELD`, `db.h:185`).
const ZCMD_STR_FIELD: usize = 79;

/// The longest prefix of `text` that is at most `limit` bytes and ends on a
/// character boundary.
fn cap_bytes(text: &str, limit: usize) -> &str {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Split the tail of a `V` command into its name and value, as
/// `sscanf(" %79s %79[^\f\n\r\t\v]")` does (`db.c:2461`): the name is one
/// whitespace-delimited word capped at 79 bytes (a longer word spills its
/// remainder into the value), the value runs from the next non-blank to a
/// form feed, newline, carriage return, tab or vertical tab, capped at 79
/// bytes, and keeps its trailing blanks.
fn split_variable(tail: &str) -> (&str, &str) {
    let tail = tail.trim_start();
    let word = tail.find(char::is_whitespace).unwrap_or(tail.len());
    let name = cap_bytes(&tail[..word], ZCMD_STR_FIELD);
    let after = tail[name.len()..].trim_start();
    let stop = after
        .find(['\x0c', '\n', '\r', '\t', '\x0b'])
        .unwrap_or(after.len());
    (name, cap_bytes(&after[..stop], ZCMD_STR_FIELD))
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
