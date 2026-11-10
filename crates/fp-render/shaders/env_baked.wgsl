// A baked place (a pack made in Blender, see src/env/pack.rs): one mesh, one
// atlas, and every texel of the atlas already the final colour of its surface.
// So there is no albedo and no per-vertex light here, only the mix of what was
// baked:
//
//   col = mix(dark, house, house_level)
//       + basis_gain * screen_light * sum_N basis_N^2 * patch_rgb_N
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

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let house = textureSample(colors, tex_sampler, in.uv, 0).rgb;
    let dark = textureSample(colors, tex_sampler, in.uv, 1).rgb;
    var col = mix(dark, house, clamp(env.ambient_up.w, 0.0, 1.0));

    // The picture's light: every patch's basis, squared back to linear, times
    // the patch's colour. screen_n.w is the Screen light slider's strength,
    // 0 with the light off (the patches are zero then too).
    var lit = vec3<f32>(0.0);
    for (var i = 0u; i < 8u; i++) {
        let b = textureSample(basis, tex_sampler, in.uv, i).rgb;
        lit += b * b * env.patch_rgb[i].rgb;
    }
    col += lit * (env.baked.x * env.screen_n.w);

    let dist = length(env.eye_pos[pc.eye].xyz - in.rpos);
    let span = max(env.fog_range.y - env.fog_range.x, 0.001);
    let fog = clamp((dist - env.fog_range.x) / span, 0.0, 1.0) * env.fog.w;
    col = mix(col, env.fog.rgb, fog);

    return vec4<f32>(dither_linear(col, in.clip.xy), 1.0);
}
