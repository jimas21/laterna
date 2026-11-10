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
//! - [`EarbudsLink`]: one background thread that keeps the connection, reconnects with a growing
//!   delay, and hands whole [`EarbudsState`] snapshots to the render thread.
//!
//! The address is fixed to `127.0.0.1` in code, and the only requests that can be built are a
//! state query and a noise mode change. Nothing here reads or logs the core's settings frames,
//! which can carry the earbuds' key material, and no log line holds a device name or address.

pub mod policy;
pub mod settings;
pub mod state;
pub mod wire;
mod worker;

pub use settings::{EarbudsSettings, PauseRule};
pub use state::{APPLE_VENDOR, Battery, BudLevel, Cell, CellStatus};
pub use state::{Earbuds, EarbudsState, Link, Noise, NoiseMode, Wear, WearReading};

use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::worker::Command;

/// The port MagicPodsCore listens on.
pub const CORE_PORT: u16 = 2020;

/// Timings and limits of the link. The defaults are the real ones; tests shrink them.
#[derive(Clone, Debug)]
pub struct EarbudsConfig {
    /// Port on `127.0.0.1`. The address itself is fixed.
    pub port: u16,
    /// How long one TCP connect may take.
    pub connect_timeout: Duration,
    /// How long the WebSocket upgrade may take. Long: the core takes the TCP connection as
    /// soon as it listens but answers the upgrade only once its Bluetooth side is up.
    pub handshake_timeout: Duration,
    /// How long to wait for the greeting after the upgrade.
    pub init_timeout: Duration,
    /// How often the worker comes back from reading to check for commands. Pings are
    /// answered inside reads, so this also bounds how late an answer can be.
    pub read_poll: Duration,
    /// Without any inbound frame for this long, the connection is dropped and redone.
    pub liveness: Duration,
    /// The first retry delay; each failure doubles it.
    pub backoff_base: Duration,
    /// The longest retry delay.
    pub backoff_cap: Duration,
    /// A session this long starts the retry delay over.
    pub stable_after: Duration,
    /// How long to wait for the core to report a noise change before giving up on it.
    pub noise_confirm: Duration,
    /// The largest frame accepted from the core, in bytes.
    pub max_message: usize,
}

impl Default for EarbudsConfig {
    fn default() -> Self {
        Self {
            port: CORE_PORT,
            connect_timeout: Duration::from_secs(1),
            handshake_timeout: Duration::from_secs(10),
            init_timeout: Duration::from_secs(5),
            read_poll: Duration::from_millis(200),
            liveness: Duration::from_secs(40),
            backoff_base: Duration::from_secs(2),
            backoff_cap: Duration::from_secs(30),
            stable_after: Duration::from_secs(10),
            noise_confirm: Duration::from_secs(3),
            max_message: 1 << 20,
        }
    }
}

/// The app's end of the connection to the core.
///
/// Everything on it is non-blocking: it is meant to be used on the render thread, once per
/// frame. Dropping it never waits for the worker.
pub struct EarbudsLink {
    cmd_tx: Option<Sender<Command>>,
    event_rx: Option<Receiver<EarbudsState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    cached: EarbudsState,
}

impl fmt::Debug for EarbudsLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EarbudsLink")
            .field("state", &self.cached)
            .finish_non_exhaustive()
    }
}

impl EarbudsLink {
    /// Starts the worker thread, named `fp-earbuds`. Does no network I/O itself.
    pub fn spawn(config: EarbudsConfig) -> io::Result<EarbudsLink> {
        let (cmd_tx, cmd_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("fp-earbuds".into())
            .spawn(move || worker::run(config, cmd_rx, event_tx, thread_stop))?;
        Ok(EarbudsLink {
            cmd_tx: Some(cmd_tx),
            event_rx: Some(event_rx),
            stop,
            worker: Some(handle),
            cached: EarbudsState {
                link: Link::Searching,
                ..EarbudsState::default()
            },
        })
    }

    /// A link with no thread that always shows `state`: for previews and tests.
    pub fn fixed(state: EarbudsState) -> EarbudsLink {
        EarbudsLink {
            cmd_tx: None,
            event_rx: None,
            stop: Arc::new(AtomicBool::new(true)),
            worker: None,
            cached: state,
        }
    }

    /// Takes the snapshots sent since the last call and keeps the newest. True when that
    /// changed what [`state`](Self::state) returns.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.event_rx else {
            return false;
        };
        let mut newest = None;
        while let Ok(state) = rx.try_recv() {
            newest = Some(state);
        }
        match newest {
            Some(state) if state != self.cached => {
                self.cached = state;
                true
            }
            _ => false,
        }
    }

    /// The latest state seen by [`poll`](Self::poll).
    pub fn state(&self) -> &EarbudsState {
        &self.cached
    }

    /// Asks for a noise mode. Ignored unless connected to an Apple device whose noise control
    /// is writable and offers `mode`. The change shows as pending in the state until the core
    /// reports it, or for a few seconds.
    pub fn set_noise(&self, mode: NoiseMode) {
        let allowed = self.cached.apple_device().and_then(|d| d.noise);
        if allowed.is_some_and(|n| n.allows(mode)) {
            self.send(Command::SetNoise(mode));
        }
    }

    /// Asks again now: a fresh query when connected, an immediate new attempt (with the retry
    /// delay started over) when searching or when the core spoke another protocol version.
    pub fn refresh(&self) {
        self.send(Command::Refresh);
    }

    fn send(&self, cmd: Command) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(cmd);
        }
    }

    /// Stops the worker and waits up to `limit` for it. False when it was stuck (inside a
    /// connect or a handshake, which cannot be woken) and was left to end by itself.
    pub fn stop_and_join(mut self, limit: Duration) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        self.cmd_tx = None;
        self.event_rx = None;
        let Some(handle) = self.worker.take() else {
            return true;
        };
        let deadline = Instant::now() + limit;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();
        true
    }
}

impl Drop for EarbudsLink {
    /// Never joins: a worker inside a connect or a handshake cannot be woken. Dropping the
    /// command sender wakes it from any wait.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.cmd_tx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> EarbudsState {
        EarbudsState {
            link: Link::Connected,
            ..EarbudsState::default()
        }
    }

    #[test]
    fn the_default_config_aims_at_the_core() {
        let c = EarbudsConfig::default();
        assert_eq!(c.port, 2020);
        assert_eq!(c.backoff_base, Duration::from_secs(2));
        assert_eq!(c.backoff_cap, Duration::from_secs(30));
        assert_eq!(c.read_poll, Duration::from_millis(200));
        assert_eq!(c.max_message, 1024 * 1024);
    }

    #[test]
    fn a_fixed_link_shows_its_state_and_ignores_everything() {
        let mut link = EarbudsLink::fixed(connected());
        assert_eq!(link.state().link, Link::Connected);
        assert!(!link.poll());
        link.set_noise(NoiseMode::Off);
        link.refresh();
        assert_eq!(link.state().link, Link::Connected);
        assert!(link.stop_and_join(Duration::from_millis(10)));
    }

    #[test]
    fn poll_keeps_only_the_newest_snapshot() {
        let (tx, rx) = unbounded();
        let mut link = EarbudsLink::fixed(EarbudsState::default());
        link.event_rx = Some(rx);
        let mut second = connected();
        second.wear_epoch = 1;
        let mut third = connected();
        third.wear_epoch = 2;
        tx.send(second).unwrap();
        tx.send(third.clone()).unwrap();
        assert!(link.poll());
        assert_eq!(link.state(), &third);
        assert!(!link.poll());
        // A snapshot equal to what is shown changes nothing.
        tx.send(third).unwrap();
        assert!(!link.poll());
    }
}
