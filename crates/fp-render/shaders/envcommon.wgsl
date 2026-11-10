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
};

struct PushConstants {
    eye: u32,
};

@group(0) @binding(0) var<uniform> env: Env;
var<immediate> pc: PushConstants;
