# Quality of life — All to VRAM, time pass, caching, and the sunset line (2026-11-06)

This is the report for the four quality-of-life requests: put the old
everything-resident behaviour back behind a switch, control the sky clock next
to "Time of day", make the load faster on both sides of the viewer window, and
fix the sunset reported against the last sky-parity PR. Every claim below was
recovered from the `pw142` client in the Google Drive folder (the
`elementclient.exe` dumps under `Findings/recovery/`) or measured from the code
in this PR; nothing is guessed.

Short version:

| request | state |
| --- | --- |
| All to VRAM switch | **done** — Graphics tab, `all_to_vram`, off by default |
| Time pass + speed | **done** — World tab beside "Time of day"; client rate is **4 real hours per game day** |
| Faster loading (pre-viewer and in-viewer) | **done** — warm-start terrain index + resident hot set, on by default when All to VRAM is off |
| Sunset sinks through the horizon | **done** — the quads now hang from the emitted point; `el > 0` is a real sunset |
| PR opened, not merged | this branch, draft |

## 1. How long is one game day — recovered, not chosen

`CECSunMoon::Tick` (`elementclient.exe` `0x4b5f10`) is the only thing that
advances the sky clock:

```
004b5f26: fild  qword [esp+0xc]        ; the elapsed milliseconds the caller passes
004b5f2a: fmul  qword [0x8db108]       ; * 6.944444444444444e-08  (1 / 14 400 000)
004b5f30: fadd  qword [esi+0x68]       ; += the stored day fraction
004b5f33: fst   qword [esi+0x68]
004b5f36: fcomp qword [0x8d6138]       ; >= 1.0 ?
004b5f46: fsub  qword [0x8d6138]       ; loop: fraction -= 1.0 while fraction >= 1.0
```

`ds:0x8db108` is `6.944444444444444e-08` = **1 / 14 400 000**, so the fraction
reaches 1.0 after 14 400 000 real milliseconds. **One authored day is four real
hours**; one authored hour is ten real minutes; one real second is four authored
minutes. `clock-constants.txt` has the raw bytes
(`b9791f962fa4723e`) next to the double, so the number is checkable.

The same file dumps the constants `SetTime` and the sun ramp read, and they agree
from the other direction:

| address | f32 lane | meaning |
| --- | --- | --- |
| `0x8d6138` | (double `1.0`) | the wrap the fraction is reduced by in `Tick` and `SetTime` |
| `0x8d6a8c` | `6.944444e-05` = 1 / 14 400 | day fraction per **real second**, applied by the `SetTime` caller `0x428f7d` to a packed real-time-of-day integer (weights 60 and 900; which field the 900 counts is not recovered) — 14 400 s is again four hours |
| `0x8db100` | `0.0003` | disc visibility per millisecond (`0x4b5f77`) |
| `0x8db13c` | `0.1458333` = 3.5 / 24 | sunrise threshold compared against the day fraction (`0x4b60e8`) |
| `0x8db138` | `6.8571429` = 1 / 0.1458333 | its reciprocal, used by the sun ramp (`0x4b60f9`) |
| `0x8d6a9c` | `100.0` | disc placement distance |
| `0x8d58a0` | `0.0` | day-flag comparison value |

`asm-25-sunmoon-tick-callers.txt` shows the caller: the world tick
(`0x429370`) passes its own millisecond counter in `edx` to the sky pointer at
`world+0x50`, so the sky clock is the world tick, not a wall clock. That is the
rate the viewer copies.

`ds:0x8db108` is dumped byte-for-byte in
`Findings/recovery/clock-constants.txt` (with the other constants
`CECSunMoon` reads: the `1.0` wrap at `0x8d5c28`, the placement distance
`0x8d6a9c`, the `±1.0` clamps in `SetTime`, and the two entry-table vtables),
and every code site that references the rate or the wrap is disassembled in
`asm-29-clock-references.txt`.

### The control

World tab, immediately below "Time of day":

* **Enable time pass** — `time_pass_enabled` (default off, so nothing changes
  for existing settings files).
* **Time pass speed** — `time_pass_speed`, logarithmic slider `0.1x .. 600x`,
  default `1.0x`. `1x` is the shipped rate; `600x` is one day per 14.4 real
  seconds, `0.1x` is one day per 40 real hours.
* A small label under the slider prints the resulting day length
  (`Client rate: 1 game day = 4.00 real hours` at `1x`) from
  `sky_time::day_length_label`.

The slider drives `sky_time::advance(hour, dt, speed)` once per frame; the
result feeds the same `SunMoon` state the viewer already used for pinned hours,
so sun and moon elevations, the dusk/night `sky.lst` swap, the regional light
blend, weather light and the zone music day/night choice all move together. The
hour is still stored as `time_of_day`, so pausing the pass leaves the clock
where you stopped it.

## 2. All to VRAM

The residency switch is the pre-optimisation behaviour, brought back behind a
checkbox instead of being deleted. PRs #22 and #10 replaced it with bounded,
paged caches; this PR keeps those as the default and adds the switch back:

* `view_settings.all_to_vram` (key `all_to_vram`, default **off**) is the
  Graphics-tab checkbox *"All to VRAM (maximum speed, uses more memory)"*.
* **In the viewer, live:** `objects::set_unbounded_textures(on)` flips a
  process-wide atomic that `objects::tex_budget_bytes()` reads. When it is on
  the decoded-texture budget becomes `usize::MAX / 4`, so the scenery, model and
  whole-world texture caches never evict anything; the whole-world, tree and
  model preloads keep every page they build.
* **At load, on Load World:** `main::terrain(&path, all_to_vram)` refuses to
  reuse or write the terrain cache and materialises the entire decoded grid in
  memory through `terrain_store::Builder::new_resident` → `Data::Resident`.
  That is `w * h * 7` bytes (154 MiB for the shipped 4096 x 5632 grid), held as
  `Vec<Resident>` with no `.pages` object, no `.index` manifest and no per-block
  decode when the block window is rebuilt. The log line says so:
  `Resident terrain (All to VRAM): N cells, X+Y MiB decoded grid held in memory;
  no disk cache, no block decode`.

The terrain half is decided before a single block is read, which is why the
tooltip says a change takes effect after Load World while the in-viewer caches
follow the checkbox immediately. Memory: the recovered whole-world texture
corpus is about 5.4 GiB of sources before geometry, terrain and models, so this
is a documented "use a lot of RAM/VRAM to get the fast path" trade.

## 3. Caching on both sides of the viewer window

With All to VRAM off (the default) the loader is now a two-layer cache:

**Pre-viewer (disk).** `terrain_store` builds `<root>/<identity>.pages` plus a
`<root>/<identity>.index` manifest (`PWTIDX01`, version 1: magic, version,
width/height/block side, page count, the pages object name, a 32-byte digest of
the pages object, one `{offset, size, hash}` record per page, and a trailing
SHA-256 over the manifest). The identity is a hash of the `world.trn2` header
and every `{name}.t2bk` body, so any change to the map invalidates the cache
instead of silently mixing worlds. On the next start `terrain_store::restore`
re-hashes the `.pages` object, checks every record's extent against the file
length, and hands back the decoded block set — the whole decode and the whole
recompression pass are skipped, and the log says
`Terrain cache: warm start, N verified blocks reused from ...`. Any mismatch
(unavailable index, damaged pages, wrong size, wrong identity) falls back to a
full rebuild rather than using a doubtful object.

**In-viewer (memory).** The restored blocks are the hot set of the block
window: rebuilding a window no longer re-decodes a block that is already
resident, and the decoded set is bounded by `terrain_store::budget()`
(32 MiB by default, `PW_TERRAIN_RAM_MIB` clamps 1..256; a radius-12 window is
about 10 MiB). When All to VRAM is on this layer becomes the whole grid.

## 4. The sunset: the sun now sinks *through* the horizon

The defect reported against #24 was "the red line sits above the horizon, then
the sun stops above the horizon and only its glow disappears". The cause is
geometric and was in the template reading, not in the elevation curve:

* `CECSunMoon::Init` (`0x4b3650`) writes the disc as `y` in `0 .. -height`
  (`x` in `-10..10`, i.e. 20 units tall, 20 wide at distance 100). The emitted
  point `direction * 100` is therefore the **top edge** of the quad, not its
  centre; the centre hangs `height / 2` below it.
* The viewer used the emitted point as the centre. That lifted the drawn disc by
  `atan(10 / 100)` = **5.71 degrees**, and because `Render` (`0x4b3e00`) hides
  the object at `elevation <= 0`, the sun vanished 5.71 degrees above the
  horizon: the visible disc vanished early, the glow went with it, and the rest
  of the descent was invisible.

Fixed by applying the native hang (`sky_celestial::template_hang`): the centre
is `direction * 100 - up * half`, where `half` is the swelled sun half extent
(`1 + 2n`) or the moon's 20 units. The emitted point keeps the recovered
direction, so the elevation curve is now the elevation of the *setting line*:
`el == 0` means the disc has just fully set, and the `el > 0` gate is a real
sunset rather than a pop above the horizon. The same hang is used for the
swelled dusk sun and for the moonrise, so nothing pops mid-sky.

Two shader-side consequences, both fixed in `sky_gpu`:

* the disc's texture coordinates are `uv = vec2(plane.x / (2h), (plane.y - h) /
  (2h)) + 0.5` — the art is sampled on the hanging quad, not on a centred one;
* the `sunshine.bmp` dusk strip hangs too (`y 10 .. -30`, centre 10 units below
  the emitted point) and is drawn and sampled on the **swelled** rectangle, so
  the red glow stays anchored to the disc while the sun grows at `1 + 2n` and
  keeps its own 120 x 40 proportions. It also gates on the rectangle itself
  rather than on the shifted sampling coordinate: `glow_center` exists only to
  put the bitmap's own bright core (at 0.75 of its height in the shipped
  `sunshine.bmp`) on the disc, and gating the shifted coordinate stretched the
  drawn strip by that offset into a red tail twice as tall as the client's
  rectangle. The strip already used the region's light colour from `+0x44`,
  which is why the glow is red at dusk rather than white.

Net effect: the sun slides down through the horizon over its last ~11 degrees of
elevation, keeps the dusk glow until the disc is actually occluded, and the
glow does not wander off the swelled disc.

## 5. Changed files

| file | change |
| --- | --- |
| `src/view_settings.rs` | `time_pass_enabled`, `time_pass_speed` (0.1..600), `all_to_vram` settings with defaults, keys, documentation and round-trip tests. |
| `src/sky_time.rs` | client clock: `CLOCK_MS_PER_DAY` = 14 400 000, rate constants, `advance(hour, real_seconds, speed)`, `day_length_seconds`/`day_length_label`, tests for the recovered 4-hour day. |
| `src/gpu.rs` | World tab: "Enable time pass" checkbox, speed slider and day-length label; Graphics tab: "All to VRAM" checkbox with the residency tooltip; per-frame `sky_time::advance`; publishes the residency to the object cache. |
| `src/objects.rs` | process-wide `UNBOUNDED_TEXTURES` switch (`set_unbounded_textures`/`unbounded_textures`) and `tex_budget_bytes()`. |
| `src/terrain_store.rs` | `Resident` cells, `Data::Resident`, `Builder::new_resident`, resident grid finish/budget, `source_identity`, warm-start `.index` manifest (`write_manifest`/`parse_manifest`), `restore`, plus five new unit tests (resident completeness, duplicate rejection, manifest reuse, damaged index/page rejection, identity coverage). |
| `src/main.rs` | `terrain(root, all_to_vram)`: residency decision before any block is read, warm-start restore, resident build, block loop skipped on a cache hit, log lines for warm start and resident mode; startup publishes the residency. |
| `src/sky_celestial.rs` | `template_hang`, `disc()` centre hangs below the emitted point, sun/moon/swelled-dusk documentation and tests (`the_disc_hangs_from_the_emitted_point_so_the_gate_is_a_real_sunset`). |
| `src/sky_gpu.rs` | disc and `sunshine.bmp` strip geometry follow the hang; swelled-strip UVs; comments tying the 5.71-degree error to the defect. |
| `tools/recover-sky-client.py` | dumps the day-length constants (`clock-constants.txt`), the `CECSunMoon::Tick`/`SetTime` callers, the world-tick callers (`asm-28`) and every reference to the rate/wrap (`asm-29`). |
| `Findings/QoL-2026-11-06-residency-time-pass-and-sunset.md` | this report. |
| `Findings/Sky-parity-checkup-2026-11-04.md` | updated with the sunset-line fix and the recovered day length. |

## 6. Validation

The whole `PR checks and Windows build` workflow is green on this branch,
including both jobs:

* `cargo check --all-targets`, the recovered-arithmetic check, the two
  requirement-status audits and the GFX light audit;
* the 734 viewer unit tests, among them the new resident-terrain completeness
  and rejection tests, the warm-start reuse and damaged-index tests, the source
  identity test, the recovered 4-hour-day clock tests and
  `the_disc_hangs_from_the_emitted_point_so_the_gate_is_a_real_sunset`;
* the bounded lossless terrain RAM residency check;
* the Phase 1 shader regressions, which now include
  `phase5-sky-transition-gpu-check`'s sun/glow bright-core alignment test — it
  renders at 60 degrees instead of 20, because the hung, swelled disc spans
  21.5 degrees and a half-peak centroid of a *clipped* disc is not the disc's
  core. Two extra findings came out of that step and are in this PR: the
  sun-shine strip now gates on the native rectangle instead of the shifted
  sampling coordinate, so the glow the shader draws is the client's 120 x 40
  strip rather than that rectangle stretched by `sunshine.bmp`'s own core
  offset;
* the terrain per-view GPU check, and the Windows x64 build with the native
  gamma and audio fixtures.

The recovery dumps listed above are committed under `Findings/recovery/` so the
numbers in this report can be checked without the client. No GPU frame
comparison was rendered in the sandbox, so the sunset fix is also verified
geometrically: the tests assert the emitted top edge is on the horizon and the
quad centre is below it.
