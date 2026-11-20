//! The projection booth's glass: a pack that declares `booth_glass` has the
//! playing film drawn on that rectangle, mirrored left to right and faint;
//! one that does not is unchanged; with no video there is nothing on it.
//!
//! The packs are two user places (black rooms, a screen of 4 m by 2.25 m and
//! the glass to the right of it, so one view holds both). The user registry
//! can only be filled once per process, so everything runs in one test.

use fp_core::format::{Projection, StereoLayout};
use fp_core::view::ViewSettings;
use fp_media::VideoFrame;
use fp_render::capture::OffscreenEyes;
use fp_render::env::{EnvFrame, Place, SeatName, user};
use fp_render::math::{Fov, projection, view};
use fp_render::{EyeView, Renderer};
use glam::Vec3;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

mod common;
use common::{SIZE, frame, ktx2_solid, place_frame, setup};

const SEAT_EYE: [f32; 3] = [0.0, 2.9, 5.0];

fn floor_mesh() -> Vec<u8> {
    let corners = [
        [-8.0f32, 0.0, 0.0, 0.0, 0.0],
        [8.0, 0.0, 0.0, 1.0, 0.0],
        [8.0, 0.0, 14.0, 1.0, 1.0],
        [-8.0, 0.0, 14.0, 0.0, 1.0],
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

/// Writes the pack `name` in `root`, with `extra` added to its manifest's top
/// level (the glass, or nothing).
fn write_pack(root: &Path, name: &str, extra: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("pack dir");
    std::fs::write(dir.join("mesh.bin"), floor_mesh()).expect("mesh");
    let black = [0, 0, 0, 255];
    let mut files = vec![
        ("house.ktx2".to_string(), ktx2_solid(8, true, black)),
        ("dark.ktx2".to_string(), ktx2_solid(8, true, black)),
    ];
    for i in 0..8 {
        files.push((format!("basis{i}.ktx2"), ktx2_solid(4, false, black)));
    }
    let bytes: usize = files.iter().map(|(_, b)| b.len()).sum();
    for (file, data) in &files {
        std::fs::write(dir.join(file), data).expect("texture");
    }
    let basis: Vec<String> = (0..8).map(|i| format!("\"basis{i}.ktx2\"")).collect();
    let manifest = format!(
        r#"{{ "format": 3, "id": "{name}", "name": "{name}", "version": "test",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [4.0, 2.25] }},
  "seats": [ {{ "name": "middle", "eye": [{}, {}, {}] }} ],
  "default_seat": 0,
  "seat_sets": {{ "middle": {{
    "mesh": {{ "file": "mesh.bin", "vertices": 4, "triangles": 2 }},
    "textures": {{ "size": 8, "basis_size": 4, "bytes": {bytes},
                   "house": "house.ktx2", "dark": "dark.ktx2",
                   "basis": [{}] }} }} }},
  "basis_gain": 1.0{extra} }}"#,
        SEAT_EYE[0],
        SEAT_EYE[1],
        SEAT_EYE[2],
        basis.join(", ")
    );
    std::fs::write(dir.join("manifest.json"), manifest).expect("manifest");
}

static CLOCK: AtomicU32 = AtomicU32::new(1);

/// The left eye's picture of the room from the seat, looking at the screen.
fn shoot(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    place: Place,
    picture: Option<&Arc<VideoFrame>>,
    house: f32,
) -> Vec<u8> {
    let layout = place.layout();
    let screen = layout.screen.expect("the pack has a screen");
    let eyes = {
        let ev = |dx: f32| EyeView {
            view: view(Vec3::new(dx, common::EYE_HEIGHT, 0.0), glam::Quat::IDENTITY),
            proj: projection(Fov::symmetric(90.0), 0.05, 100.0),
        };
        [ev(-0.032), ev(0.032)]
    };
    let mut img = Vec::new();
    for _ in 0..3 {
        let t = CLOCK.fetch_add(1, Ordering::Relaxed);
        let env = EnvFrame {
            screen_light: 0.0,
            time: f64::from(t),
            ..place_frame(place, SeatName::Middle, house)
        };
        let mut p = common::params(
            Projection::Flat,
            StereoLayout::Mono,
            ViewSettings::default(),
        );
        p.screen_pose = env.room_to_world * glam::Mat4::from_translation(screen.centre);
        p.screen_fit = Some(screen.max);
        p.background = [0.0, 0.0, 0.0, 1.0];
        p.env = Some(env);
        r.begin_frame().unwrap();
        r.set_video(picture).unwrap();
        r.draw(&e.targets(), &eyes, &p, &[]).unwrap();
        r.end_frame().unwrap();
        img = e.read(r, 0).unwrap();
    }
    img
}

/// (count, mean x) of the pixels `pick` chooses, over the picture's right
/// part (where the glass is, beside the screen).
fn find(img: &[u8], pick: impl Fn([u8; 3]) -> bool) -> (u32, f32) {
    let (mut n, mut sx) = (0u32, 0.0f32);
    for y in 0..SIZE {
        for x in SIZE * 74 / 100..SIZE {
            let i = ((y * SIZE + x) * 4) as usize;
            if pick([img[i], img[i + 1], img[i + 2]]) {
                n += 1;
                sx += x as f32;
            }
        }
    }
    (n, if n == 0 { f32::NAN } else { sx / n as f32 })
}

#[test]
fn the_film_is_mirrored_on_the_booth_glass_of_a_pack_that_has_one() {
    let root = std::env::temp_dir().join(format!("fp-gpu-booth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // The glass: 1.6 m wide and 1 m high, in the screen's plane, to its right.
    let glass = |opacity: f32| {
        format!(
            r#", "booth_glass": {{ "corners": [[2.6, 2.4, 0.0], [4.2, 2.4, 0.0],
               [4.2, 3.4, 0.0], [2.6, 3.4, 0.0]], "opacity": {opacity} }}"#
        )
    };
    write_pack(&root, "booth", &glass(0.1));
    write_pack(&root, "plain", "");
    // SAFETY: set before anything reads it; the only test in this process.
    unsafe { std::env::set_var(user::DIR_ENV, &root) };
    let booth = Place::from_id("user-booth").expect("the booth pack");
    let plain = Place::from_id("user-plain").expect("the plain pack");
    assert!(booth.layout().booth_glass.is_some());
    assert!(plain.layout().booth_glass.is_none());

    // A film red on its left half and blue on its right.
    let film = frame(
        160,
        90,
        |u, _| if u < 0.5 { [255, 0, 0] } else { [0, 0, 255] },
    );
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    r.set_place_knobs(true, false);

    let img = shoot(&mut r, &mut e, booth, Some(&film), 0.0);
    let red = |c: [u8; 3]| c[0] > 12 && c[2] < 6;
    let blue = |c: [u8; 3]| c[2] > 12 && c[0] < 6;
    let (red_n, red_x) = find(&img, red);
    let (blue_n, blue_x) = find(&img, blue);
    eprintln!("glass: red {red_n} px at x {red_x:.1}, blue {blue_n} px at x {blue_x:.1}");
    assert!(
        red_n > 40 && blue_n > 40,
        "the glass shows no film: {red_n} {blue_n}"
    );
    // Mirrored: the film's red left half is on the glass's right.
    assert!(
        blue_x + 3.0 < red_x,
        "the film is not mirrored: blue at {blue_x}, red at {red_x}"
    );
    // Faint: at an opacity of 0.1 nothing on it is near the film's own colour.
    let peak = (0..SIZE)
        .flat_map(|y| (SIZE * 74 / 100..SIZE).map(move |x| (x, y)))
        .map(|(x, y)| {
            let i = ((y * SIZE + x) * 4) as usize;
            img[i].max(img[i + 2])
        })
        .max()
        .unwrap_or(0);
    eprintln!("glass peak {peak}");
    assert!((40..120).contains(&peak), "the glass is not faint: {peak}");

    // The same room with no video: nothing on the glass.
    let none = shoot(&mut r, &mut e, booth, None, 0.0);
    assert_eq!(
        find(&none, red).0 + find(&none, blue).0,
        0,
        "glass with no film"
    );
    // A pack without the field is as it was.
    let other = shoot(&mut r, &mut e, plain, Some(&film), 0.0);
    let (a, b) = (find(&other, red).0, find(&other, blue).0);
    assert_eq!(a + b, 0, "a pack with no glass shows one: {a} {b}");
    // With the house lights up (paused) the glass is gone.
    let up = shoot(&mut r, &mut e, booth, Some(&film), 1.0);
    assert_eq!(
        find(&up, red).0 + find(&up, blue).0,
        0,
        "the glass shows with the house lights up"
    );
    e.destroy(&r);
}
