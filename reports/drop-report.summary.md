# tbaport drop report summary

- tool: `tbaport 0.1.0`
- mode: `full`
- zones: 189
- rooms: 12700
- npcs: 3705
- items: 4765
- zone commands: 18513
- deferred: 0
- dangling: 15
- unmapped: 15742
- global permanent drops: 11
- DG triggers emitted: 1461
- DG trigger owners: 1295 (1705 links)

## Unmapped drops by reason

| reason (`kind`) | count |
|---|---:|
| `affect_location` | 40 |
| `exit_description_only` | 108 |
| `item_action_description` | 38 |
| `item_affect_flag` | 6 |
| `item_extra_descriptions` | 2497 |
| `mob_aff` | 2069 |
| `mob_espec` | 954 |
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
| `trigger_attach_type_mismatch` | 8 |
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

## DG trigger attachments (#27)

- owners: 1295
- links: 1705
  - `item`: 224
  - `npc`: 799
  - `room`: 682
- zone-reset `T` folded into prototypes: 1 (0 already listed); deviation: the C attaches per instance at each reset
- unattached triggers: 432
- attach_type mismatches (kept, named drops): 8
- `V` reset commands (named drops): 0

## DG trigger corpus (#15)

Counts describe the emitted scripts (colour codes stripped), so they can differ from a count over the raw `.trg` bodies.

- emitted records: 1461
- script lines: 26305
- lines over the substitution limit: 0

### Records by attach type

| attach type | count |
|---|---:|
| `mob` | 795 |
| `obj` | 282 |
| `wld` | 384 |

### Lines by class

| class | count |
|---|---:|
| `comment` | 1809 |
| `control` | 8085 |
| `dynamic_command` | 71 |
| `magic` | 395 |
| `mud_command` | 2668 |
| `script_command` | 4985 |
| `statement` | 8247 |
| `unknown_command` | 45 |

### Control lines

| keyword | count |
|---|---:|
| `break` | 1195 |
| `case` | 1509 |
| `default` | 137 |
| `done` | 286 |
| `else` | 723 |
| `elseif` | 454 |
| `end` | 1745 |
| `if` | 1749 |
| `switch` | 161 |
| `while` | 126 |

### Statements

| statement | count |
|---|---:|
| `attach` | 14 |
| `context` | 13 |
| `detach` | 26 |
| `dg_affect` | 59 |
| `dg_cast` | 336 |
| `eval` | 1073 |
| `extract` | 18 |
| `global` | 338 |
| `halt` | 168 |
| `makeuid` | 2 |
| `nop` | 106 |
| `rdelete` | 135 |
| `remote` | 178 |
| `return` | 360 |
| `set` | 3845 |
| `unset` | 38 |
| `wait` | 1933 |

### `wait` forms

| form | count |
|---|---:|
| `dynamic` | 3 |
| `pulses` | 179 |
| `seconds` | 1706 |
| `ticks` | 3 |
| `until` | 42 |

### Script commands (top 15 of 46)

| command | count |
|---|---:|
| `osend` | 637 |
| `wecho` | 621 |
| `mecho` | 430 |
| `wsend` | 382 |
| `mload` | 379 |
| `wdoor` | 290 |
| `msend` | 279 |
| `oechoaround` | 226 |
| `oecho` | 203 |
| `wechoaround` | 163 |
| `mechoaround` | 142 |
| `oforce` | 102 |
| `wload` | 95 |
| `mpurge` | 94 |
| `opurge` | 94 |

### Structural errors

30 errors in 15 scripts.

| trigger | errors |
|---:|---|
| 1106 | line 8: 'switch' without 'done'; line 9: 'else' without 'if'; line 10: 'switch' without 'done'; line 11: 'else' without 'if'; line 12: 'switch' without 'done'; line 13: 'else' without 'if'; line 14: 'switch' without 'done'; line 55: 'done' without 'while'; line 56: 'done' without 'while'; line 57: 'done' without 'while'; line 58: 'done' without 'while' |
| 1133 | line 9: 'if' without 'end'; line 105: 'else' without 'if'; line 107: 'end' without 'if' |
| 1292 | line 2: 'while' without 'done' |
| 1324 | line 18: 'if' without 'end' |
| 1338 | line 1: 'if' without 'end' |
| 1385 | line 1: 'if' without 'end'; line 3: 'if' without 'end' |
| 1939 | line 1: 'if' without 'end' |
| 1959 | line 11: 'done' without 'while' |
| 2737 | line 1: 'if' without 'end' |
| 4601 | line 8: 'else' without 'if'; line 10: 'end' without 'if' |
| 8315 | line 7: 'end' without 'if' |
| 13005 | line 184: 'done' without 'while' |
| 21101 | line 4: 'switch' without 'done'; line 29: 'end' without 'if' |
| 34302 | line 3: 'switch' without 'done' |
| 55513 | line 2: 'if' without 'end' |

### Unknown object commands (top 14 of 14 words)

| word | lines | triggers |
|---|---:|---|
| `load` | 3 | 11712 |
| `"kamehameha!!!!"` | 1 | 10330 |
| `adjust,` | 1 | 10308 |
| `and` | 1 | 10330 |
| `blinded` | 1 | 10308 |
| `down.` | 1 | 11837 |
| `great` | 1 | 313 |
| `honorable` | 1 | 313 |
| `ki` | 1 | 10330 |
| `light` | 1 | 10308 |
| `say` | 1 | 5794 |
| `sending` | 1 | 10330 |
| `those` | 1 | 313 |
| `you,` | 1 | 10308 |

### Unknown world commands (top 15 of 24 words)

| word | lines | triggers |
|---|---:|---|
| `awakend` | 2 | 10362, 10364 |
| `much` | 2 | 1425 |
| `tobacco,` | 2 | 1425 |
| `tobacco.` | 2 | 1425 |
| `trigger` | 2 | 26 |
| `%send%` | 1 | 332 |
| `and` | 1 | 1425 |
| `batch` | 1 | 1425 |
| `but` | 1 | 1425 |
| `commands:` | 1 | 26 |
| `excellent` | 1 | 1425 |
| `flavor.` | 1 | 1425 |
| `for` | 1 | 1425 |
| `funny` | 1 | 1425 |
| `matches` | 1 | 1425 |

### Dynamic commands (top 15 of 17 words)

| word | lines | triggers |
|---|---:|---|
| `%snd%` | 26 | 1212 |
| `%look%` | 10 | 23606 |
| `%random.dir%` | 7 | 41, 174 |
| `%answer_no%` | 4 | 1298 |
| `%answer_yes%` | 4 | 1298 |
| `%passdown%` | 4 | 23606 |
| `%passup%` | 4 | 23606 |
| `%echoround%` | 2 | 62, 65 |
| `%kill%` | 2 | 23701, 23702 |
| `%actor.class(cleric)%` | 1 | 1137 |
| `%actor.hisher%` | 1 | 10308 |
| `%answer_reward%` | 1 | 1298 |
| `%context%` | 1 | 11852 |
| `%echaround%` | 1 | 10410 |
| `%speech%` | 1 | 11836 |

### Mob commands for the game (2668 lines)

Supported words: 42; unsupported words: 94 (top 15 below).

| unsupported word | count |
|---|---:|
| `smile` | 38 |
| `nod` | 11 |
| `bow` | 8 |
| `yawn` | 8 |
| `dance` | 6 |
| `hug` | 6 |
| `sigh` | 6 |
| `growl` | 5 |
| `kiss` | 5 |
| `s` | 5 |
| `wink` | 5 |
| `chuckle` | 4 |
| `clap` | 4 |
| `damage` | 4 |
| `fol` | 4 |

### Substitution fields (5545 references)

A name that several tables know is counted in each of them.

| table | distinct names | references |
|---|---:|---:|
| text function | 7 | 443 |
| character | 57 | 4393 |
| object | 24 | 3356 |
| room | 14 | 2730 |
| unknown or global | 34 | 83 |

Unknown or global names (top 15 of 34):

| name | count |
|---|---:|
| `tba_trial_vnum` | 20 |
| `roomstocheck` | 13 |
| `8^` | 6 |
| `on_quest_zone_1` | 4 |
| `zn118_knifestart` | 3 |
| `age` | 2 |
| `originset` | 2 |
| `remotes` | 2 |
| `set_life_counter` | 2 |
| `teleporter_return_room` | 2 |
| `zn118_a` | 2 |
| `zn118_scarredquest` | 2 |
| `zn118_tunnelquest` | 2 |
| `6^` | 1 |
| `catched` | 1 |

### Variables read but never provided or assigned (top 15 of 166 names)

| variable | count |
|---|---:|
| `cardpos` | 25 |
| `llc` | 7 |
| `lmc` | 7 |
| `lrc` | 7 |
| `mlc` | 7 |
| `mmc` | 7 |
| `mrc` | 7 |
| `ulc` | 7 |
| `umc` | 7 |
| `urc` | 7 |
| `reader1` | 6 |
| `1999_age` | 5 |
| `1999_recip` | 5 |
| `reader2` | 4 |
| `points` | 3 |

## Global permanent drops

- `combat_skill` / `assist`: Circle class ability `assist`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `backstab`: Circle class ability `backstab`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `bash`: Circle class ability `bash`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `consider`: Circle class ability `consider`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `kick`: Circle class ability `kick`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `combat_skill` / `rescue`: Circle class ability `rescue`; implemented as tba pack gameplay (issue #25), not lost by conversion
- `no_source_field` / `room coord`: tbaMUD `struct room_data` has no coordinates; `RoomDef::coord` is always `None`
- `out_of_scope` / `lib/misc/socials`: legacy old-format social file; `CONFIG_NEW_SOCIALS` is YES, so the run-time reads `lib/misc/socials.new` (issue #17)
- `out_of_scope` / `lib/text/credits`: the credit text is served by the engine's `credits` command from the pack's hand-authored `stdlib.credits` section (MIB #22), not as a help topic: a topic id equal to a command name is unreachable
- `out_of_scope` / `lib/text/help`: legacy `help.hlp` database; the generated command catalog plus the converted `lib/text` and `lib/misc/messages` topics supersede it, except its `CIRCLEMUD` entry, which is preserved as the `circlemud` topic for the CircleMUD licence (issue #18)
- `social_code` / `$i`: tbaMUD `perform_act` has no `$i` case; the code is preserved in `tba.socials` and expands to nothing at run time
