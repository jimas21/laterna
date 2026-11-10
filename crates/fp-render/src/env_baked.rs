//! The GPU side of a baked place (a pack, see [`crate::env::pack`]): its
//! textures, which shaders/env_baked.wgsl mixes, and one descriptor set for
//! each frame slot that points at them and at that slot's place uniform.
//!
//! The textures are two 2D arrays so the shader needs two image bindings, not
//! ten: layer 0 and 1 of `colors` are the house-lights-up and lights-off
//! pictures (sRGB, decoded by the sampler), layers 0 to 7 of `basis` the
//! light of each patch of the screen (square-root coded, so plain UNORM). All
//! of them get a mip chain, blitted on the GPU, when the device can.
//!
//! Made on the render thread when a pack is loaded: the upload is a handful
//! of one-shot submissions that wait for the GPU (a stall of a frame or so,
//! once per pack).

use crate::env::pack::{BASIS, Baked};
use crate::gpu::Gpu;
use crate::mem::{self, Buffer, Image};
use crate::{Result, VkContext};
use ash::vk;
use gpu_allocator::MemoryLocation;

/// What the sets' layout holds: the place uniform, the sampler, the colour
/// textures, the basis textures.
pub(crate) const BINDINGS: [(u32, vk::DescriptorType); 4] = [
    (0, vk::DescriptorType::UNIFORM_BUFFER),
    (1, vk::DescriptorType::SAMPLER),
    (2, vk::DescriptorType::SAMPLED_IMAGE),
    (3, vk::DescriptorType::SAMPLED_IMAGE),
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

/// Copies `staging` (the layer's `size` x `size` RGBA8 texels) into layer
/// `layer` of `image`, builds its mip chain by blitting, and leaves every
/// level of the layer readable by shaders.
fn record_layer(
    gpu: &Gpu,
    cmd: vk::CommandBuffer,
    staging: vk::Buffer,
    image: &Image,
    layer: u32,
    size: u32,
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
    let copy = vk::BufferImageCopy::default()
        .image_subresource(layer_of(0, layer))
        .image_extent(vk::Extent3D {
            width: size,
            height: size,
            depth: 1,
        });
    // SAFETY: recording; the staging buffer holds the whole layer and the
    // image level 0 is `size` square, in the layout the barrier made.
    unsafe {
        gpu.device
            .cmd_copy_buffer_to_image(cmd, staging, image.image, to_dst, &[copy]);
    }
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
    if mips > 1 {
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
    }
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

/// An array image holding `layers` (each `size` x `size` RGBA8), filled.
fn upload_array(
    gpu: &Gpu,
    layers: &[&[u8]],
    size: u32,
    format: vk::Format,
    name: &str,
) -> Result<Image> {
    let mips = mip_levels(gpu, format, size);
    let mut image = Image::array(gpu, size, size, layers.len() as u32, format, mips, name)?;
    let bytes = u64::from(size) * u64::from(size) * 4;
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
    for (i, texels) in layers.iter().enumerate() {
        if texels.len() as u64 != bytes {
            result = Err(crate::Error::Unsupported(format!(
                "{name} layer {i} has {} bytes, not {bytes}",
                texels.len()
            )));
            break;
        }
        staging.bytes()[..texels.len()].copy_from_slice(texels);
        let buffer = staging.buffer;
        result = one_shot(gpu, |cmd| {
            record_layer(gpu, cmd, buffer, &image, i as u32, size);
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
        debug_assert_eq!(baked.basis.len(), BASIS);
        let colors = upload_array(
            gpu,
            &[&baked.house, &baked.dark],
            baked.size,
            COLOR_FORMAT,
            "place colours",
        )?;
        let basis_layers: Vec<&[u8]> = baked.basis.iter().map(Vec::as_slice).collect();
        let basis = match upload_array(gpu, &basis_layers, baked.size, BASIS_FORMAT, "place basis")
        {
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
        let n = uniforms.len() as u32;
        let sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: n,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLER,
                descriptor_count: n,
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
            self.pool = d
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(n)
                        .pool_sizes(&sizes),
                    None,
                )
                .ctx("baked place descriptor pool")?;
            let sampler = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
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
        }
        self.pool = vk::DescriptorPool::null();
        self.sampler = vk::Sampler::null();
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
        for (binding, name) in [(1, "tex_sampler"), (2, "colors"), (3, "basis")] {
            let line = format!("@group(0) @binding({binding}) var {name}");
            assert!(shader.contains(&line), "env_baked.wgsl lacks `{line}`");
        }
        let numbers: Vec<u32> = BINDINGS.iter().map(|b| b.0).collect();
        assert_eq!(numbers, [0, 1, 2, 3]);
    }
}
