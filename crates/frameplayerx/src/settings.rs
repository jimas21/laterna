//! Persistent user settings (`~/.config/frameplayerx/settings.json`).
//! Source credentials live separately in `sources.json` (mode 0600).

use fp_core::view::ViewSettings;
use fp_haptics::HapticsSettings;
use fp_remote::config::RemoteConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A haptic device the user added.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HapticDeviceConfig {
    /// TCode over serial (`/dev/ttyACM0`), `tcp://host:port` or `udp://host:port`.
    Tcode { endpoint: String },
    /// Intiface Central's server.
    Buttplug { url: String },
    /// The Handy, by connection key.
    Handy { key: String },
}

impl HapticDeviceConfig {
    pub fn label(&self) -> String {
        match self {
            HapticDeviceConfig::Tcode { endpoint } => format!("TCode {endpoint}"),
            HapticDeviceConfig::Buttplug { url } => format!("Intiface {url}"),
            HapticDeviceConfig::Handy { key } => {
                let shown: String = key.chars().take(3).collect();
                format!("The Handy ({shown}…)")
            }
        }
    }
}

/// Which subtitles a video starts with (see [`crate::subtitles::choose`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleMode {
    Off,
    /// Forced subtitles, subtitles in your language when the audio is in
    /// another one, and a subtitle file named like the video.
    #[default]
    Automatic,
    /// Subtitles in your language whenever the video has them.
    Always,
}

/// Settings › General › Render resolution: how big the picture drawn for each
/// eye is, against the size SteamVR recommends. Read at start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderScale {
    /// SteamVR's own size.
    #[default]
    Auto,
    Sharper,
    Sharpest,
}

impl RenderScale {
    /// Every choice, in the order they are offered.
    pub const ALL: [RenderScale; 3] = [
        RenderScale::Auto,
        RenderScale::Sharper,
        RenderScale::Sharpest,
    ];

    /// How many times the recommended size each side is.
    pub fn factor(self) -> f32 {
        match self {
            RenderScale::Auto => 1.0,
            RenderScale::Sharper => 1.25,
            RenderScale::Sharpest => 1.5,
        }
    }
}

/// A choice this version doesn't know (from a newer one) is Auto, rather
/// than the whole file failing to load and every setting resetting.
impl<'de> Deserialize<'de> for RenderScale {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<RenderScale, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(match value.as_str() {
            Some("sharper") => RenderScale::Sharper,
            Some("sharpest") => RenderScale::Sharpest,
            _ => RenderScale::Auto,
        })
    }
}

/// Settings › Interface size: the sizes offered.
pub const UI_SCALES: std::ops::RangeInclusive<f32> = 0.8..=1.5;

/// Settings › Image cache: the sizes offered, in MB.
pub const IMAGE_CACHE_SIZES: [u32; 5] = [100, 250, 500, 1000, 2000];

/// The id of "no place": the plain background. Any id this build does not
/// know shows Plain too.
pub const PLAIN_PLACE: &str = "void";

/// Settings › Places › House lights while playing: the levels offered, as a
/// share of full light.
pub const HOUSE_PLAYING: std::ops::RangeInclusive<f32> = 0.0..=0.4;

/// The level the house lights stay at while a video plays, to begin with.
pub const HOUSE_PLAYING_START: f32 = 0.04;

/// Settings > Places > Screen light: the level to begin with, as a share of
/// the slider (0 is off). Half the slider is the strength the places are
/// tuned for; the top of it is twice that.
pub const SCREEN_LIGHT_START: f32 = 0.5;

/// What a screen light level (0 to 1) asks of the renderer: 0 is off, and
/// [`SCREEN_LIGHT_START`] is 1, the places' own strength.
pub fn screen_light_gain(level: f32) -> f32 {
    level.clamp(0.0, 1.0) / SCREEN_LIGHT_START
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Use the V4L2 hardware decoder when available.
    pub hardware_decoding: bool,
    /// ALSA device ("default" routes to PipeWire on SteamOS).
    pub audio_device: String,
    pub volume: f32,
    /// Milliseconds the sound plays later than the picture (negative:
    /// earlier), for headsets and earbuds whose sound lags or leads. One
    /// value for every output: the system's default output cannot be told
    /// apart by name.
    pub audio_delay_ms: i32,
    pub subtitles: SubtitleMode,
    /// The language subtitles are wanted in (ISO 639-1).
    pub subtitle_language: String,
    /// The interface's language, one of [`crate::i18n::LANGUAGES`]; none
    /// follows Steam's.
    pub language: Option<String>,
    /// Defaults for videos without saved adjustments.
    pub default_view: ViewSettings,
    /// Show the real world around flat screens and the UI.
    pub passthrough: bool,
    /// Resume videos where they were left.
    pub resume: bool,
    /// A video playing pauses when the headset comes off (the session drops
    /// below visible) and stays paused when it goes back on.
    pub pause_on_headset_off: bool,
    /// A video playing pauses when an earbud comes out (needs Tragus).
    pub pause_on_earbud_out: bool,
    /// The format a video starts in when nothing identifies it (no
    /// metadata, file name tag or telling shape).
    pub default_format: fp_core::format::VideoFormat,
    /// The fisheye lens angles offered as presets, in degrees, smallest
    /// first.
    pub fisheye_presets: Vec<f32>,
    /// The angle a video gets when Fisheye is picked for it, and a Fisheye
    /// default format: one of the presets (180° with none).
    pub fisheye_start: f32,
    /// Seconds of inactivity before the playback UI hides.
    pub auto_hide_secs: f32,
    /// Thumbstick seek step in seconds.
    pub seek_step: f64,
    pub remote: RemoteConfig,
    /// Haptic devices and scripts. Off, the app shows nothing about them,
    /// loads no scripts and runs no haptics engine.
    pub haptics_enabled: bool,
    pub haptics: HapticsSettings,
    pub haptic_devices: Vec<HapticDeviceConfig>,
    pub update_channel: fp_updater::Channel,
    pub check_updates: bool,
    /// UI text size multiplier, in [`UI_SCALES`].
    pub ui_scale: f32,
    /// Room for server pictures kept on the headset, in MB
    /// ([`crate::image_cache`]).
    pub image_cache_mb: u32,
    /// Make thumbnails for videos whose server offers no picture.
    pub make_thumbnails: bool,
    /// A partly watched video shows the frame where it was left off.
    pub resume_frames: bool,
    /// Flat videos and the panels follow the head ([`crate::world::follow`]).
    pub follow: bool,
    /// A flat video screen has a bar under it to move it by and a handle at
    /// its bottom right corner to resize it (`app::chrome`).
    pub window_controls: bool,
    /// The video playing starts again when it ends.
    pub loop_video: bool,
    /// The list a video was opened from plays through, the next starting
    /// when one ends (Play all, a playlist and what was queued always do).
    pub autoplay: bool,
    /// Up next starts again from the first video after the last, and X and
    /// Y step round it.
    pub loop_playlist: bool,
    /// The performance overlay (Settings › General › Diagnostics).
    pub perf_overlay: bool,
    /// The size of the picture drawn for each eye (Settings › General ›
    /// Render resolution). Takes effect when the app starts.
    pub render_scale: RenderScale,
    /// While a video plays, ask the runtime for a refresh rate the film's
    /// frame rate fits evenly (Settings › General › Match the headset to
    /// the film). Off, the user's own rate is never changed.
    pub auto_refresh_rate: bool,
    /// What the controller buttons and thumbsticks do.
    pub controls: crate::bindings::Bindings,
    /// Where the library and adjust panels were moved to: yaw and pitch in
    /// degrees from their usual places (left and up positive), and how much
    /// nearer or further.
    pub panel_offsets: PanelOffsets,
    /// How folders are ordered when browsing a source.
    pub browse_sort: fp_sources::EntrySort,
    /// Sources locked with a PIN: source id to the PIN's hash (see
    /// [`crate::pin`]). Left out of the file when empty.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub source_pins: std::collections::BTreeMap<String, String>,
    /// The Plex Home profile each linked account plays as: the account's
    /// key to the profile's uuid (see [`crate::plex_profiles`]). An account
    /// that is not here plays as itself. Never a PIN or a token. Left out of
    /// the file when empty.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub plex_profiles: std::collections::BTreeMap<String, String>,
    /// The place around flat videos and the menus: [`PLAIN_PLACE`] or a
    /// place id (`fp_render::env::Place::id`). A string, so a file from a
    /// newer build with a place this one lacks still loads; an unknown id
    /// shows Plain.
    pub environment: String,
    /// The seat picked in each room, by place id. Left out of the file when
    /// empty.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub place_seats: std::collections::BTreeMap<String, u8>,
    /// How far above (or below, negative) its seat's own height the viewer's
    /// eye sits in each room, in centimetres, by place id: -30 to +30 in
    /// steps of 5 (`app::places::HEIGHT_CM`). Left out of the file when empty.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub place_height: std::collections::BTreeMap<String, i8>,
    /// How bright a place's house lights stay while a video plays, in
    /// [`HOUSE_PLAYING`].
    pub house_lights_playing: f32,
    /// How strongly rooms take the light of the picture, 0 (off) to 1; see
    /// [`screen_light_gain`]. A file from when this was a switch has it off
    /// as 0 (see [`Settings::load`]).
    pub screen_light_level: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelOffsets {
    pub main: [f32; 2],
    /// The adjust panel, from beside the control bar.
    pub adjust: [f32; 2],
    /// The control bar, from below a flat screen (or in front of you
    /// during a 180°/360° video).
    pub bar: [f32; 2],
    /// The performance overlay, from where it starts.
    pub perf: [f32; 2],
    /// How much nearer or further than usual each of them was moved.
    pub depth: PanelDepths,
    /// How much bigger or smaller than usual the library panel was made by
    /// its corner handle. An `Option`: a settings file from before has none,
    /// and a plain number would default to 0 and shrink the panel to nothing.
    pub scale_main: Option<f32>,
}

/// Metres each panel that moves was pulled in (negative) or pushed out from
/// its usual distance from the viewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelDepths {
    pub main: f32,
    pub adjust: f32,
    pub bar: f32,
    pub perf: f32,
}

/// The fisheye presets to start with: a plain 180° lens, the Canon RF 5.2 mm,
/// the MKX200 and the MKX220.
pub const FISHEYE_PRESETS: [f32; 4] = [180.0, 190.0, 200.0, 220.0];
/// As many fisheye presets as fit on one line.
pub const MAX_FISHEYE_PRESETS: usize = 8;
/// The angle Fisheye starts at to begin with, a plain fisheye lens's.
pub const FISHEYE_START: f32 = 180.0;

/// Adds `angle`, in whole degrees, to `presets`, kept in order, unless it is
/// there or they are full.
pub fn add_fisheye_preset(presets: &mut Vec<f32>, angle: f32) {
    let angle = angle.round();
    if presets.len() < MAX_FISHEYE_PRESETS && !presets.contains(&angle) {
        presets.push(angle);
        presets.sort_by(f32::total_cmp);
    }
}

impl Settings {
    /// Takes the fisheye preset at `index` out. If Fisheye started at it,
    /// it starts at the preset nearest 180° instead (the smaller of two as
    /// near), or 180° with none left.
    pub fn remove_fisheye_preset(&mut self, index: usize) {
        if index < self.fisheye_presets.len()
            && self.fisheye_presets.remove(index) == self.fisheye_start
        {
            let off = |angle: &f32| (angle - FISHEYE_START).abs();
            self.fisheye_start = self
                .fisheye_presets
                .iter()
                .copied()
                .min_by(|a, b| off(a).total_cmp(&off(b)))
                .unwrap_or(FISHEYE_START);
        }
    }

    /// The fisheye presets and where Fisheye starts, as they began.
    pub fn reset_fisheye_presets(&mut self) {
        self.fisheye_presets = FISHEYE_PRESETS.to_vec();
        self.fisheye_start = FISHEYE_START;
    }

    /// Whether the fisheye presets or where Fisheye starts were changed.
    pub fn fisheye_presets_changed(&self) -> bool {
        self.fisheye_presets != FISHEYE_PRESETS || self.fisheye_start != FISHEYE_START
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hardware_decoding: true,
            audio_device: "default".into(),
            volume: 1.0,
            audio_delay_ms: 0,
            subtitles: SubtitleMode::default(),
            subtitle_language: crate::i18n::system_language()
                .and_then(|l| fp_core::language::from_locale(&l))
                .unwrap_or("en")
                .to_string(),
            language: None,
            default_view: ViewSettings::default(),
            passthrough: false,
            resume: true,
            pause_on_headset_off: true,
            pause_on_earbud_out: true,
            default_format: fp_core::format::VideoFormat::FALLBACK,
            fisheye_presets: FISHEYE_PRESETS.to_vec(),
            fisheye_start: FISHEYE_START,
            auto_hide_secs: 4.0,
            seek_step: 10.0,
            remote: RemoteConfig::default(),
            haptics_enabled: false,
            haptics: HapticsSettings::default(),
            haptic_devices: Vec::new(),
            // A beta build keeps getting betas until the user picks Stable.
            update_channel: if env!("CARGO_PKG_VERSION").contains('-') {
                fp_updater::Channel::Beta
            } else {
                fp_updater::Channel::Stable
            },
            check_updates: true,
            ui_scale: 1.0,
            image_cache_mb: 500,
            make_thumbnails: true,
            resume_frames: true,
            follow: false,
            window_controls: true,
            loop_video: false,
            autoplay: false,
            loop_playlist: false,
            perf_overlay: false,
            render_scale: RenderScale::default(),
            auto_refresh_rate: true,
            controls: crate::bindings::Bindings::default(),
            panel_offsets: PanelOffsets::default(),
            browse_sort: fp_sources::EntrySort::default(),
            source_pins: Default::default(),
            plex_profiles: Default::default(),
            environment: PLAIN_PLACE.to_string(),
            place_seats: Default::default(),
            place_height: Default::default(),
            house_lights_playing: HOUSE_PLAYING_START,
            screen_light_level: SCREEN_LIGHT_START,
        }
    }
}

impl Settings {
    /// Adjustments for a video that has none saved: the defaults, following
    /// the global chroma key.
    pub fn new_video_view(&self) -> ViewSettings {
        ViewSettings {
            key_own: Some(false),
            ..self.default_view
        }
    }

    pub fn path() -> PathBuf {
        fp_core::dirs::config_dir().join("settings.json")
    }

    pub fn sources_path() -> PathBuf {
        fp_core::dirs::config_dir().join("sources.json")
    }

    /// The language the interface is shown in: the one picked, else
    /// [`crate::i18n::automatic`].
    pub fn interface_language(&self) -> String {
        self.language
            .clone()
            .unwrap_or_else(|| crate::i18n::automatic().to_string())
    }

    /// Loads settings, falling back to defaults (and logging why) when the
    /// file is missing or unreadable.
    pub fn load(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(mut s) => {
                    s.audio_delay_ms = fp_core::audio_sync::clamp_delay_ms(s.audio_delay_ms);
                    // Saved before the global chroma key existed: its key
                    // was never chosen, so take the current default.
                    if s.default_view.key_own.is_none() {
                        s.default_view.set_key(&ViewSettings::default());
                        s.default_view.key_own = Some(false);
                    }
                    // Saved before height existed: the right grip's up / down
                    // was free then, so it gets the new default.
                    let before_height = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .is_some_and(|v| v["default_view"].get("height").is_none());
                    if before_height
                        && s.controls.right.grip_y == crate::bindings::AxisAction::Nothing
                    {
                        s.controls.right.grip_y = crate::bindings::AxisAction::Height;
                    }
                    // Saved when the picture's light was a switch: off stays
                    // off.
                    let value = serde_json::from_str::<serde_json::Value>(&text).ok();
                    let had_level = value
                        .as_ref()
                        .is_some_and(|v| v.get("screen_light_level").is_some());
                    let switched_off = value
                        .as_ref()
                        .and_then(|v| v.get("screen_light"))
                        .and_then(serde_json::Value::as_bool)
                        == Some(false);
                    if switched_off && !had_level {
                        s.screen_light_level = 0.0;
                    }
                    s.screen_light_level = if s.screen_light_level.is_finite() {
                        s.screen_light_level.clamp(0.0, 1.0)
                    } else {
                        SCREEN_LIGHT_START
                    };
                    s
                }
                Err(e) => {
                    log::warn!("settings {}: {e}; using defaults", path.display());
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        }
    }

    /// Writes atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plex_profiles_default_empty_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("fp-plex-profiles-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        assert!(s.plex_profiles.is_empty());
        s.save(&path).unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("plex_profiles")
        );
        s.plex_profiles
            .insert("account-key".into(), "profile-uuid".into());
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        // Saved before profiles: nobody has one chosen.
        std::fs::write(&path, r#"{"volume": 0.25}"#).unwrap();
        assert!(Settings::load(&path).plex_profiles.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trip_and_partial_files() {
        let dir = std::env::temp_dir().join(format!("fp-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings {
            volume: 0.5,
            ..Default::default()
        };
        s.haptic_devices.push(HapticDeviceConfig::Tcode {
            endpoint: "tcp://10.0.0.2:8000".into(),
        });
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        // Source PINs are only written once one is set.
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("source_pins")
        );
        s.source_pins.insert("nas".into(), crate::pin::hash("2468"));
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::write(&path, r#"{"volume": 0.25}"#).unwrap();
        let p = Settings::load(&path);
        assert_eq!(p.volume, 0.25);
        assert_eq!(p.seek_step, 10.0);
        assert!(!p.haptics_enabled, "haptics stay off until turned on");
        assert!(p.pause_on_headset_off, "old files get the new default");
        assert!(p.pause_on_earbud_out, "old files get the new default");
        assert_eq!(p.default_format, fp_core::format::VideoFormat::FALLBACK);
        assert_eq!(p.fisheye_presets, FISHEYE_PRESETS);
        assert_eq!(p.fisheye_start, FISHEYE_START);
        // Saved before places: Plain, the usual house lights, the picture's
        // light on.
        assert_eq!(p.environment, PLAIN_PLACE);
        assert!(p.place_seats.is_empty());
        assert!(p.place_height.is_empty());
        assert_eq!(p.house_lights_playing, HOUSE_PLAYING_START);
        assert_eq!(HOUSE_PLAYING_START, 0.04);
        assert_eq!(p.screen_light_level, SCREEN_LIGHT_START);
        // Saved before the global chroma key: the old green default goes.
        std::fs::write(
            &path,
            r#"{"default_view": {"zoom": 1.5, "key_color": [0.0, 1.0, 0.0]}}"#,
        )
        .unwrap();
        let p = Settings::load(&path);
        assert_eq!(p.default_view.zoom, 1.5);
        assert_eq!(p.default_view.key_color, ViewSettings::default().key_color);
        assert_eq!(p.default_view.key_own, Some(false));
        // Saved before height: the right grip's free up / down gets it; one
        // set to nothing since stays so.
        let old =
            r#"{"default_view": {"zoom": 1.0}, "controls": {"right": {"grip_y": "nothing"}}}"#;
        std::fs::write(&path, old).unwrap();
        let height = crate::bindings::AxisAction::Height;
        assert_eq!(Settings::load(&path).controls.right.grip_y, height);
        let mut s = Settings::default();
        s.controls.right.grip_y = crate::bindings::AxisAction::Nothing;
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(Settings::load(&path).volume, 1.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_audio_delay_is_stored_and_kept_in_range() {
        let dir = std::env::temp_dir().join(format!("fp-audio-delay-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(Settings::default().audio_delay_ms, 0);
        let s = Settings {
            audio_delay_ms: -120,
            ..Default::default()
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).audio_delay_ms, -120);
        // An old file has none; a hand-edited one is brought into range.
        std::fs::write(&path, r#"{"volume": 0.25}"#).unwrap();
        assert_eq!(Settings::load(&path).audio_delay_ms, 0);
        std::fs::write(&path, r#"{"audio_delay_ms": 9000}"#).unwrap();
        assert_eq!(Settings::load(&path).audio_delay_ms, 300);
        std::fs::write(&path, r#"{"audio_delay_ms": 14}"#).unwrap();
        assert_eq!(Settings::load(&path).audio_delay_ms, 10);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn places_settings_survive_a_round_trip_and_an_unknown_place() {
        let name = format!("fp-places-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        let path = dir.join("settings.json");
        // Seats are only written once one is picked.
        Settings::default().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""environment": "void""#), "{text}");
        assert!(!text.contains("place_seats"), "{text}");
        assert!(!text.contains("place_height"), "{text}");
        let mut s = Settings {
            environment: "cinema".into(),
            house_lights_playing: 0.1,
            screen_light_level: 0.0,
            ..Default::default()
        };
        s.place_seats.insert("cinema".into(), 2);
        s.place_height.insert("cinema".into(), -15);
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        // A place this build lacks (from a newer one) loads with every other
        // setting, and shows Plain.
        std::fs::write(&path, r#"{"volume": 0.25, "environment": "moon"}"#).unwrap();
        let p = Settings::load(&path);
        assert_eq!(p.volume, 0.25);
        assert_eq!(p.environment, "moon");
        let shown = crate::app::places::active_place(&p.environment, false, true, true);
        assert_eq!(shown, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_screen_light_level_replaces_the_old_switch() {
        // The starting level is the places' own strength; 0 is off and the
        // top of the slider is twice the start.
        assert_eq!(screen_light_gain(SCREEN_LIGHT_START), 1.0);
        assert_eq!(screen_light_gain(0.0), 0.0);
        assert_eq!(screen_light_gain(1.0), 2.0);
        assert_eq!(screen_light_gain(7.0), 2.0);
        assert_eq!(screen_light_gain(-1.0), 0.0);
        let name = format!("fp-screen-light-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        // The old switch, off: the light stays off. On: the starting level.
        std::fs::write(&path, r#"{"screen_light": false}"#).unwrap();
        assert_eq!(Settings::load(&path).screen_light_level, 0.0);
        std::fs::write(&path, r#"{"screen_light": true}"#).unwrap();
        assert_eq!(Settings::load(&path).screen_light_level, SCREEN_LIGHT_START);
        // A level chosen since wins, and a silly one is held to the slider.
        std::fs::write(
            &path,
            r#"{"screen_light": false, "screen_light_level": 0.8}"#,
        )
        .unwrap();
        assert_eq!(Settings::load(&path).screen_light_level, 0.8);
        std::fs::write(&path, r#"{"screen_light_level": 9.0}"#).unwrap();
        assert_eq!(Settings::load(&path).screen_light_level, 1.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn render_resolution_defaults_to_auto_and_survives_a_round_trip() {
        let old: Settings = serde_json::from_str(r#"{"volume": 0.5}"#).unwrap();
        assert_eq!(old.render_scale, RenderScale::Auto);
        assert_eq!(old.render_scale.factor(), 1.0);
        for scale in RenderScale::ALL {
            let s = Settings {
                render_scale: scale,
                ..Default::default()
            };
            let json = serde_json::to_string(&s).unwrap();
            let again: Settings = serde_json::from_str(&json).unwrap();
            assert_eq!(again.render_scale, scale);
        }
        let json = serde_json::to_string(&RenderScale::Sharpest).unwrap();
        assert_eq!(json, r#""sharpest""#);
        assert_eq!(RenderScale::Sharper.factor(), 1.25);
        assert_eq!(RenderScale::Sharpest.factor(), 1.5);
    }

    #[test]
    fn matching_the_refresh_rate_is_on_unless_the_file_says_off() {
        let old: Settings = serde_json::from_str(r#"{"volume": 0.5}"#).unwrap();
        assert!(old.auto_refresh_rate);
        let off: Settings = serde_json::from_str(r#"{"auto_refresh_rate": false}"#).unwrap();
        assert!(!off.auto_refresh_rate);
        let s = Settings {
            auto_refresh_rate: false,
            ..Default::default()
        };
        let again: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(!again.auto_refresh_rate);
    }

    #[test]
    fn an_unknown_render_resolution_is_auto_and_keeps_the_other_settings() {
        for unknown in [r#""ultra""#, "2", "null", "[1]", r#""""#] {
            let text = format!(r#"{{"volume": 0.25, "render_scale": {unknown}}}"#);
            let s: Settings = serde_json::from_str(&text).unwrap();
            assert_eq!(s.render_scale, RenderScale::Auto, "{unknown}");
            assert_eq!(s.volume, 0.25, "{unknown}");
        }
    }

    #[test]
    fn fisheye_presets_stay_in_order_without_repeats() {
        let mut presets = FISHEYE_PRESETS.to_vec();
        add_fisheye_preset(&mut presets, 129.6);
        add_fisheye_preset(&mut presets, 190.2);
        assert_eq!(presets, [130.0, 180.0, 190.0, 200.0, 220.0]);
        for angle in [140.0, 150.0, 160.0, 170.0, 250.0] {
            add_fisheye_preset(&mut presets, angle);
        }
        assert_eq!(presets.len(), MAX_FISHEYE_PRESETS);
        assert!(!presets.contains(&250.0), "full: {presets:?}");
    }

    #[test]
    fn removing_the_preset_fisheye_starts_at_moves_it_nearest_180() {
        let mut s = Settings {
            fisheye_start: 200.0,
            ..Settings::default()
        };
        s.remove_fisheye_preset(1); // 190°: not where it starts
        assert_eq!((s.fisheye_start, s.fisheye_presets.len()), (200.0, 3));
        s.remove_fisheye_preset(1); // 200°
        assert_eq!(s.fisheye_start, 180.0);
        s.remove_fisheye_preset(0); // 180°, leaving 220°
        assert_eq!(s.fisheye_start, 220.0);
        s.remove_fisheye_preset(0);
        assert_eq!(s.fisheye_start, FISHEYE_START, "none left");
        assert!(s.fisheye_presets_changed());
        s.reset_fisheye_presets();
        assert!(!s.fisheye_presets_changed());

        // Two as near: the smaller.
        s.fisheye_presets = vec![170.0, 180.0, 190.0];
        s.remove_fisheye_preset(1);
        assert_eq!(s.fisheye_start, 170.0);
    }

    #[test]
    fn panels_saved_before_depth_stay_at_their_usual_distance() {
        let old = r#"{"panel_offsets": {"main": [10.0, -5.0], "bar": [0.0, 3.0]}}"#;
        let p: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(p.panel_offsets.main, [10.0, -5.0]);
        assert_eq!(p.panel_offsets.depth, PanelDepths::default());
        let mut s = Settings::default();
        s.panel_offsets.depth.bar = -0.25;
        s.panel_offsets.depth.main = 0.5;
        let json = serde_json::to_string(&s).unwrap();
        let again: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(again.panel_offsets, s.panel_offsets);
    }
}
