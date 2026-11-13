//! What the renderer's GPU tests share: a headless renderer with offscreen
//! eyes, small frames to show, and readers for the pixels that come back.
//! Every file in `tests/` is a crate of its own and uses part of this.
#![allow(dead_code)]

use fp_core::format::{Projection, StereoLayout, VideoFormat};
use fp_core::view::ViewSettings;
use fp_render::capture::OffscreenEyes;
use fp_render::env::{EnvFrame, Place, SeatName};
use fp_render::gpu::GpuOptions;
use fp_render::math::{Fov, projection, view};
use fp_render::{EyeView, Gpu, QuadDraw, Renderer, VideoParams};
use glam::{Mat4, Quat, Vec3};
use std::sync::Arc;

/// Side of the eye targets `setup` makes.
pub const SIZE: u32 = 128;

/// A renderer and eyes `size` pixels square, or `None` where there is no
/// Vulkan device (the test then skips).
pub fn setup_sized(size: u32) -> Option<(Renderer, OffscreenEyes)> {
    setup_samples(size, 1)
}

/// Like `setup_sized`, with the eye passes at `samples` samples a pixel
/// (`Renderer::samples` says how many the device gave).
pub fn setup_samples(size: u32, samples: u32) -> Option<(Renderer, OffscreenEyes)> {
    let gpu = match Gpu::headless() {
        Ok(g) => Arc::new(g),
        Err(e) => {
            eprintln!("skipping: no Vulkan device ({e})");
            return None;
        }
    };
    let mut r =
        Renderer::with_samples(gpu, ash::vk::Format::R8G8B8A8_SRGB, samples).expect("renderer");
    // A frame that shows a place shows it: the tests draw one at a time.
    r.set_inline_places(true);
    let eyes = OffscreenEyes::new(&r, size, size).expect("eyes");
    Some((r, eyes))
}

/// Like `setup_samples` (one sample), on a device that counts the place's
/// fragment shader invocations (`Renderer::place_fragment_counts`), or `None`
/// where there is no Vulkan device or it cannot count (the test then skips).
pub fn setup_counting(size: u32) -> Option<(Renderer, OffscreenEyes)> {
    let gpu = match Gpu::headless_with(GpuOptions {
        pipeline_statistics: true,
    }) {
        Ok(g) => Arc::new(g),
        Err(e) => {
            eprintln!("skipping: no Vulkan device ({e})");
            return None;
        }
    };
    let mut r = Renderer::with_samples(gpu, ash::vk::Format::R8G8B8A8_SRGB, 1).expect("renderer");
    if let Err(e) = r.count_place_fragments() {
        eprintln!("skipping: {e}");
        return None;
    }
    r.set_inline_places(true);
    let eyes = OffscreenEyes::new(&r, size, size).expect("eyes");
    Some((r, eyes))
}

pub fn setup() -> Option<(Renderer, OffscreenEyes)> {
    setup_sized(SIZE)
}

/// Frame whose pixel colour is a function of normalised (u, v).
pub fn frame(w: u32, h: u32, f: impl Fn(f32, f32) -> [u8; 3]) -> Arc<fp_media::VideoFrame> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = f((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
            rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    Arc::new(fp_media::VideoFrame::from_rgba(w, h, &rgba).expect("frame"))
}

/// Both eyes `height` metres up, looking down -z, 90 degrees across.
pub fn eyes_at(height: f32) -> [EyeView; 2] {
    eyes_looking(height, Quat::IDENTITY)
}

/// Both eyes `height` metres up, turned to `orientation`.
pub fn eyes_looking(height: f32, orientation: Quat) -> [EyeView; 2] {
    let ev = |x: f32| EyeView {
        view: view(Vec3::new(x, height, 0.0), orientation),
        proj: projection(Fov::symmetric(90.0), 0.05, 100.0),
    };
    [ev(-0.032), ev(0.032)]
}

/// One frame from eyes at 1.6 m, read back for both eyes.
pub fn render(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    f: Option<&Arc<fp_media::VideoFrame>>,
    p: &VideoParams,
    quads: &[QuadDraw],
) -> [Vec<u8>; 2] {
    render_eyes(r, e, f, p, quads, &eyes_at(1.6))
}

/// One frame from the given eyes, read back for both eyes.
pub fn render_eyes(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    f: Option<&Arc<fp_media::VideoFrame>>,
    p: &VideoParams,
    quads: &[QuadDraw],
    eyes: &[EyeView; 2],
) -> [Vec<u8>; 2] {
    r.begin_frame().unwrap();
    r.set_video(f).unwrap();
    r.draw(&e.targets(), eyes, p, quads).unwrap();
    r.end_frame().unwrap();
    [e.read(r, 0).unwrap(), e.read(r, 1).unwrap()]
}

pub fn px(img: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * SIZE + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

pub fn alpha(img: &[u8], x: u32, y: u32) -> u8 {
    img[((y * SIZE + x) * 4 + 3) as usize]
}

pub fn centre(img: &[u8]) -> [u8; 3] {
    px(img, SIZE / 2, SIZE / 2)
}

pub fn is_red(c: [u8; 3]) -> bool {
    c[0] > 200 && c[1] < 60 && c[2] < 60
}

pub fn is_blue(c: [u8; 3]) -> bool {
    c[2] > 200 && c[0] < 60 && c[1] < 60
}

pub fn params(projection: Projection, stereo: StereoLayout, settings: ViewSettings) -> VideoParams {
    VideoParams {
        format: VideoFormat::new(projection, stereo),
        settings,
        ..Default::default()
    }
}

/// What the eye target holds for linear `c` (1.0 the screen's white).
pub fn srgb(c: [f32; 3]) -> [u8; 3] {
    c.map(|l| {
        let l = l.clamp(0.0, 1.0);
        let e = if l <= 0.003_130_8 {
            12.92 * l
        } else {
            1.055 * l.powf(1.0 / 2.4) - 0.055
        };
        (e * 255.0).round() as u8
    })
}

pub fn near(got: [u8; 3], want: [u8; 3], tol: u8) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol)
}

/// Where the viewer's eyes are in the world, metres up.
pub const EYE_HEIGHT: f32 = 1.6;

/// A place with `seat`'s eye at the viewer's eyes, and the house lights at
/// `house`.
pub fn place_frame(place: Place, seat: SeatName, house: f32) -> EnvFrame {
    let layout = place.layout();
    let index = layout.seat_index(seat).unwrap_or(layout.default_seat);
    let anchor = Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0));
    EnvFrame {
        place,
        seat,
        room_to_world: layout.seat(index).room_to_world(anchor),
        house,
        screen_light: 0.0,
        time: 0.0,
    }
}

/// The cinema's own screen holds the picture: the room's frame, the biggest
/// picture that fits it.
pub fn cinema_params(settings: ViewSettings, env: EnvFrame) -> VideoParams {
    let layout = Place::Cinema.layout();
    let screen = layout.screen.expect("the cinema has a screen");
    let mut p = params(Projection::Flat, StereoLayout::Mono, settings);
    p.screen_pose = env.room_to_world * Mat4::from_translation(screen.centre);
    p.screen_fit = Some(screen.max);
    p.background = [0.0, 0.0, 0.0, 1.0];
    p.env = Some(env);
    p
}

/// `mesh.bin` of the wall the test packs hold: an 8 m square 2 m in front of
/// the viewer.
pub fn wall_mesh() -> Vec<u8> {
    let corners = [
        [-4.0f32, -4.0, -2.0, 0.0, 1.0],
        [4.0, -4.0, -2.0, 1.0, 1.0],
        [4.0, 4.0, -2.0, 1.0, 0.0],
        [-4.0, 4.0, -2.0, 0.0, 0.0],
    ];
    let mut mesh = b"LPM1".to_vec();
    mesh.extend(4u32.to_le_bytes());
    mesh.extend(6u32.to_le_bytes());
    for f in corners.iter().flatten() {
        mesh.extend(f.to_le_bytes());
    }
    for i in [0u32, 1, 2, 0, 2, 3] {
        mesh.extend(i.to_le_bytes());
    }
    mesh
}

/// [`wall_mesh`] as `mesh.bin` in `dir`.
pub fn write_wall_mesh(dir: &std::path::Path) {
    std::fs::write(dir.join("mesh.bin"), wall_mesh()).expect("mesh");
}

/// One ASTC block that is one colour (a void-extent block).
pub fn void_block(rgba: [u8; 4]) -> [u8; 16] {
    let mut bits: u128 = 0x1FC | (0b11 << 10);
    bits |= ((1u128 << 52) - 1) << 12;
    for (k, c) in rgba.iter().enumerate() {
        bits |= u128::from(u16::from(*c) * 257) << (64 + 16 * k);
    }
    bits.to_le_bytes()
}

/// A KTX2 file: `size` texels square, ASTC 4 x 4, one colour, a full mip
/// chain (levels stored smallest first, as the format says).
pub fn ktx2_solid(size: u32, srgb: bool, rgba: [u8; 4]) -> Vec<u8> {
    let levels: Vec<Vec<u8>> = (0..=size.ilog2())
        .map(|i| {
            let blocks = (size >> i).max(1).div_ceil(4).pow(2);
            (0..blocks).flat_map(|_| void_block(rgba)).collect()
        })
        .collect();
    // VK_FORMAT_ASTC_4x4_UNORM_BLOCK is 157, the sRGB one 158.
    let mut out = vec![
        0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
    ];
    let words = [
        157 + u32::from(srgb),
        1,
        size,
        size,
        0,
        0,
        1,
        levels.len() as u32,
        0,
    ];
    for w in words {
        out.extend(w.to_le_bytes());
    }
    out.extend([0u8; 32]);
    let mut at = (80 + 24 * levels.len() as u64).next_multiple_of(16);
    let mut offsets = vec![0; levels.len()];
    for i in (0..levels.len()).rev() {
        offsets[i] = at;
        at = (at + levels[i].len() as u64).next_multiple_of(16);
    }
    for (i, level) in levels.iter().enumerate() {
        out.extend(offsets[i].to_le_bytes());
        out.extend((level.len() as u64).to_le_bytes());
        out.extend((level.len() as u64).to_le_bytes());
    }
    for i in (0..levels.len()).rev() {
        out.resize(offsets[i] as usize, 0);
        out.extend(&levels[i]);
    }
    out
}
