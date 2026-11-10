//! Headless preview: renders the app offscreen with a simulated head and
//! right controller, driven by a small script, and saves screenshots.
//!
//! Script commands (one per line; `#` starts a comment at the start of a
//! line or after a space, so a Jellyfin address's `#item=` stays):
//!
//! | command | effect |
//! |---|---|
//! | `frames N` | run N frames |
//! | `wait SECONDS` | run frames in real time |
//! | `wait-playing [TIMEOUT]` | run until a video is playing |
//! | `look YAW PITCH [ROLL]` | head direction (and roll, to lie on your side) in degrees |
//! | `recenter` | recenter on where the head faces |
//! | `aim YAW PITCH` | controller ray direction in degrees |
//! | `point PANEL X Y` | aim at a panel (`main`, `bar`, `adjust`, `keyboard`, `tracks`, `perf`, `window-bar`, `window-corner`, and the panels' own `main-bar`, `adjust-bar`, `perf-bar`, `keyboard-bar`, `main-corner`) at fractional X, Y (outside 0 to 1 is past its edge) |
//! | `click` | pull and release the trigger |
//! | `button a/b/x/y/menu/stick/shoulder` | press and release a right-hand button |
//! | `stick X Y FRAMES` | hold the thumbstick |
//! | `squeeze` | squeeze and release the grip |
//! | `grip V` / `trigger V` | hold the grip / trigger at V (0..1) until changed |
//! | `status` | print what is playing and the view settings |
//! | `dashboard` | press the dashboard button's action (`status` counts them) |
//! | `multitask` | press the bar's Multitask key's action: the screen and the library side by side, or back (`status` shows `multitask=1`) |
//! | `expect-dashboard N` | fail the script unless the dashboard button was pressed N times |
//! | `expect FIELD OP VALUE` | fail the script unless the number after `FIELD=` in the status line (`width`, `distance`, `yaw`, ...) is `<` or `>` VALUE, `==` it (to 0.01) or `~` it (within 10 %) |
//! | `open LOCATION` | open a file or URL |
//! | `testcard HEIGHT` | make the test card (HEIGHT 1080 or 2160) and open it |
//! | `hide-controls` | hide the player's panels, as the Hide button does |
//! | `close` | close the open video, as its Close button does |
//! | `seek SECONDS` | play the open video from SECONDS |
//! | `quality NAME` | play `automatic`, `original` or a transcode by its label (`1440p`) |
//! | `loop video` / `loop playlist` | turn Loop video or Up next's Loop on or off |
//! | `autoplay` | turn Autoplay on or off |
//! | `open-list` | open the library's videos as a grid does, from the first |
//! | `shuffle` | shuffle Up next, keeping the current video first |
//! | `shot NAME` | save the left eye as OUT_DIR/NAME.png |
//! | `sbs NAME` | save both eyes side by side |
//! | `screen NAME [TAB]` | show a browser screen (`home`, `library`, `sources`, `settings` and its tab) |
//! | `adjust [TAB]` | open the adjustments panel (at tab TAB) |
//! | `tracks` | open the audio and subtitles picker |
//! | `filters` | open the Filters drawer of the source page shown (a Jellyfin grid, a feed) |
//! | `chroma` | turn chroma key on for the open video |
//! | `flat` / `flat off` | show the open video as flat 2D, or back as detected |
//! | `place ID` | choose a place as the picker's chips do: `cinema`, `studio`, `void` (Plain) or `room` (Your room) |
//! | `seat N` | sit in the chosen place's seat N (a number from 0, or `front`, `middle`, `back`) |
//! | `house LEVEL` / `house auto` | hold the house lights at LEVEL (0 dark to 1 full), or let playback dim and raise them again |
//! | `panel PANEL NAME` | save a panel's image flat, pixel for pixel, as OUT_DIR/NAME.png |

use crate::app::{App, FrameInput};
use crate::{Args, Error};
use fp_render::capture::OffscreenEyes;
use fp_render::math::{Fov, pixels_per_tan, projection, view};
use fp_render::{EyeView, Gpu, Renderer};
use fp_xr::Hand;
use glam::{EulerRot, Quat, Vec2, Vec3};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_SCRIPT: &str = "wait 1.5\nshot home\n";
const EYE_HEIGHT: f32 = 1.6;
const IPD: f32 = 0.064;

struct Sim {
    app: App,
    renderer: Renderer,
    eyes: OffscreenEyes,
    size: u32,
    head: Quat,
    hand: Hand,
    start: Instant,
    last: Instant,
    frames: u64,
    gpu_ms: crate::perf::GpuWindow,
    out: std::path::PathBuf,
}

fn dir_quat(yaw: f32, pitch: f32) -> Quat {
    Quat::from_euler(EulerRot::YXZ, yaw.to_radians(), pitch.to_radians(), 0.0)
}

/// The seat a `seat` command names: a number from 0, or a seat's name.
fn seat_arg(place: fp_render::env::Place, arg: &str) -> Option<usize> {
    arg.parse().ok().or_else(|| {
        let name = fp_render::env::SeatName::from_id(arg)?;
        place.layout().seat_index(name)
    })
}

impl Sim {
    fn head_pos(&self) -> Vec3 {
        Vec3::new(0.0, EYE_HEIGHT, 0.0)
    }

    fn hand_pos(&self) -> Vec3 {
        self.head_pos() + Vec3::new(0.18, -0.3, -0.25)
    }

    fn step(&mut self) -> Result<bool, Error> {
        let dt = self.last.elapsed().as_secs_f32().clamp(0.001, 0.1);
        self.last = Instant::now();
        self.renderer.begin_frame()?;
        self.gpu_ms.push(self.renderer.gpu_times());
        let hands = [Hand::default(), self.hand];
        let head = Some((self.head_pos(), self.head));
        let proj = projection(Fov::symmetric(100.0), 0.05, 200.0);
        let out = self.app.frame(
            &mut self.renderer,
            FrameInput {
                time: self.start.elapsed().as_secs_f64(),
                dt,
                head,
                hands,
                passthrough_available: true,
                layers: false,
                frame_ms: None,
                display_hz: None,
                display_lead: Duration::ZERO,
                recentred: false,
                eye_px_per_tan: Some(pixels_per_tan(&proj, self.size)),
                dashboard: true,
            },
        );
        if let Err(e) = self.renderer.set_video(out.frame.as_ref()) {
            log::warn!("set_video: {e}");
            let _ = self.renderer.set_video(None);
        }
        let right = self.head * Vec3::X * (IPD / 2.0);
        let ev = |p: Vec3| EyeView {
            view: view(p, self.head),
            proj,
        };
        let eyes = [ev(self.head_pos() - right), ev(self.head_pos() + right)];
        self.renderer
            .draw(&self.eyes.targets(), &eyes, &out.video, &out.quads)?;
        self.renderer.end_frame()?;
        self.frames += 1;
        Ok(out.quit)
    }

    /// The library's videos, as their cards open them, by title.
    fn library_requests(&self) -> Vec<crate::playback::OpenRequest> {
        self.app
            .library()
            .search(&fp_library::Query {
                sort: fp_library::Sort::Title,
                ..Default::default()
            })
            .unwrap_or_default()
            .iter()
            .map(crate::ui::library::request)
            .collect()
    }

    fn run_frames(&mut self, n: u32) -> Result<(), Error> {
        for _ in 0..n {
            self.step()?;
            std::thread::sleep(Duration::from_millis(11));
        }
        Ok(())
    }

    fn save(&mut self, name: &str, both: bool) -> Result<(), Error> {
        self.renderer.wait_idle();
        let l = self.eyes.read(&self.renderer, 0)?;
        let (w, h) = (self.size, self.size);
        let img = if both {
            let r = self.eyes.read(&self.renderer, 1)?;
            let mut out = image::RgbaImage::new(w * 2, h);
            for y in 0..h {
                for x in 0..w {
                    let i = ((y * w + x) * 4) as usize;
                    out.put_pixel(x, y, image::Rgba([l[i], l[i + 1], l[i + 2], 255]));
                    out.put_pixel(x + w, y, image::Rgba([r[i], r[i + 1], r[i + 2], 255]));
                }
            }
            out
        } else {
            let mut px = l;
            for a in px.chunks_mut(4) {
                a[3] = 255;
            }
            image::RgbaImage::from_raw(w, h, px).ok_or("bad image size")?
        };
        let path = self.out.join(format!("{name}.png"));
        img.save(&path)?;
        println!("saved {}", path.display());
        Ok(())
    }

    fn save_panel(&mut self, panel: &str, name: &str) -> Result<(), Error> {
        let id = self
            .app
            .panel_id(panel)
            .ok_or(format!("panel {panel} is not visible"))?;
        let (w, h, mut px) = self.renderer.read_panel(id)?;
        // egui paints premultiplied alpha; PNG wants it straight.
        for p in px.chunks_mut(4) {
            let a = p[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let img = image::RgbaImage::from_raw(w, h, px).ok_or("bad panel size")?;
        let path = self.out.join(format!("{name}.png"));
        img.save(&path)?;
        println!("saved {}", path.display());
        Ok(())
    }

    fn command(&mut self, line: &str) -> Result<bool, Error> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| -> Result<f32, Error> {
            Ok(parts
                .get(i)
                .ok_or(format!("missing argument in {line:?}"))?
                .parse::<f32>()?)
        };
        match parts.first().copied() {
            None => {}
            Some("frames") => self.run_frames(num(1)? as u32)?,
            Some("wait") => {
                let until = Instant::now() + Duration::from_secs_f32(num(1)?);
                while Instant::now() < until {
                    if self.step()? {
                        return Ok(true);
                    }
                    std::thread::sleep(Duration::from_millis(11));
                }
            }
            Some("wait-playing") => {
                let timeout = num(1).unwrap_or(20.0);
                let until = Instant::now() + Duration::from_secs_f32(timeout);
                loop {
                    self.step()?;
                    let playing = self
                        .app
                        .playback
                        .as_ref()
                        .is_some_and(|p| p.player.current_frame().is_some());
                    if playing {
                        break;
                    }
                    if Instant::now() > until {
                        return Err(format!(
                            "not playing after {timeout}s: {:?}",
                            self.app.ui.open_error
                        )
                        .into());
                    }
                    std::thread::sleep(Duration::from_millis(11));
                }
            }
            Some("screen") => {
                use crate::ui::{Screen, SettingsTab};
                let screen = match parts.get(1).copied().unwrap_or("home") {
                    "home" => Screen::Home,
                    "history" => Screen::History,
                    "sources" => Screen::Sources,
                    "settings" => Screen::Settings,
                    "playlists" => Screen::Playlists,
                    "upnext" => Screen::UpNext,
                    other => return Err(format!("no screen {other:?}").into()),
                };
                let ui = &mut self.app.ui;
                ui.screen = screen;
                ui.details = None;
                ui.remap_open = None;
                ui.remap_grip = false;
                // As the tab does: Sources shows the list, not a source or
                // a form.
                ui.source = None;
                ui.source_form = None;
                ui.pin_form = None;
                if let Some(tab) = parts.get(2) {
                    ui.settings_tab = match *tab {
                        "general" => SettingsTab::General,
                        "playback" => SettingsTab::Playback,
                        "position" => SettingsTab::VideoPosition,
                        "places" => SettingsTab::Places,
                        "sound" => SettingsTab::Sound,
                        "passthrough" => SettingsTab::Passthrough,
                        "controller" => SettingsTab::Controller,
                        // The Controller tab with the right A button's choices open.
                        "controller-open" => {
                            ui.remap_open = Some(crate::ui::RemapSlot::Button(
                                crate::bindings::RIGHT,
                                crate::bindings::Button::South,
                            ));
                            SettingsTab::Controller
                        }
                        // ...with the right Y's grip choices open.
                        "controller-grip" => {
                            ui.remap_grip = true;
                            ui.remap_open = Some(crate::ui::RemapSlot::Grip(
                                crate::bindings::RIGHT,
                                crate::bindings::Button::North,
                            ));
                            SettingsTab::Controller
                        }
                        // ...and with the left thumbstick's choices open.
                        "controller-stick" => {
                            ui.remap_open = Some(crate::ui::RemapSlot::Axis(
                                crate::bindings::LEFT,
                                crate::bindings::Axis::StickX,
                            ));
                            SettingsTab::Controller
                        }
                        "haptics" => SettingsTab::Haptics,
                        "remote" => SettingsTab::Remote,
                        "updates" => SettingsTab::Updates,
                        "about" => SettingsTab::About,
                        other => return Err(format!("no settings tab {other:?}").into()),
                    };
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            Some("panel") => {
                let name = parts.get(1).copied().unwrap_or("main");
                let file = parts.get(2).copied().unwrap_or(name);
                // A missing panel shouldn't lose the rest of the shots.
                if let Err(e) = self.save_panel(name, file) {
                    log::warn!("{line}: {e}");
                }
            }
            Some("adjust") => {
                self.app.ui.adjust_open = true;
                // `adjust N` opens tab N.
                if let Some(tab) = parts.get(1).and_then(|t| t.parse().ok()) {
                    self.app.ui.adjust_tab = tab;
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            Some("filters") => {
                use crate::ui::SourcePage;
                match self.app.ui.source.as_mut().map(|s| &mut s.page) {
                    Some(SourcePage::Feed(f)) => f.filters_open = true,
                    Some(SourcePage::Catalog(j)) => j.show_filters(true),
                    _ => return Err("no Jellyfin grid or feed is open".into()),
                }
                self.app.repaint_all();
                self.run_frames(6)?;
            }
            // The tab of the library page open: `library-tab collections`.
            Some("library-tab") => {
                use crate::ui::SourcePage;
                let tab = parts.get(1).ok_or("library-tab needs a tab")?;
                let shown = match self.app.ui.source.as_mut().map(|s| &mut s.page) {
                    Some(SourcePage::Catalog(j)) => j.show_tab(tab),
                    _ => false,
                };
                if !shown {
                    return Err("library-tab needs a library page open".into());
                }
                self.app.repaint_all();
                self.run_frames(6)?;
            }
            Some("tracks") => {
                self.app.ui.tracks_open = true;
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // Pauses the open video, so a short test clip doesn't end (and
            // hide the player's panels) before the shots that follow.
            Some("pause") => {
                if let Some(p) = &self.app.playback {
                    p.player.pause();
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            Some("close") => {
                self.app.apply(crate::ui::Action::ClosePlayback);
                self.run_frames(3)?;
            }
            Some("hide-controls") => {
                self.app.apply(crate::ui::Action::HideControls);
                self.run_frames(6)?;
            }
            // The library's videos as a grid opens them, from the first.
            Some("open-list") => {
                let list = self.library_requests();
                self.app.apply(crate::ui::Action::OpenList(list, 0));
                self.run_frames(3)?;
            }
            Some("autoplay") => {
                self.app.apply(crate::ui::Action::ToggleAutoplay);
                self.run_frames(3)?;
            }
            Some("loop") => {
                let action = match parts.get(1).copied() {
                    Some("video") => crate::ui::Action::ToggleLoopVideo,
                    Some("playlist") => crate::ui::Action::ToggleLoopPlaylist,
                    _ => return Err("loop needs video or playlist".into()),
                };
                self.app.apply(action);
                self.run_frames(3)?;
            }
            Some("seek") => {
                let at = num(1)? as f64;
                if let Some(p) = &self.app.playback {
                    p.player.seek(at, true);
                }
                self.run_frames(2)?;
            }
            Some("quality") => {
                use crate::playback::Quality;
                let name = parts.get(1).ok_or("quality needs a name")?;
                let quality = match *name {
                    "automatic" => Quality::Automatic,
                    "original" => Quality::Original,
                    label => {
                        let p = self.app.playback.as_ref().ok_or("no video open")?;
                        let t = p.targets.iter().find(|t| t.label == label);
                        Quality::Transcode(t.ok_or(format!("no transcode {label}"))?.id.clone())
                    }
                };
                self.app.apply(crate::ui::Action::SelectQuality(quality));
                self.run_frames(3)?;
            }
            // Passthrough videos are fully unlocked.
            Some("unlocked") => {
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The open video uses its own mask, as an `_alpha` video does.
            Some("mask") => {
                if let Some(p) = &mut self.app.playback {
                    p.format.alpha_packed = true;
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The notice for software decoding falling behind, as a 10-bit
            // AV1 video gets it.
            Some("slow-decode") => {
                if let Some(p) = &mut self.app.playback {
                    let v = p.player.info().video_stream().cloned();
                    if let Some(mut v) = v {
                        v.codec = "av1".into();
                        v.bit_depth = 10;
                        v.pixel_format = "yuv420p10le".into();
                        p.notice = Some(crate::playback::slow_decode_notice(
                            &v,
                            true,
                            None,
                            &Default::default(),
                        ));
                    }
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // `haptics on` / `haptics off`, as Settings › General does.
            Some("haptics") => {
                self.app.settings.haptics_enabled = parts.get(1) != Some(&"off");
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // `remote on` / `remote off`: the phone and browser remote, as
            // Settings › Remote control does.
            Some("remote") => {
                self.app.settings.remote.web_enabled = parts.get(1) != Some(&"off");
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // `subtitles always ar`, as Settings › Sound and subtitles does.
            Some("subtitles") => {
                use crate::settings::SubtitleMode;
                self.app.settings.subtitles = match parts.get(1).copied() {
                    Some("off") => SubtitleMode::Off,
                    Some("always") => SubtitleMode::Always,
                    _ => SubtitleMode::Automatic,
                };
                if let Some(lang) = parts.get(2) {
                    self.app.settings.subtitle_language = lang.to_string();
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The page offered after a crash, its report made from this
            // run's log as if it had ended in a crash; `crash-report shown`
            // opens the report.
            Some("crash-report") => {
                let log = std::fs::read_to_string(crate::logger::log_path()).unwrap_or_default();
                self.app.offer_crash_report(&format!(
                    "{log}          [FATAL] crashed with signal 11
"
                ));
                if let Some(c) = &mut self.app.ui.crash {
                    c.shown = parts.get(1) == Some(&"shown");
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // Lists the subtitle languages in Settings › Sound and subtitles.
            Some("languages") => {
                self.app.ui.languages_open = true;
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The open video as a flat 2D picture (the test clips are named
            // as 180° videos), which is what a place surrounds; `flat off`
            // goes back to the format the video's name gives.
            Some("flat") => {
                use fp_core::format::{Projection, StereoLayout, VideoFormat};
                let format = if parts.get(1) == Some(&"off") {
                    None
                } else {
                    Some(VideoFormat::new(Projection::Flat, StereoLayout::Mono))
                };
                self.app.apply(crate::ui::Action::SetFormat(format));
                self.app.repaint_all();
                self.run_frames(4)?;
            }
            // `place cinema`, `place studio`, `place void` or `place room`, as
            // the picker's chips choose.
            Some("place") => {
                use crate::ui::player::{PlaceChoice, choose_place};
                use fp_render::env::Place;
                let id = parts.get(1).ok_or("place needs a name")?;
                let choice = match *id {
                    "void" | "plain" => PlaceChoice::Plain,
                    "room" => PlaceChoice::YourRoom,
                    other => {
                        let place = Place::from_id(other).ok_or(format!("no place {other}"))?;
                        PlaceChoice::Place(place)
                    }
                };
                choose_place(&mut self.app.settings, choice);
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // `seat 0` or `seat front`: where the chosen place seats you.
            Some("seat") => {
                let place = fp_render::env::Place::from_id(&self.app.settings.environment)
                    .ok_or("seat needs a place: place cinema first")?;
                let arg = parts.get(1).ok_or("seat needs a number or a name")?;
                let index = seat_arg(place, arg).ok_or("no such seat")?;
                crate::ui::player::pick_seat(&mut self.app.settings, place, index);
                self.app.repaint_all();
                self.run_frames(2)?;
            }
            // `house 0` holds the house lights dark and `house 1` full, as a
            // short test clip ends and would bring them up; `house auto` lets
            // playback dim and raise them again.
            Some("house") => {
                self.app.house_override = match parts.get(1).copied() {
                    Some("auto") => None,
                    _ => Some(num(1)?.clamp(0.0, 1.0)),
                };
                self.run_frames(2)?;
            }
            // Turns the global chroma key on, or off with `chroma-global off`
            // (Settings > Passthrough).
            Some("chroma-global") => {
                self.app.settings.default_view.chroma_key = parts.get(1) != Some(&"off");
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // Turns chroma key on for the open video (the Passthrough tab).
            Some("chroma") => {
                if let Some(p) = &mut self.app.playback {
                    p.settings.chroma_key = true;
                    p.settings.key_own = Some(true);
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // `look YAW PITCH [ROLL]`, degrees; a roll lies you on your side.
            Some("look") => {
                let roll = num(3).unwrap_or(0.0).to_radians();
                self.head = dir_quat(num(1)?, num(2)?) * Quat::from_rotation_z(roll);
            }
            Some("recenter") => {
                self.app.apply(crate::ui::Action::Recenter);
                self.run_frames(2)?;
            }
            // Presses the dashboard button as its action does (the button
            // itself is clicked with `point` and `click`).
            Some("dashboard") => {
                self.app.apply(crate::ui::Action::OpenDashboard);
                self.run_frames(2)?;
            }
            // Browse beside the video, or back (the key itself is clicked with
            // `point` and `click`).
            Some("multitask") => {
                self.app.apply(crate::ui::Action::ToggleMultitask);
                self.run_frames(6)?;
            }
            // Fails the script unless the dashboard button was pressed N times.
            Some("expect-dashboard") => {
                let want = num(1)? as u32;
                let got = self.app.dashboard_requests();
                if got != want {
                    return Err(format!("{got} dashboard requests, expected {want}").into());
                }
            }
            Some("aim") => {
                self.hand.aim = Some((self.hand_pos(), dir_quat(num(1)?, num(2)?)));
            }
            Some("point") => {
                let name = parts.get(1).copied().unwrap_or("main");
                let target = self
                    .app
                    .panel_point(name, num(2)?, num(3)?)
                    .ok_or(format!("panel {name} is not visible"))?;
                let dir = (target - self.hand_pos()).normalize();
                self.hand.aim = Some((self.hand_pos(), Quat::from_rotation_arc(Vec3::NEG_Z, dir)));
                self.run_frames(2)?;
            }
            Some("click") => {
                self.hand.trigger = 1.0;
                self.run_frames(2)?;
                self.hand.trigger = 0.0;
                self.run_frames(3)?;
            }
            Some("button") => {
                let b = parts.get(1).copied().unwrap_or("primary");
                // The simulated hand is the right one: A/south plays, B/east
                // goes back ("primary"/"secondary" are the old names).
                let set = |h: &mut Hand, v: bool| match b {
                    "east" | "b" | "secondary" => h.east = v,
                    "north" | "y" => h.north = v,
                    "west" | "x" => h.west = v,
                    "stick" => h.stick_click = v,
                    "menu" => h.menu = v,
                    "shoulder" => h.shoulder = v,
                    _ => h.south = v,
                };
                set(&mut self.hand, true);
                self.run_frames(2)?;
                set(&mut self.hand, false);
                self.run_frames(2)?;
            }
            Some("stick") => {
                self.hand.stick = Vec2::new(num(1)?, num(2)?);
                self.run_frames(num(3).unwrap_or(10.0) as u32)?;
                self.hand.stick = Vec2::ZERO;
                self.run_frames(2)?;
            }
            Some("grip") => self.hand.squeeze = num(1)?,
            Some("trigger") => self.hand.trigger = num(1)?,
            Some("status") => println!("status: {}", self.app.status_line()),
            Some("expect") => {
                let field = parts.get(1).ok_or("expect needs a field")?;
                let op = parts.get(2).ok_or("expect needs <, >, == or ~")?;
                let want = num(3)?;
                let status = self.app.status_line();
                let got = status_field(&status, field)
                    .ok_or(format!("no number {field} in {status:?}"))?;
                if !compares(got, op, want)? {
                    let said = format!("expected {field} {op} {want}, got {got}: {status}");
                    return Err(said.into());
                }
                println!("expect: {field} {op} {want} (is {got})");
            }
            // The GPU's time per frame since the last `gpu`: `gpu`.
            Some("gpu") => {
                if let Some(line) = self.gpu_ms.take_line() {
                    println!("{line}");
                }
            }
            Some("squeeze") => {
                self.hand.squeeze = 1.0;
                self.run_frames(2)?;
                self.hand.squeeze = 0.0;
                self.run_frames(2)?;
            }
            Some("open") => {
                // The rest of the line, so paths may contain spaces.
                let arg = line.trim().strip_prefix("open").unwrap_or("").trim();
                if arg.is_empty() {
                    return Err("open needs a location".into());
                }
                let loc = crate::location(arg);
                self.app.open(crate::playback::OpenRequest {
                    location: loc,
                    ..Default::default()
                });
            }
            // The generated test card as a picture of its own: `testcard 2160`.
            Some("testcard") => {
                let height = num(1)? as u32;
                let path = crate::testcard::write_png(height)?;
                self.app.open(crate::playback::OpenRequest {
                    location: path.display().to_string(),
                    ..Default::default()
                });
            }
            // Opens a video of a source as its card does, the source picking
            // the stream: `open-in SOURCE LOCATION` (a feed scene's address).
            Some("open-in") => {
                let source = parts.get(1).ok_or("open-in needs a source")?.to_string();
                let location = parts.get(2).ok_or("open-in needs a location")?.to_string();
                let name = location.rsplit('/').next().unwrap_or_default().to_string();
                let entry = fp_core::source::Entry::new(
                    name,
                    location.clone(),
                    fp_core::source::EntryKind::Video,
                );
                self.app.open(crate::playback::OpenRequest {
                    location,
                    source_id: Some(source),
                    entry: Some(entry),
                    ..Default::default()
                });
            }
            // Browses a folder of a source, as the Sources screen does:
            // `browse SOURCE [LOCATION]` (the location may contain spaces).
            Some("browse") => {
                let source = parts.get(1).ok_or("browse needs a source")?.to_string();
                let rest = line.trim().strip_prefix("browse").unwrap_or("").trim();
                let location = rest.strip_prefix(source.as_str()).unwrap_or("").trim();
                self.app.ui.screen = crate::ui::Screen::Sources;
                self.app.ui.details = None;
                self.app
                    .browse(source, (!location.is_empty()).then(|| location.to_string()));
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The order of the folder being browsed: `browse-sort newest`.
            Some("browse-sort") => {
                let name = parts.get(1).ok_or("browse-sort needs an order")?;
                let sort: fp_sources::EntrySort = serde_json::from_str(&format!("\"{name}\""))
                    .map_err(|_| format!("no browse order {name:?}"))?;
                self.app.settings.browse_sort = sort;
                if let Some(b) = self.app.ui.folder_mut() {
                    fp_sources::sort_entries_by(&mut b.entries, sort);
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // Opens a source in its own browser, as Home's row does:
            // `open-source SOURCE` (`on-device` for the headset itself).
            Some("open-source") => {
                let source = parts
                    .get(1)
                    .ok_or("open-source needs a source")?
                    .to_string();
                self.app.apply(crate::ui::Action::OpenSource {
                    source,
                    back_to: crate::ui::Screen::Home,
                });
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // Searches the catalog open, as typing in its header does.
            Some("catalog-search" | "jellyfin-search") => {
                let text = line.trim().split_once(' ').map_or("", |(_, text)| text);
                use crate::ui::catalog::Body;
                if let Some(crate::ui::SourceView {
                    page: crate::ui::SourcePage::Catalog(j),
                    ..
                }) = self.app.ui.source.as_mut()
                    && let Some(p) = j.pages.last_mut()
                    && let Body::Home { search, typed, .. } | Body::Search { search, typed, .. } =
                        &mut p.body
                {
                    *search = text.trim().to_string();
                    // Typed long enough ago for the search to go now.
                    *typed = Instant::now().checked_sub(Duration::from_secs(1));
                } else {
                    return Err("catalog-search needs a catalog page open".into());
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The form adding a source of a kind: `add-source library`.
            Some("add-source") => {
                let kind = parts.get(1).ok_or("add-source needs a kind")?;
                let kind: fp_sources::SourceKind =
                    serde_json::from_value(serde_json::Value::String(kind.to_string()))
                        .map_err(|e| format!("no source kind {kind:?}: {e}"))?;
                self.app.ui.screen = crate::ui::Screen::Sources;
                self.app.ui.source = None;
                self.app.ui.source_form = Some(crate::ui::SourceForm {
                    kind,
                    ..Default::default()
                });
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // Code-linking screens from the loopback mock, without a live
            // Plex account or production polling: `plex-link-preview URL waiting`.
            Some("plex-link-preview") => {
                use crate::ui::plex::LinkState;
                use fp_sources::plex::auth::{PlexAuthClient, PlexCredentials};
                let endpoint = parts
                    .get(1)
                    .ok_or("plex-link-preview needs a loopback API")?;
                let state = parts.get(2).copied().unwrap_or("waiting");
                let api = PlexAuthClient::with_test_endpoint(endpoint)?;
                let mut pin = api.initiate("ui-preview")?;
                if state == "expired" {
                    pin.expires_at = 0;
                }
                let state = match state {
                    "waiting" | "expired" => LinkState::Waiting {
                        pin,
                        polled: Instant::now(),
                        polling: true,
                    },
                    "servers" => {
                        let credentials =
                            PlexCredentials::manual("preview-token".into(), "ui-preview".into());
                        let servers = api.resources(&credentials)?;
                        LinkState::servers(credentials, servers, &[])
                    }
                    _ => return Err("plex-link-preview needs waiting, expired or servers".into()),
                };
                self.app.ui.screen = crate::ui::Screen::Sources;
                self.app.ui.source = None;
                self.app.ui.source_form = Some(crate::ui::SourceForm {
                    kind: fp_sources::SourceKind::Plex,
                    plex: crate::ui::plex::Connect {
                        state,
                        ..Default::default()
                    },
                    ..Default::default()
                });
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The library's videos made a playlist, as Add to playlist does:
            // `playlist NAME`.
            Some("playlist") => {
                let name = parts.get(1).ok_or("playlist needs a name")?.to_string();
                let reqs = self.library_requests();
                self.app.apply(crate::ui::Action::AddToPlaylist {
                    playlist: None,
                    name,
                    reqs,
                });
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The first playlist's page: `open-playlist`.
            Some("open-playlist") => {
                let first = self.app.library().playlists()?.into_iter().next();
                self.app.ui.screen = crate::ui::Screen::Playlists;
                self.app.ui.playlist = first.map(|p| p.id);
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The library's videos queued with Add to queue: `queue-all`.
            Some("queue-all") => {
                for req in self.library_requests() {
                    self.app
                        .apply(crate::ui::Action::Queue { req, next: false });
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            Some("shuffle") => {
                self.app.apply(crate::ui::Action::QueueShuffle);
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The first video's details, as its title opens them: `details`.
            Some("details") => {
                let first = self.app.library().search(&fp_library::Query {
                    sort: fp_library::Sort::Title,
                    limit: Some(1),
                    ..Default::default()
                })?;
                self.app.ui.details = first.first().map(|r| r.id);
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // Up next, as the control bar's button shows it: `up-next`.
            Some("up-next") => {
                self.app.apply(crate::ui::Action::ShowUpNext);
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The picker for the library's first video: `pick`.
            Some("pick") => {
                let reqs: Vec<_> = self.library_requests().into_iter().take(1).collect();
                let title = reqs
                    .first()
                    .map(crate::playback::OpenRequest::shown_title)
                    .unwrap_or_default();
                self.app.ui.picking = Some(crate::ui::Picking { reqs, title });
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The form editing a source, as its pencil opens: `edit-source ID`.
            Some("edit-source") => {
                let id = parts.get(1).ok_or("edit-source needs a source")?;
                let form = self
                    .app
                    .source_config(id)
                    .map(crate::ui::sources::edit_form)
                    .ok_or_else(|| format!("no source {id:?}"))?;
                self.app.ui.screen = crate::ui::Screen::Sources;
                self.app.ui.source_form = Some(form);
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The Folders tab of the library open, as its chip opens it:
            // `library-folders`.
            Some("library-folders") => {
                if let Some(crate::ui::SourceView {
                    source,
                    page: crate::ui::SourcePage::Library(l),
                    ..
                }) = self.app.ui.source.as_mut()
                {
                    l.folders = true;
                    l.folder = Some(crate::ui::Browse {
                        source: source.clone(),
                        loading: true,
                        ..Default::default()
                    });
                    let source = source.clone();
                    self.app.browse(source, None);
                }
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // Gives a source a PIN, which locks it: `lock SOURCE PIN`.
            Some("lock") => {
                let source = parts.get(1).ok_or("lock needs a source")?.to_string();
                let pin = parts.get(2).ok_or("lock needs a PIN")?.to_string();
                self.app.apply(crate::ui::Action::SetSourcePin {
                    source,
                    pin: Some(pin),
                });
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // `unlock SOURCE PIN`, as the PIN form does.
            Some("unlock") => {
                let source = parts.get(1).ok_or("unlock needs a source")?.to_string();
                let pin = parts.get(2).ok_or("unlock needs a PIN")?.to_string();
                self.app
                    .apply(crate::ui::Action::UnlockSource { source, pin });
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            // The PIN form of a source: `pin-form SOURCE [set] [wrong]`.
            Some("pin-form") => {
                let source = parts.get(1).ok_or("pin-form needs a source")?.to_string();
                let name = self.app.source_name(&source);
                self.app.ui.screen = crate::ui::Screen::Sources;
                self.app.ui.pin_form = Some(crate::ui::PinForm {
                    source,
                    name,
                    set: parts.contains(&"set"),
                    wrong: parts.contains(&"wrong"),
                    ..Default::default()
                });
                self.app.repaint_all();
                self.run_frames(3)?;
            }
            Some("shot") => self.save(parts.get(1).copied().unwrap_or("shot"), false)?,
            Some("sbs") => self.save(parts.get(1).copied().unwrap_or("sbs"), true)?,
            Some(c) => return Err(format!("unknown script command {c:?}").into()),
        }
        Ok(false)
    }
}

pub fn run(args: &Args, out: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(out)?;
    let gpu = Arc::new(Gpu::headless()?);
    let mut renderer = Renderer::new(gpu.clone(), ash::vk::Format::R8G8B8A8_SRGB)?;
    // A screenshot of a place has the place in it, not Plain while it builds.
    renderer.set_inline_places(true);
    fp_media::set_hevc_10bit_hardware(renderer.shows_q10c());
    fp_media::set_shared_frames(renderer.reads_decoder_buffers());
    let size = args.size.unwrap_or(1024);
    let eyes = OffscreenEyes::new(&renderer, size, size)?;
    let mut app = crate::make_app()?;
    // The rows a Steam Frame shows, so screenshots cover long values.
    app.set_about(vec![
        ("Mode".into(), "Preview (no headset)".into()),
        (
            "GPU".into(),
            format!("{} ({})", gpu.device_name, gpu.driver),
        ),
        (
            "Controllers".into(),
            "Steam Frame, Touch, Simple controller, Hand tracking".into(),
        ),
    ]);
    // The sizes a Steam Frame offers, so Settings › General shows the Render
    // resolution row.
    app.set_render_info(crate::ui::RenderInfo {
        started: crate::settings::RenderScale::Auto,
        eye_px: [(1440, 1440), (1800, 1800), (2160, 2160)],
    });
    if let Some(o) = &args.open {
        app.open(crate::playback::OpenRequest {
            location: crate::location(o),
            ..Default::default()
        });
    }
    let script = match &args.script {
        Some(p) => std::fs::read_to_string(p)?,
        None => DEFAULT_SCRIPT.to_string(),
    };
    let mut sim = Sim {
        app,
        renderer,
        eyes,
        size,
        head: Quat::IDENTITY,
        hand: Hand {
            active: true,
            aim: None,
            ..Default::default()
        },
        start: Instant::now(),
        last: Instant::now(),
        frames: 0,
        gpu_ms: Default::default(),
        out: out.to_path_buf(),
    };
    sim.hand.aim = Some((sim.hand_pos(), Quat::IDENTITY));
    for (n, line) in script.lines().enumerate() {
        let line = without_comment(line);
        if line.is_empty() {
            continue;
        }
        log::info!("script {}: {line}", n + 1);
        match sim.command(line) {
            Ok(true) => break,
            Ok(false) => {}
            Err(e) => {
                sim.renderer.wait_idle();
                return Err(format!("script line {}: {line}: {e}", n + 1).into());
            }
        }
    }
    println!("{} frames rendered", sim.frames);
    if let Some(line) = sim.gpu_ms.take_line() {
        println!("{line}");
    }
    let Sim {
        app,
        renderer,
        mut eyes,
        ..
    } = sim;
    renderer.wait_idle();
    eyes.destroy(&renderer);
    app.shutdown();
    renderer.wait_idle();
    drop(renderer);
    Ok(())
}

/// The number after `FIELD=` in a status line.
fn status_field(status: &str, field: &str) -> Option<f32> {
    let key = format!("{field}=");
    status
        .split_whitespace()
        .find_map(|word| word.strip_prefix(key.as_str())?.parse().ok())
}

/// Whether `got OP want` holds, for the `expect` command's `<`, `>`, `==`
/// (to 0.01) and `~` (within 10 %).
fn compares(got: f32, op: &str, want: f32) -> Result<bool, Error> {
    Ok(match op {
        "<" => got < want,
        ">" => got > want,
        "==" => (got - want).abs() < 0.01,
        "~" => (got - want).abs() <= 0.1 * want.abs(),
        other => return Err(format!("no comparison {other:?} (use <, >, == or ~)").into()),
    })
}

/// `line` trimmed, without its comment.
fn without_comment(line: &str) -> &str {
    let line = line.trim();
    if line.starts_with('#') {
        return "";
    }
    line.find(" #").map_or(line, |i| line[..i].trim_end())
}

#[cfg(test)]
mod tests {
    use super::{compares, status_field, without_comment};

    #[test]
    fn expect_reads_numbers_off_the_status_line() {
        let status = "A Clip pos=1.0/10.0 yaw=12.5 pitch=-3.0 width=6.00 chrome=shown";
        assert_eq!(status_field(status, "yaw"), Some(12.5));
        assert_eq!(status_field(status, "pitch"), Some(-3.0));
        assert_eq!(status_field(status, "width"), Some(6.0));
        // Not a number, a part of another word, or not there.
        assert_eq!(status_field(status, "chrome"), None);
        assert_eq!(status_field(status, "aw"), None);
        assert_eq!(status_field(status, "zoom"), None);
        assert!(compares(7.0, ">", 6.5).unwrap());
        assert!(!compares(6.0, ">", 6.5).unwrap());
        assert!(compares(6.0, "<", 6.5).unwrap());
        assert!(compares(6.004, "==", 6.0).unwrap());
        assert!(!compares(6.1, "==", 6.0).unwrap());
        assert!(compares(6.4, "~", 6.0).unwrap());
        assert!(!compares(7.0, "~", 6.0).unwrap());
        assert!(compares(1.0, "!=", 6.0).is_err());
    }

    #[test]
    fn comments_leave_addresses_whole() {
        assert_eq!(without_comment("# a comment"), "");
        assert_eq!(without_comment("  frames 8   # settle"), "frames 8");
        assert_eq!(
            without_comment("open-in emby mb:http://emby:8096#item=12"),
            "open-in emby mb:http://emby:8096#item=12"
        );
    }
}
