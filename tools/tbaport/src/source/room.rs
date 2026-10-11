//! `.wld` reader — mirrors `parse_room` / `setup_dir` (`db.c:1306`, `db.c:1490`).

use crate::error::Result;
use crate::source::RawExtraDesc;
use crate::source::tokens::{Source, ascii_flags, parse_int};

/// One room record from a `.wld` file.
#[derive(Debug, Clone)]
pub struct RawRoom {
    pub vnum: i32,
    pub name: String,
    pub description: String,
    /// The four 32-bit room-flag words (`room_flags[0..3]`).
    pub flags: [u64; 4],
    /// `sector_type` (`SECT_*`).
    pub sector: i32,
    pub exits: Vec<RawExit>,
    pub extra_descriptions: Vec<RawExtraDesc>,
    /// DG trigger vnums referenced by trailing `T <vnum>` lines (deferred).
    pub trigger_vnums: Vec<i32>,
}

/// One `D<dir>` exit block.
#[derive(Debug, Clone)]
pub struct RawExit {
    /// Direction index 0..9 (`dirs[]` in `constants.c`).
    pub dir: i32,
    pub description: String,
    pub keywords: String,
    /// The file's `exit_info` code (0 plain, 1 door, 2 door+pickproof,
    /// 3 door+hidden, 4 door+both); `setup_dir` maps these onto `EX_*` bits.
    pub exit_info_code: i32,
    pub key: i32,
    pub to_room: i32,
}

// One `E` extra-description block. (Defined in `source::mod` because both
// rooms and objects use it.)

/// Parse a single room; the `#<vnum>` line is already consumed.
pub fn parse_room(src: &mut Source, vnum: i32) -> Result<RawRoom> {
    let name = src.read_string("room name")?;
    let description = src.read_string("room description")?;

    let flag_line = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("room {vnum}: missing flag line")))?;
    let tokens: Vec<&str> = flag_line.split_whitespace().collect();
    if tokens.len() < 6 {
        return Err(src.parse_error(format!(
            "room {vnum}: expected `<zone> <f0> <f1> <f2> <f3> <sector>`, got `{flag_line}`"
        )));
    }
    let mut flags = [0u64; 4];
    for (slot, token) in flags.iter_mut().zip(&tokens[1..5]) {
        *slot = ascii_flags(token);
    }
    let sector = parse_int(src, "room sector", tokens[5])?;

    let mut exits = Vec::new();
    let mut extra_descriptions = Vec::new();
    let mut trigger_vnums = Vec::new();

    loop {
        let line = src
            .next_line()
            .ok_or_else(|| src.parse_error(format!("room {vnum}: missing S terminator")))?;
        let line = line.trim_start();
        match line.chars().next() {
            Some('D') => exits.push(parse_exit(src, vnum, &line[1..])?),
            Some('E') => extra_descriptions.push(parse_extra_desc(src)?),
            Some('S') => {
                while let Some(next) = src.peek() {
                    let trimmed = next.trim_start();
                    if let Some(rest) = trimmed.strip_prefix('T') {
                        trigger_vnums.push(parse_int(src, "room trigger vnum", rest.trim())?);
                        src.next_line();
                    } else {
                        break;
                    }
                }
                break;
            }
            _ => {
                return Err(src.parse_error(format!(
                    "room {vnum}: expected D/E/S directive, got `{line}`"
                )));
            }
        }
    }

    Ok(RawRoom {
        vnum,
        name,
        description,
        flags,
        sector,
        exits,
        extra_descriptions,
        trigger_vnums,
    })
}

fn parse_exit(src: &mut Source, vnum: i32, dir_field: &str) -> Result<RawExit> {
    let dir = dir_field
        .trim()
        .parse::<i32>()
        .map_err(|_| src.parse_error(format!("room {vnum}: bad exit direction `{dir_field}`")))?;
    let description = src.read_string("exit description")?;
    let keywords = src.read_string("exit keywords")?;
    let numeric = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("room {vnum}: exit D{dir} missing numbers")))?;
    let tokens: Vec<&str> = numeric.split_whitespace().collect();
    if tokens.len() < 3 {
        return Err(src.parse_error(format!(
            "room {vnum}: exit D{dir} expected `<info> <key> <to>`, got `{numeric}`"
        )));
    }
    Ok(RawExit {
        dir,
        description,
        keywords,
        exit_info_code: parse_int(src, "exit info", tokens[0])?,
        key: parse_int(src, "exit key", tokens[1])?,
        to_room: parse_int(src, "exit destination", tokens[2])?,
    })
}

fn parse_extra_desc(src: &mut Source) -> Result<RawExtraDesc> {
    let keywords = src.read_string("extra description keywords")?;
    let description = src.read_string("extra description text")?;
    Ok(RawExtraDesc {
        keywords,
        description,
    })
}
