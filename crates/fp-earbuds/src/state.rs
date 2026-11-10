//! What is known about the earbuds: plain data the worker fills and the app reads. No I/O.
//!
//! Nothing here is ever stored or logged with its identifiers: [`Earbuds`] prints its name and
//! address as `<redacted>`.

use std::fmt;

/// The Bluetooth vendor id Apple devices (AirPods, Beats) report.
pub const APPLE_VENDOR: u16 = 76;

/// How far the link to the local core has got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Link {
    /// Turned off: no thread, no socket.
    #[default]
    Idle,
    /// No core answering yet; retrying with a growing delay.
    Searching,
    /// The core answered and speaks the protocol this build knows.
    Connected,
    /// The core speaks a different protocol version; not retrying until asked.
    Incompatible,
}

/// Whether the earbuds are in the ears, as one aggregate over both buds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wear {
    /// Both in.
    Worn,
    /// Exactly one in.
    Partial,
    /// Neither in.
    NotWorn,
    /// The core reports "not available": both buds claim to be in a closed case, or it is
    /// not known. Only the pause policy gives this a meaning, and only straight after a
    /// reading of `Worn` or `Partial`.
    NotAvailable,
    /// Any other value; ignored.
    Unknown,
}

impl Wear {
    /// Maps the core's number: 0 not available, 1 worn, 2 partial, 3 not worn.
    pub fn from_wire(v: u64) -> Wear {
        match v {
            0 => Wear::NotAvailable,
            1 => Wear::Worn,
            2 => Wear::Partial,
            3 => Wear::NotWorn,
            _ => Wear::Unknown,
        }
    }
}

/// A noise control mode. The discriminant is the core's bit for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NoiseMode {
    /// No processing.
    Off = 1,
    /// Let outside sound through.
    Transparency = 2,
    /// Adapts to the surroundings (newer models only).
    Adaptive = 4,
    /// Wind reduction; never offered.
    Wind = 8,
    /// Noise cancellation.
    NoiseCancel = 16,
}

impl NoiseMode {
    /// The modes the app offers, in display order.
    pub const MENU: [NoiseMode; 4] = [
        NoiseMode::Off,
        NoiseMode::Transparency,
        NoiseMode::Adaptive,
        NoiseMode::NoiseCancel,
    ];

    /// The core's bit for this mode.
    pub const fn bit(self) -> u8 {
        self as u8
    }

    /// The mode for a value of exactly one known bit; anything else is `None`.
    pub fn from_bit(v: u64) -> Option<NoiseMode> {
        match v {
            1 => Some(NoiseMode::Off),
            2 => Some(NoiseMode::Transparency),
            4 => Some(NoiseMode::Adaptive),
            8 => Some(NoiseMode::Wind),
            16 => Some(NoiseMode::NoiseCancel),
            _ => None,
        }
    }

    /// The menu modes that the `options` bit set contains.
    pub fn offered(options: u8) -> impl Iterator<Item = NoiseMode> {
        NoiseMode::MENU
            .into_iter()
            .filter(move |m| options & m.bit() != 0)
    }
}

/// How a battery cell's number is to be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellStatus {
    /// Not reported; hide the cell.
    NotAvailable,
    /// Away, and never had a reading; hide the cell.
    Disconnected,
    /// A live reading.
    Connected,
    /// Away; the last reading is kept.
    Cached,
}

impl CellStatus {
    /// Maps the core's number: 1 disconnected, 2 connected, 3 cached, anything else not
    /// available.
    pub fn from_wire(v: u64) -> CellStatus {
        match v {
            1 => CellStatus::Disconnected,
            2 => CellStatus::Connected,
            3 => CellStatus::Cached,
            _ => CellStatus::NotAvailable,
        }
    }
}

/// One battery cell: a bud or the case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// 0 to 100.
    pub percent: u8,
    /// Whether it is charging right now.
    pub charging: bool,
    /// Whether the number is live, last known or absent.
    pub status: CellStatus,
}

impl Cell {
    /// The percent to show: only for a live or last-known cell, never decided by the number
    /// being 0.
    pub fn shown(&self) -> Option<u8> {
        match self.status {
            CellStatus::Connected | CellStatus::Cached => Some(self.percent),
            CellStatus::Disconnected | CellStatus::NotAvailable => None,
        }
    }
}

/// The battery capability: both buds, the case, and the single cell of one-piece headphones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    /// Left bud.
    pub left: Cell,
    /// Right bud.
    pub right: Cell,
    /// Charging case.
    pub case: Cell,
    /// One-piece headphones; stays empty for AirPods.
    pub single: Cell,
}

/// The one number that stands for the buds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudLevel {
    /// 0 to 100.
    pub percent: u8,
    /// Whether that bud is charging.
    pub charging: bool,
    /// Whether the number is a remembered one rather than live.
    pub last_known: bool,
}

impl Battery {
    /// The lower of the shown left and right buds; else the shown single cell.
    pub fn buds(&self) -> Option<BudLevel> {
        match (level(&self.left), level(&self.right)) {
            (Some(l), Some(r)) if r.percent < l.percent => Some(r),
            (Some(l), Some(_)) => Some(l),
            (Some(one), None) | (None, Some(one)) => Some(one),
            (None, None) => level(&self.single),
        }
    }
}

fn level(c: &Cell) -> Option<BudLevel> {
    let percent = c.shown()?;
    Some(BudLevel {
        percent,
        charging: c.charging,
        last_known: c.status == CellStatus::Cached,
    })
}

/// The noise control capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Noise {
    /// The mode in effect, when known.
    pub selected: Option<NoiseMode>,
    /// The bit set of modes the device supports.
    pub options: u8,
    /// Whether the app may change it.
    pub writable: bool,
}

impl Noise {
    /// Whether the app may switch to `mode`: the control is writable and the device offers it.
    pub fn allows(&self, mode: NoiseMode) -> bool {
        self.writable && self.options & mode.bit() != 0
    }
}

/// The device the core reports on.
#[derive(Clone, PartialEq, Eq)]
pub struct Earbuds {
    /// Shown on screen only; never logged or stored.
    pub name: String,
    /// Needed to change the noise mode; never logged or stored.
    pub address: String,
    /// Bluetooth vendor id.
    pub vendor: u16,
    /// Bluetooth product id.
    pub model: u16,
    /// Battery, once reported.
    pub battery: Option<Battery>,
    /// Ear detection, once reported.
    pub wear: Option<Wear>,
    /// Noise control, once reported.
    pub noise: Option<Noise>,
}

impl Earbuds {
    /// Whether this is an Apple device (AirPods or Beats), the only kind this crate serves.
    pub fn is_apple(&self) -> bool {
        self.vendor == APPLE_VENDOR
    }
}

struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Debug for Earbuds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Earbuds")
            .field("name", &Redacted)
            .field("address", &Redacted)
            .field("vendor", &self.vendor)
            .field("model", &self.model)
            .field("battery", &self.battery)
            .field("wear", &self.wear)
            .field("noise", &self.noise)
            .finish()
    }
}

/// One wear reading as the pause policy wants it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WearReading {
    /// Changes whenever readings start afresh (reconnect, other device, no device).
    pub epoch: u64,
    /// The latest value, if the core has reported one.
    pub wear: Option<Wear>,
    /// Whether the reading may be acted on: connected, and an Apple device.
    pub usable: bool,
}

/// Everything the app can know about the earbuds. The worker sends whole snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EarbudsState {
    /// How far the link has got.
    pub link: Link,
    /// The core's version string, informational.
    pub core_version: Option<String>,
    /// The active device; `None` when the core reports none.
    pub device: Option<Earbuds>,
    /// The core lists a connected Apple device that is not `device`.
    pub other_device_active: bool,
    /// The core reports Bluetooth switched off.
    pub bluetooth_off: bool,
    /// A noise change was sent and not yet confirmed.
    pub noise_pending: Option<NoiseMode>,
    /// Bumps whenever wear readings start afresh.
    pub wear_epoch: u64,
}

impl EarbudsState {
    /// The wear reading for the pause policy.
    pub fn reading(&self) -> WearReading {
        WearReading {
            epoch: self.wear_epoch,
            wear: self.device.as_ref().and_then(|d| d.wear),
            usable: self.apple_device().is_some(),
        }
    }

    /// The device, only while the link is up and it is an Apple one.
    pub fn apple_device(&self) -> Option<&Earbuds> {
        if self.link == Link::Connected {
            self.device.as_ref().filter(|d| d.is_apple())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(percent: u8, status: CellStatus) -> Cell {
        Cell {
            percent,
            charging: false,
            status,
        }
    }

    fn battery(left: Cell, right: Cell) -> Battery {
        Battery {
            left,
            right,
            case: cell(0, CellStatus::NotAvailable),
            single: cell(0, CellStatus::NotAvailable),
        }
    }

    #[test]
    fn wear_numbers() {
        assert_eq!(Wear::from_wire(0), Wear::NotAvailable);
        assert_eq!(Wear::from_wire(1), Wear::Worn);
        assert_eq!(Wear::from_wire(2), Wear::Partial);
        assert_eq!(Wear::from_wire(3), Wear::NotWorn);
        assert_eq!(Wear::from_wire(7), Wear::Unknown);
    }

    #[test]
    fn noise_bits() {
        assert_eq!(NoiseMode::from_bit(16), Some(NoiseMode::NoiseCancel));
        assert_eq!(NoiseMode::from_bit(8), Some(NoiseMode::Wind));
        assert_eq!(NoiseMode::from_bit(3), None);
        assert_eq!(NoiseMode::from_bit(0), None);
        assert_eq!(NoiseMode::from_bit(32), None);
        assert_eq!(NoiseMode::Transparency.bit(), 2);
    }

    #[test]
    fn offered_follows_the_options_and_skips_wind() {
        let modes = |o| NoiseMode::offered(o).collect::<Vec<_>>();
        let three = [
            NoiseMode::Off,
            NoiseMode::Transparency,
            NoiseMode::NoiseCancel,
        ];
        assert_eq!(modes(19), three);
        let four = modes(23);
        assert_eq!(four.len(), 4);
        assert!(four.contains(&NoiseMode::Adaptive));
        assert!(modes(8).is_empty());
        assert!(modes(0).is_empty());
    }

    #[test]
    fn a_cell_is_shown_by_status_never_by_percent() {
        assert_eq!(cell(0, CellStatus::Connected).shown(), Some(0));
        assert_eq!(cell(50, CellStatus::Cached).shown(), Some(50));
        assert_eq!(cell(50, CellStatus::Disconnected).shown(), None);
        assert_eq!(cell(50, CellStatus::NotAvailable).shown(), None);
    }

    #[test]
    fn buds_is_the_lower_shown_bud() {
        let live = CellStatus::Connected;
        let left = cell(80, live);
        let right = cell(70, live);
        let level = battery(left, right).buds().unwrap();
        assert_eq!(level.percent, 70);
        assert!(!level.last_known);

        // A tie goes to the left bud; a remembered number is marked last known.
        let left = cell(60, CellStatus::Cached);
        let right = cell(60, live);
        assert!(battery(left, right).buds().unwrap().last_known);

        // One bud away with no reading: the other one stands for both.
        let away = cell(0, CellStatus::Disconnected);
        let level = battery(cell(90, live), away).buds().unwrap();
        assert_eq!(level.percent, 90);

        // Nothing shown, nothing to say; the single cell is the last resort.
        let none = cell(0, CellStatus::NotAvailable);
        let mut b = battery(none, away);
        assert_eq!(b.buds(), None);
        b.single = Cell {
            percent: 33,
            charging: true,
            status: live,
        };
        let level = b.buds().unwrap();
        assert_eq!(level.percent, 33);
        assert!(level.charging && !level.last_known);
    }

    fn device(vendor: u16, wear: Option<Wear>) -> Earbuds {
        Earbuds {
            name: "Test Pods".into(),
            address: "AA:BB:CC:DD:EE:FF".into(),
            vendor,
            model: 8212,
            battery: None,
            wear,
            noise: None,
        }
    }

    #[test]
    fn debug_hides_name_and_address() {
        let text = format!("{:?}", device(APPLE_VENDOR, Some(Wear::Worn)));
        assert!(!text.contains("Test Pods"), "{text}");
        assert!(!text.contains("AA:BB:CC:DD:EE:FF"), "{text}");
        assert!(text.contains("<redacted>"));
        let state = EarbudsState {
            device: Some(device(APPLE_VENDOR, None)),
            ..Default::default()
        };
        let text = format!("{state:?}");
        assert!(!text.contains("Test Pods"), "{text}");
        assert!(!text.contains("AA:BB"), "{text}");
    }

    #[test]
    fn a_reading_is_usable_only_for_a_connected_apple_device() {
        let mut s = EarbudsState {
            link: Link::Connected,
            device: Some(device(APPLE_VENDOR, Some(Wear::Partial))),
            wear_epoch: 4,
            ..Default::default()
        };
        let r = s.reading();
        assert!(r.usable);
        assert_eq!((r.epoch, r.wear), (4, Some(Wear::Partial)));
        assert!(s.apple_device().is_some());

        s.link = Link::Searching;
        assert!(!s.reading().usable);
        assert!(s.apple_device().is_none());

        s.link = Link::Connected;
        s.device = Some(device(117, Some(Wear::Worn)));
        assert!(!s.reading().usable);

        s.device = None;
        let r = s.reading();
        assert!(!r.usable && r.wear.is_none());
    }
}
