//! Places: the rooms and skies the picture can be shown in.
//!
//! A place is plain data. `Place::layout` is cheap (seats and the screen's
//! frame, no meshes); `Place::build` makes the meshes, the material palette
//! and the lighting a renderer needs. Nothing here touches the GPU, so all of
//! it is unit tested without a device.
//!
//! Plain (the empty background) and "your room" (passthrough) are not places:
//! the app keeps them as they were, and shows a place only when one is chosen.
//!
//! Room space, for a `PlaceKind::Room`: the screen plane is z = 0, the
//! audience is towards +z, the stage floor is y = 0 and x is to the right. A
//! seat is an eye position in room space, and a room is placed in the world
//! with `Seat::room_to_world`, which puts the seat's eye at the anchor's
//! origin. A `PlaceKind::Sky` has no room: its geometry is in anchor space,
//! with the origin at the viewer's eye.
//!
//! Nothing in a place is ever in front of the picture. The scene pass draws
//! the picture over the place without a depth test, so the rooms are built (and
//! tested) so that no geometry covers the screen from any seat.

pub mod cinema;
pub mod kit;
pub mod ktx2;
pub mod light;
pub mod pack;
pub mod studio;

use glam::{Mat4, Vec2, Vec3};
use std::sync::Arc;

/// A place the picker offers. More are added here as they are built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    Cinema,
    Studio,
}

/// Whether a place is a room around the screen or a backdrop around the
/// viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceKind {
    /// Open air: a gradient and a floor. The screen is wherever the user's
    /// own screen settings put it.
    Sky,
    /// Walls, seats and a screen of its own size.
    Room,
}

impl Place {
    /// Every place, in the order the picker lists them.
    pub const ALL: &'static [Place] = &[Place::Cinema, Place::Studio];

    /// The places the picker offers. The Studio is built and tested but not
    /// offered for now; a stored choice of it still works.
    pub const PICKER: &'static [Place] = &[Place::Cinema];

    /// The stable id stored in settings: lowercase words joined by hyphens.
    pub fn id(self) -> &'static str {
        match self {
            Place::Cinema => "cinema",
            Place::Studio => "studio",
        }
    }

    /// The place with this id, or `None` for an id this build does not know
    /// (a settings file from a newer build), which shows as Plain.
    pub fn from_id(id: &str) -> Option<Place> {
        Place::ALL.iter().copied().find(|p| p.id() == id)
    }

    pub fn kind(self) -> PlaceKind {
        match self {
            Place::Cinema => PlaceKind::Room,
            Place::Studio => PlaceKind::Sky,
        }
    }

    /// The seats and screen frame. Cheap: no meshes are built.
    ///
    /// A place with an installed pack (see [`pack`]) has the pack's seats and
    /// screen.
    pub fn layout(self) -> Layout {
        if let Some(pack) = pack::replacement(self) {
            return pack.layout.clone();
        }
        match self {
            Place::Cinema => cinema::layout(),
            Place::Studio => studio::layout(),
        }
    }

    /// The meshes, palette, lighting and sky: the installed pack's when the
    /// place has one that loads (decoding its textures takes a moment, so
    /// this is for a worker thread), else the built-in room.
    pub fn build(self) -> PlaceData {
        self.build_set(None)
    }

    /// [`Place::build`] for the set of a pack's textures that a viewer in
    /// some seat draws, as [`Place::seat_set`] named it (`None` is the
    /// pack's default seat's, and all there is for a place whose pack has one
    /// set for everybody, and for the built-in rooms).
    pub fn build_set(self, set: Option<SeatName>) -> PlaceData {
        if let Some(data) = pack::load_replacement_for(self, set) {
            return data;
        }
        self.build_builtin()
    }

    /// Which set of this place's pack a viewer in `seat` draws, named by the
    /// seat it was baked for: only a pack with a mesh and textures for each
    /// seat has one to choose (see [`pack::seat_set`]); `None` otherwise.
    pub fn seat_set(self, seat: SeatName) -> Option<SeatName> {
        pack::seat_set(self, seat)
    }

    /// The built-in room, whatever packs are installed.
    pub fn build_builtin(self) -> PlaceData {
        match self {
            Place::Cinema => cinema::build(),
            Place::Studio => studio::build(),
        }
    }
}

/// Where the viewer sits in a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeatName {
    Front,
    Middle,
    Back,
    Couch,
}

impl SeatName {
    pub const ALL: &'static [SeatName] = &[
        SeatName::Front,
        SeatName::Middle,
        SeatName::Back,
        SeatName::Couch,
    ];

    /// A stable id, for settings and the preview commands.
    pub fn id(self) -> &'static str {
        match self {
            SeatName::Front => "front",
            SeatName::Middle => "middle",
            SeatName::Back => "back",
            SeatName::Couch => "couch",
        }
    }

    pub fn from_id(id: &str) -> Option<SeatName> {
        SeatName::ALL.iter().copied().find(|s| s.id() == id)
    }
}

/// An eye position, in room space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seat {
    pub name: SeatName,
    pub eye: Vec3,
}

impl Seat {
    /// The transform that puts this seat's eye at the origin of `anchor` (the
    /// picture's anchor, as the app computes it): room space to world space.
    pub fn room_to_world(&self, anchor: Mat4) -> Mat4 {
        anchor * Mat4::from_translation(-self.eye)
    }
}

/// The room's own screen: the largest picture it can show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenFrame {
    /// Centre of the frame, in room space. The frame faces +z.
    pub centre: Vec3,
    /// Largest width and height of a picture, metres.
    pub max: Vec2,
    /// 0 flat. (The cinema's screen is flat; this is for curved rooms.)
    pub curvature: f32,
}

/// A place's seats and screen. Cheap to make.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The seats on offer. One entry (a Sky's implicit seat at the origin)
    /// means there is nothing to choose.
    pub seats: Vec<Seat>,
    /// Index into `seats` used until the user picks another.
    pub default_seat: usize,
    /// Rooms place the picture on their own screen. A Sky has none: the
    /// user's screen settings apply.
    pub screen: Option<ScreenFrame>,
    /// Height of the floor, metres. A Room: the stage floor in room space
    /// (0). A Sky: below the eye, in anchor space.
    pub floor_y: f32,
}

impl Layout {
    /// The seat at `index`, or the default one when the index is out of range
    /// (a stored choice from a place that has since changed).
    pub fn seat(&self, index: usize) -> &Seat {
        let fallback = &self.seats[self.default_seat];
        self.seats.get(index).unwrap_or(fallback)
    }

    /// The index of the seat called `name`, if the place has one.
    pub fn seat_index(&self, name: SeatName) -> Option<usize> {
        self.seats.iter().position(|s| s.name == name)
    }
}

/// How far under a sky place's screen its floor lies at least, metres: the
/// picture is never closer to the floor than this.
pub const SCREEN_FLOOR_GAP: f32 = 0.15;

/// The height of a sky place's floor when the screen's bottom edge is at
/// `screen_bottom_y` (the same space as `floor_y`, the floor the place is
/// built with): the built floor, or lower when a big screen would reach it,
/// so the picture never sinks into the floor.
pub fn sky_floor(floor_y: f32, screen_bottom_y: f32) -> f32 {
    floor_y.min(screen_bottom_y - SCREEN_FLOOR_GAP)
}

/// One vertex of a place's mesh. 44 bytes, no padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    /// Unit length, pointing into the room.
    pub normal: [f32; 3],
    /// Pattern coordinates, metres on the surface (a `Pattern::Screen` panel
    /// uses 0 to 1 across the panel instead).
    pub uv: [f32; 2],
    /// Baked ambient occlusion: 1 open, down to 0.35 in a crease.
    pub ao: f32,
    /// Baked share of the house lights reaching this vertex, 0 to 1.
    pub house: f32,
    /// Index into `PlaceData::palette`.
    pub material: u32,
}

/// How a surface is dressed, beyond its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    Plain,
    Carpet,
    Fabric,
    Panel,
    Wood,
    /// The screen panel: matte white, lit by the picture's own light.
    Screen,
    /// Glows by itself at a constant level.
    Emissive,
    /// Glows with the picture's mean colour (its strength is a gain on it):
    /// a projector's window. Dark with no picture, and when the picture's
    /// light is off.
    EmissivePicture,
    /// Glows with the house lights: dark when they are down.
    EmissiveHouse,
}

impl Pattern {
    /// Whether the surface makes its own light.
    pub fn is_emissive(self) -> bool {
        matches!(
            self,
            Pattern::Emissive | Pattern::EmissivePicture | Pattern::EmissiveHouse
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// Linear RGB.
    pub albedo: [f32; 3],
    pub pattern: Pattern,
    /// Strength of the glow of an emissive pattern.
    pub emissive: f32,
    /// Extra grazing-angle shine: fabric, or a glossy floor.
    pub sheen: f32,
}

/// The light on a place's surfaces without the picture's light. A surface
/// gets `ambient_up` or `ambient_down` by the way it faces, plus
/// `house_rgb` times its vertex's `house` share times the house-light level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lighting {
    pub ambient_up: [f32; 3],
    pub ambient_down: [f32; 3],
    pub house_rgb: [f32; 3],
    /// How much of the picture's light lands on the place (1 is as measured).
    pub spill: f32,
    pub fog: Option<Fog>,
}

/// Linear fog, in metres from the eye.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    pub rgb: [f32; 3],
    pub start: f32,
    pub end: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sky {
    None,
    Gradient {
        /// Linear RGB straight up.
        zenith: [f32; 3],
        /// Linear RGB at the horizon, which the fog fades to.
        horizon: [f32; 3],
    },
}

/// Everything a renderer needs to draw a place.
#[derive(Clone, Debug)]
pub struct PlaceData {
    pub vertices: Vec<Vertex>,
    /// Triangles, counter-clockwise seen from the side the normal points to.
    pub indices: Vec<u32>,
    /// At most 16 materials.
    pub palette: Vec<Material>,
    pub lighting: Lighting,
    pub sky: Sky,
    /// A pack's baked mesh and textures, drawn instead of `vertices`,
    /// `indices` and `palette` (which are empty then).
    pub baked: Option<Arc<pack::Baked>>,
}

/// What the app hands the renderer each frame.
#[derive(Clone, Copy, Debug)]
pub struct EnvFrame {
    pub place: Place,
    /// The seat the viewer sits in: a pack with textures baked for each
    /// seat draws that seat's (see [`Place::seat_set`]).
    pub seat: SeatName,
    pub room_to_world: Mat4,
    /// House-light level: 0 dark, 1 full.
    pub house: f32,
    /// How strongly the picture lights the place: 0 is off, 1 the strength
    /// the place is tuned for (the Settings slider's middle), 4 its top.
    pub screen_light: f32,
    /// Seconds, for slow effects.
    pub time: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built() -> Vec<(Place, PlaceData)> {
        Place::ALL.iter().map(|&p| (p, p.build())).collect()
    }

    #[test]
    fn vertex_is_44_bytes() {
        assert_eq!(size_of::<Vertex>(), 44);
    }

    #[test]
    fn every_place_builds_within_budget() {
        for (place, data) in built() {
            let id = place.id();
            let len = data.indices.len();
            assert!(len.is_multiple_of(3), "{id}: partial triangle");
            let tris = len / 3;
            assert!(tris <= 50_000, "{id}: {tris} triangles");
            let count = data.vertices.len() as u32;
            let in_range = data.indices.iter().all(|&i| i < count);
            assert!(in_range, "{id}: bad index");
            let materials = data.palette.len();
            let ok = (1..=16).contains(&materials);
            assert!(ok, "{id}: {materials} materials");
            for v in &data.vertices {
                let len = Vec3::from(v.normal).length();
                assert!((len - 1.0).abs() < 1e-3, "{id}: normal length {len}");
                assert!((0.0..=1.0).contains(&v.ao), "{id}: ao {}", v.ao);
                assert!((0.0..=1.0).contains(&v.house), "{id}: house {}", v.house);
                assert!((v.material as usize) < materials, "{id}: material");
            }
        }
    }

    #[test]
    fn every_triangle_faces_its_normal() {
        for (place, data) in built() {
            let id = place.id();
            let pos = |i: u32| Vec3::from(data.vertices[i as usize].pos);
            for t in data.indices.chunks(3) {
                let (a, b, c) = (pos(t[0]), pos(t[1]), pos(t[2]));
                let cross = (b - a).cross(c - a);
                if cross.length_squared() < 1e-12 {
                    continue;
                }
                let n = Vec3::from(data.vertices[t[0] as usize].normal);
                let facing = cross.normalize().dot(n);
                assert!(facing > 0.99, "{id}: triangle faces away ({facing})");
            }
        }
    }

    #[test]
    fn ids_round_trip_and_are_kebab_case() {
        for &place in Place::ALL {
            let id = place.id();
            assert_eq!(Place::from_id(id), Some(place));
            let ok_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
            assert!(id.chars().all(ok_char), "{id}");
            assert!(!id.starts_with('-') && !id.ends_with('-'), "{id}");
            assert!(!id.contains("--"), "{id}");
        }
        for &seat in SeatName::ALL {
            assert_eq!(SeatName::from_id(seat.id()), Some(seat));
        }
    }

    #[test]
    fn the_picker_offers_the_cinema_and_not_the_studio() {
        assert_eq!(Place::PICKER, &[Place::Cinema]);
        assert!(Place::PICKER.iter().all(|p| Place::ALL.contains(p)));
        // Hidden, not removed: a stored choice of it still resolves.
        assert_eq!(Place::from_id("studio"), Some(Place::Studio));
    }

    #[test]
    fn unknown_ids_are_none() {
        for id in ["", "plain", "Cinema", "cinema ", "home theater", "void"] {
            assert_eq!(Place::from_id(id), None, "{id:?}");
        }
        assert_eq!(SeatName::from_id("row-9"), None);
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = Place::ALL.iter().copied().map(Place::id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Place::ALL.len());
    }

    #[test]
    fn layouts_are_usable() {
        for &place in Place::ALL {
            let layout = place.layout();
            let id = place.id();
            assert!(!layout.seats.is_empty(), "{id}: no seats");
            let n = layout.seats.len();
            assert!(layout.default_seat < n, "{id}: default seat");
            let kind = place.kind();
            assert_eq!(layout.screen.is_some(), kind == PlaceKind::Room, "{id}");
        }
    }

    #[test]
    fn a_stale_seat_index_falls_back_to_the_default() {
        let layout = Place::Cinema.layout();
        let default = layout.seats[layout.default_seat];
        assert_eq!(*layout.seat(99), default);
        let back = layout.seat_index(SeatName::Back).unwrap();
        assert_eq!(layout.seat(back).name, SeatName::Back);
        assert_eq!(layout.seat_index(SeatName::Couch), None);
    }

    #[test]
    fn sky_floor_drops_under_a_big_screen() {
        let built = Place::Studio.layout().floor_y;
        // A screen well above the floor leaves it where it was built.
        assert_eq!(sky_floor(built, -0.4), built);
        assert_eq!(sky_floor(built, built + 1.0), built);
        // One whose bottom edge is lower, or within the gap, takes it down
        // to the gap under the edge.
        let low = sky_floor(built, -3.0);
        assert!((low - (-3.0 - SCREEN_FLOOR_GAP)).abs() < 1e-6);
        let close = sky_floor(built, built + 0.05);
        assert!((close - (built + 0.05 - SCREEN_FLOOR_GAP)).abs() < 1e-6);
        assert!(close < built);
    }

    #[test]
    fn the_seat_moves_the_room_so_its_eye_is_at_the_anchor() {
        let layout = Place::Cinema.layout();
        let seat = layout.seat(layout.default_seat);
        let anchor = Mat4::from_translation(Vec3::new(0.0, 1.0, -2.0));
        let eye = seat.room_to_world(anchor).transform_point3(seat.eye);
        assert!((eye - Vec3::new(0.0, 1.0, -2.0)).length() < 1e-4);
    }
}
