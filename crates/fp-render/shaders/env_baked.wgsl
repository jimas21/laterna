// A baked place (a pack made in Blender, see src/env/pack.rs): one mesh, one
// atlas, and every texel of the atlas already the final colour of its surface.
// So there is no albedo and no per-vertex light here, only the mix of what was
// baked:
//
//   col = mix(dark, house, house_level)
//       + basis_gain * screen_light * sum_N basis_N^2 * patch_rgb_N
//   (the second line only when basis_gain * screen_light > 0 and the basis
//   switch, env.baked.y, is on)
//
// `dark` and `house` are sRGB textures (the sampler decodes them to linear);
// each `basis_N` is the light of the screen's patch N alone, stored as the
// square root of linear light, so it is squared here. The patches are the same
// eight cells (4 across, 2 down, the top row first) as the picture's measured
// light (`patch_rgb`, written by env_pass.rs), then the same fog and dither as
// every place. Opaque, depth tested and written like env.wgsl. build.rs joins
// dither.wgsl and envcommon.wgsl in front.

@group(0) @binding(1) var tex_sampler: sampler;
// Layer 0 the house-lights-up texture, layer 1 the lights-off one.
@group(0) @binding(2) var colors: texture_2d_array<f32>;
// Layers 0 to 7: the screen's patches' light.
@group(0) @binding(3) var basis: texture_2d_array<f32>;
// Trilinear only: the basis light is smooth, so the anisotropic taps of
// `tex_sampler` buy nothing there. Which one the basis reads use is
// env.baked.z, a uniform, so the branch costs nothing.
@group(0) @binding(4) var basis_sampler: sampler;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rpos: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    o.clip = env.room_view_proj[pc.eye] * vec4<f32>(v.pos, 1.0);
    o.rpos = v.pos;
    o.uv = v.uv;
    return o;
}

// One layer of the colour textures: with the anisotropic sampler, or, when
// env.baked.w is 0 (FRAMEPLAYERX_PLACE_ANISO=0), the trilinear one. The switch
// is a uniform, so every fragment takes the same side.
fn color_layer(uv: vec2<f32>, layer: i32) -> vec3<f32> {
    if (env.baked.w > 0.5) {
        return textureSample(colors, tex_sampler, uv, layer).rgb;
    }
    return textureSample(colors, basis_sampler, uv, layer).rgb;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Only the texture the house level uses is read at the two ends (lights
    // all the way up, or all the way down, which is where a film plays): the
    // mix would multiply the other one by zero. mix() at 0 and 1 gives its
    // two inputs exactly, so the colour is the same bit for bit. The level is
    // a uniform, so the whole draw takes the same side.
    let level = clamp(env.ambient_up.w, 0.0, 1.0);
    var col: vec3<f32>;
    if (level >= 1.0) {
        col = color_layer(in.uv, 0);
    } else if (level <= 0.0) {
        col = color_layer(in.uv, 1);
    } else {
        col = mix(color_layer(in.uv, 1), color_layer(in.uv, 0), level);
    }

    // The picture's light: every patch's basis, squared back to linear, times
    // the patch's colour. screen_n.w is the Screen light slider's strength,
    // 0 with the light off (the patches are zero then too).
    //
    // The eight reads are skipped when the term is zero (no picture light, a
    // basis gain of 0, or env.baked.y = 0: FRAMEPLAYERX_PLACE_BASIS=0). Every
    // input is a uniform, so the whole draw takes the same side of the branch
    // and the texture reads stay in uniform control flow.
    let strength = env.baked.x * env.screen_n.w;
    if (env.baked.y > 0.5 && strength > 0.0) {
        var lit = vec3<f32>(0.0);
        if (env.baked.z > 0.5) {
            for (var i = 0u; i < 8u; i++) {
                let b = textureSample(basis, tex_sampler, in.uv, i).rgb;
                lit += b * b * env.patch_rgb[i].rgb;
            }
        } else {
            for (var i = 0u; i < 8u; i++) {
                let b = textureSample(basis, basis_sampler, in.uv, i).rgb;
                lit += b * b * env.patch_rgb[i].rgb;
            }
        }
        col += lit * strength;
    }

    let dist = length(env.eye_pos[pc.eye].xyz - in.rpos);
    let span = max(env.fog_range.y - env.fog_range.x, 0.001);
    let fog = clamp((dist - env.fog_range.x) / span, 0.0, 1.0) * env.fog.w;
    col = mix(col, env.fog.rgb, fog);

    return vec4<f32>(dither_linear(col, in.clip.xy), 1.0);
}
