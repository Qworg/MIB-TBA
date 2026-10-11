//! Low-level reading of the tbaMUD flat-file format.
//!
//! The authoritative grammar is [`src/db.c`](../../../tbamud/src/db.c):
//!
//! - `get_line` reads a single `\n`-terminated line and strips the newline.
//! - `fread_string` (db.c:3124) accumulates lines until one ends in `~`,
//!   joining them with a newline, and applies `parse_at` (modify.c:80), which
//!   turns `@<c>` colour codes into a tab + code.
//! - `asciiflag_conv` / `asciiflag_conv_aff` (db.c:1209/1232) decode a flag
//!   field that is either a decimal number or a string of letters where
//!   `a`..`z` are bits 0..25 and `A`..`Z` are bits 26..51. The `_aff` variant
//!   shifts every bit up by one (`AFF_DONTUSE == 0` is "no bits set").
//!
//! This module is deliberately line-oriented: the C reader is too, and the
//! record bodies (exits, extra descriptions, affect blocks) are interpreted by
//! the per-type readers on top of it.

use std::path::{Path, PathBuf};

use crate::error::{ConvertError, Result};

/// A source file split into lines with a cursor and one-line pushback.
pub struct Source {
    path: PathBuf,
    lines: Vec<String>,
    pos: usize,
}

impl Source {
    /// Read a file as text. Non-UTF-8 bytes are replaced (lossy) so a stray
    /// Latin-1 byte in one zone cannot abort the whole conversion.
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|source| ConvertError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let text = String::from_utf8_lossy(&bytes);
        // `split('\n')` yields a trailing empty entry for a final newline; that
        // is harmless because fixed-field readers reject blank lines and the
        // record loops skip them.
        let lines = text
            .split('\n')
            .map(|raw| raw.strip_suffix('\r').unwrap_or(raw).to_owned())
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            lines,
            pos: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 1-based line number of the next line to be read.
    pub fn line_no(&self) -> usize {
        self.pos + 1
    }

    pub fn parse_error(&self, message: impl Into<String>) -> ConvertError {
        ConvertError::Parse {
            path: self.path.clone(),
            line: self.line_no(),
            message: message.into(),
        }
    }

    /// Peek at the next line without consuming it.
    pub fn peek(&self) -> Option<&str> {
        self.lines.get(self.pos).map(String::as_str)
    }

    /// Consume the next line, or return `None` at end of file.
    pub fn next_line(&mut self) -> Option<String> {
        let line = self.lines.get(self.pos).cloned();
        if line.is_some() {
            self.pos += 1;
        }
        line
    }

    /// Consume the next non-blank line, skipping blanks.
    pub fn next_nonblank(&mut self) -> Option<String> {
        loop {
            let line = self.next_line()?;
            if !line.trim().is_empty() {
                return Some(line);
            }
        }
    }

    /// Push the most recently read line back so the next `next_line` returns
    /// it again. Object records are not `$`-terminated and must hand the next
    /// record's `#` line back to the caller.
    pub fn unread(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
        }
    }

    /// `fread_string`: accumulate until a line ends in `~`.
    ///
    /// Returns the colour-stripped text. An empty string is returned for a
    /// bare `~` line (the C code returns `NULL`, which callers treat as "").
    pub fn read_string(&mut self, what: &str) -> Result<String> {
        let mut out = String::new();
        loop {
            let line = self
                .next_line()
                .ok_or_else(|| self.parse_error(format!("unexpected end of file in {what}")))?;
            if let Some(stripped) = line.strip_suffix('~') {
                out.push_str(stripped);
                break;
            }
            out.push_str(&line);
            out.push('\n');
        }
        Ok(strip_colour_codes(&out))
    }
}

/// Translate tbaMUD `@`-colour codes to plain text.
///
/// `@@` is a literal `@`; any other `@<c>` is a colour/format code and is
/// dropped. MIB renders its own styling, so the codes must not survive into
/// content.
pub fn strip_colour_codes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '@' {
            match chars.next() {
                Some('@') => out.push('@'),
                Some(_) => {}
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `asciiflag_conv` (db.c:1209).
pub fn ascii_flags(field: &str) -> u64 {
    let trimmed = field.trim();
    if trimmed.is_empty() {
        return 0;
    }
    if is_decimal(trimmed) {
        return trimmed.parse::<i64>().map(|v| v as u64).unwrap_or(0);
    }
    let mut flags: u64 = 0;
    for c in trimmed.chars() {
        if c.is_ascii_lowercase() {
            flags |= 1u64 << (c as u32 - 'a' as u32);
        } else if c.is_ascii_uppercase() {
            flags |= 1u64 << (26 + (c as u32 - 'A' as u32));
        }
    }
    flags
}

/// `asciiflag_conv_aff` (db.c:1232): as [`ascii_flags`], shifted up by one.
pub fn ascii_flags_aff(field: &str) -> u64 {
    let trimmed = field.trim();
    if trimmed.is_empty() {
        return 0;
    }
    if is_decimal(trimmed) {
        return trimmed.parse::<i64>().map(|v| v as u64).unwrap_or(0);
    }
    let mut flags: u64 = 0;
    for c in trimmed.chars() {
        if c.is_ascii_lowercase() {
            flags |= 1u64 << (c as u32 - 'a' as u32 + 1);
        } else if c.is_ascii_uppercase() {
            flags |= 1u64 << (26 + (c as u32 - 'A' as u32 + 1));
        }
    }
    flags
}

/// True when the field is a (possibly negative) decimal integer.
fn is_decimal(field: &str) -> bool {
    let mut chars = field.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !(first.is_ascii_digit() || first == '-') {
        return false;
    }
    chars.all(|c| c.is_ascii_digit())
}

/// Test a bit in a flag word.
pub fn bit(flags: u64, index: u32) -> bool {
    (flags >> index) & 1 == 1
}

/// Parse a whitespace-delimited integer token.
pub fn parse_int(src: &Source, what: &str, token: &str) -> Result<i32> {
    token
        .parse::<i32>()
        .map_err(|_| src.parse_error(format!("expected an integer for {what}, got `{token}`")))
}

/// Attempts at a `Dice` expression. Kept here so the per-type readers share it.
pub fn parse_dice(context: &str, value: &str) -> Result<mib_core::prelude::Dice> {
    value.parse().map_err(|_| ConvertError::Dice {
        context: context.to_owned(),
        value: value.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn ascii_flags_numeric_and_letters() {
        assert_eq!(ascii_flags("26635"), 26635);
        assert_eq!(ascii_flags("cdq"), (1 << 2) | (1 << 3) | (1 << 16));
        assert_eq!(ascii_flags("0"), 0);
    }

    #[test]
    fn ascii_flags_aff_shifts_by_one() {
        assert_eq!(ascii_flags_aff("a"), 1 << 1);
        assert_eq!(ascii_flags_aff("A"), 1 << 27);
    }

    #[test]
    fn strip_colour_keeps_doubled_at() {
        assert_eq!(strip_colour_codes("a@@b@rc"), "a@bc");
    }
}
