//! Graphics pipeline construction (dynamic rendering, no render passes).

use crate::gpu::Gpu;
use crate::{Result, VkContext};
use ash::vk;
use std::ffi::CStr;

/// What a pipeline does with the depth attachment of the rendering instance it
/// is bound in. Vulkan wants every pipeline bound in one instance to name the
/// instance's depth format (dynamic rendering without
/// `VK_EXT_dynamic_rendering_unused_attachments`), whether or not it uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Depth {
    /// No depth attachment in the instance.
    None,
    /// A depth attachment of this format is there; the pipeline neither tests
    /// against it nor writes it.
    Ignore(vk::Format),
    /// Tests against the attachment (nearer or equal passes) and writes what
    /// passes: opaque geometry.
    TestWrite(vk::Format),
    /// Tests against the attachment but leaves it as it is: a backdrop drawn
    /// at the far plane after the geometry.
    Test(vk::Format),
}

impl Depth {
    /// The format of the depth attachment the pipeline is bound with, if any.
    fn format(self) -> Option<vk::Format> {
        match self {
            Depth::None => None,
            Depth::Ignore(f) | Depth::TestWrite(f) | Depth::Test(f) => Some(f),
        }
    }

    /// Whether the pipeline tests, and whether it writes.
    fn test_write(self) -> (bool, bool) {
        match self {
            Depth::None | Depth::Ignore(_) => (false, false),
            Depth::TestWrite(_) => (true, true),
            Depth::Test(_) => (true, false),
        }
    }
}

pub(crate) struct PipelineDesc<'a> {
    pub spv: &'a [u8],
    pub layout: vk::PipelineLayout,
    pub color_format: vk::Format,
    pub vertex_stride: u32,
    pub attributes: &'a [vk::VertexInputAttributeDescription],
    /// Premultiplied-alpha blending.
    pub blend: bool,
    /// Its descriptors come from descriptor buffers, not sets.
    pub descriptor_buffer: bool,
    /// The depth attachment's format, and what this pipeline does with it.
    pub depth: Depth,
    /// Samples a pixel of the instance it is bound in: every pipeline bound
    /// in one instance names the same count.
    pub samples: vk::SampleCountFlags,
}

pub(crate) fn create(gpu: &Gpu, d: &PipelineDesc) -> Result<vk::Pipeline> {
    create_entry(gpu, d, c"fs_main")
}

/// [`create`] with the fragment stage's entry point `fragment` instead of
/// `fs_main`: one module can hold several fragment shaders (the scene's
/// `fs_glow`).
pub(crate) fn create_entry(gpu: &Gpu, d: &PipelineDesc, fragment: &CStr) -> Result<vk::Pipeline> {
    let module = gpu.shader(d.spv)?;
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(module)
            .name(c"vs_main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(module)
            .name(fragment),
    ];
    let bindings = [vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(d.vertex_stride)
        .input_rate(vk::VertexInputRate::VERTEX)];
    let vertex_input = if d.vertex_stride > 0 {
        vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(d.attributes)
    } else {
        vk::PipelineVertexInputStateCreateInfo::default()
    };
    let ia = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let vp = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let rs = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .line_width(1.0);
    let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(d.samples);
    let att = [if d.blend {
        vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)
    } else {
        vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
    }];
    let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&att);
    let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
    let formats = [d.color_format];
    let mut rendering =
        vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
    if let Some(format) = d.depth.format() {
        rendering = rendering.depth_attachment_format(format);
    }
    // Standard depth: the buffer is cleared to 1.0 and nearer is smaller.
    let (test, write) = d.depth.test_write();
    let depth_state = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(test)
        .depth_write_enable(write)
        .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
    let flags = if d.descriptor_buffer {
        vk::PipelineCreateFlags::DESCRIPTOR_BUFFER_EXT
    } else {
        vk::PipelineCreateFlags::empty()
    };
    let mut info = vk::GraphicsPipelineCreateInfo::default()
        .flags(flags)
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&ia)
        .viewport_state(&vp)
        .rasterization_state(&rs)
        .multisample_state(&ms)
        .color_blend_state(&cb)
        .dynamic_state(&dynamic)
        .layout(d.layout)
        .push_next(&mut rendering);
    if d.depth != Depth::None {
        info = info.depth_stencil_state(&depth_state);
    }
    // SAFETY: all referenced state lives until the call returns.
    let result = unsafe {
        gpu.device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
    };
    // SAFETY: the module is no longer needed once the pipeline exists.
    unsafe { gpu.device.destroy_shader_module(module, None) };
    result
        .map_err(|(_, e)| e)
        .ctx("create pipeline")
        .map(|p| p[0])
}

pub(crate) fn set_layout(
    gpu: &Gpu,
    bindings: &[(u32, vk::DescriptorType)],
) -> Result<vk::DescriptorSetLayout> {
    let b: Vec<_> = bindings
        .iter()
        .map(|&(i, t)| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(i)
                .descriptor_type(t)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT | vk::ShaderStageFlags::VERTEX)
        })
        .collect();
    // SAFETY: valid create info.
    unsafe {
        gpu.device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(&b),
            None,
        )
    }
    .ctx("descriptor set layout")
}

pub(crate) fn layout(
    gpu: &Gpu,
    set: vk::DescriptorSetLayout,
    push_bytes: u32,
) -> Result<vk::PipelineLayout> {
    let sets = [set];
    let ranges = [vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(push_bytes)];
    // SAFETY: valid create info.
    unsafe {
        gpu.device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&sets)
                .push_constant_ranges(&ranges),
            None,
        )
    }
    .ctx("pipeline layout")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_geometry_writes_depth_and_only_depth_users_test() {
        let f = vk::Format::D32_SFLOAT;
        assert_eq!(Depth::None.test_write(), (false, false));
        assert_eq!(Depth::Ignore(f).test_write(), (false, false));
        assert_eq!(Depth::TestWrite(f).test_write(), (true, true));
        assert_eq!(Depth::Test(f).test_write(), (true, false));
    }

    #[test]
    fn every_pipeline_in_an_instance_names_its_depth_format() {
        let f = vk::Format::D16_UNORM;
        assert_eq!(Depth::None.format(), None);
        for depth in [Depth::Ignore(f), Depth::TestWrite(f), Depth::Test(f)] {
            assert_eq!(depth.format(), Some(f));
        }
    }
}
