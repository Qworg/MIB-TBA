//! The `lib/misc/socials.new` reader (issue #17).
//!
//! The source checkout runs the aedit format (`use_new_socials = YES`,
//! `config.c:271`), so `SOCMESS_FILE_NEW` (`lib/misc/socials.new`) is the
//! active social table and `lib/misc/socials` is the stale legacy file. The
//! grammar is `boot_social_messages` (`src/db.c:195`) for the new branch, and
//! `save_socials` (`src/aedit.c:341`) for the on-disk shape:
//!
//! ```text
//! ~<command> <sort_as> <hide> <min_char_pos> <min_victim_pos> <min_level>
//! <char_no_arg>
//! <others_no_arg>
//! <char_found>
//! <others_found>
//! <vict_found>
//! <not_found>
//! <char_auto>
//! <others_auto>
//! <char_body_found>
//! <others_body_found>
//! <vict_body_found>
//! <char_obj_found>
//! <others_obj_found>
//! <blank separator>
//! ```
//!
//! A message line beginning with `#` is Circle's NULL. The file ends with `$`.

use crate::error::Result;
use crate::source::tokens::{Source, strip_colour_codes};

/// Number of message strings per social.
pub const SOCIAL_MESSAGES: usize = 13;

/// A parsed social record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSocial {
    pub command: String,
    pub sort_as: String,
    pub hide: i32,
    pub min_char_position: i32,
    pub min_victim_position: i32,
    pub min_level_char: i32,
    /// The thirteen message strings in source order; `None` is the `#` NULL.
    pub messages: Vec<Option<String>>,
}

/// Parse every social in a `socials.new` file.
pub fn parse_socials(src: &mut Source) -> Result<Vec<RawSocial>> {
    let mut out = Vec::new();
    while let Some(header) = src.next_nonblank() {
        let header = header.trim();
        if header.starts_with('$') {
            break;
        }
        let Some(fields) = header.strip_prefix('~') else {
            continue;
        };
        let tokens: Vec<&str> = fields.split_whitespace().collect();
        if tokens.len() < 6 {
            return Err(src.parse_error(format!(
                "social header needs 6 fields, got {}: `{header}`",
                tokens.len()
            )));
        }
        let (command, sort_as) = (tokens[0].to_owned(), tokens[1].to_owned());
        let mut numbers = [0i32; 4];
        for (index, slot) in numbers.iter_mut().enumerate() {
            *slot = tokens[2 + index].parse::<i32>().map_err(|_| {
                src.parse_error(format!(
                    "social `{command}` field {} is not an integer: `{}`",
                    index + 2,
                    tokens[index + 2]
                ))
            })?;
        }
        let mut messages = Vec::with_capacity(SOCIAL_MESSAGES);
        for _ in 0..SOCIAL_MESSAGES {
            let line = src.next_line().ok_or_else(|| {
                src.parse_error(format!("social `{command}`: missing message line"))
            })?;
            let line = line.trim_end();
            messages.push((!line.starts_with('#')).then(|| strip_colour_codes(line)));
        }
        out.push(RawSocial {
            command,
            sort_as,
            hide: numbers[0],
            min_char_position: numbers[1],
            min_victim_position: numbers[2],
            min_level_char: numbers[3],
            messages,
        });
    }
    Ok(out)
}
