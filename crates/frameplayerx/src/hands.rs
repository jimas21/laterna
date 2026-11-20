//! The controllers as the eyes see them. With the real room hidden (inside a
//! place, or in the empty dark of Just the screen) nothing of your hands is
//! left, so each tracked controller is drawn as a small solid shape at its
//! pointing pose, the laser coming out of its front.
//!
//! Plain flat-coloured faces drawn into the eyes after the room, so they
//! never go dark with the house lights and read in a black auditorium; the
//! only shading is a fixed one from above, so the shape keeps its form.
//! [`controller`] is pure geometry.

use fp_render::QuadDraw;
use glam::{Quat, Vec3};
use std::sync::OnceLock;

/// `FRAMEPLAYERX_CONTROLLERS=0` draws none (a kill switch, as for places).
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("FRAMEPLAYERX_CONTROLLERS").is_none_or(|v| v != "0"))
}

/// A box in the controller's frame (-Z is where it points): the corners it
/// spans, and its colour.
struct Part {
    min: Vec3,
    max: Vec3,
    /// Linear colour.
    rgb: [f32; 3],
}

/// The grip, a plate on its top with the thumbstick's room, and the trigger
/// under the front. The pointing pose is at the front of the grip, so the
/// body reaches back from it into the palm.
fn parts(trigger: f32, squeeze: f32) -> [Part; 3] {
    let pull = trigger.clamp(0.0, 1.0);
    let hold = squeeze.clamp(0.0, 1.0);
    // The accent of the interface, for what is being pressed.
    let accent = [0.06, 0.38, 1.0];
    let lit = |base: [f32; 3], by: f32| {
        [
            base[0] + (accent[0] - base[0]) * by,
            base[1] + (accent[1] - base[1]) * by,
            base[2] + (accent[2] - base[2]) * by,
        ]
    };
    [
        Part {
            min: Vec3::new(-0.017, -0.022, 0.0),
            max: Vec3::new(0.017, 0.010, 0.115),
            rgb: lit([0.17, 0.18, 0.21], hold * 0.6),
        },
        Part {
            min: Vec3::new(-0.030, 0.010, -0.012),
            max: Vec3::new(0.030, 0.019, 0.052),
            rgb: [0.34, 0.36, 0.41],
        },
        Part {
            min: Vec3::new(-0.006, -0.034, 0.012),
            max: Vec3::new(0.006, -0.022, 0.040),
            rgb: lit([0.50, 0.52, 0.56], pull),
        },
    ]
}

/// The faces of a box: the outward normal and the corners going round, as
/// the unit cube's (min or max on each axis).
const FACES: [([f32; 3], [[usize; 3]; 4]); 6] = [
    (
        [1.0, 0.0, 0.0],
        [[1, 1, 0], [1, 1, 1], [1, 0, 1], [1, 0, 0]],
    ),
    (
        [-1.0, 0.0, 0.0],
        [[0, 1, 1], [0, 1, 0], [0, 0, 0], [0, 0, 1]],
    ),
    (
        [0.0, 1.0, 0.0],
        [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]],
    ),
    (
        [0.0, -1.0, 0.0],
        [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]],
    ),
    (
        [0.0, 0.0, 1.0],
        [[0, 1, 1], [0, 0, 1], [1, 0, 1], [1, 1, 1]],
    ),
    (
        [0.0, 0.0, -1.0],
        [[1, 1, 0], [1, 0, 0], [0, 0, 0], [0, 1, 0]],
    ),
];

/// How much of a face's colour shows facing away from the light at the
/// least, so that nothing goes black.
const AMBIENT: f32 = 0.55;

/// One controller held at `origin`, turned by `rot` (the pointing pose: -Z
/// is the way it points), seen from `eye`: its faces that face the eye,
/// farthest first, in the order they are to be drawn over each other.
/// `trigger` and `squeeze` (0 to 1) light the parts they press.
pub fn controller(origin: Vec3, rot: Quat, eye: Vec3, trigger: f32, squeeze: f32) -> Vec<QuadDraw> {
    if !(origin.is_finite() && rot.is_finite() && eye.is_finite()) {
        return Vec::new();
    }
    let rot = rot.normalize();
    let to_world = |p: Vec3| origin + rot * p;
    // Light from above and a little ahead of the eye.
    let light = (Vec3::Y + (eye - origin).normalize_or_zero() * 0.4).normalize();
    let mut faces: Vec<(f32, QuadDraw)> = Vec::new();
    for part in parts(trigger, squeeze) {
        let pick = |c: [usize; 3]| {
            to_world(Vec3::new(
                if c[0] == 1 { part.max.x } else { part.min.x },
                if c[1] == 1 { part.max.y } else { part.min.y },
                if c[2] == 1 { part.max.z } else { part.min.z },
            ))
        };
        for (normal, corners) in FACES {
            let normal = rot * Vec3::from(normal);
            let corners = corners.map(pick);
            let centre = corners.iter().sum::<Vec3>() / 4.0;
            // Faces turned from the eye are behind the ones in front.
            if normal.dot(eye - centre) <= 0.0 {
                continue;
            }
            let shade = AMBIENT + (1.0 - AMBIENT) * normal.dot(light).max(0.0);
            let [r, g, b] = part.rgb.map(|c| c * shade);
            faces.push((
                (centre - eye).length_squared(),
                QuadDraw {
                    texture: fp_render::QuadTexture::White,
                    corners,
                    uv: [glam::Vec2::ZERO, glam::Vec2::ONE],
                    color: [r, g, b, 1.0],
                    eye: None,
                },
            ));
        }
    }
    faces.sort_by(|a, b| b.0.total_cmp(&a.0));
    faces.into_iter().map(|(_, q)| q).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held() -> (Vec3, Quat) {
        // Out in front, pointing a little up and to the left.
        (
            Vec3::new(0.25, 1.1, -0.3),
            Quat::from_rotation_y(0.4) * Quat::from_rotation_x(0.3),
        )
    }

    #[test]
    fn a_controller_is_drawn_from_its_pointing_pose_back_into_the_hand() {
        let (at, rot) = held();
        let eye = Vec3::new(0.0, 1.6, 0.0);
        let quads = controller(at, rot, eye, 0.0, 0.0);
        assert!(!quads.is_empty());
        let forward = rot * Vec3::NEG_Z;
        for q in &quads {
            for c in q.corners {
                assert!(c.is_finite());
                // Behind the pointing pose (the laser starts in front), and
                // no more than a hand's length from it.
                let along = (c - at).dot(forward);
                assert!(along < 0.03, "{along}");
                assert!((c - at).length() < 0.14, "{}", (c - at).length());
            }
        }
    }

    #[test]
    fn only_the_faces_toward_the_eye_are_drawn_farthest_first() {
        let (at, rot) = held();
        let eye = Vec3::new(0.0, 1.6, 0.0);
        let quads = controller(at, rot, eye, 0.0, 0.0);
        // At most three faces of a box face the eye.
        assert!(quads.len() <= 9, "{}", quads.len());
        let far = |q: &QuadDraw| (q.corners.iter().sum::<Vec3>() / 4.0 - eye).length_squared();
        assert!(quads.windows(2).all(|w| far(&w[0]) >= far(&w[1])));
        // Looked at from the other side, a different set shows.
        let other = controller(at, rot, Vec3::new(0.5, 0.5, 0.5), 0.0, 0.0);
        assert_ne!(
            quads.iter().map(|q| q.corners[0]).collect::<Vec<_>>(),
            other.iter().map(|q| q.corners[0]).collect::<Vec<_>>()
        );
    }

    #[test]
    fn it_reads_in_a_dark_room_and_never_goes_transparent() {
        let (at, rot) = held();
        let quads = controller(at, rot, Vec3::new(0.0, 1.6, 0.0), 0.0, 0.0);
        for q in &quads {
            let [r, g, b, a] = q.color;
            assert_eq!(a, 1.0);
            // Light enough to stand out from black (about 33 on a 255 scale
            // once shown) at the darkest face, without glaring.
            let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            assert!((0.04..0.6).contains(&lum), "{lum}");
        }
    }

    #[test]
    fn pulling_the_trigger_lights_it() {
        let (at, rot) = held();
        let eye = Vec3::new(0.0, 0.5, 0.0);
        // The trigger is under the grip: look at the controller from below.
        let blue = |trigger: f32| {
            controller(at, rot, eye, trigger, 0.0)
                .iter()
                .map(|q| q.color[2] - q.color[0])
                .fold(f32::MIN, f32::max)
        };
        assert!(blue(1.0) > blue(0.0) + 0.2, "{} {}", blue(1.0), blue(0.0));
    }

    #[test]
    fn nonsense_poses_draw_nothing_and_do_not_panic() {
        let eye = Vec3::ZERO;
        assert!(controller(Vec3::NAN, Quat::IDENTITY, eye, 0.0, 0.0).is_empty());
        assert!(
            controller(
                Vec3::ONE,
                Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0),
                eye,
                0.0,
                0.0
            )
            .is_empty()
        );
        // With the eye on it, nothing is dropped or divided by zero.
        let quads = controller(Vec3::ONE, Quat::IDENTITY, Vec3::ONE, 5.0, -2.0);
        assert!(
            quads
                .iter()
                .all(|q| q.corners.iter().all(|c| c.is_finite()))
        );
    }

    #[test]
    fn the_kill_switch_defaults_to_on() {
        // Not set in the test environment.
        if std::env::var("FRAMEPLAYERX_CONTROLLERS").is_err() {
            assert!(enabled());
        }
    }
}
