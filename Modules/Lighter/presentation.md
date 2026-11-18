# Presentation ownership and device loss (Step 20)

| Stage | Dependency / owner while Lighter is active |
| --- | --- |
| World shadows, reflections, refraction and fog | Trace paths. Do not run the Native water-reflection or underwater tint a second time. |
| Temporal accumulation and spatial filtering | Lighter's linear radiance and guides. Current reverse-Z is preserved. Reference bypasses both. |
| Resolution reconstruction | Lighter resolves its ray image into a full-size scene target, with guide-aware reconstruction **before** the scene post pass. The Native resolution scale and upscaler selection remain saved. |
| Bloom, sharpen, scene gamma and final display encoding | The viewer's existing post/present stages, after ray reconstruction. No ray-world dependence for HUD or menus. |
| Vendor upscalers, FSR1 and generated frames | Paused on the ray route, not fed stale Native motion, exposure or depth. Native/off keeps its prior behavior. A future temporal/FG integration needs coherent ray-world inputs before display post. |
| Minimap, full map and Librarian preview | Independent Native/artwork cameras and offscreen passes. They do not reuse the main ray buffer or evict its scene. |

The window handles both wgpu loss notifications and explicitly reported
Vulkan `ERROR_DEVICE_LOST`. On recognized loss it stops draws, presents and
resizes on the old device. A replacement process uses the existing world
handoff protocol, preserving map, eye, orientation, hour and Lighter settings.
It requests a fresh device and builds the current streamed window at that
camera. It does not transfer GPU objects, temporal history, SDK contexts, or
transient actor/particle state. The old window retires only after a usable
present; failures are logged and a replacement does not recursively restart
if its own device is lost. Ordinary surface loss/resize does not take this path.

Validation: `b95089b` passed the fresh-device 960×720 Software preview after
an explicit wgpu destroy/lost callback (including the admitted BMD), camera
snapshot round-trip and wrong-map rejection, Software/Native dispatch in the
streamed fixture, and Vulkan-query → Software → Vulkan-query with resized
color and depth on Mesa 25.0.7. CI's Xvfb test additionally covers Software
at 160×96, then Software/Native/Software at 192×112. This does not simulate a
physical driver reset or validate third-party frame generation on a ray scene.
