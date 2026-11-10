//! Measures the colour of the picture for the place it lights: the scene
//! shader's `fs_glow` entry draws the picture's cells (4x2, each the mean of
//! 8x8 taps after the whole transfer, tone map and picture chain) into a tiny
//! image, which is copied to a buffer the frame slot owns and read once that
//! slot's fence has signalled, as [`crate::peak::PeakPass`] reads its
//! histogram. Nothing waits for it: the colours arrive two frames late and
//! the CPU eases them (`crate::env::light`).
//!
//! The same shader module serves every way a frame reaches the eye passes
//! (copied planes, a shared NV12 buffer, TP10 sampled in place), so the pass
//! reads whatever the eye passes read. Made on the first frame that wants the
//! light, so nobody who never picks a place pays for it.

use crate::env::light::{COLS, PATCHES, ROWS};
use crate::gpu::Gpu;
use crate::mem::{self, Buffer, Image};
use crate::pipeline::{self, Depth, PipelineDesc};
use ash::vk;
use glam::Vec3;
use gpu_allocator::MemoryLocation;

/// The glow image's format: half floats, so a bright HDR cell is not clipped.
const FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;
/// Bytes a pixel of it takes.
const PIXEL: usize = 8;
/// The brightest a cell may read, linear: tone mapping keeps it within 1, so
/// anything more is a bad read.
const CEILING: f32 = 16.0;

struct SlotGlow {
    /// The image's pixels, row by row from the top left: patch order.
    readback: Buffer,
    /// The slot's submission measures a frame.
    pending: bool,
}

pub(crate) struct ScreenGlow {
    image: Image,
    pipe: vk::Pipeline,
    slots: Vec<SlotGlow>,
}

impl ScreenGlow {
    /// The pass for the scene pipeline layout `layout` (the copied planes and
    /// shared NV12 paths), with a readback buffer for each of `slots` frame
    /// slots.
    pub fn new(gpu: &Gpu, layout: vk::PipelineLayout, slots: usize) -> crate::Result<ScreenGlow> {
        let usage = vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC;
        let image = Image::new(
            gpu,
            COLS as u32,
            ROWS as u32,
            &[FORMAT],
            usage,
            1,
            "screen glow",
        )?;
        let pipe = pipeline::create_entry(gpu, &desc(crate::SCENE_SPV, layout, false), c"fs_glow");
        let pipe = match pipe {
            Ok(p) => p,
            Err(e) => {
                let mut image = image;
                image.destroy(gpu);
                return Err(e);
            }
        };
        let mut g = ScreenGlow {
            image,
            pipe,
            slots: Vec::new(),
        };
        for _ in 0..slots {
            let readback = Buffer::new(
                gpu,
                (PATCHES * PIXEL) as u64,
                vk::BufferUsageFlags::TRANSFER_DST,
                MemoryLocation::GpuToCpu,
                "screen glow readback",
            );
            match readback {
                Ok(readback) => g.slots.push(SlotGlow {
                    readback,
                    pending: false,
                }),
                Err(e) => {
                    g.destroy(gpu);
                    return Err(e);
                }
            }
        }
        Ok(g)
    }

    /// Records the measurement of the picture into slot `slot`'s command
    /// buffer `cmd`, outside any rendering: a pass of its own into the glow
    /// image, then the copy out of it. `bind` binds the pipeline to draw with
    /// and what it reads (it is given this pass's own pipeline, for the
    /// scene's planes; the TP10 path has one of its own) and gives back the
    /// pipeline layout, for the eye (always 0, the left one) pushed to it.
    pub fn record(
        &mut self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        slot: usize,
        bind: impl FnOnce(vk::Pipeline) -> Option<vk::PipelineLayout>,
    ) {
        let Some(s) = self.slots.get_mut(slot) else {
            return;
        };
        let d = &gpu.device;
        let image = self.image.image;
        // The previous measurement's copy out of the image is done first.
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(0, 1),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            mem::TRANSFER_READ,
            mem::COLOR_WRITE,
        );
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: self.image.extent,
        };
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(self.image.level0)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue { float32: [0.0; 4] },
            })];
        // SAFETY: recording outside any rendering with a valid pipeline; the
        // caller's `bind` binds what the pipeline reads.
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
            if let Some(layout) = bind(self.pipe) {
                d.cmd_push_constants(
                    cmd,
                    layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    &0u32.to_le_bytes(),
                );
                d.cmd_draw(cmd, 3, 1, 0, 0);
            }
            d.cmd_end_rendering(cmd);
        }
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(0, 1),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            mem::COLOR_WRITE,
            mem::TRANSFER_READ,
        );
        let copy = [vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: area.extent.width,
                height: area.extent.height,
                depth: 1,
            })];
        let to_host = [vk::BufferMemoryBarrier2::default()
            .buffer(s.readback.buffer)
            .size(vk::WHOLE_SIZE)
            .src_stage_mask(mem::TRANSFER_WRITE.0)
            .src_access_mask(mem::TRANSFER_WRITE.1)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST)
            .dst_access_mask(vk::AccessFlags2::HOST_READ)];
        // SAFETY: the slot's readback buffer is not in use (its fence was
        // waited for), and the image is in the layout the copy reads it in.
        unsafe {
            d.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                s.readback.buffer,
                &copy,
            );
            d.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().buffer_memory_barriers(&to_host),
            );
        }
        s.pending = true;
    }

    /// The colours slot `slot`'s last submission measured, if it measured
    /// any; call once its fence has signalled. Linear light.
    pub fn take(&mut self, slot: usize) -> Option<[Vec3; PATCHES]> {
        let s = self.slots.get_mut(slot)?;
        if !std::mem::take(&mut s.pending) {
            return None;
        }
        let bytes = s.readback.bytes();
        if bytes.len() < PATCHES * PIXEL {
            return None;
        }
        let mut cells = [Vec3::ZERO; PATCHES];
        for (cell, pixel) in cells.iter_mut().zip(bytes.as_chunks::<PIXEL>().0) {
            let channel = |i: usize| {
                let half = u16::from_le_bytes([pixel[2 * i], pixel[2 * i + 1]]);
                sane(f16_to_f32(half))
            };
            *cell = Vec3::new(channel(0), channel(1), channel(2));
        }
        Some(cells)
    }

    /// Forgets measurements still on their way (the light was turned off).
    pub fn discard(&mut self) {
        for s in &mut self.slots {
            s.pending = false;
        }
    }

    /// Frees everything. The device must be idle.
    pub fn destroy(&mut self, gpu: &Gpu) {
        for mut s in self.slots.drain(..) {
            s.readback.destroy(gpu);
        }
        // SAFETY: not in use (the renderer waits for its frames first); a
        // null handle is ignored.
        unsafe { gpu.device.destroy_pipeline(self.pipe, None) };
        self.pipe = vk::Pipeline::null();
        self.image.destroy(gpu);
    }
}

/// How a glow pipeline is made: a fullscreen triangle into the glow image,
/// with no depth attachment and no blending. `descriptor_buffer` is for the
/// TP10 path, whose descriptors are in descriptor buffers.
pub(crate) fn desc(
    spv: &[u8],
    layout: vk::PipelineLayout,
    descriptor_buffer: bool,
) -> PipelineDesc<'_> {
    PipelineDesc {
        spv,
        layout,
        color_format: FORMAT,
        vertex_stride: 0,
        attributes: &[],
        blend: false,
        descriptor_buffer,
        depth: Depth::None,
    }
}

/// A reading that cannot be light (NaN, infinite, negative) is black, and a
/// huge one is capped.
fn sane(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, CEILING)
    } else {
        0.0
    }
}

/// An IEEE half float as a single one.
pub(crate) fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let fraction = f32::from(bits & 0x3ff);
    match exponent {
        // Zero and subnormals: fraction / 1024 * 2^-14.
        0 => sign * fraction * 2f32.powi(-24),
        31 if bits & 0x3ff == 0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_decodes() {
        let cases = [
            (0x0000, 0.0),
            (0x3c00, 1.0),
            (0xc000, -2.0),
            (0x3800, 0.5),
            (0x4900, 10.0),
            (0x7bff, 65504.0),
            (0x0400, 2f32.powi(-14)),
            (0x0001, 2f32.powi(-24)),
            (0x3555, (1.0 + 341.0 / 1024.0) / 4.0),
        ];
        for (bits, want) in cases {
            let got = f16_to_f32(bits);
            assert!(
                (got - want).abs() <= want.abs() * 1e-6,
                "{bits:#06x} is {got}, not {want}"
            );
        }
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert_eq!(f16_to_f32(0xfc00), f32::NEG_INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
        // Negative zero keeps its sign, which does not matter to a colour.
        assert!(f16_to_f32(0x8000).is_sign_negative());
    }

    #[test]
    fn bad_readings_are_black_and_big_ones_capped() {
        assert_eq!(sane(f32::NAN), 0.0);
        assert_eq!(sane(f32::INFINITY), 0.0);
        assert_eq!(sane(-0.5), 0.0);
        assert_eq!(sane(0.25), 0.25);
        assert_eq!(sane(1000.0), CEILING);
    }
}
