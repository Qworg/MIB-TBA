//! `lib/text/*` and `lib/misc/messages` → `stdlib.help` topics (issue #18).
//!
//! tbaMUD keeps its login/information prose in `lib/text/` and its combat
//! message table in `lib/misc/messages`. MIB has no dedicated home for either,
//! so both are rendered as authored `stdlib.help` topics: one topic per text
//! file, and one per attack type for the message file. Every line is carried
//! into a topic body, so nothing is dropped silently.
//!
//! Colour: the text files contain Circle `@`-codes (`@RHELP COLORCODES@n`).
//! The engine's help renderer strips ANSI/control characters but not the
//! literal `@R` sequence, so the converter resolves the codes here with
//! [`strip_colour_codes`] — otherwise they would reach the client verbatim.
//!
//! The message file grammar is `load_messages` (`src/msgedit.c:97`): a `*`
//! comment names the attack type, an `M` line begins an entry, the next line
//! is the numeric attack type, and the following twelve lines are the
//! die/miss/hit/god × attacker/victim/room strings (a line beginning with `#`
//! is Circle's NULL). Several `M` entries may share one attack type; each is a
//! random variant, grouped here under one topic.

use std::collections::BTreeMap;
use std::path::Path;

use mib_content::prelude::SmolStr;
use mib_stdlib::content::{HelpAudience, HelpTopicDef};

use crate::error::{ConvertError, Result};
use crate::source::tokens::strip_colour_codes;

/// One `lib/text/` file and the help topic it becomes.
struct TextFile {
    /// File name under `lib/text/`.
    file: &'static str,
    /// Topic id (`help <id>`).
    id: &'static str,
    title: &'static str,
    category: &'static str,
    audience: HelpAudience,
}

/// The ten `lib/text/` files converted by issue #18. `imotd`, `handbook`,
/// `wizlist` and `immlist` are immortal-facing in Circle, so they become
/// [`HelpAudience::Staff`] topics (hidden from non-admins at list and detail
/// time). `lib/text/credits` is not a help topic: the engine's `credits`
/// command serves the credit text, from the pack's `stdlib.credits` section.
const TEXT_FILES: [TextFile; 10] = [
    TextFile {
        file: "motd",
        id: "motd",
        title: "Message of the Day",
        category: "information",
        audience: HelpAudience::Public,
    },
    TextFile {
        file: "imotd",
        id: "imotd",
        title: "Immortal Message of the Day",
        category: "staff",
        audience: HelpAudience::Staff,
    },
    TextFile {
        file: "news",
        id: "news",
        title: "News",
        category: "information",
        audience: HelpAudience::Public,
    },
    TextFile {
        file: "handbook",
        id: "handbook",
        title: "Immortal Handbook",
        category: "staff",
        audience: HelpAudience::Staff,
    },
    TextFile {
        file: "policies",
        id: "policies",
        title: "Policies",
        category: "information",
        audience: HelpAudience::Public,
    },
    TextFile {
        file: "background",
        id: "background",
        title: "Background",
        category: "information",
        audience: HelpAudience::Public,
    },
    TextFile {
        file: "info",
        id: "info",
        title: "Game Information",
        category: "information",
        audience: HelpAudience::Public,
    },
    TextFile {
        file: "wizlist",
        id: "wizlist",
        title: "Wizard List",
        category: "staff",
        audience: HelpAudience::Staff,
    },
    TextFile {
        file: "greetings",
        id: "greetings",
        title: "Login Greeting",
        category: "information",
        audience: HelpAudience::Public,
    },
    TextFile {
        file: "immlist",
        id: "immlist",
        title: "Immortal List",
        category: "staff",
        audience: HelpAudience::Staff,
    },
];

/// Read every `lib/text/` file into a help topic. A missing file is skipped
/// (the caller records the whole file as a drop if it exists but is empty).
pub fn load_text_topics(text_dir: &Path) -> Result<Vec<(String, HelpTopicDef)>> {
    let mut topics = Vec::new();
    for spec in &TEXT_FILES {
        let path = text_dir.join(spec.file);
        if !path.is_file() {
            continue;
        }
        let raw = read_text(&path)?;
        let body = raw.trim().to_owned();
        let summary = raw
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or(spec.title)
            .chars()
            .take(200)
            .collect::<String>();
        topics.push((
            spec.id.to_owned(),
            HelpTopicDef {
                title: spec.title.to_owned(),
                category: SmolStr::new(spec.category),
                summary,
                body,
                keywords: vec![SmolStr::new(spec.id)],
                see_also: Vec::new(),
                audience: spec.audience,
            },
        ));
    }
    Ok(topics)
}

/// The `CIRCLEMUD` help entry from `lib/text/help/help.hlp`, or `None`.
///
/// The CircleMUD licence requires this entry to be maintained intact and
/// displayed for `help circlemud`, so it must survive the legacy-`help.hlp`
/// drop. `help.hlp` entries are blocks of text terminated by a `#<level>` line
/// and keyed by their first line, which is a list of words; this returns the
/// block whose keywords include `circlemud`.
pub fn load_circle_help(text_dir: &Path) -> Result<Option<(String, HelpTopicDef)>> {
    let path = text_dir.join("help/help.hlp");
    if !path.is_file() {
        return Ok(None);
    }
    let raw = read_text(&path)?;
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in raw.lines() {
        let terminator = line
            .strip_prefix('#')
            .is_some_and(|level| !level.is_empty() && level.bytes().all(|b| b.is_ascii_digit()));
        if terminator {
            if !current.trim().is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        blocks.push(current);
    }
    for block in blocks {
        let Some(first) = block.lines().map(str::trim).find(|line| !line.is_empty()) else {
            continue;
        };
        let words: Vec<&str> = first.split_whitespace().collect();
        if !words
            .iter()
            .any(|word| word.eq_ignore_ascii_case("circlemud"))
        {
            continue;
        }
        return Ok(Some((
            "circlemud".to_owned(),
            HelpTopicDef {
                title: "CircleMUD Credits".to_owned(),
                category: SmolStr::new("information"),
                summary: first.chars().take(200).collect(),
                body: block.trim().to_owned(),
                keywords: words
                    .iter()
                    .map(|word| SmolStr::new(word.to_ascii_lowercase()))
                    .collect(),
                see_also: Vec::new(),
                audience: HelpAudience::Public,
            },
        )));
    }
    Ok(None)
}

/// The twelve combat strings of one `M` entry, in source order.
struct CombatVariant {
    messages: [Option<String>; 12],
}

/// Read `lib/misc/messages` into one help topic per attack type.
pub fn load_message_topics(path: &Path) -> Result<Vec<(String, HelpTopicDef)>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let raw = read_text(path)?;
    let lines: Vec<&str> = raw.lines().collect();

    let mut names: BTreeMap<i32, String> = BTreeMap::new();
    let mut variants: BTreeMap<i32, Vec<CombatVariant>> = BTreeMap::new();

    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.is_empty() || line.starts_with('*') {
            if let Some((name, type_number)) = parse_comment(line) {
                names.entry(type_number).or_insert(name);
            }
            index += 1;
            continue;
        }
        if !line.starts_with('M') {
            index += 1;
            continue;
        }
        // `M` begins an entry; the next non-blank line is the attack type.
        index += 1;
        while index < lines.len() && lines[index].trim().is_empty() {
            index += 1;
        }
        let Some(type_line) = lines.get(index) else {
            break;
        };
        let type_number = match type_line.trim().parse::<i32>() {
            Ok(number) => number,
            Err(_) => continue,
        };
        index += 1;
        let mut messages: [Option<String>; 12] = Default::default();
        for slot in &mut messages {
            *slot = message(lines.get(index).copied());
            index += 1;
        }
        variants
            .entry(type_number)
            .or_default()
            .push(CombatVariant { messages });
    }

    let mut topics = Vec::new();
    for (type_number, entries) in variants {
        let name = names
            .get(&type_number)
            .cloned()
            .unwrap_or_else(|| format!("type {type_number}"));
        let mut body = format!("Combat messages for {name} (attack type {type_number}).\n");
        for (variant, entry) in entries.iter().enumerate() {
            body.push_str(&format!("\nVariant {}:\n", variant + 1));
            for (label, message) in LABELS.iter().zip(entry.messages.iter()) {
                body.push_str(&format!(
                    "  {label}: {}\n",
                    message.as_deref().unwrap_or("(none)")
                ));
            }
        }
        topics.push((
            format!("combat-message-{type_number}"),
            HelpTopicDef {
                title: format!("Combat messages: {name}"),
                category: SmolStr::new("combat"),
                summary: format!("The {name} attack messages"),
                body,
                keywords: vec![
                    SmolStr::new("combat"),
                    SmolStr::new("messages"),
                    SmolStr::new(name.to_lowercase()),
                ],
                see_also: Vec::new(),
                audience: HelpAudience::Public,
            },
        ));
    }
    Ok(topics)
}

/// The twelve slot labels, matching `load_messages`' read order.
const LABELS: [&str; 12] = [
    "die/attacker",
    "die/victim",
    "die/room",
    "miss/attacker",
    "miss/victim",
    "miss/room",
    "hit/attacker",
    "hit/victim",
    "hit/room",
    "god/attacker",
    "god/victim",
    "god/room",
];

/// Read a text file lossily and resolve `@`-colour codes.
fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|source| ConvertError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(strip_colour_codes(&text.replace('\r', "")))
}

/// `#`-prefixed lines are Circle's NULL message.
fn message(line: Option<&str>) -> Option<String> {
    let line = line?;
    if line.starts_with('#') {
        None
    } else {
        Some(line.to_owned())
    }
}

/// A `* <name> <type>` comment names an attack type. `* !UNUSED! 202` and
/// `* burning hands 5` both parse; a comment with no trailing number is
/// ignored.
fn parse_comment(line: &str) -> Option<(String, i32)> {
    let rest = line.strip_prefix('*')?.trim();
    let (name, number) = rest.rsplit_once(char::is_whitespace)?;
    let type_number = number.trim().parse::<i32>().ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| (name.to_owned(), type_number))
}
