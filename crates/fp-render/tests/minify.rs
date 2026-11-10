//! The video pyramid and the picture where it is minified: fine detail turns
//! grey instead of shimmering, steadily under small head turns, without ever
//! mixing the two eyes' halves of a stereo frame, whatever the upload layout
//! and turn of the frame. On a real (software) Vulkan device. Anisotropy is
//! not asserted: the software device may not have it.

use fp_core::format::{Projection, StereoLayout};
use fp_core::view::ViewSettings;
use fp_media::PixelLayout;
use fp_render::capture::OffscreenEyes;
use fp_render::testcard;
use fp_render::{Quality, Renderer, VideoParams};
use glam::{Mat4, Quat, Vec3};
use std::sync::Arc;

mod common;
use common::{
    SIZE, centre, eyes_at, eyes_looking, frame, is_blue, is_red, params, px, render, render_eyes,
    setup,
};

type Frame = Arc<fp_media::VideoFrame>;

/// A 1-texel black and white checker of `size` texels square.
fn checker_frame(size: u32) -> Frame {
    let rgba = testcard::checker(size, size, 1);
    Arc::new(fp_media::VideoFrame::from_rgba(size, size, &rgba).expect("frame"))
}

/// A screen that fills the 90 degree view across (4 m wide, 2 m away), so a
/// picture `n` texels across has `n / 128` texels under each pixel of the 128
/// pixel eyes.
fn filling_screen(stereo: StereoLayout) -> VideoParams {
    let settings = ViewSettings {
        screen_width: 4.0,
        ..ViewSettings::default()
    };
    let mut p = params(Projection::Flat, stereo, settings);
    p.screen_pose = Mat4::from_translation(Vec3::new(0.0, 1.6, -2.0));
    p.background = [0.0, 0.0, 0.0, 1.0];
    p
}

/// The green value of every pixel at least 8 from the image's edge.
fn interior(img: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    for y in 8..SIZE - 8 {
        for x in 8..SIZE - 8 {
            v.push(px(img, x, y)[1]);
        }
    }
    v
}

/// Mean and standard deviation.
fn stats(v: &[u8]) -> (f32, f32) {
    let n = v.len() as f32;
    let mean = v.iter().map(|&x| f32::from(x)).sum::<f32>() / n;
    let mut var = 0.0;
    for &x in v {
        var += (f32::from(x) - mean).powi(2);
    }
    (mean, (var / n).sqrt())
}

/// The most two pictures' values differ by anywhere.
fn worst_difference(a: &[u8], b: &[u8]) -> u8 {
    let mut worst = 0;
    for (x, y) in a.iter().zip(b) {
        worst = worst.max(x.abs_diff(*y));
    }
    worst
}

/// How much the interior changes when the head turns a quarter of a pixel
/// (90 degrees across 128 pixels is 0.0123 rad a pixel): the mean absolute
/// difference of the two renders, in levels.
fn shimmer(r: &mut Renderer, e: &mut OffscreenEyes, f: &Frame, p: &VideoParams) -> f32 {
    let turned = eyes_looking(1.6, Quat::from_rotation_y(0.00307));
    let [a, _] = render_eyes(r, e, Some(f), p, &[], &eyes_at(1.6));
    let [b, _] = render_eyes(r, e, Some(f), p, &[], &turned);
    let (a, b) = (interior(&a), interior(&b));
    let mut sum = 0.0;
    for (x, y) in a.iter().zip(&b) {
        sum += f32::from(x.abs_diff(*y));
    }
    sum / a.len() as f32
}

fn without_pyramid() -> Quality {
    Quality {
        pyramid: false,
        ..Quality::default()
    }
}

#[test]
fn minified_checker_turns_grey() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    // 640 texels across 128 pixels: five texels under each.
    let f = checker_frame(640);
    let p = filling_screen(StereoLayout::Mono);
    let [with, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert!(r.pyramid_levels() > 0, "the pyramid was built");
    let (mean, spread) = stats(&interior(&with));
    assert!((mean - 128.0).abs() < 12.0, "mean {mean}");
    assert!(spread < 12.0, "flat grey: spread {spread}");

    // Without the pyramid or the four taps the same set-up aliases hard, so
    // the grey above is the pyramid's doing.
    r.set_quality(Quality {
        supersample: false,
        ..without_pyramid()
    });
    let [without, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert_eq!(r.pyramid_levels(), 0);
    let (_, aliased) = stats(&interior(&without));
    assert!(aliased > 40.0, "one tap aliases: spread {aliased}");
    e.destroy(&r);
}

#[test]
fn pyramid_steadies_subpixel_motion() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let f = checker_frame(640);
    let p = filling_screen(StereoLayout::Mono);
    let steady = shimmer(&mut r, &mut e, &f, &p);
    assert!(r.pyramid_levels() > 0, "the pyramid was built");
    r.set_quality(without_pyramid());
    let four_taps = shimmer(&mut r, &mut e, &f, &p);
    assert!(four_taps > 1.0, "the four taps shimmer: {four_taps}");
    assert!(
        steady * 2.0 < four_taps,
        "shimmer {steady} with the pyramid, {four_taps} with the four taps"
    );
    e.destroy(&r);
}

#[test]
fn magnified_picture_is_unchanged_by_the_pyramid() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    // 32 texels across 128 pixels: four pixels to a texel.
    let f = frame(32, 32, |u, v| [(u * 255.0) as u8, (v * 255.0) as u8, 90]);
    let p = filling_screen(StereoLayout::Mono);
    let [with, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert_eq!(r.pyramid_levels(), 0, "magnified: nothing to build");
    r.set_quality(without_pyramid());
    let [without, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
    let worst = worst_difference(&with, &without);
    assert!(worst <= 1, "the pictures differ by {worst}");
    e.destroy(&r);
}

#[test]
fn stereo_halves_stay_apart_when_minified() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    // Red in the left half, blue in the right: 1024 texels of each eye's own
    // half across 128 pixels, eight under each.
    let f = frame(2048, 512, |u, _| {
        if u < 0.5 {
            [230, 20, 20]
        } else {
            [20, 20, 230]
        }
    });
    let p = filling_screen(StereoLayout::SideBySide);
    let [left, right] = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert!(r.pyramid_levels() > 0, "the pyramid was built");
    assert!(is_red(centre(&left)), "left eye {:?}", centre(&left));
    assert!(is_blue(centre(&right)), "right eye {:?}", centre(&right));
    for y in 0..SIZE {
        for x in 0..SIZE {
            let [lr, _, lb] = px(&left, x, y);
            assert!(lr <= 60 || lb <= 60, "blue in the left eye at {x},{y}");
            let [rr, _, rb] = px(&right, x, y);
            assert!(rr <= 60 || rb <= 60, "red in the right eye at {x},{y}");
        }
    }
    e.destroy(&r);
}

/// Draws `rgba` (a 640 texel square picture) as every upload layout and range
/// and checks each matches the 8-bit full range one to within `tolerance`.
///
/// Each pattern gets a renderer of its own: the pyramid is given up for a
/// picture size once its builds keep going over their time budget, and on the
/// software device (which builds on the CPU) every build does, so one renderer
/// can be asked for about a dozen builds before it falls back to the four taps.
fn layouts_minify_alike(name: &str, rgba: &[u8], tolerance: u8) {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let p = filling_screen(StereoLayout::Mono);
    let size = 640u32;
    let layouts = [
        PixelLayout::I420 { bits: 10 },
        PixelLayout::Nv12,
        PixelLayout::P010,
    ];
    {
        let mut draw = |layout, full_range| {
            let f = fp_media::VideoFrame::from_rgba_as(size, size, rgba, layout, full_range);
            let f = Arc::new(f.expect("frame"));
            let [l, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
            assert!(r.pyramid_levels() > 0, "{name} as {layout:?}: no pyramid");
            interior(&l)
        };
        let want = draw(PixelLayout::I420 { bits: 8 }, true);
        for layout in layouts {
            for full_range in [true, false] {
                let got = draw(layout, full_range);
                let worst = worst_difference(&got, &want);
                assert!(
                    worst <= tolerance,
                    "{name} as {layout:?} full_range={full_range}: off by {worst}"
                );
            }
        }
    }
    e.destroy(&r);
}

#[test]
fn every_upload_layout_minifies_a_checker_alike() {
    let rgba = testcard::checker(640, 640, 1);
    layouts_minify_alike("black and white", &rgba, 4);
}

#[test]
fn every_upload_layout_minifies_coloured_squares_alike() {
    // Red and blue squares four texels across, so chroma has detail of its own.
    let size = 640u32;
    let mut squares = Vec::new();
    for y in 0..size {
        for x in 0..size {
            if (x / 4 + y / 4).is_multiple_of(2) {
                squares.extend_from_slice(&[230, 20, 20, 255]);
            } else {
                squares.extend_from_slice(&[30, 40, 220, 255]);
            }
        }
    }
    layouts_minify_alike("red and blue", &squares, 8);
}

#[test]
fn a_sideways_clip_minified_stays_upright() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    // Stored on its side, 360 wide and 640 tall; turned a quarter it is a
    // 640 x 360 picture with red top left, green top right, blue bottom left
    // and white bottom right (the upright point (u, v) is stored at
    // (v, 1 - u)).
    let f = frame(360, 640, |su, sv| match (sv > 0.5, su < 0.5) {
        (true, true) => [230, 20, 20],
        (false, true) => [20, 220, 40],
        (true, false) => [30, 40, 220],
        (false, false) => [230, 230, 230],
    });
    let mut p = filling_screen(StereoLayout::Mono);
    p.rotation = 90;
    for pyramid in [true, false] {
        r.set_quality(Quality {
            pyramid,
            ..Quality::default()
        });
        let [l, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
        assert_eq!(r.pyramid_levels() > 0, pyramid);
        // The picture is 128 pixels across and 72 down, in the middle.
        let at = |x, y| px(&l, x, y);
        let (tl, tr, bl, br) = (at(32, 46), at(96, 46), at(32, 82), at(96, 82));
        assert!(is_red(tl), "{pyramid}: top left {tl:?}");
        let green = tr[1] > 180 && tr[0] < 80 && tr[2] < 100;
        assert!(green, "{pyramid}: top right {tr:?}");
        assert!(is_blue(bl), "{pyramid}: bottom left {bl:?}");
        let white = br.iter().all(|&c| c > 200);
        assert!(white, "{pyramid}: bottom right {br:?}");
    }
    e.destroy(&r);
}

#[test]
fn renders_without_a_pyramid() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let f = checker_frame(640);
    let p = filling_screen(StereoLayout::Mono);
    r.set_quality(without_pyramid());
    let [first, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
    let [again, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert_eq!(r.pyramid_levels(), 0);
    assert!(first == again, "the four taps render the same twice");
    // Turned back on, the same frame gets its pyramid at the next draw.
    r.set_quality(Quality::default());
    let [with, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert!(r.pyramid_levels() > 0);
    assert!(with != first, "the pyramid changes a minified picture");
    // And off again, it goes back to what it was.
    r.set_quality(without_pyramid());
    let [back, _] = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert_eq!(r.pyramid_levels(), 0);
    assert!(back == first);
    e.destroy(&r);
}

#[test]
fn a_new_picture_size_gets_a_pyramid_of_its_own() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let p = filling_screen(StereoLayout::Mono);
    let small = checker_frame(640);
    let large = checker_frame(1024);
    render(&mut r, &mut e, Some(&small), &p, &[]);
    // 640 texels: level 0 is 320 across, nine levels down to one.
    assert_eq!(r.pyramid_levels(), 9);
    let [l, _] = render(&mut r, &mut e, Some(&large), &p, &[]);
    // 1024 texels: 512 across, ten levels.
    assert_eq!(r.pyramid_levels(), 10);
    let (_, spread) = stats(&interior(&l));
    assert!(spread < 12.0, "grey at the new size: spread {spread}");
    // Back to the first size, on a pyramid made again.
    let [l, _] = render(&mut r, &mut e, Some(&small), &p, &[]);
    assert_eq!(r.pyramid_levels(), 9);
    let (_, spread) = stats(&interior(&l));
    assert!(spread < 12.0, "grey again: spread {spread}");
    e.destroy(&r);
}

#[test]
fn the_pyramids_build_is_timed_apart_from_the_eyes() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let p = filling_screen(StereoLayout::Mono);
    // Two different frames, so each draw has a pyramid to build.
    let (a, b) = (checker_frame(640), checker_frame(640));
    for f in [&a, &b, &a] {
        render(&mut r, &mut e, Some(f), &p, &[]);
    }
    let Some(t) = r.gpu_times() else {
        eprintln!("skipping: no GPU timestamps");
        e.destroy(&r);
        return;
    };
    let (Some(pyramid), Some(eyes), Some(total)) = (t.pyramid, t.eyes, t.total) else {
        panic!("parts missing: {t:?}");
    };
    assert!(pyramid > 0.0 && eyes > 0.0, "{t:?}");
    assert!(pyramid + eyes <= total, "{t:?}");
    e.destroy(&r);
}
