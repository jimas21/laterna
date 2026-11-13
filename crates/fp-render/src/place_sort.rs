//! Draws a pack's triangles front to back.
//!
//! A baked place is one mesh of tens of thousands of triangles drawn in the
//! order the exporter wrote them, behind and in front of each other alike.
//! The fragment shader reads ten textures, so every hidden fragment that
//! gets shaded before a nearer triangle covers it is wasted work, at 2160
//! pixels square an eye. Drawn nearest first, the depth test (and on a tiler
//! its low-resolution depth pass) rejects what is hidden before it is shaded.
//!
//! The order depends on where the viewer is, but only coarsely: a seat is
//! metres from the next one, and a head moves well under a metre within one.
//! So the order is made for an eye position, off the render thread, and made
//! again when the eye is more than [`RESORT_DISTANCE`] from the one it was
//! made for (a seat change, a teleport). Until a new order is ready the old
//! one is drawn: any order draws the same picture, a bad one only costs time.
//!
//! No GPU is touched here, so all of it is unit tested without a device.

use crate::env::pack::{Baked, BakedVertex};
use glam::Vec3;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

/// How far (metres) the eye may move from where the order was made before it
/// is made again.
pub(crate) const RESORT_DISTANCE: f32 = 1.0;

/// Whether the order made for `made_for` (none yet: `None`) is stale for an
/// eye at `eye`. A position that is not a number never asks for one.
pub(crate) fn needs_order(made_for: Option<Vec3>, eye: Vec3) -> bool {
    if !eye.is_finite() {
        return false;
    }
    made_for.is_none_or(|at| at.distance(eye) > RESORT_DISTANCE)
}

/// A mesh's triangles reduced to what sorting needs: where each one is, and
/// its indices as the pack wrote them.
pub(crate) struct Triangles {
    centroids: Vec<Vec3>,
    indices: Vec<u32>,
}

impl Triangles {
    /// The triangles of `indices` (three to a triangle; a ragged end is left
    /// out) over `vertices`. An index past the vertices reads as the origin:
    /// the loader rejects such packs, this just does not panic.
    pub fn new(vertices: &[BakedVertex], indices: &[u32]) -> Triangles {
        let at = |i: u32| {
            vertices
                .get(i as usize)
                .map_or(Vec3::ZERO, |v| Vec3::from(v.pos))
        };
        let whole = indices.len() / 3 * 3;
        let indices = indices[..whole].to_vec();
        let centroids = indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| (at(t[0]) + at(t[1]) + at(t[2])) / 3.0)
            .collect();
        Triangles { centroids, indices }
    }

    pub fn len(&self) -> usize {
        self.centroids.len()
    }

    /// The indices with the triangles nearest `eye` first (by the distance
    /// of their centroids). Every triangle is kept, whole and wound as it
    /// was; ties keep the pack's order, so the result is the same every time
    /// for the same eye.
    pub fn sorted(&self, eye: Vec3) -> Vec<u32> {
        let mut order: Vec<(f32, u32)> = self
            .centroids
            .iter()
            .enumerate()
            .map(|(i, c)| (c.distance_squared(eye), i as u32))
            .collect();
        order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut out = Vec::with_capacity(self.indices.len());
        for (_, t) in order {
            let at = t as usize * 3;
            out.extend_from_slice(&self.indices[at..at + 3]);
        }
        out
    }
}

/// A finished order.
pub(crate) struct Sorted {
    /// The place it is for (see [`PlaceSorter::load`]).
    pub generation: u64,
    /// The eye it was made for.
    pub eye: Vec3,
    pub indices: Vec<u32>,
}

enum Job {
    Load(u64, Arc<Baked>),
    Sort(u64, Vec3),
}

/// A worker thread that sorts the triangles of the pack that is loaded.
pub(crate) struct PlaceSorter {
    jobs: Sender<Job>,
    done: Receiver<Sorted>,
    // Ends when `jobs` is dropped; never joined (a sort in progress is
    // nobody's business once the renderer is gone).
    _worker: JoinHandle<()>,
}

impl PlaceSorter {
    /// `None` when no thread can be had (the place is then drawn unsorted).
    pub fn new() -> Option<PlaceSorter> {
        let (jobs, jobs_rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("place-sort".into())
            .spawn(move || work(jobs_rx, done_tx))
            .map_err(|e| log::warn!("no place sort worker ({e}): the place is drawn unsorted"))
            .ok()?;
        Some(PlaceSorter {
            jobs,
            done,
            _worker: worker,
        })
    }

    /// Hands over the pack loaded as generation `generation`: the worker
    /// keeps only its triangles, not its textures.
    pub fn load(&self, generation: u64, baked: Arc<Baked>) {
        let _ = self.jobs.send(Job::Load(generation, baked));
    }

    /// Asks for the order of generation `generation`'s triangles for `eye`.
    /// Asks made while one is being sorted are collapsed into the newest.
    pub fn sort(&self, generation: u64, eye: Vec3) {
        let _ = self.jobs.send(Job::Sort(generation, eye));
    }

    /// The oldest finished order not yet taken.
    pub fn poll(&self) -> Option<Sorted> {
        self.done.try_recv().ok()
    }
}

fn work(jobs: Receiver<Job>, done: Sender<Sorted>) {
    let mut held: Option<(u64, Triangles)> = None;
    while let Ok(first) = jobs.recv() {
        // Everything queued behind it too: loads in order, sorts only the
        // newest (an older eye is not worth the time).
        let mut batch = vec![first];
        batch.extend(jobs.try_iter());
        let mut wanted = None;
        for job in batch {
            match job {
                Job::Load(generation, baked) => {
                    held = Some((generation, Triangles::new(&baked.vertices, &baked.indices)));
                    wanted = None;
                }
                Job::Sort(generation, eye) => wanted = Some((generation, eye)),
            }
        }
        let Some((generation, eye)) = wanted else {
            continue;
        };
        let Some((held_generation, triangles)) = held.as_ref() else {
            continue;
        };
        if *held_generation != generation {
            continue;
        }
        let start = Instant::now();
        let indices = triangles.sorted(eye);
        log::info!(
            "sorted {} place triangles front to back in {:.1} ms",
            triangles.len(),
            start.elapsed().as_secs_f64() * 1e3
        );
        if done
            .send(Sorted {
                generation,
                eye,
                indices,
            })
            .is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn vertex(x: f32, y: f32, z: f32) -> BakedVertex {
        BakedVertex {
            pos: [x, y, z],
            uv: [0.0, 0.0],
        }
    }

    /// `n` small triangles in a row along +z, the first nearest the origin;
    /// written in the order given by `order` (indices into the row).
    fn row(order: &[usize]) -> (Vec<BakedVertex>, Vec<u32>) {
        let n = order.iter().max().map_or(0, |m| m + 1);
        let mut v = Vec::new();
        for k in 0..n {
            let z = k as f32;
            v.extend([
                vertex(0.0, 0.0, z),
                vertex(1.0, 0.0, z),
                vertex(0.0, 1.0, z),
            ]);
        }
        let mut idx = Vec::new();
        for &k in order {
            idx.extend([3 * k as u32, 3 * k as u32 + 1, 3 * k as u32 + 2]);
        }
        (v, idx)
    }

    fn triangles_of(indices: &[u32]) -> Vec<[u32; 3]> {
        indices.as_chunks::<3>().0.to_vec()
    }

    #[test]
    fn the_nearest_triangle_comes_first() {
        let (v, idx) = row(&[3, 1, 4, 0, 2]);
        let t = Triangles::new(&v, &idx);
        // From the near end: 0, 1, 2, 3, 4. From the far end: reversed.
        let near = triangles_of(&t.sorted(Vec3::new(0.3, 0.3, -5.0)));
        let firsts: Vec<u32> = near.iter().map(|t| t[0] / 3).collect();
        assert_eq!(firsts, [0, 1, 2, 3, 4]);
        let far = triangles_of(&t.sorted(Vec3::new(0.3, 0.3, 50.0)));
        let firsts: Vec<u32> = far.iter().map(|t| t[0] / 3).collect();
        assert_eq!(firsts, [4, 3, 2, 1, 0]);
    }

    #[test]
    fn every_triangle_is_kept_whole_and_wound_as_it_was() {
        // Some rotated windings, and a repeated triangle.
        let (v, mut idx) = row(&[0, 1, 2, 3]);
        idx[3..6].rotate_left(1);
        idx.extend_from_slice(&[idx[0], idx[1], idx[2]]);
        let t = Triangles::new(&v, &idx);
        for eye in [Vec3::ZERO, Vec3::new(9.0, -3.0, 2.5), Vec3::splat(100.0)] {
            let out = t.sorted(eye);
            assert_eq!(out.len(), idx.len());
            let mut want = triangles_of(&idx);
            let mut got = triangles_of(&out);
            want.sort();
            got.sort();
            assert_eq!(got, want, "{eye}");
        }
    }

    #[test]
    fn ties_keep_the_packs_order_so_the_result_is_the_same_every_time() {
        // Two triangles the same distance from the eye, either way round.
        let mut v = vec![];
        for z in [-1.0f32, 1.0] {
            v.extend([
                vertex(0.0, 0.0, z),
                vertex(0.0, 0.0, z),
                vertex(0.0, 0.0, z),
            ]);
        }
        let t = Triangles::new(&v, &[3, 4, 5, 0, 1, 2]);
        assert_eq!(t.sorted(Vec3::ZERO), [3, 4, 5, 0, 1, 2]);
        let t = Triangles::new(&v, &[0, 1, 2, 3, 4, 5]);
        assert_eq!(t.sorted(Vec3::ZERO), [0, 1, 2, 3, 4, 5]);
        assert_eq!(t.sorted(Vec3::ZERO), t.sorted(Vec3::ZERO));
    }

    #[test]
    fn a_ragged_end_and_a_wild_index_do_not_panic() {
        let (v, mut idx) = row(&[0, 1]);
        idx.push(0);
        idx.push(1);
        let t = Triangles::new(&v, &idx);
        assert_eq!(t.len(), 2);
        assert_eq!(t.sorted(Vec3::ZERO).len(), 6);
        let t = Triangles::new(&v, &[0, 1, 2, 1000, 1, 2]);
        assert_eq!(t.sorted(Vec3::ZERO).len(), 6);
        let t = Triangles::new(&[], &[]);
        assert!(t.sorted(Vec3::ZERO).is_empty());
    }

    #[test]
    fn a_seat_change_asks_for_a_new_order_and_a_head_turn_does_not() {
        let seat = Vec3::new(0.0, 1.2, 6.0);
        assert!(needs_order(None, seat));
        assert!(!needs_order(Some(seat), seat + Vec3::new(0.3, 0.2, -0.4)));
        assert!(needs_order(Some(seat), Vec3::new(0.0, 1.2, 2.0)));
        // Not a number: never.
        assert!(!needs_order(None, Vec3::NAN));
        assert!(!needs_order(Some(seat), Vec3::INFINITY));
    }

    fn wait(s: &PlaceSorter) -> Option<Sorted> {
        for _ in 0..1000 {
            if let Some(done) = s.poll() {
                return Some(done);
            }
            thread::sleep(Duration::from_millis(5));
        }
        None
    }

    fn baked_row(order: &[usize]) -> Arc<Baked> {
        let (vertices, indices) = row(order);
        let none = || crate::env::pack::TextureArray {
            size: 4,
            encoding: crate::env::ktx2::Encoding::Rgba8,
            layers: Vec::new(),
        };
        Arc::new(Baked {
            vertices,
            indices,
            colors: none(),
            basis: none(),
            basis_gain: 1.0,
            seat: None,
        })
    }

    #[test]
    fn the_worker_sorts_the_loaded_place_off_thread() {
        let s = PlaceSorter::new().expect("a thread");
        s.load(7, baked_row(&[2, 0, 1]));
        s.sort(7, Vec3::new(0.0, 0.0, -10.0));
        let done = wait(&s).expect("sorted");
        assert_eq!(done.generation, 7);
        assert_eq!(done.eye, Vec3::new(0.0, 0.0, -10.0));
        assert_eq!(done.indices, [0, 1, 2, 3, 4, 5, 6, 7, 8]);
        // From the other side.
        s.sort(7, Vec3::new(0.0, 0.0, 10.0));
        assert_eq!(
            wait(&s).expect("sorted").indices,
            [6, 7, 8, 3, 4, 5, 0, 1, 2]
        );
    }

    #[test]
    fn a_sort_for_a_place_that_is_gone_is_dropped_and_the_newest_ask_is_answered() {
        let s = PlaceSorter::new().expect("a thread");
        s.load(1, baked_row(&[0, 1]));
        s.load(2, baked_row(&[0, 1, 2]));
        s.sort(1, Vec3::ZERO);
        s.sort(2, Vec3::new(0.0, 0.0, 99.0));
        s.sort(2, Vec3::new(0.0, 0.0, -99.0));
        // Older asks may or may not run before the newest is seen; none is
        // for the place that was replaced, and the newest is answered.
        let mut last = None;
        while let Some(done) = wait(&s) {
            assert_eq!(done.generation, 2);
            assert_eq!(done.indices.len(), 9);
            let newest = done.eye.z == -99.0;
            last = Some(done);
            if newest {
                break;
            }
        }
        assert_eq!(last.expect("answered").indices, [0, 1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
