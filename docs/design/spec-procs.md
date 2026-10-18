# Issue #31 system ownership — stdlib vs game pack

Status: **partially shipped**. MIB PR #25 shipped the stdlib services
(mail, boards, escrow, followers); the TBA side has mail, boards and rent in
`feat/31-spec-procs`. Followers (pet shop), bank and practice/guild remain.
Companion to PR #36 (e-specs) and PR #37 (the spec-proc series).

This note classifies each system behind the Circle special procedures by the
layer that should own it. It is a boundary decision, not an implementation
plan; the per-system APIs below are the seams the game pack should consume.

## The rule

MIB's layering (D12/D13 in `MIB/docs/reimpl_design.md`) is:

| Layer | Owns | Should not own |
|---|---|---|
| **Engine** (`mib-core`, `mib-persist`, `mib-content`, `mib-protocol`, `mib-bridge`) | Generic mechanisms: documents, save registry, command/verb plumbing, messages, content loading | Any gameplay policy, NPC behaviour, genre data |
| **`mib-stdlib`** | Genre-agnostic gameplay: sessions/accounts, rooms/movement, the generic item core, channels, quests, NPC dialogue/memory, and `stdlib.*` schemas | Circle/Diku class tables, world-specific NPCs, prices, schedules, flags |
| **Game pack** (`mib-tba`) | Circle/Diku rules and world data: classes, spells, combat, e-specs, shops, special procedures, and the `tba.*` schemas | Generic offline mail/board storage, generic item escrow, generic ownership primitives |

The practical test for a new system:

1. Would a second, unrelated game want it unchanged? → **stdlib**.
2. Is it generic mechanism only (a doc kind, a save hook)? → **engine**.
3. Does it depend on Circle's `spell_info`, rent formula, board vnums, castle
   paths, `MOB_SPEC`, or Diku attributes? → **game pack**.

The existing persistence machinery (`mib-persist` + stdlib `SaveRegistry`) is
already sufficient for almost all of this: `Mailbox`/`BankAccount` on `Player`,
`BoardPosts` on the board `Item`, a stored `ItemPlacement` variant, and a
`PetOwner` on the pet `Npc` all fit the shipped `Player`/`Item`/`Npc` document
kinds. No new `DocKind` is required for the recommendations below.

## Ownership table

| System | Engine | `mib-stdlib` | `mib-tba` | Decision |
|---|---|---|---|---|
| **Mail storage** (send/read/delete, offline delivery, expiry) | — | mailbox data + persistence + service | — | **stdlib** (D-06 already names the `Mailbox` + persistence extension point) |
| **Postmaster NPC**, `mail`/`check`/`receive` at a postmaster, postage rules | — | — | spec proc + command gate + cost policy | **pack** |
| **Practice / skill proficiency** (`practice`, practices, `GET_SKILL` percentages) | — | — | Circle class/spell tables, gain rules, proficiency state | **pack** |
| **Guildmaster NPC** (`SPECIAL(guild)`) | — | — | command interception + wording | **pack** |
| **Rent / cryogenic storage** (park an item set for an offline owner, restore it) | item docs (D-40) already exist | a `Stored` item placement + storage/escrow service | receptionist/cryo NPCs, rent formula, deadline, free-rent policy | **split** |
| **Bank** (persisted balance, deposit/withdraw) | player docs exist | a ledger if MIB ever defines a generic wallet/currency | `TbaGold`, the bank/ATM objects, fees, cashcard | **split, pack-now** |
| **Bulletin boards** (posts, read/write/remove, moderation) | item docs exist | board post storage + service | board object spec proc, board ids, which boards exist | **stdlib** |
| **Pets** (owned NPC, follow/stay/order, persist, dismiss) | NPC docs exist | ownership/follow/order primitives | `SPECIAL(pet_shops)`, stock, prices, level limits | **split** |
| **Mayor / King paths, banzaii, rescue, cleaning, `block_way`** | — | a generic NPC schedule primitive is a *future* stdlib candidate (D-12) | the behaviours and paths | **pack** |
| **E-specs** (attributes/saves) | — | — | Diku attributes/saves | **pack** |

## Per-system notes

### Mail — stdlib

Mail is the clearest stdlib case. It is genre-agnostic social infrastructure,
and MIB already records it as deferred **D-06** with exactly this extension
point: a `Mailbox` component per player plus the existing player persistence.
The stdlib should own:

- the mailbox document shape (messages, sender, timestamp, read/unread,
  attachments if ever supported);
- send, list, read, delete, expiry and quota;
- offline delivery (the recipient need not be online);
- the `Mailbox`/`MailService` API and its persistence registration.

The game pack owns the **postmaster**: Circle's `mail`, `check` and `receive`
verbs only work at a postmaster (`SPECIAL(postmaster)`), with Circle's postage
and level checks. The postmaster handler calls the stdlib service; it must not
grow its own mailbox storage. That also keeps the mail data usable by a future
frontend/admin surface without going through the TBA NPC.

Do **not** put the postmaster NPC, its vnums, or Circle's postage rules in
stdlib. Do **not** implement mailbox persistence in `mib-tba` "for now" — that
would create a second, incompatible mail format.

### Practice / proficiency — pack

Circle's `practice` command is entirely genre data: `spell_info[]`,
`GET_PRACTICES`, `practice_gain_percent`, class/level minima and the
`LEARNED` cap. The generic shape ("spend a point, raise a proficiency") is
tempting to promote, but MIB already has a genre-pack skill model
(`classic-rpg`'s `SkillRanks`/`train_skill`) and TBA has its own Circle
classes/spells. A stdlib proficiency system would either duplicate that or
force two different skill models into one abstraction.

TBA owns the practices counter, the per-skill/spell percentage map, the
`practice` verb, the guildmaster interception and the use of the percentage in
`skills.rs`/`spells.rs`. If a shared trainer abstraction is ever wanted, the
right home is a genre pack (or a later stdlib trainer service), not the
genre-agnostic core.

### Rent / cryo — split

The generic part is **durable storage for an offline owner**: take an item
tree out of play, persist it under a storage record, and restore it (possibly
for a fee or after a deadline) when the owner returns. MIB's item persistence
already has the hard part — stable item save ids, container trees, and
`PendingHolder` for an offline holder. The clean stdlib seam is a `Stored`
item placement (or a small storage/escrow service) that owns the parking and
restoration mechanics.

TBA owns the Circle policy: the receptionist/cryogenicist NPCs, `rent` vs
`offer`, per-item rent cost, the rent deadline, the free-rent config, and the
"your belongings" messages. Do not encode Circle's rent formula in the storage
primitive.

If the stdlib does not grow the storage API, TBA can use `PendingHolder` as a
temporary mechanism, but that is an internal parking hook, not a rent API;
promote it before shipping a second game's storage.

### Bank — split, implemented in the pack first

A bank is a persisted integer balance plus deposit/withdraw. That ledger is
generic, but **MIB has no generic currency**: `TbaGold` and the gold item are
TBA's, and MIB's economy entry (D-02) is deferred. Adding a stdlib wallet for
one game would be premature abstraction.

Therefore:

- **now:** TBA owns `TbaGold`, the `bank`/ATM object spec procs, fees and the
  cashcard. The bank balance is a TBA player component (registered under
  `DocKind::Player`), so it persists without a new doc kind.
- **later:** if MIB defines a generic wallet/currency for a second game, the
  ledger (not the banker) moves to stdlib and TBA's gold becomes an adapter.

The split line is: stdlib may own *a balance keyed by currency*; TBA owns
*what currency is, how it is earned, and the NPC that banks it*.

### Boards — stdlib

A bulletin board is persistent social data with read/write/remove semantics
and moderation. That is the same class of feature as mail and channels, not
Circle-specific. Stdlib should own the post store and operations; the pack
owns the board object, its id, and which vnums get a board. Posts can live on
the board item's `Item` document (`BoardPosts`) or a small stdlib board
document; either way, the format belongs to stdlib so a second game can reuse
it and an admin surface can read it.

Circle's `gen_board` is only the object-spec-proc front end; its command
surface (`read`, `write`, `remove`, `look at board`) and moderation policy can
be a thin pack adapter over the stdlib board service.

### Pets / pet shops — split

Pets are generic: an owned NPC that follows its owner, can be ordered to stay
or follow, persists across logout, and is dismissed/reclaimed. MIB already has
NPC persistence and follower machinery, so the generic ownership/order/persist
piece belongs in stdlib (or is a small extension of the existing follower
system). The **pet shop** is game content: stock, prices, level limits,
`SPECIAL(pet_shops)` and the "list/buy/offer" wording.

If pets are implemented only in TBA, a second game reimplements ownership and
persistence. If the pet shop is put in stdlib, it drags prices and Circle's
shop rules into the genre-agnostic layer. Split them.

### NPC schedules and special procedures — pack

`SPECIAL(mayor)`, `SPECIAL(king_welmar)`, banzaii, rescue, cleaning and
`block_way` are Circle world behaviour. They belong in TBA.

One piece is a legitimate future stdlib candidate: a generic **NPC schedule /
waypoint** component (MIB's D-12 "NPC daily schedules" extension point). The
mayor/King paths are an instance of that. If stdlib grows a schedule primitive,
TBA should supply the path data and the emotes, not the iteration driver.
Until then, `mib_tba::spec` is the right home — do not generalize the castle's
paths into stdlib.

### E-specs — pack

Diku prime attributes, saving throws and the loader's 11/13 baselines are genre
rules. They stay in `tba.npc_stats` and `mib_tba::combat`. No stdlib change.

## Shipped / remaining

- **Shipped in MIB:** `mail.rs` + `stdlib.mail`, `boards.rs` + `stdlib.boards`,
  `ItemPlacement::Stored` + `storage.rs`, and `follower.rs` (the owned-NPC
  primitive, deliberately named Followers rather than Pets).
- **Shipped in TBA:** the postmaster (`check`/`receive`/`mail`), the board
  commands (`look`/`read`/`write`/`remove` + `stdlib.boards.yaml`), and the
  receptionist/cryogenicist (`offer`/`rent` over escrow).
- **Shipped in TBA (continued):** the bank/ATM (`balance`/`deposit`/`withdraw`
  over `TbaGold` + the bank vnums), the pet shop (`list`/`buy` over the
  stdlib `Followers` primitive), and practice/guild (a persisted
  `TbaPractice` counter + per-spell/skill percentages, with the level-up
  practice grant).

## Suggested cross-repo split

1. **MIB stdlib PR:** `Mailbox` + mail service (D-06), board post store +
   service, `Stored` item placement/storage service. These are reusable and
   unblock the most TBA work. (Pets can follow as a separate stdlib PR.)
2. **MIB-TBA PRs:** postmaster adapter, board object adapter, receptionist/
   cryogenicist + rent policy, bank/ATM (on `TbaGold`), pet shop.
3. **Practice/guild:** TBA only; no stdlib dependency.
4. **Castle/mayor:** already TBA (`mib_tba::spec`).

This ordering keeps the pack consuming stable stdlib APIs instead of
reimplementing mail/board/storage formats that a later stdlib change would
strand.

## Non-goals for stdlib

Do not move these into `mib-stdlib` even though they are adjacent:

- Circle practice gain, `spell_info`, and class/level minima;
- rent formulas, deadlines, free-rent policy;
- bank fees, cashcards, and the gold item;
- pet shop prices/stock/level limits;
- the castle paths, banzaii/cleaning behaviours, `block_way`, and `MOB_SPEC`;
- e-spec attributes/saves.
