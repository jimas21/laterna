//! OpenXR instance and system, and Vulkan creation through the runtime.

use crate::{Error, Result, XrContextExt};
use ash::vk::{self, Handle};
use fp_xr_loader::{self as loader, RuntimeManifest};
use openxr as xr;

/// Defines `/interaction_profiles/valve/frame_controller_valve`. Not in the
/// Khronos registry yet; SteamVR on the Steam Frame provides it.
pub const FRAME_CONTROLLER_EXTENSION: &[u8] = b"XR_VALVE_frame_controller_interaction\0";

/// How the OpenXR instance is created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XrOptions {
    /// Enable only what a frame loop needs (`XR_KHR_vulkan_enable2` and the
    /// Steam Frame controller profile), none of the optional extensions
    /// (hand and eye tracking, refresh rate, layer types). For finding out
    /// whether one of them changes how the runtime paces the app.
    pub minimal_extensions: bool,
}

/// The per-eye image sizes the runtime offers, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EyeSizes {
    /// What the runtime recommends: SteamVR's own choice, from its
    /// resolution settings.
    pub recommended: (u32, u32),
    /// The most it allows.
    pub max: (u32, u32),
}

/// The eye image size for a render `scale`: the recommended size times the
/// scale, each side a multiple of 8 within 64 and the runtime's maximum. A
/// scale of 1 (and one that is not a number) gives the recommended size
/// unchanged; others are limited to 0.5 to 2.
pub fn scaled_eye_size(sizes: &EyeSizes, scale: f32) -> (u32, u32) {
    let k = if scale.is_finite() {
        scale.clamp(0.5, 2.0)
    } else {
        1.0
    };
    if k == 1.0 {
        return sizes.recommended;
    }
    let side = |recommended: u32, max: u32| {
        let max = if max == 0 { u32::MAX } else { max.max(64) };
        let rounded = (recommended as f32 * k / 8.0).round() as u32 * 8;
        rounded.clamp(64, max)
    };
    (
        side(sizes.recommended.0, sizes.max.0),
        side(sizes.recommended.1, sizes.max.1),
    )
}

/// The names of the extensions in `set` that FramePlayerX can ask for.
fn extension_names(set: &xr::ExtensionSet) -> Vec<String> {
    let known = [
        ("XR_KHR_vulkan_enable2", set.khr_vulkan_enable2),
        ("XR_FB_display_refresh_rate", set.fb_display_refresh_rate),
        ("XR_EXT_eye_gaze_interaction", set.ext_eye_gaze_interaction),
        ("XR_EXT_hand_interaction", set.ext_hand_interaction),
        ("XR_EXT_hand_tracking", set.ext_hand_tracking),
        (
            "XR_KHR_composition_layer_cylinder",
            set.khr_composition_layer_cylinder,
        ),
        (
            "XR_KHR_composition_layer_equirect2",
            set.khr_composition_layer_equirect2,
        ),
        ("XR_KHR_visibility_mask", set.khr_visibility_mask),
    ];
    let mut names = Vec::new();
    for (name, on) in known {
        if on {
            names.push(name.to_string());
        }
    }
    for other in &set.other {
        let name = String::from_utf8_lossy(other);
        names.push(name.trim_end_matches('\0').to_string());
    }
    names
}

pub struct XrContext {
    pub instance: xr::Instance,
    pub system: xr::SystemId,
    pub manifest: RuntimeManifest,
    pub runtime: String,
    pub system_name: String,
    /// Extensions we enabled.
    pub enabled: xr::ExtensionSet,
    pub blend_modes: Vec<xr::EnvironmentBlendMode>,
}

impl XrContext {
    /// Loads the active runtime and creates an instance with the extensions
    /// FramePlayerX uses when they are available (or only the ones it can't
    /// do without, with [`XrOptions::minimal_extensions`]).
    pub fn new(app_name: &str, opts: XrOptions) -> Result<XrContext> {
        let manifest = loader::find_active_runtime(&loader::SearchEnv::from_process())
            .map_err(Error::Runtime)?;
        let loaded = loader::load_runtime(&manifest).map_err(Error::Runtime)?;
        let entry = loaded.entry;
        let avail = entry.enumerate_extensions().ctx("enumerate extensions")?;
        if !avail.khr_vulkan_enable2 {
            return Err(Error::Runtime(
                "the OpenXR runtime lacks XR_KHR_vulkan_enable2".into(),
            ));
        }
        let mut e = xr::ExtensionSet::default();
        e.khr_vulkan_enable2 = true;
        if !opts.minimal_extensions {
            e.fb_display_refresh_rate = avail.fb_display_refresh_rate;
            e.ext_eye_gaze_interaction = avail.ext_eye_gaze_interaction;
            e.ext_hand_interaction = avail.ext_hand_interaction;
            e.ext_hand_tracking = avail.ext_hand_tracking;
            e.khr_composition_layer_cylinder = avail.khr_composition_layer_cylinder;
            e.khr_composition_layer_equirect2 = avail.khr_composition_layer_equirect2;
            e.khr_visibility_mask = avail.khr_visibility_mask;
        }
        // The Steam Frame controller profile only exists with this enabled.
        if avail
            .other
            .iter()
            .any(|n| n.as_slice() == FRAME_CONTROLLER_EXTENSION)
        {
            e.other.push(FRAME_CONTROLLER_EXTENSION.to_vec());
        }
        let mut instance = None;
        let mut last = None;
        for api in [xr::Version::new(1, 1, 0), xr::Version::new(1, 0, 0)] {
            let app = xr::ApplicationInfo {
                application_name: app_name,
                application_version: 1,
                engine_name: "frameplayerx",
                engine_version: 1,
                api_version: api,
            };
            match entry.create_instance(&app, &e, &[], &()) {
                Ok(i) => {
                    instance = Some(i);
                    break;
                }
                Err(err) => last = Some(err),
            }
        }
        let Some(instance) = instance else {
            return Err(Error::Xr(
                "xrCreateInstance",
                last.unwrap_or(xr::sys::Result::ERROR_RUNTIME_FAILURE),
            ));
        };
        let props = instance.properties().ctx("xrGetInstanceProperties")?;
        let system = instance
            .system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)
            .ctx("xrGetSystem (is the headset on?)")?;
        let sp = instance
            .system_properties(system)
            .ctx("xrGetSystemProperties")?;
        let blend_modes = instance
            .enumerate_environment_blend_modes(system, xr::ViewConfigurationType::PRIMARY_STEREO)
            .ctx("xrEnumerateEnvironmentBlendModes")?;
        log::info!(
            "OpenXR: {} {} on {}",
            props.runtime_name,
            props.runtime_version,
            sp.system_name
        );
        log::info!(
            "OpenXR extensions enabled (minimal {}): {:?}",
            opts.minimal_extensions,
            extension_names(&e)
        );
        log::info!(
            "OpenXR blend modes {blend_modes:?}; XR_KHR_convert_timespec_time offered: {}",
            avail.khr_convert_timespec_time
        );
        Ok(XrContext {
            instance,
            system,
            manifest,
            runtime: format!("{} {}", props.runtime_name, props.runtime_version),
            system_name: sp.system_name,
            enabled: e,
            blend_modes,
        })
    }

    /// Closes SteamVR's dashboard, without waiting for it. Does nothing on
    /// other runtimes.
    pub fn hide_steamvr_dashboard(&self) {
        if let Some(mut c) = self.vrcmd("--hidedashboard") {
            std::thread::spawn(move || c.wait());
        }
    }

    /// Opens SteamVR's dashboard and waits for that. Does nothing on other
    /// runtimes.
    pub fn show_steamvr_dashboard(&self) {
        if let Some(mut c) = self.vrcmd("--showdashboard") {
            let _ = c.wait();
        }
    }

    /// Starts SteamVR's `vrcmd` tool, beside the runtime library, with `arg`.
    fn vrcmd(&self, arg: &str) -> Option<std::process::Child> {
        if !self.manifest.is_steamvr {
            return None;
        }
        let vrcmd = std::path::Path::new(&self.manifest.resolved_library_path)
            .parent()?
            .join("vrcmd");
        // With Steam's variables SteamVR would file vrcmd under this app, and
        // the library path is the launcher's, for our own libraries.
        let child = std::process::Command::new(&vrcmd)
            .arg(arg)
            .env_remove("SteamAppId")
            .env_remove("SteamGameId")
            .env_remove("SteamOverlayGameId")
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LD_PRELOAD")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match child {
            Ok(c) => {
                log::info!("vrcmd {arg}");
                Some(c)
            }
            Err(e) => {
                log::warn!("{}: {e}", vrcmd.display());
                None
            }
        }
    }

    /// The per-eye image sizes the runtime recommends and allows.
    pub fn eye_sizes(&self) -> Result<EyeSizes> {
        let views = self
            .instance
            .enumerate_view_configuration_views(
                self.system,
                xr::ViewConfigurationType::PRIMARY_STEREO,
            )
            .ctx("xrEnumerateViewConfigurationViews")?;
        let v = views
            .first()
            .ok_or_else(|| Error::Runtime("no views".into()))?;
        Ok(EyeSizes {
            recommended: (
                v.recommended_image_rect_width,
                v.recommended_image_rect_height,
            ),
            max: (v.max_image_rect_width, v.max_image_rect_height),
        })
    }

    /// A [`fp_render::Creator`] that lets the runtime create Vulkan objects.
    pub fn creator(&self) -> XrCreator<'_> {
        XrCreator { ctx: self }
    }
}

pub struct XrCreator<'a> {
    ctx: &'a XrContext,
}

impl fp_render::Creator for XrCreator<'_> {
    fn create_instance(
        &self,
        entry: &ash::Entry,
        info: &vk::InstanceCreateInfo,
    ) -> fp_render::Result<vk::Instance> {
        let x = self.ctx;
        x.instance
            .graphics_requirements::<xr::Vulkan>(x.system)
            .map_err(|e| {
                fp_render::Error::Unsupported(format!("XR graphics requirements: {e:?}"))
            })?;
        // SAFETY: the function pointer comes from the loaded Vulkan library;
        // the create info is valid for the duration of the call.
        let raw = unsafe {
            x.instance.create_vulkan_instance(
                x.system,
                std::mem::transmute::<
                    vk::PFN_vkGetInstanceProcAddr,
                    xr::sys::platform::VkGetInstanceProcAddr,
                >(entry.static_fn().get_instance_proc_addr),
                info as *const _ as *const _,
            )
        }
        .map_err(|e| fp_render::Error::Unsupported(format!("xrCreateVulkanInstanceKHR: {e:?}")))?
        .map_err(|r| fp_render::Error::Vk {
            context: "vkCreateInstance (via OpenXR)",
            result: vk::Result::from_raw(r),
        })?;
        Ok(vk::Instance::from_raw(raw as u64))
    }

    fn physical_device(&self, instance: &ash::Instance) -> fp_render::Result<vk::PhysicalDevice> {
        let x = self.ctx;
        // SAFETY: the instance was created through this runtime.
        let raw = unsafe {
            x.instance
                .vulkan_graphics_device(x.system, instance.handle().as_raw() as _)
        }
        .map_err(|e| {
            fp_render::Error::Unsupported(format!("xrGetVulkanGraphicsDevice2KHR: {e:?}"))
        })?;
        Ok(vk::PhysicalDevice::from_raw(raw as u64))
    }

    fn create_device(
        &self,
        instance: &ash::Instance,
        pdev: vk::PhysicalDevice,
        info: &vk::DeviceCreateInfo,
    ) -> fp_render::Result<vk::Device> {
        let x = self.ctx;
        // SAFETY: the static loader entry point and valid create info; see
        // create_instance.
        let entry =
            unsafe { ash::Entry::load() }.map_err(|e| fp_render::Error::Load(e.to_string()))?;
        let _ = instance;
        // SAFETY: as for create_vulkan_instance; pdev came from this
        // runtime's instance.
        let raw = unsafe {
            x.instance.create_vulkan_device(
                x.system,
                std::mem::transmute::<
                    vk::PFN_vkGetInstanceProcAddr,
                    xr::sys::platform::VkGetInstanceProcAddr,
                >(entry.static_fn().get_instance_proc_addr),
                pdev.as_raw() as _,
                info as *const _ as *const _,
            )
        }
        .map_err(|e| fp_render::Error::Unsupported(format!("xrCreateVulkanDeviceKHR: {e:?}")))?
        .map_err(|r| fp_render::Error::Vk {
            context: "vkCreateDevice (via OpenXR)",
            result: vk::Result::from_raw(r),
        })?;
        Ok(vk::Device::from_raw(raw as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: EyeSizes = EyeSizes {
        recommended: (1440, 1440),
        max: (4096, 4096),
    };

    #[test]
    fn scale_one_is_the_recommended_size_exactly() {
        let odd = EyeSizes {
            recommended: (1441, 1587),
            max: (4096, 4096),
        };
        assert_eq!(scaled_eye_size(&odd, 1.0), (1441, 1587));
        assert_eq!(scaled_eye_size(&SIZES, 1.0), (1440, 1440));
    }

    #[test]
    fn scales_round_to_multiples_of_eight() {
        assert_eq!(scaled_eye_size(&SIZES, 1.5), (2160, 2160));
        assert_eq!(scaled_eye_size(&SIZES, 1.25), (1800, 1800));
        let odd = EyeSizes {
            recommended: (1441, 1587),
            max: (4096, 4096),
        };
        // 1441 x 1.25 = 1801.25, 1587 x 1.25 = 1983.75.
        assert_eq!(scaled_eye_size(&odd, 1.25), (1800, 1984));
    }

    #[test]
    fn the_runtimes_maximum_and_the_scale_limits_hold() {
        let capped = EyeSizes {
            recommended: (1440, 1440),
            max: (2560, 2560),
        };
        assert_eq!(scaled_eye_size(&capped, 2.0), (2560, 2560));
        assert_eq!(scaled_eye_size(&SIZES, 9.0), (2880, 2880));
        assert_eq!(scaled_eye_size(&SIZES, 0.1), (720, 720));
        assert_eq!(scaled_eye_size(&SIZES, -3.0), (720, 720));
    }

    #[test]
    fn a_scale_that_is_not_a_number_is_one() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(scaled_eye_size(&SIZES, bad), (1440, 1440));
        }
    }

    #[test]
    fn a_runtime_reporting_no_maximum_does_not_limit_the_size() {
        let none = EyeSizes {
            recommended: (1440, 1440),
            max: (0, 0),
        };
        assert_eq!(scaled_eye_size(&none, 1.5), (2160, 2160));
    }

    #[test]
    fn extension_names_list_what_is_enabled() {
        let mut set = xr::ExtensionSet::default();
        assert!(extension_names(&set).is_empty());
        set.khr_vulkan_enable2 = true;
        set.ext_hand_tracking = true;
        set.other
            .push(b"XR_VALVE_frame_controller_interaction\0".to_vec());
        assert_eq!(
            extension_names(&set),
            [
                "XR_KHR_vulkan_enable2",
                "XR_EXT_hand_tracking",
                "XR_VALVE_frame_controller_interaction",
            ]
        );
    }
}
