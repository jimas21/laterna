//! The test card as the player shows it: a PNG to open like any picture
//! (`FRAMEPLAYERX_TESTCARD`), and the same card as a compositor layer
//! beside it to compare how the two are filtered
//! (`FRAMEPLAYERX_TESTCARD_LAYER`). The picture itself is
//! [`fp_render::testcard`].

use fp_core::view::ViewSettings;
use fp_render::testcard as pictures;
use glam::{Quat, Vec2, Vec3};
use std::path::PathBuf;

/// The heights the card is made in: 1080 is 1920 wide, 2160 is 3840.
pub const HEIGHTS: [u32; 2] = [1080, 2160];

/// How far to the right of where the head faces the layer hangs, in degrees.
/// The layer is the default screen's width, so this is a little more than
/// that screen's own angle (73.7 degrees at the default distance): the
/// layer sits beside the card shown in the eyes, not over half of it.
pub const LAYER_YAW_DEG: f32 = -77.0;

/// A card height from a switch's value: one of [`HEIGHTS`].
pub fn parse_height(value: Option<&str>) -> Option<u32> {
    let height = value?.trim().parse().ok()?;
    HEIGHTS.contains(&height).then_some(height)
}

/// The width of a card `height` tall (16:9).
pub fn width(height: u32) -> u32 {
    height / 9 * 16
}

/// Makes the card `height` tall and saves it as a PNG in the cache folder,
/// returning where. Opaque RGB, so a player sees a plain picture.
pub fn write_png(height: u32) -> Result<PathBuf, String> {
    if !HEIGHTS.contains(&height) {
        return Err(format!("the test card comes in {HEIGHTS:?}, not {height}"));
    }
    let w = width(height);
    let rgb: Vec<u8> = pictures::card(w, height)
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let dir = fp_core::dirs::cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("testcard-{height}p.png"));
    let img = image::RgbImage::from_raw(w, height, rgb).ok_or("bad test card size")?;
    img.save(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// How the card is also shown as a compositor layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerMode {
    /// One level: whatever the compositor does to minify it.
    Plain,
    /// The whole mip chain, from [`pictures::box_mips`].
    Mips,
}

impl LayerMode {
    pub fn parse(value: Option<&str>) -> Option<LayerMode> {
        match value?.trim() {
            "plain" => Some(LayerMode::Plain),
            "mips" => Some(LayerMode::Mips),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LayerMode::Plain => "plain",
            LayerMode::Mips => "mips",
        }
    }
}

/// The card as a compositor layer's pictures.
pub struct LayerCard {
    pub width: u32,
    pub height: u32,
    levels: Vec<(u32, u32, Vec<u8>)>,
}

impl LayerCard {
    /// The card `height` tall, with every mip level for [`LayerMode::Mips`].
    pub fn new(height: u32, mode: LayerMode) -> LayerCard {
        let width = width(height);
        let card = pictures::card(width, height);
        let levels = match mode {
            LayerMode::Plain => vec![(width, height, card)],
            LayerMode::Mips => pictures::box_mips(&card, width, height),
        };
        LayerCard {
            width,
            height,
            levels,
        }
    }

    /// The mip levels to ask the runtime for.
    pub fn mips(&self) -> u32 {
        self.levels.len() as u32
    }

    /// The first `granted` levels, for a swapchain that got no more.
    pub fn levels(&self, granted: u32) -> Vec<(u32, u32, &[u8])> {
        self.levels
            .iter()
            .take(granted.max(1) as usize)
            .map(|(w, h, px)| (*w, *h, px.as_slice()))
            .collect()
    }

    /// Its size in the world, in metres: the default screen's width.
    pub fn size(&self) -> Vec2 {
        let w = ViewSettings::default().screen_width;
        Vec2::new(w, w * self.height as f32 / self.width as f32)
    }
}

/// Where the layer hangs for a head at `head`: [`LAYER_YAW_DEG`] round from
/// where it faces, at the default screen's distance and the head's height,
/// upright and facing the head.
pub fn layer_pose(head: (Vec3, Quat)) -> (Vec3, Quat) {
    let (position, rotation) = head;
    let ahead = rotation * Vec3::NEG_Z;
    let facing = (-ahead.x).atan2(-ahead.z);
    let turn = Quat::from_rotation_y(facing + LAYER_YAW_DEG.to_radians());
    let distance = ViewSettings::default().screen_distance;
    (position + turn * Vec3::new(0.0, 0.0, -distance), turn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_are_the_two_sizes() {
        assert_eq!(parse_height(Some("1080")), Some(1080));
        assert_eq!(parse_height(Some(" 2160 ")), Some(2160));
        for bad in ["", "0", "1", "720", "2160p", "-1080", "abc"] {
            assert_eq!(parse_height(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(parse_height(None), None);
    }

    #[test]
    fn cards_are_sixteen_by_nine() {
        assert_eq!(width(1080), 1920);
        assert_eq!(width(2160), 3840);
    }

    #[test]
    fn layer_modes_parse_by_name() {
        assert_eq!(LayerMode::parse(Some("plain")), Some(LayerMode::Plain));
        assert_eq!(LayerMode::parse(Some(" mips ")), Some(LayerMode::Mips));
        for bad in ["", "1", "Mips", "both"] {
            assert_eq!(LayerMode::parse(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(LayerMode::parse(None), None);
        for mode in [LayerMode::Plain, LayerMode::Mips] {
            assert_eq!(LayerMode::parse(Some(mode.label())), Some(mode));
        }
    }

    #[test]
    fn a_plain_layer_has_one_level_and_a_mips_layer_the_chain() {
        let plain = LayerCard::new(1080, LayerMode::Plain);
        assert_eq!(plain.mips(), 1);
        assert_eq!((plain.width, plain.height), (1920, 1080));
        let mips = LayerCard::new(1080, LayerMode::Mips);
        assert_eq!(mips.mips(), pictures::mip_count(1920, 1080));
        // Level 0 is the same picture either way.
        assert!(plain.levels(1)[0].2 == mips.levels(1)[0].2);
    }

    #[test]
    fn a_swapchain_with_fewer_levels_gets_the_first_ones() {
        let card = LayerCard::new(1080, LayerMode::Mips);
        assert_eq!(card.levels(3).len(), 3);
        assert_eq!(card.levels(3)[2].0, 480);
        assert_eq!(card.levels(0).len(), 1, "never none");
        assert_eq!(card.levels(99).len() as u32, card.mips());
    }

    #[test]
    fn the_layer_is_the_default_screen_wide() {
        let size = LayerCard::new(1080, LayerMode::Plain).size();
        assert!((size.x - ViewSettings::default().screen_width).abs() < 1e-6);
        assert!((size.x / size.y - 16.0 / 9.0).abs() < 1e-4);
    }

    /// The angle from where the head faces to the layer, clockwise seen
    /// from above (to the right) positive, in degrees.
    fn clockwise_deg(head: (Vec3, Quat)) -> f32 {
        let (position, _) = layer_pose(head);
        let to = (position - head.0).normalize();
        let ahead = head.1 * Vec3::NEG_Z;
        let cross = ahead.cross(to).y;
        let along = ahead.dot(to);
        (-cross).atan2(along).to_degrees()
    }

    #[test]
    fn the_layer_hangs_to_the_right_at_the_screen_distance() {
        let distance = ViewSettings::default().screen_distance;
        for yaw in [0.0f32, 40.0, -120.0, 179.0] {
            let head = (
                Vec3::new(0.3, 1.6, -0.2),
                Quat::from_rotation_y(yaw.to_radians()),
            );
            let (position, _) = layer_pose(head);
            assert!(((position - head.0).length() - distance).abs() < 1e-4);
            assert!((position.y - head.0.y).abs() < 1e-5, "head height");
            let got = clockwise_deg(head);
            assert!((got + LAYER_YAW_DEG).abs() < 0.01, "yaw {yaw}: {got}");
        }
        // Positive is right of the head, as the constant's sign says.
        assert!(LAYER_YAW_DEG < 0.0);
    }

    #[test]
    fn the_layer_faces_the_head_and_stands_upright() {
        let head = (Vec3::new(0.0, 1.6, 0.0), Quat::from_rotation_y(0.7));
        let (position, rotation) = layer_pose(head);
        let normal = rotation * Vec3::Z;
        let to_head = (head.0 - position).normalize();
        assert!(normal.dot(to_head) > 0.9999, "{normal} vs {to_head}");
        assert!(normal.y.abs() < 1e-6, "upright");
        assert!((rotation * Vec3::Y).dot(Vec3::Y) > 0.9999);
    }

    #[test]
    fn the_layer_ignores_the_heads_pitch_and_roll() {
        let level = (Vec3::ZERO, Quat::from_rotation_y(0.4));
        let pitched = Quat::from_rotation_y(0.4) * Quat::from_rotation_x(-0.5);
        let tilted = (Vec3::ZERO, pitched * Quat::from_rotation_z(0.3));
        let (a, _) = layer_pose(level);
        let (b, _) = layer_pose(tilted);
        assert!((a - b).length() < 1e-3, "{a} vs {b}");
    }
}
