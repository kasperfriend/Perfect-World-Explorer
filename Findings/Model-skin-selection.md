# How the client selects a model's skin (and the bug that came from guessing)

Found while censusing `models.pck` for P3.01. This is a rendering correctness
issue, not a format question.

## The two skin lists

An EC model involves two files that both mention skins:

* the **`.smd`** (`MOXBDMSA`) lists a `Skins` string table — `.bon` skeleton,
  then one or more `.ski` names;
* the **`.ecm`** descriptor carries `AddiSkinCount` / `AddiSkinPath` entries.

The loader treated the `.smd` table as the list of skins to load. It is not. The
`.smd` table is a **menu of skins that exist for the model family**; the `.ecm`
is what **selects** one. Retail data makes the difference obvious:

```
models/npcs/宠物/精灵/冬/冬.smd   lists   冬.ski      (does not exist)
                                  folder  冬1.ski 冬2.ski 冬3.ski 冬4.ski
models/npcs/宠物/精灵/冬/冬1.ecm  selects 冬1.SKI    (exists)
models/npcs/npc/女npc21amd/…      .smd   女npc21.ski (does not exist there)
                                   .ecm   女npc21AMD.ski (does)
```

## What the guessing cost

Reading the `.smd` list as authoritative and failing the load on a missing
`.ski` — which is what `model_asset.rs` did, via `?` — produced this, measured
over all 3,745 `.ecm` descriptors in `models.pck`:

| | models |
|---|---:|
| would fail to load (`.smd` list only) | **514** |
| fail to load with `AddiSkinPath` first | **55** |
| **recovered by the change** | **461** |

461 of 3,745 character and NPC models — 12% of the pack — could not be drawn at
all. Nothing in the flags or the census would have surfaced this; it needed the
two lists compared.

## Where the paths point

`AddiSkinPath` values are stored **pack-root relative but without the `models/`
prefix**, using Windows separators:

```
AddiSkinPath: NPCs\怪物\31水生其它\旋龟\浮游锯齿.ski
  -> models/npcs/怪物/31水生其它/旋龟/浮游锯齿.ski
```

Prefixing `models/` resolves **1,455 of 1,473** references. Two other candidate
readings were tried and measured, and both are worse:

| resolution base | resolved |
|---|---:|
| `models/` + value | **1,455** |
| bare leaf next to the `.smd` | 16 more |
| relative to the `.ecm`'s own directory | ~0 |

That last row is worth stating because it is the intuitive guess — the value
already carries its full directory, so joining it to the descriptor's folder
double-counts it. The loader now tries `models/` + value first, then the bare
leaf beside the `.smd`, and fails loudly if neither resolves. Two references in
the whole archive resolve neither way (`兔子成年.ski`, `武侠男头.ski`); those
models still report a precise error.

## Descriptor validation

`AddiSkinCount` is now checked against the number of `AddiSkinPath` entries and
a mismatch is an error rather than a silent truncation. In the retail archive
that check rejects **0 of 3,745** descriptors, so it constrains malformed data
without touching real content.

## Honest boundary

This recovers the *selection* and the *path resolution*. It does not by itself
verify that the chosen skin's weights line up with the skeleton for all 3,745
models; that is runtime behaviour and belongs with P7.01/P7.02.
