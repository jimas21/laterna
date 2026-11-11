//! Buffers, images and barriers.

use crate::gpu::Gpu;
use crate::{Error, Result, VkContext};
use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};

pub struct Buffer {
    pub buffer: vk::Buffer,
    pub size: u64,
    alloc: Option<Allocation>,
}

impl Buffer {
    pub fn new(
        gpu: &Gpu,
        size: u64,
        usage: vk::BufferUsageFlags,
        location: MemoryLocation,
        name: &str,
    ) -> Result<Buffer> {
        let size = size.max(16);
        // SAFETY: valid create info.
        let buffer = unsafe {
            gpu.device.create_buffer(
                &vk::BufferCreateInfo::default().size(size).usage(usage),
                None,
            )
        }
        .ctx("create buffer")?;
        let mut b = Buffer {
            buffer,
            size,
            alloc: None,
        };
        // Out of memory, what was made so far is given back.
        if let Err(e) = b.back(gpu, location, name) {
            b.destroy(gpu);
            return Err(e);
        }
        Ok(b)
    }

    /// Gives the buffer its memory.
    fn back(&mut self, gpu: &Gpu, location: MemoryLocation, name: &str) -> Result<()> {
        // SAFETY: valid buffer.
        let requirements = unsafe { gpu.device.get_buffer_memory_requirements(self.buffer) };
        let alloc = self.alloc.insert(
            gpu.alloc()
                .allocate(&AllocationCreateDesc {
                    name,
                    requirements,
                    location,
                    linear: true,
                    allocation_scheme: AllocationScheme::GpuAllocatorManaged,
                })
                .map_err(|e| Error::Alloc(e.to_string()))?,
        );
        // SAFETY: memory from the allocator matches the requirements.
        unsafe {
            gpu.device
                .bind_buffer_memory(self.buffer, alloc.memory(), alloc.offset())
        }
        .ctx("bind buffer")
    }

    /// The mapped bytes of a host-visible buffer.
    pub fn bytes(&mut self) -> &mut [u8] {
        match self.alloc.as_mut().and_then(|a| a.mapped_slice_mut()) {
            Some(s) => s,
            None => &mut [],
        }
    }

    pub fn destroy(&mut self, gpu: &Gpu) {
        if let Some(a) = self.alloc.take() {
            let _ = gpu.alloc().free(a);
        }
        // SAFETY: the buffer is no longer in use (callers wait for fences).
        unsafe { gpu.device.destroy_buffer(self.buffer, None) };
        self.buffer = vk::Buffer::null();
    }
}

pub struct Image {
    pub image: vk::Image,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
    pub mips: u32,
    /// Array layers: 1 for every image but [`Image::array`]'s.
    pub layers: u32,
    /// One view per requested format, all mips (and all layers).
    pub views: Vec<vk::ImageView>,
    /// Single-mip view of level 0 in the first format (render target).
    pub level0: vk::ImageView,
    /// True when the image sits in LAZILY_ALLOCATED memory: on a tiler
    /// whose pass never stores it, it then takes no memory at all.
    pub lazy: bool,
    alloc: Option<Allocation>,
    /// Memory of our own, for an image in a lazily allocated type (the
    /// allocator crate has no location for one).
    raw_memory: vk::DeviceMemory,
}

impl Image {
    /// Creates a 2D image. `view_formats[0]` is the image format; more formats
    /// make the image mutable-format (e.g. UNORM rendering, sRGB sampling).
    pub fn new(
        gpu: &Gpu,
        width: u32,
        height: u32,
        view_formats: &[vk::Format],
        usage: vk::ImageUsageFlags,
        mips: u32,
        name: &str,
    ) -> Result<Image> {
        let format = view_formats[0];
        let mut format_list = vk::ImageFormatListCreateInfo::default().view_formats(view_formats);
        let mut info = image_info(
            format,
            width,
            height,
            mips,
            usage,
            vk::SampleCountFlags::TYPE_1,
        );
        if view_formats.len() > 1 {
            info = info
                .flags(vk::ImageCreateFlags::MUTABLE_FORMAT)
                .push_next(&mut format_list);
        }
        Image::create(
            gpu,
            &info,
            vk::ImageAspectFlags::COLOR,
            view_formats,
            name,
            false,
        )
    }

    /// A 2D array image of `layers` layers and `mips` levels in `format`,
    /// to be sampled and filled by transfers (and, with `blit`, mip blits:
    /// compressed formats are only copied into). Its one view (`views[0]`) is
    /// a 2D array view of everything.
    #[allow(clippy::too_many_arguments)]
    pub fn array(
        gpu: &Gpu,
        width: u32,
        height: u32,
        layers: u32,
        format: vk::Format,
        mips: u32,
        blit: bool,
        name: &str,
    ) -> Result<Image> {
        let mut usage = vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST;
        if blit {
            usage |= vk::ImageUsageFlags::TRANSFER_SRC;
        }
        let info = image_info(
            format,
            width,
            height,
            mips,
            usage,
            vk::SampleCountFlags::TYPE_1,
        )
        .array_layers(layers.max(1));
        Image::create(
            gpu,
            &info,
            vk::ImageAspectFlags::COLOR,
            &[format],
            name,
            false,
        )
    }

    /// A depth attachment image of `format` with `samples` samples a pixel,
    /// for rendering only: its contents never leave the tile, so it is a
    /// transient attachment. Its single view (`views[0]`) has the depth
    /// aspect.
    pub fn depth(
        gpu: &Gpu,
        width: u32,
        height: u32,
        format: vk::Format,
        samples: vk::SampleCountFlags,
        name: &str,
    ) -> Result<Image> {
        let usage = vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
            | vk::ImageUsageFlags::TRANSIENT_ATTACHMENT;
        let info = image_info(format, width, height, 1, usage, samples);
        Image::create(
            gpu,
            &info,
            vk::ImageAspectFlags::DEPTH,
            &[format],
            name,
            true,
        )
    }

    /// A multisampled colour attachment image of `format` with `samples`
    /// samples a pixel, for rendering only: the pass resolves it into the
    /// eye's own image and never stores it, so it is a transient attachment
    /// (on a tiler it need never leave the tile).
    pub fn multisampled(
        gpu: &Gpu,
        width: u32,
        height: u32,
        format: vk::Format,
        samples: vk::SampleCountFlags,
        name: &str,
    ) -> Result<Image> {
        let usage =
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSIENT_ATTACHMENT;
        let info = image_info(format, width, height, 1, usage, samples);
        Image::create(
            gpu,
            &info,
            vk::ImageAspectFlags::COLOR,
            &[format],
            name,
            true,
        )
    }

    /// `lazy`: the image is only ever an attachment a pass never stores
    /// (transient usage), so lazily allocated memory is used when the
    /// device has a type for it.
    fn create(
        gpu: &Gpu,
        info: &vk::ImageCreateInfo,
        aspect: vk::ImageAspectFlags,
        view_formats: &[vk::Format],
        name: &str,
        lazy: bool,
    ) -> Result<Image> {
        // SAFETY: valid create info.
        let image = unsafe { gpu.device.create_image(info, None) }.ctx("create image")?;
        let mut img = Image {
            image,
            format: info.format,
            extent: vk::Extent2D {
                width: info.extent.width,
                height: info.extent.height,
            },
            mips: info.mip_levels,
            layers: info.array_layers,
            views: Vec::new(),
            level0: vk::ImageView::null(),
            lazy: false,
            alloc: None,
            raw_memory: vk::DeviceMemory::null(),
        };
        // Out of memory, what was made so far is given back.
        if let Err(e) = img.back(gpu, aspect, view_formats, name, lazy) {
            img.destroy(gpu);
            return Err(e);
        }
        Ok(img)
    }

    /// Gives the image its memory and views.
    fn back(
        &mut self,
        gpu: &Gpu,
        aspect: vk::ImageAspectFlags,
        view_formats: &[vk::Format],
        name: &str,
        lazy: bool,
    ) -> Result<()> {
        // SAFETY: valid image.
        let requirements = unsafe { gpu.device.get_image_memory_requirements(self.image) };
        if !(lazy && self.back_lazily(gpu, &requirements, name)) {
            let alloc = self.alloc.insert(
                gpu.alloc()
                    .allocate(&AllocationCreateDesc {
                        name,
                        requirements,
                        location: MemoryLocation::GpuOnly,
                        linear: false,
                        allocation_scheme: AllocationScheme::GpuAllocatorManaged,
                    })
                    .map_err(|e| Error::Alloc(e.to_string()))?,
            );
            // SAFETY: memory matches the requirements.
            unsafe {
                gpu.device
                    .bind_image_memory(self.image, alloc.memory(), alloc.offset())
            }
            .ctx("bind image")?;
        }
        for &f in view_formats {
            self.views.push(view_of(
                gpu,
                self.image,
                f,
                aspect,
                0,
                self.mips,
                self.layers,
            )?);
        }
        // A depth image has no single-mip colour view: it is only ever
        // attached through `views[0]`.
        if aspect == vk::ImageAspectFlags::COLOR {
            self.level0 = view(gpu, self.image, self.format, 0, 1)?;
        }
        Ok(())
    }

    /// Binds the image to its own lazily allocated memory, when the device
    /// has such a type for it. False (nothing changed) when it has not or
    /// the allocation failed: the caller then allocates as usual.
    fn back_lazily(&mut self, gpu: &Gpu, req: &vk::MemoryRequirements, name: &str) -> bool {
        // SAFETY: valid physical device.
        let props = unsafe {
            gpu.instance
                .get_physical_device_memory_properties(gpu.pdev)
        };
        let types: Vec<vk::MemoryPropertyFlags> = props.memory_types
            [..props.memory_type_count as usize]
            .iter()
            .map(|t| t.property_flags)
            .collect();
        let Some(index) = lazy_memory_type(&types, req.memory_type_bits) else {
            log::info!("{name}: this device has no lazily allocated memory type");
            return false;
        };
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(index as u32);
        // SAFETY: valid allocate info; freed by `destroy`.
        let Ok(memory) = (unsafe { gpu.device.allocate_memory(&info, None) }) else {
            return false;
        };
        // SAFETY: memory of a type the image allows, large enough, unbound.
        if unsafe { gpu.device.bind_image_memory(self.image, memory, 0) }.is_err() {
            // SAFETY: just made, not in use.
            unsafe { gpu.device.free_memory(memory, None) };
            return false;
        }
        log::info!("{name}: lazily allocated memory, type {index}");
        self.raw_memory = memory;
        self.lazy = true;
        true
    }

    /// Destroys what the image holds; null handles (an image given up half
    /// made) are allowed.
    pub fn destroy(&mut self, gpu: &Gpu) {
        // SAFETY: not in use (callers defer destruction past in-flight frames).
        unsafe {
            for v in self.views.drain(..) {
                gpu.device.destroy_image_view(v, None);
            }
            gpu.device.destroy_image_view(self.level0, None);
            gpu.device.destroy_image(self.image, None);
        }
        if let Some(a) = self.alloc.take() {
            let _ = gpu.alloc().free(a);
        }
        if self.raw_memory != vk::DeviceMemory::null() {
            // SAFETY: the image that used it is gone and no frame reads it.
            unsafe {
                gpu.device
                    .free_memory(std::mem::take(&mut self.raw_memory), None)
            };
        }
    }
}

/// The index of the first memory type of `types` among those `bits` allows
/// that is device-local and lazily allocated: where a transient attachment a
/// tiler never stores can live without taking any memory.
pub(crate) fn lazy_memory_type(types: &[vk::MemoryPropertyFlags], bits: u32) -> Option<usize> {
    let want = vk::MemoryPropertyFlags::LAZILY_ALLOCATED | vk::MemoryPropertyFlags::DEVICE_LOCAL;
    types
        .iter()
        .enumerate()
        .find(|&(i, f)| i < 32 && bits & (1 << i) != 0 && f.contains(want))
        .map(|(i, _)| i)
}

/// The `CreateInfo` of an optimally tiled 2D image with `samples` samples a
/// pixel.
fn image_info<'a>(
    format: vk::Format,
    width: u32,
    height: u32,
    mips: u32,
    usage: vk::ImageUsageFlags,
    samples: vk::SampleCountFlags,
) -> vk::ImageCreateInfo<'a> {
    vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: width.max(1),
            height: height.max(1),
            depth: 1,
        })
        .mip_levels(mips.max(1))
        .array_layers(1)
        .samples(samples)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(usage)
        .initial_layout(vk::ImageLayout::UNDEFINED)
}

pub fn view(
    gpu: &Gpu,
    image: vk::Image,
    format: vk::Format,
    base_mip: u32,
    mips: u32,
) -> Result<vk::ImageView> {
    view_of(
        gpu,
        image,
        format,
        vk::ImageAspectFlags::COLOR,
        base_mip,
        mips,
        1,
    )
}

/// A view of `layers` layers from the first: a 2D view of one, else a 2D
/// array view.
fn view_of(
    gpu: &Gpu,
    image: vk::Image,
    format: vk::Format,
    aspect: vk::ImageAspectFlags,
    base_mip: u32,
    mips: u32,
    layers: u32,
) -> Result<vk::ImageView> {
    let sub = vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: base_mip,
        level_count: mips,
        base_array_layer: 0,
        layer_count: layers,
    };
    let view_type = if layers > 1 {
        vk::ImageViewType::TYPE_2D_ARRAY
    } else {
        vk::ImageViewType::TYPE_2D
    };
    // SAFETY: valid image and format compatible with it.
    unsafe {
        gpu.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(view_type)
                .format(format)
                .subresource_range(sub),
            None,
        )
    }
    .ctx("create image view")
}

pub fn range(base_mip: u32, mips: u32) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: base_mip,
        level_count: mips,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// The one level of a depth-only image.
pub fn depth_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::DEPTH,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// One image layout transition (synchronization2).
#[allow(clippy::too_many_arguments)]
pub fn barrier(
    gpu: &Gpu,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    sub: vk::ImageSubresourceRange,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src: (vk::PipelineStageFlags2, vk::AccessFlags2),
    dst: (vk::PipelineStageFlags2, vk::AccessFlags2),
) {
    let b = [vk::ImageMemoryBarrier2::default()
        .image(image)
        .subresource_range(sub)
        .old_layout(old)
        .new_layout(new)
        .src_stage_mask(src.0)
        .src_access_mask(src.1)
        .dst_stage_mask(dst.0)
        .dst_access_mask(dst.1)];
    // SAFETY: recording into a command buffer in the recording state.
    unsafe {
        gpu.device.cmd_pipeline_barrier2(
            cmd,
            &vk::DependencyInfo::default().image_memory_barriers(&b),
        );
    };
}

/// [`barrier`] over a whole single-mip image that also moves it from queue
/// family `families.0` to `families.1`: for memory the video decoder
/// writes, from the foreign family before the GPU reads it and back after
/// ([`Gpu::decoder_family`](crate::gpu::Gpu)).
#[allow(clippy::too_many_arguments)]
pub fn ownership_barrier(
    gpu: &Gpu,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src: (vk::PipelineStageFlags2, vk::AccessFlags2),
    dst: (vk::PipelineStageFlags2, vk::AccessFlags2),
    families: (u32, u32),
) {
    let b = [vk::ImageMemoryBarrier2::default()
        .image(image)
        .subresource_range(range(0, 1))
        .old_layout(old)
        .new_layout(new)
        .src_stage_mask(src.0)
        .src_access_mask(src.1)
        .dst_stage_mask(dst.0)
        .dst_access_mask(dst.1)
        .src_queue_family_index(families.0)
        .dst_queue_family_index(families.1)];
    // SAFETY: recording into a command buffer in the recording state.
    unsafe {
        gpu.device.cmd_pipeline_barrier2(
            cmd,
            &vk::DependencyInfo::default().image_memory_barriers(&b),
        );
    };
}

/// After everything, for a release to another queue family.
pub const BOTTOM: (vk::PipelineStageFlags2, vk::AccessFlags2) = (
    vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
    vk::AccessFlags2::NONE,
);

pub const TOP: (vk::PipelineStageFlags2, vk::AccessFlags2) =
    (vk::PipelineStageFlags2::TOP_OF_PIPE, vk::AccessFlags2::NONE);
pub const TRANSFER_WRITE: (vk::PipelineStageFlags2, vk::AccessFlags2) = (
    vk::PipelineStageFlags2::TRANSFER,
    vk::AccessFlags2::TRANSFER_WRITE,
);
pub const TRANSFER_READ: (vk::PipelineStageFlags2, vk::AccessFlags2) = (
    vk::PipelineStageFlags2::TRANSFER,
    vk::AccessFlags2::TRANSFER_READ,
);
pub const SHADER_READ: (vk::PipelineStageFlags2, vk::AccessFlags2) = (
    vk::PipelineStageFlags2::FRAGMENT_SHADER,
    vk::AccessFlags2::SHADER_SAMPLED_READ,
);
pub const COLOR_WRITE: (vk::PipelineStageFlags2, vk::AccessFlags2) = (
    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
    vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
);

/// Depth attachment reads and writes (the tests before and after a draw).
pub const DEPTH_WRITE: (vk::PipelineStageFlags2, vk::AccessFlags2) = (
    vk::PipelineStageFlags2::from_raw(
        vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS.as_raw()
            | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS.as_raw(),
    ),
    vk::AccessFlags2::from_raw(
        vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_READ.as_raw()
            | vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE.as_raw(),
    ),
);

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: vk::MemoryPropertyFlags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    const LAZY: vk::MemoryPropertyFlags = vk::MemoryPropertyFlags::from_raw(
        vk::MemoryPropertyFlags::DEVICE_LOCAL.as_raw()
            | vk::MemoryPropertyFlags::LAZILY_ALLOCATED.as_raw(),
    );

    #[test]
    fn a_lazily_allocated_type_is_found_only_where_the_image_allows_it() {
        // Turnip-like: nothing lazy.
        assert_eq!(lazy_memory_type(&[LOCAL, LOCAL], 0b11), None);
        // A tiler that offers one (second type), allowed by the image.
        let types = [LOCAL, LAZY, LOCAL];
        assert_eq!(lazy_memory_type(&types, 0b111), Some(1));
        // Not one of the types this image can use.
        assert_eq!(lazy_memory_type(&types, 0b101), None);
        // The first of several; lazily allocated but not device-local is no use.
        assert_eq!(lazy_memory_type(&[LAZY, LAZY], 0b11), Some(0));
        let host_lazy = vk::MemoryPropertyFlags::LAZILY_ALLOCATED;
        assert_eq!(lazy_memory_type(&[host_lazy], 0b1), None);
        assert_eq!(lazy_memory_type(&[], 0b1), None);
    }
}
