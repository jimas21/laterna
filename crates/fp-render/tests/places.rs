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
use std::time::{Duration, Instant};

mod common;
use common::{
    EYE_HEIGHT, SIZE, centre, cinema_params, eyes_looking, frame, is_blue, is_red, near, params,
    place_frame, px, render, render_eyes, setup, setup_samples, srgb,
};

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
        baked: None,
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
        screen_light: 0.0,
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
#[test]
fn switching_places_never_shows_plain_in_between() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    r.set_inline_places(false);
    let red = frame(160, 90, |_, _| [230, 20, 20]);
    let cinema = cinema_params(
        ViewSettings::default(),
        place_frame(Place::Cinema, SeatName::Middle, 1.0),
    );
    let mut shown = false;
    for _ in 0..500 {
        let [l, _] = render(&mut r, &mut e, Some(&red), &cinema, &[]);
        if corners(&l).iter().all(|&c| c != [0, 0, 0]) {
            shown = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(shown, "the cinema was never shown");
    // Asked for the studio, which is not built yet: the cinema stays up
    // until the studio is ready, so no frame is the Plain one.
    let mut studio = cinema;
    studio.env = Some(place_frame(Place::Studio, SeatName::Middle, 1.0));
    let mut plain = studio;
    plain.env = None;
    let [bare, _] = render(&mut r, &mut e, Some(&red), &plain, &[]);
    for n in 0..60 {
        let [l, _] = render(&mut r, &mut e, Some(&red), &studio, &[]);
        assert!(l != bare, "frame {n} after the switch is Plain");
        thread::sleep(Duration::from_millis(5));
    }
    e.destroy(&r);
}

/// The cinema playing, as the picture's light sees it: the house lights down
/// to their level while a film plays, the light on or off, `time` seconds in.
fn lit_params(time: f64, light: bool) -> VideoParams {
    let mut env = place_frame(Place::Cinema, SeatName::Middle, 0.04);
    env.screen_light = if light { 1.0 } else { 0.0 };
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

/// The mean colour of the pixels in `cols` and `rows`.
fn region_mean(img: &[u8], cols: std::ops::Range<u32>, rows: std::ops::Range<u32>) -> [f32; 3] {
    let mut sum = [0u32; 3];
    let n = ((cols.end - cols.start) * (rows.end - rows.start)) as f32;
    for y in rows {
        for x in cols.clone() {
            for (s, v) in sum.iter_mut().zip(px(img, x, y)) {
                *s += u32::from(v);
            }
        }
    }
    sum.map(|s| s as f32 / n)
}

/// The first row below the picture and its edge from the middle seat: from
/// here down the eye sees the floor and the seats in front of it, with the
/// stage above them (the picture spans 72 degrees, so it reaches row 91; the
/// hall's rebuilt rake leaves this edge where it was).
const BELOW: u32 = 94;

/// What the middle seat sees of the room the picture lights: the ceiling
/// above it (rows 0 to 30, every column) and the seats in front below it
/// (rows from `BELOW`). Neither holds any of the picture itself.
fn ceiling_and_seats(img: &[u8]) -> ([f32; 3], [f32; 3]) {
    (
        region_mean(img, 0..SIZE, 0..30),
        region_mean(img, 0..SIZE, BELOW..SIZE),
    )
}

#[test]
fn the_ceiling_and_seats_visibly_take_the_pictures_colour() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let red = frame(160, 90, |_, _| [230, 20, 20]);
    let blue = frame(160, 90, |_, _| [20, 20, 230]);
    let (ceiling_red, seats_red) = ceiling_and_seats(&watch(&mut r, &mut e, &red, 1, true));
    let (ceiling_blue, seats_blue) = ceiling_and_seats(&watch(&mut r, &mut e, &blue, 5, true));
    // Red then blue turns the ceiling and the seats from one colour to the
    // other by several codes in 255, clearly and not by a rounding error
    // (beta.6's light moved the room by about one; on lavapipe the ceiling
    // moves by about 28 and the seats by 8 to 14).
    for (name, in_red, in_blue) in [
        ("ceiling", ceiling_red, ceiling_blue),
        ("seats", seats_red, seats_blue),
    ] {
        let red_up = in_red[0] - in_blue[0];
        let blue_up = in_blue[2] - in_red[2];
        assert!(
            red_up > 4.0,
            "{name}: red {in_red:?}, then blue {in_blue:?}"
        );
        assert!(
            blue_up > 4.0,
            "{name}: red {in_red:?}, then blue {in_blue:?}"
        );
    }
    e.destroy(&r);
}

#[test]
fn a_dark_scene_does_not_black_the_room_out() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let black = frame(160, 90, |_, _| [0, 0, 0]);
    let (off, _) = ceiling_and_seats(&watch(&mut r, &mut e, &black, 1, false));
    let (on, _) = ceiling_and_seats(&watch(&mut r, &mut e, &black, 5, true));
    // The light's floor keeps some of it on the ceiling even with nothing on
    // the screen, and never takes any away. The ceiling is 11.5 m up now, to
    // clear the steeper rake, and the floor's lift reaches it more thinly: 0.2
    // of a code a channel on lavapipe (it was about 2 in the old, lower hall),
    // so the sum must clear 0.3, not 1.
    let lifted: f32 = (0..3).map(|c| on[c] - off[c]).sum();
    assert!(lifted > 0.3, "off {off:?}, on {on:?}");
    for c in 0..3 {
        assert!(on[c] >= off[c], "off {off:?}, on {on:?}");
    }
    e.destroy(&r);
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
    let in_red = lower_mean(&watch(&mut r, &mut e, &red, 1, true), BELOW);
    let in_blue = lower_mean(&watch(&mut r, &mut e, &blue, 5, true), BELOW);
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
        rows_from(&off_red, BELOW) == rows_from(&off_blue, BELOW),
        "the room changed with the picture while its light was off"
    );
    // And it is the same room, lit by the picture or not, in the dark: only
    // the light on the floor and treads differs (a code or so in 255 over
    // these rows, see the_room_takes_the_pictures_colour).
    let on_red = watch(&mut r, &mut e, &red, 9, true);
    let (dark, lit) = (lower_mean(&off_red, BELOW), lower_mean(&on_red, BELOW));
    assert!(lit[0] > dark[0] + 0.25, "off {dark:?}, on {lit:?}");
    e.destroy(&r);
}

/// The pixels of `img` that are neither the red square's nor the blue
/// behind it: the ones an edge between them is smoothed into.
fn between_red_and_blue(img: &[u8]) -> usize {
    (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let c = px(img, x, y);
            !is_red(c) && !is_blue(c)
        })
        .count()
}

/// A red square in front of blue, seen with the head tilted so the square's
/// edges are slanted, at `samples` samples a pixel: the two eyes' images and
/// the samples the device gave.
fn slanted_edge(samples: u32) -> Option<([Vec<u8>; 2], u32)> {
    let (mut r, mut e) = setup_samples(SIZE, samples)?;
    let mut data = red_and_blue();
    square(&mut data, -2.0, 1.0, 0);
    square(&mut data, -4.0, 4.0, 1);
    r.set_place_data(Place::Cinema, data);
    let mut p = plain_params();
    p.env = Some(EnvFrame {
        place: Place::Cinema,
        room_to_world: Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0)),
        house: 1.0,
        screen_light: 0.0,
        time: 0.0,
    });
    let eyes = eyes_looking(EYE_HEIGHT, Quat::from_rotation_z(0.3));
    let img = render_eyes(&mut r, &mut e, None, &p, &[], &eyes);
    let had = r.samples();
    e.destroy(&r);
    Some((img, had))
}

#[test]
fn msaa_smooths_a_room_edge() {
    let Some((one, had_one)) = slanted_edge(1) else {
        return;
    };
    assert_eq!(had_one, 1);
    for img in &one {
        assert_eq!(
            between_red_and_blue(img),
            0,
            "at one sample an edge is two values"
        );
    }
    let Some((four, had)) = slanted_edge(4) else {
        return;
    };
    if had < 4 {
        eprintln!("skipping: the device has no 4x multisampling");
        return;
    }
    for (eye, img) in four.iter().enumerate() {
        // The square's slanted edges are some 120 pixels long: nearly every
        // pixel on them takes a value between the two colours.
        let smoothed = between_red_and_blue(img);
        assert!(smoothed > 40, "eye {eye}: {smoothed} pixels are between");
        // The middle of the square and the blue far from it are untouched.
        assert!(is_red(centre(img)), "eye {eye}: centre {:?}", centre(img));
        assert!(
            is_blue(px(img, 3, 3)),
            "eye {eye}: corner {:?}",
            px(img, 3, 3)
        );
    }
}

// ---- Place packs: baked rooms (a mesh and an atlas of baked light) ----

/// `mesh.bin` of the wall the test packs hold: an 8 m square 2 m in front of
/// the viewer.
fn write_wall_mesh(dir: &std::path::Path) {
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
    std::fs::write(dir.join("mesh.bin"), mesh).expect("mesh");
}

/// A pack folder holding one wall, an 8 m square 2 m in front of the viewer
/// at the origin: its house texture `house`, its lights-off texture `dark`,
/// the light of patch 0 white and the other seven patches black. All the
/// textures are 4 texels square, so every pixel of the wall is one colour.
fn wall_pack(name: &str, house: [u8; 3], dark: [u8; 3]) -> std::path::PathBuf {
    use std::io::BufWriter;
    let dir = std::env::temp_dir().join(format!("fp-gpu-pack-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("pack dir");
    let png = |file: &str, rgb: [u8; 3]| {
        let out = BufWriter::new(std::fs::File::create(dir.join(file)).expect("png file"));
        let mut e = png::Encoder::new(out, 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let texels: Vec<u8> = (0..16).flat_map(|_| rgb).collect();
        e.write_header()
            .expect("png header")
            .write_image_data(&texels)
            .expect("png data");
    };
    png("house.png", house);
    png("dark.png", dark);
    for i in 0..8 {
        png(&format!("basis{i}.png"), [255 * u8::from(i == 0); 3]);
    }
    write_wall_mesh(&dir);
    let basis: Vec<String> = (0..8).map(|i| format!("\"basis{i}.png\"")).collect();
    let manifest = format!(
        r#"{{ "format": 1, "id": "cinema-v2", "name": "Cinema", "version": "test",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [9.0, 3.8] }},
  "seats": [ {{ "name": "front", "eye": [0, 2.9, 5.0] }},
             {{ "name": "middle", "eye": [0, 3.2, 9.0] }},
             {{ "name": "back", "eye": [0, 3.6, 13.0] }} ],
  "default_seat": 1,
  "mesh": {{ "file": "mesh.bin", "vertices": 4, "triangles": 2 }},
  "textures": {{ "size": 4, "house": "house.png", "dark": "dark.png", "basis": [{}] }},
  "basis_gain": 1.0, "sky": null }}"#,
        basis.join(", ")
    );
    std::fs::write(dir.join("manifest.json"), manifest).expect("manifest");
    dir
}

/// The eye image's mean colour over the four corners, which the wall fills
/// and the small picture in the middle does not reach.
fn wall_colour(img: &[u8]) -> [u8; 3] {
    let spots = corners(img);
    let sum = |ch: usize| (spots.iter().map(|c| u32::from(c[ch])).sum::<u32>() / 4) as u8;
    [sum(0), sum(1), sum(2)]
}

static CLOCK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// The baked wall with the house lights at `house` and the picture's light
/// at `screen_light`, after five frames of `picture` a second apart (the
/// colours are measured a little behind and ease). The last left eye.
fn baked_wall(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    house: f32,
    screen_light: f32,
    picture: Option<&Arc<VideoFrame>>,
) -> Vec<u8> {
    let mut last = Vec::new();
    for _ in 0..5 {
        // Time only goes forward, so the eased colours follow each picture.
        let time = f64::from(CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let env = EnvFrame {
            place: Place::Cinema,
            room_to_world: Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0)),
            house,
            screen_light,
            time,
        };
        let mut p = plain_params();
        p.background = [0.0, 0.0, 0.0, 1.0];
        p.env = Some(env);
        let [left, _] = render(r, e, picture, &p, &[]);
        last = left;
    }
    last
}

fn load_wall(r: &mut Renderer, name: &str, house: [u8; 3], dark: [u8; 3]) {
    let dir = wall_pack(name, house, dark);
    let pack = fp_render::env::pack::load(&dir).expect("the fixture pack loads");
    r.set_place_data(Place::Cinema, pack.into_place_data());
}

#[test]
fn a_baked_place_shows_the_house_texture_with_the_lights_up_and_the_dark_one_without() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let (house, dark) = ([180, 90, 30], [12, 24, 36]);
    load_wall(&mut r, "textures", house, dark);
    let up = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
    assert!(near(up, house, 4), "lights up: {up:?}, wanted {house:?}");
    let down = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.0, None));
    assert!(
        near(down, dark, 4),
        "lights down: {down:?}, wanted {dark:?}"
    );
    // Between the two the colour is the mix, in light (not in codes).
    let lin = |c: u8| (f32::from(c) / 255.0).powf(2.2);
    let half = wall_colour(&baked_wall(&mut r, &mut e, 0.5, 0.0, None));
    let want = srgb([0, 1, 2].map(|k| 0.5 * (lin(house[k]) + lin(dark[k]))));
    assert!(near(half, want, 6), "half: {half:?}, wanted about {want:?}");
    e.destroy(&r);
}

#[test]
fn a_baked_places_light_follows_the_pictures_colour() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let dark = [10, 10, 10];
    load_wall(&mut r, "light", [180, 90, 30], dark);
    // Only the top left cell of the picture is lit, red or blue; basis 0
    // (the white one) is that cell's light, the others are black.
    let cell = |c: [u8; 3]| {
        frame(
            160,
            90,
            move |u, v| {
                if u < 0.25 && v < 0.5 { c } else { [0, 0, 0] }
            },
        )
    };
    let (red, blue) = (cell([230, 20, 20]), cell([20, 20, 230]));
    let in_red = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 1.0, Some(&red)));
    assert!(
        in_red[0] > 180 && in_red[1] < 90 && in_red[2] < 90,
        "red: {in_red:?}"
    );
    let in_blue = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 1.0, Some(&blue)));
    assert!(
        in_blue[2] > 180 && in_blue[0] < 90 && in_blue[1] < 90,
        "blue: {in_blue:?}"
    );
    // The light off, the picture changes nothing: the dark texture alone.
    let off = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.0, Some(&red)));
    assert!(near(off, dark, 4), "light off: {off:?}");
    // A stronger Screen light slider makes the wall brighter still.
    let weak = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.25, Some(&red)));
    let strong = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 1.0, Some(&red)));
    assert!(weak[0] + 20 < strong[0], "weak {weak:?}, strong {strong:?}");
    // And the house lights add to it (a lit room under a film).
    let both = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 1.0, Some(&red)));
    assert!(
        both[1] > strong[1] + 30,
        "house on: {both:?}, off: {strong:?}"
    );
    e.destroy(&r);
}

#[test]
fn a_baked_place_replaced_by_another_shows_the_new_one() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    load_wall(&mut r, "first", [200, 20, 20], [0; 3]);
    let first = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
    assert!(near(first, [200, 20, 20], 5), "{first:?}");
    // The pack replaced by another, then by a built-in room, then back.
    load_wall(&mut r, "second", [20, 20, 200], [0; 3]);
    let second = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
    assert!(near(second, [20, 20, 200], 5), "{second:?}");
    r.set_place_data(Place::Cinema, Place::Cinema.build_builtin());
    let room = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
    assert!(
        !is_blue(room) && !is_red(room),
        "the built-in room: {room:?}"
    );
    load_wall(&mut r, "third", [20, 200, 20], [0; 3]);
    let third = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
    assert!(third[1] > 150 && third[0] < 80, "{third:?}");
    e.destroy(&r);
}

// ---- Place packs, format 2: KTX2 textures, ASTC compressed ----

/// One ASTC block that is one colour (a void-extent block).
fn void_block(rgba: [u8; 4]) -> [u8; 16] {
    let mut bits: u128 = 0x1FC | (0b11 << 10);
    bits |= ((1u128 << 52) - 1) << 12;
    for (k, c) in rgba.iter().enumerate() {
        bits |= u128::from(u16::from(*c) * 257) << (64 + 16 * k);
    }
    bits.to_le_bytes()
}

/// A KTX2 file: `size` texels square, ASTC 4 x 4, one colour, a full mip
/// chain (levels stored smallest first, as the format says).
fn ktx2_solid(size: u32, srgb: bool, rgba: [u8; 4]) -> Vec<u8> {
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

/// [`wall_pack`] as a format 2 pack: the colours 8 texels square, the basis
/// 4 (they need not match), every texture ASTC with its own mip levels.
fn wall_pack_v2(name: &str, house: [u8; 3], dark: [u8; 3]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fp-gpu-pack2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("pack dir");
    let opaque = |c: [u8; 3]| [c[0], c[1], c[2], 255];
    let mut files = vec![
        ("house.ktx2".to_string(), ktx2_solid(8, true, opaque(house))),
        ("dark.ktx2".to_string(), ktx2_solid(8, true, opaque(dark))),
    ];
    for i in 0..8 {
        let light = opaque([255 * u8::from(i == 0); 3]);
        files.push((format!("basis{i}.ktx2"), ktx2_solid(4, false, light)));
    }
    let bytes: usize = files.iter().map(|(_, b)| b.len()).sum();
    for (file, data) in &files {
        std::fs::write(dir.join(file), data).expect("texture");
    }
    write_wall_mesh(&dir);
    let basis: Vec<String> = (0..8).map(|i| format!("\"basis{i}.ktx2\"")).collect();
    let manifest = format!(
        r#"{{ "format": 2, "id": "cinema-v2", "name": "Cinema", "version": "test",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [9.0, 3.8] }},
  "seats": [ {{ "name": "middle", "eye": [0, 3.2, 9.0] }} ],
  "mesh": {{ "file": "mesh.bin", "vertices": 4, "triangles": 2 }},
  "textures": {{ "size": 8, "basis_size": 4, "bytes": {bytes},
                 "house": "house.ktx2", "dark": "dark.ktx2", "basis": [{}] }},
  "basis_gain": 1.0 }}"#,
        basis.join(", ")
    );
    std::fs::write(dir.join("manifest.json"), manifest).expect("manifest");
    dir
}

/// A pack kept as ASTC blocks that the device then refuses (lavapipe, which
/// has no ASTC, refuses them all) is not the end of places: it goes up
/// decoded and draws the same.
#[test]
fn an_astc_pack_the_device_refuses_goes_up_decoded_instead() {
    use fp_render::env::pack;
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    if pack::astc_on_gpu() {
        // A device that takes ASTC has nothing to refuse.
        e.destroy(&r);
        return;
    }
    let (house, dark) = ([180, 90, 30], [12, 24, 36]);
    let dir = wall_pack_v2("refused", house, dark);
    let kept = pack::load_with(&dir, true).expect("loads as blocks");
    assert!(kept.baked.is_astc());
    r.set_place_data(Place::Cinema, kept.into_place_data());
    let up = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
    assert!(near(up, house, 4), "lights up: {up:?}");
    let down = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.0, None));
    assert!(near(down, dark, 4), "lights down: {down:?}");
    e.destroy(&r);
}

#[test]
fn a_format_2_baked_place_draws_whichever_way_its_textures_reach_the_gpu() {
    use fp_render::env::pack::{self, TexturePath};
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let (house, dark) = ([180, 90, 30], [12, 24, 36]);
    // Decoded on the CPU always (every device takes that); as blocks too
    // where the device samples ASTC.
    let mut ways = vec![false];
    if pack::astc_on_gpu() {
        ways.push(true);
    }
    eprintln!("ASTC on the GPU: {}", pack::astc_on_gpu());
    for keep in ways {
        let dir = wall_pack_v2(&format!("draw-{keep}"), house, dark);
        let loaded = pack::load_with(&dir, keep).expect("the format 2 pack loads");
        assert_eq!(loaded.path == TexturePath::AstcOnGpu, keep);
        r.set_place_data(Place::Cinema, loaded.into_place_data());
        let up = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
        assert!(near(up, house, 4), "keep {keep}, lights up: {up:?}");
        let down = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.0, None));
        assert!(near(down, dark, 4), "keep {keep}, lights down: {down:?}");
        // The basis (a different size from the colours) lights the wall
        // with the picture's colour in its top left cell.
        let red = frame(160, 90, |u, v| {
            if u < 0.25 && v < 0.5 {
                [230, 20, 20]
            } else {
                [0, 0, 0]
            }
        });
        let lit = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 1.0, Some(&red)));
        assert!(
            lit[0] > 150 && lit[1] < 90 && lit[2] < 90,
            "keep {keep}, red light: {lit:?}"
        );
    }
    // And the way the app loads it, whatever that is on this device.
    let dir = wall_pack_v2("natural", house, dark);
    let natural = pack::load(&dir).expect("loads");
    assert_eq!(natural.path == TexturePath::AstcOnGpu, pack::astc_on_gpu());
    r.set_place_data(Place::Cinema, natural.into_place_data());
    let up = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
    assert!(near(up, house, 4), "natural, lights up: {up:?}");
    e.destroy(&r);
}

// ---- The place pass's live knobs: samples, the picture's light, the order ----

#[test]
fn the_samples_can_change_on_the_fly_and_the_room_is_still_drawn() {
    let Some((mut r, mut e)) = setup_samples(SIZE, 4) else {
        return;
    };
    let (house, dark) = ([180, 90, 30], [12, 24, 36]);
    load_wall(&mut r, "samples", house, dark);
    let mut seen = vec![r.samples()];
    // Down to one, back, and to two: the wall is the same colour at each
    // (what the device does not have is rounded down).
    for wanted in [1, 4, 2, 4] {
        let got = r.set_samples(wanted).expect("changes");
        assert_eq!(got, r.samples());
        assert!(got <= wanted.max(1), "{wanted} gave {got}");
        seen.push(got);
        let up = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
        assert!(near(up, house, 4), "{got} samples: {up:?}");
        let down = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.0, None));
        assert!(near(down, dark, 4), "{got} samples, dark: {down:?}");
    }
    // Asking for what it already does is no change.
    let now = r.samples();
    assert_eq!(r.set_samples(now).expect("same"), now);
    eprintln!("sample counts visited: {seen:?}");
    e.destroy(&r);
}

#[test]
fn the_depth_buffer_can_change_size_on_the_fly_and_the_room_is_still_drawn() {
    let Some((mut r, mut e)) = setup_samples(SIZE, 4) else {
        return;
    };
    let (house, dark) = ([180, 90, 30], [12, 24, 36]);
    load_wall(&mut r, "depth", house, dark);
    let first = r.depth_format();
    let mut seen = vec![first];
    // Each size the knob knows, the default, and back: the wall is the same
    // colour in all of them (a size the device cannot attach gives the
    // default one).
    for bits in [Some(16), Some(24), None, Some(32), Some(16)] {
        let got = r.set_depth_bits(bits).expect("changes");
        assert_eq!(got, r.depth_format());
        seen.push(got);
        let up = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
        assert!(near(up, house, 4), "{bits:?} ({got:?}): {up:?}");
        let down = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.0, None));
        assert!(near(down, dark, 4), "{bits:?} ({got:?}), dark: {down:?}");
    }
    // Asking for what it already has is no change.
    let now = r.depth_format();
    assert_eq!(r.set_depth_bits(Some(16)).expect("same"), now);
    // The samples and the depth change together too.
    r.set_samples(1).expect("one sample");
    assert!(near(
        wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None)),
        house,
        4
    ));
    eprintln!("depth formats visited: {seen:?}");
    e.destroy(&r);
}

#[test]
fn nearer_geometry_hides_farther_with_a_16_bit_depth_buffer() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    r.set_depth_bits(Some(16)).expect("16 bit");
    // The same scene as `nearer_geometry_hides_farther`.
    let mut data = red_and_blue();
    square(&mut data, -2.0, 1.0, 0);
    square(&mut data, -4.0, 4.0, 1);
    r.set_place_data(Place::Cinema, data);
    let anchor = Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0));
    let env = EnvFrame {
        place: Place::Cinema,
        room_to_world: anchor,
        house: 1.0,
        screen_light: 0.0,
        time: 0.0,
    };
    let mut p = plain_params();
    p.background = [0.0, 0.0, 0.0, 1.0];
    p.env = Some(env);
    let [l, _] = render(&mut r, &mut e, None, &p, &[]);
    assert!(is_red(centre(&l)), "centre {:?}", centre(&l));
    let beside = px(&l, SIZE / 2 + 48, SIZE / 2);
    assert!(is_blue(beside), "beside the red {beside:?}");
    e.destroy(&r);
}

#[test]
fn the_colours_read_trilinear_look_the_same_on_a_flat_wall() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let (house, dark) = ([180, 90, 30], [12, 24, 36]);
    load_wall(&mut r, "aniso", house, dark);
    for on in [true, false, true] {
        r.set_place_aniso(on);
        let up = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
        assert!(near(up, house, 4), "aniso {on}: {up:?}");
        let down = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.0, None));
        assert!(near(down, dark, 4), "aniso {on}, dark: {down:?}");
    }
    e.destroy(&r);
}

#[test]
fn a_house_level_between_the_ends_still_mixes_both_colours() {
    // The shader reads one texture at the ends and both between them.
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let (house, dark) = ([180, 90, 30], [12, 24, 36]);
    load_wall(&mut r, "mix", house, dark);
    let mid = wall_colour(&baked_wall(&mut r, &mut e, 0.5, 0.0, None));
    assert!(dark[0] + 20 < mid[0] && mid[0] + 20 < house[0], "{mid:?}");
    assert!(dark[1] + 10 < mid[1] && mid[1] + 10 < house[1], "{mid:?}");
    let up = wall_colour(&baked_wall(&mut r, &mut e, 1.0, 0.0, None));
    let down = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 0.0, None));
    assert!(near(up, house, 4) && near(down, dark, 4), "{up:?} {down:?}");
    e.destroy(&r);
}

/// The left eye's fragment shader invocations of two overlapping squares
/// drawn in the order `far_first` says, or `None` where the device cannot
/// count them.
fn invocations_of_two_squares(far_first: bool) -> Option<u64> {
    let (mut r, mut e) = setup_counting(SIZE)?;
    let mut data = red_and_blue();
    if far_first {
        square(&mut data, -4.0, 4.0, 1);
        square(&mut data, -2.0, 1.0, 0);
    } else {
        square(&mut data, -2.0, 1.0, 0);
        square(&mut data, -4.0, 4.0, 1);
    }
    r.set_place_data(Place::Cinema, data);
    let mut p = plain_params();
    p.background = [0.0, 0.0, 0.0, 1.0];
    p.env = Some(place_frame(Place::Cinema, SeatName::Middle, 1.0));
    p.env.as_mut().expect("set above").room_to_world =
        Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0));
    render(&mut r, &mut e, None, &p, &[]);
    let counted = r.place_fragment_counts()[0];
    e.destroy(&r);
    counted
}

#[test]
fn the_fragment_counter_sees_what_the_draw_order_saves() {
    let (Some(far_first), Some(near_first)) = (
        invocations_of_two_squares(true),
        invocations_of_two_squares(false),
    ) else {
        return;
    };
    let pixels = u64::from(SIZE * SIZE);
    eprintln!("two squares: far first {far_first}, near first {near_first}, {pixels} pixels");
    // The far square fills the view; the near one is a ninth of it. Drawn far
    // first, its covered part is shaded twice; drawn near first the depth test
    // keeps the far square's fragments behind it from being shaded at all.
    assert!(near_first >= pixels, "{near_first} < {pixels}");
    assert!(
        far_first > near_first + pixels / 20,
        "far first {far_first}, near first {near_first}"
    );
}

#[test]
fn the_built_in_room_survives_a_sample_change_too() {
    let Some((mut r, mut e)) = setup_samples(SIZE, 4) else {
        return;
    };
    let p = cinema_params(
        ViewSettings::default(),
        place_frame(Place::Cinema, SeatName::Middle, 1.0),
    );
    let before = render(&mut r, &mut e, None, &p, &[]);
    r.set_samples(1).expect("one");
    let one = render(&mut r, &mut e, None, &p, &[]);
    r.set_samples(4).expect("four");
    let after = render(&mut r, &mut e, None, &p, &[]);
    let want = px(&before[0], SIZE / 2, SIZE / 2);
    for img in [&one, &after] {
        assert!(near(px(&img[0], SIZE / 2, SIZE / 2), want, 12), "centre");
    }
    e.destroy(&r);
}

#[test]
fn a_baked_place_draws_the_same_sorted_as_in_its_packs_order() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    load_wall(&mut r, "sorted", [180, 90, 30], [12, 24, 36]);
    // Pack order first: the sorter has not answered on the first frame.
    r.set_place_knobs(true, false);
    let in_packs_order = baked_wall(&mut r, &mut e, 1.0, 0.0, None);
    // Draw until the worker has answered (not for a fixed time: a slow
    // machine would compare two unsorted draws), then draw with its order.
    r.set_place_knobs(true, true);
    let give_up = Instant::now() + Duration::from_secs(20);
    while !r.place_order_ready() && Instant::now() < give_up {
        let _ = baked_wall(&mut r, &mut e, 1.0, 0.0, None);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(r.place_order_ready(), "the sorter never answered");
    let sorted = baked_wall(&mut r, &mut e, 1.0, 0.0, None);
    assert_eq!(in_packs_order, sorted, "the order changes no pixel");
    e.destroy(&r);
}

#[test]
fn the_pictures_light_can_be_left_out_of_a_baked_place() {
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let dark = [10, 10, 10];
    load_wall(&mut r, "basis-off", [180, 90, 30], dark);
    let red = frame(160, 90, |u, v| {
        if u < 0.25 && v < 0.5 {
            [230, 20, 20]
        } else {
            [0, 0, 0]
        }
    });
    let lit = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 1.0, Some(&red)));
    assert!(lit[0] > 150, "on: {lit:?}");
    r.set_place_knobs(false, true);
    let off = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 1.0, Some(&red)));
    assert!(near(off, dark, 4), "off: {off:?}");
    r.set_place_knobs(true, true);
    let again = wall_colour(&baked_wall(&mut r, &mut e, 0.0, 1.0, Some(&red)));
    assert!(near(again, lit, 4), "on again: {again:?}, first {lit:?}");
    e.destroy(&r);
}
