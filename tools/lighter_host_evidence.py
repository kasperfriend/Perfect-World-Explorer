"""Shared target checks for Lighter capture and timing receipts."""

TARGET_ADAPTER = "NVIDIA GeForce RTX 3080"


def requested_target(meta):
    adapter = meta.get("adapter", "")
    heap = meta.get("vulkan_largest_device_local_heap_bytes", "")
    # The NVIDIA driver can report slightly less than nominal capacity.
    # Keep the 10 GiB and 12 GiB variants (same adapter name) distinct.
    return (meta.get("platform") == "windows" and meta.get("architecture") == "x86_64"
            and f'name: "{TARGET_ADAPTER}"' in adapter
            and "vendor: 4318," in adapter and "device_type: DiscreteGpu" in adapter
            and "backend: Vulkan" in adapter and heap.isdecimal()
            and 9.5 * (1 << 30) <= int(heap) <= 10 * (1 << 30))
