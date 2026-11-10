// A place's meshes: walls, seats, steps, a floor. Lit per vertex by what the
// place's builder baked (ambient occlusion and the house lights' share), so
// the work per pixel is a palette lookup, a filtered pattern, fog and dither.
//
// Opaque, depth tested and written (the picture is drawn over it afterwards
// with no depth test, so no mesh may stand in front of it). build.rs joins
// dither.wgsl and envcommon.wgsl in front.

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) ao: f32,
    @location(4) house: f32,
    // Plain input here; it crosses to the fragment stage as a flat varying.
    @location(5) material: u32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rpos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // x: ambient occlusion, y: share of the house lights.
    @location(3) light: vec2<f32>,
    @location(4) @interpolate(flat) material: u32,
};

// Pattern codes, the palette's w (`pattern_code` in env_pass.rs).
const CARPET: u32 = 1u;
const FABRIC: u32 = 2u;
const PANEL: u32 = 3u;
const WOOD: u32 = 4u;
const EMISSIVE: u32 = 6u;
const EMISSIVE_HOUSE: u32 = 7u;

@vertex
fn vs_main(v: VsIn) -> VsOut {
    // A sky place drops its floor under a big screen (added to y).
    let p = vec3<f32>(v.pos.x, v.pos.y + env.fog_range.z, v.pos.z);
    var o: VsOut;
    o.clip = env.room_view_proj[pc.eye] * vec4<f32>(p, 1.0);
    o.rpos = p;
    o.normal = v.normal;
    o.uv = v.uv;
    o.light = vec2<f32>(v.ao, v.house);
    o.material = v.material;
    return o;
}

// Integer hash to 0..1, the same on every GPU.
fn hash2(p: vec2<i32>) -> f32 {
    var h = bitcast<u32>(p.x) * 374761393u + bitcast<u32>(p.y) * 668265263u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 0xffffffu) / 16777216.0;
}

fn value_noise(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let t = fract(p);
    let u = t * t * (3.0 - 2.0 * t);
    let c = vec2<i32>(cell);
    let a = hash2(c);
    let b = hash2(c + vec2<i32>(1, 0));
    let d = hash2(c + vec2<i32>(0, 1));
    let e = hash2(c + vec2<i32>(1, 1));
    return mix(mix(a, b, u.x), mix(d, e, u.x), u.y);
}

// How much of a pattern repeating `cycles` times a metre is kept when a pixel
// spans `w` metres of the surface: all of it while a cycle takes several
// pixels, none once it takes fewer than two, so a fine pattern fades to its
// average instead of shimmering as the head moves.
fn keep(w: f32, cycles: f32) -> f32 {
    return 1.0 - smoothstep(0.2, 0.6, w * cycles);
}

// How much of a pixel `w` metres across a seam `width` either side of the
// line covers, at distance `d` from the line.
fn seam(d: f32, width: f32, w: f32) -> f32 {
    let wide = max(w, 0.0001);
    return clamp((width - d) / wide + 0.5, 0.0, 1.0) * min(1.0, 2.0 * width / wide);
}

// The factor a pattern puts on the albedo (1 on average). `w` is the pixel's
// footprint on the surface in metres.
fn pattern(code: u32, uv: vec2<f32>, w: f32) -> f32 {
    var v = 1.0;
    if (code == CARPET) {
        let coarse = value_noise(uv * 40.0) - 0.5;
        let fine = value_noise(uv * 110.0 + vec2<f32>(7.3, 1.9)) - 0.5;
        v = 1.0 + 0.5 * coarse * keep(w, 40.0) + 0.3 * fine * keep(w, 110.0);
    } else if (code == FABRIC) {
        let weave = sin(uv.x * 500.0) * sin(uv.y * 500.0);
        let cloud = value_noise(uv * 7.0) - 0.5;
        v = 1.0 + 0.1 * weave * keep(w, 80.0) + 0.3 * cloud * keep(w, 7.0);
    } else if (code == PANEL) {
        // Seams every 0.6 m, 8 mm wide.
        let d = abs(fract(uv / 0.6 + vec2<f32>(0.5)) - vec2<f32>(0.5)) * 0.6;
        let lines = max(seam(d.x, 0.004, w), seam(d.y, 0.004, w));
        v = 1.0 - 0.5 * lines;
    } else if (code == WOOD) {
        let grain = value_noise(vec2<f32>(uv.x * 1.5, uv.y * 50.0)) - 0.5;
        v = 1.0 + 0.6 * grain * keep(w, 50.0);
    }
    return max(v, 0.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Derivatives first: they need uniform control flow, and the branches
    // below depend on the material.
    let footprint = fwidth(in.uv);
    let w = max(footprint.x, footprint.y);

    let base = env.palette[in.material];
    let extra = env.palette2[in.material];
    let code = u32(base.w + 0.5);
    let n = normalize(in.normal);
    let to_eye = env.eye_pos[pc.eye].xyz - in.rpos;
    let dist = length(to_eye);
    let view_dir = to_eye / max(dist, 0.0001);

    var col: vec3<f32>;
    if (code >= EMISSIVE) {
        // Makes its own light: constant, or with the house lights.
        let gain = select(1.0, env.ambient_up.w, code == EMISSIVE_HOUSE);
        col = base.rgb * extra.x * gain;
    } else {
        let albedo = base.rgb * pattern(code, in.uv, w);
        let amb = mix(env.ambient_down.rgb, env.ambient_up.rgb, n.y * 0.5 + 0.5);
        // The light from the picture is added to `lit` here.
        let lit = amb * in.light.x + env.house_rgb.rgb * in.light.y;
        // Grazing-angle shine: fabric, or a glossy floor.
        let rim = pow(max(1.0 - abs(dot(n, view_dir)), 0.0), 4.0) * extra.y;
        col = (albedo + vec3<f32>(rim)) * lit;
    }

    let span = max(env.fog_range.y - env.fog_range.x, 0.001);
    let fog = clamp((dist - env.fog_range.x) / span, 0.0, 1.0) * env.fog.w;
    col = mix(col, env.fog.rgb, fog);

    // Dark rooms band in 8 bits otherwise: the shared dither (dither.wgsl),
    // in the target's code space.
    return vec4<f32>(dither_linear(col, in.clip.xy), 1.0);
}
