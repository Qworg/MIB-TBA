//! The Phase-2/3 AST interpreter (issue #15).
//!
//! Walks a [`crate::triggers::ast::ParsedProgram`] against a
//! [`TriggerContext`], resolving `%…%` substitutions and producing text
//! emissions, variable writes, spawn/purge requests, quest-flag signals, and a
//! list of named drops. It supports `if`/`switch`/`while`,
//! `return`/`nop`/`halt`, boolean conditions, the text commands, and (Phase 3)
//! `set`/`eval`/`unset`/`remote`/`global`/`local`/`rdelete` plus `%load%` and
//! `%purge%`.
//!
//! The interpreter never touches `&mut World`: it queues [`SpawnRequest`]s,
//! [`PurgeRequest`]s, and [`VarWrite`]s, which an exclusive system drains (the
//! spawn helpers take `&mut World`). That keeps evaluation pure and unit
//! testable, and keeps the `World` mutation in one place.
//!
//! A single record is bounded by [`STEP_CAP`] node visits: a looping script
//! records a `step_cap` drop and stops, so a bad script cannot wedge a tick.

use std::collections::BTreeMap;

use crate::combat::progression::TbaRng;
use crate::triggers::ast::{CmpOp, Cond, DoorOp, LoadKind, Node, STEP_CAP, TriggerWait};
use crate::triggers::context::{NamedDrop, TriggerContext, resolve_token};

/// Where an emitted line goes. The event system maps these to entities and
/// [`mib_core::prelude::OutputEvent`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTarget {
    /// The triggering actor only.
    Actor,
    /// The trigger owner only.
    SelfOnly,
    /// The victim only.
    Victim,
    /// The object only.
    Object,
    /// Everyone in the owner's room (the actor included).
    Room,
    /// Everyone in the room except the actor.
    RoomExceptActor,
    /// Everyone in the room except the owner.
    RoomExceptSelf,
    /// A zone broadcast (parsed, deferred: recorded as a drop).
    Zone,
}

/// One line the record wants delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emitted {
    pub target: TextTarget,
    pub text: String,
}

/// The interpreter result.
#[derive(Debug, Clone, Default)]
pub struct EvalOutcome {
    pub emitted: Vec<Emitted>,
    pub drops: Vec<NamedDrop>,
    /// True when a `return`/`halt` stopped the record early.
    pub halted: bool,
    /// The `return` value, when the record executed one (`return 0` blocks a
    /// `leave`/`door` event).
    pub returned: Option<String>,
    /// Node visits performed.
    pub steps: usize,
    /// True when the step cap tripped.
    pub capped: bool,
    /// A `wait` suspended the record: resume at [`Suspension::pc`] with
    /// [`Suspension::locals`] after [`Suspension::wait`].
    pub suspension: Option<Suspension>,
    /// Persisted variable writes (`remote`/`global`/`rdelete`).
    pub var_writes: Vec<VarWrite>,
    /// Item/NPC spawns requested by `%load%`.
    pub spawns: Vec<SpawnRequest>,
    /// Despawns requested by `%purge%`.
    pub purges: Vec<PurgeRequest>,
    /// `%teleport%` moves (Phase 4).
    pub teleports: Vec<TeleportRequest>,
    /// `%door%` state changes (Phase 4).
    pub doors: Vec<DoorRequest>,
    /// `%force%` command injections (Phase 4).
    pub forces: Vec<ForceRequest>,
    /// `%at%` commands in another room (Phase 4).
    pub ats: Vec<AtRequest>,
    /// `%zoneecho%` broadcasts (Phase 4).
    pub zone_echoes: Vec<ZoneEchoRequest>,
    /// A truthy `remote` write: the name of a quest flag a `stdlib.quests`
    /// `Flag` step can consume.
    pub flags: Vec<String>,
}

/// A suspended record, ready to resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suspension {
    pub wait: TriggerWait,
    /// The instruction after the `wait`.
    pub pc: usize,
    /// The trigger-local frame at suspension.
    pub locals: BTreeMap<String, String>,
}

/// A `%teleport%` request (the interpreter has no `World`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeleportRequest {
    /// The DG recipient token (`%actor%`/`%self%`), resolved to an entity later.
    pub target: String,
    /// The room vnum after substitution.
    pub room: String,
}

/// A `%door%` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorRequest {
    /// The room vnum after substitution.
    pub room: String,
    /// The direction word.
    pub direction: String,
    pub op: DoorOp,
}

/// A `%force%` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceRequest {
    /// The DG recipient token, resolved to an entity later.
    pub target: String,
    /// The substituted command line.
    pub command: String,
}

/// An `%at%` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtRequest {
    /// The room vnum after substitution.
    pub room: String,
    /// The substituted nested command.
    pub command: String,
}

/// A `%zoneecho%` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneEchoRequest {
    /// The room vnum naming the zone after substitution.
    pub zone: String,
    pub text: String,
}

/// Which persisted scope a write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarScope {
    Remote,
    Global,
}

/// One persisted variable write (or deletion when `value` is `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarWrite {
    pub scope: VarScope,
    pub name: String,
    pub value: Option<String>,
    /// The `remote` target token (`%actor.id%`); `None` for `global`.
    pub target: Option<String>,
}

/// A `%load%` request (the interpreter has no `World`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRequest {
    pub kind: LoadKind,
    pub vnum: String,
    pub target: Option<String>,
    pub wear: Option<String>,
}

/// A `%purge%` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeRequest {
    pub target: Option<String>,
}

/// One flattened instruction (Phase 4).
///
/// The AST's nested control flow is compiled to jumps so a record has a
/// **program counter**: a `wait` suspends at the instruction after it and the
/// timer queue resumes from the same index. The compilation is a pure function
/// of the node tree, so a `usize` pc is stable across a suspend/resume cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Instr {
    /// A leaf statement.
    Node(Node),
    /// Unconditional jump.
    Jump(usize),
    /// Evaluate `cond`; jump to `target` when false.
    Branch { cond: Cond, target: usize },
    /// Evaluate and store the `switch` subject for the following `CaseMatch`es.
    SwitchSubject(String),
    /// Jump to `target` when the stored switch subject matches any pattern.
    CaseMatch {
        patterns: Vec<String>,
        target: usize,
    },
}

/// Compile a record's node tree to a flat instruction list.
fn compile(nodes: &[Node]) -> Vec<Instr> {
    let mut out = Vec::new();
    let mut breaks: Vec<Vec<usize>> = Vec::new();
    emit_block(nodes, &mut out, &mut breaks);
    out
}

fn emit_block(nodes: &[Node], out: &mut Vec<Instr>, breaks: &mut Vec<Vec<usize>>) {
    for node in nodes {
        match node {
            Node::If { arms, otherwise } => {
                let mut end_jumps = Vec::new();
                for (index, (cond, body)) in arms.iter().enumerate() {
                    let branch = out.len();
                    out.push(Instr::Branch {
                        cond: cond.clone(),
                        target: 0,
                    });
                    emit_block(body, out, breaks);
                    let is_last = index + 1 == arms.len();
                    if !(is_last && otherwise.is_empty()) {
                        let jump = out.len();
                        out.push(Instr::Jump(0));
                        end_jumps.push(jump);
                    }
                    let target = out.len();
                    if let Instr::Branch { target: slot, .. } = &mut out[branch] {
                        *slot = target;
                    }
                }
                if !otherwise.is_empty() {
                    emit_block(otherwise, out, breaks);
                }
                let end = out.len();
                for jump in end_jumps {
                    if let Instr::Jump(target) = &mut out[jump] {
                        *target = end;
                    }
                }
            }
            Node::While { cond, body } => {
                let top = out.len();
                let branch = out.len();
                out.push(Instr::Branch {
                    cond: cond.clone(),
                    target: 0,
                });
                breaks.push(Vec::new());
                emit_block(body, out, breaks);
                let body_breaks = breaks.pop().unwrap_or_default();
                out.push(Instr::Jump(top));
                let end = out.len();
                if let Instr::Branch { target, .. } = &mut out[branch] {
                    *target = end;
                }
                for jump in body_breaks {
                    if let Instr::Jump(target) = &mut out[jump] {
                        *target = end;
                    }
                }
            }
            Node::Switch {
                subject,
                cases,
                default,
            } => {
                out.push(Instr::SwitchSubject(subject.clone()));
                breaks.push(Vec::new());
                let mut tests = Vec::new();
                for (patterns, _) in cases {
                    tests.push(out.len());
                    out.push(Instr::CaseMatch {
                        patterns: patterns.clone(),
                        target: 0,
                    });
                }
                let no_match = out.len();
                out.push(Instr::Jump(0));
                let mut end_jumps = Vec::new();
                for (index, (_, body)) in cases.iter().enumerate() {
                    let target = out.len();
                    if let Instr::CaseMatch { target: slot, .. } = &mut out[tests[index]] {
                        *slot = target;
                    }
                    emit_block(body, out, breaks);
                    let jump = out.len();
                    out.push(Instr::Jump(0));
                    end_jumps.push(jump);
                }
                let default_target = out.len();
                if let Instr::Jump(target) = &mut out[no_match] {
                    *target = default_target;
                }
                if !default.is_empty() {
                    emit_block(default, out, breaks);
                }
                let end = out.len();
                for jump in end_jumps {
                    if let Instr::Jump(target) = &mut out[jump] {
                        *target = end;
                    }
                }
                for jump in breaks.pop().unwrap_or_default() {
                    if let Instr::Jump(target) = &mut out[jump] {
                        *target = end;
                    }
                }
            }
            Node::Break => {
                if breaks.is_empty() {
                    out.push(Instr::Node(Node::Break));
                } else {
                    let jump = out.len();
                    out.push(Instr::Jump(0));
                    if let Some(list) = breaks.last_mut() {
                        list.push(jump);
                    }
                }
            }
            other => out.push(Instr::Node(other.clone())),
        }
    }
}

/// Run a parsed record to completion (or to its first `wait`).
pub fn run(nodes: &[Node], ctx: &TriggerContext, rng: &mut TbaRng) -> EvalOutcome {
    run_from(nodes, 0, ctx.vars.local.clone(), ctx, rng)
}

/// Resume a record at `pc` with a saved trigger-local frame.
pub fn run_from(
    nodes: &[Node],
    pc: usize,
    locals: BTreeMap<String, String>,
    ctx: &TriggerContext,
    rng: &mut TbaRng,
) -> EvalOutcome {
    let instrs = compile(nodes);
    let mut state = State {
        ctx,
        rng,
        locals,
        switch_value: String::new(),
        outcome: EvalOutcome::default(),
    };
    match state.run_instrs(&instrs, pc) {
        Flow::Normal | Flow::Suspend => {}
        Flow::Break | Flow::Return | Flow::Halt => state.outcome.halted = true,
    }
    state.outcome
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Normal,
    Break,
    Return,
    Halt,
    Suspend,
}

struct State<'a> {
    ctx: &'a TriggerContext,
    rng: &'a mut TbaRng,
    locals: BTreeMap<String, String>,
    /// The stored `switch` subject, set by [`Instr::SwitchSubject`].
    switch_value: String,
    outcome: EvalOutcome,
}

impl State<'_> {
    fn tick(&mut self) -> bool {
        self.outcome.steps += 1;
        if self.outcome.steps > STEP_CAP {
            if !self.outcome.capped {
                self.outcome.capped = true;
                self.outcome
                    .drops
                    .push(NamedDrop::new("step_cap", "record exceeded STEP_CAP"));
            }
            return false;
        }
        true
    }

    fn run_instrs(&mut self, instrs: &[Instr], start: usize) -> Flow {
        let mut pc = start;
        while pc < instrs.len() {
            if !self.tick() {
                return Flow::Halt;
            }
            match &instrs[pc] {
                Instr::Jump(target) => pc = *target,
                Instr::Branch { cond, target } => {
                    if self.eval_cond(cond) {
                        pc += 1;
                    } else {
                        pc = *target;
                    }
                }
                Instr::SwitchSubject(subject) => {
                    self.switch_value = self.substitute(subject);
                    pc += 1;
                }
                Instr::CaseMatch { patterns, target } => {
                    let value = self.switch_value.clone();
                    if patterns
                        .iter()
                        .any(|pattern| self.compare_eq(pattern, &value))
                    {
                        pc = *target;
                    } else {
                        pc += 1;
                    }
                }
                Instr::Node(Node::Wait(wait)) => {
                    if let TriggerWait::Until { minute, .. } = wait
                        && *minute != 0
                    {
                        // The MIB clock is whole-hour; a sub-hour `until`
                        // target is honoured at the hour boundary and the
                        // minute is a named drop rather than silently ignored.
                        self.outcome.drops.push(NamedDrop::new(
                            "wait_minute",
                            format!("sub-hour wait target ignored: {minute}"),
                        ));
                    }
                    self.outcome.suspension = Some(Suspension {
                        wait: wait.clone(),
                        pc: pc + 1,
                        locals: self.locals.clone(),
                    });
                    return Flow::Suspend;
                }
                Instr::Node(node) => match self.run_node(node) {
                    Flow::Normal => pc += 1,
                    other => return other,
                },
            }
        }
        Flow::Normal
    }

    fn run_node(&mut self, node: &Node) -> Flow {
        match node {
            Node::Say(text) | Node::Emote(text) | Node::Echo(text) => {
                self.emit(TextTarget::Room, text);
                Flow::Normal
            }
            Node::Send { target, text } => {
                match recipient(target) {
                    Some(recipient) => {
                        let rendered = self.substitute(text);
                        self.outcome.emitted.push(Emitted {
                            target: recipient,
                            text: rendered,
                        });
                    }
                    None => self.drop_unsupported("send_target", target),
                }
                Flow::Normal
            }
            Node::EchoAround { target, text } => {
                match recipient(target) {
                    Some(TextTarget::Actor) => self.emit(TextTarget::RoomExceptActor, text),
                    Some(TextTarget::SelfOnly) => self.emit(TextTarget::RoomExceptSelf, text),
                    Some(other) => {
                        let rendered = self.substitute(text);
                        self.outcome.emitted.push(Emitted {
                            target: other,
                            text: rendered,
                        });
                    }
                    None => self.drop_unsupported("echoaround_target", target),
                }
                Flow::Normal
            }
            Node::ZoneEcho { zone, text } => {
                let zone = self.substitute(zone);
                let text = self.substitute(text);
                self.outcome
                    .zone_echoes
                    .push(ZoneEchoRequest { zone, text });
                Flow::Normal
            }
            // Control flow is flattened by `compile`; a raw control node here
            // would mean the compiler missed a case. Count it rather than
            // executing it recursively (which would break the program counter).
            Node::If { .. } | Node::Switch { .. } | Node::While { .. } => {
                self.outcome
                    .drops
                    .push(NamedDrop::new("internal_control", "unflattened block"));
                Flow::Normal
            }
            Node::Wait(wait) => {
                // `run_instrs` handles a wait so it can record `pc + 1`; this
                // arm keeps the match exhaustive for a direct `run_node`.
                self.outcome.suspension = Some(Suspension {
                    wait: wait.clone(),
                    pc: 0,
                    locals: self.locals.clone(),
                });
                Flow::Suspend
            }
            Node::Set { name, value } => {
                let rendered = self.substitute(value);
                self.assign(name, rendered);
                Flow::Normal
            }
            Node::Eval { name, expr } => {
                let rendered = self.substitute(expr);
                let value = eval_arithmetic(&rendered)
                    .map(|number| number.to_string())
                    .unwrap_or(rendered);
                self.assign(name, value);
                Flow::Normal
            }
            Node::Unset { name } => {
                self.locals.remove(&normalize_name(name));
                Flow::Normal
            }
            Node::Remote { name, target } => {
                let name = normalize_name(name);
                let value = self.locals.get(&name).cloned().unwrap_or_default();
                let target = self.substitute(target);
                self.outcome.var_writes.push(VarWrite {
                    scope: VarScope::Remote,
                    name: name.clone(),
                    value: Some(value.clone()),
                    target: Some(target),
                });
                if truthy(&value) {
                    self.outcome.flags.push(name);
                }
                Flow::Normal
            }
            Node::Global { name, value } => {
                let name = normalize_name(name);
                let value = if value.trim().is_empty() {
                    self.locals.get(&name).cloned().unwrap_or_default()
                } else {
                    self.substitute(value)
                };
                self.outcome.var_writes.push(VarWrite {
                    scope: VarScope::Global,
                    name,
                    value: Some(value),
                    target: None,
                });
                Flow::Normal
            }
            Node::Local { name, value } => {
                if !value.trim().is_empty() {
                    let rendered = self.substitute(value);
                    self.assign(name, rendered);
                }
                Flow::Normal
            }
            Node::RDelete { name, target } => {
                let name = normalize_name(name);
                self.locals.remove(&name);
                let target = self.substitute(target);
                self.outcome.var_writes.push(VarWrite {
                    scope: VarScope::Remote,
                    name,
                    value: None,
                    target: Some(target),
                });
                Flow::Normal
            }
            Node::Load {
                kind,
                vnum,
                target,
                wear,
            } => {
                let vnum = self.substitute(vnum);
                // The recipient is a DG token (`%actor%`/`%self%`), not a
                // substituted value; the event dispatch resolves it to an
                // entity so a name change cannot break the holder.
                let target = target.clone();
                let wear = wear.as_ref().map(|wear| self.substitute(wear));
                self.outcome.spawns.push(SpawnRequest {
                    kind: *kind,
                    vnum,
                    target,
                    wear,
                });
                Flow::Normal
            }
            Node::Purge { target } => {
                self.outcome.purges.push(PurgeRequest {
                    target: target.clone(),
                });
                Flow::Normal
            }
            Node::Teleport { target, room } => {
                // The recipient is a DG token (`%actor%`/`%self%`), resolved to
                // an entity by the effect drain; the vnum is substituted now.
                let room = self.substitute(room);
                self.outcome.teleports.push(TeleportRequest {
                    target: target.clone(),
                    room,
                });
                Flow::Normal
            }
            Node::Door {
                room,
                direction,
                op,
            } => {
                let room = self.substitute(room);
                let direction = self.substitute(direction);
                let op = match op {
                    DoorOp::Flags(flags) => DoorOp::Flags(self.substitute(flags)),
                    other => other.clone(),
                };
                self.outcome.doors.push(DoorRequest {
                    room,
                    direction,
                    op,
                });
                Flow::Normal
            }
            Node::Force { target, command } => {
                let command = self.substitute(command);
                self.outcome.forces.push(ForceRequest {
                    target: target.clone(),
                    command,
                });
                Flow::Normal
            }
            Node::At { room, command } => {
                // The nested command is re-parsed by the queue layer, so its
                // directive token (`%load%`) must survive verbatim; only the
                // room vnum is substituted here.
                let room = self.substitute(room);
                self.outcome.ats.push(AtRequest {
                    room,
                    command: command.clone(),
                });
                Flow::Normal
            }
            Node::Return(value) => {
                self.outcome.returned = value.clone();
                Flow::Return
            }
            Node::Halt => Flow::Halt,
            Node::Break => Flow::Break,
            Node::Nop(command) => {
                self.outcome.drops.push(NamedDrop::new(
                    "nop",
                    format!("deferred command: {command}"),
                ));
                Flow::Normal
            }
            Node::Deferred { kind, line } => {
                self.outcome
                    .drops
                    .push(NamedDrop::new(kind, format!("deferred: {line}")));
                Flow::Normal
            }
            Node::Unsupported { kind, line } => {
                self.outcome
                    .drops
                    .push(NamedDrop::new(kind, format!("unsupported: {line}")));
                Flow::Normal
            }
        }
    }

    fn emit(&mut self, target: TextTarget, template: &str) {
        let text = self.substitute(template);
        self.outcome.emitted.push(Emitted { target, text });
    }

    fn drop_unsupported(&mut self, kind: &str, detail: &str) {
        self.outcome
            .drops
            .push(NamedDrop::new(kind, format!("unresolved: {detail}")));
    }

    /// Assign a trigger-local variable, normalising the name the way the
    /// resolver looks it up.
    fn assign(&mut self, name: &str, value: String) {
        let name = normalize_name(name);
        if name.is_empty() {
            self.outcome
                .drops
                .push(NamedDrop::new("set", "empty variable name"));
            return;
        }
        self.locals.insert(name, value);
    }

    /// Substitute every `%…%` token in `template`.
    fn substitute(&mut self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let chars: Vec<char> = template.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != '%' {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            let start = i + 1;
            let mut end = start;
            let mut closed = false;
            while end < chars.len() {
                if chars[end] == '%' {
                    closed = true;
                    break;
                }
                if chars[end] == '\n' {
                    break;
                }
                end += 1;
            }
            if closed && end > start {
                let token: String = chars[start..end].iter().collect();
                let resolved = resolve_token(&token, self.ctx, &self.locals, self.rng);
                if !resolved.known {
                    self.outcome
                        .drops
                        .push(NamedDrop::new("unknown_token", token.clone()));
                }
                out.push_str(&resolved.value);
                i = end + 1;
            } else {
                out.push('%');
                i = start;
            }
        }
        out
    }

    fn eval_cond(&mut self, cond: &Cond) -> bool {
        match cond {
            Cond::Truthy(token) => {
                let value = self.substitute(token);
                truthy(&value)
            }
            Cond::Cmp(left, op, right) => {
                let left = self.substitute(left);
                let right = self.substitute(right);
                compare(&left, *op, &right)
            }
            Cond::Not(inner) => !self.eval_cond(inner),
            Cond::And(left, right) => self.eval_cond(left) && self.eval_cond(right),
            Cond::Or(left, right) => self.eval_cond(left) || self.eval_cond(right),
            Cond::Unsupported(text) => {
                self.outcome
                    .drops
                    .push(NamedDrop::new("unsupported_condition", text.clone()));
                false
            }
        }
    }

    fn compare_eq(&mut self, pattern: &str, value: &str) -> bool {
        let pattern = self.substitute(pattern);
        if let (Ok(a), Ok(b)) = (pattern.trim().parse::<i64>(), value.trim().parse::<i64>()) {
            return a == b;
        }
        pattern.trim().eq_ignore_ascii_case(value.trim())
    }
}

/// Whether a substituted condition value is truthy (non-empty and not `0`).
fn truthy(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value != "0"
}

/// The canonical spelling of a DG variable name: trimmed and lowercased, so
/// `%HOURS_IN_DAY%` finds `set HOURS_IN_DAY`.
fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// Evaluate the bounded integer arithmetic DG uses in `eval`: `+ - * / %`,
/// parentheses, and unary minus, all saturating and never panicking. Returns
/// `None` when the expression is not pure arithmetic (a bare string), so the
/// caller stores it verbatim.
fn eval_arithmetic(expr: &str) -> Option<i64> {
    let chars: Vec<char> = expr.chars().collect();
    let mut parser = ExprParser {
        chars: &chars,
        pos: 0,
    };
    let value = parser.parse_add()?;
    parser.skip_ws();
    (parser.pos == parser.chars.len()).then_some(value)
}

struct ExprParser<'a> {
    chars: &'a [char],
    pos: usize,
}

impl ExprParser<'_> {
    fn skip_ws(&mut self) {
        while self.chars.get(self.pos).is_some_and(|c| c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn parse_add(&mut self) -> Option<i64> {
        let mut value = self.parse_mul()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.pos += 1;
                    value = value.saturating_add(self.parse_mul()?);
                }
                Some('-') => {
                    self.pos += 1;
                    value = value.saturating_sub(self.parse_mul()?);
                }
                _ => return Some(value),
            }
        }
    }

    fn parse_mul(&mut self) -> Option<i64> {
        let mut value = self.parse_unary()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    value = value.saturating_mul(self.parse_unary()?);
                }
                Some('/') => {
                    self.pos += 1;
                    let divisor = self.parse_unary()?;
                    value = if divisor == 0 { 0 } else { value / divisor };
                }
                Some('%') => {
                    self.pos += 1;
                    let divisor = self.parse_unary()?;
                    value = if divisor == 0 { 0 } else { value % divisor };
                }
                _ => return Some(value),
            }
        }
    }

    fn parse_unary(&mut self) -> Option<i64> {
        self.skip_ws();
        match self.peek() {
            Some('-') => {
                self.pos += 1;
                Some(-self.parse_unary()?)
            }
            Some('+') => {
                self.pos += 1;
                self.parse_unary()
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_atom(&mut self) -> Option<i64> {
        self.skip_ws();
        if self.peek() == Some('(') {
            self.pos += 1;
            let value = self.parse_add()?;
            self.skip_ws();
            if self.peek() == Some(')') {
                self.pos += 1;
                return Some(value);
            }
            return None;
        }
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        let digits: String = self.chars[start..self.pos].iter().collect();
        digits.parse::<i64>().ok()
    }
}

/// Compare two substituted values, numerically when both parse.
fn compare(left: &str, op: CmpOp, right: &str) -> bool {
    if let (Ok(a), Ok(b)) = (left.trim().parse::<i64>(), right.trim().parse::<i64>()) {
        return match op {
            CmpOp::Eq => a == b,
            CmpOp::Ne => a != b,
            CmpOp::Lt => a < b,
            CmpOp::Gt => a > b,
            CmpOp::Le => a <= b,
            CmpOp::Ge => a >= b,
        };
    }
    let a = left.trim().to_ascii_lowercase();
    let b = right.trim().to_ascii_lowercase();
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
        CmpOp::Lt => a < b,
        CmpOp::Gt => a > b,
        CmpOp::Le => a <= b,
        CmpOp::Ge => a >= b,
    }
}

/// Map a DG recipient token (`%actor%`, `%self%`, `%victim%`, `%object%`) to
/// its [`TextTarget`].
fn recipient(token: &str) -> Option<TextTarget> {
    let normalized = token.trim().trim_matches('%').to_ascii_lowercase();
    match normalized.as_str() {
        "actor" => Some(TextTarget::Actor),
        "self" => Some(TextTarget::SelfOnly),
        "victim" => Some(TextTarget::Victim),
        "object" | "obj" => Some(TextTarget::Object),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::triggers::ast::parse_script;
    use crate::triggers::context::{ActorView, EntityView};

    fn context() -> TriggerContext {
        TriggerContext {
            actor: Some(ActorView {
                name: "Aria".to_owned(),
                is_pc: true,
                id: 1,
                vnum: 0,
                level: 7,
                class: "warrior".to_owned(),
            }),
            self_: Some(EntityView {
                name: "the questmaster".to_owned(),
                vnum: 139,
            }),
            ..TriggerContext::default()
        }
    }

    fn run_body(body: &str, ctx: &TriggerContext) -> EvalOutcome {
        let program = parse_script(body);
        assert!(program.is_valid(), "{:?}", program.error);
        let mut rng = TbaRng::from_seed(7);
        run(&program.nodes, ctx, &mut rng)
    }

    #[test]
    fn say_substitutes_the_actor_name() {
        let out = run_body("say Can you help me, %actor.name%?", &context());
        assert_eq!(out.emitted.len(), 1);
        assert_eq!(out.emitted[0].target, TextTarget::Room);
        assert_eq!(out.emitted[0].text, "Can you help me, Aria?");
        assert!(out.drops.is_empty(), "{:?}", out.drops);
    }

    #[test]
    fn if_condition_gates_output() {
        let out = run_body(
            "if %actor.is_pc% && %actor.level% < 10\n\
             \x20 say hello %actor.name%\n\
             else\n\
             \x20 say go away\n\
             end\n",
            &context(),
        );
        assert_eq!(out.emitted.len(), 1);
        assert_eq!(out.emitted[0].text, "hello Aria");
    }

    #[test]
    fn unknown_tokens_resolve_empty_and_drop() {
        let out = run_body("say value is %anumber%", &context());
        assert_eq!(out.emitted[0].text, "value is ");
        assert!(
            out.drops
                .iter()
                .any(|drop| drop.kind == "unknown_token" && drop.detail == "anumber"),
            "{:?}",
            out.drops
        );
    }

    #[test]
    fn unsupported_construct_is_reported_not_executed() {
        let out = run_body("%transform% %actor% 10\nsay after\n", &context());
        assert_eq!(out.emitted.len(), 1);
        assert_eq!(out.emitted[0].text, "after");
        assert!(out.drops.iter().any(|drop| drop.kind == "transform"));
    }

    #[test]
    fn wait_suspends_with_a_resume_position() {
        let out = run_body("say before\nwait 2 sec\nsay after\n", &context());
        assert_eq!(out.emitted.len(), 1);
        assert_eq!(out.emitted[0].text, "before");
        let suspension = out.suspension.expect("a wait suspends");
        assert_eq!(suspension.wait, TriggerWait::Seconds(2));

        // Resume from the saved program counter.
        let program = parse_script("say before\nwait 2 sec\nsay after\n");
        let mut rng = TbaRng::from_seed(7);
        let resumed = run_from(
            &program.nodes,
            suspension.pc,
            suspension.locals,
            &context(),
            &mut rng,
        );
        assert_eq!(resumed.emitted.len(), 1);
        assert_eq!(resumed.emitted[0].text, "after");
    }

    #[test]
    fn movement_directives_queue_requests() {
        let out = run_body(
            "%teleport% %actor% 3001\n\
             %door% 225 up flags ab\n\
             %force% %actor% look\n\
             %at% 165 %load% m 15\n\
             %zoneecho% 3000 the zone hums.\n",
            &context(),
        );
        assert_eq!(out.teleports.len(), 1);
        assert_eq!(out.teleports[0].target, "%actor%");
        assert_eq!(out.teleports[0].room, "3001");
        assert_eq!(out.doors.len(), 1);
        assert_eq!(out.doors[0].op, DoorOp::Flags("ab".to_owned()));
        assert_eq!(out.forces.len(), 1);
        assert_eq!(out.forces[0].command, "look");
        assert_eq!(out.ats.len(), 1);
        assert_eq!(out.ats[0].command, "%load% m 15");
        assert_eq!(out.zone_echoes.len(), 1);
        assert_eq!(out.zone_echoes[0].text, "the zone hums.");
        assert!(out.drops.is_empty(), "{:?}", out.drops);
    }

    #[test]
    fn return_records_its_value() {
        let out = run_body("return 0\n", &context());
        assert_eq!(out.returned.as_deref(), Some("0"));
        assert!(out.halted);
    }

    #[test]
    fn set_eval_and_unset_drive_conditions() {
        let out = run_body(
            "set value 10\n\
             eval value %value% + 5\n\
             if %value% == 15\n\
               say fifteen\n\
             end\n\
             unset value\n\
             say done\n",
            &context(),
        );
        let lines: Vec<&str> = out.emitted.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(lines, vec!["fifteen", "done"]);
    }

    #[test]
    fn eval_handles_parentheses_and_missing_operands() {
        let out = run_body(
            "set day 3\n\
             set hour 5\n\
             eval ticks (%day%/24)+%hour%\n\
             eval word hello\n\
             say %ticks% %word%\n",
            &context(),
        );
        assert_eq!(out.emitted[0].text, "5 hello");
    }

    #[test]
    fn remote_persists_a_quest_flag() {
        let out = run_body(
            "set solved_tutorial 1\nremote solved_tutorial %actor.id%\n",
            &context(),
        );
        assert_eq!(out.var_writes.len(), 1);
        let write = &out.var_writes[0];
        assert_eq!(write.scope, VarScope::Remote);
        assert_eq!(write.name, "solved_tutorial");
        assert_eq!(write.value.as_deref(), Some("1"));
        assert_eq!(write.target.as_deref(), Some("1"), "%actor.id% substitutes");
        assert_eq!(out.flags, vec!["solved_tutorial".to_owned()]);
    }

    #[test]
    fn varexists_sees_a_local_assignment() {
        let out = run_body(
            "set on_quest_zone_1 1\n\
             if %actor.varexists(on_quest_zone_1)%\n\
               say already\n\
             else\n\
               say fresh\n\
             end\n",
            &context(),
        );
        assert_eq!(out.emitted[0].text, "already");
    }

    #[test]
    fn load_and_purge_queue_effects() {
        let out = run_body(
            "%load% obj 50 %actor% light\n%load% mob %self.vnum%\n%purge% %object%\n",
            &context(),
        );
        assert_eq!(out.spawns.len(), 2);
        assert_eq!(out.spawns[0].kind, LoadKind::Obj);
        assert_eq!(out.spawns[0].target.as_deref(), Some("%actor%"));
        assert_eq!(out.spawns[1].kind, LoadKind::Mob);
        assert_eq!(out.spawns[1].vnum, "139", "%self.vnum% substituted");
        assert_eq!(out.purges.len(), 1);
        assert_eq!(out.purges[0].target.as_deref(), Some("%object%"));
        assert!(out.drops.is_empty(), "{:?}", out.drops);
    }

    #[test]
    fn a_looping_script_hits_the_step_cap() {
        // `while %self%` never terminates because `self` is always truthy and
        // no variable is mutated; the cap must stop it.
        let out = run_body("while %self%\n  say forever\n done\n", &context());
        assert!(out.capped, "the step cap trips");
        assert!(out.halted);
        assert!(out.drops.iter().any(|drop| drop.kind == "step_cap"));
    }

    #[test]
    fn return_halts_before_later_commands() {
        let out = run_body("return 0\nsay unreachable\n", &context());
        assert!(out.halted);
        assert!(out.emitted.is_empty());
    }

    #[test]
    fn while_break_stops_the_loop() {
        let out = run_body(
            "set i 0\n\
             while %i% < 10\n\
               eval i %i% + 1\n\
               if %i% == 3\n\
                 break\n\
               end\n\
               say %i%\n\
             done\n\
             say done\n",
            &context(),
        );
        let lines: Vec<&str> = out.emitted.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(lines, vec!["1", "2", "done"]);
    }

    #[test]
    fn wait_inside_a_loop_resumes_the_iteration() {
        let body = "set i 0\nwhile %i% < 2\neval i %i% + 1\nwait 1 sec\nsay %i%\ndone\n";
        let program = parse_script(body);
        let mut rng = TbaRng::from_seed(7);

        let first = run(&program.nodes, &context(), &mut rng);
        assert!(first.emitted.is_empty(), "suspends before the first say");
        let suspension = first.suspension.expect("wait suspends");
        assert_eq!(suspension.locals.get("i").map(String::as_str), Some("1"));

        let second = run_from(
            &program.nodes,
            suspension.pc,
            suspension.locals,
            &context(),
            &mut rng,
        );
        let lines: Vec<&str> = second.emitted.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(lines, vec!["1"], "the first iteration resumes");
        let suspension = second.suspension.expect("the second iteration waits");
        assert_eq!(suspension.locals.get("i").map(String::as_str), Some("2"));

        let third = run_from(
            &program.nodes,
            suspension.pc,
            suspension.locals,
            &context(),
            &mut rng,
        );
        let lines: Vec<&str> = third.emitted.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(lines, vec!["2"], "the loop then exits");
        assert!(third.suspension.is_none());
    }

    #[test]
    fn switch_matches_a_case() {
        let out = run_body(
            "switch %actor.level%\n\
             \x20 case 1\n\
             \x20   say novice\n\
             \x20 break\n\
             \x20 case 7\n\
             \x20   say veteran\n\
             \x20 break\n\
             \x20 default\n\
             \x20   say other\n\
             done\n",
            &context(),
        );
        assert_eq!(out.emitted.len(), 1);
        assert_eq!(out.emitted[0].text, "veteran");
    }

    #[test]
    fn send_targets_the_actor_and_echoaround_excludes_them() {
        let out = run_body(
            "%send% %actor% you feel watched.\n\
             %echoaround% %actor% %actor.name% is watched.\n",
            &context(),
        );
        assert_eq!(out.emitted.len(), 2);
        assert_eq!(out.emitted[0].target, TextTarget::Actor);
        assert_eq!(out.emitted[1].target, TextTarget::RoomExceptActor);
        assert_eq!(out.emitted[1].text, "Aria is watched.");
    }
}
