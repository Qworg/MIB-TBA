//! `.obj` reader — mirrors `parse_object` (`db.c:2087`).
//!
//! Object records have no end-of-record marker, so the parser reads until the
//! next `#`/`$` line and hands it back to the caller with `Source::unread`.

use crate::error::Result;
use crate::source::RawExtraDesc;
use crate::source::tokens::{Source, ascii_flags, ascii_flags_aff, parse_int};

/// `(type, extra_flags, wear_flags, affect_flags)` for one object.
type ObjFlags = (i32, [u64; 4], [u64; 4], [u64; 4]);

/// One object prototype.
#[derive(Debug, Clone)]
pub struct RawObj {
    pub vnum: i32,
    pub name: String,
    pub short_description: String,
    pub description: String,
    pub action_description: String,
    pub obj_type: i32,
    pub extra_flags: [u64; 4],
    pub wear_flags: [u64; 4],
    /// `GET_OBJ_AFFECT` (`AFF_*`) bits — carried, but no target field yet.
    pub affect_flags: [u64; 4],
    pub values: [i32; 4],
    pub weight: i32,
    pub cost: i32,
    pub rent: i32,
    pub level: i32,
    pub timer: i32,
    pub applies: Vec<RawApply>,
    pub extra_descriptions: Vec<RawExtraDesc>,
    pub trigger_vnums: Vec<i32>,
}

/// One `A` affect line.
#[derive(Debug, Clone, Copy)]
pub struct RawApply {
    pub location: i32,
    pub modifier: i32,
}

/// Parse a single object; the `#<vnum>` line is already consumed.
pub fn parse_obj(src: &mut Source, vnum: i32) -> Result<RawObj> {
    let name = src.read_string("object keyword list")?;
    let short_description = src.read_string("object short description")?;
    let description = src.read_string("object description")?;
    let action_description = src.read_string("object action description")?;

    let first = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("object {vnum}: missing flag line")))?;
    let (obj_type, extra_flags, wear_flags, affect_flags) = parse_obj_flags(src, vnum, &first)?;

    let values_line = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("object {vnum}: missing value line")))?;
    let vt: Vec<&str> = values_line.split_whitespace().collect();
    if vt.len() < 4 {
        return Err(src.parse_error(format!(
            "object {vnum}: expected four values, got `{values_line}`"
        )));
    }
    let values = [
        parse_int(src, "object value 0", vt[0])?,
        parse_int(src, "object value 1", vt[1])?,
        parse_int(src, "object value 2", vt[2])?,
        parse_int(src, "object value 3", vt[3])?,
    ];

    let third = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("object {vnum}: missing weight/cost line")))?;
    let wt: Vec<&str> = third.split_whitespace().collect();
    if wt.len() < 3 {
        return Err(src.parse_error(format!(
            "object {vnum}: expected `<weight> <cost> <rent> [level] [timer]`, got `{third}`"
        )));
    }
    let weight = parse_int(src, "object weight", wt[0])?;
    let cost = parse_int(src, "object cost", wt[1])?;
    let rent = parse_int(src, "object rent", wt[2])?;
    let level = wt
        .get(3)
        .map_or(Ok(0), |t| parse_int(src, "object level", t))?;
    let timer = wt
        .get(4)
        .map_or(Ok(0), |t| parse_int(src, "object timer", t))?;

    let mut applies = Vec::new();
    let mut extra_descriptions = Vec::new();
    let mut trigger_vnums = Vec::new();

    while let Some(line) = src.next_line() {
        let trimmed = line.trim_start();
        match trimmed.chars().next() {
            Some('A') => applies.push(parse_apply(src, vnum)?),
            Some('E') => {
                let keywords = src.read_string("object extra description keywords")?;
                let description = src.read_string("object extra description text")?;
                extra_descriptions.push(RawExtraDesc {
                    keywords,
                    description,
                });
            }
            Some('T') => {
                let rest = trimmed[1..].trim();
                // `dg_obj_trigger` receives the whole `T <vnum>` line.
                let vnum_field = rest.split_whitespace().next().unwrap_or("");
                trigger_vnums.push(parse_int(src, "object trigger vnum", vnum_field)?);
            }
            Some('#') | Some('$') => {
                src.unread();
                break;
            }
            _ => {
                return Err(
                    src.parse_error(format!("object {vnum}: expected A/E/T/#/$, got `{line}`"))
                );
            }
        }
    }

    Ok(RawObj {
        vnum,
        name,
        short_description,
        description,
        action_description,
        obj_type,
        extra_flags,
        wear_flags,
        affect_flags,
        values,
        weight,
        cost,
        rent,
        level,
        timer,
        applies,
        extra_descriptions,
        trigger_vnums,
    })
}

fn parse_obj_flags(src: &Source, vnum: i32, line: &str) -> Result<ObjFlags> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut extra = [0u64; 4];
    let mut wear = [0u64; 4];
    let mut affect = [0u64; 4];
    if tokens.len() >= 13 {
        let obj_type = parse_int(src, "object type", tokens[0])?;
        for (slot, token) in extra.iter_mut().zip(&tokens[1..5]) {
            *slot = ascii_flags(token);
        }
        for (slot, token) in wear.iter_mut().zip(&tokens[5..9]) {
            *slot = ascii_flags(token);
        }
        for (slot, token) in affect.iter_mut().zip(&tokens[9..13]) {
            *slot = ascii_flags_aff(token);
        }
        Ok((obj_type, extra, wear, affect))
    } else if tokens.len() >= 4 {
        // Legacy: type extra wear affect.
        let obj_type = parse_int(src, "object type", tokens[0])?;
        extra[0] = ascii_flags(tokens[1]);
        wear[0] = ascii_flags(tokens[2]);
        affect[0] = ascii_flags_aff(tokens[3]);
        Ok((obj_type, extra, wear, affect))
    } else {
        Err(src.parse_error(format!(
            "object {vnum}: expected 13-field (or legacy 4-field) flag line, got `{line}`"
        )))
    }
}

fn parse_apply(src: &mut Source, vnum: i32) -> Result<RawApply> {
    let line = src
        .next_nonblank()
        .ok_or_else(|| src.parse_error(format!("object {vnum}: A block missing numbers")))?;
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return Err(src.parse_error(format!(
            "object {vnum}: A block expected `<location> <modifier>`, got `{line}`"
        )));
    }
    Ok(RawApply {
        location: parse_int(src, "affect location", tokens[0])?,
        modifier: parse_int(src, "affect modifier", tokens[1])?,
    })
}
