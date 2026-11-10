//! Docking the screen to a controller, apart from the app: pure maths.
//!
//! A docked screen is a small screen held above the controller, tilted back
//! towards your face, that goes where the hand goes. It has a width of its
//! own (the floating screen's size is left alone, so undocking brings that
//! screen back as it was). The hand's pose is the aim pose, the only one read.

use glam::{Mat3, Mat4, Quat, Vec3};
use std::ops::RangeInclusive;

/// How wide a screen is when first docked, metres.
pub const WIDTH: f32 = 0.30;

/// How wide a docked screen may be made, metres.
pub const WIDTHS: RangeInclusive<f32> = 0.12..=0.80;

/// How far the eye is taken to be from a docked screen when its window
/// controls are sized (they are in degrees, as the eye sees them), metres.
pub const EYE: f32 = 0.5;

/// Where the screen's middle is from the controller's aim pose: above it and a
/// little ahead, in the aim pose's own axes, metres.
const OFFSET: Vec3 = Vec3::new(0.0, 0.10, -0.06);

/// How far the screen is tilted back towards the face, degrees.
const TILT: f32 = 55.0;

/// How long a hand that is not tracked (covered by the other, say) leaves the
/// screen where it was, seconds, and then how long the screen takes to
/// vanish.
pub const HOLD: f32 = 1.0;
pub const FADE: f32 = 0.4;

/// The least the screen shrinks to while it is hidden, metres.
const HIDDEN: f32 = 0.002;

/// The floating screen's distance from where a docked one is torn off: that
/// many times as far as the docked screen was from you, within `NEAR..=FAR`.
const TEAR_DISTANCE: f32 = 4.0;
const TEAR_NEAR: f32 = 1.0;
const TEAR_FAR: f32 = 20.0;

/// `rot` with its roll about its own forward axis taken out, so the screen
/// held up on it stays level whichever way the controller is rolled. Held
/// straight up or down there is no level to find, and it stays as it is.
fn upright(rot: Quat) -> Quat {
    let forward = rot * Vec3::NEG_Z;
    let side = forward.cross(Vec3::Y);
    if side.length() < 0.2 {
        return rot;
    }
    let right = side.normalize();
    let up = right.cross(forward);
    Quat::from_mat3(&Mat3::from_cols(right, up, -forward))
}

/// Where the screen is when docked to a hand pointing `aim`: its middle is
/// at `OFFSET` from the hand, tilted back towards the face, level.
pub fn docked_pose(aim: (Vec3, Quat)) -> Mat4 {
    let (pos, rot) = aim;
    Mat4::from_rotation_translation(upright(rot), pos)
        * Mat4::from_translation(OFFSET)
        * Mat4::from_rotation_x(-TILT.to_radians())
}

/// The frame the window controls are placed in while docked: the eye taken
/// to be `EYE` metres in front of the screen, which is straight ahead of it
/// there (see `layout`).
pub fn picture(pose: Mat4) -> Mat4 {
    pose * Mat4::from_translation(Vec3::new(0.0, 0.0, EYE))
}

/// A screen docked to a hand.
#[derive(Clone, Copy, Debug)]
pub struct Dock {
    /// Which hand holds it (0 left, 1 right).
    pub hand: usize,
    /// How wide it is, metres.
    pub width: f32,
    pose: Mat4,
    /// How long the hand has not been tracked, seconds. It starts out as long
    /// as it takes to vanish: a screen that has not seen its hand yet is
    /// hidden.
    lost: f32,
}

impl Dock {
    pub fn new(hand: usize) -> Dock {
        Dock {
            hand,
            width: WIDTH,
            pose: Mat4::IDENTITY,
            lost: HOLD + FADE,
        }
    }

    /// Follows the hand for a frame (`aim` is `None` while it is not
    /// tracked).
    pub fn track(&mut self, aim: Option<(Vec3, Quat)>, dt: f32) {
        match aim {
            Some(aim) => {
                self.pose = docked_pose(aim);
                self.lost = 0.0;
            }
            None => self.lost = (self.lost + dt).min(HOLD + FADE),
        }
    }

    /// Where the screen is: where the hand last was while it is lost.
    pub fn pose(&self) -> Mat4 {
        self.pose
    }

    /// How much of its width it shows: all of it, then smaller as a lost hand
    /// stays lost, until it is gone.
    pub fn scale(&self) -> f32 {
        ((HOLD + FADE - self.lost) / FADE).clamp(0.0, 1.0)
    }

    /// The hand is not tracked.
    pub fn lost(&self) -> bool {
        self.lost > 0.0
    }

    /// The width it is drawn at now, metres.
    pub fn shown_width(&self) -> f32 {
        (self.width * self.scale()).max(HIDDEN)
    }
}

/// Where a floating screen goes when the docked one at `pose` is torn off
/// into the room: the yaw and pitch (degrees, as a video's settings have
/// them, left and up positive) of its direction from the room's frame
/// `anchor`, and how far away, metres.
pub fn tear_off(pose: Mat4, anchor: Mat4) -> (f32, f32, f32) {
    let (_, rot, origin) = anchor.to_scale_rotation_translation();
    let to = pose.w_axis.truncate() - origin;
    let d = rot.inverse() * to;
    let yaw = (-d.x).atan2(-d.z).to_degrees();
    let pitch = d.y.atan2(d.x.hypot(d.z)).to_degrees();
    (
        yaw,
        pitch,
        (to.length() * TEAR_DISTANCE).clamp(TEAR_NEAR, TEAR_FAR),
    )
}

/// Where a floating screen with these settings is: the middle of it.
#[cfg(test)]
fn floating_centre(anchor: Mat4, yaw: f32, pitch: f32, distance: f32) -> Vec3 {
    let picture = crate::world::turned_with_picture(anchor, yaw, pitch);
    picture.transform_point3(Vec3::new(0.0, 0.0, -distance))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Vec3, b: Vec3, within: f32) -> bool {
        (a - b).length() < within
    }

    fn aim(pos: Vec3, yaw: f32, pitch: f32, roll: f32) -> (Vec3, Quat) {
        let rot = Quat::from_rotation_y(yaw.to_radians())
            * Quat::from_rotation_x(pitch.to_radians())
            * Quat::from_rotation_z(roll.to_radians());
        (pos, rot)
    }

    #[test]
    fn a_docked_screen_rides_the_hand() {
        let mut dock = Dock::new(0);
        for (i, pos) in [
            Vec3::new(-0.2, 1.1, -0.3),
            Vec3::new(-0.25, 1.3, -0.5),
            Vec3::new(0.1, 0.9, -0.2),
        ]
        .into_iter()
        .enumerate()
        {
            let hand = aim(pos, 10.0 * i as f32, 15.0, 0.0);
            dock.track(Some(hand), 1.0 / 72.0);
            // Its middle is the offset from the hand, in the hand's own turn.
            let want = pos + upright(hand.1) * OFFSET;
            let centre = dock.pose().transform_point3(Vec3::ZERO);
            assert!(near(centre, want, 1e-4), "{centre} vs {want}");
        }
        // Moving the hand moves the screen the same way.
        let a = aim(Vec3::new(0.0, 1.0, -0.4), 0.0, 0.0, 0.0);
        let b = aim(Vec3::new(0.3, 1.2, -0.4), 0.0, 0.0, 0.0);
        let before = docked_pose(a).w_axis.truncate();
        let after = docked_pose(b).w_axis.truncate();
        assert!(near(after - before, b.0 - a.0, 1e-4));
    }

    #[test]
    fn it_faces_the_face_and_stays_level_however_the_hand_rolls() {
        for roll in [0.0, 30.0, -45.0, 90.0] {
            let pose = docked_pose(aim(Vec3::new(0.0, 1.0, -0.3), 20.0, 5.0, roll));
            // Its width runs level: the roll is taken out.
            assert!(pose.x_axis.y.abs() < 1e-4, "roll {roll}: {}", pose.x_axis);
            // Its front faces up and back towards the user, not away.
            let facing = pose.z_axis;
            assert!(facing.y > 0.5, "{facing}");
        }
        // Pointing straight up there is no level to find, and it still works.
        let up = docked_pose((Vec3::ZERO, Quat::from_rotation_x(90f32.to_radians())));
        assert!(up.to_cols_array().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_lost_hand_holds_then_hides() {
        let mut dock = Dock::new(1);
        let hand = aim(Vec3::new(0.2, 1.1, -0.3), 0.0, 0.0, 0.0);
        dock.track(Some(hand), 0.01);
        let held = dock.pose();
        assert_eq!(dock.scale(), 1.0);
        assert!(!dock.lost());
        // Lost for most of a second: where it was, all of it.
        for _ in 0..9 {
            dock.track(None, 0.1);
        }
        assert!(dock.lost());
        assert_eq!(dock.pose(), held);
        assert_eq!(dock.scale(), 1.0);
        // Then it shrinks, still where it was, and then it is gone.
        dock.track(None, 0.3);
        let shrinking = dock.scale();
        assert!(shrinking > 0.0 && shrinking < 1.0, "{shrinking}");
        assert_eq!(dock.pose(), held);
        for _ in 0..10 {
            dock.track(None, 0.1);
        }
        assert_eq!(dock.scale(), 0.0);
        assert!(dock.shown_width() < 0.01);
        // Tracked again, it is back at the hand at once.
        let moved = aim(Vec3::new(0.3, 1.0, -0.2), 5.0, 0.0, 0.0);
        dock.track(Some(moved), 0.01);
        assert_eq!(dock.scale(), 1.0);
        assert!((dock.shown_width() - WIDTH).abs() < 1e-6);
        assert!(near(
            dock.pose().w_axis.truncate(),
            docked_pose(moved).w_axis.truncate(),
            1e-6
        ));
    }

    #[test]
    fn a_new_dock_is_hidden_until_its_hand_is_seen() {
        let mut dock = Dock::new(0);
        assert_eq!(dock.scale(), 0.0);
        dock.track(Some(aim(Vec3::ZERO, 0.0, 0.0, 0.0)), 0.01);
        assert_eq!(dock.scale(), 1.0);
    }

    #[test]
    fn tearing_off_places_it_where_let_go() {
        // A room turned a little, you standing off its origin.
        let anchor = Mat4::from_rotation_translation(
            Quat::from_rotation_y(25f32.to_radians()),
            Vec3::new(0.3, 1.5, 0.2),
        );
        for hand in [
            aim(Vec3::new(-0.2, 1.2, -0.3), 0.0, 0.0, 0.0),
            aim(Vec3::new(0.4, 1.4, -0.6), 30.0, 10.0, 20.0),
        ] {
            let pose = docked_pose(hand);
            let (yaw, pitch, distance) = tear_off(pose, anchor);
            let centre = pose.w_axis.truncate();
            let origin = anchor.w_axis.truncate();
            let floating = floating_centre(anchor, yaw, pitch, distance);
            // The floating screen is in the same direction as the docked one
            // was, further along it.
            let was = (centre - origin).normalize();
            let now = (floating - origin).normalize();
            assert!(near(was, now, 1e-3), "{was} vs {now}");
            assert!((1.0..=20.0).contains(&distance), "{distance}");
            assert!(distance >= (centre - origin).length());
        }
        // Close to you it floats at least a metre out; far, no more than 20.
        let close = tear_off(
            Mat4::from_translation(Vec3::new(0.0, 1.5, -0.1)),
            Mat4::from_translation(Vec3::new(0.0, 1.5, 0.0)),
        );
        assert_eq!(close.2, 1.0);
        let far = tear_off(
            Mat4::from_translation(Vec3::new(0.0, 0.0, -9.0)),
            Mat4::IDENTITY,
        );
        assert_eq!(far.2, 20.0);
        // Straight ahead is yaw and pitch zero.
        let ahead = tear_off(
            Mat4::from_translation(Vec3::new(0.0, 0.0, -0.5)),
            Mat4::IDENTITY,
        );
        assert!(ahead.0.abs() < 1e-4 && ahead.1.abs() < 1e-4);
        assert!((ahead.2 - 2.0).abs() < 1e-4);
    }

    #[test]
    fn the_controls_frame_puts_the_screen_the_eyes_distance_ahead() {
        let pose = docked_pose(aim(Vec3::new(0.0, 1.0, -0.3), 40.0, 0.0, 0.0));
        let frame = picture(pose);
        let at = frame.inverse().transform_point3(pose.w_axis.truncate());
        assert!(near(at, Vec3::new(0.0, 0.0, -EYE), 1e-4), "{at}");
    }
}
