//! Places (a cinema, a studio) drawn around the picture, rendered on a real
//! (software) Vulkan device: the room surrounds the screen, nearer geometry
//! hides farther, the sky is above and the floor below, the house lights
//! light the room, a spherical picture hides it, and without a place the
//! eye passes draw what they always did. The picture's light on the room is
//! measured from real frames, in every layout a frame can arrive in.

use fp_core::format::{Projection, StereoLayout};
use fp_core::view::ViewSettings;
use fp_media::{PixelLayout, VideoFrame};
use fp_render::capture::OffscreenEyes;
use fp_render::env::{
    EnvFrame, Lighting, Material, Pattern, Place, PlaceData, SeatName, Sky, Vertex,
};
use fp_render::{Renderer, VideoParams};
use glam::{Mat4, Quat, Vec3};
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

mod common;
use common::{
    SIZE, centre, eyes_looking, frame, is_blue, is_red, near, params, px, render, render_eyes,
    setup, srgb,
};

/// Where the viewer's eyes are in the world, metres up.
const EYE_HEIGHT: f32 = 1.6;

/// A place with `seat`'s eye at the viewer's eyes, and the house lights at
/// `house`.
fn place_frame(place: Place, seat: SeatName, house: f32) -> EnvFrame {
    let layout = place.layout();
    let index = layout.seat_index(seat).unwrap_or(layout.default_seat);
    let anchor = Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0));
    EnvFrame {
        place,
        room_to_world: layout.seat(index).room_to_world(anchor),
        house,
        screen_light: false,
        time: 0.0,
    }
}

/// The cinema's own screen holds the picture: the room's frame, the biggest
/// picture that fits it.
fn cinema_params(settings: ViewSettings, env: EnvFrame) -> VideoParams {
    let layout = Place::Cinema.layout();
    let screen = layout.screen.expect("the cinema has a screen");
    let mut p = params(Projection::Flat, StereoLayout::Mono, settings);
    p.screen_pose = env.room_to_world * Mat4::from_translation(screen.centre);
    p.screen_fit = Some(screen.max);
    p.background = [0.0, 0.0, 0.0, 1.0];
    p.env = Some(env);
    p
}

/// The same picture on a screen of the user's own, in front of nothing.
fn plain_params() -> VideoParams {
    let mut p = params(
        Projection::Flat,
        StereoLayout::Mono,
        ViewSettings::default(),
    );
    p.screen_pose = Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, -4.0));
    p.background = [0.0, 0.0, 1.0, 1.0];
    p
}

/// The four corners of an eye image, a little in from the edge.
fn corners(img: &[u8]) -> [[u8; 3]; 4] {
    let far = SIZE - 3;
    let spots = [(2, 2), (far, 2), (2, far), (far, far)];
    spots.map(|(x, y)| px(img, x, y))
}

/// The mean of all channels over the corners.
fn corner_mean(img: &[u8]) -> f32 {
    let sum: u32 = corners(img).iter().flatten().map(|&c| c as u32).sum();
    sum as f32 / 12.0
}

/// A flat square of side `2 * half` at depth `z` in front of a viewer at the
/// origin, facing +z, made of `material`.
fn square(data: &mut PlaceData, z: f32, half: f32, material: u32) {
    let base = data.vertices.len() as u32;
    for (x, y) in [(-half, -half), (half, -half), (half, half), (-half, half)] {
        data.vertices.push(Vertex {
            pos: [x, y, z],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0, 0.0],
            ao: 1.0,
            house: 0.0,
            material,
        });
    }
    let triangles = [base, base + 1, base + 2, base, base + 2, base + 3];
    data.indices.extend(triangles);
}

fn glowing(albedo: [f32; 3]) -> Material {
    Material {
        albedo,
        pattern: Pattern::Emissive,
        emissive: 1.0,
        sheen: 0.0,
    }
}

/// An empty place with a red material 0 and a blue material 1, both glowing.
fn red_and_blue() -> PlaceData {
    let dark = [0.0; 3];
    PlaceData {
        vertices: Vec::new(),
        indices: Vec::new(),
        palette: vec![glowing([1.0, 0.0, 0.0]), glowing([0.0, 0.0, 1.0])],
        lighting: Lighting {
            ambient_up: dark,
            ambient_down: dark,
            house_rgb: dark,
            spill: 1.0,
            fog: None,
        },
        sky: Sky::None,
    }
}

#[test]
fn a_room_surrounds_the_screen() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let red = frame(160, 90, |_, _| [230, 20, 20]);
    let env = place_frame(Place::Cinema, SeatName::Middle, 1.0);
    let p = cinema_params(ViewSettings::default(), env);
    let [l, rr] = render(&mut r, &mut e, Some(&red), &p, &[]);
    for (name, img) in [("left", &l), ("right", &rr)] {
        assert!(is_red(centre(img)), "{name} centre {:?}", centre(img));
        for c in corners(img) {
            assert_ne!(c, [0, 0, 0], "{name}: a corner is the background");
            assert!(!is_red(c), "{name}: a corner is the picture {c:?}");
        }
    }
    e.destroy(&r);
}

#[test]
fn a_place_shows_around_the_menus_with_no_video() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let env = place_frame(Place::Cinema, SeatName::Middle, 1.0);
    let p = cinema_params(ViewSettings::default(), env);
    let [l, _] = render(&mut r, &mut e, None, &p, &[]);
    assert_ne!(centre(&l), [0, 0, 0], "the screen panel is there");
    for c in corners(&l) {
        assert_ne!(c, [0, 0, 0], "a corner is the background");
    }
    e.destroy(&r);
}

#[test]
fn nearer_geometry_hides_farther() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    // A red square 2 m away, then a blue one 4 m away that fills the view,
    // drawn last: only the depth test keeps the red in front.
    let mut data = red_and_blue();
    square(&mut data, -2.0, 1.0, 0);
    square(&mut data, -4.0, 4.0, 1);
    r.set_place_data(Place::Cinema, data);
    let anchor = Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0));
    let env = EnvFrame {
        place: Place::Cinema,
        room_to_world: anchor,
        house: 1.0,
        screen_light: false,
        time: 0.0,
    };
    let mut p = plain_params();
    p.background = [0.0, 0.0, 0.0, 1.0];
    p.env = Some(env);
    let [l, _] = render(&mut r, &mut e, None, &p, &[]);
    assert!(is_red(centre(&l)), "centre {:?}", centre(&l));
    // The red square spans a third of the view: past it the blue shows.
    let beside = px(&l, SIZE / 2 + 48, SIZE / 2);
    assert!(is_blue(beside), "beside the red {beside:?}");
    e.destroy(&r);
}

#[test]
fn the_studio_sky_is_above_and_the_floor_below() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let Sky::Gradient { zenith, .. } = Place::Studio.build().sky else {
        panic!("the studio has a gradient sky");
    };
    let mut p = plain_params();
    p.env = Some(place_frame(Place::Studio, SeatName::Middle, 1.0));
    let [up, _] = look(&mut r, &mut e, &p, FRAC_PI_2);
    assert!(
        near(centre(&up), srgb(zenith), 6),
        "up {:?}, the zenith is {:?}",
        centre(&up),
        srgb(zenith)
    );
    let [down, _] = look(&mut r, &mut e, &p, -FRAC_PI_2);
    let floor = centre(&down);
    let lit = floor.iter().all(|&c| c >= 8);
    assert!(lit, "the floor is lit: {floor:?}");
    assert!(!is_blue(floor), "the background shows {floor:?}");
    assert_ne!(floor, centre(&up));
    e.destroy(&r);
}

/// A frame of `p` with no video from eyes turned `pitch` radians up.
fn look(r: &mut Renderer, e: &mut OffscreenEyes, p: &VideoParams, pitch: f32) -> [Vec<u8>; 2] {
    let eyes = eyes_looking(EYE_HEIGHT, Quat::from_rotation_x(pitch));
    render_eyes(r, e, None, p, &[], &eyes)
}

/// The sRGB code, in codes, of linear `l`.
fn code(l: f32) -> f32 {
    255.0 * (1.055 * l.powf(1.0 / 2.4) - 0.055)
}

#[test]
fn a_dark_sky_is_dithered_to_the_part_of_a_code() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let Sky::Gradient { zenith, .. } = Place::Studio.build().sky else {
        panic!("the studio has a gradient sky");
    };
    let mut p = plain_params();
    p.env = Some(place_frame(Place::Studio, SeatName::Middle, 1.0));
    let [up, _] = look(&mut r, &mut e, &p, FRAC_PI_2);
    // Straight above, the sky is one colour in the middle of the view: its
    // light is a fraction of a code, which only noise can show in 8 bits.
    let mut sums = [0u32; 3];
    let mut seen = std::collections::BTreeSet::new();
    let (lo, hi) = (SIZE / 2 - 20, SIZE / 2 + 20);
    for y in lo..hi {
        for x in lo..hi {
            let c = px(&up, x, y);
            seen.insert(c[1]);
            for (sum, v) in sums.iter_mut().zip(c) {
                *sum += v as u32;
            }
        }
    }
    let n = ((hi - lo) * (hi - lo)) as f32;
    for (i, sum) in sums.iter().enumerate() {
        let (got, want) = (*sum as f32 / n, code(zenith[i]));
        assert!(
            (got - want).abs() < 0.3,
            "channel {i}: the mean is {got}, the sky is {want} codes"
        );
    }
    // Noise of half a code moves a pixel no further than the next code.
    let (first, last) = (seen.first().unwrap(), seen.last().unwrap());
    assert!(last - first <= 2, "grain beyond a code: {seen:?}");
    assert!(seen.len() >= 2, "no dither at all: {seen:?}");
    e.destroy(&r);
}

#[test]
fn house_lights_brighten_the_room() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let mut mean = |house: f32| {
        let env = place_frame(Place::Cinema, SeatName::Middle, house);
        let p = cinema_params(ViewSettings::default(), env);
        let [l, _] = render(&mut r, &mut e, None, &p, &[]);
        corner_mean(&l)
    };
    let (playing, up) = (mean(0.04), mean(1.0));
    assert!(
        up > 3.0 * playing,
        "lights up {up} against lights down {playing}"
    );
    e.destroy(&r);
}

#[test]
fn spherical_video_hides_the_place() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let gradient = |u: f32, v: f32| [(u * 255.0) as u8, (v * 255.0) as u8, 90];
    let f = frame(256, 128, gradient);
    let mut p = params(
        Projection::EQUIRECT_180,
        StereoLayout::Mono,
        ViewSettings::default(),
    );
    p.background = [0.0, 0.0, 0.0, 1.0];
    let without = render(&mut r, &mut e, Some(&f), &p, &[]);
    p.env = Some(place_frame(Place::Cinema, SeatName::Middle, 1.0));
    let with = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert!(
        with == without,
        "the place showed through a 180 degree video"
    );
    p.format.projection = Projection::fisheye(180.0);
    let fish_with = render(&mut r, &mut e, Some(&f), &p, &[]);
    p.env = None;
    let fish_without = render(&mut r, &mut e, Some(&f), &p, &[]);
    assert!(
        fish_with == fish_without,
        "the place showed through a fisheye"
    );
    e.destroy(&r);
}

#[test]
fn keyed_pixels_show_the_place() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let green = frame(160, 90, |_, _| [20, 220, 40]);
    let keyed = ViewSettings {
        chroma_key: true,
        key_color: [0.0, 1.0, 0.0],
        ..ViewSettings::default()
    };
    let env = place_frame(Place::Cinema, SeatName::Middle, 1.0);
    let room_only = render(
        &mut r,
        &mut e,
        None,
        &cinema_params(ViewSettings::default(), env),
        &[],
    );
    let see_through = render(
        &mut r,
        &mut e,
        Some(&green),
        &cinema_params(keyed, env),
        &[],
    );
    let (room, seen) = (centre(&room_only[0]), centre(&see_through[0]));
    assert_ne!(room, [0, 0, 0], "the room is the screen panel: {room:?}");
    assert!(
        near(seen, room, 6),
        "keyed out, the centre is the room's {room:?}, not {seen:?}"
    );
    // Not keyed, the same frame is the picture.
    let shown = render(
        &mut r,
        &mut e,
        Some(&green),
        &cinema_params(ViewSettings::default(), env),
        &[],
    );
    assert!(centre(&shown[0])[1] > 180, "{:?}", centre(&shown[0]));
    e.destroy(&r);
}

#[test]
fn no_place_is_unchanged() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let green = frame(160, 90, |_, _| [0, 255, 0]);
    let before = render(&mut r, &mut e, Some(&green), &plain_params(), &[]);
    let corner = px(&before[0], 2, 2);
    assert!(is_blue(corner), "the surround {corner:?}");
    assert!(centre(&before[0])[1] > 200, "{:?}", centre(&before[0]));

    // A place, then none again: nothing of it stays behind, in the colour
    // or in the depth image the eye passes share.
    let mut with_place = plain_params();
    with_place.env = Some(place_frame(Place::Studio, SeatName::Middle, 1.0));
    let placed = render(&mut r, &mut e, Some(&green), &with_place, &[]);
    assert!(placed != before, "the studio changed nothing");
    let after = render(&mut r, &mut e, Some(&green), &plain_params(), &[]);
    assert!(after == before, "Plain differs after a place was shown");

    // And from the eyes a renderer that never showed a place has.
    let Some((mut fresh, mut fresh_eyes)) = setup() else {
        return;
    };
    let never = render(
        &mut fresh,
        &mut fresh_eyes,
        Some(&green),
        &plain_params(),
        &[],
    );
    assert!(never == before, "Plain differs with no place pass");
    fresh_eyes.destroy(&fresh);
    e.destroy(&r);
}

#[test]
fn a_place_built_on_a_worker_shows_when_it_is_ready() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    // As in the app: asked for on the first frame, Plain until it is built
    // off the render thread, then drawn.
    r.set_inline_places(false);
    let red = frame(160, 90, |_, _| [230, 20, 20]);
    let env = place_frame(Place::Cinema, SeatName::Middle, 1.0);
    let p = cinema_params(ViewSettings::default(), env);
    let mut shown_at = None;
    for n in 0..500 {
        let [l, _] = render(&mut r, &mut e, Some(&red), &p, &[]);
        if corners(&l).iter().all(|&c| c != [0, 0, 0]) {
            shown_at = Some(n);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(shown_at.is_some(), "the cinema was never shown");
    // And stays.
    let [l, _] = render(&mut r, &mut e, Some(&red), &p, &[]);
    assert!(is_red(centre(&l)), "centre {:?}", centre(&l));
    for c in corners(&l) {
        assert_ne!(c, [0, 0, 0], "a corner is the background");
    }
    e.destroy(&r);
}
/// The cinema playing, as the picture's light sees it: the house lights down
/// to their level while a film plays, the light on or off, `time` seconds in.
fn lit_params(time: f64, light: bool) -> VideoParams {
    let mut env = place_frame(Place::Cinema, SeatName::Middle, 0.04);
    env.screen_light = light;
    env.time = time;
    cinema_params(ViewSettings::default(), env)
}

/// Four frames of `picture` in the cinema, a second apart from second
/// `start`: the colours are measured two frames behind and ease over a
/// quarter of a second, so the last frame has the room fully lit by it.
/// That frame's left eye.
fn watch(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    picture: &Arc<VideoFrame>,
    start: u32,
    light: bool,
) -> Vec<u8> {
    let mut last = Vec::new();
    for k in 0..4 {
        let p = lit_params(f64::from(start + k), light);
        let [left, _] = render(r, e, Some(picture), &p, &[]);
        last = left;
    }
    last
}

/// The rows of an eye image from `first` down, as bytes.
fn rows_from(img: &[u8], first: u32) -> &[u8] {
    &img[(first * SIZE * 4) as usize..]
}

/// The mean colour of the rows from `first` down, every column: the stage,
/// floor and seats below the screen, which the picture lights most.
fn lower_mean(img: &[u8], first: u32) -> [f32; 3] {
    let mut sum = [0u32; 3];
    for y in first..SIZE {
        for x in 0..SIZE {
            for (s, v) in sum.iter_mut().zip(px(img, x, y)) {
                *s += u32::from(v);
            }
        }
    }
    let n = ((SIZE - first) * SIZE) as f32;
    sum.map(|s| s as f32 / n)
}

#[test]
fn glow_reads_each_patch_of_the_picture() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    // Top left red, top right blue, bottom left green, bottom right white.
    let quadrants = frame(160, 90, |u, v| match (u < 0.5, v < 0.5) {
        (true, true) => [230, 20, 20],
        (false, true) => [20, 20, 230],
        (true, false) => [20, 200, 20],
        (false, false) => [235, 235, 235],
    });
    let p = lit_params(0.0, true);
    for _ in 0..4 {
        render(&mut r, &mut e, Some(&quadrants), &p, &[]);
    }
    // Four cells across, two down, the top row first: two cells of each.
    let cells = r.screen_light();
    let (red, blue, green, white) = (0..2, 2..4, 4..6, 6..8);
    for (name, cells_of, ok) in [
        ("red", red, [true, false, false]),
        ("blue", blue, [false, false, true]),
        ("green", green, [false, true, false]),
    ] {
        for i in cells_of {
            let c = cells[i];
            for (ch, lit) in ok.into_iter().enumerate() {
                if lit {
                    assert!(c[ch] > 0.4, "cell {i} should be {name}: {c:?}");
                } else {
                    assert!(c[ch] < 0.15, "cell {i} should be {name}: {c:?}");
                }
            }
        }
    }
    for i in white {
        let c = cells[i];
        let bright = c.iter().all(|&v| v > 0.6);
        assert!(bright, "cell {i} should be white: {c:?}");
    }
    e.destroy(&r);
}

#[test]
fn the_room_takes_the_pictures_colour() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let red = frame(160, 90, |_, _| [230, 20, 20]);
    let blue = frame(160, 90, |_, _| [20, 20, 230]);
    let in_red = lower_mean(&watch(&mut r, &mut e, &red, 1, true), 90);
    let in_blue = lower_mean(&watch(&mut r, &mut e, &blue, 5, true), 90);
    // The light follows the film: red then blue turns the room from one to
    // the other, in the colour channel it was lit by. The rows measured are
    // mostly the backs of the seats in front, which face away from the screen
    // and get none of its light, and teal fabric that gives back little red,
    // so the mean moves by about a code in 255, not by tens: the changes
    // asked for are several times what rounding could make (the render is the
    // same every time, and with the light off the two pictures give exactly
    // the same room, see the next test).
    let tint = |m: [f32; 3]| m[0] - m[2];
    assert!(
        tint(in_red) - tint(in_blue) > 0.8,
        "red {in_red:?}, then blue {in_blue:?}"
    );
    let (red_up, blue_up) = (in_red[0] - in_blue[0], in_blue[2] - in_red[2]);
    assert!(red_up > 0.25, "red {in_red:?}, blue {in_blue:?}");
    assert!(blue_up > 0.4, "red {in_red:?}, blue {in_blue:?}");
    e.destroy(&r);
}

#[test]
fn every_upload_layout_lights_the_room_alike() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let p = lit_params(0.0, true);
    // 200 pixels wide: every layout's rows get padded.
    let (w, h) = (200u32, 100u32);
    let mut measure = |rgb: [u8; 3], layout: PixelLayout, full_range: bool| {
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect();
        let f = VideoFrame::from_rgba_as(w, h, &rgba, layout, full_range).expect("frame");
        let f = Arc::new(f);
        for _ in 0..4 {
            render(&mut r, &mut e, Some(&f), &p, &[]);
        }
        r.screen_light()
    };
    let layouts = [
        PixelLayout::I420 { bits: 10 },
        PixelLayout::Nv12,
        PixelLayout::P010,
    ];
    let paints = [
        ("red", [230, 20, 20]),
        ("green", [20, 200, 40]),
        ("blue", [30, 40, 220]),
        ("grey", [100, 120, 140]),
    ];
    for (name, rgb) in paints {
        let want = measure(rgb, PixelLayout::I420 { bits: 8 }, true);
        // The picture is lit up, not black.
        assert!(want[3].iter().any(|&v| v > 0.05), "{name}: {:?}", want[3]);
        for layout in layouts {
            for full_range in [true, false] {
                // Quantisation moves a level by a few codes, a wrong plane
                // offset or matrix by tens: in linear light, a few hundredths
                // against tenths.
                let got = measure(rgb, layout, full_range);
                for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                    let mut worst = 0.0f32;
                    for (a, b) in g.iter().zip(w) {
                        worst = worst.max((a - b).abs());
                    }
                    assert!(
                        worst < 0.06,
                        "{name} as {layout:?} full_range={full_range}: cell {i} is \
                         {g:?}, I420 gives {w:?}"
                    );
                }
            }
        }
    }
    e.destroy(&r);
}

#[test]
fn screen_light_off_leaves_the_room_unlit_by_the_picture() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let red = frame(160, 90, |_, _| [230, 20, 20]);
    let blue = frame(160, 90, |_, _| [20, 20, 230]);
    let off_red = watch(&mut r, &mut e, &red, 1, false);
    assert_eq!(r.screen_light(), [[0.0; 3]; 8], "something was measured");
    let off_blue = watch(&mut r, &mut e, &blue, 5, false);
    // Below the picture the room is the same whatever it shows.
    assert!(
        rows_from(&off_red, 90) == rows_from(&off_blue, 90),
        "the room changed with the picture while its light was off"
    );
    // And it is the same room, lit by the picture or not, in the dark: only
    // the light on the floor and treads differs (a code or so in 255 over
    // these rows, see the_room_takes_the_pictures_colour).
    let on_red = watch(&mut r, &mut e, &red, 9, true);
    let (dark, lit) = (lower_mean(&off_red, 90), lower_mean(&on_red, 90));
    assert!(lit[0] > dark[0] + 0.25, "off {dark:?}, on {lit:?}");
    e.destroy(&r);
}
