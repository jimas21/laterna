//! Sound from the screen: each channel of a flat video is a virtual speaker
//! in the room, and the speakers are rendered to two ears for the way the
//! head is turned, so the sound stays where the screen is.
//!
//! Stereo's left and right sit at the screen's left and right edges (on its
//! curve), the centre in the middle of it, and 5.1 and 7.1 surrounds beside
//! and behind the viewer at ITU-R BS.775 angles measured from the direction
//! of the screen. The LFE has no direction. Stereo can get a gentle widening
//! between its two front speakers; surrounds are never made up.
//!
//! Each speaker is placed by its own direction, not through ambisonics, which
//! would smear left, centre and right into one another: an interaural time
//! difference (a fractional delay on the far ear), an interaural level
//! difference, and a low-pass that closes as the source goes round to the far
//! side or behind the head. A small feedback delay network gives a room its
//! echo. Everything is a handful of multiply-adds per sample and channel.

use super::ambisonic::{IDENTITY, Quat, rotate_inverse};

/// Sample rate the renderer works at.
const FS: f32 = super::RATE as f32;
/// Speed of sound, m/s, and the head's radius, m (Woodworth's model).
const SOUND_SPEED: f32 = 343.0;
const HEAD_RADIUS: f32 = 0.0875;
/// Longest interaural delay: a source at the side, in samples (about 32).
const MAX_ITD: f32 = HEAD_RADIUS / SOUND_SPEED * (std::f32::consts::FRAC_PI_2 + 1.0) * FS;
/// Level difference between the ears for a source at the side, dB.
const ILD_DB: f32 = 6.0;
/// Where the surrounds sit, degrees from the screen's direction (BS.775: 5.1
/// surrounds 100 to 120, 7.1 sides at 90 and backs at 150).
const SURROUND_51: f32 = 110.0;
const SIDE_71: f32 = 90.0;
const BACK_71: f32 = 150.0;
/// Where the LFE goes in each ear.
const LFE_GAIN: f32 = 0.5;
/// Values this small are flushed to zero so filters never run on denormals.
const TINY: f32 = 1e-18;
/// History kept for the delay, a power of two above the longest delay.
const HIST: usize = 64;

/// The screen in the room, for placing speakers: where its left edge, middle
/// and right edge are, in metres from the listener, in the same frame the
/// head's turn is measured in (OpenXR axes: x right, y up, -z ahead).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenAudio {
    pub left: [f32; 3],
    pub centre: [f32; 3],
    pub right: [f32; 3],
}

impl ScreenAudio {
    /// A flat screen `width` metres wide, `distance` metres straight ahead.
    pub fn flat(width: f32, distance: f32) -> ScreenAudio {
        ScreenAudio {
            left: [-width / 2.0, 0.0, -distance],
            centre: [0.0, 0.0, -distance],
            right: [width / 2.0, 0.0, -distance],
        }
    }
}

/// How a place sounds. Packs name one of these.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Acoustics {
    /// No echo: plain video and the real room in passthrough.
    #[default]
    Dry,
    /// A small room.
    Room,
    /// A large hall, like a cinema.
    Hall,
}

impl Acoustics {
    /// The tail's length (to 60 dB down), seconds.
    fn rt60(self) -> f32 {
        match self {
            Acoustics::Dry => 0.0,
            Acoustics::Room => 0.45,
            Acoustics::Hall => 1.7,
        }
    }
    /// The echo's size: the delay lines' lengths are scaled by this.
    fn size(self) -> f32 {
        match self {
            Acoustics::Dry => 0.0,
            Acoustics::Room => 0.4,
            Acoustics::Hall => 1.0,
        }
    }
    /// The echo's own gain with the echo level at its top (1).
    fn wet_max(self) -> f32 {
        match self {
            Acoustics::Dry => 0.0,
            Acoustics::Room => 0.4,
            Acoustics::Hall => 0.4,
        }
    }
    /// Seconds between the sound and its first echo.
    fn predelay(self) -> f32 {
        match self {
            Acoustics::Dry => 0.0,
            Acoustics::Room => 0.006,
            Acoustics::Hall => 0.022,
        }
    }
}

/// What the renderer is told, every few milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpatialParams {
    pub screen: ScreenAudio,
    pub acoustics: Acoustics,
    /// How much echo, 0 (none) to 1.
    pub echo: f32,
    /// Stereo's side level: 1 leaves it as mixed, above 1 widens a little.
    pub widen: f32,
}

impl SpatialParams {
    /// A flat screen straight ahead with no echo and no widening.
    pub fn dry(screen: ScreenAudio) -> SpatialParams {
        SpatialParams {
            screen,
            acoustics: Acoustics::Dry,
            echo: 0.0,
            widen: 1.0,
        }
    }
}

/// A source channel layout the renderer places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Mono,
    Stereo,
    /// FL FR FC LFE SL SR
    Surround51,
    /// FL FR FC LFE BL BR SL SR
    Surround71,
}

impl Layout {
    /// The layout of this many channels, if it is one that is placed.
    pub fn of(channels: usize) -> Option<Layout> {
        match channels {
            1 => Some(Layout::Mono),
            2 => Some(Layout::Stereo),
            6 => Some(Layout::Surround51),
            8 => Some(Layout::Surround71),
            _ => None,
        }
    }

    pub fn channels(self) -> usize {
        match self {
            Layout::Mono => 1,
            Layout::Stereo => 2,
            Layout::Surround51 => 6,
            Layout::Surround71 => 8,
        }
    }

    /// What a channel is worth in the mix: the more speakers, the less each.
    fn trim(self) -> f32 {
        match self {
            Layout::Mono => 0.8,
            Layout::Stereo => 0.7,
            Layout::Surround51 => 0.55,
            Layout::Surround71 => 0.5,
        }
    }
}

/// Where one channel is heard from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    /// A unit vector from the listener, in the frame of [`ScreenAudio`].
    At([f32; 3]),
    /// Everywhere at once.
    Lfe,
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-6 && l.is_finite() {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, -1.0]
    }
}

/// Where each channel of `layout` sits for this screen, in channel order.
pub fn places(layout: Layout, screen: &ScreenAudio) -> Vec<Place> {
    let l = Place::At(norm(screen.left));
    let r = Place::At(norm(screen.right));
    let c = Place::At(norm(screen.centre));
    // The way the screen faces from the listener, level with the floor.
    let front = {
        let f = [screen.centre[0], 0.0, screen.centre[2]];
        if f[0] * f[0] + f[2] * f[2] < 1e-6 {
            [0.0, 0.0, -1.0]
        } else {
            norm(f)
        }
    };
    // Right of `front`: front x up.
    let right = [-front[2], 0.0, front[0]];
    // `deg` from the front, towards the right when positive.
    let at = |deg: f32| {
        let (s, c) = deg.to_radians().sin_cos();
        Place::At([
            front[0] * c + right[0] * s,
            0.0,
            front[2] * c + right[2] * s,
        ])
    };
    match layout {
        Layout::Mono => vec![c],
        Layout::Stereo => vec![l, r],
        Layout::Surround51 => vec![l, r, c, Place::Lfe, at(-SURROUND_51), at(SURROUND_51)],
        Layout::Surround71 => vec![
            l,
            r,
            c,
            Place::Lfe,
            at(-BACK_71),
            at(BACK_71),
            at(-SIDE_71),
            at(SIDE_71),
        ],
    }
}

/// What one speaker does to the sound at one moment: a delay for each ear,
/// a gain, and how far that ear's low-pass is open.
#[derive(Clone, Copy, Default)]
struct Ears {
    delay: [f32; 2],
    gain: [f32; 2],
    open: [f32; 2],
}

impl Ears {
    /// From a direction in the head's frame (x right, y up, z behind).
    fn of(dir: [f32; 3]) -> Ears {
        let lat = dir[0].clamp(-1.0, 1.0);
        let theta = lat.asin();
        let itd = HEAD_RADIUS / SOUND_SPEED * (theta + theta.sin()) * FS;
        // Which ear is on the side of the source: +1 near, -1 far.
        let near = [-lat, lat];
        let behind = dir[2].clamp(0.0, 1.0);
        let mut e = Ears::default();
        for (ear, &s) in near.iter().enumerate() {
            // The far ear hears it later.
            e.delay[ear] = if s < 0.0 { itd.abs().min(MAX_ITD) } else { 0.0 };
            e.gain[ear] = 10f32.powf(ILD_DB / 2.0 * s / 20.0) * (1.0 - 0.2 * behind);
            let fc = ((12_000.0 + 9_000.0 * s) * (1.0 - 0.5 * behind)).clamp(1_500.0, 20_000.0);
            e.open[ear] = 1.0 - (-std::f32::consts::TAU * fc / FS).exp();
        }
        e
    }

    fn lerp(a: &Ears, b: &Ears, t: f32) -> Ears {
        let mix = |x: [f32; 2], y: [f32; 2]| [x[0] + (y[0] - x[0]) * t, x[1] + (y[1] - x[1]) * t];
        Ears {
            delay: mix(a.delay, b.delay),
            gain: mix(a.gain, b.gain),
            open: mix(a.open, b.open),
        }
    }
}

/// One channel's memory: its recent input, for the delay, and its two
/// low-passes.
struct Voice {
    hist: [f32; HIST],
    pos: usize,
    low: [f32; 2],
    ears: Ears,
}

impl Voice {
    fn new() -> Voice {
        Voice {
            hist: [0.0; HIST],
            pos: 0,
            low: [0.0; 2],
            ears: Ears::default(),
        }
    }
}

/// A feedback delay network: eight lines mixed through a Hadamard matrix.
struct Reverb {
    lines: Vec<Vec<f32>>,
    heads: [usize; 8],
    gains: [f32; 8],
    damp: [f32; 8],
    damp_coef: f32,
    pre: Vec<f32>,
    pre_pos: usize,
    pre_len: usize,
    kind: Acoustics,
    wet: f32,
}

/// Delay line lengths for a hall at 48 kHz: mutually prime, 21 to 64 ms.
const LINES: [usize; 8] = [1031, 1327, 1523, 1801, 2111, 2437, 2713, 3079];
const PRE_MAX: usize = 2048;

impl Reverb {
    fn new() -> Reverb {
        Reverb {
            lines: Vec::new(),
            heads: [0; 8],
            gains: [0.0; 8],
            damp: [0.0; 8],
            damp_coef: 0.0,
            pre: vec![0.0; PRE_MAX],
            pre_pos: 0,
            pre_len: 0,
            kind: Acoustics::Dry,
            wet: 0.0,
        }
    }

    /// Sets up for `kind`, starting from silence when it changes.
    fn configure(&mut self, kind: Acoustics) {
        if kind == self.kind {
            return;
        }
        self.kind = kind;
        self.heads = [0; 8];
        self.damp = [0.0; 8];
        self.pre.iter_mut().for_each(|s| *s = 0.0);
        self.pre_len = ((kind.predelay() * FS) as usize).min(PRE_MAX - 1);
        if kind == Acoustics::Dry {
            self.lines.clear();
            return;
        }
        self.lines = LINES
            .iter()
            .map(|&n| vec![0.0; ((n as f32 * kind.size()) as usize | 1).max(31)])
            .collect();
        for (i, l) in self.lines.iter().enumerate() {
            // 60 dB down after rt60 seconds.
            self.gains[i] = 10f32.powf(-3.0 * l.len() as f32 / (FS * kind.rt60()));
        }
        // High frequencies die faster, as in a room full of seats.
        self.damp_coef = (-std::f32::consts::TAU * 4_500.0 / FS).exp();
    }

    /// Adds the echo of `send` (one value per frame) to `out` (stereo).
    fn process(&mut self, send: &[f32], wet: f32, out: &mut [f32]) {
        if self.lines.is_empty() {
            return;
        }
        let from = self.wet;
        let step = (wet - from) / send.len().max(1) as f32;
        let mut w = from;
        let norm = 1.0 / 8f32.sqrt();
        for (n, &x) in send.iter().enumerate() {
            w += step;
            // Pre-delay.
            self.pre[self.pre_pos] = x;
            let read = (self.pre_pos + PRE_MAX - self.pre_len) % PRE_MAX;
            let input = self.pre[read];
            self.pre_pos = (self.pre_pos + 1) % PRE_MAX;
            // What comes out of the lines, darkened.
            let mut v = [0.0f32; 8];
            for i in 0..8 {
                let y = self.lines[i][self.heads[i]];
                let d = (1.0 - self.damp_coef) * y + self.damp_coef * self.damp[i];
                self.damp[i] = if d.abs() < TINY { 0.0 } else { d };
                v[i] = self.damp[i] * self.gains[i];
            }
            // Where it is heard from: alternate lines to each ear.
            let (mut l, mut r) = (0.0, 0.0);
            for i in 0..4 {
                l += v[2 * i] * if i % 2 == 0 { 1.0 } else { -1.0 };
                r += v[2 * i + 1] * if i < 2 { 1.0 } else { -1.0 };
            }
            out[2 * n] += l * 0.5 * w;
            out[2 * n + 1] += r * 0.5 * w;
            // Hadamard mixing, then back into the lines.
            let mut h = 1;
            while h < 8 {
                for i in (0..8).step_by(h * 2) {
                    for j in i..i + h {
                        let (a, b) = (v[j], v[j + h]);
                        v[j] = a + b;
                        v[j + h] = a - b;
                    }
                }
                h *= 2;
            }
            for i in 0..8 {
                let s = v[i] * norm + input;
                self.lines[i][self.heads[i]] = if s.abs() < TINY { 0.0 } else { s };
                self.heads[i] += 1;
                if self.heads[i] == self.lines[i].len() {
                    self.heads[i] = 0;
                }
            }
        }
        self.wet = wet;
    }
}

/// Renders the channels of a flat video to two ears for the head's turn.
pub struct Spatializer {
    layout: Option<Layout>,
    voices: Vec<Voice>,
    reverb: Reverb,
    fresh: bool,
    send: Vec<f32>,
}

impl Default for Spatializer {
    fn default() -> Self {
        Spatializer::new()
    }
}

impl Spatializer {
    pub fn new() -> Spatializer {
        Spatializer {
            layout: None,
            voices: Vec::new(),
            reverb: Reverb::new(),
            fresh: true,
            send: Vec::new(),
        }
    }

    /// Forgets the sound heard so far (after a seek).
    pub fn reset(&mut self) {
        self.layout = None;
        self.reverb = Reverb::new();
    }

    /// Renders `input`, interleaved with `channels` channels, to interleaved
    /// stereo appended to `out`. A channel count that is not a placed layout
    /// is returned as it is when stereo and dropped otherwise (the caller
    /// asks for layouts [`Layout::of`] knows).
    pub fn process(
        &mut self,
        input: &[f32],
        channels: usize,
        head: Quat,
        params: &SpatialParams,
        out: &mut Vec<f32>,
    ) {
        let Some(layout) = Layout::of(channels) else {
            return;
        };
        let frames = input.len() / channels;
        if frames == 0 {
            return;
        }
        if self.layout != Some(layout) {
            self.layout = Some(layout);
            self.voices = (0..channels).map(|_| Voice::new()).collect();
            self.fresh = true;
        }
        let head = if head.iter().all(|c| c.is_finite()) {
            let n = head.iter().map(|c| c * c).sum::<f32>().sqrt();
            if n > 1e-3 {
                head.map(|c| c / n)
            } else {
                IDENTITY
            }
        } else {
            IDENTITY
        };
        let base = out.len();
        out.resize(base + frames * 2, 0.0);
        let dest = &mut out[base..];
        let trim = layout.trim();
        let places = places(layout, &params.screen);
        let wet = params.acoustics.wet_max() * params.echo.clamp(0.0, 1.0);
        self.reverb.configure(params.acoustics);
        let reverb_on = wet > 0.0 || self.reverb.wet > 0.0;
        self.send.clear();
        if reverb_on {
            self.send.resize(frames, 0.0);
        }
        let send_gain = trim / (channels as f32).sqrt();

        for (ch, place) in places.iter().enumerate() {
            let voice = &mut self.voices[ch];
            match place {
                Place::Lfe => {
                    for f in 0..frames {
                        let x = clean(input[f * channels + ch]) * LFE_GAIN * trim;
                        dest[2 * f] += x;
                        dest[2 * f + 1] += x;
                    }
                }
                Place::At(dir) => {
                    let target = Ears::of(rotate_inverse(head, *dir));
                    if self.fresh {
                        voice.ears = target;
                    }
                    let from = voice.ears;
                    let widen = layout == Layout::Stereo && (params.widen - 1.0).abs() > 1e-3;
                    for f in 0..frames {
                        let base = f * channels;
                        let mut x = clean(input[base + ch]);
                        if widen {
                            // Side up or down, the middle as it was.
                            let other = clean(input[base + (1 - ch)]);
                            let mid = 0.5 * (x + other);
                            x = mid + (x - mid) * params.widen;
                        }
                        let e = Ears::lerp(&from, &target, (f + 1) as f32 / frames as f32);
                        voice.hist[voice.pos] = x;
                        for ear in 0..2 {
                            // A fractional delay by linear interpolation.
                            let d = e.delay[ear];
                            let whole = d as usize;
                            let frac = d - whole as f32;
                            let a = voice.hist[(voice.pos + HIST - whole) % HIST];
                            let b = voice.hist[(voice.pos + HIST - whole - 1) % HIST];
                            let s = a + (b - a) * frac;
                            let z = &mut voice.low[ear];
                            *z += e.open[ear] * (s - *z);
                            if z.abs() < TINY {
                                *z = 0.0;
                            }
                            dest[2 * f + ear] += *z * e.gain[ear] * trim;
                        }
                        voice.pos = (voice.pos + 1) % HIST;
                        if reverb_on {
                            self.send[f] += x * send_gain;
                        }
                    }
                    voice.ears = target;
                }
            }
        }
        self.fresh = false;
        if reverb_on {
            self.reverb.process(&self.send, wet, dest);
        }
        for s in dest.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
    }
}

/// A sample that is a number, and not a vanishing one.
fn clean(x: f32) -> f32 {
    if x.is_finite() && x.abs() > TINY {
        x
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::super::ambisonic::yaw;
    use super::*;
    use std::f32::consts::FRAC_PI_2;
    use std::time::Instant;

    fn params() -> SpatialParams {
        SpatialParams::dry(ScreenAudio::flat(4.0, 3.0))
    }

    /// A deterministic noise in -1..1.
    struct Noise(u32);
    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
            (self.0 >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        }
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }
    /// Energy of one ear of interleaved stereo.
    fn ear(out: &[f32], e: usize) -> f32 {
        rms(&out.iter().skip(e).step_by(2).copied().collect::<Vec<_>>())
    }

    /// Plays `channel` alone (noise) through `layout` for `blocks` blocks.
    fn solo(layout: Layout, channel: usize, head: Quat, p: &SpatialParams) -> Vec<f32> {
        let n = layout.channels();
        let mut s = Spatializer::new();
        let mut noise = Noise(7);
        let mut out = Vec::new();
        for _ in 0..8 {
            let mut input = vec![0.0; 1024 * n];
            for f in 0..1024 {
                input[f * n + channel] = noise.next();
            }
            s.process(&input, n, head, p, &mut out);
        }
        out
    }

    fn dir_of(p: Place) -> [f32; 3] {
        match p {
            Place::At(d) => d,
            Place::Lfe => panic!("no direction"),
        }
    }
    /// Degrees to the right of straight ahead, in the horizontal plane.
    fn azimuth(d: [f32; 3]) -> f32 {
        d[0].atan2(-d[2]).to_degrees()
    }

    #[test]
    fn stereo_speakers_sit_at_the_screen_edges() {
        let screen = ScreenAudio::flat(4.0, 3.0);
        let p = places(Layout::Stereo, &screen);
        let edge = (2.0f32).atan2(3.0).to_degrees();
        assert!((azimuth(dir_of(p[0])) + edge).abs() < 0.1);
        assert!((azimuth(dir_of(p[1])) - edge).abs() < 0.1);
        // A wider, nearer screen spreads them further apart.
        let wide = places(Layout::Stereo, &ScreenAudio::flat(8.0, 2.0));
        assert!(azimuth(dir_of(wide[1])) > edge + 20.0);
    }

    #[test]
    fn a_curved_screens_edges_are_where_the_curve_puts_them() {
        // Edges 2 m out, bent 0.5 m towards the listener.
        let screen = ScreenAudio {
            left: [-2.0, 0.0, -2.5],
            centre: [0.0, 0.0, -3.0],
            right: [2.0, 0.0, -2.5],
        };
        let p = places(Layout::Stereo, &screen);
        assert!((azimuth(dir_of(p[1])) - (2.0f32).atan2(2.5).to_degrees()).abs() < 0.1);
    }

    #[test]
    fn the_centre_is_in_the_middle_of_the_screen() {
        let screen = ScreenAudio {
            left: [0.5, 0.0, -3.0],
            centre: [2.5, 0.0, -3.0],
            right: [4.5, 0.0, -3.0],
        };
        for layout in [Layout::Mono, Layout::Surround51, Layout::Surround71] {
            let p = places(layout, &screen);
            let c = dir_of(p[if layout == Layout::Mono { 0 } else { 2 }]);
            assert_eq!(c, norm(screen.centre));
        }
    }

    #[test]
    fn surrounds_are_at_the_standard_angles_from_the_screen() {
        let s = ScreenAudio::flat(4.0, 3.0);
        let p5 = places(Layout::Surround51, &s);
        assert!(matches!(p5[3], Place::Lfe));
        assert!((azimuth(dir_of(p5[4])) + 110.0).abs() < 0.1);
        assert!((azimuth(dir_of(p5[5])) - 110.0).abs() < 0.1);
        let p7 = places(Layout::Surround71, &s);
        let az: Vec<f32> = [4, 5, 6, 7]
            .iter()
            .map(|&i| azimuth(dir_of(p7[i])))
            .collect();
        assert!(
            (az[0] + 150.0).abs() < 0.1 && (az[1] - 150.0).abs() < 0.1,
            "{az:?}"
        );
        assert!(
            (az[2] + 90.0).abs() < 0.1 && (az[3] - 90.0).abs() < 0.1,
            "{az:?}"
        );
    }

    #[test]
    fn surrounds_follow_the_screens_direction_not_the_world() {
        // The screen is 90 degrees to the right: "front" is the right, so
        // the left surround is behind-ish, 110 degrees round from there.
        let s = ScreenAudio::flat(4.0, 3.0);
        let turned = ScreenAudio {
            left: [3.0, 0.0, 2.0],
            centre: [3.0, 0.0, 0.0],
            right: [3.0, 0.0, -2.0],
        };
        let p = places(Layout::Surround51, &turned);
        let base = places(Layout::Surround51, &s);
        let rot = |d: [f32; 3]| [-d[2], d[1], d[0]]; // 90 degrees to the right
        for i in [4, 5] {
            let want = rot(dir_of(base[i]));
            let got = dir_of(p[i]);
            for k in 0..3 {
                assert!((want[k] - got[k]).abs() < 1e-4, "{want:?} {got:?}");
            }
        }
    }

    #[test]
    fn layouts_are_known_by_their_channel_count() {
        assert_eq!(Layout::of(1), Some(Layout::Mono));
        assert_eq!(Layout::of(2), Some(Layout::Stereo));
        assert_eq!(Layout::of(6), Some(Layout::Surround51));
        assert_eq!(Layout::of(8), Some(Layout::Surround71));
        assert_eq!(Layout::of(3), None);
        for l in [
            Layout::Mono,
            Layout::Stereo,
            Layout::Surround51,
            Layout::Surround71,
        ] {
            assert_eq!(places(l, &params().screen).len(), l.channels());
        }
    }

    #[test]
    fn a_centre_sound_is_even_when_facing_the_screen() {
        let out = solo(Layout::Mono, 0, IDENTITY, &params());
        let (l, r) = (ear(&out, 0), ear(&out, 1));
        assert!((l - r).abs() / l < 0.02, "{l} {r}");
    }

    #[test]
    fn turning_the_head_90_degrees_moves_the_image_to_the_far_ear() {
        // Turned left, the screen is on the right.
        let left_turn = solo(Layout::Mono, 0, yaw(FRAC_PI_2), &params());
        assert!(ear(&left_turn, 1) > ear(&left_turn, 0) * 1.3);
        // Turned right, on the left.
        let right_turn = solo(Layout::Mono, 0, yaw(-FRAC_PI_2), &params());
        assert!(ear(&right_turn, 0) > ear(&right_turn, 1) * 1.3);
    }

    #[test]
    fn the_near_ear_hears_it_first() {
        // An impulse from the centre with the head turned left arrives at
        // the right ear before the left.
        let mut s = Spatializer::new();
        let mut input = vec![0.0; 256];
        input[0] = 1.0;
        let mut out = Vec::new();
        s.process(&input, 1, yaw(FRAC_PI_2), &params(), &mut out);
        let first = |e: usize| {
            out.iter()
                .skip(e)
                .step_by(2)
                .position(|v| v.abs() > 0.02)
                .unwrap()
        };
        let (l, r) = (first(0), first(1));
        assert!(l > r + 15 && l < r + 40, "left {l} right {r}");
    }

    #[test]
    fn stereo_left_is_on_the_left_and_stays_on_the_screen() {
        let p = params();
        let out = solo(Layout::Stereo, 0, IDENTITY, &p);
        assert!(ear(&out, 0) > ear(&out, 1) * 1.3);
        // Turn the head 90 degrees right: the screen is on the left, and the
        // left speaker is further round to the left.
        let out = solo(Layout::Stereo, 0, yaw(-FRAC_PI_2), &p);
        let ratio = ear(&out, 0) / ear(&out, 1);
        assert!(ratio > 1.3, "{ratio}");
        // Turned left, the right speaker is round to the right.
        let out = solo(Layout::Stereo, 1, yaw(FRAC_PI_2), &p);
        assert!(ear(&out, 1) > ear(&out, 0), "right speaker, turned left");
    }

    #[test]
    fn a_surround_is_beside_and_behind_you() {
        let p = SpatialParams::dry(ScreenAudio::flat(4.0, 3.0));
        // The left surround (channel 4) of 5.1 is on the left.
        let out = solo(Layout::Surround51, 4, IDENTITY, &p);
        assert!(ear(&out, 0) > ear(&out, 1) * 1.3);
        // Turn round to face it (about 110 degrees left): it is ahead, so
        // even in the ears.
        let out = solo(Layout::Surround51, 4, yaw(110f32.to_radians()), &p);
        let (l, r) = (ear(&out, 0), ear(&out, 1));
        assert!((l - r).abs() / l < 0.05, "{l} {r}");
    }

    #[test]
    fn the_lfe_has_no_direction() {
        for head in [IDENTITY, yaw(1.0), yaw(-2.0)] {
            let out = solo(Layout::Surround51, 3, head, &params());
            let (l, r) = (ear(&out, 0), ear(&out, 1));
            assert!(l > 0.0 && (l - r).abs() < 1e-6, "{l} {r}");
        }
    }

    #[test]
    fn every_layout_renders_stereo_of_the_right_length() {
        for l in [
            Layout::Mono,
            Layout::Stereo,
            Layout::Surround51,
            Layout::Surround71,
        ] {
            let n = l.channels();
            let mut s = Spatializer::new();
            let mut out = vec![9.0]; // appended to, not replaced
            s.process(&vec![0.1; 480 * n], n, IDENTITY, &params(), &mut out);
            assert_eq!(out.len(), 1 + 960);
            assert_eq!(out[0], 9.0);
        }
        let mut out = Vec::new();
        Spatializer::new().process(&[0.0; 15], 3, IDENTITY, &params(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn the_level_is_about_what_plain_stereo_has() {
        let mut noise = Noise(3);
        let input: Vec<f32> = (0..2 * 48_000).map(|_| 0.5 * noise.next()).collect();
        let dry = rms(&input);
        let mut s = Spatializer::new();
        let mut out = Vec::new();
        for c in input.chunks(2048) {
            s.process(c, 2, IDENTITY, &params(), &mut out);
        }
        let ratio = rms(&out) / dry;
        assert!(ratio > 0.5 && ratio < 1.5, "{ratio}");
    }

    #[test]
    fn widening_adds_side_and_leaves_a_centred_sound_alone() {
        let mut s = Spatializer::new();
        let mut p = params();
        p.widen = 1.5;
        let mut out = Vec::new();
        // Identical left and right: nothing to widen.
        let mono: Vec<f32> = (0..2048)
            .flat_map(|i| [(i as f32 * 0.1).sin(); 2])
            .collect();
        s.process(&mono, 2, IDENTITY, &p, &mut out);
        let mut plain = Vec::new();
        Spatializer::new().process(&mono, 2, IDENTITY, &params(), &mut plain);
        for (a, b) in out.iter().zip(&plain) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn a_hall_has_a_tail_that_dies_away() {
        let mut p = params();
        p.acoustics = Acoustics::Hall;
        p.echo = 1.0;
        let mut s = Spatializer::new();
        let mut noise = Noise(11);
        let mut out = Vec::new();
        // Half a second of sound, then silence.
        for _ in 0..24 {
            let input: Vec<f32> = (0..1000).map(|_| noise.next()).collect();
            s.process(&input, 1, IDENTITY, &p, &mut out);
        }
        out.clear();
        let silence = vec![0.0; 1000];
        let mut energies = Vec::new();
        for _ in 0..240 {
            let before = out.len();
            s.process(&silence, 1, IDENTITY, &p, &mut out);
            energies.push(rms(&out[before..]));
        }
        let early = energies[..24].iter().sum::<f32>() / 24.0;
        let late = energies[216..].iter().sum::<f32>() / 24.0;
        assert!(early > 1e-3, "no tail: {early}");
        // 5 s later it is far down (rt60 is 1.7 s).
        assert!(late < early * 0.01, "{early} {late}");
        assert!(out.iter().all(|v| v.is_finite() && v.abs() < 2.0));
    }

    #[test]
    fn the_echo_is_modest_and_off_when_dry_or_at_zero() {
        let mut noise = Noise(5);
        let input: Vec<f32> = (0..48_000).map(|_| noise.next()).collect();
        let run = |acoustics, echo| {
            let mut p = params();
            p.acoustics = acoustics;
            p.echo = echo;
            let mut s = Spatializer::new();
            let mut out = Vec::new();
            for c in input.chunks(1000) {
                s.process(c, 1, IDENTITY, &p, &mut out);
            }
            out
        };
        let dry = run(Acoustics::Dry, 1.0);
        assert_eq!(dry, run(Acoustics::Hall, 0.0));
        let hall = run(Acoustics::Hall, 0.5);
        let ratio = rms(&hall) / rms(&dry);
        assert!(ratio > 1.0 && ratio < 1.5, "{ratio}");
        // A small room rings less than a hall at the same level.
        assert!(rms(&run(Acoustics::Room, 0.5)) < rms(&run(Acoustics::Hall, 1.0)));
    }

    #[test]
    fn nothing_goes_wrong_with_silence_bad_numbers_or_a_bad_head() {
        let mut p = params();
        p.acoustics = Acoustics::Hall;
        p.echo = 1.0;
        let mut s = Spatializer::new();
        let mut out = Vec::new();
        let mut input = vec![0.5; 6 * 512];
        input[10] = f32::NAN;
        input[11] = f32::INFINITY;
        input[12] = 1e-39; // denormal
        s.process(&input, 6, [f32::NAN, 0.0, 0.0, 1.0], &p, &mut out);
        s.process(&input, 6, [0.0; 4], &p, &mut out);
        // A long silence: no denormals left in the output or the filters.
        for _ in 0..400 {
            out.clear();
            s.process(&vec![0.0; 6 * 1024], 6, yaw(0.3), &p, &mut out);
            assert!(out.iter().all(|v| v.is_finite() && !v.is_subnormal()));
        }
        assert!(
            s.voices
                .iter()
                .all(|v| v.low.iter().all(|z| !z.is_subnormal()))
        );
        assert!(
            s.reverb
                .lines
                .iter()
                .all(|l| l.iter().all(|z| !z.is_subnormal()))
        );
    }

    #[test]
    fn head_movement_between_buffers_does_not_click() {
        // A steady tone with the head swinging fast: the output never jumps
        // by more than the tone itself could.
        let p = params();
        let mut s = Spatializer::new();
        let mut out = Vec::new();
        let mut t = 0usize;
        for k in 0..100 {
            let input: Vec<f32> = (0..512)
                .map(|i| 0.3 * ((t + i) as f32 * 0.05).sin())
                .collect();
            t += 512;
            s.process(&input, 1, yaw((k as f32 * 0.4).sin() * 1.5), &p, &mut out);
        }
        let worst = out
            .iter()
            .skip(0)
            .step_by(2)
            .zip(out.iter().skip(2).step_by(2))
            .map(|(a, b)| (b - a).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.15, "{worst}");
    }

    #[test]
    fn a_buffer_costs_little_cpu() {
        // One second of 7.1 with the hall on: the work for 48000 frames.
        // Real time is a second; even in a debug build this must be a small
        // part of it (release is more than 20 times quicker).
        let mut p = params();
        p.acoustics = Acoustics::Hall;
        p.echo = 0.5;
        let mut s = Spatializer::new();
        let mut noise = Noise(1);
        let input: Vec<f32> = (0..1024 * 8).map(|_| noise.next()).collect();
        let mut out = Vec::new();
        let start = Instant::now();
        for k in 0..47 {
            out.clear();
            s.process(&input, 8, yaw(k as f32 * 0.05), &p, &mut out);
        }
        let used = start.elapsed().as_secs_f64();
        let limit = if cfg!(debug_assertions) { 0.5 } else { 0.05 };
        assert!(used < limit, "{used} s for a second of 7.1");
    }
}
