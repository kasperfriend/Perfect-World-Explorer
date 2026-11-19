# A1 — Reconstruction bisection / ownership (S)

- `reconstruction_bisect_gates_opt_in_loads_before_path_lookup`: single
  switch, matching loader only, off never resolves a library path.
- `rejects_excessive_raw_device_features_and_limits`: the raw import now
  rejects non-subsets before extension discovery or `vkCreateDevice`.
- `lighter_reconstruction_bisect_uses_valid_portable_draws` (llvmpipe):
  raw import was bypassed; an interop stand-in opened and rejected the
  image stage without publishing an epoch; the fallback pixels and depth
  exactly matched the portable control.
- Seven reconstruction unit tests passed, including FFX and RR ABI layouts.
  `check_ffx_abi.cpp` compiled with the upstream FFX 2.4.0 and Vulkan
  headers. Neither result executes a Windows vendor DLL.

## Audit boundaries

`bootstrap.rs` uses the same Vulkan entry/instance and physical adapter for
queries and creation. Runtime extension names are bounded, checked against
the enumerated set, and the exact enabled list is passed to HAL. The device
starts with HAL's feature chain (including acceleration dependencies), then
adds supported SDK bits; no queried `pNext` chain is passed back to create.
The import checks requested features/limits and graphics+compute family 0
(the wgpu 0.19 choice). The import error path destroys the raw instance/device
once; a loader reference now survives instance import failure cleanup. HAL
owns imported handles only after success. `Raw` borrows function tables and
handles from the owning wgpu device; it does not destroy that device.

FFX create/destroy/query/dispatch pointer directions and scalar widths were
compared to `ffx_api.h`; `repr(C)` field layouts were checked against the
pinned headers. RR Rust images/eval structures match the ABI-v2 C bridge;
its public C entry points adapt to the official SDK types in C++. The bridge
keeps a unique owner until `open` succeeds. A failed FFX create does not
transfer a context; once created, RAII covers later query or allocation
failure. Libraries are requested explicitly and kept alive through their
contexts. SDK file pins, bridge ABI and adapter gates still apply.

An epoch owns its command pool/fence, views, images and a provider context.
Its fence retires commands before releasing those resources; an ineligible
or failed SDK remains portable. In `reconstruction.rs` descriptor allocation
is through wgpu bindings or inside the provider, not a direct raw
`vkCreateDescriptorPool` call. The `interop` switch stops before our images,
views and bindings; it cannot bisect operations internal to the DLL.

Raw logs: `cache/lighter-perf/S-A1/` (not committed). These tests do not
reproduce or attribute the reported Windows `0xc0000005`.
