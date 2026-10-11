//! `.mob` reader — mirrors `parse_mobile` / `parse_simple_mob` /
//! `parse_enhanced_mob` (`db.c:1890`, `db.c:1682`, `db.c:1870`).

use mib_core::prelude::Dice;

use crate::error::Result;
use crate::source::tokens::{Source, ascii_flags, ascii_flags_aff, parse_dice, parse_int};

/// One mobile prototype.
#[derive(Debug, Clone)]
pub struct RawMob {
    pub vnum: i32,
    /// Keyword list (the first string).
    pub name: String,
    /// Display name (`short_descr`).
    pub short_descr: String,
    pub long_descr: String,
    pub description: String,
    pub mob_flags: [u64; 4],
    pub aff_flags: [u64; 4],
    pub alignment: i32,
    /// True for the `E` (enhanced) form, which appends an e-spec section.
    pub enhanced: bool,
    /// Raw e-spec lines (recorded in the drop report; no target fields).
    pub especs: Vec<String>,
    pub level: i32,
    /// The file's second stat: `GET_HITROLL = 20 - field`.
    pub hitroll_field: i32,
    /// The file's third stat: `GET_AC = 10 * field`.
    pub ac_field: i32,
    pub hp: Dice,
    /// The damage dice `#d#+#`; `damroll` is its bonus.
    pub damage: Dice,
    pub gold: u32,
    pub experience: u32,
    pub position: i32,
    pub default_position: i32,
    pub sex: i32,
    pub trigger_vnums: Vec<i32>,
}

/// Parse a single mobile; the `#<vnum>` line is already consumed.
pub fn parse_mob(src: &mut Source, vnum: i32) -> Result<RawMob> {
    let name = src.read_string("mob keyword list")?;
    let short_descr = src.read_string("mob short description")?;
    let long_descr = src.read_string("mob long description")?;
    let description = src.read_string("mob description")?;

    let flag_line = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("mob {vnum}: missing flag line")))?;
    let tokens: Vec<&str> = flag_line.split_whitespace().collect();
    let (mob_flags, aff_flags, alignment, letter) = parse_mob_flags(src, vnum, &tokens)?;

    let mut enhanced = false;
    let mut especs = Vec::new();
    let stat1 = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("mob {vnum}: missing first stat line")))?;
    let s1: Vec<&str> = stat1.split_whitespace().collect();
    if s1.len() < 5 {
        return Err(src.parse_error(format!(
            "mob {vnum}: expected `<level> <hr> <ac> <hp> <dam>`, got `{stat1}`"
        )));
    }
    let level = parse_int(src, "mob level", s1[0])?;
    let hitroll_field = parse_int(src, "mob hitroll field", s1[1])?;
    let ac_field = parse_int(src, "mob ac field", s1[2])?;
    let hp = parse_dice(&format!("mob {vnum} hp"), s1[3])?;
    let damage = parse_dice(&format!("mob {vnum} damage"), s1[4])?;

    let gold_exp = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("mob {vnum}: missing gold/exp line")))?;
    let ge: Vec<&str> = gold_exp.split_whitespace().collect();
    if ge.len() < 2 {
        return Err(src.parse_error(format!(
            "mob {vnum}: expected `<gold> <exp>`, got `{gold_exp}`"
        )));
    }
    let gold = non_negative(parse_int(src, "mob gold", ge[0])?);
    let experience = non_negative(parse_int(src, "mob experience", ge[1])?);

    let pos_line = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("mob {vnum}: missing position line")))?;
    let p: Vec<&str> = pos_line.split_whitespace().collect();
    if p.len() < 3 {
        return Err(src.parse_error(format!(
            "mob {vnum}: expected `<pos> <default_pos> <sex>`, got `{pos_line}`"
        )));
    }
    let position = parse_int(src, "mob position", p[0])?;
    let default_position = parse_int(src, "mob default position", p[1])?;
    let sex = parse_int(src, "mob sex", p[2])?;

    if letter.eq_ignore_ascii_case(&'E') {
        enhanced = true;
        loop {
            let line = src
                .next_line()
                .ok_or_else(|| src.parse_error(format!("mob {vnum}: unterminated E section")))?;
            if line.trim() == "E" {
                break;
            }
            if line.trim_start().starts_with('#') {
                return Err(src.parse_error(format!(
                    "mob {vnum}: unterminated E section before `{}`",
                    line.trim()
                )));
            }
            if !line.trim().is_empty() {
                especs.push(line);
            }
        }
    } else if !letter.eq_ignore_ascii_case(&'S') {
        return Err(src.parse_error(format!(
            "mob {vnum}: unsupported mob letter `{letter}` (expected S or E)"
        )));
    }

    let mut trigger_vnums = Vec::new();
    while let Some(next) = src.peek() {
        let trimmed = next.trim_start();
        if let Some(rest) = trimmed.strip_prefix('T') {
            trigger_vnums.push(parse_int(src, "mob trigger vnum", rest.trim())?);
            src.next_line();
        } else {
            break;
        }
    }

    Ok(RawMob {
        vnum,
        name,
        short_descr,
        long_descr,
        description,
        mob_flags,
        aff_flags,
        alignment,
        enhanced,
        especs,
        level,
        hitroll_field,
        ac_field,
        hp,
        damage,
        gold,
        experience,
        position,
        default_position,
        sex,
        trigger_vnums,
    })
}

/// Decode the mobile flag line (128-bit form: 8 flag groups + alignment +
/// letter; legacy form: 4 fields).
fn parse_mob_flags(
    src: &Source,
    vnum: i32,
    tokens: &[&str],
) -> Result<([u64; 4], [u64; 4], i32, char)> {
    let mut mob_flags = [0u64; 4];
    let mut aff_flags = [0u64; 4];
    if tokens.len() >= 10 {
        for (slot, token) in mob_flags.iter_mut().zip(&tokens[0..4]) {
            *slot = ascii_flags(token);
        }
        for (slot, token) in aff_flags.iter_mut().zip(&tokens[4..8]) {
            *slot = ascii_flags_aff(token);
        }
        let alignment = parse_int(src, "mob alignment", tokens[8])?;
        let letter = tokens[9].chars().next().unwrap_or('S');
        Ok((mob_flags, aff_flags, alignment, letter))
    } else if tokens.len() == 4 {
        // Legacy 4-field form: f1 f2 alignment letter.
        mob_flags[0] = ascii_flags(tokens[0]);
        aff_flags[0] = ascii_flags_aff(tokens[1]);
        let alignment = parse_int(src, "mob alignment", tokens[2])?;
        let letter = tokens[3].chars().next().unwrap_or('S');
        Ok((mob_flags, aff_flags, alignment, letter))
    } else {
        Err(src.parse_error(format!(
            "mob {vnum}: expected 10-field (or legacy 4-field) flag line, got {tokens:?}"
        )))
    }
}

fn non_negative(value: i32) -> u32 {
    value.max(0) as u32
}
