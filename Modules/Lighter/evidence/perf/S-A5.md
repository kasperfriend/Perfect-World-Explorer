# A5 — Separate exact RT geometry counts (S)

`PW_RAY_EXACT_PROXY=1` admits an exact secondary geometry reduction for
single-material, opaque, unordered static scenery. It drops only a redundant
suffix of identical ordered vertex-index triples. Primitive numbers,
attachments, UVs, materials and the full raster draw packet are preserved;
cutouts, emissive/ordered faces, independently oriented faces and posed actors
are not simplified. The reduced primitive count is part of the BLAS key so a
subsequent full packet cannot alias a trimmed build. Rigid transforms share
BLAS just as before; only changing posed geometry can refit its BLAS.

`exact_rt_proxy_leaves_distinct_faces_and_materials_intact` checks a 20-to-2
tri suffix (10%), and distinct-winding and mixed-material controls (20-to-20).
This case is not a retail reduction estimate. Travel records unique active
Vulkan BLAS triangles and actual TLAS instance counts separately, per draw.

General geometric LOD, small-object/grass removal, distant merged clusters
and comparison of shadows/reflections on the target remain open. Removing
arbitrary triangles based on a percentage alone would create ray leaks.
