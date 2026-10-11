//! `TriggerContext` and the DG `%…%` substitution resolver (issue #15,
//! Phase 2).
//!
//! The context is a plain-data snapshot of the entities one trigger record
//! sees: the `actor` (the player who spoke/entered/commanded), `self` (the
//! trigger owner), `object`, `victim`/`target`, the `direction`, and the
//! `cmd`/`speech` text. Keeping it free of Bevy types makes substitution
//! independently testable.
//!
//! An unknown token resolves to the empty string **and is counted** by the
//! caller as a named drop; a token MIB models but cannot fill (for example
//! `%actor.sex%`, since MIB has no sex attribute) resolves known-empty. The
//! distinction is [`Resolved::known`].

use std::collections::BTreeMap;

use crate::combat::progression::TbaRng;

/// The actor (the player who triggered the record).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActorView {
    pub name: String,
    pub is_pc: bool,
    pub id: i64,
    pub vnum: i32,
    pub level: u32,
    pub class: String,
}

/// A named, vnum-bearing entity (`self`, `object`, `victim`, `target`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityView {
    pub name: String,
    pub vnum: i32,
}

/// One trigger firing's data.
#[derive(Debug, Clone, Default)]
pub struct TriggerContext {
    pub actor: Option<ActorView>,
    pub self_: Option<EntityView>,
    pub object: Option<EntityView>,
    pub victim: Option<EntityView>,
    pub target: Option<EntityView>,
    pub direction: Option<String>,
    pub cmd: Option<String>,
    pub speech: Option<String>,
    /// The room vnum, for `%room.vnum%`/`%inroom.vnum%`.
    pub room_vnum: Option<i32>,
}

/// A resolved token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub value: String,
    pub known: bool,
}

impl Resolved {
    fn known(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            known: true,
        }
    }

    fn empty_known() -> Self {
        Self {
            value: String::new(),
            known: true,
        }
    }

    fn unknown() -> Self {
        Self {
            value: String::new(),
            known: false,
        }
    }
}

/// Resolve one `%…%` token (without the surrounding `%`).
pub fn resolve_token(token: &str, ctx: &TriggerContext, rng: &mut TbaRng) -> Resolved {
    let normalized = token.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "direction" => Resolved::known(ctx.direction.clone().unwrap_or_default()),
        "cmd" | "cmd.mudcommand" => Resolved::known(ctx.cmd.clone().unwrap_or_default()),
        "speech" => Resolved::known(ctx.speech.clone().unwrap_or_default()),
        "room" | "room.vnum" | "inroom" | "inroom.vnum" => Resolved::known(opt_vnum(ctx.room_vnum)),
        "actor" => Resolved::known(actor(ctx).map(|a| a.name.clone()).unwrap_or_default()),
        "self" => Resolved::known(entity(ctx.self_.as_ref())),
        "object" | "obj" => Resolved::known(entity(ctx.object.as_ref())),
        "victim" => Resolved::known(entity(ctx.victim.as_ref())),
        "target" => Resolved::known(entity(ctx.target.as_ref())),
        "random.char" | "random.dir" | "random.obj" => {
            // Phase 2 does not have an entity pool to draw from; the token is
            // modelled but resolves empty (a known-empty, not a drop).
            Resolved::empty_known()
        }
        "time" | "time.hour" | "time.day" | "time.month" | "time.year" | "hours_in_day" => {
            Resolved::empty_known()
        }
        _ => resolve_prefixed(&normalized, ctx, rng),
    }
}

/// Whether a token is one the resolver understands (used by the coverage
/// report, so an unknown token is counted without needing a live context).
pub fn is_known_token(token: &str) -> bool {
    let normalized = token.trim().to_ascii_lowercase();
    if let Some(digits) = normalized.strip_prefix("random.")
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    if matches!(
        normalized.as_str(),
        "direction"
            | "cmd"
            | "cmd.mudcommand"
            | "speech"
            | "room"
            | "room.vnum"
            | "inroom"
            | "inroom.vnum"
            | "actor"
            | "self"
            | "object"
            | "obj"
            | "victim"
            | "target"
            | "random.char"
            | "random.dir"
            | "random.obj"
            | "time"
            | "time.hour"
            | "time.day"
            | "time.month"
            | "time.year"
            | "hours_in_day"
    ) {
        return true;
    }
    let Some((prefix, _)) = normalized.split_once('.') else {
        return false;
    };
    matches!(
        prefix,
        "actor" | "self" | "object" | "obj" | "victim" | "target" | "room" | "inroom" | "time"
    )
}

fn resolve_prefixed(normalized: &str, ctx: &TriggerContext, rng: &mut TbaRng) -> Resolved {
    if let Some(digits) = normalized
        .strip_prefix("random.")
        .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    {
        // A parse that cannot fail for the filter above; `unwrap_or(1)` keeps
        // the resolver panic-free if the invariant ever changes.
        let high = digits.parse::<i32>().unwrap_or(1).max(1);
        return Resolved::known(rng.range(1, high).to_string());
    }
    let Some((prefix, field)) = normalized.split_once('.') else {
        return Resolved::unknown();
    };
    match prefix {
        "actor" => resolve_actor_field(field, ctx),
        "self" => resolve_entity_field(field, ctx.self_.as_ref()),
        "object" | "obj" => resolve_object_field(field, ctx.object.as_ref()),
        "victim" => resolve_entity_field(field, ctx.victim.as_ref()),
        "target" => resolve_entity_field(field, ctx.target.as_ref()),
        "room" | "inroom" => match field {
            "vnum" | "number" => Resolved::known(opt_vnum(ctx.room_vnum)),
            _ => Resolved::empty_known(),
        },
        "time" => Resolved::empty_known(),
        _ => Resolved::unknown(),
    }
}

fn resolve_actor_field(field: &str, ctx: &TriggerContext) -> Resolved {
    let Some(actor) = actor(ctx) else {
        return Resolved::empty_known();
    };
    match field {
        "name" => Resolved::known(actor.name.clone()),
        "is_pc" => Resolved::known(if actor.is_pc { "1" } else { "0" }),
        "id" => Resolved::known(actor.id.to_string()),
        "vnum" => Resolved::known(actor.vnum.to_string()),
        "level" => Resolved::known(actor.level.to_string()),
        "class" => Resolved::known(actor.class.clone()),
        "hisher" => Resolved::known("their"),
        "himher" => Resolved::known("them"),
        // Modelled-but-unavailable attributes resolve empty rather than
        // dropping: MIB has no sex/alignment/raw stats on the actor here.
        "sex" | "align" | "str" | "dex" | "int" | "wis" | "con" | "cha" | "hitp" | "gold"
        | "exp" | "questpoints" | "inventory" | "contents" | "room" | "follower" | "carried_by"
        | "position" | "fighting" | "mount" | "leader" | "affect" => Resolved::empty_known(),
        _ if field.starts_with("varexists(") || field.starts_with("has_item(") => {
            // No variables this pass: `varexists` is false.
            Resolved::known("0")
        }
        _ if field.starts_with("eq(")
            || field.starts_with("gold(")
            || field.starts_with("exp(") =>
        {
            Resolved::empty_known()
        }
        _ => Resolved::unknown(),
    }
}

fn resolve_entity_field(field: &str, entity: Option<&EntityView>) -> Resolved {
    let Some(entity) = entity else {
        return Resolved::empty_known();
    };
    match field {
        "name" => Resolved::known(entity.name.clone()),
        "vnum" => Resolved::known(entity.vnum.to_string()),
        "room" | "hisher" | "himher" | "follower" | "carried_by" | "position" => {
            if field == "hisher" {
                Resolved::known("their")
            } else if field == "himher" {
                Resolved::known("them")
            } else {
                Resolved::empty_known()
            }
        }
        _ => Resolved::unknown(),
    }
}

fn resolve_object_field(field: &str, object: Option<&EntityView>) -> Resolved {
    let Some(object) = object else {
        return Resolved::empty_known();
    };
    match field {
        "name" | "shortdesc" => Resolved::known(object.name.clone()),
        "vnum" => Resolved::known(object.vnum.to_string()),
        "type" | "cost" | "carried_by" | "in_room" => Resolved::empty_known(),
        _ => Resolved::unknown(),
    }
}

fn actor(ctx: &TriggerContext) -> Option<&ActorView> {
    ctx.actor.as_ref()
}

fn entity(entity: Option<&EntityView>) -> String {
    entity.map(|e| e.name.clone()).unwrap_or_default()
}

fn opt_vnum(vnum: Option<i32>) -> String {
    vnum.map(|v| v.to_string()).unwrap_or_default()
}

/// A named drop recorded during evaluation or coverage.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NamedDrop {
    pub kind: String,
    pub detail: String,
}

impl NamedDrop {
    pub fn new(kind: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            detail: detail.into(),
        }
    }
}

/// Count unknown `%…%` tokens in `source` for the coverage report.
pub fn unknown_tokens(source: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for token in extract_tokens(source) {
        if !is_known_token(&token) {
            *counts.entry(token).or_default() += 1;
        }
    }
    counts
}

/// Extract every `%…%` token from a script body.
pub fn extract_tokens(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
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
            // A token never spans a newline.
            if chars[end] == '\n' {
                break;
            }
            end += 1;
        }
        if closed && end > start {
            tokens.push(chars[start..end].iter().collect());
            i = end + 1;
        } else {
            i = start;
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn context() -> TriggerContext {
        TriggerContext {
            actor: Some(ActorView {
                name: "Aria".to_owned(),
                is_pc: true,
                id: 42,
                vnum: 0,
                level: 7,
                class: "warrior".to_owned(),
            }),
            self_: Some(EntityView {
                name: "the guild guard".to_owned(),
                vnum: 3024,
            }),
            object: Some(EntityView {
                name: "a golden key".to_owned(),
                vnum: 1300,
            }),
            direction: Some("south".to_owned()),
            speech: Some("hello there".to_owned()),
            room_vnum: Some(3001),
            ..TriggerContext::default()
        }
    }

    #[test]
    fn resolves_the_common_tokens() {
        let mut rng = TbaRng::from_seed(1);
        let ctx = context();
        assert_eq!(resolve_token("actor.name", &ctx, &mut rng).value, "Aria");
        assert_eq!(resolve_token("actor.is_pc", &ctx, &mut rng).value, "1");
        assert_eq!(
            resolve_token("self.name", &ctx, &mut rng).value,
            "the guild guard"
        );
        assert_eq!(resolve_token("self.vnum", &ctx, &mut rng).value, "3024");
        assert_eq!(resolve_token("object.vnum", &ctx, &mut rng).value, "1300");
        assert_eq!(resolve_token("direction", &ctx, &mut rng).value, "south");
        assert_eq!(resolve_token("cmd", &ctx, &mut rng).value, "");
        assert_eq!(resolve_token("speech", &ctx, &mut rng).value, "hello there");
        let roll: i32 = resolve_token("random.10", &ctx, &mut rng)
            .value
            .parse()
            .expect("a number");
        assert!((1..=10).contains(&roll), "random roll {roll} in 1..=10");
    }

    #[test]
    fn varexists_is_false_and_unknown_tokens_drop() {
        let mut rng = TbaRng::from_seed(1);
        let ctx = context();
        let resolved = resolve_token("actor.varexists(quest_mice)", &ctx, &mut rng);
        assert_eq!(resolved.value, "0");
        assert!(resolved.known);

        let unknown = resolve_token("anumber", &ctx, &mut rng);
        assert_eq!(unknown.value, "");
        assert!(!unknown.known);
        assert!(!is_known_token("anumber"));
        assert!(is_known_token("random.99"));
        assert!(is_known_token("actor.name"));
        assert!(!is_known_token("i.vnum"));
    }

    #[test]
    fn sex_resolves_known_empty_not_a_drop() {
        let mut rng = TbaRng::from_seed(1);
        let resolved = resolve_token("actor.sex", &context(), &mut rng);
        assert!(resolved.known);
        assert_eq!(resolved.value, "");
    }

    #[test]
    fn extracts_tokens_across_a_body() {
        let tokens = extract_tokens("say hi %actor.name%, %self.vnum% %random.10%");
        assert_eq!(tokens, vec!["actor.name", "self.vnum", "random.10"]);
    }
}
