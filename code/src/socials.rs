//! Issue #17 — Circle social emotes: `social <name> [target]` and `socials`.
//!
//! Circle registers every one of its ~491 socials as a top-level command and
//! renders three strings per activation (actor, room, victim) after `act()`
//! substitution (`src/act.social.c`, `src/comm.c:perform_act`). MIB's fixed
//! command grammar has no `Command` variant for an open-ended verb set, and
//! `stdlib.verbs` cannot express a per-social message table, so the pack owns a
//! `tba.socials` section instead. [`handle_social_commands`] runs in
//! [`BridgeSet::Game`](mib_core::prelude::BridgeSet::Game) after the stdlib
//! router, reading [`PlayerInput::RawLine`] the way `shops.rs`/`spells.rs` do;
//! `social`/`socials` are declared (effect-free) in `content/stdlib.verbs.yaml`
//! so the router claims them rather than reporting an unknown verb.
//!
//! Substitution follows `perform_act` for the codes the source uses:
//! `$n`/`$N` (names), `$e`/`$E`, `$m`/`$M`, `$s`/`$S` (pronouns — MIB models no
//! sex, so the neutral singular *they/them/their* is used), `$t`/`$T` (the
//! named body part or object), `$p`/`$P`/`$o`/`$O` (object names), and the
//! `$u`/`$U` case switches. An unknown code (the source's stray `$i`) resolves
//! to nothing exactly as `perform_act`'s default branch does.
//!
//! Pronoun note: tbaMUD's `HMHR`/`HSHR`/`HSSH` choose by `GET_SEX`. MIB models
//! no sex (`Player` and `Npc` carry only a name; the converter records mob sex
//! as a permanent drop), so a `he`/`she` choice is impossible without inventing
//! data. Neutral singular *they* is the only non-arbitrary rendering.

use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{CarriedBy, Item, Location, Npc, Player, SessionState};

use crate::combat::commands::text;
use crate::combat::{TbaCombatant, TbaProgress};
use crate::content::Position;
use crate::dg::intake::commands::{DgClaimed, is_claimed};
use crate::module::TbaContent;
use crate::views::ActorView;

/// Circle's exact refusal for an unknown social (`do_action`, `act.social.c`).
const MSG_UNSUPPORTED: &str = "That action is not supported.";
/// Circle's refusal when a body part is supplied to a social without body text.
const MSG_NO_BODY_PART: &str = "Sorry, this social does not support body parts.";
/// Fallback when a social has no `not_found` string (`do_action`).
const MSG_DEFAULT_NOT_FOUND: &str = "I don't see anything by that name here.";
/// Fallback when a self-target social has no `char_auto` string.
const MSG_DEFAULT_AUTO: &str = "Erm, no.";
/// The actor's position gate.
const MSG_BAD_POSITION: &str = "You are in no position to do that.";
/// The actor's level gate.
const MSG_BAD_LEVEL: &str = "You are not experienced enough to use that social.";
/// The victim's position gate.
const MSG_BAD_VICTIM_POSITION: &str = "They are not in a proper position for that.";

/// One parsed social command, before the social or target is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocialCommand {
    /// `socials` — list the available socials.
    List,
    /// `social <name> [<target>] [<body part>]`.
    Act {
        name: String,
        target: String,
        part: String,
    },
}

/// Parse a raw line as a social command, or `None` when it is not one.
pub fn parse_social_line(line: &str) -> Option<SocialCommand> {
    let line = line.trim();
    let (verb, rest) = match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index + 1..].trim()),
        None => (line, ""),
    };
    if verb.eq_ignore_ascii_case("socials") {
        return Some(SocialCommand::List);
    }
    if !verb.eq_ignore_ascii_case("social") {
        return None;
    }
    let mut parts = rest.split_whitespace();
    let name = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();
    let part = parts.next().unwrap_or("").to_owned();
    Some(SocialCommand::Act { name, target, part })
}

/// The names a social template can substitute. `target`, `object` and `body`
/// are `None` when that argument was not supplied.
#[derive(Debug, Clone, Copy, Default)]
pub struct SocialNames<'a> {
    pub actor: &'a str,
    pub target: Option<&'a str>,
    pub object: Option<&'a str>,
    pub body: Option<&'a str>,
}

/// Expand one Circle social message against the resolved names, following
/// `perform_act` (`comm.c:2604`). Unknown `$`-codes expand to nothing.
pub fn substitute(template: &str, names: &SocialNames<'_>) -> String {
    // MIB has no sex model; neutral singular they/them/their is the documented
    // fallback (see the module docs).
    let actor_pronouns = ("they", "them", "their");
    let target_pronouns = ("they", "them", "their");

    let mut out = String::with_capacity(template.len() + names.actor.len());
    let mut chars = template.chars().peekable();
    let mut uppercase_next = false;
    while let Some(ch) = chars.next() {
        if ch != '$' {
            if uppercase_next && !ch.is_whitespace() {
                out.extend(ch.to_uppercase());
                uppercase_next = false;
            } else {
                out.push(ch);
            }
            continue;
        }
        let code = match chars.next() {
            Some(code) => code,
            None => {
                out.push('$');
                break;
            }
        };
        let substitution: &str = match code {
            'n' => names.actor,
            'N' => names.target.unwrap_or(""),
            'e' => actor_pronouns.0,
            'm' => actor_pronouns.1,
            's' => actor_pronouns.2,
            'E' => target_pronouns.0,
            'M' => target_pronouns.1,
            'S' => target_pronouns.2,
            't' => names.body.unwrap_or(""),
            'T' => names.target.or(names.body).unwrap_or(""),
            'p' | 'o' => names.object.unwrap_or(""),
            'P' | 'O' => names.object.unwrap_or(""),
            'a' | 'A' | 'i' | 'F' => "",
            'u' => {
                // Uppercase the first letter of the previous word.
                uppercase_last_word(&mut out);
                ""
            }
            'U' => {
                uppercase_next = true;
                ""
            }
            '$' => "$",
            _ => "",
        };
        for subst in substitution.chars() {
            if uppercase_next && !subst.is_whitespace() {
                out.extend(subst.to_uppercase());
                uppercase_next = false;
            } else {
                out.push(subst);
            }
        }
    }
    out
}

/// Uppercase the first character after the final whitespace in `text`.
fn uppercase_last_word(text: &mut String) {
    let start = text.rfind(char::is_whitespace).map_or(0, |index| index + 1);
    if let Some(rest) = text.get(start..)
        && let Some(first) = rest.chars().next()
    {
        let upper: String = first.to_uppercase().collect();
        let mut replaced = String::with_capacity(text.len() + upper.len());
        replaced.push_str(&text[..start]);
        replaced.push_str(&upper);
        replaced.push_str(&rest[first.len_utf8()..]);
        *text = replaced;
    }
}

/// Why a social cannot run, or `None` when it can. Mirrors Circle's command
/// table gate (`minimum_position`/`minimum_level`, set from the social in
/// `create_command_list`) plus the victim gate in `do_action`.
pub fn gate_reason(
    social: &crate::content::TbaSocialDef,
    actor_position: Position,
    actor_level: u32,
    victim_position: Option<Position>,
) -> Option<&'static str> {
    if let Some(min) = position_from_i32(social.min_char_position)
        && actor_position < min
    {
        return Some(MSG_BAD_POSITION);
    }
    if social.min_level_char > 0 && actor_level < social.min_level_char as u32 {
        return Some(MSG_BAD_LEVEL);
    }
    if let Some(min) = position_from_i32(social.min_victim_position)
        && let Some(victim) = victim_position
        && victim < min
    {
        return Some(MSG_BAD_VICTIM_POSITION);
    }
    None
}

/// Map the source `POS_*` number onto [`Position`]. A negative or unknown
/// value means "no gate" (`None`).
fn position_from_i32(value: i32) -> Option<Position> {
    match value {
        0 => Some(Position::Dead),
        1 => Some(Position::MortallyWounded),
        2 => Some(Position::Incapacitated),
        3 => Some(Position::Stunned),
        4 => Some(Position::Sleeping),
        5 => Some(Position::Resting),
        6 => Some(Position::Sitting),
        7 => Some(Position::Fighting),
        8 => Some(Position::Standing),
        _ => None,
    }
}

/// Case-insensitive `contains` match, the same convention the item/shop
/// lookups use.
fn name_matches(needle: &str, name: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    !needle.is_empty() && name.to_lowercase().contains(&needle)
}

fn actor_name(players: &Query<(Entity, &Player, &Location)>, session: Entity) -> String {
    players
        .iter()
        .find(|(entity, _, _)| *entity == session)
        .map(|(_, player, _)| player.name.clone())
        .unwrap_or_else(|| "Someone".to_owned())
}

fn actor_room(players: &Query<(Entity, &Player, &Location)>, session: Entity) -> Option<Entity> {
    players
        .iter()
        .find(|(entity, _, _)| *entity == session)
        .map(|(_, _, location)| location.room)
}

fn position_of(combatants: &Query<&TbaCombatant>, entity: Entity) -> Position {
    combatants
        .get(entity)
        .map(|combatant| combatant.position)
        .unwrap_or(Position::Standing)
}

fn level_of(progress: &Query<&TbaProgress>, session: Entity) -> u32 {
    progress.get(session).map(|p| p.level).unwrap_or(1)
}

/// A social target: an entity, its display name, and whether it is a player
/// (so the `vict_*` line can reach it).
struct Target {
    entity: Entity,
    name: String,
    is_player: bool,
    /// True when the target resolved through the item fallback rather than
    /// `get_char_vis`, so the object strings apply (`do_action`).
    is_item: bool,
}

/// Resolve `needle` against the players and NPCs in `room`, then (Circle's
/// fallback order, `do_action`) against the actor's carried items and the room
/// floor. Items are returned with their display name so `$p`/`$P` can render.
#[allow(clippy::type_complexity)]
fn find_target(
    needle: &str,
    session: Entity,
    room: Entity,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
    carried: &Query<(Entity, &Item, &CarriedBy)>,
    ground: &Query<(Entity, &Item, &Location)>,
) -> Option<Target> {
    if needle.trim().is_empty() {
        return None;
    }
    if let Some((entity, player, _)) = players
        .iter()
        .find(|(_, player, location)| location.room == room && name_matches(needle, &player.name))
    {
        return Some(Target {
            entity,
            name: player.name.clone(),
            is_player: true,
            is_item: false,
        });
    }
    if let Some((entity, npc, _)) = npcs
        .iter()
        .find(|(_, npc, location)| location.room == room && name_matches(needle, &npc.name))
    {
        return Some(Target {
            entity,
            name: npc.name.clone(),
            is_player: false,
            is_item: false,
        });
    }
    if let Some((entity, item, _)) = carried
        .iter()
        .find(|(_, item, holder)| holder.0 == session && name_matches(needle, &item.name))
    {
        return Some(Target {
            entity,
            name: item.name.clone(),
            is_player: false,
            is_item: true,
        });
    }
    ground
        .iter()
        .find(|(_, item, location)| location.room == room && name_matches(needle, &item.name))
        .map(|(entity, item, _)| Target {
            entity,
            name: item.name.clone(),
            is_player: false,
            is_item: true,
        })
}

/// Send one line to every player in `room` except `session` and `victim`.
fn broadcast_except(
    players: &Query<(Entity, &Player, &Location)>,
    room: Entity,
    session: Entity,
    victim: Option<Entity>,
    output: &mut MessageWriter<OutputEvent>,
    body: &str,
) {
    for (entity, _, location) in players.iter() {
        if entity != session && Some(entity) != victim && location.room == room {
            output.write(text(entity, body));
        }
    }
}

/// One social activation: resolve the target, apply the gates, and render the
/// actor/room/victim lines.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
/// A social the plan resolved: the message pair to say and the names to
/// substitute into it.
///
/// The plan decides *what* is said — which pair, with which names — and the
/// act only says it. That keeps the branch order (no argument, the auto pair,
/// not found, the body-part refusal, the found pairs) in one place.
struct SocialPlan {
    /// The line for the actor, if any.
    char_line: Option<String>,
    /// The line for the room, if any.
    others_line: Option<String>,
    /// The actor's name.
    actor: String,
    /// The target's name, when the social named one.
    target: Option<String>,
    /// The object name, when the target resolved through the item fallback.
    object: Option<String>,
    /// The body part, when the social named one.
    body: Option<String>,
    /// The line for the victim, if any.
    victim_line: Option<String>,
    /// The room to broadcast to.
    room: Entity,
    /// The victim to leave out of the broadcast.
    victim: Option<Entity>,
}

/// What a social command resolved to.
enum SocialOutcome {
    /// A refusal, with the line to say.
    Refused(String),
    /// The social to perform.
    Perform(SocialPlan),
}

/// Resolve a social command into the message pair it would say.
#[allow(clippy::too_many_arguments)]
fn plan_social(
    session: Entity,
    name: &str,
    target_needle: &str,
    part: &str,
    socials: &std::collections::BTreeMap<smol_str::SmolStr, crate::content::TbaSocialDef>,
    players: &Query<(Entity, &Player, &Location)>,
    npcs: &Query<(Entity, &Npc, &Location)>,
    combatants: &Query<&TbaCombatant>,
    progress: &Query<&TbaProgress>,
    carried: &Query<(Entity, &Item, &CarriedBy)>,
    ground: &Query<(Entity, &Item, &Location)>,
) -> Option<SocialOutcome> {
    let Some(social) = socials.get(name).or_else(|| {
        socials
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, s)| s)
    }) else {
        return Some(SocialOutcome::Refused(MSG_UNSUPPORTED.to_owned()));
    };
    let room = actor_room(players, session)?;
    let actor = actor_name(players, session);

    let target = find_target(target_needle, session, room, players, npcs, carried, ground);
    let victim_position = target.as_ref().map(|t| position_of(combatants, t.entity));
    if let Some(reason) = gate_reason(
        social,
        position_of(combatants, session),
        level_of(progress, session),
        victim_position,
    ) {
        return Some(SocialOutcome::Refused(reason.to_owned()));
    }

    let body = (!part.is_empty()).then(|| part.to_owned());

    // No argument: the no-arg pair.
    if target_needle.trim().is_empty() {
        return Some(SocialOutcome::Perform(SocialPlan {
            char_line: social.char_no_arg.clone(),
            others_line: social.others_no_arg.clone(),
            actor,
            target: None,
            object: None,
            body: None,
            victim_line: None,
            room,
            victim: None,
        }));
    }

    // The target is the actor: the auto pair.
    if let Some(target) = &target
        && target.entity == session
    {
        return Some(SocialOutcome::Perform(SocialPlan {
            char_line: Some(
                social
                    .char_auto
                    .clone()
                    .unwrap_or_else(|| MSG_DEFAULT_AUTO.to_owned()),
            ),
            others_line: social.others_auto.clone(),
            actor: actor.clone(),
            target: Some(actor),
            object: None,
            body,
            victim_line: None,
            room,
            victim: None,
        }));
    }

    let Some(target) = target else {
        return Some(SocialOutcome::Perform(SocialPlan {
            char_line: Some(
                social
                    .not_found
                    .clone()
                    .unwrap_or_else(|| MSG_DEFAULT_NOT_FOUND.to_owned()),
            ),
            others_line: None,
            actor,
            target: None,
            object: None,
            body: None,
            victim_line: None,
            room,
            victim: None,
        }));
    };

    // A body part needs the body strings; Circle refuses otherwise.
    if !part.is_empty() && social.char_body_found.is_none() {
        return Some(SocialOutcome::Refused(MSG_NO_BODY_PART.to_owned()));
    }

    // An object target only has the obj strings; a char/object target always
    // has the plain found strings in the source data.
    let victim = target.is_player.then_some(target.entity);
    let (char_line, others_line, victim_line) = if !part.is_empty() {
        (
            social.char_body_found.clone(),
            social.others_body_found.clone(),
            social.vict_body_found.clone(),
        )
    } else if !target.is_item {
        (
            social
                .char_found
                .clone()
                .or_else(|| Some(MSG_DEFAULT_AUTO.to_owned())),
            social.others_found.clone(),
            social.vict_found.clone(),
        )
    } else {
        (
            social.char_obj_found.clone(),
            social.others_obj_found.clone(),
            None,
        )
    };
    Some(SocialOutcome::Perform(SocialPlan {
        char_line,
        others_line,
        victim_line,
        actor,
        target: Some(target.name.clone()),
        object: Some(target.name.clone()),
        body,
        room,
        victim,
    }))
}

/// Say a resolved social.
fn apply_social_plan(
    session: Entity,
    plan: &SocialPlan,
    players: &Query<(Entity, &Player, &Location)>,
    output: &mut MessageWriter<OutputEvent>,
) {
    let names = SocialNames {
        actor: &plan.actor,
        target: plan.target.as_deref(),
        object: plan.object.as_deref(),
        body: plan.body.as_deref(),
    };
    if let Some(line) = &plan.char_line {
        output.write(text(session, substitute(line, &names)));
    }
    if let Some(line) = &plan.others_line {
        broadcast_except(
            players,
            plan.room,
            session,
            plan.victim,
            output,
            &substitute(line, &names),
        );
    }
    if let (Some(entity), Some(line)) = (plan.victim, &plan.victim_line) {
        output.write(text(entity, substitute(line, &names)));
    }
    write_prompt(output, session);
}

/// The `socials` listing: every social name, sorted.
fn list_socials(
    session: Entity,
    socials: &std::collections::BTreeMap<smol_str::SmolStr, crate::content::TbaSocialDef>,
    output: &mut MessageWriter<OutputEvent>,
) {
    if socials.is_empty() {
        output.write(text(session, "There are no socials available.".to_owned()));
        write_prompt(output, session);
        return;
    }
    let mut names: Vec<&str> = socials.keys().map(|name| name.as_str()).collect();
    names.sort_unstable();
    output.write(text(
        session,
        format!(
            "The following {} socials are available:\n{}",
            names.len(),
            names.join("  ")
        ),
    ));
    write_prompt(output, session);
}

/// `BridgeSet::Game`: translate `social`/`socials` raw lines into emotes.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_social_commands(
    mut reader: MessageReader<CommandReceived>,
    claimed: Option<Res<DgClaimed>>,
    actors: ActorView<'_, '_>,
    npcs: Query<(Entity, &Npc, &Location)>,
    combatants: Query<&TbaCombatant>,
    carried: Query<(Entity, &Item, &CarriedBy)>,
    ground: Query<(Entity, &Item, &Location)>,
    content: Option<Res<TbaContent>>,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    // The view is this system's own access; the helpers below keep taking the
    // individual queries they need.
    let ActorView {
        sessions,
        players,
        progress,
        carried: _,
    } = &actors;

    for msg in reader.read() {
        let session = msg.session;
        let playing = sessions
            .get(session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        if is_claimed(claimed.as_deref(), msg.id) {
            continue;
        }
        let PlayerInput::RawLine(line) = &msg.input else {
            continue;
        };
        let Some(command) = parse_social_line(line) else {
            continue;
        };
        match command {
            SocialCommand::List => list_socials(session, &content.socials, &mut output),
            SocialCommand::Act { name, target, part } => {
                // The plan decides which message pair to say; the act says it.
                let Some(outcome) = plan_social(
                    session,
                    &name,
                    &target,
                    &part,
                    &content.socials,
                    players,
                    &npcs,
                    &combatants,
                    progress,
                    &carried,
                    &ground,
                ) else {
                    continue;
                };
                match outcome {
                    SocialOutcome::Refused(line) => {
                        output.write(text(session, line));
                        write_prompt(&mut output, session);
                    }
                    SocialOutcome::Perform(plan) => {
                        apply_social_plan(session, &plan, players, &mut output);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn social() -> crate::content::TbaSocialDef {
        crate::content::TbaSocialDef {
            sort_as: smol_str::SmolStr::new("wave"),
            hide: false,
            min_char_position: 5,
            min_victim_position: 0,
            min_level_char: 1,
            char_no_arg: Some("You wave.".to_owned()),
            others_no_arg: Some("$n waves.".to_owned()),
            char_found: Some("You wave at $N.".to_owned()),
            others_found: Some("$n waves at $N.".to_owned()),
            vict_found: Some("$n waves at you.".to_owned()),
            not_found: Some("Wave at who?".to_owned()),
            char_auto: Some("You wave at yourself.".to_owned()),
            others_auto: Some("$n waves at $mself.".to_owned()),
            char_body_found: None,
            others_body_found: None,
            vict_body_found: None,
            char_obj_found: None,
            others_obj_found: None,
        }
    }

    #[test]
    fn parses_the_social_verbs() {
        assert_eq!(parse_social_line("socials"), Some(SocialCommand::List));
        assert_eq!(
            parse_social_line("social wave"),
            Some(SocialCommand::Act {
                name: "wave".to_owned(),
                target: String::new(),
                part: String::new(),
            })
        );
        assert_eq!(
            parse_social_line("SOCIAL wave bob face"),
            Some(SocialCommand::Act {
                name: "wave".to_owned(),
                target: "bob".to_owned(),
                part: "face".to_owned(),
            })
        );
        assert_eq!(parse_social_line("list"), None);
    }

    #[test]
    fn substitutes_names_pronouns_and_case_switches() {
        let names = SocialNames {
            actor: "Bob",
            target: Some("Alice"),
            object: Some("a rose"),
            body: Some("nose"),
        };
        assert_eq!(substitute("$n waves at $N.", &names), "Bob waves at Alice.");
        // MIB has no sex; neutral singular they/them/their is the documented
        // rendering for $e/$m/$s (and $E/$M/$S).
        assert_eq!(
            substitute("$n sighs to $mself; $E is $s friend.", &names),
            "Bob sighs to themself; they is their friend."
        );
        assert_eq!(substitute("$n waves $p.", &names), "Bob waves a rose.");
        assert_eq!(
            substitute("$n waves at $S $t.", &names),
            "Bob waves at their nose."
        );
        // `$$` is a literal `$`; an unknown code (the source's stray `$i`)
        // expands to nothing, matching `perform_act`'s default branch.
        assert_eq!(
            substitute("cost $$5, $isilence", &names),
            "cost $5, silence"
        );
        assert_eq!(substitute("$Uhello", &names), "Hello");
        assert_eq!(substitute("hello world$u", &names), "hello World");
    }

    #[test]
    fn sleeping_actor_and_low_level_are_gated() {
        let def = social();
        assert_eq!(
            gate_reason(&def, Position::Sleeping, 10, None),
            Some(MSG_BAD_POSITION),
            "min_char_position 5 (resting) blocks a sleeping actor"
        );
        assert_eq!(gate_reason(&def, Position::Resting, 10, None), None);
        assert_eq!(
            gate_reason(&def, Position::Standing, 0, None),
            Some(MSG_BAD_LEVEL),
            "min_level_char 1 blocks a level-0 actor"
        );

        let mut victim_gate = social();
        victim_gate.min_victim_position = 6;
        assert_eq!(
            gate_reason(
                &victim_gate,
                Position::Standing,
                10,
                Some(Position::Sleeping)
            ),
            Some(MSG_BAD_VICTIM_POSITION)
        );
        assert_eq!(
            gate_reason(
                &victim_gate,
                Position::Standing,
                10,
                Some(Position::Sitting)
            ),
            None
        );
    }

    #[test]
    fn every_source_position_number_maps_and_unknown_disables_the_gate() {
        assert_eq!(position_from_i32(-1), None);
        assert_eq!(position_from_i32(0), Some(Position::Dead));
        assert_eq!(position_from_i32(8), Some(Position::Standing));
        assert_eq!(position_from_i32(99), None);
    }
}
