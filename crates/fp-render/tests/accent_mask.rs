//! A baked place's accent lights (exit signs, step lights): the pack's
//! material texture's B channel marks them (1 an accent, 0 anything else), and
//! with Minimal lighting on (`Renderer::set_accent_lights(0.0)`) the marked
//! texels go dark for the film. A pack without a material map, or whose map
//! has B at 0, is not touched.
//!
//! The pack is one lit floor (house and dark textures the same grey, no
//! basis light); the catalog can only be set once per process, so every
//! check runs one after the other in one test.

use fp_core::view::ViewSettings;
use fp_render::EyeView;
use fp_render::env::{EnvFrame, Place, SeatName, pack};
use fp_render::math::{Fov, projection, view};
use glam::{Quat, Vec3};
use std::path::{Path, PathBuf};

mod common;
use common::{SIZE, cinema_params, ktx2_solid, place_frame, setup};

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
    std::env::temp_dir().join(format!("fp-gpu-accent-{}", std::process::id()))
}

/// Writes the pack in `<root>/cinema-v2`, with the material texture `rgb`
/// (roughness, specular mask, accent) or none.
fn write_pack(root: &Path, material: Option<[u8; 3]>) {
    let dir = root.join("cinema-v2");
    std::fs::create_dir_all(&dir).expect("pack dir");
    std::fs::write(dir.join("mesh.bin"), floor_mesh()).expect("mesh");
    let grey = [150, 150, 150, 255];
    let black = [0, 0, 0, 255];
    let mut files = vec![
        ("house.ktx2".to_string(), ktx2_solid(8, true, grey)),
        ("dark.ktx2".to_string(), ktx2_solid(8, true, grey)),
    ];
    for i in 0..8 {
        files.push((format!("basis{i}.ktx2"), ktx2_solid(4, false, black)));
    }
    let mut material_json = String::new();
    if let Some([rough, mask, accent]) = material {
        files.push((
            "material.ktx2".into(),
            ktx2_solid(4, false, [rough, mask, accent, 255]),
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

/// Both eyes looking down at the floor.
fn eyes() -> [EyeView; 2] {
    let look = Quat::from_rotation_x(-1.1);
    let ev = |dx: f32| EyeView {
        view: view(Vec3::new(dx, common::EYE_HEIGHT, 0.0), look),
        proj: projection(Fov::symmetric(90.0), 0.05, 100.0),
    };
    [ev(-0.032), ev(0.032)]
}

/// The floor's brightness (mean colour code of the middle of the left eye)
/// with the accent lights at `accent`, for the pack written with `material`.
/// NaN where there is no Vulkan device.
fn floor_with(material: Option<[u8; 3]>, accent: f32) -> f32 {
    write_pack(&pack_root(), material);
    let Some((mut r, mut e)) = setup() else {
        return f32::NAN;
    };
    r.set_place_knobs(true, false);
    r.set_accent_lights(accent);
    let mut img = Vec::new();
    for t in 1..=4 {
        let env = EnvFrame {
            screen_light: 0.0,
            time: f64::from(t),
            ..place_frame(Place::Cinema, SeatName::Middle, 0.0)
        };
        let mut p = cinema_params(ViewSettings::default(), env);
        p.background = [0.0, 0.0, 0.0, 1.0];
        r.begin_frame().unwrap();
        r.draw(&e.targets(), &eyes(), &p, &[]).unwrap();
        r.end_frame().unwrap();
        img = e.read(&r, 0).unwrap();
    }
    e.destroy(&r);
    // The floor fills the lower middle of the view.
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in SIZE * 3 / 5..SIZE * 4 / 5 {
        for x in SIZE * 2 / 5..SIZE * 3 / 5 {
            let i = ((y * SIZE + x) * 4) as usize;
            sum += (f32::from(img[i]) + f32::from(img[i + 1]) + f32::from(img[i + 2])) / 3.0;
            n += 1.0;
        }
    }
    sum / n
}

#[test]
fn minimal_lighting_dims_the_texels_the_material_maps_blue_channel_marks() {
    let root = pack_root();
    let _ = std::fs::remove_dir_all(&root);
    write_pack(&root, None);
    assert!(pack::use_catalog_for_tests(&root), "the catalog was read");

    let base = floor_with(None, 1.0);
    if base.is_nan() {
        return; // no Vulkan device here
    }
    assert!(base > 60.0, "the floor is lit: {base}");

    // No material map: Minimal lighting changes nothing.
    let none_off = floor_with(None, 0.0);
    assert!((none_off - base).abs() < 2.0, "{none_off} against {base}");

    // A map with B at 0: nothing is an accent.
    let plain = floor_with(Some([217, 64, 0]), 0.0);
    assert!((plain - base).abs() < 2.0, "{plain} against {base}");

    // B at 1 and the accent lights as baked: untouched; with them off: dark.
    let marked_on = floor_with(Some([217, 64, 255]), 1.0);
    assert!((marked_on - base).abs() < 2.0, "{marked_on} against {base}");
    let marked_off = floor_with(Some([217, 64, 255]), 0.0);
    assert!(marked_off < 3.0, "accent texels stayed lit: {marked_off}");

    // In between, in between.
    let half = floor_with(Some([217, 64, 255]), 0.5);
    assert!(half > marked_off + 10.0 && half < base - 10.0, "{half}");
    // A half-marked texel is half dimmed.
    let part = floor_with(Some([217, 64, 128]), 0.0);
    assert!(part > 10.0 && part < base - 10.0, "{part}");
}
