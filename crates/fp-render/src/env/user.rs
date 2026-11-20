//! The person's own places: an `environments` folder beside the settings
//! file. Two kinds of thing in it become places, listed in the picker as
//! "Yours":
//!
//! - an equirectangular picture (a 2:1 JPEG or PNG, a 360 photo) becomes a
//!   sky place named after the file: the picture all round the viewer, with
//!   no room and no floor, and the screen wherever the screen settings put
//!   it. It is drawn the way a baked room is (a sphere mesh and two
//!   textures: lights up, and half as bright for the lights down), so the
//!   renderer needs nothing of its own for it;
//! - a folder holding a place pack is read by [`super::pack`] like any
//!   installed pack (`manifest.json`, its limits, every file at its size).
//!   One that passes is offered as a room of its own, under its own name; one
//!   that does not is left out with a line in the log. Unlike a pack in the
//!   places folder it replaces nothing.
//!
//! The folder is read once, the first time anyone asks, like the installed
//! packs: a file put there while the app runs shows up on the next start. At
//! most [`MAX_PLACES`] are offered. Nothing in here touches the GPU, and
//! nothing panics on any bytes: a picture or pack that cannot be used is
//! skipped with a message.

use super::ktx2::Encoding;
use super::pack::{self, BASIS, Baked, BakedVertex, Replacement, TextureArray};
use super::{Layout, PlaceData, PlaceKind, SeatName, Sky, studio};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// The most places the folder gives the picker.
pub const MAX_PLACES: usize = 16;
/// Overrides the folder (for tests and the desktop preview).
pub const DIR_ENV: &str = "LATERNA_ENVIRONMENTS_DIR";
/// Every user place's id starts with this, so it cannot be a built-in's.
pub const ID_PREFIX: &str = "user-";

/// A picture file bigger than this is refused unread.
const MAX_IMAGE_FILE: u64 = 96 << 20;
/// The widest picture read: 8192 x 4096 is 100 MB as RGB.
pub const MAX_WIDTH: u32 = 8192;
/// The narrowest picture that looks like anything all round.
pub const MIN_WIDTH: u32 = 512;
/// The side of the square textures the picture is stored in, at most.
const MAX_TEXTURE: u32 = 4096;
/// The sky sphere's radius, metres: further than any screen, nearer than the
/// far plane.
const SKY_RADIUS: f32 = 50.0;
/// How bright the sky is with the house lights down, against up: the same
/// halving the studio's gradient has.
const DARK_LEVEL: f32 = 0.5;

/// What a user place is made from.
#[derive(Clone, Debug)]
pub enum Source {
    /// A 2:1 picture, as found: its size is from the header.
    Image {
        path: PathBuf,
        width: u32,
        height: u32,
    },
    /// A place pack folder that passed the pack checks.
    Pack(Arc<Replacement>),
}

/// One place in the folder.
#[derive(Clone, Debug)]
pub struct UserPlace {
    /// `user-` and the file's name in lowercase words joined by hyphens.
    pub id: String,
    /// The file's (or folder's) name as the person wrote it, without the
    /// extension and with spaces for underscores.
    pub name: String,
    pub source: Source,
}

/// The folder the places are looked for in.
pub fn dir() -> PathBuf {
    match std::env::var_os(DIR_ENV).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => fp_core::dirs::config_dir().join("environments"),
    }
}

static REGISTRY: OnceLock<Vec<UserPlace>> = OnceLock::new();

/// The places in [`dir`], read at the first ask (none in this crate's own
/// tests, whatever is on the machine running them).
pub fn registry() -> &'static [UserPlace] {
    REGISTRY.get_or_init(|| {
        if cfg!(test) {
            return Vec::new();
        }
        let found = scan_dir(&dir());
        for p in &found {
            log::info!("your place {} ({})", p.id, p.name);
        }
        found
    })
}

/// The index of the place with this id, if the folder has it.
pub fn index_of(id: &str) -> Option<u8> {
    registry()
        .iter()
        .position(|p| p.id == id)
        .and_then(|i| u8::try_from(i).ok())
}

/// A file name as the person sees it: the name without its extension, with
/// underscores as spaces and the spaces tidied, at most 60 characters.
pub fn display_name(stem: &str) -> String {
    let spaced: String = stem
        .chars()
        .map(|c| if c == '_' || c.is_control() { ' ' } else { c })
        .collect();
    let words: Vec<&str> = spaced.split_whitespace().collect();
    words.join(" ").chars().take(60).collect()
}

/// The part of a place's id made from its name: lowercase letters and
/// digits, words joined by single hyphens, at most 40 characters; `place`
/// when the name has none.
pub fn slug(stem: &str) -> String {
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.chars().take(40).collect();
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "place".to_string()
    } else {
        out.to_string()
    }
}

fn unique_id(stem: &str, taken: &[UserPlace]) -> String {
    let base = format!("{ID_PREFIX}{}", slug(stem));
    let mut id = base.clone();
    let mut n = 2;
    while taken.iter().any(|p| p.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Whether `path` names a picture by its extension.
fn is_picture_name(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["jpg", "jpeg", "png"].contains(&e.to_ascii_lowercase().as_str()))
}

fn reader(path: &Path) -> Result<image::ImageReader<std::io::BufReader<std::fs::File>>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("cannot open: {e}"))?;
    image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .map_err(|e| format!("cannot read: {e}"))
}

/// Checks a picture from its header, without decoding it: a JPEG or PNG (by
/// its bytes, not its name), twice as wide as high, between [`MIN_WIDTH`] and
/// [`MAX_WIDTH`] wide. Returns the size, or what is wrong.
pub fn inspect_image(path: &Path) -> Result<(u32, u32), String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("cannot look at it: {e}"))?
        .len();
    if len > MAX_IMAGE_FILE {
        return Err(format!("{len} bytes is too big (at most {MAX_IMAGE_FILE})"));
    }
    let r = reader(path)?;
    if !matches!(
        r.format(),
        Some(image::ImageFormat::Jpeg | image::ImageFormat::Png)
    ) {
        return Err("not a JPEG or PNG picture".into());
    }
    let (w, h) = r
        .into_dimensions()
        .map_err(|e| format!("not a picture: {e}"))?;
    if h == 0 || (f64::from(w) / f64::from(h) - 2.0).abs() > 0.04 {
        return Err(format!(
            "{w} x {h} is not 2:1 (a 360 picture is twice as wide as high)"
        ));
    }
    if !(MIN_WIDTH..=MAX_WIDTH).contains(&w) {
        return Err(format!(
            "{w} pixels wide ({MIN_WIDTH} to {MAX_WIDTH} are used)"
        ));
    }
    Ok((w, h))
}

/// Everything in `root` that makes a place: pictures and pack folders, by
/// name (case does not matter), at most [`MAX_PLACES`]. What cannot be used
/// is skipped with a line in the log.
pub fn scan_dir(root: &Path) -> Vec<UserPlace> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut items: Vec<(String, PathBuf, bool)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // A link is never followed: file_type() does not.
            let kind = e.file_type().ok()?;
            let hidden = name.starts_with('.');
            (!hidden && (kind.is_file() || kind.is_dir())).then(|| (name, e.path(), kind.is_dir()))
        })
        .collect();
    items.sort_by_key(|(name, _, _)| (name.to_lowercase(), name.clone()));
    let mut found: Vec<UserPlace> = Vec::new();
    for (name, path, is_dir) in items {
        if found.len() >= MAX_PLACES {
            log::warn!("your places: only the first {MAX_PLACES} are offered");
            break;
        }
        if is_dir {
            // A folder with no manifest is not a pack: not worth a warning.
            if !path.join("manifest.json").is_file() {
                continue;
            }
            match pack::check_folder(&path) {
                Ok(r) => {
                    let shown = if r.manifest.name.trim().is_empty() {
                        display_name(&name)
                    } else {
                        display_name(&r.manifest.name)
                    };
                    found.push(UserPlace {
                        id: unique_id(&name, &found),
                        name: shown,
                        source: Source::Pack(Arc::new(r)),
                    });
                }
                Err(e) => log::warn!("your place {name} is not used: {e}"),
            }
        } else if is_picture_name(&path) {
            let stem = path
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            match inspect_image(&path) {
                Ok((width, height)) => found.push(UserPlace {
                    id: unique_id(&stem, &found),
                    name: display_name(&stem),
                    source: Source::Image {
                        path,
                        width,
                        height,
                    },
                }),
                Err(e) => log::warn!("your picture {name} is not used: {e}"),
            }
        }
    }
    found
}

impl UserPlace {
    /// A picture is a sky; a pack is a room.
    pub fn kind(&self) -> PlaceKind {
        match self.source {
            Source::Image { .. } => PlaceKind::Sky,
            Source::Pack(_) => PlaceKind::Room,
        }
    }

    /// The seats and screen: a sky's one seat at the origin, or the pack's.
    pub fn layout(&self) -> Layout {
        match &self.source {
            Source::Image { .. } => studio::layout(),
            Source::Pack(r) => r.layout.clone(),
        }
    }

    /// The set of a pack's textures a viewer in `seat` draws; `None` for a
    /// picture and for a pack with one set for everybody.
    pub fn seat_set(&self, seat: SeatName) -> Option<SeatName> {
        match &self.source {
            Source::Image { .. } => None,
            Source::Pack(r) => r.manifest.set_key(seat),
        }
    }

    /// Reads and builds the place: decoding takes a moment, so this is for a
    /// worker thread. `set` is as for [`super::Place::build_set`].
    pub fn build(&self, set: Option<SeatName>) -> Result<PlaceData, String> {
        match &self.source {
            Source::Image { path, .. } => sky_from_picture(path),
            Source::Pack(r) => pack::load_seat(&r.dir, set)
                .map(pack::Pack::into_place_data)
                .map_err(|e| e.to_string()),
        }
    }
}

/// The sky place for the equirectangular picture at `path`.
pub fn sky_from_picture(path: &Path) -> Result<PlaceData, String> {
    let (w, h) = inspect_image(path)?;
    let mut r = reader(path)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_WIDTH);
    limits.max_image_height = Some(MAX_WIDTH / 2 + 256);
    limits.max_alloc = Some(512 << 20);
    r.limits(limits);
    let rgb = r
        .decode()
        .map_err(|e| format!("cannot decode: {e}"))?
        .into_rgb8();
    if rgb.dimensions() != (w, h) {
        return Err("the picture changed while it was read".into());
    }
    let size = texture_size(w);
    let lit = to_square(rgb.as_raw(), w, h, size);
    let dark = dimmed(&lit);
    let (vertices, indices) = sphere(SKY_RADIUS, 32, 96);
    let layer = |bytes: Vec<u8>| vec![bytes];
    let basis_size = 4u32;
    let black: Vec<u8> = [0u8, 0, 0, 255].repeat((basis_size * basis_size) as usize);
    let baked = Baked {
        vertices,
        indices,
        colors: TextureArray {
            size,
            encoding: Encoding::Rgba8,
            layers: vec![layer(lit), layer(dark)],
        },
        basis: TextureArray {
            size: basis_size,
            encoding: Encoding::Rgba8,
            layers: (0..BASIS).map(|_| layer(black.clone())).collect(),
        },
        // No picture light: the sky is not lit by the screen.
        basis_gain: 0.0,
        seat: None,
    };
    let mut data = pack::sky_place_data(baked);
    data.sky = Sky::None;
    Ok(data)
}

/// The side of the stored square texture for a picture `width` wide: the
/// largest power of two not above the width, 512 to 4096.
pub fn texture_size(width: u32) -> u32 {
    let w = width.clamp(MIN_WIDTH, MAX_TEXTURE);
    1 << w.ilog2()
}

/// The range of source pixels (of `n`) that output pixel `i` (of `size`)
/// covers: at least one.
fn span(i: u32, size: u32, n: u32) -> (usize, usize) {
    let (i, size, n) = (u64::from(i), u64::from(size), u64::from(n));
    let a = i * n / size;
    let b = ((i + 1) * n).div_ceil(size).clamp(a + 1, n);
    (a as usize, b as usize)
}

/// A `w` x `h` RGB picture as a `size` x `size` RGBA8 texture, the whole
/// picture stretched over the square (the sphere's texture coordinates run
/// from 0 to 1 across both ways). Where the picture is bigger than the
/// square the texels it covers are averaged.
pub fn to_square(rgb: &[u8], w: u32, h: u32, size: u32) -> Vec<u8> {
    let columns: Vec<(usize, usize)> = (0..size).map(|x| span(x, size, w)).collect();
    let mut out = Vec::with_capacity((size as usize).pow(2) * 4);
    for y in 0..size {
        let (y0, y1) = span(y, size, h);
        for &(x0, x1) in &columns {
            let mut sum = [0u32; 3];
            for row in y0..y1 {
                let base = row * w as usize;
                for x in x0..x1 {
                    let at = (base + x) * 3;
                    for (s, v) in sum.iter_mut().zip(&rgb[at..at + 3]) {
                        *s += u32::from(*v);
                    }
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(sum.map(|s| ((s + count / 2) / count) as u8));
            out.push(255);
        }
    }
    out
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// The RGBA8 texels at [`DARK_LEVEL`] of their light (in linear light, then
/// back to sRGB): the picture with the house lights down.
pub fn dimmed(rgba: &[u8]) -> Vec<u8> {
    let table: Vec<u8> = (0..=255u8)
        .map(|v| {
            let lin = srgb_to_linear(f32::from(v) / 255.0) * DARK_LEVEL;
            (linear_to_srgb(lin) * 255.0).round().clamp(0.0, 255.0) as u8
        })
        .collect();
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            [
                table[p[0] as usize],
                table[p[1] as usize],
                table[p[2] as usize],
                p[3],
            ]
        })
        .collect()
}

/// A sphere of `radius` round the origin with an equirectangular picture's
/// texture coordinates: the picture's middle straight ahead (-z), its right
/// edge towards +x, its top straight up. `rings` rows of quads from the top
/// to the bottom and `segments` round; the seam has its own column of
/// vertices.
pub fn sphere(radius: f32, rings: u32, segments: u32) -> (Vec<BakedVertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(((rings + 1) * (segments + 1)) as usize);
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let lat = std::f32::consts::FRAC_PI_2 - v * std::f32::consts::PI;
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            let lon = (u - 0.5) * std::f32::consts::TAU;
            vertices.push(BakedVertex {
                pos: [
                    radius * lon.sin() * lat.cos(),
                    radius * lat.sin(),
                    -radius * lon.cos() * lat.cos(),
                ],
                uv: [u, v],
            });
        }
    }
    let stride = segments + 1;
    let mut indices = Vec::with_capacity((rings * segments * 6) as usize);
    for r in 0..rings {
        for s in 0..segments {
            let a = r * stride + s;
            let (b, c, d) = (a + 1, a + stride, a + stride + 1);
            indices.extend([a, c, b, b, c, d]);
        }
    }
    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("laterna-user-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn png(path: &Path, w: u32, h: u32, colour: [u8; 3]) {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb(colour));
        img.save_with_format(path, image::ImageFormat::Png).unwrap();
    }

    fn jpg(path: &Path, w: u32, h: u32) {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([90, 120, 200]));
        img.save_with_format(path, image::ImageFormat::Jpeg)
            .unwrap();
    }

    #[test]
    fn a_name_is_the_file_name_in_words() {
        assert_eq!(display_name("my_forest  at dawn"), "my forest at dawn");
        assert_eq!(display_name("  __Beach__ "), "Beach");
        assert_eq!(display_name(&"x".repeat(100)).chars().count(), 60);
        assert_eq!(slug("My Forest, at Dawn!"), "my-forest-at-dawn");
        assert_eq!(slug("---"), "place");
        assert_eq!(slug("Wälder 360"), "w-lder-360");
        assert!(slug(&"ab ".repeat(40)).len() <= 40);
        assert!(!slug(&"ab ".repeat(40)).ends_with('-'));
    }

    #[test]
    fn pictures_in_the_folder_become_sky_places_named_after_the_file() {
        let dir = temp("scan");
        png(&dir.join("Mountain_lake.png"), 1024, 512, [10, 20, 30]);
        jpg(&dir.join("beach.JPG"), 1024, 512);
        let found = scan_dir(&dir);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["beach", "Mountain lake"]);
        let ids: Vec<&str> = found.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["user-beach", "user-mountain-lake"]);
        for p in &found {
            assert_eq!(p.kind(), PlaceKind::Sky);
            assert!(p.layout().screen.is_none());
            assert_eq!(p.layout().seats.len(), 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_files_with_the_same_name_get_their_own_ids() {
        let dir = temp("same");
        png(&dir.join("sea.png"), 1024, 512, [1, 2, 3]);
        jpg(&dir.join("sea.jpg"), 1024, 512);
        let found = scan_dir(&dir);
        let mut ids: Vec<&str> = found.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["user-sea", "user-sea-2"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_files_are_left_out() {
        let dir = temp("bad");
        png(&dir.join("good.png"), 1024, 512, [1, 2, 3]);
        // Not 2:1, too small, a text file named like a picture, a picture
        // named like text, a hidden file, a folder with nothing in it.
        png(&dir.join("square.png"), 512, 512, [1, 2, 3]);
        png(&dir.join("tiny.png"), 256, 128, [1, 2, 3]);
        std::fs::write(dir.join("fake.png"), b"not a picture at all").unwrap();
        std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
        png(&dir.join(".hidden.png"), 1024, 512, [1, 2, 3]);
        std::fs::create_dir(dir.join("empty")).unwrap();
        std::fs::write(dir.join("empty/readme.txt"), b"x").unwrap();
        // A GIF renamed to .png is not a JPEG or PNG.
        std::fs::write(dir.join("gif.png"), b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
        let found = scan_dir(&dir);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["good"]);
        assert!(
            inspect_image(&dir.join("square.png"))
                .unwrap_err()
                .contains("2:1")
        );
        assert!(
            inspect_image(&dir.join("tiny.png"))
                .unwrap_err()
                .contains("wide")
        );
        assert!(inspect_image(&dir.join("fake.png")).is_err());
        assert!(inspect_image(&dir.join("missing.png")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pack_folder_is_checked_like_any_pack_and_a_bad_one_is_skipped() {
        let dir = temp("pack");
        // No manifest: not a pack. A manifest that is not usable: refused.
        std::fs::create_dir(dir.join("junk")).unwrap();
        std::fs::create_dir(dir.join("broken")).unwrap();
        std::fs::write(dir.join("broken/manifest.json"), b"{ not json").unwrap();
        png(&dir.join("sky.png"), 1024, 512, [5, 5, 5]);
        let found = scan_dir(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "sky");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_more_than_sixteen_places_are_offered() {
        let dir = temp("many");
        for n in 0..20 {
            png(&dir.join(format!("p{n:02}.png")), 512, 256, [n as u8, 0, 0]);
        }
        assert_eq!(scan_dir(&dir).len(), MAX_PLACES);
        assert!(scan_dir(&dir.join("nowhere")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_builds_a_sphere_sky_with_a_lit_and_a_dim_texture() {
        let dir = temp("build");
        let file = dir.join("sky.png");
        png(&file, 1024, 512, [200, 100, 50]);
        let place = scan_dir(&dir).remove(0);
        let data = place.build(None).unwrap();
        assert!(data.vertices.is_empty() && data.palette.is_empty());
        let b = data.baked.expect("drawn as a baked place");
        assert_eq!(b.colors.size, 1024);
        assert_eq!(b.colors.layers.len(), 2);
        assert_eq!(b.basis.layers.len(), BASIS);
        assert_eq!(b.basis_gain, 0.0);
        let (lit, dark) = (&b.colors.layers[0][0], &b.colors.layers[1][0]);
        assert_eq!(lit.len(), 1024 * 1024 * 4);
        assert_eq!(&lit[..4], &[200, 100, 50, 255]);
        assert!(dark[0] < lit[0] && dark[1] < lit[1] && dark[2] < lit[2]);
        assert_eq!(dark[3], 255);
        // Every vertex is on the sphere, and every index is in range.
        for v in &b.vertices {
            assert!((Vec3::from(v.pos).length() - SKY_RADIUS).abs() < 1e-3);
            assert!((0.0..=1.0).contains(&v.uv[0]) && (0.0..=1.0).contains(&v.uv[1]));
        }
        assert!(b.indices.len().is_multiple_of(3));
        assert!(b.indices.iter().all(|&i| (i as usize) < b.vertices.len()));
        // A broken file fails with a reason, never a panic.
        std::fs::write(&file, b"\x89PNG\r\n\x1a\nbroken").unwrap();
        assert!(sky_from_picture(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_picture_is_laid_the_way_round_a_viewer_expects() {
        let (v, _) = sphere(1.0, 4, 8);
        let at = |u: f32, vv: f32| {
            v.iter()
                .find(|p| (p.uv[0] - u).abs() < 1e-6 && (p.uv[1] - vv).abs() < 1e-6)
                .map(|p| Vec3::from(p.pos))
                .unwrap()
        };
        // Middle of the picture: straight ahead, -z. Right edge-ward: +x.
        assert!((at(0.5, 0.5) - Vec3::NEG_Z).length() < 1e-5);
        assert!(at(0.75, 0.5).x > 0.99);
        assert!(at(0.25, 0.5).x < -0.99);
        // Top row: straight up; bottom: down.
        assert!(at(0.5, 0.0).y > 0.99 && at(0.5, 1.0).y < -0.99);
    }

    #[test]
    fn the_stored_texture_is_as_big_as_the_picture_up_to_4096() {
        assert_eq!(texture_size(512), 512);
        assert_eq!(texture_size(1000), 512);
        assert_eq!(texture_size(2048), 2048);
        assert_eq!(texture_size(4096), 4096);
        assert_eq!(texture_size(8192), 4096);
    }

    #[test]
    fn stretching_to_a_square_averages_what_it_covers() {
        // 4 x 2 picture: left half black, right half white, to a 2 x 2.
        let mut rgb = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 0 } else { 255 };
                rgb.extend([v, v, v]);
            }
        }
        let out = to_square(&rgb, 4, 2, 2);
        assert_eq!(
            out,
            [
                0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255
            ]
        );
        // A mix averages: black and white columns into one texel.
        let one = to_square(&rgb, 4, 2, 1);
        assert_eq!(&one[..3], &[128, 128, 128]);
    }

    #[test]
    fn dimming_halves_the_light_and_keeps_black_and_alpha() {
        let out = dimmed(&[0, 0, 0, 77, 255, 255, 255, 255]);
        assert_eq!(&out[..4], &[0, 0, 0, 77]);
        // Half the linear light of white is sRGB 188.
        assert!((i32::from(out[4]) - 188).abs() <= 1, "{}", out[4]);
        assert_eq!(out[7], 255);
    }
}
