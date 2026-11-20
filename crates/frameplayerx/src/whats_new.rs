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
pub const ENTRIES: &[Entry] = &[
    Entry {
        version: "0.18.0-beta.19",
        bullets: &[
            "The cinema's room murmur is gone: it sounded harsh. Room sound comes back once it is a real recording.",
            "Shine on a place now needs the place to ask for it, so older places look as before.",
            "Plain is now called Just the screen.",
            "Bigger place packs with a view from several seats can be downloaded.",
        ],
    },
    Entry {
        version: "0.18.0-beta.18",
        bullets: &[
            "Glossy surfaces in a place now catch the film's light and the shine moves as you move your head.",
            "The cinema has a soft room murmur that fades as the lights go down, with its own slider; Minimal lighting turns exit and step lights off.",
            "Drop a 360 photo or a place pack into the environments folder and it shows under Yours; you can add your own place sources too.",
            "A sleep timer in the player's More list, and a short opening you can turn off.",
            "Smoother 4K and 8K: the decoder gets the buffers it needs, and Plex sends HEVC for files the headset can't decode itself.",
        ],
    },
    Entry {
        version: "0.18.0-beta.17",
        bullets: &[
            "Sound now comes from the screen: each speaker stays put as you turn your head, and every place has its own room echo.",
            "Places light up from the film with less work for the headset.",
            "Move the screen anywhere and push it near or far by reaching with your arm.",
            "Point at the screen to bring up just the move bar and resize corner; the player's extra controls live in a More list.",
            "Plex: choose who is watching, and jump to any library from Home.",
        ],
    },
    Entry {
        version: "0.18.0-beta.8",
        bullets: &[
            "Smoother edges everywhere (4x edge smoothing is now on).",
            "The cinema is rebuilt like a real theater: the front row is no longer at the screen, and nothing covers the picture.",
            "Screen size has simple choices: Home theater, Cinema, Giant. Change it while watching with the Size button.",
            "The room takes the film's colour as it plays, with a Screen light slider in Settings > Places.",
            "Recenter while lying back and the whole room tilts to face you.",
            "Plex: switch between people in your Plex Home, playlists and a richer Home page.",
        ],
    },
];

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

/// Whether the one-time hint card is due: after an update from a version
/// that ran before (`last` is empty on a first run, which has nothing to
/// point out yet). The caller has already checked it was not shown.
pub fn tips_due(last: &str) -> bool {
    !last.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_card_follows_an_update_not_a_first_run() {
        assert!(!tips_due(""));
        assert!(tips_due("0.18.0-beta.17"));
        assert!(tips_due(crate::settings::EARLIER_VERSION));
    }

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
