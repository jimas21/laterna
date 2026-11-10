//! Where the window controls go, apart from the app: pure placement.
//!
//! Everything is worked out in the picture's frame: the eye at the origin
//! looking down -Z, the screen `distance` metres ahead and upright. Sizes are
//! given in degrees (as the eye sees them) and turned into metres at the
//! control's own distance, so a control looks the same size however far or
//! wide the screen is.

use crate::app::BAR_DISTANCE;
use crate::world::{place, place_around};
use fp_render::ScreenShape;
use glam::{Mat4, Vec2, Vec3};

/// What the controls look like and where they sit, in degrees at Interface
/// size 1 (all of them scale with it). They are guesses until compared with
/// the Frame's own windows, so every number is here.
pub struct Sizes {
    /// The move bar as drawn: a pill this wide and tall.
    pub pill: Vec2,
    /// What a ray can hit around it: roomier than the pill.
    pub bar_hit: Vec2,
    /// From the screen's bottom edge to the top of the pill.
    pub gap: f32,
    /// The resize handle: the square a ray can hit (a side).
    pub corner_hit: f32,
    /// The quarter ring it is drawn as, hugging the screen's corner from
    /// outside: its radius and thickness.
    pub ring_radius: f32,
    pub ring_stroke: f32,
    /// How far the square's middle is out from the corner, right and down
    /// each.
    pub corner_out: f32,
}

pub const LOOK: Sizes = Sizes {
    pill: Vec2::new(6.0, 0.7),
    bar_hit: Vec2::new(8.0, 1.8),
    gap: 1.0,
    corner_hit: 3.5,
    ring_radius: 2.0,
    ring_stroke: 0.7,
    corner_out: 1.1,
};

impl Sizes {
    /// The pill's share of the move bar's panel, across and down.
    pub fn pill_share(&self) -> Vec2 {
        self.pill / self.bar_hit
    }

    /// Where the screen's corner is in the resize handle's panel, as a share
    /// of it across and down (the ring is centred on it).
    pub fn ring_centre(&self) -> Vec2 {
        Vec2::splat(0.5 - self.corner_out / self.corner_hit)
    }
}

/// Metres across `deg` degrees at `distance` metres.
pub fn deg_to_m(deg: f32, distance: f32) -> f32 {
    2.0 * distance * (deg.to_radians() / 2.0).tan()
}

/// A control's place in the picture's frame: where its middle is, how it is
/// turned (to face the eye, upright) and its size in metres.
#[derive(Clone, Copy, Debug)]
pub struct Spot {
    pub pose: Mat4,
    pub size: Vec2,
}

/// A panel `distance` metres from the eye in the direction `yaw` (left
/// positive) and `pitch` (up positive), degrees, facing the eye, `across`
/// degrees wide and tall at Interface size `scale`.
fn facing_eye(yaw: f32, pitch: f32, distance: f32, across: Vec2, scale: f32) -> Spot {
    let wide = deg_to_m(across.x * scale, distance);
    let tall = deg_to_m(across.y * scale, distance);
    Spot {
        pose: place_around(Mat4::IDENTITY, yaw, pitch, distance, 0.0),
        size: Vec2::new(wide, tall),
    }
}

/// The move bar: under the middle of the screen's bottom edge, on the sphere
/// through that edge.
pub fn move_bar_spot(shape: &ScreenShape, distance: f32, scale: f32) -> Spot {
    let half = shape.height / 2.0;
    let edge = half.atan2(distance).to_degrees();
    let down = edge + (LOOK.gap + LOOK.pill.y / 2.0) * scale;
    facing_eye(0.0, -down, half.hypot(distance), LOOK.bar_hit, scale)
}

/// The resize handle: at the screen's bottom right corner, its square a
/// little out from it so the ring hugs the corner from outside.
pub fn corner_spot(shape: &ScreenShape, distance: f32, scale: f32) -> Spot {
    let corner = Vec3::new(0.0, 0.0, -distance) + shape.point(1.0, -1.0);
    let yaw = (-corner.x).atan2(-corner.z).to_degrees();
    let pitch = corner.y.atan2(corner.x.hypot(corner.z)).to_degrees();
    let out = LOOK.corner_out * scale;
    // A degree to the right is wider in yaw the higher or lower it is.
    let right = out / pitch.to_radians().cos().max(0.1);
    let across = Vec2::splat(LOOK.corner_hit);
    facing_eye(yaw - right, pitch - out, corner.length(), across, scale)
}

/// A region as the eye sees it, in degrees: across (`yaw`, left positive) and
/// how far below level (`down`), each as (least, most).
#[derive(Clone, Copy, Debug)]
pub struct Sky {
    pub yaw: (f32, f32),
    pub down: (f32, f32),
}

impl Sky {
    /// What a flat rectangle `size` metres across at `pose` covers, from the
    /// eye at the origin. The edges' middles count as well as the corners: a
    /// straight edge reaches further up or down at its middle.
    pub fn of(pose: Mat4, size: Vec2) -> Sky {
        let mut sky = Sky {
            yaw: (f32::MAX, f32::MIN),
            down: (f32::MAX, f32::MIN),
        };
        for (u, v) in [
            (-1.0, -1.0),
            (0.0, -1.0),
            (1.0, -1.0),
            (-1.0, 0.0),
            (1.0, 0.0),
            (-1.0, 1.0),
            (0.0, 1.0),
            (1.0, 1.0),
        ] {
            let p = pose.transform_point3(Vec3::new(u * size.x / 2.0, v * size.y / 2.0, 0.0));
            let yaw = (-p.x).atan2(-p.z).to_degrees();
            let down = (-p.y).atan2(p.x.hypot(p.z)).to_degrees();
            sky.yaw = (sky.yaw.0.min(yaw), sky.yaw.1.max(yaw));
            sky.down = (sky.down.0.min(down), sky.down.1.max(down));
        }
        sky
    }

    /// The two cover some of the same directions, or come within `margin`
    /// degrees of it.
    pub fn overlaps(&self, other: &Sky, margin: f32) -> bool {
        self.yaw.0 < other.yaw.1 + margin
            && other.yaw.0 < self.yaw.1 + margin
            && self.down.0 < other.down.1 + margin
            && other.down.0 < self.down.1 + margin
    }
}

/// Where the control bar hangs when nothing needs it moved: the fraction of
/// its distance that its middle is below where it faces, as it always was.
pub const BAR_DROP: f32 = 0.32 / BAR_DISTANCE;

/// The control bar is tilted back towards the eye by this much, degrees.
const BAR_TILT: f32 = 22.0;

/// The control bar's pose in the frame `bar_frame` (where it is placed from),
/// its middle `drop` (a fraction of `distance`) below where that faces.
pub fn bar_pose(bar_frame: Mat4, distance: f32, drop: f32) -> Mat4 {
    place(bar_frame, 0.0, distance, -drop * distance, BAR_TILT)
}

/// How far from a window control the control bar's body must stay, degrees.
const BAR_CLEAR: f32 = 0.5;

/// The lowest and highest the control bar's middle may hang, degrees below
/// level: close enough to read, not so far down it needs a bowed head.
const BAR_REACH: (f32, f32) = (8.0, 35.0);

/// The control bar's body: its size in metres and its middle's place on the
/// panel, from the panel's middle (the panel is its body and the shadow
/// round it).
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub size: Vec2,
    pub centre: Vec2,
}

/// How far below level the control bar's middle hangs (see [`bar_pose`]) so
/// its body does not cover any of `avoid`: where it always was unless that
/// would, then lower, just clear of what it would cover. `bar_frame` is
/// where the bar is placed from, in the picture's frame (turned by where the
/// user moved the bar to), `distance` how far away it is.
pub fn clear_drop(bar_frame: Mat4, distance: f32, body: Body, avoid: &[Sky]) -> f32 {
    let at = |drop: f32| {
        let pose = bar_pose(bar_frame, distance, drop);
        let to_body = Mat4::from_translation(body.centre.extend(0.0));
        Sky::of(pose * to_body, body.size)
    };
    let mut drop = BAR_DROP;
    for _ in 0..4 {
        let bar = at(drop);
        let covered = avoid
            .iter()
            .filter(|a| bar.overlaps(a, BAR_CLEAR))
            .map(|a| a.down.1)
            .fold(f32::MIN, f32::max);
        if covered == f32::MIN {
            break;
        }
        // Lower it by how far its top is above the bottom of what it covers,
        // and the gap to keep. (It moves about that far for a change in the
        // angle of its middle.)
        let lower = (covered + BAR_CLEAR - bar.down.0).max(0.1);
        drop = (drop.atan() + lower.to_radians()).tan();
    }
    let degrees = drop.atan().to_degrees();
    if (BAR_REACH.0..=BAR_REACH.1).contains(&degrees) {
        drop
    } else {
        degrees.clamp(BAR_REACH.0, BAR_REACH.1).to_radians().tan()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The control bar's panel at Interface size 1 (see `growing_panel` and
    /// `BAR_PX` in `app`): its body is the panel less the shadow's room.
    fn bar_body(scale: f32) -> Body {
        let per_point = 1.2429 / 1160.0 * scale;
        let room = crate::ui::widgets::SHADOW_ROOM;
        let (l, r) = (room.left as f32, room.right as f32);
        let (t, b) = (room.top as f32, room.bottom as f32);
        Body {
            size: Vec2::new(1160.0 - l - r, 250.7 - t - b) * per_point,
            centre: Vec2::new((l - r) / 2.0, (b - t) / 2.0) * per_point,
        }
    }

    fn shape(width: f32, aspect: f32, curvature: f32) -> ScreenShape {
        ScreenShape {
            width,
            height: width / aspect,
            arc: curvature * std::f32::consts::TAU / 3.0,
        }
    }

    /// The angle a control's width and height each cover at the eye, degrees.
    fn covers(spot: &Spot) -> Vec2 {
        let from_eye = spot.pose.w_axis.truncate().length();
        let deg = |m: f32| 2.0 * (m / 2.0 / from_eye).atan().to_degrees();
        Vec2::new(deg(spot.size.x), deg(spot.size.y))
    }

    #[test]
    fn the_bar_and_corner_keep_their_angle() {
        for distance in [1.0, 2.5, 4.0, 10.0, 20.0] {
            for width in [0.5, 3.0, 6.0, 15.0, 30.0] {
                for scale in [0.8, 1.0, 1.5] {
                    for curvature in [0.0, 1.0] {
                        let s = shape(width, 16.0 / 9.0, curvature);
                        let want = LOOK.bar_hit * scale;
                        let got = covers(&move_bar_spot(&s, distance, scale));
                        let what = format!("{distance} m, {width} m wide, x{scale}");
                        assert!((got.x / want.x - 1.0).abs() < 0.02, "bar {what}: {got}");
                        assert!((got.y / want.y - 1.0).abs() < 0.02, "bar {what}: {got}");
                        let want = LOOK.corner_hit * scale;
                        let got = covers(&corner_spot(&s, distance, scale));
                        assert!((got.x / want - 1.0).abs() < 0.02, "corner {what}: {got}");
                        assert!((got.y / want - 1.0).abs() < 0.02, "corner {what}: {got}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_corner_sits_on_the_corner() {
        for curvature in [0.0, 0.5, 1.0] {
            let s = shape(6.0, 16.0 / 9.0, curvature);
            let spot = corner_spot(&s, 4.0, 1.0);
            let corner = Vec3::new(0.0, 0.0, -4.0) + s.point(1.0, -1.0);
            // The corner is in the square, up and to the left of its middle
            // by the offset the ring is drawn for.
            let local = spot.pose.inverse().transform_point3(corner);
            assert!(local.x.abs() < spot.size.x / 2.0, "{curvature}: {local}");
            assert!(local.y.abs() < spot.size.y / 2.0, "{curvature}: {local}");
            assert!(local.x < 0.0 && local.y > 0.0, "{curvature}: {local}");
            let share = LOOK.ring_centre();
            let drawn_x = share.x * spot.size.x - spot.size.x / 2.0;
            let drawn_y = spot.size.y / 2.0 - share.y * spot.size.y;
            assert!((drawn_x - local.x).abs() < 0.02, "{curvature}: {local}");
            assert!((drawn_y - local.y).abs() < 0.02, "{curvature}: {local}");
            // And the square is turned to face the eye.
            let facing = spot.pose.transform_vector3(Vec3::Z);
            let to_eye = -spot.pose.w_axis.truncate().normalize();
            assert!(facing.dot(to_eye) > 0.999, "{curvature}");
        }
    }

    #[test]
    fn the_move_bar_hangs_below_the_middle_of_the_bottom_edge() {
        let s = shape(6.0, 16.0 / 9.0, 0.0);
        let spot = move_bar_spot(&s, 4.0, 1.0);
        let sky = Sky::of(spot.pose, spot.size);
        let edge = (s.height / 2.0).atan2(4.0).to_degrees();
        // Clear of the picture, close under it, and in the middle.
        assert!(sky.down.0 > edge, "{sky:?}");
        assert!(sky.down.0 < edge + LOOK.gap, "{sky:?}");
        assert!((sky.yaw.0 + sky.yaw.1).abs() < 0.01, "{sky:?}");
    }

    /// Where the window controls sit for a screen, as the control bar must
    /// avoid them.
    fn controls(s: &ScreenShape, distance: f32, scale: f32) -> [Sky; 2] {
        let bar = move_bar_spot(s, distance, scale);
        let corner = corner_spot(s, distance, scale);
        [
            Sky::of(bar.pose, bar.size),
            Sky::of(corner.pose, corner.size),
        ]
    }

    #[test]
    fn the_default_bar_is_where_it_was() {
        // A 16:9 screen, 6 m wide at 4 m, at the usual interface size.
        let s = shape(6.0, 16.0 / 9.0, 0.0);
        let avoid = controls(&s, 4.0, 1.0);
        let drop = clear_drop(Mat4::IDENTITY, BAR_DISTANCE, bar_body(1.0), &avoid);
        assert_eq!(drop, BAR_DROP);
        // The pose it gives is the one the bar always had.
        let was = place(Mat4::IDENTITY, 0.0, BAR_DISTANCE, -0.32, 22.0);
        let now = bar_pose(Mat4::IDENTITY, BAR_DISTANCE, drop);
        assert!(was.abs_diff_eq(now, 1e-5));
        // With nothing to avoid it never moves.
        for scale in [0.8, 1.0, 1.5] {
            let drop = clear_drop(Mat4::IDENTITY, BAR_DISTANCE, bar_body(scale), &[]);
            assert_eq!(drop, BAR_DROP);
        }
    }

    /// The control bar's body in the picture's frame, hung at `drop`.
    fn bar_sky(body: Body, drop: f32) -> Sky {
        let pose = bar_pose(Mat4::IDENTITY, BAR_DISTANCE, drop);
        let at = Mat4::from_translation(body.centre.extend(0.0));
        Sky::of(pose * at, body.size)
    }

    #[test]
    fn the_control_bar_never_covers_the_window_controls() {
        let lowest = BAR_REACH.1.to_radians().tan();
        let aspects = [16.0 / 9.0, 2.35, 9.0 / 16.0, 1.0];
        for aspect in aspects {
            for width in [1.5, 3.0, 6.0, 12.0] {
                for distance in [2.0, 4.0, 8.0] {
                    for scale in [0.8, 1.0, 1.5] {
                        let avoid = controls(&shape(width, aspect, 0.0), distance, scale);
                        let body = bar_body(scale);
                        let drop = clear_drop(Mat4::IDENTITY, BAR_DISTANCE, body, &avoid);
                        let bar = bar_sky(body, drop);
                        let what = format!("{aspect:.2}, {width} m at {distance} m, x{scale}");
                        for a in avoid {
                            // Unless it has gone as low as it may.
                            let covers = bar.overlaps(&a, 0.0);
                            assert!(!covers || drop > lowest - 1e-3, "{what}: {bar:?} {a:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_bar_stays_in_comfortable_reach() {
        for width in [0.5, 3.0, 6.0, 30.0] {
            for distance in [1.0, 4.0, 20.0] {
                for scale in [0.8, 1.5] {
                    let s = shape(width, 16.0 / 9.0, 0.0);
                    let avoid = controls(&s, distance, scale);
                    let drop = clear_drop(Mat4::IDENTITY, BAR_DISTANCE, bar_body(scale), &avoid);
                    let degrees = drop.atan().to_degrees();
                    assert!((8.0 - 1e-3..=35.0 + 1e-3).contains(&degrees), "{degrees}");
                }
            }
        }
    }

    #[test]
    fn a_bar_the_user_moved_is_avoided_where_it_is() {
        // The user has turned the bar 20 degrees to the right: it now covers
        // the corner of a modest screen, which it did not before.
        let s = shape(4.0, 16.0 / 9.0, 0.0);
        let avoid = controls(&s, 4.0, 1.0);
        let straight = clear_drop(Mat4::IDENTITY, BAR_DISTANCE, bar_body(1.0), &avoid);
        let frame = place_around(Mat4::IDENTITY, -20.0, 0.0, 0.0, 0.0);
        let body = bar_body(1.0);
        let turned = clear_drop(frame, BAR_DISTANCE, body, &avoid);
        assert!(turned >= straight - 1e-4);
        let pose = bar_pose(frame, BAR_DISTANCE, turned);
        let at = Mat4::from_translation(body.centre.extend(0.0));
        let bar = Sky::of(pose * at, body.size);
        assert!(!bar.overlaps(&avoid[1], 0.0), "{bar:?}");
    }
}
