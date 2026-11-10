//! The app's writes to files and the database, off the frame loop.
//!
//! The frame loop must never wait on a disk: a settings save, a progress
//! save and the library lock they take each cost milliseconds on the Frame's
//! storage, and a frame is 14. So they are handed to one thread here, which
//! writes them in turn.
//!
//! * A write has a [`Key`]. One submitted while another with the same key is
//!   still waiting replaces it (the newest settings are all that matter), in
//!   the older one's place in the queue.
//! * What a write reports ([`Done`]) comes back through [`Writer::poll`], so
//!   the caller keeps what it did when a save failed: say so once, and try
//!   again (see [`Failing`]).
//! * Dropping the writer writes everything still waiting, then stops. The app
//!   drops it as it exits, and a video closing hands its last progress over
//!   before that.
//! * Something that reads what was written from another thread (opening a
//!   video reads its saved tracks and position) waits for the queue to empty
//!   first, with a [`Barrier`].

use crossbeam_channel::{Receiver, Sender, unbounded};
use fp_core::sync::lock;
use fp_library::{MediaId, SessionId};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What a write is. A newer write with the key of one still waiting takes
/// its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// `settings.json`.
    Settings,
    /// `sources.json`: the sources and their refreshed credentials.
    Sources,
    /// Where a playback session has got to.
    Progress(SessionId),
    /// A video's chroma key (the Passthrough tab).
    Chroma(MediaId),
    /// The audio and subtitles picked for a video.
    Tracks(MediaId),
}

impl Key {
    /// What a failed write failed to save, for the log.
    pub fn what(&self) -> &'static str {
        match self {
            Key::Settings => "settings",
            Key::Sources => "sources",
            Key::Progress(_) => "progress",
            Key::Chroma(_) => "the background removal",
            Key::Tracks(_) => "the tracks picked",
        }
    }
}

type Job = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// How a write went.
pub struct Done {
    pub key: Key,
    pub result: Result<(), String>,
}

#[derive(Default)]
struct State {
    /// In the order first submitted.
    waiting: Vec<(Key, Job)>,
    running: Option<Key>,
    /// Stop once `waiting` is empty.
    quit: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a write is queued, a write ends, or the writer quits.
    changed: Condvar,
}

impl Shared {
    fn wait<'a>(&self, state: MutexGuard<'a, State>, limit: Duration) -> MutexGuard<'a, State> {
        self.changed
            .wait_timeout(state, limit)
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }
}

/// The writer thread and the way to it.
pub struct Writer {
    shared: Arc<Shared>,
    done: Receiver<Done>,
    /// For a write run in place when the thread could not start.
    report: Sender<Done>,
    thread: Option<JoinHandle<()>>,
}

impl Writer {
    pub fn start() -> Writer {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        let (report, done) = unbounded();
        let thread = {
            let (shared, report) = (shared.clone(), report.clone());
            std::thread::Builder::new()
                .name("fp-writer".into())
                .spawn(move || run(&shared, &report))
                .inspect_err(|e| log::error!("cannot start the writer thread: {e}"))
                .ok()
        };
        Writer {
            shared,
            done,
            report,
            thread,
        }
    }

    /// Queues `job`, replacing a write with the same `key` that has not
    /// started. Without a thread to run it, it runs here.
    pub fn submit(&self, key: Key, job: impl FnOnce() -> Result<(), String> + Send + 'static) {
        if self.thread.is_none() {
            let _ = self.report.send(Done { key, result: job() });
            return;
        }
        let mut st = lock(&self.shared.state);
        match st.waiting.iter_mut().find(|(k, _)| *k == key) {
            Some(waiting) => waiting.1 = Box::new(job),
            None => st.waiting.push((key, Box::new(job))),
        }
        drop(st);
        self.shared.changed.notify_all();
    }

    /// Drops the write with `key` if it is waiting, and waits for it to end
    /// if it is being written, so that a write the caller makes next, on
    /// this thread, is the last. For a user's own click, not for the frame
    /// loop's routine saves.
    pub fn cancel(&self, key: Key) {
        let mut st = lock(&self.shared.state);
        st.waiting.retain(|(k, _)| *k != key);
        while st.running == Some(key) {
            st = self.shared.wait(st, Duration::from_millis(50));
        }
    }

    /// How the writes that ended since the last call went.
    pub fn poll(&self) -> Vec<Done> {
        self.done.try_iter().collect()
    }

    /// A way to wait, from another thread, for the writes queued so far.
    pub fn barrier(&self) -> Barrier {
        Barrier(self.shared.clone())
    }
}

impl Drop for Writer {
    /// Writes what is waiting, then stops.
    fn drop(&mut self) {
        lock(&self.shared.state).quit = true;
        self.shared.changed.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Waits for the writer to have nothing left to write.
#[derive(Clone)]
pub struct Barrier(Arc<Shared>);

impl Barrier {
    /// Returns once nothing is waiting or being written, or after `limit`
    /// (a disk that has stopped answering must not hold everything else).
    pub fn wait(&self, limit: Duration) {
        let end = Instant::now() + limit;
        let mut st = lock(&self.0.state);
        while !st.waiting.is_empty() || st.running.is_some() {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                log::warn!("writes still not done after {} ms", limit.as_millis());
                return;
            }
            st = self.0.wait(st, left);
        }
    }
}

fn run(shared: &Shared, report: &Sender<Done>) {
    loop {
        let (key, job) = {
            let mut st = lock(&shared.state);
            loop {
                if !st.waiting.is_empty() {
                    let next = st.waiting.remove(0);
                    st.running = Some(next.0);
                    break next;
                }
                if st.quit {
                    return;
                }
                st = shared.wait(st, Duration::from_secs(60));
            }
        };
        // A job that panics must not end the writer: the next save would
        // wait for ever.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
            .unwrap_or_else(|_| Err("the write panicked".into()));
        lock(&shared.state).running = None;
        shared.changed.notify_all();
        let _ = report.send(Done { key, result });
    }
}

/// A write that keeps failing: the user is told once, and it is tried again
/// every so often until it works.
#[derive(Debug, Default)]
pub struct Failing(Option<Instant>);

impl Failing {
    /// Notes how the last write went. True when this is a failure after
    /// successes (or the first): the one to tell the user about.
    pub fn record(&mut self, ok: bool, now: Instant) -> bool {
        let first = !ok && self.0.is_none();
        self.0 = (!ok).then_some(now);
        first
    }

    /// The last write failed, more than `after` ago.
    pub fn retry_due(&self, now: Instant, after: Duration) -> bool {
        self.0
            .is_some_and(|at| now.saturating_duration_since(at) > after)
    }

    /// A retry was queued at `now`; the next waits another period, until the
    /// retry says how it went.
    pub fn postpone(&mut self, now: Instant) {
        if self.0.is_some() {
            self.0 = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fp-writer-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Keeps the writer busy with a write under `key` until the returned
    /// sender is used; returns once it has started.
    fn hold(w: &Writer, key: Key) -> std::sync::mpsc::Sender<()> {
        let (release, gate) = channel::<()>();
        let (started_tx, started) = channel::<()>();
        w.submit(key, move || {
            started_tx.send(()).unwrap();
            let _ = gate.recv();
            Ok(())
        });
        started.recv().unwrap();
        release
    }

    #[test]
    fn a_newer_write_replaces_an_older_one_still_waiting() {
        let w = Writer::start();
        let release = hold(&w, Key::Sources);
        let ran = Arc::new(Mutex::new(Vec::new()));
        for n in 1..=3 {
            let ran = ran.clone();
            w.submit(Key::Settings, move || {
                ran.lock().unwrap().push(n);
                Ok(())
            });
        }
        release.send(()).unwrap();
        drop(w);
        assert_eq!(
            *ran.lock().unwrap(),
            vec![3],
            "three sent, the last written once"
        );
    }

    #[test]
    fn writes_with_other_keys_all_happen_in_the_order_first_sent() {
        let w = Writer::start();
        let release = hold(&w, Key::Sources);
        let ran = Arc::new(Mutex::new(Vec::new()));
        let log = |tag: &'static str| {
            let ran = ran.clone();
            move || {
                ran.lock().unwrap().push(tag);
                Ok(())
            }
        };
        w.submit(Key::Settings, log("settings 1"));
        w.submit(Key::Tracks(MediaId(1)), log("tracks"));
        w.submit(Key::Settings, log("settings 2"));
        release.send(()).unwrap();
        drop(w);
        assert_eq!(*ran.lock().unwrap(), ["settings 2", "tracks"]);
    }

    #[test]
    fn what_is_waiting_is_written_when_the_writer_is_dropped() {
        let d = dir("flush");
        let path = d.join("settings.json");
        let w = Writer::start();
        let settings = Settings {
            volume: 0.25,
            ..Default::default()
        };
        let to = path.clone();
        w.submit(Key::Settings, move || {
            settings.save(&to).map_err(|e| e.to_string())
        });
        drop(w);
        assert_eq!(Settings::load(&path).volume, 0.25);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_write_that_works_and_one_that_fails_are_both_reported() {
        let d = dir("report");
        let w = Writer::start();
        let good = d.join("settings.json");
        let settings = Settings::default();
        let (s, to) = (settings.clone(), good.clone());
        w.submit(Key::Settings, move || {
            s.save(&to).map_err(|e| e.to_string())
        });
        w.barrier().wait(Duration::from_secs(10));
        let done = w.poll();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].key, Key::Settings);
        assert!(done[0].result.is_ok());
        assert!(good.exists());

        // A folder that cannot be made, as under a file.
        let file = d.join("a-file");
        std::fs::write(&file, "x").unwrap();
        let bad = file.join("settings.json");
        w.submit(Key::Settings, move || {
            settings.save(&bad).map_err(|e| e.to_string())
        });
        w.barrier().wait(Duration::from_secs(10));
        let done = w.poll();
        assert_eq!(done.len(), 1);
        assert!(done[0].result.as_ref().is_err_and(|e| !e.is_empty()));
        assert!(w.poll().is_empty(), "reported once");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_write_that_panics_does_not_stop_the_writer() {
        let w = Writer::start();
        w.submit(Key::Sources, || panic!("a bad write"));
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        w.submit(Key::Settings, move || {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        w.barrier().wait(Duration::from_secs(10));
        assert!(ran.load(Ordering::SeqCst));
        let done = w.poll();
        assert_eq!(done.len(), 2);
        assert!(done[0].result.is_err() && done[1].result.is_ok());
    }

    #[test]
    fn cancel_drops_a_waiting_write_and_waits_for_a_running_one() {
        let w = Writer::start();
        let release = hold(&w, Key::Sources);
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        w.submit(Key::Chroma(MediaId(7)), move || {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        w.cancel(Key::Chroma(MediaId(7)));

        // The held write ends only once cancel is waiting for it.
        let ended = Arc::new(AtomicBool::new(false));
        let later = {
            let ended = ended.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                ended.store(true, Ordering::SeqCst);
                release.send(()).unwrap();
            })
        };
        w.cancel(Key::Sources);
        assert!(ended.load(Ordering::SeqCst), "waited for the running write");
        later.join().unwrap();
        drop(w);
        assert!(!ran.load(Ordering::SeqCst), "the cancelled write never ran");
    }

    #[test]
    fn a_barrier_waits_for_the_queue_but_not_for_ever() {
        let w = Writer::start();
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        w.submit(Key::Settings, move || {
            std::thread::sleep(Duration::from_millis(40));
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        w.barrier().wait(Duration::from_secs(10));
        assert!(done.load(Ordering::SeqCst));

        let release = hold(&w, Key::Sources);
        let t = Instant::now();
        w.barrier().wait(Duration::from_millis(60));
        assert!(t.elapsed() >= Duration::from_millis(60));
        release.send(()).unwrap();
    }

    #[test]
    fn a_failure_is_told_once_and_retried_after_a_while() {
        let t0 = Instant::now();
        let after = Duration::from_secs(10);
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut f = Failing::default();
        assert!(!f.retry_due(at(100), after), "nothing failed");
        assert!(!f.record(true, at(0)));
        assert!(f.record(false, at(1)), "the first failure is told");
        assert!(!f.retry_due(at(5), after));
        assert!(f.retry_due(at(12), after));
        // A retry is queued: the next waits another period for its answer.
        f.postpone(at(12));
        assert!(!f.retry_due(at(15), after));
        assert!(!f.record(false, at(13)), "still failing: not told again");
        assert!(f.retry_due(at(24), after));
        assert!(!f.record(true, at(25)), "it worked");
        assert!(!f.retry_due(at(100), after));
        assert!(f.record(false, at(101)), "and failing again is news again");
        f.postpone(at(102));
        f.record(true, at(103));
        f.postpone(at(104));
        assert!(
            !f.retry_due(at(200), after),
            "postponing nothing does nothing"
        );
    }
}
