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
//
// On top of that, the picture is a lamp the glossy surfaces reflect (see
// `shine_light`): an analytic highlight from the rectangle of the screen that
// moves with the head, weighted by the pack's material texture. Skipped
// whole when the picture's light is off or FRAMEPLAYERX_PLACE_SHINE=0.
//
// Two fragment entries: `fs_main` reads the eight basis textures; `fs_lit`
// reads the second line as one texture that env_collapse.wgsl made from them
// (FRAMEPLAYERX_PLACE_COLLAPSE, on by default).

@group(0) @binding(1) var tex_sampler: sampler;
// Layer 0 the house-lights-up texture, layer 1 the lights-off one.
@group(0) @binding(2) var colors: texture_2d_array<f32>;
// Layers 0 to 7: the screen's patches' light.
@group(0) @binding(3) var basis: texture_2d_array<f32>;
// Trilinear only: the basis light is smooth, so the anisotropic taps of
// `tex_sampler` buy nothing there. Which one the basis reads use is
// env.baked.z, a uniform, so the branch costs nothing.
@group(0) @binding(4) var basis_sampler: sampler;
// The picture's light as one texture (strength already in it), made from the
// eight patch textures by env_collapse.wgsl whenever the patches' colours have
// moved: what `fs_lit` reads in place of the eight reads of `fs_main`.
@group(0) @binding(5) var lightmap: texture_2d<f32>;

// The pack's material texture: R roughness, G specular mask (0 matte,
// 1 a glossy dielectric), B the accent mask (1 an exit sign, step or other
// accent light, dimmed with Minimal lighting; 0 default), A reserved. A pack without one gets a 1 x 1
// weak, rough one (src/env/pack.rs `DEFAULT_MATERIAL`).
@group(0) @binding(6) var material: texture_2d<f32>;

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

// The house and dark textures mixed for the house-light level.
fn base_color(uv: vec2<f32>) -> vec3<f32> {
    // Only the texture the house level uses is read at the two ends (lights
    // all the way up, or all the way down, which is where a film plays): the
    // mix would multiply the other one by zero. mix() at 0 and 1 gives its
    // two inputs exactly, so the colour is the same bit for bit. The level is
    // a uniform, so the whole draw takes the same side.
    let level = clamp(env.ambient_up.w, 0.0, 1.0);
    if (level >= 1.0) {
        return color_layer(uv, 0);
    }
    if (level <= 0.0) {
        return color_layer(uv, 1);
    }
    return mix(color_layer(uv, 1), color_layer(uv, 0), level);
}

// Fog and dither: the end of every place's fragment.
fn finish(colour: vec3<f32>, in: VsOut) -> vec4<f32> {
    var lit_color = colour * screen_mask(in.rpos);
    // The pack's accent texels (material B: exit signs, step lights) glow as
    // baked, or less when Minimal lighting dims them for the film. env.shine.z
    // is a uniform, so the read is skipped (and stays in uniform control
    // flow) when they are as baked.
    if (env.shine.z < 1.0) {
        let accent = clamp(textureSample(material, basis_sampler, in.uv).b, 0.0, 1.0);
        lit_color *= 1.0 - accent * (1.0 - clamp(env.shine.z, 0.0, 1.0));
    }
    let dist = length(env.eye_pos[pc.eye].xyz - in.rpos);
    let span = max(env.fog_range.y - env.fog_range.x, 0.001);
    let fog = clamp((dist - env.fog_range.x) / span, 0.0, 1.0) * env.fog.w;
    let col = mix(lit_color, env.fog.rgb, fog);
    return vec4<f32>(dither_linear(col, in.clip.xy), 1.0);
}

// The weakest Fresnel reflectance there is (a dielectric facing the viewer).
const F0: f32 = 0.04;

// Half the width of the reflection's lobe, radians, from the surface's alpha
// (roughness squared): twice alpha, as the lobe doubles on reflection, but
// never past about 70 degrees, and a floor so a mirror stays finite.
const LOBE_SCALE: f32 = 2.0;
const LOBE_MAX: f32 = 1.2;
const LOBE_MIN: f32 = 0.004;

// How much of a Gaussian of width `s` centred `c` from the middle of a span of
// half size `h` lies inside it, 0 to 1 (tanh in place of erf: the same S).
fn span_cover(c: f32, h: f32, s: f32) -> f32 {
    let a = clamp((h - c) / s, -8.0, 8.0);
    let b = clamp((h + c) / s, -8.0, 8.0);
    return 0.5 * (tanh(a) + tanh(b));
}

// The picture's highlight on this surface: the view ray reflected in the
// surface lands on the screen's plane; the reflection's lobe (wider for a
// rougher surface, wider for a farther screen) covers some share of the
// rectangle of the picture, and what it covers is the picture's colour
// there. Times Fresnel and the pack's specular mask. Representative-point
// style, no pass of its own: one material read and a few dozen operations.
//
// The surface's normal is the triangle's (from the screen-space derivatives
// of its position: a baked room is flat panels and the pack keeps no
// normals), turned to face the viewer. The texture read and the derivatives
// come first, in uniform control flow; everything after is arithmetic that
// may differ from pixel to pixel.
fn shine_light(in: VsOut) -> vec3<f32> {
    let m = textureSample(material, basis_sampler, in.uv).rgb;
    let slope = cross(dpdx(in.rpos), dpdy(in.rpos));

    let to_eye = env.eye_pos[pc.eye].xyz - in.rpos;
    let dist = max(length(to_eye), 0.0001);
    let v = to_eye / dist;
    let flat_len = length(slope);
    var n = slope / max(flat_len, 1e-20);
    n = select(n, -n, dot(n, v) < 0.0);
    let n_dot_v = clamp(dot(n, v), 0.0, 1.0);
    let r = reflect(-v, n);

    // Where the reflected ray meets the screen's plane (which faces +screen_n
    // towards the audience): it has to be heading for its face.
    let sn = env.screen_n.xyz;
    let heading = dot(r, sn);
    let t = dot(env.screen_c.xyz - in.rpos, sn) / min(heading, -0.001);
    let hit = in.rpos + r * t - env.screen_c.xyz;
    let right = env.screen_r.xyz;
    let up = cross(sn, right);
    let x = dot(hit, right);
    let y = dot(hit, up);
    let half_w = env.screen_c.w;
    let half_h = env.screen_r.w;

    let rough = clamp(m.x, 0.05, 1.0);
    let alpha = rough * rough;
    // The lobe's footprint on the screen's plane, metres, stretched where the
    // ray meets the plane at a slant.
    let lobe = clamp(LOBE_SCALE * alpha, LOBE_MIN, LOBE_MAX);
    let sigma = t * tan(lobe) / max(-heading, 0.2);
    let s = max(1.18 * sigma, 0.02);
    let cover = span_cover(x, half_w, s) * span_cover(y, half_h, s);

    // The picture's colour where the lobe is centred: the patches' centres
    // are at 1/8, 3/8, 5/8, 7/8 of the width (4 across) and 1/4 and 3/4 of
    // the height (2 down, row 0 the top), blended between.
    let u = 0.5 + 0.5 * clamp(x / half_w, -1.0, 1.0);
    let w = 0.5 - 0.5 * clamp(y / half_h, -1.0, 1.0);
    let fx = clamp(u * 4.0 - 0.5, 0.0, 3.0);
    let fy = clamp(w * 2.0 - 0.5, 0.0, 1.0);
    let ix = min(u32(fx), 2u);
    let tx = fx - f32(ix);
    let top = mix(env.patch_rgb[ix].rgb, env.patch_rgb[ix + 1u].rgb, tx);
    let bottom = mix(env.patch_rgb[ix + 4u].rgb, env.patch_rgb[ix + 5u].rgb, tx);
    let colour = mix(top, bottom, fy);

    // Schlick's Fresnel, rising towards grazing less on a rougher surface.
    let edge = pow(1.0 - n_dot_v, 5.0);
    let fresnel = F0 + (max(1.0 - rough, F0) - F0) * edge;

    let valid = flat_len > 1e-20 && heading < -0.001 && t > 0.0;
    // The pack's accent lights (material B: rails, exit signs, step lights)
    // are lamps, not mirrors: the film is not reflected in them, so nothing
    // of the highlight is added where the pack marks one.
    let mirror = clamp(m.y, 0.0, 1.0) * (1.0 - clamp(m.z, 0.0, 1.0));
    let strength = env.shine.y * env.screen_n.w * mirror * fresnel * cover;
    return select(vec3<f32>(0.0), colour * strength, valid);
}

// Whether the highlight is drawn at all: a uniform, so the whole draw takes
// the same side and the texture read stays in uniform control flow.
fn shine_on() -> bool {
    return env.shine.x > 0.5 && env.screen_n.w > 0.0 && env.screen_c.w > 0.0;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var col = base_color(in.uv);

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
    if (shine_on()) {
        col += shine_light(in);
    }
    return finish(col, in);
}

// The same colour from the collapsed light: one fetch for the picture's light
// instead of eight, under the same conditions.
@fragment
fn fs_lit(in: VsOut) -> @location(0) vec4<f32> {
    var col = base_color(in.uv);
    let strength = env.baked.x * env.screen_n.w;
    if (env.baked.y > 0.5 && strength > 0.0) {
        col += textureSample(lightmap, basis_sampler, in.uv).rgb;
    }
    if (shine_on()) {
        col += shine_light(in);
    }
    return finish(col, in);
}
