//! The video pyramid: a mip chain of the stored frame that the scene shader
//! samples where a flat picture is minified.
//!
//! A frame read in the decoder's own buffer (or in copied planes) has one
//! level and nothing coarser to filter with, and a few taps per eye pixel
//! shimmer on fine detail once there are more than two or three texels under
//! each. So each new video frame gets a small pyramid of its own, owned by the
//! renderer: level 0 is the frame at half size (a pass over the same planes
//! the eye passes read, one bilinear tap per 2x2 luma texels), the rest a chain
//! of blits down to one texel. It holds the raw Y'CbCr codes, so the shader
//! converts after filtering, as it does for its taps.
//!
//! One pyramid serves every frame slot: a build waits (by the barrier that
//! starts it) for the earlier submission's reads. A frame is built once however
//! many refreshes show it, and not at all where it can't matter or can't fit.

use crate::Result;
use crate::gpu::Gpu;
use crate::mem::{self, Image};
use crate::params::SceneParams;
use crate::pipeline::{self, Depth, PipelineDesc};
use crate::renderer::{LevelState, blit_chain};
use ash::vk;
use glam::{Vec2, Vec3};

/// Pictures wider or taller than this (luma texels) get no pyramid.
pub(crate) const MAX_SOURCE: u32 = 8192;

/// Below this many texels per eye pixel at the screen's middle (an estimate)
/// the picture is magnified or close to it, the shader would not read the
/// pyramid, and building it would be wasted.
const MIN_FOOTPRINT: f32 = 0.7;

/// GPU milliseconds one build may take: a little over the 1.0 ms a 4K frame is
/// meant to cost.
pub(crate) const BUDGET_MS: f32 = 1.25;

/// The formats a pyramid may have, best first: 10 bits hold the codes of 10-bit
/// video exactly; the others are for devices that can't attach or blit it.
const FORMATS: [vk::Format; 3] = [
    vk::Format::A2B10G10R10_UNORM_PACK32,
    vk::Format::R16G16B16A16_SFLOAT,
    vk::Format::R8G8B8A8_UNORM,
];

/// The first of [`FORMATS`] that `usable` accepts.
pub(crate) fn pick_format(usable: impl Fn(vk::Format) -> bool) -> Option<vk::Format> {
    FORMATS.into_iter().find(|&f| usable(f))
}

/// The pyramid's format on this device, if it can have one: attached to render
/// level 0, sampled with filtering, and blitted level to level.
fn format(gpu: &Gpu) -> Option<vk::Format> {
    let needs = vk::FormatFeatureFlags::COLOR_ATTACHMENT
        | vk::FormatFeatureFlags::SAMPLED_IMAGE
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
        | vk::FormatFeatureFlags::BLIT_SRC
        | vk::FormatFeatureFlags::BLIT_DST;
    pick_format(|f| {
        // SAFETY: valid physical device.
        let p = unsafe {
            gpu.instance
                .get_physical_device_format_properties(gpu.pdev, f)
        };
        p.optimal_tiling_features.contains(needs)
    })
}

/// Size of level 0 for a `source` picture: half of it, rounded up.
pub(crate) fn first_level(source: (u32, u32)) -> (u32, u32) {
    (source.0.div_ceil(2).max(1), source.1.div_ceil(2).max(1))
}

/// Levels in a chain from `w` x `h` down to one texel.
pub(crate) fn level_count(w: u32, h: u32) -> u32 {
    u32::BITS - w.max(h).max(1).leading_zeros()
}

/// The upright picture an eye shows, in luma texels: the stored frame's size,
/// swapped for an odd number of quarter `turns`, halved across (side by side,
/// `stereo` 1) or down (top and bottom, 2) for one eye's half. As the shader's
/// `eye_texels`.
pub(crate) fn eye_texels(source: (u32, u32), turns: u32, stereo: u32) -> Vec2 {
    let (mut w, mut h) = (source.0 as f32, source.1 as f32);
    if turns % 2 == 1 {
        (w, h) = (h, w);
    }
    match stereo {
        1 => w *= 0.5,
        2 => h *= 0.5,
        _ => {}
    }
    Vec2::new(w.max(1.0), h.max(1.0))
}

/// Texels of the picture under one eye pixel at the middle of a flat screen,
/// estimated from the set-up: `texels` the picture's size ([`eye_texels`]),
/// `screen` its size in metres, `distance` metres from the eye to its middle,
/// `px_per_tan` eye pixels to a unit of tangent. The larger of the two axes, as
/// the shader's footprint is.
pub(crate) fn footprint(texels: Vec2, screen: Vec2, distance: f32, px_per_tan: f32) -> f32 {
    let pixels = screen / distance.max(0.01) * px_per_tan;
    (texels.x / pixels.x.max(1e-3)).max(texels.y / pixels.y.max(1e-3))
}

/// [`footprint`] for a scene: `p` the shader's parameters (the screen's size,
/// the picture's turns and stereo layout), `source` the stored frame's size,
/// `eye_pos` and `screen_pos` where the eye and the screen's middle are.
pub(crate) fn scene_footprint(
    p: &SceneParams,
    source: (u32, u32),
    px_per_tan: f32,
    eye_pos: Vec3,
    screen_pos: Vec3,
) -> f32 {
    footprint(
        eye_texels(source, p.display[0], p.mode[1]),
        Vec2::new(p.screen.x, p.screen.y),
        eye_pos.distance(screen_pos),
        px_per_tan,
    )
}

/// Whether a frame should have a pyramid: it is on, the picture is flat and
/// not read in TP10 bands (which have none yet), the size is one we build, and
/// the estimated footprint `fp` ([`footprint`]) says the picture is minified.
pub(crate) fn wanted(on: bool, flat: bool, tp10: bool, source: (u32, u32), fp: f32) -> bool {
    let fits = source.0 <= MAX_SOURCE && source.1 <= MAX_SOURCE;
    on && flat && !tp10 && fits && fp > MIN_FOOTPRINT
}

/// Leaky count of builds that took longer than [`BUDGET_MS`]: each one
/// counts double and each build within budget takes one off, so a pyramid
/// that is mostly too slow is given up on while a stray slow frame (the GPU
/// clock stepping up) is forgiven.
#[derive(Default, Debug)]
pub(crate) struct Budget {
    strikes: u32,
}

impl Budget {
    const GIVE_UP: u32 = 24;

    /// Counts a build that took `ms`; true when it's time to give up.
    pub fn observe(&mut self, ms: f32) -> bool {
        if ms > BUDGET_MS {
            self.strikes += 2;
        } else {
            self.strikes = self.strikes.saturating_sub(1);
        }
        self.strikes >= Self::GIVE_UP
    }
}

/// The pyramid image for pictures of one size.
struct Pyramid {
    image: Image,
    /// The stored frame's size it was made for.
    source: (u32, u32),
    /// The frame it holds, once built.
    key: Option<u64>,
}

/// The pyramid pass and its image.
pub(crate) struct PyramidPass {
    format: vk::Format,
    pipeline: vk::Pipeline,
    image: Option<Pyramid>,
    budget: Budget,
    /// The source size the pyramid is not built for any more: a build went
    /// over budget, or its image couldn't be made.
    left_out: Option<(u32, u32)>,
}

impl PyramidPass {
    /// The pass for the scene pipeline layout `layout`, or `None` (logged)
    /// where this device can't build pyramids.
    pub fn new(gpu: &Gpu, layout: vk::PipelineLayout) -> Option<PyramidPass> {
        let Some(format) = format(gpu) else {
            log::info!("video pyramid: no format to build it in, pictures use the four taps");
            return None;
        };
        let made = pipeline::create_entry(
            gpu,
            &PipelineDesc {
                spv: crate::SCENE_SPV,
                layout,
                color_format: format,
                vertex_stride: 0,
                attributes: &[],
                blend: false,
                descriptor_buffer: false,
                depth: Depth::None,
            },
            c"fs_pyramid",
        );
        match made {
            Ok(pipeline) => Some(PyramidPass {
                format,
                pipeline,
                image: None,
                budget: Budget::default(),
                left_out: None,
            }),
            Err(e) => {
                log::warn!("video pyramid: not built, pictures use the four taps: {e}");
                None
            }
        }
    }

    /// Whether the pyramid is left out for pictures of this size.
    pub fn left_out(&self, source: (u32, u32)) -> bool {
        self.left_out == Some(source)
    }

    /// The sampled view of the whole chain, if an image exists. It is always
    /// shader-readable between builds.
    pub fn view(&self) -> Option<vk::ImageView> {
        self.image.as_ref().map(|p| p.image.views[0])
    }

    /// Levels in the image, 0 without one.
    pub fn levels(&self) -> u32 {
        self.image.as_ref().map_or(0, |p| p.image.mips)
    }

    /// Whether the image holds frame `id`.
    pub fn holds(&self, id: u64) -> bool {
        self.image.as_ref().is_some_and(|p| p.key == Some(id))
    }

    /// Makes sure there is an image for pictures of `source` size, recording
    /// its first layout change in `cmd`; one of another size goes to `garbage`
    /// (frames in flight may still read it). True when the image the scene
    /// sets should point at changed.
    pub fn ensure(
        &mut self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        garbage: &mut Vec<Image>,
        source: (u32, u32),
    ) -> Result<bool> {
        if self.image.as_ref().is_some_and(|p| p.source == source) {
            return Ok(false);
        }
        // Another size: most likely another video, so a verdict on the last
        // one doesn't carry over.
        self.budget = Budget::default();
        self.left_out = None;
        if let Some(old) = self.image.take() {
            garbage.push(old.image);
        }
        let (w, h) = first_level(source);
        let mips = level_count(w, h);
        let usage = vk::ImageUsageFlags::COLOR_ATTACHMENT
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST;
        let image = Image::new(gpu, w, h, &[self.format], usage, mips, "video pyramid")?;
        // Shader-readable from the start, so the scene sets can point at it
        // before its first build.
        mem::barrier(
            gpu,
            cmd,
            image.image,
            mem::range(0, mips),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            mem::TOP,
            mem::SHADER_READ,
        );
        let format = self.format;
        log::info!("video pyramid: {format:?}, {w}x{h}, {mips} levels");
        self.image = Some(Pyramid {
            image,
            source,
            key: None,
        });
        Ok(true)
    }

    /// Gives the image up (to `garbage`, as in [`PyramidPass::ensure`]); true
    /// if there was one.
    pub fn drop_image(&mut self, garbage: &mut Vec<Image>) -> bool {
        match self.image.take() {
            Some(old) => {
                garbage.push(old.image);
                true
            }
            None => false,
        }
    }

    /// Leaves the pyramid out for pictures of `source` size, with `why`.
    pub fn give_up(&mut self, source: (u32, u32), why: &str) {
        if self.left_out != Some(source) {
            log::warn!(
                "video pyramid: left out for {}x{} pictures, they use the four taps: {why}",
                source.0,
                source.1
            );
        }
        self.left_out = Some(source);
    }

    /// Counts the GPU time of a build the GPU has finished; gives the
    /// pyramid up for its picture size when builds keep going over budget.
    pub fn observe(&mut self, ms: f32) {
        let Some(source) = self.image.as_ref().map(|p| p.source) else {
            return;
        };
        if self.left_out.is_none() && self.budget.observe(ms) {
            self.give_up(
                source,
                &format!("builds keep taking over {BUDGET_MS} ms (last {ms:.2} ms)"),
            );
        }
    }

    /// Records the build of frame `frame_id`'s pyramid into `cmd`, from the
    /// planes `set` (the scene set, bound as the eye passes bind it) points
    /// at, with the scene's parameters already in its uniform buffer.
    pub fn record(
        &mut self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        layout: vk::PipelineLayout,
        set: vk::DescriptorSet,
        frame_id: u64,
    ) {
        let Some(p) = self.image.as_mut() else {
            return;
        };
        let d = &gpu.device;
        let (image, mips) = (p.image.image, p.image.mips);
        // The earlier frames' eye passes may be reading the chain: this waits
        // for them, and the old contents aren't wanted.
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(0, 1),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            mem::SHADER_READ,
            mem::COLOR_WRITE,
        );
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(p.image.level0)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: p.image.extent,
        };
        // SAFETY: recording with a valid pipeline, layout and set; the target
        // is in the layout the barrier above left it in.
        unsafe {
            d.cmd_begin_rendering(
                cmd,
                &vk::RenderingInfo::default()
                    .render_area(area)
                    .layer_count(1)
                    .color_attachments(&attachment),
            );
            d.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: area.extent.width as f32,
                    height: area.extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            d.cmd_set_scissor(cmd, 0, &[area]);
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            d.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[set],
                &[],
            );
            d.cmd_draw(cmd, 3, 1, 0, 0);
            d.cmd_end_rendering(cmd);
        }
        let extent = p.image.extent;
        blit_chain(
            gpu,
            cmd,
            image,
            (extent.width, extent.height, mips),
            LevelState::RENDERED,
            LevelState::DISCARDED,
        );
        p.key = Some(frame_id);
    }

    pub fn destroy(&mut self, gpu: &Gpu) {
        if let Some(mut p) = self.image.take() {
            p.image.destroy(gpu);
        }
        // SAFETY: the device is idle.
        unsafe { gpu.device.destroy_pipeline(self.pipeline, None) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_zero_is_half_the_picture_rounded_up() {
        assert_eq!(first_level((3840, 2160)), (1920, 1080));
        assert_eq!(first_level((1919, 1079)), (960, 540));
        assert_eq!(first_level((1, 1)), (1, 1));
    }

    #[test]
    fn the_chain_runs_down_to_one_texel() {
        assert_eq!(level_count(1920, 1080), 11);
        assert_eq!(level_count(1, 1), 1);
        assert_eq!(level_count(2, 1), 2);
        assert_eq!(level_count(960, 540), 10);
        // Every level halves, rounding down, never below one.
        let (mut w, mut h, mut n) = (1920u32, 1080u32, 1);
        while w > 1 || h > 1 {
            (w, h, n) = ((w / 2).max(1), (h / 2).max(1), n + 1);
        }
        assert_eq!(level_count(1920, 1080), n);
    }

    #[test]
    fn the_best_format_the_device_can_use_wins() {
        assert_eq!(
            pick_format(|_| true),
            Some(vk::Format::A2B10G10R10_UNORM_PACK32)
        );
        assert_eq!(
            pick_format(|f| f != vk::Format::A2B10G10R10_UNORM_PACK32),
            Some(vk::Format::R16G16B16A16_SFLOAT)
        );
        assert_eq!(
            pick_format(|f| f == vk::Format::R8G8B8A8_UNORM),
            Some(vk::Format::R8G8B8A8_UNORM)
        );
        assert_eq!(pick_format(|_| false), None);
    }

    #[test]
    fn an_eye_sees_its_half_of_the_upright_picture() {
        let size = (3840, 2160);
        assert_eq!(eye_texels(size, 0, 0), Vec2::new(3840.0, 2160.0));
        assert_eq!(eye_texels(size, 1, 0), Vec2::new(2160.0, 3840.0));
        assert_eq!(eye_texels(size, 2, 0), Vec2::new(3840.0, 2160.0));
        assert_eq!(eye_texels(size, 0, 1), Vec2::new(1920.0, 2160.0));
        assert_eq!(eye_texels(size, 0, 2), Vec2::new(3840.0, 1080.0));
        assert_eq!(eye_texels(size, 3, 1), Vec2::new(1080.0, 3840.0));
    }

    #[test]
    fn the_footprint_estimate_is_texels_per_pixel() {
        // 1080p on a 6 m screen 4 m away, 64 pixels to a unit of tangent: the
        // screen is 96 pixels across and 54 down, so 20 texels to a pixel.
        let f = footprint(Vec2::new(1920.0, 1080.0), Vec2::new(6.0, 3.375), 4.0, 64.0);
        assert!((f - 20.0).abs() < 1e-3, "{f}");
        // Filling a 128 pixel view with a 16 texel picture: one eighth.
        let f = footprint(Vec2::new(16.0, 16.0), Vec2::new(4.0, 4.0), 2.0, 64.0);
        assert!((f - 0.125).abs() < 1e-6, "{f}");
        // The taller axis counts when it is the more minified.
        let f = footprint(Vec2::new(100.0, 1000.0), Vec2::new(4.0, 4.0), 2.0, 64.0);
        assert!((f - 1000.0 / 128.0).abs() < 1e-4, "{f}");
        // On top of the eye, or with no screen, it stays finite.
        let f = footprint(Vec2::splat(100.0), Vec2::ZERO, 0.0, 64.0);
        assert!(f.is_finite(), "{f}");
    }

    #[test]
    fn the_pyramid_is_wanted_for_minified_flat_pictures_it_can_build() {
        let size = (3840, 2160);
        assert!(wanted(true, true, false, size, 5.0));
        assert!(!wanted(false, true, false, size, 5.0), "switched off");
        assert!(!wanted(true, false, false, size, 5.0), "not flat");
        assert!(!wanted(true, true, true, size, 5.0), "TP10 has none yet");
        assert!(!wanted(true, true, false, size, 0.5), "magnified");
        assert!(!wanted(true, true, false, (16384, 8192), 5.0), "too wide");
        assert!(!wanted(true, true, false, (8192, 8193), 5.0), "too tall");
        assert!(wanted(true, true, false, (8192, 8192), 5.0), "the largest");
    }

    #[test]
    fn a_pyramid_that_is_mostly_too_slow_is_given_up_a_stray_slow_build_is_not() {
        let mut b = Budget::default();
        // Twelve slow builds in a row.
        for _ in 0..11 {
            assert!(!b.observe(2.0));
        }
        assert!(b.observe(2.0));

        // A slow one now and then, forgiven by the fast ones around it.
        let mut b = Budget::default();
        for i in 0..400 {
            let ms = if i % 4 == 0 { 3.0 } else { 0.5 };
            assert!(!b.observe(ms), "gave up at build {i}");
        }

        // Within budget, however long it goes on, counts for nothing.
        let mut b = Budget::default();
        for _ in 0..1000 {
            assert!(!b.observe(BUDGET_MS));
        }
        assert!(!b.observe(f32::NAN));
    }
}
