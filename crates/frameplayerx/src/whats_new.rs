//! What changed in each version, in plain words, for the "What's new" card
//! shown once after an update (and from Settings > About).
//!
//! Add a new version's entry at the top of [`ENTRIES`]: three to six short
//! one-line bullets about what a person will notice.

/// One version's notes.
pub struct Entry {
    pub version: &'static str,
    pub bullets: &'static [&'static str],
}

/// Newest first.
pub const ENTRIES: &[Entry] = &[Entry {
    version: "0.18.0-beta.8",
    bullets: &[
        "Smoother edges everywhere (4x edge smoothing is now on).",
        "The cinema is rebuilt like a real theater: the front row is no longer at the screen, and nothing covers the picture.",
        "Screen size has simple choices: Home theater, Cinema, Giant. Change it while watching with the Size button.",
        "The room takes the film's colour as it plays, with a Screen light slider in Settings > Places.",
        "Recenter while lying back and the whole room tilts to face you.",
        "Plex: switch between people in your Plex Home, playlists and a richer Home page.",
    ],
}];

/// The notes written for exactly `version`.
pub fn entry(version: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|e| e.version == version)
}

/// What About's "What's new" shows: this version's notes, or the newest
/// there are (a development build has none of its own).
pub fn for_about(current: &str) -> Option<&'static Entry> {
    entry(current).or_else(|| ENTRIES.first())
}

/// The notes to show at start. `last` is the version that ran before: empty
/// on the very first run (nothing to compare, so nothing is shown). Shown
/// only when the version changed and has notes of its own.
pub fn to_show(last: &str, current: &str) -> Option<&'static Entry> {
    if last.is_empty() || last == current {
        return None;
    }
    entry(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_run_shows_nothing() {
        assert!(to_show("", "0.18.0-beta.8").is_none());
    }

    #[test]
    fn an_update_shows_the_new_versions_notes_once() {
        let e = to_show("0.18.0-beta.7", "0.18.0-beta.8").unwrap();
        assert_eq!(e.version, "0.18.0-beta.8");
        // A settings file from before versions were kept counts as an update.
        assert!(to_show(crate::settings::EARLIER_VERSION, "0.18.0-beta.8").is_some());
        // Once the version is recorded, the next start shows nothing.
        assert!(to_show("0.18.0-beta.8", "0.18.0-beta.8").is_none());
    }

    #[test]
    fn a_version_without_notes_shows_nothing() {
        assert!(to_show("0.18.0-beta.8", "9.9.9").is_none());
        assert!(to_show("0.0.0-dev", "0.0.0-dev").is_none());
    }

    #[test]
    fn about_falls_back_to_the_newest_notes() {
        assert_eq!(for_about("0.0.0-dev").unwrap().version, ENTRIES[0].version);
        assert_eq!(for_about("0.18.0-beta.8").unwrap().version, "0.18.0-beta.8");
    }

    #[test]
    fn every_entry_is_short_and_plain() {
        let mut seen = std::collections::HashSet::new();
        for e in ENTRIES {
            assert!(seen.insert(e.version), "{} is listed twice", e.version);
            assert!(
                (3..=6).contains(&e.bullets.len()),
                "{}: {} bullets",
                e.version,
                e.bullets.len()
            );
            for b in e.bullets {
                assert!(!b.is_empty() && !b.contains('\n'), "{b:?}");
                assert!(b.ends_with('.'), "{b:?}");
            }
        }
    }
}
