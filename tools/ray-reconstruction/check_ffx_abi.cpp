// Compile-only check against the unmodified FidelityFX SDK v2.4.0 headers.
// Keep these x64 offsets in sync with lighter/reconstruction/ffx.rs.
#include <cstddef>
#if !defined(_WIN32)
#define __declspec(x)
#endif
#include "vk/ffx_api_vk.h"
#include "ffx_upscale.h"

static_assert(sizeof(void*) == 8);
static_assert(FFX_API_CREATE_CONTEXT_DESC_TYPE_BACKEND_VK == 0x01000003);
static_assert(FFX_API_CREATE_CONTEXT_DESC_TYPE_UPSCALE == 0x10000);
static_assert(FFX_UPSCALER_VERSION == ((4u << 22) | (1u << 12) | 2u));
static_assert(FFX_API_SURFACE_FORMAT_R16G16B16A16_FLOAT == 4);
static_assert(FFX_API_SURFACE_FORMAT_R32G32_FLOAT == 6);
static_assert(FFX_API_SURFACE_FORMAT_R32_FLOAT == 28);
static_assert(FFX_API_RESOURCE_STATE_COMPUTE_READ == 4);
static_assert(FFX_API_RESOURCE_USAGE_UAV == 2 && FFX_API_RESOURCE_STATE_UNORDERED_ACCESS == 2);
static_assert(sizeof(ffxCreateBackendVKDesc) == 40);
static_assert(sizeof(ffxQueryDescGetVersions) == 56);
static_assert(sizeof(ffxCreateContextDescUpscale) == 48);
static_assert(sizeof(FfxApiResource) == 48);
static_assert(offsetof(ffxDispatchDescUpscale, reset) == 408);
static_assert(sizeof(ffxDispatchDescUpscale) == 432);
