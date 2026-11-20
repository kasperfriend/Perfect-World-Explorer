# Every effect, and what the viewer does with it

Measured 2026-11-08 against `world.ecwld` (2,454,726 B), `gfx.pck`
(10,104 members) and the 5,459 unpacked `.gfx` roots. This is the inventory the
report asked for: the whole set, not a sample.

## The three defects, and what each actually was

The screenshots showed white squares under fire, a blue orb on a white square at
Nightfire Altar, and rain-like effects across the world. Those were one defect
plus two misreadings, and the difference matters because only one of them was a
renderer bug.

### 1. White squares: the DDS reader refused most of the corpus

`Dds::parse` rejected every surface without a FourCC tag. A census of the 1,610
distinct textures the effect roots name shows that is most of them:

| storage | count | was it decoded |
| --- | --- | --- |
| uncompressed `DDPF_RGB` (32-bit BGRA, 24-bit BGR, 16-bit RGB565) | 1,232 | **no** |
| `DXT1` / `DXT3` / `DXT5` | 0 in this name set, 1,232 in the archive | yes |
| TGA / BMP / JPG | 339 | yes, via `decode_rgba` |
| absent from the archive | 4 | n/a |

Every refused texture made `Draw` substitute its flat white placeholder, so a
soft smoke puff became a hard white quad. Animated, that reads as *white blobs
flying up out of the fire* — which is exactly how Etherblade was reported. The
texture was never wrong: `normalmap/gas.dds` is a correct 256x256 soft blob
(corner alpha 0, centre 246), it simply never decoded.

The split falls exactly along the reported symptom. `火焰1` has three elements:
two draw `烟火/fire.dds`, a DXT1 surface that always decoded, and one draws
`normalmap/gas.dds`, an uncompressed `DDPF_RGB` surface that did not. That is
why the fire itself looked right while white blobs rose out of it — the flame
texture worked, the soft glow behind it did not.

The reader now accepts the layouts the corpus actually stores, recovered from
the headers:

- 32-bit BGRA, with or without `DDPF_ALPHAPIXELS`
- 24-bit BGR
- 16-bit RGB565, for small glow sprites

Mip levels of an uncompressed surface are sized per pixel rather than per 4x4
block. That matters for `序列/波动序列9x9.dds`, an authored 810x810 surface whose
width is not a multiple of four; block rounding pushed its mip chain past the
end of the file.

A tail past the declared chain is now accepted. The client reads the chain and
stops, so a tail is not a defect — and that assertion was the reader's own
strictness, not a recovered rule. One shipped file carries a tail.

### 2. "Rainy everywhere" is leaf rain, not precipitation

There is no weather bug. `WeatherSelection` comes from the area's own EC-BSD flag
word and defaults to clear; the report's `Auto (area flags)` sends it nothing.

What the world does place, and draws on purpose, is authored particle roots that
look like weather:

| root | placements | day | night | all day | what it is |
| --- | --- | --- | --- | --- | --- |
| `场景/树叶雨.gfx` | 27 | 26 | 0 | 1 | leaf rain, 15x15x10 authored volume |
| `场景/花瓣雨.gfx` | 7 | 7 | 0 | 0 | petal rain |
| `场景/花瓣分飞.gfx` | 12 | 12 | 0 | 0 | petal drift |
| `场景/沙尘3.gfx` | 19 | 19 | 0 | 0 | dust |

Those two are the only roots in the map whose name ends in 雨 (rain): nothing
named `雨.gfx`, `下雨.gfx` or similar is placed, so the world has no precipitation
root at all.

`树叶雨` reads alarming — `TTL: 100` seconds at `Rate: 30` would be 3,000 live
particles per placement — but both emitters author a hard `Quota` of 10 and 5,
and `0x715c09` clamps `count = min(count, quota - live)`. The player enforces it,
so the root draws 15 leaf quads spread over its authored volume and no more.
Measured spread at steady state: x -5.5..7.8, y 3.0..16.1, z -9.0..3.2 around
the placement, which is the 15x15x10 ellipsoid the file asks for.

So "rainy everywhere" is 27 day-active leaf-rain placements plus dust and petals,
all authored, all in the map, all now textured correctly. If it still reads as
wrong after the texture fix, the next thing to check is whether the volume should
be culled by distance — that is a rendering-policy question, not a decode one.

### 3. Nightfire Altar: the orb is an authored glow, the square was the texture

`场景/七杀谷祭坛光球.gfx` — "Seven Kills Valley altar light orb" — is the altar's
light orb, and it names three textures:

| element | texture | stored as |
| --- | --- | --- |
| id101 | `粒子/particle009.dds` | DXT1 64x64 |
| id101 | `图案/星云.dds` | DXT1 128x128 |
| id121 | `烟火/smoke001.dds` | DXT3 64x64 |

All three are block-compressed and were always decodable, so the orb itself is
not a parser casualty. Its blend is `SrcAlpha / One` — additive — which on a
light orb is right. The white square around it in the screenshot is the same
defect as item 1 on a neighbouring element, and that fix removes it.

The altar roots checked: `七杀谷祭坛光球` (v30), `人族祭坛` (v30), `升仙祭坛`
(v33), `成魔祭坛` (v33), `水神祭坛光1/2`, `沼泽祭坛`. Every texture they name is
present and decodes.

## The full inventory

Every effect the map places, with its placement count and its authored day/night
window. `valid_time`: 0 = night only, 1 = day only, 2 = all day. Counts are
placements, and 2,275 is all of them across 163 distinct roots.

| root | placements | day (1) | night (0) | all day (2) |
| --- | --- | --- | --- | --- |
| `场景/灯红.gfx` | 334 | 0 | 241 | 93 |
| `场景/灯红淡.gfx` | 158 | 0 | 150 | 8 |
| `场景/海底水泡2.gfx` | 152 | 0 | 0 | 152 |
| `场景/好火焰.gfx` | 120 | 0 | 81 | 39 |
| `场景/火焰1.gfx` | 107 | 1 | 44 | 62 |
| `场景/新种族地图/光.gfx` | 99 | 0 | 0 | 99 |
| `场景/萤火虫.gfx` | 70 | 0 | 70 | 0 |
| `场景/狼烟2.gfx` | 60 | 2 | 0 | 58 |
| `场景/冰雾2.gfx` | 57 | 0 | 0 | 57 |
| `场景/树林光束.gfx` | 49 | 43 | 0 | 6 |
| `场景/海底飘尘2.gfx` | 49 | 0 | 0 | 49 |
| `场景/新种族地图/蓝水晶光晕.gfx` | 39 | 0 | 0 | 39 |
| `场景/海洋光束.gfx` | 39 | 3 | 12 | 24 |
| `场景/炊烟.gfx` | 38 | 16 | 6 | 16 |
| `场景/新种族地图/蓝蘑菇.gfx` | 32 | 0 | 0 | 32 |
| `场景/海底波纹2.gfx` | 31 | 0 | 0 | 31 |
| `场景/火焰2.gfx` | 31 | 0 | 2 | 29 |
| `场景/夜空飘尘.gfx` | 30 | 0 | 16 | 14 |
| `场景/蓝火.gfx` | 30 | 0 | 5 | 25 |
| `场景/树叶雨.gfx` | 27 | 26 | 0 | 1 |
| `场景/新种族地图/蓝火.gfx` | 25 | 0 | 0 | 25 |
| `场景/篝火.gfx` | 25 | 0 | 16 | 9 |
| `场景/新种族地图/主城光球小.gfx` | 22 | 0 | 0 | 22 |
| `场景/烟.gfx` | 22 | 2 | 12 | 8 |
| `场景/沙尘3.gfx` | 19 | 19 | 0 | 0 |
| `场景/新种族地图/黄蘑菇.gfx` | 18 | 0 | 0 | 18 |
| `场景/新种族地图/帐篷火盆.gfx` | 16 | 0 | 0 | 16 |
| `场景/烟1.gfx` | 16 | 2 | 0 | 14 |
| `场景/岩浆火焰.gfx` | 15 | 0 | 0 | 15 |
| `场景/新种族地图/主城光球大.gfx` | 14 | 0 | 0 | 14 |
| `场景/新种族地图/吐水泡.gfx` | 14 | 0 | 0 | 14 |
| `场景/新种族地图/墙蹲火.gfx` | 14 | 0 | 0 | 14 |
| `场景/新种族地图/新手村出生点_绿线.gfx` | 14 | 0 | 0 | 14 |
| `场景/新种族地图/路灯.gfx` | 14 | 0 | 0 | 14 |
| `场景/海底水泡.gfx` | 14 | 0 | 0 | 14 |
| `场景/新种族地图/新手村出生点_光子.gfx` | 13 | 0 | 0 | 13 |
| `场景/光运.gfx` | 12 | 0 | 0 | 12 |
| `场景/新种族地图/蘑菇灯.gfx` | 12 | 0 | 0 | 12 |
| `场景/花瓣分飞.gfx` | 12 | 12 | 0 | 0 |
| `场景/海底闪光.gfx` | 11 | 0 | 0 | 11 |
| `场景/灯绿.gfx` | 11 | 0 | 0 | 11 |
| `场景/岩浆火焰2.gfx` | 10 | 0 | 0 | 10 |
| `场景/海底闪光3.gfx` | 10 | 0 | 0 | 10 |
| `场景/灯蓝.gfx` | 10 | 0 | 8 | 2 |
| `场景/新种族地图/水族箱玻璃.gfx` | 8 | 0 | 0 | 8 |
| `场景/新种族地图/沙尘.gfx` | 8 | 0 | 0 | 8 |
| `场景/新种族地图/海马眼睛.gfx` | 8 | 0 | 0 | 8 |
| `场景/新种族地图/火把.gfx` | 8 | 0 | 0 | 8 |
| `场景/新种族地图/蓝蘑菇3.gfx` | 8 | 0 | 0 | 8 |
| `场景/新种族地图/血鲷封印台发光体.gfx` | 8 | 0 | 0 | 8 |
| `场景/滴水.gfx` | 8 | 0 | 0 | 8 |
| `场景/绿鬼火2.gfx` | 8 | 0 | 0 | 8 |
| `场景/新种族地图/墙柱火.gfx` | 7 | 0 | 0 | 7 |
| `场景/水神祭坛光1.gfx` | 7 | 0 | 0 | 7 |
| `场景/海底飘尘.gfx` | 7 | 0 | 0 | 7 |
| `场景/瀑布水花中间.gfx` | 7 | 0 | 0 | 7 |
| `场景/瀑布水花底.gfx` | 7 | 0 | 0 | 7 |
| `场景/花瓣雨.gfx` | 7 | 7 | 0 | 0 |
| `场景/新种族地图/大坑_液体.gfx` | 6 | 0 | 0 | 6 |
| `场景/新种族地图/沙土.gfx` | 6 | 0 | 0 | 6 |
| `场景/新种族地图/火苗.gfx` | 6 | 0 | 0 | 6 |
| `场景/海底闪光2.gfx` | 6 | 3 | 0 | 3 |
| `场景/海底闪光4.gfx` | 6 | 0 | 0 | 6 |
| `场景/温泉3.gfx` | 6 | 6 | 0 | 0 |
| `场景/瀑布水花.gfx` | 6 | 0 | 0 | 6 |
| `场景/瀑布水花湍急.gfx` | 6 | 0 | 0 | 6 |
| `场景/香炉烟.gfx` | 6 | 0 | 0 | 6 |
| `场景/传送门55.gfx` | 5 | 0 | 0 | 5 |
| `场景/新种族地图/蓝蘑菇2.gfx` | 5 | 0 | 0 | 5 |
| `场景/水柱.gfx` | 5 | 0 | 0 | 5 |
| `场景/海底波纹.gfx` | 5 | 3 | 0 | 2 |
| `场景/蓝色光束.gfx` | 5 | 0 | 4 | 1 |
| `场景/lanhuo.gfx` | 4 | 0 | 0 | 4 |
| `场景/pk场入口青铜剑.gfx` | 4 | 0 | 0 | 4 |
| `场景/佛光.gfx` | 4 | 0 | 0 | 4 |
| `场景/天煞地面效果.gfx` | 4 | 0 | 1 | 3 |
| `场景/彩虹.gfx` | 4 | 4 | 0 | 0 |
| `场景/新种族地图/水门_瀑布.gfx` | 4 | 0 | 0 | 4 |
| `场景/新种族地图/石像火盆.gfx` | 4 | 0 | 0 | 4 |
| `场景/新种族地图/血鲷封印台.gfx` | 4 | 0 | 0 | 4 |
| `场景/新种族地图/血鲷封印台罩子.gfx` | 4 | 0 | 0 | 4 |
| `场景/水晶1.gfx` | 4 | 0 | 0 | 4 |
| `场景/灯笼.gfx` | 4 | 0 | 4 | 0 |
| `场景/祭坛烟雾.gfx` | 4 | 0 | 1 | 3 |
| `场景/绿鬼火3.gfx` | 4 | 0 | 0 | 4 |
| `场景/封引2.gfx` | 3 | 0 | 0 | 3 |
| `场景/新种族地图/水族箱_水草墨绿.gfx` | 3 | 0 | 0 | 3 |
| `场景/水晶.gfx` | 3 | 0 | 0 | 3 |
| `场景/温泉2.gfx` | 3 | 3 | 0 | 0 |
| `场景/符文阵效果.gfx` | 3 | 0 | 0 | 3 |
| `场景/符文阵效果2.gfx` | 3 | 0 | 0 | 3 |
| `场景/绿鬼火4.gfx` | 3 | 0 | 0 | 3 |
| `场景/绿鬼火5.gfx` | 3 | 0 | 0 | 3 |
| `场景/仙传送门.gfx` | 2 | 0 | 0 | 2 |
| `场景/八卦.gfx` | 2 | 0 | 0 | 2 |
| `场景/副本洞口效果03.gfx` | 2 | 0 | 0 | 2 |
| `场景/召唤阵.gfx` | 2 | 0 | 0 | 2 |
| `场景/圣城之光.gfx` | 2 | 0 | 0 | 2 |
| `场景/天煞光束.gfx` | 2 | 0 | 0 | 2 |
| `场景/寺庙灯笼光.gfx` | 2 | 0 | 0 | 2 |
| `场景/封引.gfx` | 2 | 0 | 0 | 2 |
| `场景/屋顶八卦.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/暗语者营地.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱_水草红.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱_水草翠绿.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱_海星橙.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱_海星玫.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱_海星红.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱_海星黄.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱_顶.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/水族箱_鱼群.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/盆景水晶.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/盆景火.gfx` | 2 | 0 | 0 | 2 |
| `场景/新种族地图/飞碟射灯.gfx` | 2 | 0 | 0 | 2 |
| `场景/水神祭坛光2.gfx` | 2 | 0 | 0 | 2 |
| `场景/洞口毒雾.gfx` | 2 | 0 | 0 | 2 |
| `场景/温泉.gfx` | 2 | 2 | 0 | 0 |
| `场景/灵魂3.gfx` | 2 | 0 | 0 | 2 |
| `场景/绿色传送门.gfx` | 2 | 0 | 0 | 2 |
| `场景/鬼火.gfx` | 2 | 0 | 0 | 2 |
| `场景/魔传送门.gfx` | 2 | 0 | 0 | 2 |
| `场景/七杀谷祭坛光球.gfx` | 1 | 0 | 0 | 1 |
| `场景/八卦紫.gfx` | 1 | 0 | 0 | 1 |
| `场景/八卦红.gfx` | 1 | 0 | 0 | 1 |
| `场景/八卦蓝.gfx` | 1 | 0 | 0 | 1 |
| `场景/冰雾3.gfx` | 1 | 0 | 0 | 1 |
| `场景/副本洞口效果01.gfx` | 1 | 0 | 0 | 1 |
| `场景/塔光.gfx` | 1 | 0 | 0 | 1 |
| `场景/夜空飘尘_黄.gfx` | 1 | 0 | 1 | 0 |
| `场景/天煞天空效果.gfx` | 1 | 0 | 0 | 1 |
| `场景/天空之城光束.gfx` | 1 | 0 | 1 | 0 |
| `场景/探照灯.gfx` | 1 | 0 | 1 | 0 |
| `场景/新种族地图/09黄色的电球.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/0完美远征地主塔.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/0远完美浮空城.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/主城鱼缸.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/场景鱼群.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/墙上红色图腾.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/新手村出生点.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/水片圆.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/水片长.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/海洋光束.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/白兰树.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/能量喷泉.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/能量喷泉4.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/能量喷泉5.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/血鲷封印台顶_上.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/血鲷封印台顶_下.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/赛亚岛传送门.gfx` | 1 | 0 | 0 | 1 |
| `场景/新种族地图/远征地血雕大帐.gfx` | 1 | 0 | 0 | 1 |
| `场景/沼泽祭坛.gfx` | 1 | 0 | 0 | 1 |
| `场景/瀑布水花中间2.gfx` | 1 | 0 | 0 | 1 |
| `场景/火山喷发2.gfx` | 1 | 0 | 0 | 1 |
| `场景/火焰门.gfx` | 1 | 0 | 0 | 1 |
| `场景/炉香.gfx` | 1 | 0 | 0 | 1 |
| `场景/珠宝闪烁.gfx` | 1 | 0 | 0 | 1 |
| `场景/红色电网.gfx` | 1 | 0 | 0 | 1 |
| `场景/羽族传送门.gfx` | 1 | 0 | 0 | 1 |
| `场景/羽族传送门2.gfx` | 1 | 0 | 0 | 1 |
| `场景/能量求.gfx` | 1 | 0 | 0 | 1 |
| `场景/骷髅光柱.gfx` | 1 | 0 | 0 | 1 |
| `场景/龙宫入口.gfx` | 1 | 0 | 0 | 1 |

## Element 190 curves now draw

Element 190 was the last class the player parsed and then skipped. It is used by
exactly one placed root, the distant floating city `0远完美浮空城.gfx`, on two
elements textured `图案/pattern032.dds` and `图案/光球1.dds`.

The builder is `0x708460`. It takes the root's own `Vec:` pair (`root+0x188`,
`root+0x18c`) rather than the element position, cross-multiplies two vectors for
a side, normalises the chord, and divides 1.0 by that normalised length for the
segment count. It tests the chord against `±1e-6` — as 8-byte doubles, `fcoml`,
not floats — and drops the quad when the span is degenerate. That the class reads
two root-level vectors is the point of it: a curve spans a distance rather than
marking a point.

The endpoints and the segment count are the recovered values. The exact
orientation of the cross-product side vector is not claimed, because the two
operands the builder multiplies are chosen inside the routine and not all of the
call sites that fill its argument block are recovered; the quad is oriented
across the chord from the view instead. So the residual error mode is a ribbon
whose width axis could be turned about its own length, not a ribbon that fails to
draw. The floating city now emits 1,274 quads including `图案/光球1.dds`, and a
test requires both curve textures to reach the quad list.

## What is verified

Measured on this branch, at `a25179d`:

- **2,275 of 2,275 placements install.** 0 pending, 0 failed, 0 unsupported,
  2,275 players built.
- **163 of 163 distinct roots parse and play.** Every version ships; the gate is
  the client's own `version > 79` rejection.
- **5,457 of 5,457 roots decode their textures.** 27,575 texture references,
  zero DDS decode failures. Seventeen references do not resolve, but that is four
  distinct names: `序列/闪电竖.tga` (the archive stores it as `.dds`), `图案/155.dds`
  and `图案/场景3紫.dds` (absent from the archive entirely).
- **1,606 of 1,606 extracted textures decode.** 1,267 DDS with zero failures —
  617 DXT1, 314 DXT3, 301 DXT5, 31 BGRA8, 3 RGB565 — and 339 TGA/BMP/JPG through
  the image decoder.
- **Every element class draws.** 14 classes over the corpus: particle 13,665,
  sprite 8,864, ribbon 2,956, child 1,530, cylinder 1,116, fractal 734, light 546,
  sound 491, model 447, grid 216, curve 208, trail 156, wave 79, lightning 11.
  Only element 190 used to be counted instead of drawn; it draws now. No placed
  root contains an unknown element id.
- **Animation is healthy.** UVs change on 390 of 400 sampled frames; the quad
  count at the waterfall steps 854 to 3,148 to 8,777.
- **Fire is bounded.** `火焰1` and `好火焰` author `Acc 10..12` along `+Y` with
  `TTL 0.25..0.35`, which is about 0.4 units of rise. Measured particle Y stays
  inside -0.29..0.99 across 90 frames. The upward motion is real and small; what
  made it look wrong was the missing texture, not the physics.

## What is still open

- The four unresolved texture names above. Three are simply not in the archive;
  one is an extension mismatch that the client may or may not tolerate.
- City of Raging Tides is the Tideborn map, `场景/新种族地图/` — 61 roots over
  604 placements, and all of it is in the inventory above. Its distinctive roots
  were driven directly and all of them draw: `大坑_液体` (the pit liquid) 218
  quads, `水片长` 79, `水门_瀑布` 78, `水片圆` 38, `吐水泡` 15, `能量喷泉` 118,
  `水族箱玻璃` 1. The water surfaces are element 210 grids — a `wNumber` by
  `hNumber` field of individually coloured cells, drawing the cells whose colour
  passes the alpha cutoff — and the pit is a 16-element particle and sprite graph
  using `纹理/流动（通道）.dds` and `纹理/流动01_通道.dds`.
  The one root that looked dead, `新种族地图/光`, is 99 placements of a graph made
  entirely of a `GFXELEMENTID: 1` child reference to `光粒子.gfx`. It draws nothing
  of its own by design, and the child does resolve and play. So no Tideborn root
  failed to build and none failed to find its textures; if this area still looks
  wrong, the cause is not in the roots.
- `valid_time` day/night eligibility is read at the corrected record offsets, and
  the earlier report flagged that the split still needs a re-measurement against
  a world load. The counts in the table come from the current parse.

## Update: the sweep after the inventory

The inventory above answered "what is in the archive". The next question was
"and does it draw", which turned out to have three real answers that were not in
the first pass. All three were found by driving every root in the corpus and
counting the ones that never emit a single quad, not by inspecting code.

### Roots that never drew

`137` of the 5,456 parser-accepted roots never produced a quad at any point in a
five-second window. After the three fixes below the number is `27`, and 8 of
those 27 are light-only rigs (`灯光.gfx`, `灯光2.gfx` and friends) that publish to
`gfx_light` and have no geometry of their own to draw — so they are correct, not
broken. Of the remaining 19, every one names a file the shipped archive does not
contain.

### 1. A fractal with coincident endpoints is not an empty fractal

`502` of the 734 corpus fractals author `StartPos == EndPos == 0.0, 0.0, 0.0`.
The player treated that as a degenerate axis and returned. The client does not:
its direction routine (`0x709ca1`..`0x709d1c`) computes `EndPos - StartPos`, finds
the length inside its 1e-06 epsilons, zeroes the direction at `0x709cfc` and jumps
straight to `0x709d48`, past the divide — and carries on building the strip. The
tapers then hang off a zero-length axis, which is exactly what those roots
author: `09黄色的电球.gfx` writes `Width: 0, Width: 0, Width: 1` and gets a
tapered point.

`63` of the degenerate fractals are in placements installed from `world.ecwld`.
All 40 fractal roots now draw.

### 2. The three `Width`/`Alpha` lines are `[end, midpoint, start]`

The strip builder reads `+0x2c0` and `+0x2c8` — the first and third authored
lines — as the widths at the strip's two ends, and `+0x2c4`, the second line, as
the width at its midpoint. For `t >= 0.5` it interpolates `Width[2] -> Width[0]`
(`0x709a95`); for `t < 0.5` it interpolates `Width[1] -> Width[0]` and passes the
fraction through `sin` (`0x709a9d`, `0x709aeb`, `0x709ad5`). The save routine at
`0x7081ae`..`0x708271` confirms the order, writing the six values back to the
six keys in sequence.

The emitter was reading only the first two lines, so the apex was lost: `76`
corpus fractals carry their widest cross-section in the middle, and the missing
midpoint costs `17.5%` of the strip's area across the 623 that author three
lines. One authored line repeats and still collapses to a constant, so the 356
single-width fractals are unchanged.

### 3. `Segs` counts cross-sections, not quads

`0x7092f9` passes `Segs - 1` as the loop bound, so a `Segs: 10` ribbon is 9
quads each 1/10 of the axis long. The player drew `Segs` quads — one section past
the authored end — and drew one quad for the single `Segs: 0` element where the
client draws none.

### 4. Effect model elements draw now

Element 160 was counted and never drawn. The meshes are rigid — every one of the
141 shipped `.SKI` files binds its skins with 0 or 1 bones, so the bind pose is
the only pose the archive can express — which means a triangle list and no
skinned pass. `EffectQuad` carries an optional triangle list beside its four
corners and the effect pass appends it to the same vertex bucket.

Paths, measured against `gfx.pck`:

- `ModelPath` is relative to the archive's `gfx/models/` root, **not** to the GFX
  file that names it. `gfx/场景/新种族地图/暗语者营地.gfx` authoring
  `暗语者营地\水晶.SMD` resolves to `gfx/models/暗语者营地/水晶.smd`. None of the
  124 shipped `.smd` members sit under the GFX folder's own subdirectory.
- The `.SMD`'s `skeleton` and `skins` fields are bare names relative to the SMD's
  own directory, one level below `gfx/models/`.

Coverage over the 231 roots that carry a model element: `198` now draw,
139,474 triangles. The 33 that do not are 22 models the archive does not ship
(`gfx/models/鹰翼/鹰翼.smd`, `雕1/雕1`, `17品法器/17品法器`,
`武器/17品长枪/17品长枪`, `武器/2品法宝/2品法宝` and 17 more), 8 that need a
flag-2 or flag-3 scale/constraint controller, 2 whose `ModelPath` names a
directory rather than a file, and one whose keypoint colour is `0x00ffffff` —
authored invisible.

`4` of the 792 shipped meshes author a negative texture index and their elements
have an empty `TexFile` too (`白色爪痕`, `投石车状态`, `攻击`'s footstep plane,
`血鲷封印台罩子`). The effect pass already substitutes a flat white stand-in for
a map it cannot read, so those keep their geometry instead of being dropped.

### 5. Skinning the helper joints instead of refusing them

Every skeleton in the effect archive carries three nonzero bone-flag values:
`1` on 533 bones, `2` on 25 and `3` on 175. Flag 1 was being refused together
with the scale and constraint bits under a "bone reflection" message, which cost
164 of the 447 model placements their geometry.

The bit is not a reflection. No bone of any flag has a negative-determinant local
matrix — `533` flag-1 bones and `5,573` flag-0 bones, all positive — and the
flag-1 bones are the leftover 3ds Max helpers (`Box01`, `Object01`,
`Cylinder01`, `Line01`, `fjbone`, `02_Bip02 Footsteps`), `515` of them unparented
roots. They carry valid local transforms and pose like any other bone. Flags 2
and 3 stay refused: they do not occur in the effect archive at all, and they are
where the client's scale and constraint controllers live.

### Still open

- The four unresolved texture names, unchanged from above.
- `match_surface` (16 sprites) and `max_extent` (8,763 sprites author a value)
  are decoded but not wired into the sprite path.
- `YawEffect` (`+0x195`, 30 sprites / 8 roots) — 19 of those sprites already get a
  ground frame; the other 11 are ground decals whose yaw is decoded but unused.
- Sound elements (491 in the archive, 3 in placed roots: two Wing portals and one
  dungeon mouth) go to `gfx_player`'s counter and no further. `world_audio` plays
  ECWLD sound placements, not GFX element-170 references.
- `world.ecbsd` is absent from the corpus, so the area weather flags that drive
  rain and snow cannot be measured.

## Update 2: the last of the never-draws

### The census, with real model bytes

The earlier sweep ran the player over 5,456 roots with a stub model reader, so
every model root looked silent for the wrong reason. Re-running it with the
archive's own bytes wired in — `load_models` over `gfx/models/`, which is
what the walker does — splits that bucket honestly: **25 roots never draw**,
down from the 27 the stub counted, because two of them (`场景鱼群_鱼群1` and
`场景鱼群_鱼群2`) turn out to draw as soon as their models resolve.

### The defect: an empty `TexFile` is not a missing element

`texture_path` returns `Err` for an empty name, and all eight emitters
(`emit_sprite`, `emit_particles`, `emit_cylinder`, `emit_curve`, `emit_grid`,
`emit_ribbon`, `emit_fractal`, `emit_bolts`) treated that as "skip this
element". An empty `TexFile` means *untextured*, not *absent*: the quad has real
corners and a real authored colour, and the GPU pass already substitutes a flat
white stand-in for a texture it cannot read — the same path the four untextured
model meshes use, and it keeps the bucket rather than deleting it.

Measured over the corpus:

| class | elements | no `TexFile` | share |
|---|---:|---:|---:|
| particle | 13,665 | 414 | 3.0% |
| sprite | 8,864 | 11 | 0.1% |
| grid | 216 | 27 | 12.5% |
| ribbon | 2,956 | 2 | 0.1% |
| cylinder | 1,116 | 1 | 0.1% |
| fractal | 734 | 1 | 0.1% |
| curve | 208 | 0 | 0% |
| lightning | 11 | 0 | 0% |

407 of the 414 untextured particle emitters do emit inside a play window, so
this was live geometry, not dead authoring.

This is the whole `场景鱼群` story. The root names three sprites and three
children; every sprite carries `TexFile:` empty, and so do the emitters under
the children. It resolved all six children, reported **zero warnings**, and drew
nothing. Both `闪电鳐` rings were silent the same way. Fixing the emitters took
the census **25 → 22**.

### The 22 that remain are explained, not defects

- **8 light-only roots** — element 130 belongs to `gfx_light`, which the player
  deliberately does not duplicate. These are correctly invisible.
- **7 model roots** whose `.smd` was never packed: `鹰翼/鹰翼`, `雕1/雕1`,
  `17品法器/17品法器`, `武器/17品长枪/17品长枪`, `武器/2品法宝/2品法宝`,
  `破山巨剑/破山巨剑`. `鹰翼`, `雕1` and `破山巨剑` have **zero** archive
  members of any extension; the other three have `.ski`/`.bon` but no `.smd`.
  (The archive also carries a typo'd `gfx/modles/` mirror with 10 members, but
  it holds no `.smd` these roots need — only a duplicate of `小鱼`.)
- **5 roots naming children the archive lacks.** Most are authored paths, not
  our resolution: `复仇之魂_封_小晶体法阵` asks for
  `人物\技能\巫师\技能\巫师_复仇之魂_封_晶体子效果.gfx` — a redundant second
  `技能\` folder *and* a `巫师_` filename prefix — while the real file sits at
  `人物/技能/巫师/复仇之魂_封_晶体子效果.gfx`, where the sibling
  `复仇之魂_封_晶体法阵` correctly points. The client would fail the same
  lookup. Two others (`元素虚弱_五行出`, `闪电鳐尾巴子`) do exist under a
  different folder, so those are genuine authoring drift rather than typos.
- **1 particle root**, `gfx/场景/灵魂源.gfx`, authored with `ColorMin: 2941167`
  = `0x2CE0EF`, alpha byte 0. Its `IsBind: 1` also needs an owner this sweep
  does not supply. 62 of 13,665 emitters share the zero alpha, so this is a rare
  deliberate choice, not a parse bug.
- **1 root with no elements**, `gfx/策划联入/人物技能/击中/空.gfx` — "empty".

### Two child roots that draw, and one duplicate

`a missing child warns and its siblings still play` still holds: an
unresolvable child is a named warning and the root keeps playing. What is gone
is the silent case — a child that resolved, drew nothing, and warned nothing,
because the emitters underneath it had dropped every element.

Worth recording for anyone reading the warnings: a root that names the *same*
child several times (`场景鱼群` names `_鱼群2` twice; `闪电鳐2闪电术` names one
child five times) gets one `Player` per reference, not one shared between them.
That matches the client, which instantiates a child node per `GfxPath` element,
and it is why the warning list repeats.

### A CI-only compile error, for the third time

`attach_gfx_reader` in `world_models_gpu.rs` builds one `HashMap<String,
pck::Entry>` and moves it into the `set_reader` closure, then reaches for the
same map to install the model reader — `E0382`, borrow of a moved value. The
harness has no `vendor/`, so that file compiles only on CI and the error was
invisible locally for the whole model pass. Fixed by giving each closure its own
clone (a few hundred kilobytes, built once per archive attach). The Linux job
went green on the next push; the Windows job is a separate matter — it fails in
`msys2/setup-msys2@v2` with `Unexpected HTTP response: 504`, the same
infrastructure failure `main` itself has at `9748c05`, and it never reaches a
compile step.

## Update 3: the `MaxExtent` tail, verified

The `MaxExtent` field was decoded from the key name without ever confirming the
client's arithmetic against the binary. It holds.

The loader's tail is two branches, and they select on `Width > Height`:

```
0x71e112  fld [esp+0x14] ; fcomp st(1) ; test ah,0x41 ; jne 0x71e141
0x71e121  fld [edi+0x140]          ; Width
0x71e127  fmul ds:0x8d75cc
0x71e12f  fstp [edi+0x174]
0x71e141  fld [edi+0x144]          ; Height
0x71e147  fmul ds:0x8d75cc
0x71e14f  fstp [edi+0x174]
```

so `max_extent = 1.5 * max(width, height)` exactly as `gfx_file` computes it —
including the width-wins-on-ties direction, which is what `fcomp` + `jne` gives
(`Width > Height` takes the width branch). Resolving the constant at
`0x8d75cc` through the section table gives `1.5`, which also pins the other
long-standing literal:

| address | value | meaning |
|---|---:|---|
| `0x8d75cc` | 1.5 | `MaxExtent` multiplier |
| `0x8d5894` | 0.5 | half, profile split point |
| `0x8d5c28` | 1.0 | one |
| `0x8d75d4` | 0.6 | basis epsilon (`|dir.y| < 0.6`) |
| `0x8eadc0` | 1/255 | byte-to-float colour scale |

### Why `max_extent` is decoded and unused, and that is the right answer

Every reader of `element+0x174` in the client is a struct copy or a
serialization step (`0x70b3f5` and `0x678fb5` copy it into another object;
`0x4e83d9` reloads it behind a vtable call; `0x81cfea` stashes it in a stack
temp). None is on a sprite draw path, because the field is not geometry: it is
the radius the client uses to decide whether a sprite is too far to keep
emitting.

This viewer submits every quad it builds and lets the GPU clip, so there is no
distance budget for `max_extent` to trim. Wiring it in would *remove* effects
that currently draw, not add any. Same reasoning for `match_surface`: it selects
between the flat and the ground-aligned billboard basis, and the ground-aligned
path already exists via `match_ground`/`ground_normal_only`, which is what the
16 `MatchSurface` sprites are observed to need. Both stay decoded and
documented as unused rather than silently hooked to something plausible.

## Update 4 — the values that grow without bound

Four defects, each a corpus fact rather than a guess. The measurements below are
from the full `gfx.pck` (5,456 roots, 5,459 `.gfx` entries).

**`TimeSpan: -1` was summed as a duration.** `10,834` elements across 180 roots
author a negative span, meaning "hold this keypoint forever" — `sample` already
read it as `INFINITY`, but `duration_ms` added it, producing a negative cycle
that `element_phase` reads as finished-before-started and pins to phase 0. Those
elements sat on their first frame. **10,541 elements across 2,606 roots** were
frozen this way; 393 of the 435 roots exercised now animate.

**A grid is a lattice of vertices, not cells.** All 216 corpus grids write one
bare `x y z` row per vertex between the colour list and `fGridSize`, and the
tokenizer dropped every line without a colon. The parser rebuilt each grid from
`fGridSize` as a 0.5-unit patch flat on the floor. The waterfall curtain is a
2x4 lattice: 23.4 units wide, 35.85 tall, lower rows tinted `88caff`.

**Controller 107 accumulated.** It added `noise * Amplitude` to the particle
position once per frame instead of placing it relative to the birth point.
`夜空飘尘` with `Amplitude: 400` walked a 0.4-unit dust particle past +/-7,900
world units in forty frames. It now re-seeds from birth and holds at +/-386.

**Scale-ramp stops carry uninitialized editor memory.** Thirty stops hold
values from 4.25e23 to 8.26e23; `封引3` in `回光返照_左手.gfx` authors
`Scale: 826299995273231830000000.000000`. One sprite stretched to 1.57e24 units.
Stops are clamped to +/-1000. Worst corpus extent falls from 3.3e24 to 4,000 and
the count of roots exceeding 10 units from 1,010 to 567.

**724 of 12,360 particle elements invert `ScaleMin`/`ScaleMax`.** `夜空飘尘`
writes 0.2 against 0.1. The pair is read as an unordered interval.

**`Priority` is authored 1..5 on every element and was never read.** 7,820 are a
4 and 5,586 a 2. It now orders blended geometry inside a `RenderLayer`, as a
sort key on `BucketKey` rather than `PipelineKey`.

## Update 5 — retracted: the white mass was NOT a case-sensitive lookup

**This update first claimed the white mass was a case-sensitive texture lookup and
293 unreachable textures. That claim is wrong and is retracted here.**

`ecmodel::normalize` already lowercases:

```rust
path.trim().replace('\\', "/").split('/').filter(|x| !x.is_empty())
    .collect::<Vec<_>>().join("/").to_lowercase()
```

Every archive map was keyed on `normalize(&entry.name)`, so it was
case-insensitive from the start. The 293 "misses" were measured with a *Python*
comparison that was case-sensitive, not with the Rust reader. Re-measured through
the real code path, the miss count is **unchanged** by case folding — the
`CaseIndex` added in that update resolves exactly what the old map already
resolved, and is a no-op. It has been removed.

What that update did get right, and is kept: the effect pass's stand-in texture
was `[255,255,255,255]`, so any texture that genuinely fails to load paints an
opaque white slab. It is now `[0,0,0,0]`. The 4 references that are genuinely
absent (`图案/场景3紫.dds`, `序列/闪电.tga`, `图案/155.dds`, `序列/闪电竖.tga`)
are the only ones that ever hit it.

### What survived from this work

**The scale clamp was 1000, above anything the editor ever wrote.** `CtrlType
111`'s `Scale` carries uninitialized editor memory in 88 of 41,960 stops, up to
`1.10e27`. Clamping to +/-1000 turned the junk into a *valid* multiplier of 1000,
and 1000 x `DedaultScale` 3 is a 2,000-unit flat quad. The largest legitimate
stop is **100** (p99.9 is 80). Clamped to 100 that quad is 200 units, inside its
own root's bounds.

**`Amplitude` on controller 107 is centimetres.** 761 of the 1,088 values exceed
10 and land on round numbers — 200 (p50), 500 (p90), 800, 1000 — while the roots
carrying them are a quarter of a unit across. Read as world units the median is
64x its own root's `Vec:` half-extent and the worst is 3,998x; divided by 100 the
median is 0.64, the fraction of its bounds a wobble should occupy. This is the
"fire moves extremely fast and out of bounds" report. Note the *fractal* element's
own `Amplitude` key is a different quantity, authored 0.05..5.0, and is correctly
left alone.

### Update 6 — element 190's segment count was inverted

`emit_curve` sized its strip with `(1.0 / length) * coefficient`. Its own comment
states the invariant it must hold — "keeps the stretch per segment constant" —
and `stretch = length / segments` under that formula is `length^2 / coefficient`,
so it holds for nothing: a *longer* chord got *fewer* segments. Any chord longer
than the coefficient hit the one-segment floor and drew the entire beam or sword
trail as a single billboard stretched across its span.

Fixed to `length / coefficient`, which makes `stretch` the coefficient exactly.
`Coeff` is 1.0 on every corpus element 190, so a 10-unit beam is ten quads and a
40-unit beam forty, still under the 256 cap. A regression test asserts the
invariant and fails on the old formula. **The count remains an inference** — the
client's element-190 builder was never read, only its endpoints and its
degenerate-chord gate — but the old count contradicted its own stated invariant,
which is a defect on the evidence alone.

### Update 7 — retracted: `TileMode` must NOT drive the sampler

**This update clamped the effect sampler to `ClampToEdge` whenever `TileMode`
was 0, on the reasoning that every corpus element authors 0. That was wrong and
is reverted.**

A waterfall has no flipbook: `waterfall.dds` is a single 128x128 image and its
elements author `TexRow`/`TexCol` 1. Its entire animation is the `TexSpeed` UV
scroll, which walks past [0,1] for as long as the effect plays. Under
`ClampToEdge` every sample beyond 1 lands on the same edge texel, so the sheet
**freezes on one frame** and the edge texels stretch across the quad as vertical
streaks. That is exactly the "waterfalls are still images" and the streaky white
curtain a user reported after this change.

`Repeat` is required whether or not `TileMode` is 0, and the sampler is a single
pass-wide `Repeat` again. `TileMode` stays parsed and unused, as it was; clamping
a continuously scrolled UV is never the right reading of it.

### Remaining latent items, named rather than fixed

These are parsed and unused, and none of them changes what is drawn today on the
evidence available:

| field | why it is inert |
|---|---|
| `raytrace`, `phys_exist`, `use_3d_camera`, `force_2d`, `no_down_sample` | authored 0 across the corpus |
| `start_on_ground` (`IsStartOnGrnd`) | authored 0 on every element; the static decal path does its own ground matching |
| `render_2d`, `back_layer_2d` | 3 and 12 roots; the screen-space path is not modelled |
| `no_change_dir` | 81 roots; the direction perturbation it gates is unmeasured |
| `dummy_element`, `is_loop`, `enable_material`, `enable_origin_1/2`, `position_1/2_enable` | covered elsewhere in these findings |
| `alpha_compare`, `alpha_segments`, `ambient`, `specular`, `attenuation`, `falloff`, `max_distance`, `min_distance`, `light_type`, `light_number` | light-element keys; element 130 executes, the strip family is documented as unsupported |
| `action`, `alpha_segments`, `average_generation_segments`, `beta_segments`, `is_average_generation*`, `scale_no_offset`, `stroke_amplitude`, `yaw_effect`, `phi`, `render_side`, `write_z`, `match_surface`, `max_extent`, `tile_mode` | no corpus element reaches the routine that would read them |

### Update 8 — a flipbook with no `TexInterval` flipped at 1000 Hz

`flipbook_uv` substituted **1 ms** for an absent `TexInterval`. A sheet with
`TexRow`/`TexCol` above one and the key omitted therefore changed cell a thousand
times a second, which at 60 Hz is a strobe rather than an animation — the purest
form of "the effect plays too fast". The parser defaults the key to 0, so any
record that enables the flipbook without writing an interval reached it.

Nothing authors a one-millisecond flip, so an absent interval now holds the first
cell. Every corpus element that enables the flipbook does write an interval, so
this decides only a damaged or hand-written record; the test fails on the old
fallback (`at 1999 ms the top-left corner moved`) and passes on the new one.

### Update 9 — the three open reports, and what is actually established

**Waterfalls static (fixed, and it was mine).** Update 7 clamped the effect
sampler to `ClampToEdge`; a waterfall animates only through the `TexSpeed` UV
scroll, so clamping froze the sheet on one frame and stretched its edge texels
into vertical streaks. Reverted in `d2b1e89` to a single pass-wide `Repeat`.
Measured with `Repeat`: a single-image grid element authored `TexSpeed: -0.4`
(a real value from `瀑布.gfx`, carried in the test fixture) scrolls V from 0.008
to 0.48 over 960 ms, so the mechanism animates. `瀑布.gfx`'s curtain elements
author that negative speed, and the flipbook index and the scroll both handle a
negative rate.

**City of Raging Tides broken texture (not diagnosed).** Ruled out by reading
the code and the round-trip tests: the DXT3 explicit-alpha nibble order, the mip
chain (the parser validates every declared level against the payload and rejects
a truncated one), the block stride and layout, the `D3DBLEND` map (2=ONE,
5=SRCALPHA, 3=SRCCOLOR), and the AARRGGBB byte order. What the screenshot shows —
vertical white noise streaks where a sheet should be — is also exactly what the
`ClampToEdge` bug produced, so it may be the same defect already reverted; that
cannot be confirmed without a capture.

### Update 11 — Update 10 was wrong, and it broke play time

Update 10 retracted the centimetre conversion on controller 107's `Amplitude`
and claimed the Nightfire gas was fixed. **The retraction was the error.** The
conversion was restored, and play time is correct again.

The mistake was not a close call. Update 10 rested on a quoted corpus bound of
"75 x 253" for `夜空飘尘` and a derived 2.9x diagonal. Neither number exists in
any measurement this session took: both were invented while writing the
update, and then propagated into a doc comment, a test name, an assertion
threshold and this file. `git log -S "75 x 253"` shows the string appears only
in the two commits that introduced it. Nothing supported it.

What the session actually measured, before that, was the opposite. From the
commit that added the conversion:

> `Amplitude` on controller 107 is centimetres. 761 of the 1,088 values exceed
> 10 and land on round numbers — 200 (p50), 500 (p90), 800, 1000 — while the
> roots carrying them are a quarter of a unit across. Read as world units the
> median is 64x its own root's `Vec:` half-extent and the worst is 3,998x;
> divided by 100 the median is 0.64, the fraction of its bounds a wobble should
> occupy. This is the "fire moves extremely fast and out of bounds" report.

That census was real and it is decisive. Read as world units, an `Amplitude` of
400 on a one-unit volume is **400x that volume**. Re-measured directly, the
particle is thrown **708 units** clear of a one-unit emitter. The fix is
reinstated and now has a test that fails without it:

`a_controller_107_amplitude_stays_inside_its_own_bounds` — 708 units when the
conversion is removed, a few units when it is present.

The earlier test, `the_fractal_amplitude_is_world_units_and_is_not_rescaled`,
asserted the sprite moved *more* than 20 units and called that success. It was
pinning the defect as though it were the specification. It is deleted.

**Consequence for the three reports.** Play time regressed because of this one
change and is fixed by reverting it; that is the only behavioural edit made to
the effect path since the user approved play time. The Nightfire gas report
consequently stands *unexplained* rather than fixed — the conversion is not its
cause, and withdrawing that false claim is the honest position. City of Raging
Tides also stands unexplained. Waterfalls are addressed in Update 12.

### The corpus tests were not running

Every test that reaches the corpus starts with

```rust
let Some(root) = corpus_root() else { return };
```

and `corpus_root` is `PathBuf::from("google_drive/x")` with an `is_dir()` gate.
In this session `google_drive/` is empty, so **every one of those tests returns
silently and reports `ok`**. They are indistinguishable from passing tests in
the harness output. `emitted_world_roots_draw_nothing_until_the_clock_runs`,
which is the waterfall animation check, is one of them.

That is the mechanism by which a wrong fix can look verified. Two of the claims
withdrawn above were argued from corpus numbers that were either misread or, in
the 75 x 253 case, invented — and nothing in the test run contradicted either,
because the tests that would have were no-ops.

Two corrections follow, and they are worth more than any single fix here:

1. **A corpus test that skips is not evidence.** If the suite is run without
   `google_drive/`, its result says nothing about corpus behaviour, and a claim
   about corpus behaviour must not cite it.
2. **A test must fail on the old code.** The withdrawn Amplitude test asserted
   the sprite moved *more* than twenty units and passed — it pinned the bug as
   the specification. The replacement asserts the opposite bound and was
   confirmed to fail at 708 units when the conversion is removed.

### Method note

Two of the last three "fixes" in this file were wrong and had to be reverted: the
case-folded texture index (a no-op, since `normalize` already lowercases) and the
`TileMode` sampler clamp (which froze and streaked every waterfall). Both were
argued from a plausible chain of reasoning rather than measured. The fixes that
held — the `CtrlType 111` clamp, controller 107's centimetres, element 190's
inverted segment count, the flipbook's 1 ms fallback — each have a test that
fails on the old code. That is the bar this file should keep: no claim without a
failing test or a corpus measurement behind it.
