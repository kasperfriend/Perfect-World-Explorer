# Why the world showed no effects at all

**Date:** 2026-11-07
**Fix:** `6e72f2e` — "Serve the effect textures from the archive that actually holds them"
**Status:** verified by measurement over the shipped corpus; CI green (`42408167014`).

## The report

The world rendered its terrain, its buildings and its EC models, but no effect it
authored — no waterfall, no torch or brazier fire, no mist, no glow, no sun
shaft — appeared anywhere. The effect **gallery** did render effects, though it
did not visibly animate them. Everything below the presentation layer had
already been shown to be correct: the ECWLD effect records parse with the right
layout, their positions are absolute world coordinates, the roots load, the
keys are interpolated, the quads are built, and the UVs advance between frames.

So the defect had to sit between "a correct quad list exists" and "a draw call
reaches the screen". It did.

## The cause

The effect draw is handed a byte reader — the closure that turns an archive path
into file bytes.

```rust
// src/world_models_gpu.rs, World::new
self.read = Box::new(move |path: &str| { /* answers only from models.pck */ });
```

`World::new` opens `models.pck`, builds the entry map, and installs a reader over
it. That reader is later passed, unchanged, into every effect update:

```rust
// src/world_models_gpu.rs, update_effects
draw.update(device, queue, &frame, &self.owners.effect_quads(...), &mut self.read);
```

But effect textures do not live in `models.pck`. An element's `TexFile` value is
resolved by

```rust
// src/gfx_player.rs
pub fn texture_path(texture: &str) -> Result<String, String> {
    Ok(format!("gfx/textures/{path}"))
}
```

while model textures resolve under `models/.../textures/...`
(`src/model_asset.rs`). Two different archives, two different namespaces — and
the effect draw was reading the model one. Every effect texture lookup therefore
returned "missing model texture".

That alone would only have lost the texture. What made it total is the next
step. `gfx_effect_gpu::Draw::update` groups quads by texture and uploads one
bucket per group; on a failed read it did this:

```rust
Err(error) => {
    self.note_failure(&key.texture, &error);
    continue;          // <-- drops the whole bucket
}
```

So each unreadable texture deleted every quad that shared it. A world with
**2,275 correctly placed effect records over 163 roots** drew **nothing at all**.

`attach_gfx_archive` was the one place that could have fixed this and did not: it
opened `gfx.pck`, read its directory, and handed the handle to
`owners.set_reader` so root *text* could be fetched — but it never touched
`self.read`, the reader used for asset *bytes*.

## The measurement that confirms it

Taken over the actual shipped archives, not assumed:

| Quantity | Value |
| --- | --- |
| Effect records in `world.ecwld` | 2,275 |
| Distinct roots those records name | 163 |
| Roots missing from `gfx.pck` | **0** |
| Distinct `gfx/textures/...` those roots name | 211 |
| Textures missing from `gfx.pck` | **0** |
| Elements with an empty `TexFile` | 110 (guarded at `gfx_player.rs:885`) |

Every legitimate asset was present the whole time. The viewer simply asked the
wrong archive.

The waterfall is the clearest single case: `场景/瀑布水花湍急.gfx` names
`物体\waterfalldrops.tga`, which resolves to
`gfx/textures/物体/waterfalldrops.tga` and **is** a member of `gfx.pck`.

The Rust parser agrees with that python census, and adds the residency answer.
Parsing `world.ecwld` through `scene::Scene::parse` gives 2,275 effects over 163
distinct roots spanning x −3,875..3,790 and z −5,462..5,232 — the whole map, not
a cluster near the origin (the earlier "clustered near origin" census was itself
a symptom of the bad record offsets). The six `瀑布水花湍急` records sit at
(2300.5, 217.8, −3167.8) and (2276.4, 216.8, −3169.3), scale 3.11, play speed
1.00 — and **157 of the map's 2,275 effects are inside the 448-unit detail
radius of that spot**. Residency was never the reason the waterfall was absent.

## Two fixes

**1. The reader serves the archive the assets are in.** `attach_gfx_archive` now
replaces `self.read` with a reader that answers from `gfx.pck` first and falls
back to the previous model reader, so a model texture that a root happens to
name still resolves. Root text keeps its own file handle so a root load and a
texture load can never interleave file positions.

```rust
let gfx_entries = entries.clone();
let mut gfx_pack = pack;
let mut previous = std::mem::replace(&mut self.read,
    Box::new(move |path: &str| Err(format!("unset reader: {path}"))));
self.read = Box::new(move |path: &str| {
    let key = crate::ecmodel::normalize(path);
    match gfx_entries.get(&key) {
        Some(entry) => crate::pck::read_entry(&mut gfx_pack, entry),
        None => previous(path),
    }
});
```

**2. A failed texture no longer deletes geometry.** `Draw` gained a 1×1 opaque
white stand-in texture. When an element's map will not load, the element draws
with it instead of being skipped. This matches what the client does — the
element keeps its authored vertex colour and only loses its detail map — and it
means no future missing asset can silently erase a whole particle group. The
failure is still recorded through `note_failure`, so nothing is swallowed
quietly.

## Consequence for the "no matter what" requirement

The texture surface is now closed against the shipped corpus: every root the map
names exists, every texture those roots name exists, and — because the failure
path still draws — a texture that a *future* corpus lacks can no longer remove
geometry.

The radius gate in `apply_activation` is **not** a defect and was deliberately
left alone. It unloads effects outside the detail radius (7 blocks × 64 units =
448 units at the shipped preset), which is the client's own streaming rule; over
the map's effects that keeps a mean of 10.5 live in the playable neighbourhood
and up to 79 within 4,096 units. Removing it would not show more of the map, it
would only tick effects nobody can see.

## The corpus suite had been passing without a corpus

Worth recording, because it changes how much the earlier green runs proved. The
`corpus_*` tests in `src/gfx_player.rs` are guarded by

```rust
fn corpus_root() -> Option<PathBuf> {
    let root = PathBuf::from("google_drive/x");
    root.is_dir().then_some(root)
}
```

and every one of them returns early when that directory is absent — which it was,
repeatedly, because the sandbox wipes `google_drive` between sessions. A skip and
a pass are indistinguishable in the test output, so seven tests that assert over
the whole archive had never actually run.

With the archive extracted again (all 5,459 `.gfx` members, 51 MB) they do run,
and the suite is **240 passed, 0 failed** — the seven included:

- `corpus_bind_names_resolve` (the corpus authors 968 `BindEle` names)
- `corpus_camera_shakes_all_author_the_noise_controller`
- `corpus_cylinder_emission_holds_on_authored_area_sizes`
- `corpus_plays_through_with_the_surface_path`
- `corpus_surface_emission_holds_on_authored_area_sizes`
- `corpus_trails_play_through_the_vertex_ring`
- `corpus_unbound_emission_carries_the_basis_scalar`

The animated waterfall test now exercises the real root as well:
`corpus_waterfall_plays_from_its_authored_placement` reports `live=1`,
`players=1`, growing quad counts frame over frame, and **moving UVs** — the
spray scrolls its texture on the real archive.

## The gallery is not affected by this, and I checked

The obvious next hypothesis was that the effect gallery had the same defect —
it renders effects, and the user said it did not visibly animate them. It shares
`gfx_effect_gpu::Draw`, so a shared texture failure was plausible.

It is not the case, and the correction is worth recording because it is a near
miss. The Walker module does *not* use `walker::archive::Archive` for its reads;
it uses `modules::designer::assets::Assets`, which opens `gfx.pck` as its own
field and dispatches on the path prefix:

```rust
pub fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
    let normalized = ecmodel::normalize(path);
    if normalized.starts_with("gfx/") {
        return self.gfx.read(&normalized);   // the right archive, already
    }
    ...
}
```

So `gfx/textures/...` already resolved for the gallery. A change chaining
`gfx.pck` into the module archive was written, tested — the offline harness does
not cover the Walker (it needs egui and winit), so the first real signal was CI —
and failed to compile because `use super::Assets` in the test resolved to the
*designer's* `Assets`, which has no such constructor. Reverted in `595dbfd`.

The lesson is in the type: this codebase has two different `Assets` and they
behave differently. Anything that touches "the archive reader" has to name which
one it means.

That leaves the gallery's animation as a **separate, still-unexplained** issue:
its `dt_ms` is a real clamped wall-time delta and its roots advance through the
same `advance_with_ground` path the world uses, so the remaining candidates are
in the draw or the root set rather than in the clock.

## What remains unproven

This fix explains "nothing at all" in the world. (The gallery's stationary
sprites are a separate question — see above.) It does not by itself prove that the
surviving effects **animate** in the running viewer; that depends on
`PlaySpeed`, `TexSpeed` and the controllers receiving a non-zero delta, which is
a separate surface. The next test is the one the user can run: if effects now
appear but stand still, the remaining defect is in the time base rather than in
residency, placement or textures.
