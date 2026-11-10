//! The background thread behind `EarbudsLink`: keeps one WebSocket to the local core, folds
//! what it says into an `EarbudsState` and hands whole snapshots to the render thread.
//!
//! Logging rules: never a frame, never a device name or address, never anything from a
//! settings frame. Errors are logged by kind only, because the text of a library error can
//! echo the bytes that caused it.

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use tungstenite::{HandshakeError, Message, WebSocket};

use crate::EarbudsConfig;
use crate::state::{APPLE_VENDOR, Earbuds, EarbudsState, Link, NoiseMode};
use crate::wire::{self, Frame, Headphone, Request};

/// What the app can ask of the worker.
pub(crate) enum Command {
    /// Change the noise mode.
    SetNoise(NoiseMode),
    /// Ask again now: a fresh `GetAll`, or an immediate reconnect with the backoff reset.
    Refresh,
}

/// How long a write may take once connected; also bounds the final close.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

type Socket = WebSocket<TcpStream>;

/// Why a connection attempt failed. Kinds only: nothing that could echo input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectError {
    Refused,
    TimedOut,
    Handshake,
    Io(io::ErrorKind),
}

impl ConnectError {
    fn from_kind(kind: io::ErrorKind) -> ConnectError {
        match kind {
            io::ErrorKind::ConnectionRefused => ConnectError::Refused,
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ConnectError::TimedOut,
            other => ConnectError::Io(other),
        }
    }

    fn from_io(e: io::Error) -> ConnectError {
        ConnectError::from_kind(e.kind())
    }

    fn from_ws(e: &tungstenite::Error) -> ConnectError {
        match e {
            tungstenite::Error::Io(err) => ConnectError::from_kind(err.kind()),
            _ => ConnectError::Handshake,
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Refused => f.write_str("connection refused"),
            ConnectError::TimedOut => f.write_str("timed out"),
            ConnectError::Handshake => f.write_str("handshake failed"),
            ConnectError::Io(kind) => write!(f, "I/O error: {kind}"),
        }
    }
}

/// How a connected session ended.
enum SessionEnd {
    /// The app is done with the link.
    Stopped,
    /// The connection dropped, or the core went quiet.
    Lost,
    /// The core speaks another protocol version.
    Incompatible,
}

/// What one read from the socket produced.
enum Incoming {
    Frame(Box<Frame>),
    /// Something arrived (a ping, an unreadable frame) but nothing to act on.
    Activity,
    /// The read poll timed out.
    Idle,
    Closed,
}

/// The delay between attempts: base, twice, four times and so on up to the cap.
struct Backoff {
    base: Duration,
    cap: Duration,
    n: u32,
}

impl Backoff {
    fn new(base: Duration, cap: Duration) -> Backoff {
        Backoff { base, cap, n: 0 }
    }

    fn next_delay(&mut self) -> Duration {
        let factor: u32 = 1 << self.n.min(4);
        self.n = self.n.saturating_add(1);
        self.base.saturating_mul(factor).min(self.cap)
    }

    fn reset(&mut self) {
        self.n = 0;
    }
}

fn is_timeout(e: &tungstenite::Error) -> bool {
    let tungstenite::Error::Io(err) = e else {
        return false;
    };
    let kind = err.kind();
    kind == io::ErrorKind::WouldBlock || kind == io::ErrorKind::TimedOut
}

/// Runs until the app drops the link. Never panics on what the core sends.
pub(crate) fn run(
    config: EarbudsConfig,
    cmds: Receiver<Command>,
    events: Sender<EarbudsState>,
    stop: Arc<AtomicBool>,
) {
    Worker::new(config, cmds, events, stop).main_loop();
}

struct Worker {
    config: EarbudsConfig,
    cmds: Receiver<Command>,
    events: Sender<EarbudsState>,
    stop: Arc<AtomicBool>,
    state: EarbudsState,
    /// The last snapshot handed out; only changes are sent.
    sent: Option<EarbudsState>,
    backoff: Backoff,
    /// The core's list of paired devices.
    headphones: Vec<Headphone>,
    /// When the pending noise change was sent.
    pending_since: Option<Instant>,
    /// Whether the log already says an Apple device is ready.
    announced: bool,
    /// Whether the log already says the core is out of reach.
    outage: bool,
}

impl Worker {
    fn new(
        config: EarbudsConfig,
        cmds: Receiver<Command>,
        events: Sender<EarbudsState>,
        stop: Arc<AtomicBool>,
    ) -> Worker {
        let backoff = Backoff::new(config.backoff_base, config.backoff_cap);
        Worker {
            config,
            cmds,
            events,
            stop,
            state: EarbudsState::default(),
            sent: None,
            backoff,
            headphones: Vec::new(),
            pending_since: None,
            announced: false,
            outage: false,
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn main_loop(&mut self) {
        while !self.stopped() {
            self.state.link = Link::Searching;
            self.publish();
            match self.connect() {
                Ok(ws) => {
                    let started = Instant::now();
                    match self.session(ws) {
                        SessionEnd::Stopped => return,
                        SessionEnd::Incompatible => {
                            if !self.wait_for_refresh() {
                                return;
                            }
                            self.backoff.reset();
                            continue;
                        }
                        SessionEnd::Lost => self.on_lost(started.elapsed()),
                    }
                }
                Err(e) => self.on_unreachable(e),
            }
            let delay = self.backoff.next_delay();
            if !self.wait(delay) {
                return;
            }
        }
    }

    /// Opens the socket and does the WebSocket handshake.
    fn connect(&self) -> Result<Socket, ConnectError> {
        let cfg = &self.config;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, cfg.port));
        let stream = TcpStream::connect_timeout(&addr, cfg.connect_timeout)
            .map_err(ConnectError::from_io)?;
        stream.set_nodelay(true).map_err(ConnectError::from_io)?;
        // The core takes the TCP connection as soon as it listens but answers the upgrade
        // only once its Bluetooth side is up, so the handshake gets a long timeout.
        stream
            .set_read_timeout(Some(cfg.handshake_timeout))
            .map_err(ConnectError::from_io)?;
        stream
            .set_write_timeout(Some(cfg.handshake_timeout))
            .map_err(ConnectError::from_io)?;
        let url = format!("ws://127.0.0.1:{}/", cfg.port);
        let limits = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(cfg.max_message))
            .max_frame_size(Some(cfg.max_message));
        match tungstenite::client::client_with_config(url.as_str(), stream, Some(limits)) {
            Ok((ws, _response)) => {
                // From here on reads return every poll so pings keep being answered.
                let tcp = ws.get_ref();
                tcp.set_read_timeout(Some(cfg.read_poll))
                    .map_err(ConnectError::from_io)?;
                tcp.set_write_timeout(Some(WRITE_TIMEOUT))
                    .map_err(ConnectError::from_io)?;
                Ok(ws)
            }
            Err(HandshakeError::Interrupted(_)) => Err(ConnectError::TimedOut),
            Err(HandshakeError::Failure(e)) => Err(ConnectError::from_ws(&e)),
        }
    }

    /// Runs one connection to its end, then closes it.
    fn session(&mut self, mut ws: Socket) -> SessionEnd {
        let end = self.run_session(&mut ws);
        let _ = ws.close(None);
        let _ = ws.flush();
        end
    }

    fn run_session(&mut self, ws: &mut Socket) -> SessionEnd {
        let deadline = Instant::now() + self.config.init_timeout;
        let init = loop {
            if self.stopped() {
                return SessionEnd::Stopped;
            }
            if Instant::now() >= deadline {
                log::debug!("earbuds: the core did not greet us");
                return SessionEnd::Lost;
            }
            match self.read_one(ws) {
                Incoming::Frame(frame) => {
                    if let Some(init) = frame.init {
                        break init;
                    }
                }
                Incoming::Closed => return SessionEnd::Lost,
                Incoming::Activity | Incoming::Idle => {}
            }
        };
        self.state.core_version = init.version;
        if init.api != Some(wire::API) {
            let need = wire::API;
            let said = init.api;
            log::warn!("earbuds: core API is {said:?}, this build needs {need}");
            self.state.link = Link::Incompatible;
            self.publish();
            return SessionEnd::Incompatible;
        }
        self.state.link = Link::Connected;
        self.outage = false;
        self.publish();
        let version = self.state.core_version.as_deref().unwrap_or("unknown");
        log::info!("earbuds: connected to core {version}");
        if !self.send(ws, &Request::GetAll) {
            return SessionEnd::Lost;
        }
        self.connected_loop(ws)
    }

    fn connected_loop(&mut self, ws: &mut Socket) -> SessionEnd {
        let mut last_rx = Instant::now();
        loop {
            if self.stopped() {
                return SessionEnd::Stopped;
            }
            if let Some(end) = self.drain_commands(ws) {
                return end;
            }
            self.expire_pending();
            if last_rx.elapsed() > self.config.liveness {
                log::info!("earbuds: the core went silent");
                return SessionEnd::Lost;
            }
            match self.read_one(ws) {
                Incoming::Frame(frame) => {
                    last_rx = Instant::now();
                    self.fold(*frame);
                    self.publish();
                }
                Incoming::Activity => last_rx = Instant::now(),
                Incoming::Idle => {}
                Incoming::Closed => return SessionEnd::Lost,
            }
        }
    }

    /// Runs the queued commands; `Some` when the session has to end.
    fn drain_commands(&mut self, ws: &mut Socket) -> Option<SessionEnd> {
        loop {
            let cmd = match self.cmds.try_recv() {
                Ok(cmd) => cmd,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => return Some(SessionEnd::Stopped),
            };
            let sent = match cmd {
                Command::SetNoise(mode) => self.set_noise(ws, mode),
                Command::Refresh => self.send(ws, &Request::GetAll),
            };
            if !sent {
                return Some(SessionEnd::Lost);
            }
        }
    }

    /// Sends a noise change when the device offers it; otherwise ignores the request.
    /// False when the connection is gone.
    fn set_noise(&mut self, ws: &mut Socket, mode: NoiseMode) -> bool {
        let Some(device) = self.state.apple_device() else {
            return true;
        };
        // Asking for the mode already in effect changes nothing, so nothing is pending.
        let allowed = device
            .noise
            .is_some_and(|n| n.allows(mode) && n.selected != Some(mode));
        if !allowed {
            return true;
        }
        let address = device.address.clone();
        if !self.send(ws, &Request::SetNoise { address, mode }) {
            return false;
        }
        self.state.noise_pending = Some(mode);
        self.pending_since = Some(Instant::now());
        self.publish();
        true
    }

    /// Gives up waiting for the core to confirm a noise change.
    fn expire_pending(&mut self) {
        let wait = self.config.noise_confirm;
        let expired = self.pending_since.is_some_and(|t| t.elapsed() >= wait);
        if expired {
            self.pending_since = None;
            self.state.noise_pending = None;
            self.publish();
            log::info!("earbuds: noise mode not confirmed");
        }
    }

    /// False when the connection is gone. A write that only timed out stays queued and goes
    /// out with the next flush.
    fn send(&self, ws: &mut Socket, request: &Request) -> bool {
        match ws.send(Message::text(request.to_json())) {
            Ok(()) => true,
            Err(e) => is_timeout(&e),
        }
    }

    fn read_one(&self, ws: &mut Socket) -> Incoming {
        let message = ws.read();
        // `read` queues the answers to pings and closes; push them out now.
        let _ = ws.flush();
        match message {
            Ok(Message::Text(text)) => match wire::parse(text.as_str()) {
                Ok(frame) => {
                    log::debug!("earbuds: frame with keys {}", frame.keys.join(","));
                    Incoming::Frame(Box::new(frame))
                }
                Err(e) => {
                    log::debug!("earbuds: {e}");
                    Incoming::Activity
                }
            },
            Err(e) if is_timeout(&e) => Incoming::Idle,
            Ok(Message::Close(_)) | Err(_) => Incoming::Closed,
            Ok(_) => Incoming::Activity,
        }
    }

    /// Folds a frame into the state, in the order the core's replies are meant to be read.
    fn fold(&mut self, frame: Frame) {
        if let Some(list) = frame.headphones {
            self.headphones = list;
        }
        if let Some(on) = frame.bluetooth_enabled {
            self.state.bluetooth_off = !on;
        }
        match frame.info {
            None => {}
            Some(None) => {
                if self.state.device.is_some() {
                    self.state.wear_epoch += 1;
                }
                self.forget_device();
            }
            Some(Some(device)) => self.set_device(device),
        }
        self.state.other_device_active = self.other_device_active();
    }

    fn set_device(&mut self, new: Earbuds) {
        let old = self.state.device.take();
        let same = old.as_ref().is_some_and(|o| o.address == new.address);
        let had_wear = old.as_ref().is_some_and(|o| o.wear.is_some());
        // Readings start afresh for another device, and when the wear report vanishes.
        if !same || (had_wear && new.wear.is_none()) {
            self.state.wear_epoch += 1;
        }
        if !same {
            self.state.noise_pending = None;
            self.pending_since = None;
        }
        if let Some(wanted) = self.state.noise_pending {
            let selected = new.noise.and_then(|n| n.selected);
            if selected == Some(wanted) {
                self.state.noise_pending = None;
                self.pending_since = None;
            }
        }
        let apple = new.is_apple();
        if apple && !self.announced {
            log::info!("earbuds: Apple device ready (model {})", new.model);
        } else if !apple && self.announced {
            log::info!("earbuds: the Apple device is gone");
        }
        self.announced = apple;
        self.state.device = Some(new);
    }

    fn forget_device(&mut self) {
        self.state.device = None;
        self.state.noise_pending = None;
        self.pending_since = None;
        if self.announced {
            self.announced = false;
            log::info!("earbuds: the Apple device is gone");
        }
    }

    /// Whether the core lists a connected Apple device that is not the active one.
    fn other_device_active(&self) -> bool {
        let current = self.state.device.as_ref().map(|d| d.address.as_str());
        for h in &self.headphones {
            let apple = h.connected && h.vendor == APPLE_VENDOR;
            if apple && Some(h.address.as_str()) != current {
                return true;
            }
        }
        false
    }

    /// Hands the app a snapshot, only when it differs from the last one.
    fn publish(&mut self) {
        if self.sent.as_ref() == Some(&self.state) {
            return;
        }
        self.sent = Some(self.state.clone());
        if self.events.send(self.state.clone()).is_err() {
            // The app dropped the link.
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    /// The connection ended after it had been up (or half up).
    fn on_lost(&mut self, lasted: Duration) {
        if self.state.link == Link::Connected {
            self.state.wear_epoch += 1;
            self.outage = true;
            log::info!("earbuds: lost the core; looking for it again");
        } else {
            self.on_unreachable(ConnectError::TimedOut);
        }
        self.forget_device();
        self.headphones.clear();
        self.state.other_device_active = false;
        self.state.bluetooth_off = false;
        self.state.link = Link::Searching;
        self.publish();
        if lasted >= self.config.stable_after {
            self.backoff.reset();
        }
    }

    /// One `info` line per outage, `debug` for the retries after it.
    fn on_unreachable(&mut self, e: ConnectError) {
        if self.outage {
            log::debug!("earbuds: core not reachable ({e})");
        } else {
            self.outage = true;
            log::info!("earbuds: core not reachable ({e}); will keep trying");
        }
    }

    /// Waits out a retry delay. False when the worker should end. A refresh cuts the wait
    /// short and starts the backoff over.
    fn wait(&mut self, delay: Duration) -> bool {
        let deadline = Instant::now() + delay;
        loop {
            if self.stopped() {
                return false;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            match self.cmds.recv_timeout(left) {
                Ok(Command::Refresh) => {
                    self.backoff.reset();
                    return true;
                }
                Ok(Command::SetNoise(_)) => {}
                Err(RecvTimeoutError::Timeout) => return true,
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    /// Blocks until the app asks again. False when the link is gone. Dropping the link
    /// drops the command sender, which wakes this up.
    fn wait_for_refresh(&self) -> bool {
        loop {
            match self.cmds.recv() {
                Ok(Command::Refresh) => return true,
                Ok(Command::SetNoise(_)) => {}
                Err(_) => return false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Wear;
    use crossbeam_channel::unbounded;

    const ADDR: &str = "AA:BB:CC:DD:EE:FF";

    fn worker() -> (Worker, Receiver<EarbudsState>) {
        let (_cmd_tx, cmd_rx) = unbounded();
        let (ev_tx, ev_rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let mut w = Worker::new(EarbudsConfig::default(), cmd_rx, ev_tx, stop);
        w.state.link = Link::Connected;
        (w, ev_rx)
    }

    fn info(address: &str, wear: u64, selected: u64) -> Frame {
        let value = serde_json::json!({"info": {
            "name": "Test Pods", "address": address, "vendor": 76, "model": 8212,
            "connected": true,
            "capabilities": {
                "anc": {"selected": selected, "options": 23, "readonly": false},
                "earDetectionState": {"readonly": true, "selected": wear},
            },
        }});
        wire::parse(&value.to_string()).unwrap()
    }

    fn frame(text: &str) -> Frame {
        wire::parse(text).unwrap()
    }

    fn drain(rx: &Receiver<EarbudsState>) -> Vec<EarbudsState> {
        rx.try_iter().collect()
    }

    #[test]
    fn backoff_doubles_to_the_cap_and_restarts() {
        let mut b = Backoff::new(Duration::from_secs(2), Duration::from_secs(30));
        let secs: Vec<u64> = (0..6).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(secs, [2, 4, 8, 16, 30, 30]);
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(2));
    }

    #[test]
    fn io_kinds_map_to_connect_errors() {
        let refused = ConnectError::from_kind(io::ErrorKind::ConnectionRefused);
        assert_eq!(refused, ConnectError::Refused);
        let slow = ConnectError::from_kind(io::ErrorKind::TimedOut);
        assert_eq!(slow, ConnectError::TimedOut);
        let other = ConnectError::from_kind(io::ErrorKind::BrokenPipe);
        assert_eq!(other, ConnectError::Io(io::ErrorKind::BrokenPipe));
    }

    #[test]
    fn a_burst_of_identical_frames_is_published_once() {
        let (mut w, rx) = worker();
        for _ in 0..3 {
            w.fold(info(ADDR, 1, 16));
            w.publish();
        }
        let sent = drain(&rx);
        assert_eq!(sent.len(), 1);
        let reading = sent[0].reading();
        assert_eq!(reading.wear, Some(Wear::Worn));
        assert!(reading.usable);
    }

    #[test]
    fn a_wear_change_is_published_without_a_new_epoch() {
        let (mut w, rx) = worker();
        w.fold(info(ADDR, 1, 16));
        w.publish();
        let epoch = w.state.wear_epoch;
        w.fold(info(ADDR, 2, 16));
        w.publish();
        let sent = drain(&rx);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].reading().wear, Some(Wear::Partial));
        assert_eq!(sent[1].wear_epoch, epoch);
    }

    #[test]
    fn another_address_or_an_empty_info_starts_the_readings_afresh() {
        let (mut w, _rx) = worker();
        w.fold(info(ADDR, 1, 16));
        let first = w.state.wear_epoch;
        w.fold(info("11:22:33:44:55:66", 1, 16));
        let second = w.state.wear_epoch;
        assert!(second > first);
        w.fold(frame(r#"{"info":{}}"#));
        assert!(w.state.wear_epoch > second);
        assert!(w.state.device.is_none());
        // Nothing to forget: no further bump.
        let third = w.state.wear_epoch;
        w.fold(frame(r#"{"info":{}}"#));
        assert_eq!(w.state.wear_epoch, third);
    }

    #[test]
    fn the_wear_report_vanishing_starts_the_readings_afresh() {
        let (mut w, _rx) = worker();
        w.fold(info(ADDR, 1, 16));
        let before = w.state.wear_epoch;
        let bare = r#"{"info":{"address":"AA:BB:CC:DD:EE:FF","vendor":76,"connected":true}}"#;
        w.fold(frame(bare));
        assert!(w.state.wear_epoch > before);
        assert_eq!(w.state.reading().wear, None);
    }

    #[test]
    fn a_noise_change_is_confirmed_by_the_core_reporting_it() {
        let (mut w, _rx) = worker();
        w.fold(info(ADDR, 1, 2));
        w.state.noise_pending = Some(NoiseMode::NoiseCancel);
        w.pending_since = Some(Instant::now());
        w.fold(info(ADDR, 1, 2));
        assert_eq!(w.state.noise_pending, Some(NoiseMode::NoiseCancel));
        w.fold(info(ADDR, 1, 16));
        assert_eq!(w.state.noise_pending, None);
        assert!(w.pending_since.is_none());
    }

    #[test]
    fn another_connected_apple_device_is_noticed() {
        let (mut w, _rx) = worker();
        let both = r#"{"headphones":[
            {"address":"11:22:33:44:55:66","vendor":76,"connected":true},
            {"address":"AA:BB:CC:DD:EE:FF","vendor":76,"connected":true}]}"#;
        w.fold(frame(both));
        // No active device yet, two connected Apple devices.
        assert!(w.state.other_device_active);
        w.fold(info(ADDR, 1, 16));
        // The active one is one of them; the other one is still listed.
        assert!(w.state.other_device_active);
        let alone = r#"{"headphones":[
            {"address":"AA:BB:CC:DD:EE:FF","vendor":76,"connected":true},
            {"address":"11:22:33:44:55:66","vendor":76,"connected":false},
            {"address":"22:22:33:44:55:66","vendor":117,"connected":true}]}"#;
        w.fold(frame(alone));
        assert!(!w.state.other_device_active);
    }

    #[test]
    fn bluetooth_off_follows_the_core() {
        let (mut w, _rx) = worker();
        w.fold(frame(r#"{"defaultbluetooth":{"enabled":false}}"#));
        assert!(w.state.bluetooth_off);
        w.fold(frame(r#"{"defaultbluetooth":{"enabled":true}}"#));
        assert!(!w.state.bluetooth_off);
    }

    #[test]
    fn losing_the_connection_clears_the_device_and_restarts_readings() {
        let (mut w, rx) = worker();
        w.fold(info(ADDR, 1, 16));
        w.publish();
        let before = w.state.wear_epoch;
        w.on_lost(Duration::from_secs(1));
        let sent = drain(&rx);
        let last = sent.last().unwrap();
        assert_eq!(last.link, Link::Searching);
        assert!(last.device.is_none());
        assert!(last.wear_epoch > before);
        // A session that short does not reset the delay.
        assert_eq!(w.backoff.n, 0);
        let _ = w.backoff.next_delay();
        w.on_lost(Duration::from_secs(1));
        assert_eq!(w.backoff.n, 1);
        w.on_lost(w.config.stable_after);
        assert_eq!(w.backoff.n, 0);
    }
}
