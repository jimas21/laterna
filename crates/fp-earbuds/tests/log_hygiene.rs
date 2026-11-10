//! No log record from this crate holds a settings frame, key material, a device address or
//! a device name, whatever the core sends and however the connection ends.
//!
//! This file installs the process-wide logger, so it is a test binary of its own.

mod common;

use std::sync::Mutex;
use std::time::Duration;

use common::{ADDR, FakeCore, config, get_all_frame, wait_state, wait_until};
use fp_earbuds::{EarbudsLink, Link, NoiseMode};
use log::{LevelFilter, Log, Metadata, Record};
use serde_json::json;
use tungstenite::Message;

static RECORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl Log for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        // Only this crate's own records; other crates log by their own rules.
        if record.target().starts_with("fp_earbuds") {
            let line = format!("{} {}", record.level(), record.args());
            RECORDS.lock().unwrap().push(line);
        }
    }

    fn flush(&self) {}
}

static LOGGER: Capture = Capture;

#[test]
fn nothing_secret_reaches_the_log() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(LevelFilter::Trace);

    let secret_settings = json!({"settings": {"AA_BB_CC_DD_EE_FF": {
        "irk": "FAKEKEY-IRK", "enc": "FAKEKEY-ENC"
    }}});
    let replies = vec![
        secret_settings.to_string(),
        r#""FAKEKEY""#.to_owned(),
        r#"[1,"FAKEKEY"]"#.to_owned(),
        r#"{"init":"FAKEKEY"}"#.to_owned(),
        r#"{"info":"FAKEKEY"}"#.to_owned(),
        r#"{"headphones":"FAKEKEY","FAKEKEY":1}"#.to_owned(),
        "FAKEKEY not json".to_owned(),
        get_all_frame("FAKEKEY-NAME", 1, 2, 23),
    ];
    let core = FakeCore::standard(replies);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, |s| s.device.is_some()));

    // A noise change nobody confirms, and a lost connection.
    link.set_noise(NoiseMode::NoiseCancel);
    assert!(wait_until(|| core.shared.received().len() == 2));
    assert!(wait_state(&mut link, |s| s.noise_pending.is_some()));
    core.shared.push(Message::text(secret_settings.to_string()));
    assert!(wait_state(&mut link, |s| s.noise_pending.is_none()));
    core.shared.kill_below(1);
    assert!(wait_state(&mut link, |s| s.link == Link::Searching));
    assert!(wait_state(&mut link, |s| s.device.is_some()));
    std::thread::sleep(Duration::from_millis(400));
    drop(link);
    core.finish();

    let records = RECORDS.lock().unwrap().clone();
    assert!(!records.is_empty(), "no log records were captured");
    for line in &records {
        assert!(!line.contains("FAKEKEY"), "{line}");
        assert!(!line.contains("AA_BB"), "{line}");
        assert!(!line.contains(ADDR), "{line}");
    }
}
