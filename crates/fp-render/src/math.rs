//! Projection and view matrices following OpenXR conventions, for Vulkan clip
//! space (x right, y down, depth 0..1).

use glam::{Mat4, Quat, Vec3};

/// Field of view as OpenXR reports it: angles in radians, left and down
/// negative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fov {
    pub left: f32,
    pub right: f32,
    pub up: f32,
    pub down: f32,
}

impl Fov {
    /// Symmetric field of view, `deg` degrees wide and tall.
    pub fn symmetric(deg: f32) -> Fov {
        let h = (deg / 2.0).to_radians();
        Fov {
            left: -h,
            right: h,
            up: h,
            down: -h,
        }
    }
}

/// Port of `XrMatrix4x4f_CreateProjectionFov` for Vulkan.
pub fn projection(fov: Fov, near: f32, far: f32) -> Mat4 {
    let tan_l = fov.left.tan();
    let tan_r = fov.right.tan();
    let tan_d = fov.down.tan();
    let tan_u = fov.up.tan();
    let w = tan_r - tan_l;
    let h = tan_d - tan_u; // Vulkan: y points down
    let mut m = [0.0f32; 16];
    m[0] = 2.0 / w;
    m[8] = (tan_r + tan_l) / w;
    m[5] = 2.0 / h;
    m[9] = (tan_u + tan_d) / h;
    m[10] = -far / (far - near);
    m[14] = -(far * near) / (far - near);
    m[11] = -1.0;
    Mat4::from_cols_array(&m)
}

/// How many pixels of an eye image `width_px` wide fit in one unit of tangent
/// (the width of the view at distance 1) at the middle of the view, for the
/// projection `proj` made by [`projection`]. A flat thing `t` wide at distance
/// `d` spans `t / d * pixels_per_tan` pixels there.
pub fn pixels_per_tan(proj: &Mat4, width_px: u32) -> f32 {
    width_px as f32 * proj.x_axis.x / 2.0
}

/// World → eye transform for an eye at `position` with `orientation`.
pub fn view(position: Vec3, orientation: Quat) -> Mat4 {
    Mat4::from_rotation_translation(orientation, position).inverse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_per_tan_counts_the_pixels_of_a_unit_of_tangent() {
        // 90 degrees across is two units of tangent: 500 pixels each.
        let p = projection(Fov::symmetric(90.0), 0.05, 100.0);
        assert!((pixels_per_tan(&p, 1000) - 500.0).abs() < 0.01);
        // A view tilted to one side has the same pixels per tangent.
        let skew = projection(
            Fov {
                left: -0.9,
                right: 0.5,
                up: 0.7,
                down: -0.7,
            },
            0.05,
            100.0,
        );
        let width = 0.5f32.tan() + 0.9f32.tan();
        assert!((pixels_per_tan(&skew, 1000) - 1000.0 / width).abs() < 0.01);
    }

    #[test]
    fn forward_maps_to_centre_and_up_to_top() {
        let p = projection(Fov::symmetric(90.0), 0.05, 100.0);
        let c = p * glam::Vec4::new(0.0, 0.0, -1.0, 1.0);
        assert!((c.x / c.w).abs() < 1e-6 && (c.y / c.w).abs() < 1e-6);
        let up = p * glam::Vec4::new(0.0, 0.5, -1.0, 1.0);
        assert!(
            up.y / up.w < 0.0,
            "up should be towards -y in Vulkan clip space"
        );
        let z = p * glam::Vec4::new(0.0, 0.0, -0.05, 1.0);
        assert!((z.z / z.w).abs() < 1e-5, "near plane maps to depth 0");
    }
}
