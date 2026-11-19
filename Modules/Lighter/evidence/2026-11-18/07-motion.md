# 7 — Retail motion review (blocked)

The [host and client access blocker](01-sdk.md) rules out a matched retail
flight here. **P/V: no retained retail sequence for any provider.** Absence
of a sequence is not evidence of artifact absence.

Use the same scouted `travel.md` plan for portable, FSR and RR, at the same
pose indices and settings. `PW_LIGHTER_TRAVEL_CAPTURE_DIR` opts into a
separate readback run with one PNG per measured present (30–600); keep its
CSV and all logs alongside the frames. The active provider is checked before
accepting each sample and a fallback invalidates the flight. These are
world-color frames before UI/display encoding, not desktop captures.
Readback changes cadence; do not use these logs for Step 8 timing. For
fast cuts, use consecutive waypoints and retain the frames on each side.

| Artifact | Location | Provider | Severity / finding | Screenshot |
| --- | --- | --- | --- | --- |
| Ghosting | Retail path not run | Portable, FSR, RR | Not assessed | None |
| Reveal trails | Retail path not run | Portable, FSR, RR | Not assessed | None |
| Foliage shimmer | Retail path not run | Portable, FSR, RR | Not assessed | None |
| Water flicker | Retail path not run | Portable, FSR, RR | Not assessed | None |
| Camera-cut history | Retail path not run | Portable, FSR, RR | Not assessed | None |

The earlier [temporal interop stand-in](03-rr.md) checks deterministic
history resets. It is not a visual motion or per-material retail verdict.

**S capture plumbing:** [Linux/Xvfb CI](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44523578638/job/135558183606) passed the
30-frame, 160×96 synthetic Native readback run and verified that the timing
report refuses its CSV. The [CI artifact](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44523578638/artifacts/14529426969)
also contains the separate normal and synchronized fixtures. Its signed
transfer failed here, so the raw live sequence is retained on GitHub, not
in the local bundle. The missing retail observations above are unchanged.
