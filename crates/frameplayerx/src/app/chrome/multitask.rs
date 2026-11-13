//! Browse while watching: the playing screen and the library side by side in
//! front of you, both live, so a film can be picked while another plays.
//!
//! [`layout`] and [`Multitask`] are plain maths and bookkeeping: where the
//! two go so they do not overlap, and what was where before so leaving puts
//! it all back. The `impl App` below wires that to the app. It lasts for the
//! session only: the screen's turn and width are the open video's unsaved
//! adjustments (put back on leaving), and the library's offsets are kept out
//! of what Settings saves (see [`Multitask::kept`]).

use crate::app::{App, MAIN, MAIN_DISTANCE};
use crate::settings::PanelOffsets;
use crate::ui::Action;
use fp_core::format::Projection;
use fp_render::ScreenShape;
use glam::Vec2;
use std::time::Instant;

/// Degrees between the screen's nearest edge and the library's.
const GAP: f32 = 4.0;

/// The widest the screen is made to look, as the eye sees it: half of its
/// angle across, degrees. A wider one is narrowed (never a narrower one
/// widened), or the pair would reach round the head.
const MAX_HALF: f32 = 24.0;

/// The screen takes this share of the turn between the two's middles, the
/// library the rest: about 22 degrees left and 28 right for a screen and
/// library as wide as the library.
const SCREEN_SHARE: f32 = 0.44;

/// Where the screen and the library go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spread {
    /// The screen's yaw (left positive) and the library's, degrees.
    pub screen_yaw: f32,
    pub main_yaw: f32,
    /// What to multiply the screen's width by: 1 unless it was wider than
    /// [`MAX_HALF`] allows.
    pub screen_scale: f32,
}

/// The angle a width `across` metres at `distance` metres covers on each side
/// of its middle, degrees.
fn half_angle(across: f32, distance: f32) -> f32 {
    (across / 2.0 / distance.max(0.01)).atan().to_degrees()
}

/// The screen (`shape`, `distance` metres away) on the left and the library
/// (`main` metres across and tall, `main_distance` metres away) on the right,
/// at the same distances they have apart, with [`GAP`] degrees between them.
pub fn layout(shape: &ScreenShape, distance: f32, main: Vec2, main_distance: f32) -> Spread {
    let half = half_angle(shape.width, distance);
    let (screen_scale, half) = if half > MAX_HALF {
        // The same distance, so the widths go as the tangents of the angles.
        (
            MAX_HALF.to_radians().tan() / half.to_radians().tan(),
            MAX_HALF,
        )
    } else {
        (1.0, half)
    };
    let library = half_angle(main.x, main_distance);
    // How far apart the two middles must be.
    let apart = half + library + GAP;
    Spread {
        screen_yaw: apart * SCREEN_SHARE,
        main_yaw: -apart * (1.0 - SCREEN_SHARE),
        screen_scale,
    }
}

/// What multitasking moves, for putting it back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrangement {
    /// The open video's screen.
    pub yaw: f32,
    pub pitch: f32,
    pub width: f32,
    /// Whether the video's adjustments differed from what is saved.
    pub dirty: bool,
    /// The library's turn and how much nearer or further than usual.
    pub main: [f32; 2],
    pub main_depth: f32,
}

impl Arrangement {
    /// `self` with the screen and library where `spread` puts them (the
    /// screen level, the library at its usual distance).
    pub fn spread(&self, spread: &Spread) -> Arrangement {
        Arrangement {
            yaw: spread.screen_yaw,
            pitch: 0.0,
            width: self.width * spread.screen_scale,
            dirty: true,
            main: [spread.main_yaw, 0.0],
            main_depth: 0.0,
        }
    }
}

/// Multitasking is on: where everything was before, and whether the library
/// was open then.
#[derive(Clone, Copy, Debug)]
pub struct Multitask {
    pub before: Arrangement,
    pub show_browser: bool,
}

impl Multitask {
    /// The same, for a video that opened since: its screen is what it
    /// goes back to, the library's place and whether it was open stay.
    pub fn rebased(self, screen: &Arrangement) -> Multitask {
        Multitask {
            before: Arrangement {
                yaw: screen.yaw,
                pitch: screen.pitch,
                width: screen.width,
                dirty: screen.dirty,
                ..self.before
            },
            ..self
        }
    }

    /// `offsets` as Settings may keep them while this is on: the library's
    /// are where they were before, not where this put them.
    pub fn kept(&self, offsets: PanelOffsets) -> PanelOffsets {
        let mut kept = offsets;
        kept.main = self.before.main;
        kept.depth.main = self.before.main_depth;
        kept
    }
}

impl App {
    /// Whether the screen and library are side by side.
    pub(in crate::app) fn multitasking(&self) -> bool {
        self.chrome.multitask.is_some()
    }

    /// The button: side by side, or back as it was.
    pub(in crate::app) fn toggle_multitask(&mut self) {
        if self.multitasking() {
            self.end_multitask(false);
        } else {
            self.start_multitask();
        }
    }

    fn start_multitask(&mut self) {
        // Only a flat screen outside a room has a place to move.
        let Some((shape, distance)) = self.screen_geometry() else {
            return;
        };
        let main = self.panels[MAIN].size;
        let Some(before) = self.current_arrangement() else {
            return;
        };
        self.chrome.multitask = Some(Multitask {
            before,
            show_browser: self.show_browser,
        });
        let spread = layout(&shape, distance, main, MAIN_DISTANCE);
        self.arrange(before.spread(&spread));
        // The library shows what the browser shows, fresh.
        self.apply(Action::ShowBrowser(true));
        self.ui.adjust_open = false;
        self.ui.tracks_open = false;
        self.ui.more_open = false;
        self.ui_visible = true;
        self.last_activity = Instant::now();
    }

    /// Puts the screen and library as `now` has them.
    fn arrange(&mut self, now: Arrangement) {
        if let Some(p) = &mut self.playback {
            p.settings.yaw = now.yaw;
            p.settings.pitch = now.pitch;
            p.settings.screen_width = now.width;
            p.settings_dirty = now.dirty;
        }
        self.offsets.main = now.main;
        self.offsets.depth.main = now.main_depth;
    }

    /// Puts everything back as it was. `to_video` shows the video alone (Back
    /// to video); otherwise the library is open or not as it was.
    pub(in crate::app) fn end_multitask(&mut self, to_video: bool) {
        let Some(m) = self.chrome.multitask.take() else {
            return;
        };
        let flat = self
            .playback
            .as_ref()
            .is_some_and(|p| p.format.projection == Projection::Flat);
        if flat {
            self.arrange(m.before);
        } else {
            self.offsets.main = m.before.main;
            self.offsets.depth.main = m.before.main_depth;
        }
        self.show_browser = self.playback.is_none() || (!to_video && m.show_browser);
        self.ui_visible = true;
        self.last_activity = Instant::now();
    }

    /// Ends it when it cannot go on: the video is gone or no longer a flat
    /// screen, or a room has set where the screen is.
    pub(in crate::app) fn check_multitask(&mut self) {
        if self.multitasking() && self.screen_geometry().is_none() {
            self.end_multitask(false);
        }
    }

    /// A video opened (and was placed where the library was before): it
    /// takes the screen's place, and goes back there when this ends.
    pub(in crate::app) fn multitask_new_video(&mut self) {
        let Some(m) = self.chrome.multitask else {
            return;
        };
        let (Some((shape, distance)), Some(now)) =
            (self.screen_geometry(), self.current_arrangement())
        else {
            self.end_multitask(false);
            return;
        };
        let m = m.rebased(&now);
        self.chrome.multitask = Some(m);
        let spread = layout(&shape, distance, self.panels[MAIN].size, MAIN_DISTANCE);
        self.arrange(m.before.spread(&spread));
    }

    /// Where the open video's screen and the library are now.
    fn current_arrangement(&self) -> Option<Arrangement> {
        let p = self.playback.as_ref()?;
        Some(Arrangement {
            yaw: p.settings.yaw,
            pitch: p.settings.pitch,
            width: p.settings.screen_width,
            dirty: p.settings_dirty,
            main: self.offsets.main,
            main_depth: self.offsets.depth.main,
        })
    }

    /// Where a video that opens now is placed from: the library's yaw as it
    /// was before multitasking moved it.
    pub(in crate::app) fn placed_from(&self) -> f32 {
        match &self.chrome.multitask {
            Some(m) => m.before.main[0],
            None => self.offsets.main[0],
        }
    }

    /// The offsets Settings keeps (see [`Multitask::kept`]).
    pub(in crate::app) fn kept_offsets(&self) -> PanelOffsets {
        match &self.chrome.multitask {
            Some(m) => m.kept(self.offsets),
            None => self.offsets,
        }
    }

    /// Recentring turned the library and screen to face forward: side by side
    /// again, about where you now look, and put back there (not where they
    /// were) when this ends.
    pub(in crate::app) fn recenter_multitask(&mut self) {
        let Some(m) = &mut self.chrome.multitask else {
            return;
        };
        m.before.yaw = 0.0;
        m.before.pitch = 0.0;
        m.before.main = [0.0; 2];
        m.before.main_depth = 0.0;
        let (Some((shape, distance)), Some(now)) =
            (self.screen_geometry(), self.current_arrangement())
        else {
            return;
        };
        // Its width is already narrowed: the pair still fits as it is.
        let spread = Spread {
            screen_scale: 1.0,
            ..layout(&shape, distance, self.panels[MAIN].size, MAIN_DISTANCE)
        };
        self.arrange(now.spread(&spread));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(width: f32) -> ScreenShape {
        ScreenShape {
            width,
            height: width * 9.0 / 16.0,
            arc: 0.0,
        }
    }

    /// The library panel's body (floating, shadow room and all).
    const LIBRARY: Vec2 = Vec2::new(1.8, 1.125);

    /// The screen's and the library's edges after `s`: the screen's right
    /// edge and the library's left, yaw degrees (left positive).
    fn edges(width: f32, distance: f32, main: Vec2, s: &Spread) -> (f32, f32) {
        let screen = half_angle(width * s.screen_scale, distance);
        let library = half_angle(main.x, MAIN_DISTANCE);
        (s.screen_yaw - screen, s.main_yaw + library)
    }

    #[test]
    fn multitask_lays_out_without_overlap() {
        for width in [0.5, 2.0, 6.0, 12.0, 30.0] {
            for distance in [1.0, 2.5, 4.0, 10.0, 20.0] {
                for scale in [0.8, 1.0, 1.5, 2.0] {
                    let main = LIBRARY * scale;
                    let s = layout(&shape(width), distance, main, MAIN_DISTANCE);
                    let what = format!("{width} m at {distance} m, library x{scale}: {s:?}");
                    let (screen_edge, library_edge) = edges(width, distance, main, &s);
                    assert!(
                        screen_edge - library_edge >= GAP - 1e-3,
                        "{what}: {screen_edge} vs {library_edge}"
                    );
                    assert!(s.screen_yaw > 0.0 && s.main_yaw < 0.0, "{what}");
                    // Only ever narrowed, and never wider than allowed.
                    assert!(s.screen_scale > 0.0 && s.screen_scale <= 1.0, "{what}");
                    assert!(
                        half_angle(width * s.screen_scale, distance) <= MAX_HALF + 1e-3,
                        "{what}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_usual_screen_goes_left_and_the_library_right() {
        // 6 m at 4 m is wider than the pair allows: narrowed to the cap.
        let s = layout(&shape(6.0), 4.0, LIBRARY, MAIN_DISTANCE);
        assert!(s.screen_scale < 1.0);
        assert!((20.0..35.0).contains(&s.screen_yaw), "{s:?}");
        assert!((-40.0..-24.0).contains(&s.main_yaw), "{s:?}");
        // A small one is left as it is.
        let small = layout(&shape(2.0), 4.0, LIBRARY, MAIN_DISTANCE);
        assert_eq!(small.screen_scale, 1.0);
    }

    fn before() -> Arrangement {
        Arrangement {
            yaw: 12.5,
            pitch: -3.0,
            width: 6.0,
            dirty: false,
            main: [-8.0, 4.0],
            main_depth: 0.3,
        }
    }

    #[test]
    fn leaving_puts_everything_back() {
        let was = before();
        let s = layout(&shape(was.width), 4.0, LIBRARY, MAIN_DISTANCE);
        let now = was.spread(&s);
        assert_ne!(now, was);
        assert_eq!((now.pitch, now.main_depth), (0.0, 0.0));
        // A video opened meanwhile goes back to its own screen, and the
        // library back to where it was before this began.
        let next = Arrangement {
            yaw: 0.0,
            pitch: 0.0,
            width: 8.0,
            dirty: false,
            main: now.main,
            main_depth: 0.0,
        };
        let m = Multitask {
            before: was,
            show_browser: true,
        }
        .rebased(&next);
        assert_eq!(m.before.width, 8.0);
        assert_eq!(m.before.main, was.main);
        assert_eq!(m.before.main_depth, was.main_depth);
        assert!(m.show_browser);
    }

    #[test]
    fn nothing_is_saved() {
        let was = before();
        let saved = PanelOffsets {
            main: was.main,
            bar: [1.0, 2.0],
            depth: crate::settings::PanelDepths {
                main: was.main_depth,
                bar: -0.2,
                ..Default::default()
            },
            ..Default::default()
        };
        let m = Multitask {
            before: was,
            show_browser: false,
        };
        let s = layout(&shape(was.width), 4.0, LIBRARY, MAIN_DISTANCE);
        let now = was.spread(&s);
        // The offsets as multitasking leaves them, and the bar moved too.
        let mut offsets = saved;
        offsets.main = now.main;
        offsets.depth.main = now.main_depth;
        offsets.bar = [5.0, 6.0];
        let kept = m.kept(offsets);
        // The library's are the old ones; the bar's change is kept.
        assert_eq!(kept.main, saved.main);
        assert_eq!(kept.depth.main, saved.depth.main);
        assert_eq!(kept.bar, [5.0, 6.0]);
        assert_eq!(kept.depth.bar, saved.depth.bar);
    }
}
