//! The JSON the core speaks, for the small part of its API this crate uses.
//!
//! No I/O and no logging here. Reading is forgiving: unknown keys are skipped without being
//! built into values (a frame of key material never takes shape in memory), and a capability
//! with a missing, mistyped or out-of-range field is treated as absent instead of failing.
//! Error values carry the kind and position of a problem, never text from the frame.

use std::fmt;

use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::{Value, json};

use crate::state::{Battery, Cell, CellStatus, Earbuds, Noise, NoiseMode, Wear};

/// The only protocol version this crate speaks.
pub const API: u64 = 1;

const MAX_ADDRESS: usize = 40;
const MAX_NAME: usize = 64;
const MAX_VERSION: usize = 32;
const MAX_LIST: usize = 64;

/// What this crate may send. Nothing else can be built, by construction.
#[derive(Clone, PartialEq, Eq)]
pub enum Request {
    /// Ask for the device list, Bluetooth state and active device in one reply.
    GetAll,
    /// Change the noise control mode of the device with this address.
    SetNoise {
        /// The address the core reported for the device.
        address: String,
        /// The mode to switch to; the caller has checked the device offers it.
        mode: NoiseMode,
    },
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Request::GetAll => f.write_str("GetAll"),
            Request::SetNoise { mode, .. } => write!(f, "SetNoise(<redacted>, {mode:?})"),
        }
    }
}

impl Request {
    /// The frame to send, as JSON text.
    pub fn to_json(&self) -> String {
        match self {
            Request::GetAll => json!({"method": "GetAll"}).to_string(),
            Request::SetNoise { address, mode } => {
                let anc = json!({"selected": mode.bit()});
                let caps = json!({"address": address.as_str(), "capabilities": {"anc": anc}});
                json!({"method": "SetCapabilities", "arguments": caps}).to_string()
            }
        }
    }
}

/// One frame from the core, reduced to the parts this crate reads.
#[derive(Debug, Default, PartialEq)]
pub struct Frame {
    /// The greeting, sent once per connection.
    pub init: Option<Init>,
    /// Every paired device.
    pub headphones: Option<Vec<Headphone>>,
    /// Whether the Bluetooth adapter is on.
    pub bluetooth_enabled: Option<bool>,
    /// The active device: `Some(None)` when the core says there is none.
    pub info: Option<Option<Earbuds>>,
    /// The frame's top-level key names, mapped to fixed labels, for debug logs.
    pub keys: Vec<&'static str>,
}

/// The greeting.
#[derive(Debug, PartialEq, Eq)]
pub struct Init {
    /// The protocol version.
    pub api: Option<u64>,
    /// The core's version string.
    pub version: Option<String>,
}

/// A paired device, as far as this crate looks at it.
#[derive(PartialEq, Eq)]
pub struct Headphone {
    /// Bluetooth address; never logged.
    pub address: String,
    /// Bluetooth vendor id.
    pub vendor: u16,
    /// Whether it is connected right now.
    pub connected: bool,
}

impl fmt::Debug for Headphone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Headphone")
            .field("address", &"<redacted>")
            .field("vendor", &self.vendor)
            .field("connected", &self.connected)
            .finish()
    }
}

/// A frame that is not JSON, or not a JSON object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// Kind, line and column of the problem; nothing from the frame itself.
    #[error("unreadable frame ({0:?} error at line {1}, column {2})")]
    Json(serde_json::error::Category, usize, usize),
}

fn wire_error(e: serde_json::Error) -> WireError {
    WireError::Json(e.classify(), e.line(), e.column())
}

/// Reads one text frame. Empty text is an empty frame: the core answers an invalid request
/// with one.
pub fn parse(text: &str) -> Result<Frame, WireError> {
    if text.trim().is_empty() {
        return Ok(Frame::default());
    }
    let raw: RawFrame = serde_json::from_str(text).map_err(wire_error)?;
    Ok(raw.into_frame())
}

#[derive(Default)]
struct RawFrame {
    init: Option<Value>,
    headphones: Option<Value>,
    defaultbluetooth: Option<Value>,
    info: Option<Value>,
    keys: Vec<&'static str>,
}

impl<'de> Deserialize<'de> for RawFrame {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RawFrameVisitor)
    }
}

struct RawFrameVisitor;

impl<'de> Visitor<'de> for RawFrameVisitor {
    type Value = RawFrame;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawFrame, A::Error> {
        let mut raw = RawFrame::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "init" => raw.init = map.next_value()?,
                "headphones" => raw.headphones = map.next_value()?,
                "defaultbluetooth" => raw.defaultbluetooth = map.next_value()?,
                "info" => raw.info = map.next_value()?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            raw.keys.push(key_label(&key));
        }
        Ok(raw)
    }
}

fn key_label(key: &str) -> &'static str {
    match key {
        "init" => "init",
        "headphones" => "headphones",
        "defaultbluetooth" => "defaultbluetooth",
        "info" => "info",
        "settings" => "settings",
        "animation" => "animation",
        _ => "other",
    }
}

impl RawFrame {
    fn into_frame(self) -> Frame {
        Frame {
            init: self.init.as_ref().and_then(read_init),
            headphones: self.headphones.as_ref().map(read_headphones),
            bluetooth_enabled: self.defaultbluetooth.as_ref().and_then(read_enabled),
            info: self.info.as_ref().and_then(read_info),
            keys: self.keys,
        }
    }
}

fn get_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key)?.as_u64()
}

fn get_bool(v: &Value, key: &str) -> Option<bool> {
    v.get(key)?.as_bool()
}

fn get_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

fn u16_of(v: &Value, key: &str) -> u16 {
    let n = get_u64(v, key).unwrap_or(0);
    u16::try_from(n).unwrap_or(0)
}

/// Drops control characters and cuts the text short.
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn read_init(v: &Value) -> Option<Init> {
    if !v.is_object() {
        return None;
    }
    Some(Init {
        api: get_u64(v, "api"),
        version: get_str(v, "version").map(|s| clean(s, MAX_VERSION)),
    })
}

fn read_enabled(v: &Value) -> Option<bool> {
    get_bool(v, "enabled")
}

fn read_headphones(v: &Value) -> Vec<Headphone> {
    let items = v.as_array().into_iter().flatten().take(MAX_LIST);
    items.filter_map(read_headphone).collect()
}

fn read_headphone(v: &Value) -> Option<Headphone> {
    let address = clean(get_str(v, "address")?, MAX_ADDRESS);
    Some(Headphone {
        address,
        vendor: u16_of(v, "vendor"),
        connected: get_bool(v, "connected").unwrap_or(false),
    })
}

/// `Some(None)` is "no active device"; `None` is "cannot read this, ignore it".
fn read_info(v: &Value) -> Option<Option<Earbuds>> {
    let fields = v.as_object()?;
    if fields.is_empty() || get_bool(v, "connected") == Some(false) {
        return Some(None);
    }
    let address = clean(get_str(v, "address")?, MAX_ADDRESS);
    let name = clean(get_str(v, "name").unwrap_or(""), MAX_NAME);
    let caps = v.get("capabilities");
    let cap = |key: &str| caps.and_then(|c| c.get(key));
    Some(Some(Earbuds {
        name,
        address,
        vendor: u16_of(v, "vendor"),
        model: u16_of(v, "model"),
        battery: cap("battery").and_then(read_battery),
        wear: cap("earDetectionState").and_then(read_wear),
        noise: cap("anc").and_then(read_noise),
    }))
}

fn read_battery(v: &Value) -> Option<Battery> {
    let single = v.get("single").and_then(read_cell);
    Some(Battery {
        left: read_cell(v.get("left")?)?,
        right: read_cell(v.get("right")?)?,
        case: read_cell(v.get("case")?)?,
        single: single.unwrap_or(Cell {
            percent: 0,
            charging: false,
            status: CellStatus::NotAvailable,
        }),
    })
}

fn read_cell(v: &Value) -> Option<Cell> {
    let percent = get_u64(v, "battery")?;
    if percent > 100 {
        return None;
    }
    Some(Cell {
        percent: u8::try_from(percent).ok()?,
        charging: get_bool(v, "charging").unwrap_or(false),
        status: CellStatus::from_wire(get_u64(v, "status").unwrap_or(0)),
    })
}

fn read_noise(v: &Value) -> Option<Noise> {
    let options = get_u64(v, "options")?;
    if options > 31 {
        return None;
    }
    let selected = NoiseMode::from_bit(get_u64(v, "selected")?)?;
    Some(Noise {
        selected: Some(selected),
        options: u8::try_from(options).ok()?,
        writable: !get_bool(v, "readonly").unwrap_or(true),
    })
}

fn read_wear(v: &Value) -> Option<Wear> {
    get_u64(v, "selected").map(Wear::from_wire)
}
