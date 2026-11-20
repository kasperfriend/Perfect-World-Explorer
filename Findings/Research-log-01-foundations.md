> Historical research log. Entries are chronological and describe the state at the time; later entries can supersede earlier ones. See the README and roadmap for current status. Client data and private storage details are not included.

# Perfect World 1.4.2 client investigation

## Access and scope

Research used selected files from a privately supplied client archive. Client files and archive listings are not distributed here.

## Verified observations

- `element/elementclient.exe`: 32-bit Windows PE, 6,000,640 bytes. Strings include `A3DTerrain2`, `CECWorld::LoadTerrain`, `CECScene::LoadWaterArea`, `CECScene::LoadGrassArea`, `CECWorld::InitPlantsObjects`, and `D3DX8 Shader Assembler Version 0.91`. These are evidence of engine components and legacy shader tooling, not proof of their implementations.
- Main world data is under `element/maps/world/`: `world.ecwld`, `world.ecbsd`, `world.trn2`, `world.t2hc` and numbered `world_N.t2bk`, `world_N.t2mk`, and lightmap files. The ZIP contains 88 numbered terrain block files. `world.trn2` starts with ASCII `2NRT` followed by little-endian 1; `world_1.t2bk` starts `BKBT` followed by 5; `world_1.t2mk` starts `BKMT` followed by 2; `world.ecwld` starts `DWCE` followed by 12. Semantics of these fields still require verification.
- Game assets include `shaders.pck`, `building.pck`, `litmodels.pck`, `models.pck`, `trees.pck`, `grasses.pck`, `surfaces.pck`, and `textures.pck`. `shaders.pck` ends with a Perfect World file package marker; its internal directory and compression must be decoded before its shaders can be ported.
- The executable names individual shader paths including `Shaders\\vs\\shadow_vs.txt` and `Shaders\\ps\\fullglow1_14.txt`, and references terrain water, but no shader behavior has yet been reconstructed.

## Renderer status

No client format parser, game shader port, world renderer, or visual parity has been completed. Full parity requires reverse engineering and testing each format and pipeline against the game; it cannot be inferred from filenames or magic bytes. First implementation targets should be a bounds-checked PCK reader and validated TRN2/T2BK terrain decoder, followed by placement, models, materials, water, vegetation, lighting and shader translation. Keep all game assets user-supplied.

## Update

The full ZIP was downloaded to workspace cache and extracted; unzip reported filename mismatches for some script files and returned a warning status. Workspace cache is not persisted between turns. A Rust elevation-shaded main-world terrain image prototype now decodes all 88 numbered tile files and has been tested against this client. It does not deliver the full world renderer described above.

## Interactive terrain and package access

The code now includes an experimental Rust software-rasterized 3D view of the terrain, with pan, rotate, tilt and zoom. A headless render was generated and inspected, but the window itself has not been tested in this headless environment. Geometry is downsampled and colors are diagnostic; this is not the game's renderer.

A separate PCK v2 reader lists the supplied retail packages and reads individual entries. The actual shader archive has 166 entries, including `shaders/vs/terrain_vs.txt` and multiple water shader programs; `shaders/vs/shadow_vs.txt` was decompressed and inspected as vertex shader 1.1 assembly. No shader translation or integration has been implemented. The reader does not yet cover split archives or alternative keys.

## Flight camera

The interactive terrain prototype now uses a first-person perspective with free movement and altitude controls rather than an orbit/map camera. It still uses approximated colors and lighting and contains no models, trees, water, or game shader rendering. Its flight screenshot was generated headlessly from actual client terrain; the desktop window remains untested here.

## Terrain materials

The renderer now decodes GBK texture paths, reads the 162 referenced DDS terrain textures from textures.pck, parses the version-2 T2MK layer tables and decompresses their alpha masks. A perspective-correct CPU rasterizer samples the texture layers for each terrain pixel. A headless run against the client decoded all 162 textures and 4,521 masks in 1,408 areas. Texture sampling, filtering, projections, lighting, blending and shader behavior have not been validated against the game; this remains far from one-to-one parity. No placed objects, water, vegetation, engine shaders or sky are implemented.

## Full-resolution terrain and normals

The CPU renderer now uses every stored terrain height sample in its visible region (rather than stepping over three out of four samples), and decodes the client's compressed angular vertex normals. It interpolates directional shading using these normals and bilinearly samples textures and masks. The light direction and ambient terms are still placeholders; texture mipmapping, lighting and game parity have not been verified. A headless image was generated against all 88 original T2BK files. Full-resolution software rendering is expected to be slow; GPU batching and streaming are needed.

`world.ecwld` includes `.spt`, `.mox`, `.ecm` and `.gfx` path strings, consistent with trees, vegetation, models and effects, but their placement records are not yet decoded. `world.ecbsd` references `BSData` files. The presence of filenames does not establish how to position or render these assets, so no placements are guessed.

## Scene records and static models

Using the v12 ECWLD block layout documented at https://forum.ragezone.com/threads/map-editor.1252114/, the Rust parser validates all 22,528 block boundaries and decodes tree, water-reference, grass-reference, ornament-reference, effect, EC-model, sound and other scene records. It follows ornament references into EC-BSD, deduplicates shared IDs and parses 16,037 unique transforms and BMD paths; all resolve in litmodels.pck. Tree, grass, EC-model and GFX paths also all resolved against their packages. Parsing references does not imply rendering those classes of asset.

A Rust A3DLitModel BMD parser now decodes all 16,335 world BMDs (7,802,464 vertices). Of 3,240 distinct mesh texture names, 3,238 are present in building.pck. The viewer streams nearby BMDs and textures and rasterizes the geometry with their real transform and UVs. It has only been checked with a headless screenshot; alpha, per-vertex colors, lightmaps, full material semantics and shader parity remain unimplemented. Missing textures are logged and meshes skipped. Caches are evicted outside the current streaming radius. No SpeedTree, EC-model, water, grass, effect or sky renderer has yet been implemented.

## Water and SpeedTree investigation

Reverse-engineered the supplied elementclient.exe's scene water loader at 0x41d740, BSData resolver at 0x41e750, water-area loader at 0x780d30 and water cell iteration at 0x781809. EC-BSD water references identify a BSData subterrain and record index; the BSData footer contains the offsets. The client water-area version 0x10003000 stores color, elevation, dimensions, cell size, center and one byte per cell; the engine tests bit 0 before using a cell. The Rust loader resolved 107 unique areas from the client's data. It renders their cells as opaque flat-color planes, not the game's shader, waves or reflections; a headless water-region image has been generated.

The ECWLD lists 59 tree source types and 3,938 placements. All referenced SPT sources and composite maps were extracted and verified with the new tree-source tool. The source SPT is procedural, not pre-baked geometry. The supplied 32-bit Windows speedtreert.dll exports the original LoadTree, Compute, GetGeometry and GetTextures APIs. No safe cross-platform implementation or tested DLL bridge has yet been built, and trees are not rendered; placeholder trees were deliberately not generated.

A later experimental wgpu backend now exists; the preceding CPU results do not represent GPU performance. See the update below.

## Experimental GPU and SpeedTree ABI audit

The default interactive renderer uses wgpu with GPU vertex and index buffers, depth testing and an atlas. It covers a 13×13 tile terrain region, restricting water to the resident terrain region and nearby objects to a shorter radius. The atlas is baked and rebuilt synchronously on the CPU when the camera moves two tiles; this is not asynchronous streaming and can stall. A headless test of the real client succeeded on Vulkan llvmpipe, which is software, not a hardware GPU. Native desktop and physical GPU are untested. At that stage, the original D3D8 water shader had not been ported and trees were not rendered; later experimental additions are described below.

The `speedtree-check` tool confirms the client DLL is PE32 x86, tests exact decorated constructor, destructor, LoadTree, Compute, GetGeometry, GetTextures and SetCamera names and validates all 59 SPT source signatures. CORRECTION: MSVC decoration `FFF` means three **short** parameters, not floats (`M` is float). That initial audit did not invoke the DLL; the subsequent bridge and Wine probes below do.

## Working PE32 SpeedTree bridge

On this client, the executable calls the SpeedTree allocator with object size 0x9c, initializes branch/leaf/frond lighting with enum 0 and wind with enum 2, calls `GetTreeSize`, multiplies the size by 0.3048 (float at VA 0x8e7b20), passes zero variance to `SetTreeSize`, and calls `Compute(NULL, 1, true)`. Correction from subsequent investigation: the original DLL export `KFFF` uses `unsigned long, short, short, short` (MSVC `F` = short). Passing floats accidentally supplied zero in the low 16-bit argument slots and pinned geometry at highest LOD; corrected in the helper. The project now calls the supplied client DLL in a separate 32-bit Windows helper. Under Wine with the supplied DLL, all 59 SPTs produced caches. The WGSL viewer loads all 59 using source and DLL hashes, draws branch/frond strips and textured leaf cards, and renders actual placements. A degenerate-axis issue initially affected six types. Selecting the strongest of the four card corners fixed it; four independent runtime camera directions now pass the 0.02 tolerance for all 59 types at highest detail. Invalid normals are replaced by up normals. No claim of game parity: lighting, wind, LOD, leaf exceptions and materials remain.

The Rust Windows GNU target passes cargo check; the 32-bit helper was tested under Wine, not on native Windows. GPU screenshots were rendered via llvmpipe, not a hardware GPU. At that earlier stage water remained flat; see the subsequent multipass update.

## Water GPU passes and SpeedTree LOD update

A later wgpu path loads all 29 original uncompressed ARGB normal-map DDS frames from `textures/water/waternormal/water_normal00.dds` through `28.dds` (the path is present in the client executable). The renderer draws the opaque scene to reflected and scene-color maps, clips the reflected world to the chosen plane, samples the normal sequence, applies approximate Fresnel and specular, and mixes against the original scene with a depth-based shoreline fade. This is not the original D3D8 shader or final parity: constants, UV scaling, frame rate, distorted refraction and depth edge are provisional; one nearest plane is shared across all visible areas, and wave mesh, precise clipping, sky and underwater rendering remain. Two real-client GPU screenshots passed on Vulkan llvmpipe; hardware and Windows desktop remain untested.

The SpeedTree helper now uses the corrected short-argument ABI and records three runtime LOD snapshots. The viewer switches snapshots at provisional 180/400-unit distances during terrain-region rebuilds and uses the highest-detail leaf-card dimensions at other LODs; fades, secondary leaf layers, billboard LOD and per-frame game selection are absent. The new PWTREE04 format requires rebaking older caches. All 59 trees produced three snapshots under Wine and loaded in the viewer.

## Windows executable and background streaming

Cross-compiled x86-64 Windows viewer and tree-source plus the 32-bit SpeedTree helper. The packaged batch launcher was tested end-to-end under Wine against the supplied client: extracted 59 trees, baked them, loaded all 59 types, launched the window and exited normally. A separate Wine interactive flight test triggered several background terrain builds. Wine Vulkan used llvmpipe software. The first Wine DX12 offscreen attempt was killed during rendering in the constrained sandbox; the DX12/native-Windows hardware path has not been validated. The desktop renderer now owns terrain and building data on a background worker; at most one rebuild is in flight, with the latest camera position used next. Uploads remain on the main thread; rapid flight can outpace streaming. WGPU_BACKEND and WGPU_ADAPTER_NAME can select a backend and adapter.

## Single Windows executable

The 64-bit Windows GUI executable now embeds the 32-bit helper, prompts for a client folder, shows a loading window, extracts SPT sources itself, temporarily runs its embedded PE32 bridge against the user-provided client DLL, removes the helper and extracted source files, and keeps only the generated cache under LocalAppData. Cache keys include the canonical client path, PCK/world metadata, DLL contents, bridge contents and cache version; trees are validated against source and DLL hashes when loaded. Concurrent preparations use distinct temporary directories. End-to-end folder-picker selection, first-run setup, all 59 cached tree types, interactive flight, background rebuilding and clean exit were tested under Wine + Vulkan llvmpipe. The Wine DX12 attempt from the prior build was killed by the resource-limited environment; native Windows and hardware GPUs have not been tested.


## Multi-height water and further SpeedTree investigation

The water pass now groups indices by exact stored height, conservatively culls each group against the camera frustum, and submits a separate reflected scene and water draw for each visible height, reusing one reflection texture in queue order. The opaque scene and depth are generated once. The original BSData survey found 107 areas and four approximately distinct elevations (216, 251, 251.07, and 264.675). Real-client Linux and Windows-under-Wine Vulkan llvmpipe previews processed visible planes 251.07, 216, and 251.0 without validation errors. The updated Windows build also completed a Wine interactive flight, background terrain rebuilds and a clean Escape exit. Rendering an extra full-scene reflection per visible height is expensive and has not been profiled on physical GPUs. It still is an approximation, not the original water renderer.

Further executable inspection found that client setup enables leaf rocking and disables billboard dropping. The client passes SpeedTree SetLodLimits near=5 * max(tree bounding height, 5) * factor and far=16 * max(height, 5) * factor; the manager initializes the factor to 1.0 and may update it at runtime. The client advances SpeedTree's time and supplies wind strength dynamically. The viewer does not yet reproduce these settings: it still uses three static LOD snapshots and fixed thresholds, and no rocking. Simply changing thresholds without reconstructing the runtime's discrete LOD transitions would be an unverified approximation.


## Grass masks, MOXB geometry, and water pixel arithmetic

The supplied executable's grass loader at 0x41dcb0 reads the 36-byte grass record from EC-BSD (type, center, grid size, width, height, BSData file and record index). BSData v2 records hold compressed-size and raw-size followed by a zlib bitmap. The grass loop around 0x653bd7 tests bitmap bits most-significant-first. The supplied world has 2,181 unique referenced areas, 512 by 512 bits at 0.5-unit grid size, with 1,496,155 set bits. The viewer decodes these and 220/220 referenced MOXB grass types, resolves 53 textures, and emits nearby real MOXB geometry into GPU buffers. One texture name in the MOXB uses a space but the archive uses an underscore; an exact alternate spelling resolves it. The loader handles both unversioned MOXB and VERSION: 2 in this client, with bounded vertex/index validation. Rust tests exercise MSB bit order, malformed inputs and a versioned triangle.

The current grass renderer samples terrain triangles and uses the type size/alpha reference but still approximates the precise placement transforms, jitter, size variation, wind, sight range, material state, and streaming. Its mesh generation is CPU-side and expands instances into GPU vertex/index buffers; it is not the client's original GPU instancing or grass parity. Linux and Windows-under-Wine software Vulkan previews produced actual grass geometry; no game visual comparison or physical GPU test has been performed.

The water_expensive.txt shader uses an edge-alpha-squared factor for normal-map distortion with c1.x=0.25 and c1.y=-0.15, c4.rgb=0.95 for refraction and a fifth-power Fresnel mix. The viewer now follows those pixel-shader operations, and the paired water_vs.txt sets its secondary diffuse/specular color to zero. Its edge alpha is still reconstructed from depth instead of the client edge pass, min reflectivity, normal UV animation, render targets, scene clipping and underwater handling remain provisional. This does not constitute the original complete shader pipeline.


## World-scale GPU renderer revision

Persistent 8-grid-step terrain mesh plus a locally streamed full-resolution 13×13-block mesh; the shader blends original client material layers from GPU texture arrays, with mipmaps and 128×128 masks. Source textures up to 256×256 remain in a 256 array; larger textures use a 512 array. All 107 water areas and 3,938 tree placements are submitted globally; far trees use instanced runtime-derived LOD2 geometry. Buildings and grass remain local. Original sky, lighting, animated models, effects and exact tree/water rendering are unfinished. Linux Vulkan software tests passed for scene output and mouse capture/release; this revision's Wine software-Vulkan render exceeded sandbox memory, and native Windows hardware is not verified.


## Per-LOD SpeedTree card correction and menu

PWTREE05 captures per-LOD card tables rather than reusing LOD0 card dimensions. On the supplied DLL, the table remained stale when only a lower LOD was queried; setting LOD0, setting the camera, calling GetGeometry, then setting the desired LOD and calling GetGeometry refreshed the table. For 59 types and three sampled LODs, four extra direction checks returned no transform errors and the bridge completed with zero failures. The viewer allocates a GPU tree atlas and instance batches for all three snapshots and selects by distance and estimated height on the CPU; geometry and leaf camera orientation are rendered on the GPU. No wind, continuous transition, billboards or game image comparison yet.

In that earlier build, F7 toggled a wgpu/egui overlay, releasing captured mouse on open; it controls far clip, streaming radius, LOD scale and movement controls. Linux and Wine menu-open/close were tested with a stripped diagnostic render; full Wine software rendering exhausted the 2 GB sandbox. Native Windows and a hardware GPU have not been tested.


## Whole-world static geometry, sky, water animation

The optional whole-world loader processes BMD ornaments in batches, using independent 4096² GPU atlas chunks and a per-chunk vertex cap rather than the nearby atlas's 1,024-texture ceiling. A full CPU validation of this client processed 16,037 placements into eight pages: 7,476,490 vertices, 4,914,548 triangles, three meshes needing a gray fallback for two genuinely absent textures. Explicit TGA decoding fixes six paths that magic-byte detection misidentified. GPU test coverage is currently a 300-placement sample; full GPU residency requires a larger machine. Whole-world mode sets a 32,000-unit minimum far plane, while local grass and missing animated models/effects remain limitations.

Water had a hard-coded floor(time × 10) normal sequence; the shader now interpolates the current and next client DDS frames and normalizes the result. The graphics menu can disable expensive per-height reflection passes. These changes do not establish original timing or pipeline parity.

Sky preview uses nine original 512² client BMPs (clear/evening/night2, 01/02/roof) from textures.pck, mapped on an approximated panorama, crossfaded by hour. Approximate day/night world illumination is not reverse-engineered from the client. Linux software Vulkan screenshots at noon and midnight were generated, including water with reflections on and off. The egui focus event previously was lost while the menu was closed, causing clicks to be ignored; forwarding focus events fixed interactive checkboxes and sliders in the Linux diagnostic.


## Plume alpha, sky joins, and precinct music

The Plume giant tree is ECWLD ornament 9183, BMD `52/litmodel_1465.bmd`, with foliage DDS `Building/textures/d/3bf.dds`. The DDS has 150,061 fully transparent pixels which the earlier RGB-only BMD atlas dropped. Local and whole-world GPU BMD atlases retain RGBA and use a fragment alpha cutoff. Linux software GPU previews of both paths show transparent leaf shapes. Alpha cutoff/material settings remain approximate.

At panorama seams the adjacent sky source is sampled at its own clamped edge (.999/.001) instead of mirroring the current UV, so the two 50/50 blends converge at the boundary. All joins and roof mapping remain to be compared to original game output.

`maps/world/precinct.clt` is UTF-16LE and supplies 332 zone names, polygons and music playlists. At (-755,-1352), the Plume playlist contains `Music\\city_d1.mp3`; at (-2520,4100), Snowy Village contains `Music\\village_b1.mp3`. The implemented overlap choice (smallest polygon) and sequential playback are unverified heuristics. Case-insensitive lookup matches client paths; unavailable paths are skipped. Wine with a null audio device successfully reported switching between those two files and stopping, and decoded actual MP3 and a generated Vorbis OGG. Physical audio and native Windows playback have not been tested.


## Launcher, packed music, full-resolution terrain

The supplied `elementclient.exe` is PE32 x86, 6,000,640 bytes, SHA-256 `a290998dc13cc5558b54bda3f65d099f95bc0702e54a0613c6d7669f35c6cdb0`. The launcher validates PE structure and this fingerprint to identify that known client; other builds are explicitly unrecognized, not assumed to be 1.4.2. It validates essential paths, remembers the chosen element folder in UTF-16, and shows loading progress through GPU initialization. Wine tested remembered detection and Load World; full first-run picker selection and native Windows still require validation.

The City of Raging Tides precinct at (2673,-4251) lists `Music\\island2_n.mp3`, absent from the loose folder. In fact the real track is compressed in `sfx.pck` as `sfx/music/island2_n.ogg` (2,159,326 decompressed bytes, OggS). There are several dozen `sfx/music/*.ogg` tracks including missing islands and theme music. The player indexes the PCK, maps missing MP3 playlist basenames to OGG, and decodes client audio directly from memory without extracting files. Wine's null audio backend successfully switched Plume MP3 -> Snowy Village MP3 -> actual Raging Tides OGG. Actual audible Windows output remains untested.

The 162 terrain textures in this client are 112×256², 49×512² and 1×1024²; all 4,521 terrain masks are 256². A third texture array preserves the 1024² source and full-resolution mask pages retain the native masks. Full-resolution pages take about 288 MiB versus about 80 MiB at 128². The menu is open on world entry and the launcher offers lower-memory masks. Full-resolution atlas upload and shader creation succeeded in software Vulkan, but the 2 GB sandbox could not complete a rendered frame at that quality. The lower-memory path rendered with the new shader; physical GPU and screenshot comparison remain outstanding.


