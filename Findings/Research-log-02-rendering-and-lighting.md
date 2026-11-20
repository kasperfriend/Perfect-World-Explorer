> Historical research log. Entries are chronological and describe the state at the time; later entries can supersede earlier ones. See the README and roadmap for current status. Client data and private storage details are not included.

## Latest model, lighting and post-process research

The 73 unique EC model placements reference 14 ECM assets; parsing their GBK `SkinModelPath` fields and primary SMD v5/v8 skeleton/skin filename records resolves 14 SMDs and 14 primary SKIs plus skeleton references in `models.pck`. Diagnostic: `cargo run --release --bin ecmodel-check -- /path/to/pw142/element`. These strings do not implement mesh skinning, animation or actions.

All 88 `.t2lm` and `.t2lm1` files contain 16 DXT1 256² DDS images, 2,816 images total. The renderer maps them to 4×4 areas per terrain tile, samples day and night using approximate mixing, and uses native BC1 when supported (12 atlas pages, 96 MiB). A Linux Vulkan software preview uploaded and rendered these client maps and static scenery; mapping and constants remain to be validated against the original. BMD packed diffuse colors are decoded and used with an estimated 2× scale.

`fullglow1_14.txt` defines a five-sample weighted blur and `fullglow2_14.txt` composites `mix(original, blur, c0.r) + c0.g*blur*blur`. The viewer implements this structure with estimated weights and intensity; it is not the original runtime shader or exact constants. Underwater cell detection uses active water bits and height; the fog/distortion pass is an approximation. Reflection sky now uses the whole mirrored camera-ray basis rather than only changing ray Y.


## Underwater and texture fidelity — latest investigation

The supplied PE32 executable calls `0x410f10` from `CECFullGlowRender::LoadResource`; the error string identifies it as the under-water bump creation. It fills a 256×256 texture procedurally. Constants at `0x8d6158=1/256`, `0x8d6154=4π`, `0x8d6150=-32`, and `0x8d5894=0.5` yield `R≈128+32*cos(4π*(x/256+y/256-1))`, `G≈128+32*sin(...)`, opaque alpha. The underwater branch at `0x410314` calls the water containment check; `0x4103b6` supplies 0.005 and the fullscreen geometry uses 3 horizontal repeats, aspect-scaled vertical repeats and a scrolling phase driven by milliseconds×0.0005. `shaders/ps/spacewarp_tl.txt` multiplies signed bump RG by (-x,+y) and alpha before sampling the render target. The new WGSL uses this procedural wave and scale rather than independent invented x/y sine waves. Texture quantization/filtering, per-region fog and exact pipeline order remain unverified. `configs/AutoFamilyConfigs/autoscene.cfg` has `fog_color_water`, but it describes the separate auto-family scene, so it must not be assumed to define main-world underwater fog.

The previous terrain shader collapsed gradients to a scalar isotropic max-footprint LOD; shallow-angle textures became blurry. It now submits both actual gradients and requests 16× anisotropic filtering (adapter/driver may fall back). Static BMD materials previously resampled every source to 128×128. An Archosaur 320-unit survey found 332 distinct textures including 44 at 512×512, 139 at 256×256, and one at 512×1024. New rectangular paged atlases retain up to 4096 on each axis with seven mip reductions, plus edge guarding. Grass and SpeedTree atlas limits remain. On the supplied world, grouping placements by model path reduced full-world CPU construction from 101 native-size pages to 66; after trimming unused page rows this is 4,144 MiB base or ~5,525 MiB including mips, before geometry and other resources. Full GPU residency has not been tested. The reduced-memory Linux test rendered Archosaur with new page and mip filtering; trees and day/night lightmaps were disabled in that test.

## Texture audit: foliage and sky

The 59 prepared tree types contain 118 bark/composite images: one 64×256, 54 128×512, 21 256×512, four 256×1024, 24 512×512, two 512×1024 and twelve 1024×1024. All fit in the new 8192² source-sized tree atlas using the current allocator; hardware rendering of that full atlas is unverified. The 53 decoded grass textures are four 128×128, two 128×256, one 256×128, nineteen 256×256, five 512×256, twenty-one 512×512 and one 1024×1024. Grass now uses square tiles with a side at least the largest source dimension, with the original alpha threshold; atlas space is finite. All nine selected client sky BMP frames are exactly 512×512, so the existing 512×512 array does not downsample these sources. At the time of this audit both lacked mipmaps; the later update below implements isolated tree mipmaps, while grass still lacks them.

## Tree mip filtering and Escape-menu update

Each native tree atlas rectangle now receives seven separately generated reductions, avoiding cross-tree bleeding. Cutout composite tiles use alpha-weighted RGB and approximate alpha-test coverage correction; bark uses ordinary filtering. Gradient sampling, mip-aware edge guards and anisotropy are applied. This is a reconstruction and not proven to match the client. An isolated Vulkan software GPU test uploaded and drew the full 8192² atlas, 118 textures, 177 batches and 3,938 placements without world terrain. A reduced-memory full-viewer session tested the new Escape menu, large Settings button and Graphics, Sound and Interface tabs.

## Client-driven SpeedTree leaf motion and terrain-lighting research

The client uses imported `SetTime`, `SetWindStrength`, `SetNumLeafRockingGroups(1)` and `SetLeafRockingState(true)`. At 0x650080 it advances runtime time by elapsed milliseconds times a constant; around 0x656a1e it calls SetWindStrength with dynamic strength and two -1 defaults. The client initializes branch/frond/leaf wind methods to enum 2. With enum 2, the runtime's geometry wind-weight pointers were null in the probe; enabling leaf rocking and changing the DLL clock nevertheless changes `GetLeafBillboardTable`. In a `tree-000.spt` test at strength 1, max corner deltas from t=0 reached 0.401609 at 0.25 seconds and 1.375130 at one second. Camera transform reconstruction against a second direction had maximum error ~0.000002 for the measured type.

The PE32 bridge now creates PWTREE06 records with 64 samples at 4 Hz over 16 seconds for each LOD's cluster corners, through the user's exact DLL. The GPU interpolates and crossfades the loop and exposes a strength slider; static-to-sampled linear blending is not a substitute for the original continuously changing wind strength. Fixed-camera fallback applies to 5,731/11,662 leaf records, including lower LODs, and those records now animate at that fixed camera orientation. All 59 types and all 11,662 sampled LOD leaf records showed a nonzero t=0 to t=1 corner change; the GPU test showed 14,987 differing pixels, and zero differences with strength zero. A Wine first-run bridge test generated and reused the Windows cache. Branch/frond motion, actual dynamic weather, fixed-card orientations, exact real-time runtime behavior and native Windows visual testing remain.

Client `configs/terrain2_shaders.cfg` selects `shaders/ps/t2_3layer_batch_14.txt`. This shader mixes layers and day/night lightmaps, applies `v0` and `mul_x2`, with specular contribution via `v1`; `shaders/vs/terrain_vs.txt` emits oD0=1 and oD1=0 for the inspected path. The new lightmapped terrain path no longer multiplies by a guessed normal-based Lambert factor in addition to the baked map. It mixes the layers, applies the 2× lightmap in an approximation of encoded RGB, then converts to the sRGB target. Texture filtering color space, exact renderer state/constants, other shader paths and specular/scene lighting are not verified. A reduced-memory Vulkan render loaded all 1,408 day/night map pairs successfully.

## Per-LOD leaf corner correction and minimap refresh

At client 0x65714b–0x6572bc the leaf renderer adds each leaf center (SLeaf +0x18) to the four per-leaf corner offsets from SLeaf +0x20, and swizzles native x/z/y into world x/y/z. These camera-facing cards are genuinely used by this client; a running-game visual reference remains unavailable. The previous bridge instead copied GetLeafBillboardTable by cluster. On tree 000 LOD 0.5 this global table differed from the actual per-leaf card offsets by as much as 1.176414 world units, despite matching at LOD 1.0. Cards sharing a cluster have identical per-leaf corners for the measured type.

PWTREE07 uses the SLeaf +0x20 offsets per cluster; it rejects inconsistent corners among leaves. Each discrete LOD is extracted from a freshly initialized client runtime instance to prevent previous animation samples contaminating the static camera table. At repeated discrete leaf LODs the bridge uses the first sampled representative for leaf cards while retaining each requested LOD's branch/frond geometry. The bridge validated four independent camera directions at each sampled LOD for all 59 trees, plus animated samples at three times against an additional direction; zero fallback/camera-error warnings across this client. The GPU audit loaded 11,662 leaf records with zero fixed-camera fallback and rendered time-varying cards. The 16-second loop, 4 Hz sampling, wind strength interpolation and lack of in-game comparison remain limitations. The user-facing default is now a more conservative 0.2; this is not claimed to be the game's measured wind setting.

The former minimap capped updates to 350ms (2.86 Hz). It now renders at the current viewer frame rate, costing another scene pass per frame; both menu windows default to top-left and are user-draggable/resizable.


## Main-world environmental fog boxes (current release)
The ECWLD block box references point into `world.ecbsd`: loader `0x41da70` constructs a 0x134-byte box object and calls `0x48a510`. For scene version 11+ it reads 0xe0 bytes at each reference. Real 1.4.2 world has 99 unique referenced boxes. The record contains an OBB center at 0x08, extents at 0x14 and axes at 0x20; when the first dword is nonzero the loader substitutes 1000-unit extents on each axis. At `0x41eba9` through `0x41ec7a` the client tests boxes and prefers a non-global box to a global one. The viewer uses nearest center to resolve overlap; exact client iteration/streaming order is not reproduced.

`0x48a998–0x48aad5` maps record fields at 0x44 (day air), 0x54 (day underwater), 0x64 (night air), and 0xd0 (night underwater) to box fields subsequently copied by `0x41ecb5–0x41eead` into the scene fog state. Each group is float start, float end, float density, D3DCOLOR ARGB. Main-world `0x4211fe–0x42145f` blends night/day values and sets D3D fog start, end, density, and color. The D3D engine constructor `0x778f4a/0x778fc4` initializes fog mode 3 (D3DFOG_LINEAR), and `0x77ab70` applies it as table or vertex fog depending on capabilities. Example box 7 contains underwater day (10,100,0.5,ff18647c) and night (10,100,0.5,ff242448); another real box has night start 200 and end 100, so inputs are not silently normalized. The viewer samples these records rather than auto-family config, approximates time weights, overlap and smooth transitions, and blends in encoded RGB on sRGB render targets. Scene fog is optional because authentic visibility distances conflict with unrestricted world viewing. Screen-space fog is not equivalent to fogging individual draws, especially water, translucency, sky, reflections and bloom; exact day/night weights, view-distance scaling, transitions and a visual game comparison remain outstanding.

The client cell routine `0x781c50` tests the entire byte as nonzero. The containment test was corrected accordingly; a survey of all 107 supplied world areas found 23,093 zero cells and 30,652 cells with value 1, so this does not change those particular assets. GPU previews of both fog paths ran on Vulkan llvmpipe with reduced-memory settings; no new Windows hardware comparison was performed.


## Grass mip filtering and persisted settings
The previously unmipped local grass atlas now has seven mip reductions. Each texture/alpha-reference pair gets its own isolated slot, alpha-weighted RGB reduction and approximate alpha-test coverage preservation at its client-defined threshold; sampling clamps within the mip footprint to reduce neighbor bleed. The allocator reserves minimum slots for the remaining nearby pairs and can reduce very large textures to avoid starving later materials. Preparation runs on the existing scene streaming worker; queue uploads happen when that result is installed. Coverage correction and anisotropic/wrap behavior remain approximations, and more than 1024 simultaneous material/threshold pairs can still exhaust a page. The added synthetic Vulkan regression validates all seven levels, two different cutoffs, the exact world WGSL, and re-upload of swapped neighboring tiles. The actual client and tree cache were not present in this session's restored sandbox, so this release has not had a fresh real-client visual test.

Viewer controls are persisted as bounded, validated key/value settings with defaults for missing or malformed fields; saves are debounced and also performed at normal exits. Folder selection remains separate. Native Windows persistence, update failures, and the separate launcher low-memory checkbox still require validation or implementation.


## Foliage LOD and water-pass progress
The source client's previous analysis identified near/far SpeedTreeRT limits and the three extracted PWTREE07 geometry snapshots, but not the precise runtime transition curve or thresholds of each individual geometry component. The viewer now renders both adjacent snapshots inside a +/-10% band around the existing height-based heuristic thresholds. Complementary 4x4 ordered screen coverage ensures the two snapshots do not shade the same pixel, and outside the band it draws one LOD. This reduces hard switching but is explicitly not claimed as the original client's fade. It increases tree draw work inside transition bands; real-asset and physical-GPU performance/appearance remains untested. The sampled 16-second runtime leaf rocking and static-to-strength-1 mixing are unchanged and are not a complete live runtime implementation. A synthetic GPU test checks pixel stability for identical LOD geometry before, during and after both transitions, as well as sampled leaf motion and the disabled-wind case.

The water pipeline previously disabled depth writes. Overlapping horizontal water planes could therefore render in the wrong order if bounding-box sort order differed from per-pixel depth; it now writes the nearest water depth. A synthetic GPU test renders overlapping near/far planes in both orders and checks the depth result. Distorted refraction now rejects a displaced opaque-depth sample that lies in front of the original water fragment, reducing foreground leakage at shorelines. This is an approximation to screen-space occlusion, not a recovered client water-plane-clipped refraction render. It does not solve water-to-water refraction, reflected foreground reconstruction, exact shoreline alpha, runtime material constants or underwater pipeline ordering.

The original 3.8 GB client archive from the earlier investigation is no longer in the restored sandbox. Its Drive link reported download quota exceeded when recovery was attempted, so the new code has only synthetic GPU tests, not a fresh comparison with the client assets or running game. The code continues to consume the user's own client at runtime.


## Instanced MOXB grass renderer
The renderer now groups streamed grass placements by client grass type and MOXB mesh, retaining one copy of each mesh vertex/index set and a compact 16-byte position/scale per instance. The vertex stage applies the same position and scale the previous CPU expansion used; alpha-reference materials, atlas UVs, normals, textures, scene and reflected clip plane are retained. Grass uses the same depth/lighting shader and works in the ordinary scene, reflection and minimap passes. Background streaming swaps the instance buffers along with its atlas. A synthetic Vulkan test compared pixel output with the former CPU-expanded path: 2,120 pixels matched without clipping, 420 with reflected-style clipping, and an empty update succeeded. This is an efficiency and scalability improvement, not evidence that the client used identical GPU instancing, or that the viewer's placement, size variation, range, lighting, animation or LOD are exact. The original archive remained subject to Drive download quota on retry; real-client validation is outstanding.


## Client grass lightmaps (unpacked client)
Read `shaders.pck` from the user's unpacked game. `shaders/ps/grassrender.txt` is PS 1.1: texture 0 is base, 1 is day lightmap, 2 is night; `lrp r1, c0.a, t2, t1`, then `mul_x2 r0.rgb, t0, r1`; alpha multiplies texture alpha by vertex alpha. `shaders/vs/grass_wave.txt` generates both lightmap UVs by the same world-space matrix and includes distance-dependent vertex alpha and wind, neither yet ported. The grass renderer now samples the existing client terrain day/night lightmap pages using the terrain's world-grid/atlas mapping and applies 2× modulation in approximated encoded RGB; lightmaps-off retains the prior directional fallback. It shares texture resources rather than duplicating 2,816 maps and is used in ordinary, reflected and minimap passes. A Vulkan llvmpipe GPU regression compares the rendered channel with a CPU PS1.1 arithmetic reference for night, midpoint and day. It was also run with a real `world_1.t2lm`/`world_1.t2lm1` texel from coordinate (183,104), producing green output 53, 44 and 36. This validates that texel and shader arithmetic, not real-world UV alignment, original time interpolation, blend state, sRGB state, wind or distance alpha. `scenelights.dat` and `world_light.dat` were obtained but have not yet been ported; real tree and BMD lighting remain approximate. Full client lighting parity remains incomplete and has not been compared to the running game.

Client `CECSceneLights::Load` at `0x424f89` reads a version `0x80000001`, a record count and 112-byte records; `scenelights.dat` contains 1,993 records. Query function `0x4250c0` spatially indexes candidates and compares distance to each record's first xyz position; this establishes a light-selection path but does not yet establish its colors, types, blending, or which world classes receive them. Do not treat nearest-position selection alone as a complete dynamic-light port.


## BMD baked vertex lighting and depth precision
Executable loader `0x7ba568` (v4) and `0x7ba79c` (v5) read the packed vertex block, triangle indices, normals, then one four-byte-per-vertex array into object member `+0x284` and another into `+0x288`. The previous parser skipped both. At `0x7b9bc0` the engine multiplies its blend factor by 255, forms complementary integer weights and blends the arrays per RGB channel with a shift by 8 into vertex diffuse. `0x7a9730` sets stage-0 color operation to 5 (D3DTOP_MODULATE2X) through `0x779fc0`; `0x779fe0` sets the texture and current-color arguments. Thus the litmodel batch follows texture × blended baked vertex diffuse ×2, not the additional arbitrary Lambert term previously used by the viewer. The viewer now carries both arrays into GPU vertices for local and whole-world BMD pages, reproduces the integer blending and 2× operation in approximately encoded RGB, and no longer applies the guessed sun term to those BMDs. Endpoint association with day/night is inferred from the day/night factor, and the viewer's 24-hour factor, other fixed-function states, materials and color space are not proven exact. Synthetic BMD parser and GPU regressions cover distinct endpoints and midpoint. A real BMD sample could not be extracted through the Drive connector because litmodels.pck is 397 MB, above its 100 MB per-file download limit; no original-game image comparison was made.

The previous renderer submitted already-uploaded world pages alongside all local scenery until `world_done`, creating guaranteed overlapping copies during preload. World pages are now held offscreen and swapped in when preload completes. In addition the main perspective, reflection and minimap use a reversed floating-point depth buffer, including corresponding water depth compare/refraction tests and post-fog reconstruction. A unit test finds far surfaces that collapse to the same forward f32 depth but remain distinct in reversed depth. This mitigates precision issues rather than guaranteeing a fix for genuinely coplanar polygons. Synthetic tree, grass and water regressions exercise the convention; affected real-world buildings, full-world preload, water and physical Windows GPU require validation.

### Further scene-light call-site trace (not yet a rendering port)
The only direct caller found for the `0x4250c0` scene-light lookup is `0x41a740`. It supplies the target object's position at `+0x3c/+0x40/+0x44`, copies the selected record into a larger light-parameter structure, marks that local light enabled only if lookup succeeds, and calls `0x7924f0` with the structure. Thus the records participate in an object-specific lighting path, not automatically in every static BMD or terrain pass. The object type, field semantics, and downstream shader/states still need to be proven before applying these lights. The client also initializes SpeedTree's global light 0 via `SetLightAttributes` and `SetLightState` at `0x64fd7a–0x64fd83`; this is initialization evidence, not proof that the current viewer reproduces tree lighting or its time evolution.

The `litmodels.pck` binary was not obtainable through the available Drive download path: a temporary, separately copied, unlisted-link file still returned a quota HTML page after following the full confirmation form. Public access to the temporary copy was revoked and the copy deleted; the original's permissions were not modified. Real BMD endpoint ordering and full scene-light parity remain unverified.

### Real litmodels archive, winding and baked-light audit
The five split ZIP parts supplied by the user were checked against Drive's individual MD5 values, concatenated, and `unzip -t` verified the archive. Its extracted 397,425,471-byte `litmodels.pck` has MD5 `359be12bc2aab785210ab82506a67aaf`, matching the original Drive file. `bmd-light-survey` parses all 16,335 world BMDs, 40,579 meshes and 7,802,464 vertices with no parse failures. For 7,618,900 vertices, serialized diffuse equals the client endpoint `floor(array0*255/256)` in each channel; array0's average RGB was 103.33 versus 93.29 for array1. This supports, but does not alone prove, the first=day interpretation. `litmodels/world/10/litmodel_131.bmd` first vertex has serialized 0xff2c3f55 and arrays 0xff2d4056 / 0xff323e54. The actual world WGSL, fed that real pair and a controlled texture, produced RGB [1,44,3], [1,45,3], [1,47,3] at night/mid/day on llvmpipe, agreeing with the integer blend and encoded-RGB reference within two output levels. This is a controlled arithmetic test, not a client screenshot comparison.

`bmd-overlap-survey` found 174,744 exactly coincident triangles in 5,416 models, of which 168,225 had opposite winding; only 12 duplicates crossed mesh boundaries. Their back and front surfaces can carry different UVs and baked colors. The original renderer's `0x7a98c5–0x7a98d1` disables D3D culling only for the mesh flag at `+0x26c`, restoring it after the draw, through `0x77a540`/D3DRS_CULLMODE (22). The viewer previously drew all BMDs with culling disabled. It now discards back-facing BMD fragments; the water-reflection uniform marks the mirrored transform so the facing test reverses there. Trees/grass are unaffected. A Vulkan GPU test exercises front and back winding for normal and reflected cases. Per-mesh cull overrides have not yet been fully traced from authored data; applying the default to every BMD is an approximation and may hide intended two-sided surfaces. In particular, check Plume foliage and the user's affected buildings before treating the issue as resolved.

The Graphics setting **Cull building back faces (experimental)** is on by default and is persisted. Turning it off restores the prior two-sided viewer behavior if an intended surface disappears. The GPU winding regression also checks the off state. This option is a safety fallback, not the missing per-mesh client flag implementation.

### Scene-light record ingestion
`scenelights.dat` has now been parsed directly from the supplied client: version 0x80000001, 1,993 records of 112 bytes, all finite; 1,174 records have flag bit zero set. Each stores position, range (0.3–500), three stored attenuation floats, two four-float color/intensity vectors, center/half-extents/min/max and flags. First record is centered at (-3592.807, 274.039, 5202.683), range 25; its primary vector is (0.05882, 0.38824, 1.0, 2.0), secondary vector (0.01176, 0.07765, 0.2, 0.4). The first `world_light.dat` record contains the same position/range and RGB with a D3DLIGHT9-like layout, a useful cross-check, not a proof that all 2,997 records map one-to-one. At 0x4250c0 the client uses spatial cells, first compares distances to candidate record centers, then modifies output attenuation to (0,0.4,0) and scales both four-float vectors when flag bit 0 is set. A second, separate cell list and further logic also participate. Merely choosing the globally nearest light is therefore not the full client query. The parser is wired to load and validate the file at startup; these lights are **not yet applied to rendering** because the query, dynamic entries, affected object classes and shader integration are incomplete. Static BMDs already contain baked lighting; blindly adding these lights risks double lighting.

### Regional fog transition timing (additional executable trace)
The v12 EC-BSD `u32` at record `0x9c` is copied to box object `+0x7c`. At `0x41f302–0x41f33c` the client converts it to a float and calculates `1000/value`, or zero for a zero duration (snap). During each update `0x41fdd2–0x41fdfe` converts the elapsed-milliseconds argument to seconds (constant `0.001`) and calls `0x421e80`, which advances each current component along its precomputed slope and clamps at the target. Thus the file field is transition duration in milliseconds. A survey of all 99 unique referenced world boxes finds **all 99 use 5000 ms**. The previous viewer used a fixed exponential smoothing with a half-life of about 0.35 seconds; the new implementation interpolates air and underwater day/night endpoints linearly over the entering box's authored duration, supports interrupted transitions and zero-duration snap, and then applies the existing approximate time-of-day factor. The unmeasured area-selection and time-of-day schedules remain approximations; this is not evidence of a ten-point overall renderer-parity gain.

### Recovered client world-clock day/night blend
`0x428f53–0x428f86` converts world hour/minute/second to normalized day by multiplying seconds since midnight by approximately 1/86400, then calls `0x4b6030`. `0x4b60e0` calculates the night factor in sky object `+0x70`: 1 until 03:00, linearly falling to 0 at 07:00, 0 until 18:00, linearly rising to 1 at 21:00, then 1 until midnight. `0x41ec86–0x41ec9e` uses night and 1-night for region color/fog, `0x42b4fe–0x42b50d` calls `0x7a9ad0` to set BMD renderer `+0x2c`, `0x42bd6e–0x42bd74` supplies it to terrain water renderer `0x780970` / terrain `0x7ccf60`, and `0x42c10d` calls tree `0x651510`. Replaced the viewer's sine approximation with this curve for atmospheric/underwater fog, BMD baked arrays, terrain/grass lightmaps and the existing approximate generic foliage lighting. This does not claim exact SpeedTree shaders; the executable has a dedicated factor/texture path, and `shaders/vs/treebranchvs.txt` plus `shaders/ps/treebranch.txt` describe tangent-space normal-map and specular calculations not yet implemented. Sky panorama blending, foliage material lighting, color-space/sampler behavior and scene-light application are not fully recovered. At `0x41ec65`, lack of a containing region returns early; the viewer no longer snaps to synthetic fallback on a gap after a region has been selected (its changing-time handling in a gap remains an approximation). Overlap order remains unresolved: the client picks the last matching non-global or global item in its runtime iterator; the viewer still chooses nearest within each priority class because the iterator's order/streaming semantics are not established.

GPU regression: twelve clock points 00,03,04,05,06,07,12,18,19,20,21,24 were compared with CPU reference weights, a real `world_1` lightmap sample `[41,56,65]` day / `[54,67,74]` night, and the real `world/10/litmodel_131.bmd` vertex colors `ff2d4056` / `ff323e54`. Software Vulkan grass green values were `[53,53,49,44,40,36,36,36,42,47,53,53]`; BMD RGB channel samples matched fixed-function arithmetic within two quantization units. These are isolated GPU arithmetic checks, not matched-camera, matched-time visual comparisons or a percentage parity measurement.

### Regional tree-lighting experiment
The v12 EC-BSD record contains an authored directional vector at `0x74..0x7c` (loader `0x48a950`, box object `+0xc4..+0xcc`); packed ARGB day ambient at `0x80` and day directional at `0x84`; night ambient at `0x88` and night directional at `0x8c`; and directional multipliers at `0xb8` and `0xbc`. Client `0x41eee3–0x41f030` initializes these colors, scaling directional components, and `0x41f83a–0x41f9...` sets transition targets and slopes. Client `0x420ddc–0x420e84` combines day/night ambient components and sends the D3D ambient render state, and `0x420807–0x4208ad` combines directional components. The original v12 source light vector is installed in the D3D light direction through `0x41fd58–0x41fdb1` / `0x79b390`; this direction is distinct from a sky dome animation.

The viewer now parses and smoothly transitions these regional values alongside fog, blending with the recovered time-of-day factor. World settings contains **Experimental regional tree lighting**, disabled by default to preserve the user-approved appearance. If enabled, the tree GPU pass uses a bounded linear-space Lambert approximation using the reversed authored light direction; the light color mixes 75% regional luminance and 25% RGB tint. These neutralization weights are a visual safeguard, not recovered client constants. This does not add lights to already-baked BMDs, terrain, or grass, and missing/zero direction falls back to the old tree shader. It is deliberately experimental: branch normal maps, tangents, specular, proper leaf material data and original color/sRGB behavior are not reproduced; some SpeedTree paths may not use precisely this material equation. The established tree geometry/rocking paths are unchanged, and a mode-off GPU check is byte-identical to the prior synthetic baseline. After the user reported the first regional experiment was oversaturated and overexposed, the shared `mul_x2` encoded-color implementation was removed: its prior synthetic result clipped a bright green leaf to 255. The reference shader's `mul_x2` is specifically a branch shader with separate textures/normal mapping and material constants; applying it to all foliage with unverified constants was not justified. A llvmpipe GPU test using box-7 coefficients on synthetic leaves now matches modeled linear RGB [1,145,2] (day) and [1,146,2] (night) at 620 pixels each, in an RGBA8 unorm test target; these are not screenshot or sRGB display values. High-coefficient and colored-light cases are bounded by the unlit sample, and disabling the mode restores the original test pixels byte for byte. Survey successfully parsed all 99 boxes. Real game visual comparisons and regional material behavior remain unverified; this is not a measured overall parity improvement.


## Client grass per-vertex distance fade
The user confirmed the revised experimental regional tree lighting works perfectly; its shader and the water shader are unchanged in this iteration. A separate retail path was traced: `0x650d4e` loads `Shaders/vs/grass_wave.txt`, `0x650d79` loads `Shaders/ps/grassrender.txt`, and `0x6513ce–0x651404` supplies the actual camera position to VS constant c9. `0x652ed3` obtains the grass-type sight range (`+0x128`), and `0x652fa8–0x652fdd` supplies `1/(range*range)` to c0.w. The VS computes `max(1 - dot(v0-camera,v0-camera)/range², 0)` at each pre-wind vertex; `grassrender.txt` multiplies texture alpha by the interpolated vertex alpha. At `0x652117–0x652130` grass type initialization copies the serialized properties then forces the alpha-blend byte to false and alpha reference to 84; the alpha-tested path `0x652351–0x652371` enables alpha test, disables blending, sets D3DCMP_GREATER and that ref. The ECWLD four-byte field at the front includes nonzero padding: it must not be treated as a 32-bit boolean. Census of the actual 220 grass types: 59 sight range 50 and 161 sight range 100; low bool byte is zero and stored alpha reference is 84 in all 220.

Implemented the distance alpha and material cutout on instanced grass, enabled by default with an opt-out in World settings. The per-type float is supplied via a field unused by the grass vertex material; fade is computed on each reconstructed world vertex and interpolated before the alpha comparison. It affects standard, lightmapped, reflection and minimap grass; reflection currently uses the real camera position, pending exact client reflection-camera verification. Does not change the tree or water shader. A llvmpipe regression produced 2,120/1,444/570/0/2,120 lit pixels for off/near/mid/far/off, and the interpolated synthetic-quad CPU reference matched all pixels (zero mismatches); the lightmapped variant gave exactly the same 570-pixel coverage. Tree toggle-off and LOD tests, real BMD arithmetic and water depth tests are separate. Streaming radius 200, procedural transforms/variation, alpha mips, wind, per-pixel clipping, original fog and real-client visuals are not fully reproduced, so no percentage gain or full parity is claimed.


## Grass patch placement and optional wind — current iteration

The actual client initializes grass patch boundaries at `0x653639–0x6536b2` as 16×16 cells. `0x653b33–0x653b61` seeds the client RANMAR with `(patch_x + patch_y) % 31328` and `(patch_x + patch_y) % 30081`. The 97-value table and carry generation at `0x766680–0x7668c0` match the canonical RANMAR 20,000-draw test vector. At maximum density, `0x653d21–0x653eda` generates five random numbers per traversed cell even if the mask bit is clear: x and z jitter in ±half a grid cell, yaw in 0..6.28 radians, scale between size×(1−variation) and size×(1+variation), and a wind-history selector. Matrix `0x788bb0` establishes the orientation. `0x6526e0–0x65277a` establishes the center-to-grid corner; `0x653dc4` subtracts 0.02 from the terrain height. `0x652026–0x6520b0` computes squared local Y divided by model height; `0x653f8a–0x653f9e` multiplies it by the type softness. The selector uses `random(0,10)+2*abs(world_x−10000)` modulo 80.

The viewer now precomputes these transforms, skipping entirely empty patches but consuming the required random samples before each active cell. GPU instances contain position, scale, yaw and wind selector; rotated normals also affect the non-lightmapped fallback. The optional World wind slider is default-off: its GPU sine-based displacement is an approximation, not the client’s 80-sample noise history or its timing. Weight uses model mesh bounds as a reconstruction. Height still uses the viewer's terrain triangles and runtime density/streaming do not match. A survey found all 2,181 real area dimensions divisible by 16; for real `bsdata/54.dat`, 8 areas and 656 active cells generated valid transforms. One full-client or game-side positional comparison has not been made. Synthetic rotated GPU output exactly matched CPU-expanded geometry for 2,094 pixels (417 clipped); optional wind changed 823 pixels and zero strength restored the original frame. The grass fade CPU reference remained exact; independent tree LOD/lighting and water depth tests passed. No measured percentage parity gain is claimed.


## Grass noise/history and denser world terrain
`0x650cb0` constructs the grass noise generator with seed `0x12345`, 256 samples, amplitude 1, initial period 256, persistence 0.95 and 8 octaves; sets the absolute-value noise flag. `0x7668f0` is Park–Miller with modulus 2147483647 and multiplier 16807; `0x766910` maps the next seed to `(seed % 2001 - 1000) * 0.001`. `0x766320` creates three 256-element channels, smoothing each value as 0.25 previous + 0.5 center + 0.25 next, and `0x766940` generates eight offsets modulo 1023 and normalized persistence weights. `0x766490` interpolates periodic samples and sums absolute values at periods 256,128,...,2. At `0x651040`, the client shifts 80 vec4 wind samples once per update, samples the noise at elapsed seconds ×20, multiplies it by the initial direction (1,-0.5,0), then runs a spring routine (`0x651a70`, `0x651840`, `0x651960`) and puts the resulting position at the newest end. The shader loads the vector selected by each grass instance.

The viewer now implements this seeded noise, the 80-entry history, and a spring integration derived from that trace, feeding the chosen vec4 to the GPU grass vertex shader. The filter reconstruction and frame timing have not been compared to client output; the viewer clamps update pauses at 100 ms, the weight uses reconstructed bounds, and slider strength is a viewer control. It remains default-off. Synthetic GPU geometry changed 619 pixels at a supplied history sample and returned exactly to its original image when strength was zero; deterministic, history-shift and boundedness tests pass. That is not a live-client parity validation.

Separately the world terrain mesh now samples every fourth rather than eighth source-grid point in each axis. A new 16-byte packed far vertex (12-byte position, 4-byte signed-normalized normal) replaces the 60-byte generic vertex, and a separate GPU vertex pipeline expands the normal. The nearby full-resolution path is unchanged. Synthetic llvmpipe rendering of identical geometry through the actual near/far vertex entry points matched all 2,704 covered pixels. Sampling is still a uniform viewer LOD rather than the client's LOD and has not been assessed against game screenshots; roughly four times as many distant triangles/index entries require more GPU memory. No tree, water, or established lighting shader was changed.

## Map, teleport coordinates and handedness
The supplied main-world `world_targets.txt` has 54 valid main-world entries (all distinct names), including exact XYZ. `surfaces.pck` contains an 11-row × 8-column set named `surfaces/maps/000.dds` through `107.dds`, each 1024×1024. `world.trn2` specifies 8×11 subterrains, 512 vertices at 2 units per grid: world bounds x −4096..4096 and z −5632..5632. Tile row increases southward and column increases eastward. The viewer's map transform is u = x/8192 + 0.5, v = 0.5 − z/11264; orthographic projection was tested to match that mapping.

Independent client-data checks: City of a Thousand Streams `(-2825.774,379.020,3092.210)` maps to tile `021`, local (0.240,0.480), with client `world_18.t2bk` nearest height 379.0097. Archosaur North `(1470.490,219.608,1180.864)` maps to tile `045`, local (0.436,0.347), with client `world_38.t2bk` height 219.60785; `045.dds` visibly depicts Archosaur. City of Raging Tides `(2681.930,241.793,-4268.359)` maps to `096`, local (0.619,0.668), and that tile depicts Raging Tides; nearest terrain height 235.99, lower than the destination, consistent with an elevated location. This checks world/terrain coordinates and several landmarks, not every model's handedness or a matched screenshot of the running game. No horizontal world-coordinate flip was detected. The reader decodes the 88 client DDS tiles into a 2048×2816 atlas asynchronously; navigation uses a shared terrain-height buffer and does not duplicate its 92 MiB array.

Local-light research: the previously identified `0x41a740` query writes a per-object lighting structure and calls `0x7924f0`. The latter copies the structure into object fields `+0x1a4..+0x21c` and recursively calls itself on a child array at `+0x118/+0x11c`; nearby functions reference A3DSkinModel load/save errors. This further supports a hierarchical animated-model lighting path, not evidence for adding point lights to already-baked static BMDs. Exact model identification, candidate-cell setup, dynamic-list selection, shader constants and validation remain open. No lighting appearance was changed on that incomplete evidence.

### Environment selection re-check (2026-11-01)
`elementclient.exe` at `0x41ebce–0x41ec7a` tests candidate box containment. Each matching non-global box replaces the previous candidate, while matching global boxes are retained as fallback; if no box is found the routine returns false. This reinforces that the viewer's nearest-box tie-break is not proven client-exact. Candidate enumeration and streaming order still require tracing, so no lighting/region priority change was applied. The early return does not by itself establish whether a separate frame update advances an existing transition outside boxes.

### Terrain seam mitigation (viewer approximation)
The distant terrain mesh uses a four-grid-step triangle mesh with diagonal `(b,c)`; the detailed mesh uses the same triangle orientation but every grid point. The viewer now interpolates high-resolution boundary positions and normals onto the distant triangle, blending toward original samples over four grid cells. This minimizes LOD cracks/shading discontinuities, but is not a port of the client LOD algorithm. Synthetic tests cover all four edges and preserve the inner terrain.

### Camera handedness correction (2026-11-01)
The earlier north-up map test was insufficient: it checked that +X projects right in the map but did **not** check that a perspective camera looking along +Z projects +X right. In the old right-handed `look_to_rh` camera, +X instead projected left when facing north; mouse-left could therefore rotate toward +X (east). The user correctly reported the mirrored view. The perspective projection now reverses clip-space X and camera turn/strafe controls are adjusted consistently, while client coordinates, terrain, destinations and original map atlas remain unchanged. The sky uses the same horizontal projection and reflected sky ray transform. Static-BMD face tests account for the extra projection reversal: main/map use the opposite winding flag, water reflections reverse it again. Synthetic GPU tests exercise actual WGSL with one and two axis flips; these are not substitutes for a full-client Windows visual comparison.

### Sky-object direction curve (additional executable trace)
At `0x4b60b0` the client stores an azimuth `atan2(-z,-x)` from a supplied direction. The per-clock update `0x4b60e0` computes elevation with four piecewise-linear segments: from 60 degrees at midnight to 0 at 03:30; from 0 at 03:30 to 70 degrees at noon; from 70 at noon to 0 at 21:00; from 0 at 21:00 to 60 at midnight. Its vector is `(-cos(azimuth)*cos(elevation), -sin(elevation), -sin(azimuth)*cos(elevation))`. Constant bit patterns are used in `src/client_sun.rs`, with tests at boundaries. At `0x41f2e6` region processing calls the azimuth setter, and `0x4b5f5e` updates the sky object each tick. This is a useful recovered component but the consumers and correspondence to actual world-light/material passes are not fully established. It is **not wired into the renderer**, avoiding an unverified change to the approved foliage or baked building lighting.

### Encoded texture filtering investigation (lighting update)
The inspected `elementclient.exe` imports Direct3D 8; wrapper `0x77bdb0`
sets texture-stage state through the D3D8 device (`SetTextureStageState`,
vtable offset `0xfc`). D3D8 texture formats and its fixed-function pipeline do
not provide D3D9-style automatic sRGB texture decode/write render states. This
supports filtering authored color values in encoded UNORM space before the
already-recovered lightmap/MODULATE2X arithmetic, rather than decoding every
texel before filtering and re-encoding the result. This is a renderer-path
correction, not proof of full end-to-end client color management: original
gamma-ramp settings, every shader and the final presentation chain have not
been fully traced.

Terrain texture arrays, the local object/grass atlas, and streamed BMD atlases
now use UNORM sampling. Lightmapped terrain and grass and baked BMD arithmetic
consume the sampled encoded RGB directly, then convert the result to linear
for the viewer's preferred sRGB presentation target. Non-lightmapped paths
decode the *filtered* sample before their prior approximate lighting. Tree
atlas and dedicated tree shader remain unchanged. This can alter high-contrast
filtered edges, so visual comparison on actual game scenes remains important.
The existing non-sRGB surface fallback still needs a separately verified output
transform; synthetic test targets are UNORM and intentionally inspect linear
shader results rather than reproduce an sRGB display.

The BMD GPU regression now renders a 2x2 black/white checker with bilinear UV
at its center through the actual shared BMD shader and checks the fixed
function encoded-space reference: linear-output byte 55 vs expected 54 on
llvmpipe. The old sRGB-filtered sampling would yield a substantially brighter
result. Day/night BMD and grass tests cover multiple time values; they are
synthetic validation, not a full-client screenshot comparison.

### Fullglow weights, gamma ramp and scene-light candidate grid
The client blur globals at `0x94ecf4/f8/fc` contain 0.107, 0.238 and 0.310. `0x40ffeb–0x410002` passes them to `0x4112d0`, which places them in PS constants c0.a, c1.a and c1.r. Thus the kernel is [0.107, 0.238, 0.310, 0.238, 0.107], totaling 1. The viewer now uses these weights both axes, performs a distinct quarter-size stretch copy before horizontal blur, and samples encoded UNORM views for its postprocess to match the D3D8 A8R8G8B8-style arithmetic. sRGB output encoding is applied once on presentation. The client effect object dynamically supplies glow-level and glow-power to `fullglow2_14`; the viewer's 0.16/0.13 composite strengths remain estimates, and additional effects, exact filtering/UV alignment, pass order and UI composition remain open. Windows game comparison is required.

Client config `Gamma` defaults to 41 (`0x40d385–0x40d3a8`). At `0x430226–0x430241` integer config value is transformed by `int(30 + 1.7 * setting)` and passed to `0x77b7c0`. This function skips hardware ramp construction when its device flag indicates the windowed path; otherwise it uses `SetGammaRamp` (D3D8 vtable +0x48) with 256 identical 16-bit RGB entries: for entry i, `int(65535*(i/255)^(100/gamma))`, and positive i is clamped to at least 1. `_ftol` supplies integer truncation. The new opt-in scene-only post simulation evaluates that LUT and interpolates entries, leaving the user's desktop and overlay UI untouched. Driver interpolation and actual display management vary; it is not equivalent to a hardware ramp covering the entire desktop or UI. Default is off to preserve the existing approved appearance.

Scene light: `0x428c25–0x428c32` initializes a 10x10 grid with 12-unit cells. `0x42b16a–0x42b17a` passes the sky object's night factor and tracked position to `0x425470`, which rebuilds the camera-centered 120x120 X/Z region when the position moves over 12 units; Y query range is 0..800. Static candidates from record AABB x/z bounds are assigned to cells. `0x4250c0` chooses nearest candidate by 3D position within the queried cell (not globally), changes attenuation to (0,0.4,0), scales flag-1 color by night, and sets both color alphas to 1. Added a static-candidate grid and World-tab diagnostic, with synthetic tests and a sample at the first real client light. It is not full scene-light rendering: a second runtime dynamic candidate list can replace the selection, client target `0x41a5d0` is called from the separate `0x6d1550` object path and forwards to `0x7924f0`; mapping those model classes and their material/shader behavior is unresolved. The viewer does not yet render those animated objects. Do not apply the light to baked BMDs or the approved tree shader without proof.

### Ordinary world glow, runtime candidate cells, and fog ordering
`0x421483–0x4214d7` chooses ordinary glow mode 1 while night factor <0.5 and mode 2 otherwise, except special modes 3–5. `0x410bf8–0x410c7e` sets the normal mode level and power channels to 0.25 (day) and 0.35 (night) with 5,000 ms linear transitions; `0x411160`, `0x4111a0`, `0x4111f0` update the two channels. The viewer now uses those normal-mode targets from the current value instead of the earlier 0.16/0.13 estimates. The startup and special-mode behavior, constants, exact underwater ordering and all render-target details still need further tracing. The user strength slider scales both parameters, which is a viewer extension, not a client control proven by this trace. 

`0x425922–0x425bc3` fills separate runtime-light candidate lists, capped at three per X/Z grid cell in iteration order; `0x4252ca–0x42544d` compares candidate distances to the current static winner and replaces it only if strictly closer. Runtime candidate +0x34 supplies position, +0x4c insertion radius, +0x04 RGBA where alpha scales RGB; replacement has range 10, attenuation (0,.4,0), RGB diffuse=color*alpha*2, ambient=color*alpha*.2, with output alpha=1. Added separately fed dynamic grid and synthetic cap/selection/color tests, but no actual runtime effect-light producer or animated model rendering exists yet; the on-screen inspector remains static. These mechanics do not relight BMDs.

The depth-based fog approximation previously ran after glow. It now uses a separate encoded-color pass before glow and before underwater distortion, with actual GPU regression for fog-only and fog-plus-bloom. This is still not client per-pass fog: it cannot account for transparencies, reflection fog, per-material decisions, or exact underwater order. The original game's image has not been checked. The approved tree shader remains unchanged.

### Skin light and experimental SKI work (unfinished)

- `0x792a39` gates point-light constants on model state byte `+0x1e0`; `0x792b10` applies directional and optionally point D3D light records. At `0x792d52`, a missing/invalid point range is replaced with 50 in the fixed-function light record. This does not establish which model instances or animation states use this path.
- `src/model_light.rs` implements the arithmetic of the extracted `skin_mesh_m4_ns_dl`/`rigid_mesh_ns_dl` shader as a standalone CPU oracle and WGSL helper. The coefficients are already material-combined, not raw scene-light colors. It is not wired into rendering or yet GPU tested.
- `src/skin.rs` is an experimental bounds-checked subset reader informed by GameForge's GPL-2.0-or-later importer (linked in source). It parses binary standard skinned/rigid records; morph/supple/muscle meshes and text materials deliberately return errors. Only synthetic fixtures have been tested; 1.4.2 assets, skeletons, animation tracks and renderer integration remain unverified/missing.
- The static grid now rebuilds at exactly a 12-unit camera displacement, following the comparison at `0x4254a8`; a regression verifies this and the static-vs-dynamic equal-distance tie.

### Underwater glow research (work in progress)

`0x41eb69` evaluates the camera position with the water-area routine `0x780790` / `0x781c50`, and `0x4214ad` selects glow mode 3 when true (unless higher-priority player modes 4/5 apply). `0x410c84` sets level to .5 and power to .6 over 2 seconds, and starts mono level from 0 toward .5 in 1.5 seconds with color 0xffbbeabb. `0x4114e0` transitions mono back to zero in five seconds in ordinary modes. The original `monofullglow_14` shader differs from `fullglow2_14`: its additive term is blur*original, not blur squared; then it uses `dp3_sat` with (0.35,0.6,0.25), multiplies the result by mono hue, and interpolates by mono level. The viewer now models the underwater case; test coverage is synthetic and water detection, player-state overrides, target formats and overall pass parity are not yet established. The standalone model light helper was compared with a CPU reference on six synthetic GPU compute cases; it remains unused by rendered world models.

### BMD v5 material sidedness (inspection, not Phase 1 completion)
The actual `litmodels.pck` world survey parsed all 16,335 world BMDs without error: 31,802 v4 meshes and 8,777 v5 meshes. The v5 trailing material record has four RGBA float colors, a float power, and a one-byte two-sided flag; 2,363 v5 meshes set it. All 8,777 have diffuse alpha 1.0. The client loads that material into mesh +0x21c (`0x7baa14`), placing its flag at +0x26c; `0x7a98c5–0x7a98d6` disables culling for that flag and `0x7a997d–0x7a9994` restores the previous state. The same rule is now passed per mesh into the existing fragment culling path for local and whole-world BMDs, including reflection. The v4 default is still inferred from client initialization; visual verification remains outstanding. No regional tree shading was changed.

A standalone active regional-box hash reconstruction and tests were added (`client_region_map.rs`). It is deliberately not wired to scene selection: the loaded-block lifecycle and asynchronous arrival order are still insufficiently established. Its ordering should not be represented as exact until those inputs are recovered.

The production fragment shader's per-mesh sidedness was tested with both triangle windings, both normal/reflected projection winding flags, and the user culling override (`src/bin/bmd-sided-gpu-check.rs`); both sides of two-sided material rasterize, and one-sided material follows the applicable winding. This is an automated synthetic software-GPU check, not live-client visual verification.

### Continued scene-model and skin investigation (interim)

The actual shader `shaders/vs/skin_mesh_m4_ns_dl.txt` computes a fourth
weight as `1 - w0 - w1 - w2` and blends four position and normal transforms
before normalizing the normal and lighting. `src/skin_pose.rs` supplies an
isolated CPU oracle and WGSL equivalent, and `skin-pose-gpu-check` compares
four synthetic poses feeding the existing experimental vertex-light helper on
GPU against CPU results. It is not wired to the renderer: palette matrices,
BON/STCK animation, mesh GPU buffers, and real 1.4.2 model compatibility are
still unimplemented/unverified. The checked examples avoid zero-length normals;
normalization of degenerate data can differ on D3D hardware.

`scene-model-light-survey` uses the 73 unique world model placements and the
existing *diagnostic* static light grid. On the inspected `world.ecwld` and
`scenelights.dat`, 15 placements obtain candidates (nine distinct selected
positions) and 58 do not; all 15 candidates differ between day and night.
These are candidate queries, not proof that these models use that light or are
visible. The grid can return a candidate outside its declared range (e.g.
about 215 units versus range 150); shader attenuation and fixed-function range
semantics must be traced separately, not inferred from candidate selection.

SMD dependency parsing now reads the header's skin count instead of assuming
one. This follows the attributed later-client Angelica layout and synthetic
tests only; neither the full tail nor actual 1.4.2 assets have been validated.
Regional-light priority, material application, grass lightmap alignment and
per-pass fog still require investigation. No approved tree lighting was changed.

### Runtime streamed regions (experimental)

The viewer now retains ECWLD block-to-box references at world load and maintains
an active map during camera movement. Rectangle calculation uses 500 units,
`ceil(radius / block_size)`, half-open row/column ranges and edge shifting as
in `0x41c480`; client bounds are `centerX - width/2` and `centerZ + length/2`
(`0x41bc51–0x41bc90`). Existing overlapping blocks retain their references;
new ones are added in row-major order, and departing blocks are released.
Selection traverses the reconstructed hash table, prefers the last containing
local area over the last containing global area, and feeds the existing
in-flight regional endpoint transitions for atmospheric/underwater fog and the
optional bounded tree-lighting path. Camera motion between blocks, not frames,
causes region-map rebuilds. The client's asynchronous load completion order
and transient queue behavior are not fully established; row-major immediate
completion is an approximation. The feature is optional and defaults off.
Existing baked terrain/building materials are not re-lit; scene point lights
still have no confirmed eligible rendered-model path.

### Whole-frame fullscreen gamma

In borderless fullscreen, enabling client gamma now renders the scene and all
eGUI/map overlays into one sRGB-compatible intermediate and applies the
recovered 256-entry, 16-bit ramp as the final GPU pass. The input is read
through a matching UNORM view so the curve operates on encoded framebuffer
values, and the result is decoded only when writing an sRGB presentation
surface. The ramp is applied once, after bloom and UI composition, without
changing the operating-system display gamma. The intermediate/pipeline are
created only while the option is active and are resized with the window.
Windowed mode retains the older optional scene-only approximation; the retail
client instead skips its fullscreen display ramp in windowed mode. Borderless
compositor/monitor behavior and live visual output remain unverified.

### Grass area lightmap selection integrated

`0x6526e0–0x652723` copies the supplied grass-area center to object `+0x38`;
`0x652d9a–0x652da0` passes that exact vector to terrain lookup `0x77f480`, which
returns a day/night texture pair and world rectangle. The grass shader applies
the resulting world-to-lightmap transform to wind-displaced positions. Thus a
single grass object uses the lightmap selected from its area center, not a
separate lookup per vertex or fragment. The instanced renderer now carries
that real center to its fragment shader and can select one tile for the entire
area in main/reflected/minimap passes. An opt-in World setting retains the old
per-fragment tile selection by default. In the experimental mode the atlas UV
is clamped at the selected tile edge; D3D texture addressing and unloaded-page
fallback have not been fully traced, and the terrain atlas mapping itself
remains approximate. Consequently this is not declared exact client parity.

### Actual client SKI, BON and animation data (unfinished model path)

The experimental SKI header previously used synthetic `ASKI` bytes. Actual
1.4.2 binary files serialize the signature as `MOXBIKSA`; corrected the reader
and its fixture. After the correction it parsed all 143 `.ski` files found in
the supplied `gfx.pck` (43 v8, 100 v9). Those are effect assets, not the 73
world model files in `models.pck`, so they do not establish that the latter
are all supported.

Added bounds-checked BON v1–v6 skeleton/embedded-track loading and STCK v1/v2
track loading, including raw v1 segment tables, v2 key IDs and compressed
quaternions, plus interpolated track sampling. All 184 available effect BONs
(67 v5 and 117 v6) and 198 STCK files (all v1) parsed from `gfx.pck`; the v2
track path has only synthetic coverage. One real BON has an end-key index equal
to its key count; the importer-derived segment expansion clamps that end index.
Original joint-to-bone binding, palette matrix construction, STCK timing,
material state, pose application, actual world models and animation rendering
remain unverified or absent. This work does not connect scene lights to any
rendered object and is not an observed visual improvement.

### SMD descriptors (continued)
Expanded the experimental model descriptor parser using the pinned GPL-2.0-or-later GameForge 3D Studio `angelica_model.cc` layout to read physique and track directories, action ranges and joint records, model/effect hangers and properties (declared versions 1–9). Survey of the 1.4.2 gfx archive parsed 124/124 SMD files: 39 v5, 15 v6, 70 v8, containing 169 actions and no hangers/properties. Other versions and real hanger/property records remain unverified; effect models are not the world models. No animation playback, GPU rendering, attachment rendering, or scene-light application was implemented by this parsing change.

### Deferred world-model archive and terrain upload memory
World-model archive inspection was deferred after a storage-limited reconstruction attempt. World models remain uninspected and unintegrated.

Terrain lightmap initialization now uploads each day/night DDS tile directly into its original GPU atlas slot rather than building all full-size atlas pages on the CPU. Uploads of terrain textures, masks and lightmaps are submitted and awaited in bounded batches to reduce temporary wgpu staging growth. The GPU texture layout and shader calculations are unchanged; this reduces loading memory and is not a new visual parity implementation. A real client day-lightmap tile was uploaded and read back byte-identically for both native BC1 and decoded RGBA paths on a local GPU adapter. Full-world peak memory and Windows startup latency remain unmeasured.

### Experimental sky sun and moon
`elementclient.exe`'s CECSunMoon initialization loads `textures/nature/sun.bmp` (0x4b2da0), `textures/nature/sunshine.bmp` (0x4b2ddd), and `textures/nature/moon.tga` (0x4b2e17). Added optional GPU sky discs using the client's sun and moon textures directly from the user's `textures.pck`, and the already recovered sky-object time/azimuth direction; the active region's authored direction supplies azimuth, with a fallback when no region is active. The reflected sky uses the same ray transformation and thus includes the discs. Rendering occurs only in the sky pass; it does not change baked models, trees, or material lighting. The option defaults off and is in World settings. Missing or undecodable textures become transparent. The angular size, choice of sun versus moon at 03:30/21:00, visibility fade and compositing are approximations; sunshine, lens flare, occlusion, region flags and original quad transforms are not implemented. The sky shader pipeline validated on a software GPU, but the actual 117 MB textures archive was not acquired, so these textures and live output have not been verified here. This is not full sky or lighting parity.


### Phase 1 audit and follow-up validation (2026-11-02)

See [the requirement-by-requirement audit](Phase-1-audit-2026-11-02.md) and
[the eight-phase status ledger](Eight-phase-status.md). Phase 1 is not signed off.
PR #1 fixes the viewer's windowed scene-only gamma, shares LUT arithmetic and
adds an executed production whole-frame GPU regression. CI at `8866669` passed
formatting, all binary checks, viewer unit tests, four Phase 1 GPU checks and
the Windows release build. A follow-up aligns the standalone CPU/WGSL light
helper's small-vector fallback and expands its synthetic coverage; it does not
add scene lighting to any rendered world object.

The executable and shader archive are now locally available in ignored storage.
Their fog-state trace distinguishes table fog from the shader's vertex fog
output. In particular, ordinary grass requests table fog, whereas its separate
refraction mode requests vertex fog and uploads c91; the ordinary shader's
constant oFog alone is not evidence that ordinary grass should be fog-free.
Do not remove main-view grass fog or change approved trees on that inference.
The separate exponential refraction fog path remains unfinished.

### Bloom coordinates and exact BMD byte arithmetic

Further direct inspection recovered the blur quad tap step at `0x40edd0`
(horizontal) and `0x40f04b–0x40f053` (vertical): the float at `0x94ed00`
is **4.0**, divided by full width/height. This is not exactly the reciprocal
of the integer quarter-target dimension when the window size is not divisible
by four. Corrected both shader axes. New patterned-image GPU checks compare
all pixels against an independent bilinear-clamp/UNORM8 CPU reference at
64×64, 23×19 and 67×61, with ordinary and monochrome glow and UNORM/sRGB outputs.
The CPU fixture explicitly rejects the old reciprocal-quarter formula beyond
the two-byte GPU tolerance. CI at `909df63` passed these checks and Windows build.

Rechecked `0x7b9bc0`: it truncates night×255, forms integer channel products
with complementary weight 255−w, divides by 256 via a shift and forces alpha
255. The existing shader attempted that blend via unpacked UNORM floats;
it now extracts and combines packed channels as integers before final
normalization, avoiding potential floor-boundary errors from floating-point
round trips. The new compute regression calls the actual production helper
for eight endpoint pairs and all 256 weights (2,048 exact packed-color checks).
This is not completion of other BMD material/alpha/texture-stage behavior.

See [Phase 1 completion checks](Phase-1-completion-checks.md) for the explicit
requirement-by-requirement acceptance results. Unfinished items remain open.

### Skin/rigid material constants and alpha (2026-11-02)

Followed `0x7a6eff` → `0x7929c0` (receiver direction, point position,
attenuation constants) and `0x7a6f07` → `0x7a6110` (material constants).
The latter produces c7 = material ambient × model environment ambient +
material emissive + model additional ambient; c8 = material diffuse ×
directional diffuse. Mesh transparency strictly greater than zero replaces
c8.a with `1 - transparency`, rather than multiplying it. Point-enabled
binding produces c12 = material ambient × point ambient and c11.rgb =
material diffuse × point diffuse, **forcing c11.a to zero** at `0x7a63af`.
`src/model_material.rs` now expresses these CPU/WGSL binding rules separately
from baked BMDs and terrain. This covers the ordinary material coefficients,
not the untraced replacement-material modes or specular path.

The extracted `vs/rigid_mesh_ns.txt` explicitly copies c8.a without Lambert;
`vs/rigid_mesh_ns_dl.txt` multiplies all c8 channels by Lambert. Both add c7
and clamp only above one. Added RGBA shader/reference handling for this
variant difference and extended `model-light-gpu-check` to 31 cases, including
front/back normals, point on/off and transparency endpoints/overrides.
The dependency-free CPU suite now passes 31 tests. GPU results must be read
from CI, not inferred from these CPU results. Neither helper is represented
as completed world-model integration.

The private split models ZIP was reassembled and extracted successfully:
`models.pck` is 888,028,116 bytes. All archive/client content remains ignored.

### Ordinary skin/rigid specular variant is not the non-specular variant

The extracted `rigid_mesh_s_dl.txt` and `skin_mesh_m4_s_dl.txt` both contain
`max r9.x, r9.x, c0.x` before reciprocal attenuation: their point contribution
cannot amplify beyond one. Both preserve c8.a, add c7.a, and omit point alpha
from diffuse output. Their directional specular term uses the normalized half
vector `-light_direction - camera_forward`, the `lit` frontface gate and
material power, then multiplies by c9 and upper-clamps. Added separate CPU/
WGSL functions and 16 more GPU cases (47 total), including near lights that
would expose incorrectly reusing `_ns_dl` attenuation, backfaces and point
on/off. CPU core suite passes 33 tests; CI 41433732886 passed the 47-case GPU comparison and the Windows executable build at 184f7d8.

`morph_rigid_mesh_c4_m1_s_dl.txt` notably lacks that attenuation clamp and
scales alpha with the light terms. It must not be routed through the ordinary
skin/rigid specular helper. The actual client's SKI census contains no morph
meshes, but that absence is not a universal format-support claim.

### Grass refraction fog — production pass integration

`0x651319–0x65134c` selects the refraction grass shader and uploads c91 as
`(1, water_height, 0.075, 0)` (`0x3d99999a` is 0.075). The extracted
`grass_wave_refract.txt` computes submerged depth using the displaced vertex
Y, but the normalized eye ray uses original v0. `exp` / reciprocal produces
`exp2(-0.075 * depth * length(eye-original)/(eye.y-original.y))`. This is not
an `exp`-base-e fog, full camera distance, or the terrain table-fog interval.

The instanced production grass shader now emits this per-vertex factor only
in the below-water refraction pass (`camera.clip.y < 0`). Its fragment path
uses that interpolated factor to blend encoded RGB with water fog color.
Ordinary and reflected grass still use the existing table-fog function;
terrain, baked BMD and approved trees are unchanged. Above-water vertex
factors are not prematurely clamped: clipping/interpolation occurs before
fragment fog saturation. The undefined horizontal/coincident ray cases have
explicit finite safety fallbacks, not a retail-degenerate-behavior claim.

Five CPU tests cover base two, submerged-versus-full distance, oblique rays,
wind-displaced depth/original direction, translation, unclamped vertex
factors and degenerate safety. The dependency-free suite now has 38 tests.
The existing production grass GPU binary adds three rendered comparisons
(two water heights and wind on/off) against independently calculated vertex
fog/interpolation/encoded blending. GPU results are pending validation.
This implements the grass component, not the complete per-pass fog gate.

### Opaque baked-model refraction fog

`0x7a91dd–0x7a9208` binds the lit-model refraction c0 with water height,
fixed density 0.075 and offset zero; `lm_refract_vs.txt` computes the same
base-two eye-ray fog but **clamps above-plane vertex depth to zero** before
exponentiation. This differs from grass and must be preserved across
triangles crossing the water plane. The production opaque BMD vertex path
now outputs that separate factor; its existing baked texture/color path
uses interpolated refraction fog only for the below-water clipped pass.
Ordinary/reflected BMD fog and baked-color lighting remain unchanged; no
point light is added to baked vertex colors. GPU regressions add two BMD
water-height cases alongside the three grass cases. CPU core: 39 tests pass.

Grass commit `1375f2a` passed [CI 41434813651](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41434813651): Linux production GPU regressions and Windows build. The BMD extension is separately pending its own validation.

### Terrain refraction geometry, darkness and fog

Followed environment shader `+0x30` from `0x7c4edb` and renderer selection
`0x7c1714` to `terrain_refract_vs.txt`. `0x7c26e0–0x7c287a` uploads c0;
the density global `0x98f368` starts at 0.075 and the offset is BSS zero
(the executable also contains developer-key adjustments, not enabled here).
The VS projects above-water Y onto the water plane, retains original
position for UV generation and eye ray, emits depth-only darkness
`exp2(-2*density*depth)` and separately emits eye-ray transmittance.
`t2_3layer_batch_14.txt` multiplies encoded material RGB by vertex diffuse
before the ×2 day/night lightmap operation. Thus darkness is not a second
point/directional lighting term and must be applied before color saturation.

Near and far terrain vertex entries now share this production refraction
constructor; above-plane terrain is projected, not discarded before the
projection. The lightmapped material path uses interpolated darkness before
saturation and interpolated exponential fog afterward in encoded RGB.
Ordinary and reflection paths retain their existing geometry/table fog.
Added `terrain-refraction-gpu-check` to CI: three water planes plus main,
reflection and disabled-fog controls exercise these production functions
with an independent interpolation/color oracle. This fixture isolates the
vertex/fog path; it does not validate every terrain texture/layer variant.
CPU core now passes 40 tests; terrain GPU validation is pending.

BMD extension `6a903ad` passed [CI 41435206213](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41435206213), including the two BMD and three grass rendered refraction cases and the Windows executable build.

### Grass area-center lightmap selection and clamp addressing

At `0x652d99–0x652da0`, grass passes the area's center (`area+0x38`) to
`0x77f480`, not each expanded vertex. That function resolves the terrain
block/mask area and returns its day/night lightmap pair plus UV rectangle.
`0x651287–0x65129c` calls `0x77a0e0` with `(stage,3,3)` for stages 1/2;
`0x77a0e0` maps these to D3DTSS_ADDRESSU/V (13/14), proving CLAMP for both
lightmaps. This resolves the previously unconfirmed edge-addressing part.
It does not yet prove the unloaded-pair fixed-function fallback.

The grass area-center path now defaults on (explicit saved legacy false
settings remain respected). Its UI no longer describes edge addressing as
unknown. Both terrain/grass atlas normalization now uses actual texture
extent instead of a hardcoded 4096; normal production textures are still
4096, so this retains their coordinates and supports meaningful small-atlas
regressions. The production grass GPU test adds six day/night × area/legacy
cases on a real 8x8 atlas with distinct neighboring and opposite-edge
texels. They catch selecting the vertex's tile, sampling the wrong edge,
bleeding across tile boundaries, or keeping the hardcoded atlas divisor.
GPU results pending; 41 dependency-free CPU tests pass locally.

Terrain refraction commit `3ab718b` passed [CI 41435695632](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41435695632): all eight GPU binaries, unit/compile/format checks and Windows executable build.

### World external animation dependencies

Resolved SMD8 actions under `SMD parent / track_directory / track_file`,
not the archive root. All 18 external STCK files for the nine BON6 world
chains exist in models.pck. The production parser consumes their complete
payloads: STCK1, 188 joint tracks, 15fps. A native isolated harness also
checks every track ID against its owning BON joint count and samples all
position/quaternion tracks at start/mid/end (564 transform sample times).
All 14 world chains pass; the five older BON5 chains retain their separate
77 embedded tracks. No private client files are committed.

`ecmodel-check` now includes these external dependencies, parser errors,
invalid joint references and nonfinite samples in its failure gate. Added
STCK trailing-data rejection and truncation/tail/joint-bound tests. Native
animation+descriptor tests pass (7); the application build is pending CI.
These data/sampling results do not establish runtime pose matrices, palette
ordering or a world skin draw path. Those implementation gates stay No.

Grass lightmap commit `598393d` passed [CI 41436006986](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41436006986), including all six area/legacy × day/night atlas-edge renders and the new readiness-audit tests. The independent readiness audit still reports P1.01–P1.10 unfinished and no malformed/missing acceptance rows.

### Underwater distortion is an input to glow, not a composite UV change

`0x40fce0` first copies the scene to its capture target. When warp is enabled,
`0x40fd33` calls `0x410250` (the spacewarp path), then `0x40fd58` copies that
result back into the capture target. Only afterward does `0x40fd64` invoke
`0x40fdb0` (quarter copy / glow). Thus the blur input is the separately
quantized warped scene. The old viewer sampled unwarped blur at a warped UV
in the final composite, which is not equivalent. `0x41029b–0x4102a4` also
explicitly sets the captured scene's sampler (stage 1) to CLAMP U/V; the
previous artificial 0.001/0.999 UV inset has no basis in this trace.

Added a full-resolution encoded UNORM warp target/pass before downsample,
used with or without glow and after any screen-fog diagnostic. Kept the
already traced procedural bump amplitude/repeat/scroll. The independent
CPU reference now rasterizes that warp into bytes before its existing glow
passes. A patterned CPU case distinguishes the reversed ordering by more
than the two-byte GPU tolerance. The GPU fixture now compares normal/mono
with/without warp on all three sizes and both output formats (24 cases).
42 core CPU tests pass locally; GPU/build validation pending. This fixes the
traced ordinary/underwater ordering, not every special effect/interaction.

### Preview the two other traced glow modes without gameplay systems

The `0x410bb0` switch targets mode 4 at `0x410cdb` and mode 5 at
`0x410d28`. Mode 4 transitions level/power to 1.0/0.7 in 2s and resets
mono from 0 to 1 in 2s with ARGB `0xffe2ffff`. Mode 5 transitions level/power
to 0.1/0.25 in 1s and uses the ordinary current-to-zero 5s mono fade
(`0x4114e0`). These parameters are now available as explicit Graphics bloom
previews, with persisted bounded settings and `PW_PREVIEW_BLOOM_MODE=4|5`
for offscreen checks. Default remains Automatic; no player-state/gameplay
system or approved tree-material replacement was added.

Native transition tests cover targets, tint, different mono/glow durations,
mode-4-to-5 transition and disabling. Settings tests cover saved modes,
legacy defaults and malformed values. Added both endpoint modes to the
production patterned glow GPU fixture (now 36 cases). 43 core CPU tests and
three isolated production WorldGlow transition tests pass locally; new GPU
cases/application integration pending CI. Additional effect/fade interactions
remain outside this proof; the full P1.08 acceptance decision stays No.

The preceding distortion-order change `d7774c4` passed
[CI 41436969265](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41436969265), including all 24 normal/mono × ordinary/underwater patterned comparisons, all binary checks and the Windows executable build. This also validates compilation of the external-track census changes.

Special-mode validation exposed a numerical-reference limit, not a build
failure: CI 41437959940 reports normal RGBA8 64x64, level=1, power=.7,
mono=1, pixel (2,0) green GPU=232 / scalar reference=229 (3 bytes).
The ideal floating-point bilinear CPU reference does not emulate the GPU's
per-pass/filter rounding exactly. Propagating a one-byte intermediate
rounding difference through full mono gives `(1+.7) * (.35+.6+.25) = 2.04`
bytes, whose integer bound is 3. The fixture now derives that gain bound
with the established floor of 2. A unit test explicitly preserves the
original 2-byte bounds for every preceding ordinary/underwater case and
mode 5; only the stronger mode-4 endpoint gets 3. No production shader was
changed to hide this discrepancy. The rerun must still pass all dimensions,
formats and earlier regressions before this component is called validated.

Special-mode rerun `1eb7aa9` passed
[CI 41438393668](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41438393668): all 36 glow comparisons, all eight GPU binaries, full application checks/tests, seven readiness-audit tests and Windows executable. Local dependency-free core is now 44 passing tests. The full-scope readiness audit still has ten No decisions; no confirmation poll is justified yet.

#### Next unresolved glow inputs (not implemented)

Both `fullglow2_14` and `monofullglow_14` multiply final output by PS c3.
`0x411340` converts the caller ARGB (`renderer+0x18`, passed at `0x4100a1`)
and builds c3 at `0x4113be–0x41145f`: RGB is additionally multiplied by
`1 - effect_object[+0x20]`, alpha by `1 - effect_object[+0x30]`.
`0x411100` sets the +0x20 channel; `0x4114a0(true)` transitions its current
value to zero over 1000ms. Constructor `0x4110c0` initializes these fade
channels to zero. Current viewer reproduces default white/no-fade case,
not all caller-color/fade combinations. The extra renderer flag +6 calls
`0x410970` after glow; its exact effect/classification still needs tracing.
These remain concrete reasons not to promote the full P1.08 gate to Yes.

#### Narrower receiver-class identity evidence

The CECModel type descriptor is `0x959240` (its name starts at `0x959248`).
BCD `0x902e58` is in the base array at `0x902e88` for CECModelWrapper;
CHD `0x902ea0`, COL `0x902eb0` resolve its vtable to `0x8dabb4`.
`0x4a7214` calls base constructor `0x6cef80` and then assigns that wrapper
vtable. Wrapper position setter `0x6d4210` forwards to the +0xc4 skin object.
This resolves a derived wrapper relationship, not every scene receiver's
runtime flags or world placement class. Do not relight baked BMD/terrain
from this limited type evidence.

### Confirmation checkpoint and caller glow modulation

The user confirmed **No — continue implementation** at the explicit Phase 1
checkpoint. The audit still has P1.01–P1.10 No. Completion approval is not
available while any gate remains open; checkpoints report No and resume work.

Implemented the traced c3 binding from `0x411340–0x41145f`: caller ARGB,
RGB multiplied by `1-brightness_fade`, alpha by `1-opacity_fade`. Both
ordinary and monochrome glow multiply their result by c3; the renderer sets
SRCALPHA/INVSRCALPHA at `0x41007b–0x410089`. The viewer now composites that
source over the pre-glow scene in encoded RGB, before gamma/output encoding.
The source is clamped for the UNORM blend; comparisons cover saturated glow
as well as partial and zero opacity. Default white/zero fades retain the
previous result. Invalid fade preview values are bounded for viewer safety,
not claimed to be recovered retail invalid-input behavior.

The Graphics controls and remembered settings expose caller tint/opacity and
both independent fade values, explicitly as previews. Offscreen capture
supports `PW_PREVIEW_BLOOM_TINT` (hex ARGB),
`PW_PREVIEW_BLOOM_BRIGHTNESS_FADE`, and `PW_PREVIEW_BLOOM_OPACITY_FADE`.
These static inputs are wired into the actual renderer, not only a helper.
Automatic client fade-event triggers/transition timing are not implemented
by these controls and remain open.

The patterned production GPU fixture adds four modulation states to every
previous case: neutral, opaque colored, independently faded translucent,
and zero caller alpha (144 total). Its independent CPU reference applies
modulation before the encoded blend, not to an already-quantized result.
Core CPU suite passes 48 tests. Full application/GPU validation pending.

Caller modulation cycle `b66e905` passed
[CI 41440063378](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41440063378): all 144 tinted/faded glow comparisons, all eight production GPU binaries, Linux application checks/tests and Windows executable build. Readiness remains No for P1.01–P1.10; this is component validation, not Phase 1 completion.

### Ordinary bone palette prerequisites for the receiving-model renderer

Continued without another user question, as requested. The readiness audit
still requires further implementation; no No was relabeled as Yes.

Followed `0x7b4b63–0x7b4c2b`: normalize sampled quaternion, write its local
matrix and sampled translation. `0x7b4c35` initializes the extra matrix to
identity (`0x787fa0`), then `0x7b4dc9` composes through the parent.
`0x79269c`/`0x7926d2` combine inverse bind, bone transform and model transform
before GPU transpose. The new ordinary, unscaled pose evaluator maps track
IDs through `joint.child` instead of assuming joint ID equals bone index,
processes parents before children without recursion, and builds the skin
palette plus separate model/root slot. v9 skin names reorder the palette;
v8 retains direct skeleton indices. Reflection/scale controllers and
non-animation joints fail explicitly, not silently draw a guessed pose.

A native production-reader harness evaluated all 14 actual world chains:
69 start/mid/end poses from 18 external action files and five embedded-track
skeletons, with a nonidentity model rotation/translation, then 26,643 finite
vertex deformations through production skin deformation. Three crafted
palette tests separately verify inverse-bind order, joint/bone remapping,
parent ordering, v9 name reordering/root slot and invalid/unsupported inputs.
The combined native parser/material/pose suite passes 19 tests.
`ecmodel-check` now includes sampled pose/palette/deformed-vertex failures
in its nonzero-exit gate, rather than counting descriptor parsing as proof
of a usable pose.

Added the existing four-case skin-deformation-plus-lighting GPU binary to CI
(it was previously only compiled) and a second eight-case palette-input GPU
fixture with rotations, nonuniform scales, implicit fourth weights, negative
weights and zero normals. Full build/GPU validation pending. These fixtures
are synthetic compute checks, not a world-model screenshot. World drawing,
action scheduling and all receiving-class coverage remain open.

The ECM payload audit also found 36 EventType-100 GFX references across the
world descriptors, even though the SMD hanger count is zero. SMD hanger
absence must NOT be mistaken for absence of ECM-attached effects. Combined
action names also differ from underlying SMD actions (e.g. 海马右 `飘动` maps
to `飘动右`, 浑天仪 `旋转` maps to `转`), and the plant has four base actions.
Do not enable a guessed first-SMD-action renderer or claim the dependency
survey proves these action/effect semantics.

#### Follow-up: post-load eligibility, action candidates and descriptor metadata

The previously missing `+0x2f4` enabling write is at **0x6cf930**: after
loading the skin model through `0x41a3e0` and storing it at `+0xc4`, the
initializer assigns `+0x2f4 = (+0xc4 != nullptr)` (`0x6cf91f–0x6cf930`).
This is not constructor default behavior: unsuccessful/no-skin loads remain
ineligible. Together with constructor `+0x34d=1`, it supplies the ordinary
successful-load eligibility predicate for `0x6d1558–0x6d1578`.
`0x6d28a0` disables independent lighting after the child skin-model sharing
call `0x793610`; detach restores `+0x2f4=1` only when `+0xc4` exists
(`0x6d2ad8–0x6d2ae2`). Do not apply a blanket always-lit rule to shared child
models or a failed load. This is source evidence for the runtime consumer,
not evidence that the consumer is already wired into our world renderer.

The ECM reader now retains combined/base action names, start times, old
LoopCount and newer loop ranges, optional speed/render header fields, and
scoped event fields. Missing legacy color/blend/speed fields stay absent,
not guessed defaults. Counts, malformed timing, inverted nonnegative loop
ranges and nonfinite/nonpositive speed are rejected. EventType-100 GFX
paths are separate from SMD hangers and unknown event kinds are retained.

**Correction to any combined-only assumption:** seven plant placements use
SMD base names `站立1`/`站立3`, not their ECM's combined name. The native check
initially rejected those seven; it now reports candidate namespaces rather
than imposing an unproved combined-only playback rule. All **73** placements
have a combined-name (66) or direct-base-name (7) candidate, all **27** base
references exist in their SMDs, and all **36** GFX references are retained.
All **14** dependency chains again pass, including **69** poses/**26,643**
finite vertex deformations. This does NOT establish namespace priority,
random-loop scheduling, GFX playback or visual parity. The combined native
suite passes **22** tests. `ecmodel-check` exposes these dependencies and
name-candidate checks explicitly; it does not validate gfx.pck.

The preceding palette implementation **c544392** passed full CI
**41441677811**, including application/test builds, Linux GPU checks
(the two newly executed skin GPU binaries included) and Windows release.
Descriptor metadata changes described in this subsection still require
full CI validation. Phase1 readiness remains open.

#### Palette normalization edge regression

ECM metadata head `bb6ec74` passed full Linux/application/unit/ten-GPU-binary
and Windows release CI **41442128584**. Continued the readiness audit rather
than treating that pass as completion.

The broader palette-input fixture identified a CPU/WGSL mismatch for tiny
nonzero normals: the WGSL helper erased lengths below 1e-6, whereas the CPU
normalizes them. Removed the arbitrary epsilon cutoff, retaining an explicit
zero-length guard. Added four tiny-normal GPU cases (12 palette-input cases
total); this is an arithmetic consistency correction, not a claim of retail
image parity. CPU deformation now explicitly rejects overflowed intermediate
position/normal values instead of returning an Ok with nonfinite geometry.
The combined native suite passes 23 tests; new GPU validation pending.

#### Retained model assets and single-base loop policy

Normalization fix `b5d7d4d` passed full CI **41442531115** (including all 12
palette-input GPU cases and the Windows executable). The full readiness gate
still reports P1.01–P1.10 No with no audit-format errors; work continues.

Added a retained `ModelAsset` load/evaluation path: normalized ECM→SMD→BON,
all referenced skins and per-action external/embedded tracks, reference
validation, ordinary palette and deformed mesh batches with stable skin/mesh
identities. It uses caller-owned archive reads and preserves dependency error
context; no missing-asset placeholders or fallback-to-another-action tracks.
`ecmodel-check` now exercises this shared asset path, rather than only
independent reader probes. Native actual-world validation again passed all
14 assets/69 poses/26,643 deformations through that retained API. The combined
native suite now passes 26 tests. This API is not yet a rendered world draw;
texture/material binding, instance submission and verified scheduling remain
required. It explicitly rejects rigid pose binding rather than applying the
weighted inverse-bind palette in an unproved coordinate space.

A further loader trace proves a subtle loop policy: `0x6f2061–0x6f2079`
forces both sole-base bounds to -1 when the combined loop count is -1.
`0x6f2004–0x6f2012` initializes base bounds to 1; `0x6f2a93–0x6f2b38`
reads legacy single counts below version 36 and min/max ranges from version
36 onward. The production descriptor helper now exposes that effective
single-base policy and tests the override/default/bad-index cases. It does
not pretend to implement elapsed-time/random/event scheduling.

Correction to the earlier defensive metadata guard: raw finite zero/reverse
speed values are now retained instead of being declared invalid. The loader
uses a raw `%f` store (`0x6f1f96–0x6f1fa8`); a positive-only decoding
constraint was unproved. Nonfinite speed is still rejected. Playback policy
for these values is a separate responsibility, not a guessed parser default.

#### Source-backed model texture lookup and authored mip payloads

Retained model asset head `97d976e` passed full CI **41443151784**.
Continued toward the material/draw consumer. Skin texture lookup is now
traced, rather than inferred from nearby files: `0x79491b` obtains the skin
parent and `0x794928` the filename; `0x794934` removes the last extension
(`0x764540` imports `strrchr`). `0x794952–0x794991` construct
`<skin-parent>/Tex_<skin-stem>/`; `0x7949bb–0x7949ca` construct the generic
`<skin-parent>/Textures/`. The first texture-manager load is at `0x794a63`;
failure/null texture data triggers release and the generic retry at
`0x794add`. The new retained skin API uses that exact ordered retry, including
fallback after decoder failure, and retains both dependency errors if neither
load succeeds. No guessed same-directory fallback.

Actual world: **41** texture references resolve (**16** skin-specific windmill
references, **25** generic); **39** distinct DDS payloads total **3,676,184**
bytes. Production DDS parsing validates **383** referenced authored mip levels
(363 distinct levels), retaining raw DXT1/DXT3/DXT5 blocks and exact level
boundaries rather than replacing the client's mips with generated averages.
All 39 actual files are DXT1 or DXT3 with exact full payload lengths. Separately,
Pillow 13 decoded all 39 base images: **3,653,632** pixels, **29** opaque and
**10** graded-alpha images. This is pixel-decoder feasibility, not proof of
renderer alpha semantics or sampler state. DDS cube/volume/DX10/uncompressed
formats fail explicitly until supported.

Native retained load/texture/palette/mesh test: **14 passed/0 failed, 69 poses,
26,643 finite deformations, 41 DDS references/383 mips**. Combined native
reader/material/asset suite: **31 passed**. Dependency-free core now includes
DDS truncation/format/mip tests: **51 passed**. CI validation of this texture
lookup/payload follow-up is pending. The actual receiving-model draw is still
absent; readiness has not been relabeled.

#### Authored-mip GPU upload stage

Texture lookup/payload head `672f9dd` passed full CI **41443902584**.
Added production GPU upload of the retained DDS mip chain: native BC1/BC2/BC3
when the enabled device supports it, otherwise a decoded UNORM fallback.
Neither path regenerates authored mip levels or introduces sRGB sampling.
The fallback decodes complete stored blocks and crops tiny/non-block-aligned
mips, and explicitly retains DXT1's authored one-bit transparency even with
an RGB-only image decoder. Upload rejects device dimension overruns.

Added a GPU check for 32 per-mip RGBA comparisons: all levels of differently
colored/alpha DXT1/DXT3/DXT5 chains, including 2x2 and 1x1, both auto/native
and forced-decoded paths, plus transparent BC1. The test reports whether
native BC was actually enabled; it must not imply native testing on a device
without BC support. Full CI validation pending. This remains a texture stage,
not an actual world-model material shader or sampler/blending parity proof.

#### Recovered ordinary render defaults and latest validation

Authored-mip upload head **f74fc8a** passed full CI **41444228075**: Linux
application/all-target/unit checks, **eleven** GPU binaries (including the
32 mip-RGBA comparisons), and the Windows release executable. The GPU program
reports native-BC availability, but the run's raw log download is blocked by
the results-receiver EOF; do not claim the native-BC flag's value from an
empty downloaded log. The successful check status proves the test passed,
not an unobserved hardware-feature value.

Completed the previously unproved legacy descriptor defaults: static-data
constructor `0x6d453f` sets OrgColor to white, `0x6d4547` sets DefSpeed to 1,
and `0x6d46b5/0x6d46bc` set SrcBlend/DestBlend to 5/6. `0x6cfa69–0x6cfa7e`
copy those blend fields into the loaded skin model. The production descriptor
now exposes these recovered effective defaults while retaining the raw
optional fields for auditing; explicit zero speed/custom blends/colors are
not overwritten. Combined native suite: **32 passed**.

Two further runtime prerequisites: `0x6cfa64` pushes 1 to `0x7924b0`, whose
`0x7924c1` sets the skin-model `+0x1a1` specular selector (and propagates it to
children). Thus ordinary successful ECM loading enables that selector; do not
choose `_ns` merely because it was the first isolated helper implemented.
Skin-renderer constructor `0x7a4d16` sets color op 4 (MODULATE), with override
application at `0x7a6768` and restore to 4 at `0x7a69e0`. This differs from the
baked BMD path's MODULATE2X; the receiver's texture stage must not reuse that
baked multiplication. Actual model submission/material/pass integration is
still required.

Downloaded the actual `world_light.dat` and the `world_1.t2lm`/`t2lm1` pair
for the next data checks, without adding client assets to Git. The editor
light file has 2,997 108-byte records, distinct from the runtime scene-light
file's 1,993 112-byte records. Position association is not exact (1,966 of
1,993 runtime positions match at least one editor record), so these are NOT
interchangeable sources. Do not add the editor lights to baked surfaces.
The selected lightmap pair has the expected T2LM2/16×256 header; this alone
does not close the atlas/fallback census.

#### Apply bounded DDS validation to the existing terrain consumer

Render-defaults head **0d201e8** passed full CI **41444973586**. Continued
checking actual consumers, without approving the still-open Phase1 gates.

The existing terrain BC upload checked only DDS magic/FourCC and a minimum
length, then uploaded a fixed slice; unlike its decoded fallback, it did not
validate the authored width/height. It now uses the bounded DDS reader,
requires the supported single 256×256 DXT1 level and submits that validated
level's bytes. This leaves valid tile color/coordinates/sampler arithmetic
unchanged and prevents malformed/mismatched compressed tiles from bypassing
the fallback's dimension check. The downloaded real day/night pair passes
all **32** tile payload/dimension checks through this production DDS reader.
Full atlas and unloaded-pair semantics still require work; one pair is not a
full-world census. Full CI for this consumer hardening is pending.

#### Receiver entry census and camera fallback for the selection center

Terrain DDS consumer head **cafcab8** passed full CI **41445434461**:
Linux application/unit/eleven-GPU checks and Windows release. Readiness still
has ten No gates; the world receiving-model draw remains the next integration
step, not an approved completion.

A raw direct-call census of every executable PE section finds exactly these
entry edges: `0x6d1578 -> 0x41a5d0`, `0x41a740 -> 0x4250c0`, and
`0x42b17a -> 0x425470`. There are no absolute-address references to those
three function entries in the binary. This documents the ordinary traced
consumer chain, not a claim to have implemented every subclass/material.

The selection update's center is no longer an unproved always-player rule.
`0x42afa2–0x42afab` fills the stack vector through `0x438290`; `0x42b16d`
passes that same vector to `0x425470`. `0x438290–0x4382b6` uses the tracked
object's +0x3c position only when the object exists and its +0x91c flag is set.
Otherwise `0x4382b9–0x4382e7` obtains the supplied/global viewport camera and
uses `0x7744f0` (camera position, following its parent-camera flag if needed).
A no-player/no-tracked-object world viewer therefore has an actual client
camera-position fallback to implement; it need not invent a simulated player.
Dynamic effect-light production/lifetime and the runtime receiver submission
still remain open.

### Receiving-model raster integration (next confirmation cycle)

Continued directly after the user's instruction, without asking for another
continuation decision. Added an actual ordinary receiving-model raster stage,
not just compute-helper tests: pre-posed mesh buffers, material/light uniforms,
texture sampling, specular/non-specular vertex variants, supplied vertex fog,
clipping, sidedness, alpha test and encoded-space SRCALPHA blending. It only
accepts encoded UNORM targets; it rejects sRGB attachments rather than blending
model alpha in linear space. Both color and alpha use the source blend factors,
not wgpu's convenience source-over alpha equation. Depth comparison/write policy
and sampler are explicit caller inputs (the viewer uses reversed depth).

A new render-to-texture regression exercises the production raster stage with
120 RGBA comparisons covering light/material variants, fog endpoints/midpoint,
clip rejection, alpha rejection and reversed/ordinary face decisions. This is
not yet scene submission or complete shader/material variant coverage. Full
CI validation is pending, so P1.03 remains No. Next: validate the raster stage
and connect retained world instances to it without relighting baked objects.

Raster checkpoint: commit `3f76217`, CI `41447009510`, completed successfully
on Linux and Windows, including all 120 production raster cases. The earlier
`fb7b1a8` run failed to compile the depth-state tuple; this was corrected before
that successful run. This validates the isolated raster stage, not submission.

The next extension adds pixel/table linear fog using reciprocal fragment W
(perspective view depth), separately from explicitly supplied vertex fog. The
ordinary skin shader emits no `oFog`; its `oD1.w` is explicitly 1 in both the
specular and non-specular variants. Table fog therefore must not be substituted
with interpolation of per-vertex radial distances. The test now intends 144
cases: the extra case overrides three distinct vertex-fog values with table
fog at depth 1, start 0/end 2. Validation of this extension is pending.

Additional material binding evidence: `7a62db–7a630f` multiplies all four
material-specular components by directional-light specular for constant 9;
this is distinct from directional diffuse. The shader header independently
identifies c9 as `material.specular * dir_light.specular`. A diagnostic through
the production retained asset reader inspected all 23 loaded mesh materials:
all have specular RGB zero, alpha 1 (powers include 0 and approximately 10).
This does not justify disabling the specular shader variant: its diffuse-alpha
calculation still differs. Nonzero specular materials must use the actual light
specular, not an invented copy of directional diffuse.

Alpha-state follow-up (verified instruction boundaries): ordinary receiver
setup `41a32e–41a349` selects GREATER_EQUAL (7) and reference 0x54;
`42b479–42b4a3` enables testing but selects GREATER (5), also reference 0x54.
The skin queue `7a76a0` saves/restores test enable, applies model blend factors,
and enables testing for model+2ac; this does not independently establish the
inherited comparison function. Accordingly the raster API now takes the actual
D3DCMPFUNC (0 disabled, 1–8 the eight comparisons), not a boolean that silently
assumes GREATER_EQUAL. The diagnostic adds all eight comparisons against
reference 1 with nonopaque fragment alpha: 336 intended RGBA cases total.
This follow-up requires full CI validation; runtime pass/state wiring is still
not complete. No readiness point has been changed to Yes on this evidence.

Validation checkpoint: `91a17fb`, CI `41447611763`, passed Linux and Windows.
All twelve production GPU regression binaries ran, including **336 ordinary
model raster RGBA cases**, table-fog override, and all alpha comparison modes.
The local core suite passed 52 tests. The seven readiness-audit tests passed.
Internal confirmation still returns ten unfinished points and no audit errors;
the raster API now exposes the pass inputs but world submission is not wired.

### Retained asset draw-contract confirmation

Added production `model_draw::prepare`: joins every posed mesh to its authored
UVs, index topology, material and source-ordered texture paths. Validates full
mesh coverage/uniqueness before GPU writes; rejects changed topology, bad
indices/nonfinite inputs and unsupported ECM color/blend overrides instead of
silently drawing a partial model. Three regressions cover the join and errors.
The combined native suite passes 36 tests. The actual world corpus passes the
new production contract for all 14 assets / 69 poses / 26,643 posed vertices.
This closes the asset-to-draw-data subtask, not scene submission or scheduling.
Confirmation: P1.03 remains No; all ten full-scope No gates remain. Continue to
GPU draw ownership/update and actual scene submission.

### Retained GPU draw ownership confirmation

Commit `7a282c9`, CI `41448989595`: Linux and Windows passed. The thirteenth
production GPU binary exercises `model_draw_gpu::Draw`, not hand-built raster
bindings: two authored material meshes, shared DDS upload, preferred-path
decode failure followed by fallback, material/light frame updates, rejected
last-mesh update atomicity, and movement of actual posed GPU vertices. One draw
owns its pass uniforms; distinct reflection/refraction passes cannot overwrite
an earlier encoded pass via a shared mutable uniform. Full-set CPU validation
precedes queue writes. Unsupported texture/topology replacement is rejected.
Confirmation: this closes retained GPU ownership/update, but P1.03 is still No
until world frame/pass submission and verified action scheduling are connected.
The full-scope readiness check still reports ten No; continue implementation.

### Retained ECWLD placements and shared dependencies

The viewer now retains the parsed ECWLD models, rather than dropping their
placements after printing an unsupported count. The production world asset
loader shares each normalized ECM chain across instances, preserves failures
and unloaded placements by name, and uses the production PCK reader for client
I/O. Missing chains do not become placeholder meshes or erase baked scenery.
The extracted-dependency I/O test retains all 14 assets and 73 placements with
zero failures. Its combined native suite passes 37 tests. The PCK-backed viewer
integration requires the next full CI run; animation scheduling/submission is
explicitly not enabled by this load step.

World-retention checkpoint: `62260fb`, CI `41449426963`, passed Linux checks,
all thirteen GPU regression binaries and Windows. Retaining the actual model
assets is now application code, not only a separate corpus diagnostic.
Confirmation: dependency retention Yes; automatic pass submission No; the
full-scope audit still has ten No. Continue.

### Scene playback dispatch: corrected namespace assumption

Public related-client source was found at
`https://github.com/coussiraty/PWClient-152`, pinned commit
`f321e68953a5502ff5673d6099dab8c193c597a0`. This is a **1.5.2 reference**, not
proof of 1.4.2 parity. Its `ElementCore/ElementClient/EC_ManOrnament.cpp`
(LoadECModel / DeliverLoadedECModels), `Common/GfxCommon2/EC_Model.cpp`
(PlayActionByName), and `A3DCombActDynData.cpp` supplied names for targeted
cross-checks. Do not transplant later-version branches or assume identical
layouts. No reference implementation was copied into the repository.

The actual 1.4.2 executable confirms the important scene dispatch:
- `5b3a4f–5b3a9a` queries the ECM combined-action map for the scene name.
- `5b3a9c–5b3aca` selects that name when present; otherwise it passes the
  literal at `95a750`, decoded directly from the executable as **休闲**.
- `6d1df0–6d1e98` looks up the combined map and returns false if missing;
  there is no direct-SMD fallback in that path.
- `5b3acf–5b3ae0` applies the initial `rand() % 1000` tick.

The production `EcModel::scene_action` and retained instance records now use
this source-backed decision. On the actual 73 placements: **66 combined
selections, seven failed idle fallbacks**. The earlier diagnostic's seven
“direct SMD candidates” must NOT become direct playback in the viewer. It was
only a candidate census, not dispatch evidence. Named-base coincidence is not
permission to animate a model the client leaves without an active action.

The retained model API now also exposes authored bind-pose evaluation, without
choosing any animation. All 14 real assets' bind poses pass production draw
preparation. A regression checks model/root transformation in this path. The
combined native suite passes 39 tests. Full CI of this dispatch/bind-pose change
is pending. Random/tick/transition playback still needs its own integration.

### Ordinary world playback clock confirmation

Added `model_playback::Playback` and compile it for retained scene instances.
Scope is explicit: one base, start 0, fixed/infinite loop policy, ordinary
nonnegative speed; multi-base/random/delayed policies fail rather than choose
a first base. This covers **all 66 actual playing world placements**; the
seven failed scene-name/idle lookups have no sampled action and use bind pose.

Further exact executable checks, guided by the pinned related-client source:
`7c5580` truncates frame*1000/fps to milliseconds; `7c50e0–7c5118` sets
span=end_ms-start_ms+1 (**one millisecond**, not one extra frame).
`6d208e–6d20a1` multiplies combined speed by descriptor default speed;
`6d0e4f–6d0e62` truncates delta*speed per tick, without fractional carry.
`6f7890–6f7a53` matches the ordinary combined base progression/loop decision;
`6f7a60–6f7a7b` subtracts the base time span on a loop. No later-version
random/event/transition behavior was enabled by analogy alone.

Four native regressions test inclusive-ms wrap, large-tick multi-wrap, finite
end-pose hold, failed dispatch, forced infinite sole-base policy, per-tick speed
truncation and explicit unsupported-policy rejection. Combined suite: 43 pass.
Actual-world validation advances all 73 programs through five clock/pose/draw
preparation checkpoints each: 365 checks, zero failures. Full CI pending.
Confirmation: ordinary clock subtask Yes locally; P1.03 still No because GPU
world-pass submission remains missing. Continue implementation and CI checks.

### World main-pass harness confirmation / ordinary queue correction

The opt-in `PW_WORLD_MODELS_TEST` harness now calls the actual viewer main
scene pass: retained instances -> source-selected clock/bind pose -> source
placement matrix -> authored geometry/material/textures -> regional light and
camera-centered static grid -> encoded UNORM attachment with reversed depth.
It does not relight BMDs, terrain or approved trees. Distinct per-pass variants,
source random initial phase, effect/material overrides and complete alpha queues
are not enabled; this remains a harness, not default parity.

Commit `f1fda96`, CI `41451041196`, passed Linux and Windows, including the
extended world-instance GPU regression: production placement/bind/texture/draw
code, depth attachment, regional updates and night-scaled static point ambient.
Native placement-basis test initially compared a normalized float exactly
(0.99999994 vs 1); changed to 1e-6 float tolerance, then all 44 native tests
passed. Actual 73 placements pass 365 world-transform clock/pose/draw-preparation
checks. Confirmation: harness integration Yes; P1.03 full/default integration
No; ten full-scope No gates remain. Continue.

**Important 1.4.2 versus related-source difference:** the actual executable
`7a5874–7a5886` checks texture+36 (shader texture) AND texture+35 (alpha texture)
for alpha-shader queue eligibility. The 1.5.2 `IsModelAlphaMesh` source has the
shader-texture condition commented out. Copying that later rule would route
ordinary graded DDS files to the wrong pass. `7a588c–7a58c2` then tests mesh
transparency >=0, nondefault blend factors, or that alpha-shader flag.
`78ad42–78ad62` confirms DDS DXT3/5 alpha flags, excluding DXT1.

Added production queue classification preserving the 1.4.2 conjunction and
separate authored DDS alpha metadata through decoded/native uploads. Ordinary
scene DDS + default transparency -1 is the non-alpha queue, even for DXT3.
The actual world caller `42b479–42b4a3` uses GREATER/84 alpha test; blending is
separate. The raster/draw API now supports a blend-disabled pipeline, rather
than forcing SRCALPHA on the ordinary queue. The world GPU regression is
extended to a graded-alpha DXT3 (136 alpha): source RGB must remain unattenuated
when blending is off. New GPU validation pending. Core CPU suite: 56 passing.

CI `41451770107` (commit `93b41c9`) passed Linux/Windows and all GPU
regressions, including graded DXT3 alpha 136 through the blend-disabled
ordinary world queue. Confirmation: queue correction Yes; full P1.03 No.

### Ordinary model initialization, winding, and clipped water passes

The client calls MSVCRT `rand` through IAT `8d5478` before the initial
`Tick(rand()%1000)` in scene model loading (`5b3a32–5b3ae0`). Added the
MSVCRT wrapping LCG with a deterministic per-world seed (default 1;
`PW_MODEL_RANDOM_SEED` accepts u32) and one initial tick per instance.
Native regression verifies first phases 41,467,334,500,169. This preserves
the initialization distribution/clock operation, NOT the client's global RNG
sequence or asynchronous loading order; no byte-exact initial-phase claim.

The viewer projection mirrors X. The opt-in model harness formerly used
unmirrored front interpretation, unlike the existing BMD caller. Actual SKI
cross(edge1,edge2) dot normal census is predominantly positive across all
14 world assets (e.g. horse 1897/6 positive/negative; windmill 901/0).
The shared model-pass caller now derives winding from the existing pass
uniform: main/refraction reverse front; reflected Y plus projected X restores
the original winding. Added one-sided GPU checks for correct main winding,
explicit rejection with wrong winding, reflection double-flip, world-space
clipping and table fog. These new GPU cases await CI.

Related 1.5.2 `CECOrnamentMan::RenderForReflect/RenderForRefract` was checked
against actual 1.4.2 calls at `5b2092` and `5b23ae`. In particular
`5b2336–5b2354` gates model refraction on a nonempty model table, underwater
camera flag, and water effect level 2. Models must NOT be blindly drawn into
the above-water camera's underwater-terrain refraction pass. The opt-in
harness now submits ordinary models to reflection and underwater-camera
above-plane refraction, using each pass's matrix, clip plane and air fog.
Encoded UNORM views avoid a second sRGB conversion. Main advances playback;
extra passes tick zero. Queue writes occur after preceding pass submissions,
so reusing a model uniform buffer cannot overwrite an earlier queued draw.

Native suite: 45 passing; core CPU suite: 56 passing. Harness remains opt-in.
Default enablement, all queue/material variants, and runtime FX light lifecycle
remain open. Confirmation: pass implementation present; full P1.03 and P1.10
No pending complete coverage and new CI evidence; continue.

CI `41452346626`, head `f003127`, passed Linux/Windows and all 13 GPU
binaries, including one-sided main/reflection winding, world clipping and
fog regressions. Confirmation: these harness pass checks Yes; full P1.03 No.

### T2LM independent record lengths and checked atlas layout

Found a separate production loader bug: it inferred tile length from the
next mask area's offset. T2LM's offset table is indexed by mask area, not
payload storage order. The v2 header gives an explicit common record length.
Related `A3DTerrain2File.h` explicitly warns not to assume index storage
order; actual 1.4.2 `7a3268–7a3286` seeks the indexed offset then reads
`subloader+888` bytes, and `7a32da–7a32f5` passes that same byte count to DDS
loading. Both actual selected files have flags=1 and record size=32896.

Added a dependency-free production parser that uses the declared record
size, validates DDS flags/header/ranges/nonoverlap and accepts unordered
records. Both native BC1 and decoded RGBA paths now perform identical DDS
format/mip/dimension validation. Added checked rectangular atlas mapping,
rejecting inconsistent area/subterrain counts instead of dividing by zero or
leaving unnamed slots. Four new CPU tests cover reversed record order,
truncation, overlap, unsupported headers and bijective rectangular layouts.
Core suite: 60 passing. Renderer/GPU validation for this change pending.

`tools/validate-terrain-lightmaps.rs` is an asset-free, dependency-free
reproducible census tool. Actual TRN2: 32x44 mask areas, 88 subterrain pairs,
1408 destinations per day/night set. Available downloaded pair validates
32 DDS records; 174 files are not local. The tool exits 1 and explicitly
reports incomplete census rather than calling that a full-world success.
Drive metadata lists all 176 corresponding 526424-byte lightmap files;
metadata is not payload validation. Full corpus and unloaded-pair material
fallback remain open, so P1.06 remains No. Continue.

CI `41452828587` (`e9d0735`) passed Linux/Windows and all 13 GPU binaries.
Confirmation: independent record lengths / checked layout Yes; full P1.06 No.

### Separate terrain and grass missing-map policies

Actual 1.4.2 terrain `ApplyLightMap` is `7c2650`: day texture is mask+0c,
night is mask+10; missing night falls back to day (`7c2678–7c2683`), while
missing day returns without binding. Lighting disabled uses environment white
texture. This agrees with the related-source routine, but it is NOT grass's
policy. Grass `652db4–652dc1` requires BOTH maps; otherwise `652e84–652ec5`
sets stage0 SELECTARG1 RGB, disables stage1 and retains texture-alpha
modulation. Related `EL_GrassType.cpp` confirms the same no-map branch.

Corrected the production grass no-lightmap shader path to retain texture RGB,
not apply the provisional global directional multiplier. Complete-pair atlas
sampling, fade, alpha test and per-pass fog are unchanged. Added a GPU case
with no-map selection and deliberately black scene lighting; expected output
is the original texture color. The current strict terrain loader still
requires files, so per-area missing/unloaded metadata and lifecycle remain
open, as does the full-world payload census. No claim that a global no-map
shader switch implements per-area unloading. GPU validation pending.

CI `41453221464` (`7908c06`) passed Linux/Windows and all GPU regressions,
including deliberately dark scene lighting with the no-map grass fallback.
Confirmation: no-map shader RGB policy Yes; per-area availability lifecycle
and full P1.06 still No. Continue.

### Enable the verified ordinary world receiver by default

The ordinary model renderer no longer requires `PW_WORLD_MODELS_TEST`.
It now initializes when retained instances exist, unless the explicit
`PW_DISABLE_WORLD_MODELS` diagnostic opt-out is set. Empty/failed dependency
sets do not require reopening models.pck just to construct an empty renderer.
Main/reflection/underwater-camera refraction retain the validated state and
shader paths. The approved tree and baked receiving exclusions are unchanged.
The World UI reports ready/total and rejected instance counts, including
retained dependency failures. GPU regression now verifies status transitions
from pending to drawn to explicit rejection; a rejected instance cannot later
be counted ready by silently retrying with a stand-in.

This removes the opt-in/default integration gap, not the remaining alpha and
effect-material variants or runtime FX light production. The ten full-scope
No gates are retained. Local core 60 passing; native world suite previously
45 passing; full CI for the new default/UI/status checks pending.

CI `41453546142` (`258cd03`) passed Linux/Windows and all 13 GPU binaries,
including default receiver UI/status compilation and rejected-instance checks.
All 73 actual model placement positions have a regional-light endpoint in the
production environment selector (99 actual boxes). Confirmation: default
ordinary receiver integration Yes; full P1.03 No. Continue.

### Actual GFX runtime-light dependency census

Downloaded the 97,723,094-byte gfx.pck through the authorized Drive connector;
private assets stay ignored. Directory: 10,104 entries; 5,459 GFX text files.
Archive scan finds 524 light-bearing files / 546 elements of type 130, all
point LightType 1. Every light has one keypoint; 542 have no controllers and
four have one controller. This is presence metadata, not implemented playback.

The world's 2,275 scene-effect references resolve to 163 unique roots. All
36 model-event GFX references resolve to 14 additional roots. Following all
74 GfxPath container edges produces 214 unique files, no missing dependency,
**zero light elements**. The production audit parser independently validates
all 214 decoded actual files and their element counts; unknown dependency
fields/versions and incomplete counts fail rather than silently disappear.

Added `gfx-light-audit` CLI using production PCK/ECWLD/ECM readers, normalized
archive duplicate checks and transitive container traversal. It reports the
asset graph, not runtime support, and exits nonzero if light-bearing assets
need unimplemented playback. Three CPU regressions cover presence, malformed
counts/light types and hidden/unclassified dependencies. Core suite 63 pass;
CLI compilation/full CI pending. No private fixture is uploaded to CI.

Related light TickAnimation source was checked against actual 1.4.2 vtable
load `70b4a0`, save `70b780`, tick `70ba00`: base tick, visibility, priority,
keypoint/controller evaluation, packed outer-color multiplication, and parent
transforms precede light emission. These cannot be replaced with a permanent
constant point merely because most files have one keypoint. General light
production/lifetime remains open. The current world's empty runtime list is
now supported by its resolved asset graph, not an assumption that gfx.pck has
no lights. P1.01 remains No until the general lifecycle criterion is covered.

CI `41454453164` (`403cd31`) passed Linux/Windows, the three new GFX audit
unit tests, all binary target checks and all 13 GPU regressions. Confirmation:
world GFX presence audit Yes; general lifecycle P1.01 No. Continue.

### Opaque ECM original-color material scaling

Actual 1.4.2 loader `6cfa9f` calls `6d0230`: it reads static descriptor
OrgColor at +5c, decodes packed ARGB (`6d0263–6d026b`), applies alpha through
`6d04f0`, then explicitly replaces the color scale's alpha with 1 before
`6d0820` (`6d0270–6d028b`). Color application calls `6d09a0`.
Ordinary material binding `7a6170–7a61a8` selects method 2, clones the original
material and calls `7b6b70` with clamp=true. That routine multiplies all RGBA
ambient/diffuse/specular/emissive coefficients and clamps each to [0,1].
It does not replace the original two-sided flag or power. White/original
material bypasses this clamp entirely; don't unconditionally clamp HDR data.
The related older commented source agrees, but actual instructions are the
implementation authority.

Retained draw now supports descriptor RGB scaling with opaque OrgColor alpha.
It scales a temporary per-mesh material before binding light constants, without
mutating shared authored materials or baking a second light into vertices.
Nonopaque OrgColor and nondefault blend queues remain rejected explicitly.
Added a coefficient-clamp CPU test and distinct RGB ARGB-order GPU update
regression (`ff804020`), followed by restoring original method. Core 64 pass;
native world/material/pose suite 46 pass; GPU validation pending. Full P1.02/
P1.03 material coverage remains No. Continue.

CI `41454850805` (`37561bf`) passed Linux/Windows/all 13 GPU regressions,
including the distinct-channel original-color material update. Confirmation:
opaque original-color scaling Yes; full material/alpha integration No.

### Alpha insertion must preserve another 1.4.2 version difference

Actual alpha registration `7a59c5–7a5b6f` computes mesh distance then walks
an insertion list, not a generic total-order sort. Same model: mesh weight
(nonzero) or skin weight fallback; lower new weight inserts immediately.
Crucially, `7a5a45–7a5a7c` still tests original AABB MaxY/MaxZ/MinZ when the
new weight is GREATER, not only equal. The related 1.5.2 source puts these
AABB checks in an equality branch. Copying it changes 1.4.2 ordering.
Different models with the same nonzero group ID compare group weights;
otherwise distance is far-first and a new equal-distance node inserts before
an old one. This is not generally a transitive comparator.

Added bounded `alpha_insert_index` implementing that insertion policy, with
four CPU regressions: distance/ties, source-version weight/AABB behavior,
group precedence and invalid-input rejection. Core suite now 68 passing.
This is queue-order implementation, NOT enabled alpha scene submission.
Remaining work includes retaining/feeding source sort metadata, distance
construction, per-mesh ordered GPU draws, pass-state verification and the
above/below-water alpha split. Existing ordinary draws remain unchanged.
Full P1.02/P1.03/P1.10 remain No. Continue.

CI `41455486665` (`5f049e3`) passed Linux/Windows/all 13 GPU checks.
Follow-up disassembly of `7a5460` confirms distance is the minimum over
eight transformed ORIGINAL mesh AABB corners, capped by float bits
`497423fe` (999999.875), not geometric point-to-box distance. Added
`alpha_corner_distance` and three CPU tests for inside-box behavior,
rotation/nonuniform scale/translation, sentinel saturation and invalid inputs.
Core now 71 passing. Alpha scene submission and its water split remain No.

### Per-area grass availability and terrain-only missing-night fallback

Implemented the verified `7c2678–7c2683` missing-night terrain policy in the
production atlas loader: copy the day records into the night slots only when
the night file is absent. Missing day and other I/O/invalid-payload errors
remain explicit failures (no guessed dayless terrain binding). Two CPU file
pair tests cover these distinctions. The upload format/BC1/RGBA and bounded
staging paths are unchanged.

Added a separate per-area authored availability buffer to grass. The area
selected by its center (or the explicit diagnostic legacy selection) must
have BOTH original maps. Terrain's fallback copy does not mark night ready;
grass instead uses the unlit texture RGB, preserving alpha/fade/fog.
Production binds the actual loader metadata, rather than a global flag.
GPU regressions now include a missing-night area adjacent to an available
area (both selection modes / night-middle-day), plus all four availability
combinations and RGB/alpha oracle checks with unrelated directional lighting
black. Local core: 73 passing. GPU/new build validation pending.

Confirmation: file pair policy/per-area production metadata implemented;
GPU verification pending. Full P1.06 No: full-world payload census and
dynamic loading/unloading lifecycle are not completed by startup metadata.

CI `41456144458` (`9de9c53`) passed Linux/Windows/all GPU checks for the
corner-distance follow-up. CI `41456559295` (`de89d91`) passed Linux/Windows
and all 13 GPU binaries, including the adjacent-area availability regression,
all four grass availability combinations and RGB/alpha oracles. Native
retained-world suite remains 46 passing. Confirmation: terrain-only missing
night fallback and startup per-area grass metadata Yes; complete P1.06 No.

### Resolve renderer +6: authored fullscreen sharpen, not another glow mode

Native `40f5e4–40f5fe` loads `Shaders\\ps\\fullsharpen_14.txt` into +2d0;
`410aae–410ad5` binds its stream/shader and draws with ONE/ZERO blending.
`40fd64–40fd9c` runs it after glow and a backbuffer copy when flag +6 is set.
This matches the related source's DoSharpen, but the actual client's shader
is authoritative (SHA256
`d9d5863bd6e83d29db666e618517e9beed88bc97a81b0cd57b755184cdb06d03`).

Important authored behavior: each of four MADs writes r0 from the ORIGINAL
r5, overwriting the preceding result, not accumulating it. After the final
ADD, RGB is clamp(2*center - below,0,1), alpha 1. Replacing this with a
standard symmetric five-tap sharpen would not reproduce this client.

Implemented an encoded UNORM composite target followed by the authored
sharpen pass, then display gamma / sRGB presentation. No read/write attachment
aliasing or sharpening in linear display space. Resize recreates the target;
sharpen is independent of bloom. Added persisted Graphics toggle (off by
default, preserving approved/default appearance). The normal non-sharpen
path retains its existing pass and bounds.

Added an independent integer reference with overwrite/bottom-edge/one-row
fixtures and settings roundtrip test (75 core tests pass). Patterned production
GPU fixture now covers sharpen on/off, bloom on/off, ordinary/underwater/mono,
caller RGBA/fades, odd dimensions, both attachment formats and resize.
Sharpen propagates prior input error E by at most 3E for 2*C-N; existing
non-sharpen bounds are not relaxed. New build/GPU validation pending.
Full P1.08 remains No: this resolves the +6 classification/pass, not automatic
fade-event lifecycle. Continue implementation after validation.

CI `41457607134` (`76344d9`) passed Linux/Windows and all 13 GPU binaries,
including 432 patterned ordinary/sharpened/bloom-disabled comparisons.
Confirmation: +6 authored sharpen pass/settings/resize Yes; full P1.08 No.
Added a follow-up independent byte-input LUT reference and a fixture that
rejects gamma-before-sharpen. The GPU matrix now also exercises sharpen
then gamma at setting 67 on exact unwarped patterns, in both UNORM and
sRGB targets. Existing ordinary tolerances remain unchanged. Local core
76 passing; this new gamma-order extension awaits CI.


## Full world terrain lightmap payload census and rigid receiving path

`3364cdc` CI 41457984418 completed SUCCESS (Linux, all GPU binaries,
Windows); the 76-test core count remains current. This validates the
sharpen-before-gamma follow-up previously marked pending above.

Downloaded every actual `.t2lm`/`.t2lm1` named by the world's TRN2:
176 files, 92,650,624 bytes. Rebuilt and executed the existing production
validator against `google_drive` (not just Drive metadata):

```
rustc --edition=2021 tools/validate-terrain-lightmaps.rs -o validate-lightmaps
validate-lightmaps /path/to/world
World: 32x44 mask areas; 88 subterrain pairs. Validated 176 files / 2816 DDS tiles; missing 0; invalid 0.
```

[Per-file size/SHA-256 manifest](Phase-1-lightmap-payload-manifest.csv),
SHA-256 `830736129ef9accf45a01225fd6ad21884ff3dae2b3e14090932ec76d54827a2`.
Checks include complete nonoverlapping rectangular area coverage in both
maps and the production DDS payload parser; private payloads stay ignored.
**Confirmation: full world lightmap payload census Yes. Full P1.06 No**:
missing-day runtime binding and dynamic loading/unloading are separate gates.

Rigid receiving-path research verified against retail 1.4.2:
`0x7a714f..0x7a7158` loads the rigid mesh's bone at +0x64 and applies
three VS constants starting at c14. `0x792969..0x792986` indexes the
skeleton TVS blend array (+0x3ac) directly with `bone << 6`, without the
v9 SKI weighted name remap. `0x7926d2..0x79275a` constructs/transposes
inverse-bind/ordinary bone/model matrices; `0x7927e9..0x792822` appends
the virtual model slot. Scale/controllers remain explicitly unsupported.
The actual `rigid_mesh_ns.txt` shader transforms position with m4x3,
normal with m3x3, then normalizes normal (dp3/rsq/mul), matching the
ordinary supported CPU pose-space path. Related 1.5.2 source was used
only to identify candidates; the implementation is independently written.

Added direct skeleton-index rigid posing (not skin-name remapping),
inverse bind and virtual slot, preserving bind-name validation and rejecting
negative/out-of-range/nonfinite indices/matrices. Retained prepare/upload
now accepts validated rigid geometry and keeps authored material/texture.
GPU fixture now uses production pose_bind instead of hand-built poses,
with one weighted and one rigid mesh, direct real-bone motion, virtual
slot, frame updates, then existing ordinary world/pass checks. New CPU
regressions cover reordered SKI names, inverse bind, ignored weighted
bytes, normalized normals, virtual slot, >255 skeleton indices and invalid
inputs. New GPU/Windows validation pending; do not treat it as passed yet.
**Confirmation: rigid pose CPU implementation Yes; full P1.02/P1.03 No**
until GPU validation and the remaining receiving/material/lifecycle paths.


CI `41459370262` / `b452e05` completed SUCCESS (Linux including the
expanded actual-pose rigid/weighted production GPU fixture; Windows build).
48 native tests passed; all 73 world placements still pass five clock/pose/
draw checkpoints. **Confirmation: supported ordinary rigid receiver path Yes**;
full P1.02/P1.03 still No for remaining classes/materials/runtime producers.

## Native FX publication consumption/reset (not persistent-active membership)

Verified retail `0x425bf8..0x425c2c`: after dynamic-cell construction,
iterate the manager publications, clear each object's +0x1a8 insertion flag,
then erase the complete ordered vector. This agrees with the related
1.5.2 SceneLights.cpp's explicit ClearLightParamList call after consumption.
A live object must publish again at a later eligible tick. Destruction
before consumption removes its pending entry (`0x70b130`); duplicate
publications update params without moving the first insertion position
(`0x70bd20`). Do not confuse live FX lifetime with queue membership.

Implemented the ordered publication bridge and connected its consume step
to ordinary World::update, replacing unconditional update_dynamic(&[]).
Public producer-facing publish/unregister methods preserve identity/order;
no publications produce an empty dynamic grid on the next update. This is
not a GFX controller player and does not manufacture lights for unsupported
FX. Added two core lifecycle regressions (78 core tests now pass), including
next-update expiration, re-publication, duplicate latest params, destruction,
ordered removal and the three-candidate-per-cell cap. Added GPU bridge
fixtures for duplicate params, actual weighted+rigid scene queries, next-
update disappearance and destruction before consumption. The .5 ambient
oracle matches the independently authored static-light fixture, with dynamic
ambient .2*2.5 and zero material diffuse RGB; no tolerance relaxation.
New bridge GPU validation pending. **Confirmation: publication lifecycle CPU
Yes; full P1.01 No** until real GFX controller/producer integration and its
lifetime/visibility/parent transform verification. Continue implementation.


Bridge CI `41459882957` / `cde918a`: Windows SUCCESS, Linux GPU step
FAILED. Check-run annotation identified stale static-cell IDs when the
fixture changed the static corpus to empty without moving the camera
(scene_lights.rs index out of bounds), before the new pixel oracle. Fixed
cache invalidation on static-record replacement, including same-size bounds
changes. Stable corpus/camera thresholds are unchanged. Added regression
for populated -> empty -> populated -> same-size relocated light, all at
one camera position. 79 core tests pass. This is a viewer cache-safety fix,
not a claim of recovered client asynchronous streaming order. GPU rerun
pending; prior rigid/sharpen successful runs remain valid.


CI `41460362278` / `0ba3c51` completed SUCCESS, Linux including the
publication GPU oracles and Windows. **Confirmation: publication bridge
and static-corpus invalidation Yes; full P1.01 No.**

Follow-up review found the viewer updates model uniforms separately for
main/reflection/refraction. Consuming publications again in the latter
passes would incorrectly drop dynamic lights mid-frame. Split the frame-
advancing and pass-only updates: only the first/main update consumes and
ticks; clipped passes retain the lighting snapshot and playback time. The
actual renderer now propagates its existing advance flag. StaticGrid queries
read their own captured static records rather than accepting potentially
changed external record IDs. Added a reflected/clipped same-frame dynamic
light GPU check (nonadvancing call even receives a nonzero delta); the next
advancing update still clears unpublished lights. 79 core / 48 native CPU
checks pass. New multipass GPU validation pending.

Additional full archive shape census (not controller implementation): all
546 light elements have one keypoint and interpolation mode 0; 542 have
zero controllers and four have one controller, type 108 (color transition).
All 546 have empty BindEle/DummyEle, IsDummy=0, MatchGrnd=0,
RepeatCount=1 and RepeatDelay=0. The four controlled assets are white-tiger
invulnerability ground, shield, freeze and demon-state effects. Native
`0x6edd80` advances keypoint time, stops on completed repeat count, applies
start delay, treats duration -1 as infinite and advances finite intervals at
`span >= duration`. The light tick `0x70ba00` invokes that base tick before
visibility/priority checks. Inherited keypoint resolution and parent/GFX
lifetime still must be implemented/verified; this census does not close them.


CI `41461016032` / `7e37614` completed SUCCESS (Linux, all 13 GPU
binaries, Windows executable). The reflected/clipped same-frame publication
fixture passed, with next-frame expiration and destruction checks unchanged.
Full-world lightmap census was rerun: 176 files / 2,816 DDS tiles, zero
missing and zero invalid. Latest local counts: 79 core and 48 native tests;
73 actual world placements × five clock/pose/draw preparation checkpoints.
**Confirmation: rigid receiver, payload census and publication/multipass
components Yes. Full audit: 10 No / 2 qualified Yes; Phase 1 not complete.**

## Retail GFX light decoding, clock execution and controller 108

Continuing the unresolved implementation rather than relabeling its checklist.
Audit also caught a real viewer-routing defect: sharpen alone did not engage
postprocessing if bloom/fog/underwater/gamma were all disabled. Fixed the
actual activation predicate; all 32 independent-effect combinations now have
a CPU regression. Existing patterned GPU coverage already tests sharpen alone
inside the post chain; this fixes reaching that chain from the viewer.

Recovered retail controller factory 0x6ea910: type 108 is jump-table slot 8,
constructor 0x6eabc5, vtable 0x8ebc78, Process 0x6ec190, Load 0x6ec3a0.
Unlike related 1.5.2, retail DOES NOT continuously interpolate packed color.
0x6ec5dd..0x6ec73d builds 128 packed entries per segment; endpoints are exact.
Interior samples use stored f32 reciprocal 0x3c010204 at 0x8ebdcc, exactly
2113665/268435456, not mathematical 1/127. Process computes floor(fraction*127)
and indexes that LUT, including the original start/end eligibility checks.
Implemented an independent exact integer table; exhaustive all 256x256 channel
endpoints x 126 interior entries compare against a separate binary-float/x87
arithmetic oracle (8,257,536 comparisons). This catches the reciprocal and
LUT quantization differences from both continuous and exact-fraction lerp.

Added actual text light-element parsing and single-keypoint execution,
including controller 108, finite/infinite/start-delayed lifetimes, visibility
and signed priority gating AFTER clock progression, parent-matrix position,
unscaled authored range and packed optional outer color. Fail closed on
unsupported bindings/repeats/controllers/keypoint layouts. Parent graph state,
scaled owner clock and resolved transform are explicit caller inputs: this
is not yet general GFX graph/attachment/visual playback. Added the execution
bridge to world receivers and a production GPU fixture from text decoding
through lifetime/publication/selection to weighted and rigid pixels.

Actual extracted archive corpus: 524 GFX files / all 546 light elements parse;
4,259 active clock samples were finite. 18 combined native runtime/selection
unit tests pass, including parser rejection, boundaries, controller lookup,
parent scale/translation, range independence, visibility, priority and reset.
New GPU/Windows validation pending. Full P1.01 remains No for general graph/
scene/model-event ownership and parent/lifetime integration; no fabricated Yes.

CI `41463476329` / `8189af2` completed SUCCESS: Linux compilation/unit
checks, all production GPU binaries including decoded-emitter-to-weighted/
rigid pixel assertions and the Windows executable. Runtime source clock
boundary, hidden/priority progression and publication expiry pass GPU checks.
Local core is now 83 tests. Added a further native DWORD-wrap regression:
0x6edd80's pre-start early return leaves an already active keypoint intact,
so an infinite light must not disappear at the unsigned clock wrap. All 19
combined runtime/selection tests pass; the 524-file / 546-light corpus still
produces 4,259 finite active samples. Added reproducible production archive
survey CLI (`gfx-light-runtime-survey`); it states its element-only scope and
does not pretend to validate surrounding owner graphs or visual effects.
Full P1.01 remains No. Continue with unresolved owner/parent/event integration.

### Scene-effect owner inputs retained (native 142 verified)

The ECWLD loader previously discarded effect orientation, scale, speed,
time eligibility, alpha and attenuation. It now preserves all 60-byte v12
record fields. Verified native `CECOrnamentMan::LoadEffect` at
`0x5b33f3..0x5b34b0`: reads 0x3c bytes and copies position/direction/up,
scale/speed, valid-time, alpha and attenuation into the GFX node. The matching
pinned-152 `ECWDFILEEFFECT12` header was used as a research aid, not copied.
Native `0x5b3700..0x5b37b9` builds the normalized placement matrix via
`0x788960`, applies authored scale and play speed, then installs the parent.

`0x5b18a1..0x5b18d4` uses strict `<0.5` daytime / `>0.5` nighttime checks;
exactly 0.5 rejects both time-limited modes. The raw first x87 C0 test also
accepts unordered as daytime; regression records that behavior rather than
substituting a guessed inclusive comparison. The scene parser now preserves
all owner fields and exposes this actual validity predicate. Tests cover
record truncation at every byte, padding, full layout and the native boundary.
All 50 combined native world/model/scene tests pass. Real `world.ecwld`:
2,275 records, all numeric owner values finite; day/night/all counts
154/676/1445, 2,027 attenuation enabled, 266 non-default authored alphas.

**Confirmation: Is P1.01 finished? No — continue implementation.** Record
retention does not install scene GFX owners into the renderer. The native
manager ticks before checking time eligibility, lazily loads only one new
root per frame, and returns immediately after that load (`0x5b1a00` onward).
It also fades/stops/releases roots; a blanket independent tick over emitters
would not reproduce that ordered owner lifecycle. Full graph traversal and
model-event attachment ownership still require implementation and tests.

CI `41464185202` for `e59268e` completed SUCCESS (all Linux/unit/GPU and
Windows checks); it includes the infinite-light wrap fix and archive survey
binary. The owner-record changes above are subsequent and require their own CI.

### Production PCK → light execution survey reproduced

Ran `gfx-light-runtime-survey google_drive/gfx.pck` using the real production
PCK reader and production light parser/emitter, not the earlier extracted-text
wrapper. Local rustc build used real flate2 1.1.10 / miniz_oxide 0.9.1 /
adler2 2.0.1 / crc32fast 1.5.3 / simd-adler32 0.3.10 sources; no mock archive
or decompressor. Result: 5,459 text GFX files scanned; 524 light-bearing files,
546 light elements, 4,259 active source-clock samples, all finite (including
radius). This remains element-only validation, not owner-graph playback.
Private client assets stay ignored; reproducibility identities:

- gfx.pck SHA256: `d121fe967b9485ce0ef088a4935d395f02f4b17e16a8346d711dd3681be80f49`
- world.ecwld SHA256: `6a763c0ea0ac5717adf0a806cc2d2c09e86a4e4a809df60fc8ed0f9a5c319fc8`

CI `41464867197` for `cb44233` completed SUCCESS: Linux all-binary/unit/
production GPU validation and Windows executable. This includes complete
owner record retention and the two new scene tests. Confirmation remains
**No — continue implementation** for full P1.01; the other full-scope gates
have not been promoted based on this survey or compilation.

### Native glow presenter timing and shader-register ordering correction

Re-traced `0x410bb0..0x410d68`, `0x411100..0x4112bf`,
`0x411340..0x411391`, `0x4114a0..0x4114f4`, and scene caller
`0x4214d5..0x4214e6`. This exposed an actual production integration error:
setter +0 is **power**, +0x10 is **level**, but c0.r receives +0x10 and c0.g
receives +0. Previous `WorldGlow` supplied setter order as shader order. Thus
underwater/special-mode coefficients were swapped despite static shader
fixtures passing. Corrected output is [level,power]: underwater [.6,.5],
mode 4 [.7,1], mode 5 [.25,.1]. Earlier historical log attributions of these
values as [.5,.6]/[1,.7]/[.1,.25] in level/power order are superseded.

Replaced elapsed-ratio interpolation with native stored-f32-step updates and
signed integer-millisecond remaining times, including exact end snap. Native
zero-duration behavior holds start, negative duration snaps on first update.
Added timed SetFade, StopFade(true/false), automatic StopFade on types 1/2/3/5
but not type 4, type-OR-caller-color change detection, and disabled-state
freeze/resume rather than erasing the presenter. Viewer now consumes monotonic
integer-ms differences and actual timed fade state. The fade slider explicitly
requests a one-second transition; it no longer forcibly overrides every frame
and defeats mode-triggered StopFade. No gameplay triggers are fabricated.

`glow_state.rs` is dependency-free and production-shared; nine CPU regressions
pass, bringing the core script to 92. Golden recurrence checks distinguish
native accumulation by 24 ULPs after 312 frames from the old elapsed-ratio
implementation. StopFade 1->0 at 500 ms is 0x3effffff, not guessed exact 0.5.
Production GPU fixture now exercises actual presenter state -> composite
pixels with independent native register-order assertions and timed fade.
Expanded targets to BGRA/BGRA-sRGB in addition to RGBA/RGBA-sRGB because the
interactive surface commonly chooses BGRA. Readback correctly decodes BGRA
memory channel order; reference comparison still checks RGB semantics.

**Confirmation: Is P1.08 finished? No — continue validation and final pass
coverage audit.** CPU success alone cannot sign off GPU or the full criterion.

### Quantized display gamma: native constants and uploaded table

CI `41467358187` / `ff684ab` completed SUCCESS (Linux all unit/binary/
production shader checks, including new RGBA/BGRA presenter fixtures, and
Windows executable). Confirmation still finds full-scope No points, so work
continues rather than treating that CI as Phase1 acceptance.

Re-traced gamma `0x430226..0x430241`, `0x77b7c0..0x77b88c` and the
`MSVCRT.dll!_CIpow` import at `0x8d5488`. The former CPU/GPU formula was
not the native 16-bit ramp: native multiplies configuration by stored f32 1.7
then truncates; it computes reciprocal of (integer gamma * stored f32 .01),
stores the exponent to f32, and multiplies integer index by stored f32 1/255
without an intervening f32 store before _CIpow. Native then multiplies by
65535 in extended/double arithmetic and truncates. Replaced the approximate
float formula with these operations. 39 of 25,856 words differ from the old
CPU float formula by one word, e.g. setting 2/index235=51166 (not51165),
setting29/index64=11389 (not11390).

Both production postprocessing and whole-frame display now upload the same
CPU-built 256-entry ramp in uniform-safe 64-vec4 packing, rather than
independently approximating it with shader pow. GPU retains explicit LUT
interpolation and output encoding. Independent standard-library Fraction /
80-digit Decimal oracle `tools/check-native-gamma-reference.py` recomputes
all 101x256 words without platform pow/powf; FNV-1a over little-endian u16
words is `1988df3e411fa151`, matching Rust. New CPU tests cover that complete
fingerprint and every packed uniform entry. Core now 94 tests.

Expanded whole-frame GPU regression from selected single-color/settings to
all 101 settings and every channel input byte, plus fractional/invalid bounds,
in RGBA/BGRA UNORM/sRGB targets. Added independent Windows rustc runs of gamma
and glow-state tests before executable build to check the platform math too.
New GPU/Windows CI pending. **Confirmation: Is P1.09 finished? No — continue
validation; upstream material conversions and final presentation audit remain.**

### Regional window admission: native persistent FIFO, not guessed async order

CI `41468496702` / `d3c4102` completed SUCCESS: Linux all binaries/unit/
production GPU checks, Windows native gamma/glow CPU references, and Windows
executable. Full No verification still reports unfinished Phase1 points.

Re-traced `0x41c5b0..0x41c8fb` (active window), `0x41c900` (initial full
load), `0x41cb20..0x41cc22` (candidate FIFO), and constructor store
`0x41b495` (+0x534=1). The related pinned152 source aided identification;
142 assembly independently confirms the queue. Corrected the previous
"asynchronous completion order" description for this scene-block path:
initial/disjoint windows load all blocks row-major, overlapping moves retain
loaded AND null overlapping slots, append new blocks row-major, unload old
blocks, then consume min(queue count, load-speed) FIFO attempts. Stale and
already-loaded candidates consume budget; old candidates survive teleports.
This is not a time-raced worker completion order.

Follow-up narrowed the queue cap in the scene object. `0x41cb20` uses
`min(pending_count, scene+0x534)`. Scene construction initializes the cap to
1 at `0x41b495`; environment setup assigns 2 at `0x42aaaa`; the per-update
path `0x42ae60..0x42af64` computes a fractional quota using the pending count,
global frame-time state (`global+0x49c`), configured grid size (`scene+0x560`)
and another runtime scalar, carries the fractional remainder in the owner, and
stores the integer quota back to `scene+0x534`. This is dynamic and is not the
RDTSC timestamp path. `0x7845f0` writes `+0x530/+0x534` on a separate render
object; the matching field offsets were a false cross-class association.
Viewer scheduling remains a fixed one-attempt-per-frame approximation until
those quota inputs and carry are implemented/validated.

Production `RegionStreaming` now uses shared `client_region_stream::Window`
instead of immediately loading every exposed region block and returning
early while stationary. Four standalone scheduler tests pass; a real
Environment integration regression confirms a local region referenced only
by the second candidate appears on the next stationary frame, not immediately.
Combined scene/environment tests: 15; dependency-free core: 98. The regional
path remains opt-in while full transition arithmetic/material consumers are
unverified; approved tree defaults remain unchanged.

**Confirmation: Is P1.04 finished? No — continue.** In particular, related
source shows environment direction comes from the selected default box,
not necessarily the local box supplying ambient/fog, and transition target
setup has a different frame branch from ongoing shading. These require 142
consumer/arithmetic verification, not a speculative checklist promotion.

### Native region target/shade branches, default direction and FPU context

`01746b1` / CI 41469462371 passed Linux/all-bin/GPU and Windows reference/exe
checks. The following follow-up is based on additional 142 disassembly:

- `41ebaf..41ec65` independently retains last containing local (ebp) and
  default (`[esp+64]`) during native map enumeration. Local supplies fog and
  light colors; default supplies direction on init (`41f244`) AND area-key
  changes (`41fd58`, `41fdb1`). Sun/moon visibility/setup is init-only here;
  it is not a reason to animate material light direction with celestial discs.
- `41fdbe` jumps past shading on a changed-key frame. Same-key `41fdc3` uses
  integer milliseconds times stored `.001f`, then `421e80` advances each
  stored component step and clamps according to step sign. Elapsed-ratio
  interpolation and advancing on the target-setup frame were both incorrect.
- **Do not infer extended precision just from x87 instructions.** Native game
  flags are 1/5 at `42fef6..42ff1a`, not FPU-preserve bit 8. Engine
  `7795c6..7795ca` maps that bit to D3D's FPU_PRESERVE bit 2; it is absent.
  D3D8 therefore selects PC24. Scalar subtraction/multiplication round just
  like the stored color-vector subtraction. Initial f64-derived scalar
  fixtures were rejected, not promoted as native proof. The independent
  `tools/check-native-region-reference.c` executes the recovered operations
  on actual x87 with explicit PC24 and contrasting PC53, restoring the CW.
  PC24/53 target delta words are 3b1a33cf / 3b1a33ce respectively; 312x16ms
  forward/reverse step words are 3f6612a9 / 3dcf6ab4.
- Native game clamps logic dt to 250ms (`42fc7d..42fc94`); the regional caller
  reads +490 (`42b7fd`), not uncapped +494. Viewer now uses integer tick
  differences and that independent cap, not the camera's .1-second cap.

Production streamed selection retains the default independently and uses its
noninterpolated direction while local colors/fog transition. Missing-default
invalid data uses an explicit safe viewer direction, not invented local-box
parity. Legacy opt-in/approved tree settings have not changed. Core: 102
passing; real scene/environment: 19 passing. Actual 99-box world traversal
(176x128 centers, three samples each) produces 67,584 finite fog/light samples;
this is a data-path safety census, NOT a native pixel/runtime oracle.

**Confirmation: P1.04 finished? No — continue.** Default-direction and target
frame gaps are addressed, but full native celestial diffuse/specular/visibility
and material consumers remain. **P1.09 finished? No — continue.** The FPU-context
finding also requires revalidating previous gamma/color arithmetic references:
their Decimal/exhaustive agreement proves the implemented mathematical model,
not that the native live rendering thread uses that assumed precision. No
all-Yes or parity claim follows from those earlier green tests.

Regional packed-color follow-up: `799460` multiplies bytes by the stored
reciprocal at `98bf68` (3b808081); division by 255 is not word-equivalent.
Environment fog/ambient/diffuse parsing now uses that constructor arithmetic.
The independent actual-x87 reference exhausts all 256 channels, FNV
**af1301c35b66c4a5**, matching Rust; samples 3, 7, 235 produce 3c40c0c2,
3ce0e0e2, 3f6bebed (each differs from division). Core 103 / scene-environment
20 tests pass. **Confirmation P1.04/P1.09: No — continue.**

`848342f` adds a separate Windows-x86 capture using the client's imported
MSVCRT `_CIpow` and `_ftol` exports with recovered gamma instructions under
explicit PC24 and PC53, checking control-word restoration. CI 41471519297's
capture/build and Linux GPU tests passed; binary reference download is blocked
by the sandbox's artifact-storage host restriction. Reference hashes are also
published as GitHub annotations so precision verification can proceed without
claiming inaccessible artifacts were inspected.

### PC24 gamma: reject the mathematical substitute, retain actual CRT words

Completed Windows-x86 captures in CI 41473598574 (job 125373256487) use the
client's actual MSVCRT `_CIpow`/`_ftol` imports and recovered instructions.
PC53 gives the old **1988df3e411fa151**; PC24 gives **9f351cddfec7875f**.
Both control words are unchanged after all calls (027f / 007f). This positively
identifies the earlier oracle's precision assumption, rather than attributing
small mismatches to GPU tolerance.

The portable double/Decimal-power substitution with PC24 surrounding operations
still differs at eight entries: (setting,index,native) = (2,239,53851),
(26,128,25820), (29,194,46363), (61,151,44194), (70,204,56420),
(75,42,20776), (82,59,27563), (87,45,24596). It was rejected, not accepted
by loosening a threshold. Production now reads a small captured numeric table
(51,712 bytes; SHA256 6fae72f31f9fd16b926482b306a0c7cd0fce1cacbf11eaf92b242b61f11ef2b3).
Windows CI independently regenerates all values through the CRT exports and
compares EVERY word with production, not just a checksum or self-oracle.
The Python diagnostic explicitly retains the rejected substitution and reports
its eight differences. Source/reproduction details are in `src/data/README.md`.
The existing production RGBA/BGRA all-byte/all-setting GPU tests consume this
same table; their new-head result must be checked before claiming success.

**Confirmation P1.09: No — continue.** Native word generation is addressed;
the full upstream color chain/compositor acceptance is not thereby waived.

### Glow uses the same frozen logic tick and PC24 component operations

142 `4214dc..4214e6` passes the environment's argument to glow. That argument
is the capped +490 logic tick, not independent uncapped wall time. The viewer
now passes the SAME frame delta to regional shading and glow (including first
frame and 250ms cap), instead of resampling a separate glow clock. Presenter
subtraction, integer multiply, add, packed caller color and mono tint now honor
PC24/stored reciprocal boundaries. An actual-x87 reference confirms half-time
StopFade is **3f000000**, not the earlier f64-derived **3effffff**. Existing
nine presenter tests pass with this independently confirmed word. Core 103
passes; native x87 color/region/fade references pass. Approved tree defaults
are unchanged. **Confirmation P1.08: No — continue** complete-pass auditing.

Controller-108 loading/sampling precision must also be checked in its actual
owner/load-thread context; the prior PC53 exhaustive arithmetic oracle alone
is not proof of that context. Full P1.01 ownership remains No.

### Native regional diffuse/specular material consumer

`acc8bd0` CI **41474331139 succeeded**, including ALL 25,856 production words
versus a fresh independent x86 PC24 MSVCRT capture, production GPU suites and
Windows executable. This closes the newly found ramp-precision regression,
not the full upstream color-chain acceptance gate.

Traced `4208d0..420db0` and `4b60e0..4b6218` in 142, rather than copying the
related152 source. Regional material light is NOT simply day/night endpoint
lerp and NOT the animated sky-object direction. It packs warm day/orange sun
and authored night moon to ARGB, then unpacks them for directional colors.
Sun pitch > 0 chooses sun time. Sunset threshold is night factor .5; sunrise
threshold is .25. The latter branch really subtracts stored **1/3** from nf
(`420b35`), while sun/specular weight is df*stored **4/3** (`420b8d`): do not
"repair" it into a symmetric lerp. Moon-time specular is pitch*stored
**1.9098593**, clamped to [0,.8]. SetLightParam receives both diffuse and
scaled specular (`420db0`); authored direction remains independently retained.

Production `client_regional_light` implements these proven consumers and
`420e77/420e84`'s packed device-ambient conversion (including signed upper-only
ToRGBA clamp from `799540`). Environment exposes real regional specular and
ordinary world receiving models now bind it instead of hardcoded zero. Removed
the blanket rejection of nonzero specular materials, whose earlier rationale
was precisely the missing directional input. Existing opaque queue eligibility
is unchanged. The GPU regression now carries authored specular materials on
both weighted/rigid meshes and checks noon/midnight/horizon regional inputs
through actual world submission; its new-head result remains pending.

Three new primitive tests, 23 combined real scene/environment tests and 106
core tests pass. They cover packed noon/moon, horizon zero specular, native
asymmetric sunrise, sunset, quantized ambient and retained global direction.
The new arithmetic fixtures are assembly-derived cases, not original-client
runtime screenshots. Approved tree defaults and baked surface exclusions are
unchanged. **Confirmation P1.02/P1.04/P1.05: No — continue** remaining full-scope
receiving/material/visibility coverage and native-time context verification.

`ee250d4` / CI **41475894315 succeeded**: Linux all-bin/unit/production GPU
(including the new clock-to-nonzero-specular weighted AND rigid world pixels),
Windows regional primitives, fresh CRT gamma comparison and executable build.

### Controller-108 process-time sampling uses render-thread PC24

Re-traced `6ec190..6ec22e`: stored keypoint seconds are multiplied by 1000,
compared/subtracted against stored segment durations, divided by duration and
multiplied by 127 before `_ftol`. On the render/update thread, PC24 rounds
EVERY arithmetic instruction. Production sampling no longer incorrectly
retains f64 products/quotients merely because the registers are x87.

New regression: stored 1/127 seconds selects index 1 (native x87 index word
**3f800000**), not legacy index 0; actual x87 reference now executes this
sequence. Stored .29 seconds crosses a 290ms segment exactly and returns its
authored endpoint, independent of table-construction precision. Existing LUT
construction oracle is retained separately: load-thread precision is NOT
established by this render-thread sampling proof. No silent table-mode guess
was made. Four track tests pass; expanded Windows CPU coverage includes them.
**Confirmation P1.01: No — continue** ownership/parent/load-context integration.

CI **41476374791** (`a89d4bd`) succeeded, including controller tests on Windows
and all production GPU checks. Confirmation: controller runtime sampling
regression verified; full P1.01 remains No (owner integration/load context).

### Receiving-model frame tick shares the environment/presenter snapshot

Removed the second wall-time counter from interactive model drawing. It had
bypassed the root 250ms cap and included asset-loading elapsed time on its
first draw. Main model submission now consumes `camera.logic_delta_ms`, the
same already-capped root tick as regional transitions and the glow presenter;
clipped water submissions still consume zero and reuse the pose/snapshot.
The diagnostic Playback API remains uncapped: explicit initial random-phase
updates and independent fixture inputs must NOT be capped a second time.
Core cap tests and existing multipass GPU snapshots protect those separate
contracts. This corrects viewer integration, not FX owner playback completeness.
Confirmation P1.03: No — continue material/FX lifecycle implementation.

### Default-area sun/moon visibility is not a material light switch

Verified 1.4.2 load `48a983..48a98e` maps EC-BSD record +4 to area +80;
`420992..4209bf` extracts SUN bit 3 / MOON bit 4 from the independently
selected DEFAULT area, even while a local region owns color/fog. Flags are
published every render, not blended on region transitions. The directional
color branch immediately after them tests sun pitch instead, NOT these flags.

Verified constructor `4b2d0c..4b2d25` sets both flags true and both visibility
floats to 1. `4b5f63..4b6021` advances each visibility independently with
PC24 `ms * stored(0.0003)` and add/subtract, clamping 0..1. Render
`4b3b80..4b3b9e` truncates visibility*255 to BYTE alpha. World nature tick
`429370..42938e` runs before AdjustEnvironment/render; the new visibility
state consumes the last published flags before latching the current ones.
Zero-tick login-world behavior is separately visible in that native caller;
this viewer's world-map playback uses the root logic tick, not login mode.

Implemented flag parsing, independent default-region publication (including
same-local-key changes), per-frame visibility state and quantized sky alpha.
Reflections reuse the same state, with no second visibility tick. The opt-in
experimental sky remains opt-in; disc size/halo/blending are NOT declared
native-complete. Flags never suppress model diffuse/specular/ambient.

Actual x87 PC24 instruction witness after 100 hidden 16ms ticks:
**3f051e94**. Core is now **109 passed**; real scene/environment suite remains
**23 passed**, with assertions for flag bytes, default-versus-local ownership,
same-key changes and unchanged directional/specular values. The production sky
WGSL is additionally compiled by the GPU integration binary; pixel/material
parity is not inferred from that shader-compilation check. New CI pending.
**Confirmation P1.04/P1.05: No — continue** full material/celestial consumers.

Validation checkpoint: **a9c4b35 / CI 41478115664 SUCCESS** (Linux and Windows).
This includes actual production sky WGSL validation, all production GPU checks,
109 standalone core tests, Windows executable, and fresh native gamma-word
comparison. Local real scene/environment tests: 23 PASS. Readiness rechecked:
all ten full-scope gates still No; no complete phase claim is justified.

### Phase 4-C follow-up: static SpeedTree fields and sampler helper (3 November 2026)

Static disassembly of the downloaded, hash-verified PE32 client and `speedtreert.dll` narrows—but does not close—the remaining vegetation material gaps. SpeedTreeRT `SGeometry` constructor `0x1001dc40` initializes its `+0x38` and `+0x74` floats to `0xbf800000` (-1.0), so those are unset defaults, not evidence of the runtime alpha-reference values used by the tree draw. `SetLeafTargetAlphaMask` (`0x100201b0`) writes a separate byte at `+0x44`. The exported branch/leaf material setters copy material records into subobjects; no inspected constructor/setter/getter path established the runtime values consumed by the client. Client sites `0x656bf5` and `0x656d3e` still read `+0x38`/`+0x74`, convert them, and route them through `0x77a380` (D3DRS_ALPHAREF, 24); actual values and the matching alpha-function state remain unresolved.

Client helper `0x779f60` applies a supplied value to texture-stage MAGFILTER (16) and MINFILTER (17), and applies the same value to MIPFILTER (18) when `global_config+7` is enabled, otherwise MIPFILTER is set to 1. The config object's constructor initializes that flag to true. This establishes generic filter-state plumbing, but the inspected call sites do not tie its resulting tuple to the vegetation path. The previously traced grass calls at `0x651287–0x65129c` establish address mode only: stage 0 WRAP; stages 1/2 CLAMP. Do not infer filtering or anisotropy from these calls. Vegetation sampler parity and tree cutout thresholds remain open; authored tails beyond LOD 7 and tree lighting/reflection parity remain explicit gaps.

### Vegetation cache threshold capture follow-up (2026-11-03)

The earlier conclusion that branch/frond ALPHAREF values were absent from the tree cache has been addressed in source. `tools/speedtree/bake.cpp` now emits PWTREE08 and serializes the `GetGeometry` `alpha_branch` / `alpha_frond` outputs for each of the three cached LODs. `src/trees.rs` parses and bounds-checks those fields; older PWTREE05–07 caches leave them unset. The opt-in far-tree vertex path passes the cached per-LOD values to WGSL and applies them with a strict GREATER alpha test; leaves use the independently confirmed 84 reference (`0x650513`). The Windows PR and release workflows now rebuild `speedtree-bake.exe` with the 32-bit MinGW compiler before building the app, so shipped workflow artifacts should embed the PWTREE08 writer.

CI run `41719211642` passed after rebuilding the PE32 helper, running the all-target Linux build and shader regression, and compiling the x64 Windows viewer. This validates source/build integration and the synthetic cache/parser path, but does not invoke the bridge against the proprietary runtime DLL or compare rendered output. The checked-in local helper remains PWTREE07 unless rebuilt. Tree alpha-compare parity beyond the leaf reference remains unverified; sampler filter/address/anisotropy parity, authored mip levels below atlas LOD 7, and vegetation lighting/reflection parity remain open. Follow-up CI run `41722258155` also passed after adding a compact per-type/per-LOD CSV report to the bridge output. At the user’s direction, native capture is skipped and P4.07 is marked Implemented for the best-supported viewer path with the parity gaps explicitly deferred; this is not a claim of exact native equivalence.
