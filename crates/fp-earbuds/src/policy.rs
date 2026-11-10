//! When to pause or resume the open video as earbuds come out and go back in.
//!
//! Pure: the app passes the player's state and the time, and acts on the answer. The worker
//! never touches the player, and nothing in here reads a clock.
//!
//! The app pauses itself. It does not rely on the core's own media pause, which can only
//! reach players that speak MPRIS. A pause this policy asks for is remembered as a hold and is
//! the only kind it ever resumes.

use std::time::{Duration, Instant};

use crate::settings::PauseRule;
use crate::state::{Wear, WearReading};

/// What the player is doing, as far as this policy cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerNow {
    /// Nothing open.
    Closed,
    /// Open and waiting for data.
    Buffering,
    /// Open and running.
    Playing,
    /// Open and paused.
    Paused,
    /// Reached the end; playing again would restart it.
    Ended,
    /// Could not play.
    Failed,
}

/// What the app tells the policy each frame.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// The user's pause rule.
    pub rule: PauseRule,
    /// Whether a pause made here may be undone when the earbuds are back in.
    pub resume_on_reinsert: bool,
    /// The player's state.
    pub player: PlayerNow,
    /// Whether the player is paused. A video that ended can be paused too.
    pub paused: bool,
    /// Counts the videos opened; a hold belongs to one of them.
    pub open_seq: u64,
    /// The time, taken once for the frame.
    pub now: Instant,
}

/// What the app should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Leave the player alone.
    Nothing,
    /// Pause it.
    Pause,
    /// Play it again.
    Resume,
}

/// Remembers what was last seen and whether this policy owns the current pause.
#[derive(Debug, Default)]
pub struct EarbudPolicy {
    /// The `open_seq` of the video this policy paused, while it owns that pause.
    hold: Option<u64>,
    /// The wear epoch of the readings seen so far.
    epoch: Option<u64>,
    /// The last meaningful reading in this epoch.
    seen: Option<Wear>,
    /// When both earbuds went in, while they are in.
    worn_since: Option<Instant>,
}

impl EarbudPolicy {
    /// How long both earbuds must stay in before a resume.
    pub const SETTLE: Duration = Duration::from_secs(1);

    /// Call once per frame with the latest reading.
    ///
    /// The same reading may arrive any number of times: only a move into the set the rule
    /// pauses in, from a known value outside it, pauses. The first reading of an epoch
    /// (after a reconnect, say) only seeds. A closed-case report straight after a reading of
    /// worn or partial counts as not worn.
    pub fn step(&mut self, r: WearReading, cx: &Context) -> Decision {
        if cx.rule == PauseRule::Off {
            self.hold = None;
            return Decision::Nothing;
        }
        self.check_hold(cx);
        if self.epoch != Some(r.epoch) {
            self.epoch = Some(r.epoch);
            self.seen = None;
            self.worn_since = None;
        }
        if !r.usable {
            return Decision::Nothing;
        }
        let Some(wear) = self.meaningful(r.wear) else {
            self.worn_since = None;
            return Decision::Nothing;
        };
        let prev = self.seen.replace(wear);
        if wear == Wear::Worn {
            let _ = self.worn_since.get_or_insert(cx.now);
        } else {
            self.worn_since = None;
        }
        let enters = prev.is_some_and(|p| !cx.rule.pauses_on(p)) && cx.rule.pauses_on(wear);
        if enters {
            return self.pause_if_playing(cx);
        }
        self.resume_if_settled(cx)
    }

    /// A video just opened: pause it at once if an earbud is already out.
    pub fn opened(&mut self, r: WearReading, cx: &Context) -> Decision {
        let fresh = r.usable && self.epoch == Some(r.epoch);
        let out = self.seen.is_some_and(|w| cx.rule.pauses_on(w));
        if fresh && out {
            self.pause_if_playing(cx)
        } else {
            Decision::Nothing
        }
    }

    /// Gives up a pause this policy owns, without resuming: the user played or paused, the
    /// video closed, or the feature was turned off.
    pub fn release(&mut self) {
        self.hold = None;
    }

    /// Whether this policy owns the current pause.
    pub fn holds(&self) -> bool {
        self.hold.is_some()
    }

    /// Drops a hold that no longer describes the player.
    fn check_hold(&mut self, cx: &Context) {
        let other_video = self.hold.is_some_and(|seq| seq != cx.open_seq);
        if other_video || !cx.paused || cx.player == PlayerNow::Closed {
            self.hold = None;
        }
    }

    /// The reading to act on, or `None` for one to ignore.
    fn meaningful(&self, wear: Option<Wear>) -> Option<Wear> {
        match wear? {
            Wear::Unknown => None,
            Wear::NotAvailable => match self.seen {
                Some(Wear::Worn | Wear::Partial) => Some(Wear::NotWorn),
                _ => None,
            },
            known => Some(known),
        }
    }

    fn pause_if_playing(&mut self, cx: &Context) -> Decision {
        let running = matches!(cx.player, PlayerNow::Playing | PlayerNow::Buffering);
        if self.hold.is_none() && running && !cx.paused {
            self.hold = Some(cx.open_seq);
            Decision::Pause
        } else {
            Decision::Nothing
        }
    }

    fn resume_if_settled(&mut self, cx: &Context) -> Decision {
        let (Some(seq), Some(since)) = (self.hold, self.worn_since) else {
            return Decision::Nothing;
        };
        if cx.now.saturating_duration_since(since) < Self::SETTLE {
            return Decision::Nothing;
        }
        self.hold = None;
        let playable = !matches!(cx.player, PlayerNow::Ended | PlayerNow::Failed);
        if cx.resume_on_reinsert && seq == cx.open_seq && cx.paused && playable {
            Decision::Resume
        } else {
            Decision::Nothing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::{Nothing, Pause, Resume};
    use PlayerNow::{Buffering, Closed, Ended, Failed, Paused, Playing};
    use Wear::{NotAvailable, NotWorn, Partial, Unknown, Worn};

    const SEQ: u64 = 7;

    /// A policy plus a fixed base time; every call names the player's state and the time
    /// in milliseconds after the base.
    struct Rig {
        p: EarbudPolicy,
        rule: PauseRule,
        resume: bool,
        seq: u64,
        t0: Instant,
    }

    impl Rig {
        fn new(rule: PauseRule) -> Rig {
            Rig {
                p: EarbudPolicy::default(),
                rule,
                resume: true,
                seq: SEQ,
                t0: Instant::now(),
            }
        }

        fn cx(&self, player: PlayerNow, ms: u64) -> Context {
            Context {
                rule: self.rule,
                resume_on_reinsert: self.resume,
                player,
                paused: matches!(player, Paused | Ended),
                open_seq: self.seq,
                now: self.t0 + Duration::from_millis(ms),
            }
        }

        fn feed(&mut self, wear: Wear, player: PlayerNow, ms: u64) -> Decision {
            self.feed_reading(read(wear), player, ms)
        }

        fn feed_reading(&mut self, r: WearReading, player: PlayerNow, ms: u64) -> Decision {
            let c = self.cx(player, ms);
            self.p.step(r, &c)
        }

        fn open(&mut self, r: WearReading, player: PlayerNow, ms: u64) -> Decision {
            let c = self.cx(player, ms);
            self.p.opened(r, &c)
        }
    }

    fn read(wear: Wear) -> WearReading {
        read_in(0, wear)
    }

    fn read_in(epoch: u64, wear: Wear) -> WearReading {
        WearReading {
            epoch,
            wear: Some(wear),
            usable: true,
        }
    }

    /// Plays; seeds with `first`; then moves to `then` and returns the decision.
    fn after(rule: PauseRule, first: Wear, then: Wear) -> Decision {
        let mut rig = Rig::new(rule);
        assert_eq!(rig.feed(first, Playing, 0), Nothing);
        rig.feed(then, Playing, 1)
    }

    fn check(rule: PauseRule, cases: &[(Wear, Wear, Decision)]) {
        for &(first, then, want) in cases {
            assert_eq!(after(rule, first, then), want, "{first:?} {then:?}");
        }
    }

    /// Plays, then pauses because a bud came out at 10 ms.
    fn paused_by_us(rule: PauseRule) -> Rig {
        let mut rig = Rig::new(rule);
        rig.feed(Worn, Playing, 0);
        assert_eq!(rig.feed(NotWorn, Playing, 10), Pause);
        rig
    }

    #[test]
    fn the_first_reading_only_seeds() {
        let mut rig = Rig::new(PauseRule::EitherBud);
        assert_eq!(rig.feed(NotWorn, Playing, 0), Nothing);
        assert!(!rig.p.holds());
    }

    #[test]
    fn either_bud_pauses_on_the_first_bud_out() {
        let cases = [
            (Worn, Partial, Pause),
            (Worn, NotWorn, Pause),
            (Partial, NotWorn, Nothing),
            (NotWorn, Partial, Nothing),
            (Worn, Worn, Nothing),
        ];
        check(PauseRule::EitherBud, &cases);
    }

    #[test]
    fn both_buds_waits_for_the_second_bud() {
        let cases = [
            (Worn, Partial, Nothing),
            (Partial, NotWorn, Pause),
            (Worn, NotWorn, Pause),
        ];
        check(PauseRule::BothBuds, &cases);
    }

    #[test]
    fn off_never_pauses() {
        let cases = [(Worn, Partial, Nothing), (Worn, NotWorn, Nothing)];
        check(PauseRule::Off, &cases);
    }

    #[test]
    fn a_burst_of_the_same_reading_pauses_once() {
        let mut rig = Rig::new(PauseRule::EitherBud);
        rig.feed(Worn, Playing, 0);
        assert_eq!(rig.feed(Partial, Playing, 1), Pause);
        // The app has paused; the same reading arrives again, in bursts and on later frames.
        for ms in [20, 21, 22, 40, 60] {
            assert_eq!(rig.feed(Partial, Paused, ms), Nothing);
        }
        assert!(rig.p.holds());
    }

    #[test]
    fn a_pause_the_user_made_is_left_alone() {
        let mut rig = Rig::new(PauseRule::EitherBud);
        rig.feed(Worn, Paused, 0);
        assert_eq!(rig.feed(NotWorn, Paused, 10), Nothing);
        assert!(!rig.p.holds());
        rig.feed(Worn, Paused, 100);
        assert_eq!(rig.feed(Worn, Paused, 2000), Nothing);
    }

    #[test]
    fn only_a_running_video_is_paused() {
        let cases = [
            (Playing, Pause),
            (Buffering, Pause),
            (Ended, Nothing),
            (Failed, Nothing),
            (Closed, Nothing),
        ];
        for (player, want) in cases {
            let mut rig = Rig::new(PauseRule::EitherBud);
            rig.feed(Worn, player, 0);
            assert_eq!(rig.feed(NotWorn, player, 1), want, "{player:?}");
        }
    }

    #[test]
    fn it_resumes_after_the_settle_time() {
        let mut rig = paused_by_us(PauseRule::EitherBud);
        assert_eq!(rig.feed(Worn, Paused, 100), Nothing);
        assert_eq!(rig.feed(Worn, Paused, 600), Nothing);
        assert_eq!(rig.feed(Worn, Paused, 1100), Resume);
        assert!(!rig.p.holds());
        // Once resumed, further readings do nothing.
        assert_eq!(rig.feed(Worn, Playing, 1200), Nothing);
    }

    #[test]
    fn a_bud_coming_out_again_restarts_the_settle_time() {
        let mut rig = paused_by_us(PauseRule::EitherBud);
        rig.feed(Worn, Paused, 100);
        rig.feed(Partial, Paused, 400);
        assert_eq!(rig.feed(Worn, Paused, 500), Nothing);
        assert_eq!(rig.feed(Worn, Paused, 1400), Nothing);
        assert_eq!(rig.feed(Worn, Paused, 1500), Resume);
    }

    #[test]
    fn resume_can_be_turned_off() {
        let mut rig = paused_by_us(PauseRule::EitherBud);
        rig.resume = false;
        rig.feed(Worn, Paused, 100);
        assert_eq!(rig.feed(Worn, Paused, 1200), Nothing);
        assert!(!rig.p.holds());
    }

    #[test]
    fn the_user_taking_over_ends_the_hold() {
        let mut rig = paused_by_us(PauseRule::EitherBud);
        rig.p.release();
        assert!(!rig.p.holds());
        rig.feed(Worn, Paused, 100);
        assert_eq!(rig.feed(Worn, Paused, 1500), Nothing);
    }

    #[test]
    fn another_video_or_a_played_video_ends_the_hold() {
        let mut rig = paused_by_us(PauseRule::EitherBud);
        rig.seq = SEQ + 1;
        rig.feed(Worn, Paused, 100);
        assert!(!rig.p.holds());
        assert_eq!(rig.feed(Worn, Paused, 1500), Nothing);

        let mut rig = paused_by_us(PauseRule::EitherBud);
        rig.feed(Worn, Playing, 100);
        assert!(!rig.p.holds());
    }

    #[test]
    fn a_video_that_ended_is_never_played_again() {
        let mut rig = paused_by_us(PauseRule::EitherBud);
        rig.feed(Worn, Ended, 100);
        assert_eq!(rig.feed(Worn, Ended, 1500), Nothing);
        assert!(!rig.p.holds());
    }

    #[test]
    fn turning_the_rule_off_drops_the_hold() {
        let mut rig = paused_by_us(PauseRule::EitherBud);
        rig.rule = PauseRule::Off;
        assert_eq!(rig.feed(Worn, Paused, 100), Nothing);
        assert!(!rig.p.holds());
        assert_eq!(rig.feed(Worn, Paused, 2000), Nothing);
    }

    #[test]
    fn a_new_epoch_reseeds_but_keeps_the_hold() {
        let mut rig = paused_by_us(PauseRule::EitherBud);
        let back = read_in(1, Worn);
        assert_eq!(rig.feed_reading(back, Paused, 100), Nothing);
        assert_eq!(rig.feed_reading(back, Paused, 1200), Resume);

        // The first reading after the reconnect says the buds are out: neither pause nor resume.
        let mut rig = paused_by_us(PauseRule::EitherBud);
        let out = read_in(1, NotWorn);
        assert_eq!(rig.feed_reading(out, Paused, 100), Nothing);
        assert_eq!(rig.feed_reading(out, Paused, 2000), Nothing);
        assert!(rig.p.holds());
    }

    #[test]
    fn unknown_and_absent_readings_are_ignored() {
        let mut rig = Rig::new(PauseRule::EitherBud);
        let gone = WearReading {
            epoch: 0,
            wear: None,
            usable: true,
        };
        rig.feed(Worn, Playing, 0);
        assert_eq!(rig.feed(Unknown, Playing, 1), Nothing);
        assert_eq!(rig.feed_reading(gone, Playing, 2), Nothing);
        assert_eq!(rig.feed(Worn, Playing, 3), Nothing);
        assert_eq!(rig.feed(Unknown, Playing, 4), Nothing);
        assert_eq!(rig.feed(NotWorn, Playing, 5), Pause);
    }

    #[test]
    fn an_unusable_reading_does_nothing() {
        let mut rig = Rig::new(PauseRule::EitherBud);
        rig.feed(Worn, Playing, 0);
        let mut r = read(NotWorn);
        r.usable = false;
        assert_eq!(rig.feed_reading(r, Playing, 1), Nothing);
        assert!(!rig.p.holds());
    }

    #[test]
    fn a_closed_case_report_after_worn_counts_as_not_worn() {
        let cases = [
            (Worn, NotAvailable, Pause),
            (Partial, NotAvailable, Nothing),
            (NotWorn, NotAvailable, Nothing),
        ];
        check(PauseRule::EitherBud, &cases);
        let cases = [
            (Worn, NotAvailable, Pause),
            (Partial, NotAvailable, Pause),
            (NotWorn, NotAvailable, Nothing),
        ];
        check(PauseRule::BothBuds, &cases);
    }

    #[test]
    fn a_closed_case_report_from_nothing_is_ignored() {
        let cases = [(NotWorn, NotAvailable, Nothing)];
        check(PauseRule::EitherBud, &cases);
        // Nothing seen yet: the report neither seeds nor pauses.
        let mut rig = Rig::new(PauseRule::EitherBud);
        assert_eq!(rig.feed(NotAvailable, Playing, 0), Nothing);
        assert_eq!(rig.feed(NotWorn, Playing, 1), Nothing);
    }

    #[test]
    fn after_a_closed_case_pause_the_buds_going_back_in_resume() {
        let mut rig = Rig::new(PauseRule::EitherBud);
        rig.feed(Worn, Playing, 0);
        assert_eq!(rig.feed(NotAvailable, Playing, 10), Pause);
        assert_eq!(rig.feed(NotAvailable, Paused, 50), Nothing);
        rig.feed(Worn, Paused, 500);
        assert_eq!(rig.feed(Worn, Paused, 1600), Resume);
    }

    /// Seeds with `first` while nothing is open, then opens a video and asks.
    fn opened_with(rule: PauseRule, first: Wear) -> Decision {
        let mut rig = Rig::new(rule);
        rig.feed(first, Closed, 0);
        let d = rig.open(read(first), Playing, 10);
        assert_eq!(rig.p.holds(), d == Pause);
        d
    }

    #[test]
    fn a_video_opened_with_a_bud_out_is_paused_at_once() {
        let either = PauseRule::EitherBud;
        assert_eq!(opened_with(either, Partial), Pause);
        assert_eq!(opened_with(either, NotWorn), Pause);
        assert_eq!(opened_with(either, Worn), Nothing);
        let both = PauseRule::BothBuds;
        assert_eq!(opened_with(both, Partial), Nothing);
        assert_eq!(opened_with(both, NotWorn), Pause);
        assert_eq!(opened_with(PauseRule::Off, NotWorn), Nothing);
    }

    #[test]
    fn opened_needs_a_current_reading() {
        let mut rig = Rig::new(PauseRule::EitherBud);
        // Never seeded.
        assert_eq!(rig.open(read(NotWorn), Playing, 0), Nothing);
        // Seeded, then the readings started afresh.
        rig.feed(NotWorn, Playing, 1);
        assert_eq!(rig.open(read_in(3, NotWorn), Playing, 2), Nothing);
        // Seeded and current, but not usable.
        let mut unusable = read(NotWorn);
        unusable.usable = false;
        assert_eq!(rig.open(unusable, Playing, 3), Nothing);
    }

    #[test]
    fn a_hold_made_on_open_resumes_like_any_other() {
        let mut rig = Rig::new(PauseRule::EitherBud);
        rig.feed(NotWorn, Closed, 0);
        assert_eq!(rig.open(read(NotWorn), Playing, 10), Pause);
        rig.feed(Worn, Paused, 100);
        assert_eq!(rig.feed(Worn, Paused, 1200), Resume);
    }
}
