# NPC and monster fields: where each number comes from

The Librarian's **NPCs** tab renders `elements.data` only. Every offset below was
checked twice: once by parsing the shipped file with the v27 layout, and once by
comparing the offset against the layout's own field heading, so a wrong field
name cannot hide behind a plausible-looking value.

## Layout

`PerfectWorld/cfg/PW_1.4.2_v27_2.cfg` (ipasetto/MyAceEditor) is the authority for
section names, field order and widths. Walking it as

- `int32` / `float` → 4 bytes,
- `string:n` / `wstring:n` → `n` bytes (a `wstring` is UTF-16LE),
- `byte:AUTO` → variable, only in the three sections this tab never reads,

reproduces `NPC_ESSENCE` = 852 bytes and `MONSTER_ESSENCE` = 1528 bytes, and the
whole 131-section container ends exactly on the 28,613,176-byte file length.

Both sections render their text as **GBK**, not UTF-16: `Models\NPCs\npc\男npc4\男npc4.ecm`
is stored as single-byte GBK. The `Name` and `hello_msg` fields are `wstring`
(UTF-16LE) and hold real Chinese names.

## Enumerations

Three of the integer fields are enumerations, and guessing them from the values
in the data is how a viewer ends up lying. Each label below is checked against a
client or server artefact.

**`inhabit_type`** (§39@716). Not a bitmask. The server's placement switch
(`npcgenerator.cpp`, huzeming/176source, case 0–6) and the editor's own strings
(`language_en.txt` 3100–3106 in Wrechid/sELedit-) agree:

| value | label | server move environment |
| --- | --- | --- |
| 0 | Ground | ground |
| 1 | Water | water |
| 2 | Air | air |
| 3 | Ground+Water | water |
| 4 | Ground+Air | ground |
| 5 | Water+Sky | air |
| 6 | Ground+Water+Air | ground |

Shipped values: 3680 × 0, 166 × 1, 359 × 2, 2 × 3, 1 × 6.

**`short_range_mode`** (§39@604). sELedit labels `""` as *Ranged* (3130) and `1`
as *Close Range* (3131); the server turns a non-zero value into the 4 m
close-range attack distance (`pImp->_cur_item.short_range = 4.0f`). Shipped
values: 734 × 0, 3474 × 1.

**`stand_mode`** (§39@724). The header comment is byte-damaged in every mirror,
but the three names survive as 站立 / 双足站立 / 悬空站立 — standing, biped
standing, hovering. Shipped values: 1249 × 0, 2212 × 1, 747 × 2.

**`after_death`** (§39@968). The header lists a no-effect case, 自爆
(self-destruct) and a third case that the record pairs with the
`after_death_summon_monster_*` fields. Shipped values: 4131 × 0, 77 × 1, and no
record uses the third.

## Units that are not what they look like

- **`tax_rate`** (§58@208) is already a *percentage*: the file holds 0.05, 3.7616,
  8, 17 and 100. Multiplying by 100 would print 376% for a black-market vendor.
- **`refresh_time`** (§58@72) is a float of seconds (60, 10, 300), not a count.
- **`id_pet_egg_captured`** (§39@552) sits immediately before `life` (§39@556);
  reading life at 552 shifts every later field by one slot and reports egg ids as
  hit points.

The egg id is shown as an attribute. It is not used as the creature's icon: the
egg's artwork belongs to the pet system, and neither section carries a
`file_icon` field of its own.

## What cannot be resolved from `elements.data`

Monster skill ids (`id_skill`, `skills_1..32`) and the pet-egg skill lists name
skills defined in `configs.pck`. Without the client's `skillstr.txt` the tab
prints the id and the level and does not invent a name. Levels in the shipped
data run 0–15 across 221 distinct ids.

Drop tables reference 71,733 item ids. 71,252 resolve to an item template in the
same file; the 475 references to `3365` and the 6 references to `14145` do not,
and the tab says so rather than dropping them silently.
