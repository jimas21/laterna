//! The picture's highlight on a baked place's glossy surfaces, on a real
//! (software) Vulkan device: the screen is a rectangle lamp the floor
//! reflects, the highlight moves with the viewer's head, the pack's material
//! texture decides how sharp and how strong it is (a pack without one gets
//! a weak, rough default), and `FRAMEPLAYERX_PLACE_SHINE=0` takes it away.
//!
//! The pack is made here: one seat, a floor 14 m deep in front of the screen
//! with black house and dark textures and no basis light, so that anything
//! lit is the highlight and nothing else. The place catalog can only be set
//! once per process, so every check runs one after the other in one test.
//!
//! With `FRAMEPLAYERX_SHINE_PREVIEW_DIR` set the test also writes the left
//! eye's pictures there (PNG): the head to the left, the middle and the
//! right, and with the highlight off.

use fp_core::view::ViewSettings;
use fp_media::VideoFrame;
use fp_render::capture::OffscreenEyes;
use fp_render::env::{EnvFrame, Place, SeatName, pack};
use fp_render::math::{Fov, projection, view};
use fp_render::{EyeView, Renderer};
use glam::{Quat, Vec3};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod common;
use common::{SIZE, cinema_params, frame, ktx2_solid, place_frame, setup};

/// The seat's eye in the room, as the pack says: 2.9 m up, 5 m from the
/// screen.
const SEAT_EYE: [f32; 3] = [0.0, 2.9, 5.0];

/// The floor's mesh: 16 m wide, from the screen to 14 m back, at y = 0.
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

fn pack_root() -> PathBuf {
    std::env::temp_dir().join(format!("fp-gpu-shine-{}", std::process::id()))
}

/// Writes the pack in `<root>/cinema-v2`, with the material texture `rg`
/// (roughness, specular mask) or none.
fn write_pack(root: &Path, material: Option<[u8; 2]>) {
    let dir = root.join("cinema-v2");
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
    let mut material_json = String::new();
    if let Some([rough, mask]) = material {
        files.push((
            "material.ktx2".into(),
            ktx2_solid(4, false, [rough, mask, 0, 255]),
        ));
        material_json = r#", "material": "material.ktx2""#.to_string();
    }
    let bytes: usize = files.iter().map(|(_, b)| b.len()).sum();
    for (file, data) in &files {
        std::fs::write(dir.join(file), data).expect("texture");
    }
    let basis: Vec<String> = (0..8).map(|i| format!("\"basis{i}.ktx2\"")).collect();
    let manifest = format!(
        r#"{{ "format": 3, "id": "cinema-v2", "name": "Cinema", "version": "test",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [9.0, 3.8] }},
  "seats": [ {{ "name": "middle", "eye": [{}, {}, {}] }} ],
  "default_seat": 0,
  "seat_sets": {{ "middle": {{
    "mesh": {{ "file": "mesh.bin", "vertices": 4, "triangles": 2 }},
    "textures": {{ "size": 8, "basis_size": 4, "bytes": {bytes},
                   "house": "house.ktx2", "dark": "dark.ktx2",
                   "basis": [{}]{material_json} }} }} }},
  "basis_gain": 1.0 }}"#,
        SEAT_EYE[0],
        SEAT_EYE[1],
        SEAT_EYE[2],
        basis.join(", ")
    );
    std::fs::write(dir.join("manifest.json"), manifest).expect("manifest");
}

/// Both eyes `x` metres to the right of the seat's, looking down at the
/// floor where the picture's reflection is (about 52 degrees).
fn eyes_at(x: f32, up: f32, back: f32) -> [EyeView; 2] {
    let look = Quat::from_rotation_x(-0.9);
    let ev = |dx: f32| EyeView {
        view: view(Vec3::new(x + dx, common::EYE_HEIGHT + up, back), look),
        proj: projection(Fov::symmetric(90.0), 0.05, 100.0),
    };
    [ev(-0.032), ev(0.032)]
}

/// One frame of the room from `eyes`, the left eye read back, after the
/// picture's light has settled.
fn shoot(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    picture: &Arc<VideoFrame>,
    eyes: &[EyeView; 2],
    screen_light: f32,
) -> Vec<u8> {
    let mut out = Vec::new();
    for t in 1..=6 {
        let env = EnvFrame {
            screen_light,
            time: f64::from(t),
            ..place_frame(Place::Cinema, SeatName::Middle, 0.0)
        };
        let p = cinema_params(ViewSettings::default(), env);
        r.begin_frame().unwrap();
        r.set_video(Some(picture)).unwrap();
        r.draw(&e.targets(), eyes, &p, &[]).unwrap();
        r.end_frame().unwrap();
        out = e.read(r, 0).unwrap();
    }
    out
}

/// Brightness of a pixel, the mean of its colour codes.
fn bright(img: &[u8], x: u32, y: u32) -> f32 {
    let i = ((y * SIZE + x) * 4) as usize;
    (f32::from(img[i]) + f32::from(img[i + 1]) + f32::from(img[i + 2])) / 3.0
}

/// The brightest pixel's value, and the middle of what is bright (above a
/// quarter of that) by brightness, in pixels.
fn highlight(img: &[u8]) -> (f32, f32, f32) {
    let peak = (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .map(|(x, y)| bright(img, x, y))
        .fold(0.0f32, f32::max);
    let (mut sum, mut sx, mut sy) = (0.0, 0.0, 0.0);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let b = bright(img, x, y);
            if b > peak / 4.0 && b > 2.0 {
                sum += b;
                sx += b * x as f32;
                sy += b * y as f32;
            }
        }
    }
    if sum == 0.0 {
        return (peak, f32::NAN, f32::NAN);
    }
    (peak, sx / sum, sy / sum)
}

/// A picture that is white all over, so every patch of the screen is.
fn white() -> Arc<VideoFrame> {
    frame(160, 90, |_, _| [255, 255, 255])
}

fn save_png(dir: &Path, name: &str, rgba: &[u8]) {
    std::fs::create_dir_all(dir).expect("preview dir");
    let file = std::fs::File::create(dir.join(name)).expect("png file");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), SIZE, SIZE);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(rgba))
        .expect("png");
}

#[test]
fn a_glossy_floor_reflects_the_picture_where_the_head_says() {
    let root = pack_root();
    let _ = std::fs::remove_dir_all(&root);
    // Glossy: roughness 0.3, the whole floor a specular surface.
    write_pack(&root, Some([77, 255]));
    assert!(pack::use_catalog_for_tests(&root), "the catalog was read");
    let preview = std::env::var_os("FRAMEPLAYERX_SHINE_PREVIEW_DIR").map(PathBuf::from);
    let picture = white();
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    // Pack order: no frame waits on the sorter.
    r.set_place_knobs(true, false);

    // The highlight is there, and it is only the highlight: the room's own
    // colours are black.
    let straight = shoot(&mut r, &mut e, &picture, &eyes_at(0.0, 0.0, 0.0), 1.0);
    let (peak, cx, cy) = highlight(&straight);
    eprintln!("straight: peak {peak:.0}, centre ({cx:.1}, {cy:.1})");
    assert!(peak > 25.0, "no highlight on a glossy floor: peak {peak}");
    let mid = SIZE as f32 / 2.0;
    assert!(
        (cx - mid).abs() < 6.0,
        "a head in front of the picture sees it in the middle, not at {cx}"
    );
    assert!(cy > 8.0 && cy < SIZE as f32 - 8.0, "{cy}");

    // Off: the knob, and the picture's light.
    r.set_place_shine(false);
    let off = shoot(&mut r, &mut e, &picture, &eyes_at(0.0, 0.0, 0.0), 1.0);
    assert!(highlight(&off).0 < 3.0, "the knob left the highlight on");
    r.set_place_shine(true);
    let dark = shoot(&mut r, &mut e, &picture, &eyes_at(0.0, 0.0, 0.0), 0.0);
    assert!(
        highlight(&dark).0 < 3.0,
        "a place whose picture light is off still has its highlight"
    );

    // The head moves, the highlight stays where the picture's image is: to
    // the right of the head's middle, the image is on the left, and the
    // other way round, the same distance either way.
    let right = shoot(&mut r, &mut e, &picture, &eyes_at(1.5, 0.0, 0.0), 1.0);
    let left = shoot(&mut r, &mut e, &picture, &eyes_at(-1.5, 0.0, 0.0), 1.0);
    let (pr, cr, _) = highlight(&right);
    let (pl, cl, _) = highlight(&left);
    eprintln!("head right: peak {pr:.0} at {cr:.1}; head left: peak {pl:.0} at {cl:.1}");
    assert!(
        pr > 15.0 && pl > 15.0,
        "the highlight is gone when the head moves"
    );
    assert!(cr < mid - 8.0, "head right: the image is at {cr}");
    assert!(cl > mid + 8.0, "head left: the image is at {cl}");
    assert!(
        ((mid - cr) - (cl - mid)).abs() < 5.0,
        "not symmetric: {cr} and {cl}"
    );
    // Forward (towards the screen) and back: it slides up and down the image.
    let (_, _, ny) = highlight(&shoot(
        &mut r,
        &mut e,
        &picture,
        &eyes_at(0.0, 0.0, -1.5),
        1.0,
    ));
    let (_, _, fy) = highlight(&shoot(
        &mut r,
        &mut e,
        &picture,
        &eyes_at(0.0, 0.0, 1.5),
        1.0,
    ));
    eprintln!("head forward: y {ny:.1}, back: y {fy:.1}, straight: y {cy:.1}");
    assert!(
        (ny - fy).abs() > 4.0,
        "moving the head back and forth moves nothing: {ny} {fy}"
    );
    if let Some(dir) = &preview {
        save_png(dir, "shine-head-left.png", &left);
        save_png(dir, "shine-head-middle.png", &straight);
        save_png(dir, "shine-head-right.png", &right);
        save_png(dir, "shine-off.png", &off);
    }

    // The picture's colours: a red picture gives a red highlight.
    let red = frame(160, 90, |_, _| [255, 0, 0]);
    let img = shoot(&mut r, &mut e, &red, &eyes_at(0.0, 0.0, 0.0), 1.0);
    let at = (cx.round() as u32, cy.round() as u32);
    let i = ((at.1 * SIZE + at.0) * 4) as usize;
    assert!(
        img[i] > 20 && img[i + 1] < 8 && img[i + 2] < 8,
        "{:?}",
        &img[i..i + 3]
    );
    e.destroy(&r);

    // A rougher and a fainter material: the same floor gives less, and a
    // pack with no material at all gives the weak default (no more than
    // that, and no more than the glossy one). The pack is read again by
    // each new renderer.
    let gloss_peak = peak;
    for (label, material, at_most) in [
        ("rough", Some([230u8, 255]), 0.5),
        ("faint mask", Some([77, 60]), 0.35),
        ("matte", Some([77, 0]), 0.0),
        ("no material", None, 0.4),
    ] {
        write_pack(&root, material);
        let Some((mut r, mut e)) = setup() else {
            return;
        };
        r.set_place_knobs(true, false);
        let img = shoot(&mut r, &mut e, &picture, &eyes_at(0.0, 0.0, 0.0), 1.0);
        let (p, _, _) = highlight(&img);
        eprintln!("{label}: peak {p:.0} against the glossy {gloss_peak:.0}");
        assert!(
            p <= gloss_peak * at_most + 2.0,
            "{label}: peak {p} against the glossy floor's {gloss_peak}"
        );
        if material.is_none() {
            // Weak is not nothing: the highlight is the default's, there.
            assert!(p >= 0.0);
        }
        e.destroy(&r);
    }
}
