// Optional NVIDIA DLSS-RR Vulkan bridge. Build with the official NGX SDK
// loader library, not the nvngx_dlssd feature binary. No C++ virtual-table
// layout or unpublished driver signatures cross into the Rust viewer.
#include <vulkan/vulkan.h>
#include "nvsdk_ngx_helpers_vk.h"
#include "nvsdk_ngx_helpers_dlssd_vk.h"
#include <codecvt>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <locale>
#include <memory>
#include <string>

#if defined(_WIN32)
#define PWE_API extern "C" __declspec(dllexport)
#else
#define PWE_API extern "C" __attribute__((visibility("default")))
#endif

static const char project[] = "5f2c9a41-7d13-4e6b-9c88-1a2b3d4e5f60";
static const char version[] = "1.0.0";
static constexpr auto fail = NVSDK_NGX_Result_FAIL_InvalidParameter;

struct Paths {
    std::wstring sdk, log;
    const wchar_t* dirs[1];
    NVSDK_NGX_FeatureCommonInfo info{};
    Paths(const char* sdk_dir, const char* log_dir) {
#if defined(_WIN32)
        std::wstring_convert<std::codecvt_utf8_utf16<wchar_t>> utf;
#else
        std::wstring_convert<std::codecvt_utf8<wchar_t>> utf;
#endif
        sdk = utf.from_bytes(sdk_dir);
        log = utf.from_bytes(log_dir);
        dirs[0] = sdk.c_str();
        info.PathListInfo = {dirs, 1};
    }
    Paths(const Paths&) = delete;
    NVSDK_NGX_FeatureDiscoveryInfo discovery() const {
        NVSDK_NGX_FeatureDiscoveryInfo d{};
        d.SDKVersion = NVSDK_NGX_Version_API;
        d.FeatureID = NVSDK_NGX_Feature_RayReconstruction;
        d.Identifier.IdentifierType = NVSDK_NGX_Application_Identifier_Type_Project_Id;
        d.Identifier.v.ProjectDesc = {project, NVSDK_NGX_ENGINE_TYPE_CUSTOM, version};
        d.ApplicationDataPath = log.c_str();
        d.FeatureInfo = &info;
        return d;
    }
};

PWE_API uint32_t pwe_rr_api_version() { return 2; }

// A null instance queries **instance** requirements before vkCreateInstance.
// Otherwise validate support and query **device** requirements before vkCreateDevice.
PWE_API uint32_t pwe_rr_extensions(VkInstance instance, VkPhysicalDevice physical,
    const char* sdk, const char* log, VkExtensionProperties* dst, uint32_t capacity,
    uint32_t* count) try {
    Paths paths(sdk, log);
    auto discovery = paths.discovery();
    VkExtensionProperties* ext = nullptr;
    uint32_t n = 0;
    NVSDK_NGX_Result rc;
    if (instance != VK_NULL_HANDLE) {
        NVSDK_NGX_FeatureRequirement requirements{};
        rc = NVSDK_NGX_VULKAN_GetFeatureRequirements(instance, physical, &discovery, &requirements);
        if (NVSDK_NGX_FAILED(rc)) return rc;
        if (requirements.FeatureSupported != NVSDK_NGX_FeatureSupportResult_Supported)
            return NVSDK_NGX_Result_FAIL_FeatureNotSupported;
        rc = NVSDK_NGX_VULKAN_GetFeatureDeviceExtensionRequirements(instance, physical, &discovery, &n, &ext);
    } else {
        rc = NVSDK_NGX_VULKAN_GetFeatureInstanceExtensionRequirements(&discovery, &n, &ext);
    }
    if (NVSDK_NGX_FAILED(rc)) return rc;
    if (n > capacity || (n && !ext)) return fail;
    if (n) std::memcpy(dst, ext, sizeof(*ext) * n);
    *count = n;
    return NVSDK_NGX_Result_Success;
} catch (...) { return fail; }

struct Image {
    VkImage image;
    VkImageView view;
    VkFormat format;
    uint32_t size[2];
    NVSDK_NGX_Resource_VK resource(bool write = false) const {
        return NVSDK_NGX_Create_ImageView_Resource_VK(view, image,
            {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}, format, size[0], size[1], write);
    }
};
struct Images { Image color, depth, velocity, output, guides[4]; };
struct Eval { float jitter[2], ms; uint32_t reset; float view[16], proj[16]; };
static_assert(sizeof(Image) == 32 && offsetof(Image, size) == 20 &&
    sizeof(Images) == 256 && offsetof(Images, guides) == 128 &&
    sizeof(Eval) == 144 && offsetof(Eval, view) == 16 && offsetof(Eval, proj) == 80,
    "The viewer requires the x64 PWE RR v2 ABI");

struct Context {
    Paths paths;
    VkDevice device;
    bool initialized = false;
    NVSDK_NGX_Parameter *params = nullptr;
    NVSDK_NGX_Handle *feature = nullptr;
    NVSDK_NGX_DLSSD_Create_Params create{};
    Context(const char* sdk, const char* logs, VkDevice dev) : paths(sdk, logs), device(dev) {}
    ~Context() {
        if (feature) NVSDK_NGX_VULKAN_ReleaseFeature(feature);
        if (params) NVSDK_NGX_VULKAN_DestroyParameters(params);
        if (initialized) NVSDK_NGX_VULKAN_Shutdown1(device);
    }
};

PWE_API uint32_t pwe_rr_open(VkInstance instance, VkPhysicalDevice physical, VkDevice device,
    PFN_vkGetInstanceProcAddr gipa, PFN_vkGetDeviceProcAddr gdpa, const char* sdk,
    const char* log, const uint32_t* extent, Context** result) try {
    *result = nullptr;
    auto ctx = std::make_unique<Context>(sdk, log, device);
    auto rc = NVSDK_NGX_VULKAN_Init_with_ProjectID(project, NVSDK_NGX_ENGINE_TYPE_CUSTOM,
        version, ctx->paths.log.c_str(), instance, physical, device, gipa, gdpa,
        &ctx->paths.info, NVSDK_NGX_Version_API);
    if (NVSDK_NGX_FAILED(rc)) return rc;
    ctx->initialized = true;
    rc = NVSDK_NGX_VULKAN_GetCapabilityParameters(&ctx->params);
    if (NVSDK_NGX_FAILED(rc) || !ctx->params) return NVSDK_NGX_FAILED(rc) ? rc : fail;
    int needs_driver = 0;
    NVSDK_NGX_Parameter_GetI(ctx->params, NVSDK_NGX_Parameter_SuperSamplingDenoising_NeedsUpdatedDriver, &needs_driver);
    if (needs_driver) return NVSDK_NGX_Result_FAIL_OutOfDate;
    int available = 0;
    NVSDK_NGX_Parameter_GetI(ctx->params, NVSDK_NGX_Parameter_SuperSamplingDenoising_Available, &available);
    if (!available) return NVSDK_NGX_Result_FAIL_FeatureNotSupported;

    void* query = nullptr;
    NVSDK_NGX_Parameter_GetVoidPointer(ctx->params, NVSDK_NGX_Parameter_DLSSDOptimalSettingsCallback, &query);
    if (!query) return NVSDK_NGX_Result_FAIL_OutOfDate;
    const auto optimal = reinterpret_cast<PFN_NVSDK_NGX_DLSS_GetOptimalSettingsCallback>(query);
    bool matched = false;
    struct Optimal { int tier; uint32_t status, w, h; } tried[5]{};
    size_t attempts = 0;
    // Query an exact supported size. RR does not support dynamic resolution.
    for (const auto tier : {NVSDK_NGX_PerfQuality_Value_DLAA, NVSDK_NGX_PerfQuality_Value_MaxQuality,
            NVSDK_NGX_PerfQuality_Value_Balanced, NVSDK_NGX_PerfQuality_Value_MaxPerf,
            NVSDK_NGX_PerfQuality_Value_UltraPerformance}) {
        auto* p = ctx->params;
        NVSDK_NGX_Parameter_SetUI(p, NVSDK_NGX_Parameter_Width, extent[2]);
        NVSDK_NGX_Parameter_SetUI(p, NVSDK_NGX_Parameter_Height, extent[3]);
        NVSDK_NGX_Parameter_SetI(p, NVSDK_NGX_Parameter_PerfQualityValue, tier);
        NVSDK_NGX_Parameter_SetI(p, NVSDK_NGX_Parameter_RTXValue, 0);
        unsigned int w = 0, h = 0;
        const auto status = optimal(p);
        if (NVSDK_NGX_SUCCEED(status)) {
            NVSDK_NGX_Parameter_GetUI(p, NVSDK_NGX_Parameter_OutWidth, &w);
            NVSDK_NGX_Parameter_GetUI(p, NVSDK_NGX_Parameter_OutHeight, &h);
        }
        tried[attempts++] = {static_cast<int>(tier), static_cast<uint32_t>(status), w, h};
        if (w == extent[0] && h == extent[1]) {
            ctx->create.InPerfQualityValue = tier;
            matched = true;
            break;
        }
    }
    if (!matched) {
        std::fprintf(stderr, "DLSSD: %ux%u -> %ux%u rejected; no exact optimal input reported\n",
            extent[0], extent[1], extent[2], extent[3]);
        for (size_t i = 0; i < attempts; ++i) {
            const auto& t = tried[i];
            std::fprintf(stderr, "DLSSD: tier %d optimal %ux%u, callback 0x%08x\n", t.tier, t.w, t.h, t.status);
        }
        return fail;
    }
    ctx->create.InDenoiseMode = NVSDK_NGX_DLSS_Denoise_Mode_DLUnified;
    ctx->create.InRoughnessMode = NVSDK_NGX_DLSS_Roughness_Mode_Packed;
    ctx->create.InUseHWDepth = NVSDK_NGX_DLSS_Depth_Type_HW;
    ctx->create.InWidth = extent[0]; ctx->create.InHeight = extent[1];
    ctx->create.InTargetWidth = extent[2]; ctx->create.InTargetHeight = extent[3];
    ctx->create.InFeatureCreateFlags = NVSDK_NGX_DLSS_Feature_Flags_IsHDR |
        NVSDK_NGX_DLSS_Feature_Flags_MVLowRes | NVSDK_NGX_DLSS_Feature_Flags_DepthInverted;
    *result = ctx.release();
    return NVSDK_NGX_Result_Success;
} catch (...) { return fail; }

PWE_API uint32_t pwe_rr_record(Context* ctx, VkCommandBuffer cmd, const Images* images, const Eval* eval) try {
    if (!ctx->feature) {
        const auto rc = NGX_VULKAN_CREATE_DLSSD_EXT1(ctx->device, cmd, 1, 1, &ctx->feature,
            ctx->params, &ctx->create);
        if (NVSDK_NGX_FAILED(rc) || !ctx->feature) return NVSDK_NGX_FAILED(rc) ? rc : fail;
    }
    auto c = images->color.resource(), d = images->depth.resource(), mv = images->velocity.resource();
    auto out = images->output.resource(true);
    auto diffuse = images->guides[0].resource(), spec = images->guides[1].resource();
    auto nr = images->guides[2].resource(), distance = images->guides[3].resource();
    NVSDK_NGX_VK_DLSSD_Eval_Params p{};
    p.pInColor = &c; p.pInDepth = &d; p.pInMotionVectors = &mv; p.pInOutput = &out;
    p.pInDiffuseAlbedo = &diffuse; p.pInSpecularAlbedo = &spec;
    p.pInNormals = &nr; p.pInRoughness = &nr;
    p.pInSpecularHitDistance = &distance;
    p.InRenderSubrectDimensions = {images->color.size[0], images->color.size[1]};
    p.InJitterOffsetX = eval->jitter[0]; p.InJitterOffsetY = eval->jitter[1];
    p.InMVScaleX = 1.f; p.InMVScaleY = 1.f; p.InPreExposure = 1.f; p.InExposureScale = 1.f;
    p.InFrameTimeDeltaInMsec = eval->ms; p.InReset = eval->reset;
    p.pInWorldToViewMatrix = const_cast<float*>(eval->view);
    p.pInViewToClipMatrix = const_cast<float*>(eval->proj);
    return NGX_VULKAN_EVALUATE_DLSSD_EXT(cmd, ctx->feature, ctx->params, &p);
} catch (...) { return fail; }

PWE_API void pwe_rr_close(Context* ctx) { delete ctx; }
