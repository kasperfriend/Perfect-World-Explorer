# A3 — First-use failure retirement (S)

A failed first preparation retires the bootstrap worker receiver, optional
vendor resolve, post/history and ray backend allocations before the caller
can handle the error. An explicit reload that failed to draw its requested
renderer also runs retirement before exiting. The owning device survives
that retirement; a worker in progress independently holds a device reference
until it exits. This is not a promise that a stalled driver can be joined.
The parent watchdog can still reap the entire replacement process.

On Linux CPU Vulkan, `lighter_failed_start_releases_owned_allocations_twice`
created two devices and injected a failure after actual Hardware output and
scene allocation. Both attempts returned the charged raw Vulkan allocation
counter and live resource context owners to zero. Dropping each harness
released its device owner (`Weak` no longer upgrades). That measures this
code's ownership; it does not query driver reclamation/VRAM or simulate TDR.
Native's existing 960×720 S PNG remains byte-identical (`1b20187d05a3…`).
`cache/lighter-perf/S-A3/` has the local logs. Its A2 rerun measured one
0.126657 ms copy submit and three 0.020246–0.024916 ms BLAS submits on this
small CPU-Vulkan fixture, not a retail-world or RTX 3080 timing claim.
