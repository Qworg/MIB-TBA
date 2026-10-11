//! The Phase-2 AST interpreter (issue #15).
//!
//! Walks a [`crate::triggers::ast::ParsedProgram`] against a
//! [`TriggerContext`], resolving `%…%` substitutions and producing text
//! emissions plus a list of named drops. It supports `if`/`switch`/`while`,
//! `return`/`nop`/`halt`, boolean conditions, and the text commands. `set`,
//! `eval`, `unset`, `%load%`, `%purge%`, `wait`, `dg_cast` and every other
//! directive are parsed-but-deferred and recorded as named drops — never
//! executed.
//!
//! A single record is bounded by [`STEP_CAP`] node visits: a looping script
//! records a `step_cap` drop and stops, so a bad script cannot wedge a tick.

use crate::combat::progression::TbaRng;
use crate::triggers::ast::{CmpOp, Cond, Node, STEP_CAP};
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
    /// Node visits performed.
    pub steps: usize,
    /// True when the step cap tripped.
    pub capped: bool,
}

/// Run a parsed record.
pub fn run(nodes: &[Node], ctx: &TriggerContext, rng: &mut TbaRng) -> EvalOutcome {
    let mut state = State {
        ctx,
        rng,
        outcome: EvalOutcome::default(),
    };
    match state.run_nodes(nodes) {
        Flow::Normal => {}
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
}

struct State<'a> {
    ctx: &'a TriggerContext,
    rng: &'a mut TbaRng,
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

    fn run_nodes(&mut self, nodes: &[Node]) -> Flow {
        for node in nodes {
            if !self.tick() {
                return Flow::Halt;
            }
            match self.run_node(node) {
                Flow::Normal => {}
                other => return other,
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
            Node::ZoneEcho { .. } => {
                self.outcome
                    .drops
                    .push(NamedDrop::new("zoneecho", "zone broadcast is Phase 4"));
                Flow::Normal
            }
            Node::If { arms, otherwise } => {
                for (cond, body) in arms {
                    if self.eval_cond(cond) {
                        return self.run_nodes(body);
                    }
                }
                self.run_nodes(otherwise)
            }
            Node::Switch {
                subject,
                cases,
                default,
            } => {
                let value = self.substitute(subject);
                for (patterns, body) in cases {
                    if patterns
                        .iter()
                        .any(|pattern| self.compare_eq(pattern, &value))
                    {
                        return match self.run_nodes(body) {
                            Flow::Break => Flow::Normal,
                            other => other,
                        };
                    }
                }
                self.run_nodes(default)
            }
            Node::While { cond, body } => loop {
                if !self.eval_cond(cond) {
                    return Flow::Normal;
                }
                match self.run_nodes(body) {
                    Flow::Break | Flow::Normal => {}
                    other => return other,
                }
                if !self.tick() {
                    return Flow::Halt;
                }
            },
            Node::Return(_) => Flow::Return,
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
                let resolved = resolve_token(&token, self.ctx, self.rng);
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
        let out = run_body("%load% obj 1300\nsay after\n", &context());
        assert_eq!(out.emitted.len(), 1);
        assert_eq!(out.emitted[0].text, "after");
        assert!(out.drops.iter().any(|drop| drop.kind == "load"));
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
