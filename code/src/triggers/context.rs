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

use std::collections::{BTreeMap, BTreeSet};

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

/// The in-world clock fields (`%time.hour%` etc.) the resolver exposes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClockView {
    pub hour: u32,
    pub day: u32,
    pub month: u32,
    pub year: u32,
}

/// The variable scopes one trigger firing sees (issue #15, Phase 3).
///
/// `local` is the trigger-local frame; the interpreter clones it into a mutable
/// working copy. `remote`/`global` are the persisted scopes read from the
/// owner's [`TbaTriggerVars`](crate::triggers::effects::TbaTriggerVars)
/// component. The vectors back the built-in pseudo-arrays
/// (`%actor.inventory%`, `%self.contents%`, `%random.char%`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggerVars {
    pub local: BTreeMap<String, String>,
    pub remote: BTreeMap<String, String>,
    pub global: BTreeMap<String, String>,
    pub self_remote: BTreeMap<String, String>,
    pub actor_inventory: Vec<EntityView>,
    pub self_contents: Vec<EntityView>,
    pub room_characters: Vec<EntityView>,
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
    /// The in-world clock, for `%time.hour%`/`%time.day%` (Phase 4).
    pub clock: Option<ClockView>,
    /// The variable scopes and pseudo-array sources.
    pub vars: TriggerVars,
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
///
/// `locals` is the interpreter's mutable trigger-local frame; the persisted
/// `remote`/`global` scopes are read from `ctx.vars`. A bare name resolves in
/// the order local, remote, global. An `actor.<name>` field that is not a
/// modelled attribute resolves to the actor's persisted variable (empty when
/// absent), which is what `%actor.varexists(name)%` distinguishes.
pub fn resolve_token(
    token: &str,
    ctx: &TriggerContext,
    locals: &BTreeMap<String, String>,
    rng: &mut TbaRng,
) -> Resolved {
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
        "random.char" => Resolved::known(random_char(ctx, rng)),
        "hours_in_day" => Resolved::known("24"),
        "time" | "time.hour" => Resolved::known(clock_field(ctx, |clock| clock.hour)),
        "time.day" => Resolved::known(clock_field(ctx, |clock| clock.day)),
        "time.month" => Resolved::known(clock_field(ctx, |clock| clock.month)),
        "time.year" => Resolved::known(clock_field(ctx, |clock| clock.year)),
        "random.dir" | "random.obj" => {
            // Modelled but not filled this pass: a known-empty, not a drop.
            Resolved::empty_known()
        }
        _ => {
            if let Some(value) = variable_value(&normalized, ctx, locals) {
                return Resolved::known(value);
            }
            resolve_prefixed(&normalized, ctx, locals, rng)
        }
    }
}

/// Look up a bare variable name in the local, then remote, then global scope.
fn variable_value(
    name: &str,
    ctx: &TriggerContext,
    locals: &BTreeMap<String, String>,
) -> Option<String> {
    locals
        .get(name)
        .or_else(|| ctx.vars.remote.get(name))
        .or_else(|| ctx.vars.global.get(name))
        .cloned()
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

fn resolve_prefixed(
    normalized: &str,
    ctx: &TriggerContext,
    locals: &BTreeMap<String, String>,
    rng: &mut TbaRng,
) -> Resolved {
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
        "actor" => resolve_actor_field(field, ctx, locals),
        "self" => {
            if field == "contents" {
                Resolved::known(first_name(&ctx.vars.self_contents))
            } else {
                resolve_entity_field(field, ctx.self_.as_ref())
            }
        }
        "object" | "obj" => resolve_object_field(field, ctx.object.as_ref()),
        "victim" => resolve_entity_field(field, ctx.victim.as_ref()),
        "target" => resolve_entity_field(field, ctx.target.as_ref()),
        "room" | "inroom" => match field {
            "vnum" | "number" => Resolved::known(opt_vnum(ctx.room_vnum)),
            _ => Resolved::empty_known(),
        },
        "time" => match field {
            "hour" => Resolved::known(clock_field(ctx, |clock| clock.hour)),
            "day" => Resolved::known(clock_field(ctx, |clock| clock.day)),
            "month" => Resolved::known(clock_field(ctx, |clock| clock.month)),
            "year" => Resolved::known(clock_field(ctx, |clock| clock.year)),
            _ => Resolved::empty_known(),
        },
        _ => Resolved::unknown(),
    }
}

fn resolve_actor_field(
    field: &str,
    ctx: &TriggerContext,
    locals: &BTreeMap<String, String>,
) -> Resolved {
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
        "inventory" => Resolved::known(first_name(&ctx.vars.actor_inventory)),
        // Modelled-but-unavailable attributes resolve empty rather than
        // dropping: MIB has no sex/alignment/raw stats on the actor here.
        "sex" | "align" | "str" | "dex" | "int" | "wis" | "con" | "cha" | "hitp" | "gold"
        | "exp" | "questpoints" | "contents" | "room" | "follower" | "carried_by" | "position"
        | "fighting" | "mount" | "leader" | "affect" => Resolved::empty_known(),
        _ if field.starts_with("varexists(") => {
            Resolved::known(if actor_variable_exists(field, ctx, locals) {
                "1"
            } else {
                "0"
            })
        }
        _ if field.starts_with("has_item(") => Resolved::known("0"),
        _ if field.starts_with("eq(")
            || field.starts_with("gold(")
            || field.starts_with("exp(") =>
        {
            Resolved::empty_known()
        }
        _ => match ctx.vars.remote.get(field) {
            // A persisted actor variable (DG's `%actor.<name>%`). Absent is
            // empty, so `varexists` remains the way to test existence.
            Some(value) => Resolved::known(value.clone()),
            None => Resolved::empty_known(),
        },
    }
}

/// `%actor.varexists(name)%`: true when the name is a local, remote, or global
/// variable with any value.
fn actor_variable_exists(
    field: &str,
    ctx: &TriggerContext,
    locals: &BTreeMap<String, String>,
) -> bool {
    let Some(name) = field
        .strip_prefix("varexists(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let name = name.trim().to_ascii_lowercase();
    locals.contains_key(&name)
        || ctx.vars.remote.contains_key(&name)
        || ctx.vars.global.contains_key(&name)
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

fn first_name(entities: &[EntityView]) -> String {
    entities
        .first()
        .map(|entity| entity.name.clone())
        .unwrap_or_default()
}

/// One in-world clock field as a string, or empty when the clock is absent.
fn clock_field(ctx: &TriggerContext, field: impl FnOnce(&ClockView) -> u32) -> String {
    ctx.clock
        .as_ref()
        .map(|clock| field(clock).to_string())
        .unwrap_or_default()
}

/// Draw a random character in the room (`%random.char%`), or empty when none.
fn random_char(ctx: &TriggerContext, rng: &mut TbaRng) -> String {
    let candidates = &ctx.vars.room_characters;
    if candidates.is_empty() {
        return String::new();
    }
    let index = rng.range(0, candidates.len() as i32 - 1) as usize;
    candidates
        .get(index)
        .map(|entity| entity.name.clone())
        .unwrap_or_default()
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

/// Count unknown `%…%` tokens in `source` for the coverage report. A token in
/// `known` (an assigned variable) is never counted.
pub fn unknown_tokens_excluding(source: &str, known: &BTreeSet<String>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for token in extract_tokens(source) {
        let normalized = token.trim().to_ascii_lowercase();
        if known.contains(&normalized) {
            continue;
        }
        if !is_known_token(&token) {
            *counts.entry(token).or_default() += 1;
        }
    }
    counts
}

/// Count unknown `%…%` tokens in `source` (no known-variable set).
pub fn unknown_tokens(source: &str) -> BTreeMap<String, usize> {
    unknown_tokens_excluding(source, &BTreeSet::new())
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

    fn no_locals() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn resolves_the_common_tokens() {
        let mut rng = TbaRng::from_seed(1);
        let ctx = context();
        let locals = no_locals();
        assert_eq!(
            resolve_token("actor.name", &ctx, &locals, &mut rng).value,
            "Aria"
        );
        assert_eq!(
            resolve_token("actor.is_pc", &ctx, &locals, &mut rng).value,
            "1"
        );
        assert_eq!(
            resolve_token("self.name", &ctx, &locals, &mut rng).value,
            "the guild guard"
        );
        assert_eq!(
            resolve_token("self.vnum", &ctx, &locals, &mut rng).value,
            "3024"
        );
        assert_eq!(
            resolve_token("object.vnum", &ctx, &locals, &mut rng).value,
            "1300"
        );
        assert_eq!(
            resolve_token("direction", &ctx, &locals, &mut rng).value,
            "south"
        );
        assert_eq!(resolve_token("cmd", &ctx, &locals, &mut rng).value, "");
        assert_eq!(
            resolve_token("speech", &ctx, &locals, &mut rng).value,
            "hello there"
        );
        let roll: i32 = resolve_token("random.10", &ctx, &locals, &mut rng)
            .value
            .parse()
            .expect("a number");
        assert!((1..=10).contains(&roll), "random roll {roll} in 1..=10");
    }

    #[test]
    fn bare_names_resolve_the_local_frame_first() {
        let mut rng = TbaRng::from_seed(1);
        let ctx = context();
        let mut locals = no_locals();
        locals.insert("polly".to_owned(), "7".to_owned());
        let resolved = resolve_token("polly", &ctx, &locals, &mut rng);
        assert_eq!(resolved.value, "7");
        assert!(resolved.known);
    }

    #[test]
    fn varexists_reflects_persisted_and_local_variables() {
        let mut rng = TbaRng::from_seed(1);
        let mut ctx = context();
        let mut locals = no_locals();
        let absent = resolve_token("actor.varexists(quest_mice)", &ctx, &locals, &mut rng);
        assert_eq!(absent.value, "0");
        assert!(absent.known);
        ctx.vars
            .remote
            .insert("quest_mice".to_owned(), "1".to_owned());
        let present = resolve_token("actor.varexists(quest_mice)", &ctx, &locals, &mut rng);
        assert_eq!(present.value, "1");
        assert_eq!(
            resolve_token("actor.quest_mice", &ctx, &locals, &mut rng).value,
            "1"
        );
        locals.insert("local_flag".to_owned(), "1".to_owned());
        assert_eq!(
            resolve_token("actor.varexists(local_flag)", &ctx, &locals, &mut rng).value,
            "1"
        );
    }

    #[test]
    fn pseudo_arrays_resolve_to_a_real_value() {
        let mut rng = TbaRng::from_seed(1);
        let mut ctx = context();
        let locals = no_locals();
        ctx.vars.actor_inventory = vec![EntityView {
            name: "a rusty sword".to_owned(),
            vnum: 100,
        }];
        ctx.vars.room_characters = vec![
            EntityView {
                name: "Aria".to_owned(),
                vnum: 0,
            },
            EntityView {
                name: "the guard".to_owned(),
                vnum: 3024,
            },
        ];
        assert_eq!(
            resolve_token("actor.inventory", &ctx, &locals, &mut rng).value,
            "a rusty sword"
        );
        let draw = resolve_token("random.char", &ctx, &locals, &mut rng).value;
        assert!(draw == "Aria" || draw == "the guard", "draw was {draw}");
        assert!(resolve_token("random.char", &ctx, &locals, &mut rng).known);
    }

    #[test]
    fn unknown_tokens_drop_but_assigned_names_do_not() {
        let mut rng = TbaRng::from_seed(1);
        let ctx = context();
        let locals = no_locals();
        let unknown = resolve_token("anumber", &ctx, &locals, &mut rng);
        assert_eq!(unknown.value, "");
        assert!(!unknown.known);
        assert!(!is_known_token("anumber"));
        assert!(is_known_token("random.99"));
        assert!(is_known_token("actor.name"));
        assert!(!is_known_token("i.vnum"));

        let mut known = BTreeSet::new();
        known.insert("anumber".to_owned());
        assert!(unknown_tokens_excluding("say %anumber%", &known).is_empty());
        assert_eq!(
            unknown_tokens_excluding("say %other%", &known).get("other"),
            Some(&1)
        );
    }

    #[test]
    fn sex_resolves_known_empty_not_a_drop() {
        let mut rng = TbaRng::from_seed(1);
        let locals = no_locals();
        let resolved = resolve_token("actor.sex", &context(), &locals, &mut rng);
        assert!(resolved.known);
        assert_eq!(resolved.value, "");
    }

    #[test]
    fn extracts_tokens_across_a_body() {
        let tokens = extract_tokens("say hi %actor.name%, %self.vnum% %random.10%");
        assert_eq!(tokens, vec!["actor.name", "self.vnum", "random.10"]);
    }
}
