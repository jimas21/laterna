//! Builds places off the render thread.
//!
//! `Place::build` makes a place's meshes, ambient occlusion and baked
//! house-light shares: tens of thousands of triangles of CPU work that would
//! stall an XR frame if done while recording it. A worker thread does it
//! instead; the renderer asks for a place, keeps showing what it shows (Plain
//! the first time, the place loaded before it when switching) while it is
//! made, and uploads the data on the render thread (a copy of a few
//! megabytes) on the first frame it is ready. No GPU is touched here, so all of it is unit
//! tested without a device.
//!
//! A pack with a set of textures for each seat is several things to build for
//! one place: what is asked for and handed over is a place *and* the set
//! (see [`Place::seat_set`]). Only the newest ask for a place counts: a set
//! built for a seat the viewer has since left is dropped, not kept.

use crate::env::{Place, PlaceData, SeatName};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

/// A place and which set of a pack's textures it is built with (`None`: the
/// only one there is).
pub(crate) type Key = (Place, Option<SeatName>);

/// How a place being built is getting on.
#[derive(Debug)]
pub(crate) enum Built {
    /// Not done yet (or never asked for).
    Pending,
    /// Done: handed over once.
    Ready(PlaceData),
    /// The worker died before it finished: it will not come.
    Failed,
}

/// What builds a place: [`Place::build_set`], or a stand-in in tests.
pub(crate) type BuildFn = fn(Place, Option<SeatName>) -> PlaceData;

/// Makes `place` (its `set`) with `build`, saying how long it took.
pub(crate) fn build_timed(build: BuildFn, place: Place, set: Option<SeatName>) -> PlaceData {
    let start = Instant::now();
    let data = build(place, set);
    let indices = data
        .baked
        .as_ref()
        .map_or(data.indices.len(), |b| b.indices.len());
    let seat = set.map_or(String::new(), |s| format!(" ({} seat)", s.id()));
    log::info!(
        "built place {}{seat}: {} triangles in {:.1} ms",
        place.id(),
        indices / 3,
        start.elapsed().as_secs_f64() * 1e3
    );
    data
}

/// Places being built, built and waiting to be taken.
pub(crate) struct PlaceBuilder {
    build: BuildFn,
    tx: Sender<(Key, PlaceData)>,
    rx: Receiver<(Key, PlaceData)>,
    /// Workers that have not handed their place over yet.
    working: Vec<(Key, JoinHandle<()>)>,
    ready: Vec<(Key, PlaceData)>,
    failed: Vec<Key>,
    /// What each place was last asked for with: an older set that turns up
    /// is not wanted.
    latest: Vec<Key>,
}

impl PlaceBuilder {
    pub fn new() -> PlaceBuilder {
        PlaceBuilder::with(Place::build_set)
    }

    /// Builds with `build`: for tests.
    pub fn with(build: BuildFn) -> PlaceBuilder {
        let (tx, rx) = mpsc::channel();
        PlaceBuilder {
            build,
            tx,
            rx,
            working: Vec::new(),
            ready: Vec::new(),
            failed: Vec::new(),
            latest: Vec::new(),
        }
    }

    fn known(&self, key: Key) -> bool {
        self.working.iter().any(|(k, _)| *k == key)
            || self.ready.iter().any(|(k, _)| *k == key)
            || self.failed.contains(&key)
    }

    /// Whether `key` is what its place was last asked for with.
    fn wanted(&self, key: Key) -> bool {
        self.latest.contains(&key)
    }

    /// Starts building `place` unless it is already being built, built, or
    /// failed. Cheap to call every frame.
    pub fn request(&mut self, place: Place, set: Option<SeatName>) {
        let key = (place, set);
        // The newest ask is the only one that counts: what was built for
        // another set of the same place is no use now.
        self.latest.retain(|k| k.0 != place);
        self.latest.push(key);
        self.ready.retain(|(k, _)| k.0 != place || *k == key);
        self.failed.retain(|k| k.0 != place || *k == key);
        if self.known(key) {
            return;
        }
        let (tx, build) = (self.tx.clone(), self.build);
        let worker = thread::Builder::new().name("place-build".into());
        let spawned = worker.spawn(move || {
            let data = build_timed(build, place, set);
            // The builder going away first leaves nobody to want it.
            let _ = tx.send((key, data));
        });
        match spawned {
            Ok(worker) => self.working.push((key, worker)),
            Err(e) => {
                // No thread to be had: build here (a frame stalls for it)
                // rather than never show the place.
                log::warn!("no place worker ({e}): building {} here", place.id());
                self.ready.push((key, build_timed(self.build, place, set)));
            }
        }
    }

    /// Moves what workers have finished to `ready`, and notes the workers
    /// that died without finishing.
    fn collect(&mut self) {
        // Workers over before the results are read: a result sent before
        // its worker ended is then certainly in the channel.
        let over: Vec<Key> = self
            .working
            .iter()
            .filter(|(_, w)| w.is_finished())
            .map(|(k, _)| *k)
            .collect();
        while let Ok((key, data)) = self.rx.try_recv() {
            self.working.retain(|(k, _)| *k != key);
            if self.wanted(key) {
                self.ready.push((key, data));
            } else {
                log::debug!(
                    "place {} built for a seat that was left: dropped",
                    key.0.id()
                );
            }
        }
        for key in over {
            let Some(i) = self.working.iter().position(|(k, _)| *k == key) else {
                continue;
            };
            let (_, worker) = self.working.swap_remove(i);
            // It is over, so this does not wait.
            if worker.join().is_err() {
                log::warn!("building place {} failed", key.0.id());
            }
            if self.wanted(key) {
                self.failed.push(key);
            }
        }
    }

    /// `place` with `set` if it is built (once: it is handed over), else how
    /// it is getting on.
    pub fn take(&mut self, place: Place, set: Option<SeatName>) -> Built {
        let key = (place, set);
        self.collect();
        if let Some(i) = self.ready.iter().position(|(k, _)| *k == key) {
            return Built::Ready(self.ready.swap_remove(i).1);
        }
        if let Some(i) = self.failed.iter().position(|k| *k == key) {
            self.failed.swap_remove(i);
            return Built::Failed;
        }
        Built::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{Lighting, Sky};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    fn empty() -> PlaceData {
        let dark = [0.0; 3];
        PlaceData {
            vertices: Vec::new(),
            indices: vec![0, 1, 2],
            palette: Vec::new(),
            lighting: Lighting {
                ambient_up: dark,
                ambient_down: dark,
                house_rgb: dark,
                spill: 1.0,
                fog: None,
            },
            sky: Sky::None,
            baked: None,
        }
    }

    /// Polls until `place` is no longer pending (or five seconds pass).
    fn wait(b: &mut PlaceBuilder, place: Place) -> Built {
        wait_set(b, place, None)
    }

    fn wait_set(b: &mut PlaceBuilder, place: Place, set: Option<SeatName>) -> Built {
        for _ in 0..1000 {
            match b.take(place, set) {
                Built::Pending => thread::sleep(Duration::from_millis(5)),
                done => return done,
            }
        }
        Built::Pending
    }

    static COUNTED: AtomicUsize = AtomicUsize::new(0);

    fn counted(_: Place, _: Option<SeatName>) -> PlaceData {
        COUNTED.fetch_add(1, Ordering::SeqCst);
        empty()
    }

    #[test]
    fn a_place_is_built_once_however_often_it_is_asked_for() {
        let mut b = PlaceBuilder::with(counted);
        for _ in 0..5 {
            b.request(Place::Cinema, None);
        }
        assert!(matches!(wait(&mut b, Place::Cinema), Built::Ready(_)));
        // Handed over once; the renderer asks no more once it is loaded.
        assert!(matches!(b.take(Place::Cinema, None), Built::Pending));
        assert_eq!(COUNTED.load(Ordering::SeqCst), 1);
    }

    static GO: AtomicBool = AtomicBool::new(false);

    fn held(_: Place, _: Option<SeatName>) -> PlaceData {
        while !GO.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        empty()
    }

    #[test]
    fn asking_does_not_wait_for_the_build() {
        let mut b = PlaceBuilder::with(held);
        b.request(Place::Studio, None);
        // The worker is held, so this is the render thread not waiting.
        assert!(matches!(b.take(Place::Studio, None), Built::Pending));
        assert!(matches!(b.take(Place::Cinema, None), Built::Pending));
        GO.store(true, Ordering::SeqCst);
        assert!(matches!(wait(&mut b, Place::Studio), Built::Ready(_)));
    }

    fn broken(place: Place, _: Option<SeatName>) -> PlaceData {
        assert_ne!(place, Place::Cinema, "this place can't be built");
        empty()
    }

    #[test]
    fn a_worker_that_dies_is_reported_not_waited_for() {
        let mut b = PlaceBuilder::with(broken);
        b.request(Place::Cinema, None);
        b.request(Place::Studio, None);
        assert!(matches!(wait(&mut b, Place::Cinema), Built::Failed));
        assert!(matches!(wait(&mut b, Place::Studio), Built::Ready(_)));
    }

    /// A build that says which set it was made for, in its triangle count
    /// (front 1, middle 2, back 3, none 9).
    fn tagged(_: Place, set: Option<SeatName>) -> PlaceData {
        let n = match set {
            Some(SeatName::Front) => 1,
            Some(SeatName::Middle) => 2,
            Some(SeatName::Back) => 3,
            _ => 9,
        };
        let mut data = empty();
        data.indices = vec![0; 3 * n];
        data
    }

    fn triangles(built: Built) -> usize {
        match built {
            Built::Ready(data) => data.indices.len() / 3,
            other => panic!("not ready: {other:?}"),
        }
    }

    #[test]
    fn each_seats_set_is_built_and_handed_over_as_its_own() {
        let mut b = PlaceBuilder::with(tagged);
        for set in [SeatName::Front, SeatName::Back] {
            b.request(Place::Cinema, Some(set));
            let got = triangles(wait_set(&mut b, Place::Cinema, Some(set)));
            assert_eq!(got, if set == SeatName::Front { 1 } else { 3 });
        }
        // The set that is not asked for is not there.
        assert!(matches!(
            b.take(Place::Cinema, Some(SeatName::Middle)),
            Built::Pending
        ));
    }

    #[test]
    fn a_set_built_for_a_seat_that_was_left_is_dropped() {
        let mut b = PlaceBuilder::with(held_then_tagged);
        // The viewer sits in the front, then moves on to the back before the
        // front's set is done.
        b.request(Place::Cinema, Some(SeatName::Front));
        b.request(Place::Cinema, Some(SeatName::Back));
        GO_SEATS.store(true, Ordering::SeqCst);
        assert_eq!(
            triangles(wait_set(&mut b, Place::Cinema, Some(SeatName::Back))),
            3
        );
        // Both workers are over by now; the front's set never turns up.
        thread::sleep(Duration::from_millis(100));
        assert!(matches!(
            b.take(Place::Cinema, Some(SeatName::Front)),
            Built::Pending
        ));
        assert!(b.ready.is_empty() && b.working.is_empty());
    }

    static GO_SEATS: AtomicBool = AtomicBool::new(false);

    fn held_then_tagged(place: Place, set: Option<SeatName>) -> PlaceData {
        while !GO_SEATS.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        tagged(place, set)
    }

    #[test]
    fn going_back_to_a_seat_asks_again_only_when_its_set_is_gone() {
        let mut b = PlaceBuilder::with(tagged);
        b.request(Place::Cinema, Some(SeatName::Front));
        assert_eq!(
            triangles(wait_set(&mut b, Place::Cinema, Some(SeatName::Front))),
            1
        );
        // Handed over, so it is built again when the viewer returns to it.
        b.request(Place::Cinema, Some(SeatName::Middle));
        assert_eq!(
            triangles(wait_set(&mut b, Place::Cinema, Some(SeatName::Middle))),
            2
        );
        b.request(Place::Cinema, Some(SeatName::Front));
        assert_eq!(
            triangles(wait_set(&mut b, Place::Cinema, Some(SeatName::Front))),
            1
        );
    }

    #[test]
    fn the_real_places_build_the_same_off_thread() {
        let mut b = PlaceBuilder::new();
        for &place in Place::ALL {
            b.request(place, None);
        }
        for &place in Place::ALL {
            let Built::Ready(data) = wait(&mut b, place) else {
                panic!("{} was not built", place.id());
            };
            let direct = place.build();
            assert_eq!(data.indices, direct.indices, "{}", place.id());
            let (got, want) = (data.vertices.len(), direct.vertices.len());
            assert_eq!(got, want, "{}", place.id());
        }
    }
}
