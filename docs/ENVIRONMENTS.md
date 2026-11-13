# Environments: design (fork work, Jim)

Goal: watch Plex movies and shows on the Steam Frame from inside a place:
a cinema, a home theater, a living room, a beach, space, instead of the
current solid-colour void. Many places to choose from, and the picker is
one click away while a video plays. Flat (2D and 3D) video only; 180°/360°
video fills the view and hides the environment, as now.

## What exists today

- `fp-render/src/renderer.rs`: each eye pass clears to
  `VideoParams::background` (and a depth image), draws the chosen place when
  there is one (`env_pass.rs`: its meshes with depth, then its sky at the far
  plane), draws `scene.wgsl` as one full-screen triangle over it (it
  ray-casts onto the flat or curved screen and blends the picture in, so
  the background or the place shows outside it), then egui panels as quads.
  The first places are the cinema (a room) and the studio (a sky), built in
  `fp-render/src/env/`. A 180/360/fisheye picture hides the place.
- Passthrough already works (background alpha 0).
- Plex is done upstream: linking, catalog, Continue Watching, search,
  direct play with transcode fallback, watch-state reports.

## Render changes

1. **Depth.** The renderer owns one depth image per eye (D32_SFLOAT, made
   lazily for the target's extent, old ones to the slot's garbage list):
   `EyeTarget` is built by fp-xr and capture.rs and the XR swapchain cannot
   supply depth, so neither changes. Every pipeline in the eye pass declares
   the depth format; the scene and quad pipelines get it with the test off.
2. **Environment pass** (new `env.wgsl` + pipeline), before the scene pass:
   - Static meshes: position, normal, UV, a lightmap/AO UV; one albedo
     texture per material; depth write on.
   - Lighting = baked/ambient term + an analytic area light for the screen
     (a few samples over the screen rectangle, Lambert + falloff) coloured
     by the video's average colour, so the room glows with the film. The
     screen rectangle depends on the frame's size, known only inside
     `Renderer::draw`, so the light is computed there and is off until the
     first frame arrives.
   - Only for flat video (`projection == Flat`): 180°/360° video also paints
     `bg_color` past the picture's edge and fills the view anyway.
   - A room forces the OPAQUE blend mode; `passthrough` becomes an
     environment id (the one that keeps the alpha-0 path) and the existing
     `Settings::passthrough` bool and its three toggle sites migrate into it.
     "House lights" dim from the room's own level to near-dark while
     playing, up again on pause.
   - Sky: an equirect texture drawn where no mesh is (space, outdoor),
     reusing the equirect UV maths already in `scene.wgsl`.
3. **Scene pass with an environment active:** outside the screen
   `discard` instead of returning `bg_color` (both returns, and the
   chroma-key blend at the end discards or blends without the bg term, so
   the room shows behind keyed pixels); inside, write `frag_depth` from the
   ray's hit point (needs the forward view-proj in `Params`) so seats and
   the stage occlude correctly. `frag_depth` disables early-Z, which is why
   the room pass runs first.
4. **Screen colour.** A tiny compute pass after the video upload samples a
   coarse grid of the frame (both YUV paths, Q10C after conversion) into a
   storage buffer, smoothed over ~150 ms so cuts don't strobe. Lit edges
   ("ambilight") can read a 4–8 cell border of the same grid.

Budget: 2160 px per eye on the Frame's Adreno, at whatever refresh rate is
in use. frame-probe's fixture lists 72, 90, 120 and 144 Hz; the app does
not choose one today and will (film wants 72, 120 or 144), so a room has to
hold 120 Hz: about 8.3 ms a frame including the video. Rooms stay under
~50k triangles, room textures ≤2k, no dynamic shadows, everything else
baked.

Sky sharpness: an eye sees roughly 20 px per degree, a 4k × 2k equirect
gives 11 and an 8k × 4k gives 23. An uncompressed 8k sky is about 130 MB,
so sharp skies need a compressed texture path (ASTC on the Frame, something
else in the desktop preview). Start at 4k and accept a softer sky.

## Environment definition

`fp-core` or a new `fp-env` crate:

```rust
struct Environment {
    id: &'static str,              // "cinema", "home-theater", ...
    meshes: Vec<Mesh>,             // built procedurally or loaded from glTF
    sky: Option<Sky>,              // equirect texture + intensity
    screen: ScreenPlacement,       // default pose, size, curvature
    seats: Vec<Seat>,              // named viewer positions; the first is the default
    lighting: Lighting,            // ambient, house-light level, screen spill strength
}
```

Assets must be CC0 or public domain: Poly Haven (HDRIs, models), Kenney
kits, NASA imagery. All designs are original, not copies of another app's
rooms, and carry no cinema or studio brand names.

## Three kinds, so that adding a place is content, not code

| Kind | What it is | Cost of one more |
|---|---|---|
| Sky | An equirect sky image, a shared ground or platform mesh, a screen placement, the screen's glow on the ground | An image and a few lines of data |
| Room | Built in Rust from one shared kit: shell, stepped floor, seat rows, wall panels, curtains, sconces, windows. A room is a set of parameters and materials | A parameter set, sometimes a new kit part |
| Model | An authored glTF scene with baked lighting (the `gltf` crate) | An asset, made or sourced |

Sky places are cheap: once the sky draw and the shared ground exist, each
further one is an image. Rooms share one kit, so the second room costs far
less than the first. A Sky place depends on a CC0 image that fits; where
none exists it becomes a Room or a Model, or waits.

**User places.** Any equirect JPEG or PNG put in an `environments/` folder
beside the app's settings shows up in the picker as a Sky place on the
default platform; a `.glb` later. People get their own rooms without a
release.

## The set

26 places. `void` and `passthrough` exist today.

| Id | Kind | What |
|---|---|---|
| **Plain** | | |
| `void` | | Today's behaviour (solid colour), the default |
| `passthrough` | | Today's passthrough |
| `studio` | Sky | Soft gradient backdrop, matte floor that takes the screen's glow |
| **Theaters** | | |
| `cinema` | Room | Stadium cinema: a large flat screen on a low stage, a flat floor then steps up to steeply raked rows (every seat sees the whole picture over the heads in front), aisle lights, dimming house lights |
| `movie-palace` | Room | 1930s picture house: proscenium, red curtain, gilt plaster, a balcony seat |
| `giant-screen` | Room | Steep stadium seating, a tall wall-to-wall screen close to the viewer |
| `screening-room` | Room | Private room: a dozen armchairs, wood and fabric walls |
| `arthouse` | Room | Small old cinema: brick, a low stage, worn seats |
| `home-theater` | Room | Small dark room, couch, acoustic panels, wall sconces |
| **At home** | | |
| `living-room` | Room | Day and night variants, TV-sized screen on a wall, windows |
| `loft` | Room + sky | Open loft at night, tall windows onto a city skyline |
| `bedroom` | Room | Screen on the ceiling for watching lying down, or on the far wall |
| `cabin` | Room + sky | Timber cabin, fireplace glow, snow outside the window |
| **Outdoors** | | |
| `drive-in` | Sky | Night sky, big screen on a frame, gravel, distant hills |
| `rooftop` | Sky | Rooftop cinema: string lights, a parapet, the city at night |
| `backyard` | Sky | A sheet on the fence as the screen, lawn, dusk |
| `beach` | Sky | Shoreline at dusk, screen standing in the sand |
| `campsite` | Sky | Forest clearing at night, campfire glow |
| `desert` | Sky | Open desert under the Milky Way |
| `lake` | Sky | Mountain lake at blue hour, a wooden dock |
| `clouds` | Sky | A deck above the clouds at sunset |
| **Elsewhere** | | |
| `space` | Sky | Floating platform, starfield, a planet below |
| `observation-deck` | Room + sky | A ship's lounge, stars through a wide window |
| `moon` | Sky | Lunar surface, Earth above the horizon (NASA imagery) |
| `underwater` | Room + sky | Glass dome on the sea bed, moving caustic light |
| `neon-alley` | Sky | Rain-wet city alley at night, neon signs, the screen as a billboard |

What every place can offer:

- **Seats.** Named viewer positions (front, middle, back, balcony; couch,
  floor). Changing seat moves the viewer, not the screen.
- **Variants.** Time of day where the sky has one (`living-room`,
  `beach`, `lake`).
- **Screen.** The room's own placement by default. The user's distance,
  size and curvature controls still work, limited by the room (not through
  the front wall), and "Put back" returns to the room's placement.
- **Small motion without an animation system.** Fire flicker, star
  twinkle, slow clouds, caustics: shader-only, off until measured on the
  Frame.

## UI

- Environment picker: cards with thumbnails, grouped as in the table,
  favourites and the last few used first. One click from the player's bar,
  and in Settings › Display for the default. Thumbnails come from the
  preview harness (a script command that selects a place and takes a shot),
  so they are regenerated, not drawn by hand.
- Per-environment: seat, variant, house-light level, screen spill on/off,
  screen size preset (the room's own, or the user's existing screen
  settings).
- Remember the last environment; optionally per-library (movies → cinema,
  TV → living room), keyed by the library's section type
  (`collection_type`), which Plex, Jellyfin and Emby all report; the
  item's kind (movie / episode) is the fallback. Remembering a choice per
  video needs a schema version and a `MediaRecord` column, so it comes
  last.

## Milestone 1 as built

What shipped first, and where it differs from the sketch above.

- **Procedural only.** Two places, `cinema` (a room) and `studio` (a sky),
  made from a small kit in code. No assets, nothing downloaded, nothing
  added to the app's size. Plain stays the default; "Your room" stays the
  `passthrough` setting, and wins over a place.
- **Blend instead of discard.** The scene pass does not `discard` outside the
  screen: it returns premultiplied colour and blends over whatever is behind
  it (the cleared background, or a place). Over the plain background that is
  the same picture as before, and keyed pixels show the place.
- **No `frag_depth` yet.** The picture is drawn over the place with the depth
  test off, so rooms are built (and tested) so that no geometry is ever in
  front of the screen from any seat. `frag_depth` comes with models.
- **The screen's colour from the scene shader, not a compute pass.** The
  frame can be a copied image, a buffer shared with the decoder or 10-bit
  video sampled in place, and only the scene shader can read all three. A
  second entry point in it renders a tiny grid of the picture's colours
  through the same colour chain, and the CPU smooths it.
- **The room's light is worked out per vertex, not per pixel.** The 4x2 grid
  of colours (`fs_glow`, `glow.rs`) comes back two frames late, is eased on
  the CPU over a quarter of a second (`env/light.rs`), and lights each
  vertex of the place from eight patches of the screen (`env.wgsl`, cosine
  at the surface and at the screen, over distance squared, no shadows).
  Per pixel the cost was a loop of eight patches over millions of pixels at
  144 Hz; per vertex it is a loop over a few tens of thousands. Only a glossy
  floor (a plain material with sheen: the studio's) reflects the picture per
  pixel, and the cinema's projector window glows with the picture's mean
  colour. The measuring pass runs only when the frame shown, or the settings
  its colours pass through, changed: a paused picture, or a 24 Hz film at
  144 Hz, does not pay for it every frame. The log's GPU line gives its time
  as `glow` (inside `eyes`).
- **Places are string ids.** The setting is a string (`void`, `cinema`,
  `studio`), not an enum: a settings file with a value this build lacks
  would otherwise reset every setting. A place this build does not know shows
  Plain.
- **The picker** is the film button on the control bar (the Position tab) and
  Settings › Places. The preview harness has `place`, `seat` and `house`
  commands, and `tools/ui-preview.sh` takes the shots.
- **Seat height and lying down.** `Settings::place_height` (place id to
  centimetres, -30 to 30 in steps of 5, 0 not stored) moves the eye in room
  space: `places::room_frame` takes it. A recenter inside a place builds the
  anchor from the gaze (`world::anchor_facing_within`, level band 10 degrees,
  the head's own up near straight up), so the room and its screen tip together.
- **Off switch.** `FRAMEPLAYERX_PLACES=0` stops the app asking for a place. It
  does not undo the depth buffer and the blended picture in the eye pass.

## Place packs (testing)

A place pack is a room made outside the app, in Blender, with its light baked
into pictures. It replaces one of the built-in rooms. Official packs ship
inside the release (see "Shipping a pack" below); a place that is not in the
release can be a download the person asks for (see "Optional places" below);
other packs are copied by hand.

- **Where.** One folder per pack in `~/.local/share/frameplayerx/places/`
  (or the folder named by `FRAMEPLAYERX_PLACES_DIR`). A pack whose id is
  `cinema` or `cinema-v2` (any number) replaces the Cinema; the highest number
  wins. Without a pack nothing changes. The app reads the folder once, at
  start.
- **What is in it.** A manifest (`manifest.json`: the id, the screen, the
  seats), a mesh (`mesh.bin`), and PNG textures that are all the same size:
  the room with the house lights up, the room with them off, and eight
  pictures of the light from each eighth of the screen. The exporter that
  writes these is part of the room-building tools, not of the app.
- **What it does.** The room is drawn as one mesh, and each pixel is the dark
  picture mixed toward the lit one by the house lights, plus each of the eight
  screen pictures times the colour the film has in that part of the screen.
  So the room takes the film's colour as the built-in one does, with the
  Screen light slider, and the house lights fade the same way. The seats and
  the screen come from the manifest, so Size still picks Front, Middle or Back.
- **Limits.** At most 100,000 triangles and 200,000 vertices; textures a power
  of two up to 4096 pixels across; every number a real number; every file the
  size its counts say. A pack that breaks one is not used: the app writes a
  warning to its log and shows the built-in room.
- **Format 1 and format 2.** Format 1 (`"format": 1`) is the PNG pack above.
  Format 2 is the same pack made small enough to ship: the textures are KTX2
  files (`house.ktx2`, `dark.ktx2`, `basis0.ktx2` to `basis7.ktx2`), each with
  all its smaller sizes already made, compressed as ASTC. The manifest adds
  `textures.basis_size` (the eight light pictures may be smaller than the room
  pictures) and `textures.bytes` (the files' sizes added up, at most 32 MiB; it
  must match). A pack can be written in either format, and both are read.
  What a format 2 file must be: one 2D picture, not compressed again (no
  zstd), a full set of smaller sizes down to one texel, every size exactly the
  bytes its texels need. The room pictures are all one ASTC sRGB format, the
  light pictures all one ASTC UNORM format, any block size from 4x4 to 12x12;
  plain RGBA8 files are also accepted. A file that breaks this is not used.
- **How it reaches the screen.** If the device can sample ASTC, the blocks go
  to the GPU as they are. If not (the desktop preview on software Vulkan, for
  instance) the app decodes them to plain pixels once, while it builds the
  place on its worker thread, so the picture never stalls. The decode is
  within a level or so of the GPU's own. `FRAMEPLAYERX_ASTC=0` forces the
  decode path on any device. The log says which path was taken, the sizes and
  how much texture memory the place uses.
- **In the code.** `fp-render`'s `env::pack` reads and checks a pack on the
  place builder's worker thread; `env_baked` uploads the textures; the shader
  is `env_baked.wgsl`.

### Shipping a pack

The Blender exporter writes a pack as PNGs (format 1, about 70 MB for the
Cinema). To ship one it is turned into format 2, ASTC texture files, about
13 MB:

    tools/pack-encode.py SRC_PACK OUT_PACK --zip RELEASE_DIR

It needs Python 3 with Pillow and numpy, and `astcenc` 5.x. The defaults are
4096 pixels at ASTC 10x10 for the house and dark pictures and 2048 at 12x12 for
the eight screen pictures; `--size`, `--basis-size`, `--block`,
`--basis-block` and `--preset` change them. Mip levels are made in linear
light. The tool prints each texture's size, block, megabytes and PSNR, checks
the KTX2 files it wrote, and with `--zip` writes `<id>-<version>.zip` and prints
its sha256.

The zip is uploaded as a release asset of the public `jimas21/laterna-places`
repository, and one line in `packaging/places.lock` pins it: the pack id, the
asset's https url and the zip's sha256. CI runs `tools/fetch-places.sh`, which
downloads each pinned pack, refuses a wrong sha256 or any file outside
`<id>/`, and unpacks it; `tools/package.sh` then bundles the packs into
`places/` in the install archive and prints which ones and how large. With no
lines in the lock file nothing is bundled.

### Optional places (downloads)

Only the Cinema is bundled. Every other place is an optional download of about
10 to 15 MB from the public `jimas21/laterna-places` releases, started from
Settings > Places. The app downloads nothing else and nothing on its own.

- **The catalog is compiled in.** `packaging/places-optional.lock` has one line
  per pack: id, version, https url, sha256, size in bytes and the title shown.
  It is built into the app (`include_str!`), so the signed app release vouches
  for each pack's hash and size, and there is no index to fetch. It is empty
  today, and Settings shows nothing new while it is. A pack is added by
  publishing its zip (made as in "Shipping a pack") and adding its line; its
  scene code (a place id for it) is a separate change.
- **Asking.** The first Download asks once, in the row: the size in MB and the
  host (`github.com`). The answer is kept (`places_download_ok`); later
  downloads start at once. Each row then shows the size and Download; while it
  runs a progress bar and Cancel; once installed, Remove (and Update when the
  catalog's version differs from the installed pack's own `version`). Only one
  download runs at a time. Failures say what happened in plain words and
  offer Try again. A place installed or removed shows in the Place list the
  next time the app starts (the folder is read once, at start).
- **How it is fetched** (`fp_updater::places`, on a background thread). Https
  only, redirects included, using the updater's download code. The zip goes to
  `<places dir>/.<id>.part/`, and is refused if the catalog says more than
  40 MB, if the server sends a different number of bytes, or if its SHA-256 is
  not the catalog's. It is unpacked by the rules `tools/fetch-places.sh` uses:
  every name inside `<id>/`, none absolute or with `..`, no links, regular
  files only, at most 64 files and 64 MiB unpacked (counted as written), and a
  `manifest.json`. The pack then goes through the same checks and limits as any
  pack (`env::pack::validate_download`: it names itself with the catalog's id,
  every file is the size its counts say, the whole pack reads). Only then is it
  renamed to `<places dir>/<id>/`, an older copy being replaced in one rename.
  A failure or a Cancel removes everything it made; nothing is resumed.
  Remove deletes the pack's folder.
- **Tests and preview.** `LATERNA_PLACES_CATALOG=<file>` replaces the built-in
  catalog with a file of the same format and allows plain http to this machine,
  so tests and the desktop preview can use a local fixture server. It is not
  for use otherwise. `fp-updater`'s tests install and remove a pack against a
  local server, and check a wrong hash, a body too long or too short, a size
  over the cap, path traversal and links, a cut connection, a cancel and a pack
  the checks refuse.

The Studio is not in the picker for now. Its code and a stored choice of it
still work.

### Measuring the place pass (the jitter is still open)

The Cinema's eye passes cost 8 to 10 ms a frame on the Steam Frame (an empty
scene: 4.6 ms) against a budget of about 2.5 ms at 90 Hz for the place, and the
frame period goes to 22 to 33 ms when frames are missed. What has been done
about it is **not measured on a headset yet**, so treat the jitter as open:

- The baked pass is drawn nearest first (`place_sort`): the depth test then
  rejects hidden fragments before the ten texture reads. This is the one change
  expected to save real GPU time. The basis textures are read trilinear (not
  anisotropic) by default. The default stays 4x MSAA.
- Lazily allocated memory for the depth and MSAA attachments (`mem.rs`) only
  does anything on a device that has such a memory type. Turnip on the Frame
  most likely has none (the log then says "this device has no lazily allocated
  memory type"), so it is a no-op there: do not count it toward the budget. The
  log line "lazily allocated memory, type N" shows when it did engage.
- To get the numbers in one session, start the app with
  `FRAMEPLAYERX_PERF_AB=1`, sit in the Cinema and leave it for about 100
  seconds. It steps through ten variants, ten seconds each, and logs
  `perf-ab: ...` per variant and a `perf-ab table:` at the end with the median
  and worst GPU time of each. The first is the default, so read every other
  line against it. In order: the default (4x MSAA, the light on, sorted, the
  device's own depth format, anisotropic colours); pack order instead of
  sorted (what beta.10 did); the picture's light off; a 16-bit depth buffer; a
  24-bit one; the colours read trilinear; 2x MSAA; 2x with the light off; 1x;
  1x with the light off. The `now ...` line of each variant also says the
  depth format the device really gave. Single knobs: `FRAMEPLAYERX_MSAA=1|2`,
  `FRAMEPLAYERX_PLACE_SORT=0`, `FRAMEPLAYERX_PLACE_BASIS=0`,
  `FRAMEPLAYERX_BASIS_ANISO=1`, `FRAMEPLAYERX_PLACE_ANISO=0` (the house and
  dark textures are read trilinear, not with up to 8 anisotropic taps),
  `FRAMEPLAYERX_DEPTH=16|24|32` (D16, X8_D24 or D32 for the eye passes' depth
  buffer; a format the device cannot attach falls back to the default, and
  the log says so). A smaller depth buffer might let a tiling GPU use bigger
  tiles; it is not known that Turnip does.
- A baked place's fragment reads one colour texture where the house lights
  are all the way up or all the way down (a film playing is the second), two in
  between, and the eight basis textures while the picture lights the room: nine
  reads a fragment while a film plays, ten before this was changed. The output
  is identical.
- Cheaper reads, **not built** (each changes the picture or the pack, or needs
  a new pass, so each waits for a headset number): read the basis textures at
  a mip bias of 1 to 2 (the light is smooth; about a half to three quarters of
  the basis bytes a fragment fetches), skip a patch whose light is below a
  quarter of a code value (dark scenes only), or, the largest, blend the eight
  basis textures into one small texture whenever the picture's light changes
  and read that (one read instead of eight a fragment, for a pass over a
  1024-texel square of eight reads that is a fifth of the fragments').
- `cargo test -p fp-render --test overdraw -- --nocapture` with
  `FRAMEPLAYERX_PLACES_DIR` pointing at a folder with `cinema-v2/` in it counts
  the fragment shader invocations of the place's draw from each seat with a
  pipeline statistics query on lavapipe (it skips without the pack). Measured
  on the 2026.11.10-2 Cinema at 512x512: about 1.8, 2.0 and 2.3 shader runs a
  pixel from the front, middle and back seats in the pack's order, and 1.15,
  1.3 and 1.4 nearest first. That is a software rasteriser that bins and tests
  depth before shading, so it says how much work the order saves; it says
  nothing of what a read costs on Adreno (cache, bandwidth, ASTC, LRZ) or of any
  time. The headset's A/B is still the measurement.

## Milestones

1. Milestone 1 (above), built with CI as the only compiler and the preview
   harness as the screenshot: the depth buffer and blended picture in the eye
   pass; the room kit with `cinema` and `studio`; the environment pass and
   house lights; the picker and Settings › Places; the light from the
   screen. Where each change goes: [ENVIRONMENTS-CODEMAP.md](ENVIRONMENTS-CODEMAP.md).
2. `home-theater`, a second room from the same kit; an MSAA switch for room
   edges, its default chosen after an A/B on the Frame.
3. Photographic skies and the Sky places (about a dozen in one step), user
   places, and a default place per library.
4. The remaining rooms from the kit: `movie-palace`, `giant-screen`,
   `screening-room`, `arthouse`, `living-room`, `loft`, `bedroom`, `cabin`,
   `observation-deck`, `underwater`. Seats and variants.
5. On the Frame: frame timing at 2160 px and 120 Hz, the compressed sky
   path, then glTF models and `.glb` user places.
