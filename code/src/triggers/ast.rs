//! The DG script parser (issue #15, Phase 1).
//!
//! Parses the bounded subset `docs/design/triggers.md` §6.5 names into an AST
//! the Phase-2 evaluator walks. The parser is **lenient by design**: a line it
//! cannot represent becomes a structured [`Node::Unsupported`] marker carrying
//! the construct kind, so coverage is explicit rather than a silent no-op. A
//! *structural* error (an `if` with no `end`, a stray `else`) is reported on
//! [`ParsedProgram::error`], not panicked on.
//!
//! Supported text: `say`, `emote`, `echo`/`%echo%`, `%echoaround%`, `%send%`,
//! `%zoneecho%`. Supported control: `if`/`elseif`/`else`/`end`,
//! `switch`/`case`/`default`/`break`/`done`, `while`/`done`, `return`, `nop`,
//! `halt`. Phase 3 adds state (`set`/`eval`/`unset`/`remote`/`global`/`local`/
//! `rdelete`) and the `%load%`/`%purge%` effects into real [`Node`]s. User
//! arrays (`set txt[1] …`) remain named drops, matching the design. Everything
//! else is a named unsupported drop.

use std::collections::{BTreeMap, BTreeSet};

/// Maximum nodes a single record may execute before the cap trips. A looping
/// script is stopped and reported rather than allowed to wedge a tick.
pub const STEP_CAP: usize = 10_000;

/// A comparison operator in a condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl CmpOp {
    /// Parse the DG spelling. `/=` is intentionally absent: it is a
    /// string-substring operator Phase 2 does not model, so a script using it
    /// is a named drop rather than a wrong comparison.
    fn parse(token: &str) -> Option<Self> {
        match token {
            "==" => Some(CmpOp::Eq),
            "!=" => Some(CmpOp::Ne),
            "<" => Some(CmpOp::Lt),
            ">" => Some(CmpOp::Gt),
            "<=" => Some(CmpOp::Le),
            ">=" => Some(CmpOp::Ge),
            _ => None,
        }
    }

    /// Stable report spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Gt => ">",
            CmpOp::Le => "<=",
            CmpOp::Ge => ">=",
        }
    }
}

/// The kind argument of a `%load%` directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadKind {
    /// `%load% obj <vnum> [victim] [wear]`.
    Obj,
    /// `%load% mob <vnum>`.
    Mob,
}

impl LoadKind {
    /// Stable report spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            LoadKind::Obj => "obj",
            LoadKind::Mob => "mob",
        }
    }
}

/// A resolved `wait` duration (issue #15, Phase 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerWait {
    /// `wait N sec` / `wait N s`: N seconds of engine time.
    Seconds(u32),
    /// `wait N tick` / `wait N t`, or a bare `wait N`: N fixed engine ticks.
    Ticks(u32),
    /// `wait until HH:MM`: resume when the in-world clock next reaches the
    /// target wall time.
    Until { hour: u32, minute: u32 },
}

impl TriggerWait {
    /// Stable report spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            TriggerWait::Seconds(_) => "wait.sec",
            TriggerWait::Ticks(_) => "wait.tick",
            TriggerWait::Until { .. } => "wait.until",
        }
    }
}

/// The operation argument of a `%door%` directive (issue #15, Phase 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoorOp {
    /// `%door% <room> <dir> open`.
    Open,
    /// `%door% <room> <dir> close` (the `closed` spelling is accepted too).
    Close,
    /// `%door% <room> <dir> lock` (the `locked` spelling is accepted too).
    Lock,
    /// `%door% <room> <dir> unlock` (the `unlocked` spelling is accepted too).
    Unlock,
    /// `%door% <room> <dir> flags <letters>`: DG's `asciiflag_conv` set.
    Flags(String),
    /// `%door% <room> <dir> purge`: remove the exit. Named-dropped: MIB's
    /// authored exit set is content-owned.
    Purge,
    /// A recognised field Phase 4 does not apply (`description`, `key`,
    /// `name`, `room`).
    Deferred(String),
}

/// A boolean condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
    /// A single value: truthy when non-empty and not `0`.
    Truthy(String),
    /// `left <op> right`, compared numerically when both parse, else as
    /// case-insensitive strings.
    Cmp(String, CmpOp, String),
    /// Logical negation.
    Not(Box<Cond>),
    /// `&&`.
    And(Box<Cond>, Box<Cond>),
    /// `||`.
    Or(Box<Cond>, Box<Cond>),
    /// A condition the parser could not represent; evaluates false and is a
    /// named drop.
    Unsupported(String),
}

/// One script statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// `say <text>` — the owner speaks to the room.
    Say(String),
    /// `emote <text>` — the owner emotes to the room.
    Emote(String),
    /// `echo <text>` / `%echo% <text>` — the room.
    Echo(String),
    /// `%send% <target> <text>` — one recipient.
    Send { target: String, text: String },
    /// `%echoaround% <target> <text>` — the room except the target.
    EchoAround { target: String, text: String },
    /// `%zoneecho% <room> <text>` — parsed but not delivered (Phase 4 owns
    /// zones); a named drop at evaluation.
    ZoneEcho { zone: String, text: String },
    /// `if`/`elseif`/`else`/`end`.
    If {
        arms: Vec<(Cond, Vec<Node>)>,
        otherwise: Vec<Node>,
    },
    /// `switch`/`case`/`default`/`done`.
    Switch {
        subject: String,
        cases: Vec<(Vec<String>, Vec<Node>)>,
        default: Vec<Node>,
    },
    /// `while <cond> … done`.
    While { cond: Cond, body: Vec<Node> },
    /// `return [value]` — stops the record.
    Return(Option<String>),
    /// `halt` — stops the record (and any enclosing chain).
    Halt,
    /// `nop <command>` — parsed; the wrapped command is deferred.
    Nop(String),
    /// `break` — leaves the innermost `switch`/`while`.
    Break,
    /// `set <name> <value>` — assign the trigger-local variable `name`.
    Set { name: String, value: String },
    /// `eval <name> <expr>` — evaluate integer arithmetic and assign it.
    Eval { name: String, expr: String },
    /// `unset <name>` — remove the trigger-local variable.
    Unset { name: String },
    /// `remote <name> <target>` — persist the local `name` on the target.
    Remote { name: String, target: String },
    /// `global <name> [value]` — persist `name` in the global scope.
    Global { name: String, value: String },
    /// `local <name> [value]` — assign the trigger-local variable.
    Local { name: String, value: String },
    /// `rdelete <name> <target>` — delete the target's persisted `name`.
    RDelete { name: String, target: String },
    /// `%load% obj|mob <vnum> [victim] [wear]` — spawn from the world.
    Load {
        kind: LoadKind,
        vnum: String,
        target: Option<String>,
        wear: Option<String>,
    },
    /// `%purge% [target]` — despawn the target (default: the owner).
    Purge { target: Option<String> },
    /// `wait <n> [sec|s|tick]` / `wait until <hh:mm>` — suspend the record's
    /// program counter and resume after the delay (Phase 4).
    Wait(TriggerWait),
    /// `%teleport% <target> <room vnum>` — move the target to a room.
    Teleport { target: String, room: String },
    /// `%door% <room vnum> <direction> <op> [value]` — mutate a runtime door.
    Door {
        room: String,
        direction: String,
        op: DoorOp,
    },
    /// `%force% <target> <command>` — make the target execute a command.
    Force { target: String, command: String },
    /// `%at% <room vnum> <command>` — execute a command in another room.
    At { room: String, command: String },
    /// `dg_cast '<spell>' <target>` — cast a catalog spell (Phase 5).
    DgCast { spell: String, target: String },
    /// `%damage% <target> <amount>` — deal direct damage (Phase 5).
    Damage { target: String, amount: String },
    /// `mkill <target>` — kill an NPC (Phase 5).
    Mkill { target: String },
    /// `detach <vnum> <target>` — detach a trigger for this session (Phase 5).
    Detach { vnum: String, target: String },
    /// A construct parsed but deferred (an indexed user array; Phase 4+).
    Deferred { kind: String, line: String },
    /// A line whose construct Phase 3 does not support.
    Unsupported { kind: String, line: String },
}

impl Node {
    /// The coverage key for this node.
    pub fn construct(&self) -> String {
        match self {
            Node::Say(_) => "say".to_owned(),
            Node::Emote(_) => "emote".to_owned(),
            Node::Echo(_) => "echo".to_owned(),
            Node::Send { .. } => "send".to_owned(),
            Node::EchoAround { .. } => "echoaround".to_owned(),
            Node::ZoneEcho { .. } => "zoneecho".to_owned(),
            Node::If { .. } => "if".to_owned(),
            Node::Switch { .. } => "switch".to_owned(),
            Node::While { .. } => "while".to_owned(),
            Node::Return(_) => "return".to_owned(),
            Node::Halt => "halt".to_owned(),
            Node::Nop(_) => "nop".to_owned(),
            Node::Break => "break".to_owned(),
            Node::Set { .. } => "set".to_owned(),
            Node::Eval { .. } => "eval".to_owned(),
            Node::Unset { .. } => "unset".to_owned(),
            Node::Remote { .. } => "remote".to_owned(),
            Node::Global { .. } => "global".to_owned(),
            Node::Local { .. } => "local".to_owned(),
            Node::RDelete { .. } => "rdelete".to_owned(),
            Node::Load { kind, .. } => format!("load.{}", kind.as_str()),
            Node::Purge { .. } => "purge".to_owned(),
            Node::Wait(wait) => wait.as_str().to_owned(),
            Node::Teleport { .. } => "teleport".to_owned(),
            Node::Door { .. } => "door".to_owned(),
            Node::Force { .. } => "force".to_owned(),
            Node::At { .. } => "at".to_owned(),
            Node::DgCast { .. } => "dg_cast".to_owned(),
            Node::Damage { .. } => "damage".to_owned(),
            Node::Mkill { .. } => "mkill".to_owned(),
            Node::Detach { .. } => "detach".to_owned(),
            Node::Deferred { kind, .. } => kind.clone(),
            Node::Unsupported { kind, .. } => format!("unsupported:{kind}"),
        }
    }
}

/// The parse result for one record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedProgram {
    /// The top-level statements.
    pub nodes: Vec<Node>,
    /// Per-construct occurrence counts (`say`, `if`, `unsupported:dg_cast`…).
    pub constructs: BTreeMap<String, usize>,
    /// Unsupported construct kind -> count.
    pub unsupported: BTreeMap<String, usize>,
    /// A structural error (`if` without `end`, stray `end`) when present. A
    /// record with an error is a named drop and is not executed.
    pub error: Option<String>,
    /// Every variable name the record assigns (lowercased, array suffix
    /// stripped). The coverage scan treats these as known, so a bare `%name%`
    /// reference is not miscounted as an unknown token.
    pub vars: BTreeSet<String>,
}

impl ParsedProgram {
    /// Whether the record parsed without a structural error.
    pub fn is_valid(&self) -> bool {
        self.error.is_none()
    }

    /// Concatenate every string a **supported** node references (text and
    /// condition operands). The coverage report scans this for unknown
    /// substitution tokens, so a deferred directive (`%load%`) is not
    /// mis-counted as an unknown token.
    pub fn referenced_text(&self) -> String {
        let mut out = String::new();
        collect_references(&self.nodes, &mut out);
        out
    }
}

/// Parse one DG script body.
pub fn parse_script(source: &str) -> ParsedProgram {
    let lines: Vec<&str> = source.lines().collect();
    let mut parser = Parser {
        lines,
        pos: 0,
        error: None,
        constructs: BTreeMap::new(),
        unsupported: BTreeMap::new(),
    };
    let nodes = parser.parse_block(&[]);
    if parser.error.is_none() && parser.pos < parser.lines.len() {
        let stray = parser.lines[parser.pos].trim();
        parser.error = Some(format!("unexpected trailing `{stray}`"));
    }
    let vars = collect_assigned_vars(&nodes);
    ParsedProgram {
        nodes,
        constructs: parser.constructs,
        unsupported: parser.unsupported,
        error: parser.error,
        vars,
    }
}

struct Parser<'a> {
    lines: Vec<&'a str>,
    pos: usize,
    error: Option<String>,
    constructs: BTreeMap<String, usize>,
    unsupported: BTreeMap<String, usize>,
}

impl<'a> Parser<'a> {
    fn count(&mut self, key: &str) {
        *self.constructs.entry(key.to_owned()).or_default() += 1;
    }

    fn count_unsupported(&mut self, kind: &str) {
        *self
            .constructs
            .entry(format!("unsupported:{kind}"))
            .or_default() += 1;
        *self.unsupported.entry(kind.to_owned()).or_default() += 1;
    }

    /// Advance past blank lines and `*` comments.
    fn skip_blank(&mut self) {
        while self.pos < self.lines.len() {
            let trimmed = self.lines[self.pos].trim();
            if trimmed.is_empty() || trimmed.starts_with('*') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// The first word (lowercased) of the line at `pos`, if any.
    fn peek_keyword(&self) -> Option<String> {
        self.lines
            .get(self.pos)
            .map(|line| line.trim())
            .and_then(|line| line.split_whitespace().next())
            .map(|word| word.to_ascii_lowercase())
    }

    /// Parse statements until one of `stop` keywords (which is left
    /// unconsumed), the end of input, or a structural error.
    fn parse_block(&mut self, stop: &[&str]) -> Vec<Node> {
        let mut nodes = Vec::new();
        loop {
            self.skip_blank();
            if self.pos >= self.lines.len() {
                break;
            }
            let trimmed = self.lines[self.pos].trim();
            let keyword = trimmed
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if stop.contains(&keyword.as_str()) {
                break;
            }
            match keyword.as_str() {
                "if" => nodes.push(self.parse_if(trimmed)),
                "switch" => nodes.push(self.parse_switch(trimmed)),
                "while" => nodes.push(self.parse_while(trimmed)),
                "else" | "elseif" | "end" | "done" | "case" | "default" => {
                    // A stop keyword that was not in `stop` is a structural
                    // error; do not advance, let the caller report it.
                    if self.error.is_none() {
                        self.error = Some(format!("unexpected `{keyword}`"));
                    }
                    break;
                }
                _ => {
                    nodes.push(self.parse_statement(trimmed));
                    self.pos += 1;
                }
            }
        }
        nodes
    }

    fn parse_if(&mut self, line: &str) -> Node {
        self.pos += 1;
        self.count("if");
        let cond = parse_condition(&rest_after(line, "if"));
        let mut arms = vec![(cond, self.parse_block(&["elseif", "else", "end"]))];
        let mut otherwise = Vec::new();

        loop {
            self.skip_blank();
            match self.peek_keyword().as_deref() {
                Some("elseif") => {
                    let trimmed = self.lines[self.pos].trim().to_owned();
                    self.pos += 1;
                    self.count("elseif");
                    let cond = parse_condition(&rest_after(&trimmed, "elseif"));
                    let body = self.parse_block(&["elseif", "else", "end"]);
                    arms.push((cond, body));
                }
                Some("else") => {
                    self.pos += 1;
                    self.count("else");
                    otherwise = self.parse_block(&["end"]);
                    // The `end` must follow immediately (after blanks).
                    self.skip_blank();
                    if self.peek_keyword().as_deref() == Some("end") {
                        self.pos += 1;
                    } else if self.error.is_none() {
                        self.error = Some("`if` without `end`".to_owned());
                    }
                    break;
                }
                Some("end") => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    if self.error.is_none() {
                        self.error = Some("`if` without `end`".to_owned());
                    }
                    break;
                }
            }
        }
        Node::If { arms, otherwise }
    }

    fn parse_switch(&mut self, line: &str) -> Node {
        self.pos += 1;
        self.count("switch");
        let subject = rest_after(line, "switch");
        let mut cases: Vec<(Vec<String>, Vec<Node>)> = Vec::new();
        let mut default = Vec::new();
        loop {
            self.skip_blank();
            match self.peek_keyword().as_deref() {
                Some("case") => {
                    let trimmed = self.lines[self.pos].trim().to_owned();
                    self.pos += 1;
                    self.count("case");
                    let values: Vec<String> = rest_after(&trimmed, "case")
                        .split_whitespace()
                        .map(unquote)
                        .collect();
                    let body = self.parse_block(&["case", "default", "done", "break"]);
                    if self.peek_keyword().as_deref() == Some("break") {
                        self.pos += 1;
                        self.count("break");
                    }
                    cases.push((values, body));
                }
                Some("default") => {
                    self.pos += 1;
                    self.count("default");
                    default = self.parse_block(&["done", "break"]);
                    if self.peek_keyword().as_deref() == Some("break") {
                        self.pos += 1;
                        self.count("break");
                    }
                }
                Some("done") => {
                    self.pos += 1;
                    self.count("done");
                    break;
                }
                _ => {
                    if self.error.is_none() {
                        self.error = Some("`switch` without `done`".to_owned());
                    }
                    break;
                }
            }
        }
        Node::Switch {
            subject,
            cases,
            default,
        }
    }

    fn parse_while(&mut self, line: &str) -> Node {
        self.pos += 1;
        self.count("while");
        let cond = parse_condition(&rest_after(line, "while"));
        let body = self.parse_block(&["done"]);
        self.skip_blank();
        if self.peek_keyword().as_deref() == Some("done") {
            self.pos += 1;
            self.count("done");
        } else if self.error.is_none() {
            self.error = Some("`while` without `done`".to_owned());
        }
        Node::While { cond, body }
    }

    fn parse_statement(&mut self, line: &str) -> Node {
        let keyword = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let rest = rest_after(line, line.split_whitespace().next().unwrap_or(""));
        match keyword.as_str() {
            "say" => {
                self.count("say");
                Node::Say(rest)
            }
            "emote" => {
                self.count("emote");
                Node::Emote(rest)
            }
            "echo" => {
                self.count("echo");
                Node::Echo(rest)
            }
            "return" => {
                self.count("return");
                if rest.trim().is_empty() {
                    Node::Return(None)
                } else {
                    Node::Return(Some(rest))
                }
            }
            "halt" => {
                self.count("halt");
                Node::Halt
            }
            "nop" => {
                self.count("nop");
                Node::Nop(rest)
            }
            "break" => {
                self.count("break");
                Node::Break
            }
            "set" => {
                let (name, value) = split_target(&rest);
                if is_array_name(&name) {
                    self.count_unsupported("set_array");
                    Node::Unsupported {
                        kind: "set_array".to_owned(),
                        line: line.to_owned(),
                    }
                } else {
                    self.count("set");
                    Node::Set { name, value }
                }
            }
            "eval" => {
                let (name, expr) = split_target(&rest);
                if is_array_name(&name) {
                    self.count_unsupported("eval_array");
                    Node::Unsupported {
                        kind: "eval_array".to_owned(),
                        line: line.to_owned(),
                    }
                } else {
                    self.count("eval");
                    Node::Eval { name, expr }
                }
            }
            "unset" => {
                self.count("unset");
                Node::Unset {
                    name: rest.trim().to_owned(),
                }
            }
            "remote" => {
                let (name, target) = split_target(&rest);
                self.count("remote");
                Node::Remote { name, target }
            }
            "global" => {
                let (name, value) = split_target(&rest);
                self.count("global");
                Node::Global { name, value }
            }
            "local" => {
                let (name, value) = split_target(&rest);
                self.count("local");
                Node::Local { name, value }
            }
            "rdelete" => {
                let (name, target) = split_target(&rest);
                self.count("rdelete");
                Node::RDelete { name, target }
            }
            "%echo%" => {
                self.count("echo");
                Node::Echo(rest)
            }
            "%echoaround%" => {
                self.count("echoaround");
                let (target, text) = split_target(&rest);
                Node::EchoAround { target, text }
            }
            "%send%" => {
                self.count("send");
                let (target, text) = split_target(&rest);
                Node::Send { target, text }
            }
            "%zoneecho%" => {
                self.count("zoneecho");
                let (zone, text) = split_target(&rest);
                Node::ZoneEcho { zone, text }
            }
            "%load%" => {
                let node = parse_load(&rest);
                match &node {
                    Node::Load { kind, .. } => {
                        self.count(&format!("load.{}", kind.as_str()));
                    }
                    _ => {
                        self.count_unsupported("load");
                    }
                }
                node
            }
            "%purge%" => {
                self.count("purge");
                let target = rest.trim();
                Node::Purge {
                    target: (!target.is_empty()).then(|| target.to_owned()),
                }
            }
            "wait" => {
                let node = parse_wait(&rest);
                if let Node::Wait(wait) = &node {
                    self.count(wait.as_str());
                } else {
                    self.count_unsupported("wait");
                }
                node
            }
            "%teleport%" | "%mteleport%" | "%oteleport%" | "%wteleport%" => {
                self.count("teleport");
                let (target, room) = split_target(&rest);
                Node::Teleport { target, room }
            }
            "%door%" | "%mdoor%" | "%odoor%" | "%wdoor%" => {
                let node = parse_door(&rest);
                match &node {
                    Node::Door {
                        op: DoorOp::Purge, ..
                    } => self.count_unsupported("door_purge"),
                    Node::Door {
                        op: DoorOp::Deferred(field),
                        ..
                    } => self.count_unsupported(&format!("door_{field}")),
                    Node::Door { .. } => self.count("door"),
                    _ => self.count_unsupported("door"),
                }
                node
            }
            "%force%" | "%mforce%" | "%oforce%" | "%wforce%" => {
                self.count("force");
                let (target, command) = split_target(&rest);
                Node::Force { target, command }
            }
            "%at%" | "%mat%" | "%oat%" | "%wat%" => {
                let (room, command) = split_target(&rest);
                // `%at% <room> %load% ...` is executed by Phase 4 (the spawn
                // honours the room); every other nested command is a named
                // drop, so the committed coverage does not overstate it.
                if command
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("%load%")
                {
                    self.count("at.load");
                } else {
                    self.count_unsupported("at_command");
                }
                Node::At { room, command }
            }
            "%echoround%" => {
                self.count("echoaround");
                let (target, text) = split_target(&rest);
                Node::EchoAround { target, text }
            }
            "dg_cast" | "%dg_cast%" | "%mcast%" | "%ocast%" | "%wcast%" => {
                let node = parse_dg_cast(&rest);
                self.count("dg_cast");
                node
            }
            "%damage%" | "%mdamage%" | "%odamage%" | "%wdamage%" => {
                self.count("damage");
                let (target, amount) = split_target(&rest);
                Node::Damage { target, amount }
            }
            "mkill" => {
                self.count("mkill");
                Node::Mkill {
                    target: rest.trim().to_owned(),
                }
            }
            "detach" => {
                self.count("detach");
                let (vnum, target) = split_target(&rest);
                Node::Detach { vnum, target }
            }
            _ => {
                let kind = directive_kind(&keyword);
                self.count_unsupported(&kind);
                Node::Unsupported {
                    kind,
                    line: line.to_owned(),
                }
            }
        }
    }
}

/// The unsupported construct kind for a line: a `%directive%` name, or the
/// leading command word.
fn directive_kind(first_word: &str) -> String {
    if first_word.starts_with('%') && first_word.len() > 2 {
        first_word.trim_matches('%').to_ascii_lowercase()
    } else {
        first_word.to_ascii_lowercase()
    }
}

/// Whether a variable name uses the user-array form (`txt[1]`, `phrase(2)`).
/// User arrays are deferred past Phase 3, so the parser records them by name.
fn is_array_name(name: &str) -> bool {
    name.contains('[') || name.contains('(')
}

/// Parse a `dg_cast` body after the keyword.
///
/// `dg_cast '<spell>' <%target%>` and `dg_cast <spell> <%target%>` both parse;
/// a bare `dg_cast '<spell>'` targets the owner. The spell phrase is kept
/// verbatim (the effect layer resolves it against `tba.spells`), so an
/// unknown spell is a named drop rather than a silent no-op.
fn parse_dg_cast(rest: &str) -> Node {
    let rest = rest.trim();
    let (spell, target) =
        if let Some(after) = rest.strip_prefix('\'').or_else(|| rest.strip_prefix('"')) {
            let quote = rest.chars().next().unwrap_or('\'');
            match after.split_once(quote) {
                Some((spell, target)) => (spell.trim().to_owned(), target.trim()),
                None => (rest.trim_matches(quote).trim().to_owned(), ""),
            }
        } else {
            split_word(rest)
        };
    Node::DgCast {
        spell,
        target: target.trim().to_owned(),
    }
}

/// Split the first whitespace-delimited word from the rest.
fn split_word(line: &str) -> (String, &str) {
    let trimmed = line.trim();
    match trimmed.find(char::is_whitespace) {
        Some(index) => (trimmed[..index].to_owned(), trimmed[index + 1..].trim()),
        None => (trimmed.to_owned(), ""),
    }
}

/// Parse a `%load%` body after the directive word.
fn parse_load(rest: &str) -> Node {
    let mut parts = rest.split_whitespace();
    let kind_word = parts.next().unwrap_or("");
    let vnum = parts.next().unwrap_or("").to_owned();
    let target = parts.next().map(str::to_owned);
    let wear = parts.next().map(str::to_owned);
    let kind = match kind_word.to_ascii_lowercase().as_str() {
        "obj" | "o" | "object" => Some(LoadKind::Obj),
        "mob" | "m" | "mobile" => Some(LoadKind::Mob),
        _ => None,
    };
    match kind {
        Some(kind) if !vnum.is_empty() => Node::Load {
            kind,
            vnum,
            target,
            wear,
        },
        _ => Node::Unsupported {
            kind: "load".to_owned(),
            line: format!("%load% {rest}"),
        },
    }
}

/// Parse a `wait` body after the keyword.
///
/// Accepted: `wait N`, `wait N sec`/`wait N s`, `wait N tick`/`wait N t`, and
/// `wait until HH:MM` (or `HHMM`). A body the parser cannot read is an
/// [`Node::Unsupported`], never a silent no-op.
fn parse_wait(rest: &str) -> Node {
    let rest = rest.trim();
    let lower = rest.to_ascii_lowercase();
    if let Some(time) = lower.strip_prefix("until") {
        let time = time.trim();
        return match parse_clock_time(time) {
            Some((hour, minute)) => Node::Wait(TriggerWait::Until { hour, minute }),
            None => Node::Unsupported {
                kind: "wait".to_owned(),
                line: format!("wait {rest}"),
            },
        };
    }
    let mut parts = rest.split_whitespace();
    let Some(count) = parts.next().and_then(|n| n.parse::<u32>().ok()) else {
        return Node::Unsupported {
            kind: "wait".to_owned(),
            line: format!("wait {rest}"),
        };
    };
    match parts.next().map(str::to_ascii_lowercase).as_deref() {
        None => Node::Wait(TriggerWait::Ticks(count)),
        Some("tick") | Some("t") => Node::Wait(TriggerWait::Ticks(count)),
        Some("sec") | Some("s") | Some("secs") | Some("second") | Some("seconds") => {
            Node::Wait(TriggerWait::Seconds(count))
        }
        Some(_) => Node::Unsupported {
            kind: "wait".to_owned(),
            line: format!("wait {rest}"),
        },
    }
}

/// Parse `HH:MM` or `HHMM` into `(hour, minute)`.
fn parse_clock_time(text: &str) -> Option<(u32, u32)> {
    let (hour, minute): (String, String) = match text.split_once(':') {
        Some((hour, minute)) => (hour.trim().to_owned(), minute.trim().to_owned()),
        None => {
            let digits: String = text.chars().filter(char::is_ascii_digit).collect();
            if digits.len() != text.len() || digits.is_empty() {
                return None;
            }
            if digits.len() <= 2 {
                (digits, "0".to_owned())
            } else {
                let split = digits.len() - 2;
                (digits[..split].to_owned(), digits[split..].to_owned())
            }
        }
    };
    let hour = hour.parse::<u32>().ok()?;
    let minute = minute.parse::<u32>().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// Parse a `%door%` body after the directive word.
fn parse_door(rest: &str) -> Node {
    let mut parts = rest.splitn(4, char::is_whitespace);
    let room = parts.next().unwrap_or("").to_owned();
    let direction = parts.next().unwrap_or("").to_owned();
    let op_word = parts.next().unwrap_or("").to_ascii_lowercase();
    let value = parts.next().unwrap_or("").trim().to_owned();
    if room.is_empty() || direction.is_empty() || op_word.is_empty() {
        return Node::Unsupported {
            kind: "door".to_owned(),
            line: format!("%door% {rest}"),
        };
    }
    let op = match op_word.as_str() {
        "open" => DoorOp::Open,
        "close" | "closed" => DoorOp::Close,
        "lock" | "locked" => DoorOp::Lock,
        "unlock" | "unlocked" => DoorOp::Unlock,
        "flags" => DoorOp::Flags(value),
        "purge" => DoorOp::Purge,
        "description" | "key" | "name" | "room" => DoorOp::Deferred(op_word),
        _ => DoorOp::Deferred(op_word),
    };
    Node::Door {
        room,
        direction,
        op,
    }
}

/// Every variable name a record assigns, lowercased.
fn collect_assigned_vars(nodes: &[Node]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_assigned(nodes, &mut out);
    out
}

fn collect_assigned(nodes: &[Node], out: &mut BTreeSet<String>) {
    for node in nodes {
        match node {
            Node::Set { name, .. }
            | Node::Eval { name, .. }
            | Node::Unset { name }
            | Node::Remote { name, .. }
            | Node::Global { name, .. }
            | Node::Local { name, .. }
            | Node::RDelete { name, .. } => {
                out.insert(name.trim().to_ascii_lowercase());
            }
            Node::If { arms, otherwise } => {
                for (_, body) in arms {
                    collect_assigned(body, out);
                }
                collect_assigned(otherwise, out);
            }
            Node::Switch { cases, default, .. } => {
                for (_, body) in cases {
                    collect_assigned(body, out);
                }
                collect_assigned(default, out);
            }
            Node::While { body, .. } => collect_assigned(body, out),
            _ => {}
        }
    }
}

/// `rest` after the first whitespace-delimited word.
fn rest_after(line: &str, _keyword: &str) -> String {
    let mut parts = line.trim().splitn(2, char::is_whitespace);
    parts.next();
    parts.next().unwrap_or("").trim().to_owned()
}

/// Split `<target> <text>` where the target is one token; the text keeps its
/// internal spacing.
fn split_target(rest: &str) -> (String, String) {
    let mut parts = rest.trim().splitn(2, char::is_whitespace);
    let target = parts.next().unwrap_or("").to_owned();
    let text = parts.next().unwrap_or("").trim().to_owned();
    (target, text)
}

fn unquote(token: &str) -> String {
    let token = token.trim();
    if token.len() >= 2
        && ((token.starts_with('\'') && token.ends_with('\''))
            || (token.starts_with('"') && token.ends_with('"')))
    {
        token[1..token.len() - 1].to_owned()
    } else {
        token.to_owned()
    }
}

/// Collect the strings a supported node references into `out`, one per line.
fn collect_references(nodes: &[Node], out: &mut String) {
    for node in nodes {
        match node {
            Node::Say(text) | Node::Emote(text) | Node::Echo(text) => {
                out.push_str(text);
                out.push('\n');
            }
            Node::Send { target, text }
            | Node::EchoAround { target, text }
            | Node::ZoneEcho { zone: target, text } => {
                out.push_str(target);
                out.push('\n');
                out.push_str(text);
                out.push('\n');
            }
            Node::If { arms, otherwise } => {
                for (cond, body) in arms {
                    collect_cond(cond, out);
                    collect_references(body, out);
                }
                collect_references(otherwise, out);
            }
            Node::Switch {
                subject,
                cases,
                default,
            } => {
                out.push_str(subject);
                out.push('\n');
                for (patterns, body) in cases {
                    for pattern in patterns {
                        out.push_str(pattern);
                        out.push('\n');
                    }
                    collect_references(body, out);
                }
                collect_references(default, out);
            }
            Node::While { cond, body } => {
                collect_cond(cond, out);
                collect_references(body, out);
            }
            Node::Return(Some(value)) | Node::Nop(value) => {
                out.push_str(value);
                out.push('\n');
            }
            Node::Set { value, .. } | Node::Local { value, .. } | Node::Global { value, .. } => {
                out.push_str(value);
                out.push('\n');
            }
            Node::Eval { expr, .. } => {
                out.push_str(expr);
                out.push('\n');
            }
            Node::Load {
                vnum, target, wear, ..
            } => {
                out.push_str(vnum);
                out.push('\n');
                if let Some(target) = target {
                    out.push_str(target);
                    out.push('\n');
                }
                if let Some(wear) = wear {
                    out.push_str(wear);
                    out.push('\n');
                }
            }
            Node::Purge {
                target: Some(target),
            } => {
                out.push_str(target);
                out.push('\n');
            }
            Node::Teleport { target, room } => {
                out.push_str(target);
                out.push('\n');
                out.push_str(room);
                out.push('\n');
            }
            Node::Door {
                room,
                direction,
                op,
            } => {
                out.push_str(room);
                out.push('\n');
                out.push_str(direction);
                out.push('\n');
                if let DoorOp::Flags(flags) = op {
                    out.push_str(flags);
                    out.push('\n');
                }
            }
            Node::Force { target, command } => {
                out.push_str(target);
                out.push('\n');
                out.push_str(command);
                out.push('\n');
            }
            // The `%at%` command is a directive line, not text: its `%load%`
            // token is not a substitution, so only the room is scanned.
            Node::At { room, .. } => {
                out.push_str(room);
                out.push('\n');
            }
            Node::DgCast { spell, target } => {
                out.push_str(spell);
                out.push('\n');
                out.push_str(target);
                out.push('\n');
            }
            Node::Damage { target, amount } => {
                out.push_str(target);
                out.push('\n');
                out.push_str(amount);
                out.push('\n');
            }
            Node::Mkill { target } => {
                out.push_str(target);
                out.push('\n');
            }
            Node::Detach { vnum, target } => {
                out.push_str(vnum);
                out.push('\n');
                out.push_str(target);
                out.push('\n');
            }
            Node::Return(None)
            | Node::Halt
            | Node::Break
            | Node::Wait(_)
            | Node::Unset { .. }
            | Node::Remote { .. }
            | Node::RDelete { .. }
            | Node::Purge { target: None }
            | Node::Deferred { .. } => {}
            Node::Unsupported { .. } => {}
        }
    }
}

fn collect_cond(cond: &Cond, out: &mut String) {
    match cond {
        Cond::Truthy(value) | Cond::Unsupported(value) => {
            out.push_str(value);
            out.push('\n');
        }
        Cond::Cmp(left, _, right) => {
            out.push_str(left);
            out.push('\n');
            out.push_str(right);
            out.push('\n');
        }
        Cond::Not(inner) => collect_cond(inner, out),
        Cond::And(left, right) | Cond::Or(left, right) => {
            collect_cond(left, out);
            collect_cond(right, out);
        }
    }
}

/// Parse a boolean condition string.
pub fn parse_condition(text: &str) -> Cond {
    let tokens = tokenize_condition(text);
    if tokens.is_empty() {
        return Cond::Unsupported(text.to_owned());
    }
    let mut parser = CondParser {
        tokens: &tokens,
        pos: 0,
    };
    let cond = parser.parse_or();
    if parser.pos == tokens.len() {
        cond
    } else {
        Cond::Unsupported(text.to_owned())
    }
}

/// Split a condition into words and operators.
///
/// A `%…%` substitution is a **single** token, so `%actor.varexists(x)%` and
/// `%item.vnum%` keep their parentheses and dots instead of being split on the
/// embedded punctuation.
fn tokenize_condition(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '%' {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '%' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
            continue;
        }
        let two: Option<&str> = if i + 1 < chars.len() {
            match (ch, chars[i + 1]) {
                ('&', '&') => Some("&&"),
                ('|', '|') => Some("||"),
                ('=', '=') => Some("=="),
                ('!', '=') => Some("!="),
                ('<', '=') => Some("<="),
                ('>', '=') => Some(">="),
                ('/', '=') => Some("/="),
                _ => None,
            }
        } else {
            None
        };
        if let Some(op) = two {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            tokens.push(op.to_owned());
            i += 2;
            continue;
        }
        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            i += 1;
            continue;
        }
        if matches!(ch, '!' | '(' | ')' | '<' | '>') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            tokens.push(ch.to_string());
            i += 1;
            continue;
        }
        current.push(ch);
        i += 1;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

struct CondParser<'a> {
    tokens: &'a [String],
    pos: usize,
}

impl<'a> CondParser<'a> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(String::as_str)
    }

    fn parse_or(&mut self) -> Cond {
        let mut left = self.parse_and();
        while self.peek() == Some("||") {
            self.pos += 1;
            let right = self.parse_and();
            left = Cond::Or(Box::new(left), Box::new(right));
        }
        left
    }

    fn parse_and(&mut self) -> Cond {
        let mut left = self.parse_not();
        while self.peek() == Some("&&") {
            self.pos += 1;
            let right = self.parse_not();
            left = Cond::And(Box::new(left), Box::new(right));
        }
        left
    }

    fn parse_not(&mut self) -> Cond {
        if self.peek() == Some("!") {
            self.pos += 1;
            return Cond::Not(Box::new(self.parse_not()));
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Cond {
        if self.peek() == Some("(") {
            self.pos += 1;
            let inner = self.parse_or();
            if self.peek() == Some(")") {
                self.pos += 1;
            }
            return inner;
        }
        let Some(left) = self.tokens.get(self.pos).cloned() else {
            return Cond::Unsupported(String::new());
        };
        self.pos += 1;
        let Some(op_token) = self.tokens.get(self.pos).cloned() else {
            return Cond::Truthy(unquote(&left));
        };
        if let Some(op) = CmpOp::parse(&op_token) {
            self.pos += 1;
            match self.gather_operand(op) {
                Some(right) => Cond::Cmp(unquote(&left), op, unquote(&right)),
                None => Cond::Unsupported(format!("{left} {op_token}")),
            }
        } else if op_token == "/=" || op_token == "=" {
            // A DG string-substring operator; Phase 2 names it rather than
            // guessing at equality.
            self.pos += 1;
            if self.pos < self.tokens.len() {
                self.pos += 1;
            }
            Cond::Unsupported(format!("{left} {op_token}"))
        } else {
            Cond::Truthy(unquote(&left))
        }
    }

    /// Consume the right-hand operand of a comparison.
    ///
    /// `==`/`!=` are string comparisons in DG, and the operand may be a
    /// multi-word literal (`%actor.class% != Magic User`), so gather tokens
    /// until a logical operator, a closing paren, or another comparison. The
    /// ordering operators take a single token.
    fn gather_operand(&mut self, op: CmpOp) -> Option<String> {
        if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
            let right = self.tokens.get(self.pos).cloned();
            if right.is_some() {
                self.pos += 1;
            }
            return right;
        }
        let start = self.pos;
        while let Some(token) = self.tokens.get(self.pos) {
            if matches!(
                token.as_str(),
                "&&" | "||" | ")" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "/="
            ) {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            None
        } else {
            Some(self.tokens[start..self.pos].join(" "))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parses_a_tutorial_greet_body() {
        let body = "* greet the southbound player\n\
                    if %actor.is_pc% && %direction% == south\n\
                    \x20 say Can you help me, %actor.name%?\n\
                    \x20 wait 1 sec\n\
                    \x20 say If you slay him I'll give you all the coins I can spare.\n\
                    end\n";
        let program = parse_script(body);
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.constructs.get("if"), Some(&1));
        assert_eq!(program.constructs.get("say"), Some(&2));
        assert_eq!(program.constructs.get("wait.sec"), Some(&1));
        assert!(program.unsupported.is_empty(), "{:?}", program.unsupported);
        match &program.nodes[..] {
            [Node::If { arms, otherwise }] => {
                assert!(otherwise.is_empty());
                assert_eq!(arms.len(), 1);
                assert!(matches!(arms[0].0, Cond::And(_, _)));
                assert!(
                    arms[0]
                        .1
                        .iter()
                        .any(|n| matches!(n, Node::Say(s) if s.contains("%actor.name%")))
                );
                assert!(
                    arms[0]
                        .1
                        .iter()
                        .any(|n| matches!(n, Node::Wait(TriggerWait::Seconds(1))))
                );
            }
            other => panic!("expected one if, got {other:?}"),
        }
    }

    #[test]
    fn parses_if_elseif_else() {
        let body = "if %anumber% == 5\n\
                    \x20 say five\n\
                    elseif %anumber% > 90\n\
                    \x20 say big\n\
                    else\n\
                    \x20 say other\n\
                    end\n";
        let program = parse_script(body);
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.constructs.get("if"), Some(&1));
        assert_eq!(program.constructs.get("elseif"), Some(&1));
        assert_eq!(program.constructs.get("else"), Some(&1));
        match &program.nodes[..] {
            [Node::If { arms, otherwise }] => {
                assert_eq!(arms.len(), 2);
                assert_eq!(otherwise.len(), 1);
            }
            other => panic!("expected one if, got {other:?}"),
        }
    }

    #[test]
    fn parses_switch_and_while() {
        let body = "switch %actor.level%\n\
                    \x20 case 1\n\
                    \x20 case 2\n\
                    \x20 break\n\
                    \x20 case 4\n\
                    \x20   say four\n\
                    \x20 break\n\
                    \x20 default\n\
                    \x20   say many\n\
                    \x20 break\n\
                    done\n\
                    while %person%\n\
                    \x20 %echo% I am targetting %person.name%.\n\
                    \x20 set person %person.next_in_room%\n\
                    done\n";
        let program = parse_script(body);
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.constructs.get("switch"), Some(&1));
        assert_eq!(program.constructs.get("case"), Some(&3));
        assert_eq!(program.constructs.get("default"), Some(&1));
        assert_eq!(program.constructs.get("while"), Some(&1));
        match &program.nodes[..] {
            [Node::Switch { cases, default, .. }, Node::While { .. }] => {
                assert_eq!(cases.len(), 3);
                assert_eq!(default.len(), 1);
            }
            other => panic!("expected switch + while, got {other:?}"),
        }
    }

    #[test]
    fn parses_send_and_echoaround() {
        let program = parse_script(
            "%send% %actor% The guard humiliates you, and blocks your way.\n\
             %echoaround% %actor% The guard humiliates %actor.name%, and blocks %actor.hisher% way.\n\
             %zoneecho% 3000 the zone hums.\n",
        );
        assert!(program.is_valid(), "{:?}", program.error);
        assert!(matches!(
            &program.nodes[0],
            Node::Send { target, text } if target == "%actor%" && text.starts_with("The guard")
        ));
        assert!(matches!(
            &program.nodes[1],
            Node::EchoAround { target, text } if target == "%actor%" && text.contains("%actor.name%")
        ));
        assert!(matches!(&program.nodes[2], Node::ZoneEcho { zone, .. } if zone == "3000"));
    }

    #[test]
    fn unsupported_lines_become_named_markers() {
        let program = parse_script(
            "%transform% %actor% 10\n\
             give sword %actor%\n\
             mfollow %actor%\n\
             sleep\n",
        );
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.unsupported.get("transform"), Some(&1));
        assert_eq!(program.unsupported.get("give"), Some(&1));
        assert_eq!(program.unsupported.get("mfollow"), Some(&1));
        assert_eq!(program.unsupported.get("sleep"), Some(&1));
        assert_eq!(program.nodes.len(), 4);
        assert!(matches!(&program.nodes[0], Node::Unsupported { kind, .. } if kind == "transform"));
    }

    #[test]
    fn phase5_combat_directives_parse_as_nodes() {
        let program = parse_script(
            "dg_cast 'cure blind' %actor%\n\
             %damage% %actor% %random.5%\n\
             mkill %speech.cdr%\n\
             detach 59 %self.id%\n",
        );
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.constructs.get("dg_cast"), Some(&1));
        assert_eq!(program.constructs.get("damage"), Some(&1));
        assert_eq!(program.constructs.get("mkill"), Some(&1));
        assert_eq!(program.constructs.get("detach"), Some(&1));
        assert!(matches!(&program.nodes[0], Node::DgCast { spell, .. } if spell == "cure blind"));
        assert!(matches!(&program.nodes[1], Node::Damage { .. }));
        assert!(matches!(&program.nodes[2], Node::Mkill { .. }));
        assert!(matches!(&program.nodes[3], Node::Detach { vnum, .. } if vnum == "59"));
    }

    #[test]
    fn parses_phase4_movement_and_wait_directives() {
        let program = parse_script(
            "wait until 21:00\n\
             %teleport% %actor% 3001\n\
             %door% 225 up flags ab\n\
             %force% %actor% look\n\
             %at% 165 %load% m 15\n\
             %echoround% %actor% %actor.name% stares.\n",
        );
        assert!(program.is_valid(), "{:?}", program.error);
        assert!(matches!(
            &program.nodes[0],
            Node::Wait(TriggerWait::Until {
                hour: 21,
                minute: 0
            })
        ));
        assert!(
            matches!(&program.nodes[1], Node::Teleport { target, room } if target == "%actor%" && room == "3001")
        );
        assert!(matches!(
            &program.nodes[2],
            Node::Door { room, direction, op }
                if room == "225" && direction == "up" && *op == DoorOp::Flags("ab".to_owned())
        ));
        assert!(
            matches!(&program.nodes[3], Node::Force { target, command } if target == "%actor%" && command == "look")
        );
        assert!(
            matches!(&program.nodes[4], Node::At { room, command } if room == "165" && command == "%load% m 15")
        );
        assert!(
            matches!(&program.nodes[5], Node::EchoAround { target, .. } if target == "%actor%")
        );
        assert_eq!(program.constructs.get("teleport"), Some(&1));
        assert_eq!(program.constructs.get("door"), Some(&1));
        assert_eq!(program.constructs.get("force"), Some(&1));
        assert_eq!(program.constructs.get("at.load"), Some(&1));
        assert_eq!(program.constructs.get("echoaround"), Some(&1));
    }

    #[test]
    fn unbalanced_constructs_are_structural_errors_not_panics() {
        let missing_end = parse_script("if %actor.is_pc%\n  say hi\n");
        assert!(!missing_end.is_valid());
        let stray_end = parse_script("say hi\nend\n");
        assert!(!stray_end.is_valid());
    }

    #[test]
    fn set_eval_unset_parse_as_state_nodes() {
        let program = parse_script(
            "set value 10\neval value %value% + 1\nunset value\nremote value %actor.id%\n",
        );
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.constructs.get("set"), Some(&1));
        assert_eq!(program.constructs.get("eval"), Some(&1));
        assert_eq!(program.constructs.get("unset"), Some(&1));
        assert_eq!(program.constructs.get("remote"), Some(&1));
        assert!(
            matches!(&program.nodes[0], Node::Set { name, value } if name == "value" && value == "10")
        );
        assert!(matches!(&program.nodes[1], Node::Eval { name, .. } if name == "value"));
        assert!(matches!(&program.nodes[2], Node::Unset { name } if name == "value"));
        assert!(
            matches!(&program.nodes[3], Node::Remote { name, target } if name == "value" && target == "%actor.id%")
        );
        assert!(program.vars.contains("value"), "assigned names are known");
    }

    #[test]
    fn load_and_purge_parse_as_effects() {
        let program = parse_script(
            "%load% obj 50 %actor% light\n%load% mob %self.vnum%\n%load% m 15\n%purge% %object%\n",
        );
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.constructs.get("load.obj"), Some(&1));
        assert_eq!(program.constructs.get("load.mob"), Some(&2));
        assert_eq!(program.constructs.get("purge"), Some(&1));
        assert!(matches!(
            &program.nodes[0],
            Node::Load { kind: LoadKind::Obj, vnum, target: Some(target), wear: Some(wear) }
                if vnum == "50" && target == "%actor%" && wear == "light"
        ));
        assert!(matches!(
            &program.nodes[3],
            Node::Purge { target: Some(target) } if target == "%object%"
        ));
    }

    #[test]
    fn user_arrays_are_named_drops() {
        let program = parse_script("set txt[1] hello\nset phrase(2) world\n");
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.unsupported.get("set_array"), Some(&2));
    }

    #[test]
    fn assigned_variables_are_collected_for_coverage() {
        let program =
            parse_script("set polly 1\nif %polly%\n  eval polly %polly%+1\n  unset polly\nend\n");
        assert!(program.vars.contains("polly"));
        assert_eq!(program.unsupported.get("unset"), None);
    }

    #[test]
    fn parses_a_real_essential_guildguard_script() {
        // Verbatim `#3000` (Mage Guildguard - 3024) from the corpus.
        let body = "* Check the direction the player must go to enter the guild.\n\
                    if %direction% == south\n\
                    \x20 * Stop them if they are not the appropriate class.\n\
                    \x20 if %actor.class% != Magic User\n\
                    \x20   return 0\n\
                    \x20   %send% %actor% The guard humiliates you, and blocks your way.\n\
                    \x20   %echoaround% %actor% The guard humiliates %actor.name%, and blocks %actor.hisher% way.\n\
                    \x20 end\n\
                    end\n";
        let program = parse_script(body);
        assert!(program.is_valid(), "{:?}", program.error);
        assert!(program.unsupported.is_empty(), "{:?}", program.unsupported);
        assert_eq!(program.constructs.get("if"), Some(&2));
        assert_eq!(program.constructs.get("return"), Some(&1));
        assert_eq!(program.constructs.get("send"), Some(&1));
        assert_eq!(program.constructs.get("echoaround"), Some(&1));
    }

    #[test]
    fn condition_parser_handles_precedence_and_substring_operator() {
        assert!(matches!(
            parse_condition("%a% && %b% || %c%"),
            Cond::Or(_, _)
        ));
        assert!(matches!(
            parse_condition("concatenated /= %speech%"),
            Cond::Unsupported(_)
        ));
        assert!(matches!(parse_condition("!%actor.is_pc%"), Cond::Not(_)));
        assert!(matches!(
            parse_condition("%actor.level% <= 3"),
            Cond::Cmp(_, CmpOp::Le, _)
        ));
    }
}
