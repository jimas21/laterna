//! The opening: the logo's lantern throws its beam, the warm disc flashes and
//! cuts out, and the disc's cool afterimage swells and fades as the library
//! comes up from under a dark veil.
//!
//! It is drawn over the library while the library loads and takes no input:
//! the first frame is already usable, any press skips it, and Settings >
//! General has a switch for it. Under 2.5 seconds, all shapes (no images),
//! so it is sharp at any panel resolution.

use super::theme::{self, Weight};
use egui::{Color32, Pos2, Rect, Shape};

/// How long it runs, seconds.
pub const DURATION: f32 = 2.4;

/// The moments in it, seconds.
const BEAM_FROM: f32 = 0.05;
const BEAM_FULL: f32 = 0.45;
const DISC_ON: f32 = 0.35;
const CUT: f32 = 0.95;
const CUT_DONE: f32 = 1.2;
const VEIL_UP: f32 = 0.9;
const WORD_IN: f32 = 1.2;
const WORD_OUT: f32 = 2.2;

/// The veil's darkness at the start.
const VEIL: f32 = 0.94;

/// A running opening.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Afterglow {
    /// Seconds since it began.
    pub t: f32,
    skipped: bool,
}

impl Afterglow {
    pub fn new() -> Afterglow {
        Afterglow::default()
    }

    /// Runs it on `dt` seconds (a bad step runs nothing).
    pub fn advance(&mut self, dt: f32) {
        if dt.is_finite() && dt > 0.0 {
            self.t += dt.min(0.1);
        }
    }

    /// Ends it now.
    pub fn skip(&mut self) {
        self.skipped = true;
    }

    pub fn done(&self) -> bool {
        self.skipped || self.t >= DURATION
    }
}

/// `x` from `a` (0) to `b` (1), clamped.
fn seg(x: f32, a: f32, b: f32) -> f32 {
    ((x - a) / (b - a)).clamp(0.0, 1.0)
}

/// Slow in, slow out.
fn ease(x: f32) -> f32 {
    x * x * (3.0 - 2.0 * x)
}

/// How dark the veil over the library is at `t`: 0 clear, 1 black.
fn veil(t: f32) -> f32 {
    VEIL * (1.0 - ease(seg(t, VEIL_UP, DURATION)))
}

/// How much of the lit picture (beam and disc) shows at `t`: lights up, then
/// cuts out.
fn lit(t: f32) -> f32 {
    ease(seg(t, BEAM_FROM, BEAM_FULL)) * (1.0 - ease(seg(t, CUT, CUT_DONE)))
}

/// How strong the cool afterimage is at `t`: it rises as the light cuts,
/// then fades unevenly (a small second swell on the way down).
fn afterimage(t: f32) -> f32 {
    let rise = ease(seg(t, CUT - 0.1, CUT_DONE + 0.15));
    let fall = 1.0 - ease(seg(t, CUT_DONE + 0.1, DURATION));
    let return_once = 0.18 * (seg(t, 1.6, 2.2) * std::f32::consts::PI).sin();
    (rise * fall + return_once * rise).clamp(0.0, 1.0)
}

fn alpha(c: Color32, a: f32) -> Color32 {
    let [r, g, b, _] = c.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Draws the opening over `body` (the library panel's rect), and says
/// whether it is still running. Any press skips it.
pub fn paint(ctx: &egui::Context, body: Rect, a: &mut Afterglow) -> bool {
    if ctx.input(|i| i.pointer.any_pressed()) {
        a.skip();
    }
    if a.done() {
        return false;
    }
    let p = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("afterglow"),
    ));
    let t = a.t;
    p.rect_filled(body, 28.0, alpha(theme::LOGO_SQUARE_BOTTOM, veil(t)));

    // The logo's 512 grid, filling most of the panel's height, centred.
    let k = (body.height() * 0.8).min(body.width() * 0.8) / 512.0;
    let origin = body.center() - egui::vec2(256.0, 256.0) * k;
    let at = |x: f32, y: f32| origin + egui::vec2(x, y) * k;

    let lens = at(142.0, 380.0);
    let disc = at(310.0, 194.0);

    let light = lit(t);
    if light > 0.0 {
        // The beam grows from the lens to the disc.
        let grow = ease(seg(t, BEAM_FROM, BEAM_FULL));
        let toward = |to: Pos2| lens + (to - lens) * grow;
        let beam = vec![
            toward(at(120.0, 352.0)),
            toward(at(246.0, 116.0)),
            toward(at(398.0, 268.0)),
            toward(at(170.0, 402.0)),
        ];
        p.add(Shape::convex_polygon(
            beam,
            alpha(theme::LOGO_DISC, 0.5 * light),
            egui::Stroke::NONE,
        ));
        // The disc comes on overbright and settles.
        let on = ease(seg(t, DISC_ON, DISC_ON + 0.3));
        let flash = 1.0 - seg(t, DISC_ON, DISC_ON + 0.4);
        p.circle_filled(
            disc,
            122.0 * k * (0.6 + 0.4 * on),
            alpha(theme::LOGO_DISC, on * light),
        );
        p.circle_filled(
            disc,
            122.0 * k * (0.6 + 0.4 * on),
            alpha(Color32::WHITE, 0.6 * flash * on),
        );
        // The lens glows at the start of the beam.
        p.circle_filled(
            lens,
            44.0 * k,
            alpha(theme::LOGO_LENS_CENTRE, light.max(0.2 * grow)),
        );
        p.circle_stroke(
            lens,
            42.0 * k,
            egui::Stroke::new(16.0 * k, alpha(theme::LOGO_BRASS, light)),
        );
    }

    // The afterimage: the disc in the opposite colours, drifting a little up
    // and right, swelling and softening. Layered circles stand in for blur.
    let after = afterimage(t);
    if after > 0.0 {
        const LAYERS: u32 = 8;
        let swell = 1.0 + 0.18 * seg(t, CUT, DURATION);
        let drift = egui::vec2(10.0, -6.0) * k * seg(t, CUT, DURATION);
        for i in 0..LAYERS {
            let f = i as f32 / (LAYERS - 1) as f32;
            let radius = 122.0 * k * swell * (0.85 + 0.55 * f);
            let layer = after * 0.5 * (1.0 - f * 0.8) / LAYERS as f32 * 3.0;
            p.circle_filled(
                disc + drift,
                radius,
                alpha(Color32::from_rgb(31, 214, 181), layer),
            );
        }
    }

    // The name, faintly, as the afterimage fades.
    let word =
        ease(seg(t, WORD_IN, WORD_IN + 0.4)) * (1.0 - ease(seg(t, WORD_OUT - 0.4, WORD_OUT)));
    if word > 0.0 {
        p.text(
            body.center() + egui::vec2(0.0, 150.0 * k),
            egui::Align2::CENTER_CENTER,
            "Laterna",
            theme::font(Weight::Medium, 34.0),
            alpha(theme::LOGO_DISC, 0.75 * word),
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Short enough to be an opening, not a wait.
    const _: () = assert!(DURATION <= 2.5);

    #[test]
    fn it_ends_by_itself_and_can_be_skipped() {
        let mut a = Afterglow::new();
        assert!(!a.done());
        let mut frames = 0;
        while !a.done() {
            a.advance(1.0 / 72.0);
            frames += 1;
            assert!(frames < 400, "never ends");
        }
        assert!(a.t >= DURATION);
        let mut b = Afterglow::new();
        b.skip();
        assert!(b.done());
    }

    #[test]
    fn a_long_or_bad_step_does_not_jump_it() {
        let mut a = Afterglow::new();
        a.advance(f32::NAN);
        a.advance(-1.0);
        assert_eq!(a.t, 0.0);
        a.advance(5.0);
        assert!(a.t <= 0.1 + 1e-6, "one slow frame cannot cut it short");
    }

    #[test]
    fn the_library_is_covered_at_first_and_clear_at_the_end() {
        assert!(veil(0.0) > 0.9);
        assert_eq!(veil(DURATION), 0.0);
        let mut last = f32::MAX;
        for i in 0..=24 {
            let v = veil(0.9 + i as f32 * 0.0625);
            assert!(v <= last + 1e-6, "the veil only lifts");
            last = v;
        }
    }

    #[test]
    fn the_light_cuts_out_and_leaves_an_afterimage_that_fades() {
        assert_eq!(lit(0.0), 0.0);
        assert!(lit(0.7) > 0.9, "lit before the cut");
        assert_eq!(lit(CUT_DONE), 0.0, "the cut is clean");
        assert!(afterimage(0.7) < 0.01, "no afterimage while it is lit");
        let peak = afterimage(CUT_DONE + 0.15);
        assert!(peak > 0.8, "{peak}");
        assert!(afterimage(DURATION) < 0.01, "gone at the end");
        for i in 0..=48 {
            let a = afterimage(i as f32 * DURATION / 48.0);
            assert!((0.0..=1.0).contains(&a), "{a}");
        }
    }
}
