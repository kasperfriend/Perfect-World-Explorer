# Phase 0, Step 1 — GPU completion and readback

## Caller audit

| Site | Caller and lifetime | Policy |
| --- | --- | --- |
| `gpu::Renderer::new` staging flush | Initial viewer construction (including a new world in the replacement process), `--gpu-preview`, or a test. Runs before the interactive event loop. | Keep the startup drain to cap peak staging memory before scenery upload. |
| `gpu::run_viewer` redraw | The former `software_adapter` branch called `Maintain::Wait` in Native and waited for a submission callback in Lighter. | Poll before starting a redraw. On a CPU adapter, defer the frame while work is still queued. A 2 ms event-loop deadline schedules the retry; the loop remains available for input/close/handoff. |
| `gpu::run_viewer` surface and XeLL transitions | Reconfigure an existing swapchain; Intel's XeLL initialization requires idle GPU work. | Defer the operation until a later poll observes a drained wgpu queue rather than draining it synchronously for the transition. |
| `gpu::wait_for_capture`, `read_target_pixels` | Explicit one-shot `--gpu-preview`/`--gpu-preview-at` (including effect A/B) and tests. Lighter opts in with `for_capture()`. | Bounded synchronous wait for actual completion, using `Maintain::Poll`. wgpu 0.19's blocking poll can retire submissions after a short fence timeout on a cold CPU driver. The normal viewer doesn't call the capture wait, even for a screenshot. |
| `lighter::vulkan::Renderer::try_prepare_image` | Vulkan-only, before recording a frame; the native command pool, descriptor set, frame uniform, staging and AS resources are shared across frames. | Probe the native fence before recycling them. If busy, don't acquire or present a new world frame. On first use/resize, submit a zero fill and import on a later successful probe. Size replacement also polls for the **wgpu** post/present reads, which follow the native fence on the same queue. The old allocation remains valid until both have finished. |
| `lighter::vulkan::Renderer::drop` | Terminal device/renderer teardown, not a redraw, stream update or Lighter mode switch. | Drain the shared Vulkan queue before freeing the interop allocation. |
| `lighter::checked_pipeline` | Lazy pipeline creation/validation, not a GPU submission wait on the native wgpu backend. | Unchanged. This can still incur CPU compilation/driver cost when initializing a backend. |
| `terrain_gpu::upload_masks` / `update_residency` | The same helper serves initial terrain creation and streamed material-window changes. Texture and lightmap startup throttles are separate. | Keep the bounded startup batches. Live copies and draws are ordered on the wgpu queue, so don't wait to draw. Only one streamed mask atlas upload may be in flight; leave the next mesh in the worker's result channel until its callback completes. |
| Lighter, terrain and Librarian regression harnesses | Explicit CPU readbacks/tests. Librarian's `preview3d.rs` blocking polls are under `#[cfg(test)]`. | Not part of ordinary drawing. |

This changes application GPU waits in the world/RT frame and terrain streaming paths. It does not remove normal swapchain/vsync pacing, boot/teardown synchronization, or the separate Windows vendor upscaler/frame-generator SDK's DX12 command-ring and proxy-swapchain synchronization. Those SDK paths do not run as Lighter's ray world renderer.

## Live and explicit capture

**F12** queues a world-only PNG (current 3D render extent, before scale-up, display gamma and UI) to the `screenshots` folder under the default diagnostics directory. The completion and full path go to the session log. The default render path issues no readback copies. Requests use three reusable `MAP_READ | COPY_DST` buffers and never wait for a busy slot. At the start of a later redraw, `Maintain::Poll` drives the `map_async` callbacks; only then are pixels unpacked, the slot unmapped and a bounded PNG write queued to a background thread. Row pitch and BGRA surfaces are handled without changing the encoded color space.

The explicit headless preview still writes a PNG before returning. It uses the same copy/map code, but is allowed to wait for the result. See the [capture instructions](README.md#verification).

## Reproduce the checks

Use a Vulkan ICD (lavapipe is sufficient; the separate CPU interop test also needs `KHR_ray_query`). Set `WGPU_BACKEND=vulkan` and `PW_SMALL_TREE_ATLAS=1` on CI-sized CPU GPUs. The fixture tests do not require or publish client data.

```sh
cargo test --locked --bin perfect-world-explorer live_readback_ring_pixels -- --ignored
cargo test --locked --bin perfect-world-explorer cutoff_gpu_residency_repacking -- --ignored
PW_PREVIEW_LIGHTER=software cargo test --locked --bin perfect-world-explorer lighter_headless_preview_png -- --ignored
xvfb-run -a cargo test --locked --bin perfect-world-explorer lighter_live_viewer_frames -- --ignored
cargo test --locked --bin perfect-world-explorer lighter_vulkan_interop_on_cpu -- --ignored
```

The ring test fills all three slots before consuming them, rejects an extra request, then reuses the slots and checks pixels from an unaligned BGRA target. The viewer test presents through the real winit event loop to a virtual X11 display and verifies two later-frame world captures. The explicit preview test opens the PNG and checks its dimensions and contents. The Vulkan suite checks deferred initialization/replacement and color/depth after resize in addition to the existing ray tests. These are synthetic correctness checks, not frame-rate, physical-GPU or real-map results.
