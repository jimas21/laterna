//! Draws a place (a cinema, a studio) inside each eye's pass: its meshes
//! (shaders/env.wgsl) with depth testing and writing, then its sky
//! (shaders/sky.wgsl) at the far plane, so the sky shades only what no mesh
//! covered. The picture is drawn over all of it afterwards, with no depth
//! test (places are built so nothing stands in front of it).
//!
//! Self-contained like [`crate::peak::PeakPass`]: its own descriptor pool, set
//! layout and pipelines, one uniform buffer per frame slot, and the vertex
//! and index buffers of the place that is loaded (written once, shared by the
//! slots; replaced ones go to a slot's garbage). Made on the first frame that
//! shows a place, so users who never pick one pay nothing.

use crate::env::light::{PATCHES, screen_rect};
use crate::env::pack::BakedVertex;
use crate::env::{Lighting, Pattern, Place, PlaceData, PlaceKind, SeatName, Sky, Vertex};
use crate::env_baked::{self, BakedGpu, LIGHT_FORMAT};
use crate::gpu::Gpu;
use crate::mem::Buffer;
use crate::perf_ab::Knobs;
use crate::pipeline::{self, Depth, PipelineDesc};
use crate::place_sort::{self, PlaceSorter};
use crate::{Result, VkContext};
use ash::vk;
use glam::{Mat4, Vec2, Vec3, Vec4};
use gpu_allocator::MemoryLocation;
use std::sync::Arc;

/// Materials a place has, at most (`palette` in envcommon.wgsl).
const MATERIALS: usize = 16;

/// `struct Env` in envcommon.wgsl: every member is a vec4 or mat4, so the
/// uniform block's layout is this struct's.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct EnvUniform {
    pub room_view_proj: [Mat4; 2],
    pub inv_room_view_proj: [Mat4; 2],
    pub eye_pos: [Vec4; 2],
    pub patch_pos: [Vec4; PATCHES],
    pub patch_rgb: [Vec4; PATCHES],
    pub screen_n: Vec4,
    pub ambient_up: Vec4,
    pub ambient_down: Vec4,
    pub house_rgb: Vec4,
    pub fog: Vec4,
    pub fog_range: Vec4,
    pub sky_zenith: Vec4,
    pub sky_horizon: Vec4,
    pub palette: [Vec4; MATERIALS],
    pub palette2: [Vec4; MATERIALS],
    /// A baked place: x its basis gain, y the picture's light on (1) or off
    /// (0), z the basis read anisotropically, w the colours read
    /// anisotropically.
    pub baked: Vec4,
    /// The picture as a rectangle, for the highlight of a baked place: xyz
    /// its middle, w half its width.
    pub screen_c: Vec4,
    /// xyz the unit vector along its width, w half its height.
    pub screen_r: Vec4,
    /// x: the highlight on (1) or off (0), y: its strength (`SHINE_GAIN`).
    pub shine: Vec4,
    /// The screen's masking: xyz the frame's middle and w on (1) or off (0);
    /// xy the frame's half size, zw the half size of the opening in it.
    pub mask_c: Vec4,
    pub mask_h: Vec4,
}

/// The black masking round a film on a room's screen: the screen frame (the
/// largest picture the room shows) with an opening that is the film's own
/// shape when closed and the whole frame when open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScreenMask {
    /// The frame's middle, room space.
    pub centre: Vec3,
    pub frame_half: Vec2,
    pub hole_half: Vec2,
}

/// The masking for a flat screen of `frame_max` centred at `centre` showing a
/// picture of `picture` (width and height, metres), `open` of the way from
/// the picture's shape (0) to the whole frame (1). The picture is never made
/// smaller by it: the opening is never smaller than the picture, and the
/// picture's size is the room's own fit. `None` for a curved screen, no
/// picture, or sizes that are not numbers.
pub(crate) fn screen_mask(
    centre: Vec3,
    frame_max: Vec2,
    curvature: f32,
    picture: Vec2,
    open: f32,
) -> Option<ScreenMask> {
    let ok = |v: Vec2| v.is_finite() && v.x > 0.0 && v.y > 0.0;
    if curvature > 1e-4 || !ok(frame_max) || !ok(picture) || !centre.is_finite() {
        return None;
    }
    let open = if open.is_finite() {
        open.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let frame_half = frame_max / 2.0;
    let picture_half = picture / 2.0;
    let hole_half = picture_half + (frame_half - picture_half).max(Vec2::ZERO) * open;
    Some(ScreenMask {
        centre,
        frame_half,
        hole_half,
    })
}

/// How strongly a baked place's highlight is drawn (`shine.y`): 1 is the
/// Fresnel reflectance of the picture's colour times what the material map
/// lets through, which is already faint (a dielectric reflects 4% facing it).
pub(crate) const SHINE_GAIN: f32 = 1.0;

/// What the collapse draws the picture's light with: each patch's colour
/// times the light's strength (`env_collapse.wgsl`).
type Weights = [Vec3; PATCHES];

/// A change in a weight smaller than this, as a share of it, is left out of
/// the collapsed light until it has grown (the light is compared with the
/// weights it was last drawn with, so nothing is lost, only late): under half
/// a code value of the picture's light in the final colour, whatever its
/// level.
const COLLAPSE_RELATIVE: f32 = 0.004;
/// The same for weights near zero, linear light.
const COLLAPSE_ABSOLUTE: f32 = 1.0 / 8192.0;

/// The weights `u` asks for, or `None` when its picture's light is not read
/// (the light off, a basis gain of 0, or the knob).
fn collapse_weights(u: &EnvUniform) -> Option<Weights> {
    let strength = u.baked.x * u.screen_n.w;
    (u.baked.y > 0.5 && strength > 0.0).then(|| u.patch_rgb.map(|c| c.truncate() * strength))
}

/// Whether `new` differs from `old`, which the collapsed light was drawn
/// with, by more than the tolerance in any weight.
fn weights_moved(old: &Weights, new: &Weights) -> bool {
    old.iter().zip(new).any(|(a, b)| {
        let (a, b) = (a.to_array(), b.to_array());
        a.iter().zip(b).any(|(&a, b)| {
            (a - b).abs() > COLLAPSE_ABSOLUTE.max(COLLAPSE_RELATIVE * a.abs().max(b.abs()))
        })
    })
}

/// The picture's light on the place this frame, in room space.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LitScreen {
    /// xyz the middle of each cell of the screen, w its area in m^2.
    pub patches: [Vec4; PATCHES],
    /// Each cell's linear colour, eased.
    pub rgb: [Vec3; PATCHES],
    /// The screen's normal towards the audience.
    pub normal: Vec3,
    /// How strongly it lights the place: 1 the strength the place is tuned
    /// for (the Screen light slider's start), 0 off.
    pub gain: f32,
}

/// The least light, linear and neutral, any cell of the picture gives the
/// place while its light is on, so a dark scene (or a black frame between
/// two shots) never leaves the room completely black. About what a very dark
/// grey scene measures.
pub(crate) const LIGHT_FLOOR: f32 = 0.03;

/// `SCREEN_BOOST` in env.wgsl.
#[cfg(test)]
const SCREEN_BOOST: f32 = 5.0;

/// What changes from frame to frame.
pub(crate) struct EnvInput {
    /// Clip <- room space, for each eye.
    pub room_view_proj: [Mat4; 2],
    /// Each eye in room space.
    pub eye_pos: [Vec3; 2],
    /// House-light level, 0 dark to 1 full.
    pub house: f32,
    /// How strongly the place's own lights glow (exit signs, step lights):
    /// 0 off, 1 as built. A projector's window is not one of them.
    pub accent: f32,
    /// How far a sky place's floor is lowered (zero or below).
    pub floor_drop: f32,
    /// Seconds.
    pub time: f64,
    /// The masking round the film, if it is drawn.
    pub mask: Option<ScreenMask>,
    /// The picture's light, if it lights the place.
    pub screen: Option<LitScreen>,
}

/// How bright a sky place's backdrop (and its fog, which fades to it) is at a
/// house-light level: the ambient light of a sky place is half house light,
/// so the sky is too.
pub(crate) fn sky_level(house: f32) -> f32 {
    0.5 + 0.5 * house.clamp(0.0, 1.0)
}

/// The pattern's code in the palette's w (the constants in env.wgsl).
fn pattern_code(p: Pattern) -> u32 {
    match p {
        Pattern::Plain => 0,
        Pattern::Carpet => 1,
        Pattern::Fabric => 2,
        Pattern::Panel => 3,
        Pattern::Wood => 4,
        Pattern::Screen => 5,
        Pattern::Emissive => 6,
        Pattern::EmissiveHouse => 7,
        Pattern::EmissivePicture => 8,
    }
}

/// What a loaded place needs besides its meshes to fill the uniform.
struct Look {
    palette: [Vec4; MATERIALS],
    palette2: [Vec4; MATERIALS],
    lighting: Lighting,
    sky: Sky,
    /// A baked place's gain on its basis textures, else 0.
    baked_gain: f32,
}

fn vec4(c: [f32; 3], w: f32) -> Vec4 {
    Vec4::new(c[0], c[1], c[2], w)
}

impl Look {
    fn new(data: &PlaceData) -> Look {
        let mut palette = [Vec4::ZERO; MATERIALS];
        let mut palette2 = [Vec4::ZERO; MATERIALS];
        for (i, m) in data.palette.iter().take(MATERIALS).enumerate() {
            palette[i] = vec4(m.albedo, pattern_code(m.pattern) as f32);
            palette2[i] = Vec4::new(m.emissive, m.sheen, 0.0, 0.0);
        }
        Look {
            palette,
            palette2,
            lighting: data.lighting,
            sky: data.sky,
            baked_gain: data.baked.as_ref().map_or(0.0, |b| b.basis_gain),
        }
    }

    fn uniform(&self, input: &EnvInput) -> EnvUniform {
        let house = input.house.clamp(0.0, 1.0);
        let dim = sky_level(house);
        let l = &self.lighting;
        let mut u: EnvUniform = bytemuck::Zeroable::zeroed();
        u.room_view_proj = input.room_view_proj;
        u.inv_room_view_proj = input.room_view_proj.map(|m| m.inverse());
        u.eye_pos = input.eye_pos.map(|p| p.extend(1.0));
        u.ambient_up = vec4(l.ambient_up, house);
        u.ambient_down = vec4(l.ambient_down, house);
        u.house_rgb = vec4(l.house_rgb.map(|c| c * house), house);
        if let Some(fog) = l.fog {
            u.fog = vec4(fog.rgb.map(|c| c * dim), 1.0);
            u.fog_range.x = fog.start;
            u.fog_range.y = fog.end;
        }
        if let Some(s) = input.screen.as_ref().filter(|s| s.gain > 0.0) {
            u.patch_pos = s.patches;
            for (out, c) in u.patch_rgb.iter_mut().zip(s.rgb) {
                *out = vec4(c.to_array().map(|v| (v * l.spill).max(LIGHT_FLOOR)), 0.0);
            }
            // w: how strongly the picture's light lands here; 0 is off.
            u.screen_n = s.normal.extend(s.gain);
            if let Some(rect) = screen_rect(&s.patches) {
                u.screen_c = rect.centre.extend(rect.half.x);
                u.screen_r = rect.right.extend(rect.half.y);
            }
        }
        u.fog_range.z = input.floor_drop;
        u.fog_range.w = (input.time % 3600.0) as f32;
        if let Sky::Gradient { zenith, horizon } = self.sky {
            u.sky_zenith = vec4(zenith.map(|c| c * dim), 1.0);
            u.sky_horizon = vec4(horizon.map(|c| c * dim), 1.0);
        }
        if let Some(m) = input.mask {
            u.mask_c = m.centre.extend(1.0);
            u.mask_h = Vec4::new(m.frame_half.x, m.frame_half.y, m.hole_half.x, m.hole_half.y);
        }
        u.palette = self.palette;
        u.palette2 = self.palette2;
        let accent = if input.accent.is_finite() {
            input.accent.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if accent < 1.0 {
            for (code, extra) in self.palette.iter().zip(u.palette2.iter_mut()) {
                let code = code.w as u32;
                if code == pattern_code(Pattern::Emissive)
                    || code == pattern_code(Pattern::EmissiveHouse)
                {
                    extra.x *= accent;
                }
            }
        }
        u.baked = Vec4::new(self.baked_gain, 0.0, 0.0, 0.0);
        u
    }
}

/// The place whose meshes are on the GPU.
struct Loaded {
    place: Place,
    /// A pack with textures for each seat: the seat they were baked for.
    set: Option<SeatName>,
    kind: PlaceKind,
    /// The floor's height as the place was built.
    floor_y: f32,
    vertices: Buffer,
    indices: Buffer,
    index_count: u32,
    look: Look,
    /// A pack's textures and sets: it is drawn with the baked pipeline.
    baked: Option<BakedGpu>,
    /// Which pack this is, for the sorter's answers (they are for one).
    generation: u64,
    /// A pack's indices ordered nearest the viewer first, and how many,
    /// once the sorter has made them.
    sorted: Option<(Buffer, u32)>,
    /// The eye (room space) `sorted` was made for.
    sorted_eye: Option<Vec3>,
}

struct SlotEnv {
    uniform: Buffer,
    set: vk::DescriptorSet,
}

pub(crate) struct EnvPass {
    pool: vk::DescriptorPool,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    env_pipe: vk::Pipeline,
    sky_pipe: vk::Pipeline,
    slots: Vec<SlotEnv>,
    loaded: Option<Loaded>,
    /// What the baked pipeline is made with, when a pack first needs it.
    target: (vk::Format, vk::Format, vk::SampleCountFlags),
    baked_set_layout: vk::DescriptorSetLayout,
    baked_layout: vk::PipelineLayout,
    baked_pipe: vk::Pipeline,
    /// The baked pipeline with `fs_lit`, which reads the picture's light as
    /// one texture, and the draw that makes that texture (null when either
    /// could not be made: the eight reads are used then).
    lit_pipe: vk::Pipeline,
    collapse_pipe: vk::Pipeline,
    /// The picture's light is collapsed into one texture and read from it
    /// (the default), not read from the eight basis textures.
    collapse: bool,
    /// Levels of the basis textures' chain the collapsed light starts down,
    /// for the packs loaded from now on.
    collapse_shift: u32,
    /// The weights the collapsed light holds, once it does, and whether the
    /// next frame has to draw it again.
    collapsed: Option<Weights>,
    /// The weights the next collapse draw is for; they become `collapsed`
    /// only once that draw was recorded.
    collapse_pending: Option<Weights>,
    collapse_due: bool,
    /// Collapses recorded since the pack was loaded.
    collapse_runs: u64,
    /// The knobs (perf_ab.rs): the picture's light on a baked place, its
    /// anisotropic reads, and the nearest-first order.
    basis: bool,
    basis_aniso: bool,
    /// A baked place's colour textures read anisotropically (the default).
    place_aniso: bool,
    /// A baked place's highlight from the picture drawn (the default).
    shine: bool,
    sort: bool,
    /// Whether the order is made at all (it is when the knob or the A/B
    /// cycle can want it).
    sort_work: bool,
    sorter: Option<PlaceSorter>,
    /// The generation of the pack that is loaded.
    generation: u64,
    /// The eye the order was last asked for.
    ordered_for: Option<Vec3>,
}

fn attribute(
    location: u32,
    format: vk::Format,
    offset: usize,
) -> vk::VertexInputAttributeDescription {
    vk::VertexInputAttributeDescription::default()
        .location(location)
        .binding(0)
        .format(format)
        .offset(offset as u32)
}

/// `VsIn` in env_baked.wgsl, from the layout of [`BakedVertex`].
fn baked_attributes() -> [vk::VertexInputAttributeDescription; 2] {
    use std::mem::offset_of;
    [
        attribute(
            0,
            vk::Format::R32G32B32_SFLOAT,
            offset_of!(BakedVertex, pos),
        ),
        attribute(1, vk::Format::R32G32_SFLOAT, offset_of!(BakedVertex, uv)),
    ]
}

/// `VsIn` in env.wgsl, from the layout of [`Vertex`].
fn vertex_attributes() -> [vk::VertexInputAttributeDescription; 6] {
    use std::mem::offset_of;
    let float3 = vk::Format::R32G32B32_SFLOAT;
    [
        attribute(0, float3, offset_of!(Vertex, pos)),
        attribute(1, float3, offset_of!(Vertex, normal)),
        attribute(2, vk::Format::R32G32_SFLOAT, offset_of!(Vertex, uv)),
        attribute(3, vk::Format::R32_SFLOAT, offset_of!(Vertex, ao)),
        attribute(4, vk::Format::R32_SFLOAT, offset_of!(Vertex, house)),
        attribute(5, vk::Format::R32_UINT, offset_of!(Vertex, material)),
    ]
}

impl EnvPass {
    /// The pipelines, drawing into `color_format` with a depth attachment of
    /// `depth` in the instance, `samples` samples a pixel, and a uniform
    /// buffer for each of `slots` frame slots.
    pub fn new(
        gpu: &Gpu,
        color_format: vk::Format,
        depth: vk::Format,
        samples: vk::SampleCountFlags,
        slots: usize,
    ) -> Result<EnvPass> {
        let mut p = EnvPass {
            pool: vk::DescriptorPool::null(),
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            env_pipe: vk::Pipeline::null(),
            sky_pipe: vk::Pipeline::null(),
            slots: Vec::new(),
            loaded: None,
            target: (color_format, depth, samples),
            baked_set_layout: vk::DescriptorSetLayout::null(),
            baked_layout: vk::PipelineLayout::null(),
            baked_pipe: vk::Pipeline::null(),
            lit_pipe: vk::Pipeline::null(),
            collapse_pipe: vk::Pipeline::null(),
            collapse: true,
            collapse_shift: env_baked::DEFAULT_COLLAPSE_SHIFT,
            collapsed: None,
            collapse_pending: None,
            collapse_due: false,
            collapse_runs: 0,
            basis: true,
            basis_aniso: false,
            place_aniso: true,
            shine: true,
            sort: true,
            sort_work: true,
            sorter: None,
            generation: 0,
            ordered_for: None,
        };
        if let Err(e) = p.create(gpu, depth, samples, slots) {
            p.destroy(gpu);
            return Err(e);
        }
        Ok(p)
    }

    fn create(
        &mut self,
        gpu: &Gpu,
        depth: vk::Format,
        samples: vk::SampleCountFlags,
        slots: usize,
    ) -> Result<()> {
        let d = &gpu.device;
        self.set_layout = pipeline::set_layout(gpu, &[(0, vk::DescriptorType::UNIFORM_BUFFER)])?;
        self.layout = pipeline::layout(gpu, self.set_layout, 4)?;
        (self.env_pipe, self.sky_pipe) = self.make_pipes(gpu, depth, samples)?;
        let n = slots as u32;
        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: n,
        }];
        // SAFETY: plain object creation on a valid device; everything made
        // here is destroyed by `destroy`.
        let pool = unsafe {
            d.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(n)
                    .pool_sizes(&sizes),
                None,
            )
        }
        .ctx("place descriptor pool")?;
        self.pool = pool;
        for _ in 0..slots {
            let layouts = [self.set_layout];
            // SAFETY: the pool has room for one set per slot.
            let set = unsafe {
                d.allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.pool)
                        .set_layouts(&layouts),
                )
            }
            .ctx("place descriptor set")?[0];
            let uniform = Buffer::new(
                gpu,
                size_of::<EnvUniform>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                MemoryLocation::CpuToGpu,
                "place uniform",
            )?;
            let info = [vk::DescriptorBufferInfo::default()
                .buffer(uniform.buffer)
                .range(size_of::<EnvUniform>() as u64)];
            let write = [vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&info)];
            // SAFETY: the set is not in use yet; the buffer is alive.
            unsafe { d.update_descriptor_sets(&write, &[]) };
            self.slots.push(SlotEnv { uniform, set });
        }
        Ok(())
    }

    /// The built-in rooms' mesh and sky pipelines at `samples` samples a
    /// pixel (drawing into the target's formats).
    fn make_pipes(
        &self,
        gpu: &Gpu,
        depth: vk::Format,
        samples: vk::SampleCountFlags,
    ) -> Result<(vk::Pipeline, vk::Pipeline)> {
        let color_format = self.target.0;
        let attributes = vertex_attributes();
        let env_pipe = pipeline::create(
            gpu,
            &PipelineDesc {
                spv: crate::ENV_SPV,
                layout: self.layout,
                color_format,
                vertex_stride: size_of::<Vertex>() as u32,
                attributes: &attributes,
                blend: false,
                descriptor_buffer: false,
                depth: Depth::TestWrite(depth),
                samples,
            },
        )?;
        let sky = pipeline::create(
            gpu,
            &PipelineDesc {
                spv: crate::SKY_SPV,
                layout: self.layout,
                color_format,
                vertex_stride: 0,
                attributes: &[],
                blend: false,
                descriptor_buffer: false,
                depth: Depth::Test(depth),
                samples,
            },
        );
        match sky {
            Ok(sky_pipe) => Ok((env_pipe, sky_pipe)),
            Err(e) => {
                // SAFETY: just made, never bound.
                unsafe { gpu.device.destroy_pipeline(env_pipe, None) };
                Err(e)
            }
        }
    }

    /// The baked pipelines at `samples` samples a pixel, in the layout made
    /// by `ensure_baked_pipe`: the one with the eight basis reads, and the
    /// one that reads the collapsed light (null where that one could not be
    /// made).
    fn make_baked_pipes(
        &self,
        gpu: &Gpu,
        depth: vk::Format,
        samples: vk::SampleCountFlags,
    ) -> Result<(vk::Pipeline, vk::Pipeline)> {
        let color_format = self.target.0;
        let attributes = baked_attributes();
        let desc = PipelineDesc {
            spv: crate::ENV_BAKED_SPV,
            layout: self.baked_layout,
            color_format,
            vertex_stride: size_of::<BakedVertex>() as u32,
            attributes: &attributes,
            blend: false,
            descriptor_buffer: false,
            depth: Depth::TestWrite(depth),
            samples,
        };
        let baked = pipeline::create(gpu, &desc)?;
        let lit = pipeline::create_entry(gpu, &desc, c"fs_lit").unwrap_or_else(|e| {
            log::warn!("the picture's light is read from eight textures: {e}");
            vk::Pipeline::null()
        });
        Ok((baked, lit))
    }

    /// The draw that collapses the picture's light into one texture: a
    /// fullscreen triangle into [`LIGHT_FORMAT`], whatever the eye passes
    /// are drawn in. Null where it could not be made.
    fn make_collapse_pipe(&self, gpu: &Gpu) -> vk::Pipeline {
        pipeline::create(
            gpu,
            &PipelineDesc {
                spv: crate::ENV_COLLAPSE_SPV,
                layout: self.baked_layout,
                color_format: LIGHT_FORMAT,
                vertex_stride: 0,
                attributes: &[],
                blend: false,
                descriptor_buffer: false,
                depth: Depth::None,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )
        .unwrap_or_else(|e| {
            log::warn!("the picture's light is read from eight textures: {e}");
            vk::Pipeline::null()
        })
    }

    /// Makes the baked pipeline the first time a pack is loaded: users with
    /// no pack never pay for it.
    fn ensure_baked_pipe(&mut self, gpu: &Gpu) -> Result<()> {
        if self.baked_pipe != vk::Pipeline::null() {
            return Ok(());
        }
        let (depth, samples) = (self.target.1, self.target.2);
        self.baked_set_layout = pipeline::set_layout(gpu, &env_baked::BINDINGS)?;
        self.baked_layout = pipeline::layout(gpu, self.baked_set_layout, 4)?;
        (self.baked_pipe, self.lit_pipe) = self.make_baked_pipes(gpu, depth, samples)?;
        self.collapse_pipe = self.make_collapse_pipe(gpu);
        Ok(())
    }

    /// Makes the pipelines again for a depth attachment of `depth` and
    /// `samples` samples a pixel (the eye passes' count or depth format
    /// changed). The device must be idle. Nothing changes when it fails.
    pub fn set_target(
        &mut self,
        gpu: &Gpu,
        depth: vk::Format,
        samples: vk::SampleCountFlags,
    ) -> Result<()> {
        if (depth, samples) == (self.target.1, self.target.2) {
            return Ok(());
        }
        let (env_pipe, sky_pipe) = self.make_pipes(gpu, depth, samples)?;
        let baked = if self.baked_pipe == vk::Pipeline::null() {
            Ok((vk::Pipeline::null(), vk::Pipeline::null()))
        } else {
            self.make_baked_pipes(gpu, depth, samples)
        };
        let (baked_pipe, lit_pipe) = match baked {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: just made, never bound.
                unsafe {
                    gpu.device.destroy_pipeline(env_pipe, None);
                    gpu.device.destroy_pipeline(sky_pipe, None);
                }
                return Err(e);
            }
        };
        // SAFETY: the device is idle; the old pipelines are ours.
        unsafe {
            gpu.device.destroy_pipeline(self.env_pipe, None);
            gpu.device.destroy_pipeline(self.sky_pipe, None);
            gpu.device.destroy_pipeline(self.baked_pipe, None);
            gpu.device.destroy_pipeline(self.lit_pipe, None);
        }
        (self.env_pipe, self.sky_pipe, self.baked_pipe) = (env_pipe, sky_pipe, baked_pipe);
        self.lit_pipe = lit_pipe;
        (self.target.1, self.target.2) = (depth, samples);
        Ok(())
    }

    /// Takes the knobs the environment set (perf_ab.rs).
    pub fn set_knobs(&mut self, knobs: &Knobs) {
        self.basis = knobs.basis;
        self.basis_aniso = knobs.basis_aniso;
        self.place_aniso = knobs.place_aniso;
        self.shine = knobs.shine;
        self.sort = knobs.sort;
        // The order is worth making when something can want it.
        self.sort_work = knobs.sort || knobs.ab;
        self.collapse = knobs.collapse;
        self.collapse_shift = knobs
            .collapse_shift
            .unwrap_or(env_baked::DEFAULT_COLLAPSE_SHIFT);
        self.collapsed = None;
    }

    /// The picture's light read from one collapsed texture, or from the eight
    /// basis textures (the A/B cycle). The collapsed texture is drawn afresh
    /// when it is turned on.
    pub fn set_collapse(&mut self, on: bool) {
        self.collapse = on;
        self.collapsed = None;
    }

    /// The collapsed light of the packs loaded from now on starts `shift`
    /// levels down the basis textures' chain (for tests).
    pub fn set_collapse_shift(&mut self, shift: u32) {
        self.collapse_shift = shift;
    }

    /// How many times the collapsed light has been drawn since the knobs
    /// were last set or a pack loaded (for tests and the log).
    pub fn collapse_runs(&self) -> u64 {
        self.collapse_runs
    }

    /// Whether the loaded baked place is drawn from the collapsed light:
    /// the knob is on and the textures and pipelines for it exist.
    fn reads_collapsed(&self) -> bool {
        self.collapse
            && self.lit_pipe != vk::Pipeline::null()
            && self.collapse_pipe != vk::Pipeline::null()
            && self
                .loaded
                .as_ref()
                .and_then(|l| l.baked.as_ref())
                .is_some_and(BakedGpu::has_light)
    }

    /// Notes the weights slot `slot`'s uniform `u` asks the collapse for and
    /// whether the collapsed light has to be drawn again before the next eye
    /// pass: when it was never drawn for the light now on, or a weight has
    /// moved past the tolerance since it was.
    fn plan_collapse(&mut self, u: &EnvUniform) {
        let Some(new) = self
            .reads_collapsed()
            .then(|| collapse_weights(u))
            .flatten()
        else {
            // Not read: it will be drawn afresh when it is.
            self.collapsed = None;
            self.collapse_pending = None;
            self.collapse_due = false;
            return;
        };
        if self
            .collapsed
            .as_ref()
            .is_none_or(|old| weights_moved(old, &new))
        {
            self.collapse_pending = Some(new);
            self.collapse_due = true;
        }
    }

    /// Records the draw of the collapsed light into `cmd`, outside any
    /// rendering and before the eye passes, if the last written uniform
    /// called for it. True when it did.
    pub fn record_collapse(&mut self, gpu: &Gpu, cmd: vk::CommandBuffer, slot: usize) -> bool {
        if !std::mem::take(&mut self.collapse_due) || !self.reads_collapsed() {
            return false;
        }
        let drawn = self
            .loaded
            .as_ref()
            .and_then(|l| l.baked.as_ref())
            .is_some_and(|b| {
                b.record_collapse(gpu, cmd, slot, self.collapse_pipe, self.baked_layout)
            });
        if drawn {
            // Only a draw that was recorded makes these the light on show.
            self.collapsed = self.collapse_pending.take();
        } else {
            // Not drawn: it is still owed, so ask again next frame.
            self.collapse_due = self.collapse_pending.is_some();
        }
        self.collapse_runs += u64::from(drawn);
        drawn
    }

    /// The picture's light on a baked place, on or off (the A/B cycle).
    pub fn set_basis(&mut self, on: bool) {
        self.basis = on;
    }

    /// A baked place's colour textures read anisotropically, or trilinear
    /// (the A/B cycle).
    pub fn set_place_aniso(&mut self, on: bool) {
        self.place_aniso = on;
    }

    /// A baked place's highlight from the picture, on or off (the A/B
    /// cycle, `FRAMEPLAYERX_PLACE_SHINE`).
    pub fn set_shine(&mut self, on: bool) {
        self.shine = on;
    }

    /// A baked place drawn nearest first, or in its pack's order (the A/B
    /// cycle); the order is still kept up to date while it is off.
    pub fn set_sort(&mut self, on: bool) {
        self.sort = on;
    }

    /// Keeps the loaded pack's draw order for a viewer at `eye` (room
    /// space): takes the order the sorter has finished, if any, and asks for
    /// a new one when the eye has moved on from the last (a seat change).
    /// The sorting is on another thread; the order replaced goes to
    /// `garbage`, the recording slot's.
    pub fn update_order(&mut self, gpu: &Gpu, eye: Vec3, garbage: &mut Vec<Buffer>) {
        if !self.sort_work {
            return;
        }
        let Some(l) = self.loaded.as_mut().filter(|l| l.baked.is_some()) else {
            return;
        };
        let Some(sorter) = self.sorter.as_ref() else {
            return;
        };
        while let Some(done) = sorter.poll() {
            if done.generation != l.generation {
                continue;
            }
            log::debug!("place order made for an eye at {}", done.eye);
            let bytes: &[u8] = bytemuck::cast_slice(&done.indices);
            let made = Buffer::new(
                gpu,
                bytes.len() as u64,
                vk::BufferUsageFlags::INDEX_BUFFER,
                MemoryLocation::CpuToGpu,
                "place sorted indices",
            );
            match made {
                Ok(mut b) => {
                    b.bytes()[..bytes.len()].copy_from_slice(bytes);
                    if let Some((old, _)) = l.sorted.replace((b, done.indices.len() as u32)) {
                        garbage.push(old);
                    }
                    l.sorted_eye = Some(done.eye);
                }
                Err(e) => {
                    log::warn!("the place is drawn unsorted: {e}");
                    self.sort_work = false;
                    return;
                }
            }
        }
        if place_sort::needs_order(self.ordered_for, eye) {
            self.ordered_for = Some(eye);
            sorter.sort(l.generation, eye);
        }
    }

    /// Whether the sorter has answered for the loaded pack, so that it is
    /// drawn nearest first (when the sort is on). For tests.
    pub fn has_sorted_order(&self) -> bool {
        self.loaded.as_ref().is_some_and(|l| l.sorted.is_some())
    }

    /// The eye (room space) the loaded pack's drawn order was made for, once
    /// the sorter has answered. For tests, which wait for a new seat's order.
    pub fn sorted_eye(&self) -> Option<Vec3> {
        self.loaded.as_ref().and_then(|l| l.sorted_eye)
    }

    /// Whether `place` is the one loaded, and for a seat-by-seat pack the set
    /// baked for `set` (`None` for everything else). A place loaded with no
    /// set of a seat (the built-in room, a pack with one set for everybody,
    /// or the data a test supplied) stands for every seat. (The renderer gets
    /// it built off the render thread and `load`s it when it is ready.)
    pub fn has(&self, place: Place, set: Option<SeatName>) -> bool {
        self.loaded
            .as_ref()
            .is_some_and(|l| l.place == place && (l.set.is_none() || l.set == set))
    }

    /// Uploads `data`, which is what `place.build()` makes (or, for tests
    /// that draw their own geometry, whatever they like), as `place`. The
    /// meshes it replaces may still be read by a frame in flight: they go to
    /// `garbage`, the recording slot's, which is emptied when that slot's
    /// fence has signalled again.
    pub fn load(
        &mut self,
        gpu: &Gpu,
        place: Place,
        mut data: PlaceData,
        garbage: &mut Vec<Buffer>,
    ) -> Result<()> {
        // A material the palette does not have reads the last one, never
        // past the uniform's end.
        let last = MATERIALS as u32 - 1;
        for v in &mut data.vertices {
            v.material = v.material.min(last);
        }
        // A pack brings its own mesh in its own vertex layout, and textures.
        let (vertex_bytes, index_bytes): (&[u8], &[u8]) = match &data.baked {
            Some(b) => (
                bytemuck::cast_slice(&b.vertices),
                bytemuck::cast_slice(&b.indices),
            ),
            None => (
                bytemuck::cast_slice(&data.vertices),
                bytemuck::cast_slice(&data.indices),
            ),
        };
        let index_count = (index_bytes.len() / size_of::<u32>()) as u32;
        let pack: Option<Arc<crate::env::pack::Baked>> = data.baked.clone();
        let baked = match &data.baked {
            Some(b) => {
                self.ensure_baked_pipe(gpu)?;
                let uniforms: Vec<(vk::Buffer, u64)> = self
                    .slots
                    .iter()
                    .map(|s| (s.uniform.buffer, size_of::<EnvUniform>() as u64))
                    .collect();
                Some(BakedGpu::new(
                    gpu,
                    self.baked_set_layout,
                    &uniforms,
                    b,
                    self.collapse_shift,
                )?)
            }
            None => None,
        };
        let mut vertices = Buffer::new(
            gpu,
            vertex_bytes.len() as u64,
            vk::BufferUsageFlags::VERTEX_BUFFER,
            MemoryLocation::CpuToGpu,
            "place vertices",
        )?;
        let mut indices = match Buffer::new(
            gpu,
            index_bytes.len() as u64,
            vk::BufferUsageFlags::INDEX_BUFFER,
            MemoryLocation::CpuToGpu,
            "place indices",
        ) {
            Ok(b) => b,
            Err(e) => {
                vertices.destroy(gpu);
                if let Some(mut b) = baked {
                    gpu.wait_idle();
                    b.destroy(gpu);
                }
                return Err(e);
            }
        };
        vertices.bytes()[..vertex_bytes.len()].copy_from_slice(vertex_bytes);
        indices.bytes()[..index_bytes.len()].copy_from_slice(index_bytes);
        let loaded = Loaded {
            place,
            set: data.baked.as_ref().and_then(|b| b.seat),
            kind: place.kind(),
            floor_y: place.layout().floor_y,
            vertices,
            indices,
            index_count,
            look: Look::new(&data),
            baked,
            generation: self.generation + 1,
            sorted: None,
            sorted_eye: None,
        };
        self.generation += 1;
        self.ordered_for = None;
        // The new pack's light texture is black until it is drawn.
        self.collapsed = None;
        self.collapse_pending = None;
        self.collapse_due = false;
        self.collapse_runs = 0;
        if let Some(pack) = pack.filter(|_| self.sort_work) {
            if self.sorter.is_none() {
                self.sorter = PlaceSorter::new();
            }
            if let Some(s) = self.sorter.as_ref() {
                s.load(self.generation, pack);
            }
        }
        if let Some(mut old) = self.loaded.replace(loaded) {
            garbage.push(old.vertices);
            garbage.push(old.indices);
            if let Some((b, _)) = old.sorted.take() {
                garbage.push(b);
            }
            // Textures and sets can't wait for a slot's fence like the
            // buffers: a frame in flight may still be reading them. A pack
            // being replaced is rare, so the device goes idle once.
            if let Some(mut b) = old.baked.take() {
                gpu.wait_idle();
                b.destroy(gpu);
            }
        }
        Ok(())
    }

    /// Whether the loaded place is a room or a sky, and the height of its
    /// floor as built.
    pub fn floor(&self) -> Option<(PlaceKind, f32)> {
        self.loaded.as_ref().map(|l| (l.kind, l.floor_y))
    }

    /// The uniform for the loaded place this frame.
    pub fn uniform(&self, input: &EnvInput) -> Option<EnvUniform> {
        self.loaded.as_ref().map(|l| {
            let mut u = l.look.uniform(input);
            if l.baked.is_some() {
                u.baked.y = f32::from(self.basis);
                u.baked.z = f32::from(self.basis_aniso);
                u.baked.w = f32::from(self.place_aniso);
                let accent = if input.accent.is_finite() {
                    input.accent.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                u.shine = Vec4::new(f32::from(self.shine), SHINE_GAIN, accent, 0.0);
            }
            u
        })
    }

    /// Writes slot `slot`'s uniform. Its fence has been waited on.
    pub fn write(&mut self, slot: usize, u: &EnvUniform) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.uniform.bytes()[..size_of::<EnvUniform>()].copy_from_slice(bytemuck::bytes_of(u));
            self.plan_collapse(u);
        }
    }

    /// Records the loaded place for `eye` into `cmd`, inside the eye's
    /// rendering: the meshes, then the sky.
    pub fn record(&self, gpu: &Gpu, cmd: vk::CommandBuffer, slot: usize, eye: u32) {
        let (Some(l), Some(s)) = (self.loaded.as_ref(), self.slots.get(slot)) else {
            return;
        };
        let d = &gpu.device;
        let bind = vk::PipelineBindPoint::GRAPHICS;
        if let Some(set) = l.baked.as_ref().and_then(|b| b.set(slot)) {
            // A pack: one pipeline, no sky (the room is all there is). The
            // one that reads the collapsed light, unless that is off.
            let pipe = if self.reads_collapsed() {
                self.lit_pipe
            } else {
                self.baked_pipe
            };
            // SAFETY: as below; the baked sets point at the same uniforms.
            unsafe {
                d.cmd_bind_pipeline(cmd, bind, pipe);
                d.cmd_bind_descriptor_sets(cmd, bind, self.baked_layout, 0, &[set], &[]);
                d.cmd_push_constants(
                    cmd,
                    self.baked_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    &eye.to_le_bytes(),
                );
                // Nearest first once the sorter has the order for this pack.
                let (indices, count) = match l.sorted.as_ref().filter(|_| self.sort) {
                    Some((b, n)) => (b.buffer, *n),
                    None => (l.indices.buffer, l.index_count),
                };
                if count > 0 {
                    d.cmd_bind_vertex_buffers(cmd, 0, &[l.vertices.buffer], &[0]);
                    d.cmd_bind_index_buffer(cmd, indices, 0, vk::IndexType::UINT32);
                    d.cmd_draw_indexed(cmd, count, 1, 0, 0, 0);
                }
            }
            return;
        }
        // SAFETY: recording with valid pipelines, sets and buffers; the
        // slot's uniform was written before this frame was submitted.
        unsafe {
            d.cmd_bind_descriptor_sets(cmd, bind, self.layout, 0, &[s.set], &[]);
            d.cmd_push_constants(
                cmd,
                self.layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                &eye.to_le_bytes(),
            );
            if l.index_count > 0 {
                d.cmd_bind_pipeline(cmd, bind, self.env_pipe);
                d.cmd_bind_vertex_buffers(cmd, 0, &[l.vertices.buffer], &[0]);
                d.cmd_bind_index_buffer(cmd, l.indices.buffer, 0, vk::IndexType::UINT32);
                d.cmd_draw_indexed(cmd, l.index_count, 1, 0, 0, 0);
            }
            if matches!(l.look.sky, Sky::Gradient { .. }) {
                d.cmd_bind_pipeline(cmd, bind, self.sky_pipe);
                d.cmd_draw(cmd, 3, 1, 0, 0);
            }
        }
    }

    /// Frees everything. The device must be idle.
    pub fn destroy(&mut self, gpu: &Gpu) {
        let d = &gpu.device;
        // SAFETY: the device is idle; these are ours, and destroying a null
        // handle does nothing.
        unsafe {
            d.destroy_pipeline(self.env_pipe, None);
            d.destroy_pipeline(self.sky_pipe, None);
            d.destroy_pipeline(self.baked_pipe, None);
            d.destroy_pipeline(self.lit_pipe, None);
            d.destroy_pipeline(self.collapse_pipe, None);
            d.destroy_pipeline_layout(self.layout, None);
            d.destroy_pipeline_layout(self.baked_layout, None);
            d.destroy_descriptor_set_layout(self.set_layout, None);
            d.destroy_descriptor_set_layout(self.baked_set_layout, None);
            d.destroy_descriptor_pool(self.pool, None);
        }
        for mut s in self.slots.drain(..) {
            s.uniform.destroy(gpu);
        }
        if let Some(mut l) = self.loaded.take() {
            l.vertices.destroy(gpu);
            l.indices.destroy(gpu);
            if let Some((mut b, _)) = l.sorted.take() {
                b.destroy(gpu);
            }
            if let Some(mut b) = l.baked.take() {
                b.destroy(gpu);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::light::{COLS, ROWS};
    use std::mem::offset_of;

    fn input(house: f32) -> EnvInput {
        EnvInput {
            room_view_proj: [Mat4::IDENTITY; 2],
            eye_pos: [Vec3::new(-0.03, 1.0, 2.0), Vec3::new(0.03, 1.0, 2.0)],
            house,
            accent: 1.0,
            floor_drop: 0.0,
            time: 12.5,
            mask: None,
            screen: None,
        }
    }

    #[test]
    fn the_uniform_has_the_shaders_std140_layout() {
        assert_eq!(offset_of!(EnvUniform, room_view_proj), 0);
        assert_eq!(offset_of!(EnvUniform, inv_room_view_proj), 128);
        assert_eq!(offset_of!(EnvUniform, eye_pos), 256);
        assert_eq!(offset_of!(EnvUniform, patch_pos), 288);
        assert_eq!(offset_of!(EnvUniform, patch_rgb), 416);
        assert_eq!(offset_of!(EnvUniform, screen_n), 544);
        assert_eq!(offset_of!(EnvUniform, ambient_up), 560);
        assert_eq!(offset_of!(EnvUniform, fog), 608);
        assert_eq!(offset_of!(EnvUniform, fog_range), 624);
        assert_eq!(offset_of!(EnvUniform, sky_horizon), 656);
        assert_eq!(offset_of!(EnvUniform, palette), 672);
        assert_eq!(offset_of!(EnvUniform, palette2), 928);
        assert_eq!(offset_of!(EnvUniform, baked), 1184);
        assert_eq!(offset_of!(EnvUniform, screen_c), 1200);
        assert_eq!(offset_of!(EnvUniform, screen_r), 1216);
        assert_eq!(offset_of!(EnvUniform, shine), 1232);
        assert_eq!(offset_of!(EnvUniform, mask_c), 1248);
        assert_eq!(offset_of!(EnvUniform, mask_h), 1264);
        assert_eq!(size_of::<EnvUniform>(), 1280);
    }

    #[test]
    fn the_vertex_attributes_follow_the_vertex_layout() {
        let offsets: Vec<u32> = vertex_attributes().iter().map(|a| a.offset).collect();
        assert_eq!(offsets, [0, 12, 24, 32, 36, 40]);
        let locations: Vec<u32> = vertex_attributes().iter().map(|a| a.location).collect();
        assert_eq!(locations, [0, 1, 2, 3, 4, 5]);
        assert_eq!(size_of::<Vertex>(), 44);
    }

    #[test]
    fn the_baked_vertex_attributes_follow_the_pack_vertex_layout() {
        let a = baked_attributes();
        assert_eq!([a[0].location, a[0].offset], [0, 0]);
        assert_eq!([a[1].location, a[1].offset], [1, 12]);
        // mesh.bin's vertices are these 20 bytes.
        assert_eq!(size_of::<BakedVertex>(), 20);
    }

    #[test]
    fn a_packs_basis_gain_reaches_the_uniform_and_a_built_in_room_has_none() {
        let data = Place::Cinema.build_builtin();
        assert_eq!(Look::new(&data).uniform(&input(1.0)).baked, Vec4::ZERO);
        let none = || crate::env::pack::TextureArray {
            size: 4,
            encoding: crate::env::ktx2::Encoding::Rgba8,
            layers: Vec::new(),
        };
        let baked = crate::env::pack::Baked {
            vertices: Vec::new(),
            indices: Vec::new(),
            colors: none(),
            basis: none(),
            material: None,
            basis_gain: 2.5,
            seat: None,
        };
        let mut data = data;
        data.baked = Some(Arc::new(baked));
        let u = Look::new(&data).uniform(&input(1.0));
        assert_eq!(u.baked.x, 2.5);
    }

    #[test]
    fn pattern_codes_match_the_shader() {
        let shader = include_str!("../shaders/env.wgsl");
        let named = [
            ("CARPET", Pattern::Carpet),
            ("FABRIC", Pattern::Fabric),
            ("PANEL", Pattern::Panel),
            ("WOOD", Pattern::Wood),
            ("EMISSIVE", Pattern::Emissive),
            ("EMISSIVE_HOUSE", Pattern::EmissiveHouse),
            ("EMISSIVE_PICTURE", Pattern::EmissivePicture),
        ];
        for (name, pattern) in named {
            let line = format!("const {name}: u32 = {}u;", pattern_code(pattern));
            assert!(shader.contains(&line), "env.wgsl lacks `{line}`");
        }
        // Emissive patterns are the ones at and above EMISSIVE.
        for &place in Place::ALL {
            for m in place.build().palette {
                let emissive = pattern_code(m.pattern) >= pattern_code(Pattern::Emissive);
                assert_eq!(emissive, m.pattern.is_emissive());
            }
        }
    }

    #[test]
    fn the_palette_carries_albedo_pattern_glow_and_sheen() {
        let data = Place::Cinema.build();
        let u = Look::new(&data).uniform(&input(1.0));
        for (i, m) in data.palette.iter().enumerate() {
            assert_eq!(u.palette[i].truncate().to_array(), m.albedo);
            assert_eq!(u.palette[i].w, pattern_code(m.pattern) as f32);
            assert_eq!(u.palette2[i].x, m.emissive);
            assert_eq!(u.palette2[i].y, m.sheen);
        }
        // Slots past the palette are black, plain and dark.
        let spare = u.palette[data.palette.len()];
        assert_eq!(spare, Vec4::ZERO);
    }

    #[test]
    fn a_weight_must_move_a_share_of_itself_to_draw_the_collapse_again() {
        let w = |v: f32| [Vec3::splat(v); PATCHES];
        // Not moved at all, or by a thousandth of itself.
        assert!(!weights_moved(&w(0.5), &w(0.5)));
        assert!(!weights_moved(&w(0.5), &w(0.5005)));
        // By a percent: drawn again.
        assert!(weights_moved(&w(0.5), &w(0.505)));
        // One patch, one channel is enough.
        let mut one = w(0.5);
        one[5].y = 0.52;
        assert!(weights_moved(&w(0.5), &one));
        // Near zero the absolute floor decides, not the share.
        assert!(!weights_moved(&w(0.0), &w(0.00005)));
        assert!(weights_moved(&w(0.0), &w(0.001)));
    }

    #[test]
    fn the_collapse_asks_for_the_weights_only_while_the_light_is_read() {
        let mut u: EnvUniform = bytemuck::Zeroable::zeroed();
        u.baked.x = 0.5;
        // No picture: the strength is zero.
        assert!(collapse_weights(&u).is_none());
        u.screen_n.w = 2.0;
        u.patch_rgb[3] = Vec4::new(0.1, 0.2, 0.3, 0.0);
        // The pass sets this one from its knob.
        assert!(collapse_weights(&u).is_none());
        u.baked.y = 1.0;
        let w = collapse_weights(&u).expect("read");
        // The gain 0.5 times the strength 2.
        assert_eq!(w[3], Vec3::new(0.1, 0.2, 0.3));
        u.baked.x = 0.25;
        assert_eq!(
            collapse_weights(&u).expect("read")[3],
            Vec3::new(0.05, 0.1, 0.15)
        );
    }

    #[test]
    fn the_masking_closes_to_the_films_shape_and_never_shrinks_the_picture() {
        let frame = Vec2::new(14.0, 7.2);
        let at = |picture: Vec2, open: f32| {
            screen_mask(Vec3::new(0.0, 4.0, 0.0), frame, 0.0, picture, open).unwrap()
        };
        // 2.39, 1.85 and 16:9 films, as the room fits them (by width, or by
        // height when the film is taller than the frame).
        for picture in [
            Vec2::new(14.0, 14.0 / 2.39),
            Vec2::new(7.2 * 1.85, 7.2),
            Vec2::new(7.2 * 16.0 / 9.0, 7.2),
        ] {
            let closed = at(picture, 0.0);
            assert_eq!(
                closed.hole_half,
                picture / 2.0,
                "closed is the film's shape"
            );
            assert_eq!(closed.frame_half, frame / 2.0);
            let open = at(picture, 1.0);
            assert_eq!(open.hole_half, frame / 2.0, "open is the whole screen");
            let mid = at(picture, 0.5);
            assert!(mid.hole_half.cmpge(picture / 2.0).all());
            assert!(mid.hole_half.cmple(frame / 2.0).all());
        }
        // A picture that somehow overfills the frame is not cut.
        let big = at(Vec2::new(20.0, 10.0), 0.0);
        assert_eq!(big.hole_half, Vec2::new(10.0, 5.0));
        assert_eq!(at(Vec2::new(20.0, 10.0), 1.0).hole_half, big.hole_half);
        // Curved screens, nothing to show and nonsense get no masking.
        let c = Vec3::ZERO;
        assert!(screen_mask(c, frame, 0.3, Vec2::ONE, 0.0).is_none());
        assert!(screen_mask(c, frame, 0.0, Vec2::ZERO, 0.0).is_none());
        assert!(screen_mask(c, frame, 0.0, Vec2::new(f32::NAN, 1.0), 0.0).is_none());
        assert_eq!(
            screen_mask(c, frame, 0.0, Vec2::ONE, f32::NAN)
                .unwrap()
                .hole_half,
            frame / 2.0
        );
    }

    #[test]
    fn the_house_level_scales_the_baked_light_and_dims_the_sky() {
        let data = Place::Studio.build();
        let look = Look::new(&data);
        let (off, full) = (look.uniform(&input(0.0)), look.uniform(&input(1.0)));
        assert_eq!(off.house_rgb.truncate(), Vec3::ZERO);
        assert_eq!(
            full.house_rgb.truncate().to_array(),
            data.lighting.house_rgb
        );
        assert_eq!((off.ambient_up.w, full.ambient_up.w), (0.0, 1.0));
        // The ambient light is the same; the sky and its fog halve.
        assert_eq!(off.ambient_up.truncate(), full.ambient_up.truncate());
        assert!((off.sky_zenith.x - full.sky_zenith.x * 0.5).abs() < 1e-6);
        assert!((off.fog.x - full.fog.x * 0.5).abs() < 1e-6);
        assert_eq!(full.fog.w, 1.0);
        assert_eq!(full.sky_zenith.w, 1.0);
        // Levels outside 0 to 1 are clamped.
        assert_eq!(look.uniform(&input(7.0)).ambient_up.w, 1.0);
    }

    #[test]
    fn a_room_without_fog_or_sky_leaves_them_off() {
        let look = Look::new(&Place::Cinema.build());
        let u = look.uniform(&input(1.0));
        assert_eq!(u.fog.w, 0.0);
        assert_eq!(u.sky_zenith, Vec4::ZERO);
        assert_eq!(u.sky_horizon, Vec4::ZERO);
    }

    #[test]
    fn accent_lights_dim_with_the_scale_but_the_picture_window_does_not() {
        let data = Place::Cinema.build_builtin();
        let look = Look::new(&data);
        let at = |scale: f32| {
            let mut i = input(1.0);
            i.accent = scale;
            look.uniform(&i)
        };
        let full = at(1.0);
        let mut changed = 0;
        let mut kept_picture = 0;
        for (k, m) in data.palette.iter().enumerate() {
            let (f, off, half) = (
                full.palette2[k].x,
                at(0.0).palette2[k].x,
                at(0.5).palette2[k].x,
            );
            match m.pattern {
                Pattern::Emissive | Pattern::EmissiveHouse => {
                    assert!(f > 0.0);
                    assert_eq!(off, 0.0, "{k}");
                    assert!((half - f * 0.5).abs() < 1e-6, "{k}");
                    changed += 1;
                }
                Pattern::EmissivePicture => {
                    assert_eq!(off, f, "the projector's window follows the picture");
                    kept_picture += 1;
                }
                _ => assert_eq!(off, f),
            }
            // The rest of the material is left alone.
            assert_eq!(at(0.0).palette2[k].y, full.palette2[k].y);
        }
        assert!(changed >= 2 && kept_picture >= 1);
        // Out of range or not a number is full or off, never a surprise.
        assert_eq!(at(f32::NAN).palette2, full.palette2);
        assert_eq!(at(7.0).palette2, full.palette2);
        assert_eq!(at(-1.0).palette2, at(0.0).palette2);
    }

    #[test]
    fn the_floor_drop_and_the_eyes_reach_the_uniform() {
        let look = Look::new(&Place::Studio.build());
        let mut i = input(1.0);
        i.floor_drop = -2.5;
        let u = look.uniform(&i);
        assert_eq!(u.fog_range.z, -2.5);
        assert_eq!(u.fog_range.w, 12.5);
        assert_eq!(u.eye_pos[1], Vec4::new(0.03, 1.0, 2.0, 1.0));
        let id = Mat4::IDENTITY;
        assert_eq!(u.inv_room_view_proj, [id; 2]);
    }

    #[test]
    fn the_pictures_light_reaches_the_uniform_and_off_leaves_it_zero() {
        let look = Look::new(&Place::Cinema.build());
        let off = look.uniform(&input(1.0));
        assert_eq!(off.patch_pos, [Vec4::ZERO; PATCHES]);
        assert_eq!(off.patch_rgb, [Vec4::ZERO; PATCHES]);
        assert_eq!(off.screen_n, Vec4::ZERO);

        let mut i = input(1.0);
        let patches = std::array::from_fn(|k| Vec4::new(k as f32, 4.0, 0.0, 1.5));
        let rgb = std::array::from_fn(|k| Vec3::new(0.1, 0.2, 0.4) * (k + 1) as f32);
        i.screen = Some(LitScreen {
            patches,
            rgb,
            normal: Vec3::Z,
            gain: 1.0,
        });
        let on = look.uniform(&i);
        assert_eq!(on.patch_pos, patches);
        let spill = Place::Cinema.build().lighting.spill;
        for (k, (got, want)) in on.patch_rgb.iter().zip(rgb).enumerate() {
            assert!(got.truncate().abs_diff_eq(want * spill, 1e-6), "{k}");
        }
        assert_eq!(on.screen_n, Vec4::new(0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn the_slider_scales_the_light_and_a_dark_scene_keeps_a_floor() {
        let look = Look::new(&Place::Cinema.build());
        let lit = |rgb: Vec3, gain: f32| {
            let mut i = input(1.0);
            i.screen = Some(LitScreen {
                patches: [Vec4::new(0.0, 4.0, 0.0, 1.0); PATCHES],
                rgb: [rgb; PATCHES],
                normal: Vec3::Z,
                gain,
            });
            look.uniform(&i)
        };
        // The strength reaches the shader in screen_n.w, and not at all
        // (nothing lit) at 0, as with no screen.
        assert_eq!(lit(Vec3::splat(0.5), 1.5).screen_n.w, 1.5);
        let off = lit(Vec3::splat(0.5), 0.0);
        assert_eq!(off.screen_n, Vec4::ZERO);
        assert_eq!(off.patch_rgb, [Vec4::ZERO; PATCHES]);
        // A black picture still gives every cell the floor, in grey; a cell
        // brighter than it is left as measured.
        let dark = lit(Vec3::new(0.0, 0.001, 0.0), 1.0);
        for c in dark.patch_rgb {
            assert_eq!(c.truncate(), Vec3::splat(LIGHT_FLOOR));
        }
        let bright = lit(Vec3::new(0.8, 0.02, 0.2), 1.0);
        assert!(
            bright.patch_rgb[0]
                .truncate()
                .abs_diff_eq(Vec3::new(0.8, LIGHT_FLOOR, 0.2), 1e-6)
        );
    }

    #[test]
    fn the_shader_boost_is_the_one_here() {
        let shader = include_str!("../shaders/env.wgsl");
        let line = format!("const SCREEN_BOOST: f32 = {SCREEN_BOOST:?};");
        assert!(shader.contains(&line), "env.wgsl lacks `{line}`");
    }

    #[test]
    fn there_are_as_many_patches_as_the_shader_loops_over() {
        let shader = include_str!("../shaders/envcommon.wgsl");
        let line = format!("patch_pos: array<vec4<f32>, {PATCHES}>");
        assert!(shader.contains(&line), "envcommon.wgsl lacks `{line}`");
        let env_shader = include_str!("../shaders/env.wgsl");
        let loops = env_shader.matches("i < 8u").count();
        assert_eq!(loops, 3, "env.wgsl loops over the patches three times");
        let glow = include_str!("../shaders/scene.wgsl");
        for (name, n) in [("GLOW_COLS", COLS), ("GLOW_ROWS", ROWS)] {
            let line = format!("const {name}: u32 = {n}u;");
            assert!(glow.contains(&line), "scene.wgsl lacks `{line}`");
        }
    }

    #[test]
    fn the_sky_is_brighter_with_the_lights_up() {
        assert_eq!(sky_level(1.0), 1.0);
        assert_eq!(sky_level(0.0), 0.5);
        assert!(sky_level(0.04) < sky_level(0.5));
        assert_eq!(sky_level(-3.0), 0.5);
    }
}
