//! A place pack with a mesh and textures baked for each seat (format 3), drawn
//! on a real (software) Vulkan device: the viewer's seat picks the set, a
//! change of seat swaps one for the other (on the place builder's worker
//! thread, with the old set drawn until the new one is ready), and a set that
//! will not load drops the whole pack for the built-in room.
//!
//! The pack is made here: three walls, a red one for the front seat, a green
//! one for the middle and a blue one for the back, in a folder this file makes
//! the catalog of the places the renderer knows. That can only be done once
//! per process, so the checks run one after the other in one test.

use fp_core::format::{Projection, StereoLayout};
use fp_core::view::ViewSettings;
use fp_render::capture::OffscreenEyes;
use fp_render::env::{EnvFrame, Place, SeatName, pack};
use fp_render::{Renderer, VideoParams};
use glam::{Mat4, Vec3};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod common;
use common::{EYE_HEIGHT, SIZE, near, params, px, render, setup, wall_mesh};

/// A seat's name and the colour of its wall's house texture.
const SEATS: [(&str, [u8; 3]); 3] = [
    ("front", [200, 20, 20]),
    ("middle", [20, 200, 20]),
    ("back", [20, 20, 200]),
];

fn colour_of(seat: SeatName) -> [u8; 3] {
    SEATS
        .iter()
        .find(|(name, _)| *name == seat.id())
        .map(|(_, c)| *c)
        .expect("a seat of the pack")
}

/// The pack in `<root>/cinema-v2`: each seat's wall (an 8 m square 2 m in
/// front of the viewer) in its own mesh and textures, files named for the
/// seat.
fn write_pack(root: &Path) {
    let dir = root.join("cinema-v2");
    std::fs::create_dir_all(&dir).expect("pack dir");
    let opaque = |c: [u8; 3]| [c[0], c[1], c[2], 255];
    let mut sets = Vec::new();
    for (seat, house) in SEATS {
        std::fs::write(dir.join(format!("{seat}-mesh.bin")), wall_mesh()).expect("mesh");
        let mut files = vec![
            (
                format!("{seat}-house.ktx2"),
                common::ktx2_solid(8, true, opaque(house)),
            ),
            (
                format!("{seat}-dark.ktx2"),
                common::ktx2_solid(8, true, opaque([0; 3])),
            ),
        ];
        for i in 0..8 {
            files.push((
                format!("{seat}-basis{i}.ktx2"),
                common::ktx2_solid(4, false, opaque([0; 3])),
            ));
        }
        let bytes: usize = files.iter().map(|(_, b)| b.len()).sum();
        for (file, data) in &files {
            std::fs::write(dir.join(file), data).expect("texture");
        }
        let basis: Vec<String> = (0..8)
            .map(|i| format!("\"{seat}-basis{i}.ktx2\""))
            .collect();
        sets.push(format!(
            r#""{seat}": {{
  "mesh": {{ "file": "{seat}-mesh.bin", "vertices": 4, "triangles": 2 }},
  "textures": {{ "size": 8, "basis_size": 4, "bytes": {bytes},
                 "house": "{seat}-house.ktx2", "dark": "{seat}-dark.ktx2",
                 "basis": [{}] }} }}"#,
            basis.join(", ")
        ));
    }
    let manifest = format!(
        r#"{{ "format": 3, "id": "cinema-v2", "name": "Cinema", "version": "test",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [9.0, 3.8] }},
  "seats": [ {{ "name": "front", "eye": [0, 2.9, 5.0] }},
             {{ "name": "middle", "eye": [0, 3.2, 9.0] }},
             {{ "name": "back", "eye": [0, 3.6, 13.0] }} ],
  "default_seat": 1,
  "seat_sets": {{ {} }},
  "basis_gain": 1.0 }}"#,
        sets.join(", ")
    );
    std::fs::write(dir.join("manifest.json"), manifest).expect("manifest");
}

fn pack_root() -> PathBuf {
    std::env::temp_dir().join(format!("fp-gpu-seat-sets-{}", std::process::id()))
}

/// The four corners of the eye image, which the wall fills, averaged.
fn wall_colour(img: &[u8]) -> [u8; 3] {
    let far = SIZE - 3;
    let spots = [(2, 2), (far, 2), (2, far), (far, far)].map(|(x, y)| px(img, x, y));
    let mean = |ch: usize| (spots.iter().map(|c| u32::from(c[ch])).sum::<u32>() / 4) as u8;
    [mean(0), mean(1), mean(2)]
}

/// The cinema's walls as seen from `seat`, one frame.
fn look(r: &mut Renderer, e: &mut OffscreenEyes, seat: SeatName) -> [u8; 3] {
    let mut p: VideoParams = params(
        Projection::Flat,
        StereoLayout::Mono,
        ViewSettings::default(),
    );
    p.screen_pose = Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, -4.0));
    p.background = [0.0, 0.0, 0.0, 1.0];
    p.env = Some(EnvFrame {
        place: Place::Cinema,
        seat,
        room_to_world: Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0)),
        house: 1.0,
        screen_light: 0.0,
        time: 1.0,
    });
    let [left, _] = render(r, e, None, &p, &[]);
    wall_colour(&left)
}

fn is_seat_wall(c: [u8; 3]) -> Option<SeatName> {
    [SeatName::Front, SeatName::Middle, SeatName::Back]
        .into_iter()
        .find(|s| near(c, colour_of(*s), 6))
}

/// Frames of `seat` until the wall is its colour or five seconds are over;
/// every colour met on the way.
fn until_shown(r: &mut Renderer, e: &mut OffscreenEyes, seat: SeatName) -> (bool, Vec<[u8; 3]>) {
    let start = Instant::now();
    let mut met = Vec::new();
    while start.elapsed() < Duration::from_secs(5) {
        let c = look(r, e, seat);
        met.push(c);
        if near(c, colour_of(seat), 6) {
            return (true, met);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    (false, met)
}

#[test]
fn a_pack_baked_for_each_seat_draws_the_seat_the_viewer_is_in() {
    let root = pack_root();
    let _ = std::fs::remove_dir_all(&root);
    write_pack(&root);
    assert!(pack::use_catalog_for_tests(&root), "the catalog was read");
    for seat in [SeatName::Front, SeatName::Middle, SeatName::Back] {
        assert_eq!(Place::Cinema.seat_set(seat), Some(seat));
    }

    // Built on the render thread in the frame that wants it: each seat shows
    // its own wall, in any order, and again after a round trip.
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    for seat in [
        SeatName::Middle,
        SeatName::Back,
        SeatName::Front,
        SeatName::Back,
        SeatName::Middle,
    ] {
        let c = look(&mut r, &mut e, seat);
        assert!(near(c, colour_of(seat), 6), "{seat:?}: {c:?}");
    }
    // A seat the pack has no set for draws its default's.
    let couch = look(&mut r, &mut e, SeatName::Couch);
    assert!(near(couch, colour_of(SeatName::Middle), 6), "{couch:?}");
    e.destroy(&r);

    // On the worker thread, as in the app: the middle's wall is up, the
    // viewer moves to the back, and the middle's wall stays drawn (never
    // Plain, never the built-in room) until the back's is ready.
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    r.set_inline_places(false);
    let (shown, _) = until_shown(&mut r, &mut e, SeatName::Middle);
    assert!(shown, "the middle's wall was never shown");
    let (shown, met) = until_shown(&mut r, &mut e, SeatName::Back);
    assert!(shown, "the back's wall was never shown: {met:?}");
    for c in &met[..met.len() - 1] {
        assert_eq!(
            is_seat_wall(*c),
            Some(SeatName::Middle),
            "in between: {c:?}"
        );
    }
    // And back again, the same way.
    let (shown, met) = until_shown(&mut r, &mut e, SeatName::Front);
    assert!(shown, "the front's wall was never shown: {met:?}");
    for c in &met[..met.len() - 1] {
        assert_eq!(is_seat_wall(*c), Some(SeatName::Back), "in between: {c:?}");
    }
    e.destroy(&r);

    // A set that scans but will not load (a vertex that is not a number, the
    // length unchanged) drops the whole pack: the built-in room is the place
    // from then on, seats and all.
    let Some((mut r, mut e)) = setup() else {
        return;
    };
    let mut mesh = wall_mesh();
    mesh[12..16].copy_from_slice(&f32::NAN.to_le_bytes());
    std::fs::write(root.join("cinema-v2/back-mesh.bin"), mesh).expect("mesh");
    let c = look(&mut r, &mut e, SeatName::Middle);
    assert!(near(c, colour_of(SeatName::Middle), 6), "{c:?}");
    let c = look(&mut r, &mut e, SeatName::Back);
    assert_eq!(
        is_seat_wall(c),
        None,
        "the broken pack is still shown: {c:?}"
    );
    assert_eq!(Place::Cinema.seat_set(SeatName::Back), None);
    let c = look(&mut r, &mut e, SeatName::Middle);
    assert_eq!(is_seat_wall(c), None, "the broken pack came back: {c:?}");
    e.destroy(&r);
}
