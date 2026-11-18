# Surface scattering (Step 16)

**GGX / Lambert materials** is an opt-in Lighter setting (headless:
`PW_PREVIEW_PHYSICAL_BSDF=1`). It evaluates the same BRDF for direct lights
and sampled continuation. It does not reinterpret baked lighting, leaf
transmission, or ordered GFX sheets as PBR. The default keeps the legacy
look. Asset pipelines with only diffuse/specular/power use their linear
tint and measured power in that mode: `alpha = sqrt(2/(power+2))`.
Explicit metallic/roughness inputs use `alpha = roughness²`.

- The GGX lobe uses Smith masking/shadowing, RGB Fresnel and visible-normal
  sampling. Its PDF is converted to solid angle with the reflection Jacobian.
  The diffuse lobe uses a cosine PDF and reduces its response by the
  directional interface losses. The two densities form one mixture PDF;
  continuation is `f * cos / pdf`, not specular added on top of full diffuse.
- The microfacet normal changes the response, not the intersection. Reject
  samples on the wrong side of the geometric face without resampling. Finite
  shadow ray endpoints and opacity tests are unchanged.
- Explicit thin glass passes an unrefracted ray through the parallel-sided
  pane, with RGB transmission and the two-interface Fresnel split. A solid
  optical surface instead uses its winding to select entering/exiting IOR,
  traces Snell refraction and total internal reflection, and applies the
  radiance `eta²` factor. The factors cancel for an air–glass–air slab. Closed
  solids require consistent outward winding. This doesn't infer thickness or
  convert shader-alpha materials into dielectric objects.
- Solid glass currently blocks straight shadow connections: bent paths
  through arbitrary glass meshes and mixtures of nested dielectrics aren't
  approximated by simply tinting a shadow. The thin-glass visibility model
  and the legacy water controls are described in [direct lighting](direct-light.md)
  and [water fidelity](water-fidelity.md).

## Validation (2026-11-18)

`114a77b`: a unit-radiance furnace sampled 8,192 directions per case at
cosines 1, 0.7, 0.25 and 0.07, three GGX widths, and dielectric/conductor
responses. The maximum reflected/incident fraction was **0.94759**. Exact
normal-incidence air/glass Fresnel is 0.04 and the glass-to-air 60° probe is
TIR. Software and CPU Vulkan trace the same thin-pane split and tint,
solid slab exit, colored conductor, and primary depth. Those suites, Naga,
and the streamed regression passed on Mesa 25.0.7.
