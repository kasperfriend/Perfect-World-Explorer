# 4 — Window, reload and device recovery (physical run blocked)

**V: none.** No Windows desktop/RTX execution connection is attached (see
[host probe](01-sdk.md)). Linux/X11 or a synthetic device destroy is not a
physical driver reset; a hosted MSVC compile does not test presentation.

`lighter_live_viewer_frames` and `lighter_live_world_reload` have Win32 test
entrypoints. In SDK mode they require the requested provider on each ray
frame, including the replacement process; portable fallback fails the test.
They use 512² → 640² for resize, an explicit `Device::destroy` loss callback
after queue/fence retirement, a fresh streamed world in the replacement
process, and nonuniform before/after PNGs plus a restored-PID/provider
receipt. `PW_TEST_RAY_QUERY=1` selects Hardware; `PW_TEST_RAY_RESOLUTION=100`
selects 100% instead of 50% if required. Snapshot/stall/failure controls on
Linux retain their original assertions. The SDK branches require a real
Windows session with the SDK settings and Vulkan; set
`PW_SAVE_LIGHTER_WINDOW` to an empty evidence directory per provider/route.
No device, chunk or feature recreation was observed on the requested host.

**S:** The headless `lighter_device_recovery_fixture` passed on `7148232`
with two CPU Vulkan devices and an injected destroy callback (6.50 s;
`04-window/device-recovery-cpu.log`). Neither new SDK window path ran here.
