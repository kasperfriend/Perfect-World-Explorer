# Native-window cutoff: terrain, scenery and fog

World → **Original-game terrain cutoff (saves memory)** is live and saved, off by default. The block-window slider controls the native terrain extent (default radius 7). Enable **Client atmospheric fog** independently.

## Screenshot regression and fixes

The first implementation removed distant terrain but left the independent world-wide tree and scenery passes running. The sky shader ignored its fog background and extended the panorama's pale bottom row into missing ground. Authored fog end distances could exceed the resident window. This made a pale void with floating distant trees/buildings; the earlier terrain-only implementation was insufficient.

- All tree placements are now selected against the **installed** terrain window before LOD selection. Only admitted LOD instances are staged/uploaded. CPU staging and the GPU instance buffer shrink, empty batches are skipped, and an unchanged camera/window/LOD scale avoids another classification and upload. Shared tree meshes/textures and compact placement metadata remain global.
- Cutoff pauses/cancels whole-world ornament preload, drops its pending receiver and releases its uploaded pages/atlases and draw order. Local scenery remains active. The saved whole-world preference is restored when cutoff is disabled, not silently erased.
- Local ornament roots and grass placements respect the same XZ extent. Receiving-model draws and posed geometry outside it are released, their pose/upload work is skipped and their effect lights are deactivated. Unused receiving-model texture/pipeline cache entries are pruned after eviction; active draws pin their shared resources. Source assets still have costs.
- Water visits only intersecting grid rows/columns and clips crossing cells with continuous UVs. Unclipped cells keep the original exact UVs. Shoreline waves now receive table fog and the cutoff extent too.
- With atmospheric fog on, the open lower sky uses the authored fog colour **decoded from encoded RGB once**, not a pale sky-art row or double-encoded clear colour. A short horizon blend preserves the upper sky. This fog skirt is a viewer adaptation, not a claim that the original native sky geometry has been reconstructed.
- In cutoff mode only, the authored above-water fog range is capped conservatively to the nearest resident edge, including the screen's diagonal half-FOV. Its colour is unchanged. The distance cap is a **viewer residency adaptation**, not a newly recovered native equation. Fog off remains independent; underwater/refraction fog retains its separate rules.

## Actual memory ownership

The all-world step-4 terrain mesh is not generated/uploaded in cutoff mode and is dropped when enabling it. Reflections, refraction, realistic maps and water-edge passes cannot resurrect it. Native near geometry and internal stitching remain unchanged; the viewer-only outer far-mesh stitch is disabled.

Blend-mask atlases contain only resident areas plus a one-area halo. Global area IDs, mask texels, layer order, projection and texture coordinates are preserved; only atlas slots are repacked. Moving footprints reload masks on the worker and install matching geometry/bindings together. Initial CPU material parsing is still global and pruned before upload; subsequent window reloads read only intersecting mask files/areas. Turning cutoff off restores all masks and the far mesh.

The UI reports allocated mask/far-geometry MiB and tree instance-buffer KiB, **not total viewer RSS**. One 4096² R8 mask page is 16 MiB. Immutable terrain colour textures/global lightmaps, shared tree assets and source model assets remain costs. Old/new atlas residency may overlap briefly during a safe transition. No real-client/laptop RAM or FPS measurement is claimed.

## Validation

Dependency-free coverage includes window/border membership, native-area coverage, conservative fog limits at wide aspect ratios, ocean cell visitation, saved settings and independent fog/cutoff combinations. GPU regressions additionally check native mask texels/slots, disappearing/re-entering tree instances, receiving-model release/reload, encoded fog-background colour, shoreline fog/cutoff and both water-edge modes. The final CI run is recorded in PR #22; synthetic fixtures do not establish pixel parity with the user's actual client/location.

## Water surface follow-up

The next client screenshot confirmed scenery/window fixes but exposed an independent water bug: `water_fs` passed fragment `position.w` to its table-fog helper as view depth. WGSL fragment W is **reciprocal** clip W, so farther water moved toward zero fog distance. The surface now reconstructs positive view Z as `1 / reciprocal_w`, matching terrain/scenery; reversed-Z depth does not change this relationship. The alternate authored-water material now receives the same frame-selected fog range, colour and enable gate, mixing encoded RGB before its existing raw-target alpha blend. Underwater surface draws retain the frame's underwater triple; refraction absorption/edge reconstruction is unchanged.

The earlier `water-fog-gpu-check` supplied a synthetic depth directly to the helper and therefore could not detect this call-site error. It now supplies reciprocal W. More importantly, the full production water-pass GPU regression renders quads with non-unit clip W and checks near, half/full fog, distant fog-off, below-surface and reflection-disabled variants. The alternate-water GPU regression checks the same near/half/full/off transitions and existing animation/alpha blending on UNORM and sRGB-backed raw targets. No material resolution, fog distance policy or reflection formula changed in this follow-up.
