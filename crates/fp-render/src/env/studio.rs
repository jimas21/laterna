//! The studio: a dark gradient backdrop and a matte floor that takes the
//! screen's glow, with a soft reflection. A sky place: it has no walls, it
//! sits in anchor space (the origin at the viewer's eye), and the screen is
//! wherever the user's own settings put it.

use super::kit::MeshBuilder;
use super::{Fog, Layout, Lighting, Material, Pattern, PlaceData, Seat, SeatName, Sky};
use glam::Vec3;

/// The floor's height in anchor space: 1.25 m below the eye.
pub const FLOOR_Y: f32 = -1.25;
/// The floor is a disc this wide.
pub const FLOOR_RADIUS: f32 = 40.0;

/// Ring radii of the floor disc: close together near the viewer, where light
/// baked per vertex shows, and wide towards the horizon.
/// The last is `FLOOR_RADIUS`.
const RINGS: [f32; 11] = [1.0, 2.0, 3.5, 5.0, 7.0, 9.0, 12.0, 16.0, 22.0, 30.0, 40.0];

const SEGMENTS: usize = 64;

const ZENITH: [f32; 3] = [0.010, 0.012, 0.018];
const HORIZON: [f32; 3] = [0.045, 0.045, 0.052];

/// One implicit seat at the origin, and no screen of its own.
pub fn layout() -> Layout {
    let seat = Seat {
        name: SeatName::Middle,
        eye: Vec3::ZERO,
    };
    Layout {
        seats: vec![seat],
        default_seat: 0,
        screen: None,
        floor_y: FLOOR_Y,
    }
}

/// The floor, the gradient and the fog.
///
/// The room-light share (`house`) of the floor is 1 throughout, and half of
/// the ambient light is `house_rgb`, so the ambient light dims to 50% with the
/// house lights and comes back with them.
pub fn build() -> PlaceData {
    let floor = Material {
        albedo: [0.12, 0.115, 0.11],
        pattern: Pattern::Plain,
        emissive: 0.0,
        sheen: 0.35,
    };
    let mut m = MeshBuilder::new();
    m.disc(Vec3::new(0.0, FLOOR_Y, 0.0), &RINGS, SEGMENTS, 0);
    for v in &mut m.vertices {
        v.house = 1.0;
    }
    let half = [0.02, 0.02, 0.022];
    PlaceData {
        vertices: m.vertices,
        indices: m.indices,
        palette: vec![floor],
        lighting: Lighting {
            ambient_up: half,
            ambient_down: half.map(|c| c * 0.2),
            house_rgb: half,
            spill: 1.0,
            fog: Some(Fog {
                rgb: HORIZON,
                start: 8.0,
                end: 35.0,
            }),
        },
        sky: Sky::Gradient {
            zenith: ZENITH,
            horizon: HORIZON,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_lies_below_the_eye_and_reaches_the_horizon() {
        let data = build();
        let layout = layout();
        assert!(layout.floor_y < 0.0 && layout.screen.is_none());
        assert_eq!(layout.seats.len(), 1);
        assert_eq!(layout.seats[0].eye, Vec3::ZERO);
        let radius = |p: [f32; 3]| Vec3::from(p).xz().length();
        let widest = data.vertices.iter().map(|v| radius(v.pos));
        let widest = widest.fold(0.0, f32::max);
        assert!((widest - FLOOR_RADIUS).abs() < 1e-3);
        assert!(data.vertices.iter().all(|v| v.pos[1] == FLOOR_Y));
    }

    #[test]
    fn the_sky_is_a_dark_gradient_and_the_fog_matches_its_horizon() {
        let data = build();
        let Sky::Gradient { zenith, horizon } = data.sky else {
            panic!("the studio has a gradient sky");
        };
        assert!(zenith.iter().zip(horizon).all(|(&z, h)| z < h));
        assert!(horizon.iter().all(|&c| c < 0.1));
        let fog = data.lighting.fog.unwrap();
        assert_eq!(fog.rgb, horizon);
        assert!(fog.start < fog.end && fog.end < FLOOR_RADIUS);
    }

    #[test]
    fn the_ambient_light_halves_when_the_house_lights_go_down() {
        let l = build().lighting;
        // At house level 0: ambient only; at 1: ambient plus house_rgb.
        for (up, house) in l.ambient_up.iter().zip(l.house_rgb) {
            assert!((up / (up + house) - 0.5).abs() < 1e-5);
        }
    }
}
