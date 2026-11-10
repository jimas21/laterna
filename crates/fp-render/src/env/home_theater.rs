//! The home theater: a small dark room of our own design, built from the kit
//! with no assets. A three-seat couch faces a 3.3 m wide screen on the front
//! wall, a second row of two armchairs stands on a low riser behind it, and
//! acoustic cloth panels in wood frames line the walls between warm wall
//! sconces. Nothing in it carries a name or a logo.
//!
//! It is the cinema's small sister and costs less to draw: one room, a flat
//! floor and a riser, a few dozen boxes. A common failure of a small room is
//! to look doll-sized, so the sizes are a real living room's: a 3 m ceiling,
//! a couch 2.5 m wide, a seat 45 cm high and the eye 1.05 m up.
//!
//! Room space: the screen is the plane z = 0 facing +z, the floor is y = 0,
//! and the seats are towards +z.

use super::kit::{self, Aabb, AoPlane, MeshBuilder, PanelMaterials, PointLight, Stair, WallPanel};
use super::{Layout, Lighting, Material, Pattern, PlaceData, ScreenFrame, Seat, SeatName, Sky};
use glam::{Vec2, Vec3};

/// The side walls are at x = plus and minus this: the room is 5.6 m wide.
pub const HALF_WIDTH: f32 = 2.8;
/// The front wall, 5 cm behind the screen panel.
pub const FRONT_Z: f32 = -0.05;
/// The back wall: the room is 7 m deep.
pub const BACK_Z: f32 = 6.95;
pub const CEILING_Y: f32 = 3.0;
/// An eye's height above the floor under its seat.
pub const EYE_HEIGHT: f32 = 1.05;
/// The screen's frame is 3.3 m by 1.86 m, its middle 1.25 m up the front
/// wall.
pub const SCREEN_CENTRE: Vec3 = Vec3::new(0.0, 1.25, 0.0);
pub const SCREEN_MAX: Vec2 = Vec2::new(3.3, 1.86);

const CELL: f32 = kit::GRID_CELL;
/// Where the couch's eye is, metres from the screen.
const COUCH_EYE_Z: f32 = 3.4;
/// The armchairs' eye, on the riser.
const CHAIR_EYE_Z: f32 = 4.8;
/// Where the second row's two armchairs stand across the room.
const CHAIR_X: [f32; 2] = [-0.55, 0.55];
/// The riser under the second row starts here and runs to the back wall.
const RISER_Z: f32 = 4.15;
const RISER_HEIGHT: f32 = 0.25;

/// Half the couch's width, arms included.
const COUCH_HALF_WIDTH: f32 = 1.25;
/// The couch's front, where its backrest starts, and its back.
const COUCH_FRONT: f32 = COUCH_EYE_Z - 0.45;
const COUCH_SEAT_BACK: f32 = COUCH_EYE_Z + 0.15;
const COUCH_BACK: f32 = COUCH_EYE_Z + 0.35;
/// Half an armchair's width, arms included, and its front, backrest and back.
const CHAIR_HALF_WIDTH: f32 = 0.45;
const CHAIR_FRONT: f32 = CHAIR_EYE_Z - 0.4;
const CHAIR_SEAT_BACK: f32 = CHAIR_EYE_Z + 0.2;
const CHAIR_BACK: f32 = CHAIR_EYE_Z + 0.4;

/// Wall panels: cloth in a wood frame, 5 cm bars standing 5 cm out.
const FRAME: f32 = 0.05;
const FRAME_DEPTH: f32 = 0.05;
/// The panels on each side wall: where along z, how big, how high.
const SIDE_PANEL_Z: [f32; 4] = [1.0, 2.6, 4.2, 5.8];
const SIDE_PANEL_SIZE: Vec2 = Vec2::new(1.0, 1.5);
const SIDE_PANEL_Y: f32 = 1.55;
/// The tall panels either side of the screen: their x, size and height.
const FLANK_X: f32 = 2.25;
const FLANK_SIZE: Vec2 = Vec2::new(0.9, 2.0);
const FLANK_Y: f32 = 1.5;
/// The panels on the back wall: their x, size and height.
const BACK_PANEL_X: [f32; 3] = [-1.7, 0.0, 1.7];
const BACK_PANEL_SIZE: Vec2 = Vec2::new(1.4, 1.4);
const BACK_PANEL_Y: f32 = 1.75;

/// The wall sconces, two on each side wall: their depths and height.
const SCONCE_Z: [f32; 2] = [1.8, 5.0];
const SCONCE_Y: f32 = 1.8;
/// A sconce's depth out from the wall, height and width along the wall.
const SCONCE_SIZE: Vec3 = Vec3::new(0.1, 0.36, 0.14);
const SCONCE_POWER: f32 = 16.0;
/// The bake sum that counts as fully lit.
const HOUSE_FULL: f32 = 2.0;

/// Indices into the palette.
mod mat {
    pub const SCREEN: u32 = 0;
    pub const SURROUND: u32 = 1;
    pub const CARPET: u32 = 2;
    pub const COUCH: u32 = 3;
    pub const ARMCHAIR: u32 = 4;
    pub const LEGS: u32 = 5;
    pub const WALL: u32 = 6;
    pub const CEILING: u32 = 7;
    pub const CLOTH: u32 = 8;
    pub const WOOD: u32 = 9;
    pub const SCONCE: u32 = 10;
}

/// The floor height at depth `z`: flat, then the riser's top.
pub fn floor_at(z: f32) -> f32 {
    if z < RISER_Z { 0.0 } else { RISER_HEIGHT }
}

/// The seats (the couch, and the armchair on the left of the second row) and
/// the screen's frame.
pub fn layout() -> Layout {
    let eye = Vec3::new(0.0, EYE_HEIGHT, COUCH_EYE_Z);
    let couch = Seat {
        name: SeatName::Couch,
        eye,
    };
    let eye = Vec3::new(CHAIR_X[0], RISER_HEIGHT + EYE_HEIGHT, CHAIR_EYE_Z);
    let back = Seat {
        name: SeatName::Back,
        eye,
    };
    Layout {
        seats: vec![couch, back],
        default_seat: 0,
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
        material([0.012, 0.012, 0.014], Pattern::Plain, 0.0, 0.0),
        material([0.045, 0.04, 0.05], Pattern::Carpet, 0.0, 0.0),
        material([0.11, 0.06, 0.045], Pattern::Fabric, 0.0, 0.35),
        material([0.07, 0.08, 0.1], Pattern::Fabric, 0.0, 0.3),
        material([0.015, 0.015, 0.015], Pattern::Plain, 0.0, 0.0),
        material([0.05, 0.045, 0.05], Pattern::Plain, 0.0, 0.0),
        material([0.035, 0.035, 0.04], Pattern::Plain, 0.0, 0.0),
        material([0.09, 0.1, 0.095], Pattern::Fabric, 0.0, 0.1),
        material([0.25, 0.14, 0.07], Pattern::Wood, 0.0, 0.1),
        material([1.0, 0.72, 0.42], Pattern::EmissiveHouse, 5.0, 0.0),
    ]
}

fn lighting() -> Lighting {
    let ambient_up = [0.03, 0.028, 0.026];
    Lighting {
        ambient_up,
        ambient_down: ambient_up.map(|c| c * 0.03),
        house_rgb: [2.0, 1.6, 1.2],
        // The screen is close, so its light on the walls is strong: less of
        // it than in a hall.
        spill: 0.5,
        fog: None,
    }
}

/// The meshes, palette and lighting.
pub fn build() -> PlaceData {
    let palette = materials();
    let mut m = MeshBuilder::new();
    hall(&mut m);
    floor(&mut m);
    screen_panel(&mut m);
    for piece in furniture() {
        let Aabb { min, max } = piece.bounds;
        m.chamfered_box(min, max, piece.chamfer, piece.material);
    }
    wall_panels(&mut m);
    sconces(&mut m);
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
    let rear = RISER_HEIGHT;
    let front = [v(l, 0.0, f), v(r, 0.0, f), v(r, top, f), v(l, top, f)];
    m.grid(front, Vec3::Z, mat::SURROUND, CELL);
    let back = [v(r, rear, k), v(l, rear, k), v(l, top, k), v(r, top, k)];
    m.grid(back, Vec3::NEG_Z, mat::WALL, CELL);
    let left = [v(l, 0.0, k), v(l, 0.0, f), v(l, top, f), v(l, top, k)];
    m.grid(left, Vec3::X, mat::WALL, CELL);
    let right = [v(r, 0.0, f), v(r, 0.0, k), v(r, top, k), v(r, top, f)];
    m.grid(right, Vec3::NEG_X, mat::WALL, CELL);
    let ceiling = [v(l, top, f), v(r, top, f), v(r, top, k), v(l, top, k)];
    m.grid(ceiling, Vec3::NEG_Y, mat::CEILING, CELL);
}

/// The flat carpet in front of the riser, and the riser with its top.
fn floor(m: &mut MeshBuilder) {
    let v = Vec3::new;
    let (l, r, f, n) = (-HALF_WIDTH, HALF_WIDTH, FRONT_Z, RISER_Z);
    let flat = [v(l, 0.0, n), v(r, 0.0, n), v(r, 0.0, f), v(l, 0.0, f)];
    m.grid(flat, Vec3::Y, mat::CARPET, CELL);
    let floors = [RISER_HEIGHT];
    let stair = Stair {
        x: [l, r],
        z_front: RISER_Z,
        depth: BACK_Z - RISER_Z,
        base_y: 0.0,
        floors: &floors,
    };
    m.steps(stair, mat::CARPET);
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

/// One box of the furniture.
struct Piece {
    bounds: Aabb,
    /// How much the top edges are cut; none for a plain box.
    chamfer: f32,
    material: u32,
}

/// A piece from `x`, `y` and `z` as (low, high) pairs.
fn piece(x: [f32; 2], y: [f32; 2], z: [f32; 2], chamfer: f32, material: u32) -> Piece {
    let min = Vec3::new(x[0], y[0], z[0]);
    let max = Vec3::new(x[1], y[1], z[1]);
    Piece {
        bounds: Aabb::new(min, max),
        chamfer,
        material,
    }
}

/// The couch: a plinth, a base, three seat cushions, three back cushions and
/// an arm at each end.
fn couch() -> Vec<Piece> {
    let w = COUCH_HALF_WIDTH;
    let (f, s, k) = (COUCH_FRONT, COUCH_SEAT_BACK, COUCH_BACK);
    let arm = 0.2;
    let inner = w - arm;
    let (plinth_x, plinth_z) = ([0.04 - w, w - 0.04], [f + 0.05, k - 0.05]);
    let mut pieces = vec![
        piece(plinth_x, [0.0, 0.12], plinth_z, 0.0, mat::LEGS),
        piece([-w, w], [0.12, 0.3], [f, k], 0.03, mat::COUCH),
    ];
    let width = 2.0 * inner / 3.0;
    for i in 0..3 {
        let x0 = -inner + width * i as f32 + 0.005;
        let x = [x0, x0 + width - 0.01];
        pieces.push(piece(x, [0.3, 0.48], [f, s], 0.05, mat::COUCH));
        pieces.push(piece(x, [0.3, 0.92], [s, k], 0.06, mat::COUCH));
    }
    for x0 in [-w, inner] {
        let x = [x0, x0 + arm];
        pieces.push(piece(x, [0.3, 0.62], [f, k], 0.05, mat::COUCH));
    }
    pieces
}

/// An armchair of the second row, on the riser, centred at `x`.
fn armchair(x: f32) -> Vec<Piece> {
    let (l, r) = (x - CHAIR_HALF_WIDTH, x + CHAIR_HALF_WIDTH);
    let (f, s, k) = (CHAIR_FRONT, CHAIR_SEAT_BACK, CHAIR_BACK);
    let y = RISER_HEIGHT;
    // Between the arms.
    let (a, b) = (l + 0.12, r - 0.12);
    let (leg_x, leg_z) = ([l + 0.04, r - 0.04], [f + 0.04, k - 0.04]);
    vec![
        piece(leg_x, [y, y + 0.1], leg_z, 0.0, mat::LEGS),
        piece([l, r], [y + 0.1, y + 0.25], [f, k], 0.03, mat::ARMCHAIR),
        piece([a, b], [y + 0.25, y + 0.45], [f, s], 0.05, mat::ARMCHAIR),
        piece([a, b], [y + 0.25, y + 0.95], [s, k], 0.06, mat::ARMCHAIR),
        piece([l, a], [y + 0.25, y + 0.6], [f, k], 0.04, mat::ARMCHAIR),
        piece([b, r], [y + 0.25, y + 0.6], [f, k], 0.04, mat::ARMCHAIR),
    ]
}

/// All the furniture: the couch and the two armchairs.
fn furniture() -> Vec<Piece> {
    let mut pieces = couch();
    for x in CHAIR_X {
        pieces.extend(armchair(x));
    }
    pieces
}

/// The panels of cloth in wood frames: on each side wall, either side of the
/// screen and on the back wall.
fn panels() -> Vec<WallPanel> {
    let v = Vec3::new;
    let mut out = Vec::new();
    for (side, normal) in [(-1.0, Vec3::X), (1.0, Vec3::NEG_X)] {
        for z in SIDE_PANEL_Z {
            out.push(WallPanel {
                centre: v(side * HALF_WIDTH, SIDE_PANEL_Y, z),
                normal,
                size: SIDE_PANEL_SIZE,
                frame: FRAME,
                depth: FRAME_DEPTH,
            });
        }
    }
    for x in [-FLANK_X, FLANK_X] {
        out.push(WallPanel {
            centre: v(x, FLANK_Y, FRONT_Z),
            normal: Vec3::Z,
            size: FLANK_SIZE,
            frame: FRAME,
            depth: FRAME_DEPTH,
        });
    }
    for x in BACK_PANEL_X {
        out.push(WallPanel {
            centre: v(x, BACK_PANEL_Y, BACK_Z),
            normal: Vec3::NEG_Z,
            size: BACK_PANEL_SIZE,
            frame: FRAME,
            depth: FRAME_DEPTH,
        });
    }
    out
}

fn wall_panels(m: &mut MeshBuilder) {
    let materials = PanelMaterials {
        cloth: mat::CLOTH,
        wood: mat::WOOD,
    };
    for panel in panels() {
        m.framed_panel(panel, materials);
    }
}

/// Which wall (-1 left, 1 right) and how deep each sconce is.
fn sconce_spots() -> Vec<(f32, f32)> {
    let mut spots = Vec::new();
    for side in [-1.0, 1.0] {
        for z in SCONCE_Z {
            spots.push((side, z));
        }
    }
    spots
}

/// The sconces: a small box on the wall that glows with the house lights.
fn sconces(m: &mut MeshBuilder) {
    let half = SCONCE_SIZE * 0.5;
    for (side, z) in sconce_spots() {
        let wall = side * HALF_WIDTH;
        let inner = wall - side * SCONCE_SIZE.x;
        let (lo, hi) = (wall.min(inner), wall.max(inner));
        let min = Vec3::new(lo, SCONCE_Y - half.y, z - half.z);
        let max = Vec3::new(hi, SCONCE_Y + half.y, z + half.z);
        m.box_(min, max, mat::SCONCE);
    }
}

/// The surfaces that darken what is close to them: the shell, the riser and
/// its top.
fn ao_planes() -> Vec<AoPlane> {
    let v = Vec3::new;
    let (w, top, f, k) = (HALF_WIDTH, CEILING_Y, FRONT_Z, BACK_Z);
    let (z, y) = (RISER_Z, RISER_HEIGHT);
    vec![
        AoPlane::new(v(-w, 0.0, f), v(-w, top, k), Vec3::X),
        AoPlane::new(v(w, 0.0, f), v(w, top, k), Vec3::NEG_X),
        AoPlane::new(v(-w, 0.0, f), v(w, top, f), Vec3::Z),
        AoPlane::new(v(-w, y, k), v(w, top, k), Vec3::NEG_Z),
        AoPlane::new(v(-w, top, f), v(w, top, k), Vec3::NEG_Y),
        AoPlane::new(v(-w, 0.0, f), v(w, 0.0, z), Vec3::Y),
        AoPlane::new(v(-w, 0.0, z), v(w, y, z), Vec3::NEG_Z),
        AoPlane::new(v(-w, y, z), v(w, y, k), Vec3::Y),
    ]
}

/// The wall sconces, for the house-light bake: a little out from the wall.
fn house_lights() -> Vec<PointLight> {
    let mut lights = Vec::new();
    for (side, z) in sconce_spots() {
        let x = side * (HALF_WIDTH - SCONCE_SIZE.x - 0.1);
        lights.push(PointLight {
            pos: Vec3::new(x, SCONCE_Y, z),
            intensity: SCONCE_POWER,
        });
    }
    lights
}

#[cfg(test)]
mod tests {
    use super::kit::segment_hits;
    use super::*;

    /// The box round all of `pieces`.
    fn bounds_of(pieces: &[Piece]) -> Aabb {
        let mut min = Vec3::MAX;
        let mut max = Vec3::MIN;
        for p in pieces {
            min = min.min(p.bounds.min);
            max = max.max(p.bounds.max);
        }
        Aabb::new(min, max)
    }

    /// The furniture's boxes (a chamfer only takes material away).
    fn furniture_bounds() -> Vec<Aabb> {
        furniture().iter().map(|p| p.bounds).collect()
    }

    /// The box a framed panel fills.
    fn panel_bounds(p: &WallPanel) -> Aabb {
        let (w, h) = (p.size.x * 0.5, p.size.y * 0.5);
        let half = if p.normal.x == 0.0 {
            Vec3::new(w, h, 0.0)
        } else {
            Vec3::new(0.0, h, w)
        };
        let a = p.centre - half;
        let b = p.centre + half + p.normal * p.depth;
        Aabb::new(a.min(b), a.max(b))
    }

    /// Everything between a seat and the screen that can block a view: the
    /// furniture, the riser and the panels.
    fn obstacles() -> Vec<Aabb> {
        let v = Vec3::new;
        let w = HALF_WIDTH;
        let mut boxes = furniture_bounds();
        let (min, max) = (v(-w, 0.0, RISER_Z), v(w, RISER_HEIGHT, RISER_Z + 0.01));
        boxes.push(Aabb::new(min, max));
        for p in panels() {
            boxes.push(panel_bounds(&p));
        }
        boxes
    }

    #[test]
    fn nothing_comes_between_a_seat_and_the_screen() {
        let layout = layout();
        let screen = layout.screen.unwrap();
        let half = screen.max / 2.0;
        let (bottom, top) = (screen.centre.y - half.y, screen.centre.y + half.y);
        let boxes = obstacles();
        for seat in &layout.seats {
            let name = seat.name;
            // The screen's bottom and top edges: the middle and both corners.
            for y in [bottom, top] {
                for x in [0.0, -half.x, half.x] {
                    let target = Vec3::new(x, y, screen.centre.z);
                    let blocked = boxes.iter().find(|b| segment_hits(seat.eye, target, b));
                    assert!(blocked.is_none(), "{name:?} to x {x}, y {y}: {blocked:?}");
                }
            }
        }
    }

    #[test]
    fn the_sight_line_test_can_fail() {
        // A box straight ahead of the couch, as high as its eye.
        let eye = layout().seats[0].eye;
        let target = Vec3::new(0.0, 0.4, 0.0);
        let wall = Aabb::new(
            Vec3::new(-1.0, 0.0, eye.z - 2.0),
            Vec3::new(1.0, eye.y, eye.z - 1.0),
        );
        assert!(segment_hits(eye, target, &wall));
    }

    #[test]
    fn the_back_row_sees_over_the_couch() {
        let layout = layout();
        let back = layout.seats[layout.seat_index(SeatName::Back).unwrap()];
        let couch_top = bounds_of(&couch()).max.y;
        assert!(back.eye.y - couch_top >= 0.3, "{couch_top}");
        // The line to the bottom of the screen clears the couch's back.
        let bottom = SCREEN_CENTRE.y - SCREEN_MAX.y / 2.0;
        let t = (back.eye.z - COUCH_BACK) / back.eye.z;
        let line_y = back.eye.y + (bottom - back.eye.y) * t;
        assert!(line_y - couch_top >= 0.1, "{line_y} against {couch_top}");
    }

    #[test]
    fn seats_sit_inside_the_room_with_headroom() {
        let layout = layout();
        for seat in &layout.seats {
            let e = seat.eye;
            let name = seat.name;
            assert!(e.x.abs() < HALF_WIDTH, "{name:?}");
            assert!((FRONT_Z..BACK_Z).contains(&e.z), "{name:?}");
            let above = e.y - floor_at(e.z);
            assert!((0.95..=1.15).contains(&above), "{name:?}: {above}");
            assert!(CEILING_Y - e.y >= 1.6, "{name:?}");
        }
        let screen = layout.screen.unwrap();
        let (lo, hi) = screen_bounds();
        assert!(lo.x >= -HALF_WIDTH && hi.x <= HALF_WIDTH);
        assert!(lo.y >= 0.2 && hi.y <= CEILING_Y - 0.5);
        assert!(screen.centre.z > FRONT_Z);
    }

    #[test]
    fn the_room_has_a_real_rooms_size() {
        // Wall to wall, floor to ceiling and the couch, against a doll's room.
        assert!((2.0 * HALF_WIDTH - 5.6).abs() < 1e-4);
        assert!((BACK_Z - FRONT_Z - 7.0).abs() < 1e-4);
        assert!((CEILING_Y - 3.0).abs() < 1e-4);
        assert!((SCREEN_MAX.x - 3.3).abs() < 1e-4);
        assert!((SCREEN_MAX.y - 1.86).abs() < 1e-4);
        let width = bounds_of(&couch()).max.x - bounds_of(&couch()).min.x;
        assert!((width - 2.5).abs() < 1e-4, "{width}");
        let layout = layout();
        let couch = layout.seat(layout.default_seat);
        assert_eq!((couch.eye.z, couch.eye.y), (3.4, 1.05));
    }

    #[test]
    fn the_couch_looks_straight_at_the_picture() {
        let layout = layout();
        let seat = layout.seat(layout.default_seat);
        assert_eq!(seat.name, SeatName::Couch);
        assert_eq!(seat.eye.x, 0.0);
        let screen = layout.screen.unwrap();
        assert!((seat.eye.y - screen.centre.y).abs() < 0.3);
    }

    #[test]
    fn the_seats_offered_are_the_couch_then_the_back_row() {
        let layout = layout();
        let names: Vec<SeatName> = layout.seats.iter().map(|s| s.name).collect();
        assert_eq!(names, [SeatName::Couch, SeatName::Back]);
        assert!(layout.seats[0].eye.z < layout.seats[1].eye.z);
        // The back seat is in an armchair's place.
        assert!(CHAIR_X.contains(&layout.seats[1].eye.x));
    }

    #[test]
    fn the_furniture_stands_apart_and_inside_the_room() {
        let w = HALF_WIDTH - 0.2;
        for b in furniture_bounds() {
            assert!(b.min.x >= -w && b.max.x <= w, "{b:?}");
            assert!(b.min.z > FRONT_Z + 1.0, "{b:?}");
            assert!(b.max.z < BACK_Z - 0.5, "{b:?}");
        }
        // The couch is all in front of the riser, the armchairs on it.
        assert!(bounds_of(&couch()).max.z < RISER_Z);
        let left = bounds_of(&armchair(CHAIR_X[0]));
        let right = bounds_of(&armchair(CHAIR_X[1]));
        for chair in [left, right] {
            assert!(chair.min.z > RISER_Z, "{chair:?}");
            assert!(chair.min.y >= RISER_HEIGHT, "{chair:?}");
        }
        // The two armchairs do not touch.
        assert!(right.min.x - left.max.x >= 0.1);
    }

    #[test]
    fn the_palette_covers_every_material_index() {
        let palette = materials();
        assert_eq!(palette.len(), mat::SCONCE as usize + 1);
        assert!(palette.len() <= 16);
        // Every material is used by something.
        let data = build();
        for i in 0..palette.len() as u32 {
            assert!(data.vertices.iter().any(|v| v.material == i), "{i}");
        }
    }

    #[test]
    fn there_are_two_sconces_on_each_wall_and_they_glow_with_the_house() {
        let data = build();
        let pattern = data.palette[mat::SCONCE as usize].pattern;
        assert_eq!(pattern, Pattern::EmissiveHouse);
        let mut on_the_left = 0;
        let mut all = 0;
        for v in &data.vertices {
            if v.material != mat::SCONCE {
                continue;
            }
            all += 1;
            on_the_left += usize::from(v.pos[0] < 0.0);
            // On the wall, a hand deep.
            assert!(v.pos[0].abs() <= HALF_WIDTH + 1e-4, "{:?}", v.pos);
            assert!(v.pos[0].abs() >= HALF_WIDTH - 0.2, "{:?}", v.pos);
            // Baked as a glow: always lit, never occluded.
            assert_eq!((v.house, v.ao), (1.0, 1.0));
        }
        // Four boxes of five faces.
        assert_eq!(all, 4 * 5 * 4);
        assert_eq!(on_the_left, all / 2);
    }

    #[test]
    fn house_lights_reach_the_room_and_the_bakes_find_their_creases() {
        let data = build();
        let mut brightest = 0.0_f32;
        let mut dimmest = 1.0_f32;
        let mut darkest = 1.0_f32;
        for v in &data.vertices {
            if data.palette[v.material as usize].pattern != Pattern::EmissiveHouse {
                brightest = brightest.max(v.house);
                dimmest = dimmest.min(v.house);
            }
            darkest = darkest.min(v.ao);
        }
        assert!(brightest >= 0.99, "{brightest}");
        assert!(dimmest < 0.5 * brightest, "{dimmest}");
        // Under the couch and in the corners.
        assert!(darkest < 0.4, "{darkest}");
    }

    #[test]
    fn the_screen_panel_is_the_size_of_the_frame() {
        let data = build();
        let mut min = Vec3::MAX;
        let mut max = Vec3::MIN;
        let mut count = 0;
        for v in &data.vertices {
            if v.material != mat::SCREEN {
                continue;
            }
            count += 1;
            min = min.min(Vec3::from(v.pos));
            max = max.max(Vec3::from(v.pos));
        }
        assert_eq!(count, 4);
        assert!((max.x - min.x - SCREEN_MAX.x).abs() < 1e-4);
        assert!((max.y - min.y - SCREEN_MAX.y).abs() < 1e-4);
    }

    #[test]
    fn it_is_no_dearer_to_draw_than_the_cinema() {
        let home = build().indices.len() / 3;
        let cinema = crate::env::cinema::build().indices.len() / 3;
        assert!(home <= cinema, "{home} triangles against {cinema}");
    }

    #[test]
    fn floor_at_is_flat_then_the_riser() {
        assert_eq!(floor_at(1.0), 0.0);
        assert_eq!(floor_at(COUCH_EYE_Z), 0.0);
        assert_eq!(floor_at(CHAIR_EYE_Z), RISER_HEIGHT);
        assert_eq!(floor_at(BACK_Z), RISER_HEIGHT);
    }
}
