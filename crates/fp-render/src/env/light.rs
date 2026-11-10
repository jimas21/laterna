//! The picture's light on its place, on the CPU: the screen cut into patches
//! (the cells `fs_glow` in scene.wgsl averages the picture over) and the
//! colours measured for them, eased so a cut in the film moves the room's
//! light over a quarter of a second instead of flashing it.
//!
//! Nothing here touches the GPU. The renderer reads the colours back from
//! `crate::glow`, feeds them to [`ScreenLight::update`] every frame, and
//! writes the result and [`screen_patches`] into the place's uniform.

use crate::params::ScreenShape;
use glam::{Mat4, Vec2, Vec3, Vec4};

/// Patches across the screen (`GLOW_COLS` in scene.wgsl).
pub const COLS: usize = 4;
/// Patches down the screen (`GLOW_ROWS` in scene.wgsl).
pub const ROWS: usize = 2;
/// All the patches, row by row from the top left: patch `row * COLS + col`.
pub const PATCHES: usize = COLS * ROWS;

/// The time, seconds, a colour takes to cover 63% of the way to a new one.
pub const TAU: f64 = 0.25;

/// What the picture lights the place with now: a linear colour for each
/// patch, easing towards what was last measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScreenLight {
    /// Linear light, 1.0 the screen's white.
    pub rgb: [Vec3; PATCHES],
    /// The time of the last update, seconds; `None` before the first.
    pub at: Option<f64>,
}

impl ScreenLight {
    /// Eases the colours towards `sample` for the time that has passed since
    /// the last update (none at the first). `None` is no picture: the colours
    /// ease towards black.
    pub fn update(&mut self, sample: Option<[Vec3; PATCHES]>, time: f64) {
        let dt = self.at.map_or(0.0, |at| (time - at).max(0.0));
        self.at = Some(self.at.map_or(time, |at| at.max(time)));
        let k = (1.0 - (-dt / TAU).exp()) as f32;
        let target = sample.unwrap_or([Vec3::ZERO; PATCHES]);
        for (colour, goal) in self.rgb.iter_mut().zip(target) {
            *colour += (goal - *colour) * k;
        }
    }

    /// Black, as if never updated.
    pub fn reset(&mut self) {
        *self = ScreenLight::default();
    }

    /// The mean of the patches.
    pub fn mean(&self) -> Vec3 {
        self.rgb.iter().sum::<Vec3>() / PATCHES as f32
    }
}

/// The screen as patches in room space: xyz the centre of each cell of the
/// picture and w its area in square metres, and the screen's normal towards
/// the audience. `screen_pose` puts the screen (centred on its origin,
/// facing +z, bending towards +z when `arc` is not zero) in the world,
/// `world_to_room` puts the world in the place's own space, and `size` is
/// the screen's width along its arc and height in metres.
///
/// A curved screen's patches lie on the cylinder, but the one normal is the
/// screen's middle one.
pub fn screen_patches(
    screen_pose: Mat4,
    size: Vec2,
    arc: f32,
    world_to_room: Mat4,
) -> ([Vec4; PATCHES], Vec3) {
    let shape = ScreenShape {
        width: size.x,
        height: size.y,
        arc,
    };
    let to_room = world_to_room * screen_pose;
    let area = size.x * size.y / PATCHES as f32;
    let mut patches = [Vec4::ZERO; PATCHES];
    for row in 0..ROWS {
        for col in 0..COLS {
            // Cell centres, -1 to 1 left to right and bottom to top.
            let u = -1.0 + (2 * col + 1) as f32 / COLS as f32;
            let v = 1.0 - (2 * row + 1) as f32 / ROWS as f32;
            let centre = to_room.transform_point3(shape.point(u, v));
            patches[row * COLS + col] = centre.extend(area);
        }
    }
    let normal = to_room.transform_vector3(Vec3::Z).normalize_or_zero();
    (patches, normal)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Vec2 = Vec2::new(6.0, 3.0);

    fn pose() -> Mat4 {
        let turn = Mat4::from_rotation_y(0.7);
        Mat4::from_translation(Vec3::new(1.0, 2.0, -3.0)) * turn
    }

    #[test]
    fn patches_tile_the_screen() {
        let (patches, normal) = screen_patches(pose(), SIZE, 0.0, Mat4::IDENTITY);
        let area: f32 = patches.iter().map(|p| p.w).sum();
        assert!((area - SIZE.x * SIZE.y).abs() < 1e-4, "{area}");
        // Back in the screen's own space every centre is inside the screen,
        // in the middle of its cell: the first is top left, the last bottom
        // right.
        let local: Vec<Vec3> = patches
            .iter()
            .map(|p| pose().inverse().transform_point3(p.truncate()))
            .collect();
        for (i, c) in local.iter().enumerate() {
            assert!(c.x.abs() < SIZE.x / 2.0 && c.y.abs() < SIZE.y / 2.0, "{i}");
            assert!(c.z.abs() < 1e-4, "{i}: off the screen's plane by {}", c.z);
        }
        assert!(local[0].x < 0.0 && local[0].y > 0.0);
        assert!(local[7].x > 0.0 && local[7].y < 0.0);
        assert!((local[0].x + 2.25).abs() < 1e-4 && (local[0].y - 0.75).abs() < 1e-4);
        assert!((local[6].x - 0.75).abs() < 1e-4 && (local[6].y + 0.75).abs() < 1e-4);
        // The normal is the screen's +z, turned with it, and unit length.
        let want = Mat4::from_rotation_y(0.7).transform_vector3(Vec3::Z);
        assert!(normal.abs_diff_eq(want, 1e-5), "{normal}");
    }

    #[test]
    fn the_room_frame_moves_the_patches_and_the_normal() {
        let to_world = Mat4::from_translation(Vec3::new(0.0, 1.6, 0.0));
        let (world, _) = screen_patches(pose(), SIZE, 0.0, Mat4::IDENTITY);
        let (room, normal) = screen_patches(pose(), SIZE, 0.0, to_world.inverse());
        for (w, r) in world.iter().zip(&room) {
            let moved = w.truncate() - Vec3::new(0.0, 1.6, 0.0);
            assert!(r.truncate().abs_diff_eq(moved, 1e-4));
            assert_eq!(r.w, w.w);
        }
        assert!((normal.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn curved_patches_lie_on_the_cylinder() {
        let arc = 1.2;
        let (patches, _) = screen_patches(Mat4::IDENTITY, SIZE, arc, Mat4::IDENTITY);
        // The cylinder bends towards +z with its axis at z = radius.
        let radius = SIZE.x / arc;
        for (i, p) in patches.iter().enumerate() {
            let away = Vec2::new(p.x, p.z - radius).length();
            assert!((away - radius).abs() < 1e-4, "{i}: {away} from the axis");
            assert!(p.z >= -1e-5, "{i}: behind the screen's plane");
        }
        // The outer patches are nearer the audience than the inner ones.
        assert!(patches[0].z > patches[1].z && patches[3].z > patches[2].z);
        let area: f32 = patches.iter().map(|p| p.w).sum();
        assert!((area - SIZE.x * SIZE.y).abs() < 1e-4);
    }

    fn grey(v: f32) -> [Vec3; PATCHES] {
        [Vec3::splat(v); PATCHES]
    }

    #[test]
    fn light_eases_in_a_quarter_second() {
        let mut light = ScreenLight::default();
        // The first update has no time behind it: nothing moves, nothing is
        // NaN, and a second one at the same time is the same.
        light.update(Some(grey(1.0)), 10.0);
        assert_eq!(light.rgb, grey(0.0));
        light.update(Some(grey(1.0)), 10.0);
        assert_eq!(light.rgb, grey(0.0));
        assert!(light.mean().is_finite());
        // One time constant later, 63% of the way.
        light.update(Some(grey(1.0)), 10.0 + TAU);
        let got = light.rgb[3].x;
        assert!((got - 0.632).abs() < 0.002, "{got}");
        // A long time later, there.
        light.update(Some(grey(1.0)), 20.0);
        assert!((light.rgb[0].y - 1.0).abs() < 1e-4);
        // Time that runs backwards moves nothing.
        light.update(Some(grey(0.0)), 5.0);
        assert!((light.rgb[0].y - 1.0).abs() < 1e-4);
    }

    #[test]
    fn no_picture_decays_to_black() {
        let mut light = ScreenLight::default();
        light.update(Some(grey(0.8)), 0.0);
        light.update(Some(grey(0.8)), 30.0);
        assert!((light.mean().x - 0.8).abs() < 1e-4);
        light.update(None, 30.0 + TAU);
        let after = light.mean().x;
        assert!((after - 0.8 * 0.368).abs() < 0.002, "{after}");
        light.update(None, 60.0);
        assert!(light.mean().x < 1e-4);
        light.reset();
        assert_eq!(light, ScreenLight::default());
    }

    #[test]
    fn each_patch_eases_on_its_own() {
        let mut light = ScreenLight::default();
        let mut sample = grey(0.0);
        sample[2] = Vec3::new(1.0, 0.0, 0.0);
        light.update(Some(sample), 0.0);
        light.update(Some(sample), 40.0);
        assert!((light.rgb[2].x - 1.0).abs() < 1e-4);
        assert_eq!(light.rgb[1], Vec3::ZERO);
    }
}
