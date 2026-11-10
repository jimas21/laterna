//! Tragus: the link to its service, and pausing a video when an earbud comes out.

use super::*;
use fp_earbuds::policy::{Context, Decision, EarbudPolicy, PlayerNow};
use fp_earbuds::{EarbudsConfig, EarbudsLink, PauseRule};

/// How often to look for Tragus being installed while no link runs.
const LOOK_EVERY: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct EarbudsRun {
    pub(super) link: Option<EarbudsLink>,
    policy: EarbudPolicy,
    /// Which video is open, and how many have been opened.
    open: Option<String>,
    seq: u64,
    looked: Option<Instant>,
}

/// The policy's view of the player.
fn player_now(state: PlayerState) -> PlayerNow {
    match state {
        PlayerState::Buffering => PlayerNow::Buffering,
        PlayerState::Playing => PlayerNow::Playing,
        PlayerState::Paused => PlayerNow::Paused,
        PlayerState::Ended => PlayerNow::Ended,
        PlayerState::Failed => PlayerNow::Failed,
    }
}

impl App {
    /// Once per frame: keeps the link to Tragus (on its own thread, so nothing here
    /// waits), shares its state with the UI and pauses the video when told to.
    pub(super) fn update_earbuds(&mut self) {
        let now = Instant::now();
        let run = &mut self.earbuds;
        if run.link.is_none()
            && run
                .looked
                .is_none_or(|t| now.duration_since(t) >= LOOK_EVERY)
        {
            run.looked = Some(now);
            if crate::tragus::installed().is_some() {
                run.link = EarbudsLink::spawn(EarbudsConfig::default()).ok();
            }
        }
        let Some(link) = run.link.as_mut() else {
            return;
        };
        if link.poll() {
            self.ui.earbuds = link.state().clone();
        }
        let open = self.playback.as_ref().map(|p| p.title.clone());
        let opened = open.is_some() && open != run.open;
        if opened {
            run.seq += 1;
        }
        run.open = open;
        let (player, paused) = match &self.playback {
            Some(p) => (player_now(p.player.state()), p.player.is_paused()),
            None => (PlayerNow::Closed, false),
        };
        let cx = Context {
            rule: if self.settings.pause_on_earbud_out {
                PauseRule::EitherBud
            } else {
                PauseRule::Off
            },
            // Putting the earbud back never resumes; that stays with the user.
            resume_on_reinsert: false,
            player,
            paused,
            open_seq: run.seq,
            now,
        };
        let reading = link.state().reading();
        let decision = if opened {
            run.policy.opened(reading, &cx)
        } else {
            run.policy.step(reading, &cx)
        };
        if decision == Decision::Pause
            && let Some(p) = &mut self.playback
        {
            p.player.pause();
            p.sync_watch();
            p.save_progress(&self.services.library, true);
            self.ui.toast(tr!("toast-paused-earbud-out"));
            self.ui_visible = true;
            self.last_activity = Instant::now();
        }
    }
}
