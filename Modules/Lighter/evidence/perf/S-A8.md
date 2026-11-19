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
