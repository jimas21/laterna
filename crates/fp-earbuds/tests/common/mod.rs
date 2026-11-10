//! A fake MagicPodsCore on a loopback port, shared by the integration tests.
//!
//! It records every frame the client sends and flags any that is not on the short list the
//! crate may send: a state query, or a noise change that touches nothing but `anc`.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fp_earbuds::{EarbudsConfig, EarbudsLink, EarbudsState};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

pub const ADDR: &str = "AA:BB:CC:DD:EE:FF";
pub const INIT_OK: &str = r#"{"init":{"api":1,"version":"2.0.12"}}"#;
pub const GET_ALL: &str = r#"{"method":"GetAll"}"#;

/// What the fake core knows and what the test wants it to do.
#[derive(Default)]
pub struct Shared {
    pub received: Mutex<Vec<String>>,
    pub violations: Mutex<Vec<String>>,
    pub accept_times: Mutex<Vec<Instant>>,
    /// Frames to push to whichever connection looks next.
    pub outbox: Mutex<VecDeque<Message>>,
    pub pongs: Mutex<Vec<Vec<u8>>>,
    /// Connections with an index below this one end at once.
    kill_floor: AtomicUsize,
    stop: AtomicBool,
}

impl Shared {
    pub fn accepts(&self) -> usize {
        self.accept_times.lock().unwrap().len()
    }

    pub fn received(&self) -> Vec<String> {
        self.received.lock().unwrap().clone()
    }

    /// Ends every connection whose index is below `n`; later ones are served as usual.
    pub fn kill_below(&self, n: usize) {
        self.kill_floor.store(n, Ordering::Relaxed);
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn push(&self, message: Message) {
        self.outbox.lock().unwrap().push_back(message);
    }

    fn record(&self, text: &str) {
        if !allowed(text) {
            self.violations.lock().unwrap().push(text.to_owned());
        }
        self.received.lock().unwrap().push(text.to_owned());
    }

    fn should_end(&self, index: usize) -> bool {
        self.stopped() || index < self.kill_floor.load(Ordering::Relaxed)
    }
}

/// Whether the client may send this: `GetAll`, or a `SetCapabilities` with only `anc`.
fn allowed(text: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    if v == json!({"method": "GetAll"}) {
        return true;
    }
    let is_caps = v["method"].as_str() == Some("SetCapabilities");
    let caps = v["arguments"]["capabilities"].as_object();
    let only_anc = caps.is_some_and(|c| c.len() == 1 && c.contains_key("anc"));
    is_caps && only_anc
}

pub struct FakeCore {
    pub port: u16,
    pub shared: Arc<Shared>,
    accepter: Option<JoinHandle<()>>,
}

impl FakeCore {
    /// Listens on a free loopback port and runs `script(index, stream, shared)` on its own
    /// thread for every connection, `index` counting from 0.
    pub fn start<F>(script: F) -> FakeCore
    where
        F: Fn(usize, TcpStream, &Shared) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = Arc::new(Shared::default());
        let script = Arc::new(script);
        let accept_shared = Arc::clone(&shared);
        let accepter = std::thread::spawn(move || {
            while !accept_shared.stop.load(Ordering::Relaxed) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                let index = {
                    let mut times = accept_shared.accept_times.lock().unwrap();
                    times.push(Instant::now());
                    times.len() - 1
                };
                let conn_shared = Arc::clone(&accept_shared);
                let conn_script = Arc::clone(&script);
                std::thread::spawn(move || {
                    let state: &Shared = &conn_shared;
                    (*conn_script)(index, stream, state);
                });
            }
        });
        FakeCore {
            port,
            shared,
            accepter: Some(accepter),
        }
    }

    /// Greets with `INIT_OK` and answers a `GetAll` with `replies`.
    pub fn standard(replies: Vec<String>) -> FakeCore {
        FakeCore::start(move |index, stream, shared| {
            standard(index, stream, shared, INIT_OK, &replies);
        })
    }

    /// Stops the core and fails if the client ever sent a frame it may not send.
    pub fn finish(self) {
        let violations = self.shared.violations.lock().unwrap().len();
        assert_eq!(violations, 0, "the client sent a frame it may not send");
    }
}

impl Drop for FakeCore {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.accepter.take() {
            let _ = handle.join();
        }
    }
}

fn is_timeout(e: &tungstenite::Error) -> bool {
    let tungstenite::Error::Io(err) = e else {
        return false;
    };
    let kind = err.kind();
    kind == ErrorKind::WouldBlock || kind == ErrorKind::TimedOut
}

/// Upgrades the connection, sends `init`, records every frame, answers `GetAll` with
/// `replies`, and pushes whatever the test queued. Ends when the client goes away or the
/// test stops or kills the connection.
pub fn standard(idx: usize, stream: TcpStream, shared: &Shared, init: &str, out: &[String]) {
    let Ok(mut ws) = tungstenite::accept(stream) else {
        return;
    };
    if ws.send(Message::text(init)).is_err() {
        return;
    }
    let _ = ws
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(20)));
    serve(&mut ws, idx, shared, out);
}

fn serve(ws: &mut WebSocket<TcpStream>, idx: usize, shared: &Shared, out: &[String]) {
    loop {
        if shared.should_end(idx) {
            return;
        }
        let queued = shared.outbox.lock().unwrap().pop_front();
        if queued.is_some_and(|m| ws.send(m).is_err()) {
            return;
        }
        let message = match ws.read() {
            Ok(message) => message,
            Err(e) if is_timeout(&e) => continue,
            Err(_) => return,
        };
        match message {
            Message::Text(text) => {
                shared.record(text.as_str());
                if text.as_str() == GET_ALL {
                    for reply in out {
                        if ws.send(Message::text(reply.as_str())).is_err() {
                            return;
                        }
                    }
                }
            }
            Message::Pong(data) => shared.pongs.lock().unwrap().push(data.to_vec()),
            Message::Close(_) => return,
            _ => {}
        }
    }
}

/// The active-device object of spec section 3.4, with a worn state and a noise mode.
pub fn info_value(name: &str, wear: u64, selected: u64, options: u64) -> Value {
    json!({
        "name": name, "address": ADDR, "vendor": 76, "model": 8212, "color": 0,
        "connected": true,
        "capabilities": {
            "battery": {
                "single": {"battery": 0, "charging": false, "status": 0},
                "left": {"battery": 80, "charging": false, "status": 2},
                "right": {"battery": 70, "charging": false, "status": 2},
                "case": {"battery": 40, "charging": false, "status": 3},
                "readonly": true
            },
            "anc": {"selected": selected, "options": options, "readonly": false},
            "earDetection": {
                "readonly": false,
                "selected": {"pauseMedia": true, "switchToSpeakers": false, "lowerVolume": 0}
            },
            "earDetectionState": {"readonly": true, "selected": wear}
        }
    })
}

pub fn info_frame(wear: u64, selected: u64) -> String {
    let frame = json!({"info": info_value("Test Pods", wear, selected, 23)});
    frame.to_string()
}

/// A reply to `GetAll`: the device list, the Bluetooth state and the active device.
pub fn get_all_frame(name: &str, wear: u64, selected: u64, options: u64) -> String {
    let frame = json!({
        "headphones": [
            {"name": name, "address": ADDR, "vendor": 76, "model": 8212, "color": 0,
             "connected": true}
        ],
        "defaultbluetooth": {"enabled": true},
        "info": info_value(name, wear, selected, options)
    });
    frame.to_string()
}

/// Short timings: a retry after 50 ms at first, up to 200 ms.
pub fn config(port: u16) -> EarbudsConfig {
    EarbudsConfig {
        port,
        connect_timeout: Duration::from_millis(500),
        handshake_timeout: Duration::from_secs(5),
        init_timeout: Duration::from_secs(2),
        read_poll: Duration::from_millis(20),
        liveness: Duration::from_secs(30),
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        stable_after: Duration::from_secs(10),
        noise_confirm: Duration::from_millis(400),
        max_message: 1 << 20,
    }
}

/// Calls `cond` every few milliseconds for up to five seconds.
pub fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// Polls the link until `cond` holds for its state.
pub fn wait_state(link: &mut EarbudsLink, cond: impl Fn(&EarbudsState) -> bool) -> bool {
    wait_until(|| {
        link.poll();
        cond(link.state())
    })
}
