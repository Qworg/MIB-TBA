# tbaport drop report summary

- tool: `tbaport 0.1.0`
- mode: `full`
- zones: 189
- rooms: 12700
- npcs: 3705
- items: 4765
- zone commands: 18513
- deferred: 3027
- dangling: 42
- unmapped: 17585
- global permanent drops: 10
- DG triggers emitted: 133

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
| `dangling_trigger_attach` | 27 |
| `duplicate_shop_keeper` | 2 |

## Deferred records by section

| section | count |
|---|---:|
| `triggers` | 3027 |

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

## DG trigger parse coverage (#15)

- emitted records: 133
- parsed: 133
- parse failures: 0
- records with unsupported constructs: 50

### Emitted constructs

| construct | count |
|---|---:|
| `at.load` | 1 |
| `break` | 29 |
| `case` | 33 |
| `default` | 3 |
| `done` | 9 |
| `door` | 4 |
| `echo` | 72 |
| `echoaround` | 34 |
| `else` | 62 |
| `elseif` | 33 |
| `emote` | 62 |
| `eval` | 40 |
| `force` | 3 |
| `global` | 2 |
| `halt` | 42 |
| `if` | 224 |
| `load.mob` | 4 |
| `load.obj` | 97 |
| `nop` | 11 |
| `purge` | 17 |
| `rdelete` | 5 |
| `remote` | 6 |
| `return` | 31 |
| `say` | 205 |
| `send` | 52 |
| `set` | 45 |
| `switch` | 3 |
| `teleport` | 5 |
| `unset` | 1 |
| `unsupported:at_command` | 3 |
| `unsupported:beam` | 1 |
| `unsupported:bow` | 3 |
| `unsupported:cackle` | 1 |
| `unsupported:chuckle` | 1 |
| `unsupported:clap` | 1 |
| `unsupported:close` | 3 |
| `unsupported:d` | 1 |
| `unsupported:damage` | 5 |
| `unsupported:dance` | 2 |
| `unsupported:detach` | 1 |
| `unsupported:dg_cast` | 12 |
| `unsupported:door_purge` | 1 |
| `unsupported:drink` | 1 |
| `unsupported:drop` | 2 |
| `unsupported:em` | 1 |
| `unsupported:extract` | 4 |
| `unsupported:frown` | 2 |
| `unsupported:gaze` | 1 |
| `unsupported:give` | 34 |
| `unsupported:glare` | 2 |
| `unsupported:grin` | 2 |
| `unsupported:growl` | 1 |
| `unsupported:junk` | 2 |
| `unsupported:lock` | 3 |
| `unsupported:look` | 1 |
| `unsupported:mfollow` | 1 |
| `unsupported:mkill` | 1 |
| `unsupported:mremember` | 1 |
| `unsupported:ogle` | 1 |
| `unsupported:open` | 3 |
| `unsupported:pout` | 1 |
| `unsupported:random.dir` | 4 |
| `unsupported:roll` | 2 |
| `unsupported:set_array` | 122 |
| `unsupported:shake` | 2 |
| `unsupported:sigh` | 3 |
| `unsupported:sit` | 2 |
| `unsupported:slap` | 1 |
| `unsupported:sleep` | 1 |
| `unsupported:smile` | 5 |
| `unsupported:snarl` | 1 |
| `unsupported:stand` | 3 |
| `unsupported:tell` | 5 |
| `unsupported:transform` | 1 |
| `unsupported:unlock` | 1 |
| `unsupported:up` | 1 |
| `unsupported:wake` | 1 |
| `unsupported:whisper` | 1 |
| `unsupported:wink` | 1 |
| `unsupported:yawn` | 2 |
| `wait.sec` | 194 |
| `wait.tick` | 2 |
| `wait.until` | 2 |
| `while` | 6 |
| `zoneecho` | 4 |

### Unsupported constructs (named drops)

| construct | count |
|---|---:|
| `at_command` | 3 |
| `beam` | 1 |
| `bow` | 3 |
| `cackle` | 1 |
| `chuckle` | 1 |
| `clap` | 1 |
| `close` | 3 |
| `d` | 1 |
| `damage` | 5 |
| `dance` | 2 |
| `detach` | 1 |
| `dg_cast` | 12 |
| `door_purge` | 1 |
| `drink` | 1 |
| `drop` | 2 |
| `em` | 1 |
| `extract` | 4 |
| `frown` | 2 |
| `gaze` | 1 |
| `give` | 34 |
| `glare` | 2 |
| `grin` | 2 |
| `growl` | 1 |
| `junk` | 2 |
| `lock` | 3 |
| `look` | 1 |
| `mfollow` | 1 |
| `mkill` | 1 |
| `mremember` | 1 |
| `ogle` | 1 |
| `open` | 3 |
| `pout` | 1 |
| `random.dir` | 4 |
| `roll` | 2 |
| `set_array` | 122 |
| `shake` | 2 |
| `sigh` | 3 |
| `sit` | 2 |
| `slap` | 1 |
| `sleep` | 1 |
| `smile` | 5 |
| `snarl` | 1 |
| `stand` | 3 |
| `tell` | 5 |
| `transform` | 1 |
| `unlock` | 1 |
| `up` | 1 |
| `wake` | 1 |
| `whisper` | 1 |
| `wink` | 1 |
| `yawn` | 2 |

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
