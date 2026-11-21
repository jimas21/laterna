// What the place passes share (build.rs joins this in front of env.wgsl and
// sky.wgsl): one uniform block per frame slot, written by src/env_pass.rs
// (`EnvUniform`, same member order), and the eye in a push constant.
//
// Room space is the place's own: the screen plane is z = 0, the audience is
// towards +z and the stage floor is y = 0 (a sky place has its origin at the
// viewer's eye). Everything below is in it, and all light is linear.

struct Env {
    // Clip <- room space, for each eye, and its inverse (for the sky's rays).
    room_view_proj: array<mat4x4<f32>, 2>,
    inv_room_view_proj: array<mat4x4<f32>, 2>,
    // Each eye in room space.
    eye_pos: array<vec4<f32>, 2>,
    // Light from the picture, one patch per cell of it: xyz room position,
    // w area in m^2 (0 = off), and its eased linear colour. Zero until the
    // screen's light is wired in.
    patch_pos: array<vec4<f32>, 8>,
    patch_rgb: array<vec4<f32>, 8>,
    // xyz: the screen's normal towards the audience, w: how much of the
    // picture's light lands on the place.
    screen_n: vec4<f32>,
    // Ambient light on a surface facing up and down (rgb), and the house
    // light level 0..1 (w of ambient_up).
    ambient_up: vec4<f32>,
    ambient_down: vec4<f32>,
    // The house lights' colour, already times the level, for the baked share.
    house_rgb: vec4<f32>,
    // rgb the fog fades to, w: 1 on.
    fog: vec4<f32>,
    // x: fog start, y: fog end (metres from the eye), z: how far a sky
    // place's floor is dropped (added to y), w: seconds.
    fog_range: vec4<f32>,
    // The sky's colour straight up and at the horizon, already dimmed.
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    // Materials: rgb albedo, w pattern code (see env.wgsl); x emissive
    // strength, y sheen.
    palette: array<vec4<f32>, 16>,
    palette2: array<vec4<f32>, 16>,
    // A baked place pack (env_baked.wgsl): x its basis gain, y the picture's
    // light on, z the basis read anisotropically, w the colours read so.
    baked: vec4<f32>,
    // The picture as one rectangle, for a baked place's highlight
    // (env_baked.wgsl): xyz its middle and w half its width, xyz the unit
    // vector along its width (left to right) and w half its height.
    screen_c: vec4<f32>,
    screen_r: vec4<f32>,
    // x: the highlight on (1) or off (0), y: its strength.
    // x: the highlight on (1) or off (0), y: its strength, z (a baked place):
    // how strongly the pack's accent texels (material B) glow, 1 as baked.
    shine: vec4<f32>,
    // The screen's masking: xyz the screen frame's middle (room space) and w
    // 1 when the masking is drawn; xy the frame's half width and height, zw
    // the half width and height of the opening left in it (the film's own
    // shape when closed).
    mask_c: vec4<f32>,
    mask_h: vec4<f32>,
    // The idle screen of a pack place (env_baked.wgsl): xyz the rectangle's
    // middle and w 1 when it is drawn; xy its half size, z how far it reaches
    // past that, w the depth of the slab round the plane it replaces; then
    // its colour, already lit by the house lights.
    idle_c: vec4<f32>,
    idle_h: vec4<f32>,
    idle_rgb: vec4<f32>,
};

struct PushConstants {
    eye: u32,
};

@group(0) @binding(0) var<uniform> env: Env;
var<immediate> pc: PushConstants;

// How much of what is drawn at room position `p` shows: 0 for the black
// masking round the film, 1 everywhere else. The masking is the screen's
// plane (the panel, and a wall just behind it) inside the screen frame and
// outside the opening; the picture itself is drawn over the opening.
fn screen_mask(p: vec3<f32>) -> f32 {
    if (env.mask_c.w < 0.5) { return 1.0; }
    let d = p - env.mask_c.xyz;
    let in_plane = abs(d.z) < 0.08;
    let in_frame = abs(d.x) <= env.mask_h.x + 0.02 && abs(d.y) <= env.mask_h.y + 0.02;
    let in_hole = abs(d.x) <= env.mask_h.z && abs(d.y) <= env.mask_h.w;
    return select(1.0, 0.0, in_plane && in_frame && !in_hole);
}
