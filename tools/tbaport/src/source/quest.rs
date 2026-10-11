//! The `.qst` reader — tbaMUD quests (`src/quest.c:129`, `parse_quest`).
//!
//! Grammar (all five string blocks are `fread_string` records):
//!
//! ```text
//! #<vnum>
//! <name>~
//! <desc>~
//! <info>~
//! <done>~
//! <quit>~
//! <type> <questmaster> <flags> <target> <prev_quest> <next_quest> <prereq>
//! <points> <penalty> <minlevel> <maxlevel> <time> <returnmob> <quantity>
//! <gold_reward> <exp_reward> <obj_reward>
//! S
//! ```
//!
//! A quest's `type` selects its objective (`AQ_*`, `quest.h`); `target` is the
//! object/mob/room vnum and `values[6]` (`QST_QUANTITY`) the count. The `-1`
//! sentinels (`target`, `prev/next`, `prereq`, `obj_reward`) load as `NOTHING`.
//! `obj_reward` is left as written; the mapper resolves it against the
//! converted item set and records a dangling reference when it misses (vnum
//! `65535` occurs in the source and names no object).
//!
//! Empty files (`$~`) parse to no records.

use crate::error::Result;
use crate::source::tokens::{Source, ascii_flags};

/// A parsed quest record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawQuest {
    pub vnum: i32,
    pub name: String,
    pub desc: String,
    pub info: String,
    pub done: String,
    pub quit: String,
    pub quest_type: i32,
    pub questmaster: i32,
    /// `AQ_REPEATABLE` bitfield (`asciiflag_conv`).
    pub flags: u64,
    pub target: i32,
    pub prev_quest: i32,
    pub next_quest: i32,
    pub prereq: i32,
    /// `QST_POINTS`, `PENALTY`, `MINLEVEL`, `MAXLEVEL`, `TIME`, `RETURNMOB`,
    /// `QUANTITY`.
    pub values: [i32; 7],
    pub gold_reward: i32,
    pub exp_reward: i32,
    pub obj_reward: i32,
}

/// Parse every quest in a `.qst` file.
pub fn parse_quests(src: &mut Source) -> Result<Vec<RawQuest>> {
    let mut out = Vec::new();
    while let Some(line) = src.next_nonblank() {
        let trimmed = line.trim();
        if trimmed.starts_with('$') {
            break;
        }
        let vnum = match trimmed
            .strip_prefix('#')
            .and_then(|rest| rest.trim().parse::<i32>().ok())
        {
            Some(vnum) => vnum,
            None => continue,
        };
        out.push(parse_quest(src, vnum)?);
    }
    Ok(out)
}

fn parse_quest(src: &mut Source, vnum: i32) -> Result<RawQuest> {
    let name = src.read_string(&format!("quest {vnum} name"))?;
    let desc = src.read_string(&format!("quest {vnum} description"))?;
    let info = src.read_string(&format!("quest {vnum} info"))?;
    let done = src.read_string(&format!("quest {vnum} completion"))?;
    let quit = src.read_string(&format!("quest {vnum} abandon"))?;

    let header = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("quest {vnum}: missing header line")))?;
    let header_tokens: Vec<&str> = header.split_whitespace().collect();
    if header_tokens.len() < 7 {
        return Err(src.parse_error(format!(
            "quest {vnum}: expected 7 header fields, got {}",
            header_tokens.len()
        )));
    }
    let mut header_ints = [0i32; 7];
    for (index, slot) in header_ints.iter_mut().enumerate() {
        *slot = header_tokens[index].parse::<i32>().map_err(|_| {
            src.parse_error(format!(
                "quest {vnum}: header field {index} is not an integer: `{}`",
                header_tokens[index]
            ))
        })?;
    }
    let flags = ascii_flags(header_tokens[2]);

    let value_line = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("quest {vnum}: missing value line")))?;
    let values = ints(&value_line);
    if values.len() < 7 {
        return Err(src.parse_error(format!(
            "quest {vnum}: expected 7 value fields, got {}",
            values.len()
        )));
    }
    let mut value_array = [0i32; 7];
    value_array.copy_from_slice(&values[..7]);

    let rewards = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("quest {vnum}: missing reward line")))?;
    let rewards = ints(&rewards);
    if rewards.len() < 3 {
        return Err(src.parse_error(format!(
            "quest {vnum}: expected 3 reward fields, got {}",
            rewards.len()
        )));
    }

    // Consume the (currently empty) optional tail up to the `S` terminator.
    loop {
        let line = src
            .next_line()
            .ok_or_else(|| src.parse_error(format!("quest {vnum}: missing S terminator")))?;
        if line.trim_start().starts_with('S') {
            break;
        }
    }

    Ok(RawQuest {
        vnum,
        name,
        desc,
        info,
        done,
        quit,
        quest_type: header_ints[0],
        questmaster: header_ints[1],
        flags,
        target: header_ints[3],
        prev_quest: header_ints[4],
        next_quest: header_ints[5],
        prereq: header_ints[6],
        values: value_array,
        gold_reward: rewards[0],
        exp_reward: rewards[1],
        obj_reward: rewards[2],
    })
}

/// Parse a whitespace-separated list of ints, ignoring non-integer tokens.
fn ints(line: &str) -> Vec<i32> {
    line.split_whitespace()
        .filter_map(|token| token.parse::<i32>().ok())
        .collect()
}
