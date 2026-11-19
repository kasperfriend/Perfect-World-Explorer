# 2 — FSR on RTX 3080 (blocked)

**V: no physical samples.** The [host access probe](01-sdk.md#host-access-blocker-also-applies-to-physical-items-below)
found no attached Windows/RTX 3080 environment or way to execute on it. The
Windows DLL's [hash and ABI checks](01-sdk.md) are not provider admission.
There is no observed FFX version ID, RTX driver version, or validation-layer
output for either Software+FSR or Hardware+FSR. Neither route receives a pass.

The active FFX context's actual queried version and ID are now logged at the
first successful evaluation submission in each extent. Creation logs alone
still do not count. Explicit `WGPU_VALIDATION=1` is honored for opted-in SDK
instances; receiving no messages on a host without the Vulkan SDK layer is
not a validation pass. See the [physical smoke](../../temporal-reconstruction.md#validation-and-remaining-gates).
