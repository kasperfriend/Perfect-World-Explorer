# A8 — Ordered, fail-closed reconstruction (S)

An explicit `PW_RAY_RECONSTRUCTION=rr` with
`PW_RAY_RECONSTRUCTION_FALLBACK=rr-fsr-portable` enables a one-shot FSR
attempt after an RR loader/import/extent/dispatch failure. The FSR file is
verified by the same pinned Lighter loader; bisections and ordinary explicit
provider runs do not opt into this sequence. An extent rejected before
recording can prepare FSR for that frame. A failed recorded RR dispatch
uses portable for that frame and prepares FSR on the next, so mismatched
RR input/history never becomes an FSR frame. Device loss is not an SDK
retry. Recorded travel frames carry `actual_ray_provider`; a requested SDK
capture aborts on any mismatch.

`lighter_temporal_vendor_dispatch` now takes its expected provider from the
request, not the admitted loader. It cannot pass a requested RR trial on FSR
or portable even when the experiment is enabled. Existing size/SHA-256,
physical adapter, unsupported extent, partial recording failure and positive
finite raw radiance guards remain. The missing-FFX fallback is one-shot.
Actual RR→FSR dispatch and finite, non-black positive SDK smoke are still
Windows RTX 3080 work (V); the S stand-ins are not a vendor runtime.

Latest S controls: `lighter_temporal_rejected_extent_is_not_dispatch`
passed (no dispatch), and `lighter_temporal_fsr_masks_and_fallback` passed
with `LP_NATIVE_VECTOR_WIDTH=128 LP_NUM_THREADS=1` after the first attempt
was killed. It checks FSR mask semantics with a stand-in, simulated failed
submit retirement, bit-exact portable color/depth and Reference exclusion.
Missing/wrong RR inputs exited 101 before dispatch; Linux also rejected both
FSR requests because the real Vulkan runtime is Windows-only. The pinned
same-length wrong-hash check passed independently. See
`cache/lighter-perf/S-final/`. None of these are V evidence.
