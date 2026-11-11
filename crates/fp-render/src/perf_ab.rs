//! Live knobs for the place pass, and the A/B cycle that measures them.
//!
//! Environment (read once, when the renderer is made):
//!
//! - `FRAMEPLAYERX_PLACE_BASIS=0`: a baked place leaves out the picture's
//!   light (eight texture reads a pixel).
//! - `FRAMEPLAYERX_PLACE_SORT=0`: a baked place is drawn in the order its
//!   pack was written, not nearest first.
//! - `FRAMEPLAYERX_BASIS_ANISO=1`: the basis textures are read with the
//!   anisotropic sampler like the colours (the default is trilinear).
//! - `FRAMEPLAYERX_PERF_AB=1`: while a place is shown, step through
//!   [`VARIANTS`] ten seconds each and log the GPU time of each, so one
//!   headset session gives the table. The samples the eye passes are drawn
//!   with change on the fly (the pipelines are made again: a hitch at each
//!   change, which the first second of a variant is left out for).
//!
//! `FRAMEPLAYERX_MSAA` (in the app) is the sample count the session starts
//! with; the cycle then overrides it.

use crate::timing::Series;
use std::time::{Duration, Instant};

/// What the place pass is doing, as far as the knobs go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Variant {
    /// Samples a pixel of the eye passes: 1, 2 or 4.
    pub samples: u32,
    /// The picture's light on a baked place.
    pub basis: bool,
    /// A baked place drawn nearest first.
    pub sort: bool,
}

impl Variant {
    pub fn label(&self) -> String {
        format!(
            "msaa{} basis={} sort={}",
            self.samples,
            on(self.basis),
            on(self.sort)
        )
    }
}

fn on(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// What the cycle steps through. The first is what beta.10 did (nothing
/// sorted, the light on, 4x); then the new default and what each part costs.
pub(crate) const VARIANTS: [Variant; 7] = [
    Variant {
        samples: 4,
        basis: true,
        sort: false,
    },
    Variant {
        samples: 4,
        basis: true,
        sort: true,
    },
    Variant {
        samples: 4,
        basis: false,
        sort: true,
    },
    Variant {
        samples: 2,
        basis: true,
        sort: true,
    },
    Variant {
        samples: 2,
        basis: false,
        sort: true,
    },
    Variant {
        samples: 1,
        basis: true,
        sort: true,
    },
    Variant {
        samples: 1,
        basis: false,
        sort: true,
    },
];

/// How long each variant is measured.
pub(crate) const DWELL: Duration = Duration::from_secs(10);
/// How much of the start of a variant is left out of its numbers: the
/// pipelines are new, the GPU clock settles, and the frames the GPU
/// timestamps belong to (two back) are the last variant's.
pub(crate) const SETTLE: Duration = Duration::from_millis(1500);

/// The knobs read from the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Knobs {
    pub basis: bool,
    pub sort: bool,
    pub basis_aniso: bool,
    pub ab: bool,
}

impl Knobs {
    pub fn from_env() -> Knobs {
        Knobs::parse(|name| std::env::var(name).ok())
    }

    pub fn parse(get: impl Fn(&str) -> Option<String>) -> Knobs {
        let value = |name: &str| get(name).map(|v| v.trim().to_string());
        // On unless "0" (or "off"/"false"/"no").
        let default_on =
            |name: &str| !matches!(value(name).as_deref(), Some("0" | "off" | "false" | "no"));
        // Off unless set to something other than "", "0", "off", "false", "no".
        let default_off = |name: &str| {
            !matches!(
                value(name).as_deref(),
                None | Some("" | "0" | "off" | "false" | "no")
            )
        };
        Knobs {
            basis: default_on("FRAMEPLAYERX_PLACE_BASIS"),
            sort: default_on("FRAMEPLAYERX_PLACE_SORT"),
            basis_aniso: default_off("FRAMEPLAYERX_BASIS_ANISO"),
            ab: default_off("FRAMEPLAYERX_PERF_AB"),
        }
    }

    /// The knobs that differ from the defaults, for the log.
    pub fn describe(&self) -> String {
        let mut set = Vec::new();
        if !self.basis {
            set.push("FRAMEPLAYERX_PLACE_BASIS=0");
        }
        if !self.sort {
            set.push("FRAMEPLAYERX_PLACE_SORT=0");
        }
        if self.basis_aniso {
            set.push("FRAMEPLAYERX_BASIS_ANISO=1");
        }
        if self.ab {
            set.push("FRAMEPLAYERX_PERF_AB=1");
        }
        if set.is_empty() {
            "none".into()
        } else {
            set.join(" ")
        }
    }
}

/// One variant's result.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Measured {
    pub variant: Variant,
    /// Frames measured.
    pub frames: u64,
    /// GPU ms for the whole frame, median and worst.
    pub total: Option<(f32, f32)>,
    /// GPU ms of the eye passes, median and worst.
    pub eyes: Option<(f32, f32)>,
}

impl Measured {
    pub fn line(&self) -> String {
        let show = |v: Option<(f32, f32)>| {
            v.map_or("-".to_string(), |(m, x)| format!("{m:.2}/{x:.2}"))
        };
        format!(
            "{:<26} gpu {} ms, eyes {} ms (median/max, {} frames)",
            self.variant.label(),
            show(self.total),
            show(self.eyes),
            self.frames
        )
    }
}

/// Steps through [`VARIANTS`] while a place is shown.
pub(crate) struct AbCycle {
    index: usize,
    /// When the current variant began, or `None` before the first frame
    /// with a place (or after a pause).
    began: Option<Instant>,
    total: Series,
    eyes: Series,
    done: Vec<Measured>,
}

/// What a frame's observation asks of the renderer.
#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    /// Nothing.
    Stay,
    /// Switch to this variant (the first, or the next).
    Switch(Variant),
}

impl AbCycle {
    pub fn new() -> AbCycle {
        AbCycle {
            index: 0,
            began: None,
            total: Series::new(),
            eyes: Series::new(),
            done: Vec::new(),
        }
    }

    pub fn current(&self) -> Variant {
        VARIANTS[self.index]
    }

    /// A frame with no place shown: the clock stops and what the current
    /// variant has collected is thrown away, so every variant is measured
    /// with the place on screen.
    pub fn idle(&mut self) {
        self.began = None;
        self.total.take();
        self.eyes.take();
    }

    /// A frame with a place shown at `now`, and the GPU times that came in
    /// with it (the frame two back). The first frame starts the first
    /// variant; every [`DWELL`] after, its numbers are final and the next
    /// variant starts.
    pub fn observe(&mut self, now: Instant, total: Option<f32>, eyes: Option<f32>) -> Step {
        let Some(began) = self.began else {
            self.began = Some(now);
            self.total.take();
            self.eyes.take();
            return Step::Switch(self.current());
        };
        let age = now.saturating_duration_since(began);
        if (SETTLE..DWELL).contains(&age) {
            if let Some(t) = total {
                self.total.push(t);
            }
            if let Some(e) = eyes {
                self.eyes.push(e);
            }
        }
        if age < DWELL {
            return Step::Stay;
        }
        let (total, eyes) = (self.total.take(), self.eyes.take());
        let measured = Measured {
            variant: self.current(),
            frames: total.map_or(0, |s| s.count),
            total: total.map(|s| (s.median, s.max)),
            eyes: eyes.map(|s| (s.median, s.max)),
        };
        log::info!("perf-ab: {}", measured.line());
        self.done.push(measured);
        self.index += 1;
        if self.index == VARIANTS.len() {
            self.index = 0;
            log::info!("perf-ab table:\n{}", self.table());
            self.done.clear();
        }
        self.began = Some(now);
        Step::Switch(self.current())
    }

    /// The variants measured in this round, one to a line.
    pub fn table(&self) -> String {
        self.done
            .iter()
            .map(Measured::line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[cfg(test)]
    pub fn results(&self) -> &[Measured] {
        &self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn the_defaults_keep_the_light_and_the_sort_and_turn_the_rest_off() {
        let k = Knobs::parse(env(&[]));
        assert_eq!(
            k,
            Knobs {
                basis: true,
                sort: true,
                basis_aniso: false,
                ab: false
            }
        );
        assert_eq!(k.describe(), "none");
    }

    #[test]
    fn each_knob_reads_its_own_variable() {
        let k = Knobs::parse(env(&[("FRAMEPLAYERX_PLACE_BASIS", "0")]));
        assert!(!k.basis && k.sort);
        assert_eq!(k.describe(), "FRAMEPLAYERX_PLACE_BASIS=0");
        let k = Knobs::parse(env(&[
            ("FRAMEPLAYERX_PLACE_SORT", " 0 "),
            ("FRAMEPLAYERX_PERF_AB", "1"),
            ("FRAMEPLAYERX_BASIS_ANISO", "1"),
        ]));
        assert!(k.basis && !k.sort && k.ab && k.basis_aniso);
        assert_eq!(
            k.describe(),
            "FRAMEPLAYERX_PLACE_SORT=0 FRAMEPLAYERX_BASIS_ANISO=1 FRAMEPLAYERX_PERF_AB=1"
        );
        // Anything but 0 leaves a default-on knob on; 0 or empty leaves a
        // default-off one off.
        assert!(Knobs::parse(env(&[("FRAMEPLAYERX_PLACE_BASIS", "1")])).basis);
        assert!(!Knobs::parse(env(&[("FRAMEPLAYERX_PERF_AB", "0")])).ab);
        assert!(!Knobs::parse(env(&[("FRAMEPLAYERX_PERF_AB", "")])).ab);
    }

    #[test]
    fn the_cycle_starts_with_beta_10s_behaviour_and_covers_the_asked_grid() {
        assert_eq!(
            VARIANTS[0],
            Variant {
                samples: 4,
                basis: true,
                sort: false
            }
        );
        // Every sample count with the light on and off, sorted.
        for samples in [4, 2, 1] {
            for basis in [true, false] {
                assert!(
                    VARIANTS.contains(&Variant {
                        samples,
                        basis,
                        sort: true
                    }),
                    "{samples} {basis}"
                );
            }
        }
        // No variant twice.
        for (i, a) in VARIANTS.iter().enumerate() {
            assert!(!VARIANTS[i + 1..].contains(a));
        }
    }

    #[test]
    fn a_variant_is_measured_for_ten_seconds_after_the_settling_time() {
        let t0 = Instant::now();
        let mut c = AbCycle::new();
        assert_eq!(c.observe(t0, Some(9.0), Some(8.0)), Step::Switch(VARIANTS[0]));
        // Settling: not counted.
        assert_eq!(
            c.observe(t0 + Duration::from_millis(500), Some(50.0), Some(50.0)),
            Step::Stay
        );
        for ms in (2000..10_000).step_by(100) {
            let now = t0 + Duration::from_millis(ms);
            assert_eq!(c.observe(now, Some(8.0), Some(7.0)), Step::Stay, "{ms}");
        }
        // Ten seconds: the next variant, and the first one's numbers.
        let step = c.observe(t0 + DWELL, Some(8.0), Some(7.0));
        assert_eq!(step, Step::Switch(VARIANTS[1]));
        let first = &c.results()[0];
        assert_eq!(first.variant, VARIANTS[0]);
        assert_eq!(first.total, Some((8.0, 8.0)));
        assert_eq!(first.eyes, Some((7.0, 7.0)));
        assert_eq!(first.frames, 80);
    }

    #[test]
    fn a_pause_without_a_place_starts_the_variant_over() {
        let t0 = Instant::now();
        let mut c = AbCycle::new();
        c.observe(t0, None, None);
        c.observe(t0 + Duration::from_secs(5), Some(8.0), None);
        c.idle();
        // The place is back: the same variant, a fresh ten seconds.
        let t1 = t0 + Duration::from_secs(60);
        assert_eq!(c.observe(t1, None, None), Step::Switch(VARIANTS[0]));
        assert_eq!(
            c.observe(t1 + Duration::from_secs(9), Some(8.0), None),
            Step::Stay
        );
        assert!(c.results().is_empty());
    }

    #[test]
    fn a_round_ends_with_the_table_and_starts_again() {
        let t0 = Instant::now();
        let mut c = AbCycle::new();
        c.observe(t0, None, None);
        let mut now = t0;
        let mut seen = vec![VARIANTS[0]];
        for _ in 0..VARIANTS.len() {
            now += DWELL;
            if let Step::Switch(v) = c.observe(now, Some(8.0), Some(7.0)) {
                seen.push(v);
            }
        }
        // All of them, then the first again.
        assert_eq!(seen.len(), VARIANTS.len() + 1);
        assert_eq!(seen.last(), Some(&VARIANTS[0]));
        assert!(c.results().is_empty());
        assert_eq!(c.current(), VARIANTS[0]);
    }

    #[test]
    fn a_line_says_the_variant_and_its_numbers() {
        let m = Measured {
            variant: VARIANTS[2],
            frames: 640,
            total: Some((8.5, 11.2)),
            eyes: None,
        };
        let line = m.line();
        assert!(line.contains("msaa4 basis=off sort=on"), "{line}");
        assert!(line.contains("8.50/11.20"), "{line}");
        assert!(line.contains("eyes - ms"), "{line}");
        assert!(line.contains("640 frames"), "{line}");
    }
}
