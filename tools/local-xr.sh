#!/usr/bin/env bash
# Runs Laterna on this computer against Monado, the open-source OpenXR
# runtime, instead of building and installing on the Steam Frame each time.
# Monado shows the two eyes in a desktop window and fakes the headset and
# both controllers from the keyboard and mouse (its "qwerty" driver).
#
#   tools/local-xr.sh setup          once: Monado and build tools (apt, sudo),
#                                    then FFmpeg into third_party/
#   tools/local-xr.sh run [ARGS...]  Monado + Laterna (ARGS go to
#                                    Laterna, e.g. a video to open)
#   tools/local-xr.sh headless [ARGS...]
#                                    the same with Monado's null compositor
#                                    (no window, for quick smoke tests)
#   tools/local-xr.sh preview OUT    every screen as PNGs, no runtime at all
#                                    (tools/ui-preview.sh)
#
# Works on Ubuntu, Debian and Ubuntu under WSL2 (WSLg shows the windows and
# carries the sound). Under WSL the Linux build goes to ~/.cache so it stays
# off the Windows file system and out of the Windows target/ directory.
#
# Controls: click into Monado's debug window ("Monado!") first; keys and
# mouse go to it. They move the headset, or the left/right controller while
# left Ctrl/Alt is held: WASD/QE move, arrow keys or right-drag turn, left
# click is the trigger, Shift is faster. Its Qwerty > Help tab lists every
# key. Set QWERTY_ENABLE=0 to use real hardware Monado finds instead.
#
# The window with the eyes opens at 1600x900 ($LOCAL_XR_WINDOW, e.g.
# 1280x720); resize it freely, Monado redraws at its size.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
ffmpeg_lib="$root/third_party/ffmpeg/x86_64/lib"
runtime_json=/usr/share/openxr/1/openxr_monado.json

if [ -z "${CARGO_TARGET_DIR:-}" ] && [[ "$root" == /mnt/* ]]; then
  export CARGO_TARGET_DIR="$HOME/.cache/frameplayerx-target"
fi
wsl=0
grep -qi microsoft /proc/version 2>/dev/null && wsl=1

die() { echo "local-xr: $*" >&2; exit 1; }

setup() {
  local pkgs=(
    monado-service monado-cli libopenxr1-monado   # the runtime
    mesa-vulkan-drivers vulkan-tools              # Vulkan (llvmpipe at least)
    xdotool                                       # sizes Monado's window
    libasound2-plugins                            # ALSA into PulseAudio/PipeWire
    build-essential curl xz-utils nasm pkgconf meson ninja-build  # FFmpeg
  )
  echo "== packages: ${pkgs[*]}"
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${pkgs[@]}"
  command -v cargo >/dev/null || die "install Rust first: https://rustup.rs"
  if [ -f "$ffmpeg_lib/libavcodec.so" ]; then
    echo "== FFmpeg already in $ffmpeg_lib"
  else
    echo "== FFmpeg (a few minutes)"
    "$root/tools/build-ffmpeg.sh" x86_64
  fi
  echo "== ready: tools/local-xr.sh run"
}

build() {
  [ -f "$ffmpeg_lib/libavcodec.so" ] || die "no FFmpeg yet: run tools/local-xr.sh setup"
  (cd "$root" && cargo build -p frameplayerx)
}

binary() {
  echo "${CARGO_TARGET_DIR:-$root/target}/debug/frameplayerx"
}

# Starts monado-service with the given environment, waits for its socket.
start_monado() {
  command -v monado-service >/dev/null || die "no Monado yet: run tools/local-xr.sh setup"
  [ -f "$runtime_json" ] || die "missing $runtime_json"
  local sock="${XDG_RUNTIME_DIR:-/tmp}/monado_comp_ipc"
  # One left running (or still shutting down) would take Laterna's
  # session with it when it goes; this run starts its own.
  if pgrep -x monado-service >/dev/null; then
    echo "== stopping the Monado already running"
    pkill -x monado-service || true
    for _ in $(seq 30); do pgrep -x monado-service >/dev/null || break; sleep 0.1; done
    pkill -9 -x monado-service 2>/dev/null || true
  fi
  rm -f "$sock"
  local log="${TMPDIR:-/tmp}/monado-service.log"
  echo "== Monado (log: $log)"
  # Monado watches stdin (Enter quits it) and fails to start when that is
  # /dev/null: give it a pipe that stays open while this script runs.
  env "$@" monado-service >"$log" 2>&1 \
    < <(while kill -0 $$ 2>/dev/null; do sleep 2; done) &
  monado_pid=$!
  for _ in $(seq 100); do
    [ -S "$sock" ] && return
    kill -0 "$monado_pid" 2>/dev/null || { tail -30 "$log"; die "Monado stopped"; }
    sleep 0.1
  done
  tail -30 "$log"
  die "Monado did not start"
}

stop_monado() {
  [ -n "${monado_pid:-}" ] || return 0
  kill "$monado_pid" 2>/dev/null || true
  # It can hang shutting down with a session open: not for long.
  for _ in $(seq 30); do kill -0 "$monado_pid" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$monado_pid" 2>/dev/null || true
  wait "$monado_pid" 2>/dev/null || true
}

# Monado's X11 window opens at 640x360, too small to read the panels; it
# redraws at whatever size it is given ($LOCAL_XR_WINDOW, WIDTHxHEIGHT).
size_window() {
  command -v xdotool >/dev/null || return 0
  local size="${LOCAL_XR_WINDOW:-1600x900}"
  # In the background, until it holds: Monado sizes the window again when
  # the session starts.
  (
    held=0
    for _ in $(seq 60); do
      sleep 0.5
      id="$(xdotool search --name '^Monado$' 2>/dev/null | head -1 || true)"
      [ -n "$id" ] || continue
      now="$(xdotool getwindowgeometry "$id" 2>/dev/null | awk '/Geometry/{print $2}' || true)"
      if [ "$now" = "$size" ]; then
        held=$((held + 1))
        [ "$held" -lt 4 ] || break
      else
        held=0
        xdotool windowsize "$id" "${size%x*}" "${size#*x}" 2>/dev/null || true
      fi
    done
  ) &
}

run_player() {
  build
  trap stop_monado EXIT
  start_monado "$@"
  size_window
  echo "== Laterna"
  XR_RUNTIME_JSON="$runtime_json" \
  LD_LIBRARY_PATH="$ffmpeg_lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    "$(binary)" "${player_args[@]}"
}

cmd="${1:-run}"
[ $# -gt 0 ] && shift
player_args=("$@")
case "$cmd" in
  setup) setup ;;
  run)
    # A window for the eyes and the debug window the keyboard and mouse go
    # to. The eyes' size is a percentage of the fake headset's 640x720: 150
    # keeps text readable at about 30 frames a second on llvmpipe (WSL's
    # software Vulkan); 200 is sharper and slower, 60 smoother and blurry.
    # Under WSL, Monado's Wayland window stays black: use its X11 one.
    run_player \
      QWERTY_ENABLE="${QWERTY_ENABLE:-1}" \
      XRT_DEBUG_GUI="${XRT_DEBUG_GUI:-1}" \
      XRT_COMPOSITOR_FORCE_XCB="${XRT_COMPOSITOR_FORCE_XCB:-$wsl}" \
      XRT_COMPOSITOR_SCALE_PERCENTAGE="${XRT_COMPOSITOR_SCALE_PERCENTAGE:-150}"
    ;;
  headless)
    run_player \
      XRT_COMPOSITOR_NULL=1 \
      QWERTY_ENABLE=1 \
      XRT_COMPOSITOR_SCALE_PERCENTAGE="${XRT_COMPOSITOR_SCALE_PERCENTAGE:-60}"
    ;;
  preview)
    [ -f "$ffmpeg_lib/libavcodec.so" ] || die "no FFmpeg yet: run tools/local-xr.sh setup"
    export LD_LIBRARY_PATH="$ffmpeg_lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    exec "$root/tools/ui-preview.sh" "${player_args[@]}"
    ;;
  *) die "unknown command $cmd (setup, run, headless, preview)" ;;
esac
