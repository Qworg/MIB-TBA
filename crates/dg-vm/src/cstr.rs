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

/// C `isspace` in the "C" locale.
pub fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// C `atoi`: skip leading whitespace, read an optional sign and the leading
/// digits, ignore the rest. No digits yields `0`. Overflow wraps (glibc
/// saturates the intermediate `long` instead; the two differ only past 19
/// digits).
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

/// `utils.c` `str_cmp`: ASCII case-insensitive comparison. Returns the
/// difference of the first differing pair of lowercased characters (`0` when
/// equal), which is the value `eval_op` prints for `!=` on strings.
pub fn str_cmp(a: &str, b: &str) -> i32 {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    for index in 0..a.len().max(b.len()) {
        // Past the end the C reads the terminating NUL. `char` is signed.
        let x = i32::from(a.get(index).copied().unwrap_or(0).to_ascii_lowercase() as i8);
        let y = i32::from(b.get(index).copied().unwrap_or(0).to_ascii_lowercase() as i8);
        if x != y {
            return x - y;
        }
    }
    0
}

/// `utils.c` `strn_cmp`: as [`str_cmp`] over at most the first `n` bytes.
pub fn strn_cmp(a: &str, b: &str, n: usize) -> i32 {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    for index in 0..n {
        let x = a.get(index).copied().unwrap_or(0);
        let y = b.get(index).copied().unwrap_or(0);
        let diff =
            i32::from(x.to_ascii_lowercase() as i8) - i32::from(y.to_ascii_lowercase() as i8);
        if diff != 0 {
            return diff;
        }
        if x == 0 {
            break;
        }
    }
    0
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
///
/// This is **not** a correct substring search, and the port keeps the flaw:
/// after a partial match the C carries on from where the match failed rather
/// than backing up, so `str_str("aaab", "aab")` finds nothing. `/=`,
/// `contains` and speech matching all inherit it.
pub fn str_str(haystack: &str, needle: &str) -> Option<usize> {
    let (cs, ct) = (haystack.as_bytes(), needle.as_bytes());
    let first = ct.first()?.to_ascii_lowercase();
    let mut i = 0;
    while i < cs.len() {
        while i < cs.len() && cs[i].to_ascii_lowercase() != first {
            i += 1;
        }
        let start = i;
        let mut t = 0;
        while t < ct.len() && i < cs.len() && cs[i].eq_ignore_ascii_case(&ct[t]) {
            t += 1;
            i += 1;
        }
        if t == ct.len() {
            return Some(start);
        }
    }
    None
}

/// `interpreter.c` `skip_spaces`: drop leading whitespace. The C stops at a
/// tab; so does this.
pub fn skip_spaces(text: &str) -> &str {
    text.trim_start_matches(|c: char| c != '\t' && is_space(c))
}

/// The `fill[]` words `one_argument` skips (`interpreter.c`).
pub const FILL_WORDS: &[&str] = &["in", "from", "with", "the", "on", "at", "to"];

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

/// `interpreter.c` `one_word`: as [`any_one_arg`], but a leading `"` takes
/// everything up to the closing `"` as the word.
pub fn one_word(argument: &str) -> (String, &str) {
    let argument = skip_spaces(argument);
    match argument.strip_prefix('"') {
        Some(quoted) => match quoted.split_once('"') {
            Some((word, rest)) => (word.to_ascii_lowercase(), rest),
            None => (quoted.to_ascii_lowercase(), ""),
        },
        None => any_one_arg(argument),
    }
}

/// `interpreter.c` `search_block`: the index of `arg` in `list`.
///
/// `arg` is lowercased first, and one beginning with `!` never matches.
/// `exact` requires equality; otherwise `arg` need only be a prefix of the
/// entry (an empty `arg` matches only an empty entry). `list` has no `"\n"`
/// terminator.
pub fn search_block(arg: &str, list: &[&str], exact: bool) -> Option<usize> {
    if arg.starts_with('!') {
        return None;
    }
    let arg = arg.to_ascii_lowercase();
    list.iter().position(|entry| {
        if exact {
            *entry == arg
        } else if arg.is_empty() {
            entry.is_empty()
        } else {
            entry.starts_with(arg.as_str())
        }
    })
}

/// `dg_scripts.c` `matching_quote`: `open` is the byte index of a `"`; returns
/// the index of the closing `"`, or of the last byte when there is none. A
/// backslash escapes the next character.
///
/// The returned index plus one is always a character boundary; the index
/// itself may not be when the string ends mid-quote.
pub fn matching_quote(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut p = open + 1;
    while p < bytes.len() && bytes[p] != b'"' {
        if bytes[p] == b'\\' && p + 1 < bytes.len() {
            p += 1;
        }
        p += 1;
    }
    if p >= bytes.len() {
        p = bytes.len().saturating_sub(1);
    }
    p
}

/// `dg_scripts.c` `matching_paren`: `open` is the byte index of a `(`; returns
/// the index of the matching `)`, or of the last byte when it is unbalanced.
/// Parentheses inside double quotes do not count.
pub fn matching_paren(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut p = open + 1;
    let mut depth = 1;
    while p < bytes.len() && depth != 0 {
        match bytes[p] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'"' => p = matching_quote(text, p),
            _ => {}
        }
        p += 1;
    }
    p.saturating_sub(1).max(open)
}

/// `utils.c` `sprintbit`: the names of the set bits, each followed by a
/// space; `"NOBITS "` when none is set. A bit past the end of `names` prints
/// as `UNDEFINED`. `names` has no `"\n"` terminator.
pub fn sprintbit(mut bitvector: u64, names: &[&str]) -> String {
    let mut out = String::new();
    let mut nr = 0;
    while bitvector != 0 {
        if bitvector & 1 != 0 {
            out.push_str(names.get(nr).copied().unwrap_or("UNDEFINED"));
            out.push(' ');
        }
        if nr < names.len() {
            nr += 1;
        }
        bitvector >>= 1;
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
    out
}

/// `utils.c` `sprinttype`: the `kind`th name, or `UNDEFINED` past the end.
pub fn sprinttype(kind: i32, names: &[&str]) -> String {
    usize::try_from(kind)
        .ok()
        .and_then(|index| names.get(index))
        .copied()
        .unwrap_or("UNDEFINED")
        .to_owned()
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
        assert_eq!(str_cmp("Abc", "aBC"), 0);
        assert_eq!(str_cmp("ab", "abc"), -i32::from(b'c'));
        assert_eq!(str_cmp("b", "a"), 1);
        assert_eq!(strn_cmp("WAIT 5", "wait until", 5), 0);
        assert!(strn_cmp("wai", "wait", 4) < 0);
    }

    #[test]
    fn str_str_finds_case_insensitively() {
        assert_eq!(str_str("Hello World", "WORLD"), Some(6));
        assert_eq!(str_str("abc", ""), None);
        assert_eq!(str_str("ab", "abc"), None);
        // The C does not back up after a partial match.
        assert_eq!(str_str("aaab", "aab"), None);
        assert_eq!(str_str("xaab", "aab"), Some(1));
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
        assert_eq!(
            one_argument("into the bag"),
            ("into".to_owned(), " the bag")
        );
        assert_eq!(
            one_word("\"Magic Missile\" bob"),
            ("magic missile".to_owned(), " bob")
        );
        assert_eq!(one_word("Fireball bob"), ("fireball".to_owned(), " bob"));
    }

    #[test]
    fn abbreviations() {
        assert!(is_abbrev("nor", "north"));
        assert!(!is_abbrev("", "north"));
        assert!(!is_abbrev("northx", "north"));
        assert!(starts_with_ci("Wait 5", "wait "));
    }

    #[test]
    fn quotes_and_parens_match() {
        assert_eq!(matching_quote("\"ab\" c", 0), 3);
        assert_eq!(matching_quote("\"ab", 0), 2);
        assert_eq!(matching_quote("\"a\\\"b\"", 0), 5);
        assert_eq!(matching_paren("(a (b) c) d", 0), 8);
        assert_eq!(matching_paren("(a \")\" b)", 0), 8);
        assert_eq!(matching_paren("(a", 0), 1);
        assert_eq!(matching_paren("(", 0), 0);
    }

    #[test]
    fn search_block_and_sprint() {
        let list = ["north", "east", "south"];
        assert_eq!(search_block("SOUTH", &list, true), Some(2));
        assert_eq!(search_block("so", &list, false), Some(2));
        assert_eq!(search_block("so", &list, true), None);
        assert_eq!(search_block("!north", &list, false), None);
        assert_eq!(search_block("", &list, false), None);
        assert_eq!(sprintbit(0b101, &["A", "B", "C"]), "A C ");
        assert_eq!(sprintbit(0, &["A"]), "NOBITS ");
        assert_eq!(sprintbit(0b10, &["A"]), "UNDEFINED ");
        assert_eq!(sprinttype(1, &["x", "y"]), "y");
        assert_eq!(sprinttype(5, &["x", "y"]), "UNDEFINED");
    }
}
