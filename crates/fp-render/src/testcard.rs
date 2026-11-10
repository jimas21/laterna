//! A generated test card, for judging how sharp and calm the picture is.
//!
//! Everything here is pure, deterministic Rust that makes RGBA8 pictures
//! (opaque, sRGB codes) with no fonts or asset files, so tests can build the
//! same pictures that go to the headset:
//!
//! - [`card`]: the full card (zone plate, line wedges, checkers, grey steps,
//!   a dark ramp, colour bars, a border, corner marks and a centre cross).
//! - [`checker`], [`zone_plate`] and [`ramp`]: single patterns for tests.
//! - [`box_mips`]: the picture and its 2x2 box-filtered halves down to 1x1.
//!
//! The pictures are in gamma-encoded codes, and [`box_mips`] averages those
//! codes, as the shaders' own minification does. A 1-texel checker therefore
//! averages to a code of 128, not the 188 that linear light would give.

use std::f64::consts::PI;

/// The top of the card's dark ramp, in codes: 10 % of full scale.
pub const DARK_TOP: u8 = 26;

/// Steps in the card's grey scale.
pub const GREY_STEPS: u32 = 32;

/// The card's colour bars, left to right: white, yellow, cyan, green,
/// magenta, red, blue and black, all at full strength.
pub const BARS: [[u8; 3]; 8] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
    [0, 0, 0],
];

/// A block of pixels: columns `x0..x1` of rows `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    pub const fn new(x0: u32, y0: u32, x1: u32, y1: u32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn full(w: u32, h: u32) -> Rect {
        Rect::new(0, 0, w, h)
    }

    pub fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> u32 {
        self.y1 - self.y0
    }
}

/// Where each part of [`card`] sits.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// The zone plate, a square centred on the card.
    pub zone: Rect,
    /// Black and white lines in 1 px (left half) and 2 px (right half)
    /// widths: horizontal, vertical and at 45 degrees.
    pub wedges: [Rect; 3],
    /// Checkers of 1-texel and 2-texel cells.
    pub checkers: [Rect; 2],
    /// A flat grey of code 128, what a perfectly minified checker shows.
    pub grey_patch: Rect,
    /// [`GREY_STEPS`] grey steps from black to white.
    pub steps: Rect,
    /// A smooth ramp from 0 to [`DARK_TOP`], 8-bit, so it bands as a film's
    /// dark fades do.
    pub ramp: Rect,
    /// The eight [`BARS`].
    pub bars: Rect,
}

/// The card is laid out on a grid of 16 by 9 cells (square at 16:9).
fn cells(w: u32, h: u32, x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
    let px = |c: f64| (c * f64::from(w) / 16.0).round() as u32;
    let py = |c: f64| (c * f64::from(h) / 9.0).round() as u32;
    Rect::new(px(x0), py(y0), px(x1).max(px(x0)), py(y1).max(py(y0)))
}

/// Where the parts of a `w`x`h` card sit.
pub fn layout(w: u32, h: u32) -> Layout {
    let at = |x0, y0, x1, y1| cells(w, h, x0, y0, x1, y1);
    // The zone plate is square whatever the card's shape, centred on it.
    let by_width = 5.5 * f64::from(w) / 16.0;
    let by_height = 5.5 * f64::from(h) / 9.0;
    let side = by_width.min(by_height).round() as u32;
    let (zx, zy) = ((w - side) / 2, (h - side) / 2);
    let zone = Rect::new(zx, zy, zx + side, zy + side);
    // Three patches stacked on each side of it.
    let gap = 0.15;
    let tall = (5.5 - 2.0 * gap) / 3.0;
    let patch = |i: f64, x0: f64, x1: f64| {
        let y0 = 1.75 + i * (tall + gap);
        at(x0, y0, x1, y0 + tall)
    };
    Layout {
        zone,
        wedges: [
            patch(0.0, 0.5, 5.0),
            patch(1.0, 0.5, 5.0),
            patch(2.0, 0.5, 5.0),
        ],
        checkers: [patch(0.0, 11.0, 15.5), patch(1.0, 11.0, 15.5)],
        grey_patch: patch(2.0, 11.0, 15.5),
        steps: at(0.5, 0.5, 15.5, 1.4),
        bars: at(0.5, 7.5, 15.5, 8.1),
        ramp: at(0.5, 8.2, 15.5, 8.75),
    }
}

/// A picture being painted: RGBA8, opaque.
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Canvas {
        let n = w as usize * h as usize;
        Canvas {
            w,
            h,
            px: [0, 0, 0, 255].repeat(n),
        }
    }

    /// Paints `r` (cut to the picture) with `f(x, y)`, where `x` and `y`
    /// count from the block's corner.
    fn fill_rgb(&mut self, r: Rect, f: impl Fn(u32, u32) -> [u8; 3]) {
        let (x1, y1) = (r.x1.min(self.w), r.y1.min(self.h));
        for y in r.y0..y1 {
            for x in r.x0..x1 {
                let [red, green, blue] = f(x - r.x0, y - r.y0);
                let at = (y as usize * self.w as usize + x as usize) * 4;
                self.px[at..at + 4].copy_from_slice(&[red, green, blue, 255]);
            }
        }
    }

    fn fill_grey(&mut self, r: Rect, f: impl Fn(u32, u32) -> u8) {
        self.fill_rgb(r, |x, y| [f(x, y); 3]);
    }

    fn white(&mut self, r: Rect) {
        self.fill_grey(r, |_, _| 255);
    }

    fn black(&mut self, r: Rect) {
        self.fill_grey(r, |_, _| 0);
    }

    /// A 1 px white line round the picture's edge.
    fn border(&mut self) {
        let (w, h) = (self.w, self.h);
        self.white(Rect::new(0, 0, w, 1));
        self.white(Rect::new(0, h.saturating_sub(1), w, h));
        self.white(Rect::new(0, 0, 1, h));
        self.white(Rect::new(w.saturating_sub(1), 0, w, h));
    }

    /// An L in each corner, just inside the border and clear of the parts.
    fn corner_marks(&mut self) {
        let (w, h) = (self.w, self.h);
        let cell = f64::from(h) / 9.0;
        let inset = ((0.08 * cell).round() as u32).max(2);
        let arm = ((0.3 * cell).round() as u32).max(3);
        let thick = (f64::from(h) / 540.0).round().max(1.0) as u32;
        for right in [false, true] {
            for bottom in [false, true] {
                let along_x = span(w, inset, arm, right);
                let along_y = span(h, inset, arm, bottom);
                let thin_x = span(w, inset, thick, right);
                let thin_y = span(h, inset, thick, bottom);
                self.white(Rect::new(along_x.0, thin_y.0, along_x.1, thin_y.1));
                self.white(Rect::new(thin_x.0, along_y.0, thin_x.1, along_y.1));
            }
        }
    }

    /// A 1 px white cross in the middle, outlined in black so it shows on
    /// the zone plate's bright core.
    fn cross(&mut self) {
        let (mx, my) = (self.w / 2, self.h / 2);
        let len = ((0.35 * f64::from(self.h) / 9.0).round() as u32).max(3);
        let (x0, x1) = (mx.saturating_sub(len), mx + len + 1);
        let (y0, y1) = (my.saturating_sub(len), my + len + 1);
        self.black(Rect::new(x0, my.saturating_sub(1), x1, my + 2));
        self.black(Rect::new(mx.saturating_sub(1), y0, mx + 2, y1));
        self.white(Rect::new(x0, my, x1, my + 1));
        self.white(Rect::new(mx, y0, mx + 1, y1));
    }
}

/// `len` pixels from `extent`'s near edge, or its far edge, `inset` in.
fn span(extent: u32, inset: u32, len: u32, far: bool) -> (u32, u32) {
    if far {
        let end = extent.saturating_sub(inset);
        (end.saturating_sub(len), end)
    } else {
        (inset, inset + len)
    }
}

/// The direction of a wedge's lines.
#[derive(Clone, Copy)]
enum Lines {
    Horizontal,
    Vertical,
    Diagonal,
}

const WEDGES: [Lines; 3] = [Lines::Horizontal, Lines::Vertical, Lines::Diagonal];

/// One pixel of a wedge: lines 1 px wide left of `half`, 2 px wide from
/// there on. At 45 degrees, 1 px lines are the same as a 1-texel checker.
fn wedge(kind: Lines, half: u32, x: u32, y: u32) -> u8 {
    let (u, period) = if x < half { (x, 1) } else { (x - half, 2) };
    let along = match kind {
        Lines::Horizontal => y,
        Lines::Vertical => u,
        Lines::Diagonal => u + y,
    };
    if (along / period).is_multiple_of(2) {
        255
    } else {
        0
    }
}

/// White where the cell's column and row add up to an even number.
fn checker_value(x: u32, y: u32, cell: u32) -> u8 {
    let cell = cell.max(1);
    if (x / cell + y / cell).is_multiple_of(2) {
        255
    } else {
        0
    }
}

/// Column `x` of a `w` wide ramp from `lo` to `hi`, rounded to a code.
fn ramp_value(x: u32, w: u32, lo: u8, hi: u8) -> u8 {
    if w <= 1 {
        return lo;
    }
    let rise = u32::from(hi.max(lo) - lo);
    let steps = w - 1;
    (u32::from(lo) + (rise * x.min(steps) * 2 + steps) / (steps * 2)) as u8
}

/// A pixel's offset from the middle of an `extent` wide picture.
fn centred(x: u32, extent: u32) -> f64 {
    f64::from(x) + 0.5 - f64::from(extent) / 2.0
}

/// The zone plate's phase at `dx`, `dy` pixels from its centre, in a square
/// `side` pixels wide. The phase is `k r^2` with `k = pi / side`, so the
/// pattern's frequency along an axis (`k d / pi` cycles per pixel) reaches
/// the Nyquist limit of half a cycle at the square's edge, and the phase
/// never steps by `pi` or more between neighbouring pixels inside it: the
/// card itself can't alias.
fn zone_phase(side: u32, dx: f64, dy: f64) -> f64 {
    PI / f64::from(side.max(1)) * (dx * dx + dy * dy)
}

/// `0.5 + 0.5 cos(phase)` as a code.
fn zone_value(side: u32, dx: f64, dy: f64) -> u8 {
    (127.5 + 127.5 * zone_phase(side, dx, dy).cos()).round() as u8
}

/// The whole card, `w`x`h` RGBA8: made for 16:9 and fine at any size.
///
/// Reading it: the zone plate in the middle sweeps from flat to the finest
/// detail the picture can hold at its edges (rings beyond that are the
/// viewer's, not the card's); the three panels to its left are line wedges
/// and to its right a 1-texel checker, a 2-texel checker and a flat grey of
/// what the first should average to. Along the top are 32 grey steps and at
/// the bottom colour bars and a dark ramp. A 1 px white border, an L in each
/// corner and a cross in the centre show where the picture's edges are.
pub fn card(w: u32, h: u32) -> Vec<u8> {
    let mut c = Canvas::new(w, h);
    let l = layout(w, h);
    let side = l.zone.width();
    let zone = |x, y| zone_value(side, centred(x, side), centred(y, side));
    c.fill_grey(l.zone, zone);
    for (r, kind) in l.wedges.into_iter().zip(WEDGES) {
        let half = r.width() / 2;
        c.fill_grey(r, |x, y| wedge(kind, half, x, y));
    }
    let [one, two] = l.checkers;
    c.fill_grey(one, |x, y| checker_value(x, y, 1));
    c.fill_grey(two, |x, y| checker_value(x, y, 2));
    c.fill_grey(l.grey_patch, |_, _| 128);
    let steps = l.steps.width();
    c.fill_grey(l.steps, |x, _| grey_level(x * GREY_STEPS / steps));
    let bars = l.bars.width();
    c.fill_rgb(l.bars, |x, _| BARS[(x * 8 / bars) as usize]);
    let ramp = l.ramp.width();
    c.fill_grey(l.ramp, |x, _| ramp_value(x, ramp, 0, DARK_TOP));
    c.border();
    c.corner_marks();
    c.cross();
    c.px
}

/// The code of grey step `step` (0 is black, [`GREY_STEPS`] - 1 white).
pub fn grey_level(step: u32) -> u8 {
    ((step.min(GREY_STEPS - 1) * 510 + 31) / 62) as u8
}

/// A `w`x`h` checker of `cell`-pixel squares, white in the top left corner
/// and black next to it.
pub fn checker(w: u32, h: u32, cell: u32) -> Vec<u8> {
    let mut c = Canvas::new(w, h);
    c.fill_grey(Rect::full(w, h), |x, y| checker_value(x, y, cell));
    c.px
}

/// A zone plate over the whole `w`x`h` picture: a circular chirp
/// `0.5 + 0.5 cos(k r^2)`, centred, that reaches the Nyquist limit at the
/// edge of the largest square that fits. Past that square (a picture that
/// isn't square) it keeps sweeping into aliasing.
pub fn zone_plate(w: u32, h: u32) -> Vec<u8> {
    let mut c = Canvas::new(w, h);
    let side = w.min(h);
    let paint = |x, y| zone_value(side, centred(x, w), centred(y, h));
    c.fill_grey(Rect::full(w, h), paint);
    c.px
}

/// A `w`x`h` ramp from code `lo` on the left to `hi` on the right, rounded
/// to whole codes, so it climbs in bands about `w / (hi - lo)` pixels wide.
/// A `hi` below `lo` is raised to it.
pub fn ramp(w: u32, h: u32, lo: u8, hi: u8) -> Vec<u8> {
    let mut c = Canvas::new(w, h);
    c.fill_grey(Rect::full(w, h), |x, _| ramp_value(x, w, lo, hi));
    c.px
}

/// How many levels a full mip chain of a `w`x`h` picture has.
pub fn mip_count(w: u32, h: u32) -> u32 {
    32 - w.max(h).leading_zeros()
}

/// The picture followed by each half-size level down to 1x1, as
/// `(width, height, RGBA8)`: every texel is the rounded mean of a 2x2 block
/// of the one above, per channel, in codes. A side that is odd loses its
/// last row or column, as Vulkan sizes its levels. Empty if `rgba` isn't
/// `w * h * 4` bytes.
pub fn box_mips(rgba: &[u8], w: u32, h: u32) -> Vec<(u32, u32, Vec<u8>)> {
    if w == 0 || h == 0 || rgba.len() != w as usize * h as usize * 4 {
        return Vec::new();
    }
    let mut levels = vec![(w, h, rgba.to_vec())];
    loop {
        let (lw, lh, px) = levels.last().expect("starts with one level");
        if *lw == 1 && *lh == 1 {
            break;
        }
        let next = halve(px, *lw, *lh);
        levels.push(next);
    }
    levels
}

/// The level below a `w`x`h` one.
fn halve(src: &[u8], w: u32, h: u32) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let texel = |x: u32, y: u32, c: usize| {
        u32::from(src[(y as usize * w as usize + x as usize) * 4 + c])
    };
    let mut out = Vec::with_capacity(nw as usize * nh as usize * 4);
    for y in 0..nh {
        let (y0, y1) = (2 * y, (2 * y + 1).min(h - 1));
        for x in 0..nw {
            let (x0, x1) = (2 * x, (2 * x + 1).min(w - 1));
            for c in 0..4 {
                let top = texel(x0, y0, c) + texel(x1, y0, c);
                let bottom = texel(x0, y1, c) + texel(x1, y1, c);
                out.push(((top + bottom + 2) / 4) as u8);
            }
        }
    }
    (nw, nh, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
            (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    fn texel(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let at = (y as usize * w as usize + x as usize) * 4;
        [img[at], img[at + 1], img[at + 2], img[at + 3]]
    }

    fn grey(img: &[u8], w: u32, x: u32, y: u32) -> u8 {
        texel(img, w, x, y)[0]
    }

    #[test]
    fn card_has_the_asked_size_and_is_opaque() {
        for (w, h) in [(16, 9), (100, 57), (320, 180), (1, 1), (3, 200), (0, 5)] {
            let img = card(w, h);
            assert_eq!(img.len(), w as usize * h as usize * 4, "{w}x{h}");
            assert!(img.chunks(4).all(|p| p[3] == 255), "{w}x{h}");
        }
    }

    #[test]
    fn card_is_the_same_every_time() {
        assert_eq!(card(320, 180), card(320, 180));
    }

    #[test]
    fn checker_and_ramp_are_stable() {
        // Whole-number maths only, so these hold on every machine.
        assert_eq!(fnv(&checker(16, 16, 2)), 0xa938_299a_bffb_4d25);
        assert_eq!(fnv(&ramp(64, 2, 0, DARK_TOP)), 0x43e5_18f1_696a_04b5);
    }

    #[test]
    fn checker_alternates_by_cell() {
        let one = checker(8, 8, 1);
        for y in 0..8 {
            for x in 0..8 {
                let want = if (x + y).is_multiple_of(2) { 255 } else { 0 };
                assert_eq!(grey(&one, 8, x, y), want, "({x}, {y})");
            }
        }
        let two = checker(8, 8, 2);
        assert_eq!(grey(&two, 8, 0, 0), 255);
        assert_eq!(grey(&two, 8, 1, 1), 255);
        assert_eq!(grey(&two, 8, 2, 0), 0);
        assert_eq!(grey(&two, 8, 0, 2), 0);
        assert_eq!(grey(&two, 8, 2, 2), 255);
    }

    #[test]
    fn checker_cell_zero_is_one() {
        assert_eq!(checker(4, 4, 0), checker(4, 4, 1));
    }

    #[test]
    fn ramp_climbs_from_lo_to_hi_without_a_step_back() {
        let img = ramp(300, 1, 0, DARK_TOP);
        let row: Vec<u8> = (0..300).map(|x| grey(&img, 300, x, 0)).collect();
        assert_eq!(row[0], 0);
        assert_eq!(row[299], DARK_TOP);
        assert!(row.windows(2).all(|p| p[0] <= p[1]), "monotonic");
        assert!(row.windows(2).all(|p| p[1] - p[0] <= 1), "no jumps");
        let mut codes = row.clone();
        codes.dedup();
        assert_eq!(codes.len(), usize::from(DARK_TOP) + 1, "every code is used");
    }

    #[test]
    fn ramp_edge_cases() {
        // One column is `lo`; a `hi` below `lo` is flat at `lo`.
        assert_eq!(grey(&ramp(1, 1, 7, 90), 1, 0, 0), 7);
        let flat = ramp(10, 1, 50, 20);
        assert!((0..10).all(|x| grey(&flat, 10, x, 0) == 50));
        let full = ramp(256, 1, 0, 255);
        assert!((0..256).all(|x| usize::from(grey(&full, 256, x, 0)) == x as usize));
    }

    #[test]
    fn zone_phase_steps_by_less_than_pi_between_neighbours() {
        for side in [32, 64, 65, 100] {
            let at = |x: u32, y: u32| zone_phase(side, centred(x, side), centred(y, side));
            for y in 0..side {
                for x in 0..side - 1 {
                    let across = at(x + 1, y) - at(x, y);
                    let down = at(y, x + 1) - at(y, x);
                    assert!(across.abs() < PI, "side {side} ({x}, {y}): {across}");
                    assert!(down.abs() < PI, "side {side} ({y}, {x}): {down}");
                }
            }
        }
    }

    #[test]
    fn zone_phase_reaches_nyquist_at_the_edge() {
        // The step across the last pair of pixels is close to pi: half a
        // cycle per pixel, the finest the picture can hold.
        let side = 64;
        let last = zone_phase(side, centred(side - 1, side), 0.0);
        let before = zone_phase(side, centred(side - 2, side), 0.0);
        assert!(last - before > 0.9 * PI, "{}", last - before);
        // And it starts flat at the centre.
        let mid = zone_phase(side, centred(side / 2, side), 0.0);
        let next = zone_phase(side, centred(side / 2 + 1, side), 0.0);
        assert!(next - mid < 0.2 * PI);
    }

    #[test]
    fn zone_plate_is_bright_in_the_middle_and_busy_at_the_edge() {
        let (w, h) = (128, 128);
        let img = zone_plate(w, h);
        assert_eq!(img.len(), 128 * 128 * 4);
        assert!(grey(&img, w, 64, 64) >= 253);
        // Neighbouring pixels differ little in the middle and a lot near
        // the left edge (the mean difference, over a block of rows).
        let busy = |x0: u32| -> u32 {
            (48..80)
                .map(|y| {
                    let (a, b) = (grey(&img, w, x0, y), grey(&img, w, x0 + 1, y));
                    u32::from(a.abs_diff(b))
                })
                .sum()
        };
        let (edge, middle) = (busy(2), busy(62));
        assert!(edge > 4 * middle, "edge {edge} middle {middle}");
    }

    #[test]
    fn card_border_is_one_white_pixel() {
        let (w, h) = (320, 180);
        let img = card(w, h);
        let white = [255, 255, 255, 255];
        for x in 0..w {
            assert_eq!(texel(&img, w, x, 0), white, "top {x}");
            assert_eq!(texel(&img, w, x, h - 1), white, "bottom {x}");
        }
        for y in 0..h {
            assert_eq!(texel(&img, w, 0, y), white, "left {y}");
            assert_eq!(texel(&img, w, w - 1, y), white, "right {y}");
        }
    }

    #[test]
    fn card_corners_carry_an_l_mark() {
        let (w, h) = (320, 180);
        let img = card(w, h);
        // Inside the border, in each corner's 8 px block: an L of two arms
        // (6 px each, sharing a texel) is 11 white texels.
        for (x0, y0) in [(1, 1), (w - 9, 1), (1, h - 9), (w - 9, h - 9)] {
            let white = (0..8)
                .flat_map(|y| (0..8).map(move |x| (x, y)))
                .filter(|&(x, y)| grey(&img, w, x0 + x, y0 + y) == 255)
                .count();
            assert_eq!(white, 11, "corner at ({x0}, {y0})");
        }
    }

    #[test]
    fn card_has_a_cross_in_the_middle() {
        let (w, h) = (320, 180);
        let img = card(w, h);
        let (mx, my) = (w / 2, h / 2);
        assert_eq!(grey(&img, w, mx, my), 255);
        // The white line, and the black outline either side of it.
        assert_eq!(grey(&img, w, mx + 5, my), 255);
        assert_eq!(grey(&img, w, mx + 5, my + 1), 0);
        assert_eq!(grey(&img, w, mx + 5, my - 1), 0);
        assert_eq!(grey(&img, w, mx, my + 5), 255);
        assert_eq!(grey(&img, w, mx + 1, my + 5), 0);
    }

    #[test]
    fn card_parts_do_not_overlap_and_stay_inside() {
        for (w, h) in [(320, 180), (1920, 1080), (400, 400), (640, 360)] {
            let l = layout(w, h);
            let mut all = vec![l.zone, l.grey_patch, l.steps, l.ramp, l.bars];
            all.extend(l.wedges);
            all.extend(l.checkers);
            for (i, a) in all.iter().enumerate() {
                assert!(a.x1 <= w && a.y1 <= h, "{w}x{h} part {i} {a:?}");
                assert!(a.width() > 0 && a.height() > 0, "{w}x{h} part {i} {a:?}");
                for b in &all[i + 1..] {
                    let apart = a.x1 <= b.x0 || b.x1 <= a.x0 || a.y1 <= b.y0 || b.y1 <= a.y0;
                    assert!(apart, "{w}x{h}: {a:?} meets {b:?}");
                }
            }
            // Square, and centred.
            assert_eq!(l.zone.width(), l.zone.height());
            assert!(l.zone.x0.abs_diff(w - l.zone.x1) <= 1);
            assert!(l.zone.y0.abs_diff(h - l.zone.y1) <= 1);
        }
    }

    #[test]
    fn card_zone_plate_matches_the_pattern() {
        let (w, h) = (640, 360);
        let img = card(w, h);
        let z = layout(w, h).zone;
        let side = z.width();
        for (x, y) in [(3, 5), (40, 7), (100, 150), (side - 4, 20)] {
            let want = zone_value(side, centred(x, side), centred(y, side));
            assert_eq!(grey(&img, w, z.x0 + x, z.y0 + y), want, "({x}, {y})");
        }
    }

    #[test]
    fn card_grey_scale_has_32_steps_in_order() {
        let (w, h) = (320, 180);
        let img = card(w, h);
        let s = layout(w, h).steps;
        let y = s.y0 + s.height() / 2;
        let mut row: Vec<u8> = (s.x0..s.x1).map(|x| grey(&img, w, x, y)).collect();
        row.dedup();
        let want: Vec<u8> = (0..GREY_STEPS).map(grey_level).collect();
        assert_eq!(row, want);
        assert_eq!((want[0], want[31]), (0, 255));
    }

    #[test]
    fn card_colour_bars_are_at_full_strength() {
        let (w, h) = (320, 180);
        let img = card(w, h);
        let b = layout(w, h).bars;
        let y = b.y0 + b.height() / 2;
        for (i, want) in BARS.iter().enumerate() {
            let x = b.x0 + (2 * i as u32 + 1) * b.width() / 16;
            let [r, g, bl, a] = texel(&img, w, x, y);
            assert_eq!([r, g, bl], *want, "bar {i}");
            assert_eq!(a, 255);
        }
    }

    #[test]
    fn card_dark_ramp_runs_from_black_to_ten_percent() {
        let (w, h) = (320, 180);
        let img = card(w, h);
        let r = layout(w, h).ramp;
        let y = r.y0 + r.height() / 2;
        let row: Vec<u8> = (r.x0..r.x1).map(|x| grey(&img, w, x, y)).collect();
        assert_eq!((row[0], row[row.len() - 1]), (0, DARK_TOP));
        assert!(row.windows(2).all(|p| p[0] <= p[1]));
        assert!((f32::from(DARK_TOP) / 255.0 - 0.10).abs() < 0.005);
    }

    #[test]
    fn card_checker_patches_and_grey_reference() {
        let (w, h) = (320, 180);
        let img = card(w, h);
        let l = layout(w, h);
        let [one, two] = l.checkers;
        let at = |r: Rect, x: u32, y: u32| grey(&img, w, r.x0 + x, r.y0 + y);
        let first = [at(one, 0, 0), at(one, 1, 0), at(one, 0, 1)];
        assert_eq!(first, [255, 0, 0]);
        assert_eq!(at(one, 1, 1), 255);
        let second = [at(two, 0, 0), at(two, 1, 1), at(two, 2, 0)];
        assert_eq!(second, [255, 255, 0]);
        let g = l.grey_patch;
        assert_eq!(at(g, 0, 0), 128);
        assert_eq!(at(g, g.width() - 1, g.height() - 1), 128);
    }

    #[test]
    fn card_wedges_have_lines_one_and_two_pixels_wide() {
        let (w, h) = (320, 180);
        let img = card(w, h);
        let [horizontal, vertical, diagonal] = layout(w, h).wedges;
        let half = horizontal.width() / 2;
        let at = |r: Rect, x: u32, y: u32| grey(&img, w, r.x0 + x, r.y0 + y);
        // Horizontal lines: rows alternate, in pairs on the right.
        assert_eq!([at(horizontal, 1, 0), at(horizontal, 1, 1)], [255, 0]);
        let right = [0, 1, 2, 3].map(|y| at(horizontal, half + 1, y));
        assert_eq!(right, [255, 255, 0, 0]);
        // Vertical lines: columns alternate, in pairs on the right.
        assert_eq!([at(vertical, 0, 1), at(vertical, 1, 1)], [255, 0]);
        let right = [0, 1, 2, 3].map(|x| at(vertical, half + x, 1));
        assert_eq!(right, [255, 255, 0, 0]);
        // Diagonal: constant along a 45 degree line.
        assert_eq!(at(diagonal, 4, 3), at(diagonal, 3, 4));
        assert_eq!(at(diagonal, half + 4, 3), at(diagonal, half + 3, 4));
        let right = [0, 1, 2, 3].map(|x| at(diagonal, half + x, 0));
        assert_eq!(right, [255, 255, 0, 0]);
    }

    #[test]
    fn mip_chain_has_a_level_per_halving() {
        let img = card(1920, 1080);
        let levels = box_mips(&img, 1920, 1080);
        assert_eq!(levels.len(), 11);
        assert_eq!(levels.len() as u32, mip_count(1920, 1080));
        let sizes: Vec<(u32, u32)> = levels.iter().map(|l| (l.0, l.1)).collect();
        assert_eq!(sizes[0], (1920, 1080));
        assert_eq!(sizes[1], (960, 540));
        assert_eq!(sizes[4], (120, 67));
        assert_eq!(sizes[10], (1, 1));
        for (w, h, px) in &levels {
            assert_eq!(px.len(), *w as usize * *h as usize * 4);
        }
        // Level 0 is the picture itself.
        assert_eq!(levels[0].2, img);
    }

    #[test]
    fn mip_count_matches_the_chain_for_odd_and_thin_sizes() {
        for (w, h) in [(1, 1), (2, 1), (1, 9), (5, 3), (7, 7), (64, 1), (100, 57)] {
            let img = vec![200; w as usize * h as usize * 4];
            let levels = box_mips(&img, w, h);
            assert_eq!(levels.len() as u32, mip_count(w, h), "{w}x{h}");
            let last = levels.last().unwrap();
            assert_eq!((last.0, last.1), (1, 1), "{w}x{h}");
        }
    }

    #[test]
    fn mips_average_two_by_two() {
        let one = checker(8, 8, 1);
        let levels = box_mips(&one, 8, 8);
        assert_eq!(levels.len(), 4);
        // (255 + 0 + 0 + 255 + 2) / 4 rounds to 128 everywhere below.
        for (w, h, px) in &levels[1..] {
            assert!(
                px.chunks(4).all(|p| p == [128, 128, 128, 255]),
                "{w}x{h}: {:?}",
                &px[..4]
            );
        }
        // A 2-texel checker is still black and white at level 1, grey after.
        let two = box_mips(&checker(8, 8, 2), 8, 8);
        assert_eq!(&two[1].2[..4], [255, 255, 255, 255]);
        assert_eq!(&two[1].2[4..8], [0, 0, 0, 255]);
        assert_eq!(&two[2].2[..4], [128, 128, 128, 255]);
    }

    #[test]
    fn mips_average_every_channel_and_round() {
        // Four texels: channel values 1, 2, 2, 2 -> 7 / 4 = 1.75 -> 2; and
        // 0, 0, 0, 1 -> 0.25 -> 0; alpha 255 throughout.
        let texels = [
            [1, 0, 10, 255],
            [2, 0, 10, 255],
            [2, 0, 10, 255],
            [2, 1, 11, 255],
        ];
        let levels = box_mips(&texels.concat(), 2, 2);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[1], (1, 1, vec![2, 0, 10, 255]));
    }

    #[test]
    fn mips_of_a_flat_picture_stay_flat() {
        let img = [90, 120, 200, 255].repeat(37 * 21);
        for (w, h, px) in box_mips(&img, 37, 21) {
            assert!(px.chunks(4).all(|p| p == [90, 120, 200, 255]), "{w}x{h}");
        }
    }

    #[test]
    fn mips_reject_a_wrong_sized_picture() {
        assert!(box_mips(&[0; 12], 2, 2).is_empty());
        assert!(box_mips(&[], 0, 0).is_empty());
    }
}
