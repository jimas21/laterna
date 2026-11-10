//! Window controls on a flat video screen, as the Steam Frame's own windows
//! have: a bar under the screen to move it by and a handle at its bottom
//! right corner to resize it. Hold the trigger (or the grip) on one and move
//! your hand; the thumbstick pushes the screen away or pulls it near.
//!
//! [`layout`] works out where they go and `drag` what holding them does,
//! both as plain maths; this module wires them to the app's panels, input
//! and the open video's view.

pub mod layout;
pub mod panels;

mod drag;

use super::*;
use crate::controls::{Damped, GRIP};
use crate::ui::window::{self, HandleState};
use crate::world::{Grab, GrabBy};
use drag::{Frame, Part, ScreenDrag, ScreenView, Start};
use layout::{BAR_DROP, Body, Sky};

/// Where the move bar and the resize handle come in the slice
/// [`Pointer::route`] is given: after the five panels that take the pointer
/// and the performance overlay. Nothing indexing the app's own panels with
/// a pointed-at panel may take these (see `Pointer::hovered_below`).
pub const WINDOW_BAR: usize = 6;
pub const WINDOW_CORNER: usize = 7;

/// Their quad layers' keys (the panels' are their indices, the performance
/// overlay's `PERF_LAYER`).
pub const BAR_LAYER: u64 = 9;
pub const CORNER_LAYER: u64 = 10;

/// How long the control bar takes to ease to a new height, seconds.
const BAR_EASE: f32 = 0.15;

/// The window controls' panels and what is held.
pub struct ScreenChrome {
    pub bar: Panel,
    pub corner: Panel,
    drag: Option<ScreenDrag>,
    /// How far below level the control bar's middle hangs (see
    /// [`layout::bar_pose`]), easing to `bar_drop_to`.
    bar_drop: Damped<1>,
    bar_drop_to: f32,
    /// How each was last painted: [`HandleState`] changes repaint them.
    painted: [Option<HandleState>; 2],
}

impl Default for ScreenChrome {
    fn default() -> ScreenChrome {
        // Texture sizes in pixels and points per egui point: fine enough that
        // the control looks sharp when it covers a few degrees of the eye's
        // view, in the same shape as the angles in `layout::LOOK`.
        let mut bar = Panel::new([640, 144], Vec2::new(0.3, 0.07), 2.0).all_handle();
        let mut corner = Panel::new([192, 192], Vec2::new(0.15, 0.15), 1.5).all_handle();
        // For comparing how they sit against the picture: with this on they
        // are drawn into the eyes with it, not as layers of their own.
        let in_eyes = std::env::var("FRAMEPLAYERX_CHROME_IN_EYES").is_ok_and(|v| v == "1");
        bar.in_eyes = in_eyes;
        corner.in_eyes = in_eyes;
        ScreenChrome {
            bar,
            corner,
            drag: None,
            bar_drop: Damped::at([BAR_DROP]),
            bar_drop_to: BAR_DROP,
            painted: [None; 2],
        }
    }
}

impl ScreenChrome {
    /// Whether a window control is held.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Lets go (the screen was recentred, or put back).
    pub fn cancel(&mut self) {
        self.drag = None;
    }

    /// Which hands hold a window control.
    pub fn held_by(&self) -> [bool; 2] {
        let mut held = [false; 2];
        if let Some(d) = &self.drag {
            held[d.hand] = true;
        }
        held
    }

    /// Eases the control bar's height towards where it is wanted; once a
    /// frame.
    pub fn ease(&mut self, dt: f32) {
        self.bar_drop.step([self.bar_drop_to], BAR_EASE, dt);
    }

    /// How far below level the control bar's middle hangs now, as a fraction
    /// of its distance.
    pub fn bar_drop(&self) -> f32 {
        self.bar_drop.value()[0]
    }
}

impl App {
    /// The window controls can show: a flat video is open, Settings has
    /// them on, and no room is shown (a room's screen is where the room has
    /// it).
    fn window_controls_allowed(&self) -> bool {
        self.settings.window_controls
            && !self.in_room()
            && self
                .playback
                .as_ref()
                .is_some_and(|p| p.format.projection == Projection::Flat)
    }

    /// Which window controls show: with the control bar, or for as long as
    /// one is held. The resize handle gives way to the adjust panel, which
    /// sits over it.
    pub(super) fn show_window_controls(&mut self) {
        let part = self.chrome.drag.map(|d| d.part);
        let with_bar = self.panels[BAR].visible || part.is_some();
        let shown = self.window_controls_allowed() && with_bar;
        self.chrome.bar.visible = shown;
        let under_adjust = self.ui.adjust_open && part != Some(Part::Resize);
        self.chrome.corner.visible = shown && !under_adjust;
    }

    /// Puts the window controls where the screen is, and returns how far
    /// below level (as a fraction of its distance) the control bar should
    /// hang so that it does not cover them: where it always was unless it
    /// would. `bar_d` is how far away the control bar is.
    pub(super) fn place_window_controls(&mut self, anchor: Mat4, bar_d: f32) -> f32 {
        let geometry = self.screen_geometry();
        let Some((shape, distance)) = geometry.filter(|_| self.window_controls_allowed()) else {
            self.panels[BAR].solid = None;
            self.chrome.bar_drop_to = BAR_DROP;
            return self.chrome.bar_drop();
        };
        let (least, most) = (*UI_SCALES.start(), *UI_SCALES.end());
        let scale = self.settings.ui_scale.clamp(least, most);
        let picture = self.picture_anchor(anchor);
        let bar = layout::move_bar_spot(&shape, distance, scale);
        let corner = layout::corner_spot(&shape, distance, scale);
        self.chrome.bar.pose = picture * bar.pose;
        self.chrome.bar.size = bar.size;
        self.chrome.corner.pose = picture * corner.pose;
        self.chrome.corner.size = corner.size;
        // A ray passes through the control bar's shadow, so the bar's panel
        // does not cover a control hung close under its body.
        let room = ui::widgets::SHADOW_ROOM;
        let control_bar = &mut self.panels[BAR];
        let points = control_bar.points();
        let whole = egui::Rect::from_min_size(egui::Pos2::ZERO, points);
        control_bar.solid = Some(whole - room);
        let per_point = control_bar.size.x / points.x;
        let (left, right) = (room.left as f32, room.right as f32);
        let (top, bottom) = (room.top as f32, room.bottom as f32);
        let body = Body {
            size: Vec2::new(points.x - left - right, points.y - top - bottom) * per_point,
            centre: Vec2::new((left - right) / 2.0, (bottom - top) / 2.0) * per_point,
        };
        // Held, the bar stays where it is: it must not jump mid-drag.
        if self.chrome.drag.is_none() {
            let [yaw, pitch] = self.offsets.bar;
            let bar_frame = place_around(Mat4::IDENTITY, yaw, pitch, 0.0, 0.0);
            let mut avoid = Vec::new();
            if self.chrome.bar.visible {
                avoid.push(Sky::of(bar.pose, bar.size));
            }
            if self.chrome.corner.visible {
                avoid.push(Sky::of(corner.pose, corner.size));
            }
            self.chrome.bar_drop_to = layout::clear_drop(bar_frame, bar_d, body, &avoid);
        }
        self.chrome.bar_drop()
    }

    /// A window control was taken hold of (`grab`, from the pointer): the
    /// move bar or the resize handle.
    pub(super) fn begin_window_drag(
        &mut self,
        grab: Grab,
        input: &FrameInput,
        anchor: Mat4,
        buzz: &mut Vec<(usize, f32, i64)>,
    ) {
        let part = if grab.panel == WINDOW_CORNER {
            Part::Resize
        } else {
            Part::Move
        };
        if self.chrome.drag.is_some() {
            return;
        }
        let Some(aim) = input.hands[grab.hand].aim else {
            return;
        };
        let Some((shape, _)) = self.screen_geometry() else {
            return;
        };
        let Some(p) = &self.playback else { return };
        let view = ScreenView {
            yaw: p.settings.yaw,
            pitch: p.settings.pitch,
            distance: p.settings.screen_distance,
            width: p.settings.screen_width,
        };
        let start = Start {
            part,
            hand: grab.hand,
            by: grab.by,
            aim,
            room: anchor.to_scale_rotation_translation().1,
            head: input.head.map(|(pos, _)| pos),
            picture: self.picture_anchor(anchor),
            view,
            shape,
        };
        self.chrome.drag = Some(ScreenDrag::begin(start));
        buzz.push((grab.hand, 0.3, 14));
    }

    /// Carries on a held window control for a frame, or lets it go: the
    /// trigger or grip that took hold is let go, the video is gone or no
    /// longer flat, or the hand is lost.
    pub(super) fn update_window_drag(
        &mut self,
        input: &FrameInput,
        anchor: Mat4,
        buzz: &mut Vec<(usize, f32, i64)>,
    ) {
        let Some(mut drag) = self.chrome.drag else {
            return;
        };
        let hand = &input.hands[drag.hand];
        let held = match drag.by {
            GrabBy::Trigger => self.pointer.trigger_down(drag.hand),
            GrabBy::Grip => hand.squeeze > GRIP * 0.6,
        };
        if !held || !self.window_controls_allowed() || hand.aim.is_none() {
            self.chrome.drag = None;
            return;
        }
        let step = drag.step(&Frame {
            aim: hand.aim,
            stick_y: hand.stick.y,
            head: input.head.map(|(pos, _)| pos),
            room: anchor.to_scale_rotation_translation().1,
            picture: self.picture_anchor(anchor),
            dt: input.dt,
        });
        self.chrome.drag = Some(drag);
        if let Some(p) = &mut self.playback {
            let ScreenView {
                yaw,
                pitch,
                distance,
                width,
            } = step.view;
            input::drag_view(p, yaw, pitch, distance);
            p.settings.screen_width = width;
        }
        // Holding one keeps the controls up.
        self.last_activity = Instant::now();
        if step.limit {
            buzz.push((drag.hand, 0.2, 10));
        }
    }

    /// Paints the window controls that changed: when one is pointed at, held
    /// or let go.
    pub(super) fn paint_window_controls(&mut self, renderer: &mut Renderer, input: &FrameInput) {
        let hovered = self.pointer.hovered();
        let part = self.chrome.drag.map(|d| d.part);
        let look = |index: usize, which: Part| {
            if part == Some(which) {
                HandleState::Held
            } else if hovered == Some(index) {
                HandleState::Hover
            } else {
                HandleState::Rest
            }
        };
        let states = [
            look(WINDOW_BAR, Part::Move),
            look(WINDOW_CORNER, Part::Resize),
        ];
        let chrome = &mut self.chrome;
        for (i, state) in states.into_iter().enumerate() {
            let panel = if i == 0 {
                &mut chrome.bar
            } else {
                &mut chrome.corner
            };
            if chrome.painted[i] != Some(state) {
                chrome.painted[i] = Some(state);
                panel.request_repaint();
            }
            if !panel.visible || !panel.needs_paint() {
                continue;
            }
            let painted = panel.paint(renderer, input.time, |ctx| {
                if i == 0 {
                    window::move_bar(ctx, state);
                } else {
                    window::resize_corner(ctx, state);
                }
            });
            if let Err(e) = painted {
                log::error!("painting the window controls: {e}");
            }
        }
    }

    /// For the preview harness's status line: what the window controls are
    /// doing.
    pub(super) fn window_controls_status(&self) -> &'static str {
        match self.chrome.drag.map(|d| d.part) {
            Some(Part::Move) => "move",
            Some(Part::Resize) => "resize",
            None if self.chrome.bar.visible => "shown",
            None => "hidden",
        }
    }
}
