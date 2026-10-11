//! Source readers, one per tbaMUD file type, plus the low-level tokenizer.
//!
//! Each reader mirrors the corresponding C function in `src/db.c` (and
//! `src/dg_db_scripts.c` / `src/quest.c` for the deferred types) closely
//! enough that a record the loader accepts is parsed the same way here.

pub mod deferred;
pub mod mob;
pub mod obj;
pub mod quest;
pub mod room;
pub mod shop;
pub mod social;
pub mod text;
pub mod tokens;
pub mod zone;

use crate::error::Result;
use tokens::Source;

/// A keyword/description block shared by rooms and objects.
#[derive(Debug, Clone)]
pub struct RawExtraDesc {
    pub keywords: String,
    pub description: String,
}

/// Parse every room in a `.wld` file.
pub fn parse_rooms(src: &mut Source) -> Result<Vec<room::RawRoom>> {
    let mut out = Vec::new();
    while let Some(line) = src.next_nonblank() {
        let trimmed = line.trim();
        if trimmed.starts_with('$') {
            break;
        }
        let vnum = record_vnum(src, trimmed, "room")?;
        if vnum >= 99999 {
            break;
        }
        out.push(room::parse_room(src, vnum)?);
    }
    Ok(out)
}

/// Parse every mobile in a `.mob` file.
pub fn parse_mobs(src: &mut Source) -> Result<Vec<mob::RawMob>> {
    let mut out = Vec::new();
    while let Some(line) = src.next_nonblank() {
        let trimmed = line.trim();
        if trimmed.starts_with('$') {
            break;
        }
        let vnum = record_vnum(src, trimmed, "mob")?;
        if vnum >= 99999 {
            break;
        }
        out.push(mob::parse_mob(src, vnum)?);
    }
    Ok(out)
}

/// Parse every object in a `.obj` file. Object records are not terminated, so
/// the reader leaves the next record's header for the loop.
pub fn parse_objs(src: &mut Source) -> Result<Vec<obj::RawObj>> {
    let mut out = Vec::new();
    while let Some(line) = src.next_nonblank() {
        let trimmed = line.trim();
        if trimmed.starts_with('$') {
            break;
        }
        let vnum = record_vnum(src, trimmed, "object")?;
        if vnum >= 99999 {
            break;
        }
        out.push(obj::parse_obj(src, vnum)?);
    }
    Ok(out)
}

fn record_vnum(src: &Source, line: &str, what: &str) -> Result<i32> {
    let digits: String = line
        .trim_start_matches('#')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse::<i32>()
        .map_err(|_| src.parse_error(format!("expected `#<{what} vnum>`, got `{line}`")))
}
