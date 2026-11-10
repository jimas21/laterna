//! Battery, noise control and ear detection for AirPods and Beats, read from a local
//! MagicPodsCore.
//!
//! MagicPodsCore is a separate program with its own licence. This crate links none of its code:
//! it only talks to it over a WebSocket on this machine, using the small part of its protocol
//! described in [`wire`].
//!
//! - [`state`]: what is known about the earbuds. Plain data.
//! - [`wire`]: the JSON, parsed forgivingly and without any I/O.
//! - [`settings`]: the user's four settings, loaded forgivingly.
//! - [`policy`]: when to pause or resume a video as earbuds come out and go back in. Pure; the
//!   caller passes the time.

pub mod policy;
pub mod settings;
pub mod state;
pub mod wire;

pub use settings::{EarbudsSettings, PauseRule};
pub use state::{APPLE_VENDOR, Battery, BudLevel, Cell, CellStatus};
pub use state::{Earbuds, EarbudsState, Link, Noise, NoiseMode, Wear, WearReading};
