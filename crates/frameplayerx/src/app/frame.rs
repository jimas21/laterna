//! Each frame: where the panels go, painting them, and what the compositor
//! draws.

use super::*;
use crate::world::GrabBy;
use fp_render::ScreenShape;
use fp_render::env::SeatName;

impl App {
    /// Forward becomes where the head faces, and the browse panel comes
    /// back in front, wherever it was moved. A flat screen comes back in
    /// front too (its yaw and pitch only place it; a 180°/360° picture keeps
    /// its turn), so the two line up. The adjust panel keeps its place
    /// beside the player.
    pub(super) fn recenter(&mut self) {
        self.reanchor = true;
        self.panel_drag = None;
        self.chrome.cancel();
        self.panel_chrome.cancel();
        self.offsets.main = [0.0, 0.0];
        self.offsets.depth.main = 0.0;
        if let Some(p) = &mut self.playback
            && p.format.projection == Projection::Flat
            && (p.settings.yaw, p.settings.pitch) != (0.0, 0.0)
        {
            p.settings.yaw = 0.0;
            p.settings.pitch = 0.0;
            p.settings_dirty = true;
        }
        // Side by side, they stay side by side, facing where you look now.
        self.recenter_multitask();
        self.settings.panel_offsets = self.kept_offsets();
    }

    /// A flat screen, and the panels that move, back where they start,
    /// wherever they were moved. A 180°/360°
    /// picture's turn is the video's own adjustment and stays (its Reset
    /// puts that back).
    pub(super) fn reset_placement(&mut self) {
        self.end_multitask(false);
        self.panel_drag = None;
        self.chrome.cancel();
        self.panel_chrome.cancel();
        self.adjust_hold = None;
        self.offsets = Default::default();
        self.settings.panel_offsets = self.offsets;
        if let Some(p) = &mut self.playback
            && p.format.projection == Projection::Flat
            && (p.settings.yaw, p.settings.pitch) != (0.0, 0.0)
        {
            p.settings.yaw = 0.0;
            p.settings.pitch = 0.0;
            p.settings_dirty = true;
        }
    }

    /// The bar's Size button: the next of Home theater, Cinema and Giant for
    /// the flat screen (or, in a room, the seat that sees it so), shown at
    /// once and named on a toast.
    pub(super) fn cycle_size(&mut self) {
        let Some(p) = &mut self.playback else {
            return;
        };
        if p.format.projection != Projection::Flat {
            return;
        }
        let before = p.settings;
        let size = ui::size::cycle(&mut self.settings, &mut p.settings, self.place_shown);
        if p.settings != before {
            p.settings_dirty = true;
        }
        self.ui
            .toast(tr!("toast-screen-size", name = ui::size::name(size)));
    }

    /// The panel the keyboard types into: the library or adjust panel with
    /// a text field in focus.
    pub(super) fn keyboard_target(&self) -> Option<usize> {
        [MAIN, ADJUST]
            .into_iter()
            .find(|&i| self.panels[i].visible && self.panels[i].wants_keyboard())
    }

    /// Squeezing a grip on the library panel, the control bar, the adjust
    /// panel or the performance overlay moves it round the viewer with that
    /// hand, and nearer or further as the hand comes towards or goes away
    /// from the head; where it is let go is saved: the bar's relative to a flat screen,
    /// the adjust panel's relative to the bar, so each goes along when what it
    /// is placed from moves. Squeezing one on the keyboard moves the panel it
    /// types into, and the keyboard with it; on the audio and subtitles
    /// picker, the bar it sits on. The picture itself is dragged with grip
    /// and trigger (see `controls`).
    pub(super) fn move_panels(
        &mut self,
        grab: Option<(usize, usize, GrabBy, Option<usize>)>,
        input: &FrameInput,
        anchor: Mat4,
        buzz: &mut Vec<(usize, f32, i64)>,
    ) {
        use crate::controls::{DRAG_TAU, Damped, GRIP, angles, pushed_distance, reach, wrap};
        let hands = &input.hands;
        let head = input.head.map(|(pos, _)| pos);
        // The adjust panel sits beside the control bar, turned with the
        // picture; the library panel and the overlay are placed in the room.
        let room = anchor.to_scale_rotation_translation().1;
        let rotation = |m: Mat4| m.to_scale_rotation_translation().1;
        // Each is moved about what it is placed from: the adjust panel
        // about the control bar, the bar about a flat screen.
        let beside_bar = rotation(self.player_anchor(anchor));
        let flat = self
            .playback
            .as_ref()
            .is_some_and(|p| p.format.projection == Projection::Flat);
        // A docked screen is in your hand: the bar stays in the room, as it does
        // for a 180°/360° video.
        let under = if flat && self.docked().is_none() {
            rotation(self.picture_anchor(anchor))
        } else {
            room
        };
        let frame_of = move |panel: usize| match panel {
            ADJUST => beside_bar,
            BAR => under,
            _ => room,
        };
        let grab = grab.and_then(|(panel, hand, by, via)| match panel {
            KEYBOARD => self.keyboard_target().map(|t| (t, hand, by, via)),
            MAIN | ADJUST | PERF => Some((panel, hand, by, via)),
            BAR | TRACKS if self.playback.is_some() => Some((BAR, hand, by, via)),
            _ => None,
        });
        if let Some((panel, hand, by, via)) = grab
            && let Some((pos, rot)) = hands[hand].aim
        {
            let start = match panel {
                MAIN => self.offsets.main,
                ADJUST => self.offsets.adjust,
                PERF => self.offsets.perf,
                _ => self.offsets.bar,
            };
            self.panel_drag = Some(PanelDrag {
                panel,
                hand,
                by,
                via,
                start: angles(rot, frame_of(panel)),
                reach: head.map(|head| reach(head, pos)),
                from: start,
                depth: *self.depth_of(panel),
                follow: Damped::at([0.0; 3]),
            });
            buzz.push((hand, 0.3, 14));
        }
        let Some(PanelDrag {
            panel,
            hand,
            by,
            via: _,
            start: (y0, p0),
            reach: reach0,
            from: [oy, op],
            depth: z0,
            mut follow,
        }) = self.panel_drag
        else {
            return;
        };
        let h = &hands[hand];
        let held = match by {
            GrabBy::Grip => h.squeeze > GRIP * 0.6,
            GrabBy::Trigger => self.pointer.trigger_down(hand),
        };
        if held {
            if let Some((pos, rot)) = h.aim {
                let (y, p) = angles(rot, frame_of(panel));
                // Without the head (then or now) the depth holds where it is.
                // A trigger takes no reach from the hand: the thumbstick
                // pushes and pulls instead (as at the screen's bar).
                let moved = match (head, reach0, by) {
                    (_, _, GrabBy::Trigger) => 0.0,
                    (Some(head), Some(r), _) => reach(head, pos) - r,
                    _ => follow.value()[2],
                };
                // Where the hand is, a moment behind it: a hand's tremor would
                // otherwise be magnified at the panel's distance.
                let [dy, dp, dz] = follow.step([wrap(y - y0), p - p0, moved], DRAG_TAU, input.dt);
                if let Some(drag) = &mut self.panel_drag {
                    drag.follow = follow;
                }
                let to = [wrap(oy + dy), (op + dp).clamp(-45.0, 45.0)];
                let usual = usual_distance(panel);
                *self.depth_of(panel) = match by {
                    GrabBy::Grip => pushed_distance(usual + z0, dz, PANEL_RANGE) - usual,
                    GrabBy::Trigger => {
                        let now = panel_distance(usual, *self.depth_of(panel));
                        Self::stick_pushed(now, h.stick.y, input.dt) - usual
                    }
                };
                match panel {
                    MAIN => self.offsets.main = to,
                    ADJUST => self.offsets.adjust = to,
                    PERF => self.offsets.perf = to,
                    _ => self.offsets.bar = to,
                }
                self.last_activity = Instant::now();
            }
        } else {
            self.panel_drag = None;
            self.settings.panel_offsets = self.kept_offsets();
        }
    }

    /// How much nearer (negative) or further than usual `panel` was moved,
    /// metres.
    fn depth_of(&mut self, panel: usize) -> &mut f32 {
        let depth = &mut self.offsets.depth;
        match panel {
            MAIN => &mut depth.main,
            ADJUST => &mut depth.adjust,
            PERF => &mut depth.perf,
            _ => &mut depth.bar,
        }
    }

    pub fn frame(&mut self, renderer: &mut Renderer, input: FrameInput) -> FrameOutput {
        self.update();
        if input.recentred {
            self.recenter();
        }
        let anchor = self.anchor(&input);
        let eye = input.head.map(|h| h.0).unwrap_or(anchor.w_axis.truncate());
        self.update_dock(&input);
        self.update_visibility();
        let kb_target = self.keyboard_target();
        self.panels[KEYBOARD].visible = kb_target.is_some();
        self.chrome.ease(input.dt);
        self.place_panels(anchor, kb_target);
        self.place_panel_chrome(eye);
        let buzz = self.route_input(&input, anchor);
        // Routing the input moved the panels, the picture or both: place the
        // panels again, so they are drawn where the hands just put them rather
        // than a frame behind.
        self.place_panels(anchor, kb_target);
        self.place_panel_chrome(eye);
        self.paint_panels(renderer, &input, kb_target);
        self.paint_window_controls(renderer, &input);
        self.paint_panel_chrome(renderer, &input);
        self.perf_overlay(renderer, &input);
        let (frame, video, passthrough) = self.video(renderer, &input, anchor);
        let (quads, layers) = self.compose(&input, eye);
        let film_fps = self
            .playback
            .as_ref()
            .filter(|_| self.settings.auto_refresh_rate)
            .and_then(|p| p.player.info().video_stream())
            .map(|v| v.fps)
            .filter(|fps| *fps > 0.0);
        FrameOutput {
            frame,
            video,
            quads,
            layers,
            passthrough,
            buzz,
            quit: self.quit,
            restart: self.restart,
            open_dashboard: std::mem::take(&mut self.open_dashboard),
            film_fps,
        }
    }

    /// Background work, remote commands and saving the open video's state.
    fn update(&mut self) {
        self.poll_jobs();
        self.update_earbuds();
        // Play all, a playlist and what was queued play through, any list
        // with Autoplay; Loop video keeps to the one playing.
        if self
            .playback
            .as_ref()
            .is_some_and(|p| p.player.state() == PlayerState::Ended)
        {
            if self.settings.loop_video {
                self.play_again();
            } else if let Some(i) = self
                .queue
                .up_next(self.settings.autoplay, self.settings.loop_playlist)
            {
                self.play_queued(i);
            }
        }
        self.give_up_slow_open();
        self.services
            .set_worker_busy(self.playback.is_some() || self.ui.opening.is_some());
        self.sync_locks();
        self.check_mounts();
        self.start_server_homes();
        self.remote_events();
        if let Some(p) = &mut self.playback {
            p.save_progress(&self.services.library, &self.services.writer, false);
            p.sync_watch();
            p.save_key(&self.services.library, &self.services.writer, false);
            p.watch_decoding(self.settings.hardware_decoding);
        }
        self.publish_status();
        self.publish_details();
    }

    /// The room's frame of reference for the panels and the video.
    fn anchor(&mut self, input: &FrameInput) -> Mat4 {
        if let Some((pos, rot)) = input.head {
            if self.reanchor || self.anchor.is_none() {
                // Level at start; a recenter faces where you look. In a
                // place it tips the whole room with the gaze past 10 degrees
                // (watching reclined or lying down), not only turns it.
                let in_place = self.active_place(input.passthrough_available).is_some();
                self.anchor = Some(match self.anchor {
                    Some(_) if in_place => anchor_facing_within(pos, rot, PLACE_LEVEL_WITHIN_DEG),
                    Some(_) => anchor_facing(pos, rot),
                    None => anchor_from_head(pos, rot),
                });
                self.reanchor = false;
            } else if let Some(anchor) = self.anchor
                && self.follows(input.passthrough_available)
            {
                self.anchor = Some(follow(anchor, (pos, rot), input.dt));
            }
        }
        let mut anchor = self.anchor.unwrap_or(Mat4::IDENTITY);
        // A 180°/360° video is drawn from wherever the head is (it was
        // filmed from one spot), so it moves along when you lean; panels
        // fixed in the room then seem to slide against it. During such a
        // video the panels keep to the head's position (not its turning),
        // so they stay put relative to the picture. It never rolls, even
        // when following turned the anchor on its side.
        if let (Some((pos, _)), Some(p)) = (input.head, &self.playback)
            && p.format.projection != Projection::Flat
        {
            let (_, rot, _) = anchor.to_scale_rotation_translation();
            anchor = anchor_facing(pos, rot);
        }
        anchor
    }

    /// Whether the flat screen and panels follow the head: the setting is
    /// on, the video (if any) is flat (around a 180°/360° one you look
    /// about), neither a panel nor the screen is being moved (by its window
    /// controls or by dragging the picture), and no place is shown. A place
    /// stays where it is, menus included: a room that turned with the head
    /// would swim, and so would a sky and its floor.
    fn follows(&self, passthrough_available: bool) -> bool {
        let dragging = self.panel_drag.is_some()
            || self.drag_start.is_some()
            || self.chrome.dragging()
            || self.panel_chrome.resizing();
        // A docked screen is where your hand is, and the panels stay in the room.
        follows_head(self.settings.follow, self.flat_or_none(), dragging)
            && self.docked().is_none()
            && self.active_place(passthrough_available).is_none()
    }

    /// Whether there is no video, or it is a flat one.
    fn flat_or_none(&self) -> bool {
        self.playback
            .as_ref()
            .is_none_or(|p| p.format.projection == Projection::Flat)
    }

    /// Whether the real room shows: the runtime can show it, and either
    /// Settings › Passthrough is on for a flat video (or the menus), or the
    /// video is chroma-keyed or alpha-packed, which show the room where their
    /// background was, whatever their projection.
    fn passthrough_shown(&self, available: bool) -> bool {
        let keyed = self.playback.as_ref().is_some_and(|p| {
            p.shown_settings(&self.settings.default_view).chroma_key
                || p.format.alpha_pack_scale().is_some()
        });
        let wanted = self.settings.passthrough;
        places::passthrough_shown(available, wanted, self.flat_or_none(), keyed)
    }

    /// The place to show around the picture and the menus: the one picked in
    /// Settings, unless the real room shows, the video fills the view, or
    /// places are off.
    fn active_place(&self, passthrough_available: bool) -> Option<Place> {
        let passthrough = self.passthrough_shown(passthrough_available);
        places::active_place(
            &self.settings.environment,
            passthrough,
            self.flat_or_none(),
            self.places_enabled,
        )
    }

    /// Whether a room (not a sky) is shown: it sets where its screen is.
    pub(super) fn in_room(&self) -> bool {
        self.place_shown
            .is_some_and(|p| p.kind() == fp_render::env::PlaceKind::Room)
    }

    /// Moves the house lights towards where playback wants them: down while
    /// a video plays, up when it pauses or ends, and where they are while it
    /// rebuffers.
    fn update_house_lights(&mut self, dt: f32) {
        let state = self.playback.as_ref().map(|p| p.player.state());
        if let Some(goal) = places::house_target(state).goal() {
            self.house_ease = places::house_step(self.house_ease, goal, dt);
        }
    }

    /// The picture: what its yaw and pitch turn (by dragging it, or the
    /// Position sliders). A flat screen and the subtitles are placed from
    /// it; a 180°/360° picture turns the same way. A flat screen starts
    /// upright, straight ahead at eye height.
    pub(super) fn picture_anchor(&self, anchor: Mat4) -> Mat4 {
        match &self.playback {
            Some(p) => {
                let s = p.shown_settings(&self.settings.default_view);
                turned_with_picture(anchor, s.yaw, s.pitch)
            }
            None => anchor,
        }
    }

    /// The player, which the control bar is placed from (see
    /// [`player_frame`]).
    fn player_anchor(&self, anchor: Mat4) -> Mat4 {
        match &self.playback {
            Some(p) => {
                let s = p.shown_settings(&self.settings.default_view);
                // Docked, the screen is in your hand: the bar is in the room.
                let flat = p.format.projection == Projection::Flat && self.docked().is_none();
                player_frame(anchor, flat, [s.yaw, s.pitch], self.offsets.bar)
            }
            None => player_frame(anchor, false, [0.0; 2], self.offsets.bar),
        }
    }

    /// The performance overlay while Settings › General turns it on, with new
    /// readings once a second. It paints what it last read, so painting for
    /// any other reason can't change a number.
    fn perf_overlay(&mut self, renderer: &mut Renderer, input: &FrameInput) {
        if !self.settings.perf_overlay {
            self.perf.visible = false;
            self.perf_monitor = None;
            self.perf_readings = None;
            return;
        }
        let sample = self
            .perf_monitor
            .get_or_insert_with(crate::perf::Monitor::start)
            .latest();
        let readings = self.perf_readings.get_or_insert_with(Default::default);
        readings.frame(input.frame_ms, input.display_hz);
        let now = Instant::now();
        if readings.note_due(now) {
            let open = self
                .playback
                .as_ref()
                .map(|p| (p, p.player.stats(), p.player.net()));
            let note = open.as_ref().map(|(p, st, net)| crate::perf::VideoNote {
                serial: st.serial,
                playing: p.player.state() == PlayerState::Playing,
                shown: st.frames_shown,
                stutter: st.frames_dropped + st.stalls,
                net: net.as_ref().map(|n| {
                    (
                        n.bytes.load(std::sync::atomic::Ordering::Relaxed),
                        n.retries.load(std::sync::atomic::Ordering::Relaxed),
                    )
                }),
            });
            if readings.note(now, note) {
                let hardware = self.settings.hardware_decoding;
                self.perf_shown = ui::perf::Overlay {
                    drawn: readings.drawn(),
                    display_hz: input.display_hz,
                    frame_ms: readings.frame_ms(),
                    video: open.as_ref().and_then(|(p, st, net)| {
                        perf_video(p, st, net.is_some(), readings, hardware)
                    }),
                    sample,
                };
                self.perf.request_repaint();
            }
        }
        self.perf.visible = true;
        if !self.perf.needs_paint() {
            return;
        }
        let shown = &self.perf_shown;
        let mut card = None;
        let painted = self.perf.paint(renderer, input.time, |ctx| {
            card = Some(ui::perf::overlay(ctx, shown));
        });
        if let Err(e) = painted {
            log::error!("performance overlay: {e}");
        }
        self.perf.solid = card;
    }

    /// Which panels show: the controls hide on their own while a video
    /// plays.
    fn update_visibility(&mut self) {
        self.check_multitask();
        let playing = self.playback.is_some();
        // Side by side, the library and the player's controls both stay up.
        let multitask = self.multitasking();
        if multitask {
            self.show_browser = true;
            self.ui_visible = true;
        }
        self.ui.multitask = multitask;
        let paused = self
            .playback
            .as_ref()
            .is_none_or(|p| p.player.is_paused() || p.player.state() != PlayerState::Playing);
        if playing
            && self.ui_visible
            && !paused
            && !self.ui.adjust_open
            && !self.ui.tracks_open
            && !self.show_browser
            && self.last_activity.elapsed().as_secs_f32() > self.settings.auto_hide_secs
        {
            self.ui_visible = false;
        }
        let browser = !playing || (self.ui_visible && self.show_browser);
        self.panels[MAIN].visible = browser;
        // Said where it can be acted on, on the library panel.
        if browser && let Some(notice) = self.update_notice.take() {
            self.ui.toast(notice);
        }
        self.panels[BAR].visible = playing && self.ui_visible && (!self.show_browser || multitask);
        self.panels[ADJUST].visible = self.panels[BAR].visible && self.ui.adjust_open;
        self.panels[TRACKS].visible = self.panels[BAR].visible && self.ui.tracks_open;
        self.show_window_controls();
        let over_video = playing && !browser;
        let live = self
            .ui
            .toast
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed().as_secs_f32() < VIDEO_TOAST_SECS);
        self.panels[TOAST].visible = over_video && live;
        // Said and gone: it doesn't come back when the library opens.
        if over_video && !live {
            self.ui.toast = None;
        }
    }

    /// Where each panel goes: the control bar from the player (see
    /// `player_anchor`), the audio and subtitles picker on top of it, and
    /// the adjust panel at its own place beside the bar, upright, going
    /// wherever the bar goes.
    fn place_panels(&mut self, anchor: Mat4, kb_target: Option<usize>) {
        let bar_anchor = self.player_anchor(anchor);
        // The adjust panel holds still while the trigger is held on it: its
        // yaw and pitch sliders move a flat screen, and with it the bar and
        // this panel, away from the pointer. It catches up when let go.
        let held = self.pointer.pressed() == Some(ADJUST);
        let adjust_anchor = match self.adjust_hold.filter(|_| held) {
            Some(hold) => hold,
            None => {
                self.adjust_hold = held.then_some(bar_anchor);
                bar_anchor
            }
        };
        let moved = self.offsets.depth;
        let main_d = panel_distance(MAIN_DISTANCE, moved.main);
        let bar_d = panel_distance(BAR_DISTANCE, moved.bar);
        let adjust_d = panel_distance(ADJUST_DISTANCE, moved.adjust);
        // The window controls (app/chrome) go under the screen; the control
        // bar hangs lower when it would cover them.
        let bar_drop = self.place_window_controls(anchor, bar_d);
        // To the right, clear of the control bar's end as it grows with the
        // interface size, and as it comes nearer.
        let half_angle = |w: f32, d: f32| (w / 2.0 / d).atan().to_degrees();
        let bar = &self.panels[BAR];
        let growth = half_angle(bar.size.x, bar_d) - half_angle(bar.base_size().x, BAR_DISTANCE);
        let aw = self.panels[ADJUST].size.x;
        let adj_growth = half_angle(aw, adjust_d) - half_angle(aw, ADJUST_DISTANCE);
        let beside = -52.0 - growth - adj_growth;
        let [my, mp] = self.offsets.main;
        let [ay, ap] = self.offsets.adjust;
        self.panels[MAIN].pose = place_below(anchor, my, mp, main_d, BELOW_DEG);
        self.panels[BAR].pose = chrome::layout::bar_pose(bar_anchor, bar_d, bar_drop);
        self.panels[TRACKS].pose = above_bar(&self.panels[BAR], &self.panels[TRACKS]);
        self.panels[TOAST].pose = toast_pose(
            &self.panels[BAR],
            self.panels[TRACKS]
                .visible
                .then(|| self.panels[TRACKS].size.y),
            &self.panels[TOAST],
        );
        self.panels[ADJUST].pose = place_around(adjust_anchor, beside + ay, ap, adjust_d, 0.0);
        // The keyboard sits below the panel it types into, turned with it and
        // as much nearer or further, so it is where that panel was moved to.
        let (kb_from, [ky, kp], kb_moved) = match kb_target {
            Some(ADJUST) => (adjust_anchor, [beside + ay, ap], moved.adjust),
            Some(_) => (anchor, [my, mp - BELOW_DEG], moved.main),
            None => (anchor, [0.0, 0.0], 0.0),
        };
        let kb_anchor = place_around(kb_from, ky, kp, 0.0, 0.0);
        let kb_d = panel_distance(KEYBOARD_DISTANCE, kb_moved);
        let kb_drop = -0.55 * kb_d / KEYBOARD_DISTANCE;
        self.panels[KEYBOARD].pose = place(kb_anchor, 0.0, kb_d, kb_drop, 35.0);
        // From the same point as the library panel, so it stays in the room or
        // follows along with the menus.
        let start = perf_start_yaw(
            self.panels[MAIN].size.x,
            main_d,
            self.panels[BAR].size.x,
            bar_d,
            self.perf.size.x,
        );
        let [py, pp] = self.offsets.perf;
        let perf_d = panel_distance(PERF_DISTANCE, moved.perf);
        self.perf.pose = place_around(anchor, start + py, pp, perf_d, 0.0);
    }

    /// Pointer rays to panel events, grips moving panels, and controller
    /// commands; returns the haptic pulses to play.
    fn route_input(&mut self, input: &FrameInput, anchor: Mat4) -> Vec<(usize, f32, i64)> {
        self.pointer.rays_shown = self.any_ui();
        let routed = {
            let [a, b, c, d, e, _toast] = &mut self.panels;
            // The window controls (app/chrome) come last, after the overlay:
            // see `chrome::WINDOW_BAR`.
            let (bar, corner) = (&mut self.chrome.bar, &mut self.chrome.corner);
            let [lib_bar, adjust_bar, perf_bar, kb_bar] = &mut self.panel_chrome.bars;
            let lib_corner = &mut self.panel_chrome.corner;
            let mut panels = [
                a,
                b,
                c,
                d,
                e,
                &mut self.perf,
                bar,
                corner,
                lib_bar,
                adjust_bar,
                perf_bar,
                kb_bar,
                lib_corner,
            ];
            self.pointer.route(&mut panels, &input.hands, input.dt)
        };
        let mut buzz = Vec::new();
        // Taking hold of a window control drags the screen; any other grab
        // moves a panel.
        let mut panel_grab = None;
        match routed.grab {
            Some(g) if matches!(g.panel, chrome::WINDOW_BAR | chrome::WINDOW_CORNER) => {
                self.begin_window_drag(g, input, anchor, &mut buzz);
            }
            Some(g) if g.panel == chrome::panels::PANEL_CORNER => {
                self.begin_panel_resize(g, input, &mut buzz);
            }
            // A bar under a panel moves that panel (the keyboard's, the one
            // it types into).
            Some(g) if g.panel >= chrome::panels::PANEL_BAR_FIRST => {
                if let Some((slot, target)) = self.panel_bar_target(g.panel) {
                    panel_grab = Some((target, g.hand, g.by, Some(slot)));
                }
            }
            Some(g) => panel_grab = Some((g.panel, g.hand, g.by, None)),
            None => {}
        }
        self.move_panels(panel_grab, input, anchor, &mut buzz);
        self.update_window_drag(input, anchor, &mut buzz);
        self.update_panel_resize(input, &mut buzz);
        if let Some(hand) = routed.pressed {
            buzz.push((hand, 0.2, 8));
        }
        self.controller(
            input,
            routed.over_ui
                || self.panel_drag.is_some()
                || self.chrome.dragging()
                || self.panel_chrome.resizing(),
            routed.click_outside,
            anchor,
            &mut buzz,
        );
        buzz
    }

    /// Paints the panels that changed (and the keyboard), then carries out
    /// what they asked for.
    fn paint_panels(
        &mut self,
        renderer: &mut Renderer,
        input: &FrameInput,
        kb_target: Option<usize>,
    ) {
        let mut actions = Vec::new();
        if self.thumbs.poll(&self.panels[MAIN].ctx) {
            self.panels[MAIN].request_repaint();
        }
        // Paint the keyboard first so a pressed key's text reaches its target
        // panel and paints on the same frame.
        if let Some(target) = kb_target {
            let mut events = Vec::new();
            let mut close = false;
            let shift = &mut self.ui.keyboard_shift;
            let symbols = &mut self.ui.keyboard_symbols;
            if self.panels[KEYBOARD].needs_paint() {
                let r = self.panels[KEYBOARD].paint(renderer, input.time, |ctx| {
                    let (e, c) = ui::keyboard::keyboard(ctx, shift, symbols);
                    events = e;
                    close = c;
                });
                if let Err(e) = r {
                    log::error!("painting the keyboard: {e}");
                }
            }
            for e in events {
                self.panels[target].push(e);
            }
            if close {
                self.panels[target].push(escape());
            }
        }
        let devices = self.services.haptic_statuses();
        let passthrough_available = input.passthrough_available;
        let (places_enabled, place_shown) = (self.places_enabled, self.place_shown);
        let dashboard = input.dashboard;
        let docked = self.docked_hand();
        for idx in [MAIN, BAR, ADJUST, TRACKS, TOAST] {
            if !self.panels[idx].needs_paint() {
                continue;
            }
            let App {
                panels,
                ui: state,
                settings,
                services,
                thumbs,
                playback,
                queue,
                ..
            } = self;
            let mut view = View {
                state,
                queue,
                settings,
                services,
                thumbs,
                playback: playback.as_mut(),
                actions: &mut actions,
                passthrough_available,
                places_enabled,
                place_shown,
                dashboard,
                docked,
                devices: &devices,
            };
            let r = panels[idx].paint(renderer, input.time, |ctx| match idx {
                MAIN => ui::library::browser(ctx, &mut view),
                BAR => ui::player::control_bar(ctx, &mut view),
                TRACKS => ui::player::tracks_panel(ctx, &mut view),
                TOAST => ui::player::toast_panel(ctx, &mut view),
                _ => ui::player::adjust_panel(ctx, &mut view),
            });
            if let Err(e) = r {
                log::error!("painting panel: {e}");
            }
        }
        for a in actions {
            self.apply(a);
        }
        self.sync_settings();
    }

    /// The video frame and how to draw it, and whether the room shows.
    fn video(
        &mut self,
        renderer: &mut Renderer,
        input: &FrameInput,
        anchor: Mat4,
    ) -> (Option<Arc<VideoFrame>>, VideoParams, bool) {
        let shown = self
            .playback
            .as_ref()
            .map(|p| p.shown_settings(&self.settings.default_view));
        // The open video's colour, else the one videos start with. It is
        // sRGB; the eyes are cleared in linear light.
        let [r, g, b] = shown
            .unwrap_or(self.settings.default_view)
            .background
            .map(|c| egui::ecolor::linear_from_gamma(c.clamp(0.0, 1.0)));
        let mut video = VideoParams {
            background: [r, g, b, 1.0],
            ..VideoParams::default()
        };
        let mut frame = None;
        // A chroma-keyed or alpha-packed video shows the real room where its
        // background was, whatever its projection and whether or not
        // passthrough is on, and the real room wins over a place.
        let passthrough = self.passthrough_shown(input.passthrough_available);
        if passthrough {
            video.background = [0.0; 4];
        }
        let in_place = self.active_place(input.passthrough_available);
        self.place_shown = in_place;
        self.update_house_lights(input.dt);
        let anchor_rot = anchor.to_scale_rotation_translation().1;
        let player = self.picture_anchor(anchor);
        // A room sits round the viewer in a seat and turns with the picture;
        // a sky stays with the plain anchor.
        // The seat's name goes with the room: a pack baked for each seat
        // draws the viewer's own seat's textures.
        let (room, seat_name) = in_place.map_or((None, SeatName::Middle), |pl| {
            let layout = pl.layout();
            let seat = places::seat_index(&self.settings, pl, &layout);
            let height = places::seat_height_cm(&self.settings, pl);
            (
                places::room_frame(player, &layout, seat, height),
                layout.seat(seat).name,
            )
        });
        video.env = in_place.map(|place| fp_render::env::EnvFrame {
            place,
            seat: seat_name,
            room_to_world: room.map_or(anchor, |r| r.room_to_world),
            house: places::house_shown(
                self.house_ease,
                self.house_override,
                self.settings.house_lights_playing,
            ),
            screen_light: crate::settings::screen_light_gain(self.settings.screen_light_level),
            time: input.time,
        });
        if let (Some(p), Some(s)) = (&self.playback, shown) {
            frame = p.player.current_frame_at(input.display_lead);
            // What the screen's shape is worked out from (see
            // `screen_geometry`); the last picture's size stands in on a frame
            // the video gives none.
            if let Some(f) = &frame {
                self.last_frame_size = Some((f.width, f.height));
            }
            describe_video(&mut video, p, s);
            video.screen_pose = place(player, 0.0, s.screen_distance, 0.0, 0.0);
            video.anchor = anchor_rot;
            // A room has its own screen: its size, its curve and where it
            // hangs, whatever the picture's own screen settings say. A docked
            // one is small, flat and in your hand (the floating screen's
            // settings are left alone for when it is undocked).
            let docked = self.docked_screen().filter(|_| room.is_none());
            if let Some(r) = room {
                video.screen_pose = r.screen_pose;
                video.screen_fit = Some(r.fit);
                video.settings.screen_curvature = r.curvature;
            } else if let Some((pose, width)) = docked {
                video.screen_pose = pose;
                video.settings.screen_width = width;
                video.settings.screen_curvature = 0.0;
            }
            if let Some((_, rot)) = input.head {
                let rel = anchor_rot.inverse() * rot;
                p.player.set_head_orientation(rel.to_array());
            }
            // In a room the subtitles hang on the screen, not at the distance
            // set for a screen of your own.
            self.subtitles(
                renderer,
                input.time,
                player,
                &s,
                room.as_ref(),
                input.eye_px_per_tan,
            );
        } else {
            self.subs.visible = false;
            self.last_frame_size = None;
        }
        (frame, video, passthrough)
    }

    /// The open flat video's screen: its size and curve exactly as the shader
    /// draws them, and how far away it is. `None` without a flat video, or in
    /// a room.
    pub(super) fn screen_geometry(&self) -> Option<(ScreenShape, f32)> {
        let p = self.playback.as_ref()?;
        // A room's screen is where the room has it: no move bar or corner.
        if p.format.projection != Projection::Flat || self.in_room() {
            return None;
        }
        let mut shown = p.shown_settings(&self.settings.default_view);
        // A docked screen is as wide as it was docked, flat, and the eye is
        // taken to be a fixed distance from it.
        let mut distance = shown.screen_distance;
        if let Some(d) = self.docked() {
            shown.screen_width = d.width;
            shown.screen_curvature = 0.0;
            distance = chrome::dock::EYE;
        }
        let mut video = VideoParams::default();
        describe_video(&mut video, p, shown);
        let shape = ScreenShape::of(&video, self.last_frame_size);
        Some((shape, distance))
    }

    /// What the compositor draws: panels back to front, then pointers,
    /// as quads in the eyes or (when it can) layers of their own.
    fn compose(&mut self, input: &FrameInput, eye: Vec3) -> (Vec<QuadDraw>, Vec<LayerDraw>) {
        let mut quads = Vec::new();
        // Subtitles fade in like the panels (a panel that never fades in is
        // never drawn).
        self.subs.update_fade(input.dt);
        if self.subs.visible
            && let Some(q) = self.subs.quad()
        {
            if self.subs_split {
                // Each eye sees its half of the panel, as with a 3D video.
                quads.push(QuadDraw {
                    uv: [Vec2::ZERO, Vec2::new(0.5, 1.0)],
                    eye: Some(0),
                    ..q
                });
                quads.push(QuadDraw {
                    uv: [Vec2::new(0.5, 0.0), Vec2::ONE],
                    eye: Some(1),
                    ..q
                });
            } else {
                quads.push(q);
            }
        }
        let extra = [
            &mut self.perf,
            &mut self.chrome.bar,
            &mut self.chrome.corner,
        ];
        for p in self
            .panels
            .iter_mut()
            .chain(extra)
            .chain(self.panel_chrome.all_mut())
        {
            p.update_fade(input.dt);
        }
        let mut order: Vec<(u64, &Panel)> = self
            .panels
            .iter()
            .enumerate()
            .map(|(i, p)| (i as u64, p))
            .chain([
                (PERF_LAYER, &self.perf),
                (chrome::BAR_LAYER, &self.chrome.bar),
                (chrome::CORNER_LAYER, &self.chrome.corner),
            ])
            .chain(self.panel_chrome.all())
            .filter(|(_, p)| p.shown())
            .collect();
        let distance = |p: &Panel| (p.pose.w_axis.truncate() - eye).length();
        order.sort_by(|(_, a), (_, b)| distance(b).total_cmp(&distance(a)));
        let mut layers = Vec::new();
        for (key, p) in order {
            let layered = input.layers && !p.in_eyes && p.fully_shown();
            match p.id {
                // Fully shown: the compositor draws it. Fading: drawn into
                // the eyes, where opacity works.
                Some(panel) if layered => layers.push(LayerDraw::Panel {
                    key,
                    panel,
                    px: p.px,
                    pose: p.pose,
                    size: p.size,
                }),
                _ => quads.extend(p.quad()),
            }
        }
        let any_ui = self.any_ui();
        if input.layers {
            layers.extend(self.pointer.ray_layers(eye, any_ui));
        } else {
            quads.extend(self.pointer.quads(eye, any_ui));
        }
        (quads, layers)
    }

    /// A panel that the rays are drawn for is up. The toast and the
    /// performance overlay don't count: they stay up while you watch.
    fn any_ui(&self) -> bool {
        self.panels
            .iter()
            .enumerate()
            .any(|(i, p)| i != TOAST && p.visible)
    }

    pub(super) fn subtitles(
        &mut self,
        renderer: &mut Renderer,
        time: f64,
        anchor: Mat4,
        s: &fp_core::view::ViewSettings,
        room: Option<&places::RoomFrame>,
        px_per_tan: Option<f32>,
    ) {
        if self.sub_shaper.is_none() {
            self.sub_shaper = self.sub_fonts.poll().pop();
        }
        let Some(p) = &self.playback else { return };
        let cues = p.player.subtitles();
        let pictures = cues.iter().any(|c| !c.bitmaps.is_empty());
        let video = p.player.info().video_stream();
        let pixel_aspect = video.map_or(1.0, |v| v.pixel_aspect as f32);
        let stereo = video
            .filter(|_| pictures)
            .and_then(|v| crate::subtitles::StereoSubs::of(v, &p.format, s.swap_eyes));
        self.subs_split = stereo.is_some();
        let key: String = cues
            .iter()
            .map(|c| format!("{}|{}|{};", c.start, c.text, c.bitmaps.len()))
            .chain(stereo.map(|s| format!("{s:?}")))
            .collect();
        self.subs.visible = !cues.is_empty();
        let aspect = SUBTITLE_POINTS[1] / SUBTITLE_POINTS[0];
        // A screen in your hand gets its subtitles under it (a room's own
        // screen takes precedence).
        let docked = self
            .docked_screen()
            .map(|(pose, _)| pose)
            .filter(|_| room.is_none());
        let d = match docked {
            // Held in your hand, the eye is taken to be this far from it.
            Some(_) if room.is_none() => chrome::dock::EYE,
            _ => room
                .map_or(s.subtitle_distance, |r| r.distance)
                .clamp(0.5, 20.0),
        };
        if let Some(r) = room {
            let spot = places::subtitle_spot(r, d, aspect);
            self.subs.size = spot.size;
            self.subs.pose = spot.pose;
        } else if let Some(screen) = docked {
            // Under the little screen, a little wider than it, in its turn.
            let (wide, high) = self
                .screen_geometry()
                .map_or((chrome::dock::WIDTH, 0.17), |(sh, _)| (sh.width, sh.height));
            let w = wide * 1.6;
            self.subs.size = Vec2::new(w, w * aspect);
            let drop = high / 2.0 + w * aspect / 2.0 + 0.01;
            self.subs.pose = screen * Mat4::from_translation(Vec3::new(0.0, -drop, 0.004));
        } else {
            let w = 0.9 * d;
            self.subs.size = Vec2::new(w, w * aspect);
            self.subs.pose = place(anchor, 0.0, d, -0.2 * d, 0.0);
        }
        // Painted at the size the eyes show it, so the glyphs are drawn
        // sharp rather than drawn big and minified.
        // The last size the eyes were known to have stands in while the runtime
        // isn't asking for a picture.
        self.sub_px_per_tan = px_per_tan.or(self.sub_px_per_tan);
        let wanted = subtitle_px(self.sub_px_per_tan, self.subs.size.x / d, self.subs_split);
        let px = settled_subtitle_px(self.subs.px, wanted);
        if self.subs.visible && px != self.subs.px {
            let ppp = px[0] as f32 / SUBTITLE_POINTS[0];
            match self.subs.resize(renderer, px, ppp) {
                Ok(()) => {
                    log::debug!("subtitles painted at {}x{}", px[0], px[1]);
                    // The new picture is empty.
                    self.sub_key.clear();
                }
                Err(e) => log::error!("subtitles: {e}"),
            }
        }
        if key != self.sub_key && self.subs.visible {
            self.sub_key = key;
            let textures = &mut self.sub_textures;
            let shaper = &mut self.sub_shaper;
            if let Err(e) = self.subs.paint(renderer, time, |ctx| {
                ui::player::subtitles(
                    ctx,
                    &cues,
                    textures,
                    shaper.as_mut(),
                    stereo.as_ref(),
                    pixel_aspect,
                );
            }) {
                log::error!("subtitles: {e}");
            }
        }
    }
}

/// Whether the screen and panels follow the head, given the Follow setting,
/// whether the video (if any) is flat, and whether anything is being dragged.
fn follows_head(setting: bool, flat: bool, dragging: bool) -> bool {
    setting && flat && !dragging
}

/// What a video's format, view settings, turn and pixel shape say about the
/// picture: the parts of `VideoParams` its screen's size and curve come from.
fn describe_video(video: &mut VideoParams, p: &Playback, shown: fp_core::view::ViewSettings) {
    video.format = p.format;
    video.settings = shown;
    if let Some(v) = p.player.info().video_stream() {
        video.rotation = v.rotation;
        video.pixel_aspect = v.pixel_aspect as f32;
    }
}

/// The subtitle panel's layout in egui points, whatever its size in pixels.
const SUBTITLE_POINTS: [f32; 2] = [1600.0, 320.0];
/// The subtitle panel's width in pixels with no eye size to go by, and the most
/// it is given.
const SUBTITLE_PX_MAX: u32 = 1600;
/// Widths are rounded up to a multiple of this.
const SUBTITLE_PX_STEP: u32 = 32;
/// The least it is given: smaller would be unreadable even if the eyes were
/// that coarse.
const SUBTITLE_PX_MIN: u32 = 480;
/// Pixels painted for each eye pixel the panel covers: a little over one, so
/// the glyphs are about as fine as the eye can show them and the eye pass
/// reads the picture near its sharpest level.
const SUBTITLE_PAINT_PER_EYE_PX: f32 = 1.25;

/// The subtitle panel's picture size in pixels. `px_per_tan` is how many
/// pixels of an eye image make a unit of tangent (see
/// `fp_render::math::pixels_per_tan`), `tan_width` how many of those units the
/// panel spans (its width over its distance), `split` that it holds a picture
/// for each eye side by side, each as wide as one picture would be. With no
/// eye size known the picture is the usual 1600 x 320.
fn subtitle_px(px_per_tan: Option<f32>, tan_width: f32, split: bool) -> [u32; 2] {
    let usual = [SUBTITLE_PX_MAX, SUBTITLE_POINTS[1] as u32];
    let Some(per_tan) = px_per_tan.filter(|p| p.is_finite() && *p > 0.0) else {
        return usual;
    };
    if !tan_width.is_finite() || tan_width <= 0.0 {
        return usual;
    }
    let eye_px = tan_width * per_tan * SUBTITLE_PAINT_PER_EYE_PX;
    let one = (eye_px.ceil() as u32)
        .div_ceil(SUBTITLE_PX_STEP)
        .saturating_mul(SUBTITLE_PX_STEP)
        .clamp(SUBTITLE_PX_MIN, SUBTITLE_PX_MAX);
    let width = if split { one * 2 } else { one };
    // The panel keeps its shape.
    let height = (width as f32 * SUBTITLE_POINTS[1] / SUBTITLE_POINTS[0]).round() as u32;
    [width, height]
}

/// `wanted`, unless it is a small step down from `current`: a field of view
/// that wavers by a few pixels would otherwise make the panel change size
/// back and forth, each time a new picture to paint.
fn settled_subtitle_px(current: [u32; 2], wanted: [u32; 2]) -> [u32; 2] {
    if wanted[0] > current[0] || wanted[0] + 2 * SUBTITLE_PX_STEP <= current[0] {
        wanted
    } else {
        current
    }
}

/// The Escape key, which closes the focused text field's keyboard.
pub(super) fn escape() -> egui::Event {
    key(egui::Key::Escape)
}

/// A key press.
pub(super) fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

/// Where the audio and subtitles picker goes: on the control bar's plane,
/// right on top of it, its right edge over the end of what the bar holds.
/// Its bottom edge meets the top of the bar's body, so neither panel covers
/// the other's controls.
fn above_bar(bar: &Panel, picker: &Panel) -> Mat4 {
    let room = ui::widgets::SHADOW_ROOM;
    // Metres per point of each.
    let b = bar.size.x / bar.points().x;
    let p = picker.size.x / picker.points().x;
    let top = bar.size.y / 2.0 - room.top as f32 * b;
    let right = bar.size.x / 2.0 - (room.right as f32 + ui::player::BAR_MARGIN as f32) * b;
    let x = right + room.right as f32 * p - picker.size.x / 2.0;
    let y = top + picker.size.y / 2.0;
    bar.pose * Mat4::from_translation(Vec3::new(x, y, 0.0))
}

/// Centred above the control bar's body, and above the picker, `above` metres
/// tall, when it is open.
fn toast_pose(bar: &Panel, above: Option<f32>, toast: &Panel) -> Mat4 {
    let room = ui::widgets::SHADOW_ROOM.top as f32 * bar.size.x / bar.points().x;
    let y = bar.size.y / 2.0 - room + above.unwrap_or(0.0) + toast.size.y / 2.0;
    bar.pose * Mat4::from_translation(Vec3::new(0.0, y, 0.0))
}

/// Metres away `panel` starts.
fn usual_distance(panel: usize) -> f32 {
    match panel {
        MAIN => MAIN_DISTANCE,
        ADJUST => ADJUST_DISTANCE,
        PERF => PERF_DISTANCE,
        _ => BAR_DISTANCE,
    }
}

/// How far a panel is from the viewer: where it usually is, `moved` metres
/// nearer (negative) or further, kept within [`PANEL_RANGE`] (a settings file
/// can hold anything).
fn panel_distance(usual: f32, moved: f32) -> f32 {
    (usual + moved).clamp(*PANEL_RANGE.start(), *PANEL_RANGE.end())
}

/// Degrees left of forward where the performance overlay starts: just clear of
/// the library panel and the control bar, given their widths and its own.
fn perf_start_yaw(library: f32, library_d: f32, bar: f32, bar_d: f32, perf: f32) -> f32 {
    let half = |width: f32, distance: f32| (width / 2.0 / distance).atan().to_degrees();
    half(library, library_d).max(half(bar, bar_d)) + 2.0 + half(perf, PERF_DISTANCE)
}

/// The overlay's rows for the open video; none without a picture.
fn perf_video(
    p: &Playback,
    st: &fp_media::Stats,
    on_network: bool,
    readings: &crate::perf::Readings,
    hardware_decoding: bool,
) -> Option<ui::perf::Video> {
    use ui::perf::VideoState;
    let v = p.player.info().video_stream()?;
    let (decoder, _) = ui::player::stream_info_labels(v, st, hardware_decoding);
    let state = match p.player.state() {
        PlayerState::Playing => VideoState::Playing(readings.video_fps()),
        PlayerState::Paused => VideoState::Paused,
        PlayerState::Buffering => VideoState::Buffering,
        PlayerState::Ended | PlayerState::Failed => VideoState::Ended,
    };
    let mb_s = readings.mb_per_s();
    Some(ui::perf::Video {
        state,
        film_fps: v.fps as f32,
        dropped: st.frames_dropped,
        stalls: st.stalls,
        stuttered: readings.stuttered(),
        decoder,
        damage: playback::damage_total(st),
        buffer: (st.read_ahead.0 as f32, st.read_ahead.1 as f32),
        read_to_end: st.read_to_end,
        network: on_network.then(|| ui::perf::Network {
            mb_s,
            idle: (st.read_enough || st.read_to_end) && mb_s.is_none_or(|m| m < 0.05),
            retries: readings.retries(),
            retried: readings.retried(),
        }),
        last_jump: st.last_jump.map(|j| ui::perf::LastJump {
            skip: j.skip,
            picture_s: j.picture.as_secs_f32(),
            first_read_s: j.first_read.map(|d| d.as_secs_f32()),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_pauses_while_the_picture_is_dragged() {
        assert!(follows_head(true, true, false));
        // By a grip on a panel, by grip and trigger on the picture or by a
        // window control: whatever drags, the anchor stays put.
        assert!(!follows_head(true, true, true));
        // And it is for flat screens that have it on.
        assert!(!follows_head(true, false, false));
        assert!(!follows_head(false, true, false));
    }

    #[test]
    fn subtitles_are_painted_at_the_size_the_eyes_show_them() {
        // A panel 0.9 of a unit of tangent across.
        let px = |per_tan: Option<f32>, split| subtitle_px(per_tan, 0.9, split);
        assert_eq!(px(None, false), [1600, 320], "no eye size known");
        assert_eq!(px(None, true), [1600, 320]);
        // 1440 pixel eyes (about 507 per tangent) and 2160 pixel eyes.
        assert_eq!(px(Some(507.0), false), [576, 115]);
        assert_eq!(px(Some(760.0), false), [864, 173]);
        // Never finer than the usual size, nor coarser than readable.
        assert_eq!(px(Some(4000.0), false), [1600, 320]);
        assert_eq!(px(Some(100.0), false), [480, 96]);
        // Each eye's half is as wide as one picture would be.
        assert_eq!(px(Some(507.0), true), [1152, 230]);
        assert_eq!(px(Some(4000.0), true), [3200, 640]);
        // Nonsense falls back to the usual size.
        for bad in [f32::NAN, 0.0, -3.0, f32::INFINITY] {
            assert_eq!(px(Some(bad), false), [1600, 320], "{bad}");
        }
        assert_eq!(subtitle_px(Some(507.0), 0.0, false), [1600, 320]);
        // Layout in points stays 1600 x 320 whatever the pixels.
        let [w, h] = px(Some(507.0), false);
        let ppp = w as f32 / SUBTITLE_POINTS[0];
        assert!((h as f32 / ppp - SUBTITLE_POINTS[1]).abs() < 1.0);
    }

    #[test]
    fn the_subtitle_panel_ignores_a_wavering_field_of_view() {
        let size = |per_tan: f32| subtitle_px(Some(per_tan), 0.9, false);
        let (at, one_up, two_up) = (size(507.0), size(520.0), size(550.0));
        assert_eq!((at, one_up, two_up), ([576, 115], [608, 122], [640, 128]));
        // A step up is taken at once, a single step down is not.
        assert_eq!(settled_subtitle_px(at, one_up), one_up);
        assert_eq!(settled_subtitle_px(one_up, at), one_up);
        // Two steps down are, and so is a change of kind such as 3D pictures.
        assert_eq!(settled_subtitle_px(two_up, at), at);
        let split = subtitle_px(Some(507.0), 0.9, true);
        assert_eq!(settled_subtitle_px(at, split), split);
        assert_eq!(settled_subtitle_px(split, at), at);
        assert_eq!(settled_subtitle_px(at, at), at);
    }

    #[test]
    fn a_toast_sits_above_the_bar_and_any_picker() {
        let mut bar = growing_panel(BAR_PX, BAR_PX[0] as f32 * BAR_M_PER_PX, 1.25);
        let mut toast = growing_panel([1100, 90], 1100.0 * BAR_M_PER_PX, 1.25);
        bar.pose = place(Mat4::IDENTITY, 0.0, BAR_DISTANCE, -0.32, 22.0);
        let height = |pose: Mat4| bar.pose.inverse().transform_point3(pose.w_axis.truncate());
        toast.pose = toast_pose(&bar, None, &toast);
        let alone = height(toast.pose);
        assert!(alone.x.abs() < 1e-5, "centred: {alone}");
        assert!(
            alone.y - toast.size.y / 2.0 > 0.0,
            "clear of the bar's middle"
        );
        let with_picker = height(toast_pose(&bar, Some(0.5), &toast));
        assert!((with_picker.y - alone.y - 0.5).abs() < 1e-5);
    }

    #[test]
    fn the_performance_overlay_starts_in_view_and_clear_of_the_other_panels() {
        let half = |width: f32, distance: f32| (width / 2.0 / distance).atan().to_degrees();
        let library = floating_panel([1600, 1000], 1.8, 1.25);
        for scale in [0.8, 1.0, 1.5] {
            let mut bar = growing_panel(BAR_PX, BAR_PX[0] as f32 * BAR_M_PER_PX, 1.25);
            let mut perf = perf_panel();
            bar.set_scale(scale);
            perf.set_scale(scale);
            let yaw = perf_start_yaw(
                library.size.x,
                MAIN_DISTANCE,
                bar.size.x,
                BAR_DISTANCE,
                perf.size.x,
            );
            let inner = yaw - half(perf.size.x, PERF_DISTANCE);
            assert!(
                inner > half(library.size.x, MAIN_DISTANCE),
                "{scale}: {inner}"
            );
            assert!(inner > half(bar.size.x, BAR_DISTANCE), "{scale}: {inner}");
            if scale == 1.0 {
                let outer = yaw + half(perf.size.x, PERF_DISTANCE);
                assert!(outer < 55.0, "{outer}° is beyond the edge of the view");
            }
        }
    }

    #[test]
    fn the_picker_sits_on_the_bar_without_covering_it() {
        for scale in [1.0, 1.4] {
            let mut bar = growing_panel(BAR_PX, BAR_PX[0] as f32 * BAR_M_PER_PX, 1.25);
            let mut picker = growing_panel([850, 560], 850.0 * BAR_M_PER_PX, 1.25);
            bar.set_scale(scale);
            picker.set_scale(scale);
            bar.pose = place(Mat4::IDENTITY, 0.0, BAR_DISTANCE, -0.32, 22.0);
            picker.pose = above_bar(&bar, &picker);
            let room = ui::widgets::SHADOW_ROOM;
            let pts = bar.points();
            let bar_top = bar.world_point(egui::pos2(pts.x / 2.0, room.top as f32));
            let picker_pts = picker.points();
            let picker_bottom = picker.world_point(egui::pos2(picker_pts.x / 2.0, picker_pts.y));
            // In the same plane, the picker's bottom edge on the bar's body's
            // top one, and its body's right end over the end of the bar's
            // content.
            let axis = |v: Vec3| bar.pose.transform_vector3(v);
            assert!(axis(Vec3::Z).abs_diff_eq(picker.pose.transform_vector3(Vec3::Z), 1e-5));
            assert!((bar_top - picker_bottom).dot(axis(Vec3::Z)).abs() < 1e-5);
            let gap = (picker_bottom - bar_top).dot(axis(Vec3::Y));
            assert!(gap.abs() < 1e-4, "{scale}: {gap}");
            let margin = room.right as f32 + ui::player::BAR_MARGIN as f32;
            let content_end = bar.world_point(egui::pos2(pts.x - margin, pts.y / 2.0));
            let body_end = picker.world_point(egui::pos2(
                picker_pts.x - room.right as f32,
                picker_pts.y / 2.0,
            ));
            let off = (content_end - body_end).dot(axis(Vec3::X));
            assert!(off.abs() < 1e-4, "{scale}: {off}");
        }
    }
}
