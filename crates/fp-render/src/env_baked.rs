//! The GPU side of a baked place (a pack, see [`crate::env::pack`]): its
//! textures, which shaders/env_baked.wgsl mixes, and one descriptor set for
//! each frame slot that points at them and at that slot's place uniform.
//!
//! The textures are two 2D arrays so the shader needs two image bindings, not
//! ten: layer 0 and 1 of `colors` are the house-lights-up and lights-off
//! pictures (sRGB, decoded by the sampler), layers 0 to 7 of `basis` the
//! light of each patch of the screen (square-root coded, so plain UNORM). The
//! two arrays may differ in size. A format 1 pack's textures have one level
//! each and get a mip chain blitted on the GPU, when the device can; a format
//! 2 pack's come with every level (ASTC blocks as they are, or RGBA8 decoded
//! from them) and the levels are copied in.
//!
//! Made on the render thread when a pack is loaded: the upload is a handful
//! of one-shot submissions that wait for the GPU (a stall of a frame or so,
//! once per pack).

use crate::env::ktx2::{Encoding, astc_vk_format, full_chain};
use crate::env::pack::{BASIS, Baked, TextureArray};
use crate::gpu::Gpu;
use crate::mem::{self, Buffer, Image};
use crate::{Result, VkContext};
use ash::vk;
use gpu_allocator::MemoryLocation;

/// What the sets' layout holds: the place uniform, the sampler, the colour
/// textures, the basis textures, and the basis textures' own sampler (plain
/// trilinear: the light is smooth, anisotropic taps on it are wasted work;
/// the shader takes either by a uniform, so a headset can compare).
pub(crate) const BINDINGS: [(u32, vk::DescriptorType); 5] = [
    (0, vk::DescriptorType::UNIFORM_BUFFER),
    (1, vk::DescriptorType::SAMPLER),
    (2, vk::DescriptorType::SAMPLED_IMAGE),
    (3, vk::DescriptorType::SAMPLED_IMAGE),
    (4, vk::DescriptorType::SAMPLER),
];

/// The colour textures' format: decoded from sRGB on sampling.
const COLOR_FORMAT: vk::Format = vk::Format::R8G8B8A8_SRGB;
/// The basis textures': the stored values as they are.
const BASIS_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// Anisotropic filtering for the room's surfaces, at most.
const ANISOTROPY: f32 = 8.0;

pub(crate) struct BakedGpu {
    colors: Image,
    basis: Image,
    sampler: vk::Sampler,
    /// Trilinear, no anisotropy: for the basis textures.
    basis_sampler: vk::Sampler,
    pool: vk::DescriptorPool,
    /// One set for each frame slot.
    sets: Vec<vk::DescriptorSet>,
}

/// Levels in a full chain for a texture `size` texels square, or 1 where the
/// device can't blit `format` (the texture is then used as it is).
fn mip_levels(gpu: &Gpu, format: vk::Format, size: u32) -> u32 {
    // SAFETY: valid physical device.
    let props = unsafe {
        gpu.instance
            .get_physical_device_format_properties(gpu.pdev, format)
    };
    let need = vk::FormatFeatureFlags::BLIT_SRC
        | vk::FormatFeatureFlags::BLIT_DST
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR;
    if props.optimal_tiling_features.contains(need) {
        size.max(1).ilog2() + 1
    } else {
        1
    }
}

/// Records and runs `f` on a fresh command buffer, waiting for it to finish.
fn one_shot(gpu: &Gpu, f: impl FnOnce(vk::CommandBuffer)) -> Result<()> {
    let d = &gpu.device;
    // SAFETY: a transient pool, buffer and fence of ours, all destroyed
    // before returning, used by this thread only; the queue is the one the
    // renderer submits to, from the same (render) thread.
    unsafe {
        let pool = d
            .create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                    .queue_family_index(gpu.queue_family),
                None,
            )
            .ctx("place upload pool")?;
        let result = (|| {
            let cmd = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(1),
                )
                .ctx("place upload command buffer")?[0];
            d.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .ctx("begin place upload")?;
            f(cmd);
            d.end_command_buffer(cmd).ctx("end place upload")?;
            let fence = d
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .ctx("place upload fence")?;
            let cmds = [cmd];
            let done = d
                .queue_submit(
                    gpu.queue,
                    &[vk::SubmitInfo::default().command_buffers(&cmds)],
                    fence,
                )
                .ctx("submit place upload")
                .and_then(|()| {
                    d.wait_for_fences(&[fence], true, u64::MAX)
                        .ctx("wait for place upload")
                });
            d.destroy_fence(fence, None);
            done
        })();
        d.destroy_command_pool(pool, None);
        result
    }
}

fn subresource(base_mip: u32, mips: u32, layer: u32) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: base_mip,
        level_count: mips,
        base_array_layer: layer,
        layer_count: 1,
    }
}

fn layer_of(mip: u32, layer: u32) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: mip,
        base_array_layer: layer,
        layer_count: 1,
    }
}

/// Where a level's bytes are in the staging buffer, and the level's edge in
/// texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LevelCopy {
    offset: u64,
    edge: u32,
}

/// Copies the levels in `staging` (placed as `copies` says) into layer
/// `layer` of `image`. With `blit` there is only level 0, the `size` square
/// RGBA8 texels, and the rest of the chain is made from it by blitting.
/// Leaves every level of the layer readable by shaders.
fn record_layer(
    gpu: &Gpu,
    cmd: vk::CommandBuffer,
    staging: vk::Buffer,
    image: &Image,
    layer: u32,
    copies: &[LevelCopy],
    blit: bool,
) {
    let mips = image.mips;
    let (to_dst, to_src, to_read) = (
        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    );
    mem::barrier(
        gpu,
        cmd,
        image.image,
        subresource(0, mips, layer),
        vk::ImageLayout::UNDEFINED,
        to_dst,
        mem::TOP,
        mem::TRANSFER_WRITE,
    );
    for (mip, c) in copies.iter().enumerate() {
        let copy = vk::BufferImageCopy::default()
            .buffer_offset(c.offset)
            .image_subresource(layer_of(mip as u32, layer))
            .image_extent(vk::Extent3D {
                width: c.edge,
                height: c.edge,
                depth: 1,
            });
        // SAFETY: recording; the staging buffer holds the level at its
        // offset and the image level is `edge` square (the whole level, so
        // a compressed one needs no block alignment), in the layout the
        // barrier made.
        unsafe {
            gpu.device
                .cmd_copy_buffer_to_image(cmd, staging, image.image, to_dst, &[copy]);
        }
    }
    if !blit {
        mem::barrier(
            gpu,
            cmd,
            image.image,
            subresource(0, mips, layer),
            to_dst,
            to_read,
            mem::TRANSFER_WRITE,
            mem::SHADER_READ,
        );
        return;
    }
    let size = copies.first().map_or(1, |c| c.edge);
    let edge = |mip: u32| i32::try_from((size >> mip).max(1)).unwrap_or(1);
    for mip in 1..mips {
        mem::barrier(
            gpu,
            cmd,
            image.image,
            subresource(mip - 1, 1, layer),
            to_dst,
            to_src,
            mem::TRANSFER_WRITE,
            mem::TRANSFER_READ,
        );
        let corner = |mip: u32| vk::Offset3D {
            x: edge(mip),
            y: edge(mip),
            z: 1,
        };
        let blit = vk::ImageBlit::default()
            .src_subresource(layer_of(mip - 1, layer))
            .src_offsets([vk::Offset3D::default(), corner(mip - 1)])
            .dst_subresource(layer_of(mip, layer))
            .dst_offsets([vk::Offset3D::default(), corner(mip)]);
        // SAFETY: recording; the source level is in the source layout, the
        // destination still in the destination layout, both inside the image.
        unsafe {
            gpu.device.cmd_blit_image(
                cmd,
                image.image,
                to_src,
                image.image,
                to_dst,
                &[blit],
                vk::Filter::LINEAR,
            );
        }
    }
    mem::barrier(
        gpu,
        cmd,
        image.image,
        subresource(0, mips - 1, layer),
        to_src,
        to_read,
        mem::TRANSFER_READ,
        mem::SHADER_READ,
    );
    mem::barrier(
        gpu,
        cmd,
        image.image,
        subresource(mips - 1, 1, layer),
        to_dst,
        to_read,
        mem::TRANSFER_WRITE,
        mem::SHADER_READ,
    );
}

/// The Vulkan format textures of `encoding` are uploaded as: `srgb` ones are
/// decoded from sRGB on sampling.
fn format_of(encoding: Encoding, srgb: bool) -> Result<vk::Format> {
    match encoding {
        Encoding::Rgba8 if srgb => Ok(COLOR_FORMAT),
        Encoding::Rgba8 => Ok(BASIS_FORMAT),
        Encoding::Astc { block } => astc_vk_format(block, srgb)
            .map(|f| vk::Format::from_raw(f as i32))
            .ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "ASTC {} x {} is not a block size",
                    block.0, block.1
                ))
            }),
    }
}

/// Checks `tex` against its own claims: every layer has the same levels, a
/// full chain unless it has just one, and every level is the bytes its size
/// and encoding need. The offsets the levels go to in a staging buffer
/// (every one a multiple of `align`, the same for each layer) and the bytes
/// that takes.
fn plan(tex: &TextureArray, align: u64, name: &str) -> Result<(Vec<LevelCopy>, u64)> {
    let bad = |what: String| Err(crate::Error::Unsupported(format!("{name}: {what}")));
    let levels = tex.levels();
    if levels == 0 || tex.layers.iter().any(|l| l.len() != levels) {
        return bad("the layers do not all have the same levels".into());
    }
    if levels > 1 && levels as u32 != full_chain(tex.size) {
        return bad(format!(
            "{levels} levels is not a full chain for {}",
            tex.size
        ));
    }
    let mut copies = Vec::with_capacity(levels);
    let mut at = 0u64;
    for (mip, data) in tex.layers[0].iter().enumerate() {
        let edge = (tex.size >> mip).max(1);
        let need = tex.encoding.level_bytes(edge, edge);
        for (i, layer) in tex.layers.iter().enumerate() {
            if layer[mip].len() as u64 != need {
                return bad(format!(
                    "layer {i} level {mip} has {} bytes, not {need}",
                    layer[mip].len()
                ));
            }
        }
        debug_assert_eq!(data.len() as u64, need);
        at = at.next_multiple_of(align);
        copies.push(LevelCopy { offset: at, edge });
        at += need;
    }
    Ok((copies, at))
}

/// An array image holding the textures of `tex`, filled: RGBA8 ones with
/// one level get the rest of the chain blitted where the device can; every
/// other texture brings its own levels, which are copied as they are.
fn upload_array(gpu: &Gpu, tex: &TextureArray, srgb: bool, name: &str) -> Result<Image> {
    let compressed = matches!(tex.encoding, Encoding::Astc { .. });
    if compressed && !gpu.astc {
        return Err(crate::Error::Unsupported(format!(
            "{name} is ASTC and this device cannot sample it"
        )));
    }
    let format = format_of(tex.encoding, srgb)?;
    let (copies, bytes) = plan(tex, if compressed { 16 } else { 4 }, name)?;
    let size = tex.size;
    let generate = tex.levels() == 1 && !compressed;
    let mips = if generate {
        mip_levels(gpu, format, size)
    } else {
        tex.levels() as u32
    };
    let blit = generate && mips > 1;
    let mut image = Image::array(
        gpu,
        size,
        size,
        tex.layers.len() as u32,
        format,
        mips,
        blit,
        name,
    )?;
    let mut staging = match Buffer::new(
        gpu,
        bytes,
        vk::BufferUsageFlags::TRANSFER_SRC,
        MemoryLocation::CpuToGpu,
        "place texture staging",
    ) {
        Ok(b) => b,
        Err(e) => {
            image.destroy(gpu);
            return Err(e);
        }
    };
    let mut result = Ok(());
    for (i, layer) in tex.layers.iter().enumerate() {
        for (data, c) in layer.iter().zip(&copies) {
            let at = c.offset as usize;
            staging.bytes()[at..at + data.len()].copy_from_slice(data);
        }
        let buffer = staging.buffer;
        result = one_shot(gpu, |cmd| {
            record_layer(gpu, cmd, buffer, &image, i as u32, &copies, blit);
        });
        if result.is_err() {
            break;
        }
    }
    staging.destroy(gpu);
    match result {
        Ok(()) => Ok(image),
        Err(e) => {
            gpu.wait_idle();
            image.destroy(gpu);
            Err(e)
        }
    }
}

impl BakedGpu {
    /// Uploads `baked`'s textures and makes a set per entry of `uniforms`
    /// (each frame slot's place uniform buffer and its size) in `set_layout`.
    pub fn new(
        gpu: &Gpu,
        set_layout: vk::DescriptorSetLayout,
        uniforms: &[(vk::Buffer, u64)],
        baked: &Baked,
    ) -> Result<BakedGpu> {
        if baked.colors.layers.len() != 2 || baked.basis.layers.len() != BASIS {
            return Err(crate::Error::Unsupported(format!(
                "a baked place needs 2 colour and {BASIS} basis textures, not {} and {}",
                baked.colors.layers.len(),
                baked.basis.layers.len()
            )));
        }
        let colors = upload_array(gpu, &baked.colors, true, "place colours")?;
        let basis = match upload_array(gpu, &baked.basis, false, "place basis") {
            Ok(i) => i,
            Err(e) => {
                let mut colors = colors;
                colors.destroy(gpu);
                return Err(e);
            }
        };
        let mut b = BakedGpu {
            colors,
            basis,
            sampler: vk::Sampler::null(),
            basis_sampler: vk::Sampler::null(),
            pool: vk::DescriptorPool::null(),
            sets: Vec::new(),
        };
        if let Err(e) = b.bind(gpu, set_layout, uniforms) {
            b.destroy(gpu);
            return Err(e);
        }
        Ok(b)
    }

    fn bind(
        &mut self,
        gpu: &Gpu,
        set_layout: vk::DescriptorSetLayout,
        uniforms: &[(vk::Buffer, u64)],
    ) -> Result<()> {
        let d = &gpu.device;
        let mut info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .max_lod(vk::LOD_CLAMP_NONE);
        if let Some(max) = gpu.max_anisotropy {
            info = info
                .anisotropy_enable(true)
                .max_anisotropy(max.min(ANISOTROPY));
        }
        // The same without the anisotropy.
        let plain = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .max_lod(vk::LOD_CLAMP_NONE);
        let n = uniforms.len() as u32;
        let sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: n,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLER,
                descriptor_count: 2 * n,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: 2 * n,
            },
        ];
        // SAFETY: plain object creation on a valid device; `destroy` frees
        // them. The sets are not in use yet when written.
        unsafe {
            self.sampler = d.create_sampler(&info, None).ctx("place sampler")?;
            self.basis_sampler = d.create_sampler(&plain, None).ctx("place basis sampler")?;
            self.pool = d
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(n)
                        .pool_sizes(&sizes),
                    None,
                )
                .ctx("baked place descriptor pool")?;
            let sampler = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
            let basis_sampler = [vk::DescriptorImageInfo::default().sampler(self.basis_sampler)];
            let colors = [vk::DescriptorImageInfo::default()
                .image_view(self.colors.views[0])
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let basis = [vk::DescriptorImageInfo::default()
                .image_view(self.basis.views[0])
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            for &(buffer, range) in uniforms {
                let layouts = [set_layout];
                let set = d
                    .allocate_descriptor_sets(
                        &vk::DescriptorSetAllocateInfo::default()
                            .descriptor_pool(self.pool)
                            .set_layouts(&layouts),
                    )
                    .ctx("baked place descriptor set")?[0];
                self.sets.push(set);
                let uniform = [vk::DescriptorBufferInfo::default()
                    .buffer(buffer)
                    .range(range)];
                let write = |binding: u32, ty: vk::DescriptorType| {
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(binding)
                        .descriptor_type(ty)
                };
                let writes = [
                    write(0, vk::DescriptorType::UNIFORM_BUFFER).buffer_info(&uniform),
                    write(1, vk::DescriptorType::SAMPLER).image_info(&sampler),
                    write(2, vk::DescriptorType::SAMPLED_IMAGE).image_info(&colors),
                    write(3, vk::DescriptorType::SAMPLED_IMAGE).image_info(&basis),
                    write(4, vk::DescriptorType::SAMPLER).image_info(&basis_sampler),
                ];
                d.update_descriptor_sets(&writes, &[]);
            }
        }
        Ok(())
    }

    /// The set for frame slot `slot`.
    pub fn set(&self, slot: usize) -> Option<vk::DescriptorSet> {
        self.sets.get(slot).copied()
    }

    /// Frees everything. The device must be idle of anything that uses it.
    pub fn destroy(&mut self, gpu: &Gpu) {
        // SAFETY: not in use (the caller waited); ours; null handles are
        // fine to destroy, and the pool takes its sets with it.
        unsafe {
            gpu.device.destroy_descriptor_pool(self.pool, None);
            gpu.device.destroy_sampler(self.sampler, None);
            gpu.device.destroy_sampler(self.basis_sampler, None);
        }
        self.pool = vk::DescriptorPool::null();
        self.sampler = vk::Sampler::null();
        self.basis_sampler = vk::Sampler::null();
        self.sets.clear();
        self.colors.destroy(gpu);
        self.basis.destroy(gpu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bindings_are_the_shaders() {
        let shader = include_str!("../shaders/env_baked.wgsl");
        for (binding, name) in [
            (1, "tex_sampler"),
            (2, "colors"),
            (3, "basis"),
            (4, "basis_sampler"),
        ] {
            let line = format!("@group(0) @binding({binding}) var {name}");
            assert!(shader.contains(&line), "env_baked.wgsl lacks `{line}`");
        }
        let numbers: Vec<u32> = BINDINGS.iter().map(|b| b.0).collect();
        assert_eq!(numbers, [0, 1, 2, 3, 4]);
    }
}
