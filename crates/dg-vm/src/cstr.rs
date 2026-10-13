//! The C string helpers DG's behaviour depends on.
//!
//! Ports of the CircleMUD/tbaMUD utilities the interpreter calls
//! (`interpreter.c`, `utils.c`, `dg_scripts.c`). They are reproduced exactly
//! because script behaviour leans on their quirks: `atoi` reads a leading
//! integer and ignores the rest, `one_argument` lowercases and skips fill
//! words, `str_cmp` is ASCII case-insensitive.
//!
//! All functions are ASCII-oriented, like the C. Non-ASCII bytes pass through
//! untouched and never match a class test.

use std::cmp::Ordering;

/// C `isspace` in the "C" locale.
pub fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// C `atoi`: skip leading whitespace, read an optional sign and the leading
/// digits, ignore the rest. No digits yields `0`. Overflow wraps like a 32-bit
/// `int` accumulated in the usual way.
pub fn atoi(text: &str) -> i32 {
    let mut chars = text.chars().skip_while(|c| is_space(*c)).peekable();
    let mut negative = false;
    match chars.peek() {
        Some('-') => {
            negative = true;
            chars.next();
        }
        Some('+') => {
            chars.next();
        }
        _ => {}
    }
    let mut value: i32 = 0;
    for c in chars {
        let Some(digit) = c.to_digit(10) else {
            break;
        };
        value = value.wrapping_mul(10).wrapping_add(digit as i32);
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

/// C `atol`, as [`atoi`] but 64-bit (ids are `long`).
pub fn atol(text: &str) -> i64 {
    let mut chars = text.chars().skip_while(|c| is_space(*c)).peekable();
    let mut negative = false;
    match chars.peek() {
        Some('-') => {
            negative = true;
            chars.next();
        }
        Some('+') => {
            chars.next();
        }
        _ => {}
    }
    let mut value: i64 = 0;
    for c in chars {
        let Some(digit) = c.to_digit(10) else {
            break;
        };
        value = value.wrapping_mul(10).wrapping_add(i64::from(digit));
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

/// `dg_scripts.c` `is_num`: a non-empty string of digits with an optional
/// leading `+` or `-`. Note that a lone sign is a number here, as in the C.
pub fn is_num(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let digits = text
        .strip_prefix('+')
        .or_else(|| text.strip_prefix('-'))
        .unwrap_or(text);
    digits.chars().all(|c| c.is_ascii_digit())
}

/// `interpreter.c` `is_number`: an optional leading `-` followed by at least
/// one digit and nothing else.
pub fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// `utils.c` `str_cmp`: ASCII case-insensitive comparison.
pub fn str_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.bytes().map(|c| c.to_ascii_lowercase());
    let mut right = b.bytes().map(|c| c.to_ascii_lowercase());
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (x, y) => {
                // The C compares against the terminating NUL, so a shorter
                // string sorts first.
                let (x, y) = (x.unwrap_or(0), y.unwrap_or(0));
                if x != y {
                    return x.cmp(&y);
                }
            }
        }
    }
}

/// `!str_cmp(a, b)`.
pub fn str_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// `!strn_cmp(prefix, text, strlen(prefix))`: `text` starts with `prefix`,
/// ASCII case-insensitively.
pub fn starts_with_ci(text: &str, prefix: &str) -> bool {
    let (text, prefix) = (text.as_bytes(), prefix.as_bytes());
    text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `interpreter.c` `is_abbrev`: `arg1` is a non-empty, case-insensitive prefix
/// of `arg2`.
pub fn is_abbrev(arg1: &str, arg2: &str) -> bool {
    !arg1.is_empty() && starts_with_ci(arg2, arg1)
}

/// `dg_scripts.c` `str_str`: the byte offset of the first case-insensitive
/// occurrence of `needle` in `haystack`. An empty needle never matches.
pub fn str_str(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let (hay, pat) = (haystack.as_bytes(), needle.as_bytes());
    if pat.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - pat.len())
        .find(|&start| hay[start..start + pat.len()].eq_ignore_ascii_case(pat))
}

/// `interpreter.c` `skip_spaces`: drop leading whitespace. The C stops at a
/// tab; so does this.
pub fn skip_spaces(text: &str) -> &str {
    text.trim_start_matches(|c: char| c != '\t' && is_space(c))
}

/// The `fill[]` words `one_argument` skips (`interpreter.c`).
pub const FILL_WORDS: &[&str] = &["in", "into", "from", "with", "the", "on", "at", "to"];

/// `interpreter.c` `fill_word`.
pub fn fill_word(word: &str) -> bool {
    FILL_WORDS.iter().any(|fill| str_eq(fill, word))
}

/// `interpreter.c` `any_one_arg`: the next whitespace-delimited word,
/// **lowercased**, and the remainder (not space-skipped).
pub fn any_one_arg(argument: &str) -> (String, &str) {
    let argument = skip_spaces(argument);
    let end = argument
        .find(|c: char| is_space(c))
        .unwrap_or(argument.len());
    (argument[..end].to_ascii_lowercase(), &argument[end..])
}

/// `interpreter.c` `one_argument`: as [`any_one_arg`], but skips fill words.
pub fn one_argument(mut argument: &str) -> (String, &str) {
    loop {
        let (word, rest) = any_one_arg(argument);
        argument = rest;
        if !fill_word(&word) {
            return (word, argument);
        }
    }
}

/// `interpreter.c` `two_arguments`.
pub fn two_arguments(argument: &str) -> (String, String, &str) {
    let (first, rest) = one_argument(argument);
    let (second, rest) = one_argument(rest);
    (first, second, rest)
}

/// `interpreter.c` `half_chop`: the first word (lowercased, fill words kept)
/// and the space-skipped remainder.
pub fn half_chop(text: &str) -> (String, &str) {
    let (first, rest) = any_one_arg(text);
    (first, skip_spaces(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi_reads_a_leading_integer() {
        assert_eq!(atoi("42"), 42);
        assert_eq!(atoi("  -7 apples"), -7);
        assert_eq!(atoi("+3"), 3);
        assert_eq!(atoi("abc"), 0);
        assert_eq!(atoi(""), 0);
        assert_eq!(atoi("12abc34"), 12);
    }

    #[test]
    fn is_num_accepts_a_lone_sign_like_the_c() {
        assert!(is_num("12"));
        assert!(is_num("-12"));
        assert!(is_num("-"));
        assert!(!is_num(""));
        assert!(!is_num("1 2"));
        assert!(!is_number("-"));
        assert!(is_number("-5"));
    }

    #[test]
    fn str_cmp_is_case_insensitive_and_orders() {
        assert_eq!(str_cmp("Abc", "aBC"), Ordering::Equal);
        assert_eq!(str_cmp("ab", "abc"), Ordering::Less);
        assert_eq!(str_cmp("b", "a"), Ordering::Greater);
    }

    #[test]
    fn str_str_finds_case_insensitively() {
        assert_eq!(str_str("Hello World", "WORLD"), Some(6));
        assert_eq!(str_str("abc", ""), None);
        assert_eq!(str_str("ab", "abc"), None);
    }

    #[test]
    fn one_argument_lowercases_and_skips_fill_words() {
        assert_eq!(
            one_argument("  The Sword rest"),
            ("sword".to_owned(), " rest")
        );
        assert_eq!(any_one_arg("The Sword"), ("the".to_owned(), " Sword"));
        assert_eq!(half_chop("Give  it now"), ("give".to_owned(), "it now"));
        let (a, b, rest) = two_arguments("put sword in the bag now");
        assert_eq!(
            (a.as_str(), b.as_str(), rest),
            ("put", "sword", " in the bag now")
        );
    }

    #[test]
    fn abbreviations() {
        assert!(is_abbrev("nor", "north"));
        assert!(!is_abbrev("", "north"));
        assert!(!is_abbrev("northx", "north"));
        assert!(starts_with_ci("Wait 5", "wait "));
    }
}
