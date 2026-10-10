//! TBA-E28 — the 18-slot equipment model and its affect layer.
//!
//! CircleMUD/TBA wears objects in a fixed table of positions
//! ([`WearSlot`]), and a worn object contributes a list of `APPLY_*` affects
//! ([`AffectDef`]) to its wearer. This module owns three things:
//!
//! * [`TbaEquipment`] — the **runtime** worn map (`WearSlot → Entity`). An
//!   `Entity` is generational and must never be persisted, so this component is
//!   deliberately not serialisable.
//! * [`TbaEquipmentSave`] — the **persisted** companion (`WearSlot → SaveId`
//!   string). It is registered with the stdlib
//!   [`SaveRegistry`](mib_stdlib::prelude::SaveRegistry) under
//!   [`DocKind::Player`], so the worn set survives a restart.
//! * [`restore_equipment_from_save`] — the relink pass that rebuilds the
//!   runtime map from the saved ids once the stdlib has linked the item
//!   entities, dropping (and logging) any id that no longer resolves.
//!
//! ## Affect recompute — no stacking
//!
//! [`recompute_equipment_affects`] rebuilds a player's [`TbaCombatant`] from
//! [`derive_vitals`] plus the sum of every equipped item's affects. The
//! previous combatant is *never* added to: the base block is regenerated and
//! the current hp/mana/move are clamped into the new maxima, so equipping and
//! unequipping cannot stack (the classic-rpg lesson) and a recompute never
//! heals.
//!
//! [`AffectLocation::Str`]/`Dex`/`Int`/`Wis`/`Con`/`Cha` and the
//! `SavePara`/`SaveRod`/`SavePetri`/`SaveBreath`/`SaveSpell` saves have no
//! field on [`TbaCombatant`]; they parse and persist with the item but are
//! **carried-but-not-yet-applied** until a stat/save block exists. See
//! [`apply_affect`].
//!
//! ## `WieldedWeapon`
//!
//! [`sync_wielded_weapon`] keeps [`WieldedWeapon`] present exactly when the
//! [`WearSlot::Wield`] position holds an item whose `tba.items` `kind` is
//! [`TbaItemKind::Weapon`]. `weapon_dice` then reads the Circle-native
//! `values = [unused, dice_count, dice_sides, bonus]`.

use std::collections::{BTreeMap, HashMap};

use mib_core::prelude::*;
use mib_stdlib::dialogue::write_prompt;
use mib_stdlib::prelude::{CarriedBy, Item, ItemId, Player, SaveId, Session, SessionState};
use serde::{Deserialize, Serialize};

use crate::combat::{
    TbaCombatant, TbaNpcBase, TbaProgress, WieldedWeapon, default_class, derive_vitals,
};
use crate::content::{AffectDef, AffectLocation, TbaItemDef, TbaItemKind, WearSlot};
use crate::module::TbaContent;
use crate::spells::TbaBuffs;

/// The runtime worn state: each occupied Circle wear position and the item
/// entity sitting in it.
///
/// This is a [`Component`] but **not** serialisable — it holds `Entity`, which
/// is generational. [`TbaEquipmentSave`] is its persisted shadow; the two are
/// kept in sync by [`handle_equipment_commands`] and rebuilt from the save by
/// [`restore_equipment_from_save`].
#[derive(Debug, Component, Default)]
pub struct TbaEquipment {
    pub slots: HashMap<WearSlot, Entity>,
}

impl TbaEquipment {
    /// The item worn in `slot`, if any.
    pub fn worn(&self, slot: WearSlot) -> Option<Entity> {
        self.slots.get(&slot).copied()
    }
}

/// The persisted shadow of [`TbaEquipment`]: each occupied wear position and
/// the [`SaveId`] of the item in it.
///
/// Registered with the stdlib `SaveRegistry` under [`DocKind::Player`], so a
/// player's worn set rides in their character bundle and survives a restart.
/// `SaveId → Entity` is resolved back at login by
/// [`restore_equipment_from_save`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Component, Serialize, Deserialize)]
pub struct TbaEquipmentSave {
    /// `WearSlot → item SaveId`, in slot order.
    #[serde(default)]
    pub worn: BTreeMap<WearSlot, String>,
}

// -- wear-slot vocabulary ---------------------------------------------------

impl WearSlot {
    /// The other half of a paired position (`FingerR`/`FingerL`,
    /// `Neck1`/`Neck2`, `WristR`/`WristL`). Unpaired slots return `None`.
    pub fn partner(self) -> Option<WearSlot> {
        match self {
            WearSlot::FingerR => Some(WearSlot::FingerL),
            WearSlot::FingerL => Some(WearSlot::FingerR),
            WearSlot::Neck1 => Some(WearSlot::Neck2),
            WearSlot::Neck2 => Some(WearSlot::Neck1),
            WearSlot::WristR => Some(WearSlot::WristL),
            WearSlot::WristL => Some(WearSlot::WristR),
            _ => None,
        }
    }

    /// Whether this position is one half of a pair.
    pub fn is_paired(self) -> bool {
        self.partner().is_some()
    }

    /// A human-readable name for command output.
    pub fn display_name(self) -> &'static str {
        match self {
            WearSlot::Light => "light",
            WearSlot::FingerR => "right finger",
            WearSlot::FingerL => "left finger",
            WearSlot::Neck1 => "first neck",
            WearSlot::Neck2 => "second neck",
            WearSlot::Body => "body",
            WearSlot::Head => "head",
            WearSlot::Legs => "legs",
            WearSlot::Feet => "feet",
            WearSlot::Hands => "hands",
            WearSlot::Arms => "arms",
            WearSlot::Shield => "shield",
            WearSlot::About => "about body",
            WearSlot::Waist => "waist",
            WearSlot::WristR => "right wrist",
            WearSlot::WristL => "left wrist",
            WearSlot::Wield => "wield",
            WearSlot::Hold => "hold",
        }
    }

    /// Parse a lenient `unequip` slot word: `wield`, `wielded`, `main hand`,
    /// `finger r`, `right wrist`, `neck 2`, … Case, whitespace, `_` and `-`
    /// are all folded, and the bare group name (`finger`, `neck`, `wrist`)
    /// resolves to the right-hand / first position.
    pub fn parse(word: &str) -> Option<WearSlot> {
        let normalized: String = word
            .to_lowercase()
            .chars()
            .map(|c| if c == '_' || c == '-' { ' ' } else { c })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        match normalized.as_str() {
            "light" | "lantern" => Some(WearSlot::Light),
            "finger r" | "right finger" | "finger right" | "r finger" | "finger" | "ring" => {
                Some(WearSlot::FingerR)
            }
            "finger l" | "left finger" | "finger left" | "l finger" => Some(WearSlot::FingerL),
            "neck 1" | "neck1" | "neck" | "first neck" | "neck first" => Some(WearSlot::Neck1),
            "neck 2" | "neck2" | "second neck" | "neck second" => Some(WearSlot::Neck2),
            "body" => Some(WearSlot::Body),
            "head" | "helm" | "helmet" => Some(WearSlot::Head),
            "legs" | "leg" => Some(WearSlot::Legs),
            "feet" | "foot" | "boot" | "boots" => Some(WearSlot::Feet),
            "hands" | "hand" | "glove" | "gloves" => Some(WearSlot::Hands),
            "arms" | "arm" | "sleeve" | "sleeves" => Some(WearSlot::Arms),
            "shield" | "off hand" | "offhand" => Some(WearSlot::Shield),
            "about" | "about body" | "cloak" | "cape" => Some(WearSlot::About),
            "waist" | "belt" => Some(WearSlot::Waist),
            "wrist r" | "right wrist" | "wrist right" | "r wrist" | "wrist" | "bracelet" => {
                Some(WearSlot::WristR)
            }
            "wrist l" | "left wrist" | "wrist left" | "l wrist" => Some(WearSlot::WristL),
            "wield" | "wielded" | "main hand" | "mainhand" | "weapon" | "wield hand"
            | "primary hand" => Some(WearSlot::Wield),
            "hold" | "held" | "hold hand" | "secondary hand" | "off weapon" => Some(WearSlot::Hold),
            _ => None,
        }
    }
}

/// Pick the position an item should occupy given its declared `slots` and the
/// currently worn map.
///
/// A declared slot that is empty wins first. Only if every declared slot is
/// occupied does a paired declaration fall back to its partner — so a ring
/// declared `[finger_r]` still fills `finger_l` rather than refusing, which is
/// the Circle behaviour (`ITEM_WEAR_FINGER` covers both fingers).
pub fn choose_slot(def: &TbaItemDef, worn: &HashMap<WearSlot, Entity>) -> Option<WearSlot> {
    for slot in &def.slots {
        if !worn.contains_key(slot) {
            return Some(*slot);
        }
    }
    for slot in &def.slots {
        if let Some(partner) = slot.partner()
            && !worn.contains_key(&partner)
        {
            return Some(partner);
        }
    }
    None
}

// -- affect layer -----------------------------------------------------------

/// Apply one item affect to a combatant block.
///
/// Only the [`AffectLocation`]s with a [`TbaCombatant`] field are applied
/// (`Hitroll`, `Damroll`, `ArmorClass`, `MaxHp`, `MaxMana`, `MaxMove`). The
/// attribute (`Str`…`Cha`) and saving-throw (`SavePara`…`SaveSpell`) locations
/// are **carried-but-not-yet-applied**: they parse and persist with the item
/// but there is nowhere to put them until a stat/save block exists, so they
/// are silently skipped here rather than mis-applied to a wrong field.
pub fn apply_affect(combatant: &mut TbaCombatant, affect: &AffectDef) {
    match affect.location {
        AffectLocation::Hitroll => combatant.hitroll += affect.modifier,
        AffectLocation::Damroll => combatant.damroll += affect.modifier,
        AffectLocation::ArmorClass => combatant.armor_class += affect.modifier,
        AffectLocation::MaxHp => combatant.max_hp += affect.modifier,
        AffectLocation::MaxMana => combatant.max_mana += affect.modifier,
        AffectLocation::MaxMove => combatant.max_move_points += affect.modifier,
        // No `TbaCombatant` field yet — see the function doc.
        AffectLocation::Str
        | AffectLocation::Dex
        | AffectLocation::Int
        | AffectLocation::Wis
        | AffectLocation::Con
        | AffectLocation::Cha
        | AffectLocation::SavePara
        | AffectLocation::SaveRod
        | AffectLocation::SavePetri
        | AffectLocation::SaveBreath
        | AffectLocation::SaveSpell => {}
    }
}

/// Recompute a combatant from its class/level base block plus the sum of the
/// worn items' affects.
///
/// Pure and full: `base` is regenerated by the caller (never the current
/// value), so contributions cannot stack. The current hp/mana/move are clamped
/// into the new maxima (a recompute never heals) and the runtime position and
/// target are preserved.
pub fn recompute_combatant<'a, I>(
    current: &TbaCombatant,
    base: TbaCombatant,
    worn: I,
) -> TbaCombatant
where
    I: IntoIterator<Item = &'a TbaItemDef>,
{
    recompute_combatant_full(current, base, worn, std::iter::empty())
}

/// [`recompute_combatant`] plus the affects contributed by live spell buffs.
///
/// The equipment and buff layers share one rebuild, so a buff cannot stack on
/// top of a previous recompute (the same no-stacking rule as worn items).
pub fn recompute_combatant_full<'a, I, J>(
    current: &TbaCombatant,
    base: TbaCombatant,
    worn: I,
    buffs: J,
) -> TbaCombatant
where
    I: IntoIterator<Item = &'a TbaItemDef>,
    J: IntoIterator<Item = &'a AffectDef>,
{
    let mut next = base;
    for def in worn {
        for affect in &def.affects {
            apply_affect(&mut next, affect);
        }
    }
    for affect in buffs {
        apply_affect(&mut next, affect);
    }
    next.hp = current.hp.clamp(0, next.max_hp.max(0));
    next.mana = current.mana.clamp(0, next.max_mana.max(0));
    next.move_points = current.move_points.clamp(0, next.max_move_points.max(0));
    next.position = current.position;
    next.target = current.target;
    next
}

/// Rebuild a runtime worn map from a save block.
///
/// Returns the resolved `WearSlot → Entity` map and the list of save ids that
/// did not resolve (which the caller logs). A dangling id is dropped, never
/// kept as a placeholder — the runtime map contains only live entities.
pub fn relink_slots(
    save: &TbaEquipmentSave,
    resolve: impl Fn(&str) -> Option<Entity>,
) -> (HashMap<WearSlot, Entity>, Vec<String>) {
    let mut slots = HashMap::new();
    let mut missing = Vec::new();
    for (slot, save_id) in &save.worn {
        match resolve(save_id) {
            Some(entity) => {
                slots.insert(*slot, entity);
            }
            None => missing.push(save_id.to_owned()),
        }
    }
    (slots, missing)
}

// -- command dispatch -------------------------------------------------------

/// The session's carried items: `(entity, prototype, carried-by)`.
type CarriedQuery<'w, 's> = Query<'w, 's, (Entity, &'static Item, &'static CarriedBy)>;

/// `equip` / `unequip` / `equipment` dispatch for `Playing` sessions, in
/// [`BridgeSet::Game`](mib_core::prelude::BridgeSet) after the stdlib router.
///
/// The runtime [`TbaEquipment`] map is mutated in place (world mutation, not
/// deferred commands) so the chained [`recompute_equipment_affects`] and
/// [`sync_wielded_weapon`] see the change in the same frame; the persisted
/// [`TbaEquipmentSave`] is rewritten in lockstep. A player who has never worn
/// anything has neither component yet, so both are inserted on first equip.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_equipment_commands(
    mut reader: MessageReader<CommandReceived>,
    sessions: Query<&Session>,
    content: Option<Res<TbaContent>>,
    items: CarriedQuery<'_, '_>,
    prototypes: Query<&Item>,
    save_ids: Query<&SaveId>,
    mut equipment: Query<Option<&mut TbaEquipment>>,
    mut saves: Query<Option<&mut TbaEquipmentSave>>,
    mut commands: Commands,
    mut output: MessageWriter<OutputEvent>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);

    for msg in reader.read() {
        let session = msg.session;
        let playing = sessions
            .get(session)
            .map(|session| session.state == SessionState::Playing)
            .unwrap_or(false);
        if !playing {
            continue;
        }
        let command = match &msg.input {
            PlayerInput::Command(command) => command.clone(),
            PlayerInput::RawLine(line) => match parse_line(line) {
                Ok(command) => command,
                Err(_) => continue,
            },
        };

        match command {
            Command::Equip { item } => equip(
                session,
                &item,
                &content.items,
                &items,
                &save_ids,
                &mut equipment,
                &mut saves,
                &mut commands,
                &mut output,
            ),
            Command::Unequip { slot } => unequip(
                session,
                &slot,
                &prototypes,
                &save_ids,
                &mut equipment,
                &mut saves,
                &mut commands,
                &mut output,
            ),
            Command::Equipment => {
                equipment_listing(session, &prototypes, &mut equipment, &mut output)
            }
            _ => {}
        }
    }
}

/// A case-insensitive `contains` of the typed fragment against a display name
/// — the stdlib item core's matching convention.
fn name_matches(needle: &str, name: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    !needle.is_empty() && name.to_lowercase().contains(&needle)
}

/// The carried item matching `needle`, if any.
fn find_carried(actor: Entity, needle: &str, items: &CarriedQuery<'_, '_>) -> Option<Entity> {
    items.iter().find_map(|(entity, item, carried)| {
        (carried.0 == actor && name_matches(needle, &item.name)).then_some(entity)
    })
}

/// A system-channel notice plus the prompt.
fn notice(output: &mut MessageWriter<OutputEvent>, session: Entity, body: impl Into<String>) {
    output.write(OutputEvent {
        session,
        payload: OutboundPayload::text(TextChannel::System, body),
    });
    write_prompt(output, session);
}

/// A `NoSuchTarget` error plus the prompt (one place, no double-prompt).
fn deny(output: &mut MessageWriter<OutputEvent>, session: Entity, message: impl Into<String>) {
    output.write(OutputEvent {
        session,
        payload: OutboundPayload::error(ErrorKind::NoSuchTarget, message),
    });
    write_prompt(output, session);
}

/// Write the runtime and persisted equipment back, inserting either component
/// if the entity did not have it yet.
fn write_equipment(
    session: Entity,
    slots: HashMap<WearSlot, Entity>,
    worn: BTreeMap<WearSlot, String>,
    equipment: &mut Query<Option<&mut TbaEquipment>>,
    saves: &mut Query<Option<&mut TbaEquipmentSave>>,
    commands: &mut Commands,
) {
    match equipment.get_mut(session) {
        Ok(Some(mut component)) => component.slots = slots,
        _ => {
            commands.entity(session).insert(TbaEquipment { slots });
        }
    }
    match saves.get_mut(session) {
        Ok(Some(mut component)) => component.worn = worn,
        _ => {
            commands.entity(session).insert(TbaEquipmentSave { worn });
        }
    }
}

/// Rebuild the persisted map from a runtime map, dropping (and reporting)
/// items that carry no [`SaveId`].
fn persisted_from_runtime(
    slots: &HashMap<WearSlot, Entity>,
    save_ids: &Query<&SaveId>,
) -> BTreeMap<WearSlot, String> {
    let mut worn = BTreeMap::new();
    for (slot, entity) in slots {
        match save_ids.get(*entity) {
            Ok(save_id) => {
                worn.insert(*slot, save_id.0.to_string());
            }
            Err(_) => tracing::warn!(
                slot = slot.display_name(),
                "worn item has no SaveId; it will not survive a restart"
            ),
        }
    }
    worn
}

#[allow(clippy::too_many_arguments)]
fn equip(
    session: Entity,
    item_name: &str,
    items_defs: &BTreeMap<ItemId, TbaItemDef>,
    items: &CarriedQuery<'_, '_>,
    save_ids: &Query<&SaveId>,
    equipment: &mut Query<Option<&mut TbaEquipment>>,
    saves: &mut Query<Option<&mut TbaEquipmentSave>>,
    commands: &mut Commands,
    output: &mut MessageWriter<OutputEvent>,
) {
    let Some(entity) = find_carried(session, item_name, items) else {
        deny(output, session, "You aren't carrying that.");
        return;
    };
    let Ok((_, prototype, _)) = items.get(entity) else {
        return;
    };
    let name = prototype.name.clone();
    let def_id = prototype.def_id.clone();
    let Some(def) = items_defs.get(&def_id) else {
        deny(output, session, format!("You can't equip {name}."));
        return;
    };

    let mut slots = equipment
        .get(session)
        .ok()
        .flatten()
        .map(|current| current.slots.clone())
        .unwrap_or_default();
    if slots.values().any(|worn_entity| *worn_entity == entity) {
        notice(output, session, format!("You are already wearing {name}."));
        return;
    }
    let Some(slot) = choose_slot(def, &slots) else {
        if def.slots.is_empty() {
            deny(output, session, format!("You can't equip {name}."));
        } else {
            let occupied: Vec<String> = def
                .slots
                .iter()
                .map(|slot| slot.display_name().to_owned())
                .collect();
            deny(
                output,
                session,
                format!(
                    "You have no free slot for {name} ({}).",
                    occupied.join(", ")
                ),
            );
        }
        return;
    };

    slots.insert(slot, entity);
    let worn = persisted_from_runtime(&slots, save_ids);
    write_equipment(session, slots, worn, equipment, saves, commands);
    notice(
        output,
        session,
        format!("You equip {name} ({}).", slot.display_name()),
    );
}

#[allow(clippy::too_many_arguments)]
fn unequip(
    session: Entity,
    slot_word: &str,
    prototypes: &Query<&Item>,
    save_ids: &Query<&SaveId>,
    equipment: &mut Query<Option<&mut TbaEquipment>>,
    saves: &mut Query<Option<&mut TbaEquipmentSave>>,
    commands: &mut Commands,
    output: &mut MessageWriter<OutputEvent>,
) {
    let Some(slot) = WearSlot::parse(slot_word) else {
        deny(
            output,
            session,
            format!("There's no such slot as {slot_word}."),
        );
        return;
    };
    let mut slots = equipment
        .get(session)
        .ok()
        .flatten()
        .map(|current| current.slots.clone())
        .unwrap_or_default();
    let Some(entity) = slots.remove(&slot) else {
        deny(
            output,
            session,
            format!(
                "You aren't wearing anything on your {}.",
                slot.display_name()
            ),
        );
        return;
    };
    let name = prototypes
        .get(entity)
        .map(|prototype| prototype.name.clone())
        .unwrap_or_else(|_| "something".to_owned());
    // The runtime map is authoritative; rebuilding the save from it keeps the
    // two in lockstep and drops any stale entry for the removed slot.
    let worn = persisted_from_runtime(&slots, save_ids);
    write_equipment(session, slots, worn, equipment, saves, commands);
    notice(output, session, format!("You remove {name}."));
}

fn equipment_listing(
    session: Entity,
    prototypes: &Query<&Item>,
    equipment: &mut Query<Option<&mut TbaEquipment>>,
    output: &mut MessageWriter<OutputEvent>,
) {
    let mut worn_items: Vec<(WearSlot, String)> = equipment
        .get(session)
        .ok()
        .flatten()
        .map(|current| {
            current
                .slots
                .iter()
                .filter_map(|(slot, entity)| {
                    prototypes
                        .get(*entity)
                        .ok()
                        .map(|prototype| (*slot, prototype.name.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    worn_items.sort_by_key(|(slot, _)| *slot);
    let body = if worn_items.is_empty() {
        "You are wearing nothing.".to_owned()
    } else {
        let parts: Vec<String> = worn_items
            .iter()
            .map(|(slot, name)| format!("{}: {name}", slot.display_name()))
            .collect();
        format!("You are wearing: {}.", parts.join(", "))
    };
    notice(output, session, body);
}

// -- systems ----------------------------------------------------------------

/// Rebuild every player's [`TbaCombatant`] from class/level plus worn-item
/// and live-buff affects.
///
/// Runs every frame but only writes when the value actually changed (the
/// `TbaCombatant` `PartialEq` guard), so an unchanged player is not marked
/// dirty and the save pipeline does not rewrite them every tick. The base
/// block is regenerated by [`derive_vitals`] rather than read from the current
/// component; that is what makes equipment and buff contributions
/// non-stacking. A player with no runtime [`TbaEquipment`] (never equipped a
/// thing) is treated as wearing nothing rather than skipped.
#[allow(clippy::type_complexity)] // one ECS view per optional player component.
pub fn recompute_equipment_affects(
    content: Option<Res<TbaContent>>,
    players: Query<
        (
            Entity,
            &TbaProgress,
            Option<&TbaEquipment>,
            Option<&TbaBuffs>,
            &mut TbaCombatant,
        ),
        With<Player>,
    >,
    items: Query<&Item>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);
    for (_player, progress, equipment, buffs, mut combatant) in players {
        let class = content
            .classes
            .get(&progress.class)
            .cloned()
            .unwrap_or_else(default_class);
        let base = derive_vitals(&class, progress.level);
        let worn: Vec<&TbaItemDef> = equipment
            .into_iter()
            .flat_map(|equipment| equipment.slots.values())
            .filter_map(|item_entity| {
                let prototype = items.get(*item_entity).ok()?;
                content.items.get(&prototype.def_id)
            })
            .collect();
        let buff_affects: Vec<&AffectDef> = buffs
            .map(|buffs| buffs.affects().collect())
            .unwrap_or_default();
        let next = recompute_combatant_full(&combatant, base, worn, buff_affects);
        if *combatant != next {
            *combatant = next;
        }
    }
}

/// Rebuild every equipped NPC's [`TbaCombatant`] from its captured
/// [`TbaNpcBase`] plus worn-item affects.
///
/// The NPC analogue of [`recompute_equipment_affects`]: it exists separately
/// because an NPC's base is rolled once from `tba.npc_stats` (see
/// [`TbaNpcBase`]) rather than derived from a class/level, so it cannot reuse
/// [`derive_vitals`]. The same no-stacking rule applies — the base is the
/// captured block, never the current one — and the `PartialEq` guard keeps an
/// unchanged NPC from being marked dirty. NPCs without [`TbaEquipment`] are
/// skipped, so the cost is bounded by the number of equipped mobs, not by the
/// whole world. `Without<Player>` keeps the player path (class/level based) and
/// this one disjoint.
#[allow(clippy::type_complexity)] // one ECS view per optional NPC component.
pub fn recompute_npc_equipment_affects(
    content: Option<Res<TbaContent>>,
    npcs: Query<(Entity, &TbaNpcBase, &TbaEquipment, &mut TbaCombatant), Without<Player>>,
    items: Query<&Item>,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);
    for (_npc, base, equipment, mut combatant) in npcs {
        let worn: Vec<&TbaItemDef> = equipment
            .slots
            .values()
            .filter_map(|item_entity| {
                let prototype = items.get(*item_entity).ok()?;
                content.items.get(&prototype.def_id)
            })
            .collect();
        let next = recompute_combatant_full(&combatant, base.0.clone(), worn, std::iter::empty());
        if *combatant != next {
            *combatant = next;
        }
    }
}

/// Keep [`WieldedWeapon`] present exactly when the [`WearSlot::Wield`] holds a
/// `tba.items` weapon. Any other occupant (or an empty hand, or a missing
/// prototype) removes it.
///
/// Covers players and NPCs alike: [`WieldedWeapon`] is the same combat seam
/// for both, and a zone-reset `E` can put a weapon in a mob's hand.
pub fn sync_wielded_weapon(
    content: Option<Res<TbaContent>>,
    combatants: Query<(Entity, &TbaEquipment, Option<&WieldedWeapon>)>,
    items: Query<&Item>,
    mut commands: Commands,
) {
    let empty = TbaContent::default();
    let content = content.as_deref().unwrap_or(&empty);
    for (entity, equipment, wielded) in &combatants {
        let desired: Option<ItemId> = equipment.worn(WearSlot::Wield).and_then(|item_entity| {
            let prototype = items.get(item_entity).ok()?;
            let def = content.items.get(&prototype.def_id)?;
            (def.kind == TbaItemKind::Weapon).then(|| prototype.def_id.clone())
        });
        match (desired, wielded) {
            (Some(id), Some(current)) if current.0 == id => {}
            (Some(id), _) => {
                commands.entity(entity).insert(WieldedWeapon(id));
            }
            (None, Some(_)) => {
                commands.entity(entity).remove::<WieldedWeapon>();
            }
            (None, None) => {}
        }
    }
}

/// Rebuild [`TbaEquipment`] from a restored [`TbaEquipmentSave`].
///
/// Runs in [`BridgeSet::Game`](mib_core::prelude::BridgeSet) after the stdlib
/// `restore_player_bundles` + `attach_pending_holders` pair, so the item
/// entities exist (and are carried) by the time their save ids are resolved.
/// Only players that have a save but no runtime map are processed, so a
/// relink happens once per login and never clobbers live equipment. Ids that
/// no longer resolve are dropped from the map and logged.
pub fn restore_equipment_from_save(
    players: Query<(Entity, &TbaEquipmentSave), Without<TbaEquipment>>,
    save_ids: Query<(Entity, &SaveId)>,
    mut commands: Commands,
) {
    if players.is_empty() {
        return;
    }
    let index: HashMap<SaveId, Entity> = save_ids
        .iter()
        .map(|(entity, save_id)| (save_id.clone(), entity))
        .collect();
    for (player, save) in &players {
        let (slots, missing) = relink_slots(save, |id| index.get(&SaveId::new(id)).copied());
        if !missing.is_empty() {
            tracing::warn!(
                player = ?player,
                missing = ?missing,
                "dropping worn items whose save id no longer exists"
            );
        }
        commands.entity(player).insert(TbaEquipment { slots });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::content::Position;

    fn item(slots: Vec<WearSlot>, kind: TbaItemKind) -> TbaItemDef {
        TbaItemDef {
            kind,
            slots,
            values: [0; 4],
            affects: Vec::new(),
            weight: 0,
            cost: 0,
            rent: 0,
            min_level: 0,
            timer: None,
        }
    }

    fn combatant() -> TbaCombatant {
        TbaCombatant {
            hp: 10,
            max_hp: 10,
            mana: 5,
            max_mana: 5,
            move_points: 80,
            max_move_points: 80,
            hitroll: 0,
            damroll: 0,
            armor_class: 100,
            thaco: 20,
            position: Position::Standing,
            target: None,
        }
    }

    #[test]
    fn slot_parse_is_lenient() {
        assert_eq!(WearSlot::parse("wield"), Some(WearSlot::Wield));
        assert_eq!(WearSlot::parse("WIELDED"), Some(WearSlot::Wield));
        assert_eq!(WearSlot::parse("main hand"), Some(WearSlot::Wield));
        assert_eq!(WearSlot::parse("main-hand"), Some(WearSlot::Wield));
        assert_eq!(WearSlot::parse("finger r"), Some(WearSlot::FingerR));
        assert_eq!(WearSlot::parse("right_finger"), Some(WearSlot::FingerR));
        assert_eq!(WearSlot::parse("finger"), Some(WearSlot::FingerR));
        assert_eq!(WearSlot::parse("neck 2"), Some(WearSlot::Neck2));
        assert_eq!(WearSlot::parse("left wrist"), Some(WearSlot::WristL));
        assert_eq!(WearSlot::parse("banana"), None);
    }

    #[test]
    fn paired_partner_is_symmetric() {
        assert_eq!(WearSlot::FingerR.partner(), Some(WearSlot::FingerL));
        assert_eq!(WearSlot::FingerL.partner(), Some(WearSlot::FingerR));
        assert_eq!(WearSlot::Neck1.partner(), Some(WearSlot::Neck2));
        assert_eq!(WearSlot::WristR.partner(), Some(WearSlot::WristL));
        assert_eq!(WearSlot::Head.partner(), None);
        assert!(WearSlot::Neck1.is_paired());
        assert!(!WearSlot::Body.is_paired());
    }

    #[test]
    fn choose_slot_fills_the_free_declared_slot_then_the_partner() {
        let ring = item(vec![WearSlot::FingerR], TbaItemKind::Armor);
        let mut worn = HashMap::new();
        assert_eq!(choose_slot(&ring, &worn), Some(WearSlot::FingerR));
        worn.insert(WearSlot::FingerR, Entity::from_raw_u32(1).unwrap());
        assert_eq!(
            choose_slot(&ring, &worn),
            Some(WearSlot::FingerL),
            "a paired declaration falls back to the free partner"
        );
        worn.insert(WearSlot::FingerL, Entity::from_raw_u32(2).unwrap());
        assert_eq!(choose_slot(&ring, &worn), None, "both fingers occupied");
    }

    #[test]
    fn choose_slot_refuses_an_item_with_no_slots() {
        let rock = item(Vec::new(), TbaItemKind::Treasure);
        assert_eq!(choose_slot(&rock, &HashMap::new()), None);
    }

    #[test]
    fn affects_change_only_the_mapped_combatant_fields() {
        let mut def = item(vec![WearSlot::Body], TbaItemKind::Armor);
        def.affects = vec![
            AffectDef {
                location: AffectLocation::ArmorClass,
                modifier: -10,
            },
            AffectDef {
                location: AffectLocation::Hitroll,
                modifier: 3,
            },
            AffectDef {
                location: AffectLocation::Damroll,
                modifier: 2,
            },
            AffectDef {
                location: AffectLocation::MaxHp,
                modifier: 15,
            },
            AffectDef {
                location: AffectLocation::Str,
                modifier: 5,
            },
            AffectDef {
                location: AffectLocation::SaveSpell,
                modifier: 4,
            },
        ];
        let current = combatant();
        let next = recompute_combatant(&current, combatant(), std::iter::once(&def));
        assert_eq!(next.armor_class, 90);
        assert_eq!(next.hitroll, 3);
        assert_eq!(next.damroll, 2);
        assert_eq!(next.max_hp, 25);
        assert_eq!(next.hp, 10, "recompute never heals");
        assert_eq!(next.thaco, 20, "unaffected fields keep the base");
        assert_eq!(next.position, current.position);
    }

    #[test]
    fn recompute_clamps_current_vitals_into_reduced_maxima() {
        let mut def = item(vec![WearSlot::Body], TbaItemKind::Armor);
        def.affects = vec![AffectDef {
            location: AffectLocation::MaxHp,
            modifier: -4,
        }];
        let mut current = combatant();
        current.hp = 10;
        let next = recompute_combatant(&current, combatant(), std::iter::once(&def));
        assert_eq!(next.max_hp, 6);
        assert_eq!(next.hp, 6, "hp is clamped down to the new maximum");
    }

    #[test]
    fn relink_resolves_known_ids_and_lists_missing_ones() {
        let mut save = TbaEquipmentSave::default();
        save.worn.insert(WearSlot::Wield, "item:sword".to_owned());
        save.worn.insert(WearSlot::Body, "item:gone".to_owned());
        let live = Entity::from_raw_u32(7).unwrap();
        let (slots, missing) = relink_slots(&save, |id| (id == "item:sword").then_some(live));
        assert_eq!(slots.get(&WearSlot::Wield), Some(&live));
        assert_eq!(slots.get(&WearSlot::Body), None);
        assert_eq!(missing, vec!["item:gone".to_owned()]);
    }

    #[test]
    fn save_round_trips_through_serde() {
        let mut save = TbaEquipmentSave::default();
        save.worn
            .insert(WearSlot::Wield, "item:c:inn:sword:0".to_owned());
        save.worn.insert(WearSlot::Neck1, "item:d12".to_owned());
        let json = serde_json::to_string(&save).unwrap();
        let back: TbaEquipmentSave = serde_json::from_str(&json).unwrap();
        assert_eq!(back, save);
        // The key spelling is the stable `snake_case` enum rename.
        assert!(
            json.contains("finger_r") || json.contains("wield"),
            "{json}"
        );
    }
}
