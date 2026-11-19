// The picture's light on a baked place, as ONE texture (build.rs joins
// envcommon.wgsl in front). env_baked.wgsl's `fs_main` reads the screen's
// eight patch textures a fragment and adds
//
//   strength * sum_N basis_N^2 * patch_rgb_N
//
// where `strength` is env.baked.x * env.screen_n.w. This draws that sum, once
// for each texel of a smaller picture of the atlas, whenever the patches'
// colours have moved (src/env_pass.rs decides), and `fs_lit` in env_baked.wgsl
// reads the result with one fetch. The sum is linear in the squared basis
// values, which the pack's mip chain stores as the mean of linear light, so a
// texel of level L of the basis textures is exactly the box average of the
// full size ones and the sum of those is the box average of the sum.
//
// A fullscreen triangle into one level of the light texture. The level of the
// basis textures this one is made from is the push constant (`pc.eye` here:
// the member is shared with the place passes, where it is the eye). The basis
// texel is fetched whole, no filtering: the target level and the basis level
// have the same size by construction.

@group(0) @binding(3) var basis: texture_2d_array<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the target: (0,0), (2,0), (0,2) in texture space.
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(frag.xy);
    let level = i32(pc.eye);
    let strength = env.baked.x * env.screen_n.w;
    var lit = vec3<f32>(0.0);
    for (var i = 0; i < 8; i++) {
        let b = textureLoad(basis, texel, i, level).rgb;
        lit += b * b * env.patch_rgb[i].rgb;
    }
    return vec4<f32>(lit * strength, 1.0);
}
