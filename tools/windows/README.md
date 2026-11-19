# Run the Lighter performance session on Windows

1. On the RTX 3080 PC, set the **primary display** to **2560×1440**. Close other
   games, leave the current NVIDIA driver installed, and keep the PC awake.
   Use an interactive desktop, not Remote Desktop or an unattended service.
2. From PR #51's Windows check at the commit in the handoff, download and
   **extract** `Lighter-Windows-Perf-Kit`. This pairs the viewer, smoke test,
   script and camera recipe. It does not contain any vendor SDK files.
3. Open **PowerShell 7 (x64)**. Change the first path below to where you
   extracted the kit; leave `E:\pw142` if that's your complete 1.4.2 client.

   ```powershell
   $kit = "$HOME\Downloads\Lighter-Windows-Perf-Kit"
   pwsh -NoProfile -File "$kit\run-perf-session.ps1" -ClientRoot 'E:\pw142'
   ```

   If you already have the pinned SDKs, append `-FsrDll 'C:\…\amd_fidelityfx_upscaler_vk.dll'`
   and/or `-RrBridge 'C:\…\pwe_ray_rr.dll' -RrSdkDirectory 'C:\…\rr-feature'`.
   For the versions and bridge setup see
   [temporal reconstruction](https://github.com/kasperfriend/Perfect-World-Explorer/blob/arena/69c8ea78-perfect-world-explorer/Modules/Lighter/temporal-reconstruction.md).
   Do not download a DLL from an untrusted source or rename a different
   version. Without these inputs, the SDK rows are reported unavailable;
   Native and portable runs still proceed.
4. Let the windows open and close by themselves. Don't move the camera or
   change graphics settings during the run. The two Hardware starts are
   more than a minute apart, and the final run measures at least 10 minutes.
   The full session can take substantially longer on a slow or failing route.
5. Upload the ZIP printed at `Upload:` and its `.sha256` file. Also send one
   sentence about what you saw, for example “first start reached the world;
   second closed on the loading screen” or “both reached the world; visible
   hitch in the water approach.” Upload failed runs too. Do **not** upload
   `scratch`, DLLs, game files or your regular settings.

The default output is `Downloads\Lighter-Perf-<UTC>-<id>\Lighter-Perf-<UTC>-<id>.zip`.
Use `-OutputDirectory 'D:\ExistingFolder'` to put it somewhere else (outside
Explorer and the client). You can read `bundle\SUMMARY.txt` without Python.
The script confines its files and app/SDK working folders to the dated output
folder. Windows/driver-managed logs and caches are still owned by the OS.
Event records, local paths, GPU UUID and computer name are included; review
before sharing outside this PR. The script does not install anything or
change your existing settings. Do not run as administrator just for a capture.

**Troubleshooting:** `-StartupOnly` collects just the two launches. Use
`-StartupProvider rr` (or `fsr`) if that was the route that failed; failed SDK
starts trigger separate single-skip runs. Portable starts never load an SDK.
Default travel is 50% ray resolution at 1440p. The pinned RR path can reject
that extent under its current buffer budget. Preserve that failure. A
separate `-RayPercent 45` session tries a smaller *matched* extent; the driver
may still reject it, and the result must not be combined with the 50% rows.
Reconstruction smoke independently requests 50% and 100% at 512/640 pixels.
