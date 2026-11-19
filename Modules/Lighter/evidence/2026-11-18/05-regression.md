# 5 — Native and portable regression

**S: passed.** At `71482321ed1d60b39dad0d16746d1eb9768d5b20`, five new PNGs
byte-match the retained `6f97ea6435efc18085286bd041341833eac96c7e` controls.
Same synthetic camera and environment, 960×720 display, 240×180 rays, one
spp, one bounce and four fixed 16 ms warm-ups. CPU Vulkan llvmpipe/Mesa
25.0.7 (LLVM 19.1.7); the full-primary ray-query fixture used
`LP_NATIVE_VECTOR_WIDTH=128 LP_NUM_THREADS=1` on both sides.

| Route | New cases compared to prior | Shared PNG SHA-256 |
| --- | --- | --- |
| Native | Off, FSR requested, RR requested | `1b20187d05a3cacc8701e13ccbe4db5b13630fc48b4c0ec41aee39fb1f4c1798` |
| Software | Off | `06e71efd409b6e3225781ebf7c167cb35e30bb9169e5e3981c3f5a3854adfdd3` |
| Full-ray query (**CPU test** only) | Off | `df103549a61e8bd1527f42b61d64e747a4ce4323019b84efc1bc289605d8c4e0` |

Both requested-SDK Native logs say `Native ignores PW_RAY_RECONSTRUCTION;
no Vulkan ray SDK initialized`. Their coverage sidecars say `portable`.
Startup returns before resolving SDK paths in Native. `git diff --exit-code`
against the prior build reports no change to Native `src/{fsr,dlss,xess}` or
EASU implementations. The separate Graphics upscaler/FG branch in `gpu.rs`
continues to run independently of Lighter SDK selection.

See `05-regression/` in the [evidence bundle](14-delivery.md) for the PNGs,
sidecars, hashes, logs and commit receipt. **P/V: not run**; this is an S
pixel regression and a Native DX12 source isolation audit, not a new
Windows GPU/NGX/FG integration test.

**S repeat on `31bfeef`:** The same five routes byte-match the PNGs in the
table again, including the Native SDK-ignore controls. See
`05-regression/current/`, `05-regression/CURRENT-PNG-SHA256SUMS.txt` and the
new tested-commit receipt. Native FSR/NGX/XeSS provider source is still
unchanged relative to `6f97ea6`; no DX12 GPU test is claimed.
