# tbaport drop report summary

- tool: `tbaport 0.1.0`
- mode: `full`
- zones: 189
- rooms: 12700
- npcs: 3705
- items: 4765
- zone commands: 18513
- deferred: 3161
- dangling: 14
- unmapped: 17578
- global permanent drops: 7

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
| `dangling_shop_keeper` | 4 |
| `dangling_shop_produce` | 2 |
| `dangling_shop_room` | 3 |
| `duplicate_shop_keeper` | 2 |

## Deferred records by section

| section | count |
|---|---:|
| `quests` | 1 |
| `triggers` | 3160 |

## Global permanent drops

- `combat_skill` / `assist`: Circle combat skill with no engine implementation (non-goal)
- `combat_skill` / `backstab`: Circle combat skill with no engine implementation (non-goal)
- `combat_skill` / `bash`: Circle combat skill with no engine implementation (non-goal)
- `combat_skill` / `consider`: Circle combat skill with no engine implementation (non-goal)
- `combat_skill` / `kick`: Circle combat skill with no engine implementation (non-goal)
- `combat_skill` / `rescue`: Circle combat skill with no engine implementation (non-goal)
- `no_source_field` / `room coord`: tbaMUD `struct room_data` has no coordinates; `RoomDef::coord` is always `None`
