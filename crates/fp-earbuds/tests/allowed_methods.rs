//! The crate may ask the core for its state and change the noise mode, and nothing else. This
//! guard fails if any source file spells out a method that could read the core's settings (which
//! hold key material) or act on devices and adapters.
//!
//! Doc comments may name these in backticks; only the quoted form, as it would appear in JSON
//! or a Rust string, fails.

const SOURCES: [(&str, &str); 6] = [
    ("lib.rs", include_str!("../src/lib.rs")),
    ("policy.rs", include_str!("../src/policy.rs")),
    ("settings.rs", include_str!("../src/settings.rs")),
    ("state.rs", include_str!("../src/state.rs")),
    ("wire.rs", include_str!("../src/wire.rs")),
    ("worker.rs", include_str!("../src/worker.rs")),
];

const FORBIDDEN: [&str; 10] = [
    "\"GetSettingsAll\"",
    "\"GetSettings\"",
    "\"SetSetting\"",
    "\"GetSetting\"",
    "\"sendHex\"",
    "\"ConnectDevice\"",
    "\"DisconnectDevice\"",
    "\"EnableDefaultBluetoothAdapter\"",
    "\"DisableDefaultBluetoothAdapter\"",
    "\"earDetection\"",
];

#[test]
fn no_source_names_a_forbidden_method() {
    for (file, text) in SOURCES {
        for word in FORBIDDEN {
            assert!(!text.contains(word), "{file} contains {word}");
        }
    }
}

#[test]
fn every_source_file_is_checked() {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("src").unwrap() {
        let path = entry.unwrap().path();
        assert!(path.is_file(), "{path:?} is not a plain source file");
        let name = path.file_name().unwrap().to_string_lossy();
        found.push(name.into_owned());
    }
    found.sort();
    let mut listed: Vec<&str> = SOURCES.iter().map(|(file, _)| *file).collect();
    listed.sort_unstable();
    assert_eq!(found, listed, "add the new file to SOURCES");
}

#[test]
fn the_guard_would_notice() {
    let sample = "send(\"GetSettingsAll\")";
    assert!(FORBIDDEN.iter().any(|w| sample.contains(w)));
}
