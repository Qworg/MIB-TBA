//! Mobile special-procedure assignments, transcribed from tbaMUD's C.
//!
//! `MOB_SPEC` is only a marker bit; the function pointer comes from
//! `spec_assign.c::assign_mobiles` and `castle.c::assign_kings_castle`. The
//! converter reads neither, so the tables are copied here. The source line
//! comments name the assignment block each slice mirrors.
//!
//! The castle assignments are zone-relative (`castle_virtual(offset)`, zone
//! 150's bottom vnum + offset); the pack's zone 150 is `15000..15099`.

use mib_tba::content::SpecProc;

/// `assign_mobiles()` (`spec_assign.c:68-108`), 39 mobs.
const GUILD: &[i32] = &[
    120, 121, 122, 123, 2556, 2559, 2562, 2564, 2800, 3020, 3021, 3022, 3023, 5400, 5401, 5402,
    5403, 11518, 25720, 25721, 25722, 25723, 25726, 25732, 27572, 27573, 27574, 27575, 27721,
    29204, 29227, 31601, 31603, 31605, 31607, 31609, 31611, 31639, 31641,
];

/// `ASSIGNMOB(3105, mayor)` (`spec_assign.c:110`).
const MAYOR: &[i32] = &[3105];

/// `ASSIGNMOB(..., postmaster)` (`spec_assign.c:112-120`), 9 mobs.
const POSTMASTER: &[i32] = &[110, 1201, 3010, 10412, 10719, 25710, 27164, 30128, 31510];

/// `ASSIGNMOB(..., receptionist)` (`spec_assign.c:122-126`), 5 mobs.
const RECEPTIONIST: &[i32] = &[1200, 3005, 5404, 27713, 27730];

/// `ASSIGNMOB(3095, cryogenicist)` (`spec_assign.c:68`).
const CRYOGENICIST: &[i32] = &[3095];

/// Zone 150's bottom vnum (`Z_KINGS_C`), the base for the castle offsets.
const CASTLE_BOT: i32 = 15000;

/// `assign_kings_castle()` (`castle.c:103-129`) by offset from the zone bottom.
fn castle_proc(offset: i32) -> Option<SpecProc> {
    Some(match offset {
        // Gwydion, Jim, Brian, Mick, Matt, Jochem, Anne, Andrew, Bertram,
        // Jeanette, Michael, Hans and Boris.
        0 | 3..=11 | 27..=29 => SpecProc::CastleGuard,
        1 => SpecProc::KingWelmar,
        12 => SpecProc::Peter,
        13 => SpecProc::TrainingMaster,
        16 => SpecProc::James,
        17 => SpecProc::Cleaning,
        20 => SpecProc::Tim,
        21 => SpecProc::Tom,
        24 | 25 => SpecProc::DicknDavid,
        26 => SpecProc::Jerry,
        _ => return None,
    })
}

/// The procedure assigned to `vnum`, or `None` for an unassigned mob.
///
/// A `MOB_SPEC`-flagged mob is not necessarily assigned one (the Midgaard
/// shopkeepers `3000`-`3009` carry the stale flag), and most assigned mobs do
/// not carry the flag.
pub(super) fn spec_proc_for(vnum: i32) -> Option<SpecProc> {
    if GUILD.contains(&vnum) {
        return Some(SpecProc::Guild);
    }
    if MAYOR.contains(&vnum) {
        return Some(SpecProc::Mayor);
    }
    if POSTMASTER.contains(&vnum) {
        return Some(SpecProc::Postmaster);
    }
    if RECEPTIONIST.contains(&vnum) {
        return Some(SpecProc::Receptionist);
    }
    if CRYOGENICIST.contains(&vnum) {
        return Some(SpecProc::Cryogenicist);
    }
    if (CASTLE_BOT..CASTLE_BOT + 30).contains(&vnum) {
        return castle_proc(vnum - CASTLE_BOT);
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn every_assignment_block_is_covered() {
        // 39 guild + 1 mayor + 9 postmaster + 5 receptionist + 1 cryogenicist
        // + 22 castle = 78 assigned mobs.
        let assignments: usize = (0..=40000)
            .filter(|vnum| spec_proc_for(*vnum).is_some())
            .count();
        assert_eq!(assignments, 78);
    }

    #[test]
    fn the_stale_mob_spec_flag_does_not_imply_an_assignment() {
        for shopkeeper in [3000, 3001, 3002, 3003, 3004, 3006, 3008, 3009] {
            assert_eq!(
                spec_proc_for(shopkeeper),
                None,
                "{shopkeeper} carries the flag but has no procedure"
            );
        }
    }

    #[test]
    fn assignments_match_the_source_tables() {
        assert_eq!(spec_proc_for(120), Some(SpecProc::Guild));
        assert_eq!(spec_proc_for(3020), Some(SpecProc::Guild));
        assert_eq!(spec_proc_for(110), Some(SpecProc::Postmaster));
        assert_eq!(spec_proc_for(3010), Some(SpecProc::Postmaster));
        assert_eq!(spec_proc_for(1200), Some(SpecProc::Receptionist));
        assert_eq!(spec_proc_for(3095), Some(SpecProc::Cryogenicist));
        assert_eq!(spec_proc_for(3105), Some(SpecProc::Mayor));

        assert_eq!(spec_proc_for(15000), Some(SpecProc::CastleGuard));
        assert_eq!(spec_proc_for(15001), Some(SpecProc::KingWelmar));
        assert_eq!(spec_proc_for(15012), Some(SpecProc::Peter));
        assert_eq!(spec_proc_for(15013), Some(SpecProc::TrainingMaster));
        assert_eq!(spec_proc_for(15016), Some(SpecProc::James));
        assert_eq!(spec_proc_for(15017), Some(SpecProc::Cleaning));
        assert_eq!(spec_proc_for(15020), Some(SpecProc::Tim));
        assert_eq!(spec_proc_for(15021), Some(SpecProc::Tom));
        assert_eq!(spec_proc_for(15024), Some(SpecProc::DicknDavid));
        assert_eq!(spec_proc_for(15025), Some(SpecProc::DicknDavid));
        assert_eq!(spec_proc_for(15026), Some(SpecProc::Jerry));
        assert_eq!(spec_proc_for(15029), Some(SpecProc::CastleGuard));
        assert_eq!(spec_proc_for(15002), None, "no assigned mob at this offset");
    }
}
