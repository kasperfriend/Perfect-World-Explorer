# The world effect sweep: what is proven, and what is not

**Date:** 2026-11-07
**Commits:** `39bbe57`, `c2ac43f`
**Status:** the CPU half is proven to produce drawable geometry; the GPU half is
now measurable and is *not* proven either way.

## Why this exists

The report after the archive fix was unchanged: "waterfall is image, no sun rays,
completely no rendered effects". Two rounds of fixing one gate at a time had both
been wrong about which gate mattered, so this round swept every stage of the path
instead of guessing, and measured each one over the real map.

The path a world effect takes is:

```
world.ecwld record -> Effect -> placement_transform -> owner -> loader (1/frame)
  -> Player -> controller tick -> EffectQuad list -> Draw::update
  -> GPU bucket -> pipeline -> render pass -> scene target
```

Each stage was checked in order.

## What is proven, with numbers

Driving the real `gfx_owner::Manager` with all 2,275 placements from the shipped
`world.ecwld`, at the waterfall's own coordinates, for ten simulated seconds:

| Stage | Result |
| --- | --- |
| Records installed | 2,275 |
| Owners inside the 448-unit activation radius | 157 |
| Owners live after 10 s | 161 |
| Owners holding a player (drawable) | **148** |
| Quads in that frame | **6,002** |
| Load failures | 0 |
| Graph (player build) failures | 0 |

And with the waterfall root alone, viewed from 25 units away:

| Property | Result |
| --- | --- |
| Quads after 4 s | 9 (3, 6, 9 — the spray grows) |
| Vertices | 36 |
| Inside the viewport | 36 of 36 |
| Extent from the placement | 0.2 – 3.4 units |
| Vertex colours | finite, non-zero alpha |
| Triangle area | non-zero on every quad |
| Textures named | 21 distinct, all real `gfx/textures/...` members |

So records, offsets, placements, transforms, the day/night window, play speeds,
controllers, UV scrolling, geometry, winding and textures are all confirmed
correct on the CPU side. **This is the first time that has been shown
end-to-end**, and it rules out the entire left half of the diagram.

## What the sweep eliminated

Each of these was a real candidate and each was measured, not assumed:

- **`allows_class_with_context`** — `Pass::Main` always returns true.
- **`resident_bounds`** — `cutoff_residency::contains(None, ..)` is
  `map_or(true, ..)`, so "no bounds" means "everything is resident", not "nothing".
- **Depth clear vs compare** — `LoadOp::Clear(0.)` with `CompareFunction::Greater`
  is correct for the reverse-Z projection the world uses (near = far, far = 1).
- **Pipeline format** — `World::new` stores `format.remove_srgb_suffix()` and
  `effects.draw` runs into `water.scene_raw()`, which is an explicit non-SRGB
  view. Those agree.
- **Vertex layout** — `position [f32;3]`, `uv [f32;2]`, `color [f32;4]` is
  `repr(C)`, stride 36, matching `Float32x3/Float32x2/Float32x4`.
- **Matrix convention** — `to_cols_array_2d()` is column-major, matching
  `mat4x4<f32>`.
- **Blend factors** — every authored code maps; none fall through.
- **Texture eviction** — none; bind groups cannot go stale.
- **Back-face culling** — `cull_mode: None`, so orientation cannot hide a quad.
- **Alpha cutoff** — 5/255 on vertex alpha; the waterfall's quads carry non-zero
  alpha and survive it.
- **Settings** — `world_effects` defaults on and persists; the preview camera sets
  it explicitly.
- **Frozen clocks** — all 2,275 authored play speeds are finite and positive, so
  the "freeze an unplayable rate" branch never fires on this map.
- **`effect_visible` day/night** — the waterfall is `valid_time = 2` (always).

That leaves exactly two things that can make a proven-correct quad list invisible:
something in `Draw`'s GPU submission, or a rule that stops players existing in the
running app. Nothing in the repository could distinguish those, which is why the
last two commits exist.

## The two changes that make it answerable

**Admission prefers what can draw.** `load_one` walked owners in install order at
one root per frame. On a map whose every placement is an owner, the ~150 that are
inside the radius waited behind 2,000+ that never would be, so for a long time
after any move the visible set had no players. Active owners are now admitted
first; the one-per-frame budget is kept because it is the client's own loader.

**A degenerate camera is reported, not silently defaulted.** Recovering the eye
from the frame matrix returns `View::default()` on failure, which is eye-at-origin.
Every billboard then faces the world origin instead of the player, and the caller
could not tell that apart from "no effects near you". `try_from_inverse_view_projection`
carries the reason to the status line.

## How to settle it in one run

The preview path renders the real world offscreen. It now has an A/B mode:

```
PW_PREVIEW_EFFECT_AB=1 perfect-world-explorer.exe <client> --gpu-preview out.png
```

It renders the same camera with the map's effects and without them, then prints:

```
Effect A/B: N of M pixels differ between effects on and off (X roots playing,
Y quads, ...)
```

- **N > 0** — the effects are on screen. The remaining complaint is about which
  effects are authored where, not about the draw, and the status line says how
  many quads that view should contain.
- **N = 0** — the pass never reaches the target and the fault is in the draw.

The world also prints one `World effects:` line per run with the whole chain
(installed, loaded, pending, playing, frames, quads, eye, failure counts), so a
report of "still nothing" can say *which* count is zero.

## Honest position

This is the fourth diagnosis. The first three were each wrong about which gate
mattered, and this one does not claim a fix either: it proves where the defect is
not, and instruments where it must be. Without a GPU in the sandbox the last step
cannot be taken from here.
