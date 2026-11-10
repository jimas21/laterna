//! Pausing a video when the headset comes off.

use super::*;

/// Whether the headset going from `was` on to `now` on pauses the video:
/// only when the setting is on, the headset really went from on to off, and
/// the video is playing. It never resumes, and leaves an ended or already
/// paused video alone.
pub(super) fn pauses_on_removal(setting: bool, was: bool, now: bool, playing: bool) -> bool {
    setting && was && !now && playing
}

impl App {
    /// The headset went on or off (see [`fp_xr::is_worn`]). Called as the
    /// session's state changes, whether or not frames are being rendered.
    pub fn headset_worn_changed(&mut self, was: bool, now: bool) {
        let state = self.playback.as_ref().map(|p| p.player.state());
        let playing = matches!(state, Some(PlayerState::Playing | PlayerState::Buffering));
        if pauses_on_removal(self.settings.pause_on_headset_off, was, now, playing)
            && let Some(p) = &mut self.playback
        {
            p.player.pause();
            // No frame runs while the headset is off, so tell the server now.
            p.sync_watch();
            // Taking the headset off may be the last thing done.
            p.save_progress(&self.services.library, &self.services.writer, true);
            self.headset_paused = true;
            self.ui_visible = true;
        } else if now && !was && std::mem::take(&mut self.headset_paused) {
            if self.playback.is_some() {
                self.ui.toast(tr!("toast-paused-headset-off"));
            }
            self.ui_visible = true;
            self.last_activity = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pauses_on_removal;

    #[test]
    fn pauses_only_when_a_playing_video_loses_the_headset() {
        // (setting, was on, now on, playing)
        assert!(pauses_on_removal(true, true, false, true));
        // The setting is off.
        assert!(!pauses_on_removal(false, true, false, true));
        // Nothing playing: paused, ended or no video.
        assert!(!pauses_on_removal(true, true, false, false));
        // Still worn: the dashboard over the app (visible, not focused).
        assert!(!pauses_on_removal(true, true, true, true));
        // Going back on never pauses, and never resumes.
        assert!(!pauses_on_removal(true, false, true, true));
        // Already off.
        assert!(!pauses_on_removal(true, false, false, true));
    }
}
