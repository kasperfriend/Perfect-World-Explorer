# 1.8.4 world map: tree and effect record census

Measured from the paired map files; the measurement date is not verified in this checkout. This is a **record-level census** of the ECWLD/EC-BSD, not a claim that all referenced assets render or that the 1.8.4 GFX archive was audited.

## Inputs and method

| File | Drive ID | Bytes | SHA-256 |
|---|---|---:|---|
| `world.ecwld` | `1-JLS8LwSur1r_l5_8AD9XY4yvPTPqM76` | 2,848,200 | `c7d0895b7d9aa58d13b3a153c0a9f01fcdcaee7b7d6090cf33420fbb363d3ebb` |
| `world.ecbsd` | `1fYClaA4uhX0uy4x0rgjRWDRnP5E-B-86` | 2,678,024 | `bf09a41990058b5aaedea6683bfccf2abc5c8b06c5f2b1a4d52cf05f20099f93` |

Reproduce the summary and both complete path tables with `tools/offline_harness.sh scene-census google_drive/world.ecwld google_drive/world.ecbsd`; the broader one-file reader sweep is `tools/offline_harness.sh scene google_drive`. Both use the production `src/scene.rs` parser. The paired ornament table is checked through `Scene::load_ornaments`; it accepts EC-BSD revision 18 and resolves the ECWLD references to unique EC-BSD records. Neither source file was edited or added to Git; they remain external staging inputs, not repository fixtures.

**Current-checkout revalidation (18 November 2026):** the read-only reproduction command was attempted, but `google_drive/world.ecwld` was absent and the census exited with `No such file or directory`. The counts below are therefore preserved from the earlier recorded measurement and were **not independently rerun in this checkout**. This is a source-availability limitation, not a new census result.

## Scene-level counts

| Field | Result |
|---|---:|
| ECWLD version / block grid | v17 / 22,528 blocks (176×128), block size 64 |
| Tree types / placements | 59 / 3,649 |
| Tree types with at least one placement | 59 of 59 |
| Distinct normalized SPT / composite DDS names | 57 / 37 |
| Water / grass placements | 9,047 / 39,116 |
| Grass types | 254 |
| ECWLD ornament references / unique EC-BSD records | 41,646 / 23,068 |
| Effects / distinct root paths | 3,542 / 282 |
| Static models / critters / beziers / sounds | 95 / 1,347 / 49,547 / 1,044 |

The EC-BSD reader deduplicates references by export ID, so 41,646 placement references resolving to 23,068 unique records is expected; this is not a count of BMD files. The 59 tree rows below all have positive placement counts, so there are no unused tree-table slots in this map.

## Tree types and authored placements

| Type | Placements | SPT path as authored | Composite texture as authored |
|---:|---:|---|---|
| 0 | 99 | `Trees/精简树木/雪松/SnowPineWithSnow1.spt` | `Trees/精简树木/雪松/snowleaves.dds` |
| 1 | 85 | `Trees/精简树木/枯树/PlaneKushu.spt` | `Trees/精简树木/枯树/Kushuleaves.dds` |
| 2 | 64 | `Trees/精简树木/红松/带雪红松/GiantSequoiasnow2.spt` | `Trees/精简树木/红松/带雪红松/snowgiantleaves.dds` |
| 3 | 55 | `Trees/精简树木/巨大毛榉/带雪毛榉/Beechsnow2.spt` | `Trees/精简树木/巨大毛榉/带雪毛榉/beechsnow.dds` |
| 4 | 112 | `Trees/精简树木/巨松/SnowPineBig3.spt` | `Trees/精简树木/巨松/pinebigleaves.dds` |
| 5 | 59 | `trees/精简树木/杉树/雪杉树/雪杉树4.spt` | `trees/精简树木/杉树/雪杉树/snowshanshuleaves.dds` |
| 6 | 33 | `Trees/精简树木/枯树/PlaneKushu2.spt` | `Trees/精简树木/枯树/Kushuleaves.dds` |
| 7 | 94 | `Trees/精简树木/枯树/磨牙镇枯树/PlaneKushuhigh2.spt` | `Trees/精简树木/枯树/磨牙镇枯树/planekushuleaves.dds` |
| 8 | 62 | `Trees/精简树木/红松/GiantSequoia.spt` | `Trees/精简树木/红松/sequoialeaves.dds` |
| 9 | 52 | `Trees/精简树木/杉树/杉树2.spt` | `Trees/精简树木/杉树/shashuleaves.dds` |
| 10 | 55 | `Trees/精简树木/巨大毛榉/Beech7.spt` | `Trees/精简树木/巨大毛榉/Beechleaves.dds` |
| 11 | 525 | `Trees/精简树木/修改绿色英国像树/ColumnarEnglishGreen1.spt` | `Trees/精简树木/修改绿色英国像树/greenleaves.dds` |
| 12 | 48 | `trees/精简树木/草原的树/gimlet.spt` | `trees/精简树木/草原的树/gimletleaves.dds` |
| 13 | 272 | `trees/精简树木/英国像树/columnarenglishoak3.spt` | `trees/精简树木/英国像树/englishleaves.dds` |
| 14 | 253 | `trees/精简树木/鹅卵石像树大/shingleoak4.spt` | `trees/精简树木/鹅卵石像树大/shingleoak.dds` |
| 15 | 91 | `trees/精简树木/面包树/baobab8.spt` | `trees/精简树木/面包树/baobab.dds` |
| 16 | 49 | `trees/精简树木/歪脖枯树/waibo3.spt` | `trees/精简树木/歪脖枯树/waibo.dds` |
| 17 | 22 | `Trees/精简树木/俄国橄榄/RussianOlive2.spt` | `Trees/精简树木/俄国橄榄/oliveleaves.dds` |
| 18 | 33 | `Trees/精简树木/鹅卵石像树大/鹅卵石象树/Shingleoak.spt` | `Trees/精简树木/鹅卵石像树大/鹅卵石象树/Shingleoak_leaves.dds` |
| 19 | 172 | `trees/精简树木/巨大毛榉/新毛榉/beech7.spt` | `trees/精简树木/巨大毛榉/新毛榉/beech7leaves.dds` |
| 20 | 22 | `Trees/精简树木/歪脖枯树/waibo1.spt` | `Trees/精简树木/歪脖枯树/Waibo.dds` |
| 21 | 6 | `trees/精简树木/松树小/orientalspruce2.spt` | `trees/精简树木/松树小/spruceleaves.dds` |
| 22 | 5 | `trees/精简树木/松树小/orientalspruce.spt` | `trees/精简树木/松树小/spruceleaves.dds` |
| 23 | 5 | `trees/精简树木/松树小/orientalspruce3.spt` | `trees/精简树木/松树小/spruceleaves.dds` |
| 24 | 8 | `Trees/精简树木/草原的树/Gimlet2.spt` | `Trees/精简树木/草原的树/Gimletleaves.dds` |
| 25 | 104 | `Trees/精简树木/面包树/Baobab6.spt` | `Trees/精简树木/面包树/baobab.dds` |
| 26 | 20 | `Trees/精简树木/柳树/WeepingWillow2.spt` | `Trees/精简树木/柳树/weepingleaves.dds` |
| 27 | 12 | `Trees/精简树木/大像树/ColumnarBig5.spt` | `Trees/精简树木/大像树/Compositeleaves.dds` |
| 28 | 19 | `Trees/精简树木/大像树/绿色大像树/ColumnarBig6.spt` | `Trees/精简树木/大像树/绿色大像树/Compositegreen.dds` |
| 29 | 43 | `Trees/精简树木/枯树/磨牙镇枯树/PlaneKushuhigh4.spt` | `Trees/精简树木/枯树/磨牙镇枯树/planekushuleaves.dds` |
| 30 | 74 | `Trees/精简树木/白杨/Baiyang2.spt` | `Trees/精简树木/白杨/baiyangleaves.dds` |
| 31 | 21 | `Trees/精简树木/歪脖枯树/waibo2.spt` | `Trees/精简树木/歪脖枯树/Waibo.dds` |
| 32 | 18 | `Trees/精简树木/桑树/LondonPlane_RT5.spt` | `Trees/精简树木/桑树/londonleaves.dds` |
| 33 | 20 | `Trees/精简树木/东北当归/ManchurianAngelicaTree.spt` | `Trees/精简树木/东北当归/ManchurianAngelica_Leaves.dds` |
| 34 | 79 | `Trees/精简树木/竹林/Bamboos2.spt` | `Trees/精简树木/竹林/Bamboos.dds` |
| 35 | 36 | `Trees/精简树木/大像树/绿色像树2/ColumnarBig9.spt` | `Trees/精简树木/大像树/绿色像树2/greenleaves.dds` |
| 36 | 32 | `trees/精简树木/桃树/桃树小/taoshu3small3.spt` | `trees/精简树木/桃树/桃树小/taoshuleaves.dds` |
| 37 | 143 | `Trees/精简树木/椰子树/Coconutyellow1.spt` | `Trees/精简树木/椰子树/coconutyellowleaves.dds` |
| 38 | 159 | `Trees/精简树木/椰子树/Coconutyellow4.spt` | `Trees/精简树木/椰子树/coconutyellowleaves.dds` |
| 39 | 165 | `Trees/精简树木/棕榈/Palm1.spt` | `Trees/精简树木/棕榈/palmleaves.dds` |
| 40 | 33 | `Trees/精简树木/棕榈/Palm4.spt` | `Trees/精简树木/棕榈/palmleaves.dds` |
| 41 | 3 | `Trees/精简树木/巨大毛榉/Beech9.spt` | `Trees/精简树木/巨大毛榉/Beechleaves.dds` |
| 42 | 7 | `Trees/精简树木/糖梧桐/SugarMaple.spt` | `Trees/精简树木/糖梧桐/Leaves.dds` |
| 43 | 3 | `Trees/精简树木/枯树/PlaneKushu2.spt` | `Trees/精简树木/枯树/磨牙镇枯树/planekushuleaves.dds` |
| 44 | 14 | `Trees/精简树木/鹅卵石像树大/鹅卵石象树/Shingleoak.spt` | `Trees/精简树木/鹅卵石像树大/shingleoak.dds` |
| 45 | 16 | `Trees/精简树木/菩提树/putishu1.spt` | `Trees/精简树木/菩提树/putileaves.dds` |
| 46 | 19 | `Trees/精简树木/桃树/桃树小/Taoshu3small.spt` | `Trees/精简树木/桃树/桃树小/taoshuleaves.dds` |
| 47 | 62 | `Trees/精简树木/竹林/Bamboos1.spt` | `Trees/精简树木/竹林/Bamboos.dds` |
| 48 | 51 | `Trees/精简树木/竹林/Bamboos3.spt` | `Trees/精简树木/竹林/Bamboos.dds` |
| 49 | 15 | `trees/精简树木/梨树/梨树1.spt` | `trees/精简树木/梨树/peartreeleaves.dds` |
| 50 | 26 | `Trees/精简树木/歪脖枯树/waibo4.spt` | `Trees/精简树木/歪脖枯树/Waibo.dds` |
| 51 | 17 | `Trees/精简树木/雨林树木/雨林像树垂钓/xiangshu1.spt` | `Trees/精简树木/雨林树木/雨林像树垂钓/xiangshuleaves.dds` |
| 52 | 63 | `Trees/精简树木/雨林树木/雨林树1/Sycamore.spt` | `Trees/精简树木/雨林树木/雨林树1/sycamoreleaves.dds` |
| 53 | 30 | `Trees/精简树木/雨林树木/雨林像树垂钓/xiangshu2.spt` | `Trees/精简树木/雨林树木/雨林像树垂钓/xiangshuleaves.dds` |
| 54 | 28 | `Trees/精简树木/雨林树木/雨林像树/yulinSycamore.spt` | `Trees/精简树木/雨林树木/雨林像树/yulinsycamore.dds` |
| 55 | 19 | `Trees/精简树木/鹅卵石像树大/鹅卵石象树/Shingleoak2.spt` | `Trees/精简树木/鹅卵石像树大/鹅卵石象树/Shingleoak_leaves.dds` |
| 56 | 9 | `Trees/精简树木/雨林树木/雨林树1/Sycamore2.spt` | `Trees/精简树木/雨林树木/雨林树1/sycamoreleaves.dds` |
| 57 | 5 | `trees/精简树木/鹅卵石像树大/鹅卵石象树/shingleoak3.spt` | `trees/精简树木/鹅卵石像树大/鹅卵石象树/shingleoak_leaves.dds` |
| 58 | 3 | `Trees/精简树木/菩提树/putishu3.spt` | `Trees/精简树木/菩提树/putileaves.dds` |

Positions across the 3,649 tree records span x `-4076..3879`, y `178.27295..581.4483`, z `-5409..5491`. The scene reader validates every type index against the 59-entry table. The corresponding `trees.pck` is not staged locally, so this report does **not** independently test that each SPT/DDS path resolves in the 1.8.4 archive; the previously recorded 326-entry pack/name audit remains separate evidence.

## Effect placement records

All 3,542 records parse, have distinct export IDs, positive finite scales/speeds, and unit orthogonal direction/up vectors. The maximum absolute length errors are `1.20e-7` (direction) and `1.79e-7` (up); maximum absolute normalized dot product is `3.81e-6`. Scale spans `0.054144748..380.9532`; play speed spans `0.03..12.0`.

The authored `valid_time` counts, in the `scene.rs` convention, are 180 day-only (`0`), 789 night-only (`1`) and 2,573 both (`2`); no other value occurs. The authored alpha histogram has 23 distinct values and sums to 3,542:

| Alpha | Count |
|---:|---:|
| 1 | 3040 |
| 0.7 | 99 |
| 0.5 | 76 |
| 0.3 | 71 |
| 0.8 | 67 |
| 0.6 | 45 |
| 2 | 43 |
| 0.4 | 20 |
| 0.2 | 18 |
| 5 | 14 |
| 3 | 13 |
| 0.65 | 8 |
| 2.5 | 7 |
| 1.8 | 6 |
| 0.1 | 4 |
| 2.1 | 3 |
| 0.45 | 2 |
| 0.25 | 1 |
| 0.33 | 1 |
| 0.36 | 1 |
| 0.9 | 1 |
| 1.3 | 1 |
| 1.5 | 1 |

### Every distinct root path and placement count

This table is exhaustive for the 282 distinct paths serialized in this map. It establishes only the ECWLD reference census; it does not establish that each path exists in the client GFX archive or which elements/textures each graph contains.

| Placements | GFX root path |
|---:|---|
| 2 | `场景/2012场景/船_传送门.gfx` |
| 1 | `场景/2012场景/船_传送门中心.gfx` |
| 10 | `场景/2012场景/船_火焰.gfx` |
| 4 | `场景/2013场景/圆形火台.gfx` |
| 16 | `场景/2014新资料片/主城柱子水晶球.gfx` |
| 4 | `场景/2014新资料片/主城楼顶光柱.gfx` |
| 2 | `场景/2014新资料片/主城浮石能量光柱.gfx` |
| 14 | `场景/2014新资料片/主城船体灯.gfx` |
| 1 | `场景/2014新资料片/主城船舱右.gfx` |
| 1 | `场景/2014新资料片/主城船舱左.gfx` |
| 2 | `场景/2014新资料片/主城蛇眼睛.gfx` |
| 32 | `场景/2014新资料片/天空之中走廊柱子.gfx` |
| 48 | `场景/2014新资料片/天空之中走廊通道.gfx` |
| 6 | `场景/2014新资料片/天空之城宝石圆放射光.gfx` |
| 6 | `场景/2014新资料片/天空之城宝石放射光.gfx` |
| 8 | `场景/2014新资料片/天空之城船涡轮.gfx` |
| 28 | `场景/2014新资料片/女神雕像水花.gfx` |
| 41 | `场景/2014新资料片/枯树雾气.gfx` |
| 2 | `场景/2014新资料片/柱子光柱.gfx` |
| 15 | `场景/2014新资料片/树上蓝紫色灯光.gfx` |
| 1 | `场景/2014新资料片/泉眼瀑布.gfx` |
| 4 | `场景/2014新资料片/泉眼瀑布小流水.gfx` |
| 18 | `场景/2014新资料片/泉眼瀑布正常流水.gfx` |
| 6 | `场景/2014新资料片/泉眼瀑布直体短小流水.gfx` |
| 11 | `场景/2014新资料片/浅蓝色光晕带粒子.gfx` |
| 1 | `场景/2014新资料片/石柱窗户光.gfx` |
| 32 | `场景/2014新资料片/石碑放射光.gfx` |
| 4 | `场景/2014新资料片/紫色穿透光.gfx` |
| 2 | `场景/2014新资料片/能量树干光芒.gfx` |
| 22 | `场景/2014新资料片/船上粉色宝石.gfx` |
| 16 | `场景/2014新资料片/船上能量光柱.gfx` |
| 2 | `场景/2014新资料片/船体传送光.gfx` |
| 22 | `场景/2014新资料片/船体传送光弱化版.gfx` |
| 2 | `场景/2014新资料片/船体传送光青色.gfx` |
| 6 | `场景/2014新资料片/船体蛇咬夜明珠.gfx` |
| 13 | `场景/2014新资料片/落叶大面积绿.gfx` |
| 50 | `场景/2014新资料片/落叶大面积黄.gfx` |
| 4 | `场景/2014新资料片/蓝火.gfx` |
| 14 | `场景/2014新资料片/蓝色柱子光柱.gfx` |
| 3 | `场景/2014新资料片/闪电光柱.gfx` |
| 2 | `场景/2014新资料片/雕像火球.gfx` |
| 48 | `场景/2014新资料片/龙背光柱.gfx` |
| 2 | `场景/2017/a91_6红色灯笼.gfx` |
| 3 | `场景/2021/礼花01.gfx` |
| 4 | `场景/lanhuo.gfx` |
| 4 | `场景/pk场入口青铜剑.gfx` |
| 1 | `场景/七杀谷祭坛光球.gfx` |
| 1 | `场景/中式结婚场景/悬浮台光束.gfx` |
| 2 | `场景/中式结婚场景/红色灯笼圆.gfx` |
| 2 | `场景/仙传送门.gfx` |
| 5 | `场景/传送门55.gfx` |
| 4 | `场景/佛光.gfx` |
| 12 | `场景/光运.gfx` |
| 2 | `场景/八卦.gfx` |
| 1 | `场景/八卦紫.gfx` |
| 1 | `场景/八卦红.gfx` |
| 1 | `场景/八卦蓝.gfx` |
| 57 | `场景/冰雾2.gfx` |
| 4 | `场景/冰雾3.gfx` |
| 1 | `场景/副本洞口效果01.gfx` |
| 2 | `场景/副本洞口效果03.gfx` |
| 2 | `场景/召唤阵.gfx` |
| 2 | `场景/圣城之光.gfx` |
| 1 | `场景/塔光.gfx` |
| 31 | `场景/夜空飘尘.gfx` |
| 1 | `场景/夜空飘尘_黄.gfx` |
| 2 | `场景/天煞光束.gfx` |
| 4 | `场景/天煞地面效果.gfx` |
| 1 | `场景/天煞天空效果.gfx` |
| 1 | `场景/天空之城光束.gfx` |
| 127 | `场景/好火焰.gfx` |
| 2 | `场景/寺庙灯笼光.gfx` |
| 1 | `场景/封引.gfx` |
| 3 | `场景/封引2.gfx` |
| 2 | `场景/屋顶八卦.gfx` |
| 15 | `场景/岩浆火焰.gfx` |
| 10 | `场景/岩浆火焰2.gfx` |
| 6 | `场景/彩虹.gfx` |
| 1 | `场景/探照灯.gfx` |
| 1 | `场景/新种族地图/09黄色的电球.gfx` |
| 1 | `场景/新种族地图/0完美远征地主塔.gfx` |
| 1 | `场景/新种族地图/0远完美浮空城.gfx` |
| 14 | `场景/新种族地图/主城光球大.gfx` |
| 22 | `场景/新种族地图/主城光球小.gfx` |
| 1 | `场景/新种族地图/主城鱼缸.gfx` |
| 99 | `场景/新种族地图/光.gfx` |
| 14 | `场景/新种族地图/吐水泡.gfx` |
| 1 | `场景/新种族地图/场景鱼群.gfx` |
| 1 | `场景/新种族地图/墙上红色图腾.gfx` |
| 7 | `场景/新种族地图/墙柱火.gfx` |
| 14 | `场景/新种族地图/墙蹲火.gfx` |
| 6 | `场景/新种族地图/大坑_液体.gfx` |
| 101 | `场景/新种族地图/帐篷火盆.gfx` |
| 1 | `场景/新种族地图/新手村出生点.gfx` |
| 13 | `场景/新种族地图/新手村出生点_光子.gfx` |
| 14 | `场景/新种族地图/新手村出生点_绿线.gfx` |
| 2 | `场景/新种族地图/暗语者营地.gfx` |
| 2 | `场景/新种族地图/水族箱.gfx` |
| 3 | `场景/新种族地图/水族箱_水草墨绿.gfx` |
| 2 | `场景/新种族地图/水族箱_水草红.gfx` |
| 2 | `场景/新种族地图/水族箱_水草翠绿.gfx` |
| 2 | `场景/新种族地图/水族箱_海星橙.gfx` |
| 2 | `场景/新种族地图/水族箱_海星玫.gfx` |
| 2 | `场景/新种族地图/水族箱_海星红.gfx` |
| 2 | `场景/新种族地图/水族箱_海星黄.gfx` |
| 2 | `场景/新种族地图/水族箱_顶.gfx` |
| 2 | `场景/新种族地图/水族箱_鱼群.gfx` |
| 8 | `场景/新种族地图/水族箱玻璃.gfx` |
| 3 | `场景/新种族地图/水片圆.gfx` |
| 1 | `场景/新种族地图/水片长.gfx` |
| 4 | `场景/新种族地图/水门_瀑布.gfx` |
| 6 | `场景/新种族地图/沙土.gfx` |
| 8 | `场景/新种族地图/沙尘.gfx` |
| 1 | `场景/新种族地图/海洋光束.gfx` |
| 8 | `场景/新种族地图/海马眼睛.gfx` |
| 8 | `场景/新种族地图/火把.gfx` |
| 6 | `场景/新种族地图/火苗.gfx` |
| 1 | `场景/新种族地图/白兰树.gfx` |
| 2 | `场景/新种族地图/盆景水晶.gfx` |
| 2 | `场景/新种族地图/盆景火.gfx` |
| 4 | `场景/新种族地图/石像火盆.gfx` |
| 1 | `场景/新种族地图/能量喷泉.gfx` |
| 1 | `场景/新种族地图/能量喷泉4.gfx` |
| 1 | `场景/新种族地图/能量喷泉5.gfx` |
| 39 | `场景/新种族地图/蓝水晶光晕.gfx` |
| 25 | `场景/新种族地图/蓝火.gfx` |
| 32 | `场景/新种族地图/蓝蘑菇.gfx` |
| 5 | `场景/新种族地图/蓝蘑菇2.gfx` |
| 8 | `场景/新种族地图/蓝蘑菇3.gfx` |
| 12 | `场景/新种族地图/蘑菇灯.gfx` |
| 4 | `场景/新种族地图/血鲷封印台.gfx` |
| 8 | `场景/新种族地图/血鲷封印台发光体.gfx` |
| 4 | `场景/新种族地图/血鲷封印台罩子.gfx` |
| 1 | `场景/新种族地图/血鲷封印台顶_上.gfx` |
| 1 | `场景/新种族地图/血鲷封印台顶_下.gfx` |
| 1 | `场景/新种族地图/赛亚岛传送门.gfx` |
| 14 | `场景/新种族地图/路灯.gfx` |
| 1 | `场景/新种族地图/远征地血雕大帐.gfx` |
| 2 | `场景/新种族地图/飞碟射灯.gfx` |
| 18 | `场景/新种族地图/黄蘑菇.gfx` |
| 27 | `场景/树叶雨.gfx` |
| 50 | `场景/树林光束.gfx` |
| 3 | `场景/水晶.gfx` |
| 4 | `场景/水晶1.gfx` |
| 5 | `场景/水柱.gfx` |
| 7 | `场景/水神祭坛光1.gfx` |
| 2 | `场景/水神祭坛光2.gfx` |
| 19 | `场景/沙尘3.gfx` |
| 1 | `场景/沼泽祭坛.gfx` |
| 2 | `场景/洞口毒雾.gfx` |
| 14 | `场景/海底水泡.gfx` |
| 150 | `场景/海底水泡2.gfx` |
| 5 | `场景/海底波纹.gfx` |
| 31 | `场景/海底波纹2.gfx` |
| 9 | `场景/海底闪光.gfx` |
| 5 | `场景/海底闪光2.gfx` |
| 9 | `场景/海底闪光3.gfx` |
| 6 | `场景/海底闪光4.gfx` |
| 7 | `场景/海底飘尘.gfx` |
| 47 | `场景/海底飘尘2.gfx` |
| 39 | `场景/海洋光束.gfx` |
| 2 | `场景/温泉.gfx` |
| 3 | `场景/温泉2.gfx` |
| 6 | `场景/温泉3.gfx` |
| 8 | `场景/滴水.gfx` |
| 6 | `场景/瀑布水花.gfx` |
| 7 | `场景/瀑布水花中间.gfx` |
| 1 | `场景/瀑布水花中间2.gfx` |
| 7 | `场景/瀑布水花底.gfx` |
| 6 | `场景/瀑布水花湍急.gfx` |
| 1 | `场景/火山喷发2.gfx` |
| 107 | `场景/火焰1.gfx` |
| 31 | `场景/火焰2.gfx` |
| 1 | `场景/火焰门.gfx` |
| 19 | `场景/灯光/孔明灯光晕.gfx` |
| 4 | `场景/灯笼.gfx` |
| 334 | `场景/灯红.gfx` |
| 161 | `场景/灯红淡.gfx` |
| 11 | `场景/灯绿.gfx` |
| 10 | `场景/灯蓝.gfx` |
| 2 | `场景/灯黄.gfx` |
| 6 | `场景/灵族/丛林光点红.gfx` |
| 48 | `场景/灵族/丛林光点绿.gfx` |
| 2 | `场景/灵族/丛林光点蓝.gfx` |
| 2 | `场景/灵族/主城人脸雕像水面.gfx` |
| 4 | `场景/灵族/主城光圈.gfx` |
| 5 | `场景/灵族/主城光柱.gfx` |
| 1 | `场景/灵族/主城内天空.gfx` |
| 8 | `场景/灵族/主城喷泉弧形水柱.gfx` |
| 4 | `场景/灵族/主城喷泉弧形水柱小.gfx` |
| 4 | `场景/灵族/主城喷泉水花.gfx` |
| 5 | `场景/灵族/主城喷泉池水面.gfx` |
| 2 | `场景/灵族/主城喷泉环形水花.gfx` |
| 1 | `场景/灵族/主城喷泉环形瀑布.gfx` |
| 16 | `场景/灵族/主城喷泉直流水柱.gfx` |
| 1 | `场景/灵族/主城大水面.gfx` |
| 13 | `场景/灵族/主城天花板.gfx` |
| 1 | `场景/灵族/主城室内绸带.gfx` |
| 3 | `场景/灵族/主城小光柱.gfx` |
| 4 | `场景/灵族/主城柱子01_效果.gfx` |
| 4 | `场景/灵族/主城柱子02_效果.gfx` |
| 7 | `场景/灵族/主城琥珀石柱连线.gfx` |
| 14 | `场景/灵族/主城琥珀石柱顶.gfx` |
| 1 | `场景/灵族/主城眼睛光球.gfx` |
| 3 | `场景/灵族/主城眼睛地面.gfx` |
| 12 | `场景/灵族/主城眼睛小光球.gfx` |
| 1 | `场景/灵族/主城竖琴.gfx` |
| 9 | `场景/灵族/主城绿色树灯光晕.gfx` |
| 9 | `场景/灵族/主城镶嵌石头光.gfx` |
| 2 | `场景/灵族/传输器.gfx` |
| 19 | `场景/灵族/出生点宝石.gfx` |
| 8 | `场景/灵族/出生点宝石蓝.gfx` |
| 1 | `场景/灵族/变异蚁穴内.gfx` |
| 3 | `场景/灵族/堂主城柱子01_01.gfx` |
| 1 | `场景/灵族/堂主城柱子01_02.gfx` |
| 1 | `场景/灵族/堂主城柱子01_04.gfx` |
| 1 | `场景/灵族/堂主城眼睛.gfx` |
| 1 | `场景/灵族/堂主城门01.gfx` |
| 1 | `场景/灵族/壶口能量光柱.gfx` |
| 55 | `场景/灵族/帮植物光球绿.gfx` |
| 52 | `场景/灵族/帮植物光球蓝.gfx` |
| 2 | `场景/灵族/底层火焰.gfx` |
| 8 | `场景/灵族/教堂窗户体积光.gfx` |
| 1 | `场景/灵族/斗兽大笼子.gfx` |
| 4 | `场景/灵族/新手村悬浮能量体.gfx` |
| 4 | `场景/灵族/新手村悬浮能量体下.gfx` |
| 2 | `场景/灵族/新手村连线轨迹.gfx` |
| 5 | `场景/灵族/新手村黄色光晕.gfx` |
| 66 | `场景/灵族/木雕喷火.gfx` |
| 5 | `场景/灵族/水花.gfx` |
| 6 | `场景/灵族/水面落叶.gfx` |
| 1 | `场景/灵族/沸腾气泡.gfx` |
| 1 | `场景/灵族/瀑布整.gfx` |
| 1 | `场景/灵族/瀑布水池.gfx` |
| 5 | `场景/灵族/瀑布闪光.gfx` |
| 35 | `场景/灵族/瀑布雾气.gfx` |
| 1 | `场景/灵族/灵族传送门_蓝色顶球.gfx` |
| 1 | `场景/灵族/灵族传送门_黄色顶球.gfx` |
| 1 | `场景/灵族/灵族传送门蓝色.gfx` |
| 1 | `场景/灵族/灵族传送门黄色.gfx` |
| 5 | `场景/灵族/灵族树藤闪光.gfx` |
| 1 | `场景/灵族/灵族粮仓.gfx` |
| 8 | `场景/灵族/石桥灯.gfx` |
| 8 | `场景/灵族/落叶红.gfx` |
| 15 | `场景/灵族/落叶黄.gfx` |
| 14 | `场景/灵族/野外邪恶烟.gfx` |
| 4 | `场景/灵族/野外邪恶烟03.gfx` |
| 7 | `场景/灵族/野外黄绿色树灯光晕.gfx` |
| 22 | `场景/灵族/锅炉火焰.gfx` |
| 18 | `场景/灵族/门缝光.gfx` |
| 2 | `场景/灵魂3.gfx` |
| 1 | `场景/炉香.gfx` |
| 38 | `场景/炊烟.gfx` |
| 22 | `场景/烟.gfx` |
| 16 | `场景/烟1.gfx` |
| 60 | `场景/狼烟2.gfx` |
| 1 | `场景/珠宝闪烁.gfx` |
| 4 | `场景/祭坛烟雾.gfx` |
| 3 | `场景/符文阵效果.gfx` |
| 3 | `场景/符文阵效果2.gfx` |
| 25 | `场景/篝火.gfx` |
| 1 | `场景/红色电网.gfx` |
| 2 | `场景/绿色传送门.gfx` |
| 8 | `场景/绿鬼火2.gfx` |
| 4 | `场景/绿鬼火3.gfx` |
| 3 | `场景/绿鬼火4.gfx` |
| 3 | `场景/绿鬼火5.gfx` |
| 1 | `场景/羽族传送门.gfx` |
| 1 | `场景/羽族传送门2.gfx` |
| 1 | `场景/能量求.gfx` |
| 2 | `场景/自然/彩虹.gfx` |
| 12 | `场景/花瓣分飞.gfx` |
| 9 | `场景/花瓣雨.gfx` |
| 76 | `场景/萤火虫.gfx` |
| 30 | `场景/蓝火.gfx` |
| 6 | `场景/蓝色光束.gfx` |
| 19 | `场景/雪.gfx` |
| 6 | `场景/香炉烟.gfx` |
| 1 | `场景/骷髅光柱.gfx` |
| 2 | `场景/鬼火.gfx` |
| 2 | `场景/魔传送门.gfx` |
| 1 | `场景/龙宫入口.gfx` |

## Scope and unresolved validation

- The 1.8.4 `gfx.pck` and `trees.pck` are not downloaded here. The Drive inventory records them as larger than the connector's 10 MB direct-download limit. Therefore root/member resolution, per-root element/class counts, texture availability, and full effect-graph execution were **not** re-audited against this map's client packs.
- No GPU render check or native-client comparison was run. The census is parser/data evidence, not a visual-parity result.
- This `world.ecwld` is a map file, not a BMD container. It does not close the 1.8.4 BMD-format blocker in [`184-v300-colours.md`](184-v300-colours.md): no raw BMD from either observed model-loader route was obtained, and no per-mesh next offset was validated against a BMD fixture.
- The record-level results supersede using the older 2,275-effect count as a universal 1.8.4 world total. Different `world.ecwld` builds are different maps; see the multi-file counts in [`cross-version-matrix.md`](../cross-version-matrix.md).
