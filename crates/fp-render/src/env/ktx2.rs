//! The textures of a format 2 place pack: KTX2 files holding one 2D image
//! with a full mip chain, ASTC-compressed (or plain RGBA8), and a decoder
//! that turns the ASTC levels into RGBA8 for devices that cannot sample them.
//!
//! Only what packs need is read: no supercompression, no arrays, no cube
//! faces, no 3D. Every length is checked against the dimensions before any
//! data is sliced, and nothing here panics on any bytes (the one library call
//! that could on a corrupt block is wrapped).

use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The bytes before the level index (it starts at byte 80): the identifier,
/// the nine header words,
/// the data-format and key-value ranges, the supercompression-data range.
const INDEX_START: usize = 12 + 36 + 16 + 16;
/// One level index entry: offset, length, length uncompressed (each u64).
const LEVEL_ENTRY: usize = 24;
/// Levels of the largest texture a pack may have (4096 down to 1).
const MAX_LEVELS: usize = 13;
/// How many leading bytes of a file [`parse`] needs to read the header and
/// the whole level index.
pub const HEAD_BYTES: usize = INDEX_START + LEVEL_ENTRY * MAX_LEVELS;

const IDENTIFIER: [u8; 12] = [
    0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];

const VK_R8G8B8A8_UNORM: u32 = 37;
const VK_R8G8B8A8_SRGB: u32 = 43;
/// `VK_FORMAT_ASTC_4x4_UNORM_BLOCK`; each block size has the UNORM format at
/// 157 + 2 i and the sRGB one right after it.
const VK_ASTC_FIRST: u32 = 157;
/// The ASTC LDR block sizes, in the order Vulkan numbers their formats.
pub const ASTC_BLOCKS: [(u32, u32); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

/// How a texture's texels are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Four bytes a texel.
    Rgba8,
    /// 16-byte blocks of `block` texels.
    Astc { block: (u32, u32) },
}

impl Encoding {
    /// The bytes of a level `width` x `height` texels.
    pub fn level_bytes(self, width: u32, height: u32) -> u64 {
        let (w, h) = (u64::from(width), u64::from(height));
        match self {
            Encoding::Rgba8 => w * h * 4,
            Encoding::Astc { block: (bx, by) } => {
                w.div_ceil(u64::from(bx)) * h.div_ceil(u64::from(by)) * 16
            }
        }
    }
}

/// The Vulkan format number (`VkFormat`) of ASTC `block` texels, sRGB or
/// UNORM, or `None` for a size ASTC does not have.
pub fn astc_vk_format(block: (u32, u32), srgb: bool) -> Option<u32> {
    let i = ASTC_BLOCKS.iter().position(|b| *b == block)?;
    Some(VK_ASTC_FIRST + 2 * i as u32 + u32::from(srgb))
}

/// What a `vkFormat` number is, and whether it is the sRGB variant.
fn format_of(vk_format: u32) -> Option<(Encoding, bool)> {
    match vk_format {
        VK_R8G8B8A8_UNORM => Some((Encoding::Rgba8, false)),
        VK_R8G8B8A8_SRGB => Some((Encoding::Rgba8, true)),
        f if (VK_ASTC_FIRST..VK_ASTC_FIRST + 28).contains(&f) => {
            let block = ASTC_BLOCKS[((f - VK_ASTC_FIRST) / 2) as usize];
            Some((Encoding::Astc { block }, (f - VK_ASTC_FIRST) % 2 == 1))
        }
        _ => None,
    }
}

/// A texture file's header, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub encoding: Encoding,
    /// The format is the sRGB variant.
    pub srgb: bool,
    /// Side of level 0, texels.
    pub size: u32,
    /// Where each level's bytes are in the file, level 0 first.
    pub levels: Vec<Range<usize>>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// Levels in a full chain for a `size` texture.
pub fn full_chain(size: u32) -> u32 {
    size.max(1).ilog2() + 1
}

/// Checks a KTX2 file whose first bytes are `head` (at least
/// `min(file_len, HEAD_BYTES)` of them) and which is `file_len` bytes long:
/// a `size` x `size` 2D texture in a known format with a full mip chain, not
/// supercompressed, every level the length its texel count needs and inside
/// the file.
pub fn parse(head: &[u8], file_len: u64, size: u32) -> Result<Header, String> {
    if head.len() < INDEX_START || file_len < INDEX_START as u64 {
        return Err("is too short to be a KTX2 file".into());
    }
    if head[..12] != IDENTIFIER {
        return Err("is not a KTX2 file".into());
    }
    let vk_format = u32_at(head, 12);
    let Some((encoding, srgb)) = format_of(vk_format) else {
        return Err(format!("has vkFormat {vk_format}, which packs do not use"));
    };
    let (width, height, depth) = (u32_at(head, 20), u32_at(head, 24), u32_at(head, 28));
    let (layers, faces, levels) = (u32_at(head, 32), u32_at(head, 36), u32_at(head, 40));
    let supercompression = u32_at(head, 44);
    if (width, height) != (size, size) {
        return Err(format!("is {width} x {height}, not {size} x {size}"));
    }
    if depth != 0 || layers != 0 || faces != 1 {
        return Err("is not a plain 2D texture (depth, layers or faces)".into());
    }
    if supercompression != 0 {
        return Err(format!("is supercompressed (scheme {supercompression})"));
    }
    let want = full_chain(size);
    if levels != want {
        return Err(format!("has {levels} mip levels, a full chain is {want}"));
    }
    let index_end = INDEX_START + LEVEL_ENTRY * want as usize;
    if head.len() < index_end || file_len < index_end as u64 {
        return Err("ends inside its level index".into());
    }
    let mut ranges = Vec::with_capacity(want as usize);
    for i in 0..want {
        let at = INDEX_START + LEVEL_ENTRY * i as usize;
        let (offset, length, raw) = (
            u64_at(head, at),
            u64_at(head, at + 8),
            u64_at(head, at + 16),
        );
        let edge = (size >> i).max(1);
        let need = encoding.level_bytes(edge, edge);
        if length != need || raw != need {
            return Err(format!(
                "level {i} is {length} bytes ({raw} uncompressed), {edge} x {edge} texels need {need}"
            ));
        }
        let end = offset.checked_add(length);
        if offset < index_end as u64 || end.is_none_or(|e| e > file_len) {
            return Err(format!("level {i} is outside the file"));
        }
        // Both fit: the file is at most a few dozen MiB (the caller's cap).
        let (Ok(start), Ok(stop)) = (usize::try_from(offset), usize::try_from(offset + length))
        else {
            return Err(format!("level {i} is outside the file"));
        };
        ranges.push(start..stop);
    }
    let mut sorted: Vec<&Range<usize>> = ranges.iter().collect();
    sorted.sort_by_key(|r| r.start);
    if sorted.windows(2).any(|w| w[0].end > w[1].start) {
        return Err("has levels that overlap".into());
    }
    Ok(Header {
        encoding,
        srgb,
        size,
        levels: ranges,
    })
}

/// The levels' bytes of a whole file `bytes` that [`parse`] accepted.
pub fn level_bytes(bytes: &[u8], header: &Header) -> Vec<Vec<u8>> {
    header
        .levels
        .iter()
        .map(|r| bytes[r.clone()].to_vec())
        .collect()
}

/// Decodes one ASTC level `width` x `height` texels, made of `block` sized
/// blocks, to RGBA8. Also the number of blocks that were not valid encodings
/// (decoded as magenta). The 8-bit result is exact for constant-colour blocks
/// and within a level or so of a GPU's own decode otherwise.
pub fn decode_astc(
    data: &[u8],
    width: u32,
    height: u32,
    block: (u32, u32),
) -> Result<(Vec<u8>, u32), String> {
    if !ASTC_BLOCKS.contains(&block) {
        return Err(format!(
            "{} x {} is not an ASTC block size",
            block.0, block.1
        ));
    }
    let need = Encoding::Astc { block }.level_bytes(width, height);
    if data.len() as u64 != need {
        return Err(format!(
            "{} bytes for {width} x {height}, need {need}",
            data.len()
        ));
    }
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    let footprint = astc_decode::Footprint::new(block.0, block.1);
    let across = width.div_ceil(block.0) as usize;
    // The library asserts on some corrupt blocks.
    let decoded = catch_unwind(AssertUnwindSafe(|| {
        let mut bad = 0u32;
        for (i, chunk) in data.chunks_exact(16).enumerate() {
            let mut raw = [0u8; 16];
            raw.copy_from_slice(chunk);
            let (x0, y0) = ((i % across) as u32 * block.0, (i / across) as u32 * block.1);
            let ok = astc_decode::astc_decode_block(&raw, footprint, |x, y, c| {
                let (px, py) = ((x0 + x) as usize, (y0 + y) as usize);
                if px < w && py < h {
                    out[(py * w + px) * 4..][..4].copy_from_slice(&c);
                }
            });
            bad += u32::from(!ok);
        }
        bad
    }));
    match decoded {
        Ok(bad) => Ok((out, bad)),
        Err(_) => Err("a block could not be decoded".into()),
    }
}

/// Builds a KTX2 file for tests and tools: `levels` (level 0 first) in
/// `vk_format`, `size` x `size`, laid out the way the format says (smallest
/// level first in the file).
#[cfg(test)]
pub fn build(vk_format: u32, size: u32, levels: &[Vec<u8>]) -> Vec<u8> {
    let mut out = IDENTIFIER.to_vec();
    for word in [vk_format, 1, size, size, 0, 0, 1, levels.len() as u32, 0] {
        out.extend(word.to_le_bytes());
    }
    // Data format descriptor and key-value data: none; no global data.
    out.extend([0u8; 16]);
    out.extend([0u8; 16]);
    let index_end = INDEX_START + LEVEL_ENTRY * levels.len();
    let mut offsets = vec![0u64; levels.len()];
    let mut at = index_end as u64;
    for i in (0..levels.len()).rev() {
        at = at.next_multiple_of(16);
        offsets[i] = at;
        at += levels[i].len() as u64;
    }
    for (i, level) in levels.iter().enumerate() {
        out.extend(offsets[i].to_le_bytes());
        out.extend((level.len() as u64).to_le_bytes());
        out.extend((level.len() as u64).to_le_bytes());
    }
    for i in (0..levels.len()).rev() {
        out.resize(offsets[i] as usize, 0);
        out.extend(&levels[i]);
    }
    out
}

/// One ASTC block that is a single colour (a "void extent" block), the
/// channels 8-bit values.
#[cfg(test)]
pub fn void_block(rgba: [u8; 4]) -> [u8; 16] {
    // Bits 0..9 are 0x1FC (the void-extent block mode), bit 9 clears HDR,
    // bits 10 and 11 are ones, then four 13-bit extents of all ones (no
    // extent), then 16-bit red, green, blue and alpha.
    let mut bits: u128 = 0x1FC | (0b11 << 10);
    bits |= ((1u128 << 52) - 1) << 12;
    for (k, c) in rgba.iter().enumerate() {
        bits |= u128::from(u16::from(*c) * 257) << (64 + 16 * k);
    }
    bits.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `size` x `size` texture of one colour in `block` ASTC, every level.
    fn solid(size: u32, block: (u32, u32), srgb: bool, rgba: [u8; 4]) -> Vec<u8> {
        let encoding = Encoding::Astc { block };
        let levels: Vec<Vec<u8>> = (0..full_chain(size))
            .map(|i| {
                let edge = (size >> i).max(1);
                let blocks = encoding.level_bytes(edge, edge) / 16;
                (0..blocks).flat_map(|_| void_block(rgba)).collect()
            })
            .collect();
        build(astc_vk_format(block, srgb).unwrap(), size, &levels)
    }

    fn rgba8(size: u32, vk_format: u32) -> Vec<u8> {
        let levels: Vec<Vec<u8>> = (0..full_chain(size))
            .map(|i| vec![7; ((size >> i).max(1) as usize).pow(2) * 4])
            .collect();
        build(vk_format, size, &levels)
    }

    fn check(bytes: &[u8], size: u32) -> Result<Header, String> {
        parse(
            &bytes[..bytes.len().min(HEAD_BYTES)],
            bytes.len() as u64,
            size,
        )
    }

    #[test]
    fn the_format_numbers_are_vulkans() {
        use ash::vk::Format as F;
        let named = [
            ((4, 4), F::ASTC_4X4_UNORM_BLOCK, F::ASTC_4X4_SRGB_BLOCK),
            ((5, 4), F::ASTC_5X4_UNORM_BLOCK, F::ASTC_5X4_SRGB_BLOCK),
            ((5, 5), F::ASTC_5X5_UNORM_BLOCK, F::ASTC_5X5_SRGB_BLOCK),
            ((6, 5), F::ASTC_6X5_UNORM_BLOCK, F::ASTC_6X5_SRGB_BLOCK),
            ((6, 6), F::ASTC_6X6_UNORM_BLOCK, F::ASTC_6X6_SRGB_BLOCK),
            ((8, 5), F::ASTC_8X5_UNORM_BLOCK, F::ASTC_8X5_SRGB_BLOCK),
            ((8, 6), F::ASTC_8X6_UNORM_BLOCK, F::ASTC_8X6_SRGB_BLOCK),
            ((8, 8), F::ASTC_8X8_UNORM_BLOCK, F::ASTC_8X8_SRGB_BLOCK),
            ((10, 5), F::ASTC_10X5_UNORM_BLOCK, F::ASTC_10X5_SRGB_BLOCK),
            ((10, 6), F::ASTC_10X6_UNORM_BLOCK, F::ASTC_10X6_SRGB_BLOCK),
            ((10, 8), F::ASTC_10X8_UNORM_BLOCK, F::ASTC_10X8_SRGB_BLOCK),
            (
                (10, 10),
                F::ASTC_10X10_UNORM_BLOCK,
                F::ASTC_10X10_SRGB_BLOCK,
            ),
            (
                (12, 10),
                F::ASTC_12X10_UNORM_BLOCK,
                F::ASTC_12X10_SRGB_BLOCK,
            ),
            (
                (12, 12),
                F::ASTC_12X12_UNORM_BLOCK,
                F::ASTC_12X12_SRGB_BLOCK,
            ),
        ];
        assert_eq!(named.len(), ASTC_BLOCKS.len());
        for (block, unorm, srgb) in named {
            assert_eq!(astc_vk_format(block, false), Some(unorm.as_raw() as u32));
            assert_eq!(astc_vk_format(block, true), Some(srgb.as_raw() as u32));
            assert_eq!(
                format_of(unorm.as_raw() as u32),
                Some((Encoding::Astc { block }, false))
            );
            assert_eq!(
                format_of(srgb.as_raw() as u32),
                Some((Encoding::Astc { block }, true))
            );
        }
        assert_eq!(VK_R8G8B8A8_UNORM, F::R8G8B8A8_UNORM.as_raw() as u32);
        assert_eq!(VK_R8G8B8A8_SRGB, F::R8G8B8A8_SRGB.as_raw() as u32);
        assert_eq!(astc_vk_format((7, 7), true), None);
    }

    #[test]
    fn a_good_texture_is_read_with_every_level() {
        let bytes = solid(32, (10, 10), true, [1, 2, 3, 255]);
        let h = check(&bytes, 32).expect("parses");
        assert_eq!(h.encoding, Encoding::Astc { block: (10, 10) });
        assert!(h.srgb);
        assert_eq!(h.levels.len(), 6);
        // 32 -> 4x4 blocks of 16 bytes, then 16 -> 2x2, 8 -> 1, 4, 2, 1.
        let lens: Vec<usize> = h.levels.iter().map(|r| r.len()).collect();
        assert_eq!(lens, [4 * 4 * 16, 2 * 2 * 16, 16, 16, 16, 16]);
        assert_eq!(level_bytes(&bytes, &h)[5], void_block([1, 2, 3, 255]));
        let plain = rgba8(8, VK_R8G8B8A8_UNORM);
        let h = check(&plain, 8).expect("parses");
        assert_eq!(
            (h.encoding, h.srgb, h.levels.len()),
            (Encoding::Rgba8, false, 4)
        );
    }

    #[test]
    fn every_way_a_texture_can_be_wrong_is_refused() {
        let good = solid(16, (4, 4), true, [9, 9, 9, 255]);
        assert!(check(&good, 16).is_ok());
        let words = |at: usize, v: u32| {
            let mut b = good.clone();
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
            b
        };
        let why = |bytes: &[u8], size: u32| check(bytes, size).expect_err("accepted");
        assert!(why(&good, 8).contains("not 8 x 8"));
        assert!(why(&words(12, 9999), 16).contains("vkFormat 9999"));
        assert!(why(&words(12, 100), 16).contains("vkFormat 100"));
        assert!(why(&words(20, 12), 16).contains("12 x 16"));
        assert!(why(&words(24, 8), 16).contains("16 x 8"));
        assert!(why(&words(28, 1), 16).contains("plain 2D"));
        assert!(why(&words(32, 2), 16).contains("plain 2D"));
        assert!(why(&words(36, 6), 16).contains("plain 2D"));
        assert!(why(&words(44, 1), 16).contains("supercompressed"));
        assert!(why(&words(44, 3), 16).contains("supercompressed"));
        // Missing levels (the file claims fewer, or none to be generated).
        assert!(why(&words(40, 1), 16).contains("full chain"));
        assert!(why(&words(40, 0), 16).contains("full chain"));
        assert!(why(&words(40, 6), 16).contains("full chain"));
        // A level of the wrong length, in the index.
        let mut b = good.clone();
        b[INDEX_START + 8..INDEX_START + 16].copy_from_slice(&15u64.to_le_bytes());
        assert!(why(&b, 16).contains("level 0"));
        let mut b = good.clone();
        b[INDEX_START + 16..INDEX_START + 24].copy_from_slice(&0u64.to_le_bytes());
        assert!(why(&b, 16).contains("uncompressed"));
        // A level that starts inside the index, or runs past the end.
        let mut b = good.clone();
        b[INDEX_START..INDEX_START + 8].copy_from_slice(&0u64.to_le_bytes());
        assert!(why(&b, 16).contains("outside"));
        let mut b = good.clone();
        b[INDEX_START..INDEX_START + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(why(&b, 16).contains("outside"));
        // Two levels on the same bytes.
        let mut b = good.clone();
        let first = b[INDEX_START..INDEX_START + 8].to_vec();
        b[INDEX_START + LEVEL_ENTRY..INDEX_START + LEVEL_ENTRY + 8].copy_from_slice(&first);
        assert!(why(&b, 16).contains("overlap"));
        // Cut short, anywhere.
        assert!(why(&good[..good.len() - 1], 16).contains("outside"));
        for len in [0, 1, 11, 12, 79, 80, 103, 104, 150] {
            assert!(check(&good[..len], 16).is_err(), "{len}");
        }
        let mut b = good.clone();
        b[0] = 0;
        assert!(why(&b, 16).contains("not a KTX2"));
    }

    #[test]
    fn random_damage_never_panics_the_reader() {
        let good = solid(16, (6, 6), true, [30, 60, 90, 255]);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for round in 0..400 {
            let mut b = good.clone();
            for _ in 0..(1 + round % 5) {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let at = (seed >> 33) as usize % b.len().min(HEAD_BYTES);
                b[at] = (seed >> 24) as u8;
            }
            if let Ok(h) = check(&b, 16) {
                let levels = level_bytes(&b, &h);
                assert_eq!(levels.len(), h.levels.len());
            }
            let cut = (seed >> 40) as usize % (b.len() + 1);
            let _ = check(&b[..cut], 16);
        }
    }

    #[test]
    fn a_constant_colour_block_decodes_to_its_colour() {
        let bytes = solid(32, (10, 10), true, [200, 120, 40, 255]);
        let h = check(&bytes, 32).unwrap();
        let levels = level_bytes(&bytes, &h);
        let (rgba, bad) = decode_astc(&levels[0], 32, 32, (10, 10)).expect("decodes");
        assert_eq!(bad, 0);
        assert_eq!(rgba.len(), 32 * 32 * 4);
        assert!(rgba.chunks_exact(4).all(|p| p == [200, 120, 40, 255]));
        // The 1 x 1 level of the same colour: one texel out of a 10 x 10 block.
        let (last, _) = decode_astc(&levels[5], 1, 1, (10, 10)).expect("decodes");
        assert_eq!(last, [200, 120, 40, 255]);
        // Different blocks, different colours, in the right places.
        let (a, b) = (void_block([255, 0, 0, 255]), void_block([0, 0, 255, 255]));
        let two: Vec<u8> = a.into_iter().chain(b).collect();
        let (rgba, bad) = decode_astc(&two, 8, 4, (4, 4)).expect("decodes");
        assert_eq!(bad, 0);
        assert_eq!(&rgba[..4], [255, 0, 0, 255]);
        assert_eq!(&rgba[(3 * 8 + 3) * 4..][..4], [255, 0, 0, 255]);
        assert_eq!(&rgba[4 * 4..][..4], [0, 0, 255, 255]);
    }

    #[test]
    fn decoding_checks_lengths_and_survives_garbage() {
        assert!(decode_astc(&[0; 15], 4, 4, (4, 4)).is_err());
        assert!(decode_astc(&[0; 16], 4, 4, (7, 7)).is_err());
        assert!(decode_astc(&[0; 32], 4, 4, (4, 4)).is_err());
        let mut seed = 0x1234_5678_9abc_def0u64;
        for _ in 0..300 {
            let mut block = [0u8; 16];
            for byte in &mut block {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                *byte = (seed >> 33) as u8;
            }
            // Never a panic: an error, or a decode with some bad blocks.
            let _ = decode_astc(&block, 12, 12, (12, 12));
            let _ = decode_astc(&block, 5, 3, (6, 5));
        }
    }
}
