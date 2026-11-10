//! Session lifecycle, swapchains and the frame loop.

use crate::context::XrContext;
use crate::input::{Actions, InputState};
use crate::{Error, Result, XrContextExt};
use ash::vk::{self, Handle};
use fp_render::timing::Series;
use fp_render::{EyeTarget, EyeView, Gpu};
use glam::{Quat, Vec3};
use openxr as xr;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What happened while polling events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// Nothing that needs the app's attention.
    None,
    /// The session is running and frames should be produced.
    Running,
    /// The runtime asked us to stop (headset removed, app closed from the
    /// system menu): leave the loop.
    Exit,
}

/// Whether a session in `state` is in front of the user's eyes: visible, or
/// focused too. Taking the headset off moves it to synchronized (and on to
/// idle, stopping); SteamVR's dashboard over the app is only visible.
pub fn is_worn(state: xr::SessionState) -> bool {
    matches!(state, xr::SessionState::VISIBLE | xr::SessionState::FOCUSED)
}

struct Eye {
    swapchain: xr::Swapchain<xr::Vulkan>,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
}

/// A flat picture the compositor draws itself, at its own place in the
/// world (`XrCompositionLayerQuad`): UI panels and the pointer. The
/// compositor puts it at its pose for every displayed frame, so it stays put
/// as the head moves, where content in the projection layer is re-warped as
/// if it were far away.
struct QuadLayer {
    swapchain: xr::Swapchain<xr::Vulkan>,
    images: Vec<vk::Image>,
    width: u32,
    height: u32,
    /// Acquired this frame, to release before xrEndFrame.
    acquired: bool,
    /// An image has been released at least once: the layer can be shown.
    ready: bool,
}

/// A quad layer to show this frame.
#[derive(Clone, Copy, Debug)]
pub struct QuadSubmit {
    /// The key the layer was created under.
    pub key: u64,
    /// Centre and orientation in the play space; the picture faces +Z.
    pub pose: (Vec3, Quat),
    /// Width and height in metres.
    pub size: glam::Vec2,
}

/// Wall time, in milliseconds, of the runtime calls that can block. The app
/// takes and resets each series every few seconds.
#[derive(Debug, Default)]
pub struct XrStats {
    /// `xrWaitFrame`.
    pub wait_frame: Series,
    /// `xrBeginFrame`.
    pub begin_frame: Series,
    /// Both eye swapchains' acquire and wait, one sample per frame.
    pub eye_acquire: Series,
    /// One quad layer's acquire and wait, one sample per picture.
    pub quad_acquire: Series,
    /// `xrEndFrame`.
    pub end_frame: Series,
}

pub struct XrSession {
    ctx: Arc<XrContext>,
    gpu: Arc<Gpu>,
    pub session: xr::Session<xr::Vulkan>,
    waiter: xr::FrameWaiter,
    stream: xr::FrameStream<xr::Vulkan>,
    space: xr::Space,
    view_space: xr::Space,
    eyes: Vec<Eye>,
    pub extent: vk::Extent2D,
    pub format: vk::Format,
    actions: Actions,
    state: xr::SessionState,
    running: bool,
    pub blend_mode: xr::EnvironmentBlendMode,
    event_buf: xr::EventDataBuffer,
    /// When the system's recenter (holding the Steam button) moves the play
    /// space; see [`XrSession::recentred_by`].
    recenter_at: Option<xr::Time>,
    /// Dashboard closes left before the first focus, and when the last was;
    /// see [`XrSession::close_dashboard_at_launch`].
    launch_closes: u32,
    launch_closed_at: Option<Instant>,
    quads: std::collections::HashMap<u64, QuadLayer>,
    /// The first quad layer's image count is logged once.
    quad_images_logged: bool,
    pub stats: XrStats,
}

/// One frame in progress.
pub struct FrameCtx {
    pub state: xr::FrameState,
    /// Eye cameras for rendering (empty when the runtime says not to render).
    pub eyes: Option<[EyeView; 2]>,
    /// Head pose in the play space.
    pub head: Option<(Vec3, Quat)>,
    pub targets: Option<[EyeTarget; 2]>,
    views: Vec<xr::View>,
}

/// Preferred eye formats, sRGB first.
const FORMATS: [vk::Format; 2] = [vk::Format::R8G8B8A8_SRGB, vk::Format::B8G8R8A8_SRGB];

impl XrSession {
    /// Creates the session on `gpu` (which must have been created with
    /// `ctx.creator()`).
    pub fn new(ctx: Arc<XrContext>, gpu: Arc<Gpu>) -> Result<XrSession> {
        let mut actions = Actions::new(&ctx.instance)?;
        let info = xr::vulkan::SessionCreateInfo {
            instance: gpu.instance.handle().as_raw() as _,
            physical_device: gpu.pdev.as_raw() as _,
            device: gpu.device.handle().as_raw() as _,
            queue_family_index: gpu.queue_family,
            queue_index: 0,
        };
        // SAFETY: the Vulkan objects were created through this runtime and
        // outlive the session (the app drops the session first).
        let (session, waiter, stream) =
            unsafe { ctx.instance.create_session::<xr::Vulkan>(ctx.system, &info) }
                .ctx("xrCreateSession")?;
        actions.attach(&session)?;
        // STAGE, as games use. With LOCAL SteamVR switches to seated
        // tracking, and the dashboard opened at Quit came up far too high.
        let base = if session
            .enumerate_reference_spaces()
            .ctx("xrEnumerateReferenceSpaces")?
            .contains(&xr::ReferenceSpaceType::STAGE)
        {
            xr::ReferenceSpaceType::STAGE
        } else {
            xr::ReferenceSpaceType::LOCAL
        };
        let space = session
            .create_reference_space(base, xr::Posef::IDENTITY)
            .ctx("play space")?;
        log::info!("play space {base:?}");
        let view_space = session
            .create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)
            .ctx("view space")?;
        let formats = session
            .enumerate_swapchain_formats()
            .ctx("xrEnumerateSwapchainFormats")?;
        let format = FORMATS
            .iter()
            .copied()
            .find(|f| formats.contains(&(f.as_raw() as u32)))
            .ok_or_else(|| Error::Runtime(format!("no sRGB swapchain format among {formats:?}")))?;
        let (w, h) = ctx.eye_size()?;
        let mut eyes = Vec::new();
        for _ in 0..2 {
            let swapchain = session
                .create_swapchain(&xr::SwapchainCreateInfo {
                    create_flags: xr::SwapchainCreateFlags::EMPTY,
                    usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                        | xr::SwapchainUsageFlags::SAMPLED,
                    format: format.as_raw() as u32,
                    sample_count: 1,
                    width: w,
                    height: h,
                    face_count: 1,
                    array_size: 1,
                    mip_count: 1,
                })
                .ctx("xrCreateSwapchain")?;
            let images: Vec<vk::Image> = swapchain
                .enumerate_images()
                .ctx("xrEnumerateSwapchainImages")?
                .into_iter()
                .map(vk::Image::from_raw)
                .collect();
            let mut views = Vec::new();
            for &img in &images {
                views.push(fp_render::mem::view(&gpu, img, format, 0, 1)?);
            }
            eyes.push(Eye {
                swapchain,
                images,
                views,
            });
        }
        let blend_mode = ctx
            .blend_modes
            .first()
            .copied()
            .unwrap_or(xr::EnvironmentBlendMode::OPAQUE);
        log::info!("XR session: {w}x{h} per eye, {format:?}, blend {blend_mode:?}");
        let counts: Vec<usize> = eyes.iter().map(|e| e.images.len()).collect();
        log::info!("eye swapchain images: {counts:?}");
        Ok(XrSession {
            ctx,
            gpu,
            session,
            waiter,
            stream,
            space,
            view_space,
            eyes,
            extent: vk::Extent2D {
                width: w,
                height: h,
            },
            format,
            actions,
            state: xr::SessionState::UNKNOWN,
            running: false,
            blend_mode,
            event_buf: xr::EventDataBuffer::new(),
            recenter_at: None,
            launch_closes: 3,
            launch_closed_at: None,
            quads: std::collections::HashMap::new(),
            quad_images_logged: false,
            stats: XrStats::default(),
        })
    }

    /// Display refresh rates the app may request (`XR_FB_display_refresh_rate`),
    /// or `None` when the extension is not enabled or the call fails.
    pub fn refresh_rates(&self) -> Option<Vec<f32>> {
        if !self.ctx.enabled.fb_display_refresh_rate {
            return None;
        }
        self.session.enumerate_display_refresh_rates().ok()
    }

    /// The display's current refresh rate, or `None` as for
    /// [`XrSession::refresh_rates`].
    pub fn refresh_rate(&self) -> Option<f32> {
        if !self.ctx.enabled.fb_display_refresh_rate {
            return None;
        }
        self.session.get_display_refresh_rate().ok()
    }

    /// Asks the runtime to switch the display to `hz` (0 means the system's
    /// default). Fails when `XR_FB_display_refresh_rate` is not enabled or
    /// the runtime refuses the rate.
    pub fn request_refresh_rate(&self, hz: f32) -> Result<()> {
        if !self.ctx.enabled.fb_display_refresh_rate {
            return Err(Error::Runtime(
                "the runtime lacks XR_FB_display_refresh_rate (not enabled)".into(),
            ));
        }
        self.session
            .request_display_refresh_rate(hz)
            .ctx("xrRequestDisplayRefreshRateFB")
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn state(&self) -> xr::SessionState {
        self.state
    }

    /// Whether the app is in front of the user's eyes (see [`is_worn`]).
    pub fn headset_on(&self) -> bool {
        is_worn(self.state)
    }

    /// Whether the runtime supports a blend mode showing the real world.
    pub fn supports_passthrough_blend(&self) -> bool {
        self.ctx
            .blend_modes
            .contains(&xr::EnvironmentBlendMode::ALPHA_BLEND)
    }

    pub fn set_passthrough(&mut self, on: bool) {
        self.blend_mode = if on && self.supports_passthrough_blend() {
            xr::EnvironmentBlendMode::ALPHA_BLEND
        } else {
            xr::EnvironmentBlendMode::OPAQUE
        };
    }

    /// Asks the runtime to end the session; [`XrSession::poll`] reports
    /// [`SessionEvent::Exit`] once it has.
    pub fn request_exit(&self) {
        if self.running
            && let Err(e) = self.session.request_exit()
        {
            log::warn!("xrRequestExitSession: {e}");
        }
    }

    /// Closes SteamVR's dashboard while it covers the app before the app's
    /// first focus, 3 times at most, 2 s apart: Steam leaves it, with its
    /// game menu, in front of a non-Steam shortcut it starts, where SteamVR
    /// closes it for a store game. Call once per loop iteration.
    pub fn close_dashboard_at_launch(&mut self) {
        match self.state {
            xr::SessionState::FOCUSED => self.launch_closes = 0,
            xr::SessionState::VISIBLE
                if self.launch_closes > 0
                    && self
                        .launch_closed_at
                        .is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) =>
            {
                self.ctx.hide_steamvr_dashboard();
                self.launch_closes -= 1;
                self.launch_closed_at = Some(Instant::now());
            }
            _ => {}
        }
    }

    /// Processes runtime events. Call once per loop iteration.
    pub fn poll(&mut self) -> Result<SessionEvent> {
        let mut result = SessionEvent::None;
        while let Some(event) = self
            .ctx
            .instance
            .poll_event(&mut self.event_buf)
            .ctx("xrPollEvent")?
        {
            match event {
                xr::Event::SessionStateChanged(e) => {
                    self.state = e.state();
                    log::info!("XR session state: {:?}", self.state);
                    match self.state {
                        xr::SessionState::READY => {
                            self.session
                                .begin(xr::ViewConfigurationType::PRIMARY_STEREO)
                                .ctx("xrBeginSession")?;
                            self.running = true;
                        }
                        xr::SessionState::STOPPING => {
                            self.session.end().ctx("xrEndSession")?;
                            self.running = false;
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => {
                            return Ok(SessionEvent::Exit);
                        }
                        _ => {}
                    }
                }
                xr::Event::ReferenceSpaceChangePending(e)
                    if matches!(
                        e.reference_space_type(),
                        xr::ReferenceSpaceType::STAGE | xr::ReferenceSpaceType::LOCAL
                    ) =>
                {
                    log::info!("system recenter");
                    self.recenter_at = Some(e.change_time());
                }
                xr::Event::DisplayRefreshRateChangedFB(_) => {
                    let now = self.session.get_display_refresh_rate().ok();
                    log::info!("display refresh rate changed: now {now:?} Hz");
                }
                xr::Event::InstanceLossPending(_) => return Ok(SessionEvent::Exit),
                _ => {}
            }
        }
        if self.running {
            result = SessionEvent::Running;
        }
        Ok(result)
    }

    /// Whether the system recentered and the moved play space is in use by
    /// `time` (a frame's predicted display time); true once per recenter.
    /// The space stays level, so it only turns to where the head faces.
    pub fn recentred_by(&mut self, time: xr::Time) -> bool {
        match self.recenter_at {
            Some(at) if time.as_nanos() >= at.as_nanos() => {
                self.recenter_at = None;
                true
            }
            _ => false,
        }
    }

    /// Waits for the next frame, locates the views and acquires swapchain
    /// images. Call [`XrSession::end_frame`] after submitting GPU work.
    pub fn begin_frame(&mut self) -> Result<FrameCtx> {
        let t = Instant::now();
        let state = self.waiter.wait().ctx("xrWaitFrame")?;
        self.stats.wait_frame.push_since(t);
        let t = Instant::now();
        self.stream.begin().ctx("xrBeginFrame")?;
        self.stats.begin_frame.push_since(t);
        let mut ctx = FrameCtx {
            state,
            eyes: None,
            head: None,
            targets: None,
            views: Vec::new(),
        };
        if !state.should_render {
            return Ok(ctx);
        }
        let (flags, views) = self
            .session
            .locate_views(
                xr::ViewConfigurationType::PRIMARY_STEREO,
                state.predicted_display_time,
                &self.space,
            )
            .ctx("xrLocateViews")?;
        if !flags.contains(xr::ViewStateFlags::ORIENTATION_VALID) || views.len() < 2 {
            return Ok(ctx);
        }
        if let Ok(loc) = self
            .view_space
            .locate(&self.space, state.predicted_display_time)
        {
            ctx.head = Some(crate::pose(loc.pose));
        }
        let eye = |v: &xr::View| {
            let (p, q) = crate::pose(v.pose);
            EyeView {
                view: fp_render::math::view(p, q),
                proj: fp_render::math::projection(
                    fp_render::math::Fov {
                        left: v.fov.angle_left,
                        right: v.fov.angle_right,
                        up: v.fov.angle_up,
                        down: v.fov.angle_down,
                    },
                    0.05,
                    200.0,
                ),
            }
        };
        ctx.eyes = Some([eye(&views[0]), eye(&views[1])]);
        let mut targets = Vec::new();
        let t = Instant::now();
        for e in &mut self.eyes {
            let i = e.swapchain.acquire_image().ctx("xrAcquireSwapchainImage")? as usize;
            e.swapchain
                .wait_image(xr::Duration::INFINITE)
                .ctx("xrWaitSwapchainImage")?;
            targets.push(EyeTarget {
                image: e.images[i],
                view: e.views[i],
                extent: self.extent,
                final_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            });
        }
        self.stats.eye_acquire.push_since(t);
        ctx.targets = Some([targets[0], targets[1]]);
        ctx.views = views;
        Ok(ctx)
    }

    /// Quad layers need RGBA8 sRGB swapchains (the format panels are
    /// painted in); otherwise everything stays in the projection layer.
    pub fn supports_quad_layers(&self) -> bool {
        self.format == vk::Format::R8G8B8A8_SRGB
    }

    fn quad_layer(&mut self, key: u64, width: u32, height: u32) -> Result<&mut QuadLayer> {
        let stale = self
            .quads
            .get(&key)
            .is_some_and(|q| (q.width, q.height) != (width, height));
        if stale {
            self.quads.remove(&key);
        }
        if !self.quads.contains_key(&key) {
            let swapchain = self
                .session
                .create_swapchain(&xr::SwapchainCreateInfo {
                    create_flags: xr::SwapchainCreateFlags::EMPTY,
                    usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                        | xr::SwapchainUsageFlags::TRANSFER_DST,
                    format: self.format.as_raw() as u32,
                    sample_count: 1,
                    width,
                    height,
                    face_count: 1,
                    array_size: 1,
                    mip_count: 1,
                })
                .ctx("xrCreateSwapchain (quad layer)")?;
            let images = swapchain
                .enumerate_images()
                .ctx("xrEnumerateSwapchainImages")?
                .into_iter()
                .map(vk::Image::from_raw)
                .collect::<Vec<_>>();
            if !self.quad_images_logged {
                self.quad_images_logged = true;
                let n = images.len();
                log::info!("quad swapchain images: {n} ({width}x{height})");
            }
            self.quads.insert(
                key,
                QuadLayer {
                    swapchain,
                    images,
                    width,
                    height,
                    acquired: false,
                    ready: false,
                },
            );
        }
        Ok(self.quads.get_mut(&key).expect("inserted above"))
    }

    /// The image to draw a quad layer's new picture into this frame
    /// (created on first use, `width`x`height`). The caller records the
    /// writes into this frame's GPU work; [`XrSession::end_frame`] releases
    /// it. Call at most once per key and frame.
    pub fn quad_layer_image(&mut self, key: u64, width: u32, height: u32) -> Result<vk::Image> {
        let q = self.quad_layer(key, width, height)?;
        let t = Instant::now();
        let i = q
            .swapchain
            .acquire_image()
            .ctx("xrAcquireSwapchainImage (quad layer)")? as usize;
        q.swapchain
            .wait_image(xr::Duration::INFINITE)
            .ctx("xrWaitSwapchainImage (quad layer)")?;
        q.acquired = true;
        let image = q.images[i];
        self.stats.quad_acquire.push_since(t);
        Ok(image)
    }

    /// Whether a quad layer has a picture to show.
    pub fn quad_layer_ready(&self, key: u64) -> bool {
        self.quads.get(&key).is_some_and(|q| q.ready || q.acquired)
    }

    /// A quad layer whose picture never changes: `fill` writes it once,
    /// waiting for the GPU, and it is released straight away.
    pub fn static_quad_layer(
        &mut self,
        key: u64,
        width: u32,
        height: u32,
        fill: impl FnOnce(vk::Image) -> std::result::Result<(), String>,
    ) -> Result<()> {
        if self.quad_layer_ready(key) {
            return Ok(());
        }
        let image = self.quad_layer_image(key, width, height)?;
        fill(image).map_err(Error::Runtime)?;
        let q = self.quads.get_mut(&key).expect("created above");
        q.swapchain
            .release_image()
            .ctx("xrReleaseSwapchainImage (quad layer)")?;
        q.acquired = false;
        q.ready = true;
        Ok(())
    }

    /// Releases the images and submits the projection layer, then `quads`
    /// on top of it in order.
    pub fn end_frame(&mut self, ctx: FrameCtx, quads: &[QuadSubmit]) -> Result<()> {
        let time = ctx.state.predicted_display_time;
        for q in self.quads.values_mut() {
            if q.acquired {
                q.swapchain
                    .release_image()
                    .ctx("xrReleaseSwapchainImage (quad layer)")?;
                q.acquired = false;
                q.ready = true;
            }
        }
        if ctx.targets.is_none() {
            let t = Instant::now();
            let ended = self.stream.end(time, self.blend_mode, &[]);
            self.stats.end_frame.push_since(t);
            return ended.ctx("xrEndFrame");
        }
        for e in &mut self.eyes {
            e.swapchain.release_image().ctx("xrReleaseSwapchainImage")?;
        }
        let rect = xr::Rect2Di {
            offset: xr::Offset2Di { x: 0, y: 0 },
            extent: xr::Extent2Di {
                width: self.extent.width as i32,
                height: self.extent.height as i32,
            },
        };
        let views: Vec<xr::CompositionLayerProjectionView<xr::Vulkan>> = ctx
            .views
            .iter()
            .zip(&self.eyes)
            .map(|(v, e)| {
                xr::CompositionLayerProjectionView::new()
                    .pose(v.pose)
                    .fov(v.fov)
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&e.swapchain)
                            .image_rect(rect)
                            .image_array_index(0),
                    )
            })
            .collect();
        let mut flags = xr::CompositionLayerFlags::EMPTY;
        if self.blend_mode == xr::EnvironmentBlendMode::ALPHA_BLEND {
            flags |= xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA;
        }
        let layer = xr::CompositionLayerProjection::new()
            .layer_flags(flags)
            .space(&self.space)
            .views(&views);
        let quad_layers: Vec<xr::CompositionLayerQuad<xr::Vulkan>> = quads
            .iter()
            .filter_map(|s| {
                let q = self.quads.get(&s.key).filter(|q| q.ready)?;
                Some(
                    xr::CompositionLayerQuad::new()
                        // Rounded corners and the ray's soft edges are alpha.
                        .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
                        .space(&self.space)
                        .eye_visibility(xr::EyeVisibility::BOTH)
                        .sub_image(
                            xr::SwapchainSubImage::new()
                                .swapchain(&q.swapchain)
                                .image_rect(xr::Rect2Di {
                                    offset: xr::Offset2Di { x: 0, y: 0 },
                                    extent: xr::Extent2Di {
                                        width: q.width as i32,
                                        height: q.height as i32,
                                    },
                                })
                                .image_array_index(0),
                        )
                        .pose(crate::to_pose(s.pose.0, s.pose.1))
                        .size(xr::Extent2Df {
                            width: s.size.x,
                            height: s.size.y,
                        }),
                )
            })
            .collect();
        let mut all: Vec<&xr::CompositionLayerBase<xr::Vulkan>> = vec![&*layer];
        all.extend(quad_layers.iter().map(|q| &**q));
        let t = Instant::now();
        let ended = self.stream.end(time, self.blend_mode, &all);
        self.stats.end_frame.push_since(t);
        ended.ctx("xrEndFrame")
    }

    /// Controller state at `time`.
    pub fn input(&self, time: xr::Time) -> InputState {
        self.actions.read(&self.session, &self.space, time)
    }

    pub fn buzz(&self, hand: usize, amplitude: f32, millis: i64) {
        self.actions.buzz(&self.session, hand, amplitude, millis);
    }

    /// Interaction profiles that accepted bindings.
    pub fn bindings(&self) -> &[crate::BoundProfile] {
        &self.actions.bound
    }
}

impl Drop for XrSession {
    fn drop(&mut self) {
        self.gpu.wait_idle();
        for e in &self.eyes {
            for &v in &e.views {
                // SAFETY: GPU idle; views were created by us.
                unsafe { self.gpu.device.destroy_image_view(v, None) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_visible_and_focused_count_as_worn() {
        assert!(is_worn(xr::SessionState::FOCUSED));
        assert!(is_worn(xr::SessionState::VISIBLE));
        for off in [
            xr::SessionState::UNKNOWN,
            xr::SessionState::IDLE,
            xr::SessionState::READY,
            xr::SessionState::SYNCHRONIZED,
            xr::SessionState::STOPPING,
            xr::SessionState::LOSS_PENDING,
            xr::SessionState::EXITING,
        ] {
            assert!(!is_worn(off), "{off:?}");
        }
    }
}
