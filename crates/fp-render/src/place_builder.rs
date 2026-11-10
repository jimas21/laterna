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

use crate::env::{Place, PlaceData};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

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

/// Makes `place` with `build`, saying how long it took.
pub(crate) fn build_timed(build: fn(Place) -> PlaceData, place: Place) -> PlaceData {
    let start = Instant::now();
    let data = build(place);
    let indices = data
        .baked
        .as_ref()
        .map_or(data.indices.len(), |b| b.indices.len());
    log::info!(
        "built place {}: {} triangles in {:.1} ms",
        place.id(),
        indices / 3,
        start.elapsed().as_secs_f64() * 1e3
    );
    data
}

/// Places being built, built and waiting to be taken.
pub(crate) struct PlaceBuilder {
    build: fn(Place) -> PlaceData,
    tx: Sender<(Place, PlaceData)>,
    rx: Receiver<(Place, PlaceData)>,
    /// Workers that have not handed their place over yet.
    working: Vec<(Place, JoinHandle<()>)>,
    ready: Vec<(Place, PlaceData)>,
    failed: Vec<Place>,
}

impl PlaceBuilder {
    pub fn new() -> PlaceBuilder {
        PlaceBuilder::with(Place::build)
    }

    /// Builds with `build`: for tests.
    pub fn with(build: fn(Place) -> PlaceData) -> PlaceBuilder {
        let (tx, rx) = mpsc::channel();
        PlaceBuilder {
            build,
            tx,
            rx,
            working: Vec::new(),
            ready: Vec::new(),
            failed: Vec::new(),
        }
    }

    fn known(&self, place: Place) -> bool {
        self.working.iter().any(|(p, _)| *p == place)
            || self.ready.iter().any(|(p, _)| *p == place)
            || self.failed.contains(&place)
    }

    /// Starts building `place` unless it is already being built, built, or
    /// failed. Cheap to call every frame.
    pub fn request(&mut self, place: Place) {
        if self.known(place) {
            return;
        }
        let (tx, build) = (self.tx.clone(), self.build);
        let worker = thread::Builder::new().name("place-build".into());
        let spawned = worker.spawn(move || {
            let data = build_timed(build, place);
            // The builder going away first leaves nobody to want it.
            let _ = tx.send((place, data));
        });
        match spawned {
            Ok(worker) => self.working.push((place, worker)),
            Err(e) => {
                // No thread to be had: build here (a frame stalls for it)
                // rather than never show the place.
                log::warn!("no place worker ({e}): building {} here", place.id());
                self.ready.push((place, build_timed(self.build, place)));
            }
        }
    }

    /// Moves what workers have finished to `ready`, and notes the workers
    /// that died without finishing.
    fn collect(&mut self) {
        // Workers over before the results are read: a result sent before
        // its worker ended is then certainly in the channel.
        let over: Vec<Place> = self
            .working
            .iter()
            .filter(|(_, w)| w.is_finished())
            .map(|(p, _)| *p)
            .collect();
        while let Ok((place, data)) = self.rx.try_recv() {
            self.working.retain(|(p, _)| *p != place);
            self.ready.push((place, data));
        }
        for place in over {
            let Some(i) = self.working.iter().position(|(p, _)| *p == place) else {
                continue;
            };
            let (_, worker) = self.working.swap_remove(i);
            // It is over, so this does not wait.
            if worker.join().is_err() {
                log::warn!("building place {} failed", place.id());
            }
            self.failed.push(place);
        }
    }

    /// `place` if it is built (once: it is handed over), else how it is
    /// getting on.
    pub fn take(&mut self, place: Place) -> Built {
        self.collect();
        if let Some(i) = self.ready.iter().position(|(p, _)| *p == place) {
            return Built::Ready(self.ready.swap_remove(i).1);
        }
        if let Some(i) = self.failed.iter().position(|p| *p == place) {
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
        for _ in 0..1000 {
            match b.take(place) {
                Built::Pending => thread::sleep(Duration::from_millis(5)),
                done => return done,
            }
        }
        Built::Pending
    }

    static COUNTED: AtomicUsize = AtomicUsize::new(0);

    fn counted(_: Place) -> PlaceData {
        COUNTED.fetch_add(1, Ordering::SeqCst);
        empty()
    }

    #[test]
    fn a_place_is_built_once_however_often_it_is_asked_for() {
        let mut b = PlaceBuilder::with(counted);
        for _ in 0..5 {
            b.request(Place::Cinema);
        }
        assert!(matches!(wait(&mut b, Place::Cinema), Built::Ready(_)));
        // Handed over once; the renderer asks no more once it is loaded.
        assert!(matches!(b.take(Place::Cinema), Built::Pending));
        assert_eq!(COUNTED.load(Ordering::SeqCst), 1);
    }

    static GO: AtomicBool = AtomicBool::new(false);

    fn held(_: Place) -> PlaceData {
        while !GO.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        empty()
    }

    #[test]
    fn asking_does_not_wait_for_the_build() {
        let mut b = PlaceBuilder::with(held);
        b.request(Place::Studio);
        // The worker is held, so this is the render thread not waiting.
        assert!(matches!(b.take(Place::Studio), Built::Pending));
        assert!(matches!(b.take(Place::Cinema), Built::Pending));
        GO.store(true, Ordering::SeqCst);
        assert!(matches!(wait(&mut b, Place::Studio), Built::Ready(_)));
    }

    fn broken(place: Place) -> PlaceData {
        assert_ne!(place, Place::Cinema, "this place can't be built");
        empty()
    }

    #[test]
    fn a_worker_that_dies_is_reported_not_waited_for() {
        let mut b = PlaceBuilder::with(broken);
        b.request(Place::Cinema);
        b.request(Place::Studio);
        assert!(matches!(wait(&mut b, Place::Cinema), Built::Failed));
        assert!(matches!(wait(&mut b, Place::Studio), Built::Ready(_)));
    }

    #[test]
    fn the_real_places_build_the_same_off_thread() {
        let mut b = PlaceBuilder::new();
        for &place in Place::ALL {
            b.request(place);
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
