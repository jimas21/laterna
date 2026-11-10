//! Keeping sound in step with the picture, and the "Audio delay" setting on
//! top of that.
//!
//! The video clock is the master. Each decoded chunk of audio has a media
//! time (`pts`), and the sound card tells us how much is queued ahead of the
//! speaker, so we know which media time the clock will show when the chunk is
//! heard. A chunk heard more than [`DRIFT_TOLERANCE`] away from where it
//! belongs is skipped or padded with silence. A small drift is left alone.
//!
//! The audio delay moves where "belongs" is: with +100 ms, a sound is heard
//! 100 ms after its picture. A step of 10 ms is far inside the drift
//! tolerance, so the delay cannot ride on that alone. [`DelayState`] holds how
//! much shift the stream has really been given and, when the setting changes
//! (or after a seek, which empties the device's queue), pads silence or drops
//! samples once to make up the difference. The delay is in real time, like
//! the lag of a wireless earbud it makes up for, so it does not stretch with
//! the playback speed.

/// The shortest and longest audio delay offered, in milliseconds. A positive
/// delay plays the sound later than the picture.
pub const DELAY_MIN_MS: i32 = -200;
pub const DELAY_MAX_MS: i32 = 300;
/// The audio delay moves in steps of this many milliseconds.
pub const DELAY_STEP_MS: i32 = 10;

/// Sound this far (seconds) from where it belongs is corrected; closer is left
/// alone.
pub const DRIFT_TOLERANCE: f64 = 0.06;
/// The most silence one drift correction inserts, in seconds.
const MAX_DRIFT_PAD: f64 = 0.5;

/// A stored audio delay brought into range and onto the 10 ms steps.
pub fn clamp_delay_ms(ms: i32) -> i32 {
    let ms = ms.clamp(DELAY_MIN_MS, DELAY_MAX_MS);
    // Halves round away from zero.
    let half = DELAY_STEP_MS / 2;
    let stepped = if ms >= 0 { ms + half } else { ms - half } / DELAY_STEP_MS * DELAY_STEP_MS;
    stepped.clamp(DELAY_MIN_MS, DELAY_MAX_MS)
}

/// What to do to one chunk of interleaved samples before writing it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Interleaved samples of silence to write first.
    pub pad: usize,
    /// Interleaved samples to drop from the front of the chunk.
    pub skip: usize,
}

/// The shift the audio stream has been given so far.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DelayState {
    /// Real seconds the sound has been moved later (negative: earlier).
    applied: f64,
}

impl DelayState {
    /// Forgets the shift: the device's queue was emptied (a seek, a new device).
    pub fn reset(&mut self) {
        self.applied = 0.0;
    }

    /// Real seconds of shift the stream has been given.
    pub fn applied(&self) -> f64 {
        self.applied
    }

    /// How far (media seconds) a chunk would be from where it belongs, given
    /// the shift so far. Positive: it would be heard too early.
    pub fn drift(&self, pts: f64, speed: f64, heard_at: f64) -> f64 {
        pts + self.applied * speed - heard_at
    }

    /// Works out the padding and skipping for one chunk of `len` interleaved
    /// samples (`channels` to a frame at `rate` Hz): drift correction plus
    /// whatever is still owed to the `target_s` delay (real seconds).
    ///
    /// `pts` is the chunk's media time and `heard_at` the media time the clock
    /// shows when its first sample is heard.
    #[allow(clippy::too_many_arguments)]
    pub fn plan(
        &mut self,
        target_s: f64,
        speed: f64,
        pts: f64,
        heard_at: f64,
        len: usize,
        rate: u32,
        channels: usize,
    ) -> Plan {
        let ch = channels.max(1);
        let per_sec = rate as f64 * ch as f64;
        let whole = len - len % ch;
        let speed = speed.max(0.01);
        let diff = self.drift(pts, speed, heard_at);

        // The delay still owed. Padding and skipping realise it at once; a
        // chunk too short to skip all of it leaves the rest for the next.
        let owed = ((target_s - self.applied) * rate as f64).round() as i64 * ch as i64;
        let mut net = owed;
        if owed < 0 {
            net = -((-owed).min(whole as i64));
        }
        self.applied += net as f64 / per_sec;

        // Then the drift, as before the delay existed.
        if diff < -DRIFT_TOLERANCE {
            net -= ((-diff / speed) * rate as f64).round() as i64 * ch as i64;
        } else if diff > DRIFT_TOLERANCE {
            net += ((diff / speed).min(MAX_DRIFT_PAD) * rate as f64).round() as i64 * ch as i64;
        }

        if net >= 0 {
            Plan {
                pad: net as usize,
                skip: 0,
            }
        } else {
            Plan {
                pad: 0,
                skip: (-net as usize).min(whole),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const CH: usize = 2;
    /// 1024 frames, a typical decoded AAC chunk.
    const CHUNK: usize = 1024 * CH;
    const CHUNK_S: f64 = 1024.0 / RATE as f64;
    const PER_SEC: f64 = RATE as f64 * CH as f64;

    #[test]
    fn delay_is_kept_in_range_and_on_steps() {
        assert_eq!(clamp_delay_ms(0), 0);
        assert_eq!(clamp_delay_ms(-1000), -200);
        assert_eq!(clamp_delay_ms(1000), 300);
        assert_eq!(clamp_delay_ms(14), 10);
        assert_eq!(clamp_delay_ms(15), 20);
        assert_eq!(clamp_delay_ms(-14), -10);
        assert_eq!(clamp_delay_ms(-15), -20);
        assert_eq!(clamp_delay_ms(-200), -200);
        assert_eq!(clamp_delay_ms(300), 300);
    }

    /// A device and clock in one: chunks are written back to back, and each
    /// reports how long after its picture it is heard.
    struct Sim {
        state: DelayState,
        /// Real seconds on the clock now.
        now: f64,
        /// Clock time at which the next written sample is heard.
        write_end: f64,
        speed: f64,
        /// Media time of the next chunk.
        pts: f64,
    }

    impl Sim {
        fn new(speed: f64) -> Sim {
            Sim {
                state: DelayState::default(),
                now: 0.0,
                write_end: 0.0,
                speed,
                pts: 0.0,
            }
        }

        /// Plays one chunk with the delay `target_ms`; returns how many ms
        /// (real time) after the clock reaches its media time its first kept
        /// sample is heard.
        fn chunk(&mut self, target_ms: i32) -> f64 {
            let target = target_ms as f64 / 1000.0;
            // When the chunk would be heard if written now, as media time.
            let heard_at = self.now.max(self.write_end) * self.speed;
            let plan = self
                .state
                .plan(target, self.speed, self.pts, heard_at, CHUNK, RATE, CH);
            self.write_end = self.now.max(self.write_end) + plan.pad as f64 / PER_SEC;
            let first_pts = self.pts + plan.skip as f64 / PER_SEC * self.speed;
            let heard_clock = self.write_end;
            self.write_end += (CHUNK - plan.skip) as f64 / PER_SEC;
            self.pts += CHUNK_S * self.speed;
            // A write blocks while more than about 40 ms is queued.
            self.now = self.now.max(self.write_end - 0.040);
            (heard_clock - first_pts / self.speed) * 1000.0
        }
    }

    fn run(sim: &mut Sim, ms: i32, chunks: usize) -> f64 {
        let mut late = 0.0;
        for _ in 0..chunks {
            late = sim.chunk(ms);
        }
        late
    }

    #[test]
    fn no_delay_changes_nothing() {
        let mut s = DelayState::default();
        // On time, and a little off but inside the tolerance.
        for heard_at in [1.0, 1.0 + 0.05, 1.0 - 0.05] {
            let p = s.plan(0.0, 1.0, 1.0, heard_at, CHUNK, RATE, CH);
            assert_eq!(p, Plan::default());
        }
        assert_eq!(s.applied(), 0.0);
    }

    #[test]
    fn drift_beyond_tolerance_is_corrected_as_before() {
        let mut s = DelayState::default();
        // 100 ms late: skip 100 ms of stereo samples.
        let p = s.plan(0.0, 1.0, 1.0, 1.1, 10_000, RATE, CH);
        assert_eq!(p.skip, 4800 * 2);
        assert_eq!(p.pad, 0);
        // 100 ms early: pad 100 ms.
        let p = s.plan(0.0, 1.0, 1.0, 0.9, 10_000, RATE, CH);
        assert_eq!(p.pad, 4800 * 2);
        // Padding is capped at half a second.
        let p = s.plan(0.0, 1.0, 1.0, 0.0, 10_000, RATE, CH);
        assert_eq!(p.pad, 24_000 * 2);
        // At double speed, the correction takes half the real time.
        let p = s.plan(0.0, 2.0, 1.0, 1.2, 100_000, RATE, CH);
        assert_eq!(p.skip, 4800 * 2);
        assert_eq!(s.applied(), 0.0);
    }

    #[test]
    fn a_positive_delay_pads_silence_once() {
        let mut s = DelayState::default();
        let p = s.plan(0.1, 1.0, 1.0, 1.0, CHUNK, RATE, CH);
        assert_eq!(p.pad, 4800 * 2);
        assert_eq!(p.skip, 0);
        assert!((s.applied() - 0.1).abs() < 1e-9);
        // The next chunk is heard where the padding put it: nothing more.
        let p = s.plan(0.1, 1.0, 1.0, 1.1, CHUNK, RATE, CH);
        assert_eq!(p, Plan::default());
    }

    #[test]
    fn a_ten_ms_step_takes_effect() {
        let mut s = DelayState::default();
        let p = s.plan(0.010, 1.0, 5.0, 5.0, CHUNK, RATE, CH);
        assert_eq!(p.pad, 480 * 2);
        let p = s.plan(0.0, 1.0, 5.0, 5.01, CHUNK, RATE, CH);
        assert_eq!(p.skip, 480 * 2);
        assert_eq!(s.applied(), 0.0);
    }

    #[test]
    fn a_negative_delay_skips_across_chunks() {
        let mut s = DelayState::default();
        // 200 ms earlier is 9600 frames: nine and a bit chunks of 1024.
        let mut skipped = 0;
        let mut chunks = 0;
        while s.applied() > -0.2 + 1e-9 {
            // Heard where the shift so far puts it: no drift.
            let p = s.plan(-0.2, 1.0, 0.0, s.applied(), CHUNK, RATE, CH);
            assert_eq!(p.pad, 0);
            skipped += p.skip;
            chunks += 1;
            assert!(chunks < 20);
        }
        assert_eq!(skipped, 9600 * 2);
        assert_eq!(chunks, 10);
    }

    #[test]
    fn the_stream_settles_on_the_delay() {
        for ms in [-200, -10, 0, 10, 100, 300] {
            let mut sim = Sim::new(1.0);
            run(&mut sim, 0, 40);
            let base = run(&mut sim, 0, 1);
            let late = run(&mut sim, ms, 40);
            assert!(
                (late - base - ms as f64).abs() < 1.0,
                "{ms} ms: {late} vs {base}"
            );
        }
    }

    #[test]
    fn changing_the_delay_moves_it_the_difference() {
        let mut sim = Sim::new(1.0);
        run(&mut sim, 0, 40);
        let base = run(&mut sim, 0, 1);
        let a = run(&mut sim, 150, 40);
        assert!((a - base - 150.0).abs() < 1.0, "{a}");
        let b = run(&mut sim, -50, 40);
        assert!((b - base + 50.0).abs() < 1.0, "{b}");
        let c = run(&mut sim, 0, 40);
        assert!((c - base).abs() < 1.0, "{c}");
    }

    #[test]
    fn the_delay_is_real_time_at_other_speeds() {
        for speed in [0.5, 1.5, 2.0] {
            let mut sim = Sim::new(speed);
            run(&mut sim, 0, 40);
            let base = run(&mut sim, 0, 1);
            let late = run(&mut sim, 100, 60);
            assert!(
                (late - base - 100.0).abs() < 2.0,
                "x{speed}: {late} vs {base}"
            );
        }
    }

    #[test]
    fn a_seek_applies_the_delay_again() {
        let mut s = DelayState::default();
        s.plan(0.1, 1.0, 1.0, 1.0, CHUNK, RATE, CH);
        s.reset();
        assert_eq!(s.applied(), 0.0);
        // The queue is empty again, so the first chunk pads the delay again.
        let p = s.plan(0.1, 1.0, 30.0, 30.0, CHUNK, RATE, CH);
        assert_eq!(p.pad, 4800 * 2);
    }

    #[test]
    fn a_short_chunk_never_skips_more_than_it_has() {
        let mut s = DelayState::default();
        let p = s.plan(-0.2, 1.0, 0.0, 0.0, 100, RATE, CH);
        assert_eq!(p.skip, 100);
        // Only what was dropped counts as done.
        assert!((s.applied() + 50.0 / RATE as f64).abs() < 1e-9);
    }
}
