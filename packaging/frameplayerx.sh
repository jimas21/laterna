#!/bin/bash
# FramePlayerX launcher: what Steam (or Frame Control/FrameDrop) runs.
#
# Uses the FFmpeg libraries shipped in lib/.
here="$(cd "$(dirname "$0")" && pwd)"
export LD_LIBRARY_PATH="$here/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# Optional environment for runs started from Steam, which can't pass any:
# KEY=value lines, e.g. FRAMEPLAYERX_DEBUG_INPUT=1 or FRAMEPLAYERX_RESOLUTION=1728.
envfile=${XDG_CONFIG_HOME:-$HOME/.config}/frameplayerx/env
if [[ -f $envfile ]]; then
  set -a
  # shellcheck disable=SC1090
  . "$envfile"
  set +a
fi

# SteamVR runs each app at its own per-app video settings and overrides what
# the app asks for through OpenXR. This launcher changes none of them by
# default: SteamVR's own defaults apply, and so does anything chosen in
# SteamVR's per-app video settings. To set this Steam app's values, put a
# non-zero value in the env file above (unset or 0 writes nothing):
# - FRAMEPLAYERX_REFRESH_RATE=90 sets the preferred refresh rate in Hz.
# - FRAMEPLAYERX_RESOLUTION=2160 sets the pixels per eye.
# A value already chosen in SteamVR's per-app video settings is kept.
vrcmd=/opt/steamvr/bin/linuxarm64/vrcmd
vrsettings=$HOME/.config/openvr/config/steamvr.vrsettings
app_default() { # KEY TYPE VALUE
  [[ -n ${SteamAppId:-} && -n $3 && $3 != 0 && -x $vrcmd ]] || return 0
  python3 -c 'import json, sys
s = json.load(open(sys.argv[1])).get("steam.app." + sys.argv[2], {})
sys.exit(0 if sys.argv[3] in s else 1)' "$vrsettings" "$SteamAppId" "$1" 2>/dev/null && return 0
  LD_LIBRARY_PATH=${vrcmd%/*} "$vrcmd" --set-settings-"$2" "steam.app.$SteamAppId.$1" "$3" >/dev/null 2>&1 || true
}
app_default preferredRefreshRate float "${FRAMEPLAYERX_REFRESH_RATE:-0}"
app_default resolutionOverride int "${FRAMEPLAYERX_RESOLUTION:-0}"

exec "$here/frameplayerx" "$@"
