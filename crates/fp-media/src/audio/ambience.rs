//! Room ambience: the soft sounds of a place (a murmur of voices, a camp
//! fire, crickets), mixed on their own and played on their own output so
//! they work with a menu open as well as with a video.
//!
//! Every sound is made here, sample by sample, from a seeded noise source:
//! there are no recordings, so nothing to license, nothing to download and
//! nothing to loop (a loop would repeat; these never do). The caller says
//! which sounds should be heard and how loud ([`Ambience::set`]); changes
//! fade over a moment, and when nothing is audible the device is closed.

use super::RATE;
use super::output::{Backend, Sink, open};
use std::f32::consts::{PI, TAU};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

/// One ambient sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sound {
    /// Many people talking quietly at a distance: a foyer, an auditorium
    /// before the film.
    Murmur,
    /// A camp fire: a low roar with pops and snaps.
    Crackle,
    /// Crickets on a summer night.
    Crickets,
}

impl Sound {
    /// Every sound.
    pub const ALL: &'static [Sound] = &[Sound::Murmur, Sound::Crackle, Sound::Crickets];

    /// The name a pack's manifest calls the sound by.
    pub fn id(self) -> &'static str {
        match self {
            Sound::Murmur => "murmur",
            Sound::Crackle => "crackle",
            Sound::Crickets => "crickets",
        }
    }

    /// The sound a manifest's name stands for.
    pub fn from_id(id: &str) -> Option<Sound> {
        Sound::ALL.iter().copied().find(|s| s.id() == id)
    }
}

/// How long a sound takes to fade (the time constant of its gain), seconds.
const FADE_TAU: f32 = 0.5;
/// A gain under this is silence.
const SILENT: f32 = 1e-4;
/// Frames mixed and written at a time.
const CHUNK: usize = 1024;

/// A small, fast noise source (xorshift64*), seeded so each sound is the
/// same every time.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    /// 0 up to (not including) 1.
    fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }
    /// -1 up to 1.
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

/// A band-pass filter (a state-variable filter, stable while its centre
/// moves) with unity gain at the centre.
#[derive(Clone, Default)]
struct Band {
    ic1: f32,
    ic2: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}

impl Band {
    fn new(freq: f32, q: f32) -> Band {
        let mut b = Band::default();
        b.tune(freq, q);
        b
    }
    fn tune(&mut self, freq: f32, q: f32) {
        let g = (PI * freq / RATE as f32).tan();
        self.k = 1.0 / q;
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }
    fn process(&mut self, x: f32) -> f32 {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        self.k * v1
    }
}

/// A one-pole low-pass filter.
#[derive(Clone, Default)]
struct Low {
    y: f32,
    a: f32,
}

impl Low {
    fn new(freq: f32) -> Low {
        Low {
            y: 0.0,
            a: 1.0 - (-TAU * freq / RATE as f32).exp(),
        }
    }
    fn process(&mut self, x: f32) -> f32 {
        self.y += self.a * (x - self.y);
        self.y
    }
}

/// Left and right gains for a pan from -1 (left) to 1, keeping the power.
fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * 0.25 * PI;
    (a.cos(), a.sin())
}

/// One talker in the murmur: noise shaped by two vowel-like resonances,
/// switched on and off in syllables, and the odd pause between phrases.
#[derive(Clone)]
struct Voice {
    low: Band,
    high: Band,
    env: f32,
    target: f32,
    /// Samples until the next syllable.
    syllable: u32,
    /// Samples until the talker starts or stops talking.
    phrase: u32,
    talking: bool,
    pan: (f32, f32),
}

impl Voice {
    fn new(rng: &mut Rng) -> Voice {
        Voice {
            low: Band::new(500.0, 3.0),
            high: Band::new(1500.0, 3.0),
            env: 0.0,
            target: 0.0,
            syllable: 0,
            phrase: (rng.range(0.2, 2.5) * RATE as f32) as u32,
            talking: rng.unit() < 0.6,
            pan: pan_gains(rng.range(-0.9, 0.9)),
        }
    }

    fn step(&mut self, rng: &mut Rng) -> f32 {
        if self.phrase == 0 {
            self.talking = !self.talking;
            let secs = if self.talking {
                rng.range(1.0, 4.0)
            } else {
                rng.range(0.4, 2.5)
            };
            self.phrase = (secs * RATE as f32) as u32;
        }
        self.phrase -= 1;
        if self.syllable == 0 {
            self.syllable = (rng.range(0.07, 0.24) * RATE as f32) as u32;
            self.target = if self.talking {
                let u = rng.unit();
                0.25 + 0.75 * u * u
            } else {
                0.0
            };
            let f1 = rng.range(300.0, 800.0);
            let f2 = rng.range(900.0, 2400.0);
            self.low.tune(f1, 3.0);
            self.high.tune(f2, 3.5);
        }
        self.syllable -= 1;
        self.env += (self.target - self.env) * 0.0008;
        let noise = rng.signed();
        (self.low.process(noise) + 0.6 * self.high.process(noise)) * self.env
    }
}

#[derive(Clone)]
struct Murmur {
    rng: Rng,
    voices: Vec<Voice>,
    /// The room's own faint low rumble.
    tone: Low,
}

/// How many talkers make the murmur.
const VOICES: usize = 9;
/// Scales the murmur to a modest level (about -28 dB of full scale).
const MURMUR_GAIN: f32 = 1.2;

impl Murmur {
    fn new() -> Murmur {
        let mut rng = Rng::new(0x6D75_726D);
        let voices = (0..VOICES).map(|_| Voice::new(&mut rng)).collect();
        Murmur {
            rng,
            voices,
            tone: Low::new(180.0),
        }
    }

    fn frame(&mut self) -> (f32, f32) {
        let (mut l, mut r) = (0.0, 0.0);
        for v in &mut self.voices {
            let s = v.step(&mut self.rng);
            l += s * v.pan.0;
            r += s * v.pan.1;
        }
        let tone = self.tone.process(self.rng.signed()) * 0.05;
        let k = MURMUR_GAIN / (VOICES as f32).sqrt();
        (l * k + tone, r * k + tone)
    }
}

/// One pop of the fire.
#[derive(Clone)]
struct Pop {
    amp: f32,
    decay: f32,
    pan: (f32, f32),
    prev: f32,
}

#[derive(Clone)]
struct Crackle {
    rng: Rng,
    pops: Vec<Pop>,
    roar: Low,
    swell: Low,
}

/// The most pops sounding at once.
const MAX_POPS: usize = 24;
/// Pops a second on average.
const POP_RATE: f32 = 14.0;

impl Crackle {
    fn new() -> Crackle {
        Crackle {
            rng: Rng::new(0x6669_7265),
            pops: Vec::new(),
            roar: Low::new(420.0),
            swell: Low::new(1.2),
        }
    }

    fn frame(&mut self) -> (f32, f32) {
        let r = &mut self.rng;
        // The low roar swells and falls slowly.
        let swell = 0.6 + 4.0 * self.swell.process(r.signed()).abs();
        let roar = self.roar.process(r.signed()) * 0.22 * swell.min(1.6);
        if r.unit() < POP_RATE / RATE as f32 && self.pops.len() < MAX_POPS {
            let big = r.unit();
            let ms = if big > 0.93 {
                r.range(8.0, 16.0)
            } else {
                r.range(1.5, 6.0)
            };
            self.pops.push(Pop {
                amp: 0.08 + 0.5 * big * big * big,
                decay: (-1.0 / (ms * 0.001 * RATE as f32)).exp(),
                pan: pan_gains(r.range(-0.8, 0.8)),
                prev: 0.0,
            });
        }
        let (mut l, mut rr) = (roar, roar);
        for p in &mut self.pops {
            // Differenced noise: a click, not a thud.
            let n = r.signed();
            let s = (n - p.prev) * 0.5 * p.amp;
            p.prev = n;
            p.amp *= p.decay;
            l += s * p.pan.0;
            rr += s * p.pan.1;
        }
        self.pops.retain(|p| p.amp > 1e-3);
        (l, rr)
    }
}

/// One cricket: bursts of a high pure tone in short chirps.
#[derive(Clone)]
struct Cricket {
    freq: f32,
    phase: f32,
    /// Samples into the current chirp cycle.
    t: u32,
    /// Samples in a cycle (a chirp and the quiet after it).
    cycle: u32,
    /// Chirps left before it rests, and samples of rest left.
    chirps: u32,
    rest: u32,
    pan: (f32, f32),
    amp: f32,
}

/// Pulses in a chirp, how far apart they start and how long each lasts, in
/// samples.
const PULSES: u32 = 3;
const PULSE_GAP: u32 = (RATE as f32 * 0.034) as u32;
const PULSE_LEN: u32 = (RATE as f32 * 0.014) as u32;

impl Cricket {
    fn new(rng: &mut Rng) -> Cricket {
        Cricket {
            freq: rng.range(4300.0, 5100.0),
            phase: 0.0,
            t: (rng.unit() * RATE as f32) as u32,
            cycle: (rng.range(0.55, 0.9) * RATE as f32) as u32,
            chirps: 0,
            rest: (rng.range(0.0, 3.0) * RATE as f32) as u32,
            pan: pan_gains(rng.range(-0.9, 0.9)),
            amp: rng.range(0.4, 1.0),
        }
    }

    fn step(&mut self, rng: &mut Rng) -> f32 {
        if self.rest > 0 {
            self.rest -= 1;
            return 0.0;
        }
        if self.chirps == 0 {
            self.chirps = (rng.range(4.0, 14.0)) as u32;
        }
        let pos = self.t % PULSE_GAP.max(1);
        let pulse = self.t / PULSE_GAP.max(1);
        let env = if pulse < PULSES && pos < PULSE_LEN {
            let x = (pos as f32 / PULSE_LEN as f32 * PI).sin();
            x * x
        } else {
            0.0
        };
        self.t += 1;
        if self.t >= self.cycle {
            self.t = 0;
            self.chirps -= 1;
            if self.chirps == 0 {
                self.rest = (rng.range(1.5, 7.0) * RATE as f32) as u32;
            }
        }
        self.phase += TAU * self.freq / RATE as f32;
        if self.phase > TAU {
            self.phase -= TAU;
        }
        self.phase.sin() * env * self.amp
    }
}

#[derive(Clone)]
struct Crickets {
    rng: Rng,
    crickets: Vec<Cricket>,
}

const CRICKETS: usize = 5;
const CRICKET_GAIN: f32 = 0.15;

impl Crickets {
    fn new() -> Crickets {
        let mut rng = Rng::new(0x6372_6B74);
        let crickets = (0..CRICKETS).map(|_| Cricket::new(&mut rng)).collect();
        Crickets { rng, crickets }
    }

    fn frame(&mut self) -> (f32, f32) {
        let (mut l, mut r) = (0.0, 0.0);
        for c in &mut self.crickets {
            let s = c.step(&mut self.rng) * CRICKET_GAIN;
            l += s * c.pan.0;
            r += s * c.pan.1;
        }
        (l, r)
    }
}

#[derive(Clone)]
enum Synth {
    Murmur(Box<Murmur>),
    Crackle(Box<Crackle>),
    Crickets(Box<Crickets>),
}

impl Synth {
    fn new(sound: Sound) -> Synth {
        match sound {
            Sound::Murmur => Synth::Murmur(Box::new(Murmur::new())),
            Sound::Crackle => Synth::Crackle(Box::new(Crackle::new())),
            Sound::Crickets => Synth::Crickets(Box::new(Crickets::new())),
        }
    }

    fn frame(&mut self) -> (f32, f32) {
        match self {
            Synth::Murmur(s) => s.frame(),
            Synth::Crackle(s) => s.frame(),
            Synth::Crickets(s) => s.frame(),
        }
    }
}

struct Layer {
    sound: Sound,
    synth: Synth,
    gain: f32,
    target: f32,
}

/// Mixes the sounds that are wanted into stereo, fading each in and out.
/// A sound nobody wants costs nothing: its generator is dropped once it has
/// faded out.
#[derive(Default)]
pub struct Mixer {
    layers: Vec<Layer>,
}

impl Mixer {
    pub fn new() -> Mixer {
        Mixer::default()
    }

    /// Sets how loud each sound should be, 0 to 1 (each is mixed at that
    /// share of its own modest level). A sound left out fades to silence.
    pub fn set(&mut self, wants: &[(Sound, f32)]) {
        for l in &mut self.layers {
            l.target = 0.0;
        }
        for &(sound, level) in wants {
            let level = if level.is_finite() {
                level.clamp(0.0, 1.0)
            } else {
                0.0
            };
            if level <= 0.0 {
                continue;
            }
            match self.layers.iter_mut().find(|l| l.sound == sound) {
                Some(l) => l.target = level,
                None => self.layers.push(Layer {
                    sound,
                    synth: Synth::new(sound),
                    gain: 0.0,
                    target: level,
                }),
            }
        }
    }

    /// Whether anything is heard now or is about to be.
    pub fn audible(&self) -> bool {
        !self.layers.is_empty()
    }

    /// Mixes `out` (interleaved stereo frames), replacing what it holds.
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let slew = 1.0 - (-1.0 / (FADE_TAU * RATE as f32)).exp();
        for l in &mut self.layers {
            for frame in out.as_chunks_mut::<2>().0.iter_mut() {
                l.gain += (l.target - l.gain) * slew;
                if l.target == 0.0 && l.gain < SILENT {
                    l.gain = 0.0;
                    break;
                }
                let (a, b) = l.synth.frame();
                frame[0] += a * l.gain;
                frame[1] += b * l.gain;
            }
        }
        self.layers.retain(|l| l.target > 0.0 || l.gain > 0.0);
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }
}

enum Cmd {
    Set(Vec<(Sound, f32)>),
    Stop,
}

/// Plays the mixer on its own audio output, on its own thread. The device is
/// opened when something is to be heard and closed again once it has faded
/// out, so a film has no second stream beside it.
pub struct Ambience {
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
    last: Vec<(Sound, f32)>,
}

impl Ambience {
    pub fn start(backend: Backend) -> Ambience {
        let (tx, rx) = channel();
        let thread = std::thread::Builder::new()
            .name("ambience".into())
            .spawn(move || run(backend, rx))
            .ok();
        Ambience {
            tx,
            thread,
            last: Vec::new(),
        }
    }

    /// What should be heard now (see [`Mixer::set`]). Asking for what was
    /// asked for last does nothing.
    pub fn set(&mut self, wants: &[(Sound, f32)]) {
        if self.last == wants {
            return;
        }
        self.last = wants.to_vec();
        let _ = self.tx.send(Cmd::Set(wants.to_vec()));
    }
}

impl Drop for Ambience {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(backend: Backend, rx: Receiver<Cmd>) {
    let mut mixer = Mixer::new();
    let mut sink: Option<Box<dyn Sink>> = None;
    let mut buf = vec![0.0f32; CHUNK * 2];
    loop {
        // Idle: nothing to play, so wait for the next order. Playing: take
        // any orders that have come and carry on.
        let cmd = if mixer.audible() {
            rx.try_recv().ok()
        } else {
            sink = None;
            match rx.recv_timeout(Duration::from_secs(3600)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        match cmd {
            Some(Cmd::Stop) => return,
            Some(Cmd::Set(wants)) => mixer.set(&wants),
            None => {}
        }
        if !mixer.audible() {
            continue;
        }
        let s = sink.get_or_insert_with(|| open(&backend, 2));
        mixer.render(&mut buf);
        if s.write(&buf).is_err() {
            sink = None;
            // A device that fails is not asked again until the next order.
            match rx.recv() {
                Ok(Cmd::Set(wants)) => mixer.set(&wants),
                _ => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_secs(sound: Sound, secs: f32) -> Vec<f32> {
        let mut m = Mixer::new();
        m.set(&[(sound, 1.0)]);
        let mut out = vec![0.0; (secs * RATE as f32) as usize * 2];
        // Let the fade-in finish first.
        let mut warm = vec![0.0; 2 * RATE as usize * 4];
        m.render(&mut warm);
        m.render(&mut out);
        out
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn every_sound_is_audible_but_modest_and_in_range() {
        for &s in Sound::ALL {
            let out = render_secs(s, 6.0);
            let r = rms(&out);
            let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(out.iter().all(|v| v.is_finite()), "{s:?}");
            assert!(r > 0.008, "{s:?} is silent: rms {r}");
            assert!(r < 0.12, "{s:?} is loud: rms {r}");
            assert!(peak < 0.8, "{s:?} peaks at {peak}");
        }
    }

    #[test]
    fn the_sounds_are_the_same_every_time_and_differ_from_each_other() {
        assert_eq!(
            render_secs(Sound::Murmur, 1.0),
            render_secs(Sound::Murmur, 1.0)
        );
        assert_ne!(
            render_secs(Sound::Murmur, 1.0),
            render_secs(Sound::Crackle, 1.0)
        );
    }

    #[test]
    fn a_murmur_has_two_channels_that_are_not_the_same() {
        let out = render_secs(Sound::Murmur, 3.0);
        let diff: f32 = out
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| (f[0] - f[1]).abs())
            .sum();
        assert!(diff > 1.0, "{diff}");
    }

    #[test]
    fn crickets_sing_high_and_the_fire_does_not() {
        // A crude measure of pitch: how often the signal changes sign.
        let rate = |x: &[f32]| {
            let l: Vec<f32> = x.as_chunks::<2>().0.iter().map(|f| f[0] + f[1]).collect();
            let mut loud = 0usize;
            let mut zc = 0usize;
            for w in l.windows(2) {
                if w[0].abs() > 0.003 {
                    loud += 1;
                    if (w[0] < 0.0) != (w[1] < 0.0) {
                        zc += 1;
                    }
                }
            }
            zc as f32 / loud.max(1) as f32
        };
        let crickets = rate(&render_secs(Sound::Crickets, 20.0));
        let fire = rate(&render_secs(Sound::Crackle, 6.0));
        // 4.5 kHz at 48 kHz crosses zero about every 5th sample.
        assert!(crickets > 0.12 && crickets < 0.28, "{crickets}");
        assert!(fire < crickets, "{fire}");
    }

    #[test]
    fn a_change_fades_without_a_step() {
        let mut m = Mixer::new();
        m.set(&[(Sound::Murmur, 1.0)]);
        let mut warm = vec![0.0; 2 * RATE as usize * 4];
        m.render(&mut warm);
        m.set(&[]);
        let mut out = vec![0.0; 2 * RATE as usize * 6];
        m.render(&mut out);
        let secs =
            |a: f32, b: f32| &out[(a * RATE as f32) as usize * 2..(b * RATE as f32) as usize * 2];
        let early = rms(secs(0.0, 0.5));
        let late = rms(secs(5.0, 6.0));
        assert!(early > 0.003, "{early}");
        assert!(late == 0.0, "{late}");
        // No sample jumps by more than the sound itself moves.
        let frames = out.as_chunks::<2>().0;
        let jump = frames
            .iter()
            .zip(frames.iter().skip(1))
            .fold(0.0f32, |m, (a, b)| m.max((a[0] - b[0]).abs()));
        assert!(jump < 0.2, "{jump}");
        assert!(!m.audible());
    }

    #[test]
    fn a_fade_in_starts_from_silence() {
        let mut m = Mixer::new();
        m.set(&[(Sound::Crackle, 1.0)]);
        let mut out = vec![0.0; 2 * 64];
        m.render(&mut out);
        assert!(out.iter().all(|v| v.abs() < 0.01));
    }

    #[test]
    fn the_level_scales_the_sound_and_zero_is_nothing() {
        let loud = {
            let mut m = Mixer::new();
            m.set(&[(Sound::Murmur, 1.0)]);
            let mut warm = vec![0.0; 2 * RATE as usize * 4];
            m.render(&mut warm);
            let mut o = vec![0.0; 2 * RATE as usize * 2];
            m.render(&mut o);
            rms(&o)
        };
        let quiet = {
            let mut m = Mixer::new();
            m.set(&[(Sound::Murmur, 0.25)]);
            let mut warm = vec![0.0; 2 * RATE as usize * 4];
            m.render(&mut warm);
            let mut o = vec![0.0; 2 * RATE as usize * 2];
            m.render(&mut o);
            rms(&o)
        };
        let ratio = quiet / loud;
        assert!((0.2..0.3).contains(&ratio), "{ratio}");
        let mut m = Mixer::new();
        m.set(&[(Sound::Murmur, 0.0), (Sound::Crickets, f32::NAN)]);
        assert!(!m.audible());
    }

    #[test]
    fn the_output_starts_stops_and_drops_cleanly() {
        let mut a = Ambience::start(Backend::Null);
        a.set(&[(Sound::Murmur, 0.5)]);
        std::thread::sleep(Duration::from_millis(120));
        a.set(&[]);
        a.set(&[]);
        drop(a);
        // A handle that never plays drops at once.
        drop(Ambience::start(Backend::Null));
    }
}
