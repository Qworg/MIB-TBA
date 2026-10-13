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

/// A parsed script body (`trig_data::cmdlist`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Program {
    pub lines: Vec<Line>,
}

impl Program {
    /// Split a script body into lines.
    pub fn parse(source: &str) -> Self {
        Self {
            lines: source
                .lines()
                .map(|line| Line {
                    cmd: line.to_owned(),
                })
                .collect(),
        }
    }
}
