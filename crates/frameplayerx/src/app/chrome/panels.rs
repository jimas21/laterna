//! Window controls on Laterna's own panels, as on the video screen: a bar
//! under the library, the adjust panel, the performance overlay and the
//! keyboard to move it by (hold the trigger on it, or a grip anywhere on the
//! panel as before), and a corner handle on the library to resize it.
//!
//! A bar shows only while a ray is near its panel (or holds it), so a view
//! with every panel up does not spend a layer on each. Placing is done in the
//! panel's own frame, in degrees as the eye sees them (the same table as the
//! screen's controls, `layout::LOOK`); [`resized`] is the resize's maths.

use super::drag::{on_plane_ray, stick_depth};
use super::layout::{LOOK, Spot, deg_to_m};
use super::*;
use crate::controls::DRAG_TAU;
use std::ops::RangeInclusive;

/// Where these come in the slice [`Pointer::route`] is given: after the
/// screen's own window controls. The four bars are, in order, the library
/// panel's, the adjust panel's, the performance overlay's and the
/// keyboard's; then the library's corner.
pub const PANEL_BAR_FIRST: usize = 8;
pub const PANEL_BARS: usize = 4;
pub const PANEL_CORNER: usize = PANEL_BAR_FIRST + PANEL_BARS;

/// The panel each bar moves (the keyboard's moves the panel it types into).
const BAR_TARGETS: [usize; PANEL_BARS] = [MAIN, ADJUST, PERF, KEYBOARD];

/// Their quad layers' keys, after the screen's window controls' (9, 10) and
/// the panels' own (0 to 5, 8).
pub const PANEL_LAYER_FIRST: u64 = 11;

/// How much bigger or smaller than usual the library panel may be made.
pub const MAIN_SCALE: RangeInclusive<f32> = 0.6..=1.8;

/// How near a panel a ray must come for its bar to show, degrees.
const NEAR: f32 = 3.0;

/// How far in front of its panel a control sits, metres, so that the two
/// never fight over which is nearer.
const FRONT: f32 = 0.004;

/// The part of a panel that shows (its body, without the shadow round it),
/// from the panel's middle, in metres: where its middle is and how far it
/// reaches each way from there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelBody {
    pub centre: Vec2,
    pub half: Vec2,
}

/// The move bar under a panel whose body is `body`, `distance` metres from
/// the eye, at Interface size `scale`: where it goes in the panel's frame.
pub fn bar_spot(body: PanelBody, distance: f32, scale: f32) -> Spot {
    let pill = deg_to_m(LOOK.pill.y * scale, distance);
    let gap = deg_to_m(LOOK.gap * scale, distance);
    let size = Vec2::new(
        deg_to_m(LOOK.bar_hit.x * scale, distance),
        deg_to_m(LOOK.bar_hit.y * scale, distance),
    );
    let y = body.centre.y - body.half.y - gap - pill / 2.0;
    Spot {
        pose: Mat4::from_translation(Vec3::new(body.centre.x, y, FRONT)),
        size,
    }
}

/// The resize handle at a panel body's bottom right corner: its square a
/// little out from the corner, so the ring hugs the corner from outside.
pub fn corner_spot(body: PanelBody, distance: f32, scale: f32) -> Spot {
    let out = deg_to_m(LOOK.corner_out * scale, distance);
    let side = deg_to_m(LOOK.corner_hit * scale, distance);
    let x = body.centre.x + body.half.x + out;
    let y = body.centre.y - body.half.y - out;
    Spot {
        pose: Mat4::from_translation(Vec3::new(x, y, FRONT)),
        size: Vec2::splat(side),
    }
}

/// Whether a ray comes within [`NEAR`] degrees of a panel (at `pose`, its body
/// `body`) or of the bar under it: where it meets the panel's plane is inside
/// the body grown by the margin and by the room below for the bar.
pub fn ray_near(
    pose: Mat4,
    body: PanelBody,
    distance: f32,
    scale: f32,
    origin: Vec3,
    dir: Vec3,
) -> bool {
    let Some(p) = on_plane_ray(pose, origin, dir, 0.0) else {
        return false;
    };
    let margin = deg_to_m(NEAR * scale, distance);
    let below = deg_to_m((LOOK.gap + LOOK.bar_hit.y) * scale, distance);
    let at = p - body.centre;
    at.x.abs() <= body.half.x + margin
        && at.y <= body.half.y + margin
        && at.y >= -(body.half.y + margin + below)
}

/// The scale a resize asks for: the scale it began at times how much further
/// along `k` (the unit direction from the panel's middle to its corner) the
/// hand's point on the panel's plane is than when it took hold (`grabbed`),
/// kept within [`MAIN_SCALE`]; and whether that met a limit. Grabbing a little
/// off the corner does not jump, and the panel's middle stays where it is.
pub fn resized(start: f32, grabbed: f32, k: Vec2, now: Vec2) -> (f32, bool) {
    if grabbed <= 1e-4 {
        return (start, false);
    }
    let wanted = start * now.dot(k) / grabbed;
    let (least, most) = (*MAIN_SCALE.start(), *MAIN_SCALE.end());
    let kept = wanted.clamp(least, most);
    (kept, (kept - wanted).abs() > 1e-4)
}

/// A library resize in progress: the hand, what took hold, the scale it began
/// at, the direction to the corner, how far along it the hand's point was on
/// the plane, and the scale following the hand smoothly.
#[derive(Clone, Copy)]
struct Resize {
    hand: usize,
    by: GrabBy,
    start: f32,
    k: Vec2,
    grabbed: f32,
    scale: Damped<1>,
}

/// The panels' window controls.
pub struct PanelChrome {
    pub bars: [Panel; PANEL_BARS],
    pub corner: Panel,
    resize: Option<Resize>,
    /// How each was last painted (the bars, then the corner).
    painted: [Option<HandleState>; PANEL_BARS + 1],
}

impl Default for PanelChrome {
    fn default() -> PanelChrome {
        let bar = || Panel::new([640, 144], Vec2::new(0.3, 0.07), 2.0).all_handle();
        let mut chrome = PanelChrome {
            bars: [bar(), bar(), bar(), bar()],
            corner: Panel::new([192, 192], Vec2::new(0.15, 0.15), 1.5).all_handle(),
            resize: None,
            painted: [None; PANEL_BARS + 1],
        };
        let in_eyes = std::env::var("FRAMEPLAYERX_CHROME_IN_EYES").is_ok_and(|v| v == "1");
        for p in chrome.bars.iter_mut().chain([&mut chrome.corner]) {
            p.in_eyes = in_eyes;
        }
        chrome
    }
}

impl PanelChrome {
    /// A library resize is in progress.
    pub fn resizing(&self) -> bool {
        self.resize.is_some()
    }

    /// Lets go of a resize (the panels were put back).
    pub fn cancel(&mut self) {
        self.resize = None;
    }

    /// Every control, in the order of the keys they are drawn under.
    pub fn all(&self) -> impl Iterator<Item = (u64, &Panel)> {
        self.bars
            .iter()
            .chain([&self.corner])
            .enumerate()
            .map(|(i, p)| (PANEL_LAYER_FIRST + i as u64, p))
    }

    /// Every control, to be faded.
    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut Panel> {
        self.bars.iter_mut().chain([&mut self.corner])
    }

    /// Repaints them all next frame.
    pub fn request_repaint(&mut self) {
        for p in self.all_mut() {
            p.request_repaint();
        }
    }
}

impl App {
    /// The panel a bar moves, as the app's panel (not the pointer's index).
    fn target_panel(&self, target: usize) -> &Panel {
        if target == PERF {
            &self.perf
        } else {
            &self.panels[target]
        }
    }

    /// Where `panel`'s body is: the library, adjust panel and keyboard float
    /// with room round them for a shadow; the overlay says what it shows.
    fn body_of(&self, target: usize) -> PanelBody {
        let panel = self.target_panel(target);
        let points = panel.points();
        let whole = egui::Rect::from_min_size(egui::Pos2::ZERO, points);
        let shown = match panel.solid {
            Some(solid) => solid,
            None if target == PERF => whole,
            None => whole - ui::widgets::SHADOW_ROOM,
        };
        let per_point = panel.size.x / points.x.max(1.0);
        let middle = shown.center() - whole.center();
        PanelBody {
            centre: Vec2::new(middle.x, -middle.y) * per_point,
            half: Vec2::new(shown.width(), shown.height()) * (per_point / 2.0),
        }
    }

    /// Sizes and puts the panels' window controls, and says which show; once
    /// after each placing of the panels. `eye` is where the head is.
    pub(in crate::app) fn place_panel_chrome(&mut self, eye: Vec3) {
        let world = self.offsets.scale_main.unwrap_or(1.0);
        let (least, most) = (*MAIN_SCALE.start(), *MAIN_SCALE.end());
        self.panels[MAIN].set_world_scale(if world.is_finite() {
            world.clamp(least, most)
        } else {
            1.0
        });
        let on = self.settings.window_controls;
        let (low, high) = (*UI_SCALES.start(), *UI_SCALES.end());
        let scale = self.settings.ui_scale.clamp(low, high);
        let held = self.panel_drag.and_then(|d| d.via);
        let hovered = self.pointer.hovered();
        let rays: Vec<(Vec3, Vec3)> = self
            .pointer
            .aims
            .iter()
            .flatten()
            .map(|a| (a.origin, a.dir))
            .collect();
        for (i, target) in BAR_TARGETS.into_iter().enumerate() {
            let panel = self.target_panel(target);
            let (pose, up) = (panel.pose, panel.visible);
            let body = self.body_of(target);
            let distance = (pose.w_axis.truncate() - eye).length().max(0.3);
            let near = target == KEYBOARD
                || rays
                    .iter()
                    .any(|&(o, d)| ray_near(pose, body, distance, scale, o, d));
            let shown =
                on && up && (near || held == Some(i) || hovered == Some(PANEL_BAR_FIRST + i));
            let spot = bar_spot(body, distance, scale);
            let bar = &mut self.panel_chrome.bars[i];
            bar.visible = shown;
            bar.pose = pose * spot.pose;
            bar.size = spot.size;
        }
        // The library's corner, to resize it by.
        let main = &self.panels[MAIN];
        let (pose, up) = (main.pose, main.visible);
        let body = self.body_of(MAIN);
        let distance = (pose.w_axis.truncate() - eye).length().max(0.3);
        let spot = corner_spot(body, distance, scale);
        let bar_up = self.panel_chrome.bars[0].visible;
        let corner = &mut self.panel_chrome.corner;
        corner.visible = on
            && up
            && (bar_up || self.panel_chrome.resize.is_some() || hovered == Some(PANEL_CORNER));
        corner.pose = pose * spot.pose;
        corner.size = spot.size;
    }

    /// A bar was taken hold of by a trigger: the panel it moves, if it moves
    /// one (the keyboard's moves nothing when no panel has the keyboard).
    pub(in crate::app) fn panel_bar_target(&self, pointer_index: usize) -> Option<(usize, usize)> {
        let slot = pointer_index.checked_sub(PANEL_BAR_FIRST)?;
        let target = *BAR_TARGETS.get(slot)?;
        Some((slot, target))
    }

    /// The library's corner was taken hold of.
    pub(in crate::app) fn begin_panel_resize(
        &mut self,
        grab: Grab,
        input: &FrameInput,
        buzz: &mut Vec<(usize, f32, i64)>,
    ) {
        if self.panel_chrome.resize.is_some() {
            return;
        }
        let Some((origin, rot)) = input.hands[grab.hand].aim else {
            return;
        };
        let body = self.body_of(MAIN);
        let corner = Vec2::new(body.centre.x + body.half.x, body.centre.y - body.half.y);
        let k = corner.normalize_or_zero();
        let Some(at) = on_plane_ray(self.panels[MAIN].pose, origin, rot * Vec3::NEG_Z, 0.0) else {
            return;
        };
        let grabbed = at.dot(k);
        if k == Vec2::ZERO || grabbed <= 0.02 {
            return;
        }
        let start = self.panels[MAIN].world_scale();
        self.panel_chrome.resize = Some(Resize {
            hand: grab.hand,
            by: grab.by,
            start,
            k,
            grabbed,
            scale: Damped::at([start]),
        });
        buzz.push((grab.hand, 0.3, 14));
    }

    /// Carries on a held resize for a frame, or lets it go.
    pub(in crate::app) fn update_panel_resize(
        &mut self,
        input: &FrameInput,
        buzz: &mut Vec<(usize, f32, i64)>,
    ) {
        let Some(mut r) = self.panel_chrome.resize else {
            return;
        };
        let hand = &input.hands[r.hand];
        let held = match r.by {
            GrabBy::Trigger => self.pointer.trigger_down(r.hand),
            GrabBy::Grip => hand.squeeze > GRIP * 0.6,
        };
        let Some((origin, rot)) = hand.aim.filter(|_| held && self.panels[MAIN].visible) else {
            self.panel_chrome.resize = None;
            self.settings.panel_offsets = self.offsets;
            return;
        };
        if let Some(now) = on_plane_ray(self.panels[MAIN].pose, origin, rot * Vec3::NEG_Z, 0.0) {
            let (wanted, limit) = resized(r.start, r.grabbed, r.k, now);
            let [scale] = r.scale.step([wanted], DRAG_TAU, input.dt);
            self.offsets.scale_main = Some(scale);
            if limit {
                buzz.push((r.hand, 0.2, 10));
            }
        }
        self.panel_chrome.resize = Some(r);
        // Holding one keeps the panel up.
        self.last_activity = Instant::now();
    }

    /// How far a panel moved by a bar held with the trigger is now from the
    /// viewer: where it was, with the thumbstick's push on it, kept within
    /// range. (A trigger has no hand reach to take, as the screen's bar has
    /// none.)
    pub(in crate::app) fn stick_pushed(distance: f32, stick_y: f32, dt: f32) -> f32 {
        let (near, far) = (*PANEL_RANGE.start(), *PANEL_RANGE.end());
        stick_depth(distance, stick_y, dt).clamp(near, far)
    }

    /// Paints the panels' window controls that changed.
    pub(in crate::app) fn paint_panel_chrome(
        &mut self,
        renderer: &mut Renderer,
        input: &FrameInput,
    ) {
        let hovered = self.pointer.hovered();
        let held = self.panel_drag.and_then(|d| d.via);
        let resizing = self.panel_chrome.resize.is_some();
        let state = |slot: usize, is_held: bool| {
            if is_held {
                HandleState::Held
            } else if hovered == Some(PANEL_BAR_FIRST + slot) {
                HandleState::Hover
            } else {
                HandleState::Rest
            }
        };
        let chrome = &mut self.panel_chrome;
        for i in 0..=PANEL_BARS {
            let looks = if i == PANEL_BARS {
                state(i, resizing)
            } else {
                state(i, held == Some(i))
            };
            let panel = if i == PANEL_BARS {
                &mut chrome.corner
            } else {
                &mut chrome.bars[i]
            };
            if chrome.painted[i] != Some(looks) {
                chrome.painted[i] = Some(looks);
                panel.request_repaint();
            }
            if !panel.visible || !panel.needs_paint() {
                continue;
            }
            let painted = panel.paint(renderer, input.time, |ctx| {
                if i == PANEL_BARS {
                    window::resize_corner(ctx, looks);
                } else {
                    window::move_bar(ctx, looks);
                }
            });
            if let Err(e) = painted {
                log::error!("painting a panel's window controls: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::chrome::layout::Sky;

    fn body() -> PanelBody {
        PanelBody {
            centre: Vec2::ZERO,
            half: Vec2::new(0.8, 0.5),
        }
    }

    /// Where a panel at `pose` with this body puts its bar, as the eye sees it.
    fn bar_sky(pose: Mat4, body: PanelBody, eye_distance: f32) -> Sky {
        let spot = bar_spot(body, eye_distance, 1.0);
        Sky::of(pose * spot.pose, spot.size)
    }

    #[test]
    fn a_panel_bar_moves_its_panel() {
        // A bar is a handle, so a trigger on it grabs (as the grip does anywhere
        // on the panel); it sits clear of the panel's body and in front of it.
        let bar = Panel::new([640, 144], Vec2::new(0.3, 0.07), 2.0).all_handle();
        assert!(!bar.handles.is_empty());
        let spot = bar_spot(body(), 1.5, 1.0);
        let top = spot.pose.w_axis.y + spot.size.y / 2.0;
        assert!(top < -body().half.y, "{top}");
        assert!(spot.pose.w_axis.z > 0.0);
        // Held by a trigger, the thumbstick pushes it away and pulls it near,
        // within the panels' range.
        assert!(App::stick_pushed(1.5, 1.0, 0.5) > 1.5);
        assert!(App::stick_pushed(1.5, -1.0, 0.5) < 1.5);
        assert_eq!(App::stick_pushed(1.5, 0.1, 0.5), 1.5);
        assert_eq!(App::stick_pushed(3.4, 1.0, 5.0), *PANEL_RANGE.end());
        assert_eq!(App::stick_pushed(0.6, -1.0, 5.0), *PANEL_RANGE.start());
    }

    #[test]
    fn the_library_resizes_about_its_centre() {
        let mut panel = Panel::new([1600, 1000], Vec2::new(1.8, 1.125), 1.25);
        let pose = Mat4::from_translation(Vec3::new(0.3, -0.4, -1.5));
        panel.pose = pose;
        panel.set_world_scale(1.5);
        assert_eq!(panel.pose, pose, "the middle stays where it is");
        assert!((panel.size.x - 2.7).abs() < 1e-5);
        panel.set_world_scale(0.6);
        assert!((panel.size.y - 0.675).abs() < 1e-5);
        // Its text does not change size with it.
        assert_eq!(panel.points(), egui::vec2(1280.0, 800.0));
        // The hand pulling the corner out twice as far doubles it, clamped.
        let k = Vec2::new(0.8, -0.5).normalize();
        let first = k * 0.9;
        let close = |got: (f32, bool), want: (f32, bool)| {
            assert!(
                (got.0 - want.0).abs() < 1e-4 && got.1 == want.1,
                "{got:?} {want:?}"
            );
        };
        close(resized(1.0, 0.9, k, first * 1.5), (1.5, false));
        close(resized(1.0, 0.9, k, first * 3.0), (1.8, true));
        close(resized(1.0, 0.9, k, first * 0.2), (0.6, true));
        // Starting from a larger size, scales from there; sliding sideways of
        // the corner's direction changes nothing.
        close(resized(1.5, 0.9, k, first * 1.2), (1.8, true));
        close(resized(1.2, 0.9, k, first * 1.2), (1.44, false));
        let across = Vec2::new(k.y, -k.x) * 0.5;
        close(resized(1.0, 0.9, k, first + across), (1.0, false));
        // A grab that began at nothing never divides by it.
        close(resized(1.2, 0.0, k, first), (1.2, false));
    }

    #[test]
    fn an_old_settings_file_keeps_full_size() {
        use crate::settings::PanelOffsets;
        let old = r#"{"main":[3.0,-2.0],"depth":{"main":0.2}}"#;
        let offsets: PanelOffsets = serde_json::from_str(old).unwrap();
        assert_eq!(offsets.scale_main, None);
        assert_eq!(offsets.main, [3.0, -2.0]);
        // None means usual size, not none at all.
        let mut panel = Panel::new([1600, 1000], Vec2::new(1.8, 1.125), 1.25);
        panel.set_world_scale(offsets.scale_main.unwrap_or(1.0));
        assert_eq!(panel.size, Vec2::new(1.8, 1.125));
        // And a stored size comes back.
        let kept = serde_json::to_string(&PanelOffsets {
            scale_main: Some(1.4),
            ..Default::default()
        })
        .unwrap();
        let back: PanelOffsets = serde_json::from_str(&kept).unwrap();
        assert_eq!(back.scale_main, Some(1.4));
    }

    #[test]
    fn bars_clear_each_other_and_the_control_bar() {
        use crate::world::{place_around, place_below};
        // The panels where they start, as the eye at the origin sees them.
        let main = place_below(Mat4::IDENTITY, 0.0, 0.0, MAIN_DISTANCE, BELOW_DEG);
        let adjust = place_around(Mat4::IDENTITY, -52.0, 0.0, ADJUST_DISTANCE, 0.0);
        let perf = place_around(Mat4::IDENTITY, 30.0, 0.0, PERF_DISTANCE, 0.0);
        let bars = [
            bar_sky(main, body(), MAIN_DISTANCE),
            bar_sky(
                adjust,
                PanelBody {
                    half: Vec2::new(0.35, 0.45),
                    ..body()
                },
                ADJUST_DISTANCE,
            ),
            bar_sky(
                perf,
                PanelBody {
                    half: Vec2::new(0.23, 0.2),
                    ..body()
                },
                PERF_DISTANCE,
            ),
        ];
        for (i, a) in bars.iter().enumerate() {
            for b in &bars[i + 1..] {
                assert!(!a.overlaps(b, 0.0), "{a:?} {b:?}");
            }
        }
        // The control bar hangs at its usual place; the library's bar is
        // under the library, which shows instead of the control bar, and the
        // adjust panel's is beside it, so neither is under it.
        let control = chrome_bar_sky();
        assert!(
            !bars[1].overlaps(&control, 0.0),
            "{control:?} {:?}",
            bars[1]
        );
    }

    /// Where the control bar's body is at its usual place, to the eye.
    fn chrome_bar_sky() -> Sky {
        let pose = layout::bar_pose(Mat4::IDENTITY, BAR_DISTANCE, BAR_DROP);
        Sky::of(pose, Vec2::new(1.2, 0.25))
    }

    #[test]
    fn a_ray_near_the_panel_shows_its_bar() {
        let pose = Mat4::from_translation(Vec3::new(0.0, 0.0, -1.5));
        let at = |x: f32, y: f32| {
            let dir = Vec3::new(x, y, -1.5).normalize();
            ray_near(pose, body(), 1.5, 1.0, Vec3::ZERO, dir)
        };
        assert!(at(0.0, 0.0), "on it");
        assert!(at(0.0, -0.6), "just below it, where the bar is");
        assert!(!at(0.0, -1.5), "well below");
        assert!(!at(2.5, 0.0), "well to the side");
        assert!(
            !ray_near(pose, body(), 1.5, 1.0, Vec3::ZERO, Vec3::Z),
            "behind"
        );
    }
}
