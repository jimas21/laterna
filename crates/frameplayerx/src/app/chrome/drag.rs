//! Moving and resizing the screen by its window controls, apart from the
//! app: pure maths, stepped once a frame.
//!
//! Moving carries the screen with the hand, as the Steam Frame's own windows
//! are carried: its middle is held where it was in front of the hand, so
//! turning the hand swings it as the laser turns and moving the hand moves
//! it too, while it keeps facing you. The hand coming nearer or going
//! further brings the screen in or sends it away, by a share of how far it
//! is (so a full reach covers most of the room, near or far), whether the
//! trigger or the grip holds it, and the thumbstick
//! pushes it away or pulls it near. Resizing keeps the screen's middle and
//! shape and scales it so its corner follows the laser.

use crate::controls::{DRAG_TAU, Damped, reach, reached_distance, wrap};
use crate::world::GrabBy;
use fp_core::view::SCREEN_DISTANCE;
use fp_render::ScreenShape;
use glam::{Mat4, Quat, Vec2, Vec3};
use std::ops::RangeInclusive;

/// How wide the screen may be made, metres: as zooming it with the left
/// thumbstick allows.
pub const WIDTH: RangeInclusive<f32> = 0.5..=30.0;

/// How wide and how far away a drag may make the screen, each as (least,
/// most). A floating screen has the ranges settings give it; a docked one
/// is held at one distance from the eye and is smaller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub width: (f32, f32),
    pub distance: (f32, f32),
}

impl Limits {
    /// A floating screen's.
    pub fn floating() -> Limits {
        Limits {
            width: (*WIDTH.start(), *WIDTH.end()),
            distance: (*SCREEN_DISTANCE.start(), *SCREEN_DISTANCE.end()),
        }
    }

    /// A docked screen's: its width within `width`, held `distance` away.
    pub fn docked(width: (f32, f32), distance: f32) -> Limits {
        Limits {
            width,
            distance: (distance, distance),
        }
    }
}

/// How fast the thumbstick, tilted all the way, pushes the screen away (or
/// pulls it near): its distance is multiplied by e to this power each second,
/// about three times as far after one.
const STICK_DEPTH: f32 = 1.1;

/// How far the thumbstick must tilt before it does anything.
const DEAD_ZONE: f32 = 0.2;

/// The distance `distance` metres, after `dt` seconds of the thumbstick held
/// at `stick_y` (up positive): further up, nearer down.
pub fn stick_depth(distance: f32, stick_y: f32, dt: f32) -> f32 {
    let tilt = stick_y.abs().min(1.0);
    if tilt <= DEAD_ZONE {
        return distance;
    }
    let push = stick_y.signum() * (tilt - DEAD_ZONE) / (1.0 - DEAD_ZONE);
    distance * (STICK_DEPTH * push * dt).exp()
}

/// Which window control was taken hold of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The bar under the screen.
    Move,
    /// The handle at its corner.
    Resize,
}

/// Where the screen is: turned `yaw` (left positive) and `pitch` (up
/// positive) degrees, `distance` metres away, `width` metres wide (its
/// longer side).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenView {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub width: f32,
}

/// What a drag begins from.
#[derive(Clone, Copy, Debug)]
pub struct Start {
    pub part: Part,
    pub hand: usize,
    pub by: GrabBy,
    /// The hand's pointing ray: where it is and which way it faces.
    pub aim: (Vec3, Quat),
    /// The room's frame (the anchor's turn), which yaw and pitch are in.
    pub room: Quat,
    /// The head, for how far the hand is from it.
    pub head: Option<Vec3>,
    /// The picture's frame in the world: the screen sits straight ahead of
    /// its origin.
    pub picture: Mat4,
    pub view: ScreenView,
    pub shape: ScreenShape,
    pub limits: Limits,
}

/// What a frame brings to a drag.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub aim: Option<(Vec3, Quat)>,
    /// The holding hand's thumbstick, up positive.
    pub stick_y: f32,
    pub head: Option<Vec3>,
    pub room: Quat,
    pub picture: Mat4,
    pub dt: f32,
}

/// Where the drag has put the screen, and whether it has just met a limit.
#[derive(Clone, Copy, Debug)]
pub struct Step {
    pub view: ScreenView,
    pub limit: bool,
}

/// A window control held: the screen as it was and how the hand has moved
/// since.
#[derive(Clone, Copy, Debug)]
pub struct ScreenDrag {
    pub part: Part,
    pub hand: usize,
    pub by: GrabBy,
    from: ScreenView,
    now: ScreenView,
    limits: Limits,
    /// The thumbstick's push on the distance, as a factor.
    gain: f32,
    /// Moving: where the screen's middle was in the hand's own frame when it
    /// took hold, the point the screen turns about (the picture's origin),
    /// the yaw and pitch of the carried middle then, how far the hand was
    /// from the head, and the yaw, pitch and reach it has moved by since,
    /// following the hand smoothly.
    carry: Vec3,
    origin: Vec3,
    start: (f32, f32),
    reach: Option<f32>,
    follow: Damped<3>,
    /// Resizing: how far the screen's corner stands in front of its middle
    /// (the curve's depth) as the screen was shaped when taken hold of, the
    /// direction from the screen's middle to its corner, how far along it the
    /// hand's ray met the screen's plane when it took hold, and the scale the
    /// hand has asked for since, following it smoothly. `limited`: the width
    /// is at a limit.
    curve: f32,
    chord: Vec2,
    grabbed: f32,
    scale: Damped<1>,
    limited: bool,
}

/// Where the ray from `aim` meets the plane `z` metres along the picture's
/// frame, as the distance right and up from its origin.
fn on_plane(picture: Mat4, aim: (Vec3, Quat), z: f32) -> Option<Vec2> {
    on_plane_ray(picture, aim.0, aim.1 * Vec3::NEG_Z, z)
}

/// The yaw and pitch, degrees in the room's frame, from `origin` to the
/// point `carry` holds in front of the hand at `aim`.
fn carried(aim: (Vec3, Quat), carry: Vec3, origin: Vec3, room: Quat) -> (f32, f32) {
    let point = aim.0 + aim.1 * carry;
    let d = (room.inverse() * (point - origin)).normalize_or_zero();
    (
        (-d.x).atan2(-d.z).to_degrees(),
        d.y.clamp(-1.0, 1.0).asin().to_degrees(),
    )
}

/// [`on_plane`] for a ray given as where it starts and which way it points.
pub(super) fn on_plane_ray(picture: Mat4, origin: Vec3, dir: Vec3, z: f32) -> Option<Vec2> {
    let inverse = picture.inverse();
    let origin = inverse.transform_point3(origin);
    let dir = inverse.transform_vector3(dir);
    // The plane is ahead (-Z): a ray along it or away from it never meets it.
    if dir.z > -1e-4 {
        return None;
    }
    let t = (z - origin.z) / dir.z;
    if t <= 0.0 {
        return None;
    }
    Some(Vec2::new(origin.x + t * dir.x, origin.y + t * dir.y))
}

impl ScreenDrag {
    /// Takes hold.
    pub fn begin(start: Start) -> ScreenDrag {
        let Start {
            part,
            hand,
            by,
            aim,
            room,
            head,
            picture,
            view,
            shape,
            limits,
        } = start;
        // The corner (the screen's bottom right) in the picture's frame,
        // away from the screen's middle.
        let corner = shape.point(1.0, -1.0);
        let chord = Vec2::new(corner.x, corner.y).normalize_or_zero();
        // Where the hand took hold, along it: not exactly on the corner, but
        // the screen must not jump to put the corner there.
        let curve = match shape.radius() {
            Some(r) => r * (1.0 - (shape.arc / 2.0).cos()),
            None => 0.0,
        };
        let grabbed = match on_plane(picture, aim, curve - view.distance) {
            Some(g) => g.dot(chord),
            None => 0.0,
        };
        // The screen's middle, held in the hand's frame from now on.
        let origin = picture.transform_point3(Vec3::ZERO);
        let middle = picture.transform_point3(Vec3::new(0.0, 0.0, -view.distance));
        let carry = aim.1.inverse() * (middle - aim.0);
        ScreenDrag {
            part,
            hand,
            by,
            from: view,
            now: view,
            limits,
            gain: 1.0,
            carry,
            origin,
            start: carried(aim, carry, origin, room),
            reach: head.map(|head| reach(head, aim.0)),
            follow: Damped::at([0.0; 3]),
            curve,
            chord,
            grabbed,
            scale: Damped::at([1.0]),
            limited: false,
        }
    }

    /// One frame on: where the hand is now, and so where the screen goes.
    pub fn step(&mut self, f: &Frame) -> Step {
        // The thumbstick pushes or pulls the screen, whatever else is held.
        self.gain = stick_depth(self.gain, f.stick_y, f.dt);
        let limit = match self.part {
            Part::Move => {
                self.moving(f);
                false
            }
            Part::Resize => self.resizing(f),
        };
        Step {
            view: self.now,
            limit,
        }
    }

    /// The distance `base` metres with the thumbstick's push on it, kept
    /// within the screen's range. (The push stops growing at a limit, so
    /// pushing on does not have to be undone first.)
    fn distance(&mut self, base: f32) -> f32 {
        let (near, far) = self.limits.distance;
        self.gain = self.gain.clamp(near / base, far / base);
        (base * self.gain).clamp(near, far)
    }

    fn moving(&mut self, f: &Frame) {
        let Some(aim) = f.aim else { return };
        let (yaw, pitch) = carried(aim, self.carry, self.origin, f.room);
        let moved = match (f.head, self.reach) {
            (Some(head), Some(r)) => reach(head, aim.0) - r,
            // Without the head (then or now) the depth holds where it is.
            _ => self.follow.value()[2],
        };
        let target = [wrap(yaw - self.start.0), pitch - self.start.1, moved];
        let [dy, dp, dz] = self.follow.step(target, DRAG_TAU, f.dt);
        let (near, far) = self.limits.distance;
        let base = reached_distance(self.from.distance, dz, near..=far);
        self.now = ScreenView {
            yaw: wrap(self.from.yaw + dy),
            pitch: (self.from.pitch + dp).clamp(-90.0, 90.0),
            distance: self.distance(base),
            width: self.from.width,
        };
    }

    /// Resizing: the screen's corner goes where the ray meets the screen's
    /// plane, kept on the same line from its middle, so a bigger screen is
    /// the same shape about the same middle. The plane is the one through the
    /// corner as it is now, so a curved screen's corner, which comes nearer as
    /// it widens, stays under the laser. Returns whether the width has just
    /// met a limit.
    fn resizing(&mut self, f: &Frame) -> bool {
        let base = self.from.distance;
        let distance = self.distance(base);
        // The curve's depth grows with the width setting from the shape it
        // began with. (The setting is the picture's longer side, not
        // always the arc's length, so it cannot give the depth itself.)
        let curve = if self.from.width > 1e-3 {
            self.curve * (self.now.width / self.from.width)
        } else {
            0.0
        };
        let mut width = self.now.width;
        let mut limit = false;
        let met = f
            .aim
            .and_then(|aim| on_plane(f.picture, aim, curve - distance));
        // A ray that misses the plane leaves the width where it was.
        if let Some(g) = met
            && self.grabbed > 1e-3
        {
            let ratio = g.dot(self.chord) / self.grabbed;
            let [scale] = self.scale.step([ratio], DRAG_TAU, f.dt);
            let wanted = self.from.width * scale;
            width = wanted.clamp(self.limits.width.0, self.limits.width.1);
            let at_limit = width != wanted;
            limit = at_limit && !self.limited;
            self.limited = at_limit;
        }
        self.now = ScreenView {
            distance,
            width,
            ..self.from
        };
        limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 72.0;

    fn near(a: f32, b: f32, within: f32) -> bool {
        (a - b).abs() < within
    }

    fn flat() -> ScreenShape {
        ScreenShape {
            width: 6.0,
            height: 3.375,
            arc: 0.0,
        }
    }

    fn view() -> ScreenView {
        ScreenView {
            yaw: 0.0,
            pitch: 0.0,
            distance: 4.0,
            width: 6.0,
        }
    }

    /// A hand at the eye pointing along `dir`.
    fn aim(dir: Vec3) -> (Vec3, Quat) {
        let rot = Quat::from_rotation_arc(Vec3::NEG_Z, dir.normalize());
        (Vec3::ZERO, rot)
    }

    fn start(part: Part, by: GrabBy, aim: (Vec3, Quat), shape: ScreenShape) -> Start {
        Start {
            part,
            hand: 1,
            by,
            aim,
            room: Quat::IDENTITY,
            head: Some(Vec3::ZERO),
            picture: Mat4::IDENTITY,
            view: view(),
            shape,
            limits: Limits::floating(),
        }
    }

    fn frame(aim: Option<(Vec3, Quat)>, stick_y: f32) -> Frame {
        Frame {
            aim,
            stick_y,
            head: Some(Vec3::ZERO),
            room: Quat::IDENTITY,
            picture: Mat4::IDENTITY,
            dt: DT,
        }
    }

    /// Runs a drag for `seconds` with the hand at `aim` and the stick at
    /// `stick_y`.
    fn hold(d: &mut ScreenDrag, aim: (Vec3, Quat), stick_y: f32, seconds: f32) -> Step {
        let mut step = d.step(&frame(Some(aim), stick_y));
        for _ in 1..(seconds / DT).round() as usize {
            step = d.step(&frame(Some(aim), stick_y));
        }
        step
    }

    #[test]
    fn moving_follows_the_laser() {
        let ahead = aim(Vec3::NEG_Z);
        let mut d = ScreenDrag::begin(start(Part::Move, GrabBy::Trigger, ahead, flat()));
        // Ten degrees to the left and five up.
        let left = Quat::from_rotation_y(10f32.to_radians());
        let up = Quat::from_rotation_x(5f32.to_radians());
        let step = hold(&mut d, (Vec3::ZERO, left * up), 0.0, 0.5);
        assert!(near(step.view.yaw, 10.0, 0.5), "{:?}", step.view);
        assert!(near(step.view.pitch, 5.0, 0.5), "{:?}", step.view);
        assert_eq!(step.view.distance, 4.0);
        assert_eq!(step.view.width, 6.0);
        // It does not jump when taken hold of: the first frame is at rest.
        let mut d = ScreenDrag::begin(start(Part::Move, GrabBy::Trigger, ahead, flat()));
        let first = d.step(&frame(Some(ahead), 0.0));
        assert_eq!(first.view, view());
    }

    #[test]
    fn trigger_moves_take_the_hand_coming_closer_too() {
        let ahead = (Vec3::new(0.0, 0.0, -0.4), Quat::IDENTITY);
        let mut d = ScreenDrag::begin(start(Part::Move, GrabBy::Trigger, ahead, flat()));
        // 0.1 m further: the screen e^0.4 times as far, straight ahead still.
        let further = (Vec3::new(0.0, 0.0, -0.5), Quat::IDENTITY);
        let step = hold(&mut d, further, 0.0, 1.0);
        assert!(
            near(step.view.distance, 4.0 * 0.4f32.exp(), 0.01),
            "{:?}",
            step.view
        );
        assert!(near(step.view.yaw, 0.0, 1e-3), "{:?}", step.view);
        assert!(near(step.view.pitch, 0.0, 1e-3), "{:?}", step.view);
    }

    #[test]
    fn moving_the_hand_without_turning_it_carries_the_screen() {
        for by in [GrabBy::Trigger, GrabBy::Grip] {
            let ahead = (Vec3::new(0.0, 0.0, -0.4), Quat::IDENTITY);
            let mut d = ScreenDrag::begin(start(Part::Move, by, ahead, flat()));
            // The hand goes 0.4 m right and 0.4 m up, pointing the same way:
            // the screen's middle, 3.6 m in front of it, goes with it.
            let moved = (Vec3::new(0.4, 0.4, -0.4), Quat::IDENTITY);
            let step = hold(&mut d, moved, 0.0, 1.0);
            let yaw = (-0.4f32).atan2(4.0).to_degrees();
            assert!(near(step.view.yaw, yaw, 0.1), "{by:?} {:?}", step.view);
            let pitch = (0.4f32 / (0.4f32 * 0.4 + 16.0f32 + 0.16).sqrt()).asin();
            assert!(
                near(step.view.pitch, pitch.to_degrees(), 0.1),
                "{by:?} {:?}",
                step.view
            );
        }
    }

    #[test]
    fn a_hand_away_from_the_eye_swings_the_screen_about_itself() {
        // Held 0.5 m to the right of the eye, pointing at the screen's
        // middle; turned 10 degrees left about the hand, the middle swings
        // round the hand, not round the eye.
        let hand = Vec3::new(0.5, 0.0, 0.0);
        let rot =
            Quat::from_rotation_arc(Vec3::NEG_Z, (Vec3::new(0.0, 0.0, -4.0) - hand).normalize());
        let mut d = ScreenDrag::begin(start(Part::Move, GrabBy::Trigger, (hand, rot), flat()));
        let turned = Quat::from_rotation_y(10f32.to_radians()) * rot;
        let step = hold(&mut d, (hand, turned), 0.0, 1.0);
        let reach = (Vec3::new(0.0, 0.0, -4.0) - hand).length();
        let middle = hand + turned * Vec3::NEG_Z * reach;
        let yaw = (-middle.x).atan2(-middle.z).to_degrees();
        assert!(near(step.view.yaw, yaw, 0.1), "{:?}", step.view);
        assert!(
            step.view.yaw > 8.0 && step.view.yaw < 12.0,
            "{:?}",
            step.view
        );
    }

    #[test]
    fn grip_moves_use_it() {
        // A hand 1.6 m from the head, which is a long way, so that it can
        // come in far enough to meet the nearest the screen may be.
        let ahead = (Vec3::new(0.0, 0.0, -1.6), Quat::IDENTITY);
        let mut d = ScreenDrag::begin(start(Part::Move, GrabBy::Grip, ahead, flat()));
        // The hand goes 0.1 m further from the head: the screen e^0.4 times.
        let further = (Vec3::new(0.0, 0.0, -1.7), Quat::IDENTITY);
        let step = hold(&mut d, further, 0.0, 1.0);
        assert!(
            near(step.view.distance, 4.0 * 0.4f32.exp(), 0.01),
            "{:?}",
            step.view
        );
        // Nearer comes in...
        let nearer = (Vec3::new(0.0, 0.0, -1.3), Quat::IDENTITY);
        let step = hold(&mut d, nearer, 0.0, 1.0);
        assert!(
            near(step.view.distance, 4.0 * (-1.2f32).exp(), 0.01),
            "{:?}",
            step.view
        );
        // A whole arm's reach out from there is far across the room.
        let out = (Vec3::new(0.0, 0.0, -1.9), Quat::IDENTITY);
        let step = hold(&mut d, out, 0.0, 1.0);
        assert!(
            step.view.distance > 4.0 * 1.1f32.exp() * 0.99,
            "{:?}",
            step.view
        );
        // ...and not past the nearest.
        let at_head = (Vec3::ZERO, Quat::IDENTITY);
        let step = hold(&mut d, at_head, 0.0, 1.0);
        assert!(near(step.view.distance, 1.0, 1e-4), "{:?}", step.view);
    }

    #[test]
    fn the_stick_pushes_and_pulls() {
        let ahead = aim(Vec3::NEG_Z);
        for part in [Part::Move, Part::Resize] {
            for by in [GrabBy::Trigger, GrabBy::Grip] {
                let mut d = ScreenDrag::begin(start(part, by, ahead, flat()));
                // Full up for a second at 4 m: e^1.1 times as far.
                let step = hold(&mut d, ahead, 1.0, 1.0);
                let ratio = step.view.distance / (4.0 * 1.1f32.exp());
                assert!(near(ratio, 1.0, 0.01), "{:?}", step.view);
                // On to the far limit, and no further.
                let step = hold(&mut d, ahead, 1.0, 3.0);
                assert_eq!(step.view.distance, 20.0);
                // Down again at once, however long it was pushed on.
                let step = hold(&mut d, ahead, -1.0, 1.0);
                assert!(step.view.distance < 20.0 / 2.9, "{:?}", step.view);
                let step = hold(&mut d, ahead, -1.0, 5.0);
                assert_eq!(step.view.distance, 1.0);
            }
        }
        // A light touch is the dead zone.
        let mut d = ScreenDrag::begin(start(Part::Move, GrabBy::Trigger, ahead, flat()));
        let step = hold(&mut d, ahead, 0.15, 2.0);
        assert_eq!(step.view.distance, 4.0);
        // Gently pushed, it goes gently.
        let step = hold(&mut d, ahead, 0.6, 1.0);
        let full = 4.0 * 1.1f32.exp();
        assert!(step.view.distance > 4.0 && step.view.distance < full * 0.8);
    }

    #[test]
    fn stick_depth_is_symmetric_and_has_a_dead_zone() {
        assert_eq!(stick_depth(4.0, 0.0, 1.0), 4.0);
        assert_eq!(stick_depth(4.0, -0.2, 1.0), 4.0);
        let up = stick_depth(4.0, 1.0, 1.0);
        let down = stick_depth(4.0, -1.0, 1.0);
        assert!(near(up * down, 16.0, 1e-3), "{up} {down}");
        assert!(near(up / 4.0, 1.1f32.exp(), 1e-3));
        // Past full tilt counts as full.
        assert_eq!(stick_depth(4.0, 3.0, 1.0), up);
    }

    /// A ray from the eye at the screen's corner (the bottom right, 4 m
    /// away) with its place on the screen's plane `times` as far out along
    /// the line from the middle.
    fn towards_corner(shape: &ScreenShape, times: f32) -> (Vec3, Quat) {
        let corner = Vec3::new(0.0, 0.0, -4.0) + shape.point(1.0, -1.0);
        aim(Vec3::new(corner.x * times, corner.y * times, corner.z))
    }

    #[test]
    fn resizing_keeps_centre_and_shape() {
        let s = flat();
        let corner = towards_corner(&s, 1.0);
        let mut d = ScreenDrag::begin(start(Part::Resize, GrabBy::Trigger, corner, s));
        let step = hold(&mut d, towards_corner(&s, 1.5), 0.0, 1.0);
        // The corner goes out half as far again: the screen is half as wide
        // again, about the same middle, and where it was in front of you.
        assert!(near(step.view.width, 9.0, 0.05), "{:?}", step.view);
        let pose = (step.view.yaw, step.view.pitch, step.view.distance);
        assert_eq!(pose, (0.0, 0.0, 4.0));
        // In: it shrinks.
        let step = hold(&mut d, towards_corner(&s, 0.5), 0.0, 1.0);
        assert!(near(step.view.width, 3.0, 0.05), "{:?}", step.view);
    }

    #[test]
    fn the_corner_follows_the_laser_on_a_curved_screen_too() {
        for curvature in [0.5f32, 1.0] {
            let s = ScreenShape {
                arc: curvature * std::f32::consts::TAU / 3.0,
                ..flat()
            };
            let corner = towards_corner(&s, 1.0);
            let mut d = ScreenDrag::begin(start(Part::Resize, GrabBy::Trigger, corner, s));
            let to = towards_corner(&s, 1.4);
            let step = hold(&mut d, to, 0.0, 2.0);
            assert!(step.view.width > 6.5, "{curvature}: {:?}", step.view);
            // The screen as it is now has its corner on the laser.
            let width = step.view.width;
            let now = ScreenShape {
                width,
                height: width * 3.375 / 6.0,
                ..s
            };
            let at = Vec3::new(0.0, 0.0, -4.0) + now.point(1.0, -1.0);
            let off = at.angle_between(to.1 * Vec3::NEG_Z).to_degrees();
            assert!(off < 0.7, "{curvature}: {off}");
        }
    }

    #[test]
    fn a_portrait_curved_screen_does_not_jump_on_its_first_frame() {
        // Taller than wide: the width setting (6 m) is the longer side, the
        // height, while the arc along the shape is 3.375 m.
        let s = ScreenShape {
            width: 3.375,
            height: 6.0,
            arc: 2.09,
        };
        let corner = towards_corner(&s, 1.0);
        let mut d = ScreenDrag::begin(start(Part::Resize, GrabBy::Trigger, corner, s));
        let first = d.step(&frame(Some(corner), 0.0));
        assert!(near(first.view.width, 6.0, 1e-3), "{:?}", first.view);
        // And holds still while the hand does.
        let step = hold(&mut d, corner, 0.0, 0.5);
        assert!(near(step.view.width, 6.0, 1e-3), "{:?}", step.view);
    }

    #[test]
    fn an_off_corner_grab_does_not_jump() {
        let s = flat();
        // Taken hold of a little in from the corner, and a little to one side.
        let off = aim(Vec3::new(2.7, -1.4, -4.0));
        let mut d = ScreenDrag::begin(start(Part::Resize, GrabBy::Trigger, off, s));
        let first = d.step(&frame(Some(off), 0.0));
        assert!(near(first.view.width, 6.0, 1e-3), "{:?}", first.view);
        // And moves in proportion from there: half as far again out.
        let out = aim(Vec3::new(2.7 * 1.5, -1.4 * 1.5, -4.0));
        let step = hold(&mut d, out, 0.0, 1.0);
        assert!(near(step.view.width, 9.0, 0.05), "{:?}", step.view);
    }

    #[test]
    fn resizing_clamps_and_survives_a_parallel_ray() {
        let s = flat();
        let corner = towards_corner(&s, 1.0);
        let mut d = ScreenDrag::begin(start(Part::Resize, GrabBy::Trigger, corner, s));
        // Far out: the widest, said once.
        let far = towards_corner(&s, 20.0);
        let mut said = 0;
        let mut step = d.step(&frame(Some(far), 0.0));
        for _ in 0..144 {
            said += step.limit as u32;
            step = d.step(&frame(Some(far), 0.0));
        }
        assert_eq!(step.view.width, 30.0);
        assert_eq!(said, 1, "told once");
        // A ray along the screen's plane, or away from it, holds the width.
        for dir in [Vec3::X, Vec3::Z, Vec3::new(1.0, 0.0, 0.00001)] {
            let step = hold(&mut d, aim(dir), 0.0, 0.2);
            assert_eq!(step.view.width, 30.0, "{dir}");
        }
        // Right in: the narrowest.
        let step = hold(&mut d, towards_corner(&s, 0.01), 0.0, 2.0);
        assert_eq!(step.view.width, 0.5);
        // The hand lost for a moment changes nothing.
        let step = d.step(&frame(None, 0.0));
        assert_eq!(step.view.width, 0.5);
    }

    #[test]
    fn a_docked_screen_resizes_within_its_own_range_and_is_not_pushed_away() {
        let s = ScreenShape {
            width: 0.3,
            height: 0.169,
            arc: 0.0,
        };
        let docked = ScreenView {
            yaw: 0.0,
            pitch: 0.0,
            distance: 0.5,
            width: 0.3,
        };
        let corner = Vec3::new(0.0, 0.0, -0.5) + s.point(1.0, -1.0);
        let towards = |times: f32| aim(Vec3::new(corner.x * times, corner.y * times, corner.z));
        let mut d = ScreenDrag::begin(Start {
            part: Part::Resize,
            hand: 1,
            by: GrabBy::Trigger,
            aim: towards(1.0),
            room: Quat::IDENTITY,
            head: Some(Vec3::ZERO),
            picture: Mat4::IDENTITY,
            view: docked,
            shape: s,
            limits: Limits::docked((0.12, 0.8), 0.5),
        });
        // Half as far again out: half as wide again, no nearer or further,
        // even with the stick pushed (its depth is the eye's, not the screen's).
        let step = hold(&mut d, towards(1.5), 1.0, 1.0);
        assert!(near(step.view.width, 0.45, 0.01), "{:?}", step.view);
        assert_eq!(step.view.distance, 0.5);
        // Far out and right in: its own widest and narrowest, not the floating
        // screen's.
        let step = hold(&mut d, towards(20.0), 0.0, 3.0);
        assert_eq!(step.view.width, 0.8);
        let step = hold(&mut d, towards(0.01), 0.0, 3.0);
        assert_eq!(step.view.width, 0.12);
    }
}
