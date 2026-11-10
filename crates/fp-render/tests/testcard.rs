//! Uploading the test card and its mip chain into an image, on a real
//! (software) Vulkan device: what a compositor layer's swapchain is filled
//! with.

use ash::vk;
use fp_render::mem::{self, Buffer, Image};
use fp_render::testcard;
use fp_render::{Gpu, Renderer};
use gpu_allocator::MemoryLocation;
use std::sync::Arc;

const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

fn setup() -> Option<Renderer> {
    let gpu = match Gpu::headless() {
        Ok(g) => Arc::new(g),
        Err(e) => {
            eprintln!("skipping: no Vulkan device ({e})");
            return None;
        }
    };
    Some(Renderer::new(gpu, vk::Format::R8G8B8A8_SRGB).expect("renderer"))
}

/// An image like a swapchain's: colour attachment and copy target, plus
/// what reading it back needs.
fn image(r: &Renderer, w: u32, h: u32, mips: u32) -> Image {
    let usage = vk::ImageUsageFlags::SAMPLED
        | vk::ImageUsageFlags::COLOR_ATTACHMENT
        | vk::ImageUsageFlags::TRANSFER_DST
        | vk::ImageUsageFlags::TRANSFER_SRC;
    Image::new(r.gpu(), w, h, &[FORMAT], usage, mips, "card test").expect("image")
}

/// Level `level` of `image`, `w`x`h` there, as RGBA8. The level must be in
/// COLOR_ATTACHMENT_OPTIMAL, where an upload leaves it, and is left in
/// TRANSFER_SRC_OPTIMAL.
fn read_level(r: &Renderer, image: vk::Image, level: u32, w: u32, h: u32) -> Vec<u8> {
    let size = w as usize * h as usize * 4;
    let mut readback = Buffer::new(
        r.gpu(),
        size as u64,
        vk::BufferUsageFlags::TRANSFER_DST,
        MemoryLocation::GpuToCpu,
        "card readback",
    )
    .expect("buffer");
    let buffer = readback.buffer;
    r.one_shot(|gpu, cmd| {
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(level, 1),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            mem::COLOR_WRITE,
            mem::TRANSFER_READ,
        );
        let region = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: level,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: w,
                height: h,
                depth: 1,
            });
        // SAFETY: recording; the level is in TRANSFER_SRC and the buffer
        // holds its texels.
        unsafe {
            gpu.device.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[region],
            );
        };
    })
    .expect("read back");
    let pixels = readback.bytes()[..size].to_vec();
    readback.destroy(r.gpu());
    pixels
}

fn refs(chain: &[(u32, u32, Vec<u8>)]) -> Vec<(u32, u32, &[u8])> {
    chain
        .iter()
        .map(|(w, h, px)| (*w, *h, px.as_slice()))
        .collect()
}

#[test]
fn every_level_of_the_card_chain_reads_back_as_uploaded() {
    let Some(r) = setup() else {
        return;
    };
    let card = testcard::card(128, 72);
    let chain = testcard::box_mips(&card, 128, 72);
    assert_eq!(chain.len(), 8);
    let mut img = image(&r, 128, 72, chain.len() as u32);
    r.upload_rgba_levels(img.image, &refs(&chain))
        .expect("upload");
    for (i, (w, h, px)) in chain.iter().enumerate() {
        let got = read_level(&r, img.image, i as u32, *w, *h);
        assert!(got == *px, "level {i} ({w}x{h}) differs");
    }
    img.destroy(r.gpu());
}

#[test]
fn a_chain_cut_short_fills_the_levels_it_has() {
    let Some(r) = setup() else {
        return;
    };
    let chain = testcard::box_mips(&testcard::checker(32, 32, 1), 32, 32);
    let mut img = image(&r, 32, 32, 6);
    // Only the first two levels of the six.
    r.upload_rgba_levels(img.image, &refs(&chain[..2]))
        .expect("upload");
    for (i, (w, h, px)) in chain[..2].iter().enumerate() {
        let got = read_level(&r, img.image, i as u32, *w, *h);
        assert!(got == *px, "level {i}");
    }
    img.destroy(r.gpu());
}

#[test]
fn one_level_uploads_as_before() {
    let Some(r) = setup() else {
        return;
    };
    let rgba = testcard::zone_plate(48, 40);
    let mut img = image(&r, 48, 40, 1);
    r.upload_rgba(img.image, 48, 40, &rgba).expect("upload");
    assert!(read_level(&r, img.image, 0, 48, 40) == rgba);
    img.destroy(r.gpu());
}

#[test]
fn a_level_of_the_wrong_size_or_no_levels_is_refused() {
    let Some(r) = setup() else {
        return;
    };
    let none = vk::Image::null();
    assert!(r.upload_rgba_levels(none, &[]).is_err());
    let short = [0u8; 12];
    assert!(r.upload_rgba_levels(none, &[(2, 2, &short[..])]).is_err());
    let ok = [0u8; 16];
    let both = [(2, 2, &ok[..]), (1, 1, &short[..])];
    assert!(r.upload_rgba_levels(none, &both).is_err());
}
