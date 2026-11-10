//! The cinema: a stadium hall of our own design, built from the kit with no
//! assets. Twelve rows of seats climb away from a 14 m wide screen; the walls
//! are fabric panels between wood strips with light slots, the ceiling has a
//! grid of downlights, and the aisle steps have amber lights. Nothing in it
//! carries a name or a logo.
//!
//! Room space: the screen is the plane z = 0 facing +z, the stage floor is
//! y = 0, and the audience sits towards +z, up the steps.

use super::kit::{self, AoPlane, MeshBuilder, PointLight, SeatMaterials, Stair};
use super::{Layout, Lighting, Material, Pattern, PlaceData, ScreenFrame, Seat, SeatName, Sky};
use glam::{Vec2, Vec3};

/// The side walls are at x = plus and minus this.
pub const HALF_WIDTH: f32 = 9.0;
/// The front wall, 5 cm behind the screen panel.
pub const FRONT_Z: f32 = -0.05;
pub const BACK_Z: f32 = 22.5;
pub const CEILING_Y: f32 = 10.0;
/// Rows of seats.
pub const ROWS: usize = 12;
/// An eye's height above the floor of its row.
pub const EYE_HEIGHT: f32 = 1.15;
pub const SCREEN_CENTRE: Vec3 = Vec3::new(0.0, 4.4, 0.0);
/// The largest picture: 14 m wide, 7.2 m tall.
pub const SCREEN_MAX: Vec2 = Vec2::new(14.0, 7.2);

const CELL: f32 = kit::GRID_CELL;
/// Depth of a row's tread.
const TREAD: f32 = 1.15;
/// Where the first riser stands; the floor is flat in front of it.
const FIRST_RISER_Z: f32 = 5.425;
const STAGE_HALF_WIDTH: f32 = 8.0;
const STAGE_DEPTH: f32 = 2.0;
const STAGE_HEIGHT: f32 = 0.5;
/// The aisles' left and right edges. The seats leave them clear: the centre
/// block ends with its armrest at 4.09, and the side blocks start at 5.13.
const AISLES: [[f32; 2]; 2] = [[-5.13, -4.1], [4.1, 5.13]];
/// The first seat's centre and the seat count of each block of a row.
const BLOCKS: [(f32, usize); 3] = [(-3.72, 13), (-7.98, 5), (5.5, 5)];
/// Wood strips on each side wall.
const STRIPS: usize = 10;
const STRIP_PITCH: f32 = 2.45;
const STRIP_WIDTH: f32 = 0.12;
const STRIP_DEPTH: f32 = 0.06;
const DOWNLIGHT_X: [f32; 5] = [-6.0, -3.0, 0.0, 3.0, 6.0];
const DOWNLIGHT_Z: [f32; 5] = [3.0, 7.0, 11.0, 15.0, 19.0];
const DOWNLIGHT_POWER: f32 = 30.0;
const SLOT_POWER: f32 = 12.0;
/// The bake sum that counts as fully lit.
const HOUSE_FULL: f32 = 3.0;

/// Indices into the palette.
mod mat {
    pub const SCREEN: u32 = 0;
    pub const SURROUND: u32 = 1;
    pub const CARPET: u32 = 2;
    pub const SEAT: u32 = 3;
    pub const ARMREST: u32 = 4;
    pub const WALL: u32 = 5;
    pub const WOOD: u32 = 6;
    pub const CEILING: u32 = 7;
    pub const STEP_LIGHT: u32 = 8;
    pub const SLOT: u32 = 9;
    pub const DOWNLIGHT: u32 = 10;
    pub const BOOTH: u32 = 11;
    pub const STAGE: u32 = 12;
    pub const BACK_WALL: u32 = 13;
}

/// Floor height of row `k`, counting from the front.
fn row_floor(k: usize) -> f32 {
    0.25 + 0.42 * k as f32
}

/// Where row `k`'s seats stand, front to back.
fn row_z(k: usize) -> f32 {
    6.0 + TREAD * k as f32
}

/// Where the last row's tread ends and the flat rear platform begins.
fn rear_start() -> f32 {
    FIRST_RISER_Z + TREAD * ROWS as f32
}

/// The floor height at depth `z`.
pub fn floor_at(z: f32) -> f32 {
    if z < FIRST_RISER_Z {
        return 0.0;
    }
    let row = ((z - FIRST_RISER_Z) / TREAD) as usize;
    row_floor(row.min(ROWS - 1))
}

fn seat(name: SeatName, row: usize) -> Seat {
    let eye = Vec3::new(0.0, row_floor(row) + EYE_HEIGHT, row_z(row) + 0.1);
    Seat { name, eye }
}

/// The seats (front, middle and back, in the middle of rows 3, 7 and 11) and
/// the screen's frame.
pub fn layout() -> Layout {
    let seats = vec![
        seat(SeatName::Front, 3),
        seat(SeatName::Middle, 7),
        seat(SeatName::Back, 11),
    ];
    Layout {
        seats,
        default_seat: 1,
        screen: Some(ScreenFrame {
            centre: SCREEN_CENTRE,
            max: SCREEN_MAX,
            curvature: 0.0,
        }),
        floor_y: 0.0,
    }
}

fn material(albedo: [f32; 3], pattern: Pattern, emissive: f32, sheen: f32) -> Material {
    Material {
        albedo,
        pattern,
        emissive,
        sheen,
    }
}

/// In the order of the `mat` indices.
fn materials() -> Vec<Material> {
    vec![
        material([0.75, 0.75, 0.75], Pattern::Screen, 0.0, 0.0),
        material([0.01, 0.01, 0.012], Pattern::Plain, 0.0, 0.0),
        material([0.05, 0.05, 0.06], Pattern::Carpet, 0.0, 0.0),
        material([0.02, 0.12, 0.13], Pattern::Fabric, 0.0, 0.4),
        material([0.02, 0.02, 0.025], Pattern::Plain, 0.0, 0.0),
        material([0.06, 0.08, 0.11], Pattern::Fabric, 0.0, 0.1),
        material([0.3, 0.17, 0.08], Pattern::Wood, 0.0, 0.1),
        material([0.04, 0.04, 0.045], Pattern::Plain, 0.0, 0.0),
        material([1.0, 0.55, 0.12], Pattern::Emissive, 1.5, 0.0),
        material([1.0, 0.85, 0.6], Pattern::EmissiveHouse, 4.0, 0.0),
        material([1.0, 0.9, 0.7], Pattern::EmissiveHouse, 6.0, 0.0),
        material([0.6, 0.8, 1.0], Pattern::EmissivePicture, 3.0, 0.0),
        material([0.04, 0.03, 0.025], Pattern::Wood, 0.0, 0.1),
        material([0.04, 0.05, 0.07], Pattern::Fabric, 0.0, 0.1),
    ]
}

fn lighting() -> Lighting {
    let ambient_up = [0.03, 0.028, 0.026];
    Lighting {
        ambient_up,
        ambient_down: ambient_up.map(|c| c * 0.03),
        house_rgb: [2.2, 1.9, 1.5],
        spill: 1.0,
        fog: None,
    }
}

/// The meshes, palette and lighting.
pub fn build() -> PlaceData {
    let palette = materials();
    let mut m = MeshBuilder::new();
    hall(&mut m);
    floor_and_stage(&mut m);
    screen_panel(&mut m);
    seating(&mut m);
    wall_strips(&mut m);
    glows(&mut m);
    m.bake_ao(&ao_planes(), &palette);
    m.bake_house(&house_lights(), &palette, HOUSE_FULL);
    PlaceData {
        vertices: m.vertices,
        indices: m.indices,
        palette,
        lighting: lighting(),
        sky: Sky::None,
    }
}

/// The walls and the ceiling.
fn hall(m: &mut MeshBuilder) {
    let v = Vec3::new;
    let (l, r) = (-HALF_WIDTH, HALF_WIDTH);
    let (f, k, top) = (FRONT_Z, BACK_Z, CEILING_Y);
    let rear = row_floor(ROWS - 1);
    let front = [v(l, 0.0, f), v(r, 0.0, f), v(r, top, f), v(l, top, f)];
    m.grid(front, Vec3::Z, mat::SURROUND, CELL);
    let back = [v(r, rear, k), v(l, rear, k), v(l, top, k), v(r, top, k)];
    m.grid(back, Vec3::NEG_Z, mat::BACK_WALL, CELL);
    let left = [v(l, 0.0, k), v(l, 0.0, f), v(l, top, f), v(l, top, k)];
    m.grid(left, Vec3::X, mat::WALL, CELL);
    let right = [v(r, 0.0, f), v(r, 0.0, k), v(r, top, k), v(r, top, f)];
    m.grid(right, Vec3::NEG_X, mat::WALL, CELL);
    let ceiling = [v(l, top, f), v(r, top, f), v(r, top, k), v(l, top, k)];
    m.grid(ceiling, Vec3::NEG_Y, mat::CEILING, CELL);
}

/// The flat floor in front of the first riser, the stage on it and the flat
/// platform behind the last row.
fn floor_and_stage(m: &mut MeshBuilder) {
    let v = Vec3::new;
    let (l, r, f, n) = (-HALF_WIDTH, HALF_WIDTH, FRONT_Z, FIRST_RISER_Z);
    let flat = [v(l, 0.0, n), v(r, 0.0, n), v(r, 0.0, f), v(l, 0.0, f)];
    m.grid(flat, Vec3::Y, mat::CARPET, CELL);
    let (rear, s, k) = (row_floor(ROWS - 1), rear_start(), BACK_Z);
    let platform = [v(l, rear, k), v(r, rear, k), v(r, rear, s), v(l, rear, s)];
    m.grid(platform, Vec3::Y, mat::CARPET, CELL);
    let w = STAGE_HALF_WIDTH;
    let (min, max) = (v(-w, 0.0, 0.0), v(w, STAGE_HEIGHT, STAGE_DEPTH));
    m.box_(min, max, mat::STAGE);
}

/// The corners of the largest picture the screen shows.
fn screen_bounds() -> (Vec2, Vec2) {
    let half = SCREEN_MAX / 2.0;
    let centre = Vec2::new(SCREEN_CENTRE.x, SCREEN_CENTRE.y);
    (centre - half, centre + half)
}

/// The white panel the size of the frame, so letterbox bars look like a real
/// screen. Its pattern coordinates run 0 to 1 across it.
fn screen_panel(m: &mut MeshBuilder) {
    let v = Vec3::new;
    let (lo, hi) = screen_bounds();
    let (x0, y0, x1, y1, z) = (lo.x, lo.y, hi.x, hi.y, SCREEN_CENTRE.z);
    let panel = [v(x0, y0, z), v(x1, y0, z), v(x1, y1, z), v(x0, y1, z)];
    let uv = [Vec2::ZERO, Vec2::X, Vec2::ONE, Vec2::Y];
    m.quad(panel, Vec3::Z, uv, mat::SCREEN);
}

/// Treads, risers and the seats on them.
fn seating(m: &mut MeshBuilder) {
    let floors: Vec<f32> = (0..ROWS).map(row_floor).collect();
    let stair = Stair {
        x: [-HALF_WIDTH, HALF_WIDTH],
        z_front: FIRST_RISER_Z,
        depth: TREAD,
        base_y: 0.0,
        floors: &floors,
    };
    m.steps(stair, mat::CARPET);
    let seat = SeatMaterials {
        fabric: mat::SEAT,
        trim: mat::ARMREST,
    };
    for row in 0..ROWS {
        let (floor, z) = (row_floor(row), row_z(row));
        for (start, count) in BLOCKS {
            for i in 0..count {
                m.seat(start + kit::SEAT_PITCH * i as f32, floor, z, seat);
            }
            m.armrest(start - kit::SEAT_PITCH, floor, z, mat::ARMREST);
        }
    }
}

/// One wood strip on a side wall.
struct Strip {
    /// -1 on the left wall, 1 on the right.
    side: f32,
    z: f32,
    /// The floor at the strip.
    floor: f32,
    /// Whether it carries a light slot: every other one.
    lit: bool,
}

fn strips() -> Vec<Strip> {
    let mut out = Vec::new();
    for i in 0..STRIPS {
        let z = 0.1 + STRIP_PITCH * i as f32;
        for side in [-1.0, 1.0] {
            out.push(Strip {
                side,
                z,
                floor: floor_at(z),
                lit: i.is_multiple_of(2),
            });
        }
    }
    out
}

/// Wood strips on the side walls, with a light slot in every other one,
/// 2.5 to 4 m above the floor there.
fn wall_strips(m: &mut MeshBuilder) {
    let v = Vec3::new;
    for s in strips() {
        let wall = s.side * HALF_WIDTH;
        let inner = wall - s.side * STRIP_DEPTH;
        let (lo, hi) = (wall.min(inner), wall.max(inner));
        let top = CEILING_Y - 0.01;
        let (min, max) = (v(lo, s.floor, s.z), v(hi, top, s.z + STRIP_WIDTH));
        m.box_(min, max, mat::WOOD);
        if !s.lit {
            continue;
        }
        let x = inner - s.side * 0.005;
        let (b, t) = (s.floor + 2.5, s.floor + 4.0);
        let (f, k) = (s.z + 0.03, s.z + 0.09);
        let slot = if s.side < 0.0 {
            [v(x, b, k), v(x, b, f), v(x, t, f), v(x, t, k)]
        } else {
            [v(x, b, f), v(x, b, k), v(x, t, k), v(x, t, f)]
        };
        let n = Vec3::new(-s.side, 0.0, 0.0);
        m.quad(slot, n, kit::planar_uv(slot, n), mat::SLOT);
    }
}

/// The lights: the booth window, the ceiling downlights and the amber light
/// on every aisle step's nose.
fn glows(m: &mut MeshBuilder) {
    let v = Vec3::new;
    let (l, r, b, t, z) = (-0.6, 0.6, 8.0, 8.8, BACK_Z - 0.005);
    let window = [v(r, b, z), v(l, b, z), v(l, t, z), v(r, t, z)];
    let n = Vec3::NEG_Z;
    m.quad(window, n, kit::planar_uv(window, n), mat::BOOTH);

    let (h, y) = (0.15, CEILING_Y - 0.005);
    let n = Vec3::NEG_Y;
    for x in DOWNLIGHT_X {
        for z in DOWNLIGHT_Z {
            let (l, r, f, k) = (x - h, x + h, z - h, z + h);
            let disc = [v(l, y, f), v(r, y, f), v(r, y, k), v(l, y, k)];
            let uv = kit::planar_uv(disc, n);
            m.quad(disc, n, uv, mat::DOWNLIGHT);
        }
    }

    let n = Vec3::Y;
    for row in 0..ROWS {
        let y = row_floor(row) + 0.004;
        let f = FIRST_RISER_Z + TREAD * row as f32;
        let k = f + 0.06;
        for [l, r] in AISLES {
            let nose = [v(l, y, k), v(r, y, k), v(r, y, f), v(l, y, f)];
            let uv = kit::planar_uv(nose, n);
            m.quad(nose, n, uv, mat::STEP_LIGHT);
        }
    }
}

/// The surfaces that darken what is close to them: the shell, every riser and
/// every tread.
fn ao_planes() -> Vec<AoPlane> {
    let v = Vec3::new;
    let (w, top, f, k) = (HALF_WIDTH, CEILING_Y, FRONT_Z, BACK_Z);
    let mut planes = vec![
        AoPlane::new(v(-w, 0.0, f), v(-w, top, k), Vec3::X),
        AoPlane::new(v(w, 0.0, f), v(w, top, k), Vec3::NEG_X),
        AoPlane::new(v(-w, 0.0, f), v(w, top, f), Vec3::Z),
        AoPlane::new(v(-w, 0.0, k), v(w, top, k), Vec3::NEG_Z),
        AoPlane::new(v(-w, top, f), v(w, top, k), Vec3::NEG_Y),
        AoPlane::new(v(-w, 0.0, f), v(w, 0.0, FIRST_RISER_Z), Vec3::Y),
    ];
    let mut prev = 0.0;
    for row in 0..ROWS {
        let y = row_floor(row);
        let z = FIRST_RISER_Z + TREAD * row as f32;
        planes.push(AoPlane::new(v(-w, prev, z), v(w, y, z), Vec3::NEG_Z));
        planes.push(AoPlane::new(v(-w, y, z), v(w, y, z + TREAD), Vec3::Y));
        prev = y;
    }
    let rear = rear_start();
    planes.push(AoPlane::new(v(-w, prev, rear), v(w, prev, k), Vec3::Y));
    planes
}

/// The ceiling downlights and the wall slots, for the house-light bake.
fn house_lights() -> Vec<PointLight> {
    let mut lights = Vec::new();
    for x in DOWNLIGHT_X {
        for z in DOWNLIGHT_Z {
            lights.push(PointLight {
                pos: Vec3::new(x, CEILING_Y - 0.05, z),
                intensity: DOWNLIGHT_POWER,
            });
        }
    }
    for s in strips().into_iter().filter(|s| s.lit) {
        let x = s.side * (HALF_WIDTH - STRIP_DEPTH - 0.1);
        lights.push(PointLight {
            pos: Vec3::new(x, s.floor + 3.25, s.z + 0.06),
            intensity: SLOT_POWER,
        });
    }
    lights
}

#[cfg(test)]
mod tests {
    use super::kit::Aabb;
    use super::*;

    /// Every seat's boxes, armrests included.
    fn seat_boxes() -> Vec<Aabb> {
        let mut boxes = Vec::new();
        for row in 0..ROWS {
            let (floor, z) = (row_floor(row), row_z(row));
            for (start, count) in BLOCKS {
                for i in 0..count {
                    let x = start + kit::SEAT_PITCH * i as f32;
                    boxes.extend(kit::seat_boxes(x, floor, z));
                }
                boxes.push(kit::seat_boxes(start - kit::SEAT_PITCH, floor, z)[2]);
            }
        }
        boxes
    }

    /// Everything between the audience and the screen that can block a view:
    /// the seats, the risers and the stage.
    fn obstacles() -> Vec<Aabb> {
        let v = Vec3::new;
        let w = HALF_WIDTH;
        let mut boxes = seat_boxes();
        for row in 0..ROWS {
            let prev = row.checked_sub(1).map_or(0.0, row_floor);
            let zr = FIRST_RISER_Z + TREAD * row as f32;
            let (min, max) = (v(-w, prev, zr), v(w, row_floor(row), zr + 0.01));
            boxes.push(Aabb::new(min, max));
        }
        let sw = STAGE_HALF_WIDTH;
        let (min, max) = (v(-sw, 0.0, 0.0), v(sw, STAGE_HEIGHT, STAGE_DEPTH));
        boxes.push(Aabb::new(min, max));
        boxes
    }

    /// Whether the segment from `a` to `b` passes through the box.
    fn segment_hits(a: Vec3, b: Vec3, bx: &Aabb) -> bool {
        let d = b - a;
        let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            let (o, dir) = (a.dot(axis), d.dot(axis));
            let (lo, hi) = (bx.min.dot(axis), bx.max.dot(axis));
            if dir.abs() < 1e-9 {
                if !(lo..=hi).contains(&o) {
                    return false;
                }
                continue;
            }
            let (ta, tb) = ((lo - o) / dir, (hi - o) / dir);
            t0 = t0.max(ta.min(tb));
            t1 = t1.min(ta.max(tb));
            if t0 > t1 {
                return false;
            }
        }
        true
    }

    #[test]
    fn no_row_rises_into_any_seats_view_of_the_screen() {
        let layout = layout();
        let screen = layout.screen.unwrap();
        let bottom = screen.centre.y - screen.max.y / 2.0;
        let boxes = obstacles();
        for seat in &layout.seats {
            let name = seat.name;
            // The bottom edge of the screen: its middle and both corners.
            for x in [0.0, -screen.max.x / 2.0, screen.max.x / 2.0] {
                let target = Vec3::new(x, bottom, screen.centre.z);
                let blocked = boxes.iter().find(|b| segment_hits(seat.eye, target, b));
                assert!(blocked.is_none(), "{name:?} to x {x}: {blocked:?}");
            }
        }
    }

    #[test]
    fn the_sight_line_test_can_fail() {
        // A seat box straight ahead of the middle seat, as high as its eye.
        let eye = layout().seats[1].eye;
        let target = Vec3::new(0.0, 0.8, 0.0);
        let wall = Aabb::new(
            Vec3::new(-1.0, 0.0, eye.z - 2.0),
            Vec3::new(1.0, eye.y, eye.z - 1.0),
        );
        assert!(segment_hits(eye, target, &wall));
    }

    #[test]
    fn seats_sit_inside_the_hall_with_headroom() {
        let layout = layout();
        for seat in &layout.seats {
            let e = seat.eye;
            let name = seat.name;
            assert!(e.x.abs() < HALF_WIDTH, "{name:?}");
            assert!((FRONT_Z..BACK_Z).contains(&e.z), "{name:?}");
            let above = e.y - floor_at(e.z);
            assert!((1.0..=1.3).contains(&above), "{name:?}: {above}");
            assert!(CEILING_Y - e.y >= 2.5, "{name:?}");
        }
        let screen = layout.screen.unwrap();
        let (lo, hi) = screen_bounds();
        assert!(lo.x >= -HALF_WIDTH && hi.x <= HALF_WIDTH);
        assert!(lo.y >= STAGE_HEIGHT && hi.y <= CEILING_Y);
        assert!(screen.centre.z > FRONT_Z);
    }

    #[test]
    fn the_middle_seat_looks_straight_at_the_picture() {
        let layout = layout();
        let seat = layout.seat(layout.default_seat);
        assert_eq!(seat.name, SeatName::Middle);
        let screen = layout.screen.unwrap();
        assert!((seat.eye.y - screen.centre.y).abs() < 0.3);
    }

    #[test]
    fn the_seats_offered_are_front_middle_and_back_from_near_to_far() {
        let layout = layout();
        let names: Vec<SeatName> = layout.seats.iter().map(|s| s.name).collect();
        let want = [SeatName::Front, SeatName::Middle, SeatName::Back];
        assert_eq!(names, want);
        assert!(layout.seats.windows(2).all(|w| w[0].eye.z < w[1].eye.z));
    }

    #[test]
    fn there_are_276_seats_and_the_aisles_stay_clear() {
        let seats: usize = BLOCKS.iter().map(|b| b.1).sum();
        assert_eq!(seats * ROWS, 276);
        assert_eq!(seat_boxes().len(), (seats * 3 + BLOCKS.len()) * ROWS);
        for b in seat_boxes() {
            for [lo, hi] in AISLES {
                let clear = b.max.x <= lo + 1e-3 || b.min.x >= hi - 1e-3;
                assert!(clear, "{b:?} is in the aisle {lo} to {hi}");
            }
        }
    }

    #[test]
    fn the_palette_covers_every_material_index() {
        assert_eq!(materials().len(), mat::BACK_WALL as usize + 1);
    }

    #[test]
    fn house_lights_reach_the_hall_and_glows_follow_their_kind() {
        let data = build();
        let pattern = |v: &crate::env::Vertex| data.palette[v.material as usize].pattern;
        let mut lit = Vec::new();
        let mut darkest = 1.0_f32;
        for v in &data.vertices {
            match pattern(v) {
                Pattern::EmissiveHouse => assert_eq!(v.house, 1.0),
                Pattern::Emissive | Pattern::EmissivePicture => assert_eq!(v.house, 0.0),
                _ => lit.push(v.house),
            }
            darkest = darkest.min(v.ao);
        }
        let brightest = lit.iter().copied().fold(0.0, f32::max);
        let dimmest = lit.iter().copied().fold(1.0, f32::min);
        assert!(brightest >= 0.99, "{brightest}");
        assert!(dimmest < brightest, "{dimmest}");
        // Under the seats the occlusion bake finds its creases.
        assert!(darkest < 0.4, "{darkest}");
    }

    #[test]
    fn the_screen_panel_is_the_size_of_the_frame() {
        let data = build();
        let panel: Vec<Vec3> = data
            .vertices
            .iter()
            .filter(|v| v.material == mat::SCREEN)
            .map(|v| Vec3::from(v.pos))
            .collect();
        assert_eq!(panel.len(), 4);
        let min = panel.iter().fold(Vec3::MAX, |a, &p| a.min(p));
        let max = panel.iter().fold(Vec3::MIN, |a, &p| a.max(p));
        assert!((max.x - min.x - SCREEN_MAX.x).abs() < 1e-4);
        assert!((max.y - min.y - SCREEN_MAX.y).abs() < 1e-4);
    }

    #[test]
    fn floor_at_follows_the_treads() {
        assert_eq!(floor_at(1.0), 0.0);
        assert!((floor_at(row_z(0)) - 0.25).abs() < 1e-6);
        assert!((floor_at(row_z(7)) - row_floor(7)).abs() < 1e-6);
        assert!((floor_at(BACK_Z) - row_floor(ROWS - 1)).abs() < 1e-6);
    }
}
