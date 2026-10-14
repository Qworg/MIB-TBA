//! The DG spell numbers (`spells.h`) of the pack's spell ids.
//!
//! DG scripts name a spell by its `spell_info[]` slot (`%spell%`) and compare
//! `%spellname%`, which the host resolves from the number. The pack keys its
//! catalog (`tba.spells`) by id; the ids are the lowercased `SPELL_*` names of
//! `spells.h`, so the table below is that header's numbering.

/// The `spells.h` number of a pack spell id (`magic_missile` is 32), or `None`
/// for an id outside the table.
pub fn spell_number(id: &str) -> Option<i32> {
    SPELLS
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, number)| *number)
}

/// Pack spell id and `spells.h` number, in number order.
const SPELLS: &[(&str, i32)] = &[
    ("armor", 1),
    ("teleport", 2),
    ("bless", 3),
    ("blindness", 4),
    ("burning_hands", 5),
    ("call_lightning", 6),
    ("charm", 7),
    ("chill_touch", 8),
    ("clone", 9),
    ("color_spray", 10),
    ("control_weather", 11),
    ("create_food", 12),
    ("create_water", 13),
    ("cure_blind", 14),
    ("cure_critic", 15),
    ("cure_light", 16),
    ("curse", 17),
    ("detect_align", 18),
    ("detect_invis", 19),
    ("detect_magic", 20),
    ("detect_poison", 21),
    ("dispel_evil", 22),
    ("earthquake", 23),
    ("enchant_weapon", 24),
    ("energy_drain", 25),
    ("fireball", 26),
    ("harm", 27),
    ("heal", 28),
    ("invisibility", 29),
    ("lightning_bolt", 30),
    ("locate_object", 31),
    ("magic_missile", 32),
    ("poison", 33),
    ("protection_from_evil", 34),
    ("remove_curse", 35),
    ("sanctuary", 36),
    ("shocking_grasp", 37),
    ("sleep", 38),
    ("strength", 39),
    ("summon", 40),
    ("ventriloquate", 41),
    ("word_of_recall", 42),
    ("remove_poison", 43),
    ("sense_life", 44),
    ("animate_dead", 45),
    ("dispel_good", 46),
    ("group_armor", 47),
    ("group_heal", 48),
    ("group_recall", 49),
    ("infravision", 50),
    ("waterwalk", 51),
    ("identify", 52),
    ("fly", 53),
    ("darkness", 54),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_spells_h() {
        assert_eq!(spell_number("armor"), Some(1));
        assert_eq!(spell_number("magic_missile"), Some(32));
        assert_eq!(spell_number("darkness"), Some(54));
        assert_eq!(spell_number("not_a_spell"), None);
    }

    #[test]
    fn numbers_are_unique_and_ascending() {
        assert!(SPELLS.windows(2).all(|pair| pair[0].1 + 1 == pair[1].1));
    }
}
