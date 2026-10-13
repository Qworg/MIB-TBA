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
    /// The body: every line but the last is followed by `\r\n` (`fread_string`),
    /// so a body whose `~` stands alone ends in `\r\n`.
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

/// `FREAD_CHUNK` (`db.h`): `fgets` in `fread_string` reads at most 511 bytes.
const FREAD_CHUNK: usize = 512;
/// `READ_SIZE` (`utils.h`): the line buffer `discrete_load` hands `get_line`.
const READ_SIZE: usize = 256;

/// A file read the way the C does: `fgets` chunks, `get_line`, `fread_string`.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    /// `fgets(buf, size, fl)`: up to `size - 1` bytes, stopping after a newline.
    /// `None` at the end of the file.
    fn fgets(&mut self, size: usize) -> Option<&[u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let rest = &self.data[self.pos..];
        let limit = size.saturating_sub(1).min(rest.len());
        let take = rest[..limit]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(limit, |at| at + 1);
        let chunk = &rest[..take];
        self.pos += take;
        Some(chunk)
    }

    /// `get_line` (`utils.c`): skip lines that start with `*`, `\n` or `\r`,
    /// drop the tail of a line longer than the buffer, strip the line end.
    fn get_line(&mut self, bufsize: usize) -> Option<String> {
        loop {
            let chunk = self.fgets(bufsize)?.to_vec();
            if chunk.last().is_some_and(|b| *b != b'\n') {
                // the tail of an over-long line is discarded
                while self.pos < self.data.len() {
                    let byte = self.data[self.pos];
                    self.pos += 1;
                    if byte == b'\n' {
                        break;
                    }
                }
            }
            if matches!(chunk.first(), Some(b'*' | b'\n' | b'\r')) {
                continue;
            }
            let mut end = chunk.len();
            while end > 0 && matches!(chunk[end - 1], b'\n' | b'\r') {
                end -= 1;
            }
            return Some(String::from_utf8_lossy(&chunk[..end]).into_owned());
        }
    }

    /// `fread_string` (`db.c`): 511-byte chunks; a chunk whose last byte
    /// (after trailing CR/LF are skipped) is `~` ends the string and loses
    /// that `~`; any other chunk gets `"\r\n"` after it. Then `parse_at`.
    fn fread_string(&mut self) -> Result<String, String> {
        let mut out: Vec<u8> = Vec::new();
        loop {
            let chunk = self
                .fgets(FREAD_CHUNK)
                .ok_or_else(|| "format error: file ended inside a string".to_owned())?;
            let mut end = chunk.len();
            // `for (point--; (*point=='\r' || *point=='\n') && point > tmp; point--)`
            while end > 1 && matches!(chunk[end - 1], b'\r' | b'\n') {
                end -= 1;
            }
            if chunk[end - 1] == b'~' {
                out.extend_from_slice(&chunk[..end - 1]);
                break;
            }
            if matches!(chunk[end - 1], b'\n' | b'\r') {
                out.extend_from_slice(&chunk[..end - 1]);
            } else {
                out.extend_from_slice(&chunk[..end]);
            }
            out.extend_from_slice(b"\r\n");
        }
        Ok(parse_at(&String::from_utf8_lossy(&out)))
    }
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

/// One `%d` of `sscanf`: skip white space, an optional sign, the digits.
/// Returns the value (clamped to `i32`) and the rest of the text.
fn scan_int(text: &str) -> Option<(i32, &str)> {
    let text = text.trim_start();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let length = digits.bytes().take_while(u8::is_ascii_digit).count();
    if length == 0 {
        return None;
    }
    let magnitude: i64 = digits[..length].parse().unwrap_or(i64::MAX);
    let value = if negative { -magnitude } else { magnitude };
    let value = i32::try_from(value).unwrap_or(if negative { i32::MIN } else { i32::MAX });
    Some((value, &digits[length..]))
}

/// Parse one `.trg` file's bytes the way `discrete_load` (`db.c`) and
/// `parse_trigger` (`dg_db_scripts.c`) do.
pub fn parse_file(file: &str, text: &str) -> Result<Vec<Record>, String> {
    let mut reader = Reader {
        data: text.as_bytes(),
        pos: 0,
    };
    let mut records = Vec::new();
    loop {
        let Some(line) = reader.get_line(READ_SIZE) else {
            return Err(format!("{file}: ended without a '$' line"));
        };
        if line.starts_with('$') {
            break;
        }
        let Some(rest) = line.strip_prefix('#') else {
            return Err(format!("{file}: expected '#vnum', found {line:?}"));
        };
        let (vnum, _) = scan_int(rest).ok_or_else(|| format!("{file}: bad vnum line {line:?}"))?;
        if vnum >= 99999 {
            break;
        }
        let name = reader
            .fread_string()
            .map_err(|e| format!("{file} #{vnum}: {e}"))?;
        let header = reader
            .get_line(READ_SIZE)
            .ok_or_else(|| format!("{file} #{vnum}: missing header"))?;
        // sscanf(line, "%d %s %d", &attach_type, flags, t)
        let (attach_type, after) =
            scan_int(&header).ok_or_else(|| format!("{file} #{vnum}: bad header {header:?}"))?;
        let mut tokens = after.split_whitespace();
        let flags = tokens.next().unwrap_or("").to_owned();
        // narg is set only when the third conversion succeeded too
        let narg = tokens
            .next()
            .and_then(|token| scan_int(token))
            .map_or(0, |(n, _)| n);
        let arglist = reader
            .fread_string()
            .map_err(|e| format!("{file} #{vnum}: {e}"))?;
        let body = reader
            .fread_string()
            .map_err(|e| format!("{file} #{vnum}: {e}"))?;
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

/// The files `lib/world/trg/index` lists (up to its `$` line), in order; the
/// directory's `*.trg` files when there is no index.
fn corpus_files(dir: &std::path::Path) -> Option<Vec<PathBuf>> {
    if let Ok(index) = std::fs::read_to_string(dir.join("index")) {
        return Some(
            index
                .lines()
                .map(str::trim)
                .take_while(|line| !line.starts_with('$'))
                .filter(|line| !line.is_empty())
                .map(|line| dir.join(line))
                .collect(),
        );
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "trg"))
        .collect();
    files.sort();
    Some(files)
}

/// Read every file of the corpus index in order. `Err` says why the corpus
/// cannot be read: the directory is absent, a file its index lists is missing
/// or unreadable, or a file does not parse.
pub fn read_corpus() -> Result<Vec<Record>, String> {
    read_corpus_from(&corpus_dir())
}

/// [`read_corpus`] for the `trg` directory `dir`.
pub fn read_corpus_from(dir: &std::path::Path) -> Result<Vec<Record>, String> {
    if !dir.is_dir() {
        return Err(format!(
            "trigger corpus not found at {} (set TBAMUD_SOURCE)",
            dir.display()
        ));
    }
    let files = corpus_files(dir).ok_or_else(|| format!("{}: cannot list", dir.display()))?;
    let mut records = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        records.append(&mut parse_file(&name, &text)?);
    }
    Ok(records)
}

/// The corpus for the whole-corpus tests: `None` when the directory is absent
/// (they print the skip message), a panic when it is present but unreadable.
pub fn load_corpus() -> Option<Vec<Record>> {
    if !corpus_present() {
        return None;
    }
    match read_corpus() {
        Ok(records) => Some(records),
        Err(error) => panic!("corpus unreadable: {error}"),
    }
}

/// Find one record by vnum.
pub fn find(records: &[Record], vnum: i32) -> Option<&Record> {
    records.iter().find(|r| r.vnum == vnum)
}

/// One real trigger by vnum, read from the corpus on disk. `Err` holds the
/// reason a scenario cannot run: no corpus, an unreadable corpus, or a corpus
/// without that vnum.
pub fn trigger(vnum: i32) -> Result<Record, String> {
    let records = read_corpus()?;
    find(&records, vnum)
        .cloned()
        .ok_or_else(|| format!("trigger {vnum} is not in the corpus"))
}

/// Fetch a real trigger or make the calling test print the skip message with
/// the reason and pass.
#[macro_export]
macro_rules! real {
    ($vnum:expr) => {
        match $crate::common::corpus::trigger($vnum) {
            Ok(record) => record,
            Err(reason) => {
                println!("SKIP trigger {}: {reason}", $vnum);
                return;
            }
        }
    };
}
