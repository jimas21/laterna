//! Controller input to commands. What each button and thumbstick axis does
//! comes from [`Bindings`] (remappable in Settings > Controller); the
//! defaults:
//!
//! | Input | Right hand | Left hand |
//! |---|---|---|
//! | Thumbstick left / right | Seek back / forward (repeats while held) | the same |
//! | Thumbstick up / down | Volume | Tilt the picture |
//! | Thumbstick press | Mute | Reset the picture |
//! | A / D-pad down | Play / pause | Play / pause |
//! | B / D-pad right | Back | Your room on / off |
//! | X / D-pad left | Previous video | Adjust panel |
//! | Y / D-pad up | Next video | Show or hide the controls |
//! | Menu / View | Library | Recenter |
//! | Bumper | Forward 1 minute | Back 1 minute |
//! | Grip + thumbstick | Up / down raises or lowers you | Left / right turns the picture, up / down zooms |
//! | Grip + X / Y | Previous / next chapter | As without the grip |
//! | Grip + A | Bookmark | As without the grip |
//! | Grip + thumbstick press | Subtitles on / off | As without the grip |
//!
//! With a hand's grip held, a button runs its grip action, or its own when
//! it has none. Fixed: the trigger clicks (on empty space it shows or hides the
//! controls), grip + trigger drags the picture (turning with the hand, and
//! nearer or further as the hand comes towards or away from the head), both
//! grips recenter, and a thumbstick pointed at a list pages it left / right
//! (at a slider, steps it).

use crate::bindings::{Axis, AxisAction, Bindings, Button, ButtonAction};
use fp_xr::Hand;
use glam::{Quat, Vec3};
use std::ops::RangeInclusive;

/// What the bindings asked for this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    TogglePause,
    Back,
    Menu,
    /// -1 back, +1 forward, by the seek step.
    Seek(i32),
    /// -1 back, +1 forward, by a minute.
    Skip(i32),
    /// Picture pitch change (degrees this frame; up positive).
    Pitch(f32),
    /// Picture yaw change (degrees this frame; left positive).
    Yaw(f32),
    /// Zoom change (factor delta this frame).
    Zoom(f32),
    /// Viewer height change (metres this frame; up positive).
    Height(f32),
    /// Volume change (this frame; 1.0 is full).
    Volume(f32),
    ToggleMute,
    Next,
    Previous,
    /// -1 the previous chapter or bookmark, +1 the next.
    Chapter(i32),
    Bookmark,
    ToggleSubtitles,
    ResetImage,
    DragBegin,
    /// Where the hand is since the drag began, smoothed: degrees (yaw
    /// left-positive, pitch up-positive) and metres the hand is further from
    /// the head (`distance`; 0 when the head isn't tracked). Scale
    /// `distance` with [`pushed_distance`].
    DragTo {
        yaw: f32,
        pitch: f32,
        distance: f32,
    },
    DragEnd,
    Recenter,
    ToggleUi,
    ToggleAdjust,
    TogglePassthrough,
    /// Page the pointed-at list, or step the slider: -1 back, +1 forward.
    Page(i32),
}

/// What the app knows that changes how input is read.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context {
    /// The active hand points at a panel.
    pub over_ui: bool,
    /// The active hand pulled the trigger while pointing at nothing.
    pub click_outside: bool,
    /// A video is open.
    pub playing: bool,
    /// Where the head is, in the same space as the hands (for dragging in and
    /// out).
    pub head: Option<Vec3>,
    pub dt: f32,
}

/// How fast a thumbstick tilts or turns the picture, in degrees a second.
const PITCH_SPEED: f32 = 30.0;
const YAW_SPEED: f32 = 45.0;
/// How fast a thumbstick raises or lowers the viewer, metres a second.
const HEIGHT_SPEED: f32 = 0.4;
/// Volume change a second at full stick.
const VOLUME_SPEED: f32 = 0.6;
pub(crate) const GRIP: f32 = 0.7;
const TRIGGER: f32 = 0.7;
const STICK: f32 = 0.75;
const STICK_RELEASE: f32 = 0.35;
/// Stick deflection before a continuous action (tilt, volume...) starts.
const STICK_HOLD: f32 = 0.5;
const REPEAT_DELAY: f32 = 0.5;
const REPEAT_EVERY: f32 = 0.35;

/// How closely a dragged thing trails the hand: with this time constant
/// (seconds) it has covered about a quarter of a step after one time constant,
/// nine tenths after four (see [`Damped`]). Short enough
/// to follow at once, long enough that a hand's tremor, which is magnified at
/// a panel's distance, doesn't show.
pub(crate) const DRAG_TAU: f32 = 0.05;

/// Metres a dragged thing goes in or out for each metre the hand does.
pub(crate) const DEPTH_GAIN: f32 = 2.0;

/// How far `hand` is from `head`, metres.
pub(crate) fn reach(head: Vec3, hand: Vec3) -> f32 {
    (hand - head).length()
}

/// Where a dragged thing `start` metres away goes when the hand moves
/// `hand_moved` metres further from the head (negative: nearer), kept within
/// `range` metres.
pub(crate) fn pushed_distance(start: f32, hand_moved: f32, range: RangeInclusive<f32>) -> f32 {
    (start + DEPTH_GAIN * hand_moved).clamp(*range.start(), *range.end())
}

/// Values that follow a target critically damped (no overshoot, the quickest
/// way there without it). Each step solves the motion exactly for the time
/// that passed, so it feels the same at 72 Hz as at 120 Hz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Damped<const N: usize> {
    value: [f32; N],
    velocity: [f32; N],
}

impl<const N: usize> Damped<N> {
    /// Holding still at `value`.
    pub fn at(value: [f32; N]) -> Self {
        Damped {
            value,
            velocity: [0.0; N],
        }
    }

    /// Moves `dt` seconds towards `target` with time constant `tau`, and
    /// returns where it is.
    pub fn step(&mut self, target: [f32; N], tau: f32, dt: f32) -> [f32; N] {
        if tau <= 0.0 {
            *self = Damped::at(target);
            return self.value;
        }
        let dt = dt.max(0.0);
        let w = 1.0 / tau;
        let decay = (-w * dt).exp();
        for ((x, v), to) in self.value.iter_mut().zip(&mut self.velocity).zip(target) {
            let gap = *x - to;
            let carried = (*v + w * gap) * dt;
            *v = (*v - w * carried) * decay;
            *x = to + (gap + carried) * decay;
        }
        self.value
    }
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    hand: usize,
    start: (f32, f32),
    /// How far the hand was from the head when the drag began.
    reach: f32,
    /// The picture's yaw, pitch and distance deltas, following the hand.
    follow: Damped<3>,
}

#[derive(Default)]
pub struct Controls {
    prev: [Hand; 2],
    /// Per hand and axis: the direction held (seeking or paging) and for how long.
    held: [[(i32, f32); 4]; 2],
    drag: Option<Drag>,
}

/// Yaw and pitch, degrees, of a controller's pointing direction in the
/// anchor's frame (-Z forward).
pub(crate) fn angles(aim: Quat, anchor: Quat) -> (f32, f32) {
    let d = (anchor.inverse() * aim * Vec3::NEG_Z).normalize_or_zero();
    (
        (-d.x).atan2(-d.z).to_degrees(),
        d.y.clamp(-1.0, 1.0).asin().to_degrees(),
    )
}

pub(crate) fn wrap(deg: f32) -> f32 {
    (deg + 180.0).rem_euclid(360.0) - 180.0
}

impl Controls {
    pub fn update(
        &mut self,
        hands: &[Hand; 2],
        anchor: Quat,
        ctx: Context,
        bindings: &Bindings,
    ) -> Vec<Cmd> {
        let mut out = Vec::new();
        let prev = self.prev;
        let gripping = |h: &Hand| h.squeeze > GRIP;

        // Dome drag: grip held, then trigger, on a hand not pointing at a panel.
        if let Some(d) = &mut self.drag {
            let h = &hands[d.hand];
            if gripping(h) && h.trigger > TRIGGER * 0.5 {
                if let Some((pos, rot)) = h.aim {
                    let (yaw, pitch) = angles(rot, anchor);
                    let moved = ctx.head.map_or(0.0, |head| reach(head, pos) - d.reach);
                    let [yaw, pitch, distance] = d.follow.step(
                        [wrap(yaw - d.start.0), pitch - d.start.1, moved],
                        DRAG_TAU,
                        ctx.dt,
                    );
                    out.push(Cmd::DragTo {
                        yaw,
                        pitch,
                        distance,
                    });
                }
            } else {
                self.drag = None;
                out.push(Cmd::DragEnd);
            }
        } else if ctx.playing && !ctx.over_ui {
            for i in 0..2 {
                let h = &hands[i];
                if gripping(h)
                    && h.trigger > TRIGGER
                    && prev[i].trigger <= TRIGGER
                    && let Some((pos, rot)) = h.aim
                {
                    self.drag = Some(Drag {
                        hand: i,
                        start: angles(rot, anchor),
                        reach: ctx.head.map_or(0.0, |head| reach(head, pos)),
                        follow: Damped::at([0.0; 3]),
                    });
                    out.push(Cmd::DragBegin);
                    break;
                }
            }
        }
        let any_grip = hands.iter().any(gripping);

        let playing = ctx.playing;
        for (i, hand) in hands.iter().enumerate() {
            let map = bindings.hand(i);
            for b in Button::ALL {
                if !b.pressed(hand) || b.pressed(&prev[i]) {
                    continue;
                }
                let action = match map.grip.button(b) {
                    a if gripping(hand) && a != ButtonAction::Nothing => a,
                    _ => map.button(b),
                };
                if let Some(cmd) = button_cmd(action, playing) {
                    out.push(cmd);
                }
            }

            let s = hand.stick;
            let grip = gripping(hand);
            if !grip && (!playing || ctx.over_ui) {
                // Menus: left / right pages the list pointed at, or steps the
                // slider, repeating while held (up / down scrolls, through
                // the pointer).
                if let Some(dir) = self.step(i, Axis::StickX, s.x, ctx.dt, true) {
                    out.push(Cmd::Page(dir));
                }
                continue;
            }
            let (ax, ay, other) = if grip {
                (Axis::GripX, Axis::GripY, [Axis::StickX, Axis::StickY])
            } else {
                (Axis::StickX, Axis::StickY, [Axis::GripX, Axis::GripY])
            };
            for a in other {
                self.held[i][a.index()] = (0, 0.0);
            }
            for (axis, v, cross) in [(ax, s.x, s.y), (ay, s.y, s.x)] {
                match map.axis(axis) {
                    AxisAction::Nothing => {}
                    AxisAction::Seek => {
                        if let Some(dir) = self.step(i, axis, v, ctx.dt, true) {
                            out.push(Cmd::Seek(dir));
                        }
                    }
                    // Continuous: only along the axis the stick mostly points.
                    action if v.abs() > STICK_HOLD && cross.abs() < v.abs() => {
                        let d = v * ctx.dt;
                        out.push(match action {
                            AxisAction::Volume => Cmd::Volume(d * VOLUME_SPEED),
                            AxisAction::Tilt => Cmd::Pitch(d * PITCH_SPEED),
                            AxisAction::Turn => Cmd::Yaw(-d * YAW_SPEED),
                            AxisAction::Height => Cmd::Height(d * HEIGHT_SPEED),
                            // Down zooms in, as in DeoVR.
                            _ => Cmd::Zoom(-d * 0.8),
                        });
                    }
                    _ => {}
                }
            }
        }

        let both = |h: &[Hand; 2]| h.iter().all(|h| h.squeeze > 0.8 && h.trigger < 0.3);
        if both(hands) && !both(&prev) && self.drag.is_none() {
            out.push(Cmd::Recenter);
        }
        if ctx.click_outside && !any_grip && ctx.playing {
            out.push(Cmd::ToggleUi);
        }
        self.prev = *hands;
        out
    }

    /// Edge-triggered stick direction on `axis` of hand `i`: a push fires
    /// once, and with `repeat` again while held (after a delay).
    fn step(&mut self, i: usize, axis: Axis, v: f32, dt: f32, repeat: bool) -> Option<i32> {
        let held = &mut self.held[i][axis.index()];
        let dir = if v > STICK {
            1
        } else if v < -STICK {
            -1
        } else if v.abs() < STICK_RELEASE {
            0
        } else {
            held.0
        };
        let (was, t) = *held;
        if dir == 0 {
            *held = (0, 0.0);
            return None;
        }
        if dir != was {
            *held = (dir, 0.0);
            return Some(dir);
        }
        let t2 = t + dt;
        *held = (dir, t2);
        // Fire each time the hold crosses delay + k * interval.
        let fires = |t: f32| {
            if t < REPEAT_DELAY {
                -1.0
            } else {
                ((t - REPEAT_DELAY) / REPEAT_EVERY).floor()
            }
        };
        (repeat && fires(t2) > fires(t)).then_some(dir)
    }
}

/// The command for a pressed button, if it does anything now.
fn button_cmd(action: ButtonAction, playing: bool) -> Option<Cmd> {
    use ButtonAction as B;
    let cmd = match action {
        B::Nothing => return None,
        B::Back => Cmd::Back,
        B::Library => Cmd::Menu,
        B::Passthrough => Cmd::TogglePassthrough,
        B::Recenter => Cmd::Recenter,
        _ if !playing => return None,
        B::PlayPause => Cmd::TogglePause,
        B::PreviousVideo => Cmd::Previous,
        B::NextVideo => Cmd::Next,
        B::SeekBack => Cmd::Seek(-1),
        B::SeekForward => Cmd::Seek(1),
        B::BackOneMinute => Cmd::Skip(-1),
        B::ForwardOneMinute => Cmd::Skip(1),
        B::Mute => Cmd::ToggleMute,
        B::ResetPicture => Cmd::ResetImage,
        B::ShowHideControls => Cmd::ToggleUi,
        B::AdjustPanel => Cmd::ToggleAdjust,
        B::PreviousChapter => Cmd::Chapter(-1),
        B::NextChapter => Cmd::Chapter(1),
        B::Bookmark => Cmd::Bookmark,
        B::Subtitles => Cmd::ToggleSubtitles,
    };
    Some(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{LEFT, RIGHT};
    use glam::Vec2;

    fn ctx(playing: bool) -> Context {
        Context {
            playing,
            dt: 0.1,
            ..Default::default()
        }
    }

    fn hands() -> [Hand; 2] {
        let mut h = [Hand::default(), Hand::default()];
        for x in &mut h {
            x.active = true;
            x.aim = Some((Vec3::ZERO, Quat::IDENTITY));
        }
        h
    }

    fn run(c: &mut Controls, h: &[Hand; 2], cx: Context) -> Vec<Cmd> {
        c.update(h, Quat::IDENTITY, cx, &Bindings::default())
    }

    /// Presses and releases one button, returning what the press did.
    fn press(hand: usize, set: fn(&mut Hand, bool), playing: bool) -> Vec<Cmd> {
        let mut c = Controls::default();
        let mut h = hands();
        set(&mut h[hand], true);
        let out = run(&mut c, &h, ctx(playing));
        assert_eq!(run(&mut c, &h, ctx(playing)), vec![], "edge only");
        set(&mut h[hand], false);
        run(&mut c, &h, ctx(playing));
        out
    }

    #[test]
    fn every_button_has_one_job() {
        type Set = fn(&mut Hand, bool);
        let cases: [(usize, Set, Cmd); 14] = [
            (RIGHT, |h, v| h.south = v, Cmd::TogglePause),
            (LEFT, |h, v| h.south = v, Cmd::TogglePause),
            (RIGHT, |h, v| h.east = v, Cmd::Back),
            (RIGHT, |h, v| h.west = v, Cmd::Previous),
            (RIGHT, |h, v| h.north = v, Cmd::Next),
            (RIGHT, |h, v| h.menu = v, Cmd::Menu),
            (RIGHT, |h, v| h.shoulder = v, Cmd::Skip(1)),
            (RIGHT, |h, v| h.stick_click = v, Cmd::ToggleMute),
            (LEFT, |h, v| h.north = v, Cmd::ToggleUi),
            (LEFT, |h, v| h.west = v, Cmd::ToggleAdjust),
            (LEFT, |h, v| h.east = v, Cmd::TogglePassthrough),
            (LEFT, |h, v| h.menu = v, Cmd::Recenter),
            (LEFT, |h, v| h.shoulder = v, Cmd::Skip(-1)),
            (LEFT, |h, v| h.stick_click = v, Cmd::ResetImage),
        ];
        for (hand, set, want) in cases {
            assert_eq!(press(hand, set, true), vec![want], "{want:?}");
        }
        // In the library only navigation works.
        assert_eq!(press(RIGHT, |h, v| h.east = v, false), vec![Cmd::Back]);
        assert_eq!(press(RIGHT, |h, v| h.menu = v, false), vec![Cmd::Menu]);
        assert_eq!(press(RIGHT, |h, v| h.south = v, false), vec![]);
        assert_eq!(press(LEFT, |h, v| h.shoulder = v, false), vec![]);
        assert_eq!(
            press(LEFT, |h, v| h.east = v, false),
            vec![Cmd::TogglePassthrough],
            "passthrough works in the library too"
        );
    }

    #[test]
    fn a_held_grip_gives_a_button_its_grip_action() {
        type Set = fn(&mut Hand, bool);
        let cases: [(Set, Cmd); 5] = [
            (|h, v| (h.squeeze, h.north) = (1.0, v), Cmd::Chapter(1)),
            (|h, v| (h.squeeze, h.west) = (1.0, v), Cmd::Chapter(-1)),
            (|h, v| (h.squeeze, h.south) = (1.0, v), Cmd::Bookmark),
            (
                |h, v| (h.squeeze, h.stick_click) = (1.0, v),
                Cmd::ToggleSubtitles,
            ),
            // None of its own: what it does alone.
            (|h, v| (h.squeeze, h.east) = (1.0, v), Cmd::Back),
        ];
        for (set, want) in cases {
            assert_eq!(press(RIGHT, set, true), vec![want], "{want:?}");
        }
        // The other hand's grip doesn't count.
        let mut c = Controls::default();
        let mut h = hands();
        h[LEFT].squeeze = 1.0;
        h[RIGHT].north = true;
        assert_eq!(run(&mut c, &h, ctx(true)), vec![Cmd::Next]);
    }

    #[test]
    fn either_stick_seeks_and_repeats_while_held() {
        for hand in [LEFT, RIGHT] {
            let mut c = Controls::default();
            let mut h = hands();
            h[hand].stick = Vec2::new(0.9, 0.0);
            let mut seeks = 0;
            // 1.5 s held at 10 Hz: the first press, then repeats from 0.5 s every 0.35 s.
            for _ in 0..15 {
                seeks += run(&mut c, &h, ctx(true))
                    .iter()
                    .filter(|c| **c == Cmd::Seek(1))
                    .count();
            }
            assert_eq!(seeks, 1 + 3, "hand {hand}");
            h[hand].stick = Vec2::ZERO;
            run(&mut c, &h, ctx(true));
            h[hand].stick = Vec2::new(-0.9, 0.0);
            let over = Context {
                over_ui: true,
                ..ctx(true)
            };
            assert_eq!(run(&mut c, &h, over), vec![Cmd::Page(-1)]);
        }
    }

    #[test]
    fn right_stick_up_is_volume_left_is_tilt() {
        let mut c = Controls::default();
        let mut h = hands();
        h[RIGHT].stick = Vec2::new(0.0, 1.0);
        let up = run(&mut c, &h, ctx(true));
        assert!(matches!(up[..], [Cmd::Volume(d)] if d > 0.0), "{up:?}");
        h[RIGHT].stick = Vec2::ZERO;
        h[LEFT].stick = Vec2::new(0.0, -1.0);
        let down = run(&mut c, &h, ctx(true));
        assert!(matches!(down[..], [Cmd::Pitch(d)] if d < 0.0), "{down:?}");
        // Pointing at a menu, up/down scrolls it instead (through the pointer).
        let over = Context {
            over_ui: true,
            ..ctx(true)
        };
        assert_eq!(run(&mut c, &h, over), vec![]);
    }

    #[test]
    fn left_grip_and_stick_turn_and_zoom() {
        let mut c = Controls::default();
        let mut h = hands();
        h[LEFT].squeeze = 1.0;
        h[LEFT].stick = Vec2::new(0.9, 0.0);
        let turn = run(&mut c, &h, ctx(true));
        assert!(
            matches!(turn[..], [Cmd::Yaw(d)] if d < 0.0),
            "right turns it right: {turn:?}"
        );
        h[LEFT].stick = Vec2::new(0.0, -1.0);
        let z = run(&mut c, &h, ctx(true));
        assert!(
            matches!(z[..], [Cmd::Zoom(d)] if d > 0.0),
            "down zooms in: {z:?}"
        );
        // The right grip + stick: left / right does nothing (and does not
        // seek), up raises you.
        let mut h = hands();
        h[RIGHT].squeeze = 1.0;
        h[RIGHT].stick = Vec2::new(0.9, 0.0);
        assert_eq!(run(&mut Controls::default(), &h, ctx(true)), vec![]);
        h[RIGHT].stick = Vec2::new(0.0, 0.9);
        let up = run(&mut Controls::default(), &h, ctx(true));
        assert!(matches!(up[..], [Cmd::Height(d)] if d > 0.0), "{up:?}");
    }

    #[test]
    fn remapped_buttons_and_axes_follow_the_bindings() {
        let mut b = Bindings::default();
        b.right.south = ButtonAction::NextVideo;
        b.left.stick_x = AxisAction::Turn;
        b.right.stick_y = AxisAction::Nothing;
        let mut c = Controls::default();
        let mut h = hands();
        h[RIGHT].south = true;
        assert_eq!(c.update(&h, Quat::IDENTITY, ctx(true), &b), vec![Cmd::Next]);
        let mut h = hands();
        h[LEFT].stick = Vec2::new(-0.9, 0.0);
        h[RIGHT].stick = Vec2::new(0.0, 0.9);
        let out = c.update(&h, Quat::IDENTITY, ctx(true), &b);
        assert!(
            matches!(out[..], [Cmd::Yaw(d)] if d > 0.0),
            "left turns left: {out:?}"
        );
        // Pointed at a menu, the stick pages whatever it is bound to.
        let over = Context {
            over_ui: true,
            ..ctx(true)
        };
        let mut c = Controls::default();
        assert_eq!(c.update(&h, Quat::IDENTITY, over, &b), vec![Cmd::Page(-1)]);
    }

    #[test]
    fn grip_trigger_drags_the_dome() {
        let mut c = Controls::default();
        let mut h = hands();
        h[1].squeeze = 1.0;
        assert_eq!(run(&mut c, &h, ctx(true)), vec![]);
        h[1].trigger = 1.0;
        let click = Context {
            click_outside: true,
            ..ctx(true)
        };
        assert_eq!(
            run(&mut c, &h, click),
            vec![Cmd::DragBegin],
            "no UI toggle while gripping"
        );
        // Point 20 degrees to the right and 10 up.
        h[1].aim = Some((
            Vec3::ZERO,
            Quat::from_euler(
                glam::EulerRot::YXZ,
                (-20f32).to_radians(),
                10f32.to_radians(),
                0.0,
            ),
        ));
        // It follows the hand without overshooting, and settles on it.
        let mut last = (0.0, 0.0);
        for frame in 0..40 {
            match run(&mut c, &h, ctx(true))[..] {
                [Cmd::DragTo { yaw, pitch, .. }] => {
                    assert!((-20.01..=0.0).contains(&yaw), "{frame}: {yaw}");
                    assert!((0.0..=10.01).contains(&pitch), "{frame}: {pitch}");
                    if frame == 0 {
                        assert!(yaw > -19.0, "smoothed, not a jump: {yaw}");
                    }
                    last = (yaw, pitch);
                }
                ref o => panic!("{o:?}"),
            }
        }
        assert!(
            (last.0 + 20.0).abs() < 0.01 && (last.1 - 10.0).abs() < 0.01,
            "{last:?}"
        );
        h[1].trigger = 0.0;
        assert_eq!(run(&mut c, &h, ctx(true)), vec![Cmd::DragEnd]);
    }

    #[test]
    fn both_grips_recenter_and_empty_clicks_toggle() {
        let mut c = Controls::default();
        let mut h = hands();
        h[0].squeeze = 1.0;
        h[1].squeeze = 1.0;
        assert_eq!(run(&mut c, &h, ctx(false)), vec![Cmd::Recenter]);
        let mut h = hands();
        h[1].trigger = 1.0;
        let click = Context {
            click_outside: true,
            ..ctx(true)
        };
        assert_eq!(run(&mut c, &h, click), vec![Cmd::ToggleUi]);
    }

    /// A hand at `pos`, aiming straight ahead, with its grip and trigger held.
    fn dragging_at(pos: Vec3) -> [Hand; 2] {
        let mut h = hands();
        h[1].squeeze = 1.0;
        h[1].trigger = 1.0;
        h[1].aim = Some((pos, Quat::IDENTITY));
        h
    }

    #[test]
    fn a_hand_moving_from_the_head_drags_the_picture_out() {
        let head = Vec3::new(0.0, 1.6, 0.0);
        let cx = Context {
            head: Some(head),
            ..ctx(true)
        };
        let at = |z: f32| head + Vec3::new(0.0, -0.2, -z);
        // Holds the hand at `z` metres ahead of the head until the drag settles.
        let settle = |c: &mut Controls, h: &mut [Hand; 2], z: f32| {
            h[1].aim = Some((at(z), Quat::IDENTITY));
            let mut last = 0.0;
            for _ in 0..40 {
                if let [Cmd::DragTo { distance, .. }] = run(c, h, cx)[..] {
                    last = distance;
                }
            }
            last
        };
        let mut c = Controls::default();
        let mut h = dragging_at(at(0.3));
        assert_eq!(run(&mut c, &h, cx), vec![Cmd::DragBegin]);
        // Arm out 0.4 m further, then back in 0.2 m.
        let out = settle(&mut c, &mut h, 0.7);
        let reach_at = |z: f32| 0.2f32.hypot(z);
        let gap = reach_at(0.7) - reach_at(0.3);
        assert!((out - gap).abs() < 1e-3, "{out}");
        let back = settle(&mut c, &mut h, 0.5);
        assert!(back > 0.0 && back < out, "{back}");
        // Without a head there is no depth, only the turn.
        let mut c = Controls::default();
        let mut h = dragging_at(Vec3::new(0.0, 0.0, -0.3));
        run(&mut c, &h, ctx(true));
        h[1].aim = Some((Vec3::new(0.0, 0.0, -0.9), Quat::IDENTITY));
        let out = run(&mut c, &h, ctx(true));
        assert!(matches!(out[..], [Cmd::DragTo { distance: 0.0, .. }]));
    }

    #[test]
    fn pushing_the_hand_out_moves_the_picture_two_for_one_within_the_range() {
        let push = |moved: f32| pushed_distance(4.0, moved, 1.0..=20.0);
        assert_eq!(push(0.0), 4.0);
        assert_eq!(push(0.5), 4.0 + DEPTH_GAIN * 0.5);
        assert_eq!(push(-0.5), 4.0 - DEPTH_GAIN * 0.5);
        assert_eq!(push(-5.0), 1.0);
        assert_eq!(push(50.0), 20.0);
        assert_eq!(
            reach(Vec3::new(1.0, 1.0, 1.0), Vec3::new(1.0, 1.0, -2.0)),
            3.0
        );
    }

    #[test]
    fn a_damped_follower_settles_without_overshooting() {
        let mut d = Damped::at([0.0, 10.0]);
        // No time, no movement; no smoothing, straight there.
        assert_eq!(d.step([1.0, 0.0], DRAG_TAU, 0.0), [0.0, 10.0]);
        assert_eq!(Damped::at([0.0]).step([3.0], 0.0, 0.01), [3.0]);
        // One time constant covers 1 - 2/e of the way from rest.
        let mut d = Damped::at([0.0]);
        let x = d.step([1.0], DRAG_TAU, DRAG_TAU)[0];
        assert!((x - (1.0 - 2.0 / std::f32::consts::E)).abs() < 1e-5, "{x}");
        let mut last = x;
        for _ in 0..200 {
            let x = d.step([1.0], DRAG_TAU, 0.01)[0];
            assert!(x >= last - 1e-6 && x <= 1.0 + 1e-6, "monotone: {x}");
            last = x;
        }
        assert!((last - 1.0).abs() < 1e-4, "{last}");
    }

    #[test]
    fn the_follower_feels_the_same_at_any_frame_rate() {
        let mut slow = Damped::at([0.0, 0.0]);
        let mut fast = slow;
        // A moving target, sampled at 72 Hz and at 144 Hz.
        let target = |t: f32| [t.sin(), t * 0.5];
        let mut t = 0.0;
        for _ in 0..72 {
            slow.step(target(t), DRAG_TAU, 1.0 / 72.0);
            t += 1.0 / 72.0;
        }
        let (mut t, mut from_fast) = (0.0, [0.0; 2]);
        for _ in 0..144 {
            from_fast = fast.step(target(t), DRAG_TAU, 1.0 / 144.0);
            t += 1.0 / 144.0;
        }
        // Both end a second in, trailing the target by about the same.
        let from_slow = slow.value;
        for (s, f) in from_slow.into_iter().zip(from_fast) {
            assert!((s - f).abs() < 0.05, "{from_slow:?} {from_fast:?}");
        }
        // A constant target is exact: one big step equals many small ones.
        let (mut one, mut many) = (Damped::at([2.0]), Damped::at([2.0]));
        let a = one.step([-1.0], DRAG_TAU, 0.12)[0];
        let mut b = 0.0;
        for _ in 0..12 {
            b = many.step([-1.0], DRAG_TAU, 0.01)[0];
        }
        assert!((a - b).abs() < 1e-4, "{a} {b}");
    }

    #[test]
    fn tremor_is_smaller_after_following() {
        // A hand shaking +-1 at 10 Hz about a fixed point.
        let mut d = Damped::at([0.0]);
        let mut peak = 0.0f32;
        for frame in 0..720 {
            let t = frame as f32 / 144.0;
            let shake = (t * std::f32::consts::TAU * 10.0).sin();
            let x = d.step([shake], DRAG_TAU, 1.0 / 144.0)[0];
            if t > 1.0 {
                peak = peak.max(x.abs());
            }
        }
        assert!(peak < 0.25, "{peak}");
    }
}
