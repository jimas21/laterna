//! Place packs: a room made elsewhere (modelled and lit in Blender, baked to
//! textures) that replaces one of the built-in rooms.
//!
//! A pack is a folder: `manifest.json`, `mesh.bin` and a set of textures,
//! all described in the format notes kept with the exporter. Format 1 has
//! PNGs; format 2 has KTX2 files, ASTC compressed, with their mip chains made
//! offline (see [`super::ktx2`]); format 3 is format 2 with a mesh and a set
//! of textures for each seat, baked from that seat (a viewer in a seat draws
//! that seat's set, and changing seat loads another: see [`seat_set`]). This
//! module reads one and checks every limit before anything is allocated or
//! drawn: a pack that breaks any of them is refused with a message (the
//! caller logs a warning and shows the built-in room), and nothing in here
//! panics on any bytes.
//!
//! Three steps, none of them touching the GPU:
//! - [`scan`] reads only the manifests (and file sizes) in a folder of packs,
//!   which is cheap enough for [`Place::layout`] to ask on any frame;
//! - [`load`] reads and decodes a whole pack, which takes a moment, so
//!   [`Place::build`] does it on the place builder's worker thread. A format 2
//!   pack's ASTC textures are kept as they are when the device can sample
//!   them ([`astc_on_gpu`]) and decoded to RGBA8 here, off the render thread,
//!   when it cannot;
//! - the renderer uploads the result ([`Baked`]) and draws it with
//!   shaders/env_baked.wgsl.
//!
//! A pack replaces the built-in place its id names: `cinema` or `cinema-v2`
//! (any number) replaces the Cinema. Without a pack nothing changes.

use super::ktx2::{self, Encoding};
use super::{Layout, Lighting, Place, PlaceData, PlaceKind, ScreenFrame, Seat, SeatName, Sky};
use glam::{Vec2, Vec3};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// The most triangles a pack's mesh has.
pub const MAX_TRIANGLES: u32 = 100_000;
/// The most vertices a pack's mesh has.
pub const MAX_VERTICES: u32 = 200_000;
/// The largest side of a pack's textures, in texels.
pub const MAX_TEXTURE: u32 = 4096;
/// Light patches of the screen, one basis texture each (`PATCHES` in
/// [`super::light`]).
pub const BASIS: usize = 8;
/// The newest format number this build reads (1 to this).
pub const FORMAT: u32 = 3;
/// The first format with a mesh and textures for each seat.
pub const PER_SEAT_FORMAT: u32 = 3;
/// The most bytes one set of a format 2 or 3 pack's texture files may add up
/// to (a format 3 pack has a set for each seat, so the whole pack may be more).
pub const MAX_TEXTURE_BYTES: u64 = 32 << 20;

/// A PNG file bigger than this is refused unread: an 8-bit RGB texture of
/// the largest size is 48 MiB before compression.
const MAX_PNG_FILE: u64 = 96 << 20;
const MAX_MANIFEST_FILE: u64 = 256 << 10;
const MAGIC: [u8; 4] = *b"LPM1";
const HEADER: usize = 12;
const VERTEX_BYTES: usize = 20;
/// The folder the packs are in, overriding the default (for tests and the
/// desktop preview).
pub const PLACES_DIR_ENV: &str = "FRAMEPLAYERX_PLACES_DIR";

/// Why a pack was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackError(pub String);

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PackError {}

fn bad<T>(message: impl Into<String>) -> Result<T, PackError> {
    Err(PackError(message.into()))
}

fn io(what: &str, path: &Path, e: &std::io::Error) -> PackError {
    PackError(format!("{what} {}: {e}", path.display()))
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScreenSpec {
    pub centre: [f32; 3],
    pub max: [f32; 2],
}

#[derive(Clone, Debug, Deserialize)]
pub struct SeatSpec {
    pub name: String,
    pub eye: [f32; 3],
}

#[derive(Clone, Debug, Deserialize)]
pub struct MeshSpec {
    pub file: String,
    pub vertices: u32,
    pub triangles: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TextureSpec {
    /// Side of the house and dark textures (and of every texture, format 1).
    pub size: u32,
    /// Side of the basis textures. Format 2 only.
    #[serde(default)]
    pub basis_size: Option<u32>,
    pub house: String,
    pub dark: String,
    pub basis: Vec<String>,
    /// The sizes of the texture files added up. Format 2 only.
    #[serde(default)]
    pub bytes: Option<u64>,
}

/// A mesh with its textures: what one seat of a format 3 pack draws.
#[derive(Clone, Debug, Deserialize)]
pub struct SetSpec {
    pub mesh: MeshSpec,
    pub textures: TextureSpec,
    /// This set's `basis_gain`, when it is not the pack's: each seat's basis
    /// textures are scaled to use the 8 bits they have, so the gain that
    /// undoes the scale can differ from seat to seat.
    #[serde(default)]
    pub basis_gain: Option<f32>,
}

fn one() -> f32 {
    1.0
}

/// `manifest.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub screen: ScreenSpec,
    pub seats: Vec<SeatSpec>,
    #[serde(default)]
    pub default_seat: usize,
    /// The pack's mesh and textures (formats 1 and 2; a format 3 pack has
    /// `seat_sets` instead).
    #[serde(default)]
    pub mesh: Option<MeshSpec>,
    #[serde(default)]
    pub textures: Option<TextureSpec>,
    /// A mesh and textures for each seat, by the seat's name; every seat has
    /// one (format 3 only).
    #[serde(default)]
    pub seat_sets: BTreeMap<String, SetSpec>,
    #[serde(default = "one")]
    pub basis_gain: f32,
    /// Reserved: a sky behind the room. Only none is understood.
    #[serde(default)]
    pub sky: Option<serde_json::Value>,
}

/// A file name that stays inside the pack folder.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '\0'])
}

fn finite3(v: [f32; 3]) -> bool {
    v.iter().all(|c| c.is_finite())
}

fn seat_name(name: &str) -> Option<SeatName> {
    SeatName::from_id(name).filter(|s| *s != SeatName::Couch)
}

impl Manifest {
    /// Parses and checks `manifest.json`'s bytes.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, PackError> {
        let m: Manifest = serde_json::from_slice(bytes)
            .map_err(|e| PackError(format!("manifest.json is not usable: {e}")))?;
        m.check()?;
        Ok(m)
    }

    /// Every limit that can be checked without the other files.
    pub fn check(&self) -> Result<(), PackError> {
        if !(1..=FORMAT).contains(&self.format) {
            return bad(format!(
                "format {} (this build reads 1 to {FORMAT})",
                self.format
            ));
        }
        let id_ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if self.id.is_empty()
            || self.id.len() > 64
            || !self.id.chars().all(id_ok)
            || self.id.starts_with('-')
            || self.id.ends_with('-')
        {
            return bad(format!(
                "id {:?} is not lowercase words joined by hyphens",
                self.id
            ));
        }
        let s = &self.screen;
        if !finite3(s.centre)
            || !s
                .max
                .iter()
                .all(|c| c.is_finite() && *c > 0.0 && *c < 1000.0)
        {
            return bad("the screen's centre or size is not a number, or not positive");
        }
        if self.seats.is_empty() || self.seats.len() > 3 {
            return bad(format!("{} seats (1 to 3 wanted)", self.seats.len()));
        }
        let mut seen = Vec::new();
        for seat in &self.seats {
            let Some(name) = seat_name(&seat.name) else {
                return bad(format!("seat {:?} is not front, middle or back", seat.name));
            };
            if seen.contains(&name) {
                return bad(format!("seat {:?} twice", seat.name));
            }
            seen.push(name);
            if !finite3(seat.eye) {
                return bad(format!(
                    "seat {:?} has an eye that is not a number",
                    seat.name
                ));
            }
        }
        if self.default_seat >= self.seats.len() {
            return bad(format!("default_seat {} is not a seat", self.default_seat));
        }
        if self.format < PER_SEAT_FORMAT {
            if !self.seat_sets.is_empty() {
                return bad(format!("seat_sets needs format {PER_SEAT_FORMAT}"));
            }
            self.set(None)?.check()?;
        } else {
            if self.mesh.is_some() || self.textures.is_some() {
                return bad(format!(
                    "a format {PER_SEAT_FORMAT} pack keeps its mesh and textures in seat_sets"
                ));
            }
            // Every seat has a set of its own, and every set is a seat's.
            for seat in &self.seats {
                let Some(spec) = self.seat_sets.get(&seat.name) else {
                    return bad(format!("seat {:?} has no entry in seat_sets", seat.name));
                };
                Set::of(self, spec)
                    .check()
                    .map_err(|e| PackError(format!("seat {:?}: {e}", seat.name)))?;
            }
            if let Some(extra) = self
                .seat_sets
                .keys()
                .find(|k| !self.seats.iter().any(|s| &s.name == *k))
            {
                return bad(format!(
                    "seat_sets has {extra:?}, which is not one of the seats"
                ));
            }
        }
        let seat_gains = self.seat_sets.values().filter_map(|s| s.basis_gain);
        for gain in std::iter::once(self.basis_gain).chain(seat_gains) {
            if !(gain.is_finite() && (0.0..=1000.0).contains(&gain)) {
                return bad(format!("basis_gain {gain} is out of range"));
            }
        }
        Ok(())
    }

    /// The seats and screen. The floor is the stage floor, y = 0.
    pub fn layout(&self) -> Layout {
        let seats = self
            .seats
            .iter()
            .filter_map(|s| {
                Some(Seat {
                    name: seat_name(&s.name)?,
                    eye: Vec3::from(s.eye),
                })
            })
            .collect();
        Layout {
            seats,
            default_seat: self.default_seat,
            screen: Some(ScreenFrame {
                centre: Vec3::from(self.screen.centre),
                max: Vec2::from(self.screen.max),
                curvature: 0.0,
            }),
            floor_y: 0.0,
        }
    }

    /// Whether the pack has a mesh and textures for each seat.
    pub fn per_seat(&self) -> bool {
        self.format >= PER_SEAT_FORMAT
    }

    /// The seat the pack starts in.
    pub fn default_seat_name(&self) -> Option<SeatName> {
        self.seats
            .get(self.default_seat)
            .and_then(|s| seat_name(&s.name))
    }

    /// Which set a viewer in `seat` draws, named by the seat it was baked
    /// for: `seat`'s own, or the default seat's when the pack has none for
    /// it. `None` for a pack that has one set for everybody (formats 1 and 2).
    pub fn set_key(&self, seat: SeatName) -> Option<SeatName> {
        if !self.per_seat() {
            return None;
        }
        if self.seat_sets.contains_key(seat.id()) {
            Some(seat)
        } else {
            self.default_seat_name()
        }
    }

    /// The set key for a request: `seat`'s (see [`Manifest::set_key`]), the
    /// default seat's when none is asked for. `None` for a pack with one set
    /// for everybody.
    pub fn resolve(&self, seat: Option<SeatName>) -> Option<SeatName> {
        if !self.per_seat() {
            return None;
        }
        seat.and_then(|s| self.set_key(s))
            .or_else(|| self.default_seat_name())
    }

    /// The set keys the pack has, one for a pack with one set for everybody
    /// (`None`).
    pub fn set_keys(&self) -> Vec<Option<SeatName>> {
        if !self.per_seat() {
            return vec![None];
        }
        self.seats
            .iter()
            .filter_map(|s| seat_name(&s.name))
            .map(Some)
            .collect()
    }

    /// The mesh and textures drawn for set `key` (see [`Manifest::set_key`];
    /// `None` is the one set of formats 1 and 2, and the default seat's of
    /// format 3).
    fn set(&self, key: Option<SeatName>) -> Result<Set<'_>, PackError> {
        if !self.per_seat() {
            return match (&self.mesh, &self.textures) {
                (Some(mesh), Some(textures)) => Ok(Set {
                    format: self.format,
                    mesh,
                    textures,
                    basis_gain: self.basis_gain,
                }),
                (None, _) => bad("mesh is missing"),
                (_, None) => bad("textures is missing"),
            };
        }
        match self.resolve(key).and_then(|n| self.seat_sets.get(n.id())) {
            Some(spec) => Ok(Set::of(self, spec)),
            None => bad("the pack has no set for its default seat"),
        }
    }
}

/// A mesh and its textures: a whole format 1 or 2 pack's, or one seat's of a
/// format 3 pack.
#[derive(Clone, Copy)]
struct Set<'a> {
    format: u32,
    mesh: &'a MeshSpec,
    textures: &'a TextureSpec,
    /// The gain on its basis textures.
    basis_gain: f32,
}

impl<'a> Set<'a> {
    /// A format 3 pack's set `spec`, with the pack's gain unless it has its own.
    fn of(manifest: &Manifest, spec: &'a SetSpec) -> Set<'a> {
        Set {
            format: manifest.format,
            mesh: &spec.mesh,
            textures: &spec.textures,
            basis_gain: spec.basis_gain.unwrap_or(manifest.basis_gain),
        }
    }

    /// Every limit that can be checked without the files.
    fn check(&self) -> Result<(), PackError> {
        let mesh = self.mesh;
        if !plain_name(&mesh.file) {
            return bad(format!("mesh file name {:?}", mesh.file));
        }
        if mesh.vertices == 0 || mesh.vertices > MAX_VERTICES {
            return bad(format!(
                "{} vertices (1 to {MAX_VERTICES} allowed)",
                mesh.vertices
            ));
        }
        if mesh.triangles == 0 || mesh.triangles > MAX_TRIANGLES {
            return bad(format!(
                "{} triangles (1 to {MAX_TRIANGLES} allowed)",
                mesh.triangles
            ));
        }
        let t = self.textures;
        if t.size == 0 || t.size > MAX_TEXTURE || !t.size.is_power_of_two() {
            return bad(format!(
                "texture size {} (a power of two up to {MAX_TEXTURE} wanted)",
                t.size
            ));
        }
        if self.format >= 2 {
            let Some(basis) = t.basis_size else {
                return bad("textures.basis_size is missing (format 2 needs it)");
            };
            if basis == 0 || basis > t.size || !basis.is_power_of_two() {
                return bad(format!(
                    "basis size {basis} (a power of two up to the texture size {} wanted)",
                    t.size
                ));
            }
            match t.bytes {
                None => return bad("textures.bytes is missing (format 2 needs it)"),
                Some(0) => return bad("textures.bytes is 0"),
                Some(n) if n > MAX_TEXTURE_BYTES => {
                    return bad(format!(
                        "textures.bytes is {n} (at most {MAX_TEXTURE_BYTES} allowed)"
                    ));
                }
                Some(_) => {}
            }
        }
        if t.basis.len() != BASIS {
            return bad(format!("{} basis textures ({BASIS} wanted)", t.basis.len()));
        }
        for name in [&t.house, &t.dark].into_iter().chain(&t.basis) {
            if !plain_name(name) {
                return bad(format!("texture file name {name:?}"));
            }
        }
        Ok(())
    }

    /// The bytes `mesh.bin` must have.
    fn mesh_len(&self) -> u64 {
        self.mesh.len()
    }

    /// Side of the basis textures.
    fn basis_size(&self) -> u32 {
        match self.format {
            1 => self.textures.size,
            _ => self.textures.basis_size.unwrap_or(self.textures.size),
        }
    }

    fn texture_files(&self) -> impl Iterator<Item = &'a String> + use<'a> {
        let t = self.textures;
        [&t.house, &t.dark].into_iter().chain(&t.basis)
    }
}

impl MeshSpec {
    /// The bytes `mesh.bin` must have.
    fn len(&self) -> u64 {
        HEADER as u64
            + VERTEX_BYTES as u64 * u64::from(self.vertices)
            + 4 * 3 * u64::from(self.triangles)
    }
}

/// One vertex of a pack's mesh: a position in room space and a place in the
/// atlas.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BakedVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
}

/// One set of same-sized textures that become one GPU array image.
pub struct TextureArray {
    /// Side of every texture, texels.
    pub size: u32,
    pub encoding: Encoding,
    /// Per texture (array layer), its mip levels' bytes, level 0 first.
    /// RGBA8 rows are tightly packed; ASTC levels are whole 16-byte blocks.
    /// One level only (format 1) means the GPU makes the others.
    pub layers: Vec<Vec<Vec<u8>>>,
}

impl TextureArray {
    /// Mip levels each texture has here.
    pub fn levels(&self) -> usize {
        self.layers.first().map_or(0, Vec::len)
    }

    /// Bytes the textures take on the GPU, counting the mips it makes.
    pub fn gpu_bytes(&self) -> u64 {
        let held: u64 = self.layers.iter().flatten().map(|l| l.len() as u64).sum();
        if self.levels() == 1 && self.size > 1 {
            held + held / 3
        } else {
            held
        }
    }
}

impl fmt::Debug for TextureArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextureArray")
            .field("size", &self.size)
            .field("encoding", &self.encoding)
            .field("layers", &self.layers.len())
            .finish()
    }
}

/// A pack's mesh and decoded textures, ready to upload.
pub struct Baked {
    pub vertices: Vec<BakedVertex>,
    /// Triangles.
    pub indices: Vec<u32>,
    /// The house texture (layer 0) and the dark one (layer 1), as stored:
    /// sRGB.
    pub colors: TextureArray,
    /// The light of each patch of the screen, [`BASIS`] textures, as stored:
    /// square-root coded.
    pub basis: TextureArray,
    pub basis_gain: f32,
    /// The seat whose set this is, for a format 3 pack (the viewer sits in
    /// that seat, or one that falls back to it); `None` for a pack with one
    /// set for everybody.
    pub seat: Option<SeatName>,
}

impl fmt::Debug for Baked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Baked")
            .field("vertices", &self.vertices.len())
            .field("indices", &self.indices.len())
            .field("colors", &self.colors)
            .field("basis", &self.basis)
            .finish_non_exhaustive()
    }
}

impl Baked {
    /// Whether either texture set is ASTC blocks rather than RGBA8.
    pub fn is_astc(&self) -> bool {
        [&self.colors, &self.basis]
            .iter()
            .any(|t| matches!(t.encoding, Encoding::Astc { .. }))
    }

    /// The same pack with its ASTC textures decoded to RGBA8 (on all the
    /// cores there are), for a device that turned out not to take the
    /// blocks. `None` when nothing in it is ASTC.
    pub fn decoded(&self) -> Result<Option<Baked>, PackError> {
        if !self.is_astc() {
            return Ok(None);
        }
        let sets = [&self.colors, &self.basis];
        let jobs: Vec<(usize, usize)> = sets
            .iter()
            .enumerate()
            .flat_map(|(s, t)| (0..t.layers.len()).map(move |l| (s, l)))
            .collect();
        let results = par_map(&jobs, |&(s, l)| {
            let t = sets[s];
            let Encoding::Astc { block } = t.encoding else {
                return Ok(t.layers[l].clone());
            };
            t.layers[l]
                .iter()
                .enumerate()
                .map(|(n, data)| {
                    let edge = (t.size >> n).max(1);
                    ktx2::decode_astc(data, edge, edge, block)
                        .map(|(texels, _)| texels)
                        .map_err(|e| PackError(format!("texture level {n}: {e}")))
                })
                .collect()
        });
        let mut layers = results.into_iter();
        let mut array = |t: &TextureArray| -> Result<TextureArray, PackError> {
            let mut out = Vec::with_capacity(t.layers.len());
            for _ in 0..t.layers.len() {
                out.push(
                    layers
                        .next()
                        .unwrap_or_else(|| bad("a texture was not decoded"))?,
                );
            }
            Ok(TextureArray {
                size: t.size,
                encoding: Encoding::Rgba8,
                layers: out,
            })
        };
        let colors = array(&self.colors)?;
        let basis = array(&self.basis)?;
        Ok(Some(Baked {
            vertices: self.vertices.clone(),
            indices: self.indices.clone(),
            colors,
            basis,
            basis_gain: self.basis_gain,
            seat: self.seat,
        }))
    }
}

/// How a pack's textures got to the form the GPU takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TexturePath {
    /// PNG files, decoded; the GPU makes the mip chains.
    Png,
    /// Uncompressed KTX2 files, uploaded as they are.
    Rgba,
    /// ASTC blocks, uploaded as they are.
    AstcOnGpu,
    /// ASTC decoded to RGBA8 on the CPU, in this many milliseconds.
    AstcDecoded { ms: f64 },
}

impl fmt::Display for TexturePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TexturePath::Png => f.write_str("PNG"),
            TexturePath::Rgba => f.write_str("uncompressed KTX2"),
            TexturePath::AstcOnGpu => f.write_str("ASTC on the GPU"),
            TexturePath::AstcDecoded { ms } => {
                write!(f, "ASTC decoded on the CPU in {ms:.0} ms")
            }
        }
    }
}

/// A pack read in full.
#[derive(Debug)]
pub struct Pack {
    pub manifest: Manifest,
    pub baked: Baked,
    pub path: TexturePath,
}

static ASTC_ON_GPU: AtomicBool = AtomicBool::new(false);

/// Whether the device can sample ASTC textures, so that packs' ASTC data is
/// kept as it is. Set by [`crate::gpu::Gpu`] when it makes the device; false
/// until then (a pack loaded without a device is decoded).
pub fn astc_on_gpu() -> bool {
    ASTC_ON_GPU.load(Ordering::Relaxed)
}

/// Records what the device can do; see [`astc_on_gpu`].
pub fn set_astc_on_gpu(on: bool) {
    ASTC_ON_GPU.store(on, Ordering::Relaxed);
}

impl Pack {
    /// What the renderer takes: no per-vertex lighting or materials (the
    /// textures carry the light), nothing dark behind it.
    pub fn into_place_data(self) -> PlaceData {
        let dark = [0.0; 3];
        PlaceData {
            vertices: Vec::new(),
            indices: Vec::new(),
            palette: Vec::new(),
            lighting: Lighting {
                ambient_up: dark,
                ambient_down: dark,
                house_rgb: dark,
                // The picture's own light is the basis textures' to give.
                spill: 1.0,
                fog: None,
            },
            sky: Sky::None,
            baked: Some(Arc::new(self.baked)),
        }
    }
}

fn read_capped(path: &Path, cap: u64, what: &str) -> Result<Vec<u8>, PackError> {
    let len = std::fs::metadata(path)
        .map_err(|e| io(what, path, &e))?
        .len();
    if len > cap {
        return bad(format!(
            "{what} {} is {len} bytes (at most {cap})",
            path.display()
        ));
    }
    std::fs::read(path).map_err(|e| io(what, path, &e))
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le_f32(b: &[u8], at: usize) -> f32 {
    f32::from_bits(le_u32(b, at))
}

/// Reads `mesh.bin`'s bytes: the header, the vertices, the indices. Every
/// count is checked against the manifest and the length before a vertex is
/// made.
pub fn parse_mesh(
    bytes: &[u8],
    mesh: &MeshSpec,
) -> Result<(Vec<BakedVertex>, Vec<u32>), PackError> {
    if bytes.len() < HEADER {
        return bad(format!(
            "mesh is {} bytes, shorter than its header",
            bytes.len()
        ));
    }
    if bytes[..4] != MAGIC {
        return bad("mesh does not start with LPM1");
    }
    let (vc, ic) = (le_u32(bytes, 4), le_u32(bytes, 8));
    if vc != mesh.vertices || u64::from(ic) != 3 * u64::from(mesh.triangles) {
        return bad(format!(
            "mesh says {vc} vertices and {ic} indices, the manifest {} and {}",
            mesh.vertices,
            3 * u64::from(mesh.triangles)
        ));
    }
    if bytes.len() as u64 != mesh.len() {
        return bad(format!(
            "mesh is {} bytes, its counts need {}",
            bytes.len(),
            mesh.len()
        ));
    }
    let (vc, ic) = (vc as usize, ic as usize);
    let mut vertices = Vec::with_capacity(vc);
    for i in 0..vc {
        let at = HEADER + i * VERTEX_BYTES;
        let f = |k: usize| le_f32(bytes, at + 4 * k);
        let v = BakedVertex {
            pos: [f(0), f(1), f(2)],
            uv: [f(3), f(4)],
        };
        if !v.pos.iter().chain(&v.uv).all(|c| c.is_finite()) {
            return bad(format!("vertex {i} has a value that is not a number"));
        }
        vertices.push(v);
    }
    let first = HEADER + vc * VERTEX_BYTES;
    let mut indices = Vec::with_capacity(ic);
    for i in 0..ic {
        let index = le_u32(bytes, first + 4 * i);
        if index as usize >= vc {
            return bad(format!("index {i} is {index}, past the last vertex"));
        }
        indices.push(index);
    }
    Ok((vertices, indices))
}

/// A PNG's width and height from its header, reading 24 bytes.
fn png_size(path: &Path) -> Result<(u32, u32), PackError> {
    let mut head = [0u8; 24];
    let mut file = File::open(path).map_err(|e| io("texture", path, &e))?;
    file.read_exact(&mut head).map_err(|_| {
        PackError(format!(
            "texture {} is too short to be a PNG",
            path.display()
        ))
    })?;
    if head[..8] != *b"\x89PNG\r\n\x1a\n" || head[12..16] != *b"IHDR" {
        return bad(format!("texture {} is not a PNG", path.display()));
    }
    Ok((be_u32(&head, 16), be_u32(&head, 20)))
}

fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Decodes a `size` x `size` 8-bit RGB (or RGBA) PNG to RGBA8. Anything else
/// is refused, from the header, before the image data is touched.
pub fn decode_png(path: &Path, size: u32) -> Result<Vec<u8>, PackError> {
    let name = path.display();
    let len = std::fs::metadata(path)
        .map_err(|e| io("texture", path, &e))?
        .len();
    if len > MAX_PNG_FILE {
        return bad(format!(
            "texture {name} is {len} bytes (at most {MAX_PNG_FILE})"
        ));
    }
    let (w, h) = png_size(path)?;
    if (w, h) != (size, size) {
        return bad(format!("texture {name} is {w} x {h}, not {size} x {size}"));
    }
    let file = File::open(path).map_err(|e| io("texture", path, &e))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    let room = (MAX_TEXTURE as usize).pow(2) * 4 + (1 << 16);
    decoder.set_limits(png::Limits { bytes: room });
    let bad_png = |e: png::DecodingError| PackError(format!("texture {name}: {e}"));
    let mut reader = decoder.read_info().map_err(bad_png)?;
    let info = reader.info();
    let (color, depth) = (info.color_type, info.bit_depth);
    if depth != png::BitDepth::Eight || !matches!(color, png::ColorType::Rgb | png::ColorType::Rgba)
    {
        return bad(format!(
            "texture {name} is {color:?} at {depth:?} (8-bit RGB wanted)"
        ));
    }
    let Some(total) = reader.output_buffer_size() else {
        return bad(format!("texture {name} is too big"));
    };
    let mut raw = vec![0u8; total];
    let frame = reader.next_frame(&mut raw).map_err(bad_png)?;
    let raw = &raw[..frame.buffer_size()];
    let texels = (size as usize) * (size as usize);
    let channels = if color == png::ColorType::Rgb { 3 } else { 4 };
    if raw.len() != texels * channels {
        return bad(format!("texture {name} has {} bytes of pixels", raw.len()));
    }
    let mut rgba = Vec::with_capacity(texels * 4);
    for px in raw.chunks_exact(channels) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    Ok(rgba)
}

fn read_manifest(dir: &Path) -> Result<Manifest, PackError> {
    let path = dir.join("manifest.json");
    Manifest::parse(&read_capped(&path, MAX_MANIFEST_FILE, "manifest")?)
}

/// The checks for a pack the app has just downloaded into `dir`, before it
/// is moved into the places folder: it names itself `id` (the catalog's id),
/// every limit in the manifest holds, every file is there at its size, and the
/// whole pack reads. The textures are not decoded when they are ASTC. Takes a
/// moment: for a worker thread. Returns the pack's own version.
pub fn validate_download(dir: &Path, id: &str) -> Result<String, PackError> {
    let manifest = read_manifest(dir)?;
    if manifest.id != id {
        return bad(format!(
            "the pack calls itself {:?}, the catalog has {id:?}",
            manifest.id
        ));
    }
    preflight(dir, &manifest)?;
    // Every seat's set reads, not only the first the app would draw.
    for key in manifest.set_keys() {
        load_seat_with(dir, key, true)?;
    }
    Ok(manifest.version)
}

/// The version an installed pack's manifest names, if it has a readable one.
pub fn installed_version(dir: &Path) -> Option<String> {
    read_manifest(dir).ok().map(|m| m.version)
}

/// Reads the whole pack in `dir`, or says what is wrong with it. Takes a
/// moment (the textures are decoded): not for the render thread.
///
/// A format 2 pack's ASTC textures are kept as blocks when the device can
/// sample them ([`astc_on_gpu`]) and decoded to RGBA8 when it cannot.
///
/// A format 3 pack reads the set of its default seat; [`load_seat`] reads
/// another's.
pub fn load(dir: &Path) -> Result<Pack, PackError> {
    load_with(dir, astc_on_gpu())
}

/// [`load`], with `keep_astc` saying whether ASTC textures stay compressed.
pub fn load_with(dir: &Path, keep_astc: bool) -> Result<Pack, PackError> {
    load_seat_with(dir, None, keep_astc)
}

/// [`load`] for the viewer's `seat` (see [`Manifest::set_key`]): the mesh
/// and textures baked for it when the pack has a set for each seat, else the
/// pack as it is.
pub fn load_seat(dir: &Path, seat: Option<SeatName>) -> Result<Pack, PackError> {
    load_seat_with(dir, seat, astc_on_gpu())
}

/// [`load_seat`], with `keep_astc` saying whether ASTC textures stay
/// compressed.
pub fn load_seat_with(
    dir: &Path,
    seat: Option<SeatName>,
    keep_astc: bool,
) -> Result<Pack, PackError> {
    let manifest = read_manifest(dir)?;
    let set = manifest.set(seat)?;
    let mesh_path = dir.join(&set.mesh.file);
    let bytes = read_capped(&mesh_path, set.mesh_len(), "mesh")?;
    let (vertices, indices) = parse_mesh(&bytes, set.mesh)?;
    drop(bytes);
    let (colors, basis, path) = if manifest.format == 1 {
        load_png_textures(dir, set)?
    } else {
        load_ktx2_textures(dir, set, keep_astc)?
    };
    let baked = Baked {
        vertices,
        indices,
        colors,
        basis,
        basis_gain: set.basis_gain,
        seat: manifest.resolve(seat),
    };
    Ok(Pack {
        manifest,
        baked,
        path,
    })
}

/// Format 1: PNGs, one level each.
fn load_png_textures(
    dir: &Path,
    set: Set<'_>,
) -> Result<(TextureArray, TextureArray, TexturePath), PackError> {
    let t = set.textures;
    let decode = |name: &String| decode_png(&dir.join(name), t.size).map(|texels| vec![texels]);
    let colors = vec![decode(&t.house)?, decode(&t.dark)?];
    let basis = t.basis.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
    let array = |size, layers| TextureArray {
        size,
        encoding: Encoding::Rgba8,
        layers,
    };
    Ok((
        array(t.size, colors),
        array(set.basis_size(), basis),
        TexturePath::Png,
    ))
}

/// A format 2 texture file read and checked.
struct Texture {
    name: String,
    file_len: u64,
    header: ktx2::Header,
    levels: Vec<Vec<u8>>,
}

/// Checks that the textures of one kind agree in format (and are the sRGB
/// ones where `srgb`, UNORM where not, ASTC or RGBA8), and says which
/// format that is.
fn same_format(
    kind: &str,
    files: &[(&str, &ktx2::Header)],
    srgb: bool,
) -> Result<Encoding, PackError> {
    let (first_name, first) = files[0];
    for (name, h) in files {
        if (h.encoding, h.srgb) != (first.encoding, first.srgb) {
            return bad(format!(
                "textures {first_name} and {name} are not in the same format"
            ));
        }
    }
    if first.srgb != srgb {
        let (want, got) = if srgb {
            ("sRGB", "UNORM")
        } else {
            ("UNORM", "sRGB")
        };
        return bad(format!(
            "{kind} texture {first_name} is labelled {got} ({want} wanted)"
        ));
    }
    Ok(first.encoding)
}

/// Reads one texture file whole: capped, then every limit of [`ktx2::parse`].
fn read_texture(dir: &Path, name: &str, size: u32) -> Result<Texture, PackError> {
    let path = dir.join(name);
    let bytes = read_capped(&path, MAX_TEXTURE_BYTES, "texture")?;
    let head = &bytes[..bytes.len().min(ktx2::HEAD_BYTES)];
    let header = ktx2::parse(head, bytes.len() as u64, size)
        .map_err(|e| PackError(format!("texture {name} {e}")))?;
    let levels = ktx2::level_bytes(&bytes, &header);
    Ok(Texture {
        name: name.to_owned(),
        file_len: bytes.len() as u64,
        header,
        levels,
    })
}

/// Format 2: KTX2 files with their mips. ASTC ones are decoded (on all the
/// cores there are) unless `keep_astc`.
fn load_ktx2_textures(
    dir: &Path,
    set: Set<'_>,
    keep_astc: bool,
) -> Result<(TextureArray, TextureArray, TexturePath), PackError> {
    let t = set.textures;
    let (size, basis_size) = (t.size, set.basis_size());
    let mut colors = Vec::new();
    for name in [&t.house, &t.dark] {
        colors.push(read_texture(dir, name, size)?);
    }
    let mut basis = Vec::new();
    for name in &t.basis {
        basis.push(read_texture(dir, name, basis_size)?);
    }
    let total: u64 = colors.iter().chain(&basis).map(|x| x.file_len).sum();
    if t.bytes != Some(total) {
        return bad(format!(
            "textures are {total} bytes, the manifest says {:?}",
            t.bytes
        ));
    }
    let color_format = same_format(
        "house and dark",
        &colors
            .iter()
            .map(|x| (x.name.as_str(), &x.header))
            .collect::<Vec<_>>(),
        true,
    )?;
    let basis_format = same_format(
        "basis",
        &basis
            .iter()
            .map(|x| (x.name.as_str(), &x.header))
            .collect::<Vec<_>>(),
        false,
    )?;
    let compressed = [color_format, basis_format]
        .iter()
        .any(|e| matches!(e, Encoding::Astc { .. }));
    let decode = compressed && !keep_astc;
    let start = Instant::now();
    let mut all: Vec<Texture> = colors.into_iter().chain(basis).collect();
    if decode {
        decode_textures(&mut all)?;
    }
    let path = if decode {
        TexturePath::AstcDecoded {
            ms: start.elapsed().as_secs_f64() * 1e3,
        }
    } else if compressed {
        TexturePath::AstcOnGpu
    } else {
        TexturePath::Rgba
    };
    let basis = all.split_off(2);
    let array = |size: u32, encoding: Encoding, textures: Vec<Texture>| TextureArray {
        size,
        encoding: if decode { Encoding::Rgba8 } else { encoding },
        layers: textures.into_iter().map(|x| x.levels).collect(),
    };
    Ok((
        array(size, color_format, all),
        array(basis_size, basis_format, basis),
        path,
    ))
}

/// Decodes every ASTC texture in `textures` to RGBA8, level by level, spread
/// over the available cores.
fn decode_textures(textures: &mut [Texture]) -> Result<(), PackError> {
    let jobs: Vec<usize> = (0..textures.len()).collect();
    let results = par_map(&jobs, |&i| {
        let x = &textures[i];
        let Encoding::Astc { block } = x.header.encoding else {
            return Ok(None);
        };
        let mut levels = Vec::with_capacity(x.levels.len());
        let mut bad_blocks = 0;
        for (n, data) in x.levels.iter().enumerate() {
            let edge = (x.header.size >> n).max(1);
            let (texels, bad) = ktx2::decode_astc(data, edge, edge, block)
                .map_err(|e| PackError(format!("texture {} level {n}: {e}", x.name)))?;
            bad_blocks += bad;
            levels.push(texels);
        }
        if bad_blocks > 0 {
            log::warn!(
                "place texture {} has {bad_blocks} blocks that are not valid ASTC",
                x.name
            );
        }
        Ok(Some(levels))
    });
    for (x, decoded) in textures.iter_mut().zip(results) {
        if let Some(levels) = decoded? {
            x.levels = levels;
        }
    }
    Ok(())
}

/// `f` over `items`, on up to eight threads (this one among them), results in
/// order.
fn par_map<T: Sync, R: Send>(
    items: &[T],
    f: impl Fn(&T) -> Result<R, PackError> + Sync,
) -> Vec<Result<R, PackError>> {
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<Result<R, PackError>>>> =
        items.iter().map(|_| Mutex::new(None)).collect();
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(i) else { break };
            *fp_core::sync::lock(&slots[i]) = Some(f(item));
        }
    };
    let helpers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .clamp(1, 8)
        .min(items.len())
        .saturating_sub(1);
    std::thread::scope(|s| {
        for _ in 0..helpers {
            // No thread to be had: the others (and this one) do the work.
            let _ = std::thread::Builder::new()
                .name("place-decode".into())
                .spawn_scoped(s, work);
        }
        work();
    });
    slots
        .into_iter()
        .map(|m| {
            m.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unwrap_or_else(|| bad("a texture was not decoded"))
        })
        .collect()
}

/// What [`scan`] needs of a pack to offer it: the manifest is sound and the
/// files are there at the sizes it names. The pixels are not read; a format
/// 2 pack's texture headers and level tables are.
fn preflight(dir: &Path, m: &Manifest) -> Result<(), PackError> {
    for key in m.set_keys() {
        let set = m.set(key)?;
        preflight_set(dir, set).map_err(|e| match key {
            Some(seat) => PackError(format!("seat {}: {e}", seat.id())),
            None => e,
        })?;
    }
    Ok(())
}

fn preflight_set(dir: &Path, set: Set<'_>) -> Result<(), PackError> {
    let mesh = dir.join(&set.mesh.file);
    let len = std::fs::metadata(&mesh)
        .map_err(|e| io("mesh", &mesh, &e))?
        .len();
    if len != set.mesh_len() {
        return bad(format!(
            "mesh is {len} bytes, its counts need {}",
            set.mesh_len()
        ));
    }
    if set.format >= 2 {
        return preflight_ktx2(dir, set);
    }
    for name in set.texture_files() {
        let path = dir.join(name);
        let (w, h) = png_size(&path)?;
        if (w, h) != (set.textures.size, set.textures.size) {
            return bad(format!("texture {name} is {w} x {h}"));
        }
    }
    Ok(())
}

fn preflight_ktx2(dir: &Path, set: Set<'_>) -> Result<(), PackError> {
    let t = set.textures;
    let mut total = 0u64;
    let mut headers = Vec::new();
    for (i, name) in set.texture_files().enumerate() {
        let path = dir.join(name);
        let len = std::fs::metadata(&path)
            .map_err(|e| io("texture", &path, &e))?
            .len();
        if len > MAX_TEXTURE_BYTES {
            return bad(format!("texture {name} is {len} bytes"));
        }
        let mut head = Vec::new();
        File::open(&path)
            .and_then(|f| f.take(ktx2::HEAD_BYTES as u64).read_to_end(&mut head))
            .map_err(|e| io("texture", &path, &e))?;
        let size = if i < 2 { t.size } else { set.basis_size() };
        let header =
            ktx2::parse(&head, len, size).map_err(|e| PackError(format!("texture {name} {e}")))?;
        headers.push((name.as_str(), header));
        total += len;
    }
    if t.bytes != Some(total) {
        return bad(format!(
            "textures are {total} bytes, the manifest says {:?}",
            t.bytes
        ));
    }
    let refs = |range: Range<usize>| {
        headers[range]
            .iter()
            .map(|(n, h)| (*n, h))
            .collect::<Vec<_>>()
    };
    same_format("house and dark", &refs(0..2), true)?;
    same_format("basis", &refs(2..headers.len()), false)?;
    Ok(())
}

/// The built-in place a pack with this id replaces, and the pack's version
/// number: `cinema` is version 0, `cinema-v2` version 2. Only a room can be
/// replaced.
pub fn replaces(id: &str) -> Option<(Place, u32)> {
    let (name, version) = match id.rsplit_once("-v") {
        Some((name, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            (name, n.parse().ok()?)
        }
        _ => (id, 0),
    };
    Place::from_id(name)
        .filter(|p| p.kind() == PlaceKind::Room)
        .map(|p| (p, version))
}

/// A pack that stands in for a built-in place.
#[derive(Clone, Debug)]
pub struct Replacement {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub layout: Layout,
}

/// The packs in `root` that replace a place, the newest of each: every
/// folder's manifest is read, and a pack with anything wrong in it is
/// logged and left out.
pub fn scan(root: &Path) -> HashMap<Place, Arc<Replacement>> {
    let mut found: HashMap<Place, (u32, Arc<Replacement>)> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return HashMap::new();
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    // A dot-folder is an install's working area (`.<id>.part`, `.<id>.old`),
    // never a pack.
    let hidden = |d: &Path| {
        d.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
    };
    for dir in dirs.into_iter().filter(|d| d.is_dir() && !hidden(d)) {
        let manifest = match read_manifest(&dir) {
            Ok(m) => m,
            // A folder with no manifest is not a pack: not worth a warning.
            Err(_) if !dir.join("manifest.json").exists() => continue,
            Err(e) => {
                log::warn!("place pack {} is not used: {e}", dir.display());
                continue;
            }
        };
        let Some((place, version)) = replaces(&manifest.id) else {
            log::info!("place pack {} replaces no place here", manifest.id);
            continue;
        };
        if let Err(e) = preflight(&dir, &manifest) {
            log::warn!("place pack {} is not used: {e}", manifest.id);
            continue;
        }
        if found.get(&place).is_some_and(|(v, _)| *v > version) {
            continue;
        }
        let layout = manifest.layout();
        let pack = Replacement {
            dir,
            manifest,
            layout,
        };
        found.insert(place, (version, Arc::new(pack)));
    }
    found.into_iter().map(|(p, (_, r))| (p, r)).collect()
}

/// The packs that ship inside the app: `places/` beside the binary. The
/// Cinema comes this way; packs in [`places_dir`] replace them.
pub fn bundled_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("places"))
}

/// The folder the packs are looked for in.
pub fn places_dir() -> PathBuf {
    match std::env::var_os(PLACES_DIR_ENV).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => fp_core::dirs::data_dir().join("places"),
    }
}

static CATALOG: OnceLock<HashMap<Place, Arc<Replacement>>> = OnceLock::new();
static FAILED: Mutex<Vec<Place>> = Mutex::new(Vec::new());

/// The pack that replaces `place`, if one is installed and has not failed to
/// load. Packs are read once, the first time anyone asks: a pack installed
/// later is found on the next start.
pub fn replacement(place: Place) -> Option<Arc<Replacement>> {
    // This crate's own unit tests are about the built-in rooms, whatever is
    // installed on the machine running them.
    if cfg!(test) {
        return None;
    }
    let failed = FAILED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if failed.contains(&place) {
        return None;
    }
    drop(failed);
    let catalog = CATALOG.get_or_init(|| {
        let root = places_dir();
        // Bundled first; a pack the user installed for the same place wins.
        let mut found = bundled_dir().map(|d| scan(&d)).unwrap_or_default();
        found.extend(scan(&root));
        for (place, r) in &found {
            log::info!(
                "place pack {} (version {}) replaces {}",
                r.manifest.id,
                r.manifest.version,
                place.id()
            );
        }
        found
    });
    catalog.get(&place).cloned()
}

/// Which set of `place`'s pack a viewer in `seat` draws, named by the seat it
/// was baked for (see [`Manifest::set_key`]): `None` when the place is the
/// built-in room, has a pack with one set for everybody, or its pack has
/// failed. Cheap: the packs were read at the first ask.
pub fn seat_set(place: Place, seat: SeatName) -> Option<SeatName> {
    replacement(place)?.manifest.set_key(seat)
}

/// Makes the packs in `root` the catalog if nobody has asked for it yet, and
/// says whether that worked: for a test that needs a pack installed, which
/// the environment cannot do once the catalog has been read.
#[doc(hidden)]
pub fn use_catalog_for_tests(root: &Path) -> bool {
    CATALOG.set(scan(root)).is_ok()
}

/// Notes that the pack for `place` could not be loaded: from now on the
/// built-in room is the place, seats and all.
pub fn mark_failed(place: Place) {
    let mut failed = FAILED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !failed.contains(&place) {
        failed.push(place);
    }
}

/// The place's pack data if it has a pack that loads; otherwise `None`, and
/// the built-in room is used (a pack that fails here is dropped for good,
/// see [`mark_failed`]).
///
/// A pack with a set for each seat gives that of its default seat; see
/// [`load_replacement_for`].
pub fn load_replacement(place: Place) -> Option<PlaceData> {
    load_replacement_for(place, None)
}

/// [`load_replacement`] for the set a viewer in some seat draws (the seat
/// [`seat_set`] named; `None` for the pack's default). A pack none of whose
/// sets is for everybody (formats 1 and 2) ignores it. If the seat's set does
/// not load the whole pack is dropped, as any pack that fails is.
pub fn load_replacement_for(place: Place, seat: Option<SeatName>) -> Option<PlaceData> {
    let r = replacement(place)?;
    match load_seat(&r.dir, seat) {
        Ok(pack) => {
            let b = &pack.baked;
            let seat = b
                .seat
                .map_or(String::new(), |s| format!(", {} seat", s.id()));
            log::info!(
                "loaded place pack {} (format {}{seat}): {} triangles; textures {} \
                 ({} px colours, {} px basis, {:.1} MB)",
                r.manifest.id,
                r.manifest.format,
                b.indices.len() / 3,
                pack.path,
                b.colors.size,
                b.basis.size,
                (b.colors.gpu_bytes() + b.basis.gpu_bytes()) as f64 / (1 << 20) as f64
            );
            Some(pack.into_place_data())
        }
        Err(e) => {
            log::warn!(
                "place pack {} is not used, showing the built-in {}: {e}",
                r.manifest.id,
                place.id()
            );
            mark_failed(place);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufWriter;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fp-pack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write_png(path: &Path, size: u32, rgb: [u8; 3]) {
        let file = BufWriter::new(File::create(path).expect("png file"));
        let mut e = png::Encoder::new(file, size, size);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let data: Vec<u8> = (0..size * size).flat_map(|_| rgb).collect();
        e.write_header()
            .expect("png header")
            .write_image_data(&data)
            .expect("png data");
    }

    /// Four triangles over five vertices: a pyramid's sides, say.
    fn mesh_bytes(vertices: &[[f32; 5]], indices: &[u32]) -> Vec<u8> {
        let mut b = MAGIC.to_vec();
        b.extend((vertices.len() as u32).to_le_bytes());
        b.extend((indices.len() as u32).to_le_bytes());
        for f in vertices.iter().flatten() {
            b.extend(f.to_le_bytes());
        }
        for i in indices {
            b.extend(i.to_le_bytes());
        }
        b
    }

    const VERTICES: [[f32; 5]; 5] = [
        [-1.0, 0.0, -2.0, 0.0, 0.0],
        [1.0, 0.0, -2.0, 1.0, 0.0],
        [1.0, 2.0, -2.0, 1.0, 1.0],
        [-1.0, 2.0, -2.0, 0.0, 1.0],
        [0.0, 1.0, -3.0, 0.5, 0.5],
    ];
    const INDICES: [u32; 12] = [0, 1, 2, 0, 2, 3, 0, 1, 4, 2, 3, 4];

    fn manifest_json(id: &str, size: u32) -> String {
        let basis: Vec<String> = (0..8).map(|i| format!("\"basis{i}.png\"")).collect();
        format!(
            r#"{{
  "format": 1, "id": "{id}", "name": "Test", "version": "1",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [9.0, 3.8] }},
  "seats": [ {{ "name": "front", "eye": [0, 2.9, 5.0] }},
             {{ "name": "middle", "eye": [0, 3.2, 9.0] }},
             {{ "name": "back", "eye": [0, 3.6, 13.0] }} ],
  "default_seat": 1,
  "mesh": {{ "file": "mesh.bin", "vertices": 5, "triangles": 4 }},
  "textures": {{ "size": {size}, "house": "house.png", "dark": "dark.png",
                 "basis": [{}] }},
  "basis_gain": 1.0, "sky": null
}}"#,
            basis.join(", ")
        )
    }

    /// A good pack named `id` in a fresh folder.
    fn fixture(name: &str, id: &str) -> PathBuf {
        let dir = temp(name);
        write_fixture(&dir, id);
        dir
    }

    fn write_fixture(dir: &Path, id: &str) {
        std::fs::create_dir_all(dir).expect("pack dir");
        std::fs::write(dir.join("manifest.json"), manifest_json(id, 4)).expect("manifest");
        std::fs::write(dir.join("mesh.bin"), mesh_bytes(&VERTICES, &INDICES)).expect("mesh");
        write_png(&dir.join("house.png"), 4, [200, 120, 40]);
        write_png(&dir.join("dark.png"), 4, [10, 10, 10]);
        for i in 0..8 {
            write_png(
                &dir.join(format!("basis{i}.png")),
                4,
                [255 * (i == 0) as u8; 3],
            );
        }
    }

    fn refused(dir: &Path) -> String {
        match load(dir) {
            Ok(_) => panic!("the pack was accepted"),
            Err(e) => e.0,
        }
    }

    #[test]
    fn a_good_pack_loads() {
        let dir = fixture("good", "cinema-v2");
        let pack = load(&dir).expect("loads");
        assert_eq!(pack.manifest.id, "cinema-v2");
        let b = &pack.baked;
        assert_eq!(
            (b.vertices.len(), b.indices.len(), b.colors.size),
            (5, 12, 4)
        );
        assert_eq!(b.vertices[4].pos, [0.0, 1.0, -3.0]);
        assert_eq!(b.indices, INDICES);
        assert_eq!(b.colors.layers[0][0].len(), 4 * 4 * 4);
        assert_eq!(&b.colors.layers[0][0][..4], &[200, 120, 40, 255]);
        assert_eq!(b.basis.layers.len(), BASIS);
        assert_eq!(&b.basis.layers[0][0][..4], &[255, 255, 255, 255]);
        assert_eq!(&b.basis.layers[3][0][..4], &[0, 0, 0, 255]);
        let layout = pack.manifest.layout();
        assert_eq!(layout.seats.len(), 3);
        assert_eq!(layout.seats[2].name, SeatName::Back);
        assert_eq!(layout.default_seat, 1);
        let screen = layout.screen.expect("a screen");
        assert_eq!(screen.max, Vec2::new(9.0, 3.8));
        let data = pack.into_place_data();
        assert!(data.baked.is_some() && data.vertices.is_empty());
    }

    #[test]
    fn a_pack_with_an_rgba_texture_loads_too() {
        let dir = fixture("rgba", "cinema-v2");
        let file = BufWriter::new(File::create(dir.join("dark.png")).expect("file"));
        let mut e = png::Encoder::new(file, 4, 4);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let data: Vec<u8> = (0..16).flat_map(|_| [9, 8, 7, 0]).collect();
        e.write_header()
            .expect("header")
            .write_image_data(&data)
            .expect("data");
        let pack = load(&dir).expect("loads");
        assert_eq!(&pack.baked.colors.layers[1][0][..4], &[9, 8, 7, 255]);
    }

    #[test]
    fn a_bad_magic_is_refused() {
        let dir = fixture("magic", "cinema-v2");
        let mut bytes = mesh_bytes(&VERTICES, &INDICES);
        bytes[..4].copy_from_slice(b"NOPE");
        std::fs::write(dir.join("mesh.bin"), bytes).unwrap();
        assert!(refused(&dir).contains("LPM1"));
    }

    #[test]
    fn a_short_or_long_mesh_is_refused() {
        let dir = fixture("short", "cinema-v2");
        let bytes = mesh_bytes(&VERTICES, &INDICES);
        for len in [0, 5, 11, 12, 40, bytes.len() - 1] {
            std::fs::write(dir.join("mesh.bin"), &bytes[..len]).unwrap();
            let why = refused(&dir);
            assert!(why.contains("mesh"), "{len}: {why}");
        }
        let mut long = bytes;
        long.extend([0, 0, 0, 0]);
        std::fs::write(dir.join("mesh.bin"), long).unwrap();
        assert!(refused(&dir).contains("bytes"));
    }

    #[test]
    fn a_mesh_whose_counts_disagree_with_the_manifest_is_refused() {
        let dir = fixture("counts", "cinema-v2");
        let mut bytes = mesh_bytes(&VERTICES, &INDICES);
        bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(dir.join("mesh.bin"), bytes).unwrap();
        assert!(refused(&dir).contains("vertices"));
    }

    #[test]
    fn an_index_past_the_last_vertex_is_refused() {
        let dir = fixture("index", "cinema-v2");
        let mut indices = INDICES;
        indices[7] = 5;
        std::fs::write(dir.join("mesh.bin"), mesh_bytes(&VERTICES, &indices)).unwrap();
        assert!(refused(&dir).contains("index"));
        indices[7] = u32::MAX;
        std::fs::write(dir.join("mesh.bin"), mesh_bytes(&VERTICES, &indices)).unwrap();
        assert!(refused(&dir).contains("index"));
    }

    #[test]
    fn a_value_that_is_not_a_number_is_refused() {
        let dir = fixture("nan", "cinema-v2");
        for bad_value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for slot in [0, 2, 4] {
                let mut vertices = VERTICES;
                vertices[1][slot] = bad_value;
                std::fs::write(dir.join("mesh.bin"), mesh_bytes(&vertices, &INDICES)).unwrap();
                assert!(
                    refused(&dir).contains("not a number"),
                    "{bad_value} at {slot}"
                );
            }
        }
    }

    #[test]
    fn an_oversized_texture_is_refused() {
        let dir = fixture("big", "cinema-v2");
        let json = manifest_json("cinema-v2", 8192);
        std::fs::write(dir.join("manifest.json"), json).unwrap();
        assert!(refused(&dir).contains("8192"));
        // Within the limit by the manifest, but a PNG of another size.
        let dir = fixture("mismatch", "cinema-v2");
        write_png(&dir.join("basis5.png"), 8, [0; 3]);
        let why = refused(&dir);
        assert!(why.contains("8 x 8"), "{why}");
    }

    #[test]
    fn a_texture_that_is_not_a_power_of_two_is_refused() {
        let dir = fixture("pot", "cinema-v2");
        std::fs::write(dir.join("manifest.json"), manifest_json("cinema-v2", 6)).unwrap();
        assert!(refused(&dir).contains("power of two"));
    }

    #[test]
    fn a_missing_basis_is_refused() {
        let dir = fixture("nobasis", "cinema-v2");
        std::fs::remove_file(dir.join("basis3.png")).unwrap();
        assert!(refused(&dir).contains("basis3.png"));
        // Or too few of them in the manifest.
        let dir = fixture("fewbasis", "cinema-v2");
        let json = manifest_json("cinema-v2", 4).replace(", \"basis7.png\"", "");
        std::fs::write(dir.join("manifest.json"), json).unwrap();
        assert!(refused(&dir).contains("basis textures"));
    }

    #[test]
    fn a_texture_that_is_not_a_png_is_refused() {
        let dir = fixture("notpng", "cinema-v2");
        std::fs::write(dir.join("house.png"), b"not a png at all, really not").unwrap();
        assert!(refused(&dir).contains("not a PNG"));
        std::fs::write(dir.join("house.png"), b"").unwrap();
        assert!(refused(&dir).contains("too short"));
        // A real header, a cut-off body.
        write_png(&dir.join("house.png"), 4, [1, 2, 3]);
        let bytes = std::fs::read(dir.join("house.png")).unwrap();
        std::fs::write(dir.join("house.png"), &bytes[..bytes.len() / 2]).unwrap();
        assert!(!refused(&dir).is_empty());
    }

    #[test]
    fn a_texture_in_another_colour_type_is_refused() {
        let dir = fixture("gray", "cinema-v2");
        let file = BufWriter::new(File::create(dir.join("house.png")).unwrap());
        let mut e = png::Encoder::new(file, 4, 4);
        e.set_color(png::ColorType::Grayscale);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .unwrap()
            .write_image_data(&[7; 16])
            .unwrap();
        assert!(refused(&dir).contains("8-bit RGB"));
    }

    #[test]
    fn broken_manifests_are_refused_without_a_panic() {
        let dir = fixture("manifest", "cinema-v2");
        let good = manifest_json("cinema-v2", 4);
        let cases = [
            String::new(),
            "{".into(),
            "[]".into(),
            "null".into(),
            "{\"format\": 1}".into(),
            good.replace("\"format\": 1", "\"format\": 4"),
            good.replace("cinema-v2", "Cinema V2"),
            good.replace("cinema-v2", ""),
            good.replace("\"front\"", "\"couch\""),
            good.replace("\"back\"", "\"front\""),
            good.replace("\"default_seat\": 1", "\"default_seat\": 3"),
            good.replace("9.0, 3.8", "0.0, 3.8"),
            good.replace("9.0, 3.8", "-9.0, 3.8"),
            good.replace("\"triangles\": 4", "\"triangles\": 100001"),
            good.replace("\"triangles\": 4", "\"triangles\": 0"),
            good.replace("\"vertices\": 5", "\"vertices\": 200001"),
            good.replace("\"mesh.bin\"", "\"../mesh.bin\""),
            good.replace("\"mesh.bin\"", "\"/etc/passwd\""),
            good.replace("\"house.png\"", "\"sub/house.png\""),
            good.replace("\"basis_gain\": 1.0", "\"basis_gain\": -1.0"),
            good.replace("\"basis_gain\": 1.0", "\"basis_gain\": 1e39"),
            good.replace("\"eye\": [0, 2.9, 5.0]", "\"eye\": [0, 2.9]"),
        ];
        for (i, json) in cases.iter().enumerate() {
            std::fs::write(dir.join("manifest.json"), json).unwrap();
            assert!(load(&dir).is_err(), "case {i} was accepted: {json}");
        }
    }

    #[test]
    fn a_missing_folder_or_file_is_refused() {
        assert!(load(Path::new("/definitely/not/here")).is_err());
        let dir = fixture("nomesh", "cinema-v2");
        std::fs::remove_file(dir.join("mesh.bin")).unwrap();
        assert!(refused(&dir).contains("mesh"));
    }

    #[test]
    fn random_bytes_never_panic_the_mesh_reader() {
        let dir = fixture("fuzz", "cinema-v2");
        let m = read_manifest(&dir).expect("manifest").mesh.expect("a mesh");
        let good = mesh_bytes(&VERTICES, &INDICES);
        let mut seed = 0x1234_5678_9abc_def0u64;
        for round in 0..300 {
            let mut bytes = good.clone();
            for _ in 0..(1 + round % 6) {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let at = (seed >> 33) as usize % bytes.len();
                bytes[at] = (seed >> 24) as u8;
            }
            let _ = parse_mesh(&bytes, &m);
            let cut = (seed >> 40) as usize % (bytes.len() + 1);
            let _ = parse_mesh(&bytes[..cut], &m);
        }
    }

    #[test]
    fn ids_name_the_place_they_replace() {
        assert_eq!(replaces("cinema"), Some((Place::Cinema, 0)));
        assert_eq!(replaces("cinema-v2"), Some((Place::Cinema, 2)));
        assert_eq!(replaces("cinema-v10"), Some((Place::Cinema, 10)));
        // Not a room, not a place, not a version.
        assert_eq!(replaces("studio-v2"), None);
        assert_eq!(replaces("cinema-vx"), None);
        assert_eq!(replaces("cinema-v"), None);
        assert_eq!(replaces("cinemas"), None);
        assert_eq!(replaces("opera-v2"), None);
        assert_eq!(replaces(""), None);
    }

    #[test]
    fn scan_finds_the_newest_pack_for_a_place_and_skips_the_broken_ones() {
        let root = temp("scan");
        write_fixture(&root.join("cinema-v2"), "cinema-v2");
        write_fixture(&root.join("cinema-v3"), "cinema-v3");
        write_fixture(&root.join("cinema-v10"), "cinema-v10");
        write_fixture(&root.join("opera"), "opera");
        write_fixture(&root.join("studio-v2"), "studio-v2");
        // Newer, but broken: the short mesh keeps it out.
        write_fixture(&root.join("cinema-v11"), "cinema-v11");
        std::fs::write(root.join("cinema-v11/mesh.bin"), b"LPM1").unwrap();
        // An update's working folders hold whole packs but are not packs.
        write_fixture(&root.join(".cinema-v12.part"), "cinema-v12");
        write_fixture(&root.join(".cinema-v13.old"), "cinema-v13");
        // Not packs.
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("stray.txt"), b"hello").unwrap();
        let found = scan(&root);
        assert_eq!(found.len(), 1);
        let cinema = &found[&Place::Cinema];
        assert_eq!(cinema.manifest.id, "cinema-v10");
        assert_eq!(cinema.layout.seats.len(), 3);
        assert!(cinema.dir.ends_with("cinema-v10"));
        assert!(scan(&root.join("nothing")).is_empty());
    }

    #[test]
    fn a_pack_with_the_wrong_texture_size_on_disk_is_not_offered() {
        let root = temp("scansize");
        write_fixture(&root.join("cinema-v2"), "cinema-v2");
        write_png(&root.join("cinema-v2/basis2.png"), 8, [0; 3]);
        assert!(scan(&root).is_empty());
    }

    #[test]
    fn the_places_folder_defaults_under_the_data_dir() {
        // Not set in the test environment unless a developer did.
        if std::env::var_os(PLACES_DIR_ENV).is_none() {
            assert!(places_dir().ends_with("frameplayerx/places"));
        }
    }

    #[test]
    fn the_basis_count_is_the_screens_patch_count() {
        assert_eq!(BASIS, super::super::light::PATCHES);
    }

    // ---- Format 2: KTX2 textures, ASTC or plain ----

    const HOUSE: [u8; 4] = [200, 120, 40, 255];
    const DARK: [u8; 4] = [10, 20, 30, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    /// The format 2 fixture's sizes: the colours 16 texels square, the basis
    /// 8, ASTC 4 x 4 for the colours and 6 x 6 for the basis.
    const COLOR_SIZE: u32 = 16;
    const BASIS_SIZE: u32 = 8;

    /// A texture file of one colour: ASTC blocks of `block` (sRGB or not),
    /// or, with no block, RGBA8 in `vk_format`.
    fn solid(
        size: u32,
        format: Option<((u32, u32), bool)>,
        vk_format: u32,
        rgba: [u8; 4],
    ) -> Vec<u8> {
        let (encoding, vk) = match format {
            Some((block, srgb)) => (
                Encoding::Astc { block },
                ktx2::astc_vk_format(block, srgb).unwrap(),
            ),
            None => (Encoding::Rgba8, vk_format),
        };
        let levels: Vec<Vec<u8>> = (0..ktx2::full_chain(size))
            .map(|i| {
                let edge = (size >> i).max(1);
                match encoding {
                    Encoding::Rgba8 => (0..edge * edge).flat_map(|_| rgba).collect(),
                    Encoding::Astc { .. } => {
                        let blocks = encoding.level_bytes(edge, edge) / 16;
                        (0..blocks).flat_map(|_| ktx2::void_block(rgba)).collect()
                    }
                }
            })
            .collect();
        ktx2::build(vk, size, &levels)
    }

    fn manifest2_json(id: &str, bytes: u64) -> String {
        let basis: Vec<String> = (0..8).map(|i| format!("\"basis{i}.ktx2\"")).collect();
        format!(
            r#"{{
  "format": 2, "id": "{id}", "name": "Test", "version": "1",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [9.0, 3.8] }},
  "seats": [ {{ "name": "middle", "eye": [0, 3.2, 9.0] }} ],
  "mesh": {{ "file": "mesh.bin", "vertices": 5, "triangles": 4 }},
  "textures": {{ "size": {COLOR_SIZE}, "basis_size": {BASIS_SIZE}, "bytes": {bytes},
                 "house": "house.ktx2", "dark": "dark.ktx2", "basis": [{}] }},
  "basis_gain": 1.0
}}"#,
            basis.join(", ")
        )
    }

    /// Rewrites `manifest.json` with the bytes the texture files add up to.
    fn fix_bytes(dir: &Path, id: &str) {
        let mut total = 0;
        for name in [
            "house", "dark", "basis0", "basis1", "basis2", "basis3", "basis4", "basis5", "basis6",
            "basis7",
        ] {
            total += std::fs::metadata(dir.join(format!("{name}.ktx2"))).map_or(0, |m| m.len());
        }
        std::fs::write(dir.join("manifest.json"), manifest2_json(id, total)).expect("manifest");
    }

    fn write_fixture2(dir: &Path, id: &str) {
        std::fs::create_dir_all(dir).expect("pack dir");
        std::fs::write(dir.join("mesh.bin"), mesh_bytes(&VERTICES, &INDICES)).expect("mesh");
        let colour = Some(((4, 4), true));
        let light = Some(((6, 6), false));
        std::fs::write(dir.join("house.ktx2"), solid(COLOR_SIZE, colour, 0, HOUSE)).unwrap();
        std::fs::write(dir.join("dark.ktx2"), solid(COLOR_SIZE, colour, 0, DARK)).unwrap();
        for i in 0..8 {
            let rgba = if i == 0 { WHITE } else { BLACK };
            std::fs::write(
                dir.join(format!("basis{i}.ktx2")),
                solid(BASIS_SIZE, light, 0, rgba),
            )
            .unwrap();
        }
        fix_bytes(dir, id);
    }

    fn fixture2(name: &str) -> PathBuf {
        let dir = temp(name);
        write_fixture2(&dir, "cinema-v2");
        dir
    }

    fn refused_with(dir: &Path, keep_astc: bool) -> String {
        match load_with(dir, keep_astc) {
            Ok(_) => panic!("the pack was accepted"),
            Err(e) => e.0,
        }
    }

    #[test]
    fn a_format_2_pack_is_decoded_on_the_cpu_when_the_device_cannot_sample_astc() {
        let dir = fixture2("f2cpu");
        let pack = load_with(&dir, false).expect("loads");
        assert_eq!(pack.manifest.format, 2);
        assert!(matches!(pack.path, TexturePath::AstcDecoded { .. }));
        let b = &pack.baked;
        assert_eq!((b.colors.size, b.basis.size), (COLOR_SIZE, BASIS_SIZE));
        assert_eq!(
            (b.colors.encoding, b.basis.encoding),
            (Encoding::Rgba8, Encoding::Rgba8)
        );
        assert_eq!((b.colors.layers.len(), b.basis.layers.len()), (2, BASIS));
        // Every level is there, at its own size, the colour the file had.
        assert_eq!((b.colors.levels(), b.basis.levels()), (5, 4));
        for (layer, want) in [(0, HOUSE), (1, DARK)] {
            for (mip, level) in b.colors.layers[layer].iter().enumerate() {
                let edge = (COLOR_SIZE as usize >> mip).max(1);
                assert_eq!(level.len(), edge * edge * 4);
                assert!(
                    level.as_chunks::<4>().0.iter().all(|p| *p == want),
                    "layer {layer} level {mip}"
                );
            }
        }
        assert_eq!(&b.basis.layers[0][3], &WHITE);
        assert_eq!(&b.basis.layers[5][0][..4], &BLACK);
        assert_eq!(b.basis.layers[5][0].len(), 8 * 8 * 4);
    }

    #[test]
    fn a_format_2_pack_keeps_its_astc_blocks_when_the_device_can_sample_them() {
        let dir = fixture2("f2gpu");
        let pack = load_with(&dir, true).expect("loads");
        assert_eq!(pack.path, TexturePath::AstcOnGpu);
        let b = &pack.baked;
        assert_eq!(b.colors.encoding, Encoding::Astc { block: (4, 4) });
        assert_eq!(b.basis.encoding, Encoding::Astc { block: (6, 6) });
        // 16 -> 4 x 4 blocks, then 2 x 2, 1, 1, 1; 8 (6 x 6 blocks) -> 2 x 2, 1, 1, 1.
        let lens: Vec<usize> = b.colors.layers[0].iter().map(Vec::len).collect();
        assert_eq!(lens, [256, 64, 16, 16, 16]);
        let lens: Vec<usize> = b.basis.layers[0].iter().map(Vec::len).collect();
        assert_eq!(lens, [64, 16, 16, 16]);
        assert_eq!(b.colors.layers[0][4], ktx2::void_block(HOUSE));
        // The texture MB the log reports: all of it, nothing made up.
        assert_eq!(b.colors.gpu_bytes(), 2 * (256 + 64 + 48));
        assert_eq!(b.basis.gpu_bytes(), 8 * (64 + 48));
    }

    #[test]
    fn the_cpu_path_and_the_gpu_path_hold_the_same_picture() {
        let dir = fixture2("f2same");
        let kept = load_with(&dir, true).expect("kept");
        let decoded = load_with(&dir, false).expect("decoded");
        let Encoding::Astc { block } = kept.baked.colors.encoding else {
            panic!("not kept as ASTC");
        };
        for (l, layer) in kept.baked.colors.layers.iter().enumerate() {
            for (m, level) in layer.iter().enumerate() {
                let edge = (COLOR_SIZE >> m).max(1);
                let (texels, bad) = ktx2::decode_astc(level, edge, edge, block).unwrap();
                assert_eq!(bad, 0);
                assert_eq!(texels, decoded.baked.colors.layers[l][m]);
            }
        }
    }

    #[test]
    fn a_kept_pack_decodes_in_memory_to_what_loading_it_decoded_gives() {
        let dir = fixture2("f2later");
        let kept = load_with(&dir, true).expect("kept");
        assert!(kept.baked.is_astc());
        let later = kept.baked.decoded().expect("decodes").expect("it was ASTC");
        let direct = load_with(&dir, false).expect("decoded").baked;
        assert!(!later.is_astc());
        assert_eq!(later.colors.encoding, Encoding::Rgba8);
        assert_eq!(later.colors.size, direct.colors.size);
        assert_eq!(later.colors.layers, direct.colors.layers);
        assert_eq!(later.basis.layers, direct.basis.layers);
        assert_eq!(later.vertices, direct.vertices);
        assert_eq!(later.indices, direct.indices);
        // Nothing left to decode.
        assert!(later.decoded().expect("fine").is_none());
    }

    #[test]
    fn a_format_2_pack_of_plain_rgba_textures_loads_whatever_the_device() {
        let dir = temp("f2rgba");
        write_fixture2(&dir, "cinema-v2");
        let plain = |size, rgba| solid(size, None, 43, rgba);
        std::fs::write(dir.join("house.ktx2"), plain(COLOR_SIZE, HOUSE)).unwrap();
        std::fs::write(dir.join("dark.ktx2"), plain(COLOR_SIZE, DARK)).unwrap();
        for i in 0..8 {
            // UNORM, for the basis.
            std::fs::write(
                dir.join(format!("basis{i}.ktx2")),
                solid(BASIS_SIZE, None, 37, if i == 0 { WHITE } else { BLACK }),
            )
            .unwrap();
        }
        fix_bytes(&dir, "cinema-v2");
        for keep in [true, false] {
            let pack = load_with(&dir, keep).expect("loads");
            assert_eq!(pack.path, TexturePath::Rgba);
            assert_eq!(pack.baked.colors.levels(), 5);
            assert_eq!(&pack.baked.colors.layers[0][4], &HOUSE);
            assert_eq!(&pack.baked.basis.layers[0][0][..4], &WHITE);
        }
        // Colours ASTC, the basis plain: each array in its own format.
        std::fs::write(
            dir.join("house.ktx2"),
            solid(COLOR_SIZE, Some(((8, 8), true)), 0, HOUSE),
        )
        .unwrap();
        std::fs::write(
            dir.join("dark.ktx2"),
            solid(COLOR_SIZE, Some(((8, 8), true)), 0, DARK),
        )
        .unwrap();
        fix_bytes(&dir, "cinema-v2");
        let pack = load_with(&dir, true).expect("loads");
        assert_eq!(pack.baked.colors.encoding, Encoding::Astc { block: (8, 8) });
        assert_eq!(pack.baked.basis.encoding, Encoding::Rgba8);
    }

    #[test]
    fn a_format_2_pack_that_breaks_a_limit_is_refused_on_either_path() {
        let id = "cinema-v2";
        let colour = Some(((4, 4), true));
        let light = Some(((6, 6), false));
        let write = |dir: &Path, name: &str, bytes: Vec<u8>| {
            std::fs::write(dir.join(name), bytes).unwrap();
            fix_bytes(dir, id);
        };
        for keep in [false, true] {
            let case = |what: &str| fixture2(&format!("f2bad-{what}-{keep}"));
            // The sum of the files is not what the manifest says.
            let dir = case("bytes");
            let json = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
            let total: u64 = json
                .split("\"bytes\": ")
                .nth(1)
                .and_then(|r| r.split(',').next())
                .and_then(|n| n.parse().ok())
                .expect("bytes");
            std::fs::write(dir.join("manifest.json"), manifest2_json(id, total + 1)).unwrap();
            assert!(refused_with(&dir, keep).contains("bytes"), "{keep}");
            // Over the budget (a manifest that says so is refused unread).
            std::fs::write(
                dir.join("manifest.json"),
                manifest2_json(id, MAX_TEXTURE_BYTES + 1),
            )
            .unwrap();
            assert!(refused_with(&dir, keep).contains("at most"));
            // House and dark not alike.
            let dir = case("alike");
            write(
                &dir,
                "dark.ktx2",
                solid(COLOR_SIZE, Some(((5, 5), true)), 0, DARK),
            );
            assert!(refused_with(&dir, keep).contains("same format"));
            let dir = case("alike2");
            write(
                &dir,
                "basis4.ktx2",
                solid(BASIS_SIZE, Some(((8, 8), false)), 0, BLACK),
            );
            assert!(refused_with(&dir, keep).contains("same format"));
            // ASTC the wrong way round: UNORM colours, sRGB basis.
            let dir = case("unorm");
            write(
                &dir,
                "house.ktx2",
                solid(COLOR_SIZE, Some(((4, 4), false)), 0, HOUSE),
            );
            write(
                &dir,
                "dark.ktx2",
                solid(COLOR_SIZE, Some(((4, 4), false)), 0, DARK),
            );
            assert!(refused_with(&dir, keep).contains("sRGB wanted"));
            let dir = case("srgb");
            for i in 0..8 {
                write(
                    &dir,
                    &format!("basis{i}.ktx2"),
                    solid(BASIS_SIZE, Some(((6, 6), true)), 0, BLACK),
                );
            }
            assert!(refused_with(&dir, keep).contains("UNORM wanted"));
            // The same labels on uncompressed textures: colours UNORM,
            // basis sRGB.
            let dir = case("rgba-unorm");
            for name in ["house.ktx2", "dark.ktx2"] {
                write(&dir, name, solid(COLOR_SIZE, None, 37, HOUSE));
            }
            for i in 0..8 {
                write(
                    &dir,
                    &format!("basis{i}.ktx2"),
                    solid(BASIS_SIZE, None, 37, BLACK),
                );
            }
            let msg = refused_with(&dir, keep);
            assert!(
                msg.contains("house.ktx2") && msg.contains("sRGB wanted"),
                "{msg}"
            );
            let dir = case("rgba-srgb");
            for i in 0..8 {
                write(
                    &dir,
                    &format!("basis{i}.ktx2"),
                    solid(BASIS_SIZE, None, 43, BLACK),
                );
            }
            let msg = refused_with(&dir, keep);
            assert!(
                msg.contains("basis0.ktx2") && msg.contains("UNORM wanted"),
                "{msg}"
            );
            // A texture of the other size, or not a texture at all.
            let dir = case("size");
            write(&dir, "basis2.ktx2", solid(COLOR_SIZE, light, 0, BLACK));
            assert!(refused_with(&dir, keep).contains("basis2.ktx2"));
            let dir = case("notktx");
            write(
                &dir,
                "house.ktx2",
                b"hello, not a texture at all, no".to_vec(),
            );
            assert!(refused_with(&dir, keep).contains("house.ktx2"));
            let dir = case("empty");
            write(&dir, "dark.ktx2", Vec::new());
            assert!(refused_with(&dir, keep).contains("dark.ktx2"));
            let dir = case("gone");
            std::fs::remove_file(dir.join("basis7.ktx2")).unwrap();
            assert!(refused_with(&dir, keep).contains("basis7.ktx2"));
            // Cut short: the last level runs off the end.
            let dir = case("cut");
            let whole = solid(COLOR_SIZE, colour, 0, HOUSE);
            write(&dir, "house.ktx2", whole[..whole.len() - 1].to_vec());
            assert!(refused_with(&dir, keep).contains("house.ktx2"));
            // Supercompressed; a format packs do not use; no mips.
            let dir = case("zstd");
            let mut bytes = solid(COLOR_SIZE, colour, 0, HOUSE);
            bytes[44..48].copy_from_slice(&2u32.to_le_bytes());
            write(&dir, "house.ktx2", bytes);
            assert!(refused_with(&dir, keep).contains("supercompressed"));
            let dir = case("format");
            write(&dir, "house.ktx2", solid(COLOR_SIZE, None, 100, HOUSE));
            assert!(refused_with(&dir, keep).contains("vkFormat 100"));
            let dir = case("mips");
            let mut bytes = solid(COLOR_SIZE, colour, 0, HOUSE);
            bytes[40..44].copy_from_slice(&1u32.to_le_bytes());
            write(&dir, "house.ktx2", bytes);
            assert!(refused_with(&dir, keep).contains("full chain"));
        }
    }

    #[test]
    fn a_format_2_manifest_must_say_its_sizes_and_stay_within_them() {
        let dir = fixture2("f2manifest");
        let good = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        let cases = [
            good.replace("\"basis_size\": 8, ", ""),
            good.replace("\"basis_size\": 8", "\"basis_size\": 32"),
            good.replace("\"basis_size\": 8", "\"basis_size\": 6"),
            good.replace("\"basis_size\": 8", "\"basis_size\": 0"),
            good.replace("\"size\": 16", "\"size\": 8192"),
            good.replace("\"bytes\": ", "\"bytes_\": "),
            good.replace("\"format\": 2", "\"format\": 0"),
            good.replace("\"format\": 2", "\"format\": 4"),
            // Format 3 keeps its mesh and textures in seat_sets.
            good.replace("\"format\": 2", "\"format\": 3"),
        ];
        for (i, json) in cases.iter().enumerate() {
            std::fs::write(dir.join("manifest.json"), json).unwrap();
            assert!(
                load_with(&dir, false).is_err(),
                "case {i} was accepted: {json}"
            );
        }
        // The same manifest is fine as it was.
        std::fs::write(dir.join("manifest.json"), &good).unwrap();
        assert!(load_with(&dir, false).is_ok());
        // Format 1 ignores the format 2 fields.
        let dir = fixture("f1extra", "cinema-v2");
        let json = manifest_json("cinema-v2", 4).replace(
            "\"size\": 4,",
            "\"size\": 4, \"basis_size\": 2, \"bytes\": 7,",
        );
        std::fs::write(dir.join("manifest.json"), json).unwrap();
        let pack = load(&dir).expect("loads");
        assert_eq!((pack.baked.basis.size, pack.path), (4, TexturePath::Png));
    }

    #[test]
    fn a_format_1_pack_still_loads_as_one_level_per_texture() {
        let dir = fixture("f1", "cinema-v2");
        let pack = load_with(&dir, true).expect("loads");
        assert_eq!(pack.path, TexturePath::Png);
        let b = &pack.baked;
        assert_eq!((b.colors.levels(), b.basis.levels()), (1, 1));
        assert_eq!(b.colors.encoding, Encoding::Rgba8);
        // The GPU makes the other levels: 4 x 4 + 2 x 2 + 1 texels, about.
        assert_eq!(b.colors.gpu_bytes(), 2 * (64 + 64 / 3));
    }

    #[test]
    fn scan_offers_a_good_format_2_pack_and_leaves_out_a_broken_one() {
        let root = temp("f2scan");
        write_fixture2(&root.join("cinema-v2"), "cinema-v2");
        write_fixture2(&root.join("cinema-v3"), "cinema-v3");
        // Newer but broken in three ways, each caught without reading pixels.
        let broken = |name: &str| {
            write_fixture2(&root.join(name), name);
            root.join(name)
        };
        let dir = broken("cinema-v4");
        std::fs::write(dir.join("house.ktx2"), b"nope").unwrap();
        let dir = broken("cinema-v5");
        let mut bytes = solid(COLOR_SIZE, Some(((4, 4), true)), 0, DARK);
        bytes[44..48].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(dir.join("dark.ktx2"), bytes).unwrap();
        let dir = broken("cinema-v6");
        std::fs::write(
            dir.join("basis1.ktx2"),
            solid(BASIS_SIZE, Some(((6, 6), true)), 0, BLACK),
        )
        .unwrap();
        // The sizes of the files no longer add up to the manifest's bytes.
        let dir = broken("cinema-v7");
        std::fs::write(
            dir.join("basis1.ktx2"),
            solid(BASIS_SIZE, Some(((8, 8), false)), 0, BLACK),
        )
        .unwrap();
        let found = scan(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[&Place::Cinema].manifest.id, "cinema-v3");
        assert_eq!(found[&Place::Cinema].manifest.format, 2);
    }

    #[test]
    fn a_downloaded_pack_must_name_itself_and_read_whole() {
        let dir = temp("download-ok");
        write_fixture2(&dir, "campsite");
        assert_eq!(validate_download(&dir, "campsite").unwrap(), "1");
        assert_eq!(installed_version(&dir).as_deref(), Some("1"));
        // Another pack under the catalog's id is refused.
        let e = validate_download(&dir, "cinema-v2").unwrap_err().0;
        assert!(e.contains("calls itself"), "{e}");
        // A missing texture, a short mesh and a folder with no manifest too.
        std::fs::remove_file(dir.join("basis3.ktx2")).unwrap();
        assert!(validate_download(&dir, "campsite").is_err());
        let dir = temp("download-mesh");
        write_fixture2(&dir, "campsite");
        let mesh = std::fs::read(dir.join("mesh.bin")).unwrap();
        std::fs::write(dir.join("mesh.bin"), &mesh[..mesh.len() - 4]).unwrap();
        assert!(validate_download(&dir, "campsite").is_err());
        let empty = temp("download-empty");
        assert!(validate_download(&empty, "campsite").is_err());
        assert_eq!(installed_version(&empty), None);
    }

    // ---- Format 3: a mesh and textures for each seat ----

    /// Each seat's name, house colour and how far its mesh is moved along x
    /// (so that sets can be told apart by their mesh as well as by colour).
    const SEATS3: [(&str, [u8; 4], f32); 3] = [
        ("front", [200, 10, 10, 255], 0.0),
        ("middle", [10, 200, 10, 255], 1.0),
        ("back", [10, 10, 200, 255], 2.0),
    ];

    /// One seat's files, named for the seat; the bytes its textures add up to.
    fn write_set3(dir: &Path, seat: &str, house: [u8; 4], shift: f32) -> u64 {
        let moved: Vec<[f32; 5]> = VERTICES
            .iter()
            .map(|v| [v[0] + shift, v[1], v[2], v[3], v[4]])
            .collect();
        std::fs::write(
            dir.join(format!("{seat}-mesh.bin")),
            mesh_bytes(&moved, &INDICES),
        )
        .unwrap();
        let colour = Some(((4, 4), true));
        let light = Some(((6, 6), false));
        let mut total = 0;
        let mut put = |name: String, bytes: Vec<u8>| {
            total += bytes.len() as u64;
            std::fs::write(dir.join(name), bytes).unwrap();
        };
        put(
            format!("{seat}-house.ktx2"),
            solid(COLOR_SIZE, colour, 0, house),
        );
        put(
            format!("{seat}-dark.ktx2"),
            solid(COLOR_SIZE, colour, 0, DARK),
        );
        for i in 0..8 {
            let rgba = if i == 0 { WHITE } else { BLACK };
            put(
                format!("{seat}-basis{i}.ktx2"),
                solid(BASIS_SIZE, light, 0, rgba),
            );
        }
        total
    }

    fn set3_json(seat: &str, bytes: u64) -> String {
        let basis: Vec<String> = (0..8)
            .map(|i| format!("\"{seat}-basis{i}.ktx2\""))
            .collect();
        format!(
            r#""{seat}": {{
      "mesh": {{ "file": "{seat}-mesh.bin", "vertices": 5, "triangles": 4 }},
      "textures": {{ "size": {COLOR_SIZE}, "basis_size": {BASIS_SIZE}, "bytes": {bytes},
                     "house": "{seat}-house.ktx2", "dark": "{seat}-dark.ktx2",
                     "basis": [{}] }} }}"#,
            basis.join(", ")
        )
    }

    /// A format 3 manifest with a set for each of `sets` (seat, texture bytes).
    fn manifest3_json(id: &str, sets: &[(&str, u64)], default_seat: usize) -> String {
        let seats: Vec<String> = sets
            .iter()
            .map(|(s, _)| format!(r#"{{ "name": "{s}", "eye": [0, 3.0, 9.0] }}"#))
            .collect();
        let sets: Vec<String> = sets.iter().map(|(s, b)| set3_json(s, *b)).collect();
        format!(
            r#"{{ "format": 3, "id": "{id}", "name": "Test", "version": "1",
  "screen": {{ "centre": [0.0, 3.5, 0.0], "max": [9.0, 3.8] }},
  "seats": [{}], "default_seat": {default_seat},
  "seat_sets": {{ {} }}, "basis_gain": 1.0 }}"#,
            seats.join(", "),
            sets.join(", ")
        )
    }

    /// A good format 3 pack: front, middle and back, the middle the default.
    fn write_fixture3(dir: &Path, id: &str) {
        std::fs::create_dir_all(dir).expect("pack dir");
        let totals: Vec<(&str, u64)> = SEATS3
            .iter()
            .map(|&(seat, house, shift)| (seat, write_set3(dir, seat, house, shift)))
            .collect();
        std::fs::write(dir.join("manifest.json"), manifest3_json(id, &totals, 1))
            .expect("manifest");
    }

    fn fixture3(name: &str) -> PathBuf {
        let dir = temp(name);
        write_fixture3(&dir, "cinema-v2");
        dir
    }

    /// The manifest of `dir` edited by `edit` and parsed again.
    fn edited3(
        dir: &Path,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> Result<Manifest, PackError> {
        let bytes = std::fs::read(dir.join("manifest.json")).unwrap();
        let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        edit(&mut json);
        Manifest::parse(&serde_json::to_vec(&json).unwrap())
    }

    #[test]
    fn a_format_3_pack_loads_the_set_of_the_seat_asked_for() {
        let dir = fixture3("f3seats");
        for (seat, house, shift) in SEATS3 {
            let name = SeatName::from_id(seat);
            // Decoded on the CPU, and as the blocks the GPU takes.
            for keep in [false, true] {
                let pack = load_seat_with(&dir, name, keep).expect("loads");
                let b = &pack.baked;
                assert_eq!(b.seat, name, "{seat}");
                assert_eq!(pack.manifest.format, 3);
                assert_eq!(b.vertices[0].pos[0], -1.0 + shift, "{seat}'s own mesh");
                assert_eq!(b.indices, INDICES);
                assert_eq!(b.is_astc(), keep);
                let top = if keep {
                    let Encoding::Astc { block } = b.colors.encoding else {
                        panic!("not kept as ASTC");
                    };
                    let (texels, _) =
                        ktx2::decode_astc(&b.colors.layers[0][0], COLOR_SIZE, COLOR_SIZE, block)
                            .unwrap();
                    texels
                } else {
                    b.colors.layers[0][0].clone()
                };
                assert_eq!(&top[..4], &house, "{seat}'s own house texture");
                assert_eq!(b.basis.layers.len(), BASIS);
            }
        }
        // Which seat a set is for is kept when the blocks are decoded later.
        let kept = load_seat_with(&dir, Some(SeatName::Back), true).unwrap();
        let later = kept.baked.decoded().unwrap().expect("it was ASTC");
        assert_eq!(later.seat, Some(SeatName::Back));
    }

    #[test]
    fn a_format_3_pack_starts_with_its_default_seats_set() {
        let dir = fixture3("f3default");
        for pack in [
            load_with(&dir, false).unwrap(),
            load_seat_with(&dir, None, false).unwrap(),
            // A seat the pack has no set for is the default seat's too.
            load_seat_with(&dir, Some(SeatName::Couch), false).unwrap(),
        ] {
            assert_eq!(pack.baked.seat, Some(SeatName::Middle));
            assert_eq!(pack.baked.vertices[0].pos[0], 0.0);
            assert_eq!(&pack.baked.colors.layers[0][0][..4], &SEATS3[1].1);
        }
    }

    #[test]
    fn a_pack_with_one_set_ignores_the_seat_asked_for() {
        let dir = fixture2("f3one");
        for seat in [None, Some(SeatName::Front), Some(SeatName::Back)] {
            let pack = load_seat_with(&dir, seat, false).expect("loads");
            assert_eq!(pack.baked.seat, None);
            assert_eq!(&pack.baked.colors.layers[0][0][..4], &HOUSE);
        }
        let m = read_manifest(&dir).unwrap();
        assert_eq!(m.set_key(SeatName::Front), None);
        assert_eq!(m.set_keys(), [None]);
        let dir = fixture("f3one-png", "cinema-v2");
        let m = read_manifest(&dir).unwrap();
        assert_eq!(m.set_key(SeatName::Back), None);
        let pack = load_seat_with(&dir, Some(SeatName::Back), true).unwrap();
        assert_eq!(pack.baked.seat, None);
    }

    #[test]
    fn the_set_a_seat_draws_is_its_own_or_the_default_seats() {
        let dir = temp("f3keys");
        std::fs::write(
            dir.join("manifest.json"),
            manifest3_json("cinema-v2", &[("front", 1), ("middle", 1)], 1),
        )
        .unwrap();
        let m = read_manifest(&dir).expect("two seats, a set each");
        assert_eq!(m.set_key(SeatName::Front), Some(SeatName::Front));
        assert_eq!(m.set_key(SeatName::Middle), Some(SeatName::Middle));
        // No back seat: whoever sits there draws the default's.
        assert_eq!(m.set_key(SeatName::Back), Some(SeatName::Middle));
        assert_eq!(m.set_key(SeatName::Couch), Some(SeatName::Middle));
        assert_eq!(
            m.set_keys(),
            [Some(SeatName::Front), Some(SeatName::Middle)]
        );
        assert_eq!(m.resolve(None), Some(SeatName::Middle));
        assert_eq!(m.resolve(Some(SeatName::Front)), Some(SeatName::Front));
    }

    #[test]
    fn a_format_3_manifest_gives_every_seat_a_set_and_only_its_own() {
        use serde_json::json;
        let dir = fixture3("f3manifest");
        assert!(edited3(&dir, |_| {}).is_ok());
        let why = edited3(&dir, |v| {
            v["seat_sets"].as_object_mut().unwrap().remove("back");
        })
        .unwrap_err()
        .0;
        assert!(why.contains("back") && why.contains("seat_sets"), "{why}");
        // A set for a seat the pack does not have.
        let why = edited3(&dir, |v| {
            let front = v["seat_sets"]["front"].clone();
            v["seat_sets"]["couch"] = front;
        })
        .unwrap_err()
        .0;
        assert!(why.contains("couch"), "{why}");
        // The mesh and textures are in the sets, not beside them.
        for field in ["mesh", "textures"] {
            let why = edited3(&dir, |v| {
                v[field] = v["seat_sets"]["front"][field].clone();
            })
            .unwrap_err()
            .0;
            assert!(why.contains("seat_sets"), "{field}: {why}");
        }
        // A set that breaks a limit names its seat.
        let why = edited3(&dir, |v| {
            v["seat_sets"]["middle"]["mesh"]["triangles"] = json!(0);
        })
        .unwrap_err()
        .0;
        assert!(why.contains("middle") && why.contains("triangles"), "{why}");
        let why = edited3(&dir, |v| {
            v["seat_sets"]["front"]["textures"]["basis_size"] = json!(64);
        })
        .unwrap_err()
        .0;
        assert!(why.contains("front") && why.contains("basis size"), "{why}");
        let why = edited3(&dir, |v| {
            v["seat_sets"]["back"]["textures"]["house"] = json!("../house.ktx2");
        })
        .unwrap_err()
        .0;
        assert!(why.contains("back") && why.contains("file name"), "{why}");
        // Sets are for format 3 only, and no format is newer than this one.
        let why = edited3(&dir, |v| v["format"] = json!(2)).unwrap_err().0;
        assert!(why.contains("seat_sets needs format 3"), "{why}");
        assert!(edited3(&dir, |v| v["format"] = json!(4)).is_err());
        assert!(edited3(&dir, |v| v["seat_sets"] = json!({})).is_err());
        assert!(edited3(&dir, |v| v["seat_sets"] = json!(null)).is_err());
    }

    #[test]
    fn each_seats_set_has_the_texture_budget_not_the_whole_pack() {
        use serde_json::json;
        let dir = fixture3("f3budget");
        // Three sets that each fill the budget are fine: 96 MiB in all.
        let full = edited3(&dir, |v| {
            for seat in ["front", "middle", "back"] {
                v["seat_sets"][seat]["textures"]["bytes"] = json!(MAX_TEXTURE_BYTES);
            }
        })
        .expect("each set within the budget");
        assert_eq!(full.seat_sets.len(), 3);
        let why = edited3(&dir, |v| {
            v["seat_sets"]["back"]["textures"]["bytes"] = json!(MAX_TEXTURE_BYTES + 1);
        })
        .unwrap_err()
        .0;
        assert!(why.contains("back") && why.contains("at most"), "{why}");
    }

    #[test]
    fn a_seat_can_have_a_basis_gain_of_its_own() {
        use serde_json::json;
        let dir = fixture3("f3gain");
        let gain = |dir: &Path, seat| {
            let pack = load_seat_with(dir, Some(seat), false).expect("loads");
            pack.baked.basis_gain
        };
        // No set says: the pack's.
        for seat in [SeatName::Front, SeatName::Middle, SeatName::Back] {
            assert_eq!(gain(&dir, seat), 1.0);
        }
        let bytes = std::fs::read(dir.join("manifest.json")).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        manifest["seat_sets"]["front"]["basis_gain"] = json!(0.125);
        manifest["basis_gain"] = json!(0.0625);
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
        assert_eq!(gain(&dir, SeatName::Front), 0.125);
        assert_eq!(gain(&dir, SeatName::Middle), 0.0625);
        assert_eq!(gain(&dir, SeatName::Back), 0.0625);
        // A gain out of range is refused, whichever set it is in.
        for bad_gain in [json!(-1.0), json!(1e39), json!(1001.0)] {
            let why = edited3(&dir, |v| v["seat_sets"]["back"]["basis_gain"] = bad_gain)
                .unwrap_err()
                .0;
            assert!(why.contains("basis_gain"), "{why}");
        }
    }

    #[test]
    fn a_short_or_missing_seat_set_file_refuses_the_whole_pack() {
        let id = "cinema-v2";
        let spoiled = |what: &str, spoil: &dyn Fn(&Path)| {
            let root = temp(&format!("f3bad-{what}"));
            write_fixture3(&root.join(id), id);
            spoil(&root.join(id));
            root
        };
        let offered = |root: &Path| scan(root).contains_key(&Place::Cinema);
        let root = spoiled("good", &|_| {});
        assert!(offered(&root));
        // Each of these breaks one seat's set. The scan does not offer the
        // pack (the app shows the built-in room, as for any pack that is
        // broken) and a download of it is refused naming the seat.
        type Case<'a> = (&'a str, &'a str, &'a dyn Fn(&Path));
        let cases: [Case; 6] = [
            ("mesh-short", "back", &|d| {
                std::fs::write(d.join("back-mesh.bin"), b"LPM1").unwrap();
            }),
            ("mesh-gone", "front", &|d| {
                std::fs::remove_file(d.join("front-mesh.bin")).unwrap();
            }),
            ("tex-gone", "back", &|d| {
                std::fs::remove_file(d.join("back-basis5.ktx2")).unwrap();
            }),
            ("tex-cut", "middle", &|d| {
                let whole = std::fs::read(d.join("middle-house.ktx2")).unwrap();
                std::fs::write(d.join("middle-house.ktx2"), &whole[..whole.len() - 1]).unwrap();
            }),
            // Another size: the manifest's bytes no longer add up.
            ("tex-size", "front", &|d| {
                let other = solid(COLOR_SIZE, Some(((5, 5), true)), 0, DARK);
                std::fs::write(d.join("front-dark.ktx2"), other).unwrap();
            }),
            ("tex-notktx", "back", &|d| {
                std::fs::write(d.join("back-house.ktx2"), b"not a texture at all").unwrap();
            }),
        ];
        for (what, seat, spoil) in cases {
            let root = spoiled(what, spoil);
            assert!(!offered(&root), "{what}: the pack was offered");
            let why = validate_download(&root.join(id), id).unwrap_err().0;
            assert!(why.contains(&format!("seat {seat}")), "{what}: {why}");
            // And that seat's set does not load.
            let name = SeatName::from_id(seat);
            assert!(
                load_seat_with(&root.join(id), name, false).is_err(),
                "{what}"
            );
        }
    }

    #[test]
    fn scan_offers_a_good_format_3_pack_and_download_checks_every_set() {
        let root = temp("f3scan");
        write_fixture3(&root.join("cinema-v2"), "cinema-v2");
        let found = scan(&root);
        let cinema = &found[&Place::Cinema];
        assert_eq!(cinema.manifest.format, 3);
        assert_eq!(cinema.layout.seats.len(), 3);
        assert_eq!(cinema.layout.default_seat, 1);
        assert_eq!(
            validate_download(&root.join("cinema-v2"), "cinema-v2").unwrap(),
            "1"
        );
        // A format 2 pack beside it: the higher number wins, whichever format.
        write_fixture2(&root.join("cinema-v3"), "cinema-v3");
        assert_eq!(scan(&root)[&Place::Cinema].manifest.id, "cinema-v3");
    }

    #[test]
    fn the_log_names_the_path_taken() {
        assert_eq!(TexturePath::Png.to_string(), "PNG");
        assert_eq!(TexturePath::AstcOnGpu.to_string(), "ASTC on the GPU");
        assert_eq!(
            TexturePath::AstcDecoded { ms: 1234.4 }.to_string(),
            "ASTC decoded on the CPU in 1234 ms"
        );
    }
}
