//! Matching the headset's refresh rate to the film playing (Settings ›
//! General › Match the headset to the film).
//!
//! A film at 24 frames a second shows evenly only on a display whose rate is a
//! whole multiple of it: 72 Hz shows each frame three times. At 90 Hz some
//! frames stay up for four display frames and some for five, which is judder.
//! When the runtime offers rates (`XR_FB_display_refresh_rate`), a video asks
//! for the best one while it plays and the rate the user had comes back after.

/// The highest rate Auto asks for. 120 and 144 Hz shrink a frame's budget to
/// 8.3 and 6.9 ms, which the headset may not keep up with.
pub const AUTO_RATE_CAP: f32 = 120.0;

/// The lowest rate Auto asks for: below it a display flickers.
const AUTO_RATE_FLOOR: f32 = 72.0;

/// How far a rate may be from a whole multiple of the film's and still count
/// as one: 71.928 Hz is 3 x 23.976 and a headset offers 72.
const MULTIPLE_TOLERANCE: f64 = 0.005;

/// The rate to ask for from the `supported` ones for a film of `fps` frames a
/// second, never above `cap`. A whole multiple of the film's rate when one is
/// offered, the lowest first (24 gives 72, 30 gives 90, 60 gives 120); else
/// the highest offered. 0 (the system's own rate) when nothing usable is
/// offered.
pub fn pick_rate(supported: &[f32], fps: f64, cap: f32) -> f32 {
    let usable = |hz: &&f32| hz.is_finite() && **hz > 0.0 && **hz <= cap;
    let mut options: Vec<f32> = supported.iter().filter(usable).copied().collect();
    options.sort_by(f32::total_cmp);
    if fps.is_finite() && fps > 0.0 {
        let fits = |hz: f32| {
            let times = f64::from(hz) / fps;
            let whole = times.round();
            whole >= 1.0 && (times - whole).abs() <= MULTIPLE_TOLERANCE * whole
        };
        if let Some(hz) = options
            .iter()
            .copied()
            .find(|&hz| hz >= AUTO_RATE_FLOOR && fits(hz))
        {
            return hz;
        }
    }
    options.last().copied().unwrap_or(0.0)
}

/// Whether two rates are the same one, as a runtime reports them.
fn same_rate(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.5
}

/// Which rate to have now, and the one to go back to. Called every frame with
/// what is wanted; answers only when something is to be requested.
#[derive(Debug, Default)]
pub struct AutoRate {
    /// The rate the user had before the first change, to go back to.
    restore: Option<f32>,
    /// The rate asked for last, so it is not asked for again.
    asked: Option<f32>,
}

impl AutoRate {
    /// `film_fps` is the frame rate of the video open (`None` with none, or
    /// with Auto off); `supported` and `current` are what the runtime says.
    /// Returns a rate to request, if any.
    pub fn step(
        &mut self,
        film_fps: Option<f64>,
        supported: &[f32],
        current: Option<f32>,
    ) -> Option<f32> {
        let wanted = film_fps
            .filter(|_| !supported.is_empty())
            .map(|fps| pick_rate(supported, fps, AUTO_RATE_CAP))
            .filter(|hz| *hz > 0.0);
        match wanted {
            Some(hz) => {
                if self.asked.is_some_and(|a| same_rate(a, hz)) {
                    return None;
                }
                if self.restore.is_none() {
                    self.restore = current;
                }
                self.asked = Some(hz);
                if current.is_some_and(|c| same_rate(c, hz)) {
                    None
                } else {
                    Some(hz)
                }
            }
            None => {
                self.asked = None;
                let back = self.restore.take()?;
                if current.is_some_and(|c| same_rate(c, back)) {
                    None
                } else {
                    Some(back)
                }
            }
        }
    }

    /// The rate to go back to when the app ends with one still changed.
    pub fn restore(&mut self) -> Option<f32> {
        self.asked = None;
        self.restore.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: [f32; 4] = [72.0, 90.0, 120.0, 144.0];

    #[test]
    fn a_whole_multiple_of_the_film_rate_is_picked_lowest_first() {
        assert_eq!(pick_rate(&FRAME, 24.0, 120.0), 72.0);
        assert_eq!(pick_rate(&FRAME, 30.0, 120.0), 90.0);
        assert_eq!(pick_rate(&FRAME, 60.0, 120.0), 120.0);
        assert_eq!(pick_rate(&FRAME, 36.0, 120.0), 72.0);
        // NTSC rates count as the whole rate.
        assert_eq!(pick_rate(&FRAME, 23.976, 120.0), 72.0);
        assert_eq!(pick_rate(&FRAME, 29.97, 120.0), 90.0);
        assert_eq!(pick_rate(&FRAME, 59.94, 120.0), 120.0);
    }

    #[test]
    fn the_cap_and_the_offer_limit_the_choice() {
        // 144 is a multiple of 24 and 72 but over the cap; 60 fps wants 120.
        assert_eq!(pick_rate(&[144.0], 24.0, 120.0), 0.0);
        assert_eq!(pick_rate(&[144.0], 24.0, 144.0), 144.0);
        assert_eq!(pick_rate(&[90.0, 120.0], 24.0, 120.0), 120.0);
        // 25 fps fits none of them: the highest under the cap.
        assert_eq!(pick_rate(&FRAME, 25.0, 120.0), 120.0);
        assert_eq!(pick_rate(&FRAME, 50.0, 120.0), 120.0);
        // Unknown film rate, or nothing offered.
        assert_eq!(pick_rate(&FRAME, 0.0, 120.0), 120.0);
        assert_eq!(pick_rate(&FRAME, f64::NAN, 120.0), 120.0);
        assert_eq!(pick_rate(&[], 24.0, 120.0), 0.0);
        assert_eq!(pick_rate(&[f32::NAN, -1.0, 0.0], 24.0, 120.0), 0.0);
    }

    #[test]
    fn a_rate_below_the_floor_is_not_picked_for_its_multiple() {
        // 48 Hz is twice 24 but flickers: 72 is the pick.
        assert_eq!(pick_rate(&[48.0, 72.0, 90.0], 24.0, 120.0), 72.0);
        assert_eq!(pick_rate(&[48.0, 60.0], 24.0, 120.0), 60.0);
    }

    #[test]
    fn a_video_asks_once_and_the_users_rate_comes_back_when_it_closes() {
        let mut a = AutoRate::default();
        // Menus at 90 Hz, a 24 fps film opens.
        assert_eq!(a.step(Some(24.0), &FRAME, Some(90.0)), Some(72.0));
        // The runtime switched; nothing more is asked while it plays.
        assert_eq!(a.step(Some(24.0), &FRAME, Some(90.0)), None);
        assert_eq!(a.step(Some(24.0), &FRAME, Some(72.0)), None);
        // A film at 60 fps next: asks again, still remembering 90.
        assert_eq!(a.step(Some(60.0), &FRAME, Some(72.0)), Some(120.0));
        // Closed: back to what the user had.
        assert_eq!(a.step(None, &FRAME, Some(120.0)), Some(90.0));
        assert_eq!(a.step(None, &FRAME, Some(90.0)), None);
        // Nothing left to restore.
        assert_eq!(a.restore(), None);
    }

    #[test]
    fn nothing_is_asked_when_the_rate_already_fits_or_none_are_offered() {
        let mut a = AutoRate::default();
        assert_eq!(a.step(Some(24.0), &FRAME, Some(72.0)), None);
        // Closing changes nothing: the user's rate was never touched.
        assert_eq!(a.step(None, &FRAME, Some(72.0)), None);
        let mut b = AutoRate::default();
        assert_eq!(b.step(Some(24.0), &[], Some(90.0)), None);
        assert_eq!(b.step(None, &[], Some(90.0)), None);
        assert_eq!(b.restore(), None);
    }

    #[test]
    fn leaving_with_a_changed_rate_gives_the_one_to_go_back_to() {
        let mut a = AutoRate::default();
        assert_eq!(a.step(Some(24.0), &FRAME, Some(144.0)), Some(72.0));
        assert_eq!(a.restore(), Some(144.0));
        assert_eq!(a.restore(), None);
    }

    #[test]
    fn auto_off_never_changes_the_rate() {
        // The app passes no film rate with Auto off.
        let mut a = AutoRate::default();
        for _ in 0..10 {
            assert_eq!(a.step(None, &FRAME, Some(90.0)), None);
        }
    }
}
