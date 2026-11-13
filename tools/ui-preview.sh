#!/bin/bash
# Renders every Laterna screen to flat PNGs for design review, with the
# desktop preview (no headset; any Vulkan driver, lavapipe on CI).
#
# Usage: tools/ui-preview.sh OUT_DIR
# UI_LANGUAGE=de (or pseudo, a made-up longer language) renders them in
# another language.
# Runs against a throwaway home with a small library made from the test
# clips, so it never touches your own settings or library.
set -euo pipefail
cd "$(dirname "$0")/.."
out="$(mkdir -p "${1:?usage: $0 OUT_DIR}" && cd "$1" && pwd)"
data=crates/fp-media/tests/data

home="$(mktemp -d)"
# Fonts you installed yourself, such as the headset's Noto Sans CJK for
# UI_LANGUAGE=ja or zh-CN, which the throwaway home would hide.
if [ -d "$HOME/.local/share/fonts" ]; then
  mkdir -p "$home/.local/share"
  ln -s "$HOME/.local/share/fonts" "$home/.local/share/fonts"
fi
jf_port_file="$home/jf.port"
python3 - "$home" "$jf_port_file" <<'PY' &
import glob, http.server, json, os, random, sqlite3, sys, urllib.parse

home, port_file = sys.argv[1], sys.argv[2]

def thumb_for(title_prefix):
    db_path = os.path.join(home, ".local/share/frameplayerx/library.sqlite3")
    thumb_dir = os.path.join(home, ".cache/frameplayerx/thumbnails")
    if os.path.exists(db_path):
        with sqlite3.connect(db_path) as db:
            row = db.execute(
                "SELECT id FROM media WHERE title LIKE ? LIMIT 1",
                (title_prefix + "%",),
            ).fetchone()
            if row:
                p = os.path.join(thumb_dir, f"{row[0]}.jpg")
                if os.path.exists(p):
                    return p
    jpgs = sorted(glob.glob(os.path.join(thumb_dir, "[0-9]*.jpg")))
    return jpgs[0] if jpgs else None

THUMBS = {
    "lib_vr": "Sunset Beach Walk",
    "lib_movies": "City Lights",
    "lib_nature": "Mountain Lake",
    "v_aurora": "Aurora Timelapse",
    "v_mountain": "Mountain Lake",
    "v_garden": "Garden Party",
    "v_sunset": "Sunset Beach Walk",
    "v_city": "City Lights",
}

VIEWS = {"Items": [
    {"Id": "lib_vr", "Name": "VR Videos", "Type": "CollectionFolder", "CollectionType": "homevideos", "ChildCount": 5},
    {"Id": "lib_movies", "Name": "Movies", "Type": "CollectionFolder", "CollectionType": "movies", "ChildCount": 18},
    {"Id": "lib_nature", "Name": "Nature & Travel", "Type": "CollectionFolder", "CollectionType": "homevideos", "ChildCount": 9},
]}

RESUME = {"Items": []}

LATEST_VR = [
    {"Id": "v_sunset", "Name": "Sunset Beach Walk", "Type": "Video", "ProductionYear": 2026,
     "RunTimeTicks": 14200000000, "Path": "/media/Sunset Beach Walk_180_LR.mp4",
     "UserData": {"PlaybackPositionTicks": 6500000000}},
    {"Id": "v_aurora", "Name": "Aurora Timelapse", "Type": "Video", "ProductionYear": 2026,
     "RunTimeTicks": 8400000000, "Path": "/media/Aurora Timelapse_360_TB.mkv",
     "UserData": {"IsFavorite": True}},
    {"Id": "v_city", "Name": "City Lights", "Type": "Video", "ProductionYear": 2026,
     "RunTimeTicks": 9800000000, "Path": "/media/City Lights_180_SBS.webm",
     "UserData": {"PlaybackPositionTicks": 3400000000}},
    {"Id": "v_mountain", "Name": "Mountain Lake", "Type": "Video", "ProductionYear": 2025,
     "RunTimeTicks": 11600000000, "Path": "/media/Mountain Lake_360.webm"},
    {"Id": "v_garden", "Name": "Garden Party", "Type": "Video", "ProductionYear": 2026,
     "RunTimeTicks": 7200000000, "Path": "/media/Garden Party_180_LR.mp4",
     "UserData": {"Played": True}},
]

# What the VR Videos grid can be filtered by.
FACETS = {
    "Genres": ["Documentary", "Nature", "Timelapse", "Travel"],
    "Studios": ["Coastline Films", "Northlight VR"],
    "Years": ["2026", "2025"],
    "Tags": ["beach", "city", "drone", "night", "sunset"],
}

PLEX_ITEMS = [{
    "ratingKey": str(100 + i), "type": "movie", "title": item["Name"],
    "year": item["ProductionYear"], "duration": 10000, "viewOffset": 0,
    "summary": "A preview of a personal video from Plex.",
    "thumb": f"/plex-images/{item['Id']}", "art": f"/plex-images/{item['Id']}",
    "Genre": [{"tag": "Travel"}], "Chapter": [{"startTimeOffset": 1000, "tag": "Arrival"}],
    "Media": [{"id": 10, "height": 1080, "width": 1920, "videoCodec": "h264",
               "Part": [{"key": "/plex-media/preview.mp4", "file": item["Path"],
                         "Stream": [{"id": 201, "streamType": 2, "codec": "aac", "displayTitle": "English", "selected": True}]}]},
              {"id": 11, "height": 720, "width": 1280, "videoCodec": "h264",
               "Part": [{"key": "/plex-media/preview.mp4", "file": item["Path"]}]}],
} for i, item in enumerate(LATEST_VR)]

# What a Plex server arranged for its front page: a row of its newest, a
# collection promoted with its own picture, and its collections.
PLEX_COLLECTIONS = [{
    "ratingKey": str(900 + i), "type": "collection", "subtype": "movie", "title": title,
    "childCount": "3", "thumb": f"/plex-images/{image}", "art": f"/plex-images/{image}",
} for i, (title, image) in enumerate([("Weekend trips", "v_mountain"), ("Night owls", "v_city")])]

PLEX_HUBS = {"MediaContainer": {"Hub": [
    {"hubIdentifier": "movie.recentlyadded.1", "title": "Recently Added Movies", "type": "movie",
     "context": "hub.movie.recentlyadded", "key": "/hubs/home/recentlyAdded?type=1&sectionID=1",
     "more": True, "librarySectionID": 1, "Metadata": PLEX_ITEMS[:4]},
    {"hubIdentifier": "custom.collection.1.900", "title": "Weekend trips", "type": "movie",
     "context": "hub.custom.collection", "key": "/library/collections/900/children",
     "more": True, "librarySectionID": 1, "Metadata": PLEX_ITEMS[1:5]},
    {"hubIdentifier": "library.collections.1", "title": "Collections", "type": "collection",
     "context": "hub.library.collections", "key": "/library/sections/1/all?type=18",
     "more": False, "librarySectionID": 1, "Metadata": PLEX_COLLECTIONS},
]}}

# A video playlist the server keeps (it has a collage, not a poster), and what
# was watched on the server, newest first.
PLEX_PLAYLISTS = {"MediaContainer": {"totalSize": 1, "Metadata": [
    {"ratingKey": "950", "type": "playlist", "playlistType": "video", "title": "Friday films",
     "leafCount": 4, "composite": "/plex-images/v_sunset"},
]}}
PLEX_WATCHED = [dict(item, lastViewedAt=1_790_000_000 - 3600 * n, viewCount=1)
                for n, item in enumerate(PLEX_ITEMS[:5])]

# The people a search finds (one without a picture); each is in a few videos.
PEOPLE = [
    {"Id": "p_riley", "Name": "Riley Brooks", "Type": "Person", "ImageTags": {"Primary": "r"}},
    {"Id": "p_rivera", "Name": "Alex Rivera", "Type": "Person", "ImageTags": {}},
]

def grid_items(query):
    """The VR Videos library: by id for a shuffle's pages, shuffled when asked.
    A search's videos by title, a person's by who they are."""
    q = urllib.parse.parse_qs(query)
    if "SearchTerm" in q:
        term = q["SearchTerm"][0].lower()
        if "Video" not in q.get("IncludeItemTypes", [""])[0].split(","):
            return []
        return [i for i in LATEST_VR if term in i["Name"].lower()]
    if q.get("PersonIds"):
        return LATEST_VR[1:4]
    if q.get("ParentId") != ["lib_vr"]:
        return []
    ids = q.get("Ids", [""])[0]
    if ids:
        by_id = {i["Id"]: i for i in LATEST_VR}
        return [by_id[i] for i in ids.split(",") if i in by_id]
    items = list(LATEST_VR)
    if q.get("SortBy") == ["Random"]:
        random.shuffle(items)
    return items

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path.startswith("/plex-media/"):
            with open(os.path.join(home, "Videos/Garden Party.mp4"), "rb") as f:
                data = f.read()
            start, end = 0, len(data) - 1
            ranges = self.headers.get("Range", "")
            if ranges.startswith("bytes="):
                a, b = ranges[6:].split("-", 1)
                start, end = int(a), min(int(b), end) if b else end
            self.send_response(206 if ranges else 200)
            self.send_header("Content-Type", "video/mp4")
            self.send_header("Accept-Ranges", "bytes")
            if ranges:
                self.send_header("Content-Range", f"bytes {start}-{end}/{len(data)}")
            self.send_header("Content-Length", str(end - start + 1))
            self.end_headers()
            self.wfile.write(data[start:end + 1])
            return
        if path == "/photo/:/transcode":
            path = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)["url"][0]
        if (path.startswith("/Items/") and "/Images/" in path) or path.startswith("/plex-images/"):
            item_id = path.split("/")[2]
            jpg = thumb_for(THUMBS.get(item_id, "Sunset"))
            if jpg and os.path.exists(jpg):
                with open(jpg, "rb") as f:
                    data = f.read()
                self.send_response(200)
                self.send_header("Content-Type", "image/jpeg")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
        query = self.path.split("?", 1)[1] if "?" in self.path else ""
        q = urllib.parse.parse_qs(query)
        if path == "/identity":
            body = {"MediaContainer": {"machineIdentifier": "preview-machine"}}
        elif path == "/resources":
            connection = {"uri": f"http://127.0.0.1:{self.server.server_port}", "local": True, "relay": False}
            body = [{"name": name, "provides": "server", "clientIdentifier": machine,
                     "owned": owned, "accessToken": "preview-token", "connections": [connection]}
                    for name, machine, owned in [("Plex", "preview-machine", True), ("Shared Plex", "shared-machine", False)]]
        elif path == "/":
            body = {"MediaContainer": {"friendlyName": "Plex"}}
        elif path == "/library/sections":
            body = {"MediaContainer": {"Directory": [{"key": "1", "type": "movie", "title": "Personal videos", "thumb": "/plex-images/v_sunset"}]}}
        elif path == "/playlists":
            body = PLEX_PLAYLISTS
        elif path == "/playlists/950/items":
            body = {"MediaContainer": {"Metadata": PLEX_ITEMS[:4], "offset": 0, "totalSize": 4}}
        elif path == "/library/all":
            # Films, then episodes (the preview has no episodes).
            watched = PLEX_WATCHED if q.get("type") == ["1"] else []
            body = {"MediaContainer": {"Metadata": watched, "offset": 0, "totalSize": len(watched)}}
        elif path == "/library/sections/1/all" and q.get("type") == ["18"]:
            body = {"MediaContainer": {"Metadata": PLEX_COLLECTIONS, "offset": 0, "totalSize": len(PLEX_COLLECTIONS)}}
        elif path in ("/hubs/promoted", "/hubs", "/hubs/sections/1"):
            body = PLEX_HUBS
        elif path == "/hubs/home/recentlyAdded" or (path.startswith("/library/collections/") and path.endswith("/children")):
            start = int(q.get("X-Plex-Container-Start", [0])[0])
            size = int(q.get("X-Plex-Container-Size", [60])[0])
            body = {"MediaContainer": {"Metadata": PLEX_ITEMS[start:start + size], "offset": start, "totalSize": len(PLEX_ITEMS)}}
        elif path == "/library/sections/1/all":
            start = int(q.get("X-Plex-Container-Start", [0])[0])
            size = int(q.get("X-Plex-Container-Size", [60])[0])
            body = {"MediaContainer": {"Metadata": PLEX_ITEMS[start:start + size], "offset": start, "totalSize": len(PLEX_ITEMS)}}
        elif path.startswith("/library/sections/1/"):
            facet = {"genre": "Genres", "studio": "Studios", "year": "Years"}.get(path.rsplit("/", 1)[1], "Genres")
            body = {"MediaContainer": {"Directory": [{"title": n, "key": n} for n in FACETS[facet]]}}
        elif path.startswith("/library/metadata/"):
            ids = path.rsplit("/", 1)[1].split(",")
            body = {"MediaContainer": {"Metadata": [i for i in PLEX_ITEMS if i["ratingKey"] in ids]}}
        elif path == "/hubs/continueWatching/items":
            body = {"MediaContainer": {"Metadata": []}}
        elif path == "/hubs/search":
            term = q.get("query", [""])[0].lower()
            body = {"MediaContainer": {"Hub": [{"type": "movie", "Metadata": [i for i in PLEX_ITEMS if term in i["title"].lower()]}]}}
        elif path == "/video/:/transcode/universal/decision":
            body = {"MediaContainer": {"transcodeDecisionCode": 1000}}
        elif path.endswith("/Views"):
            body = VIEWS
        elif path == "/Users/u1/Items":
            items = grid_items(query)
            body = {"Items": items, "TotalRecordCount": len(items)}
        elif path == "/Persons":
            term = urllib.parse.parse_qs(query).get("SearchTerm", [""])[0].lower()
            body = {"Items": [p for p in PEOPLE if term in p["Name"].lower()]}
        elif path == "/Items/Filters":
            body = {"Tags": FACETS["Tags"], "Genres": FACETS["Genres"]}
        elif path.lstrip("/") in FACETS:
            body = {"Items": [{"Name": n} for n in FACETS[path.lstrip("/")]]}
        elif path.endswith("/Items/Resume"):
            body = RESUME
        elif path.endswith("/Items/Latest"):
            body = LATEST_VR if "ParentId=lib_vr" in self.path else []
        else:
            body = {"Items": [], "TotalRecordCount": 0}
        raw = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_POST(self):
        if self.path == "/pins":
            raw = json.dumps({"id": 1, "code": "32GR", "expiresIn": 600}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
            return
        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()

srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as f:
    f.write(str(srv.server_port))
srv.serve_forever()
PY
jf_pid=$!
trap 'kill "$jf_pid" 2>/dev/null || true; rm -rf "$home"' EXIT
while [ ! -s "$jf_port_file" ]; do sleep 0.05; done
jf_port="$(cat "$jf_port_file")"
mkdir -p "$home/Videos" "$home/Downloads"
cp "$data/camera_clip.mp4" "$home/Videos/Sunset Beach Walk.mp4"
cp "$data/h264_aac_180_LR.mp4" "$home/Videos/Garden Party.mp4"
cp "$data/vp9.webm" "$home/Videos/Mountain Lake_360.webm"
cp "$data/av1_opus.webm" "$home/Videos/City Lights_180_SBS.webm"
cp "$data/hevc10_tb.mkv" "$home/Videos/Aurora Timelapse_360_TB.mkv"
# Different dates, and a folder, for the browse shots' sort orders.
mkdir -p "$home/Videos/Holidays"
touch -d 2026-03-14 "$home/Videos/Sunset Beach Walk.mp4"
touch -d 2026-09-02 "$home/Videos/Garden Party.mp4"
touch -d 2025-11-20 "$home/Videos/Mountain Lake_360.webm"
touch -d 2026-06-30 "$home/Videos/City Lights_180_SBS.webm"
touch -d 2026-01-08 "$home/Videos/Aurora Timelapse_360_TB.mkv"
# The Videos library, a Jellyfin server, and a library to lock with a PIN.
mkdir -p "$home/Private" "$home/.config/frameplayerx"
cp "$data/h264_aac_180_LR.mp4" "$home/Private/Rooftop.mp4"
if [ -n "${UI_LANGUAGE:-}" ]; then
  printf '{"language": "%s"}\n' "$UI_LANGUAGE" >"$home/.config/frameplayerx/settings.json"
fi
(umask 077 && cat >"$home/.config/frameplayerx/sources.json" <<EOF
[{"type": "library", "id": "videos", "name": "Videos", "folder": "$home/Videos"},
 {"type": "mediabrowser", "id": "jellyfin", "name": "Jellyfin", "url": "http://127.0.0.1:$jf_port", "access_token": "tok", "user_id": "u1"},
 {"type": "library", "id": "private", "name": "Private videos", "folder": "$home/Private"},
 {"type": "plex", "id": "plex", "name": "Plex", "url": "http://127.0.0.1:$jf_port", "machine_identifier": "preview-machine", "access_token": "preview-token"}]
EOF
)
# A video with Arabic (joined letters, right to left) and English subtitles.
mkdir -p "$home/Subtitled"
cp "$data/h264_aac_180_LR.mp4" "$home/Subtitled/Lantern.mp4"
cat >"$home/Subtitled/Lantern.ar.srt" <<'EOF'
1
00:00:00,000 --> 00:10:00,000
مرحبًا، هذه ترجمة عربية
فيها كلمة English أيضًا
EOF
cat >"$home/Subtitled/Lantern.en.srt" <<'EOF'
1
00:00:00,000 --> 00:10:00,000
Hello, these are English subtitles
EOF

script="$home/ui.fps"
if [ "${UI_PREVIEW_PLEX_ONLY:-0}" != 1 ]; then
cat >"$script" <<EOF
# Library scan and thumbnails. Nothing watched yet: Home offers sources.
wait 6
panel main home
# The Dashboard button beside Quit (no video open): pointed at, then pressed.
point main 0.9076 0.052
frames 8
panel main home-hover-dashboard
click
expect-dashboard 1
# The first source tile pointed at: lifted whole at the row's edge.
point main 0.13 0.70
frames 8
panel main home-hover-first
point main 0.5 0.99
screen history
panel main history-empty
# A library: its videos, then its folders. The headset's own folders.
open-source videos
wait 1
panel main library
library-folders
wait 1
panel main library-folders
open-source on-device
panel main local
open-source jellyfin
wait 1
panel main jellyfin
# A library of the server, and its Filters drawer (Back closes it).
point main 0.133 0.37
click
wait 1
panel main jellyfin-grid
filters
wait 1
panel main jellyfin-filters
button secondary
frames 4
# Search: titles, and the people whose names match.
button secondary
frames 4
jellyfin-search ri
wait 1
panel main jellyfin-search
# Playlists: none yet, one made of the library, its page; Up next.
details
panel main details
screen playlists
panel main playlists-empty
pick
panel main playlist-picker
playlist Evening
screen playlists
panel main playlists
open-playlist
panel main playlist
screen sources
panel main sources
# Adding a library (a suggestion for Downloads), and editing one.
add-source library
panel main source-add-library
edit-source videos
panel main source-edit-library
# Browsing a folder of the home source, by name and then newest first.
browse device-home $home/Videos
wait 1
panel main browse
browse-sort newest
panel main browse-newest
browse-sort name
# A source locked with a PIN: its row, the unlock form, its PIN settings.
lock private 2468
screen sources
panel main sources-locked
pin-form private
panel main source-unlock
pin-form private wrong
panel main source-unlock-wrong
unlock private 2468
pin-form private set
panel main source-pin
screen settings general
panel main settings-general
screen settings playback
panel main settings-playback
screen settings position
panel main settings-position
screen settings places
panel main settings-places
place cinema
screen settings places
panel main settings-places-cinema
place void
screen settings sound
panel main settings-sound
screen settings passthrough
panel main settings-passthrough-locked
unlocked
screen settings passthrough
panel main settings-passthrough
chroma-global
screen settings passthrough
frames 8
panel main settings-passthrough-on
chroma-global off
screen settings controller
panel main settings-controller
screen settings controller-open
panel main settings-controller-open
screen settings controller-grip
panel main settings-controller-grip
screen settings controller-stick
panel main settings-controller-stick
haptics on
screen settings haptics
panel main settings-haptics
remote on
screen settings remote
panel main settings-remote
remote off
screen settings updates
panel main settings-updates
screen settings about
panel main settings-about
# The card shown once after an update (and from About's What's new).
whats-new
panel main whats-new
whats-new off
# Hover state on the first card of a library's videos.
open-source videos
point main 0.12 0.45
frames 8
panel main local-hover
# Its title instead: that opens the details.
point main 0.12 0.63
frames 8
panel main local-hover-title
# The keyboard, from the search field.
point main 0.12 0.30
click
frames 8
panel keyboard keyboard
# Its symbols page (the #+= key), then back to letters.
point keyboard 0.195 0.80
click
frames 8
panel keyboard keyboard-symbols
click
frames 3
button secondary
frames 3
# The player.
open $home/Videos/Sunset Beach Walk.mp4
wait-playing
wait 1
pause
point bar 0.5 0.5
frames 8
panel bar player-bar
sbs player-scene
# The More list over the bar's end, round a 180 degree video: its few rows.
more
frames 8
panel more more-menu
panel bar player-bar-more-vr
more off
frames 4
# Places, round the paused video. The room shows once the room renderer is in
# the build: until then these are the plain background. The clip is named as a
# 180 degree video and a place shows round flat ones only, so it is shown flat
# here. The bar's film button, then the cinema with the house lights down and
# up, from above and from the front seat, then the studio. The lights are held
# at a level, since the short clip ends and would raise them; Plain and the
# detected format again after, so every later shot is unchanged. The room takes
# the picture's light after two frames and eases to it over a second, so the
# first shot of a place waits that long (in real time) before it is taken.
flat
frames 6
panel bar player-bar-flat
# The bar's keys from the right: Close, Hide, More, Adjust. The More key's label,
# then the list it opens, every row of a flat video's.
point bar 0.85 0.69
frames 10
panel bar player-bar-more-tip
more
frames 8
panel more more-menu-flat
panel bar player-bar-more
more off
frames 4
place cinema
house 0
wait 1
sbs place-cinema
# In a room the screen is the room's: no browsing beside it, and following says
# it does nothing there.
more
frames 8
panel more more-menu-cinema
more off
frames 4
house 1
frames 2
sbs place-cinema-lit
look 0 -35
frames 2
sbs place-cinema-down
look 0 0
seat 0
frames 2
sbs place-cinema-front
seat 1
place studio
wait 1
sbs place-studio
house auto
place void
flat off
frames 4
# A hover label: the Hide button. (The bar's keys are counted from the right.)
point bar 0.894 0.69
frames 10
panel bar player-bar-tip
# The adjustments panel.
adjust
frames 8
panel adjust adjust
panel bar player-bar-adjust
# The Position tab, with Put back for moved panels.
adjust 1
frames 8
panel adjust adjust-position
# The place picker tops that tab. With the cinema chosen it adds the seats,
# the house lights and the screen light, and the room sets the screen.
flat
panel adjust place-picker-plain
# The bar's Size button: Cinema to Giant, with its name on a toast, then the
# size row lit on Giant; two more presses come back to Cinema.
size
panel toast size-toast
panel adjust size-giant
panel bar player-bar-size
size
size
place cinema
frames 4
panel adjust place-picker
# In the room the same button picks the seat: Giant is the front, Home theater
# the back.
size
panel adjust place-picker-giant
sbs place-cinema-giant
size
panel adjust place-picker-home-theater
sbs place-cinema-home-theater
size
place studio
frames 4
panel adjust place-picker-studio
place void
flat off
frames 4
look -21 -3
point bar 0.52 0.65
frames 4
shot player
look 0 0
# The Passthrough tab, with chroma key on (unlocked above).
adjust 4
chroma
mask
frames 8
panel adjust adjust-passthrough
# Software decoding falling behind.
slow-decode
frames 8
panel bar player-bar-slow-decode
# A shortcut's toast over the video, with the ray off the panels.
aim 70 40
stick 0 1 12
panel toast player-toast
# What was watched: the history, and Home's banner, over the paused video
# (Back closes the adjustments, then shows the browser).
button secondary
frames 3
button secondary
frames 3
screen history
frames 4
panel main history
screen home
frames 15
panel main home-watched
# The Dashboard button is there with a video open too (the header then shows
# Back to video, not Quit); pressing it leaves the video playing.
panel main dashboard-with-video
dashboard
expect-dashboard 2
# Subtitles in Settings › Sound and subtitles, with the languages listed.
screen settings sound
languages
point main 0.6 0.6
stick 0 -1 30
frames 8
panel main settings-subtitles
# The video with subtitle files, set to start in Arabic: the subtitles, the
# audio and subtitles picker over the bar, and the Chapters tab.
subtitles always ar
open-in device-home $home/Subtitled/Lantern.mp4
wait-playing
wait 1
pause
frames 8
panel subs subtitles-arabic
tracks
frames 8
panel tracks tracks
panel bar player-bar-tracks
sbs player-tracks-scene
point tracks 0.5 0.02
click
frames 4
adjust 5
frames 8
panel adjust adjust-chapters
# The library queued behind it: Next on the bar, and Up next from it.
queue-all
point bar 0.611 0.69
frames 10
panel bar player-bar-next
up-next
frames 8
panel main up-next
shuffle
frames 8
panel main up-next-shuffled
EOF
else
  : >"$script"
fi
cat >>"$script" <<EOF
close
wait 3
add-source plex
panel main source-add-plex
point main 0.6 0.6
stick 0 -1 25
frames 8
panel main source-add-plex-actions
plex-link-preview http://127.0.0.1:$jf_port waiting
panel main plex-link-code
plex-link-preview http://127.0.0.1:$jf_port expired
panel main plex-link-expired
plex-link-preview http://127.0.0.1:$jf_port servers
panel main plex-link-servers
edit-source plex
wait 1
panel main source-edit-plex
point main 0.6 0.6
stick 0 -1 35
frames 8
panel main source-edit-plex-actions
open-source plex
wait 2
panel main plex-home
point main 0.133 0.37
click
wait 1
panel main plex-library-recommended
library-tab library
wait 1
panel main plex-grid
filters
wait 1
panel main plex-filters
library-tab collections
wait 1
panel main plex-collections
open-source plex
wait 2
point main 0.6 0.6
stick 0 -1 30
frames 8
panel main plex-home-rows
# Home with a button per library of each server (Home opens scrolled up), then
# the Plex one tapped: all its titles, with Back returning to Home.
screen home
wait 3
point main 0.5 0.5
stick 0 1 80
frames 8
panel main home-libraries
point main 0.51 0.153
click
wait 2
panel main home-library-plex
open-source plex
wait 1
catalog-search sunset
wait 1
panel main plex-search
open-in plex plex://source/plex/preview-machine/item/100?media=10
wait-playing
pause
frames 8
tracks
frames 8
panel tracks plex-tracks
panel bar plex-player-bar
EOF

cargo build -p frameplayerx
# Two optional places for Settings > Places (the built-in list is empty). The
# urls are never fetched: the shots only show the rows.
cat >"$home/places-catalog.lock" <<'EOF'
campsite 1 http://127.0.0.1:9/campsite.zip 0000000000000000000000000000000000000000000000000000000000000000 12582912 Campsite at night
lakeside 1 http://127.0.0.1:9/lakeside.zip 1111111111111111111111111111111111111111111111111111111111111111 14680064 Lakeside deck
EOF
LATERNA_PLACES_CATALOG="$home/places-catalog.lock" \
HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
  XDG_CACHE_HOME="$home/.cache" \
  "${CARGO_TARGET_DIR:-target}/debug/frameplayerx" --preview "$out" --script "$script" --size 1400

# The generated test card, a 3840x2160 picture shown on the eye buffers' two
# sizes: what SteamVR picks (1440) and what the panel can show (2160). Not in
# another language (it has no text), and in a fresh home so nothing above
# leaks into it. The player's controls are hidden, so the card is all there is.
if [ -z "${UI_LANGUAGE:-}" ] && [ "${UI_PREVIEW_PLEX_ONLY:-0}" != 1 ]; then
  card_home="$(mktemp -d)"
  for size in 1440 2160; do
    cat >"$card_home/card.fps" <<EOF
testcard 2160
wait-playing
hide-controls
wait 1
shot testcard-$size
EOF
    HOME="$card_home" XDG_CONFIG_HOME="$card_home/.config" XDG_DATA_HOME="$card_home/.local/share" \
      XDG_CACHE_HOME="$card_home/.cache" \
      "${CARGO_TARGET_DIR:-target}/debug/frameplayerx" --preview "$out" \
      --script "$card_home/card.fps" --size "$size"
  done
  cp "$card_home/.cache/frameplayerx/testcard-2160p.png" "$out/testcard-source-2160p.png"
  # The window controls under a flat screen (the card's is 16:9, 6 m wide at
  # 4 m): the move bar and the resize handle. Each drag takes hold first (the
  # press lands on the handle) and only then moves the hand. The aim is
  # absolute (yaw, pitch): the first ends a little further right and down
  # than the corner, the second fifteen degrees to the left of the bar. Then
  # it is put back, for the look of it. The expect lines fail this step if
  # the screen did not grow or turn.
  cat >"$card_home/window.fps" <<EOF
testcard 2160
wait-playing
pause
wait 0.6
frames 6
shot window-chrome
point window-corner 0.2 0.2
trigger 1
frames 2
aim -44 -23
frames 30
trigger 0
frames 3
expect width > 6.5
shot window-chrome-resized
point window-bar 0.5 0.5
trigger 1
frames 2
aim 18 -22
frames 30
trigger 0
frames 3
expect yaw > 10
shot window-moved
recenter
point window-corner 0.2 0.2
trigger 1
frames 2
aim -38 -17
frames 30
trigger 0
frames 3
expect width < 6.8
shot window-chrome-back
# Docked to the left hand (held 0.25 m left of and 0.35 m below the head, 0.35 m
# ahead): the screen is above the hand, and goes where the hand goes. The
# floating screen's settings are not touched, so its width stays. Then
# the bar tears it off into the room.
hand -0.25 -0.35 0.35
dock left
frames 6
expect docked == 1
expect dock_y > 1.3
expect dock_y < 1.4
expect dock_width ~ 0.3
shot docked-left
hand 0.0 -0.2 0.5
frames 4
expect dock_y > 1.45
point window-bar 0.5 0.5
trigger 1
frames 3
expect docked == 0
expect width < 6.8
trigger 0
frames 3
shot docked-torn-off
EOF
  HOME="$card_home" XDG_CONFIG_HOME="$card_home/.config" XDG_DATA_HOME="$card_home/.local/share" \
    XDG_CACHE_HOME="$card_home/.cache" \
    "${CARGO_TARGET_DIR:-target}/debug/frameplayerx" --preview "$out" \
    --script "$card_home/window.fps" --size 1400
  # The window controls on the library panel: its bar (appears when the laser
  # is near the panel) and its corner. The corner drag makes the panel bigger
  # about its middle; the bar drag moves it round you. No video is open, so
  # the library is up.
  cat >"$card_home/panels.fps" <<EOF
wait 2
frames 6
point main 0.5 0.5
frames 4
shot library-bar
point main-corner 0.2 0.2
trigger 1
frames 2
aim -40 -32
frames 30
trigger 0
frames 3
expect main_scale > 1.2
shot library-resized
point main 0.5 0.5
frames 4
point main-bar 0.5 0.5
trigger 1
frames 2
aim 20 -12
frames 30
trigger 0
frames 3
expect main_yaw > 5
shot library-moved
EOF
  # Browse beside the video: the screen turns left and narrows, the library
  # comes up beside it, and pressing it again puts both back.
  cat >"$card_home/multitask.fps" <<EOF
testcard 2160
wait-playing
pause
wait 0.6
frames 6
multitask
shot multitask
panel main multitask-library
expect multitask == 1
expect yaw > 10
expect width < 5.8
multitask
expect multitask == 0
expect yaw == 0
expect width ~ 5.81
shot multitask-back
EOF
  HOME="$card_home" XDG_CONFIG_HOME="$card_home/.config" XDG_DATA_HOME="$card_home/.local/share" \
    XDG_CACHE_HOME="$card_home/.cache" \
    "${CARGO_TARGET_DIR:-target}/debug/frameplayerx" --preview "$out" \
    --script "$card_home/panels.fps" --size 1400
  # In a home of its own: it expects the video where a fresh one has it (yaw 0),
  # and the window script above leaves this video turned (it keeps what a drag
  # set, as the headset does).
  multitask_home="$(mktemp -d)"
  HOME="$multitask_home" XDG_CONFIG_HOME="$multitask_home/.config" XDG_DATA_HOME="$multitask_home/.local/share" \
    XDG_CACHE_HOME="$multitask_home/.cache" \
    "${CARGO_TARGET_DIR:-target}/debug/frameplayerx" --preview "$out" \
    --script "$card_home/multitask.fps" --size 1400
  rm -rf "$card_home" "$multitask_home"
fi
ls -l "$out"
