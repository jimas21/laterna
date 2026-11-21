//! A baked place with no film on its screen: the screen is a clean, matte,
//! dim rectangle with straight edges, never the white, blotchy sheet the pack
//! baked there. The pack here bakes a bright white sheet a little bigger than
//! its manifest's screen, so anything of it showing inside the screen's
//! rectangle is a failure, and the rectangle's border is the only edge.
//!
//! The catalog can only be set once per process, so every check runs one
//! after the other in one test.

use fp_core::view::ViewSettings;
use fp_render::env::{EnvFrame, Place, SeatName, pack};
use std::path::PathBuf;

mod common;
use common::{cinema_params, eyes_at, ktx2_solid, place_frame, setup_sized};

const SIZE: u32 = 256;
const SEAT_EYE: [f32; 3] = [0.0, 2.9, 5.0];
/// The manifest's screen: middle and size, metres.
const CENTRE: [f32; 3] = [0.0, 3.5, 0.0];
const MAX: [f32; 2] = [6.0, 2.6];
/// The size of the baked sheet, bigger than the screen on every side.
const SHEET: [f32; 2] = [8.4, 3.6];

fn root() -> PathBuf {
    std::env::temp_dir().join(format!("fp-gpu-idle-{}", std::process::id()))
}

/// The sheet: one quad on the screen's plane.
fn sheet_mesh() -> Vec<u8> {
    let (hx, hy) = (SHEET[0] / 2.0, SHEET[1] / 2.0);
    let corners = [
        [-hx, CENTRE[1] - hy, 0.0, 0.0, 1.0],
        [hx, CENTRE[1] - hy, 0.0, 1.0, 1.0],
        [hx, CENTRE[1] + hy, 0.0, 1.0, 0.0],
        [-hx, CENTRE[1] + hy, 0.0, 0.0, 0.0],
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

fn write_pack(root: &std::path::Path) {
    let dir = root.join("cinema-v2");
    std::fs::create_dir_all(&dir).expect("pack dir");
    std::fs::write(dir.join("mesh.bin"), sheet_mesh()).expect("mesh");
    let white = [255, 255, 255, 255];
    let black = [0, 0, 0, 255];
    let mut files = vec![
        ("house.ktx2".to_string(), ktx2_solid(8, true, white)),
        ("dark.ktx2".to_string(), ktx2_solid(8, true, white)),
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
        r#"{{ "format": 3, "id": "cinema-v2", "name": "Cinema", "version": "test",
  "screen": {{ "centre": [{}, {}, {}], "max": [{}, {}] }},
  "seats": [ {{ "name": "middle", "eye": [{}, {}, {}] }} ],
  "default_seat": 0,
  "seat_sets": {{ "middle": {{
    "mesh": {{ "file": "mesh.bin", "vertices": 4, "triangles": 2 }},
    "textures": {{ "size": 8, "basis_size": 4, "bytes": {bytes},
                   "house": "house.ktx2", "dark": "dark.ktx2",
                   "basis": [{}] }} }} }},
  "basis_gain": 1.0 }}"#,
        CENTRE[0],
        CENTRE[1],
        CENTRE[2],
        MAX[0],
        MAX[1],
        SEAT_EYE[0],
        SEAT_EYE[1],
        SEAT_EYE[2],
        basis.join(", ")
    );
    std::fs::write(dir.join("manifest.json"), manifest).expect("manifest");
}

/// Luma (0 to 255) of every pixel of the left eye, row by row.
fn luma(img: &[u8]) -> Vec<f32> {
    img.chunks(4)
        .map(|p| (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 3.0)
        .collect()
}

/// A metre position on the screen's plane to a pixel: the eyes look straight
/// at it from 5 m with a 90 degree field, so the view spans 10 m there.
fn column(x: f32) -> usize {
    (SIZE as f32 / 2.0 + (x - SEAT_EYE[0]) * SIZE as f32 / 10.0) as usize
}

fn row(y: f32) -> usize {
    (SIZE as f32 / 2.0 - (y - SEAT_EYE[1]) * SIZE as f32 / 10.0) as usize
}

fn at(l: &[f32], x: usize, y: usize) -> f32 {
    l[y * SIZE as usize + x]
}

/// What the screen looks like with the lights at `house`, nothing playing.
fn shoot(house: f32) -> Option<Vec<f32>> {
    let (mut r, mut e) = setup_sized(SIZE)?;
    r.set_place_knobs(true, false);
    let mut img = Vec::new();
    for t in 1..=4 {
        let env = EnvFrame {
            time: f64::from(t),
            ..place_frame(Place::Cinema, SeatName::Middle, house)
        };
        let mut p = cinema_params(ViewSettings::default(), env);
        p.background = [0.0, 0.0, 0.0, 1.0];
        r.begin_frame().unwrap();
        r.draw(&e.targets(), &eyes_at(common::EYE_HEIGHT), &p, &[])
            .unwrap();
        r.end_frame().unwrap();
        img = e.read(&r, 0).unwrap();
    }
    e.destroy(&r);
    Some(luma(&img))
}

/// Mean and spread (highest minus lowest) of the screen's middle.
fn middle(l: &[f32]) -> (f32, f32) {
    let (x0, x1) = (column(-2.5), column(2.5));
    let (y0, y1) = (row(4.5), row(2.5));
    let mut sum = 0.0;
    let mut n = 0.0;
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for y in y0..y1 {
        for x in x0..x1 {
            let v = at(l, x, y);
            sum += v;
            n += 1.0;
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    (sum / n, hi - lo)
}

/// How many pixels of a line are neither the screen's grey nor the sheet's
/// white: the width of an edge.
fn between(line: impl Iterator<Item = f32>, grey: f32) -> usize {
    line.filter(|v| *v > grey + 12.0 && *v < 240.0).count()
}

#[test]
fn a_pack_place_with_no_film_shows_a_clean_dim_matte_screen_with_crisp_edges() {
    let root = root();
    let _ = std::fs::remove_dir_all(&root);
    write_pack(&root);
    assert!(pack::use_catalog_for_tests(&root), "the catalog was read");

    let Some(lit) = shoot(1.0) else {
        return; // no Vulkan device here
    };

    // The inside is one flat colour (a pixel or two of dither), dim, and
    // nothing of the baked white sheet.
    let (mean, spread) = middle(&lit);
    assert!(spread <= 4.0, "the screen is blotchy: spread {spread}");
    assert!(
        mean > 40.0 && mean < 110.0,
        "the screen is not a dim grey: {mean}"
    );

    // The sheet is still the bright wall past the screen's border.
    let sheet = at(&lit, column(-3.6), row(3.5));
    assert!(sheet > 200.0, "the sheet beside the screen: {sheet}");

    // Edges: the side and the top go from grey to the wall in a pixel or
    // two, and straight (at the same column on every row).
    let mid_row = row(3.5);
    let side = between(
        (column(-3.4)..column(-2.7)).map(|x| at(&lit, x, mid_row)),
        mean,
    );
    assert!(side <= 2, "the side edge is {side} pixels wide");
    let mid_col = column(0.0);
    let top = between((row(5.0)..row(4.5)).map(|y| at(&lit, mid_col, y)), mean);
    assert!(top <= 2, "the top edge is {top} pixels wide");
    let edge_at = |y: usize| {
        (column(-3.4)..column(-2.7))
            .find(|x| at(&lit, *x, y) < mean + 40.0)
            .expect("an edge")
    };
    let (a, b) = (edge_at(row(3.9)), edge_at(row(3.1)));
    assert!(a.abs_diff(b) <= 1, "the edge is not straight: {a} and {b}");
    // And no baked pixel just inside the border.
    assert!(
        at(&lit, column(-3.0) + 2, mid_row) < mean + 12.0,
        "white leaks at the border"
    );

    // The lights going down take it to near black, steadily.
    let half = shoot(0.5).expect("device");
    let dark = shoot(0.0).expect("device");
    let (mean_half, _) = middle(&half);
    let (mean_dark, spread_dark) = middle(&dark);
    assert!(
        mean_dark < 12.0,
        "the screen with the lights off: {mean_dark}"
    );
    assert!(spread_dark <= 4.0, "dark spread {spread_dark}");
    assert!(
        mean_dark + 3.0 < mean_half && mean_half + 3.0 < mean,
        "the fade is not steady: {mean_dark} {mean_half} {mean}"
    );
    // The sheet past the border is still the pack's own with the lights off.
    assert!(at(&dark, column(-3.6), row(3.5)) > 200.0);
}
