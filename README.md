# Perfect World Explorer

An experimental, GPU-based, free-flight viewer for the **main world** of a Perfect World 1.4.2 client. It reads world data from a client you provide; it is not a game client, editor, server, or gameplay implementation.

> **Work in progress:** visual parity is not complete. Water, lighting, sky, vegetation and other systems contain approximations; ordinary animated world models now render with scene lights in main and clipped water passes, but complete model/material coverage and general visual effects remain unfinished. No complete side-by-side game comparison has established parity. See [Findings](Findings/README.md) for evidence, caveats and open work.

See the [full 76-point plan reassessment](Findings/Eight-phase-reassessment-2026-11-02.md) for implemented portions, remaining gates and validation limits. No phase is fully complete.

## What works

- Full-world terrain from client terrain files, with native-resolution materials, masks and day/night lightmaps; nearby detail streams as you fly.
- Static BMD buildings, rocks and scenery; optional whole-world object loading.
- Ordinary EC world models with retained skeleton/animation/material/texture data and scene-light assignment; complex actions, alpha/special queues and general effects remain incomplete.
- Authored world effects from the map's own ECWLD records and ECM model events: waterfalls, fire, mist, glows and sun shafts, positioned from the scene, played on the scene clock and textured from `gfx.pck`. A texture that an effect names but the archive lacks draws in its authored vertex colour rather than removing the effect.
- Client SpeedTree geometry through the client's 32-bit runtime, with sampled leaf motion and approximate LOD; locally streamed grass from client placements and meshes.
- Experimental animated water, per-height reflections and refraction, shoreline waves, underwater effects, environmental fog, sky, time-of-day control and optional bloom.
- Searchable teleport destinations, original and rendered world maps, a minimap, zone notices, Windows zone music, experimental stereo zone ambience and experimental positional world effects.
- Windows launcher with remembered client folder and settings, and in-app Graphics, World, Sound and Interface controls. Settings → Graphics → **Resolution scale** renders the 3D world at 10–200% of the window size (default 100%) and upscales it, while the UI stays at native resolution; lower values trade sharpness for speed, higher values supersample. The same tab has the **Upscaling** (Off / DLSS Super Resolution / FSR 1 / FSR 2 / FSR 3.1 / FSR 4.1 / XeSS), **Frame generation** and **Latency** (VSync / XeLL — Intel Xe Low Latency) selectors; resolution scale applies only when Upscaling is Off. See [Upscaling and frame generation](#upscaling-and-frame-generation-optional).
- An optional **Modules** add-on list. **Walker** loads a clothed native player character and supports terrain walking, source-backed run/jump mechanics, and first- or third-person cameras. Its integrated **Character Designer** reads PW 1.4.2 v27 catalogs and native presets, edits body/face/equipment data, saves GBK presets, and rebuilds the same spawned Walker. The executable discovers installed module packages beside itself; modules own their state and do not change persisted core settings.

Features are limited to formats and data encountered in one inspected build; other client variants may behave differently. This is not affiliated with or endorsed by the game or its publishers.

## Requirements

- 64-bit Windows 10 or 11, a compatible GPU with current drivers and a trusted, extracted Perfect World client with its data packages and 32-bit SpeedTree runtime. Game assets and the client are **not included**.
- Startup reads the terrain and may take time. Terrain DDS/mask sources remain compact during loading; supported GPUs sample the original BC1/BC2 terrain blocks, and repeated world models share textures, pipelines and animation tracks. This reduces allocations without lowering texture resolution. Memory still depends on the client, driver and enabled features. Whole-world static scenery for the inspected client uses approximately **5.4 GiB of texture pages alone**, before geometry, terrain and other GPU resources. Use nearby streaming and the launcher's lower-memory terrain-mask option if needed.
- The launcher recognizes one particular 1.4.2 executable fingerprint. Other patched or regional clients are not guaranteed to work and may be marked unrecognized.

## Run the Windows executable

1. Download `Perfect-World-Explorer-Windows-x64.zip` from the project's release and extract it. Keep the entire **`Perfect World Explorer`** folder together:
   ```text
   Perfect World Explorer/
     Perfect-World-Explorer.exe
     Modules/
       Walker/
         module.ini
       Populator/
         module.ini
     SHA256SUMS.txt
     THIRD_PARTY_NOTICES.md
   ```
2. Run `Perfect-World-Explorer.exe` and choose **Select Game Folder**. Select the extracted client root or its `element` directory.
3. Click **Load World**. The first run prepares tree geometry using the client runtime; wait for loading to finish.
4. The menu starts open. Choose Settings or close the menu to fly.

The executable resolves `Modules` relative to its own location, not the current working directory. Each installed module has its own subfolder and `module.ini` manifest. This build provides the implementation for known module IDs (`walker` and `populator`); a manifest by itself is not a dynamic-plugin SDK. Missing or malformed packages are reported in the Modules page without preventing the core viewer from running.

**Populator** draws the world population the client itself ships. It reads the selected client's `element/data/task_npc.data` — 1,572 task-NPC records of which 515 carry a real position and 1,057 are marked `0,0` for the server to place — resolves each placed record to its `NPC_ESSENCE` template in `elements.data`, and draws that template's real rig from `models.pck` at the authored world position and height. The settings window reports how many placements are in range and offers **Go to the nearest placement**, because the client's own placements cluster in towns. Each actor carries the template's own name overhead the way the client draws it: the family and height come from the client's own `Configs\fonts.txt` (`"Arial Unicode MS" 12 0 0`, the row that file labels 玩家和NPC头顶称谓文字 — player and NPC overhead names), the labels are fixed-size screen text and a label the camera cannot see is dropped, so no name shows through a building or a hill. Each actor plays its rig's own idle — the client's `站立` combined action, which the client itself falls back from to `休闲`, else the ordinary clip that names, else the rig's own `待机`/`站立`/`休闲`/`idle` clip — and the settings line reports how many drawn actors are actually moving, because a pose is fingerprinted each frame instead of assuming the schedule works. The 515 placements resolve to 92 distinct rigs, 86 of which ship an idle clip of 1.6–2.3 s (24–36 frames at the skeletons' 15 fps) with 100–478 rotation keys in the skeleton; the other six are scenery — chests, rocks, a furnace, a tree, an altar — that ship no idle action at all and correctly hold their authored pose. `tools/npc_idle_report.py` regenerates that per-rig table into `Findings/npc-idle-evidence.md` straight from `models.pck`, `elements.data` and `task_npc.data`. Each actor also stands on the client's own building floors — bridge decks, platforms and interior floors — or on the upward-facing top of the model it was authored onto when that model ships no collision hull, because `task_npc.data` stores no height for most placements and the terrain under a bridge is the river bed. A placement can also be *inside* a wall at the right height, which no height query fixes: the actor's own box is tested against the same hulls, and a placement authored into geometry either drops to the ground under a solid plate it was lifted onto or moves to the nearest free spot within 2.5 units that keeps its height, while one with nowhere to go keeps its authored column and is counted under `in walls` rather than being moved invisibly. Spawn distance defaults to the client's own 420-unit scene window and is adjustable together with the number of simultaneously drawn actors, the name distance and building-floor support; the settings window sits beside the Walker panel. The per-frame cost is bounded rather than proportional to the crowd the host renders the add-ons once per pass (main, reflection, refraction) and the module poses each actor once per frame and re-uploads vertices only when the pose actually changed, skips actors no pass can see, updates a retained rig's vertices without re-running the draw validation, re-skins distant actors every second or third frame, tests overhead-name occlusion a few times a second instead of every frame, reconciles the streamed set by index instead of a quadratic scan, caches the placement search until the camera moves, and stops decompressing rigs once a frame has spent 4 ms on them. Mobs load the real 4,208 `MONSTER_ESSENCE` templates but stay unspawned, because mob placement lives in the server's `gamedata/npcgen.data`, which the client does not ship; for the same reason actors move only along real navigation paths and therefore stand still without server route data. Nothing is invented to fill those gaps, and there is no combat, dialogue or other interaction.

**Security:** The application executes the 32-bit `speedtreert.dll` from the selected client through an embedded helper. Only use a client you trust. Settings and the selected path are stored under `%LOCALAPPDATA%\PerfectWorldViewer`; derived tree geometry is cached in its `tree-cache` subfolder. The installation itself is not modified. Do not upload client data or the derived cache.

### Controls

| Control | Action |
| --- | --- |
| Left-click | Capture mouse for looking |
| Escape | Release mouse / open menu; return from Settings |
| W/A/S/D | Fly forward/left/back/right |
| E or Space / Q or Left Ctrl | Ascend / descend |
| Shift | Faster movement |
| Arrow keys | Look |
| M | Open world map |

Use **Escape → Teleport List** to search destinations. The world map offers original client tiles or a top-down render of the currently loaded world, mouse-wheel zoom, and optional click-to-teleport. Whole-world static objects need to finish loading to appear in the rendered map.

### Walker add-on

Open **Escape → Modules**, select **Walker** to read its description, and choose **Enable Walker**. While Walker owns the camera, W/A/S/D moves the character, Space jumps (press again on foot for the client's second jump), and the mouse looks around. In water, Arrow Up/Down (or Z) swims vertically and Space takes off near the surface or ascends in deep water. Riding permits one jump; the configurable flight and mount hotkeys are shown in Walker settings. Disabling Walker restores the ordinary free-flight controls above.

The draggable, resizable **Walker settings** panel appears beneath the minimap while the Escape menu is open. Its integrated **Character Designer** selects native class/gender combinations, body and face customization, equipment, and preset loading/saving, with item pickers that provide both a scrollable list and name/ID search. Every design change immediately rebuilds the current Walker in place; there is no separate preview character, and a failed rebuild leaves the previous character active. New Walkers use an Explorer-selected preview loadout built from authentic item-level-0 weapons and level-1 armor records; those records do not establish the server's new-character grant table. Psychic soulspheres use the native `HH_qiu` parent attachment instead of the ordinary right-hand weapon hook. The panel also provides move speed, jump height, and **First Person Mode** controls. First person evaluates the model's `Bip01 Head` bone and hides the head/face, right-hand weapon (including the soulsphere), and flight meshes while retaining the body and mount; third person draws the complete animated assembly.

Walker uses authentic profession run/swim/flight speeds, ordinary jump impulse of 10.0 m/s, gravity of 9.8 m/s², support-plane movement, and the native two-jump limit on foot (one while riding). Its fixed 60 Hz integration makes the ordinary impulse's apex approximately 5.019 m; 5.102 m is only the continuous `v²/(2g)` reference, not a separate retail jump-height field. Custom height settings are converted to an equivalent launch impulse. Water uses native profession/gender AABB thresholds, first-matching-area heights, 3.0 m/s deep swimming, near-surface speed clamping, and a 7.0 m/s water-takeoff impulse. Flight speed is the class fly speed plus the selected authentic item bonus. A missing unique flight SKI is reported and skipped using the client's native tolerance, without substitute geometry. **Collision check** independently loads native BMD convex brushes for buildings/ornaments and TBH brushes for trees, then applies swept-AABB blocking and wall sliding; it does not require whole-world rendering. Terrain support and water behavior remain enabled whether or not this object-collision option is selected. Loader counts and missing/failed asset diagnostics appear beneath the option. Combat, server stat modifiers, and full gameplay are not claimed.

With **Play world and character effects** enabled, Walker schedules authentic animation **EventType 101** SFX from `sfx.pck` (with the client's loose-file fallback), including authored timing, loop/once behavior, finite-time stops, file choices, probability, volume, and pitch. Body, mount, and flight channels are stopped on action/travel changes. Playback uses the Windows rodio backend and is local/non-spatial; EventType 109 audio events and the client's complete 3D mixing, audio-group, and duplicate-selection behavior are not implemented.

## Build from source

Install stable Rust and Cargo from [rustup](https://rustup.rs/), and a toolchain suitable for the target platform. From the repository root:

```sh
cargo build --release --bin perfect-world-explorer
cargo test --bin perfect-world-explorer
cargo check --bins
```

Cargo does not copy module packages into `target`. For a source-tree run, either copy `Modules` beside the built executable or set `PW_MODULES_DIR` to the repository's `Modules` directory. The environment override is also useful for portable or test installations. Windows PR artifacts and manual release ZIPs are staged by `tools/package-windows.ps1`, which verifies the executable/module layout, stages `THIRD_PARTY_NOTICES.md`, ships the vendored AMD FSR SDK 2.4.0 and Intel XeSS/XeLL 3.0.2 binaries (see [sdk/](sdk/)), excludes development survey tools and NVIDIA proprietary DLLs, and can additionally stage vendor DLLs from your own download via `-SdkDirectory`. It verifies the executable/module layout and excludes development survey tools.

Dependency-free Phase 1 core checks can also be run with `tools/check-phase1-core.sh` (requires `rustc`; optionally set `RUSTC`). This checks CPU references only, not the application or shaders. For the production whole-frame gamma pass, run `cargo run --bin display-gamma-gpu-check` on a machine with a working GPU adapter. Client gamma is fullscreen-only and applies after world and UI composition; windowed mode does not apply the display ramp.

On Windows, use a compatible Rust Windows toolchain and the required compiler/linker (for MSVC, install the Visual Studio C++ build tools). A GNU cross-build from Linux requires the Rust target and MinGW linker:

```sh
rustup target add x86_64-pc-windows-gnu
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release --target x86_64-pc-windows-gnu --bin perfect-world-explorer
```

The viewer executable embeds `tools/speedtree/speedtree-bake.exe`, a 32-bit helper built from `tools/speedtree/bake.cpp`. It invokes the **user's** runtime DLL; the DLL is not bundled. The helper binary is retained in the repository because the Rust build uses `include_bytes!`; Windows PR and release workflows rebuild it from `bake.cpp` before compiling the viewer. For local builds after changing the bridge source, rebuild it with a 32-bit MinGW C++ compiler, e.g. `i686-w64-mingw32-g++ -static -O2 -municode tools/speedtree/bake.cpp -o tools/speedtree/speedtree-bake.exe`. Compiler versions and flags may produce a different binary. The bridge also writes `tree-alpha-refs.csv` into its prepared cache directory, containing the native branch/frond alpha references for each of the 59 tree types and three LODs. `tools/launcher-art.jpg` is embedded at build time.

The executable distributed alongside this source was rebuilt from this packaged snapshot. Native Windows/physical-GPU verification of the latest changes and game-to-viewer image comparison remain outstanding.

## Development and verification

- `src/` contains parsers, scene loading, GPU rendering, UI and audio; `src/bin/` contains diagnostic and survey programs. These are development tools, are not required to run the viewer, and are not included in shipped viewer packages.
- Example: `cargo run --release --bin pck-inspect -- /path/to/element/shaders.pck` lists archive entries. Input client files are required.
- `cargo run --bin water-record-survey -- <directory>` expects `world.ecwld`, `world.ecbsd` and referenced BSData `.dat` files in that directory. Set `PW_WATER_SAMPLE_69` to a local `69.dat` path to enable optional real-data water tests. Do not commit these files.
- The shipped-corpus tests skip when the retail archives (`google_drive/gfx.pck`, `google_drive/world.ecwld`, `google_drive/x`, `google_drive/pw142/element`) are not staged beside the checkout, which is the case on CI. Set `PWE_CORPUS=require` to turn every one of those skips into a failure and run the suite as a gate on a machine that has them; it is all-or-nothing and names the archive it is missing. See `pck::corpus_absent` in `src/pck.rs`.
- GPU diagnostics include `water-pass-gpu-check`, `simple-water-gpu-check` and `post-gpu-check`. They require a working graphics adapter; synthetic tests do not prove in-game visual parity.
- `python tools/audio-census.py /path/to/element/sfx.pck --ffmpeg /path/to/ffmpeg` surveys archive audio and independently decodes it; this is not a test of the Windows playback backend. On Windows, `cargo run --release --bin audio-corpus-check -- <element directory>` instead reads and decodes packed SFX and loose music with rodio; it does not test audible output or detect every possible mid-stream decoding error.
- `cargo run --bin scene-sound-check -- /path/to/element` surveys world sound placements and checks their paths against `sfx.pck`.
- Some helper binaries and research scripts assume specific test data layouts. Consult their source and [Findings](Findings/README.md) before use.

See [the world-loading optimization report](Findings/World-loading-optimization-2026-11-04.md) for implementation details, regression checks and measurement limits. A 6 GB laptop has not yet been hardware-verified.

## Known limitations

The water render target details, simple-water lighting/fog, quality modes, wave timing and underwater integration remain incomplete. Terrain, sky, grass placement/wind, SpeedTree LOD/wind, regional lighting and fog are approximations. Animated EC models and effects, full point-light integration and gameplay are missing; Walker is an optional terrain-locomotion add-on, not a complete game client. Whole-world memory use can be high; the map shows loaded content rather than all possible assets. Audio is Windows-only. Sound settings offer independent music, ambience and world-effects sliders. These channels default on for new settings; existing saved enable choices are respected. World effects use authored world placements, approximate distance attenuation and panning, a 16-voice cap and approximate replay timing. Zone choice and ambience day/night switching are approximations. Weather, combat and other gameplay effects are not implemented. Full decoder coverage and physical-device playback remain unverified. Rendering and timing vary with client build, graphics driver and hardware. See the [roadmap](Findings/Roadmap.md), [parity assessment](Findings/Parity-Assessment.md), and [water plan](Findings/Water-100-percent-parity.md).

## Repository layout

```text
Cargo.toml, Cargo.lock   Rust package and locked dependencies
src/                     viewer, parsers, GPU code and diagnostics
Modules/                 installed module packages copied beside the executable
tools/package-windows.ps1 shared PR/release portable-package builder
tools/speedtree/         bridge source, probes and embedded helper
tools/launcher-art.jpg   embedded launcher image
Findings/                research, evidence, assessment and roadmap
LICENSE                  project license
```

## Upscaling and frame generation (optional)

Settings → Graphics has three independent selectors:

- **Upscaling** — Off, DLSS Super Resolution, FSR, XeSS.
- **Frame generation** — Off, DLSS Frame Generation, FSR Frame Generation, XeSS Frame Generation. Independent of the upscaler, including Off.
- **Latency** — **VSync** (default: the display paces presents, no latency library — the behaviour of every earlier build) or **XeLL** (Intel Xe Low Latency: `xellSleep` plus per-frame simulation / render-submit / present markers, over the same vsync'd, sync-interval-1 present Intel requires). Independent of both selectors above: it works with any upscaler, with frame generation, and with Upscaling Off. XeLL stays selectable on every vendor (the library, `xellSleep` and the per-frame markers work on any GPU); the note under the selector says that on a non-Intel GPU Intel only turns it into actual latency reduction while XeSS Frame Generation is active — in which case the frame generator owns the XeLL context and the viewer does not create a second one. A session the driver refuses at runtime logs the reason, says so on the mark and leaves the vsync'd present path untouched.
- **FSR 1 / FSR 2 / FSR 3.1 / FSR 4.1** — all four AMD upscalers are separate Graphics tab entries; the availability of each follows what `fsr_version` used to select on supported hardware (see below).

Options the hardware or the available SDK does not support are shown disabled with the reason (for example "Requires an NVIDIA RTX GPU" or "FSR SDK not found"). Frame generation is chosen independently of the upscaler. The pairings the vendor SDKs document are exposed (DLSS Frame Generation and FSR Frame Generation are documented as usable with any temporal upscaler; XeSS Frame Generation is only documented with XeSS upscaling or none). Pairings the SDKs do not document are not offered; see `docs/dx12-port-notes.md` and the pull request report for the unsupported list.

**AMD FSR SDK 2.4.0 DX12 binaries are included** — the signed `amd_fidelityfx_loader_dx12.dll`, `amd_fidelityfx_upscaler_dx12.dll` and `amd_fidelityfx_framegeneration_dx12.dll` live in [`sdk/`](sdk/) and are staged into release packages by `tools/package-windows.ps1`. Their license (MIT-style, reproduced in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and `sdk/AMD_FidelityFX-SDK-2.4.0-LICENSE.md`) allows this redistribution. FSR detection therefore works out of the box on any real DX12 GPU; Frame Generation gates on the two DLLs plus a non-CPU adapter.

**Intel XeSS SDK 3.0.2 binaries are included too** — `libxess.dll` (XeSS Super Resolution), `libxess_fg.dll` (XeSS Frame Generation) and `libxell.dll` (Intel Xe Low Latency, used by XeSS-FG on *every* GPU and by the Latency setting) live in [`sdk/`](sdk/) and are staged into release packages with their license and third-party notice. The Intel Simplified Software License allows redistributing the unmodified binaries with the notice, and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) records the size and SHA-256 of each file. So XeSS, XeSS Frame Generation and XeLL are selectable on capable hardware without downloading anything.

Streamline is **not** included (`sl.*.dll` is never vendored and never staged). All three vendors' runtimes — AMD FSR, Intel XeSS/XeSS-FG/XeLL and NVIDIA DLSS/DLSS-G — ship in `sdk/` with their licenses, so the vendor SDKs below are only needed if you want to override them with a different release. To use your own copy, obtain the SDK from the vendor (links and terms are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)) and place its DLLs in one of these locations, then restart the viewer:

- `sdk\` next to `Perfect-World-Explorer.exe`,
- the folder named by the `PW_UPSCALER_SDK_DIR` environment variable,
- the folder set in `viewer-settings.txt` as `upscale_sdk_dir=<path>` (edit the file while the viewer is closed).

`perfect-world-explorer --diagnose-upscale` and `perfect-world-explorer --diagnose-framegen` (flags on the viewer binary) print what your hardware and SDK folder allow, and why every unavailable option is unavailable; `--sdk-dir <path>` probes a specific folder.

**Resolution scale applies only when Upscaling is Off.** With an upscaler active, the world renders at the provider's recommended internal resolution for the selected quality mode and is presented at window resolution; the UI always stays at native resolution. The AMD upscalers and Intel XeSS are selectable in the Graphics tab (XeSS runtime-unverified):

| Entry | What it is | Requirements |
|---|---|---|
| **FSR 1** | AMD EASU + RCAS as an in-tree WGSL spatial pass (no SDK, no FFX). Same MIT-derived algorithm as PR #37's FSR 1, wired into the Graphics tab with quality-driven internal resolution and an RCAS sharpness slider (`fsr1_sharpness=`). | Any real GPU; no SDK. |
| **FSR 2** | FSR 2.3.4 provider of the unified `amd_fidelityfx_upscaler_dx12.dll` dispatched as an FFX temporal upscaler context (Windows + DX12). | Vendored AMD SDK 2.4.0 (included). |
| **FSR 3.1** | FSR 3.1.6 provider, same FFX path. | Vendored AMD SDK 2.4.0 (included). |
| **FSR 4.1** | FSR 4.1.2 provider (ML), same FFX path. | Vendored AMD SDK 2.4.0 (included) **and** an AMD Radeon RX 9000 series GPU (the option is disabled with a reason elsewhere; context creation falls back to bilinear if the driver refuses). |
| **XeSS** | Intel XeSS Super Resolution (SDK 3.0.2, `libxess.dll`) dispatched as a temporal `xessD3D12Execute` on a native DX12 command list: reversed-Z depth, LDR sRGB input, Halton(2,3) jitter, and the same depth-reprojection motion-vector pass as the FSR path. | Vendored Intel XeSS SDK 3.0.2 (`libxess.dll`, in `sdk/`) + Windows/DX12 + SM 6.4 GPU. |

The **Upscaling quality** selector also offers **Native AA**: `upscale_quality=nativeaa` renders at exactly the window resolution (factor 1.0) and dispatches FSR 3.1 with `renderSize == upscaleSize`, which is how FSR 3.1 exposes temporal antialiasing without upscaling (the option is listed for FSR 3.1 only and degrades to Quality if the upscaler is changed). **FSR sharpening** (`fsr3_sharpness`, 0 = off … 1 = maximum, default 0.75) is a slider in the same tab and is passed straight through to `ffxDispatchDescUpscale.sharpness` for FSR 3.1/4 and Native AA, read every frame so it applies while dragging.

FSR 2/3/4 share the same jitter + depth-reprojection motion-vector pass (moving actors are reprojected as if static — see docs/blockers.md B-5) and `ffxDispatchDescUpscale`; an FFX jitter sequence jitters the world projection and the window-resolution result is composited before the UI pass. Any loader/dispatch failure logs and falls back to the bilinear path for the rest of the session. The FSR upscaler path has been exercised on real hardware (2026-11-15): context creation self-heals on driver quirks, but the jitter/motion-vector conventions were found wrong in code review and fixed in the same build; image stability is pending a hardware retest. Legacy `viewer-settings.txt` files that carry `upscaling=fsr` together with `fsr_version=` are migrated on read: `fsr_version=1|2|3|4` selects `fsr1|fsr2|fsr3|fsr4` (absent/`0` → `fsr4`, the old "newest" default). **DLSS Super Resolution is implemented** (Windows + DX12 only, from the vendored `nvngx_dlss.dll`): it runs the same closed-loop shape as FSR and XeSS — Halton(2,3) jitter, the shared reversed-Z depth-reprojection motion vectors, `NVSDK_NGX_D3D12_EvaluateFeature` on the viewer's own command list — over NGX's snippet ABI with a parameter table the viewer owns, because the shipped NGX DLLs export no parameter allocator. Any failure logs the named `NVSDK_NGX_Result` and falls back to the bilinear path for the rest of the session. **XeSS Super Resolution is implemented** (Windows + DX12 only) as described in the table; any loader/context/dispatch failure logs and falls back to the bilinear path for the rest of the session, same as FSR. **FSR Frame Generation is implemented** (Windows + DX12 only) and is **independent of the upscaler** — it works with FSR 1, FSR 2/3/4, DLSS/XeSS fallback, or Upscaling Off entirely; it only needs the FFX frame-generation swapchain for the window (`FrameGenerationSwapChainForHwndDX12`), which paces real and generated frames and composites an optional separate native-resolution UI texture that the swapchain draws on top of both real and generated frames (the UI is never interpolated). Camera-motion motion vectors from the same reprojection pass feed `ffxDispatchDescFrameGenerationPrepareV2`, and `ffxDispatchDescFrameGeneration` runs on the swapchain's interpolation command list. As with the upscaler, any loader/context/dispatch failure logs and restores the standard present path for the rest of the session. **This frame-generation path is runtime-unverified** — it has not run on real hardware yet (and cannot run in the development environment, which has no GPU), so treat enablement as experimental until it is exercised on a Windows machine with a DX12 GPU. While frame generation is active the viewer draws a small on-screen mark (top-left) showing the upscaler version and the FG frame counter, or a red line when either path failed and the fallback is in use. **DLSS Frame Generation is implemented** (Windows + DX12 only, RTX 40-series or newer, from the vendored `nvngx_dlssg.dll`) without Streamline: the viewer owns a flip-model swapchain for the window, copies the composition into the current backbuffer, evaluates the NGX feature so the interpolated image lands in the next backbuffer, and presents both. It is independent of the upscaler and interpolates the UI with the scene, so the HUD cannot be dropped. **XeSS Frame Generation is implemented** (Windows + DX12 only) following the same pattern as FSR frame generation: a proxy swapchain for the window paces real and interpolated frames, and the UI is drawn into a separate native-resolution surface (premultiplied alpha, `HUDLESS_UITEXTURE` composition mode) that the proxy composites onto both, so the UI is never interpolated. XeSS-FG requires a XeLL low-latency context on every GPU — Intel Arc included — which the viewer creates, enables low-latency mode on, and feeds the simulation / render-submit / present markers each frame; the XeLL context is destroyed after the XeSS-FG context, as the SDK requires. Like the FSR path, XeSS frame generation takes over presentation from the standard swapchain and any failure restores the standard present path for the rest of the session. **Both XeSS paths are runtime-unverified** — they have not run on real hardware yet (and cannot run in the development environment, which has no GPU); treat enablement as experimental until exercised on a Windows machine with a DX12 GPU. While frame generation is active the viewer draws a small on-screen mark (top-left) showing the upscaler and FG frame counters, or a red line when a path failed and the fallback is in use. The viewer starts and renders normally with no SDK present. The DLSS path's conformance to NVIDIA's own binaries, headers and programming guide — bootstrap, create/evaluate parameter list and setter types, the colour/depth/motion-vector contract, pipeline position and mip bias — is documented with reproducible checks in [docs/dlss-fidelity.md](docs/dlss-fidelity.md). See [sdk/SDK_GOES_HERE.txt](sdk/SDK_GOES_HERE.txt) for the exact DLLs and their source release, and [docs/blockers.md](docs/blockers.md) for the exact gaps and what is needed to close them.

## License, attribution and publishing

The project is marked **GPL-2.0-or-later**; see [LICENSE](LICENSE). The bundled AMD FSR SDK 2.4.0 binaries, the bundled Intel XeSS 3.0.2 binaries (XeSS SR, XeSS-FG and XeLL) and the bundled NVIDIA DLSS/NGX runtimes (`nvngx_dlss.dll`, `nvngx_dlssg.dll`, all in `sdk/`) are covered separately by [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), which records each SDK's license, redistribution terms and required attribution. Streamline is not distributed with this project. BMD and terrain format research was informed by the GPL-2.0-or-later [GameForge static importer](https://github.com/shaddowdaniel/GameForge-3D-Studio-/blob/0a7fae8fcba0df7b0daa988db8696a0feaf49f7b/source/blender/io/angelica/formats/angelica_static.cc) and [terrain importer](https://github.com/shaddowdaniel/GameForge-3D-Studio-/blob/0a7fae8fcba0df7b0daa988db8696a0feaf49f7b/source/blender/io/angelica/formats/angelica_terrain.cc), copyright 2026 GameForge 3D Studio Authors. Leaf-card methods were informed by the GPL-2.0 [mt2-world-baker](https://github.com/moleh1307/mt2-world-baker); ECWLD layout research used the [RaGEZONE discussion](https://forum.ragezone.com/threads/map-editor.1252114/). Dependency licenses also apply.

**Before public distribution**, verify that you have the rights to publish the launcher artwork and all included code/binaries, and review third-party dependencies and applicable game terms. The provenance and redistribution rights of `tools/launcher-art.jpg` have not been established in this package; replace it with artwork you have rights to if necessary. Do not redistribute the proprietary game client, its DLLs, archives, extracted assets, or derived caches. The `.gitignore` reduces accidental commits but is not a substitute for reviewing staged files.

### RAM-first lossless terrain cache

The viewer now writes validated terrain heights/normals directly into
`cache/terrain-v1/` beneath its working directory, one source block at a time.
It does **not** allocate complete height/normal grids in RAM. Row-local XOR
byte planes plus zlib preserve every original float/normal bit; a shared LRU
keeps a **2 MiB decoded hot set** for terrain and map navigation. There is also
small block-index metadata and one pinned block per active reader. This is a
CPU terrain-data budget, not a whole-program or GPU-memory limit.

- `PW_CACHE_DIR` changes the cache root (the `terrain-v1` namespace is appended).
  Choose a writable local disk with free space. No client files are modified.
- `PW_TERRAIN_RAM_MIB=1` minimizes the terrain hot set; larger values up to 256
  trade RAM for fewer page reads. Default: 2. More disk reads are an intentional
  RAM/latency tradeoff; slow disks can cause streaming stalls.
- Cache objects are content-addressed and verified; changed client data cannot
  reuse stale terrain. Incomplete builds are not published. It is safe to delete
  this derived cache **with the viewer closed**; the next load rebuilds it.
  Do not distribute cached client data. Source validation and cache construction
  still run at startup; this is not a promise of faster warm loading.
- Decoded static-scenery texture caching now defaults to **32 MiB**, down from
  256. `PW_TEX_BUDGET_MIB` remains available. Textures needed by the active
  batch are pinned and may exceed that budget; no textures are omitted or
  downscaled to enforce it.
- PNG terrain export streams rows instead of retaining a full RGB output image.

Reproduce the allocation/correctness check without client assets:
`cargo run --locked --bin terrain-memory-check` (small CI fixture), or add
`-- --full-world` for a synthetic 4096×5632 grid. This measures the owned terrain
cache allocations, not process RSS or physical integrated-GPU memory.
