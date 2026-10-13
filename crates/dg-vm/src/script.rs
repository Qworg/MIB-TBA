//! A script body as a list of lines (`cmdlist_element`).
//!
//! Ports the loading half of `dg_db_scripts.c` `parse_trigger`: the body is
//! split into lines and kept verbatim, because `script_driver` interprets it a
//! line at a time and its control flow (`find_end`, `find_else_end`,
//! `find_case`, `find_done`) scans the raw text.

/// One script line (`cmdlist_element::cmd`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The line as written, without its line terminator.
    pub cmd: String,
}

impl Line {
    /// The line without its leading whitespace: the `p` that every
    /// `for (p = c->cmd; *p && isspace(*p); p++);` in `dg_scripts.c` points
    /// at, and what `script_driver` and the `find_*` scans test their
    /// keywords against.
    pub fn body(&self) -> &str {
        self.cmd.trim_start_matches(crate::cstr::is_space)
    }
}

/// A parsed script body (`trig_data::cmdlist`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Program {
    pub lines: Vec<Line>,
}

impl Program {
    /// Split a script body into lines as `parse_trigger` does with
    /// `strtok(s, "\n\r")`: either character ends a line, and empty lines
    /// are dropped (a line of only spaces is kept).
    pub fn parse(source: &str) -> Self {
        Self {
            lines: source
                .split(['\n', '\r'])
                .filter(|line| !line.is_empty())
                .map(|line| Line {
                    cmd: line.to_owned(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parse_splits_on_either_terminator_and_drops_empty_lines() {
        let program = Program::parse("a\r\nb\n\n\rc\n   \n");
        let cmds: Vec<&str> = program.lines.iter().map(|l| l.cmd.as_str()).collect();
        assert_eq!(cmds, vec!["a", "b", "c", "   "]);
        assert!(Program::parse("").lines.is_empty());
        assert!(Program::parse("\n\r\n").lines.is_empty());
    }

    #[test]
    fn body_skips_leading_whitespace_only() {
        let program = Program::parse("  \t if x \n\x0b\x0cend\n   \n\u{a0}caf\u{e9}");
        assert_eq!(program.lines[0].body(), "if x ");
        assert_eq!(program.lines[1].body(), "end");
        assert_eq!(program.lines[2].body(), "");
        // Non-ASCII whitespace is not isspace in the C locale.
        assert_eq!(program.lines[3].body(), "\u{a0}caf\u{e9}");
    }
}
