//! Cheap duration statistics for the frame loop: a fixed-capacity sample
//! buffer the app summarises and resets every few seconds. Nothing here
//! allocates after [`Series::new`].

use std::fmt;
use std::time::Instant;

/// Samples kept per period. At 72 Hz and 10 s a call made once per frame
/// gives 720; more than this are counted and still count towards the max,
/// but are left out of the median.
const CAPACITY: usize = 2048;

/// Durations (or any millisecond values) collected over one period.
#[derive(Debug)]
pub struct Series {
    samples: Vec<f32>,
    max: f32,
    count: u64,
}

/// What a [`Series`] held when it was taken.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub median: f32,
    pub max: f32,
    /// Samples pushed, including any past the buffer's capacity.
    pub count: u64,
}

impl Default for Series {
    fn default() -> Self {
        Series {
            samples: Vec::with_capacity(CAPACITY),
            max: 0.0,
            count: 0,
        }
    }
}

impl Series {
    pub fn new() -> Series {
        Series::default()
    }

    /// Adds a value.
    pub fn push(&mut self, value: f32) {
        self.max = if self.count == 0 {
            value
        } else {
            self.max.max(value)
        };
        self.count += 1;
        if self.samples.len() < CAPACITY {
            self.samples.push(value);
        }
    }

    /// Adds the milliseconds since `start`.
    pub fn push_since(&mut self, start: Instant) {
        self.push(start.elapsed().as_secs_f32() * 1000.0);
    }

    /// Median, max and count of what was pushed since the last call, then
    /// starts over. `None` when nothing was pushed.
    pub fn take(&mut self) -> Option<Summary> {
        if self.count == 0 {
            return None;
        }
        self.samples.sort_unstable_by(f32::total_cmp);
        let summary = Summary {
            median: self.samples[(self.samples.len() - 1) / 2],
            max: self.max,
            count: self.count,
        };
        self.samples.clear();
        self.max = 0.0;
        self.count = 0;
        Some(summary)
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1}/{:.1}", self.median, self.max)
    }
}

/// Formats a summary as `median/max`, or `-` when there was none.
pub fn show(summary: Option<Summary>) -> String {
    match summary {
        Some(s) => s.to_string(),
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_series_has_no_summary() {
        assert_eq!(Series::new().take(), None);
        assert_eq!(show(None), "-");
    }

    #[test]
    fn median_and_max() {
        let mut s = Series::new();
        for v in [5.0, 1.0, 3.0, 9.0, 2.0] {
            s.push(v);
        }
        let t = s.take().unwrap();
        assert_eq!((t.median, t.max, t.count), (3.0, 9.0, 5));
        assert_eq!(t.to_string(), "3.0/9.0");
    }

    #[test]
    fn even_count_takes_the_lower_middle() {
        let mut s = Series::new();
        for v in [4.0, 1.0, 2.0, 3.0] {
            s.push(v);
        }
        assert_eq!(s.take().unwrap().median, 2.0);
    }

    #[test]
    fn take_resets() {
        let mut s = Series::new();
        s.push(7.0);
        assert!(s.take().is_some());
        assert_eq!(s.take(), None);
        s.push(2.0);
        let t = s.take().unwrap();
        assert_eq!((t.median, t.max, t.count), (2.0, 2.0, 1));
    }

    #[test]
    fn negative_values_keep_their_max() {
        let mut s = Series::new();
        s.push(-3.0);
        s.push(-1.0);
        assert_eq!(s.take().unwrap().max, -1.0);
    }

    #[test]
    fn overflow_counts_and_keeps_max_without_growing() {
        let mut s = Series::new();
        for i in 0..(CAPACITY + 10) {
            s.push(i as f32);
        }
        assert_eq!(s.samples.len(), CAPACITY);
        let t = s.take().unwrap();
        assert_eq!(t.count, (CAPACITY + 10) as u64);
        assert_eq!(t.max, (CAPACITY + 9) as f32);
    }

    #[test]
    fn push_since_is_not_negative() {
        let mut s = Series::new();
        s.push_since(Instant::now());
        assert!(s.take().unwrap().max >= 0.0);
    }
}
