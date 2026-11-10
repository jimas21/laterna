//! Frame recording: video upload, egui panels, and the two eye passes.
//!
//! Two frame slots are in flight. Per-slot resources (command buffer,
//! uniform buffer, video planes, vertex buffers) are reused only after the
//! slot's fence signals. Images freed during a frame go to that slot's
//! garbage list and are destroyed when the slot comes round again, by which
//! time no submitted frame can still read them.

use crate::env::light::{PATCHES, ScreenLight, screen_patches};
use crate::env::{EnvFrame, Place, PlaceData, PlaceKind, sky_floor};
use crate::env_pass::{EnvInput, EnvPass, LitScreen};
use crate::glow::ScreenGlow;
use crate::gpu::Gpu;
use crate::hdr::PeakDetector;
use crate::mem::{self, Buffer, Image};
use crate::params::{self, Quality, SceneParams, VideoParams};
use crate::peak::PeakPass;
use crate::pipeline::{self, Depth, PipelineDesc};
use crate::place_builder::{Built, PlaceBuilder, build_timed};
use crate::pyramid::{self, PyramidPass};
use crate::q10c::Converter;
use crate::timing::{FrameTimer, GpuTimes, Mark, Series};
use crate::tp10_eyes::Tp10Eyes;
use crate::video::VideoPlanes;
use crate::{Result, VkContext};
use ash::vk;
use egui::epaint::{ClippedPrimitive, Primitive, TextureId, textures::TexturesDelta};
use fp_core::format::Projection;
use fp_media::VideoFrame;
use glam::{Mat4, Vec2, Vec3, Vec4};
use gpu_allocator::MemoryLocation;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const SLOTS: usize = 2;
/// The most taps along a picture's long axis the video sampler takes where
/// the device allows anisotropic filtering.
const MAX_ANISOTROPY: f32 = 8.0;

/// What a measurement of the picture for the light it gives its place
/// depends on: the frame, and the settings its colours pass through. The
/// picture is measured again when it changes, not when the same frame is
/// drawn once more.
#[derive(Clone, Copy, PartialEq)]
struct GlowKey {
    frame: Option<u64>,
    /// The picture adjustments, the transfer and the HDR levels.
    colour: [Vec4; 3],
    /// Projection, stereo layout, eye swap and plane layout.
    mode: [u32; 4],
    /// Quarter turns that show the frame upright.
    turns: u32,
}

impl GlowKey {
    fn of(frame: Option<u64>, p: &SceneParams) -> GlowKey {
        GlowKey {
            frame,
            colour: [p.picture, p.extra, p.hdr],
            mode: p.mode,
            turns: p.display[0],
        }
    }
}

/// Linear-light background format of eye targets.
pub const PANEL_FORMATS: [vk::Format; 2] = [vk::Format::R8G8B8A8_UNORM, vk::Format::R8G8B8A8_SRGB];

/// Camera for one eye.
#[derive(Clone, Copy, Debug)]
pub struct EyeView {
    /// World → eye.
    pub view: Mat4,
    /// Eye → Vulkan clip space (see [`crate::math::projection`]).
    pub proj: Mat4,
}

/// Where an eye is rendered: an OpenXR swapchain image or an offscreen image.
#[derive(Clone, Copy, Debug)]
pub struct EyeTarget {
    pub image: vk::Image,
    /// sRGB colour view.
    pub view: vk::ImageView,
    pub extent: vk::Extent2D,
    /// Layout to leave the image in (COLOR_ATTACHMENT_OPTIMAL for OpenXR).
    pub final_layout: vk::ImageLayout,
}

pub type PanelId = usize;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QuadTexture {
    /// Solid colour (tinted by the quad colour).
    White,
    Panel(PanelId),
}

/// A textured quad in world space.
#[derive(Clone, Copy, Debug)]
pub struct QuadDraw {
    pub texture: QuadTexture,
    /// Top-left, top-right, bottom-right, bottom-left.
    pub corners: [Vec3; 4],
    /// Texture coordinates of the top-left and bottom-right corners.
    pub uv: [Vec2; 2],
    /// Linear, premultiplied tint.
    pub color: [f32; 4],
    /// Drawn for this eye only (0 left, 1 right); `None` for both.
    pub eye: Option<usize>,
}

impl QuadDraw {
    /// A quad centred on `transform`'s origin, `width` x `height` metres, in
    /// its local XY plane facing +Z.
    pub fn panel(
        texture: QuadTexture,
        transform: Mat4,
        width: f32,
        height: f32,
        opacity: f32,
    ) -> QuadDraw {
        let (w, h) = (width / 2.0, height / 2.0);
        let p = |x: f32, y: f32| transform.transform_point3(Vec3::new(x, y, 0.0));
        QuadDraw {
            texture,
            corners: [p(-w, h), p(w, h), p(w, -h), p(-w, -h)],
            uv: [Vec2::ZERO, Vec2::ONE],
            color: [opacity; 4],
            eye: None,
        }
    }

    /// A flat ribbon from `a` to `b`, `width` wide, facing `eye`.
    pub fn line(a: Vec3, b: Vec3, width: f32, eye: Vec3, color: [f32; 4]) -> QuadDraw {
        let dir = (b - a).normalize_or_zero();
        let to_eye = (eye - (a + b) * 0.5).normalize_or_zero();
        let side = dir.cross(to_eye).normalize_or_zero() * (width / 2.0);
        QuadDraw {
            texture: QuadTexture::White,
            corners: [a - side, a + side, b + side, b - side],
            uv: [Vec2::ZERO, Vec2::ONE],
            color,
            eye: None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadVertex {
    pos: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}

struct Tex {
    image: Image,
    set: vk::DescriptorSet,
}

struct Panel {
    image: Image,
    set: vk::DescriptorSet,
    textures: HashMap<TextureId, Tex>,
    painted: bool,
    /// Bumped by every paint, so copies of the picture know when to update.
    version: u64,
}

struct Slot {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    uniform: Buffer,
    scene_set: vk::DescriptorSet,
    scene_set_dirty: bool,
    /// The video image set (index, generation) the scene set points at.
    video_bound: Option<(usize, u64)>,
    /// Or the NV12 decoder buffer it points at, read in place
    /// ([`crate::shared`]), and that frame's description.
    shared_bound: Option<(u64, params::FrameDesc)>,
    /// Or the Q10C decoder buffer the TP10 eye pipeline samples
    /// ([`Tp10Eyes`]), and that frame's description.
    tp10_bound: Option<(u64, params::FrameDesc)>,
    has_video: bool,
    /// Frames in the decoder's buffers this slot's submission reads: kept
    /// until its fence signals, which gives the buffers back.
    frames: Vec<Arc<VideoFrame>>,
    /// Imports of decoder buffers to destroy with the garbage.
    garbage_imports: Vec<crate::shared::Imported>,
    quad_vb: Buffer,
    egui_vb: Buffer,
    egui_ib: Buffer,
    egui_vb_used: u64,
    egui_ib_used: u64,
    staging: Buffer,
    staging_used: u64,
    garbage: Vec<Image>,
    garbage_sets: Vec<vk::DescriptorSet>,
    garbage_buffers: Vec<Buffer>,
}

pub struct Renderer {
    gpu: Arc<Gpu>,
    color_format: vk::Format,
    /// The depth attachment's format, and the one image of it both eyes use
    /// (grown to the largest target; see `Renderer::depth_for`).
    depth_format: vk::Format,
    depth: Option<Image>,
    pool: vk::CommandPool,
    dpool: vk::DescriptorPool,
    sampler: vk::Sampler,
    /// Like `sampler`, and anisotropic where the device can: for the video
    /// pyramid, and for room textures ([`Renderer::aniso_sampler`]).
    aniso_sampler: vk::Sampler,
    scene_layout: vk::DescriptorSetLayout,
    scene_pl: vk::PipelineLayout,
    scene_pipe: vk::Pipeline,
    tex_layout: vk::DescriptorSetLayout,
    quad_pl: vk::PipelineLayout,
    quad_pipe: vk::Pipeline,
    egui_pl: vk::PipelineLayout,
    egui_pipe: vk::Pipeline,
    white: Tex,
    dummy: Image,
    panels: Vec<Option<Panel>>,
    slots: Vec<Slot>,
    /// Two video image sets shared by the slots (see `set_video`), which
    /// is newest, and how often each was recreated.
    videos: [Option<VideoPlanes>; 2],
    video_cur: usize,
    video_gen: [u64; 2],
    /// Shows hardware-decoded 10-bit frames, on GPUs that can.
    q10c: Option<Converter>,
    /// Samples 10-bit frames read in place without converting them.
    tp10_eyes: Option<Tp10Eyes>,
    /// Builds each frame's mip chain for the scene shader where the picture
    /// is minified (VQ5); `None` where the device has no format for it.
    pyramid: Option<PyramidPass>,
    /// The frame `set_video` showed this frame, when the scene shader reads it
    /// from planes (the pyramid is made from those): not one read in TP10 bands.
    shown_id: Option<u64>,
    /// Levels of the pyramid the last `draw` read (0: none).
    pyramid_levels: u32,
    /// Measures HDR frames for `hdr_peak`; without it HDR keeps the
    /// default peak.
    peak: Option<PeakPass>,
    hdr_peak: PeakDetector,
    /// Draws the place around the picture. Made the first time a frame asks
    /// for a place, so nobody who never picks one pays for it.
    env: Option<EnvPass>,
    /// Making the place pass or loading a place failed: places aren't shown
    /// (the picture is, as without one), and it is not tried again.
    env_failed: bool,
    /// Makes places on a worker thread: the render thread asks for one, shows
    /// Plain until it is built, and uploads it.
    builds: PlaceBuilder,
    /// Builds a place on the render thread instead, the frame that wants it
    /// (for tests and the preview, which draw frames one by one and want the
    /// place in the first).
    inline_places: bool,
    /// Measures the picture for the light it gives a place. Made the first
    /// time a place wants it.
    glow: Option<ScreenGlow>,
    /// Making the glow pass failed: a place isn't lit by the picture, and it
    /// is not tried again.
    glow_failed: bool,
    /// The colours the glow pass measured last, which come two frames after
    /// the frame they are of; kept until a newer measurement or the picture's
    /// end.
    glow_held: Option<[Vec3; PATCHES]>,
    /// What the last measurement was for.
    glow_key: Option<GlowKey>,
    /// This frame measures the picture (`picture_light` decided).
    glow_due: bool,
    /// The id of the video frame `set_video` was given last, whatever it
    /// is read from (the glow measures TP10 frames too).
    glow_frame_id: Option<u64>,
    /// The picture's light on the place, eased frame by frame.
    light: ScreenLight,
    /// Times each frame's parts on the GPU, where it has timestamps.
    timer: Option<FrameTimer>,
    /// The parts of the last frame the GPU finished.
    gpu_times: Option<GpuTimes>,
    /// Decoder buffers imported to be read in place.
    imports: crate::shared::Imports,
    /// The decoder buffer set of the last shared frame, whose imports stay
    /// while frames come copied now and then.
    shared_set: Option<u64>,
    /// What the frame shown last is, when it is an NV12 one read in the
    /// decoder's buffer: such frames have no image set to ask.
    shared_desc: Option<params::FrameDesc>,
    current: usize,
    recording: bool,
    /// Which quality features the scene shader uses.
    quality: Quality,
    /// Wall time of the slot-fence wait in [`Renderer::begin_frame`]; the
    /// app takes and resets it.
    pub fence_wait: Series,
}

impl Renderer {
    /// `color_format` is the sRGB format of the eye targets.
    pub fn new(gpu: Arc<Gpu>, color_format: vk::Format) -> Result<Renderer> {
        let d = &gpu.device;
        // SAFETY: plain object creation on a valid device.
        unsafe {
            let pool = d
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(gpu.queue_family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .ctx("command pool")?;
            let sizes = [
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::UNIFORM_BUFFER,
                    descriptor_count: 16,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLER,
                    descriptor_count: 1024,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLED_IMAGE,
                    descriptor_count: 2048,
                },
            ];
            let dpool = d
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                        .max_sets(1024)
                        .pool_sizes(&sizes),
                    None,
                )
                .ctx("descriptor pool")?;
            let sampler = d
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .max_lod(vk::LOD_CLAMP_NONE),
                    None,
                )
                .ctx("sampler")?;
            let mut aniso_info = vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .max_lod(vk::LOD_CLAMP_NONE);
            if let Some(max) = gpu.max_anisotropy {
                aniso_info = aniso_info
                    .anisotropy_enable(true)
                    .max_anisotropy(max.min(MAX_ANISOTROPY));
            }
            let aniso_sampler = d
                .create_sampler(&aniso_info, None)
                .ctx("anisotropic sampler")?;

            let depth_format = gpu.depth_format();
            // Bindings 20 and 21 are the video pyramid and its sampler
            // (planes.wgsl); the TP10 pipeline's layout has them too, in its
            // descriptor buffer (tp10_eyes.rs).
            let scene_layout = pipeline::set_layout(
                &gpu,
                &[
                    (0, vk::DescriptorType::UNIFORM_BUFFER),
                    (1, vk::DescriptorType::SAMPLER),
                    (2, vk::DescriptorType::SAMPLED_IMAGE),
                    (3, vk::DescriptorType::SAMPLED_IMAGE),
                    (4, vk::DescriptorType::SAMPLED_IMAGE),
                    (20, vk::DescriptorType::SAMPLED_IMAGE),
                    (21, vk::DescriptorType::SAMPLER),
                ],
            )?;
            let scene_pl = pipeline::layout(&gpu, scene_layout, 4)?;
            let pyramid = PyramidPass::new(&gpu, scene_pl);
            let scene_pipe = pipeline::create(
                &gpu,
                &PipelineDesc {
                    spv: crate::SCENE_SPV,
                    layout: scene_pl,
                    color_format,
                    vertex_stride: 0,
                    attributes: &[],
                    blend: true,
                    descriptor_buffer: false,
                    depth: Depth::Ignore(depth_format),
                },
            )?;
            let tex_layout = pipeline::set_layout(
                &gpu,
                &[
                    (0, vk::DescriptorType::SAMPLER),
                    (1, vk::DescriptorType::SAMPLED_IMAGE),
                ],
            )?;
            let quad_pl = pipeline::layout(&gpu, tex_layout, 64)?;
            let quad_attrs = [
                vk::VertexInputAttributeDescription {
                    location: 0,
                    binding: 0,
                    format: vk::Format::R32G32B32_SFLOAT,
                    offset: 0,
                },
                vk::VertexInputAttributeDescription {
                    location: 1,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 12,
                },
                vk::VertexInputAttributeDescription {
                    location: 2,
                    binding: 0,
                    format: vk::Format::R32G32B32A32_SFLOAT,
                    offset: 20,
                },
            ];
            let quad_pipe = pipeline::create(
                &gpu,
                &PipelineDesc {
                    spv: crate::QUAD_SPV,
                    layout: quad_pl,
                    color_format,
                    vertex_stride: size_of::<QuadVertex>() as u32,
                    attributes: &quad_attrs,
                    blend: true,
                    descriptor_buffer: false,
                    depth: Depth::Ignore(depth_format),
                },
            )?;
            let egui_pl = pipeline::layout(&gpu, tex_layout, 8)?;
            let egui_attrs = [
                vk::VertexInputAttributeDescription {
                    location: 0,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 0,
                },
                vk::VertexInputAttributeDescription {
                    location: 1,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 8,
                },
                vk::VertexInputAttributeDescription {
                    location: 2,
                    binding: 0,
                    format: vk::Format::R8G8B8A8_UNORM,
                    offset: 16,
                },
            ];
            let egui_pipe = pipeline::create(
                &gpu,
                &PipelineDesc {
                    spv: crate::EGUI_SPV,
                    layout: egui_pl,
                    color_format: PANEL_FORMATS[0],
                    vertex_stride: 20,
                    attributes: &egui_attrs,
                    blend: true,
                    descriptor_buffer: false,
                    depth: Depth::None,
                },
            )?;

            let mut r = Renderer {
                gpu: gpu.clone(),
                color_format,
                depth_format,
                depth: None,
                pool,
                dpool,
                sampler,
                aniso_sampler,
                scene_layout,
                scene_pl,
                scene_pipe,
                tex_layout,
                quad_pl,
                quad_pipe,
                egui_pl,
                egui_pipe,
                white: Tex {
                    image: Image::new(&gpu, 1, 1, &PANEL_FORMATS, usage_sampled(), 1, "white")?,
                    set: vk::DescriptorSet::null(),
                },
                dummy: Image::new(
                    &gpu,
                    1,
                    1,
                    &[vk::Format::R8G8B8A8_UNORM],
                    usage_sampled(),
                    1,
                    "dummy",
                )?,
                panels: Vec::new(),
                slots: Vec::new(),
                videos: [None, None],
                video_cur: 0,
                video_gen: [0, 0],
                q10c: Converter::new(&gpu),
                tp10_eyes: None,
                pyramid,
                shown_id: None,
                pyramid_levels: 0,
                peak: PeakPass::new(&gpu, SLOTS)
                    .inspect_err(|e| log::warn!("HDR scenes aren't measured: {e}"))
                    .ok(),
                hdr_peak: PeakDetector::default(),
                env: None,
                env_failed: false,
                builds: PlaceBuilder::new(),
                inline_places: false,
                glow: None,
                glow_failed: false,
                glow_held: None,
                glow_key: None,
                glow_due: false,
                glow_frame_id: None,
                light: ScreenLight::default(),
                timer: FrameTimer::new(&gpu, SLOTS),
                gpu_times: None,
                imports: Default::default(),
                shared_set: None,
                shared_desc: None,
                current: 0,
                recording: false,
                quality: Quality::default(),
                fence_wait: Series::new(),
            };
            r.white.set = r.tex_set(r.white.image.views[1])?;
            // Initialise the white and dummy images.
            let (white, dummy) = (r.white.image.image, r.dummy.image);
            r.one_shot(|gpu, cmd| {
                for (img, value) in [(white, 1.0f32), (dummy, 0.0)] {
                    mem::barrier(
                        gpu,
                        cmd,
                        img,
                        mem::range(0, 1),
                        vk::ImageLayout::UNDEFINED,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        mem::TOP,
                        mem::TRANSFER_WRITE,
                    );
                    gpu.device.cmd_clear_color_image(
                        cmd,
                        img,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &vk::ClearColorValue {
                            float32: [value; 4],
                        },
                        &[mem::range(0, 1)],
                    );
                    mem::barrier(
                        gpu,
                        cmd,
                        img,
                        mem::range(0, 1),
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        mem::TRANSFER_WRITE,
                        mem::SHADER_READ,
                    );
                }
            })?;
            r.tp10_eyes = tp10_eyes(
                &r.gpu,
                color_format,
                depth_format,
                (r.sampler, r.aniso_sampler, r.dummy.views[0]),
                r.q10c.is_some(),
            );
            for _ in 0..SLOTS {
                let slot = r.new_slot()?;
                r.slots.push(slot);
            }
            Ok(r)
        }
    }

    pub fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }

    pub fn color_format(&self) -> vk::Format {
        self.color_format
    }

    /// A sampler like the video's (linear, clamped to the edge, every mip
    /// level) that also filters anisotropically, up to 8 taps, where the
    /// device allows it; plain trilinear where not. For textures seen at a
    /// slant, such as a room's.
    pub fn aniso_sampler(&self) -> vk::Sampler {
        self.aniso_sampler
    }

    /// Levels of the video pyramid the last [`Renderer::draw`] read, 0 when
    /// the picture was drawn without one (magnified, or not flat, or the
    /// pyramid is off or left out).
    pub fn pyramid_levels(&self) -> u32 {
        self.pyramid_levels
    }

    fn new_slot(&self) -> Result<Slot> {
        let d = &self.gpu.device;
        // SAFETY: allocation from our pool; fence created signalled so the
        // first wait returns immediately.
        unsafe {
            let cmd = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .command_buffer_count(1),
                )
                .ctx("command buffer")?[0];
            let fence = d
                .create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
                .ctx("fence")?;
            let layouts = [self.scene_layout];
            let scene_set = d
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.dpool)
                        .set_layouts(&layouts),
                )
                .ctx("scene descriptor set")?[0];
            let uniform = Buffer::new(
                &self.gpu,
                size_of::<SceneParams>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                MemoryLocation::CpuToGpu,
                "scene params",
            )?;
            let cpu = |size, usage, name| {
                Buffer::new(&self.gpu, size, usage, MemoryLocation::CpuToGpu, name)
            };
            Ok(Slot {
                cmd,
                fence,
                uniform,
                scene_set,
                scene_set_dirty: true,
                video_bound: None,
                shared_bound: None,
                tp10_bound: None,
                has_video: false,
                frames: Vec::new(),
                garbage_imports: Vec::new(),
                quad_vb: cpu(64 << 10, vk::BufferUsageFlags::VERTEX_BUFFER, "quads")?,
                egui_vb: cpu(
                    1 << 20,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    "egui vertices",
                )?,
                egui_ib: cpu(1 << 20, vk::BufferUsageFlags::INDEX_BUFFER, "egui indices")?,
                egui_vb_used: 0,
                egui_ib_used: 0,
                staging: cpu(4 << 20, vk::BufferUsageFlags::TRANSFER_SRC, "egui staging")?,
                staging_used: 0,
                garbage: Vec::new(),
                garbage_sets: Vec::new(),
                garbage_buffers: Vec::new(),
            })
        }
    }

    fn tex_set(&self, view: vk::ImageView) -> Result<vk::DescriptorSet> {
        let layouts = [self.tex_layout];
        // SAFETY: allocation from our pool and writes of valid handles.
        unsafe {
            let set = self
                .gpu
                .device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.dpool)
                        .set_layouts(&layouts),
                )
                .ctx("texture descriptor set")?[0];
            let samplers = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
            let images = [vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(&samplers),
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&images),
            ];
            self.gpu.device.update_descriptor_sets(&writes, &[]);
            Ok(set)
        }
    }

    /// Records and runs commands immediately, waiting for completion.
    pub fn one_shot(&self, f: impl FnOnce(&Gpu, vk::CommandBuffer)) -> Result<()> {
        let d = &self.gpu.device;
        // SAFETY: temporary command buffer and fence, freed before returning.
        unsafe {
            let cmd = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .command_buffer_count(1),
                )
                .ctx("one-shot command buffer")?[0];
            d.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .ctx("begin")?;
            f(&self.gpu, cmd);
            d.end_command_buffer(cmd).ctx("end")?;
            let fence = d
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .ctx("fence")?;
            let cmds = [cmd];
            d.queue_submit(
                self.gpu.queue,
                &[vk::SubmitInfo::default().command_buffers(&cmds)],
                fence,
            )
            .ctx("submit")?;
            let r = d.wait_for_fences(&[fence], true, u64::MAX).ctx("wait");
            d.destroy_fence(fence, None);
            d.free_command_buffers(self.pool, &cmds);
            r
        }
    }

    /// Creates an off-screen UI panel of `width` x `height` pixels.
    pub fn create_panel(&mut self, width: u32, height: u32) -> Result<PanelId> {
        let (image, set) = self.panel_surface(width, height)?;
        let panel = Panel {
            image,
            set,
            textures: HashMap::new(),
            painted: false,
            version: 0,
        };
        match self.panels.iter().position(Option::is_none) {
            Some(i) => {
                self.panels[i] = Some(panel);
                Ok(i)
            }
            None => {
                self.panels.push(Some(panel));
                Ok(self.panels.len() - 1)
            }
        }
    }

    /// A cleared panel image of `width` x `height` pixels with its mip chain
    /// and the descriptor set that samples it.
    fn panel_surface(&mut self, width: u32, height: u32) -> Result<(Image, vk::DescriptorSet)> {
        let mips = 32 - width.max(height).leading_zeros();
        let image = Image::new(
            &self.gpu,
            width,
            height,
            &PANEL_FORMATS,
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST,
            mips,
            "ui panel",
        )?;
        let set = self.tex_set(image.views[1])?;
        let img = image.image;
        // SAFETY: img was just created with `mips` levels and is not in use;
        // cmd is recording.
        self.one_shot(|gpu, cmd| unsafe {
            mem::barrier(
                gpu,
                cmd,
                img,
                mem::range(0, mips),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                mem::TOP,
                mem::TRANSFER_WRITE,
            );
            gpu.device.cmd_clear_color_image(
                cmd,
                img,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue { float32: [0.0; 4] },
                &[mem::range(0, mips)],
            );
            mem::barrier(
                gpu,
                cmd,
                img,
                mem::range(0, mips),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                mem::TRANSFER_WRITE,
                mem::SHADER_READ,
            );
        })?;
        Ok((image, set))
    }

    /// Gives a panel a picture of `width` x `height` pixels, empty until it is
    /// painted again. Its id stays and so do the egui textures painted into it
    /// (egui sends each only once, so a panel made afresh would lose them).
    /// The old picture is destroyed once no frame uses it.
    pub fn resize_panel(&mut self, id: PanelId, width: u32, height: u32) -> Result<()> {
        if self.panel_size(id) == Some((width, height)) {
            return Ok(());
        }
        let (image, set) = self.panel_surface(width, height)?;
        let slot = &mut self.slots[self.current];
        let Some(Some(p)) = self.panels.get_mut(id) else {
            slot.garbage.push(image);
            slot.garbage_sets.push(set);
            return Ok(());
        };
        let old = std::mem::replace(&mut p.image, image);
        let old_set = std::mem::replace(&mut p.set, set);
        // Copies of the picture made so far are of the old one.
        p.version += 1;
        slot.garbage.push(old);
        slot.garbage_sets.push(old_set);
        Ok(())
    }

    pub fn panel_size(&self, id: PanelId) -> Option<(u32, u32)> {
        self.panels
            .get(id)?
            .as_ref()
            .map(|p| (p.image.extent.width, p.image.extent.height))
    }

    /// Destroys a panel (deferred until no frame uses it).
    pub fn destroy_panel(&mut self, id: PanelId) {
        if let Some(Some(p)) = self.panels.get_mut(id).map(Option::take) {
            let slot = &mut self.slots[self.current];
            slot.garbage.push(p.image);
            slot.garbage_sets.push(p.set);
            for (_, t) in p.textures {
                slot.garbage.push(t.image);
                slot.garbage_sets.push(t.set);
            }
        }
    }

    /// Waits for the next slot and starts recording.
    pub fn begin_frame(&mut self) -> Result<()> {
        self.current = (self.current + 1) % SLOTS;
        let d = &self.gpu.device;
        let slot = &mut self.slots[self.current];
        // SAFETY: the slot's previous submission is complete after the wait,
        // so its resources and garbage can be reused/destroyed.
        unsafe {
            let waited = Instant::now();
            d.wait_for_fences(&[slot.fence], true, u64::MAX)
                .ctx("wait for frame")?;
            self.fence_wait.push_since(waited);
            for mut img in slot.garbage.drain(..) {
                img.destroy(&self.gpu);
            }
            if !slot.garbage_sets.is_empty() {
                let _ = d.free_descriptor_sets(self.dpool, &slot.garbage_sets);
                slot.garbage_sets.clear();
            }
            for mut b in slot.garbage_buffers.drain(..) {
                b.destroy(&self.gpu);
            }
            for mut i in slot.garbage_imports.drain(..) {
                i.destroy(&self.gpu);
            }
            // Its decoder buffers go back to the decoder.
            slot.frames.clear();
            // A decoder buffer is read only in a frame that took it over
            // (set_video), so the binding doesn't carry over.
            if slot.shared_bound.take().is_some() | slot.tp10_bound.take().is_some() {
                slot.has_video = false;
                slot.scene_set_dirty = true;
            }
            d.reset_command_buffer(slot.cmd, vk::CommandBufferResetFlags::empty())
                .ctx("reset")?;
            d.begin_command_buffer(
                slot.cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .ctx("begin frame")?;
        }
        slot.egui_vb_used = 0;
        slot.egui_ib_used = 0;
        slot.staging_used = 0;
        let cmd = slot.cmd;
        if let Some(hist) = self.peak.as_mut().and_then(|p| p.take(self.current)) {
            self.hdr_peak.add(&hist);
        }
        if let Some(sample) = self.glow.as_mut().and_then(|g| g.take(self.current)) {
            self.glow_held = Some(sample);
        }
        if let Some(t) = self.timer.as_mut() {
            // Read before `begin` resets the queries for this frame.
            self.gpu_times = t.read(&self.gpu, self.current);
            t.begin(&self.gpu, cmd, self.current);
        }
        // A pyramid that keeps taking too long to build is given up on.
        if let Some(ms) = self.gpu_times.and_then(|t| t.pyramid)
            && let Some(pass) = self.pyramid.as_mut()
        {
            pass.observe(ms);
        }
        // Set again by this frame's `set_video`.
        self.shown_id = None;
        self.recording = true;
        Ok(())
    }

    /// The parts of the last frame the GPU finished, two frames back.
    pub fn gpu_times(&self) -> Option<GpuTimes> {
        self.gpu_times
    }

    fn stamp(&mut self, mark: Mark) {
        if !self.recording {
            return;
        }
        let cmd = self.slots[self.current].cmd;
        if let Some(t) = self.timer.as_mut() {
            t.stamp(&self.gpu, cmd, self.current, mark);
        }
    }

    /// Uploads the frame to show (or clears the video when `None`).
    /// Shows `frame` this frame. Each video frame is uploaded once, into
    /// whichever of the two shared image sets the frame in flight isn't
    /// reading: an 8K frame is 50 MB, and copying it for every frame slot
    /// cost the render thread its 90 Hz deadline.
    ///
    /// A frame in the decoder's own buffer ([`VideoFrame::shared`]) is read
    /// there instead, without a copy, and kept until the GPU is done with it.
    pub fn set_video(&mut self, frame: Option<&Arc<VideoFrame>>) -> Result<()> {
        self.stamp(Mark::Video);
        let r = self.show_video(frame);
        self.stamp(Mark::VideoDone);
        // The pyramid is made from planes or, for a frame read in TP10 bands,
        // from those (where the eye passes read them: `tp10_eyes`); a failed
        // frame or none has no id to make it for.
        let in_bands = self.slots[self.current].tp10_bound.is_some();
        self.shown_id = match frame {
            Some(f) if r.is_ok() && (!in_bands || self.tp10_eyes.is_some()) => Some(f.id),
            _ => None,
        };
        r
    }

    fn show_video(&mut self, frame: Option<&Arc<VideoFrame>>) -> Result<()> {
        self.glow_frame_id = frame.map(|f| f.id);
        let Some(f) = frame else {
            let slot = &mut self.slots[self.current];
            if slot.has_video {
                slot.scene_set_dirty = true;
            }
            slot.has_video = false;
            slot.shared_bound = None;
            slot.tp10_bound = None;
            // Nothing playing: the decoder's buffers can go.
            self.imports.sweep(None, &mut slot.garbage_imports);
            self.shared_set = None;
            self.shared_desc = None;
            self.forget_scene();
            return Ok(());
        };
        if let Some(p) = f.shared().copied() {
            let r = self.set_shared_video(f, &p);
            if let Err(e) = &r {
                log::warn!(
                    "reading the decoder's buffer failed, frames are copied from now on: {e}"
                );
                fp_media::set_shared_frames(false);
                let slot = &mut self.slots[self.current];
                slot.has_video = false;
                slot.shared_bound = None;
                slot.tp10_bound = None;
                slot.scene_set_dirty = true;
            }
            return r;
        }
        {
            let slot = &mut self.slots[self.current];
            if slot.shared_bound.take().is_some() | slot.tp10_bound.take().is_some() {
                slot.scene_set_dirty = true;
            }
            self.imports
                .sweep(self.shared_set, &mut slot.garbage_imports);
        }
        let resident = self.videos[self.video_cur]
            .as_ref()
            .is_some_and(|v| v.key == Some(f.id));
        if !resident {
            // The other set was last read by the frame before the one in
            // flight, which begin_frame has waited for.
            let target = 1 - self.video_cur;
            let slot = &mut self.slots[self.current];
            match VideoPlanes::upload(
                &mut self.videos[target],
                &self.gpu,
                slot.cmd,
                f,
                &mut slot.garbage,
                self.q10c.as_ref(),
            ) {
                Ok(recreated) => {
                    if recreated {
                        self.video_gen[target] += 1;
                        // Another size or layout: most likely another video.
                        self.hdr_peak.reset();
                        if let Some(p) = self.peak.as_mut() {
                            p.discard();
                        }
                    }
                    self.video_cur = target;
                    if let (Some(p), Some(v)) = (self.peak.as_mut(), self.videos[target].as_ref()) {
                        p.record(&self.gpu, slot.cmd, self.current, v);
                    }
                }
                Err(e) => {
                    log::warn!("video frame upload failed: {e}");
                    slot.has_video = false;
                    slot.scene_set_dirty = true;
                    return Err(e);
                }
            }
        }
        self.bind_video_set();
        Ok(())
    }

    /// Points this slot's scene set at the newest video image set.
    fn bind_video_set(&mut self) {
        self.shared_desc = None;
        let bound = (self.video_cur, self.video_gen[self.video_cur]);
        let slot = &mut self.slots[self.current];
        slot.tp10_bound = None;
        if slot.video_bound != Some(bound) || !slot.has_video {
            slot.scene_set_dirty = true;
            slot.video_bound = Some(bound);
        }
        slot.has_video = true;
    }

    /// Shows a frame in the decoder's buffer `p`: NV12 sampled there by
    /// the eye passes, Q10C converted from there into an image set.
    fn set_shared_video(&mut self, f: &Arc<VideoFrame>, p: &fp_media::SharedPicture) -> Result<()> {
        let slot = &mut self.slots[self.current];
        self.imports.sweep(Some(p.set), &mut slot.garbage_imports);
        self.shared_set = Some(p.set);
        if !slot.frames.iter().any(|g| g.id == f.id) {
            slot.frames.push(f.clone());
        }
        match f.layout {
            fp_media::PixelLayout::Nv12 => {
                self.imports
                    .nv12(&self.gpu, slot.cmd, p, (f.width, f.height))?;
                let desc = params::FrameDesc {
                    width: f.width,
                    height: f.height,
                    layout: f.layout,
                    color: f.color,
                };
                if slot.shared_bound.map(|(b, _)| b) != Some(p.buffer) || !slot.has_video {
                    slot.scene_set_dirty = true;
                }
                slot.shared_bound = Some((p.buffer, desc));
                slot.tp10_bound = None;
                slot.video_bound = None;
                slot.has_video = true;
                self.shared_desc = Some(desc);
                Ok(())
            }
            // HDR scenes are measured in converted planes, so HDR converts.
            fp_media::PixelLayout::Q10c
                if self.tp10_eyes.is_some()
                    && f.color.transfer == fp_media::info::Transfer::Sdr =>
            {
                self.sample_shared_q10c(f, p)
            }
            fp_media::PixelLayout::Q10c => {
                self.convert_shared_q10c(f, p)?;
                self.bind_video_set();
                Ok(())
            }
            other => Err(crate::Error::Unsupported(format!(
                "decoder buffers of {other:?} frames"
            ))),
        }
    }

    /// Shows the Q10C frame `f` in decoder buffer `p` by sampling it there
    /// in the eye passes ([`Tp10Eyes`]): no conversion, no image set.
    fn sample_shared_q10c(
        &mut self,
        f: &Arc<VideoFrame>,
        p: &fp_media::SharedPicture,
    ) -> Result<()> {
        let Some(conv) = self.q10c.as_ref() else {
            return Err(crate::Error::Unsupported(
                "this GPU can't show 10-bit hardware-decoded (Q10C) frames".into(),
            ));
        };
        let geometry = crate::q10c::Geometry {
            width: f.width,
            height: f.height,
            stride: p.stride,
            rows: p.rows,
            uv_offset: p.uv_offset,
            len: p.len,
        };
        let slot = &mut self.slots[self.current];
        let gpu = &self.gpu;
        self.imports
            .tp10(gpu, slot.cmd, p, || conv.sample(gpu, geometry, p.fd))?;
        let desc = params::FrameDesc {
            width: f.width,
            height: f.height,
            layout: f.layout,
            color: f.color,
        };
        slot.tp10_bound = Some((p.buffer, desc));
        slot.shared_bound = None;
        slot.video_bound = None;
        if !slot.has_video {
            slot.scene_set_dirty = true;
        }
        slot.has_video = true;
        self.shared_desc = Some(desc);
        Ok(())
    }

    /// Converts the Q10C frame `f` in decoder buffer `p` into the image set
    /// the frame in flight isn't reading, as copied ones are (`set_video`).
    fn convert_shared_q10c(
        &mut self,
        f: &Arc<VideoFrame>,
        p: &fp_media::SharedPicture,
    ) -> Result<()> {
        if self.videos[self.video_cur]
            .as_ref()
            .is_some_and(|v| v.key == Some(f.id))
        {
            return Ok(());
        }
        let Some(conv) = self.q10c.as_ref() else {
            return Err(crate::Error::Unsupported(
                "this GPU can't show 10-bit hardware-decoded (Q10C) frames".into(),
            ));
        };
        let target = 1 - self.video_cur;
        let slot = &mut self.slots[self.current];
        if VideoPlanes::q10c_outputs(&mut self.videos[target], &self.gpu, f, &mut slot.garbage)? {
            self.video_gen[target] += 1;
            self.imports
                .sweep_target((target, self.video_gen[target]), &mut slot.garbage_imports);
            // Another size: most likely another video.
            self.hdr_peak.reset();
            if let Some(peak) = self.peak.as_mut() {
                peak.discard();
            }
        }
        let Some(set) = self.videos[target].as_mut() else {
            return Ok(());
        };
        let geometry = crate::q10c::Geometry {
            width: f.width,
            height: f.height,
            stride: p.stride,
            rows: p.rows,
            uv_offset: p.uv_offset,
            len: p.len,
        };
        let gpu = &self.gpu;
        let picture = self.imports.q10c(p, (target, self.video_gen[target]), || {
            conv.import(gpu, geometry, p.fd, &set.images)
        })?;
        set.desc.color = f.color;
        conv.record(gpu, slot.cmd, picture, &set.images);
        set.key = Some(f.id);
        self.video_cur = target;
        // HDR frames are measured in the converted planes, as copied ones are.
        if let (Some(peak), Some(v)) = (self.peak.as_mut(), self.videos[target].as_ref()) {
            peak.record(&self.gpu, slot.cmd, self.current, v);
        }
        Ok(())
    }

    /// Turns the scene shader's quality features on or off (all are on by
    /// default); takes effect from the next [`Renderer::draw`].
    pub fn set_quality(&mut self, q: Quality) {
        self.quality = q;
    }

    /// Whether hardware-decoded 10-bit HEVC ([`fp_media::PixelLayout::Q10c`]
    /// frames) can be shown; see [`fp_media::set_hevc_10bit_hardware`].
    pub fn shows_q10c(&self) -> bool {
        self.q10c.is_some()
    }

    /// The HDR scene peak in nits that tone mapping uses now.
    pub fn hdr_peak(&self) -> f32 {
        self.hdr_peak.peak_nits()
    }

    /// The video shown last, if it is HDR: its transfer and the scene peak
    /// in nits tone mapping uses.
    pub fn hdr_scene(&self) -> Option<(fp_media::info::Transfer, f32)> {
        let desc = match self.shared_desc {
            Some(desc) => desc,
            None => self.videos[self.video_cur].as_ref()?.desc,
        };
        (desc.color.transfer != fp_media::info::Transfer::Sdr)
            .then(|| (desc.color.transfer, self.hdr_peak.peak_nits()))
    }

    /// Starts the HDR peak afresh, ignoring measurements still on their way.
    fn forget_scene(&mut self) {
        self.hdr_peak.reset();
        if let Some(p) = self.peak.as_mut() {
            p.discard();
        }
    }

    /// Whether this GPU reads frames in the hardware decoder's own buffers
    /// instead of copies; see [`fp_media::set_shared_frames`].
    pub fn reads_decoder_buffers(&self) -> bool {
        self.gpu.memory_fd.is_some()
    }

    /// The video image set this slot shows, if any.
    fn slot_video(&self, slot: usize) -> Option<&VideoPlanes> {
        let s = &self.slots[slot];
        if !s.has_video {
            return None;
        }
        self.videos[s.video_bound?.0].as_ref()
    }

    fn staging_alloc(&mut self, bytes: &[u8]) -> Result<(vk::Buffer, u64)> {
        let slot = &mut self.slots[self.current];
        let need = slot.staging_used + bytes.len() as u64 + 256;
        if need > slot.staging.size {
            let bigger = Buffer::new(
                &self.gpu,
                need.max(slot.staging.size * 2),
                vk::BufferUsageFlags::TRANSFER_SRC,
                MemoryLocation::CpuToGpu,
                "egui staging",
            )?;
            let old = std::mem::replace(&mut slot.staging, bigger);
            slot.garbage_buffers.push(old);
            // Earlier copies this frame reference the old buffer; it stays
            // alive until the slot comes round again.
            slot.staging_used = 0;
        }
        let off = (slot.staging_used + 15) & !15;
        slot.staging.bytes()[off as usize..off as usize + bytes.len()].copy_from_slice(bytes);
        slot.staging_used = off + bytes.len() as u64;
        Ok((slot.staging.buffer, off))
    }

    fn upload_egui_texture(
        &mut self,
        panel: PanelId,
        id: TextureId,
        delta: &egui::epaint::ImageDelta,
    ) -> Result<()> {
        let egui::ImageData::Color(img) = &delta.image;
        let [w, h] = img.size;
        let bytes: &[u8] = bytemuck::cast_slice(&img.pixels);
        let (buf, off) = self.staging_alloc(bytes)?;
        let cmd = self.slots[self.current].cmd;
        let need_new = delta.pos.is_none();
        if need_new {
            let image = Image::new(
                &self.gpu,
                w as u32,
                h as u32,
                &[vk::Format::R8G8B8A8_UNORM],
                usage_sampled(),
                1,
                "egui texture",
            )?;
            let set = self.tex_set(image.views[0])?;
            let Some(Some(p)) = self.panels.get_mut(panel) else {
                return Ok(());
            };
            if let Some(old) = p.textures.insert(id, Tex { image, set }) {
                let slot = &mut self.slots[self.current];
                slot.garbage.push(old.image);
                slot.garbage_sets.push(old.set);
            }
        }
        let Some(Some(p)) = self.panels.get(panel) else {
            return Ok(());
        };
        let Some(tex) = p.textures.get(&id) else {
            return Ok(());
        };
        let [x, y] = delta.pos.unwrap_or([0, 0]);
        let old_layout = if need_new {
            vk::ImageLayout::UNDEFINED
        } else {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        };
        let gpu = &self.gpu;
        mem::barrier(
            gpu,
            cmd,
            tex.image.image,
            mem::range(0, 1),
            old_layout,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            mem::SHADER_READ,
            mem::TRANSFER_WRITE,
        );
        let region = vk::BufferImageCopy::default()
            .buffer_offset(off)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_offset(vk::Offset3D {
                x: x as i32,
                y: y as i32,
                z: 0,
            })
            .image_extent(vk::Extent3D {
                width: w as u32,
                height: h as u32,
                depth: 1,
            });
        // SAFETY: recording; region lies inside the image.
        unsafe {
            gpu.device.cmd_copy_buffer_to_image(
                cmd,
                buf,
                tex.image.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        };
        mem::barrier(
            gpu,
            cmd,
            tex.image.image,
            mem::range(0, 1),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            mem::TRANSFER_WRITE,
            mem::SHADER_READ,
        );
        Ok(())
    }

    /// Paints egui output into a panel. `pixels_per_point` maps egui points
    /// to panel pixels.
    pub fn paint_panel(
        &mut self,
        panel: PanelId,
        primitives: &[ClippedPrimitive],
        delta: &TexturesDelta,
        pixels_per_point: f32,
    ) -> Result<()> {
        for (id, d) in &delta.set {
            self.upload_egui_texture(panel, *id, d)?;
        }
        let Some(Some(p)) = self.panels.get(panel) else {
            return Ok(());
        };
        let (pw, ph) = (p.image.extent.width, p.image.extent.height);
        let (img, view, mips) = (p.image.image, p.image.level0, p.image.mips);

        // Gather geometry into the slot's buffers.
        let mut draws: Vec<(vk::DescriptorSet, vk::Rect2D, u32, u32, i32)> = Vec::new();
        {
            let slot = &mut self.slots[self.current];
            let mut vbytes = Vec::new();
            let mut ibytes = Vec::new();
            let (mut vcount, mut icount) = (
                (slot.egui_vb_used / 20) as i32,
                (slot.egui_ib_used / 4) as u32,
            );
            for cp in primitives {
                let Primitive::Mesh(mesh) = &cp.primitive else {
                    continue;
                };
                if mesh.indices.is_empty() {
                    continue;
                }
                let set = match p.textures.get(&mesh.texture_id) {
                    Some(t) => t.set,
                    None => self.white.set,
                };
                let r = cp.clip_rect;
                let x0 = (r.min.x * pixels_per_point).round().clamp(0.0, pw as f32) as i32;
                let y0 = (r.min.y * pixels_per_point).round().clamp(0.0, ph as f32) as i32;
                let x1 = (r.max.x * pixels_per_point).round().clamp(0.0, pw as f32) as i32;
                let y1 = (r.max.y * pixels_per_point).round().clamp(0.0, ph as f32) as i32;
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let scissor = vk::Rect2D {
                    offset: vk::Offset2D { x: x0, y: y0 },
                    extent: vk::Extent2D {
                        width: (x1 - x0) as u32,
                        height: (y1 - y0) as u32,
                    },
                };
                vbytes.extend_from_slice(bytemuck::cast_slice(&mesh.vertices));
                ibytes.extend_from_slice(bytemuck::cast_slice(&mesh.indices));
                draws.push((set, scissor, icount, mesh.indices.len() as u32, vcount));
                vcount += mesh.vertices.len() as i32;
                icount += mesh.indices.len() as u32;
            }
            for (buf, used, bytes, usage, name) in [
                (
                    &mut slot.egui_vb,
                    &mut slot.egui_vb_used,
                    &vbytes,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    "egui vertices",
                ),
                (
                    &mut slot.egui_ib,
                    &mut slot.egui_ib_used,
                    &ibytes,
                    vk::BufferUsageFlags::INDEX_BUFFER,
                    "egui indices",
                ),
            ] {
                let need = *used + bytes.len() as u64;
                if need > buf.size {
                    // Grow; geometry already written this frame is copied over.
                    let mut bigger = Buffer::new(
                        &self.gpu,
                        (need * 2).max(buf.size * 2),
                        usage,
                        MemoryLocation::CpuToGpu,
                        name,
                    )?;
                    let keep = *used as usize;
                    bigger.bytes()[..keep].copy_from_slice(&buf.bytes()[..keep]);
                    let old = std::mem::replace(buf, bigger);
                    slot.garbage_buffers.push(old);
                }
                buf.bytes()[*used as usize..need as usize].copy_from_slice(bytes);
                *used = need;
            }
        }
        let slot = &self.slots[self.current];
        let (cmd, gpu) = (slot.cmd, &self.gpu);
        let d = &gpu.device;
        mem::barrier(
            gpu,
            cmd,
            img,
            mem::range(0, 1),
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            mem::SHADER_READ,
            mem::COLOR_WRITE,
        );
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue { float32: [0.0; 4] },
            })];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D {
                width: pw,
                height: ph,
            },
        };
        // SAFETY: recording into the slot's command buffer with valid handles.
        unsafe {
            d.cmd_begin_rendering(
                cmd,
                &vk::RenderingInfo::default()
                    .render_area(area)
                    .layer_count(1)
                    .color_attachments(&attachment),
            );
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.egui_pipe);
            d.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: pw as f32,
                    height: ph as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            let size = [pw as f32 / pixels_per_point, ph as f32 / pixels_per_point];
            d.cmd_push_constants(
                cmd,
                self.egui_pl,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                bytemuck::cast_slice(&size),
            );
            d.cmd_bind_vertex_buffers(cmd, 0, &[slot.egui_vb.buffer], &[0]);
            d.cmd_bind_index_buffer(cmd, slot.egui_ib.buffer, 0, vk::IndexType::UINT32);
            for (set, scissor, first, count, base) in draws {
                d.cmd_set_scissor(cmd, 0, &[scissor]);
                d.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.egui_pl,
                    0,
                    &[set],
                    &[],
                );
                d.cmd_draw_indexed(cmd, count, 1, first, base, 0);
            }
            d.cmd_end_rendering(cmd);
        }
        blit_chain(
            gpu,
            cmd,
            img,
            (pw, ph, mips),
            LevelState::RENDERED,
            LevelState::SAMPLED,
        );
        // Free textures egui no longer needs (after this frame).
        if let Some(Some(p)) = self.panels.get_mut(panel) {
            p.painted = true;
            p.version += 1;
            let slot = &mut self.slots[self.current];
            for id in &delta.free {
                if let Some(t) = p.textures.remove(id) {
                    slot.garbage.push(t.image);
                    slot.garbage_sets.push(t.set);
                }
            }
        }
        Ok(())
    }

    fn update_scene_set(&mut self) {
        if !self.slots[self.current].scene_set_dirty {
            return;
        }
        let dummy = self.dummy.views[0];
        let shared = self.slots[self.current]
            .shared_bound
            .filter(|_| self.slots[self.current].has_video)
            .and_then(|(buffer, _)| self.imports.nv12_views(buffer));
        let views: Vec<vk::ImageView> = match (shared, self.slot_video(self.current)) {
            (Some([y, uv]), _) => vec![y, uv, dummy],
            (None, Some(v)) => {
                let mut vs: Vec<vk::ImageView> = v.images.iter().map(|i| i.views[0]).collect();
                vs.resize(3, dummy);
                vs
            }
            (None, None) => vec![dummy; 3],
        };
        // The video pyramid, shader-readable whether or not this frame builds
        // it (see `PyramidPass::ensure`); the dummy where there is none.
        let pyramid_view = self
            .pyramid
            .as_ref()
            .and_then(PyramidPass::view)
            .unwrap_or(dummy);
        let slot = &mut self.slots[self.current];
        let ubo = [vk::DescriptorBufferInfo::default()
            .buffer(slot.uniform.buffer)
            .range(vk::WHOLE_SIZE)];
        let samplers = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
        let images: Vec<[vk::DescriptorImageInfo; 1]> = views
            .iter()
            .map(|&v| {
                [vk::DescriptorImageInfo::default()
                    .image_view(v)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
            })
            .collect();
        let mut writes = vec![
            vk::WriteDescriptorSet::default()
                .dst_set(slot.scene_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&ubo),
            vk::WriteDescriptorSet::default()
                .dst_set(slot.scene_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&samplers),
        ];
        for (i, info) in images.iter().enumerate() {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(slot.scene_set)
                    .dst_binding(2 + i as u32)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(info),
            );
        }
        let pyramid_info = [vk::DescriptorImageInfo::default()
            .image_view(pyramid_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let aniso = [vk::DescriptorImageInfo::default().sampler(self.aniso_sampler)];
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(slot.scene_set)
                .dst_binding(20)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&pyramid_info),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(slot.scene_set)
                .dst_binding(21)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&aniso),
        );
        // SAFETY: the set is not in use by any pending submission (the slot's
        // fence was waited on in begin_frame).
        unsafe { self.gpu.device.update_descriptor_sets(&writes, &[]) };
        slot.scene_set_dirty = false;
    }

    /// Records both eye passes.
    pub fn draw(
        &mut self,
        targets: &[EyeTarget; 2],
        eyes: &[EyeView; 2],
        video: &VideoParams,
        quads: &[QuadDraw],
    ) -> Result<()> {
        self.stamp(Mark::Eyes);
        let r = self.draw_eyes(targets, eyes, video, quads);
        self.stamp(Mark::EyesDone);
        r
    }

    fn draw_eyes(
        &mut self,
        targets: &[EyeTarget; 2],
        eyes: &[EyeView; 2],
        video: &VideoParams,
        quads: &[QuadDraw],
    ) -> Result<()> {
        let width = targets[0].extent.width.max(targets[1].extent.width);
        let height = targets[0].extent.height.max(targets[1].extent.height);
        let (depth_image, depth_view) = self.depth_for(width, height)?;
        let inv = [
            (eyes[0].proj * eyes[0].view).inverse(),
            (eyes[1].proj * eyes[1].view).inverse(),
        ];
        let slot_idx = self.current;
        let s = &self.slots[slot_idx];
        let frame_desc = match (s.tp10_bound, s.shared_bound) {
            (Some((_, desc)), _) | (None, Some((_, desc))) if s.has_video => Some(desc),
            _ => self.slot_video(slot_idx).map(|v| v.desc),
        };
        let mut p = params::build(video, frame_desc.as_ref(), inv);
        p.hdr = params::hdr_levels(self.hdr_peak.peak_nits());
        p.display[1] = self.quality.bits();
        let tp10 = match (self.slots[slot_idx].tp10_bound, self.tp10_eyes.as_mut()) {
            (Some((buffer, desc)), Some(t)) => self.imports.tp10_picture(buffer).map(|pic| {
                params::sample_tp10(&mut p, desc.height, pic.luma_bands, pic.chroma_bands);
                t.write(slot_idx, &p, pic);
            }),
            _ => None,
        }
        .is_some();
        let env_on = self.prepare_env(slot_idx, video, &p, eyes);
        // VQ5: the video pyramid, if this picture is minified enough to want
        // one. Decided before the scene set is written, so the set points at
        // its image, and before the uniform, which tells the shader about it.
        let footprint = frame_desc.map_or(0.0, |desc| {
            pyramid::scene_footprint(
                &p,
                (desc.width, desc.height),
                crate::math::pixels_per_tan(&eyes[0].proj, targets[0].extent.width),
                eyes[0].view.inverse().w_axis.truncate(),
                video.screen_pose.w_axis.truncate(),
            )
        });
        let plan = self.prepare_pyramid(&p, frame_desc, tp10, footprint);
        p.display[2] = plan.map_or(0, |(_, levels)| levels);
        self.pyramid_levels = p.display[2];
        if tp10 {
            // Its parameters went in before the pyramid's levels were known.
            let view = self
                .pyramid
                .as_ref()
                .and_then(PyramidPass::view)
                .unwrap_or(self.dummy.views[0]);
            if let Some(t) = self.tp10_eyes.as_mut() {
                t.finish(slot_idx, &p, view);
            }
        }
        self.update_scene_set();
        // Quad vertices: two triangles per quad.
        let mut verts: Vec<QuadVertex> = Vec::with_capacity(quads.len() * 6);
        let mut sets = Vec::with_capacity(quads.len());
        for q in quads {
            let set = match q.texture {
                QuadTexture::White => self.white.set,
                QuadTexture::Panel(id) => match self.panels.get(id).and_then(Option::as_ref) {
                    Some(p) => p.set,
                    None => continue,
                },
            };
            let [a, b] = q.uv;
            let uvs = [
                Vec2::new(a.x, a.y),
                Vec2::new(b.x, a.y),
                Vec2::new(b.x, b.y),
                Vec2::new(a.x, b.y),
            ];
            for i in [0usize, 1, 2, 0, 2, 3] {
                verts.push(QuadVertex {
                    pos: q.corners[i].to_array(),
                    uv: uvs[i].to_array(),
                    color: q.color,
                });
            }
            sets.push((set, q.eye));
        }
        {
            let slot = &mut self.slots[slot_idx];
            slot.uniform.bytes()[..size_of::<SceneParams>()]
                .copy_from_slice(bytemuck::bytes_of(&p));
            let bytes: &[u8] = bytemuck::cast_slice(&verts);
            if bytes.len() as u64 > slot.quad_vb.size {
                let bigger = Buffer::new(
                    &self.gpu,
                    bytes.len() as u64 * 2,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    MemoryLocation::CpuToGpu,
                    "quads",
                )?;
                let old = std::mem::replace(&mut slot.quad_vb, bigger);
                slot.garbage_buffers.push(old);
            }
            slot.quad_vb.bytes()[..bytes.len()].copy_from_slice(bytes);
        }
        // Build the pyramid the shader was told to read, unless it holds this
        // frame already (a paused or repeated frame costs nothing). Nothing
        // below can fail, so the pyramid is never marked built and left unbuilt.
        let build = match (plan, self.pyramid.as_ref()) {
            (Some((id, _)), Some(pass)) if !pass.holds(id) => Some(id),
            _ => None,
        };
        if let Some(id) = build {
            self.stamp(Mark::Pyramid);
            let slot = &self.slots[slot_idx];
            let (cmd, set) = (slot.cmd, slot.scene_set);
            let reads = match self.tp10_eyes.as_ref().filter(|_| tp10) {
                Some(eyes) => pyramid::Reads::Tp10 {
                    eyes,
                    slot: slot_idx,
                },
                None => pyramid::Reads::Planes {
                    layout: self.scene_pl,
                    set,
                },
            };
            if let Some(pass) = self.pyramid.as_mut() {
                pass.record(&self.gpu, cmd, reads, id);
            }
            self.stamp(Mark::PyramidDone);
        }
        if env_on {
            self.record_glow(slot_idx, tp10);
        }
        let slot = &self.slots[slot_idx];
        let (gpu, cmd) = (&self.gpu, slot.cmd);
        let d = &gpu.device;
        let bg = video.background;
        for (eye, t) in targets.iter().enumerate() {
            mem::barrier(
                gpu,
                cmd,
                t.image,
                mem::range(0, 1),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                mem::TOP,
                mem::COLOR_WRITE,
            );
            let attachment = [vk::RenderingAttachmentInfo::default()
                .image_view(t.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue { float32: bg },
                })];
            // Both eyes (and every frame) share one depth image: cleared at
            // the start of each pass, never stored, so each pass begins
            // with a transition that discards it.
            mem::barrier(
                gpu,
                cmd,
                depth_image,
                mem::depth_range(),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
                mem::DEPTH_WRITE,
                mem::DEPTH_WRITE,
            );
            let depth = vk::RenderingAttachmentInfo::default()
                .image_view(depth_view)
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .clear_value(vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue {
                        depth: 1.0,
                        stencil: 0,
                    },
                });
            let area = vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: t.extent,
            };
            // SAFETY: recording with valid pipelines, sets and buffers.
            unsafe {
                d.cmd_begin_rendering(
                    cmd,
                    &vk::RenderingInfo::default()
                        .render_area(area)
                        .layer_count(1)
                        .color_attachments(&attachment)
                        .depth_attachment(&depth),
                );
                d.cmd_set_viewport(
                    cmd,
                    0,
                    &[vk::Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: t.extent.width as f32,
                        height: t.extent.height as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    }],
                );
                d.cmd_set_scissor(cmd, 0, &[area]);
                // The place first, with depth: the picture is drawn over it.
                if let Some(env) = self.env.as_ref().filter(|_| env_on) {
                    env.record(gpu, cmd, slot_idx, eye as u32);
                }
                let scene_pl = match self.tp10_eyes.as_ref().filter(|_| tp10) {
                    Some(t) => {
                        t.bind(gpu, cmd, slot_idx);
                        t.layout
                    }
                    None => {
                        d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.scene_pipe);
                        d.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            self.scene_pl,
                            0,
                            &[slot.scene_set],
                            &[],
                        );
                        self.scene_pl
                    }
                };
                d.cmd_push_constants(
                    cmd,
                    scene_pl,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    &(eye as u32).to_le_bytes(),
                );
                d.cmd_draw(cmd, 3, 1, 0, 0);
                if !verts.is_empty() {
                    d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.quad_pipe);
                    let vp = eyes[eye].proj * eyes[eye].view;
                    d.cmd_push_constants(
                        cmd,
                        self.quad_pl,
                        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                        0,
                        bytemuck::bytes_of(&vp),
                    );
                    d.cmd_bind_vertex_buffers(cmd, 0, &[slot.quad_vb.buffer], &[0]);
                    for (i, &(set, only)) in sets.iter().enumerate() {
                        if only.is_some_and(|e| e != eye) {
                            continue;
                        }
                        d.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            self.quad_pl,
                            0,
                            &[set],
                            &[],
                        );
                        d.cmd_draw(cmd, 6, 1, (i * 6) as u32, 0);
                    }
                }
                d.cmd_end_rendering(cmd);
            }
            if t.final_layout != vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL {
                let dst = if t.final_layout == vk::ImageLayout::TRANSFER_SRC_OPTIMAL {
                    mem::TRANSFER_READ
                } else {
                    mem::SHADER_READ
                };
                mem::barrier(
                    gpu,
                    cmd,
                    t.image,
                    mem::range(0, 1),
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    t.final_layout,
                    mem::COLOR_WRITE,
                    dst,
                );
            }
        }
        Ok(())
    }

    /// Decides whether this frame's picture reads the video pyramid
    /// ([`crate::pyramid`]) and makes its image ready: the id of the frame it
    /// is of, and its level count. `footprint` is the estimate of texels per
    /// eye pixel at the screen's middle. Recording the build comes later,
    /// once the scene's parameters are in the uniform buffer.
    fn prepare_pyramid(
        &mut self,
        p: &SceneParams,
        frame: Option<params::FrameDesc>,
        tp10: bool,
        footprint: f32,
    ) -> Option<(u64, u32)> {
        let desc = frame?;
        let id = self.shown_id?;
        let pass = self.pyramid.as_mut()?;
        let slot = &mut self.slots[self.current];
        let source = (desc.width, desc.height);
        let on = self.quality.pyramid && slot.has_video && !pass.left_out(source);
        let flat = p.mode[0] == 0;
        // A picture read in TP10 bands is built from by the eye passes' own
        // descriptors: that needs the pipeline for it.
        let tp10_ready = match self.tp10_eyes.as_mut() {
            Some(t) if tp10 && on => t.ensure_pyramid(&self.gpu, pass.format()),
            _ => false,
        };
        let changed = if pyramid::wanted(on, flat, (tp10, tp10_ready), source, footprint) {
            match pass.ensure(&self.gpu, slot.cmd, &mut slot.garbage, source) {
                Ok(changed) => changed,
                Err(e) => {
                    pass.give_up(source, &e.to_string());
                    return None;
                }
            }
        } else {
            // Switched off or left out: not holding its memory for nothing.
            // Merely not wanted this frame (magnified, say): kept for the next.
            let leave = !self.quality.pyramid || pass.left_out(source);
            if leave && pass.drop_image(&mut slot.garbage) {
                for s in &mut self.slots {
                    s.scene_set_dirty = true;
                }
            }
            return None;
        };
        if changed {
            // Every frame slot's set points at the new image.
            for s in &mut self.slots {
                s.scene_set_dirty = true;
            }
        }
        Some((id, pass.levels()))
    }

    /// Makes the place pass the first time it is wanted. False when places
    /// can't be shown (it, or a place's load, failed, now or before: it is
    /// not tried again, whether or not the pass was made).
    fn ensure_env(&mut self) -> bool {
        if self.env_failed {
            return false;
        }
        if self.env.is_none() {
            match EnvPass::new(&self.gpu, self.color_format, self.depth_format, SLOTS) {
                Ok(pass) => self.env = Some(pass),
                Err(e) => {
                    log::warn!("places aren't shown: {e}");
                    self.env_failed = true;
                }
            }
        }
        self.env.is_some() && !self.env_failed
    }

    /// Builds places on the render thread, in the frame that wants them,
    /// instead of on a worker while Plain shows. For tests and the preview.
    pub fn set_inline_places(&mut self, inline: bool) {
        self.inline_places = inline;
    }

    /// Gets the place this frame draws around the picture ready, if it draws
    /// one: uploads its meshes when it is a new one and they are built (the
    /// frames before that draw none), and writes the slot's uniform. False
    /// when the eye passes draw no place. A spherical or fisheye picture
    /// fills the view and hides it.
    fn prepare_env(
        &mut self,
        slot_idx: usize,
        video: &VideoParams,
        p: &SceneParams,
        eyes: &[EyeView; 2],
    ) -> bool {
        let flat = video.format.projection == Projection::Flat;
        let Some(frame) = video.env.filter(|_| flat) else {
            return false;
        };
        if self.env_failed {
            return false;
        }
        // A place not loaded yet is built off this thread, while the pass's
        // pipelines are made here, and drawn from the frame it is ready.
        let loaded = self.env.as_ref().is_some_and(|e| e.has(frame.place));
        if !loaded && !self.inline_places {
            self.builds.request(frame.place);
        }
        if !self.ensure_env() {
            return false;
        }
        let screen = self.picture_light(slot_idx, frame, video, p);
        let Some(env) = self.env.as_mut() else {
            return false;
        };
        if !env.has(frame.place) {
            let data = if self.inline_places {
                build_timed(Place::build, frame.place)
            } else {
                match self.builds.take(frame.place) {
                    Built::Ready(data) => data,
                    Built::Pending => return false,
                    Built::Failed => {
                        log::warn!("places aren't shown: one wasn't built");
                        self.env_failed = true;
                        return false;
                    }
                }
            };
            let garbage = &mut self.slots[slot_idx].garbage_buffers;
            if let Err(e) = env.load(&self.gpu, frame.place, data, garbage) {
                log::warn!("places aren't shown: {e}");
                self.env_failed = true;
                return false;
            }
        }
        let Some((kind, built_floor)) = env.floor() else {
            return false;
        };
        let room = frame.room_to_world;
        let to_room = room.inverse();
        let room_view_proj = eyes.map(|e| e.proj * e.view * room);
        let eye_pos = eyes.map(|e| {
            let in_world = e.view.inverse().transform_point3(Vec3::ZERO);
            to_room.transform_point3(in_world)
        });
        // A sky place's floor sinks under a big screen. (A room has its
        // own screen and its floor stays.)
        let mut floor_drop = 0.0;
        if kind == PlaceKind::Sky && p.screen.w >= 0.5 {
            let edge = Vec3::new(0.0, -0.5 * p.screen.y, 0.0);
            let bottom = to_room.transform_point3(video.screen_pose.transform_point3(edge));
            floor_drop = sky_floor(built_floor, bottom.y) - built_floor;
        }
        let input = EnvInput {
            room_view_proj,
            eye_pos,
            house: frame.house,
            floor_drop,
            time: frame.time,
            screen,
        };
        match env.uniform(&input) {
            Some(u) => {
                env.write(slot_idx, &u);
                true
            }
            None => false,
        }
    }

    /// Makes the pass that measures the picture the first time a place wants
    /// its light. False when it can't be made (now or before).
    fn ensure_glow(&mut self) -> bool {
        if self.glow.is_none() && !self.glow_failed {
            match ScreenGlow::new(&self.gpu, self.scene_pl, SLOTS) {
                Ok(pass) => self.glow = Some(pass),
                Err(e) => {
                    log::warn!("the picture won't light its place: {e}");
                    self.glow_failed = true;
                }
            }
        }
        self.glow.is_some()
    }

    /// Eases the picture's light on the place for the frame being recorded,
    /// and decides whether this frame measures the picture again
    /// (`glow_due`): only when the frame shown or the colours it passes
    /// through changed, so a paused picture, or a 24 Hz film at 144 Hz, costs
    /// the pass on few frames. `None` when the light is off, there is no
    /// picture, or the pass can't be made: the place gets nothing from it.
    fn picture_light(
        &mut self,
        slot_idx: usize,
        frame: EnvFrame,
        video: &VideoParams,
        p: &SceneParams,
    ) -> Option<LitScreen> {
        self.glow_due = false;
        let showing = p.screen.w >= 0.5 && self.slots[slot_idx].has_video;
        if !(frame.screen_light && showing && self.ensure_glow()) {
            // Black at once; turned on again, it measures afresh.
            self.light.reset();
            self.glow_held = None;
            self.glow_key = None;
            if let Some(g) = self.glow.as_mut() {
                g.discard();
            }
            return None;
        }
        let key = GlowKey::of(self.glow_frame_id, p);
        if self.glow_key != Some(key) {
            self.glow_key = Some(key);
            self.glow_due = true;
        }
        self.light.update(self.glow_held, frame.time);
        let size = Vec2::new(p.screen.x, p.screen.y);
        let to_room = frame.room_to_world.inverse();
        let (patches, normal) = screen_patches(video.screen_pose, size, p.screen.z, to_room);
        Some(LitScreen {
            patches,
            rgb: self.light.rgb,
            normal,
        })
    }

    /// Records the pass that measures the picture, before the eye passes: a
    /// tiny render of the picture's cells with the scene shader's `fs_glow`,
    /// from the scene pipeline's planes or the TP10 pipeline's descriptors,
    /// whichever the eye passes read.
    fn record_glow(&mut self, slot_idx: usize, tp10: bool) {
        if !std::mem::take(&mut self.glow_due) {
            return;
        }
        let ready = match self.tp10_eyes.as_mut() {
            Some(t) if tp10 => t.ensure_glow(&self.gpu),
            _ => true,
        };
        if !ready {
            return;
        }
        self.stamp(Mark::Glow);
        let slot = &self.slots[slot_idx];
        let (gpu, cmd, set) = (&self.gpu, slot.cmd, slot.scene_set);
        let scene_pl = self.scene_pl;
        let tp10_eyes = self.tp10_eyes.as_ref().filter(|_| tp10);
        if let Some(glow) = self.glow.as_mut() {
            glow.record(gpu, cmd, slot_idx, |pipe| match tp10_eyes {
                Some(t) => t.bind_glow(gpu, cmd, slot_idx),
                None => {
                    let d = &gpu.device;
                    // SAFETY: recording; the pipeline and the slot's scene
                    // set are valid, and the set was updated for this frame.
                    unsafe {
                        d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
                        d.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            scene_pl,
                            0,
                            &[set],
                            &[],
                        );
                    }
                    Some(scene_pl)
                }
            });
        }
        self.stamp(Mark::GlowDone);
    }

    /// The colours the picture's light pass measured last, left to right and
    /// top to bottom, linear (zero before the first or with the light off):
    /// for tests.
    #[doc(hidden)]
    pub fn screen_light(&self) -> [[f32; 3]; PATCHES] {
        let held = self.glow_held.unwrap_or([Vec3::ZERO; PATCHES]);
        held.map(|c| [c.x, c.y, c.z])
    }

    /// Loads `data` as the meshes, palette and lighting of `place` instead of
    /// what `place` builds: for tests that draw geometry of their own. A
    /// frame whose `EnvFrame::place` is `place` then shows it.
    #[doc(hidden)]
    pub fn set_place_data(&mut self, place: Place, data: PlaceData) {
        if !self.ensure_env() {
            return;
        }
        let Some(env) = self.env.as_mut() else {
            return;
        };
        let garbage = &mut self.slots[self.current].garbage_buffers;
        if let Err(e) = env.load(&self.gpu, place, data, garbage) {
            log::warn!("places aren't shown: {e}");
            self.env_failed = true;
        }
    }

    /// The depth image both eyes render with, and its view: made, or made
    /// again bigger, when a target is larger than the one there is.
    fn depth_for(&mut self, width: u32, height: u32) -> Result<(vk::Image, vk::ImageView)> {
        let fits = |i: &Image| i.extent.width >= width && i.extent.height >= height;
        if !self.depth.as_ref().is_some_and(fits) {
            let bigger = Image::depth(&self.gpu, width, height, self.depth_format, "eye depth")?;
            // Frames still in flight may use the old one: it goes with the
            // slot's garbage.
            if let Some(old) = self.depth.replace(bigger) {
                self.slots[self.current].garbage.push(old);
            }
        }
        match self.depth.as_ref() {
            Some(i) => Ok((i.image, i.views[0])),
            None => Err(crate::Error::Unsupported("no depth image".into())),
        }
    }

    /// Ends recording and submits the frame.
    pub fn end_frame(&mut self) -> Result<()> {
        let slot = &self.slots[self.current];
        if let Some((buffer, _)) = slot.shared_bound {
            // Acquired in set_video; released to the decoder once read.
            self.imports.release_nv12(&self.gpu, slot.cmd, buffer);
        }
        if let Some((buffer, _)) = slot.tp10_bound {
            self.imports.release_tp10(&self.gpu, slot.cmd, buffer);
        }
        self.stamp(Mark::End);
        let slot = &self.slots[self.current];
        let d = &self.gpu.device;
        // SAFETY: the command buffer is recording; fence is unsignalled after reset.
        unsafe {
            d.end_command_buffer(slot.cmd).ctx("end frame")?;
            d.reset_fences(&[slot.fence]).ctx("reset fence")?;
            let cmds = [slot.cmd];
            d.queue_submit(
                self.gpu.queue,
                &[vk::SubmitInfo::default().command_buffers(&cmds)],
                slot.fence,
            )
            .ctx("submit frame")?;
        }
        self.recording = false;
        Ok(())
    }

    /// How many times a panel has been painted (0: never, or no such panel).
    pub fn panel_version(&self, panel: PanelId) -> u64 {
        match self.panels.get(panel) {
            Some(Some(p)) if p.painted => p.version,
            _ => 0,
        }
    }

    /// Copies a panel's picture into `dst`, an image of the same size and a
    /// compatible RGBA8 format (an OpenXR quad layer's swapchain image, left
    /// in COLOR_ATTACHMENT_OPTIMAL as OpenXR expects). Recorded into this
    /// frame, after the panel's paint; call between `begin_frame` and
    /// `end_frame`.
    pub fn copy_panel_to(&mut self, panel: PanelId, dst: vk::Image) -> Result<()> {
        let Some(Some(p)) = self.panels.get(panel) else {
            return Err(crate::Error::Unsupported("no such panel".into()));
        };
        if !self.recording {
            return Err(crate::Error::Unsupported("copy outside a frame".into()));
        }
        let (src, extent) = (p.image.image, p.image.extent);
        let cmd = self.slots[self.current].cmd;
        let gpu = &self.gpu;
        // The whole image is overwritten: its old contents can go.
        mem::barrier(
            gpu,
            cmd,
            dst,
            mem::range(0, 1),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            mem::TOP,
            mem::TRANSFER_WRITE,
        );
        mem::barrier(
            gpu,
            cmd,
            src,
            mem::range(0, 1),
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            mem::SHADER_READ,
            mem::TRANSFER_READ,
        );
        let layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        };
        let region = vk::ImageCopy::default()
            .src_subresource(layers)
            .dst_subresource(layers)
            .extent(vk::Extent3D {
                width: extent.width,
                height: extent.height,
                depth: 1,
            });
        // SAFETY: recording; both images are extent-sized and in the
        // transfer layouts set above.
        unsafe {
            gpu.device.cmd_copy_image(
                cmd,
                src,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        };
        mem::barrier(
            gpu,
            cmd,
            src,
            mem::range(0, 1),
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            mem::TRANSFER_READ,
            mem::SHADER_READ,
        );
        mem::barrier(
            gpu,
            cmd,
            dst,
            mem::range(0, 1),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            mem::TRANSFER_WRITE,
            mem::COLOR_WRITE,
        );
        Ok(())
    }

    /// Fills `dst` (`width`×`height`, RGBA8, e.g. an OpenXR swapchain image)
    /// with `rgba` and leaves it in COLOR_ATTACHMENT_OPTIMAL. Waits for the
    /// GPU; for small, static pictures.
    pub fn upload_rgba(&self, dst: vk::Image, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
        self.upload_rgba_levels(dst, &[(width, height, rgba)])
    }

    /// Fills the first `levels.len()` mip levels of `dst` (RGBA8, e.g. an
    /// OpenXR swapchain image made with that many mips) with `levels`, each
    /// `(width, height, rgba)` and sized as its level of the image, and
    /// leaves them in COLOR_ATTACHMENT_OPTIMAL. A level the image has beyond
    /// those is left untouched. Waits for the GPU; for small, static
    /// pictures.
    pub fn upload_rgba_levels(&self, dst: vk::Image, levels: &[(u32, u32, &[u8])]) -> Result<()> {
        if levels.is_empty() {
            return Err(crate::Error::Unsupported("no levels to upload".into()));
        }
        let mut offsets = Vec::with_capacity(levels.len());
        let mut total = 0usize;
        for &(width, height, rgba) in levels {
            let want = width as usize * height as usize * 4;
            if rgba.len() != want {
                return Err(crate::Error::Unsupported(format!(
                    "a {width}x{height} level needs {want} bytes, got {}",
                    rgba.len()
                )));
            }
            offsets.push(total);
            total += rgba.len();
        }
        let mut staging = Buffer::new(
            &self.gpu,
            total as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
            "upload",
        )?;
        for (&(_, _, rgba), &at) in levels.iter().zip(&offsets) {
            staging.bytes()[at..at + rgba.len()].copy_from_slice(rgba);
        }
        let buf = staging.buffer;
        let count = levels.len() as u32;
        let r = self.one_shot(|gpu, cmd| {
            mem::barrier(
                gpu,
                cmd,
                dst,
                mem::range(0, count),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                mem::TOP,
                mem::TRANSFER_WRITE,
            );
            let regions: Vec<vk::BufferImageCopy> = levels
                .iter()
                .zip(&offsets)
                .enumerate()
                .map(|(level, (&(width, height, _), &at))| {
                    vk::BufferImageCopy::default()
                        .buffer_offset(at as u64)
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: level as u32,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .image_extent(vk::Extent3D {
                            width,
                            height,
                            depth: 1,
                        })
                })
                .collect();
            // SAFETY: recording; the buffer holds every level's texels at
            // its region's offset, and each level is in TRANSFER_DST.
            unsafe {
                gpu.device.cmd_copy_buffer_to_image(
                    cmd,
                    buf,
                    dst,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &regions,
                );
            };
            mem::barrier(
                gpu,
                cmd,
                dst,
                mem::range(0, count),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                mem::TRANSFER_WRITE,
                mem::COLOR_WRITE,
            );
        });
        staging.destroy(&self.gpu);
        r
    }

    /// Blocks until all submitted frames are done.
    pub fn wait_idle(&self) {
        self.gpu.wait_idle();
    }

    /// Reads a painted panel back as `(width, height, RGBA8)`: sRGB-encoded,
    /// premultiplied alpha, as egui painted it. For screenshots of the UI
    /// (the desktop preview); waits for the GPU.
    pub fn read_panel(&self, panel: PanelId) -> Result<(u32, u32, Vec<u8>)> {
        let Some(Some(p)) = self.panels.get(panel) else {
            return Err(crate::Error::Unsupported("no such panel".into()));
        };
        let (image, extent) = (p.image.image, p.image.extent);
        let mut readback = Buffer::new(
            &self.gpu,
            extent.width as u64 * extent.height as u64 * 4,
            vk::BufferUsageFlags::TRANSFER_DST,
            MemoryLocation::GpuToCpu,
            "panel readback",
        )?;
        let buffer = readback.buffer;
        self.wait_idle();
        self.one_shot(|gpu, cmd| {
            mem::barrier(
                gpu,
                cmd,
                image,
                mem::range(0, 1),
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                mem::SHADER_READ,
                mem::TRANSFER_READ,
            );
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                });
            // SAFETY: level 0 is in TRANSFER_SRC; the buffer holds it.
            unsafe {
                gpu.device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer,
                    &[region],
                );
            };
            mem::barrier(
                gpu,
                cmd,
                image,
                mem::range(0, 1),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                mem::TRANSFER_READ,
                mem::SHADER_READ,
            );
        })?;
        let n = (extent.width * extent.height * 4) as usize;
        let px = readback.bytes()[..n].to_vec();
        readback.destroy(&self.gpu);
        Ok((extent.width, extent.height, px))
    }
}

/// The pipeline that samples 10-bit frames in the decoder's buffers, where
/// the GPU converts Q10C (`has_q10c`) and reads those buffers in place.
/// `FRAMEPLAYERX_TP10_EYES=0` converts them as before, for comparisons.
fn tp10_eyes(
    gpu: &Gpu,
    color_format: vk::Format,
    depth_format: vk::Format,
    (sampler, aniso, dummy): (vk::Sampler, vk::Sampler, vk::ImageView),
    has_q10c: bool,
) -> Option<Tp10Eyes> {
    let allowed = std::env::var("FRAMEPLAYERX_TP10_EYES").map_or(true, |v| v.trim() != "0");
    if !has_q10c || gpu.memory_fd.is_none() || !allowed {
        return None;
    }
    match Tp10Eyes::new(
        gpu,
        color_format,
        depth_format,
        sampler,
        aniso,
        dummy,
        SLOTS,
    ) {
        Ok(t) => {
            log::info!("10-bit HEVC read in place is sampled by the eye passes, not converted");
            Some(t)
        }
        Err(e) => {
            log::warn!("10-bit HEVC read in place is converted: {e}");
            None
        }
    }
}

fn usage_sampled() -> vk::ImageUsageFlags {
    vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST
}

/// What `blit_chain` finds a mip level in: its layout, and the stage and
/// access of the last use that has to finish before the level is written
/// (or, for level 0, before it is read).
#[derive(Clone, Copy)]
pub(crate) struct LevelState {
    pub layout: vk::ImageLayout,
    pub last_use: (vk::PipelineStageFlags2, vk::AccessFlags2),
}

impl LevelState {
    /// Just rendered into.
    pub const RENDERED: LevelState = LevelState {
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        last_use: mem::COLOR_WRITE,
    };
    /// Read by shaders, as a panel's lower levels are from the last paint.
    pub const SAMPLED: LevelState = LevelState {
        layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        last_use: mem::SHADER_READ,
    };
    /// Contents not wanted, but earlier shader reads of them (by frames still
    /// in flight) must finish before they are written.
    pub const DISCARDED: LevelState = LevelState {
        layout: vk::ImageLayout::UNDEFINED,
        last_use: mem::SHADER_READ,
    };
}

/// Blits level 0 (in state `first`) down the mip chain, the other levels
/// starting in state `rest`, and leaves every level shader-readable.
pub(crate) fn blit_chain(
    gpu: &Gpu,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    (w, h, mips): (u32, u32, u32),
    first: LevelState,
    rest: LevelState,
) {
    let d = &gpu.device;
    mem::barrier(
        gpu,
        cmd,
        image,
        mem::range(0, 1),
        first.layout,
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        first.last_use,
        mem::TRANSFER_READ,
    );
    let (mut mw, mut mh) = (w as i32, h as i32);
    for level in 1..mips {
        let (nw, nh) = ((mw / 2).max(1), (mh / 2).max(1));
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(level, 1),
            rest.layout,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            rest.last_use,
            mem::TRANSFER_WRITE,
        );
        let sub = |l| vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: l,
            base_array_layer: 0,
            layer_count: 1,
        };
        let blit = vk::ImageBlit {
            src_subresource: sub(level - 1),
            src_offsets: [vk::Offset3D::default(), vk::Offset3D { x: mw, y: mh, z: 1 }],
            dst_subresource: sub(level),
            dst_offsets: [vk::Offset3D::default(), vk::Offset3D { x: nw, y: nh, z: 1 }],
        };
        // SAFETY: recording; levels are in the right layouts.
        unsafe {
            d.cmd_blit_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );
        };
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(level, 1),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            mem::TRANSFER_WRITE,
            mem::TRANSFER_READ,
        );
        mw = nw;
        mh = nh;
    }
    mem::barrier(
        gpu,
        cmd,
        image,
        mem::range(0, mips),
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        mem::TRANSFER_READ,
        mem::SHADER_READ,
    );
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.gpu.wait_idle();
        let gpu = self.gpu.clone();
        let d = &gpu.device;
        // SAFETY: the device is idle; everything below was created by us.
        unsafe {
            for v in &mut self.videos {
                if let Some(mut v) = v.take() {
                    v.destroy(&gpu);
                }
            }
            if let Some(mut c) = self.q10c.take() {
                c.destroy(&gpu);
            }
            if let Some(mut p) = self.peak.take() {
                p.destroy(&gpu);
            }
            if let Some(mut e) = self.env.take() {
                e.destroy(&gpu);
            }
            if let Some(mut g) = self.glow.take() {
                g.destroy(&gpu);
            }
            if let Some(mut t) = self.timer.take() {
                t.destroy(&gpu);
            }
            if let Some(mut t) = self.tp10_eyes.take() {
                t.destroy(&gpu);
            }
            if let Some(mut p) = self.pyramid.take() {
                p.destroy(&gpu);
            }
            self.imports.destroy(&gpu);
            for mut s in self.slots.drain(..) {
                for mut i in s.garbage.drain(..) {
                    i.destroy(&gpu);
                }
                for mut i in s.garbage_imports.drain(..) {
                    i.destroy(&gpu);
                }
                for mut b in s.garbage_buffers.drain(..) {
                    b.destroy(&gpu);
                }
                for mut b in [s.uniform, s.quad_vb, s.egui_vb, s.egui_ib, s.staging] {
                    b.destroy(&gpu);
                }
                d.destroy_fence(s.fence, None);
            }
            for p in self.panels.drain(..).flatten() {
                let mut img = p.image;
                img.destroy(&gpu);
                for (_, mut t) in p.textures {
                    t.image.destroy(&gpu);
                }
            }
            if let Some(mut i) = self.depth.take() {
                i.destroy(&gpu);
            }
            self.white.image.destroy(&gpu);
            self.dummy.destroy(&gpu);
            for p in [self.scene_pipe, self.quad_pipe, self.egui_pipe] {
                d.destroy_pipeline(p, None);
            }
            for l in [self.scene_pl, self.quad_pl, self.egui_pl] {
                d.destroy_pipeline_layout(l, None);
            }
            d.destroy_descriptor_set_layout(self.scene_layout, None);
            d.destroy_descriptor_set_layout(self.tex_layout, None);
            d.destroy_sampler(self.sampler, None);
            d.destroy_sampler(self.aniso_sampler, None);
            d.destroy_descriptor_pool(self.dpool, None);
            d.destroy_command_pool(self.pool, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A renderer on a software device, or `None` where there is none (the
    /// test then skips).
    fn renderer() -> Option<Renderer> {
        let gpu = match Gpu::headless() {
            Ok(g) => Arc::new(g),
            Err(e) => {
                eprintln!("skipping: no Vulkan device ({e})");
                return None;
            }
        };
        Some(Renderer::new(gpu, vk::Format::R8G8B8A8_SRGB).expect("renderer"))
    }

    fn floor(r: &Renderer) -> Option<(PlaceKind, f32)> {
        r.env.as_ref().and_then(EnvPass::floor)
    }

    #[test]
    fn a_failed_place_pass_is_not_tried_again() {
        let Some(mut r) = renderer() else {
            return;
        };
        // Working: the pass is made and a load goes through.
        assert!(r.ensure_env());
        r.set_place_data(Place::Cinema, Place::Cinema.build());
        assert!(floor(&r).is_some(), "the control loaded");
        // A load that failed marks places failed while the pass is still
        // there: nothing more is loaded, so nothing more can fail and say so
        // every frame.
        r.env_failed = true;
        assert!(!r.ensure_env());
        r.set_place_data(Place::Studio, Place::Studio.build());
        let loaded = r.env.as_ref().map(|e| e.has(Place::Studio));
        assert_eq!(loaded, Some(false), "the studio was not loaded");
        assert!(r.env.as_ref().is_some_and(|e| e.has(Place::Cinema)));
    }

    #[test]
    fn no_pass_is_made_once_places_have_failed() {
        let Some(mut r) = renderer() else {
            return;
        };
        r.env_failed = true;
        assert!(!r.ensure_env());
        assert!(r.env.is_none());
    }
}
