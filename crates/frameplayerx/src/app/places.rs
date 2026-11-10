//! Places around the picture: which one shows, how bright its house lights
//! are, and where the viewer sits in it. Everything here is a pure function
//! of settings and player state, so it is tested without a headset or a
//! renderer.

use crate::settings::{HOUSE_PLAYING, HOUSE_PLAYING_START, Settings};
use fp_media::PlayerState;
use fp_render::env::{Layout, Place};
use glam::{Mat4, Vec2, Vec3};

/// The kill switch: `FRAMEPLAYERX_PLACES=0` makes the app never ask for a
/// place, so the renderer never makes the pipelines for one. It does not
/// undo the eye pass changes that make places possible.
pub const KILL_SWITCH: &str = "FRAMEPLAYERX_PLACES";

/// How long the house lights take to dim while a video plays, and to come
/// back up when it pauses or ends: the time constants of an exponential
/// ease, seconds. About 2 s to reach the playing level, 1 s to come up.
const DIM_TAU: f32 = 0.6;
const RAISE_TAU: f32 = 0.3;

/// Whether places are on, given a lookup of environment variables. They are,
/// unless the kill switch is set to 0 (or off, false or no).
pub fn enabled(get: impl Fn(&str) -> Option<String>) -> bool {
    let value = get(KILL_SWITCH).unwrap_or_default().to_ascii_lowercase();
    !matches!(value.trim(), "0" | "off" | "false" | "no")
}

/// [`enabled`] for the process environment. The app reads it once, when it
/// starts, so the headset and the desktop preview both see it.
pub fn enabled_from_env() -> bool {
    enabled(|name| std::env::var(name).ok())
}

/// Whether the real room shows instead of the background: the runtime can
/// show it, and either the setting is on for a flat screen (or the menus),
/// or the video is chroma-keyed or alpha-packed, which show the room where
/// their background was.
pub fn passthrough_shown(available: bool, wanted: bool, flat: bool, keyed: bool) -> bool {
    available && ((wanted && flat) || keyed)
}

/// The place to show: the one `environment` names, around a flat video or
/// the menus, unless the real room shows (passthrough wins) or places are
/// off. A 180°, 360° or fisheye video fills the view and hides it, and an id
/// this build does not know (Plain is "void") is no place.
pub fn active_place(
    environment: &str,
    passthrough: bool,
    flat_or_none: bool,
    places_on: bool,
) -> Option<Place> {
    if !places_on || passthrough || !flat_or_none {
        return None;
    }
    Place::from_id(environment)
}

/// Where the house lights are heading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HouseTarget {
    /// Come up: paused, ended, failed, or no video.
    Up,
    /// Dim: playing.
    Down,
    /// Stay as they are: a rebuffer in mid-film must not flash them.
    Hold,
}

impl HouseTarget {
    /// The ease value this heads for, or `None` to stay where it is.
    pub fn goal(self) -> Option<f32> {
        match self {
            HouseTarget::Up => Some(1.0),
            HouseTarget::Down => Some(0.0),
            HouseTarget::Hold => None,
        }
    }
}

/// Where the house lights head for a player in `state` (`None`: no video).
pub fn house_target(state: Option<PlayerState>) -> HouseTarget {
    match state {
        Some(PlayerState::Playing) => HouseTarget::Down,
        Some(PlayerState::Buffering) => HouseTarget::Hold,
        _ => HouseTarget::Up,
    }
}

/// `level` (the ease, 1 up and 0 down) moved `dt` seconds towards `target`:
/// exponentially, slowly when dimming and faster when coming up. A `dt` that
/// is not a positive number leaves it where it is.
pub fn house_step(level: f32, target: f32, dt: f32) -> f32 {
    let target = if target.is_finite() {
        target.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let level = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        target
    };
    if !dt.is_finite() || dt <= 0.0 {
        return level;
    }
    let tau = if target < level { DIM_TAU } else { RAISE_TAU };
    level + (target - level) * (1.0 - (-dt / tau).exp())
}

/// The brightness of the house lights, 0 dark to 1 full: `playing_level`
/// (Settings, clamped to the levels offered) at ease 0, full at ease 1.
pub fn house_level(ease: f32, playing_level: f32) -> f32 {
    let ease = if ease.is_finite() {
        ease.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let playing = if playing_level.is_finite() {
        playing_level
    } else {
        HOUSE_PLAYING_START
    };
    let low = playing.clamp(*HOUSE_PLAYING.start(), *HOUSE_PLAYING.end());
    low * (1.0 - ease) + ease
}

/// [`house_level`] for the ease the player's state gives, or for `forced`
/// when the preview holds the lights at one (`App::house_override`).
pub fn house_shown(ease: f32, forced: Option<f32>, playing_level: f32) -> f32 {
    house_level(forced.unwrap_or(ease), playing_level)
}

/// The index of the seat to sit in: the one picked for `place` if it is
/// still on offer, else the room's own choice.
pub fn seat_index(settings: &Settings, place: Place, layout: &Layout) -> usize {
    match settings.place_seats.get(place.id()) {
        Some(&i) if usize::from(i) < layout.seats.len() => usize::from(i),
        _ => layout.default_seat,
    }
}

/// A room placed round the viewer in a seat.
#[derive(Clone, Copy, Debug)]
pub struct RoomFrame {
    /// Room space to world space, with the seat's eye at the picture
    /// anchor's origin.
    pub room_to_world: Mat4,
    /// Where the room's screen is in the world, facing the seat.
    pub screen_pose: Mat4,
    /// The largest picture the screen takes, metres.
    pub fit: Vec2,
    pub curvature: f32,
    /// From the seat's eye to the middle of the screen, metres.
    pub distance: f32,
}

/// `layout`'s room round a viewer in seat `seat`, turned with
/// `picture_anchor` (so the whole room tilts and turns with the picture and
/// a recentre). `None` for a Sky, which has no room and no screen of its own.
pub fn room_frame(picture_anchor: Mat4, layout: &Layout, seat: usize) -> Option<RoomFrame> {
    let screen = layout.screen?;
    let seat = layout.seat(seat);
    let room_to_world = seat.room_to_world(picture_anchor);
    Some(RoomFrame {
        room_to_world,
        screen_pose: room_to_world * Mat4::from_translation(screen.centre),
        fit: screen.max,
        curvature: screen.curvature,
        distance: (screen.centre - seat.eye).length(),
    })
}

/// How far below the middle of a room's screen the subtitle line hangs, as a
/// fraction of the screen's height.
const SUBTITLE_DROP: f32 = 0.2;
/// How far in front of the screen the subtitles float, metres, so the
/// picture does not swallow them.
const SUBTITLE_LIFT: f32 = 0.05;

/// Where the subtitles go in a room: a panel on the screen's own plane.
#[derive(Clone, Copy, Debug)]
pub struct SubtitleSpot {
    /// The panel's centre, facing the seat like the screen.
    pub pose: Mat4,
    /// Width and height, metres.
    pub size: Vec2,
}

/// The subtitle panel for a room's screen. It hangs below the middle of the
/// screen by a fraction of the screen's height, not at a fraction of the
/// distance below the eye (a room's screen is not at eye height), and is as
/// wide as `distance` allows but never wider than the screen. `aspect` is the
/// panel's height over its width.
pub fn subtitle_spot(room: &RoomFrame, distance: f32, aspect: f32) -> SubtitleSpot {
    let width = 0.9 * distance.min(room.fit.x);
    let offset = Vec3::new(0.0, -SUBTITLE_DROP * room.fit.y, SUBTITLE_LIFT);
    SubtitleSpot {
        pose: room.screen_pose * Mat4::from_translation(offset),
        size: Vec2::new(width, width * aspect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_render::env::SeatName;

    fn no_vars(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn places_are_on_unless_the_kill_switch_says_otherwise() {
        assert!(enabled(no_vars));
        let with = |value: &str| enabled(|name| (name == KILL_SWITCH).then(|| value.to_string()));
        for off in ["0", " 0 ", "off", "OFF", "false", "no"] {
            assert!(!with(off), "{off:?}");
        }
        for on in ["", "1", "yes", "on", "true", "garbage"] {
            assert!(with(on), "{on:?}");
        }
        // Another variable does not count.
        let other = |name: &str| (name == "FRAMEPLAYERX_QUALITY").then(|| "0".to_string());
        assert!(enabled(other));
    }

    #[test]
    fn house_lights_dim_on_play_and_rise_on_pause() {
        let dt = 1.0 / 72.0;
        let mut ease = 1.0;
        let down = house_target(Some(PlayerState::Playing)).goal().unwrap();
        for _ in 0..144 {
            ease = house_step(ease, down, dt);
        }
        assert!(ease <= 0.05, "2 s of Playing: {ease}");
        // A rebuffer holds them where they are.
        assert_eq!(
            house_target(Some(PlayerState::Buffering)),
            HouseTarget::Hold
        );
        assert_eq!(HouseTarget::Hold.goal(), None);
        let held = ease;
        let up = house_target(Some(PlayerState::Paused)).goal().unwrap();
        for _ in 0..72 {
            ease = house_step(ease, up, dt);
        }
        assert!(ease >= 0.95, "1 s of Paused: {ease}");
        assert!(held < ease);
    }

    #[test]
    fn the_lights_come_up_unless_a_video_is_playing_or_buffering() {
        for state in [PlayerState::Paused, PlayerState::Ended, PlayerState::Failed] {
            assert_eq!(house_target(Some(state)), HouseTarget::Up, "{state:?}");
        }
        assert_eq!(house_target(None), HouseTarget::Up);
        assert_eq!(house_target(Some(PlayerState::Playing)), HouseTarget::Down);
        assert_eq!(HouseTarget::Up.goal(), Some(1.0));
        assert_eq!(HouseTarget::Down.goal(), Some(0.0));
    }

    #[test]
    fn a_step_of_no_time_or_a_bad_number_changes_nothing() {
        for level in [0.0, 0.3, 1.0] {
            assert_eq!(house_step(level, 0.0, 0.0), level);
            assert_eq!(house_step(level, 1.0, -1.0), level);
            for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert_eq!(house_step(level, 1.0, bad), level);
            }
        }
        for bad in [f32::NAN, f32::INFINITY] {
            for eased in [house_step(bad, 0.0, 0.1), house_step(0.5, bad, 0.1)] {
                assert!(eased.is_finite(), "{eased}");
                assert!((0.0..=1.0).contains(&eased), "{eased}");
            }
        }
        // A long stall (a first frame) never overshoots.
        assert_eq!(house_step(1.0, 0.0, 1000.0), 0.0);
        assert_eq!(house_step(0.0, 1.0, 1000.0), 1.0);
    }

    #[test]
    fn dimming_is_slower_than_coming_up() {
        let down = 1.0 - house_step(1.0, 0.0, 0.1);
        let up = house_step(0.0, 1.0, 0.1);
        assert!(up > down, "{up} {down}");
    }

    #[test]
    fn the_house_level_runs_from_the_playing_level_to_full() {
        assert_eq!(house_level(0.0, 0.04), 0.04);
        assert_eq!(house_level(1.0, 0.04), 1.0);
        assert!((house_level(0.5, 0.0) - 0.5).abs() < 1e-6);
        // The playing level is held to the levels Settings offers.
        assert_eq!(house_level(0.0, 0.9), 0.4);
        assert_eq!(house_level(0.0, -1.0), 0.0);
        // A bad number is full light or the usual playing level, not NaN.
        assert_eq!(house_level(f32::NAN, 0.04), 1.0);
        assert_eq!(house_level(0.0, f32::NAN), HOUSE_PLAYING_START);
    }

    #[test]
    fn the_house_override_replaces_the_ease() {
        // Playing: the ease says dim.
        assert_eq!(house_shown(0.0, None, 0.04), 0.04);
        assert_eq!(house_shown(0.0, Some(1.0), 0.04), 1.0);
        // Paused: the ease says full.
        assert_eq!(house_shown(1.0, Some(0.0), 0.04), 0.04);
        assert_eq!(house_shown(1.0, None, 0.04), 1.0);
        assert_eq!(house_shown(0.3, None, 0.04), house_level(0.3, 0.04));
        assert_eq!(house_shown(0.3, Some(0.6), 0.04), house_level(0.6, 0.04));
        // Out of range is held to the ends.
        assert_eq!(house_shown(0.0, Some(7.0), 0.04), 1.0);
        assert_eq!(house_shown(1.0, Some(-7.0), 0.04), 0.04);
    }

    #[test]
    fn places_show_only_around_flat_video() {
        // The real room wins over a place.
        assert_eq!(active_place("cinema", true, true, true), None);
        // A video that fills the view (180°, 360°, fisheye) hides it.
        assert_eq!(active_place("cinema", false, false, true), None);
        // A flat video, or the menus with no video.
        let cinema = Some(Place::Cinema);
        assert_eq!(active_place("cinema", false, true, true), cinema);
        let studio = Some(Place::Studio);
        assert_eq!(active_place("studio", false, true, true), studio);
        // Places off by the kill switch.
        assert_eq!(active_place("cinema", false, true, false), None);
        // Plain, and ids this build does not know.
        for id in ["void", "", "moon", "Cinema"] {
            assert_eq!(active_place(id, false, true, true), None, "{id:?}");
        }
    }

    #[test]
    fn keyed_video_keeps_the_real_room_over_a_place() {
        // A keyed or alpha-packed video shows the room where its background
        // was, whether or not the setting is on, while one is available.
        assert!(passthrough_shown(true, false, true, true));
        assert!(passthrough_shown(true, false, false, true));
        assert!(!passthrough_shown(false, true, true, true));
        assert_eq!(active_place("cinema", true, true, true), None);
        // The setting is for flat video and the menus.
        assert!(passthrough_shown(true, true, true, false));
        assert!(!passthrough_shown(true, true, false, false));
        assert!(!passthrough_shown(true, false, true, false));
        // Without a way to show the room, the place shows.
        let shown = passthrough_shown(false, true, true, false);
        let cinema = Some(Place::Cinema);
        assert_eq!(active_place("cinema", shown, true, true), cinema);
    }

    #[test]
    fn the_seat_is_the_one_picked_or_the_rooms_own() {
        let layout = Place::Cinema.layout();
        let mut s = Settings::default();
        assert_eq!(seat_index(&s, Place::Cinema, &layout), layout.default_seat);
        let back = layout.seat_index(SeatName::Back).unwrap();
        s.place_seats.insert("cinema".into(), back as u8);
        assert_eq!(seat_index(&s, Place::Cinema, &layout), back);
        // Another place's pick does not count, nor does a stale one.
        assert_eq!(seat_index(&s, Place::Studio, &Place::Studio.layout()), 0);
        s.place_seats.insert("cinema".into(), 200);
        assert_eq!(seat_index(&s, Place::Cinema, &layout), layout.default_seat);
    }

    #[test]
    fn a_seat_puts_the_screen_where_the_room_has_it() {
        let layout = Place::Cinema.layout();
        let middle = layout.seat_index(SeatName::Middle).unwrap();
        let room = room_frame(Mat4::IDENTITY, &layout, middle).unwrap();
        // The middle seat's eye is 4.34 m up and 14.15 m from the screen,
        // whose centre is 4.4 m up.
        let centre = room.screen_pose.transform_point3(Vec3::ZERO);
        let want = Vec3::new(0.0, 4.4 - 4.34, -14.15);
        assert!((centre - want).length() < 1e-4, "{centre}");
        // Facing the seat, as a screen placed in front of the head does.
        let normal = room.screen_pose.transform_vector3(Vec3::Z);
        assert!((normal - Vec3::Z).length() < 1e-5, "{normal}");
        assert_eq!(room.fit, Vec2::new(14.0, 7.2));
        assert_eq!(room.curvature, 0.0);
        assert!((room.distance - centre.length()).abs() < 1e-4);
    }

    #[test]
    fn a_seat_nearer_the_screen_is_nearer_the_screen() {
        let layout = Place::Cinema.layout();
        let distance = |name| {
            let i = layout.seat_index(name).unwrap();
            room_frame(Mat4::IDENTITY, &layout, i).unwrap().distance
        };
        let (front, middle, back) = (
            distance(SeatName::Front),
            distance(SeatName::Middle),
            distance(SeatName::Back),
        );
        assert!(front < middle && middle < back, "{front} {middle} {back}");
    }

    #[test]
    fn the_seat_sits_at_the_anchor_however_the_anchor_is_turned() {
        let layout = Place::Cinema.layout();
        let anchor = Mat4::from_translation(Vec3::new(0.5, 1.2, -0.3))
            * Mat4::from_rotation_y(0.7)
            * Mat4::from_rotation_x(0.4);
        for i in 0..layout.seats.len() {
            let room = room_frame(anchor, &layout, i).unwrap();
            let eye = room.room_to_world.transform_point3(layout.seats[i].eye);
            let origin = anchor.transform_point3(Vec3::ZERO);
            assert!((eye - origin).length() < 1e-4, "seat {i}: {eye}");
        }
        // A stale seat index sits in the default seat.
        let stale = room_frame(anchor, &layout, 99).unwrap();
        let usual = room_frame(anchor, &layout, layout.default_seat).unwrap();
        assert_eq!(stale.room_to_world, usual.room_to_world);
    }

    /// The subtitle panel's shape in the app: 1600 x 320.
    const ASPECT: f32 = 0.2;

    #[test]
    fn subtitles_hang_on_the_screen_from_every_seat() {
        let layout = Place::Cinema.layout();
        for name in [SeatName::Front, SeatName::Middle, SeatName::Back] {
            let seat = layout.seat_index(name).unwrap();
            let room = room_frame(Mat4::IDENTITY, &layout, seat).unwrap();
            let d = room.distance.clamp(0.5, 20.0);
            let spot = subtitle_spot(&room, d, ASPECT);
            let centre = room.screen_pose.transform_point3(Vec3::ZERO);
            let line = spot.pose.transform_point3(Vec3::ZERO);
            // The line is between the middle of the screen and its bottom
            // edge, whatever height the seat's eye is at.
            let drop = centre.y - line.y;
            assert!(drop > 0.0 && drop < room.fit.y / 2.0, "{name:?}: {drop}");
            // The whole panel is on the screen, and no wider than it.
            let half = spot.size.y / 2.0;
            assert!(drop + half <= room.fit.y / 2.0, "{name:?}: {drop} {half}");
            assert!(spot.size.x <= room.fit.x, "{name:?}: {}", spot.size.x);
            assert!((spot.size.y - spot.size.x * ASPECT).abs() < 1e-5);
            // In front of the screen, straight ahead of it.
            assert!(line.z > centre.z && line.z - centre.z < 0.1, "{name:?}");
            assert!((line.x - centre.x).abs() < 1e-5, "{name:?}");
        }
    }

    #[test]
    fn subtitles_are_not_wider_than_the_distance_allows() {
        let layout = Place::Cinema.layout();
        let front = layout.seat_index(SeatName::Front).unwrap();
        let room = room_frame(Mat4::IDENTITY, &layout, front).unwrap();
        // Near the screen the distance is the limit, as with a screen of one's own.
        let near = subtitle_spot(&room, 3.0, ASPECT);
        assert!((near.size.x - 2.7).abs() < 1e-5, "{}", near.size.x);
        // Far from it the screen is.
        let far = subtitle_spot(&room, 20.0, ASPECT);
        let wide = 0.9 * room.fit.x;
        assert!((far.size.x - wide).abs() < 1e-5, "{}", far.size.x);
    }

    #[test]
    fn subtitles_turn_with_the_screen() {
        let layout = Place::Cinema.layout();
        let anchor = Mat4::from_translation(Vec3::new(0.5, 1.2, -0.3))
            * Mat4::from_rotation_y(0.7)
            * Mat4::from_rotation_x(0.4);
        let room = room_frame(anchor, &layout, layout.default_seat).unwrap();
        let spot = subtitle_spot(&room, room.distance, ASPECT);
        let local = room.screen_pose.inverse() * spot.pose;
        let (_, rot, pos) = local.to_scale_rotation_translation();
        let want = Vec3::new(0.0, -SUBTITLE_DROP * room.fit.y, SUBTITLE_LIFT);
        assert!((pos - want).length() < 1e-4, "{pos}");
        assert!(rot.angle_between(glam::Quat::IDENTITY) < 1e-4);
    }

    #[test]
    fn a_sky_has_no_room_to_place() {
        let layout = Place::Studio.layout();
        assert!(room_frame(Mat4::IDENTITY, &layout, 0).is_none());
    }
}
