//! Reading and writing the core's JSON, through the public API only. Test data uses a made-up
//! address, a made-up name and fake key strings.

use fp_earbuds::wire::{Frame, Request, WireError, parse};
use fp_earbuds::{CellStatus, Earbuds, NoiseMode, Wear};
use serde_json::{Value, json};

const ADDR: &str = "AA:BB:CC:DD:EE:FF";

/// The `info` example of the protocol notes, with a made-up address.
const SPEC_INFO: &str = r#"{"info":{"name":"Test Pods","address":"AA:BB:CC:DD:EE:FF",
  "vendor":76,"model":8212,"color":0,"connected":true,
  "capabilities":{
    "battery":{"single":{"battery":0,"charging":false,"status":0},
               "left":{"battery":80,"charging":false,"status":2},
               "right":{"battery":70,"charging":false,"status":2},
               "case":{"battery":40,"charging":false,"status":3},"readonly":true},
    "anc":{"selected":16,"options":23,"readonly":false},
    "earDetection":{"readonly":false,
                    "selected":{"pauseMedia":true,"switchToSpeakers":false,"lowerVolume":0}},
    "earDetectionState":{"readonly":true,"selected":1}}}}"#;

fn info_with(caps: Value) -> String {
    let frame = json!({"info": {
        "name": "Test Pods", "address": ADDR, "vendor": 76, "model": 8212,
        "connected": true, "capabilities": caps
    }});
    frame.to_string()
}

fn device_of(text: &str) -> Earbuds {
    parse(text).unwrap().info.unwrap().unwrap()
}

fn cell(battery: u64, charging: bool, status: u64) -> Value {
    json!({"battery": battery, "charging": charging, "status": status})
}

fn battery_caps(left: Value, right: Value, case: Value) -> Value {
    json!({"battery": {
        "single": cell(0, false, 0), "left": left, "right": right, "case": case,
        "readonly": true
    }})
}

fn plain_battery(left: Value, right: Value) -> Value {
    battery_caps(left, right, cell(0, false, 0))
}

fn anc_caps(selected: u64, options: u64) -> Value {
    json!({"anc": {"selected": selected, "options": options, "readonly": false}})
}

fn wear_caps(selected: u64) -> Value {
    json!({"earDetectionState": {"readonly": true, "selected": selected}})
}

#[test]
fn the_example_info_reads_as_documented() {
    let d = device_of(SPEC_INFO);
    assert_eq!((d.vendor, d.model), (76, 8212));
    assert!(d.is_apple());
    assert_eq!(d.name, "Test Pods");
    assert_eq!(d.address, ADDR);
    let b = d.battery.unwrap();
    assert_eq!(b.left.percent, 80);
    assert_eq!(b.right.percent, 70);
    assert_eq!(b.left.status, CellStatus::Connected);
    assert_eq!(b.case.percent, 40);
    assert_eq!(b.case.status, CellStatus::Cached);
    assert_eq!(b.single.status, CellStatus::NotAvailable);
    let level = b.buds().unwrap();
    assert_eq!(level.percent, 70);
    assert!(!level.last_known);
    let n = d.noise.unwrap();
    assert_eq!(n.selected, Some(NoiseMode::NoiseCancel));
    assert_eq!(n.options, 23);
    assert!(n.writable);
    assert_eq!(d.wear, Some(Wear::Worn));
}

#[test]
fn battery_status_decides_what_is_shown() {
    for (status, shown) in [(0, false), (1, false), (2, true), (3, true), (9, false)] {
        let live = cell(55, false, 2);
        let left = cell(55, false, status);
        let d = device_of(&info_with(plain_battery(left, live)));
        let b = d.battery.unwrap();
        assert_eq!(b.left.shown().is_some(), shown, "status {status}");
    }
}

#[test]
fn an_empty_cell_is_hidden_and_a_cached_one_is_last_known() {
    let empty = cell(0, false, 0);
    let cached = cell(35, false, 3);
    let d = device_of(&info_with(plain_battery(empty, cached)));
    let b = d.battery.unwrap();
    assert_eq!(b.left.shown(), None);
    let level = b.buds().unwrap();
    assert_eq!(level.percent, 35);
    assert!(level.last_known);
}

#[test]
fn charging_is_carried() {
    let charging = cell(20, true, 2);
    let caps = plain_battery(charging, cell(30, false, 2));
    let d = device_of(&info_with(caps));
    let level = d.battery.unwrap().buds().unwrap();
    assert!(level.charging);
    assert_eq!(level.percent, 20);
}

#[test]
fn a_percent_over_100_drops_the_battery() {
    let caps = plain_battery(cell(101, false, 2), cell(70, false, 2));
    let d = device_of(&info_with(caps));
    assert!(d.battery.is_none());
    // The rest of the device is still read.
    assert!(d.is_apple());
}

#[test]
fn a_cell_with_a_wrong_type_drops_the_battery() {
    let bad = json!({"battery": "70", "charging": false, "status": 2});
    let caps = plain_battery(bad, cell(70, false, 2));
    assert!(device_of(&info_with(caps)).battery.is_none());
}

#[test]
fn noise_options_are_offered_as_a_menu_without_wind() {
    let offered = |options: u64| {
        let d = device_of(&info_with(anc_caps(2, options)));
        let n = d.noise.unwrap();
        NoiseMode::offered(n.options).collect::<Vec<_>>()
    };
    let three = [
        NoiseMode::Off,
        NoiseMode::Transparency,
        NoiseMode::NoiseCancel,
    ];
    assert_eq!(offered(19), three);
    let four = offered(23);
    assert_eq!(four.len(), 4);
    assert!(four.contains(&NoiseMode::Adaptive));
    assert!(!offered(31).contains(&NoiseMode::Wind));
}

#[test]
fn an_unreadable_noise_report_is_absent() {
    // Not a single known bit.
    assert!(device_of(&info_with(anc_caps(3, 23))).noise.is_none());
    assert!(device_of(&info_with(anc_caps(0, 23))).noise.is_none());
    // Options beyond the known bits.
    assert!(device_of(&info_with(anc_caps(16, 32))).noise.is_none());
    // A read-only control is read but not writable.
    let locked = json!({"anc": {"selected": 16, "options": 23, "readonly": true}});
    let n = device_of(&info_with(locked)).noise.unwrap();
    assert!(!n.writable);
}

#[test]
fn wear_values_map_and_absence_is_none() {
    let wear = |v: u64| device_of(&info_with(wear_caps(v))).wear;
    assert_eq!(wear(0), Some(Wear::NotAvailable));
    assert_eq!(wear(1), Some(Wear::Worn));
    assert_eq!(wear(2), Some(Wear::Partial));
    assert_eq!(wear(3), Some(Wear::NotWorn));
    assert_eq!(wear(7), Some(Wear::Unknown));
    assert_eq!(device_of(&info_with(json!({}))).wear, None);
    let wrong = json!({"earDetectionState": {"selected": "1"}});
    assert_eq!(device_of(&info_with(wrong)).wear, None);
}

#[test]
fn no_active_device_is_some_none() {
    assert_eq!(parse(r#"{"info":{}}"#).unwrap().info, Some(None));
    let gone = json!({"info": {"address": ADDR, "vendor": 76, "connected": false}});
    assert_eq!(parse(&gone.to_string()).unwrap().info, Some(None));
    // An info that is not an object, or has no address, is ignored rather than trusted.
    assert_eq!(parse(r#"{"info":"x"}"#).unwrap().info, None);
    assert_eq!(parse(r#"{"info":{"vendor":76}}"#).unwrap().info, None);
}

#[test]
fn a_get_all_reply_holds_three_keys() {
    let value = json!({
        "headphones": [
            {"name": "Test Pods", "address": ADDR, "vendor": 76, "model": 8212,
             "color": 0, "connected": true},
            {"name": "Speaker", "address": "11:22:33:44:55:66", "vendor": 117,
             "model": 9, "color": 0, "connected": false},
            {"name": "No address", "vendor": 76, "connected": true}
        ],
        "defaultbluetooth": {"enabled": false},
        "info": {}
    });
    let frame = parse(&value.to_string()).unwrap();
    let list = frame.headphones.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!((list[0].vendor, list[0].connected), (76, true));
    assert_eq!((list[1].vendor, list[1].connected), (117, false));
    assert_eq!(frame.bluetooth_enabled, Some(false));
    assert_eq!(frame.info, Some(None));
    assert!(frame.init.is_none());
}

#[test]
fn init_alone_parses() {
    let frame = parse(r#"{"init":{"api":1,"version":"2.0.12"}}"#).unwrap();
    let init = frame.init.unwrap();
    assert_eq!(init.api, Some(1));
    assert_eq!(init.version.as_deref(), Some("2.0.12"));
    assert!(frame.info.is_none() && frame.headphones.is_none());
    let odd = parse(r#"{"init":{"api":"1"}}"#).unwrap().init.unwrap();
    assert_eq!((odd.api, odd.version), (None, None));
    assert!(parse(r#"{"init":"x"}"#).unwrap().init.is_none());
}

#[test]
fn unknown_keys_and_empty_frames_are_ignored() {
    assert_eq!(parse("").unwrap(), Frame::default());
    assert_eq!(parse("  \n").unwrap(), Frame::default());
    let secret = r#"{"settings":{"AA_BB_CC_DD_EE_FF":{"irk":"FAKEKEY-IRK"}}}"#;
    let frame = parse(secret).unwrap();
    assert!(frame.init.is_none() && frame.info.is_none() && frame.headphones.is_none());
    assert_eq!(frame.keys, ["settings"]);
    let frame = parse(r#"{"animation":{"a":[1,2,3]},"x":1,"info":{}}"#).unwrap();
    assert_eq!(frame.keys, ["animation", "other", "info"]);
    assert_eq!(frame.info, Some(None));
}

#[test]
fn unreadable_frames_never_echo_their_text() {
    let samples = [
        r#""FAKEKEY""#,
        r#"[1,"FAKEKEY"]"#,
        r#"{"init":"FAKEKEY"}"#,
        "FAKEKEY",
    ];
    for text in samples {
        match parse(text) {
            Ok(frame) => assert!(!format!("{frame:?}").contains("FAKEKEY")),
            Err(e) => {
                assert!(!format!("{e}").contains("FAKEKEY"), "{e}");
                assert!(!format!("{e:?}").contains("FAKEKEY"), "{e:?}");
            }
        }
    }
    let err: WireError = parse("{").unwrap_err();
    assert!(format!("{err}").contains("line 1"));
    assert!(parse("[1,2]").is_err());
    assert!(parse("null").is_err());
}

#[test]
fn debug_output_hides_names_and_addresses() {
    let frame = parse(SPEC_INFO).unwrap();
    let text = format!("{frame:?}");
    assert!(!text.contains("Test Pods"), "{text}");
    assert!(!text.contains("AA:BB:CC"), "{text}");
    let list = json!({"headphones": [{"address": ADDR, "vendor": 76, "connected": true}]});
    let text = format!("{:?}", parse(&list.to_string()).unwrap());
    assert!(!text.contains("AA:BB:CC"), "{text}");
    let request = Request::SetNoise {
        address: ADDR.to_owned(),
        mode: NoiseMode::Off,
    };
    assert!(!format!("{request:?}").contains("AA:BB:CC"));
}

#[test]
fn text_from_the_core_is_cleaned_and_cut() {
    let long = "N".repeat(500);
    let frame = json!({"info": {
        "name": format!("Pods\u{7}{long}"), "address": ADDR, "vendor": 76, "connected": true
    }});
    let d = device_of(&frame.to_string());
    assert!(!d.name.contains('\u{7}'));
    assert!(d.name.chars().count() <= 64);
    let init = json!({"init": {"api": 1, "version": "1.0\n".repeat(50)}});
    let frame = parse(&init.to_string()).unwrap();
    let version = frame.init.unwrap().version.unwrap();
    assert!(version.chars().count() <= 32);
    assert!(!version.contains('\n'));
}

#[test]
fn the_list_of_devices_is_capped() {
    let one = json!({"address": ADDR, "vendor": 76, "connected": true});
    let many = json!({"headphones": vec![one; 500]});
    let frame = parse(&many.to_string()).unwrap();
    assert_eq!(frame.headphones.unwrap().len(), 64);
}

#[test]
fn requests_are_exactly_the_documented_json() {
    assert_eq!(Request::GetAll.to_json(), r#"{"method":"GetAll"}"#);
    let request = Request::SetNoise {
        address: ADDR.to_owned(),
        mode: NoiseMode::NoiseCancel,
    };
    let sent: Value = serde_json::from_str(&request.to_json()).unwrap();
    let expected = json!({
        "method": "SetCapabilities",
        "arguments": {"address": ADDR, "capabilities": {"anc": {"selected": 16}}}
    });
    assert_eq!(sent, expected);
    for (mode, bit) in [
        (NoiseMode::Off, 1),
        (NoiseMode::Transparency, 2),
        (NoiseMode::Adaptive, 4),
        (NoiseMode::NoiseCancel, 16),
    ] {
        let request = Request::SetNoise {
            address: ADDR.to_owned(),
            mode,
        };
        let v: Value = serde_json::from_str(&request.to_json()).unwrap();
        assert_eq!(v["arguments"]["capabilities"]["anc"]["selected"], bit);
    }
}
