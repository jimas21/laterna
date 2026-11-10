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

use crate::env::light::PATCHES;
use crate::env::{Lighting, Pattern, Place, PlaceData, PlaceKind, Sky, Vertex};
use crate::gpu::Gpu;
use crate::mem::Buffer;
use crate::pipeline::{self, Depth, PipelineDesc};
use crate::{Result, VkContext};
use ash::vk;
use glam::{Mat4, Vec3, Vec4};
use gpu_allocator::MemoryLocation;

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
}

/// What changes from frame to frame.
pub(crate) struct EnvInput {
    /// Clip <- room space, for each eye.
    pub room_view_proj: [Mat4; 2],
    /// Each eye in room space.
    pub eye_pos: [Vec3; 2],
    /// House-light level, 0 dark to 1 full.
    pub house: f32,
    /// How far a sky place's floor is lowered (zero or below).
    pub floor_drop: f32,
    /// Seconds.
    pub time: f64,
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
        if let Some(s) = &input.screen {
            u.patch_pos = s.patches;
            for (out, c) in u.patch_rgb.iter_mut().zip(s.rgb) {
                *out = vec4(c.to_array().map(|v| v * l.spill), 0.0);
            }
            // w: how much of the picture's light lands here; 0 is off.
            u.screen_n = s.normal.extend(l.spill);
        }
        u.fog_range.z = input.floor_drop;
        u.fog_range.w = (input.time % 3600.0) as f32;
        if let Sky::Gradient { zenith, horizon } = self.sky {
            u.sky_zenith = vec4(zenith.map(|c| c * dim), 1.0);
            u.sky_horizon = vec4(horizon.map(|c| c * dim), 1.0);
        }
        u.palette = self.palette;
        u.palette2 = self.palette2;
        u
    }
}

/// The place whose meshes are on the GPU.
struct Loaded {
    place: Place,
    kind: PlaceKind,
    /// The floor's height as the place was built.
    floor_y: f32,
    vertices: Buffer,
    indices: Buffer,
    index_count: u32,
    look: Look,
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
    /// `depth` in the instance, and a uniform buffer for each of `slots`
    /// frame slots.
    pub fn new(
        gpu: &Gpu,
        color_format: vk::Format,
        depth: vk::Format,
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
        };
        if let Err(e) = p.create(gpu, color_format, depth, slots) {
            p.destroy(gpu);
            return Err(e);
        }
        Ok(p)
    }

    fn create(
        &mut self,
        gpu: &Gpu,
        color_format: vk::Format,
        depth: vk::Format,
        slots: usize,
    ) -> Result<()> {
        let d = &gpu.device;
        self.set_layout = pipeline::set_layout(gpu, &[(0, vk::DescriptorType::UNIFORM_BUFFER)])?;
        self.layout = pipeline::layout(gpu, self.set_layout, 4)?;
        let attributes = vertex_attributes();
        self.env_pipe = pipeline::create(
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
            },
        )?;
        self.sky_pipe = pipeline::create(
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
            },
        )?;
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

    /// Whether `place` is the one loaded. (The renderer gets it built off the
    /// render thread and `load`s it when it is ready.)
    pub fn has(&self, place: Place) -> bool {
        self.loaded.as_ref().is_some_and(|l| l.place == place)
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
        let vertex_bytes: &[u8] = bytemuck::cast_slice(&data.vertices);
        let index_bytes: &[u8] = bytemuck::cast_slice(&data.indices);
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
                return Err(e);
            }
        };
        vertices.bytes()[..vertex_bytes.len()].copy_from_slice(vertex_bytes);
        indices.bytes()[..index_bytes.len()].copy_from_slice(index_bytes);
        let loaded = Loaded {
            place,
            kind: place.kind(),
            floor_y: place.layout().floor_y,
            vertices,
            indices,
            index_count: data.indices.len() as u32,
            look: Look::new(&data),
        };
        if let Some(old) = self.loaded.replace(loaded) {
            garbage.push(old.vertices);
            garbage.push(old.indices);
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
        self.loaded.as_ref().map(|l| l.look.uniform(input))
    }

    /// Writes slot `slot`'s uniform. Its fence has been waited on.
    pub fn write(&mut self, slot: usize, u: &EnvUniform) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.uniform.bytes()[..size_of::<EnvUniform>()].copy_from_slice(bytemuck::bytes_of(u));
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
            d.destroy_pipeline_layout(self.layout, None);
            d.destroy_descriptor_set_layout(self.set_layout, None);
            d.destroy_descriptor_pool(self.pool, None);
        }
        for mut s in self.slots.drain(..) {
            s.uniform.destroy(gpu);
        }
        if let Some(mut l) = self.loaded.take() {
            l.vertices.destroy(gpu);
            l.indices.destroy(gpu);
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
            floor_drop: 0.0,
            time: 12.5,
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
        assert_eq!(size_of::<EnvUniform>(), 1184);
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
        });
        let on = look.uniform(&i);
        assert_eq!(on.patch_pos, patches);
        let spill = Place::Cinema.build().lighting.spill;
        for (k, (got, want)) in on.patch_rgb.iter().zip(rgb).enumerate() {
            assert!(got.truncate().abs_diff_eq(want * spill, 1e-6), "{k}");
        }
        assert_eq!(on.screen_n, Vec4::new(0.0, 0.0, 1.0, spill));
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
