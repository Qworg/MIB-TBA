//! Id construction, matching the Phase 9.1 conventions:
//!
//! - rooms: `tba.<vnum>` (dotted, keyed by the raw source vnum as `num`)
//! - NPCs:  `tba_npc_<vnum>`
//! - items: `tba_item_<vnum>`
//! - areas: `tba_<zone vnum>` (one map region per source zone)
//!
//! The room vnum also rides in [`mib_stdlib::content::RoomDef::num`] so the
//! zone `[bot, top]` bounds can still be recovered.

use mib_stdlib::content::{AreaId, ItemId, NpcId, QuestId, RoomId};

/// `tba.<vnum>`
pub fn room_id(vnum: i32) -> RoomId {
    RoomId::from(format!("tba.{vnum}"))
}

/// `tba_npc_<vnum>`
pub fn npc_id(vnum: i32) -> NpcId {
    NpcId::from(format!("tba_npc_{vnum}"))
}

/// `tba_item_<vnum>`
pub fn item_id(vnum: i32) -> ItemId {
    ItemId::from(format!("tba_item_{vnum}"))
}

/// `tba_quest_<vnum>`
pub fn quest_id(vnum: i32) -> QuestId {
    QuestId::from(format!("tba_quest_{vnum}"))
}

/// `tba_<zone>`
pub fn area_id(zone: u32) -> AreaId {
    AreaId::from(format!("tba_{zone}"))
}
