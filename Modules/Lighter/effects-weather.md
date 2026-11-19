# GFX, ground projection and weather (Step 14)

Lighter takes the **same live effect owner/hook, strip/sprite, posed mesh,
flipbook cell and regional weather selection** as Native. It does not start a
second effect clock on backend switches. World cards are finite triangles;
secondary views see the emitted geometry, not newly camera-facing cards.
Screen-class cards are primary-only. HUD, menus and lens composition stay
outside the ray world, and `Warp:1` remains counted but not drawn. There is
no assumption that an authored framebuffer distortion is glass. See the
classification in `present.wgsl`.

- The ordered GFX pass walks layer, depth-writer bracket, priority and native
  state ties in draw order. Depth-tested layers compare against the nearest
  solid and prior GFX writes; no-Z draws may win behind visible opaque depth
  and never change depth or a secondary ray. Non-GFX transparent sheets retain
  their distance ordering. Multiple triangles in one draw use its nearest
  covered hit (so a diagonal is not blended twice). Blended depth writers use
  an effective-alpha threshold of 0.5; an entirely opaque, eligible glossy
  skin is a solid. This is a ray-space composition over the world, not a
  reconstruction of every intermediate raster framebuffer.
- Supported D3D factor pairs are evaluated as `source + carried * background`
  in **linear HDR**. This includes alpha, additive, source color, and affine
  destination-color source factors. Destination-alpha and nonlinear
  destination-color destination factors are counted as unsupported. The
  Native encoded attachment's per-draw clamp is not reproduced.
- `MatchGrnd` cells publish bounded XZ projector boxes with native quad UVs,
  tints and blend factors. Test a terrain hit against the cell and vertical
  range, then sample with its ray footprint. This has no independent depth,
  silhouette, or shadow caster. The current cap is 128 draped cells per frame;
  excess/invalid cells are counted. Free-standing decals still use their
  native placement and depth-test state.
- Region flags or the World weather override select rain/snow on the shared
  33 ms tick. Lighter uses the native weather texture masks, deterministic
  seed convention, fall rates and sizes with a reduced particle count
  (320 rain cards / 160 snow cards). The cards receive sun/geometry shadows and
  are suppressed for a submerged main eye. The inexpensive lighting uses
  direction-independent sun exposure, not a physical droplet/flake phase
  function. The native loader's missing-art fallback is still possible.
- MOXB detail uses authored centers, atlas cells, group sizes and day/night
  vertex tints. Lighter isolates sprites before making mips and retains up
  to 4,096 horizontal world-space cards. They are not Native's view-facing
  cloud silhouettes: the proxy provides stable colored sky detail and an
  opacity-dependent sun shadow. The Cloud Detail switch gates both. No
  volumetric cloud scattering or weather-triggered wet material is inferred.

For matching captures, `PW_PREVIEW_WEATHER=Clear|Rain|Snow|Auto` overrides
parsed weather selection in both backends. `PW_PREVIEW_HOUR` and explicit
`--gpu-preview-at` pin the other view inputs. An absent cloud atlas or
unavailable native rain texture is not authored parity.

Preview receipts expose root states, Warp omissions, destination-color
candidates and `rain_mask_source` (authored, archive/member failure, or
decode failure). Native and ray paths use the same mask loader, including
explicit TGA support when the signature is ambiguous. Native slot selection
and admitted ray cards are not surviving fragments. See the
[per-map census and scope](evidence/2026-11-18/12-inventory.md); none of
these counts prove how many effect or rain pixels are visible.

## Validation (2026-11-18)

`234075e` / `d1f8f11`: Naga accepts both trace paths, all 166 `gfx_player`
unit fixtures and the weather fall-clock check pass. Software and CPU Vulkan
ray-query pixel suites pass on Mesa 25.0.7, including noncoplanar GFX layers,
later draws failing an earlier depth write, primary-only no-Z composition,
the drape's vertical clip and unchanged underlying depth, and a 50%-opacity
cloud blocking part of the sun. The streamed scene and saved inventory
fixtures pass separately. These do not establish retail Warp, cloud
silhouette, or rain parity; the comparative capture matrix is Step 21.
