> **Current status correction — 2 November 2026:** Use the [full eight-phase reassessment](Eight-phase-reassessment-2026-11-02.md) and [unsupported-path register](Unsupported-client-paths.md) for current decisions. The material below preserves historical milestones and may contain superseded absence claims. Old percentages are not current completion scores. Ordinary animated world models/scene-light submission, recovered postprocessing/gamma, per-material fog and clipped water/edge passes now exist; none of that establishes full phase parity.

# Remaining work toward a one-to-one Perfect World 1.4.2 viewer

This is the current known backlog, not a guarantee that every unknown discrepancy has already been discovered. The viewer is an experimental reconstruction, not the original engine or a parity-complete port. Gameplay is intentionally out of scope.

## Highest-priority validation

- Compare matched camera positions, settings, time, and weather with a running original client across all regions; collect reference images/video and fix differences systematically. No such complete comparison has been done.
- Test the release on native Windows 10/11 with real NVIDIA, AMD and Intel GPUs, graphics drivers and audio devices. Measure startup, VRAM, RAM, frame time and streaming while flying throughout the world.
- Verify that all whole-world scenery pages can actually be resident and drawn on a suitably equipped GPU; CPU construction of all 16,037 placements passed, but full GPU residency was not tested in the limited environment.
- Test first-run extraction/bridge execution, source-client selection, tree-cache regeneration and reuse, error handling, long/non-ASCII paths, device loss, resize, DPI and input on actual Windows installations. Check problematic GPUs and memory limits.

## Models, materials and lighting

- Implement the 73 referenced EC/animated models, including skeletons, animation, skins and client material behavior. The 14 distinct ECM-to-SMD and primary BON/SKI dependency chains now resolve, but no SKI geometry or animation is rendered. Identify any other unimplemented scene object types.
- Implement the 2,275 referenced effects where they contribute to the visible world: particles, emitters, animated textures, decals and their placement, draw ordering and lifetimes.
- Reconstruct client shaders, render states, material flags, texture transforms, alpha reference/blending, the exact use and scaling of vertex diffuse colors (now read with estimated 2× modulation), normals, culling and draw order for BMD/other assets. The Plume static model alpha cutout is repaired, but per-material state is not fully reproduced.
- Recover exact client lighting: the supplied terrain batch shader uses constant vertex diffuse and 2× day/night lightmap modulation; the viewer removes its old extra Lambert multiplier and approximates encoded-space arithmetic, but specular, c3 day/night timing, lightmap mapping, actual color space and other render states are unverified. Compare the result with the game and reconstruct scene/region lights, ambient and directional colors, fog, shadows, tone/gamma, day/night transitions and any post-processing. Check sky and scene illumination together.
- Resolve the two absent client BMD texture paths if legitimate assets exist; three meshes currently use a gray fallback. Investigate missing or unsupported archive formats and all other missing references.

## SpeedTree and foliage

- Validate grass and SpeedTree filtering against the game and physical GPUs. Both use isolated per-texture alpha-aware mips and anisotropy; grass uses each material alpha reference and reserves space for other nearby textures. Coverage preservation is approximate, atlas reduction can lower resolution, and pathological scenes with more than 1,024 distinct grass material/threshold pairs can still exhaust the atlas. The 8192² tree atlas uses ~341 MiB including mip levels; constrained devices use a 128² fallback.
- Replace the sampled 16-second SpeedTree leaf-rock loop with the original live wind/strength evolution, original rocking timing, and true per-instance behavior; add branch/frond animation if required by the client. The GPU currently interpolates 4 Hz leaf-card samples extracted from the real DLL at a single wind strength, with a crossfade; the corrected LOD-specific per-leaf geometry passed camera tests with no fixed-camera fallback across all 59 types, but it still needs comparison to the actual game. Verify billboards, camera-facing behavior, materials, alpha and shading against the running client.
- Reproduce exact runtime LOD transitions, distance and clipping behavior, per-instance variation and original fade; compare all 59 tree types at close, mid and far distances to the actual game. A narrow complementary-dither transition between the three snapshots reduces hard popping, but its thresholds and width are approximate; tree atlas full-resolution rendering is not yet validated on a physical GPU.
- Validate the newly reconstructed grass patch jitter, orientation and scale against a running client; reproduce original density, wind history/timing, LOD and streaming. Grass now samples the client day/night terrain lightmaps using the client shader’s 2× modulation; validate world-to-lightmap mapping, filtering, time-of-day blend and color space against the game. Grass geometry is now GPU-instanced by client type and mesh, reducing duplicated vertices and indices; further improve background streaming, range and fade so dense grass does not pop. Grass currently remains local and has not been tested in the real client in this release.

## Terrain and full-world visibility

- Replace coarse distant terrain (8-grid-unit sampling) with detail/streaming matching the client at every distance. Source textures and 256² masks now use native dimensions by default; 16× anisotropic texture gradients replace isotropic mip selection. Verify layer ordering, seams, filtering, LOD transitions and output against the client on hardware. The optional lower-memory setting intentionally downsamples masks to 128².
- Improve startup/loading throughput and asynchronous streaming, memory management and eviction; remove avoidable stalls and provide useful progress and errors. Whole-world scenery now uses 66 native-resolution RGBA pages (roughly 5.4 GiB with mipmaps) plus geometry and is expensive. Deduplicate textures across pages or retain original block compression; move costly uploads off the critical render path.
- Test full scene visibility, terrain/objects culling, far clip and individual placements across the entire main world, including during rapid travel and repeated whole-world toggles. Verify other maps, instance/dungeon worlds and their asset formats; current world is the main world only.

## Water

- Reconstruct and compare the actual water render targets/passes, reflection/refraction scene clipping, normal-map timing/UV motion, exact Fresnel/specular, shoreline/edge alpha, surface geometry and underwater behavior. Automatic underwater distortion now follows the executable-generated procedural bump and spacewarp_tl arithmetic, and main-world EC-BSD box records now provide day/night underwater fog start/end/density/color; regional blending, pass order, edge cases and visual comparison remain approximate; the reflected sky camera-basis pitch fix still needs comparison with the client. The pass now depth-orders water surfaces and suppresses displaced refraction samples from foreground geometry; this is not a substitute for reconstructing clipped refraction render targets. Current visual result and frame interpolation are approximations, not a shader-pipeline port.
- Optimize multiple visible water heights, reflection passes and allocation; benchmark frame time on real GPUs. The optional reflection toggle reduces work, but interpolation does not raise actual frame rate.

- Match the original bloom constants, render-target formats and pass ordering; the optional bloom currently follows client blur/composite equations using estimated coefficients.

## Sky, weather and time

- Reverse-engineer original sky geometry and texture projection, all regional sky sets, cloud animation, roof/horizon behavior, sun and moon, weather and transitions. The current panorama and boundary crossfade reduce visible seams but are not verified at every join or against the original game.
- Reconstruct the original day/night schedule, regional lighting, weather data and transitions. Day/night fog values now come from 99 real main-world box records, but overlapping-box precedence, interpolation and client distance scaling remain approximate; above-water fog is optional to preserve far visibility.

## Music and ambient audio

- Verify overlapping-precinct precedence against the original executable; current selection uses the smallest containing polygon, an unverified heuristic. Check boundaries and behavior for zones with missing, empty or unresolved track lists.
- Reconstruct original randomization, repeat, crossfade, transition timing, persistence, special circumstances and zone-name display. The viewer plays available playlist entries sequentially.
- Test audible playback, volume, output-device changes, MP3 and actual client OGG tracks on native Windows audio hardware. Windows decoding and switching from MP3s to the actual archived Raging Tides Vorbis OGG were tested through Wine with a null audio device; real audio output remains untested.
- Validate the experimental stereo precinct ambience against the native client; validate the experimental positional world effects against native playback and implement weather, gameplay and other sound events. The viewer does not provide full game audio; see [audio audit](Audio-audit-2026-11-03.md).

## Interface and minimap

- Validate precinct overlap precedence, transition timing and typography against the client; exercise repeated zone crossings on native Windows.
- Validate minimap orientation, loaded-scene streaming, all water levels, trees and buildings at multiple scales on physical hardware; profile the full-scene map pass now that it updates every frame. The map currently draws real geometry with flat water, omits unimplemented models/effects and inherits scene streaming limitations; it is not the original game's map pipeline.
- Test borderless fullscreen, DPI, monitor switching and toggles on Windows; persist user settings.

## Application, correctness and distribution

- Persist the launcher lower-memory option, improve performance, richer loading feedback, recoverable errors, high-DPI layout and accessibility. The selected client folder and Escape-menu graphics, sound, interface and camera settings are remembered; verify behavior on Windows and across releases.
- Expand automated format, edge-case and visual regression tests, including all sky joins, alpha materials, unusual codecs, malformed archives, non-ASCII assets, device failures and repeated zone transitions. Compare output and performance to reference recordings.
- Build a representative hardware test matrix and package release/version/build instructions and proper dependency notices; verify trusted-client DLL behavior and compatibility. The program needs the user's own extracted client and temporarily runs an embedded 32-bit bridge against its DLL; do not redistribute game assets.
- Continue identifying undocumented engine behavior. A one-to-one parity claim requires all relevant visual behavior and reference comparisons, not just successful parsing or rendering.

- Verify reversed-Z and the atomic local/world scenery switch at the specific buildings reported by the user, on Windows; coplanar surfaces within original meshes can still fight. Verify water refraction/fog and minimap after the depth convention change.
- Compare BMD dual baked-color interpolation, endpoint ordering, material state, alpha, and color-space handling against actual client BMDs and an in-game reference; the viewer's time-of-day curve remains an approximation. Trace tree and scene/dynamic light paths to complete client lighting.

- Recover BMD mesh +0x26c two-sided overrides and reproduce them rather than applying the default facing rule to every mesh. Compare affected buildings and Plume foliage with the real client; GPU winding tests do not prove visual parity.
- The split litmodels archive has now been verified and surveyed; use real model/color fixtures and examine exceptions to endpoint correspondence. Exact day/night timing, dynamic lights, original sampler/color state and complete environment lighting still need a client comparison.

- Compare grass noise, filter output, update timing and spatial history against instrumented client/game captures; wind remains experimental and default-off.
- Compare denser world terrain and distant seams against client LOD; assess memory/performance on full world.

- Validate map controls and map/landmark alignment against a running Windows client; map scenery depends on whole-world preload.
- Trace the 0x7924f0 model hierarchy lighting upload and its shader constants; verify affected classes before applying scene lights.
