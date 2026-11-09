# Environments: design (fork work, Jim)

Goal: watch Plex movies and shows on the Steam Frame from inside a place —
a cinema, a home theater, a living room, space — instead of the current
solid-colour void. Flat (2D and 3D) video only; 180°/360° video fills the
view and hides the environment, as now.

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
   - Optional sky: an equirect texture drawn where no mesh is (space,
     outdoor), reusing the equirect UV maths already in `scene.wgsl`.
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

Budget: 2160 px per eye at 90 Hz on the Frame's Adreno. Rooms stay under
~50k triangles, textures ≤2k, no dynamic shadows, everything else baked.

## Environment definition

`fp-core` or a new `fp-env` crate:

```rust
struct Environment {
    id: &'static str,              // "cinema", "home-theater", ...
    meshes: Vec<Mesh>,             // built procedurally or loaded from glTF
    sky: Option<Sky>,              // equirect texture + intensity
    screen: ScreenPlacement,       // default pose, size, curvature
    viewer: Vec3,                  // seat position (eye height from floor)
    lighting: Lighting,            // ambient, house-light level, screen spill strength
}
```

Rooms start procedural (walls, stepped floor, instanced seat rows built in
Rust), so there is no asset pipeline to begin with; props come later as
glTF via the `gltf` crate. Assets must be CC0 or public domain: Poly Haven
(HDRIs, models), Kenney kits, NASA imagery. All designs are original, not
copies of another app's rooms.

## First set

| Id | What |
|---|---|
| `void` | Today's behaviour (solid colour), the default |
| `passthrough` | Today's passthrough |
| `cinema` | Large curved screen, raked rows of seats, dimming house lights, screen spill |
| `home-theater` | Small dark room, couch, acoustic panels, wall sconces |
| `living-room` | Daylight/night variants, TV-sized screen on a wall, windows |
| `space` | Floating platform, starfield and a planet below, screen against the stars |
| `drive-in` | Night sky, big outdoor screen, ground plane, distant hills |

## UI

- Environment picker: a row of cards in the player's view settings and in
  Settings › Display (default environment).
- Per-environment: house-light level, screen spill on/off, screen size
  preset (the room's own, or the user's existing screen settings).
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
2. Depth + env pass + one procedural room (`home-theater`) with the screen
   light; check in the preview harness and Monado.
3. Video average colour pass; house-light dimming.
4. Picker UI and settings; the remaining rooms and sky support.
5. On the Frame: frame timing at 2160 px/90 Hz, then glTF props.
