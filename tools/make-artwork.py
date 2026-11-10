#!/usr/bin/env python3
"""Generates Laterna's Steam library artwork into assets/steam/.

Sizes follow Steam's non-Steam-game artwork: portrait capsule 600x900,
hero 3840x1240, logo 1280x720 (transparent), wide capsule 920x430 and a
256x256 icon. The mark comes from assets/logo.svg and the wordmark is set
in IM Fell English (assets/fonts, SIL OFL), falling back to Inter, the
app's font, when that file is missing. Needs Pillow and an SVG rasterizer:
rsvg-convert (librsvg2-bin), or else the resvg Python binding
(`pip install resvg-py`). Re-run after design changes:
    python3 tools/make-artwork.py
"""
import io
import re
import subprocess
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "steam"
LOGO = ROOT / "assets" / "logo.svg"
FELL = ROOT / "assets" / "fonts" / "IMFeENrm28P.ttf"
INTER = ROOT / "crates" / "frameplayerx" / "assets" / "fonts" / "Inter-Bold.ttf"
FONT = FELL if FELL.is_file() else INTER
# The mark's palette: soot background, the warm disc of light, the brass of
# the lens and the cool afterimage it leaves behind.
TOP, BOTTOM = (0x1F, 0x18, 0x12), (0x0E, 0x0B, 0x08)
WARM, BRASS, TEAL = (0xFF, 0xCB, 0x80), (0xB8, 0x89, 0x4A), (0x3F, 0xB8, 0xC9)
CREAM = (0xFF, 0xE2, 0xAA)


def svg(text, size):
    try:
        png = subprocess.run(
            ["rsvg-convert", "-w", str(size), "-h", str(size)],
            input=text.encode(), capture_output=True, check=True,
        ).stdout
    except FileNotFoundError:
        import resvg_py

        png = bytes(resvg_py.svg_to_bytes(svg_string=text, width=size, height=size))
    return Image.open(io.BytesIO(png)).convert("RGBA")


def icon(size):
    """The app icon: the mark on its rounded square."""
    return svg(LOGO.read_text(encoding="utf-8"), size)


def mark(size):
    """The mark alone, on transparency."""
    text = LOGO.read_text(encoding="utf-8")
    return svg(re.sub(r'<rect id="background"[^>]*/>', "", text), size)


def gradient(w, h):
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / max(h - 1, 1)
        row = tuple(int(TOP[i] + (BOTTOM[i] - TOP[i]) * t) for i in range(3))
        for x in range(w):
            px[x, y] = row
    return img


def glow(img, cx, cy, r, color, alpha):
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(layer).ellipse((cx - r, cy - r, cx + r, cy + r), fill=color + (alpha,))
    layer = layer.filter(ImageFilter.GaussianBlur(r * 0.6))
    img.alpha_composite(layer)


def canvas(w, h):
    img = gradient(w, h).convert("RGBA")
    glow(img, int(w * 0.75), int(h * 0.3), int(min(w, h) * 0.45), WARM, 46)
    glow(img, int(w * 0.2), int(h * 0.8), int(min(w, h) * 0.4), TEAL, 34)
    return img


def place(img, m, cx, cy):
    img.alpha_composite(m, (int(cx - m.width / 2), int(cy - m.height / 2)))


def wordmark(img, x, y, height, centered=False):
    """"Laterna" in the lamp-light cream, vertically centred on `y`,
    starting at `x` (or centred on it)."""
    font = ImageFont.truetype(str(FONT), int(height))
    d = ImageDraw.Draw(img)
    if centered:
        x -= d.textlength("Laterna", font=font) / 2
    d.text((x, y), "Laterna", font=font, fill=CREAM + (255,), anchor="lm")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    p = canvas(600, 900)
    place(p, mark(440), 300, 370)
    wordmark(p, 300, 690, 96, centered=True)
    ImageDraw.Draw(p).text(
        (300, 775), "a VR media player", font=ImageFont.truetype(str(FONT), 34),
        fill=BRASS + (255,), anchor="mm",
    )
    p.convert("RGB").save(OUT / "portrait.png")

    c = canvas(920, 430)
    place(c, mark(270), 165, 215)
    wordmark(c, 305, 215, 104)
    c.convert("RGB").save(OUT / "capsule.png")

    h = canvas(3840, 1240)
    place(h, mark(1000), 2900, 620)
    h.convert("RGB").save(OUT / "hero.png")

    logo = Image.new("RGBA", (1280, 720), (0, 0, 0, 0))
    place(logo, mark(340), 200, 360)
    wordmark(logo, 360, 360, 160)
    logo.save(OUT / "logo.png")

    icon(256).save(OUT / "icon.png")
    for f in sorted(OUT.iterdir()):
        print(f, Image.open(f).size)


if __name__ == "__main__":
    main()
