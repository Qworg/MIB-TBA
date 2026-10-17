//! `text_processed` (`dg_variables.c`): string functions applied to a
//! variable's value (`%var.strlen%`, `%var.trim%`, `%var.contains(x)%`,
//! `%var.car%`, `%var.cdr%`, `%var.charat(n)%`, `%var.mudcommand%`,
//! `%var.toupper%`).
//!
//! # Deviations from the C
//!
//! - The C writes into a caller buffer of `slen` bytes and truncates; this
//!   returns the whole string and leaves any limit to the caller.
//! - `charat` returns one *byte* in the C. A `String` cannot hold half a
//!   character, so an ASCII byte is returned as itself, the first byte of a
//!   multi-byte character yields that whole character, and a continuation
//!   byte yields the empty string. `strlen` still counts bytes, as the C
//!   does, so indices agree with it.
//! - `trim` works on a copy of at most `MAX_STRING_LENGTH - 1` bytes in the
//!   C; values are far shorter than that here.

use crate::cstr::{atoi, is_space, str_cmp, str_str};
use crate::host::DgHost;

/// Every name [`text_processed`] recognises as a string function, lower-cased
/// and sorted. The converter's coverage analysis classifies script references
/// against this list; a test fails if it and the function disagree.
pub const TEXT_FUNCTIONS: &[&str] = &[
    "car",
    "cdr",
    "charat",
    "contains",
    "mudcommand",
    "strlen",
    "toupper",
    "trim",
];

/// `text_processed(field, subfield, vd, str, slen)`.
///
/// `value` is the variable's value. Returns `Some(result)` when `field` is a
/// text function, `None` when it is not (the caller goes on to treat the
/// value as an entity reference). The host is needed only for `mudcommand`
/// ([`crate::host::BaseHost::mud_command_name`]).
pub fn text_processed<H: DgHost + ?Sized>(
    host: &H,
    field: &str,
    subfield: &str,
    value: &str,
) -> Option<String> {
    if str_cmp(field, "strlen") == 0 {
        Some(i32::try_from(value.len()).unwrap_or(i32::MAX).to_string())
    } else if str_cmp(field, "toupper") == 0 {
        // Only the first character, and only if it is ASCII (`UPPER`).
        let mut chars = value.chars();
        Some(match chars.next() {
            Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
            None => String::new(),
        })
    } else if str_cmp(field, "trim") == 0 {
        Some(value.trim_matches(is_space).to_owned())
    } else if str_cmp(field, "contains") == 0 {
        Some(
            if str_str(value, subfield).is_some() {
                "1"
            } else {
                "0"
            }
            .to_owned(),
        )
    } else if str_cmp(field, "car") == 0 {
        let end = value.find(is_space).unwrap_or(value.len());
        Some(value[..end].to_owned())
    } else if str_cmp(field, "cdr") == 0 {
        // Skip the first word, then the whitespace after it.
        let rest = value.trim_start_matches(|c: char| !is_space(c));
        Some(rest.trim_start_matches(is_space).to_owned())
    } else if str_cmp(field, "charat") == 0 {
        Some(char_at(value, atoi(subfield)))
    } else if str_cmp(field, "mudcommand") == 0 {
        // `strncmp(command, value, strlen(value))`: the first command that
        // `value` abbreviates (so an empty value names the first command).
        Some(host.mud_command_name(value).unwrap_or_default())
    } else {
        None
    }
}

/// `charat`: the `index`th byte (1-based) of `value`, or empty when out of
/// range. `index` is a `size_t` in the C, so a negative index is huge and
/// out of range too.
fn char_at(value: &str, index: i32) -> String {
    let Some(offset) = usize::try_from(index)
        .ok()
        .filter(|index| (1..=value.len()).contains(index))
        .map(|index| index - 1)
    else {
        return String::new();
    };
    // `offset` is in range, and a character starting here is whole.
    value
        .get(offset..)
        .and_then(|tail| tail.chars().next())
        .map(String::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::mock::MockHost;

    #[test]
    fn text_functions_match_the_function() {
        let source = include_str!("text.rs");
        let body = source.split("pub fn text_processed").nth(1).unwrap_or("");
        let body = body.split("/// `charat`: the").next().unwrap_or("");
        let mut arms: Vec<&str> = body
            .split("str_cmp(field, \"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect();
        arms.sort_unstable();
        assert_eq!(TEXT_FUNCTIONS, arms.as_slice());
        for name in TEXT_FUNCTIONS {
            assert!(run(name, "", "abc").is_some(), "{name}");
            assert!(
                run(&name.to_ascii_uppercase(), "", "abc").is_some(),
                "{name}"
            );
        }
    }

    fn run(field: &str, subfield: &str, value: &str) -> Option<String> {
        let mut host = MockHost::new();
        host.globals
            .texts
            .insert("commands".to_owned(), "north east say sit smile".to_owned());
        text_processed(&host, field, subfield, value)
    }

    fn text(field: &str, subfield: &str, value: &str) -> String {
        run(field, subfield, value).unwrap()
    }

    #[test]
    fn unknown_fields_are_not_text_functions() {
        assert_eq!(run("hitp", "", "abc"), None);
        assert_eq!(run("", "", "abc"), None);
        assert_eq!(run("strlen2", "", "abc"), None);
    }

    #[test]
    fn field_names_are_case_insensitive() {
        assert_eq!(text("STRLEN", "", "abc"), "3");
        assert_eq!(text("CharAt", "2", "abc"), "b");
    }

    #[test]
    fn strlen_counts_bytes() {
        assert_eq!(text("strlen", "", ""), "0");
        assert_eq!(text("strlen", "", "hello world"), "11");
        assert_eq!(text("strlen", "", "\u{e9}"), "2");
        assert_eq!(text("strlen", "", &"x".repeat(1000)), "1000");
    }

    #[test]
    fn toupper_changes_only_the_first_ascii_letter() {
        assert_eq!(text("toupper", "", "hello world"), "Hello world");
        assert_eq!(text("toupper", "", "Hello"), "Hello");
        assert_eq!(text("toupper", "", "1abc"), "1abc");
        assert_eq!(text("toupper", "", ""), "");
        assert_eq!(text("toupper", "", "\u{e9}a"), "\u{e9}a");
        assert_eq!(text("toupper", "", "z"), "Z");
    }

    #[test]
    fn trim_strips_both_ends() {
        assert_eq!(text("trim", "", "  hi there \t\r\n"), "hi there");
        assert_eq!(text("trim", "", "hi"), "hi");
        assert_eq!(text("trim", "", "   "), "");
        assert_eq!(text("trim", "", ""), "");
        assert_eq!(text("trim", "", " \u{e9} "), "\u{e9}");
    }

    #[test]
    fn contains_uses_the_flawed_str_str() {
        assert_eq!(text("contains", "WORLD", "Hello world"), "1");
        assert_eq!(text("contains", "xyz", "Hello world"), "0");
        // An empty subfield never matches.
        assert_eq!(text("contains", "", "Hello"), "0");
        assert_eq!(text("contains", "x", ""), "0");
        // No backing up after a partial match: "aaab" does not contain "aab".
        assert_eq!(text("contains", "aab", "aaab"), "0");
        assert_eq!(text("contains", "aab", "xaab"), "1");
    }

    #[test]
    fn car_is_the_first_word() {
        assert_eq!(text("car", "", "give sword now"), "give");
        assert_eq!(text("car", "", "single"), "single");
        assert_eq!(text("car", "", ""), "");
        // Leading whitespace: the first word is empty.
        assert_eq!(text("car", "", " lead"), "");
        assert_eq!(text("car", "", "tab\tsep"), "tab");
        assert_eq!(text("car", "", "\u{e9}t\u{e9} x"), "\u{e9}t\u{e9}");
    }

    #[test]
    fn cdr_is_everything_after_the_first_word() {
        assert_eq!(text("cdr", "", "give  sword now"), "sword now");
        assert_eq!(text("cdr", "", "single"), "");
        assert_eq!(text("cdr", "", ""), "");
        assert_eq!(text("cdr", "", "word   "), "");
        // Leading whitespace: the first word is empty, so whitespace is
        // skipped and the next word is returned.
        assert_eq!(text("cdr", "", " lead rest"), "lead rest");
        assert_eq!(text("cdr", "", "a b  c "), "b  c ");
    }

    #[test]
    fn charat_is_one_based_and_bounds_checked() {
        assert_eq!(text("charat", "1", "abc"), "a");
        assert_eq!(text("charat", "3", "abc"), "c");
        assert_eq!(text("charat", "4", "abc"), "");
        assert_eq!(text("charat", "0", "abc"), "");
        assert_eq!(text("charat", "-1", "abc"), "");
        assert_eq!(text("charat", "", "abc"), "");
        assert_eq!(text("charat", "x", "abc"), "");
        assert_eq!(text("charat", "2junk", "abc"), "b");
        assert_eq!(text("charat", "1", ""), "");
        assert_eq!(text("charat", "99999999999999999999", "abc"), "");
        assert_eq!(text("charat", "1", " "), " ");
    }

    #[test]
    fn charat_on_multibyte_text_never_splits_a_character() {
        // "\u{e9}" is bytes 1 and 2; a lone continuation byte is empty.
        assert_eq!(text("charat", "1", "\u{e9}x"), "\u{e9}");
        assert_eq!(text("charat", "2", "\u{e9}x"), "");
        assert_eq!(text("charat", "3", "\u{e9}x"), "x");
        assert_eq!(text("charat", "4", "\u{e9}x"), "");
    }

    #[test]
    fn mudcommand_names_the_first_command_the_value_abbreviates() {
        assert_eq!(text("mudcommand", "", "sa"), "say");
        assert_eq!(text("mudcommand", "", "s"), "say");
        assert_eq!(text("mudcommand", "", "sm"), "smile");
        assert_eq!(text("mudcommand", "", "nor"), "north");
        assert_eq!(text("mudcommand", "", "zzz"), "");
        // strncmp is case-sensitive.
        assert_eq!(text("mudcommand", "", "NORTH"), "");
        // An empty value matches the first command, as strncmp(…, 0) == 0.
        assert_eq!(text("mudcommand", "", ""), "north");
    }

    #[test]
    fn mudcommand_with_no_command_table_is_empty() {
        let host = MockHost::new();
        assert_eq!(text_processed(&host, "mudcommand", "", "say").unwrap(), "");
    }
}
