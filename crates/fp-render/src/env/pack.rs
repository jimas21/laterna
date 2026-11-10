//! Place packs: a room made elsewhere (modelled and lit in Blender, baked to
//! textures) that replaces one of the built-in rooms.
//!
//! A pack is a folder: `manifest.json`, `mesh.bin` and a set of PNGs, all
//! described in the format notes kept with the exporter. This module reads
//! one and checks every limit before anything is allocated or drawn: a pack
//! that breaks any of them is refused with a message (the caller logs a
//! warning and shows the built-in room), and nothing in here panics on any
//! bytes.
//!
//! Three steps, none of them touching the GPU:
//! - [`scan`] reads only the manifests (and file sizes) in a folder of packs,
//!   which is cheap enough for [`Place::layout`] to ask on any frame;
//! - [`load`] reads and decodes a whole pack, which takes a moment, so
//!   [`Place::build`] does it on the place builder's worker thread;
//! - the renderer uploads the result ([`Baked`]) and draws it with
//!   shaders/env_baked.wgsl.
//!
//! A pack replaces the built-in place its id names: `cinema` or `cinema-v2`
//! (any number) replaces the Cinema. Without a pack nothing changes.

use super::{Layout, Lighting, Place, PlaceData, PlaceKind, ScreenFrame, Seat, SeatName, Sky};
use glam::{Vec2, Vec3};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// The most triangles a pack's mesh has.
pub const MAX_TRIANGLES: u32 = 100_000;
/// The most vertices a pack's mesh has.
pub const MAX_VERTICES: u32 = 200_000;
/// The largest side of a pack's textures, in texels.
pub const MAX_TEXTURE: u32 = 4096;
/// Light patches of the screen, one basis texture each (`PATCHES` in
/// [`super::light`]).
pub const BASIS: usize = 8;
/// The format number this build reads.
pub const FORMAT: u32 = 1;

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
    pub size: u32,
    pub house: String,
    pub dark: String,
    pub basis: Vec<String>,
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
    pub mesh: MeshSpec,
    pub textures: TextureSpec,
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
        if self.format != FORMAT {
            return bad(format!(
                "format {} (this build reads {FORMAT})",
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
        let mesh = &self.mesh;
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
        let t = &self.textures;
        if t.size == 0 || t.size > MAX_TEXTURE || !t.size.is_power_of_two() {
            return bad(format!(
                "texture size {} (a power of two up to {MAX_TEXTURE} wanted)",
                t.size
            ));
        }
        if t.basis.len() != BASIS {
            return bad(format!("{} basis textures ({BASIS} wanted)", t.basis.len()));
        }
        for name in [&t.house, &t.dark].into_iter().chain(&t.basis) {
            if !plain_name(name) {
                return bad(format!("texture file name {name:?}"));
            }
        }
        if !(self.basis_gain.is_finite() && (0.0..=1000.0).contains(&self.basis_gain)) {
            return bad(format!("basis_gain {} is out of range", self.basis_gain));
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

    /// The bytes `mesh.bin` must have.
    fn mesh_len(&self) -> u64 {
        HEADER as u64
            + VERTEX_BYTES as u64 * u64::from(self.mesh.vertices)
            + 4 * 3 * u64::from(self.mesh.triangles)
    }

    fn texture_files(&self) -> impl Iterator<Item = &String> {
        [&self.textures.house, &self.textures.dark]
            .into_iter()
            .chain(&self.textures.basis)
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

/// A pack's mesh and decoded textures, ready to upload.
pub struct Baked {
    pub vertices: Vec<BakedVertex>,
    /// Triangles.
    pub indices: Vec<u32>,
    /// Side of every texture, texels.
    pub size: u32,
    /// RGBA8, `size` x `size`, the colour as stored (sRGB for the house and
    /// dark textures, square-root coded for the basis ones).
    pub house: Vec<u8>,
    pub dark: Vec<u8>,
    pub basis: Vec<Vec<u8>>,
    pub basis_gain: f32,
}

impl fmt::Debug for Baked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Baked")
            .field("vertices", &self.vertices.len())
            .field("indices", &self.indices.len())
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// A pack read in full.
#[derive(Debug)]
pub struct Pack {
    pub manifest: Manifest,
    pub baked: Baked,
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
pub fn parse_mesh(bytes: &[u8], m: &Manifest) -> Result<(Vec<BakedVertex>, Vec<u32>), PackError> {
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
    if vc != m.mesh.vertices || u64::from(ic) != 3 * u64::from(m.mesh.triangles) {
        return bad(format!(
            "mesh says {vc} vertices and {ic} indices, the manifest {} and {}",
            m.mesh.vertices,
            3 * u64::from(m.mesh.triangles)
        ));
    }
    if bytes.len() as u64 != m.mesh_len() {
        return bad(format!(
            "mesh is {} bytes, its counts need {}",
            bytes.len(),
            m.mesh_len()
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

/// Reads the whole pack in `dir`, or says what is wrong with it. Takes a
/// moment (the textures are decoded): not for the render thread.
pub fn load(dir: &Path) -> Result<Pack, PackError> {
    let manifest = read_manifest(dir)?;
    let mesh_path = dir.join(&manifest.mesh.file);
    let bytes = read_capped(&mesh_path, manifest.mesh_len(), "mesh")?;
    let (vertices, indices) = parse_mesh(&bytes, &manifest)?;
    drop(bytes);
    let t = &manifest.textures;
    let decode = |name: &String| decode_png(&dir.join(name), t.size);
    let house = decode(&t.house)?;
    let dark = decode(&t.dark)?;
    let basis = t.basis.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
    let baked = Baked {
        vertices,
        indices,
        size: t.size,
        house,
        dark,
        basis,
        basis_gain: manifest.basis_gain,
    };
    Ok(Pack { manifest, baked })
}

/// What [`scan`] needs of a pack to offer it: the manifest is sound and the
/// files are there at the sizes it names. The pixels are not read.
fn preflight(dir: &Path, m: &Manifest) -> Result<(), PackError> {
    let mesh = dir.join(&m.mesh.file);
    let len = std::fs::metadata(&mesh)
        .map_err(|e| io("mesh", &mesh, &e))?
        .len();
    if len != m.mesh_len() {
        return bad(format!(
            "mesh is {len} bytes, its counts need {}",
            m.mesh_len()
        ));
    }
    for name in m.texture_files() {
        let path = dir.join(name);
        let (w, h) = png_size(&path)?;
        if (w, h) != (m.textures.size, m.textures.size) {
            return bad(format!("texture {name} is {w} x {h}"));
        }
    }
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
    for dir in dirs.into_iter().filter(|d| d.is_dir()) {
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
        let found = scan(&root);
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
pub fn load_replacement(place: Place) -> Option<PlaceData> {
    let r = replacement(place)?;
    match load(&r.dir) {
        Ok(pack) => {
            log::info!(
                "loaded place pack {}: {} triangles, {} px textures",
                r.manifest.id,
                pack.baked.indices.len() / 3,
                pack.baked.size
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
        assert_eq!((b.vertices.len(), b.indices.len(), b.size), (5, 12, 4));
        assert_eq!(b.vertices[4].pos, [0.0, 1.0, -3.0]);
        assert_eq!(b.indices, INDICES);
        assert_eq!(b.house.len(), 4 * 4 * 4);
        assert_eq!(&b.house[..4], &[200, 120, 40, 255]);
        assert_eq!(b.basis.len(), BASIS);
        assert_eq!(&b.basis[0][..4], &[255, 255, 255, 255]);
        assert_eq!(&b.basis[3][..4], &[0, 0, 0, 255]);
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
        assert_eq!(&pack.baked.dark[..4], &[9, 8, 7, 255]);
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
            good.replace("\"format\": 1", "\"format\": 2"),
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
        let m = read_manifest(&dir).expect("manifest");
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
}
