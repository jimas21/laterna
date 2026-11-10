#!/usr/bin/env python3
"""Turns a place pack in format 1 (PNG) into format 2 (ASTC in KTX2) for shipping.

    tools/pack-encode.py SRC_PACK_DIR OUT_PACK_DIR [--zip DIR] [options]

Needs Python 3 with Pillow and numpy, and astcenc 5.x on PATH (or --astcenc).
The contract is `.local/pack-format.md` ("Place pack format v2") and docs/ENVIRONMENTS.md.

 * house / dark: sRGB colour, encoded as ASTC sRGB (VK_FORMAT_ASTC_<b>_SRGB_BLOCK).
 * basis0..7: the screen's light, stored sqrt-encoded linear, encoded as ASTC UNORM.
 * Every texture gets a full mip chain down to 1x1. Mips (and any downscale) are made
   in LINEAR light: house/dark are sRGB-decoded first, basis is squared first.
 * The KTX2 container is written here, to the Khronos spec (no libktx needed).

Examples:
    tools/pack-encode.py ~/packs/cinema-v2 ~/packs-v2/cinema-v2 --zip ~/release
    tools/pack-encode.py src out --size 2048 --block 8x8 --basis-size 1024 --basis-block 6x6
"""
import argparse
import hashlib
import json
import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zipfile

import numpy as np
from PIL import Image

# VK_FORMAT_ASTC_<w>x<h>_UNORM_BLOCK; the SRGB variant is the next number.
ASTC_UNORM = {
    (4, 4): 157, (5, 4): 159, (5, 5): 161, (6, 5): 163, (6, 6): 165,
    (8, 5): 167, (8, 6): 169, (8, 8): 171, (10, 5): 173, (10, 6): 175,
    (10, 8): 177, (10, 10): 179, (12, 10): 181, (12, 12): 183,
}
KTX2_ID = bytes([0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A])
KHR_DF_MODEL_ASTC = 162
KHR_DF_PRIMARIES_BT709 = 1
KHR_DF_TRANSFER_LINEAR = 1
KHR_DF_TRANSFER_SRGB = 2
BLOCK_BYTES = 16
LEVEL_ALIGN = 16  # lcm(texel block size 16, 4)
WRITER = b"Laterna tools/pack-encode.py"

SRGB_TO_LIN = np.array(
    [(c / 255.0) / 12.92 if c / 255.0 <= 0.04045 else ((c / 255.0 + 0.055) / 1.055) ** 2.4 for c in range(256)],
    dtype=np.float32,
)


def die(msg):
    sys.exit("pack-encode: " + msg)


def parse_block(text):
    try:
        w, h = (int(p) for p in text.lower().split("x"))
    except ValueError:
        die("bad block %r (want e.g. 10x10)" % text)
    if (w, h) not in ASTC_UNORM:
        die("%dx%d is not an ASTC 2D block size" % (w, h))
    return w, h


def is_pow2(n):
    return n >= 1 and n & (n - 1) == 0


def level_count(size):
    return int(math.log2(size)) + 1


def blocks(n, b):
    return (n + b - 1) // b


def align(n, a):
    return (n + a - 1) // a * a


# ---------------------------------------------------------------- image maths

def lin_encode_srgb(lin):
    lin = np.clip(lin, 0.0, 1.0)
    out = np.where(lin <= 0.0031308, lin * 12.92, 1.055 * np.power(lin, 1 / 2.4) - 0.055)
    return np.rint(np.clip(out, 0, 1) * 255).astype(np.uint8)


def lin_encode_sqrt(lin):
    return np.rint(np.sqrt(np.clip(lin, 0.0, 1.0)) * 255).astype(np.uint8)


def to_linear(img8, kind):
    if kind == "srgb":
        return SRGB_TO_LIN[img8]
    f = img8.astype(np.float32) / 255.0
    return f * f


def box_half(a):
    h, w = a.shape[0], a.shape[1]
    nh, nw = max(1, h // 2), max(1, w // 2)
    sh, sw = (2 if h > 1 else 1), (2 if w > 1 else 1)
    return a.reshape(nh, sh, nw, sw, a.shape[2]).mean(axis=(1, 3), dtype=np.float32)


def load_rgb(path):
    with Image.open(path) as im:
        return np.asarray(im.convert("RGB"))


def mip_chain(src8, size, kind):
    """Yields (level, uint8 HxWx3) from `size` down to 1x1, filtered in linear light."""
    enc = lin_encode_srgb if kind == "srgb" else lin_encode_sqrt
    lin = to_linear(src8, kind)
    first = True
    while lin.shape[0] > size:
        lin = box_half(lin)
        first = False
    level = 0
    while True:
        # Keep the untouched source at level 0 when nothing was resized.
        yield level, (src8 if (level == 0 and first) else enc(lin))
        if lin.shape[0] == 1 and lin.shape[1] == 1:
            return
        lin = box_half(lin)
        level += 1


def psnr(a, b):
    """PSNR in dB between two uint8 arrays of one shape (over all channels), chunked."""
    se = 0.0
    for y in range(0, a.shape[0], 256):
        d = a[y:y + 256].astype(np.int32) - b[y:y + 256].astype(np.int32)
        se += float(np.sum(d * d, dtype=np.int64))
    mse = se / a.size
    return 99.0 if mse == 0 else 10 * math.log10(255.0 * 255.0 / mse)


# ------------------------------------------------------------------- astcenc

def run_astc(astcenc, args, label):
    r = subprocess.run([astcenc] + args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if r.returncode != 0:
        die("astcenc failed (%s): %s\n%s" % (label, " ".join(args), r.stdout))


def encode_level(astcenc, png, kind, block, preset, threads, tmp, name):
    out = os.path.join(tmp, name + ".astc")
    args = ["-cs" if kind == "srgb" else "-cl", png, out, "%dx%d" % block, "-" + preset, "-silent"]
    if threads:
        args += ["-j", str(threads)]
    run_astc(astcenc, args, name)
    with open(out, "rb") as f:
        data = f.read()
    if data[:4] != bytes([0x13, 0xAB, 0xA1, 0x5C]):
        die("%s: not an .astc file" % name)
    bx, by, bz = data[4], data[5], data[6]
    w = data[7] | data[8] << 8 | data[9] << 16
    h = data[10] | data[11] << 8 | data[12] << 16
    if (bx, by, bz) != (block[0], block[1], 1):
        die("%s: astcenc wrote block %dx%dx%d" % (name, bx, by, bz))
    return out, w, h, data[16:]


def decode_level0(astcenc, astc_path, kind, tmp, name):
    out = os.path.join(tmp, name + "-dec.png")
    run_astc(astcenc, ["-ds" if kind == "srgb" else "-dl", astc_path, out, "-silent"], name + " decode")
    return load_rgb(out)


# ---------------------------------------------------------------------- KTX2

def make_dfd(block, srgb):
    words = [
        44,                                   # dfdTotalSize
        0,                                    # vendorId 0, descriptorType 0 (basic)
        2 | (40 << 16),                       # versionNumber 2, descriptorBlockSize 24 + 16
        KHR_DF_MODEL_ASTC | (KHR_DF_PRIMARIES_BT709 << 8)
        | ((KHR_DF_TRANSFER_SRGB if srgb else KHR_DF_TRANSFER_LINEAR) << 16),  # flags 0: not premultiplied
        (block[0] - 1) | ((block[1] - 1) << 8),  # texelBlockDimension0..3, minus one
        BLOCK_BYTES,                          # bytesPlane0..3
        0,                                    # bytesPlane4..7
        0 | (127 << 16),                      # sample: bitOffset 0, bitLength 128-1, channelType ASTC_DATA 0
        0,                                    # samplePosition0..3
        0,                                    # sampleLower
        0xFFFFFFFF,                           # sampleUpper
    ]
    return struct.pack("<11I", *words)


def make_kvd():
    kv = b"KTXwriter\0" + WRITER + b"\0"
    entry = struct.pack("<I", len(kv)) + kv
    return entry + b"\0" * (-len(entry) % 4)


def write_ktx2(path, vk_format, block, srgb, width, height, levels):
    """levels: list of raw ASTC byte strings, level 0 (largest) first."""
    n = len(levels)
    dfd = make_dfd(block, srgb)
    kvd = make_kvd()
    dfd_off = 80 + 24 * n
    kvd_off = dfd_off + len(dfd)
    data_start = align(kvd_off + len(kvd), LEVEL_ALIGN)
    # File order is smallest level first; the index is level 0 first.
    offsets = [0] * n
    pos = data_start
    for i in range(n - 1, -1, -1):
        pos = align(pos, LEVEL_ALIGN)
        offsets[i] = pos
        pos += len(levels[i])
    head = KTX2_ID + struct.pack(
        "<9I", vk_format, 1, width, height, 0, 0, 1, n, 0
    ) + struct.pack("<4I", dfd_off, len(dfd), kvd_off, len(kvd)) + struct.pack("<2Q", 0, 0)
    index = b"".join(struct.pack("<3Q", offsets[i], len(levels[i]), len(levels[i])) for i in range(n))
    out = bytearray(head + index + dfd + kvd)
    out += b"\0" * (data_start - len(out))
    for i in range(n - 1, -1, -1):
        out += b"\0" * (offsets[i] - len(out))
        out += levels[i]
    with open(path, "wb") as f:
        f.write(out)
    return len(out)


def check_ktx2(path, vk_format, block, srgb, size):
    """Re-reads a KTX2 file and verifies header, DFD, level index and sizes. Returns None or raises."""
    with open(path, "rb") as f:
        d = f.read()

    def need(cond, what):
        if not cond:
            die("self-check %s: %s" % (os.path.basename(path), what))

    need(d[:12] == KTX2_ID, "bad identifier")
    (fmt, tsize, w, h, depth, layers, faces, nlev, sc) = struct.unpack_from("<9I", d, 12)
    dfd_off, dfd_len, kvd_off, kvd_len = struct.unpack_from("<4I", d, 48)
    sgd_off, sgd_len = struct.unpack_from("<2Q", d, 64)
    need(fmt == vk_format, "vkFormat %d != %d" % (fmt, vk_format))
    need(tsize == 1, "typeSize %d" % tsize)
    need((w, h) == (size, size), "size %dx%d" % (w, h))
    need((depth, layers, faces, sc) == (0, 0, 1, 0), "depth/layers/faces/supercompression")
    need(nlev == level_count(size), "levelCount %d" % nlev)
    need(sgd_off == 0 and sgd_len == 0, "unexpected supercompression global data")
    need(dfd_off == 80 + 24 * nlev, "dfd offset")
    need(kvd_off == dfd_off + dfd_len and kvd_off + kvd_len <= len(d), "kvd range")
    need(dfd_len == 44 and struct.unpack_from("<I", d, dfd_off)[0] == dfd_len, "dfd size")
    need(d[dfd_off:dfd_off + dfd_len] == make_dfd(block, srgb), "dfd content")
    need(kvd_len % 4 == 0, "kvd padding")
    end = 0
    prev_off = None
    for i in range(nlev):
        off, ln, unc = struct.unpack_from("<3Q", d, 80 + 24 * i)
        lw, lh = max(1, size >> i), max(1, size >> i)
        want = blocks(lw, block[0]) * blocks(lh, block[1]) * BLOCK_BYTES
        need(ln == want and unc == want, "level %d length %d != %d" % (i, ln, want))
        need(off % LEVEL_ALIGN == 0, "level %d offset alignment" % i)
        need(off >= align(kvd_off + kvd_len, LEVEL_ALIGN), "level %d overlaps header" % i)
        if prev_off is not None:
            need(off + ln <= prev_off, "level %d not before level %d" % (i, i - 1))
        prev_off = off
        end = max(end, off + ln)
    need(end == len(d), "file size %d != end of last level %d" % (len(d), end))
    return nlev


# --------------------------------------------------------------------- driver

def encode_texture(astcenc, src_png, out_path, kind, size, block, preset, threads, tmp, name):
    src8 = load_rgb(src_png)
    if src8.shape[0] != src8.shape[1] or not is_pow2(src8.shape[0]):
        die("%s: want a square power-of-two image, got %dx%d" % (src_png, src8.shape[1], src8.shape[0]))
    if src8.shape[0] < size:
        die("%s is %d px, smaller than the requested %d" % (src_png, src8.shape[0], size))
    srgb = kind == "srgb"
    vk = ASTC_UNORM[block] + (1 if srgb else 0)
    levels = []
    ref0 = None
    astc0 = None
    for lvl, img in mip_chain(src8, size, kind):
        png = os.path.join(tmp, "%s-l%d.png" % (name, lvl))
        Image.fromarray(img).save(png, compress_level=1)
        astc, w, h, raw = encode_level(astcenc, png, kind, block, preset, threads, tmp, "%s-l%d" % (name, lvl))
        want = blocks(w, block[0]) * blocks(h, block[1]) * BLOCK_BYTES
        if len(raw) != want or w != max(1, size >> lvl):
            die("%s level %d: astcenc gave %d bytes for %dx%d" % (name, lvl, len(raw), w, h))
        levels.append(raw)
        if lvl == 0:
            ref0, astc0 = img, astc
        else:
            os.remove(astc)
        os.remove(png)
    dec = decode_level0(astcenc, astc0, kind, tmp, name)
    os.remove(astc0)
    p_codec = psnr(dec, ref0)
    if size == src8.shape[0]:
        p_full = p_codec
    else:  # compare against the full-resolution source, the decoded level scaled back up
        up = np.asarray(Image.fromarray(dec).resize((src8.shape[1], src8.shape[0]), Image.BILINEAR))
        p_full = psnr(up, src8)
    nbytes = write_ktx2(out_path, vk, block, srgb, size, size, levels)
    check_ktx2(out_path, vk, block, srgb, size)
    return nbytes, p_codec, p_full


def make_zip(out_dir, zip_dir, pack_id, version):
    os.makedirs(zip_dir, exist_ok=True)
    zpath = os.path.join(zip_dir, "%s-%s.zip" % (pack_id, version))
    names = sorted(os.listdir(out_dir))
    with zipfile.ZipFile(zpath, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for n in names:
            zi = zipfile.ZipInfo("%s/%s" % (pack_id, n), date_time=(1980, 1, 1, 0, 0, 0))
            zi.compress_type = zipfile.ZIP_DEFLATED
            zi.external_attr = 0o644 << 16
            with open(os.path.join(out_dir, n), "rb") as f:
                z.writestr(zi, f.read(), compresslevel=9)
    h = hashlib.sha256()
    with open(zpath, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return zpath, h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description="Place pack format 1 (PNG) -> format 2 (ASTC in KTX2).")
    ap.add_argument("src", help="format-1 pack folder")
    ap.add_argument("out", help="format-2 pack folder to write (replaced if it exists)")
    ap.add_argument("--size", type=int, default=4096, help="house/dark size, power of two (default 4096)")
    ap.add_argument("--basis-size", type=int, default=2048, help="basis size, power of two <= --size (default 2048)")
    ap.add_argument("--block", default="10x10", help="ASTC block for house/dark (default 10x10)")
    ap.add_argument("--basis-block", default="12x12", help="ASTC block for the basis textures (default 12x12)")
    ap.add_argument("--preset", default="thorough",
                    choices=["fastest", "fast", "medium", "thorough", "verythorough", "exhaustive"])
    ap.add_argument("--zip", metavar="DIR", help="also write DIR/<id>-<version>.zip and print its sha256")
    ap.add_argument("--astcenc", default=shutil.which("astcenc") or "astcenc", help="astcenc binary")
    ap.add_argument("--threads", type=int, default=0, help="astcenc threads (default: all cores)")
    a = ap.parse_args()

    for n, v in (("--size", a.size), ("--basis-size", a.basis_size)):
        if not is_pow2(v) or v > 4096:
            die("%s must be a power of two <= 4096" % n)
    if a.basis_size > a.size:
        die("--basis-size must not exceed --size")
    block, bblock = parse_block(a.block), parse_block(a.basis_block)
    if not shutil.which(a.astcenc):
        die("astcenc not found (install it or pass --astcenc)")

    with open(os.path.join(a.src, "manifest.json"), encoding="utf-8") as f:
        man = json.load(f)
    if man.get("format") != 1:
        die("source manifest format is %r, want 1" % man.get("format"))
    tex = man["textures"]
    jobs = [("house", tex["house"], "srgb", a.size, block), ("dark", tex["dark"], "srgb", a.size, block)]
    for i, fn in enumerate(tex["basis"]):
        jobs.append(("basis%d" % i, fn, "unorm", a.basis_size, bblock))
    if len(tex["basis"]) != 8:
        die("want 8 basis textures, the manifest lists %d" % len(tex["basis"]))

    if os.path.exists(a.out):
        shutil.rmtree(a.out)
    os.makedirs(a.out)
    t0 = time.time()
    total = 0
    names = {}
    print("%-8s %5s %-6s %8s %10s %10s" % ("texture", "size", "block", "MB", "PSNR dB", "vs source"))
    with tempfile.TemporaryDirectory(prefix="pack-encode-") as tmp:
        for name, fn, kind, size, blk in jobs:
            out_name = name + ".ktx2"
            nbytes, pc, pf = encode_texture(
                a.astcenc, os.path.join(a.src, fn), os.path.join(a.out, out_name), kind, size, blk, a.preset,
                a.threads, tmp, name)
            total += nbytes
            names[name] = out_name
            print("%-8s %5d %-6s %8.2f %10.2f %10.2f" % (name, size, "%dx%d" % blk, nbytes / 1048576, pc, pf), flush=True)

    for extra in (man["mesh"]["file"], man.get("credits")):
        if extra:
            shutil.copyfile(os.path.join(a.src, extra), os.path.join(a.out, extra))
    man["format"] = 2
    man["textures"] = {
        "size": a.size,
        "basis_size": a.basis_size,
        "house": names["house"],
        "dark": names["dark"],
        "basis": [names["basis%d" % i] for i in range(8)],
        "bytes": total,
    }
    with open(os.path.join(a.out, "manifest.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(man, f, indent=2)
        f.write("\n")
    mesh_bytes = os.path.getsize(os.path.join(a.out, man["mesh"]["file"]))
    folder = sum(os.path.getsize(os.path.join(a.out, n)) for n in os.listdir(a.out))
    print("textures %.2f MB (+ mesh %.2f MB); pack folder %.2f MB; %.0f s" % (
        total / 1048576, mesh_bytes / 1048576, folder / 1048576, time.time() - t0))
    if a.zip:
        zpath, digest = make_zip(a.out, a.zip, man["id"], man["version"])
        print("zip %s  %.2f MB" % (zpath, os.path.getsize(zpath) / 1048576))
        print("sha256 %s" % digest)


if __name__ == "__main__":
    main()
