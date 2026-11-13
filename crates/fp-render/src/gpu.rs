//! Vulkan instance and device.

use crate::{Error, Result, VkContext};
use ash::vk;
use fp_core::sync::lock;
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc};
use std::ffi::CString;
use std::mem::ManuallyDrop;
use std::sync::Mutex;

/// FramePlayerX needs Vulkan 1.3 (dynamic rendering, synchronization2).
pub const API_VERSION: u32 = vk::API_VERSION_1_3;

/// Creates the instance and device. OpenXR runtimes need to do this
/// themselves (XR_KHR_vulkan_enable2), so creation is pluggable.
pub trait Creator {
    fn create_instance(
        &self,
        entry: &ash::Entry,
        info: &vk::InstanceCreateInfo,
    ) -> Result<vk::Instance>;
    fn physical_device(&self, instance: &ash::Instance) -> Result<vk::PhysicalDevice>;
    fn create_device(
        &self,
        instance: &ash::Instance,
        pdev: vk::PhysicalDevice,
        info: &vk::DeviceCreateInfo,
    ) -> Result<vk::Device>;
}

/// Plain Vulkan: first device that supports Vulkan 1.3 with a graphics queue.
pub struct DirectCreator;

impl Creator for DirectCreator {
    fn create_instance(
        &self,
        entry: &ash::Entry,
        info: &vk::InstanceCreateInfo,
    ) -> Result<vk::Instance> {
        // SAFETY: valid create info.
        unsafe { entry.create_instance(info, None) }
            .map(|i| i.handle())
            .ctx("vkCreateInstance")
    }
    fn physical_device(&self, instance: &ash::Instance) -> Result<vk::PhysicalDevice> {
        // SAFETY: valid instance.
        let devices = unsafe { instance.enumerate_physical_devices() }.ctx("enumerate devices")?;
        devices
            .into_iter()
            .find(|&d| {
                // SAFETY: valid physical device.
                let p = unsafe { instance.get_physical_device_properties(d) };
                p.api_version >= API_VERSION && graphics_queue(instance, d).is_some()
            })
            .ok_or_else(|| Error::Unsupported("no Vulkan 1.3 device with a graphics queue".into()))
    }
    fn create_device(
        &self,
        instance: &ash::Instance,
        pdev: vk::PhysicalDevice,
        info: &vk::DeviceCreateInfo,
    ) -> Result<vk::Device> {
        // SAFETY: valid create info for this physical device.
        unsafe { instance.create_device(pdev, info, None) }
            .map(|d| d.handle())
            .ctx("vkCreateDevice")
    }
}

pub(crate) fn graphics_queue(instance: &ash::Instance, pdev: vk::PhysicalDevice) -> Option<u32> {
    // SAFETY: valid physical device.
    unsafe { instance.get_physical_device_queue_family_properties(pdev) }
        .iter()
        .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .map(|i| i as u32)
}

/// What a device is asked to have beyond what the player needs. The default
/// asks for nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuOptions {
    /// Turn on `pipelineStatisticsQuery` where the device has it, for
    /// [`crate::Renderer::count_place_fragments`] (tests and tools).
    pub pipeline_statistics: bool,
}

pub struct Gpu {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub pdev: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub allocator: ManuallyDrop<Mutex<Allocator>>,
    pub device_name: String,
    pub driver: String,
    /// Turnip with what [`crate::q10c`] needs enabled: descriptor buffers,
    /// buffer device addresses, DRM format modifiers and 16-bit storage
    /// images.
    pub(crate) q10c_features: bool,
    /// Imports DMA-BUFs ([`crate::shared`]): the video decoder's buffers
    /// are read in place. With [`Gpu::q10c_features`] only.
    pub(crate) memory_fd: Option<ash::khr::external_memory_fd::Device>,
    /// The queue family owning memory the video decoder writes: foreign
    /// when the driver has `VK_EXT_queue_family_foreign`, else external.
    pub(crate) decoder_family: u32,
    /// The most a sampler may filter anisotropically (`max_sampler_anisotropy`),
    /// where the device has `samplerAnisotropy` and it was enabled; with
    /// `None` samplers must not ask for it.
    pub max_anisotropy: Option<f32>,
    /// The device can sample ASTC LDR textures and the feature is enabled:
    /// place packs' ASTC data is uploaded as it is (see
    /// [`crate::env::pack::astc_on_gpu`]).
    pub astc: bool,
    /// The sample counts a colour attachment and a depth attachment can both
    /// have (`framebuffer_color_sample_counts` and
    /// `framebuffer_depth_sample_counts`): what multisampling the eye passes
    /// may ask for ([`pick_samples`]).
    pub framebuffer_samples: vk::SampleCountFlags,
    /// Pipeline statistics queries are enabled (see [`GpuOptions`]).
    pub pipeline_statistics: bool,
}

/// Device extensions [`crate::q10c`] needs.
const Q10C_EXTENSIONS: [&std::ffi::CStr; 2] = [
    ash::ext::image_drm_format_modifier::NAME,
    ash::ext::descriptor_buffer::NAME,
];

/// Device extensions importing the decoder's DMA-BUFs needs, besides
/// [`Q10C_EXTENSIONS`].
const SHARED_EXTENSIONS: [&std::ffi::CStr; 2] = [
    ash::khr::external_memory_fd::NAME,
    ash::ext::external_memory_dma_buf::NAME,
];

/// Whether `pdev` has device extension `name`.
fn has_extension(
    instance: &ash::Instance,
    pdev: vk::PhysicalDevice,
    name: &std::ffi::CStr,
) -> bool {
    // SAFETY: valid physical device.
    unsafe { instance.enumerate_device_extension_properties(pdev) }
        .unwrap_or_default()
        .iter()
        .any(|e| e.extension_name_as_c_str() == Ok(name))
}

/// Whether `pdev` is Turnip with everything [`crate::q10c`] needs.
fn q10c_supported(
    instance: &ash::Instance,
    pdev: vk::PhysicalDevice,
    driver: vk::DriverId,
) -> bool {
    if driver != vk::DriverId::MESA_TURNIP {
        return false;
    }
    // SAFETY: valid physical device.
    let exts = unsafe { instance.enumerate_device_extension_properties(pdev) }.unwrap_or_default();
    let has = |n: &std::ffi::CStr| exts.iter().any(|e| e.extension_name_as_c_str() == Ok(n));
    if !Q10C_EXTENSIONS.iter().all(|n| has(n)) {
        return false;
    }
    let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut fdb = vk::PhysicalDeviceDescriptorBufferFeaturesEXT::default();
    let mut f2 = vk::PhysicalDeviceFeatures2::default()
        .push_next(&mut f12)
        .push_next(&mut fdb);
    // SAFETY: valid physical device; the chain holds initialised structs.
    unsafe { instance.get_physical_device_features2(pdev, &mut f2) };
    let storage_formats = f2.features.shader_storage_image_extended_formats;
    f12.buffer_device_address == vk::TRUE
        && fdb.descriptor_buffer == vk::TRUE
        && storage_formats == vk::TRUE
}

/// Depth formats in order of preference: 32-bit float, then 24-bit, then
/// 16-bit. Vulkan guarantees one of the first two and the last everywhere.
const DEPTH_FORMATS: [vk::Format; 3] = [
    vk::Format::D32_SFLOAT,
    vk::Format::X8_D24_UNORM_PACK32,
    vk::Format::D16_UNORM,
];

/// The first of [`DEPTH_FORMATS`] that `supported` accepts; 16-bit, which
/// every device can attach, when none is.
pub(crate) fn pick_depth_format(supported: impl Fn(vk::Format) -> bool) -> vk::Format {
    DEPTH_FORMATS
        .into_iter()
        .find(|&f| supported(f))
        .unwrap_or(vk::Format::D16_UNORM)
}

/// The depth format that has `bits` bits of depth: 16, 24 or 32 (a
/// `FRAMEPLAYERX_DEPTH` value). Anything else is `None`.
pub(crate) fn depth_format_of_bits(bits: u32) -> Option<vk::Format> {
    match bits {
        16 => Some(vk::Format::D16_UNORM),
        24 => Some(vk::Format::X8_D24_UNORM_PACK32),
        32 => Some(vk::Format::D32_SFLOAT),
        _ => None,
    }
}

/// The depth format asked for with `wanted` bits (see [`depth_format_of_bits`])
/// when `supported` accepts it, else what [`pick_depth_format`] picks: a
/// device that cannot do what a knob asks for still draws.
pub(crate) fn pick_depth_format_wanted(
    wanted: Option<u32>,
    supported: impl Fn(vk::Format) -> bool,
) -> vk::Format {
    wanted
        .and_then(depth_format_of_bits)
        .filter(|&f| supported(f))
        .unwrap_or_else(|| pick_depth_format(supported))
}

/// The anisotropy limit to use: the device's, when it has the feature and a
/// limit above 1 (a limit of 1 filters nothing extra).
fn anisotropy_limit(supported: bool, max: f32) -> Option<f32> {
    (supported && max > 1.0).then_some(max)
}

/// Whether place packs' ASTC textures can be used as they are: the device
/// has `textureCompressionASTC_LDR` and every ASTC LDR format a pack may use
/// can be sampled with linear filtering in optimal tiling (`features_of` says
/// what a format can do). `env` is `FRAMEPLAYERX_ASTC`, where 0 turns it off
/// (the textures are then decoded on the CPU, which is how to compare the
/// two paths).
fn astc_usable(
    feature: bool,
    env: Option<&str>,
    mut features_of: impl FnMut(vk::Format) -> vk::FormatFeatureFlags,
) -> bool {
    let need =
        vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR;
    feature
        && env != Some("0")
        && crate::env::ktx2::ASTC_BLOCKS.iter().all(|&block| {
            [false, true].into_iter().all(|srgb| {
                crate::env::ktx2::astc_vk_format(block, srgb)
                    .is_some_and(|f| features_of(vk::Format::from_raw(f as i32)).contains(need))
            })
        })
}

/// The sample count to render the eyes with: `wanted` (1, 2, 4 or 8) when
/// `supported` has it, else the nearest lower one it has (1 at the least,
/// which every device can do). Anything that is not a sample count is 1.
pub fn pick_samples(wanted: u32, supported: vk::SampleCountFlags) -> vk::SampleCountFlags {
    [
        (8, vk::SampleCountFlags::TYPE_8),
        (4, vk::SampleCountFlags::TYPE_4),
        (2, vk::SampleCountFlags::TYPE_2),
    ]
    .into_iter()
    .find(|&(n, flag)| n <= wanted && supported.contains(flag))
    .map_or(vk::SampleCountFlags::TYPE_1, |(_, flag)| flag)
}

/// How many samples a pixel `flags` is (1 for anything but 2, 4 and 8).
pub fn sample_count(flags: vk::SampleCountFlags) -> u32 {
    if flags == vk::SampleCountFlags::TYPE_8 {
        8
    } else if flags == vk::SampleCountFlags::TYPE_4 {
        4
    } else if flags == vk::SampleCountFlags::TYPE_2 {
        2
    } else {
        1
    }
}

impl Gpu {
    pub fn new(creator: &dyn Creator, app_name: &str) -> Result<Gpu> {
        Gpu::with_options(creator, app_name, GpuOptions::default())
    }

    /// Like [`Gpu::new`], with `options` on top of what the player needs.
    pub fn with_options(creator: &dyn Creator, app_name: &str, options: GpuOptions) -> Result<Gpu> {
        // SAFETY: loads the system Vulkan loader.
        let entry = unsafe { ash::Entry::load() }.map_err(|e| Error::Load(e.to_string()))?;
        let name = CString::new(app_name).unwrap_or_default();
        let app = vk::ApplicationInfo::default()
            .application_name(&name)
            .engine_name(&name)
            .api_version(API_VERSION);
        let ici = vk::InstanceCreateInfo::default().application_info(&app);
        let raw = creator.create_instance(&entry, &ici)?;
        // SAFETY: raw is a live instance created with this entry.
        let instance = unsafe { ash::Instance::load(entry.static_fn(), raw) };
        let pdev = creator.physical_device(&instance)?;
        let mut driver_props = vk::PhysicalDeviceDriverProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver_props);
        // SAFETY: valid physical device.
        unsafe { instance.get_physical_device_properties2(pdev, &mut props2) };
        let props = props2.properties;
        if props.api_version < API_VERSION {
            return Err(Error::Unsupported("GPU does not support Vulkan 1.3".into()));
        }
        let device_name = props
            .device_name_as_c_str()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        let driver = format!(
            "{} {}",
            driver_props
                .driver_name_as_c_str()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default(),
            driver_props
                .driver_info_as_c_str()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let queue_family = graphics_queue(&instance, pdev)
            .ok_or_else(|| Error::Unsupported("no graphics queue".into()))?;
        let prio = [1.0f32];
        let qci = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&prio)];
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
            .dynamic_rendering(true)
            .synchronization2(true);
        let q10c_features = q10c_supported(&instance, pdev, driver_props.driver_id);
        let shared = q10c_features
            && SHARED_EXTENSIONS
                .iter()
                .all(|n| has_extension(&instance, pdev, n));
        let foreign =
            shared && has_extension(&instance, pdev, ash::ext::queue_family_foreign::NAME);
        let mut q10c_exts: Vec<_> = Q10C_EXTENSIONS.map(std::ffi::CStr::as_ptr).into();
        if shared {
            q10c_exts.extend(SHARED_EXTENSIONS.map(std::ffi::CStr::as_ptr));
        }
        if foreign {
            q10c_exts.push(ash::ext::queue_family_foreign::NAME.as_ptr());
        }
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default().buffer_device_address(true);
        let mut fdb =
            vk::PhysicalDeviceDescriptorBufferFeaturesEXT::default().descriptor_buffer(true);
        // SAFETY: valid physical device.
        let supported = unsafe { instance.get_physical_device_features(pdev) };
        let max_anisotropy = anisotropy_limit(
            supported.sampler_anisotropy == vk::TRUE,
            props.limits.max_sampler_anisotropy,
        );
        let astc = astc_usable(
            supported.texture_compression_astc_ldr == vk::TRUE,
            std::env::var("FRAMEPLAYERX_ASTC").ok().as_deref(),
            |f| {
                // SAFETY: valid physical device.
                unsafe { instance.get_physical_device_format_properties(pdev, f) }
                    .optimal_tiling_features
            },
        );
        let pipeline_statistics =
            options.pipeline_statistics && supported.pipeline_statistics_query == vk::TRUE;
        // Always passed: the video's anisotropic sampler needs
        // `samplerAnisotropy`, the Q10C converter the extended storage formats;
        // ASTC only where the device has it.
        let features = vk::PhysicalDeviceFeatures::default()
            .sampler_anisotropy(max_anisotropy.is_some())
            .shader_storage_image_extended_formats(q10c_features)
            .texture_compression_astc_ldr(astc)
            .pipeline_statistics_query(pipeline_statistics);
        let mut dci = vk::DeviceCreateInfo::default()
            .queue_create_infos(&qci)
            .enabled_features(&features)
            .push_next(&mut f13);
        if q10c_features {
            dci = dci
                .enabled_extension_names(&q10c_exts)
                .push_next(&mut f12)
                .push_next(&mut fdb);
        }
        let raw_dev = creator.create_device(&instance, pdev, &dci)?;
        // SAFETY: raw_dev was created from this instance.
        let device = unsafe { ash::Device::load(instance.fp_v1_0(), raw_dev) };
        // SAFETY: queue 0 of the family was requested.
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device: pdev,
            debug_settings: Default::default(),
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        })
        .map_err(|e| Error::Alloc(e.to_string()))?;
        log::info!(
            "GPU: {device_name} ({driver}); place ASTC textures {}",
            if astc {
                "are sampled as they are"
            } else {
                "are decoded on the CPU"
            }
        );
        crate::env::pack::set_astc_on_gpu(astc);
        let memory_fd =
            shared.then(|| ash::khr::external_memory_fd::Device::new(&instance, &device));
        if shared {
            log::info!("GPU: imports the video decoder's buffers (zero copy)");
        }
        Ok(Gpu {
            entry,
            instance,
            pdev,
            device,
            queue,
            queue_family,
            allocator: ManuallyDrop::new(Mutex::new(allocator)),
            device_name,
            driver,
            q10c_features,
            memory_fd,
            decoder_family: if foreign {
                vk::QUEUE_FAMILY_FOREIGN_EXT
            } else {
                vk::QUEUE_FAMILY_EXTERNAL
            },
            max_anisotropy,
            astc,
            framebuffer_samples: props.limits.framebuffer_color_sample_counts
                & props.limits.framebuffer_depth_sample_counts,
            pipeline_statistics,
        })
    }

    /// Headless device for tests and tools.
    pub fn headless() -> Result<Gpu> {
        Gpu::new(&DirectCreator, "frameplayerx")
    }

    /// Headless device with `options`.
    pub fn headless_with(options: GpuOptions) -> Result<Gpu> {
        Gpu::with_options(&DirectCreator, "frameplayerx", options)
    }

    /// The format of the depth attachment the eye passes use: the first of
    /// 32-bit float, 24-bit and 16-bit that can be a depth attachment in
    /// optimal tiling.
    pub fn depth_format(&self) -> vk::Format {
        self.depth_format_for(None)
    }

    /// Like [`Gpu::depth_format`], but with `wanted` bits of depth (16, 24 or
    /// 32: `FRAMEPLAYERX_DEPTH`) when the device can attach that format; the
    /// default pick otherwise.
    pub fn depth_format_for(&self, wanted: Option<u32>) -> vk::Format {
        let format = pick_depth_format_wanted(wanted, |f| {
            // SAFETY: valid physical device.
            let p = unsafe {
                self.instance
                    .get_physical_device_format_properties(self.pdev, f)
            };
            p.optimal_tiling_features
                .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
        });
        let asked = wanted.and_then(depth_format_of_bits);
        match asked {
            Some(a) if a != format => log::warn!(
                "GPU: asked for a {a:?} depth buffer, which this device cannot attach, so the eye passes use {format:?}"
            ),
            _ => log::info!("GPU: eye passes use a {format:?} depth buffer"),
        }
        format
    }

    pub(crate) fn alloc(&self) -> std::sync::MutexGuard<'_, Allocator> {
        lock(&self.allocator)
    }

    pub fn wait_idle(&self) {
        // SAFETY: valid device.
        let _ = unsafe { self.device.device_wait_idle() };
    }

    pub(crate) fn shader(&self, spv: &[u8]) -> Result<vk::ShaderModule> {
        let words: Vec<u32> = spv
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        // SAFETY: valid SPIR-V produced by naga at build time.
        unsafe {
            self.device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        }
        .ctx("create shader")
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        self.wait_idle();
        // SAFETY: all resources were destroyed by their owners; the
        // allocator must go before the device, the device before the instance.
        unsafe {
            ManuallyDrop::drop(&mut self.allocator);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anisotropy_needs_the_feature_and_a_limit_above_one() {
        assert_eq!(anisotropy_limit(true, 16.0), Some(16.0));
        assert_eq!(anisotropy_limit(false, 16.0), None);
        assert_eq!(anisotropy_limit(true, 1.0), None);
        assert_eq!(anisotropy_limit(true, 0.0), None);
    }

    #[test]
    fn astc_needs_the_feature_every_format_and_no_switch_off() {
        let all = vk::FormatFeatureFlags::SAMPLED_IMAGE
            | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR;
        assert!(astc_usable(true, None, |_| all));
        assert!(astc_usable(true, Some("1"), |_| all));
        assert!(!astc_usable(true, Some("0"), |_| all));
        assert!(!astc_usable(false, None, |_| all));
        // One format that cannot be filtered, or sampled, is enough.
        let no_linear = |f: vk::Format| {
            if f == vk::Format::ASTC_10X10_SRGB_BLOCK {
                vk::FormatFeatureFlags::SAMPLED_IMAGE
            } else {
                all
            }
        };
        assert!(!astc_usable(true, None, no_linear));
        assert!(!astc_usable(true, None, |_| vk::FormatFeatureFlags::empty()));
        let mut asked = Vec::new();
        astc_usable(true, None, |f| {
            asked.push(f);
            all
        });
        assert_eq!(asked.len(), 28);
        assert!(asked.contains(&vk::Format::ASTC_12X12_UNORM_BLOCK));
    }

    #[test]
    fn pick_depth_format_prefers_32_bit_float() {
        assert_eq!(pick_depth_format(|_| true), vk::Format::D32_SFLOAT);
    }

    #[test]
    fn pick_depth_format_falls_back_in_order() {
        let no_32 = pick_depth_format(|f| f != vk::Format::D32_SFLOAT);
        assert_eq!(no_32, vk::Format::X8_D24_UNORM_PACK32);
        let only_16 = pick_depth_format(|f| f == vk::Format::D16_UNORM);
        assert_eq!(only_16, vk::Format::D16_UNORM);
        // None offered: 16-bit, which every device can attach.
        let none = pick_depth_format(|_| false);
        assert_eq!(none, vk::Format::D16_UNORM);
    }

    #[test]
    fn a_depth_size_asked_for_is_used_when_the_device_has_it() {
        for (bits, f) in [
            (16, vk::Format::D16_UNORM),
            (24, vk::Format::X8_D24_UNORM_PACK32),
            (32, vk::Format::D32_SFLOAT),
        ] {
            assert_eq!(pick_depth_format_wanted(Some(bits), |_| true), f);
        }
        // Not asked for: the default.
        assert_eq!(
            pick_depth_format_wanted(None, |_| true),
            vk::Format::D32_SFLOAT
        );
    }

    #[test]
    fn a_depth_size_the_device_lacks_falls_back_to_the_default_pick() {
        let no_24 = |f: vk::Format| f != vk::Format::X8_D24_UNORM_PACK32;
        assert_eq!(
            pick_depth_format_wanted(Some(24), no_24),
            vk::Format::D32_SFLOAT
        );
        let only_16 = |f: vk::Format| f == vk::Format::D16_UNORM;
        assert_eq!(
            pick_depth_format_wanted(Some(32), only_16),
            vk::Format::D16_UNORM
        );
        // Not a depth size: the default.
        assert_eq!(
            pick_depth_format_wanted(Some(8), |_| true),
            vk::Format::D32_SFLOAT
        );
        assert_eq!(
            pick_depth_format_wanted(Some(16), |_| false),
            vk::Format::D16_UNORM
        );
    }

    #[test]
    fn samples_are_what_was_asked_when_the_device_has_them() {
        let all = vk::SampleCountFlags::TYPE_1
            | vk::SampleCountFlags::TYPE_2
            | vk::SampleCountFlags::TYPE_4;
        assert_eq!(pick_samples(4, all), vk::SampleCountFlags::TYPE_4);
        assert_eq!(pick_samples(1, all), vk::SampleCountFlags::TYPE_1);
        assert_eq!(pick_samples(0, all), vk::SampleCountFlags::TYPE_1);
        // 8 is not there: the nearest lower one is.
        assert_eq!(pick_samples(8, all), vk::SampleCountFlags::TYPE_4);
        // 4 is not there: 2 is, then 1.
        let no_4 = vk::SampleCountFlags::TYPE_1 | vk::SampleCountFlags::TYPE_2;
        assert_eq!(pick_samples(4, no_4), vk::SampleCountFlags::TYPE_2);
        assert_eq!(
            pick_samples(4, vk::SampleCountFlags::TYPE_1),
            vk::SampleCountFlags::TYPE_1
        );
        assert_eq!(
            pick_samples(4, vk::SampleCountFlags::empty()),
            vk::SampleCountFlags::TYPE_1
        );
    }

    #[test]
    fn a_sample_flag_counts_back_to_its_number() {
        for n in [1, 2, 4, 8] {
            assert_eq!(
                sample_count(pick_samples(n, vk::SampleCountFlags::from_raw(0xF))),
                n
            );
        }
    }
}
