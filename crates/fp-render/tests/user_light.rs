//! The Screen light slider on a place pack the person put in their own
//! `environments` folder ("Yours"): at 0 the film gives the room nothing (no
//! picture light, no highlight), above 0 both come, and the slider's top
//! gives more than its start. The picture's light (the basis textures) and
//! the highlight (the material map) are each checked alone, so one cannot
//! cover for the other.
//!
//! The pack is one floor, 14 m deep in front of the screen, with black house
//! and dark textures: anything lit is the film's. The user registry can only
//! be filled once per process, so every check runs one after the other in one
//! test.

use fp_core::view::ViewSettings;
use fp_media::VideoFrame;
use fp_render::capture::OffscreenEyes;
use fp_render::env::{EnvFrame, Place, SeatName, user};
use fp_render::math::{Fov, projection, view};
use fp_render::{EyeView, Renderer};
use glam::{Quat, Vec3};
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

/// What a test pack is made of.
struct Spec {
    /// The folder's name (and so the place's id, `user-<name>`).
    name: &'static str,
    /// The manifest's `basis_gain`.
    gain: f32,
    /// The basis textures' code (square-root coded light), 0 for none.
    basis: u8,
    /// The house and dark textures' sRGB code.
    colours: u8,
    /// The material texture: roughness, specular mask, accent.
    material: [u8; 3],
}

/// Writes the pack in `<root>/<name>`.
fn write_pack(root: &Path, spec: &Spec) {
    let dir = root.join(spec.name);
    std::fs::create_dir_all(&dir).expect("pack dir");
    std::fs::write(dir.join("mesh.bin"), floor_mesh()).expect("mesh");
    let c = spec.colours;
    let mut files = vec![
        (
            "house.ktx2".to_string(),
            ktx2_solid(8, true, [c, c, c, 255]),
        ),
        ("dark.ktx2".to_string(), ktx2_solid(8, true, [c, c, c, 255])),
    ];
    let b = spec.basis;
    for i in 0..8 {
        files.push((
            format!("basis{i}.ktx2"),
            ktx2_solid(4, false, [b, b, b, 255]),
        ));
    }
    let [rough, mask, accent] = spec.material;
    files.push((
        "material.ktx2".into(),
        ktx2_solid(4, false, [rough, mask, accent, 255]),
    ));
    let bytes: usize = files.iter().map(|(_, b)| b.len()).sum();
    for (file, data) in &files {
        std::fs::write(dir.join(file), data).expect("texture");
    }
    let basis: Vec<String> = (0..8).map(|i| format!("\"basis{i}.ktx2\"")).collect();
    let manifest = format!(
        r#"{{ "format": 3, "id": "{}", "name": "{}", "version": "test",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [9.0, 3.8] }},
  "seats": [ {{ "name": "middle", "eye": [{}, {}, {}] }} ],
  "default_seat": 0,
  "seat_sets": {{ "middle": {{
    "mesh": {{ "file": "mesh.bin", "vertices": 4, "triangles": 2 }},
    "textures": {{ "size": 8, "basis_size": 4, "bytes": {bytes},
                   "house": "house.ktx2", "dark": "dark.ktx2",
                   "basis": [{}], "material": "material.ktx2" }} }} }},
  "basis_gain": {} }}"#,
        spec.name,
        spec.name,
        SEAT_EYE[0],
        SEAT_EYE[1],
        SEAT_EYE[2],
        basis.join(", "),
        spec.gain
    );
    std::fs::write(dir.join("manifest.json"), manifest).expect("manifest");
}

fn eyes() -> [EyeView; 2] {
    let look = Quat::from_rotation_x(-1.1);
    let ev = |dx: f32| EyeView {
        view: view(Vec3::new(dx, common::EYE_HEIGHT, 0.0), look),
        proj: projection(Fov::symmetric(90.0), 0.05, 100.0),
    };
    [ev(-0.032), ev(0.032)]
}

static CLOCK: AtomicU32 = AtomicU32::new(1);

/// The mean brightness (colour codes) of the left eye's picture of the room,
/// after the picture's light has settled, with the Screen light at
/// `screen_light` and the place's own lights (Minimal lighting) at `accent`.
fn shoot(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    place: Place,
    picture: &Arc<VideoFrame>,
    screen_light: f32,
    accent: f32,
) -> f32 {
    let layout = place.layout();
    let screen = layout.screen.expect("the pack has a screen");
    r.set_accent_lights(accent);
    let mut img = Vec::new();
    for _ in 0..6 {
        let t = CLOCK.fetch_add(1, Ordering::Relaxed);
        let env = EnvFrame {
            screen_light,
            time: f64::from(t),
            ..place_frame(place, SeatName::Middle, 0.0)
        };
        let mut p = common::params(
            fp_core::format::Projection::Flat,
            fp_core::format::StereoLayout::Mono,
            ViewSettings::default(),
        );
        p.screen_pose = env.room_to_world * glam::Mat4::from_translation(screen.centre);
        p.screen_fit = Some(screen.max);
        p.background = [0.0, 0.0, 0.0, 1.0];
        p.env = Some(env);
        r.begin_frame().unwrap();
        r.set_video(Some(picture)).unwrap();
        r.draw(&e.targets(), &eyes(), &p, &[]).unwrap();
        r.end_frame().unwrap();
        img = e.read(r, 0).unwrap();
    }
    let mut sum = 0.0f64;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let i = ((y * SIZE + x) * 4) as usize;
            sum += f64::from(img[i]) + f64::from(img[i + 1]) + f64::from(img[i + 2]);
        }
    }
    (sum / f64::from(SIZE * SIZE * 3)) as f32
}

#[test]
fn the_places_a_person_installed_themselves_light_shine_and_dim_like_the_built_in_ones() {
    let root = std::env::temp_dir().join(format!("fp-gpu-userlight-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let specs = [
        // A dim bake like the Multiplex's (a gain of 1/128: its basis
        // textures are stored 128 times too bright), black colours.
        Spec {
            name: "dim",
            gain: 1.0 / 128.0,
            basis: 255,
            colours: 0,
            material: [77, 255, 0],
        },
        // Glossy everywhere, no picture light, and the same with every texel
        // an accent light.
        Spec {
            name: "mirror",
            gain: 1.0,
            basis: 0,
            colours: 0,
            material: [77, 255, 0],
        },
        Spec {
            name: "rails",
            gain: 1.0,
            basis: 0,
            colours: 0,
            material: [77, 255, 255],
        },
        // Lit grey as baked, with accent lights on every texel.
        Spec {
            name: "lamps",
            gain: 1.0,
            basis: 0,
            colours: 150,
            material: [217, 64, 255],
        },
    ];
    for spec in &specs {
        write_pack(&root, spec);
    }
    // SAFETY: set before anything reads it, and the only test in this
    // process.
    unsafe { std::env::set_var(user::DIR_ENV, &root) };
    let place = |name: &str| {
        let id = format!("{}{name}", user::ID_PREFIX);
        Place::from_id(&id).unwrap_or_else(|| panic!("{id} was not found among Yours"))
    };
    assert_eq!(Place::yours().len(), specs.len());
    // A dim film: a grey about a quarter as bright as white (linear).
    let film = frame(160, 90, |_, _| [137, 137, 137]);
    let white = frame(160, 90, |_, _| [255, 255, 255]);
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    r.set_place_knobs(true, false);

    // The picture's light on a dim bake follows the slider, and is there to
    // be seen at its start: the gain of 1/128 alone would leave it a third of
    // that.
    r.set_place_shine(false);
    let dim = place("dim");
    let zero = shoot(&mut r, &mut e, dim, &film, 0.0, 1.0);
    let one = shoot(&mut r, &mut e, dim, &film, 1.0, 1.0);
    let top = shoot(&mut r, &mut e, dim, &film, 4.0, 1.0);
    eprintln!("dim bake, picture light: 0 -> {zero:.2}, 1 -> {one:.2}, 4 -> {top:.2}");
    assert!(zero < 0.5, "Screen light 0 still lights the place: {zero}");
    assert!(
        one > 50.0,
        "the picture's light is invisible at the start: {one}"
    );
    assert!(top > one * 1.5, "the slider's top is no brighter: {top}");

    // The highlight: none at 0 whatever the film, the film's colour above it,
    // more at the top; and none on a texel the pack marks as a lamp.
    r.set_place_shine(true);
    r.set_place_knobs(false, false);
    let mirror = place("mirror");
    let zero = shoot(&mut r, &mut e, mirror, &white, 0.0, 1.0);
    let one = shoot(&mut r, &mut e, mirror, &white, 1.0, 1.0);
    let top = shoot(&mut r, &mut e, mirror, &white, 4.0, 1.0);
    eprintln!("mirror, highlight: 0 -> {zero:.2}, 1 -> {one:.2}, 4 -> {top:.2}");
    assert!(zero < 0.5, "a highlight with the Screen light at 0: {zero}");
    assert!(one > 3.0, "no highlight on a glossy pack of yours: {one}");
    assert!(top > one * 1.25, "the highlight ignores the slider: {top}");
    let rails = shoot(&mut r, &mut e, place("rails"), &white, 4.0, 1.0);
    eprintln!("rails, highlight at 4: {rails:.2}");
    assert!(rails < 0.5, "the film is reflected in a lamp: {rails}");

    // Minimal lighting: the lamps go dark with the house lights down, and
    // come back.
    let lamps = place("lamps");
    let lit = shoot(&mut r, &mut e, lamps, &film, 1.0, 1.0);
    let dark = shoot(&mut r, &mut e, lamps, &film, 1.0, 0.0);
    let back = shoot(&mut r, &mut e, lamps, &film, 1.0, 1.0);
    eprintln!("lamps: lit {lit:.2}, minimal {dark:.2}, back {back:.2}");
    assert!(lit > 60.0, "the lamps are lit: {lit}");
    assert!(
        dark < 3.0,
        "Minimal lighting left the lamps of a pack of yours on: {dark}"
    );
    assert!((back - lit).abs() < 1.0, "{back} against {lit}");
    e.destroy(&r);
}
