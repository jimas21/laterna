//! Counts the fragment shader invocations of the place's draw in each eye
//! pass, with a pipeline statistics query. Off unless a test or tool asks
//! ([`crate::Renderer::count_place_fragments`]): it needs the device feature
//! `pipelineStatisticsQuery`, which [`crate::gpu::GpuOptions`] turns on, and
//! the app does not.
//!
//! Dividing the count by the pixels the place covers gives the overdraw: how
//! many times a pixel's fragment shader ran, which is what a nearest-first
//! draw order is for. What the count means depends on the GPU: a tiler that
//! rejects hidden fragments before shading (Adreno's LRZ) runs fewer
//! invocations than one that tests depth after, and the number says nothing
//! of the time a fragment takes.

use crate::gpu::Gpu;
use crate::{Result, VkContext};
use ash::vk;

/// One query for each eye.
const EYES: u32 = 2;

pub(crate) struct FragmentCounter {
    pool: vk::QueryPool,
    /// Whether each eye's query was written by the last frame recorded.
    written: [bool; 2],
}

impl FragmentCounter {
    /// The query pool, or an error where the device lacks the feature.
    pub fn new(gpu: &Gpu) -> Result<FragmentCounter> {
        if !gpu.pipeline_statistics {
            return Err(crate::Error::Unsupported(
                "pipeline statistics queries are not enabled on this device".into(),
            ));
        }
        // SAFETY: valid create info; destroyed by `destroy`.
        let pool = unsafe {
            gpu.device.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::PIPELINE_STATISTICS)
                    .query_count(EYES)
                    .pipeline_statistics(
                        vk::QueryPipelineStatisticFlags::FRAGMENT_SHADER_INVOCATIONS,
                    ),
                None,
            )
        }
        .ctx("pipeline statistics query pool")?;
        Ok(FragmentCounter {
            pool,
            written: [false; 2],
        })
    }

    /// Resets `eye`'s query: before its pass begins, outside any rendering.
    pub fn reset(&mut self, gpu: &Gpu, cmd: vk::CommandBuffer, eye: usize) {
        // SAFETY: recording, outside rendering; the previous submission that
        // used the query has finished (the callers wait for the device).
        unsafe {
            gpu.device
                .cmd_reset_query_pool(cmd, self.pool, eye as u32, 1);
        }
        self.written[eye] = false;
    }

    /// Starts counting `eye`'s fragments, inside its pass.
    pub fn begin(&mut self, gpu: &Gpu, cmd: vk::CommandBuffer, eye: usize) {
        // SAFETY: recording; the query was reset by `reset` and is not active.
        unsafe {
            gpu.device
                .cmd_begin_query(cmd, self.pool, eye as u32, vk::QueryControlFlags::empty());
        }
        self.written[eye] = true;
    }

    /// Stops counting `eye`'s fragments.
    pub fn end(&mut self, gpu: &Gpu, cmd: vk::CommandBuffer, eye: usize) {
        // SAFETY: recording; the query was begun by `begin`.
        unsafe { gpu.device.cmd_end_query(cmd, self.pool, eye as u32) };
    }

    /// The counts of the last frame's eyes (`None` for an eye the frame did
    /// not count). The device must be idle.
    pub fn read(&self, gpu: &Gpu) -> [Option<u64>; 2] {
        let mut out = [None; 2];
        for (eye, slot) in out.iter_mut().enumerate() {
            if !self.written[eye] {
                continue;
            }
            let mut data = [0u64; 1];
            // SAFETY: the frame has finished; one 64-bit value is asked for.
            let r = unsafe {
                gpu.device.get_query_pool_results(
                    self.pool,
                    eye as u32,
                    &mut data,
                    vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
                )
            };
            if r.is_ok() {
                *slot = Some(data[0]);
            }
        }
        out
    }

    pub fn destroy(&mut self, gpu: &Gpu) {
        // SAFETY: no submitted frame uses it (the device is idle).
        unsafe { gpu.device.destroy_query_pool(self.pool, None) };
        self.pool = vk::QueryPool::null();
    }
}
