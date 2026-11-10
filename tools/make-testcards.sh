#!/bin/bash
# Makes the test clips for judging the picture on a headset, from FFmpeg's
# own generated sources only (no footage, no assets). Each is a few MB and
# 30 s long:
#
#   card_1080p_h264.mp4          8-bit H.264 1080p: 8-bit hardware frames
#                                read in place (NV12)
#   card_2160p_hevc10.mp4        10-bit HEVC 2160p (Main10): 10-bit SDR
#                                hardware frames read in place (TP10)
#   pan_1080p24_h264.mp4         the card sliding sideways 2 px a frame at
#                                24 fps, to judge cadence (how evenly
#                                motion arrives) and lip-sync-free judder
#   darkramp_2160p_hevc10.mp4    smooth dark gradients, 0 to 10 % of full
#                                scale in 10-bit, for banding (dither)
#
# The card has the parts of the in-app test card (`FRAMEPLAYERX_TESTCARD`)
# that matter in a clip: a zone plate in the middle (a chirp reaching the
# finest detail at the edge of its square), 1-texel and 2-texel checkers and
# a flat grey to compare them with on the left, 32 grey steps along the top,
# a dark ramp along the bottom, and a 1 px white border.
#
# Usage: tools/make-testcards.sh OUT_DIR
#   CLIP_SECONDS=30   length of each clip (try 2 to check the script)
#   ONLY=name         make just the clips whose file name contains `name`
#
# Needs ffmpeg 7 or newer with libx264 and libx265. The clips are never
# committed. To try them in the player, copy them to the headset yourself.
set -euo pipefail

out="${1:?usage: $0 OUT_DIR}"
seconds="${CLIP_SECONDS:-30}"
only="${ONLY:-}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

command -v ffmpeg >/dev/null || { echo "ffmpeg is needed" >&2; exit 1; }

# The card as one grey picture, `w` wide and 9/16 as tall: `gray` (8-bit) or
# `gray16be` (16-bit, so a 10-bit clip keeps its fine steps). `u` is a ninth
# of the height, the unit the parts are placed in; each part is a value from
# 0 to 1, and the last line scales it to the format's full range.
card_picture() { # NAME WIDTH FORMAT MAX
  local name="$1" w="$2" fmt="$3" max="$4"
  local h=$((w * 9 / 16))
  local expr
  expr="$(
    tr -d ' \n' <<'EOF'
st(0,H/9);
st(1,5.5*ld(0));
st(2,X+0.5-W/2);
st(3,Y+0.5-H/2);
st(4,(X-0.5*ld(0))/(W-ld(0)));
(
if(lt(X,1)+lt(Y,1)+gt(X,W-2)+gt(Y,H-2),1,
if(lt(abs(ld(2)),ld(1)/2)*lt(abs(ld(3)),ld(1)/2),0.5+0.5*cos(PI/ld(1)*(ld(2)*ld(2)+ld(3)*ld(3))),
if(between(X,0.8*ld(0),3.8*ld(0))*between(Y,1.75*ld(0),3.45*ld(0)),mod(floor(X)+floor(Y),2),
if(between(X,0.8*ld(0),3.8*ld(0))*between(Y,3.6*ld(0),5.3*ld(0)),mod(floor(X/2)+floor(Y/2),2),
if(between(X,0.8*ld(0),3.8*ld(0))*between(Y,5.45*ld(0),7.15*ld(0)),0.5,
if(between(X,0.5*ld(0),W-0.5*ld(0))*between(Y,0.5*ld(0),1.4*ld(0)),min(floor(ld(4)*32),31)/31,
if(between(X,0.5*ld(0),W-0.5*ld(0))*between(Y,8.2*ld(0),8.75*ld(0)),0.1*ld(4),
0)))))))
)
EOF
  )"
  ffmpeg -v error -y -f lavfi -i "color=c=black:s=${w}x${h}:r=1:d=1,format=$fmt,geq=lum='($expr)*$max'" \
    -frames:v 1 -pix_fmt "$fmt" "$tmp/$name.png"
}

# A smooth dark picture for banding: a horizontal ramp from black to 10 %
# above, the same on a diagonal below.
dark_picture() { # NAME WIDTH
  local name="$1" w="$2"
  local h=$((w * 9 / 16))
  ffmpeg -v error -y -f lavfi -i "color=c=black:s=${w}x${h}:r=1:d=1,format=gray16be,geq=lum='if(lt(Y,H/2),0.1*X/W,0.1*(X/W+Y/H-0.5)/1.5)*65535'" \
    -frames:v 1 -pix_fmt gray16be "$tmp/$name.png"
}

# Colour tags a player reads as plain SDR BT.709, limited range.
sdr=(-colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv)

wanted() { [[ -z "$only" || "$1" == *"$only"* ]]; }

# H.264 High, 4:2:0 8-bit, from the input options given, into $OUT.
h264() {
  ffmpeg -v error -y "$@" -t "$seconds" -c:v libx264 -preset slow -crf 14 \
    -profile:v high -pix_fmt yuv420p "${sdr[@]}" -movflags +faststart -an "$out/$OUT"
}

# HEVC Main10, 4:2:0 10-bit, from the input options given, into $OUT.
hevc10() {
  ffmpeg -v error -y "$@" -t "$seconds" -c:v libx265 -preset medium -crf 14 \
    -x265-params log-level=error -pix_fmt yuv420p10le "${sdr[@]}" -tag:v hvc1 \
    -movflags +faststart -an "$out/$OUT"
}

if wanted card_1080p_h264; then
  OUT=card_1080p_h264.mp4
  card_picture card1080 1920 gray 255
  h264 -loop 1 -framerate 24 -i "$tmp/card1080.png"
  echo "$OUT"
fi

if wanted card_2160p_hevc10; then
  OUT=card_2160p_hevc10.mp4
  card_picture card2160 3840 gray16be 65535
  hevc10 -loop 1 -framerate 24 -i "$tmp/card2160.png"
  echo "$OUT"
fi

if wanted pan_1080p24_h264; then
  OUT=pan_1080p24_h264.mp4
  [[ -f "$tmp/card1080.png" ]] || card_picture card1080 1920 gray 255
  # Slides left 2 px a frame (2/1920 of the width), wrapping round.
  ffmpeg -v error -y -loop 1 -framerate 24 -i "$tmp/card1080.png" -t "$seconds" \
    -vf "scroll=horizontal=0.00104167,format=yuv420p" -c:v libx264 -preset slow -crf 14 \
    -profile:v high "${sdr[@]}" -movflags +faststart -an "$out/$OUT"
  echo "$OUT"
fi

if wanted darkramp_2160p_hevc10; then
  OUT=darkramp_2160p_hevc10.mp4
  dark_picture dark2160 3840
  hevc10 -loop 1 -framerate 24 -i "$tmp/dark2160.png"
  echo "$OUT"
fi

ls -l "$out"
