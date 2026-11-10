// A place's meshes: walls, seats, steps, a floor. Lit per vertex by what the
// place's builder baked (ambient occlusion and the house lights' share) and by
// the picture (the screen's patches, see `screen_light`), so the work per
// pixel is a palette lookup, a filtered pattern, fog and dither. The one
// per-pixel term from the picture is a glossy floor's reflection of it.
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
    // The picture's light on the surface, linear (irradiance over pi).
    @location(5) screen: vec3<f32>,
};

// Pattern codes, the palette's w (`pattern_code` in env_pass.rs).
const CARPET: u32 = 1u;
const FABRIC: u32 = 2u;
const PANEL: u32 = 3u;
const WOOD: u32 = 4u;
const EMISSIVE: u32 = 6u;
const EMISSIVE_HOUSE: u32 = 7u;
const EMISSIVE_PICTURE: u32 = 8u;

const PI: f32 = 3.14159265358979;
// The glossy lobe is cos^12 around the reflected view ray: GLOSS_NORM is its
// normalisation (12 + 2) / 2 pi, and GLOSS_GAIN how much of the picture a
// glossy floor gives back, times its sheen.
const GLOSS_NORM: f32 = 2.228;
const GLOSS_GAIN: f32 = 0.25;

// How much of the screen's light `i` reaches `p`: the light from patch `i`
// comes with `l` (towards it, shortened when the patch is close, which keeps
// a point right by the screen from being blinded) and the patch's solid
// angle times the cosine it shines with, over `pi`. Zero for a patch that is
// off (area 0).
struct Lamp {
    l: vec3<f32>,
    amount: f32,
};

fn lamp_at(i: u32, p: vec3<f32>) -> Lamp {
    let area = env.patch_pos[i].w;
    let v = env.patch_pos[i].xyz - p;
    let d2 = max(dot(v, v), area * 0.3);
    let l = v * inverseSqrt(max(d2, 1e-6));
    let shines = max(dot(env.screen_n.xyz, -l), 0.0);
    var o: Lamp;
    o.l = l;
    o.amount = select(0.0, area * shines / (PI * max(d2, 1e-6)), area > 0.0);
    return o;
}

// The picture's light on a surface at `p` facing `n`, summed over the screen's
// patches: each patch lights it by the cosine of the angle between `n` and
// the patch. Linear, times the albedo later (the 1/pi of a matte surface is
// in `lamp_at`). No shadows: seats do not shade the seats behind them.
fn screen_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    var e = vec3<f32>(0.0);
    if (env.screen_n.w <= 0.0) { return e; }
    for (var i = 0u; i < 8u; i++) {
        let lamp = lamp_at(i, p);
        e += env.patch_rgb[i].rgb * (lamp.amount * max(dot(n, lamp.l), 0.0));
    }
    return e;
}

// A glossy floor's mirror image of the picture: each patch through a lobe
// around the reflected view ray (`pow(x, 12)` as products, no pow).
fn screen_gloss(p: vec3<f32>, n: vec3<f32>, view_dir: vec3<f32>) -> vec3<f32> {
    var s = vec3<f32>(0.0);
    if (env.screen_n.w <= 0.0) { return s; }
    let r = reflect(-view_dir, n);
    for (var i = 0u; i < 8u; i++) {
        let lamp = lamp_at(i, p);
        let c = max(dot(r, lamp.l), 0.0);
        let c2 = c * c;
        let c4 = c2 * c2;
        let c8 = c4 * c4;
        // The patch's solid angle (without the 1/pi) times the lobe.
        s += env.patch_rgb[i].rgb * (lamp.amount * PI * c8 * c4);
    }
    return s * GLOSS_NORM;
}

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
    o.screen = screen_light(p, v.normal);
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
        // Makes its own light: constant, with the house lights, or with the
        // picture's mean colour (a little over dark, so the window shows).
        if (code == EMISSIVE_PICTURE) {
            var mean = vec3<f32>(0.0);
            for (var i = 0u; i < 8u; i++) { mean += env.patch_rgb[i].rgb; }
            col = base.rgb * (mean * (extra.x / 8.0) + vec3<f32>(0.03));
        } else {
            let gain = select(1.0, env.ambient_up.w, code == EMISSIVE_HOUSE);
            col = base.rgb * extra.x * gain;
        }
    } else {
        let albedo = base.rgb * pattern(code, in.uv, w);
        let amb = mix(env.ambient_down.rgb, env.ambient_up.rgb, n.y * 0.5 + 0.5);
        // The picture's light (per vertex) is lit by the same occlusion.
        let lit = amb * in.light.x + env.house_rgb.rgb * in.light.y + in.screen * in.light.x;
        // Grazing-angle shine: fabric, or a glossy floor.
        let rim = pow(max(1.0 - abs(dot(n, view_dir)), 0.0), 4.0) * extra.y;
        col = (albedo + vec3<f32>(rim)) * lit;
        // A plain surface with sheen is glossy: it also gives the picture back.
        if (code == 0u && extra.y > 0.0) {
            col += screen_gloss(in.rpos, n, view_dir) * (extra.y * GLOSS_GAIN);
        }
    }

    let span = max(env.fog_range.y - env.fog_range.x, 0.001);
    let fog = clamp((dist - env.fog_range.x) / span, 0.0, 1.0) * env.fog.w;
    col = mix(col, env.fog.rgb, fog);

    // Dark rooms band in 8 bits otherwise: the shared dither (dither.wgsl),
    // in the target's code space.
    return vec4<f32>(dither_linear(col, in.clip.xy), 1.0);
}
