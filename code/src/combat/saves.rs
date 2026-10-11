//! CircleMUD/tbaMUD saving throws (TBA-E18).
//!
//! `mag_savingthrow()` (`tbamud/src/magic.c:36-53`) resolves a spell by adding
//! the victim's class/level base throw to the `apply_saving_throw[]` modifiers
//! (the `TbaSaves` block from TBA-E18/#6) and comparing the result against a
//! 0..99 roll:
//!
//! ```text
//! save = saving_throws(class, type, GET_LEVEL(victim)) + GET_SAVE(victim, type)
//! if (MAX(1, save) < rand_number(0, 99))  ->  saved
//! ```
//!
//! A **lower** total is a **better** save: the `saving_throws` tables fall with
//! level and a negative `apply_saving_throw[]` modifier improves the throw, so
//! `MAX(1, save)` is the smallest value the 0..99 roll must exceed.
//!
//! The `saving_throws()` tables (`tbamud/src/class.c:160-1250`) are transcribed
//! verbatim, all five categories and all four classes (levels 0..40, plus the
//! warrior's 0..50 rows). NPCs use the warrior column, exactly as
//! `mag_savingthrow` does (`class_sav = CLASS_WARRIOR`).
//!
//! `cast` always passes `SAVING_SPELL`: `call_magic` maps `CAST_SPELL` to
//! `savetype = SAVING_SPELL` (`tbamud/src/spell_parser.c:205-222`).

use crate::combat::model::{SaveType, TbaSaves};
use crate::combat::progression::TbaRng;

/// `saving_throws()` (`tbamud/src/class.c:160`): the class/level base throw for
/// one category, in `SAVING_*` order (`Para, Rod, Petri, Breath, Spell`).
///
/// `class` is the `tba.classes` key (`magic_user`, `cleric`, `thief`,
/// `warrior`); anything else — including an NPC, which has no class — falls
/// back to the warrior table, matching `mag_savingthrow`'s `IS_NPC` branch. The
/// level clamps to the table's final row.
pub fn saving_throws(class: &str, save: SaveType, level: u32) -> i32 {
    let table: &[&[i32]; 5] = match class {
        "magic_user" => &MAGE,
        "cleric" => &CLERIC,
        "thief" => &THIEF,
        _ => &WARRIOR,
    };
    let column = table[save.index()];
    column[(level as usize).min(column.len() - 1)]
}

/// `mag_savingthrow()` (`tbamud/src/magic.c:36-53`): roll the save for a victim.
///
/// Returns `true` when the victim resists. `saves` is the victim's
/// [`TbaSaves`] affect modifiers; `save` is the category (`SAVING_SPELL` for a
/// `cast`). The roll is `rand_number(0, 99)` through [`TbaRng`], so a seeded
/// replay is reproducible.
pub fn mag_savingthrow(
    rng: &mut TbaRng,
    class: &str,
    level: u32,
    saves: &TbaSaves,
    save: SaveType,
) -> bool {
    let value = (saving_throws(class, save, level) + saves.get(save)).max(1);
    // `MAX(1, save) < rand_number(0, 99)`; strict, so an equal value is a
    // failure to save.
    value < rng.range(0, 99)
}

const MAGE: [&[i32]; 5] = [
    &[
        90, 70, 69, 68, 67, 66, 65, 63, 61, 60, 59, 57, 55, 54, 53, 53, 52, 51, 50, 48, 46, 45, 44,
        42, 40, 38, 36, 34, 32, 30, 28, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 55, 53, 51, 49, 47, 45, 43, 41, 40, 39, 37, 35, 33, 31, 30, 29, 27, 25, 23, 21, 20, 19,
        17, 15, 14, 13, 12, 11, 10, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 65, 63, 61, 59, 57, 55, 53, 51, 50, 49, 47, 45, 43, 41, 40, 39, 37, 35, 33, 31, 30, 29,
        27, 25, 23, 21, 19, 17, 15, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 75, 73, 71, 69, 67, 65, 63, 61, 60, 59, 57, 55, 53, 51, 50, 49, 47, 45, 43, 41, 40, 39,
        37, 35, 33, 31, 29, 27, 25, 23, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 60, 58, 56, 54, 52, 50, 48, 46, 45, 44, 42, 40, 38, 36, 35, 34, 32, 30, 28, 26, 25, 24,
        22, 20, 18, 16, 14, 12, 10, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
];

const CLERIC: [&[i32]; 5] = [
    &[
        90, 60, 59, 48, 46, 45, 43, 40, 37, 35, 34, 33, 31, 30, 29, 27, 26, 25, 24, 23, 22, 21, 20,
        18, 15, 14, 12, 10, 9, 8, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 70, 69, 68, 66, 65, 63, 60, 57, 55, 54, 53, 51, 50, 49, 47, 46, 45, 44, 43, 42, 41, 40,
        38, 35, 34, 32, 30, 29, 28, 27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 65, 64, 63, 61, 60, 58, 55, 53, 50, 49, 48, 46, 45, 44, 43, 41, 40, 39, 38, 37, 36, 35,
        33, 31, 29, 27, 25, 24, 23, 22, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 80, 79, 78, 76, 75, 73, 70, 67, 65, 64, 63, 61, 60, 59, 57, 56, 55, 54, 53, 52, 51, 50,
        48, 45, 44, 42, 40, 39, 38, 37, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 75, 74, 73, 71, 70, 68, 65, 63, 60, 59, 58, 56, 55, 54, 53, 51, 50, 49, 48, 47, 46, 45,
        43, 41, 39, 37, 35, 34, 33, 32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
];

const THIEF: [&[i32]; 5] = [
    &[
        90, 65, 64, 63, 62, 61, 60, 59, 58, 57, 56, 55, 54, 53, 52, 51, 50, 49, 48, 47, 46, 45, 44,
        43, 42, 41, 40, 39, 38, 37, 36, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 70, 68, 66, 64, 62, 60, 58, 56, 54, 52, 50, 48, 46, 44, 42, 40, 38, 36, 34, 32, 30, 28,
        26, 24, 22, 20, 18, 16, 14, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 60, 59, 58, 58, 56, 55, 54, 53, 52, 51, 50, 49, 48, 47, 46, 45, 44, 43, 42, 41, 40, 39,
        38, 37, 36, 35, 34, 33, 32, 31, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 80, 79, 78, 77, 76, 75, 74, 73, 72, 71, 70, 69, 68, 67, 66, 65, 64, 63, 62, 61, 60, 59,
        58, 57, 56, 55, 54, 53, 52, 51, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    &[
        90, 75, 73, 71, 69, 67, 65, 63, 61, 59, 57, 55, 53, 51, 49, 47, 45, 43, 41, 39, 37, 35, 33,
        31, 29, 27, 25, 23, 21, 19, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
];

const WARRIOR: [&[i32]; 5] = [
    &[
        90, 70, 68, 67, 65, 62, 58, 55, 53, 52, 50, 47, 43, 40, 38, 37, 35, 32, 28, 25, 24, 23, 22,
        20, 19, 17, 16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0,
        0,
    ],
    &[
        90, 80, 78, 77, 75, 72, 68, 65, 63, 62, 60, 57, 53, 50, 48, 47, 45, 42, 38, 35, 34, 33, 32,
        30, 29, 27, 26, 25, 24, 23, 22, 20, 18, 16, 14, 12, 10, 8, 6, 5, 4, 3, 2, 1, 0, 0, 0, 0, 0,
        0, 0,
    ],
    &[
        90, 75, 73, 72, 70, 67, 63, 60, 58, 57, 55, 52, 48, 45, 43, 42, 40, 37, 33, 30, 29, 28, 26,
        25, 24, 23, 21, 20, 19, 18, 17, 16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0,
        0, 0, 0,
    ],
    &[
        90, 85, 83, 82, 80, 75, 70, 65, 63, 62, 60, 55, 50, 45, 43, 42, 40, 37, 33, 30, 29, 28, 26,
        25, 24, 23, 21, 20, 19, 18, 17, 16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0,
        0, 0, 0,
    ],
    &[
        90, 85, 83, 82, 80, 77, 73, 70, 68, 67, 65, 62, 58, 55, 53, 52, 50, 47, 43, 40, 39, 38, 36,
        35, 34, 33, 31, 30, 29, 28, 27, 25, 23, 21, 19, 17, 15, 13, 11, 9, 7, 6, 5, 4, 3, 2, 1, 0,
        0, 0, 0,
    ],
];
