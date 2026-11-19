//! The sleep timer: after a set time, or at the end of the video, the sound
//! fades out over ten seconds and the video pauses. Off until it is chosen
//! from the player's More list, and gone when the video is closed.

/// Seconds the sound takes to fade out before the pause.
pub const FADE_SECS: f32 = 10.0;

/// The choices of minutes, in the order the More row steps through them.
pub const MINUTES: [u32; 4] = [15, 30, 45, 60];

/// How close to the end of the video "end of item" pauses it, so the next
/// video of a queue never starts.
const END_MARGIN: f32 = 0.25;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Sleep {
    #[default]
    Off,
    /// `minutes` was chosen; `left` seconds of watching remain.
    After { minutes: u32, left: f32 },
    /// Until the open video ends.
    EndOfItem,
}

/// What a frame of the timer asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tick {
    /// Nothing: the sound is at the volume that was set.
    Idle,
    /// The sound is fading: this fraction (0 to 1) of the volume that was set.
    Fade(f32),
    /// Time is up: pause the video.
    Pause,
}

impl Sleep {
    /// The choice after this one when the row is pressed: off, then each
    /// time, then the end of the video, then off again.
    pub fn next(self) -> Sleep {
        match self {
            Sleep::Off => Sleep::after(MINUTES[0]),
            Sleep::After { minutes, .. } => match MINUTES.iter().position(|m| *m == minutes) {
                Some(i) if i + 1 < MINUTES.len() => Sleep::after(MINUTES[i + 1]),
                _ => Sleep::EndOfItem,
            },
            Sleep::EndOfItem => Sleep::Off,
        }
    }

    fn after(minutes: u32) -> Sleep {
        Sleep::After {
            minutes,
            left: minutes as f32 * 60.0,
        }
    }

    /// Whole minutes left, rounded up, for a timed choice.
    pub fn minutes_left(self) -> Option<u32> {
        match self {
            Sleep::After { left, .. } => Some((left.max(0.0) / 60.0).ceil() as u32),
            _ => None,
        }
    }

    /// Moves the timer on `dt` seconds of watching. `item_left` is how many
    /// seconds of the open video remain at its speed (`None` when unknown).
    pub fn tick(&mut self, dt: f32, item_left: Option<f32>) -> Tick {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        let left = match self {
            Sleep::Off => return Tick::Idle,
            Sleep::After { left, .. } => {
                *left -= dt;
                *left
            }
            Sleep::EndOfItem => match item_left {
                Some(l) => l - END_MARGIN,
                None => return Tick::Idle,
            },
        };
        if left <= 0.0 {
            *self = Sleep::Off;
            Tick::Pause
        } else if left < FADE_SECS {
            Tick::Fade(left / FADE_SECS)
        } else {
            Tick::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_steps_through_every_choice_and_back_to_off() {
        let mut s = Sleep::Off;
        let mut seen = Vec::new();
        for _ in 0..6 {
            s = s.next();
            seen.push(match s {
                Sleep::Off => "off".to_string(),
                Sleep::After { minutes, .. } => minutes.to_string(),
                Sleep::EndOfItem => "end".to_string(),
            });
        }
        assert_eq!(seen, ["15", "30", "45", "60", "end", "off"]);
    }

    #[test]
    fn off_does_nothing() {
        let mut s = Sleep::Off;
        assert_eq!(s.tick(100.0, Some(1.0)), Tick::Idle);
        assert_eq!(s, Sleep::Off);
    }

    #[test]
    fn a_timed_choice_fades_for_ten_seconds_then_pauses() {
        let mut s = Sleep::Off.next();
        assert_eq!(s.minutes_left(), Some(15));
        assert_eq!(s.tick(15.0 * 60.0 - 20.0, None), Tick::Idle);
        assert_eq!(s.tick(10.0, None), Tick::Idle, "10 s left: not yet");
        match s.tick(5.0, None) {
            Tick::Fade(g) => assert!((g - 0.5).abs() < 1e-4, "{g}"),
            other => panic!("{other:?}"),
        }
        match s.tick(4.0, None) {
            Tick::Fade(g) => assert!((g - 0.1).abs() < 1e-4, "{g}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(s.tick(2.0, None), Tick::Pause);
        assert_eq!(s, Sleep::Off, "it fires once");
        assert_eq!(s.tick(1.0, None), Tick::Idle);
    }

    #[test]
    fn the_end_of_the_item_fades_over_its_last_ten_seconds() {
        let mut s = Sleep::EndOfItem;
        assert_eq!(s.tick(0.016, Some(600.0)), Tick::Idle);
        assert_eq!(s.tick(0.016, None), Tick::Idle, "length unknown");
        match s.tick(0.016, Some(5.25)) {
            Tick::Fade(g) => assert!((g - 0.5).abs() < 1e-4),
            other => panic!("{other:?}"),
        }
        assert_ne!(s, Sleep::Off);
        assert_eq!(s.tick(0.016, Some(0.2)), Tick::Pause);
        assert_eq!(s, Sleep::Off);
    }

    #[test]
    fn a_bad_step_never_runs_the_clock_backwards() {
        let mut s = Sleep::Off.next();
        let before = s;
        assert_eq!(s.tick(f32::NAN, None), Tick::Idle);
        assert_eq!(s.tick(-5.0, None), Tick::Idle);
        assert_eq!(s, before);
    }
}
