//! The kit places are built from: quads, grids, boxes, steps, seats, a disc,
//! and two per-vertex bakes (ambient occlusion and the house lights).
//!
//! Everything is deterministic, so tests can assert on the results. A surface
//! is a plane with a normal and its triangles wind counter-clockwise seen from
//! the side the normal points to.

use super::{Material, Pattern, Vertex};
use glam::{Vec2, Vec3};

/// Width of the cells a `grid` is cut into, metres. Small enough that the
/// light baked per vertex is smooth, large enough to keep the count down.
pub const GRID_CELL: f32 = 0.5;

/// How far from a surface another one darkens it, metres.
const AO_RADIUS: f32 = 0.45;

/// The darkest a baked vertex gets.
const AO_FLOOR: f32 = 0.35;

/// Distance between the centres of neighbouring seats in a row.
pub const SEAT_PITCH: f32 = 0.62;

/// An axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn new(min: Vec3, max: Vec3) -> Aabb {
        Aabb { min, max }
    }
}

/// A flat rectangle that darkens the surfaces near it in `bake_ao`: the shell
/// of a room, and risers. `min` to `max` is flat on one axis, and `normal` is
/// the side that faces into the room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AoPlane {
    pub min: Vec3,
    pub max: Vec3,
    pub normal: Vec3,
}

impl AoPlane {
    pub fn new(min: Vec3, max: Vec3, normal: Vec3) -> AoPlane {
        AoPlane { min, max, normal }
    }
}

/// A light for `bake_house`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointLight {
    pub pos: Vec3,
    pub intensity: f32,
}

/// The materials a seat is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeatMaterials {
    /// Cushion and back.
    pub fabric: u32,
    /// Armrests.
    pub trim: u32,
}

/// Steps rising away from the screen, for `MeshBuilder::steps`.
#[derive(Clone, Copy, Debug)]
pub struct Stair<'a> {
    /// Left and right edge.
    pub x: [f32; 2],
    /// Where the first tread starts.
    pub z_front: f32,
    /// Depth of every tread.
    pub depth: f32,
    /// Floor height in front of the first riser.
    pub base_y: f32,
    /// Height of each tread's floor, front to back.
    pub floors: &'a [f32],
}

/// A mesh being built. Quads add four vertices and two triangles each.
#[derive(Clone, Debug, Default)]
pub struct MeshBuilder {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

/// The pattern coordinates of a flat surface: its position in metres along
/// the two axes it spreads over, so a pattern keeps its size on any quad.
pub fn planar_uv(corners: [Vec3; 4], n: Vec3) -> [Vec2; 4] {
    let a = n.abs();
    corners.map(|p| {
        if a.y >= a.x && a.y >= a.z {
            Vec2::new(p.x, p.z)
        } else if a.x >= a.z {
            Vec2::new(p.z, p.y)
        } else {
            Vec2::new(p.x, p.y)
        }
    })
}

/// The three boxes of the seat centred at `x` on a row whose floor is `floor`
/// and whose middle is at `z`: cushion, back and the armrest on its right. The
/// seat faces -z; its back is at +z.
pub fn seat_boxes(x: f32, floor: f32, z: f32) -> [Aabb; 3] {
    let v = Vec3::new;
    let (l, r, y) = (x - 0.25, x + 0.25, floor);
    let cushion = Aabb::new(v(l, y + 0.12, z - 0.25), v(r, y + 0.45, z + 0.2));
    let back = Aabb::new(v(l, y + 0.2, z + 0.2), v(r, y + 0.95, z + 0.32));
    let arm = Aabb::new(v(r, y + 0.4, z - 0.25), v(r + 0.12, y + 0.62, z + 0.2));
    [cushion, back, arm]
}

impl MeshBuilder {
    pub fn new() -> MeshBuilder {
        MeshBuilder::default()
    }

    fn push(&mut self, pos: Vec3, normal: Vec3, uv: Vec2, material: u32) -> u32 {
        let index = self.vertices.len() as u32;
        self.vertices.push(Vertex {
            pos: pos.to_array(),
            normal: normal.to_array(),
            uv: uv.to_array(),
            ao: 1.0,
            house: 0.0,
            material,
        });
        index
    }

    /// One flat quad. `corners` run counter-clockwise seen from the side `n`
    /// points to; `uv` is each corner's pattern coordinates.
    pub fn quad(&mut self, corners: [Vec3; 4], n: Vec3, uv: [Vec2; 4], material: u32) {
        let a = self.push(corners[0], n, uv[0], material);
        let b = self.push(corners[1], n, uv[1], material);
        let c = self.push(corners[2], n, uv[2], material);
        let d = self.push(corners[3], n, uv[3], material);
        self.indices.extend([a, b, c, a, c, d]);
    }

    /// A flat quad cut into cells of about `cell` metres, so light baked per
    /// vertex varies smoothly across it. Corners as for `quad`.
    pub fn grid_quad(
        &mut self,
        corners: [Vec3; 4],
        n: Vec3,
        uv: [Vec2; 4],
        material: u32,
        cell: f32,
    ) {
        let along = (corners[1] - corners[0]).length();
        let up = (corners[3] - corners[0]).length();
        let nx = (along / cell).ceil().max(1.0) as u32;
        let ny = (up / cell).ceil().max(1.0) as u32;
        let base = self.vertices.len() as u32;
        for j in 0..=ny {
            let t = j as f32 / ny as f32;
            for i in 0..=nx {
                let s = i as f32 / nx as f32;
                let bottom = corners[0].lerp(corners[1], s);
                let top = corners[3].lerp(corners[2], s);
                let uv_bottom = uv[0].lerp(uv[1], s);
                let uv_top = uv[3].lerp(uv[2], s);
                let uv_here = uv_bottom.lerp(uv_top, t);
                self.push(bottom.lerp(top, t), n, uv_here, material);
            }
        }
        let stride = nx + 1;
        for j in 0..ny {
            for i in 0..nx {
                let a = base + j * stride + i;
                let (b, c, d) = (a + 1, a + stride + 1, a + stride);
                self.indices.extend([a, b, c, a, c, d]);
            }
        }
    }

    /// A grid quad whose pattern coordinates are metres on the surface.
    pub fn grid(&mut self, corners: [Vec3; 4], n: Vec3, material: u32, cell: f32) {
        self.grid_quad(corners, n, planar_uv(corners, n), material, cell);
    }

    fn face(&mut self, corners: [Vec3; 4], n: Vec3, material: u32) {
        self.quad(corners, n, planar_uv(corners, n), material);
    }

    /// A box with five faces: the bottom is left out, because places are only
    /// seen from above and the side.
    pub fn box_(&mut self, min: Vec3, max: Vec3, material: u32) {
        let v = Vec3::new;
        // Left, right; bottom, top; front (-z), back (+z).
        let (l, r, b, t, f, k) = (min.x, max.x, min.y, max.y, min.z, max.z);
        let top = [v(l, t, k), v(r, t, k), v(r, t, f), v(l, t, f)];
        self.face(top, Vec3::Y, material);
        let back = [v(l, b, k), v(r, b, k), v(r, t, k), v(l, t, k)];
        self.face(back, Vec3::Z, material);
        let front = [v(r, b, f), v(l, b, f), v(l, t, f), v(r, t, f)];
        self.face(front, Vec3::NEG_Z, material);
        let right = [v(r, b, k), v(r, b, f), v(r, t, f), v(r, t, k)];
        self.face(right, Vec3::X, material);
        let left = [v(l, b, f), v(l, b, k), v(l, t, k), v(l, t, f)];
        self.face(left, Vec3::NEG_X, material);
    }

    /// Treads and risers, rising away from the screen: each tread is a grid
    /// of floor at `floors[k]`, and each riser faces the screen.
    pub fn steps(&mut self, s: Stair<'_>, material: u32) {
        let v = Vec3::new;
        let [l, r] = s.x;
        let mut prev = s.base_y;
        for (k, &y) in s.floors.iter().enumerate() {
            let f = s.z_front + s.depth * k as f32;
            let back = f + s.depth;
            if y - prev > 1e-4 {
                let riser = [v(r, prev, f), v(l, prev, f), v(l, y, f), v(r, y, f)];
                self.grid(riser, Vec3::NEG_Z, material, GRID_CELL);
            }
            let tread = [v(l, y, back), v(r, y, back), v(r, y, f), v(l, y, f)];
            self.grid(tread, Vec3::Y, material, GRID_CELL);
            prev = y;
        }
    }

    /// A seat: cushion, back and the armrest on its right (three boxes).
    pub fn seat(&mut self, x: f32, floor: f32, z: f32, m: SeatMaterials) {
        let [cushion, back, arm] = seat_boxes(x, floor, z);
        self.box_(cushion.min, cushion.max, m.fabric);
        self.box_(back.min, back.max, m.fabric);
        self.box_(arm.min, arm.max, m.trim);
    }

    /// Only the armrest a seat at `x` has on its right: for the left end of a
    /// block of seats, which has no neighbour to share one with.
    pub fn armrest(&mut self, x: f32, floor: f32, z: f32, material: u32) {
        let arm = seat_boxes(x, floor, z)[2];
        self.box_(arm.min, arm.max, material);
    }

    /// A flat disc facing up, as rings out to each radius in `radii` (nearest
    /// first, each above zero) of `segments` steps. Rings can be spaced wide
    /// towards the edge, where nobody looks closely.
    pub fn disc(&mut self, centre: Vec3, radii: &[f32], segments: usize, material: u32) {
        if radii.is_empty() || segments < 3 {
            return;
        }
        let up = Vec3::Y;
        let uv = |p: Vec3| Vec2::new(p.x, p.z);
        let mid = self.push(centre, up, uv(centre), material);
        let mut rings = Vec::new();
        for &radius in radii {
            rings.push(self.vertices.len() as u32);
            for i in 0..segments {
                let angle = std::f32::consts::TAU * i as f32 / segments as f32;
                let p = centre + radius * Vec3::new(angle.cos(), 0.0, angle.sin());
                self.push(p, up, uv(p), material);
            }
        }
        let steps = segments as u32;
        for i in 0..steps {
            let j = (i + 1) % steps;
            self.indices.extend([mid, rings[0] + j, rings[0] + i]);
        }
        for pair in rings.windows(2) {
            let (inner, outer) = (pair[0], pair[1]);
            for i in 0..steps {
                let j = (i + 1) % steps;
                self.indices.extend([inner + i, outer + j, outer + i]);
                self.indices.extend([inner + i, inner + j, outer + j]);
            }
        }
    }

    /// Darkens vertices that sit close to another surface. `planes` are the
    /// surfaces that can do it; a vertex is darkened by planes it is in front
    /// of and not parallel to, so a floor is not darkened by itself. The value
    /// is `distance / 0.45` clamped to 0.35 to 1, and 1 for surfaces that glow
    /// or show the picture.
    pub fn bake_ao(&mut self, planes: &[AoPlane], palette: &[Material]) {
        for v in &mut self.vertices {
            let pattern = palette.get(v.material as usize).map(|m| m.pattern);
            if pattern.is_some_and(|p| p.is_emissive() || p == Pattern::Screen) {
                v.ao = 1.0;
                continue;
            }
            let pos = Vec3::from(v.pos);
            let normal = Vec3::from(v.normal);
            let mut nearest = f32::INFINITY;
            for plane in planes {
                if plane.normal.dot(normal).abs() > 0.99 {
                    continue;
                }
                let off = pos - pos.clamp(plane.min, plane.max);
                if plane.normal.dot(off) <= 1e-4 {
                    continue;
                }
                nearest = nearest.min(off.length());
            }
            v.ao = (nearest / AO_RADIUS).clamp(AO_FLOOR, 1.0);
        }
    }

    /// Bakes the share of the house lights that reaches each vertex: the sum
    /// over `lights` of `intensity / (1 + d^2)` times the cosine of the angle
    /// to the surface (no shadows), divided by `full` and clamped to 0 to 1.
    /// Constant glows get 0 (`Emissive`) or 1 (`EmissiveHouse`), which scale
    /// with the house-light level themselves.
    pub fn bake_house(&mut self, lights: &[PointLight], palette: &[Material], full: f32) {
        for v in &mut self.vertices {
            let pattern = palette.get(v.material as usize).map(|m| m.pattern);
            if pattern == Some(Pattern::Emissive) {
                v.house = 0.0;
                continue;
            }
            if pattern == Some(Pattern::EmissiveHouse) {
                v.house = 1.0;
                continue;
            }
            let pos = Vec3::from(v.pos);
            let normal = Vec3::from(v.normal);
            let mut sum = 0.0;
            for light in lights {
                let to = light.pos - pos;
                let facing = to.normalize_or_zero().dot(normal).max(0.0);
                sum += light.intensity / (1.0 + to.length_squared()) * facing;
            }
            v.house = (sum / full).clamp(0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Vec<Material> {
        let m = Material {
            albedo: [0.5; 3],
            pattern: Pattern::Plain,
            emissive: 0.0,
            sheen: 0.0,
        };
        vec![m]
    }

    fn glow(pattern: Pattern) -> Material {
        Material {
            albedo: [1.0; 3],
            pattern,
            emissive: 1.0,
            sheen: 0.0,
        }
    }

    /// A `w` by `h` rectangle in the z = 0 plane, facing +z.
    fn rect(w: f32, h: f32) -> [Vec3; 4] {
        let v = Vec3::new;
        [Vec3::ZERO, v(w, 0.0, 0.0), v(w, h, 0.0), v(0.0, h, 0.0)]
    }

    /// A unit square facing up at height `y`, with corners from (x, z) to
    /// (x + 1, z + 1).
    fn square(x: f32, y: f32, z: f32) -> [Vec3; 4] {
        let v = Vec3::new;
        let (x1, z1) = (x + 1.0, z + 1.0);
        [v(x, y, z1), v(x1, y, z1), v(x1, y, z), v(x, y, z)]
    }

    #[test]
    fn a_quad_is_four_vertices_and_two_triangles() {
        let mut m = MeshBuilder::new();
        let c = rect(1.0, 1.0);
        m.quad(c, Vec3::Z, planar_uv(c, Vec3::Z), 0);
        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.indices, [0, 1, 2, 0, 2, 3]);
    }

    #[test]
    fn a_grid_is_cut_into_cells_and_keeps_its_corners() {
        let mut m = MeshBuilder::new();
        let c = rect(2.0, 1.0);
        m.grid(c, Vec3::Z, 0, 0.5);
        // 4 by 2 cells.
        assert_eq!(m.vertices.len(), 5 * 3);
        assert_eq!(m.indices.len(), 4 * 2 * 6);
        let last = Vec3::from(m.vertices[14].pos);
        assert!((last - c[2]).length() < 1e-5);
        let uv = m.vertices[14].uv;
        assert!((uv[0] - 2.0).abs() < 1e-5 && (uv[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_box_has_five_faces_and_stays_inside_its_bounds() {
        let mut m = MeshBuilder::new();
        let (min, max) = (Vec3::new(1.0, 2.0, 3.0), Vec3::new(2.0, 4.0, 6.0));
        m.box_(min, max, 0);
        assert_eq!(m.vertices.len(), 5 * 4);
        assert_eq!(m.indices.len(), 5 * 6);
        for v in &m.vertices {
            let p = Vec3::from(v.pos);
            assert!(p.cmpge(min).all() && p.cmple(max).all());
        }
    }

    #[test]
    fn steps_rise_with_a_riser_each_and_none_where_flat() {
        let mut m = MeshBuilder::new();
        let floors = [0.0, 0.5, 1.0];
        let s = Stair {
            x: [0.0, 1.0],
            z_front: 2.0,
            depth: 1.0,
            base_y: 0.0,
            floors: &floors,
        };
        m.steps(s, 0);
        // Three treads of 2 by 2 cells; two risers of 2 by 1 cells.
        assert_eq!(m.indices.len(), (3 * 4 + 2 * 2) * 6);
        let top = m.vertices.iter().map(|v| v.pos[1]).fold(0.0, f32::max);
        assert!((top - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_disc_covers_its_radius_and_faces_up() {
        let mut m = MeshBuilder::new();
        m.disc(Vec3::new(0.0, -1.0, 0.0), &[1.0, 3.0], 16, 0);
        assert_eq!(m.vertices.len(), 1 + 2 * 16);
        // The fan: 16 triangles; between the rings: 16 quads of 2.
        assert_eq!(m.indices.len(), (16 + 16 * 2) * 3);
        for v in &m.vertices {
            let p = Vec3::from(v.pos);
            assert!((p.y + 1.0).abs() < 1e-6 && p.length() <= 3.2);
        }
    }

    #[test]
    fn a_seat_is_three_boxes_inside_its_pitch() {
        let mut m = MeshBuilder::new();
        let (fabric, trim) = (0, 0);
        m.seat(0.0, 1.0, 5.0, SeatMaterials { fabric, trim });
        assert_eq!(m.vertices.len(), 3 * 5 * 4);
        let [cushion, back, arm] = seat_boxes(0.0, 1.0, 5.0);
        // The armrest ends where the next seat's cushion starts.
        let next = seat_boxes(SEAT_PITCH, 1.0, 5.0)[0];
        assert!((arm.max.x - next.min.x).abs() < 1e-5);
        assert!(back.max.y > cushion.max.y);
    }

    #[test]
    fn ao_is_dark_near_other_surfaces_and_open_elsewhere() {
        let mut m = MeshBuilder::new();
        let v = Vec3::new;
        // A wall facing +x, with an up-facing square 0.1 m and 2 m from it.
        let wall = AoPlane::new(v(0.0, 0.0, -5.0), v(0.0, 5.0, 5.0), Vec3::X);
        let (near, far) = (square(0.1, 1.0, 0.0), square(2.0, 1.0, 0.0));
        m.quad(near, Vec3::Y, planar_uv(near, Vec3::Y), 0);
        m.quad(far, Vec3::Y, planar_uv(far, Vec3::Y), 0);
        m.bake_ao(&[wall], &plain());
        assert!((m.vertices[0].ao - 0.35).abs() < 1e-5);
        assert!((m.vertices[4].ao - 1.0).abs() < 1e-5);
    }

    #[test]
    fn ao_ignores_the_surface_a_vertex_lies_on() {
        let mut m = MeshBuilder::new();
        let floor = square(0.0, 0.0, 0.0);
        m.quad(floor, Vec3::Y, planar_uv(floor, Vec3::Y), 0);
        let v = Vec3::new;
        let plane = AoPlane::new(v(-5.0, 0.0, -5.0), v(5.0, 0.0, 5.0), Vec3::Y);
        m.bake_ao(&[plane], &plain());
        assert!(m.vertices.iter().all(|v| v.ao == 1.0));
    }

    #[test]
    fn house_light_falls_off_and_needs_a_facing_surface() {
        let lit = square(0.0, 0.0, 0.0);
        let light = PointLight {
            pos: Vec3::new(0.5, 2.0, 0.5),
            intensity: 10.0,
        };
        let mut m = MeshBuilder::new();
        m.quad(lit, Vec3::Y, planar_uv(lit, Vec3::Y), 0);
        m.bake_house(&[light], &plain(), 2.0);
        let houses: Vec<f32> = m.vertices.iter().map(|v| v.house).collect();
        assert!(houses.iter().all(|&h| h > 0.5 && h <= 1.0));
        // From below, the same square gets nothing.
        let mut m = MeshBuilder::new();
        m.quad(lit, Vec3::NEG_Y, planar_uv(lit, Vec3::NEG_Y), 0);
        m.bake_house(&[light], &plain(), 2.0);
        assert!(m.vertices.iter().all(|v| v.house == 0.0));
    }

    #[test]
    fn glows_are_not_baked() {
        let palette = [glow(Pattern::Emissive), glow(Pattern::EmissiveHouse)];
        let mut m = MeshBuilder::new();
        let c = rect(1.0, 1.0);
        m.quad(c, Vec3::Z, planar_uv(c, Vec3::Z), 0);
        m.quad(c, Vec3::Z, planar_uv(c, Vec3::Z), 1);
        let v = Vec3::new;
        // Close enough to the first vertices to darken them, if they were baked.
        let wall = AoPlane::new(v(-0.1, 0.0, -1.0), v(-0.1, 1.0, 1.0), Vec3::X);
        m.bake_ao(&[wall], &palette);
        let light = PointLight {
            pos: v(0.0, 0.0, 3.0),
            intensity: 5.0,
        };
        m.bake_house(&[light], &palette, 1.0);
        let (first, second) = m.vertices.split_at(4);
        assert!(first.iter().all(|v| v.ao == 1.0 && v.house == 0.0));
        assert!(second.iter().all(|v| v.ao == 1.0 && v.house == 1.0));
    }
}
