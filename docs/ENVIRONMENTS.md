# Environments: design (fork work, Jim)

Goal: watch Plex movies and shows on the Steam Frame from inside a place:
a cinema, a home theater, a living room, a beach, space, instead of the
current solid-colour void. Many places to choose from, and the picker is
one click away while a video plays. Flat (2D and 3D) video only; 180°/360°
video fills the view and hides the environment, as now.

## What exists today

- `fp-render/src/renderer.rs`: each eye pass clears to
  `VideoParams::background`, draws `scene.wgsl` as one full-screen triangle
  (it ray-casts onto the flat or curved screen and returns `bg_color`
  outside it), then egui panels as quads. No depth buffer, no meshes.
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
| `cinema` | Room | Modern multiplex: large curved screen, raked rows, aisle lights, dimming house lights |
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

## Milestones

1. Toolchain: WSL2 Ubuntu (needs a reboot, so it waits for a window:
   `wsl --install --web-download`, reboot, `wsl --install -d Ubuntu-24.04`),
   then `tools/local-xr.sh setup`, build and run the preview harness
   unchanged. Where each change goes: [ENVIRONMENTS-CODEMAP.md](ENVIRONMENTS-CODEMAP.md).
2. Depth + env pass + the first part of the room kit, with the screen
   light: `home-theater` (the smallest room that proves the hard parts);
   check in the preview harness and Monado.
3. Video average colour pass; house-light dimming; stepped floor and seat
   rows in the kit: `cinema`.
4. Picker UI and settings; sky draw and the shared ground; the Sky places
   (about a dozen in one step) and user places.
5. The remaining rooms from the kit: `movie-palace`, `giant-screen`,
   `screening-room`, `arthouse`, `living-room`, `loft`, `bedroom`, `cabin`,
   `observation-deck`, `underwater`. Seats and variants.
6. On the Frame: frame timing at 2160 px and 120 Hz, the compressed sky
   path, then glTF models and `.glb` user places.
