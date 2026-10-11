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
//! `halt`. `set`/`eval`/`unset` parse into [`Node::Deferred`] (Phase 3 owns
//! them). Everything else is a named unsupported drop.

use std::collections::BTreeMap;

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
    /// `set`/`eval`/`unset` — parsed but deferred to Phase 3.
    Deferred { kind: String, line: String },
    /// A line whose construct Phase 2 does not support.
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
    ParsedProgram {
        nodes,
        constructs: parser.constructs,
        unsupported: parser.unsupported,
        error: parser.error,
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
            "set" | "eval" | "unset" => {
                self.count(&keyword);
                Node::Deferred {
                    kind: keyword,
                    line: line.to_owned(),
                }
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
            Node::Return(None) | Node::Halt | Node::Break | Node::Deferred { .. } => {}
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
fn tokenize_condition(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
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
            let right = self.tokens.get(self.pos).cloned();
            match right {
                Some(right) => {
                    self.pos += 1;
                    Cond::Cmp(unquote(&left), op, unquote(&right))
                }
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
        assert_eq!(program.unsupported.get("wait"), Some(&1));
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
            "%load% obj 1300\n\
             give sword %actor%\n\
             dg_cast 'poison' %actor%\n\
             mkill %actor%\n",
        );
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.unsupported.get("load"), Some(&1));
        assert_eq!(program.unsupported.get("give"), Some(&1));
        assert_eq!(program.unsupported.get("dg_cast"), Some(&1));
        assert_eq!(program.unsupported.get("mkill"), Some(&1));
        assert_eq!(program.nodes.len(), 4);
        assert!(matches!(&program.nodes[0], Node::Unsupported { kind, .. } if kind == "load"));
    }

    #[test]
    fn unbalanced_constructs_are_structural_errors_not_panics() {
        let missing_end = parse_script("if %actor.is_pc%\n  say hi\n");
        assert!(!missing_end.is_valid());
        let stray_end = parse_script("say hi\nend\n");
        assert!(!stray_end.is_valid());
    }

    #[test]
    fn set_eval_unset_are_deferred() {
        let program = parse_script("set value 10\neval value %value% + 1\nunset value\n");
        assert!(program.is_valid(), "{:?}", program.error);
        assert_eq!(program.constructs.get("set"), Some(&1));
        assert_eq!(program.constructs.get("eval"), Some(&1));
        assert_eq!(program.constructs.get("unset"), Some(&1));
        assert!(
            program
                .nodes
                .iter()
                .all(|node| matches!(node, Node::Deferred { .. }))
        );
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
