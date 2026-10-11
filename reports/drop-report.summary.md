# tbaport drop report summary

- tool: `tbaport 0.1.0`
- mode: `full`
- zones: 189
- rooms: 12700
- npcs: 3705
- items: 4765
- zone commands: 18513
- deferred: 3160
- dangling: 15
- unmapped: 17585
- global permanent drops: 10

## Unmapped drops by reason

| reason (`kind`) | count |
|---|---:|
| `affect_location` | 40 |
| `exit_description_only` | 108 |
| `item_action_description` | 38 |
| `item_affect_flag` | 6 |
| `item_extra_descriptions` | 2497 |
| `mob_aff` | 2069 |
| `mob_espec` | 2805 |
| `mob_flag` | 45 |
| `mob_sex` | 3705 |
| `npc_keywords` | 3705 |
| `quest_condition` | 3 |
| `quest_gold_reward` | 1 |
| `quest_text` | 3 |
| `room_flag` | 1726 |
| `shop_bitvector` | 311 |
| `shop_broke_temper` | 67 |
| `shop_buy_word` | 53 |
| `shop_profit_buy` | 1 |
| `shop_record` | 238 |
| `zone_flag` | 164 |

## Dangling references by reason

| reason (`kind`) | count |
|---|---:|
| `dangling_door_key` | 3 |
| `dangling_quest_reward_item` | 1 |
| `dangling_shop_keeper` | 4 |
| `dangling_shop_produce` | 2 |
| `dangling_shop_room` | 3 |
| `duplicate_shop_keeper` | 2 |

## Deferred records by section

| section | count |
|---|---:|
| `triggers` | 3160 |

## DG trigger triage (#15)

- records: 1461
- script lines (non-blank): 26305
- zones with triggers: 113

### Triggers by attach

| attach | count |
|---|---:|
| `mob` | 795 |
| `object` | 282 |
| `world` | 384 |

### Triggers by interactivity bucket

| bucket | count |
|---|---:|
| `ambience_flavour` | 206 |
| `combat_behaviour` | 166 |
| `gatekeeper_transport` | 111 |
| `interactive_response` | 902 |
| `quest_objective` | 76 |

### Triggers by expanded type

| type | count |
|---|---:|
| `Act` | 25 |
| `Bribe` | 10 |
| `Cast` | 7 |
| `Command` | 376 |
| `Consume` | 6 |
| `Death` | 46 |
| `Door` | 5 |
| `Drop` | 19 |
| `Enter` | 108 |
| `Entry` | 11 |
| `Fight` | 66 |
| `Get` | 18 |
| `Give` | 4 |
| `Global` | 28 |
| `Greet` | 197 |
| `Greet-All` | 36 |
| `HitPrcnt` | 8 |
| `Leave` | 69 |
| `Load` | 39 |
| `Login` | 3 |
| `Memory` | 6 |
| `Random` | 229 |
| `Receive` | 42 |
| `Remove` | 9 |
| `Speech` | 120 |
| `Time` | 2 |
| `Timer` | 14 |
| `Wear` | 22 |
| `Zone Reset` | 10 |

## Global permanent drops

- `combat_skill` / `assist`: Circle class ability `assist`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `backstab`: Circle class ability `backstab`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `bash`: Circle class ability `bash`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `consider`: Circle class ability `consider`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `kick`: Circle class ability `kick`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `rescue`: Circle class ability `rescue`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `no_source_field` / `room coord`: tbaMUD `struct room_data` has no coordinates; `RoomDef::coord` is always `None`
- `out_of_scope` / `lib/misc/socials`: legacy old-format social file; `CONFIG_NEW_SOCIALS` is YES, so the run-time reads `lib/misc/socials.new` (issue #17)
- `out_of_scope` / `lib/text/help`: legacy `help.hlp` database; the generated command catalog plus the converted `lib/text` and `lib/misc/messages` topics supersede it (issue #18)
- `social_code` / `$i`: tbaMUD `perform_act` has no `$i` case; the code is preserved in `tba.socials` and expands to nothing at run time
