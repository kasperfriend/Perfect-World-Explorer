//! A Vulkan SDK can only use features/extensions enabled at creation. Never
//! graft them onto an existing wgpu device or advertise them from a probe alone.
use ash::vk;
use std::ffi::{CStr, CString};
use std::sync::Mutex;
use wgpu::hal::{api::Vulkan, vulkan::Instance as HalInstance};

use super::{Request, R};

// HAL retains extension names for the lifetime of its device and instance
// and requires &'static CStr. Intern only supported names, with a hard cap.
static EXTENSIONS: Mutex<Vec<&'static CStr>> = Mutex::new(Vec::new());

fn extend(
    enabled: &mut Vec<&'static CStr>,
    available: &[vk::ExtensionProperties],
    requested: &[CString],
) -> R<()> {
    for ext in requested {
        if enabled.contains(&ext.as_c_str()) {
            continue;
        }
        if !available.iter().any(|p| {
            // Vulkan promises a NUL-terminated name in each property.
            unsafe { CStr::from_ptr(p.extension_name.as_ptr()) == ext.as_c_str() }
        }) {
            return Err(format!(
                "required Vulkan extension {} is absent",
                ext.to_string_lossy()
            ));
        }
        let mut names = EXTENSIONS.lock().unwrap();
        let name = if let Some(&name) = names.iter().find(|&&name| name == ext.as_c_str()) {
            name
        } else {
            if names.len() >= 96 {
                return Err("ray SDK extension registry is full".into());
            }
            let name: &'static CStr = Box::leak(ext.clone().into_boxed_c_str());
            names.push(name);
            name
        };
        enabled.push(name);
    }
    Ok(())
}

fn vkerr(step: &str) -> impl FnOnce(vk::Result) -> String + '_ {
    move |e| format!("{step}: {e:?}")
}

pub(super) fn instance(required: &[CString], flags: wgpu::InstanceFlags) -> R<wgpu::Instance> {
    // SAFETY: create Vulkan from this very Entry, with at least the desired
    // HAL extensions, and transfer sole instance ownership to HAL.
    unsafe {
        let entry = ash::Entry::load().map_err(|e| e.to_string())?;
        let version = entry
            .try_enumerate_instance_version()
            .map_err(vkerr("Vulkan version"))?
            .unwrap_or(vk::API_VERSION_1_0)
            .min(vk::API_VERSION_1_3);
        if version < vk::API_VERSION_1_2 {
            return Err("ray reconstruction requires Vulkan 1.2".into());
        }
        let available = entry
            .enumerate_instance_extension_properties(None)
            .map_err(vkerr("instance extensions"))?;
        let mut extensions =
            HalInstance::desired_extensions(&entry, version, flags).map_err(|e| e.to_string())?;
        extend(&mut extensions, &available, required)?;
        let names: Vec<_> = extensions.iter().map(|ext| ext.as_ptr()).collect();
        let app = vk::ApplicationInfo::builder().api_version(version);
        let mut layers = Vec::new();
        if flags.contains(wgpu::InstanceFlags::VALIDATION) {
            let layer = CStr::from_bytes_with_nul(b"VK_LAYER_KHRONOS_validation\0").unwrap();
            if entry
                .enumerate_instance_layer_properties()
                .map_err(vkerr("instance layers"))?
                .iter()
                .any(|p| CStr::from_ptr(p.layer_name.as_ptr()) == layer)
            {
                layers.push(layer.as_ptr());
                eprintln!("Lighter Vulkan instance: VK_LAYER_KHRONOS_validation enabled");
            } else if std::env::var("WGPU_VALIDATION").as_deref() == Ok("1") {
                eprintln!("Lighter Vulkan instance: VK_LAYER_KHRONOS_validation requested but unavailable");
            }
        }
        let raw = entry
            .create_instance(
                &vk::InstanceCreateInfo::builder()
                    .application_info(&app)
                    .enabled_extension_names(&names)
                    .enabled_layer_names(&layers),
                None,
            )
            .map_err(vkerr("create reconstruction instance"))?;
        // Keep our loader reference until error cleanup has destroyed the
        // raw instance. An ash::Instance alone does not own the loader DLL.
        let hal = HalInstance::from_raw(
            entry.clone(),
            raw.clone(),
            version,
            0,
            None,
            extensions,
            flags,
            false,
            None,
        )
        .map_err(|e| {
            raw.destroy_instance(None);
            e.to_string()
        })?;
        Ok(wgpu::Instance::from_hal::<Vulkan>(hal))
    }
}

pub(super) fn device(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
    request: &Request,
) -> R<(wgpu::Device, wgpu::Queue)> {
    // Do not use another device, hidden queue, or a separately selected GPU;
    // the swapchain, ray buffers, SDK and compositor use the same queue.
    let features = desc.required_features;
    // Unlike request_device, the raw HAL import relies on the caller to
    // supply a subset of the adapter's features/limits. Check before any FFI
    // or SDK extension discovery, not after constructing an owned VkDevice.
    check_descriptor(desc, adapter.features(), &adapter.limits())?;
    let hal = unsafe {
        adapter.as_hal::<Vulkan, _, _>(|a| {
            let a = a.ok_or("adapter is not Vulkan")?;
            let shared = a.shared_instance();
            let instance = shared.raw_instance();
            let physical = a.raw_physical_device();
            let props = instance.get_physical_device_properties(physical);
            if props.api_version < vk::API_VERSION_1_2 {
                return Err("ray reconstruction requires a Vulkan 1.2 device".into());
            }
            let required = request.device_extensions(shared, physical, &props)?;
            let available = instance
                .enumerate_device_extension_properties(physical)
                .map_err(vkerr("device extensions"))?;
            let mut extensions = a.required_device_extensions(features);
            extend(&mut extensions, &available, &required)?;
            let supports = |name: &CStr| {
                available
                    .iter()
                    .any(|ext| CStr::from_ptr(ext.extension_name.as_ptr()) == name)
            };
            // A fresh query chain is not a device-create chain: queried structs
            // retain pNext links. Copy only admitted feature values below.
            let mut queried_fp16 = vk::PhysicalDeviceShaderFloat16Int8Features::default();
            let mut queried_16bit = vk::PhysicalDevice16BitStorageFeatures::default();
            let mut queried_sync2 = vk::PhysicalDeviceSynchronization2Features::default();
            let mut queried_subgroup = vk::PhysicalDeviceSubgroupSizeControlFeatures::default();
            let mut queried_coherent = vk::PhysicalDeviceCoherentMemoryFeaturesAMD::default();
            let mut queried_bda = vk::PhysicalDeviceBufferDeviceAddressFeatures::default();
            let mut queried = vk::PhysicalDeviceFeatures2::builder().push_next(&mut queried_bda);
            let need_fsr = request.is_fsr();
            let subgroup_ext = vk::ExtSubgroupSizeControlFn::name();
            let sync2_ext = vk::KhrSynchronization2Fn::name();
            let fp16_ext = vk::KhrShaderFloat16Int8Fn::name();
            let coherent_ext = vk::AmdDeviceCoherentMemoryFn::name();
            let use_fp16 = (need_fsr || extensions.contains(&fp16_ext)) && supports(fp16_ext);
            let use_sync2 = (need_fsr || extensions.contains(&sync2_ext)) && supports(sync2_ext);
            let use_subgroup =
                (need_fsr || extensions.contains(&subgroup_ext)) && supports(subgroup_ext);
            let use_coherent = need_fsr && supports(coherent_ext);
            for (use_it, name) in [
                (use_fp16, fp16_ext),
                (use_sync2, sync2_ext),
                (use_subgroup, subgroup_ext),
                (use_coherent, coherent_ext),
            ] {
                if use_it && !extensions.contains(&name) {
                    extensions.push(name);
                }
            }
            if need_fsr {
                // The backend tests physical extension presence for these
                // capabilities. Match it on the logical device as well.
                for name in [
                    vk::KhrGetMemoryRequirements2Fn::name(),
                    vk::KhrDedicatedAllocationFn::name(),
                    vk::AmdBufferMarkerFn::name(),
                ] {
                    if supports(name) && !extensions.contains(&name) {
                        extensions.push(name);
                    }
                }
            }
            if use_fp16 {
                queried = queried
                    .push_next(&mut queried_fp16)
                    .push_next(&mut queried_16bit);
            }
            if use_sync2 {
                queried = queried.push_next(&mut queried_sync2);
            }
            if use_subgroup {
                queried = queried.push_next(&mut queried_subgroup);
            }
            if use_coherent {
                queried = queried.push_next(&mut queried_coherent);
            }
            instance.get_physical_device_features2(physical, &mut queried);
            let base = queried.features;
            if base.shader_storage_image_extended_formats == 0 {
                return Err("ray motion and SDK storage image formats are unsupported".into());
            }
            let bda_ext = vk::KhrBufferDeviceAddressFn::name();
            if extensions.contains(&bda_ext) && queried_bda.buffer_device_address == 0 {
                return Err(
                    "the requested Vulkan buffer-device-address feature is unsupported".into(),
                );
            }
            if need_fsr {
                let unorm = instance.get_physical_device_format_properties(
                    physical,
                    vk::Format::R16G16B16A16_UNORM,
                );
                if !unorm.optimal_tiling_features.contains(
                    vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::STORAGE_IMAGE,
                ) {
                    return Err("FSR requires sampled and writable RGBA16 UNORM".into());
                }
            }
            let mut fp16 = vk::PhysicalDeviceShaderFloat16Int8Features::builder()
                .shader_float16(queried_fp16.shader_float16 != 0)
                .build();
            let mut storage = vk::PhysicalDevice16BitStorageFeatures::builder()
                .storage_buffer16_bit_access(queried_16bit.storage_buffer16_bit_access != 0)
                .uniform_and_storage_buffer16_bit_access(
                    queried_16bit.uniform_and_storage_buffer16_bit_access != 0,
                )
                .build();
            let mut sync2 = vk::PhysicalDeviceSynchronization2Features::builder()
                .synchronization2(queried_sync2.synchronization2 != 0)
                .build();
            let mut subgroup = vk::PhysicalDeviceSubgroupSizeControlFeatures::builder()
                .subgroup_size_control(queried_subgroup.subgroup_size_control != 0)
                .compute_full_subgroups(queried_subgroup.compute_full_subgroups != 0)
                .build();
            let mut coherent = vk::PhysicalDeviceCoherentMemoryFeaturesAMD::builder()
                .device_coherent_memory(queried_coherent.device_coherent_memory != 0)
                .build();
            let mut enabled = a.physical_device_features(&extensions, features);
            let family = 0; // Same queue family selected by this wgpu HAL's surface support/open.
            let queues = instance.get_physical_device_queue_family_properties(physical);
            if !queues.first().is_some_and(|q| {
                q.queue_count > 0
                    && q.queue_flags
                        .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
            }) {
                return Err("the wgpu Vulkan family lacks graphics and compute queues".into());
            }
            let queue = [vk::DeviceQueueCreateInfo::builder()
                .queue_family_index(family)
                .queue_priorities(&[1.])
                .build()];
            let names: Vec<_> = extensions.iter().map(|ext| ext.as_ptr()).collect();
            let mut info = enabled.add_to_device_create_builder(
                vk::DeviceCreateInfo::builder()
                    .queue_create_infos(&queue)
                    .enabled_extension_names(&names),
            );
            // Keep every required HAL bit; add only the queried SDK capabilities.
            let mut core = *info.p_enabled_features;
            core.shader_storage_image_extended_formats = vk::TRUE;
            core.shader_storage_image_read_without_format =
                base.shader_storage_image_read_without_format;
            core.shader_storage_image_write_without_format =
                base.shader_storage_image_write_without_format;
            core.shader_int16 = base.shader_int16;
            info.p_enabled_features = &core;
            if use_sync2 {
                info = info.push_next(&mut sync2);
            }
            if use_subgroup {
                info = info.push_next(&mut subgroup);
            }
            if use_coherent {
                info = info.push_next(&mut coherent);
            }
            if use_fp16 && !features.contains(wgpu::Features::SHADER_F16) {
                info = info.push_next(&mut fp16).push_next(&mut storage);
            }
            let raw = instance
                .create_device(physical, &info, None)
                .map_err(vkerr("create reconstruction device"))?;
            a.device_from_raw(raw.clone(), true, &extensions, features, family, 0)
                .map_err(|e| {
                    raw.destroy_device(None);
                    e.to_string()
                })
        })?
    };
    // `hal` came from this adapter with the required features and HAL defaults.
    unsafe { adapter.create_device_from_hal::<Vulkan>(hal, desc, None) }.map_err(|e| e.to_string())
}

fn check_descriptor(
    desc: &wgpu::DeviceDescriptor<'_>,
    features: wgpu::Features,
    limits: &wgpu::Limits,
) -> R<()> {
    if !features.contains(desc.required_features) {
        return Err("raw reconstruction device requested unsupported wgpu features".into());
    }
    if !desc.required_limits.check_limits(limits) {
        return Err("raw reconstruction device requested unsupported wgpu limits".into());
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn rejects_excessive_raw_device_features_and_limits() {
    let limits = wgpu::Limits::downlevel_defaults();
    let mut desc = wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TIMESTAMP_QUERY,
        required_limits: limits.clone(),
        ..Default::default()
    };
    assert!(check_descriptor(&desc, wgpu::Features::empty(), &limits).is_err());
    assert!(check_descriptor(&desc, wgpu::Features::TIMESTAMP_QUERY, &limits).is_ok());
    desc.required_limits.max_texture_dimension_2d += 1;
    assert!(check_descriptor(&desc, wgpu::Features::TIMESTAMP_QUERY, &limits).is_err());
}
