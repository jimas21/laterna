#!/usr/bin/env python3
"""Makes the real-astcenc fixtures in crates/fp-render/tests/data/astc/.

    tools/make-astc-fixture.py [OUTDIR]

Needs Python 3 with Pillow and numpy, and astcenc 5.x on PATH. A 16x16 test
pattern (gradients, stripes, a checker and seeded noise, so blocks use real
endpoints and partitions) gets a full mip chain (made as tools/pack-encode.py
makes them), every level is encoded by astcenc and written as a KTX2 file the
way pack-encode writes it. astcenc then decodes every level itself; those
pixels (RGBA8, levels concatenated, level 0 first) are the `.rgba` reference
the CPU fallback in fp-render is tested against.

Files: `p8-srgb.ktx2` (8x8 blocks, sRGB) and `p12-unorm.ktx2` (12x12 blocks,
UNORM), each with its `.rgba`.
"""
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image

here = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("pack_encode", os.path.join(here, "pack-encode.py"))
pe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pe)

SIZE = 16


def pattern():
    y, x = np.mgrid[0:SIZE, 0:SIZE]
    rng = np.random.default_rng(7)
    img = np.zeros((SIZE, SIZE, 3), np.uint8)
    img[..., 0] = x * 255 // (SIZE - 1)
    img[..., 1] = y * 255 // (SIZE - 1)
    img[..., 2] = np.where(((x // 2) + (y // 2)) % 2 == 0, 40, 220)
    stripes = ((x + y) % 5) == 0
    img[stripes] = 255 - img[stripes]
    img[SIZE // 2 :, : SIZE // 2] = rng.integers(0, 256, (SIZE // 2, SIZE // 2, 3), dtype=np.uint8)
    return img


def make(out, name, block, kind, tmp):
    srgb = kind == "srgb"
    vk = pe.ASTC_UNORM[block] + (1 if srgb else 0)
    levels, ref = [], []
    for lvl, img in pe.mip_chain(pattern(), SIZE, kind):
        png = os.path.join(tmp, "l%d.png" % lvl)
        Image.fromarray(img).save(png)
        astc, w, h, raw = pe.encode_level("astcenc", png, kind, block, "exhaustive", 1, tmp, "l%d" % lvl)
        levels.append(raw)
        dec = os.path.join(tmp, "d%d.png" % lvl)
        subprocess.run(["astcenc", "-ds" if srgb else "-dl", astc, dec, "-silent"], check=True)
        ref.append(np.asarray(Image.open(dec).convert("RGBA")).tobytes())
    pe.write_ktx2(os.path.join(out, name + ".ktx2"), vk, block, srgb, SIZE, SIZE, levels)
    with open(os.path.join(out, name + ".rgba"), "wb") as f:
        f.write(b"".join(ref))


def main():
    if not shutil.which("astcenc"):
        sys.exit("astcenc not found")
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "crates", "fp-render", "tests", "data", "astc")
    os.makedirs(out, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        make(out, "p8-srgb", (8, 8), "srgb", tmp)
        make(out, "p12-unorm", (12, 12), "lin", tmp)


if __name__ == "__main__":
    main()
