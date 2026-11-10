//! Frame timing: cheap duration statistics for the frame loop on the CPU
//! side ([`Series`]), and how long each part of a frame kept the GPU busy
//! ([`GpuTimes`]), from timestamps written between the parts and read once
//! the frame's fence has signalled.

use crate::VkContext;
use crate::gpu::Gpu;
use ash::vk;
use std::fmt;
use std::time::Instant;

/// Samples kept per period. At 72 Hz and 10 s a call made once per frame
/// gives 720; more than this are counted and still count towards the max,
/// but are left out of the median.
const CAPACITY: usize = 2048;

/// Durations (or any millisecond values) collected over one period.
#[derive(Debug)]
pub struct Series {
    samples: Vec<f32>,
    max: f32,
    count: u64,
}

/// What a [`Series`] held when it was taken.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub median: f32,
    pub max: f32,
    /// Samples pushed, including any past the buffer's capacity.
    pub count: u64,
}

impl Default for Series {
    fn default() -> Self {
        Series {
            samples: Vec::with_capacity(CAPACITY),
            max: 0.0,
            count: 0,
        }
    }
}

impl Series {
    pub fn new() -> Series {
        Series::default()
    }

    /// Adds a value.
    pub fn push(&mut self, value: f32) {
        self.max = if self.count == 0 {
            value
        } else {
            self.max.max(value)
        };
        self.count += 1;
        if self.samples.len() < CAPACITY {
            self.samples.push(value);
        }
    }

    /// Adds the milliseconds since `start`.
    pub fn push_since(&mut self, start: Instant) {
        self.push(start.elapsed().as_secs_f32() * 1000.0);
    }

    /// Median, max and count of what was pushed since the last call, then
    /// starts over. `None` when nothing was pushed.
    pub fn take(&mut self) -> Option<Summary> {
        if self.count == 0 {
            return None;
        }
        self.samples.sort_unstable_by(f32::total_cmp);
        let summary = Summary {
            median: self.samples[(self.samples.len() - 1) / 2],
            max: self.max,
            count: self.count,
        };
        self.samples.clear();
        self.max = 0.0;
        self.count = 0;
        Some(summary)
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1}/{:.1}", self.median, self.max)
    }
}

/// Formats a summary as `median/max`, or `-` when there was none.
pub fn show(summary: Option<Summary>) -> String {
    match summary {
        Some(s) => s.to_string(),
        None => "-".to_string(),
    }
}

/// Where a frame writes its timestamps, in the order its work runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Start,
    Video,
    VideoDone,
    Eyes,
    EyesDone,
    End,
    /// Around building the video pyramid, which runs inside the eye passes'
    /// span (before the first pass).
    Pyramid,
    PyramidDone,
}

const MARKS: usize = 8;

/// One frame's GPU time per part, in milliseconds. A part the frame didn't
/// have (no video, no eye passes) is `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuTimes {
    /// Painting the menus and panels.
    pub panels: Option<f32>,
    /// The video frame: an upload, a 10-bit conversion, HDR measuring.
    pub video: Option<f32>,
    /// Both eye passes (without the pyramid, which is timed on its own).
    pub eyes: Option<f32>,
    /// Building the video pyramid: only for a frame that built one.
    pub pyramid: Option<f32>,
    /// Copying panels into the compositor's layers.
    pub layers: Option<f32>,
    /// The whole frame.
    pub total: Option<f32>,
}

/// Parts from the ticks each mark was written at (`None`: not written),
/// for a counter of `bits` valid bits counting every `period_ns`.
fn parts(ticks: [Option<u64>; MARKS], bits: u32, period_ns: f32) -> GpuTimes {
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let ms = |from: Mark, to: Mark| {
        let (a, b) = (ticks[from as usize]?, ticks[to as usize]?);
        Some((b.wrapping_sub(a) & mask) as f32 * period_ns / 1e6)
    };
    let pyramid = ms(Mark::Pyramid, Mark::PyramidDone);
    // The pyramid is built after the eye passes' first mark, so it is taken out.
    let eyes = ms(Mark::Eyes, Mark::EyesDone);
    GpuTimes {
        panels: ms(Mark::Start, Mark::Video),
        video: ms(Mark::Video, Mark::VideoDone),
        eyes: eyes.map(|e| (e - pyramid.unwrap_or(0.0)).max(0.0)),
        pyramid,
        layers: ms(Mark::EyesDone, Mark::End),
        total: ms(Mark::Start, Mark::End),
    }
}

/// A timestamp query pool per frame slot.
pub(crate) struct FrameTimer {
    pools: Vec<vk::QueryPool>,
    /// Marks written into each slot's current command buffer.
    written: Vec<[bool; MARKS]>,
    bits: u32,
    period_ns: f32,
}

impl FrameTimer {
    /// A timer for `slots` frame slots, or `None` (logged) where the queue
    /// has no timestamps.
    pub fn new(gpu: &Gpu, slots: usize) -> Option<FrameTimer> {
        // SAFETY: valid physical device.
        let (families, limits) = unsafe {
            (
                gpu.instance
                    .get_physical_device_queue_family_properties(gpu.pdev),
                gpu.instance.get_physical_device_properties(gpu.pdev).limits,
            )
        };
        let bits = families
            .get(gpu.queue_family as usize)
            .map_or(0, |f| f.timestamp_valid_bits);
        if bits == 0 || limits.timestamp_period <= 0.0 {
            log::info!("GPU: no timestamps, frames aren't timed");
            return None;
        }
        let mut t = FrameTimer {
            pools: Vec::new(),
            written: vec![[false; MARKS]; slots],
            bits,
            period_ns: limits.timestamp_period,
        };
        for _ in 0..slots {
            // SAFETY: valid create info; destroyed by `destroy`.
            let pool = unsafe {
                gpu.device.create_query_pool(
                    &vk::QueryPoolCreateInfo::default()
                        .query_type(vk::QueryType::TIMESTAMP)
                        .query_count(MARKS as u32),
                    None,
                )
            }
            .ctx("timestamp query pool");
            match pool {
                Ok(p) => t.pools.push(p),
                Err(e) => {
                    log::warn!("GPU: frames aren't timed: {e}");
                    t.destroy(gpu);
                    return None;
                }
            }
        }
        Some(t)
    }

    /// Starts slot `slot`'s frame in `cmd`, which has just begun recording.
    pub fn begin(&mut self, gpu: &Gpu, cmd: vk::CommandBuffer, slot: usize) {
        let Some(&pool) = self.pools.get(slot) else {
            return;
        };
        // SAFETY: recording, outside any rendering; the slot's previous
        // submission has finished.
        unsafe { gpu.device.cmd_reset_query_pool(cmd, pool, 0, MARKS as u32) };
        self.written[slot] = [false; MARKS];
        self.stamp(gpu, cmd, slot, Mark::Start);
    }

    /// Writes `mark` once all work recorded before it has run; a mark
    /// already written this frame stays as it is.
    pub fn stamp(&mut self, gpu: &Gpu, cmd: vk::CommandBuffer, slot: usize, mark: Mark) {
        let Some(&pool) = self.pools.get(slot) else {
            return;
        };
        let written = &mut self.written[slot][mark as usize];
        if std::mem::replace(written, true) {
            return;
        }
        // SAFETY: recording, outside any rendering; the query was reset in
        // `begin` and isn't written twice.
        unsafe {
            gpu.device.cmd_write_timestamp2(
                cmd,
                vk::PipelineStageFlags2::ALL_COMMANDS,
                pool,
                mark as u32,
            );
        }
    }

    /// The parts of slot `slot`'s last submitted frame; once its fence has
    /// signalled, before `begin` starts its next one.
    pub fn read(&self, gpu: &Gpu, slot: usize) -> Option<GpuTimes> {
        let &pool = self.pools.get(slot)?;
        if self.written[slot] == [false; MARKS] {
            return None;
        }
        let mut data = [[0u64; 2]; MARKS];
        // SAFETY: the frame that wrote the queries has finished; each entry
        // is a value and its availability, as the flags ask.
        let r = unsafe {
            gpu.device.get_query_pool_results(
                pool,
                0,
                &mut data,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_AVAILABILITY,
            )
        };
        // Marks the frame didn't write are unavailable, which makes the
        // call report NOT_READY while still filling in the others.
        if let Err(e) = r
            && e != vk::Result::NOT_READY
        {
            return None;
        }
        let ticks = data.map(|[value, available]| (available != 0).then_some(value));
        Some(parts(ticks, self.bits, self.period_ns))
    }

    pub fn destroy(&mut self, gpu: &Gpu) {
        for pool in self.pools.drain(..) {
            // SAFETY: no submitted frame uses it (the device is idle).
            unsafe { gpu.device.destroy_query_pool(pool, None) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_series_has_no_summary() {
        assert_eq!(Series::new().take(), None);
        assert_eq!(show(None), "-");
    }

    #[test]
    fn median_and_max() {
        let mut s = Series::new();
        for v in [5.0, 1.0, 3.0, 9.0, 2.0] {
            s.push(v);
        }
        let t = s.take().unwrap();
        assert_eq!((t.median, t.max, t.count), (3.0, 9.0, 5));
        assert_eq!(t.to_string(), "3.0/9.0");
    }

    #[test]
    fn even_count_takes_the_lower_middle() {
        let mut s = Series::new();
        for v in [4.0, 1.0, 2.0, 3.0] {
            s.push(v);
        }
        assert_eq!(s.take().unwrap().median, 2.0);
    }

    #[test]
    fn take_resets() {
        let mut s = Series::new();
        s.push(7.0);
        assert!(s.take().is_some());
        assert_eq!(s.take(), None);
        s.push(2.0);
        let t = s.take().unwrap();
        assert_eq!((t.median, t.max, t.count), (2.0, 2.0, 1));
    }

    #[test]
    fn negative_values_keep_their_max() {
        let mut s = Series::new();
        s.push(-3.0);
        s.push(-1.0);
        assert_eq!(s.take().unwrap().max, -1.0);
    }

    #[test]
    fn overflow_counts_and_keeps_max_without_growing() {
        let mut s = Series::new();
        for i in 0..(CAPACITY + 10) {
            s.push(i as f32);
        }
        assert_eq!(s.samples.len(), CAPACITY);
        let t = s.take().unwrap();
        assert_eq!(t.count, (CAPACITY + 10) as u64);
        assert_eq!(t.max, (CAPACITY + 9) as f32);
    }

    #[test]
    fn push_since_is_not_negative() {
        let mut s = Series::new();
        s.push_since(Instant::now());
        assert!(s.take().unwrap().max >= 0.0);
    }

    /// Ticks for the marks up to `End`, the pyramid's two not written.
    fn at(ticks: [u64; 6]) -> [Option<u64>; MARKS] {
        let mut all = [None; MARKS];
        for (mark, tick) in all.iter_mut().zip(ticks) {
            *mark = Some(tick);
        }
        all
    }

    #[test]
    fn parts_are_the_gaps_between_marks() {
        // 52 ns a tick, as on Adreno: 19.2 MHz.
        let t = parts(at([0, 1000, 21_000, 21_000, 81_000, 85_000]), 64, 52.083);
        let near = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 1e-3);
        assert!(near(t.panels, 0.052), "{t:?}");
        assert!(near(t.video, 1.042), "{t:?}");
        assert!(near(t.eyes, 3.125), "{t:?}");
        assert!(near(t.layers, 0.208), "{t:?}");
        assert!(near(t.total, 4.427), "{t:?}");
    }

    #[test]
    fn the_pyramid_is_a_part_of_its_own_and_not_of_the_eyes() {
        let mut ticks = at([0, 1000, 21_000, 21_000, 81_000, 85_000]);
        let t = parts(ticks, 64, 52.083);
        assert_eq!(t.pyramid, None, "a frame that built none has no part");
        ticks[Mark::Pyramid as usize] = Some(21_000);
        ticks[Mark::PyramidDone as usize] = Some(31_000);
        let t = parts(ticks, 64, 52.083);
        let near = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 1e-3);
        assert!(near(t.pyramid, 0.521), "{t:?}");
        assert!(near(t.eyes, 2.604), "{t:?}");
        assert!(near(t.total, 4.427), "{t:?}");
    }

    #[test]
    fn a_missing_mark_leaves_only_its_parts_out() {
        let mut ticks = at([0, 10, 20, 20, 70, 80]);
        ticks[Mark::Eyes as usize] = None;
        ticks[Mark::EyesDone as usize] = None;
        let t = parts(ticks, 64, 1.0);
        assert_eq!((t.eyes, t.layers), (None, None));
        assert!(t.video.is_some() && t.total.is_some());
    }

    #[test]
    fn a_counter_that_wraps_still_gives_the_gap() {
        // 32 valid bits, wrapping between the marks.
        let start = (1u64 << 32) - 100;
        let t = parts(at([start, start, start, start, 50, 100]), 32, 1e6);
        assert_eq!(t.eyes, Some(150.0));
        assert_eq!(t.total, Some(200.0));
    }
}
