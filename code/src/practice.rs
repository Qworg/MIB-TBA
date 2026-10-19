//! The guildmaster's `practice` command (`SPECIAL(guild)`,
//! `tbamud/src/spec_procs.c:147`).
//!
//! Circle stores a practices counter and a percentage per spell/skill. The
//! pack keeps that in [`TbaPractice`] (persisted with the player); a character
//! without the component keeps the old "fully learned" behaviour so existing
//! fixtures and scripted characters are unchanged.
//!
//! The class tables are transcribed from `class.c`/`constants.c`:
//! `prac_params` (learned level, min/max gain), `int_app[].learn` and
//! `wis_app[].bonus`.

use std::collections::BTreeMap;

use bevy::ecs::message::MessageCursor;
use bevy::prelude::*;
use mib_core::prelude::{CommandReceived, PlayerInput};
use mib_stdlib::prelude::{Location, Player};
use smol_str::SmolStr;

use crate::combat::{TbaCombatant, TbaProgress};
use crate::dg::intake::commands::{DgClaimed, claim_id, is_claimed};
use crate::dg::out::{is_playing, send_to_char};
use crate::module::TbaContent;
use crate::skills::{SKILL_LEARNED, skill_min_level};
use crate::spec::TbaSpecProc;

/// Circle's `prac_params` (`class.c:127-133`), indexed by class id.
#[derive(Debug, Clone, Copy)]
struct PracParams {
    learned: i32,
    max_gain: i32,
    min_gain: i32,
}

fn prac_params(class: &str) -> PracParams {
    match class {
        "magic_user" => PracParams {
            learned: 95,
            max_gain: 100,
            min_gain: 25,
        },
        "cleric" => PracParams {
            learned: 95,
            max_gain: 100,
            min_gain: 25,
        },
        "thief" => PracParams {
            learned: 85,
            max_gain: 12,
            min_gain: 0,
        },
        _ => PracParams {
            learned: 80,
            max_gain: 12,
            min_gain: 0,
        },
    }
}

/// `int_app[].learn` (`constants.c:736-763`), clamped to the table.
fn int_learn(intelligence: i32) -> i32 {
    const LEARN: [i32; 26] = [
        3, 5, 7, 8, 9, 10, 11, 12, 13, 15, 17, 19, 22, 25, 30, 35, 40, 45, 50, 53, 55, 56, 57, 58,
        59, 60,
    ];
    LEARN[intelligence.clamp(0, 25) as usize]
}

/// `wis_app[].bonus` (`constants.c:767-794`), clamped to the table.
fn wis_bonus(wisdom: i32) -> i32 {
    const BONUS: [i32; 26] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 3, 3, 3, 4, 5, 6, 6, 6, 6, 7, 7, 7,
    ];
    BONUS[wisdom.clamp(0, 25) as usize]
}

/// `practice_gain_percent` (`spec_procs.c:104`).
fn practice_gain_percent(class: &str, intelligence: i32) -> i32 {
    let params = prac_params(class);
    int_learn(intelligence).clamp(params.min_gain, params.max_gain)
}

/// `practices_per_level` (`class.c:1484`).
pub fn practices_per_level(class: &str, wisdom: i32) -> i32 {
    let bonus = wis_bonus(wisdom);
    if matches!(class, "magic_user" | "cleric") {
        bonus.max(2)
    } else {
        bonus.clamp(1, 2)
    }
}

/// The player's practices and per-spell/skill percentages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Component, serde::Serialize, serde::Deserialize)]
pub struct TbaPractice {
    #[serde(default)]
    pub practices: i32,
    /// Percentage by normalized spell/skill id. Absent means 0 (Circle's
    /// `GET_SKILL`), not learned.
    #[serde(default)]
    pub skills: BTreeMap<SmolStr, i32>,
}

fn normalize(name: &str) -> SmolStr {
    SmolStr::new(name.trim().to_lowercase().replace([' ', '-'], "_"))
}

impl TbaPractice {
    /// `GET_SKILL`: 0 when the character has never practiced it.
    pub fn percent(&self, skill: &str) -> i32 {
        self.skills
            .get(&normalize(skill))
            .copied()
            .unwrap_or(0)
            .clamp(0, SKILL_LEARNED)
    }

    fn set_percent(&mut self, skill: &str, percent: i32) {
        self.skills
            .insert(normalize(skill), percent.clamp(0, SKILL_LEARNED));
    }
}

/// `CharacterCreated`: every player gets the Circle practice record.
pub fn ensure_practice(
    mut created: MessageReader<mib_stdlib::prelude::CharacterCreated>,
    players: Query<Entity, (With<Player>, Without<TbaPractice>)>,
    mut commands: Commands,
) {
    for event in created.read() {
        if players.contains(event.session) {
            commands
                .entity(event.session)
                .insert(TbaPractice::default());
        }
    }
}

/// `Update`, after the router: `practice` at a guildmaster.
pub fn handle_practice_commands(
    world: &mut World,
    mut cursor: Local<MessageCursor<CommandReceived>>,
) {
    let messages: Vec<CommandReceived> = {
        let Some(messages) = world.get_resource::<Messages<CommandReceived>>() else {
            return;
        };
        cursor.read(messages).cloned().collect()
    };
    for msg in messages {
        if !is_playing(world, msg.session) || is_claimed(world.get_resource::<DgClaimed>(), msg.id)
        {
            continue;
        }
        let Some(line) = (match &msg.input {
            PlayerInput::RawLine(line) => Some(line.as_str()),
            _ => None,
        }) else {
            continue;
        };
        let line = line.trim();
        let (verb, argument) = match line.find(char::is_whitespace) {
            Some(index) => (&line[..index], line[index + 1..].trim()),
            None => (line, ""),
        };
        if !verb.eq_ignore_ascii_case("practice") {
            continue;
        }
        let Some(room) = world
            .get::<Location>(msg.session)
            .map(|location| location.room)
        else {
            continue;
        };
        let at_guild = {
            let mut query = world.query::<(&TbaSpecProc, &Location)>();
            query.iter(world).any(|(proc, location)| {
                proc.0 == crate::content::SpecProc::Guild && location.room == room
            })
        };
        if !at_guild {
            continue;
        }
        let Some((class, level, intelligence)) = world
            .get::<TbaProgress>(msg.session)
            .zip(world.get::<TbaCombatant>(msg.session))
            .map(|(progress, combatant)| {
                (
                    progress.class.clone(),
                    progress.level,
                    combatant.attributes.intelligence,
                )
            })
        else {
            continue;
        };
        let content = world
            .get_resource::<TbaContent>()
            .cloned()
            .unwrap_or_default();
        let known: Vec<SmolStr> = content
            .classes
            .get(class.as_str())
            .map(|class| class.granted_skills.clone())
            .unwrap_or_default();
        if world.get::<TbaPractice>(msg.session).is_none() {
            world.entity_mut(msg.session).insert(TbaPractice::default());
        }
        if argument.is_empty() {
            let (practices, lines) = {
                let Some(practice) = world.get::<TbaPractice>(msg.session) else {
                    continue;
                };
                let lines: Vec<String> = known
                    .iter()
                    .map(|skill| {
                        format!(
                            "{:3}%  {} (level {})\r\n",
                            practice.percent(skill),
                            skill,
                            min_level(&content, class.as_str(), skill)
                        )
                    })
                    .collect();
                (practice.practices, lines)
            };
            let mut text = if practices == 1 {
                format!("You have {practices} practice session.\r\n")
            } else {
                format!("You have {practices} practice sessions.\r\n")
            };
            for line in lines {
                text.push_str(&line);
            }
            send_to_char(world, msg.session, &text);
            claim_id(world, msg.id, msg.session);
            continue;
        }
        let target = normalize(argument);
        let Some(skill) = known
            .iter()
            .find(|skill| normalize(skill).as_str() == target.as_str())
            .cloned()
        else {
            send_to_char(
                world,
                msg.session,
                &format!(
                    "You do not know of that {}.\r\n",
                    if is_caster(class.as_str()) {
                        "spell"
                    } else {
                        "skill"
                    }
                ),
            );
            claim_id(world, msg.id, msg.session);
            continue;
        };
        if level < min_level(&content, class.as_str(), &skill) {
            send_to_char(
                world,
                msg.session,
                &format!(
                    "You do not know of that {}.\r\n",
                    if is_caster(class.as_str()) {
                        "spell"
                    } else {
                        "skill"
                    }
                ),
            );
            claim_id(world, msg.id, msg.session);
            continue;
        }
        let params = prac_params(class.as_str());
        let Some((current, practices)) = world
            .get::<TbaPractice>(msg.session)
            .map(|practice| (practice.percent(&skill), practice.practices))
        else {
            continue;
        };
        if current >= params.learned {
            send_to_char(
                world,
                msg.session,
                "You are already learned in that area.\r\n",
            );
            claim_id(world, msg.id, msg.session);
            continue;
        }
        if practices <= 0 {
            send_to_char(
                world,
                msg.session,
                "You do not seem to be able to practice now.\r\n",
            );
            claim_id(world, msg.id, msg.session);
            continue;
        }
        send_to_char(world, msg.session, "You practice for a while...\r\n");
        let gain = practice_gain_percent(class.as_str(), intelligence);
        let learned = (current + gain).min(params.learned) >= params.learned;
        if let Some(mut practice) = world.get_mut::<TbaPractice>(msg.session) {
            practice.practices -= 1;
            practice.set_percent(&skill, (current + gain).min(params.learned));
        }
        if learned {
            send_to_char(world, msg.session, "You are now learned in that area.\r\n");
        }
        claim_id(world, msg.id, msg.session);
    }
}

fn is_caster(class: &str) -> bool {
    matches!(class, "magic_user" | "cleric")
}

/// `spell_info[].min_level[class]`, using `tba.spells` for spells and
/// `tba.classes.skill_levels` for skills.
fn min_level(content: &TbaContent, class: &str, skill: &SmolStr) -> u32 {
    if let Some(spell) = content.spells.get(skill.as_str())
        && let Some(level) = spell.min_level.get(class)
    {
        return *level;
    }
    skill_min_level(content, class, skill)
}

/// Fill practice percentages for a level-up. Called by the reward pass.
pub fn award_level_practices(practice: &mut TbaPractice, class: &str, wisdom: i32) {
    practice.practices = practice
        .practices
        .saturating_add(practices_per_level(class, wisdom));
}

/// `PreRoute`: Circle's `GET_SKILL == 0` refusal for a known spell
/// (`spell_parser.c:546`). It runs before the router so the spell command is
/// claimed with the C's message; a character with no practice record keeps the
/// old fully-learned behaviour.
pub fn practice_cast_gate(world: &mut World, mut cursor: Local<MessageCursor<CommandReceived>>) {
    let messages: Vec<CommandReceived> = {
        let Some(messages) = world.get_resource::<Messages<CommandReceived>>() else {
            return;
        };
        cursor.read(messages).cloned().collect()
    };
    let content = world
        .get_resource::<TbaContent>()
        .cloned()
        .unwrap_or_default();
    for msg in messages {
        if !is_playing(world, msg.session) || is_claimed(world.get_resource::<DgClaimed>(), msg.id)
        {
            continue;
        }
        let Some(line) = (match &msg.input {
            PlayerInput::RawLine(line) => Some(line.as_str()),
            _ => None,
        }) else {
            continue;
        };
        let line = line.trim();
        if !line
            .get(..4)
            .is_some_and(|head| head.eq_ignore_ascii_case("cast"))
        {
            continue;
        }
        let rest = line[4..].trim_start();
        let Some(rest) = rest.strip_prefix('\'') else {
            continue;
        };
        let Some((spell, _)) = rest.split_once('\'') else {
            continue;
        };
        let Some(spell_id) = content
            .spells
            .keys()
            .find(|id| normalize(id).as_str() == normalize(spell).as_str())
            .cloned()
        else {
            continue;
        };
        let Some(practice) = world.get::<TbaPractice>(msg.session) else {
            continue;
        };
        if practice.percent(spell_id.as_str()) == 0 {
            send_to_char(
                world,
                msg.session,
                "You are unfamiliar with that spell.\r\n",
            );
            claim_id(world, msg.id, msg.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_tables_match_the_c_source() {
        assert_eq!(prac_params("warrior").learned, 80);
        assert_eq!(prac_params("thief").learned, 85);
        assert_eq!(prac_params("magic_user").learned, 95);
        assert_eq!(practice_gain_percent("warrior", 25), 12);
        assert_eq!(practice_gain_percent("magic_user", 10), 25);
        assert_eq!(practices_per_level("warrior", 25), 2);
        assert_eq!(practices_per_level("magic_user", 10), 2);
    }

    #[test]
    fn a_missing_skill_reads_zero_and_is_clamped() {
        let mut practice = TbaPractice::default();
        assert_eq!(practice.percent("kick"), 0);
        practice.set_percent("kick", 200);
        assert_eq!(practice.percent("kick"), SKILL_LEARNED);
    }
}
