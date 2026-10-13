//! The tbaMUD trigger corpus (`lib/world/trg/*.trg`) as records.
//!
//! Mirrors `parse_trigger` (`dg_db_scripts.c`) and `fread_string` (`db.c`):
//!
//! - `#vnum`, then the name (`fread_string`), then `"%d %s %d"` (attach type,
//!   type letters, narg), then the argument list (`fread_string`), then the
//!   body (`fread_string`).
//! - `fread_string` reads lines until one **ends** in `~` (after trailing
//!   CR/LF are dropped), strips only that trailing `~`, and gives every
//!   earlier line a `"\r\n"`. A `~` inside a line is ordinary text. An empty
//!   result is NULL, read here as the empty string.
//! - `parse_at` then turns every `@` not followed by `@` into a tab.
//! - A `$` line ends the file.

use std::path::PathBuf;

/// How many records the corpus holds (`docs/design/triggers.md` section 1).
pub const EXPECTED_RECORDS: usize = 1461;

/// One `.trg` record.
#[derive(Debug, Clone)]
pub struct Record {
    pub vnum: i32,
    pub name: String,
    /// `0` mob, `1` object, `2` world.
    pub attach_type: i32,
    /// The type letters as written.
    pub flags: String,
    pub narg: i32,
    pub arglist: String,
    /// The body, lines joined with `\r\n`, no trailing newline.
    pub body: String,
    /// The file the record came from.
    pub file: String,
}

/// `<root>/lib/world/trg`, where root is `$TBAMUD_SOURCE` or the default.
pub fn corpus_dir() -> PathBuf {
    let root = std::env::var("TBAMUD_SOURCE")
        .unwrap_or_else(|_| "/home/qworg/Development/tbamud".to_owned());
    PathBuf::from(root).join("lib/world/trg")
}

/// Whether the corpus is on disk.
pub fn corpus_present() -> bool {
    corpus_dir().is_dir()
}

/// Print the skip message every corpus test uses when the corpus is absent.
pub fn skip_message(test: &str) {
    println!(
        "SKIP {test}: trigger corpus not found at {} (set TBAMUD_SOURCE)",
        corpus_dir().display()
    );
}

/// `fread_string` over an iterator of lines (each without its terminator).
fn fread_string<'a>(lines: &mut impl Iterator<Item = &'a str>) -> Result<String, String> {
    let mut out = String::new();
    loop {
        let line = lines
            .next()
            .ok_or_else(|| "format error: file ended inside a string".to_owned())?;
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(stripped) = line.strip_suffix('~') {
            out.push_str(stripped);
            break;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    Ok(parse_at(&out))
}

/// `parse_at`: `@` not followed by `@` becomes a tab; `@@` is left alone.
fn parse_at(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            if chars.get(i + 1) == Some(&'@') {
                out.push('@');
                out.push('@');
                i += 2;
                continue;
            }
            out.push('\t');
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }
    out
}

/// Parse one `.trg` file's text.
pub fn parse_file(file: &str, text: &str) -> Result<Vec<Record>, String> {
    let mut lines = text.split('\n');
    let mut records = Vec::new();
    while let Some(line) = lines.next() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('$') {
            break;
        }
        let Some(rest) = line.strip_prefix('#') else {
            if line.trim().is_empty() {
                continue;
            }
            return Err(format!("{file}: expected '#vnum', found {line:?}"));
        };
        let vnum: i32 = rest
            .trim()
            .parse()
            .map_err(|_| format!("{file}: bad vnum line {line:?}"))?;
        let name = fread_string(&mut lines).map_err(|e| format!("{file} #{vnum}: {e}"))?;
        let header = lines
            .next()
            .ok_or_else(|| format!("{file} #{vnum}: missing header"))?;
        let mut fields = header.split_whitespace();
        let attach_type: i32 = fields
            .next()
            .and_then(|f| f.parse().ok())
            .ok_or_else(|| format!("{file} #{vnum}: bad header {header:?}"))?;
        let flags = fields.next().unwrap_or("").to_owned();
        // `sscanf` yields narg only when all three fields converted.
        let narg: i32 = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
        let arglist = fread_string(&mut lines).map_err(|e| format!("{file} #{vnum}: {e}"))?;
        let body = fread_string(&mut lines).map_err(|e| format!("{file} #{vnum}: {e}"))?;
        records.push(Record {
            vnum,
            name,
            attach_type,
            flags,
            narg,
            arglist,
            body,
            file: file.to_owned(),
        });
    }
    Ok(records)
}

/// Read every `*.trg` file of the corpus in file-name order. `None` when the
/// corpus directory is absent.
pub fn load_corpus() -> Option<Vec<Record>> {
    let dir = corpus_dir();
    if !dir.is_dir() {
        return None;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "trg"))
        .collect();
    files.sort();
    let mut records = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bytes = std::fs::read(&path).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        match parse_file(&name, &text) {
            Ok(mut parsed) => records.append(&mut parsed),
            Err(error) => panic!("corpus parse error: {error}"),
        }
    }
    Some(records)
}

/// Find one record by vnum.
pub fn find(records: &[Record], vnum: i32) -> Option<&Record> {
    records.iter().find(|r| r.vnum == vnum)
}

/// The whole corpus, read once per test binary. `None` when it is absent.
pub fn cached() -> Option<&'static [Record]> {
    static CORPUS: std::sync::OnceLock<Option<Vec<Record>>> = std::sync::OnceLock::new();
    CORPUS.get_or_init(load_corpus).as_deref()
}

/// One real trigger by vnum, from the cached corpus.
pub fn trigger(vnum: i32) -> Option<Record> {
    cached().and_then(|records| find(records, vnum).cloned())
}

/// Fetch a real trigger or make the calling test print the skip message and
/// pass.
#[macro_export]
macro_rules! real {
    ($vnum:expr) => {
        match $crate::common::corpus::trigger($vnum) {
            Some(record) => record,
            None => {
                $crate::common::corpus::skip_message(&format!("trigger {}", $vnum));
                return;
            }
        }
    };
}
