//! The link against a fake core on loopback: connect, read, answer pings, reconnect, stop.

mod common;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use common::{
    ADDR, FakeCore, GET_ALL, INIT_OK, config, get_all_frame, info_frame, standard, wait_state,
    wait_until,
};
use fp_earbuds::{EarbudsConfig, EarbudsLink, EarbudsState, Link, NoiseMode, Wear};
use serde_json::{Value, json};
use tungstenite::{Bytes, Message};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn full_reply() -> String {
    get_all_frame("Test Pods", 1, 16, 23)
}

fn has_device(s: &EarbudsState) -> bool {
    s.device.is_some()
}

fn not_worn(s: &EarbudsState) -> bool {
    s.reading().wear == Some(Wear::NotWorn)
}

#[test]
fn connects_reads_info_and_shows_battery() {
    let core = FakeCore::standard(vec![full_reply()]);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    let s = link.state();
    assert_eq!(s.link, Link::Connected);
    assert_eq!(s.core_version.as_deref(), Some("2.0.12"));
    assert!(!s.other_device_active);
    assert!(!s.bluetooth_off);
    let d = s.device.as_ref().unwrap();
    assert_eq!((d.vendor, d.model), (76, 8212));
    assert_eq!(d.wear, Some(Wear::Worn));
    let battery = d.battery.unwrap();
    assert_eq!(battery.left.percent, 80);
    assert_eq!(battery.right.percent, 70);
    assert_eq!(battery.case.shown(), Some(40));
    assert_eq!(battery.buds().unwrap().percent, 70);
    let noise = d.noise.unwrap();
    assert_eq!(noise.selected, Some(NoiseMode::NoiseCancel));
    assert_eq!(noise.options, 23);
    assert!(noise.writable);
    // The only thing the client has said is the state query.
    assert_eq!(core.shared.received(), [GET_ALL]);
    drop(link);
    core.finish();
}

#[test]
fn slow_upgrade_still_connects() {
    let replies = vec![full_reply()];
    let core = FakeCore::start(move |index, stream, shared| {
        std::thread::sleep(ms(1200));
        standard(index, stream, shared, INIT_OK, &replies);
    });
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    assert_eq!(core.shared.accepts(), 1);
    drop(link);
    core.finish();
}

#[test]
fn close_then_backoff_then_success() {
    let replies = vec![full_reply()];
    let core = FakeCore::start(move |index, stream, shared| {
        if index == 0 {
            drop(stream);
        } else {
            standard(index, stream, shared, INIT_OK, &replies);
        }
    });
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    assert_eq!(core.shared.accepts(), 2);
    let times = core.shared.accept_times.lock().unwrap().clone();
    assert!(times[1] - times[0] >= ms(40), "retry came too soon");
    drop(link);
    core.finish();
}

#[test]
fn answers_server_ping() {
    let core = FakeCore::standard(vec![]);
    let ping = Message::Ping(Bytes::from_static(b"hi"));
    core.shared.push(ping);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    let ponged = wait_until(|| {
        let pongs = core.shared.pongs.lock().unwrap();
        pongs.iter().any(|p| p.as_slice() == b"hi")
    });
    assert!(ponged, "no pong came back");
    link.poll();
    assert_eq!(link.state().link, Link::Connected);
    assert_eq!(core.shared.accepts(), 1);
    drop(link);
    core.finish();
}

#[test]
fn ignores_empty_and_unknown_frames() {
    let secrets = r#"{"settings":{"AA_BB_CC_DD_EE_FF":{"irk":"FAKEKEY"}}}"#;
    let replies = vec![
        String::new(),
        r#"{"animation":{}}"#.to_owned(),
        secrets.to_owned(),
        r#"{"x":1}"#.to_owned(),
        info_frame(1, 16),
    ];
    let core = FakeCore::standard(replies);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    assert_eq!(link.state().link, Link::Connected);
    assert_eq!(core.shared.accepts(), 1);
    drop(link);
    core.finish();
}

#[test]
fn empty_info_clears_device_and_bumps_epoch() {
    let core = FakeCore::standard(vec![info_frame(1, 16)]);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    let before = link.state().wear_epoch;
    core.shared.push(Message::text(r#"{"info":{}}"#));
    assert!(wait_state(&mut link, |s| s.device.is_none()));
    assert!(link.state().wear_epoch > before);
    assert_eq!(link.state().link, Link::Connected);
    assert_eq!(core.shared.accepts(), 1);
    drop(link);
    core.finish();
}

#[test]
fn link_loss_bumps_epoch_and_clears_device() {
    let core = FakeCore::standard(vec![info_frame(1, 16)]);
    let slow = EarbudsConfig {
        backoff_base: ms(400),
        backoff_cap: ms(400),
        ..config(core.port)
    };
    let mut link = EarbudsLink::spawn(slow).unwrap();
    assert!(wait_state(&mut link, has_device));
    let before = link.state().wear_epoch;
    // End the first connection only; the next one is served.
    core.shared.kill_below(1);
    assert!(wait_state(&mut link, |s| s.link == Link::Searching));
    assert!(link.state().device.is_none());
    assert!(link.state().wear_epoch > before);
    assert!(wait_state(&mut link, has_device));
    assert_eq!(link.state().link, Link::Connected);
    assert_eq!(core.shared.accepts(), 2);
    drop(link);
    core.finish();
}

#[test]
fn wrong_api_stops_retrying() {
    let core = FakeCore::start(|index, stream, shared| {
        let init = r#"{"init":{"api":2,"version":"9.9.9"}}"#;
        standard(index, stream, shared, init, &[]);
    });
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, |s| s.link == Link::Incompatible));
    assert_eq!(link.state().core_version.as_deref(), Some("9.9.9"));
    // Several retry delays pass without another attempt, and nothing was asked.
    std::thread::sleep(ms(300));
    assert_eq!(core.shared.accepts(), 1);
    assert!(core.shared.received().is_empty());
    link.refresh();
    assert!(wait_until(|| core.shared.accepts() == 2));
    drop(link);
    core.finish();
}

/// Connects to a core that offers `options` with `selected` active, and waits for the device.
fn noise_link(core: &FakeCore, selected: u64, options: u64) -> EarbudsLink {
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    let noise = link.state().device.as_ref().unwrap().noise.unwrap();
    assert_eq!(noise.options, u8::try_from(options).unwrap());
    assert_eq!(noise.selected, NoiseMode::from_bit(selected));
    link
}

#[test]
fn set_noise_sends_exact_json_and_waits_for_info() {
    let core = FakeCore::standard(vec![get_all_frame("Test Pods", 1, 2, 23)]);
    let slow = EarbudsConfig {
        noise_confirm: Duration::from_secs(10),
        ..config(core.port)
    };
    let mut link = EarbudsLink::spawn(slow).unwrap();
    assert!(wait_state(&mut link, has_device));
    link.set_noise(NoiseMode::NoiseCancel);
    assert!(wait_state(&mut link, |s| s.noise_pending.is_some()));
    assert_eq!(link.state().noise_pending, Some(NoiseMode::NoiseCancel));
    assert!(wait_until(|| core.shared.received().len() == 2));
    let sent: Value = serde_json::from_str(&core.shared.received()[1]).unwrap();
    let expected = json!({
        "method": "SetCapabilities",
        "arguments": {"address": ADDR, "capabilities": {"anc": {"selected": 16}}}
    });
    assert_eq!(sent, expected);
    // The core confirms with a new info.
    core.shared.push(Message::text(info_frame(1, 16)));
    assert!(wait_state(&mut link, |s| s.noise_pending.is_none()));
    let noise = link.state().device.as_ref().unwrap().noise.unwrap();
    assert_eq!(noise.selected, Some(NoiseMode::NoiseCancel));
    drop(link);
    core.finish();
}

#[test]
fn an_unconfirmed_noise_change_is_given_up() {
    let core = FakeCore::standard(vec![get_all_frame("Test Pods", 1, 2, 23)]);
    let slow = EarbudsConfig {
        noise_confirm: ms(300),
        ..config(core.port)
    };
    let mut link = EarbudsLink::spawn(slow).unwrap();
    assert!(wait_state(&mut link, has_device));
    link.set_noise(NoiseMode::Adaptive);
    assert!(wait_state(&mut link, |s| s.noise_pending.is_some()));
    assert!(wait_state(&mut link, |s| s.noise_pending.is_none()));
    let noise = link.state().device.as_ref().unwrap().noise.unwrap();
    assert_eq!(noise.selected, Some(NoiseMode::Transparency));
    drop(link);
    core.finish();
}

#[test]
fn a_mode_the_device_does_not_offer_is_never_sent() {
    let core = FakeCore::standard(vec![get_all_frame("Test Pods", 1, 2, 19)]);
    let link = noise_link(&core, 2, 19);
    link.set_noise(NoiseMode::Adaptive);
    link.set_noise(NoiseMode::Wind);
    std::thread::sleep(ms(200));
    assert_eq!(core.shared.received(), [GET_ALL]);
    assert_eq!(link.state().noise_pending, None);
    drop(link);
    core.finish();
}

#[test]
fn liveness_reconnects_a_silent_core() {
    let core = FakeCore::standard(vec![]);
    let quick = EarbudsConfig {
        liveness: ms(300),
        ..config(core.port)
    };
    let link = EarbudsLink::spawn(quick).unwrap();
    assert!(wait_until(|| core.shared.accepts() >= 2));
    drop(link);
    core.finish();
}

#[test]
fn oversized_frame_drops_the_connection() {
    let big = vec!["x".repeat(2 << 20)];
    let normal = vec![full_reply()];
    let core = FakeCore::start(move |index, stream, shared| {
        let out = if index == 0 { &big } else { &normal };
        standard(index, stream, shared, INIT_OK, out);
    });
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    assert_eq!(core.shared.accepts(), 2);
    drop(link);
    core.finish();
}

#[test]
fn closed_port_retries_slowly() {
    let core = FakeCore::start(|_, stream, _| drop(stream));
    let slow = EarbudsConfig {
        backoff_base: ms(100),
        backoff_cap: ms(400),
        ..config(core.port)
    };
    let link = EarbudsLink::spawn(slow).unwrap();
    std::thread::sleep(ms(1500));
    let times = core.shared.accept_times.lock().unwrap().clone();
    let n = times.len();
    assert!((2..=8).contains(&n), "{n} attempts");
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps[0] >= ms(80), "first gap {:?}", gaps[0]);
    for pair in gaps.windows(2) {
        assert!(pair[1] + ms(200) >= pair[0], "{gaps:?}");
    }
    drop(link);
    core.finish();
}

#[test]
fn a_port_nobody_listens_on_stays_searching() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut link = EarbudsLink::spawn(config(port)).unwrap();
    std::thread::sleep(ms(300));
    link.poll();
    assert_eq!(link.state().link, Link::Searching);
    assert!(link.state().device.is_none());
    assert!(link.state().core_version.is_none());
}

#[test]
fn drop_returns_at_once() {
    let core = FakeCore::standard(vec![full_reply()]);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    let start = Instant::now();
    drop(link);
    let took = start.elapsed();
    assert!(took < ms(500), "drop took {took:?}");
    core.finish();
}

#[test]
fn stop_and_join_is_bounded() {
    let core = FakeCore::standard(vec![full_reply()]);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    let start = Instant::now();
    assert!(link.stop_and_join(Duration::from_secs(2)));
    let took = start.elapsed();
    assert!(took < ms(500), "stop took {took:?}");

    // A core that takes the connection and never answers the upgrade.
    let mute = FakeCore::start(|_, stream, shared| {
        while !shared.stopped() {
            std::thread::sleep(ms(10));
        }
        drop(stream);
    });
    let link = EarbudsLink::spawn(config(mute.port)).unwrap();
    assert!(wait_until(|| mute.shared.accepts() == 1));
    let start = Instant::now();
    assert!(!link.stop_and_join(ms(200)));
    assert!(start.elapsed() < Duration::from_secs(1));
    core.finish();
}

#[test]
fn a_burst_of_identical_infos_changes_nothing() {
    let core = FakeCore::standard(vec![info_frame(1, 16)]);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    let settled = link.state().clone();
    for _ in 0..3 {
        core.shared.push(Message::text(info_frame(1, 16)));
    }
    std::thread::sleep(ms(300));
    assert!(!link.poll());
    assert_eq!(link.state(), &settled);
    drop(link);
    core.finish();
}

#[test]
fn a_wear_change_arrives_without_a_new_epoch() {
    let core = FakeCore::standard(vec![info_frame(1, 16)]);
    let mut link = EarbudsLink::spawn(config(core.port)).unwrap();
    assert!(wait_state(&mut link, has_device));
    let epoch = link.state().wear_epoch;
    for wear in [2, 2, 2, 3] {
        core.shared.push(Message::text(info_frame(wear, 16)));
    }
    assert!(wait_state(&mut link, not_worn));
    assert_eq!(link.state().wear_epoch, epoch);
    drop(link);
    core.finish();
}

#[test]
fn fixed_link_never_connects() {
    let state = EarbudsState {
        link: Link::Connected,
        ..EarbudsState::default()
    };
    let mut link = EarbudsLink::fixed(state.clone());
    assert_eq!(link.state(), &state);
    assert!(!link.poll());
    assert_eq!(link.state(), &state);
}
