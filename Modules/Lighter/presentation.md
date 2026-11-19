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
if its device is lost before that first present. Ordinary surface loss/resize
does not take this path.

**Reload World** uses the same snapshot and watched handoff without marking the
old device lost. The Lighter menu stages renderer changes until Reload World
is pressed. The old world continues drawing with its last active renderer and
can cancel a stalled replacement. A separate parent-side deadline begins when
first-frame work starts, so a blocking compile call cannot defeat the timeout
by preventing the child from polling its event loop. The old world is kept on
failure or timeout. The saved renderer is updated on a successful same-map
handoff, not when choosing a dropdown entry or on child launch.

An ordinary world open uses Native first, even with a ray renderer saved. The
saved renderer remains selected for an explicit **Reload World**; no ray work
starts automatically. This does not affect explicit headless captures or the
mode restored by a watched device-loss replacement.

At startup, Explorer builds and submits the first world and menu offscreen,
then polls for GPU completion before opening the standard swapchain. The first
swapchain pass copies that finished composition (sRGB or UNORM), and only then
are audio, minimap drawing, whole-world preload and the optional temporal/FG
paths allowed to start. On Windows the
separate loading UI remains available up to first present. This does not make
all synchronous calls inside a graphics driver interruptible; the loading UI
can still quit, or the old viewer can cancel a replacement.

Validation:

- `b95089b` passed the fresh-device 960×720 Software preview after an explicit
  wgpu destroy/lost callback (including the admitted BMD), camera snapshot
  round-trip and wrong-map rejection, Software/Native dispatch in the streamed
  fixture, and Vulkan-query → Software → Vulkan-query with resized color and
  depth on Mesa 25.0.7.
- [Linux CI on `ed2cb1e`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44423387785)
  passed the Xvfb test: Software at 160×96; Software/Native/Software at
  192×112; then explicit device destruction and a fresh **replacement process**
  that presented with the same fixture map, camera, hour and Lighter settings.
  Failure to open the requested map or initialize its device cannot report
  readiness through a main-world fallback. The test regenerates fixture
  assets in the child; it is not a retail map-load stress test.

Neither test injects a physical driver reset or validates vendor frame
interpolation on ray scenes.
