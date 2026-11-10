// Per-pixel VR video projection.
//
// Every fragment builds its view ray from the eye's inverse view-projection,
// maps the ray to a video coordinate for the active projection, picks the
// eye's half of a stereo frame, converts YUV to RGB, applies HDR tone mapping
// and picture adjustments, and writes linear colour to an sRGB target.

struct Params {
    inv_view_proj: array<mat4x4<f32>, 2>,
    // Rotation applied to rays in the room frame before projection
    // (inverse of the video's yaw/pitch/roll correction).
    correction: mat4x4<f32>,
    // World -> the room frame the video was centred in (inverse of the
    // recentering anchor's rotation).
    anchor_inv: mat4x4<f32>,
    // World -> screen-local transform for flat screens.
    screen_inv: mat4x4<f32>,
    // x: projection kind (0 flat, 1 equirect, 2 fisheye, 3 eac)
    // y: stereo (0 mono, 1 sbs, 2 tb), z: swap eyes, w: plane layout (0 I420, 1 NV12/P010)
    mode: vec4<u32>,
    // x: h_fov, y: v_fov (radians), z: zoom, w: ipd offset (radians)
    proj: vec4<f32>,
    // x: lens k1, y: lens k2, z: vertical align, w: rotation align (radians)
    lens: vec4<f32>,
    // Fisheye: xy the image circle's diameter in eye-image units
    circle: vec4<f32>,
    // Fisheye: each image circle's centre offset in eye-image units, the
    // left or top image's in xy, the other's in zw
    centres: vec4<f32>,
    // x: screen width, y: screen height, z: curvature arc (radians), w: has video
    screen: vec4<f32>,
    // YUV -> RGB: rgb = m * (yuv * scale - offset)
    yuv_r: vec4<f32>,
    yuv_g: vec4<f32>,
    yuv_b: vec4<f32>,
    // xyz: offsets, w: sample scale (10-bit-in-16 = 65535/1023)
    yuv_offset: vec4<f32>,
    // x brightness, y contrast, z saturation, w gamma
    picture: vec4<f32>,
    // x sharpen, y transfer (0 sdr, 1 pq, 2 hlg), z wide gamut, w background alpha
    extra: vec4<f32>,
    // x width, y height, z 1/width, w 1/height (luma texels)
    tex_size: vec4<f32>,
    // Premultiplied; alpha 0 lets passthrough show through. The shader no
    // longer reads it: the clear colour is the same and the pipeline blends
    // over it. Kept for the layout (the TP10 parameters mirror it).
    bg_color: vec4<f32>,
    // Chroma key: x, y the key colour's Cb, Cr; z similarity; w smoothness
    key: vec4<f32>,
    // x spill, y on, z alpha-packed mask scale (0 off)
    key2: vec4<f32>,
    // xyz: the viewer's offset (height, forward) in the room frame, metres,
    // for spherical video; w: radius of the sphere it is drawn on when offset
    viewer: vec4<f32>,
    // x: clockwise quarter turns that show the frame upright
    // y: quality bits (QUALITY_*)
    // z: levels of the video pyramid built for this frame (0: none; VQ5)
    display: vec4<u32>,
    // HDR, as PQ codes: x the scene's peak, y the screen's white
    hdr: vec4<f32>,
    // A Q10C picture's overlapping bands (planes_tp10.wgsl), luma then
    // chroma: x first-row step, y rows, z the plane's rows, w count
    tp10: vec4<f32>,
    tp10_uv: vec4<f32>,
};

struct PushConstants {
    eye: u32,
};

// The video planes, `fetch_yuv` (raw scaled/offset YUV at a point) and `luma_at` come from
// planes.wgsl, or planes_tp10.wgsl for the pipeline sampling Q10C in place (build.rs).
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var samp: sampler;
var<immediate> pc: PushConstants;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    // One triangle covering the screen.
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - 1.0;
    var o: VsOut;
    o.pos = vec4<f32>(xy, 0.0, 1.0);
    o.ndc = xy;
    return o;
}

const PI: f32 = 3.14159265358979;
const INVALID: vec2<f32> = vec2<f32>(-1.0, -1.0);

// Quality features, switched by bits of `params.display.y` (Quality in
// params.rs; uniform, so derivative uniformity is unchanged):
//   1 dither (dither.wgsl), 2 supersample, 4 edge AA.
// Edge AA: analytic antialiasing of the picture's edge (flat or curved screen,
// and the fisheye image circle): edge pixels blend towards the background by
// the fraction of the pixel the picture covers; off gives a hard edge.
// Supersample: four-tap rotated-grid supersampling of a flat screen's picture
// where it is minified (more than one video texel per eye pixel); off gives
// one tap.
// Pyramid (VQ5): where the picture is minified past what four taps hold, the
// frame's half-size mip chain (built per video frame, `display.z` levels) is
// sampled instead, trilinear and anisotropic.
const QUALITY_DITHER: u32 = 1u;
const QUALITY_SUPERSAMPLE: u32 = 2u;
const QUALITY_EDGE_AA: u32 = 4u;
const QUALITY_PYRAMID: u32 = 8u;

fn rot_y(a: f32) -> mat3x3<f32> {
    let c = cos(a); let s = sin(a);
    return mat3x3<f32>(vec3<f32>(c, 0.0, -s), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(s, 0.0, c));
}
fn rot_x(a: f32) -> mat3x3<f32> {
    let c = cos(a); let s = sin(a);
    return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, c, s), vec3<f32>(0.0, -s, c));
}
fn rot_z(a: f32) -> mat3x3<f32> {
    let c = cos(a); let s = sin(a);
    return mat3x3<f32>(vec3<f32>(c, s, 0.0), vec3<f32>(-s, c, 0.0), vec3<f32>(0.0, 0.0, 1.0));
}

// Angle-preserving zoom: scales the angle from the forward axis.
fn zoom_dir(d: vec3<f32>, zoom: f32) -> vec3<f32> {
    if (abs(zoom - 1.0) < 1e-4) { return d; }
    let theta = acos(clamp(-d.z, -1.0, 1.0));
    let r = length(d.xy);
    if (r < 1e-6) { return d; }
    let t2 = min(theta / zoom, PI);
    let phi = d.xy / r;
    return vec3<f32>(phi * sin(t2), -cos(t2));
}

// The direction from the sphere's centre to where a ray from `p` (inside a
// sphere of radius `r`) along `d` meets it: seen from off the centre, near
// parts of the picture grow and slide against far ones. Both eyes use the
// same `p`, so the video's own stereo depth is kept.
fn off_centre(d: vec3<f32>, p: vec3<f32>, r: f32) -> vec3<f32> {
    if (dot(p, p) < 1e-8) { return d; }
    let q = p * min(1.0, 0.9 * r / length(p));
    let b = dot(q, d);
    let t = -b + sqrt(b * b - dot(q, q) + r * r);
    return normalize(q + t * d);
}

// Ray (forward = -Z, right = +X, up = +Y) to normalised video coordinates
// (0..1, origin top-left) for spherical projections.
fn equirect_uv(d: vec3<f32>, h_fov: f32, v_fov: f32) -> vec2<f32> {
    let lon = atan2(d.x, -d.z);
    let lat = asin(clamp(d.y, -1.0, 1.0));
    var u = 0.5 + lon / h_fov;
    let v = 0.5 - lat / v_fov;
    if (h_fov >= 2.0 * PI - 1e-3) {
        u = fract(u);
    } else if (u < 0.0 || u > 1.0) {
        return INVALID;
    }
    if (v < 0.0 || v > 1.0) { return INVALID; }
    return vec2<f32>(u, v);
}

// xy: video coordinates (unclamped, so they stay continuous across the image
// circle's edge and screen-space derivatives are valid there); z: signed
// distance inside the image circle in units of the circle's radius fraction
// (0.5 - r, negative outside) before lens distortion.
fn fisheye_uv(d: vec3<f32>, fov: f32, k1: f32, k2: f32, size: vec2<f32>, centre: vec2<f32>) -> vec3<f32> {
    let theta = acos(clamp(-d.z, -1.0, 1.0));
    var r = theta / fov; // 0.5 at the edge of the image circle
    let inside = 0.5 - r;
    let r2 = r * r * 4.0;
    r = r * (1.0 + k1 * r2 + k2 * r2 * r2);
    let len = length(d.xy);
    var dir = vec2<f32>(0.0, 0.0);
    if (len > 1e-6) { dir = d.xy / len; }
    let uv = centre + vec2<f32>(dir.x, -dir.y) * r * size;
    return vec3<f32>(uv, inside);
}

// YouTube equi-angular cubemap, 3x2 layout as FFmpeg's v360 "eac" output:
// top row left|front|right, bottom row down|back|up, bottom faces rotated.
fn eac_face_uv(a: f32, b: f32) -> vec2<f32> {
    // Equi-angular warp of cube-face coordinates in [-1, 1].
    return vec2<f32>(atan(a) * 4.0 / PI, atan(b) * 4.0 / PI) * 0.5 + 0.5;
}

fn eac_uv(d: vec3<f32>, h_fov: f32) -> vec2<f32> {
    let ax = abs(d.x); let ay = abs(d.y); let az = abs(d.z);
    var face: i32;
    var a: f32; var b: f32; // face-local: a right, b down
    if (az >= ax && az >= ay) {
        if (d.z < 0.0) { face = 1; a = d.x / az; b = -d.y / az; }      // front
        else { face = 4; a = -d.x / az; b = -d.y / az; }               // back
    } else if (ax >= ay) {
        if (d.x > 0.0) { face = 2; a = d.z / ax; b = -d.y / ax; }      // right
        else { face = 0; a = -d.z / ax; b = -d.y / ax; }               // left
    } else {
        if (d.y > 0.0) { face = 5; a = d.x / ay; b = d.z / ay; }       // up
        else { face = 3; a = d.x / ay; b = -d.z / ay; }                // down
    }
    var f = eac_face_uv(a, b);
    var col: f32; var row: f32;
    switch face {
        case 0: { col = 0.0; row = 0.0; }
        case 1: { col = 1.0; row = 0.0; }
        case 2: { col = 2.0; row = 0.0; }
        // Orientations verified against FFmpeg's v360 EAC output
        // (tests/projections.rs): down and up are anti-transposed, back is
        // rotated 90 degrees.
        case 3: { col = 0.0; row = 1.0; f = vec2<f32>(1.0 - f.y, 1.0 - f.x); }
        case 4: { col = 1.0; row = 1.0; f = vec2<f32>(1.0 - f.y, f.x); }
        default: { col = 2.0; row = 1.0; f = vec2<f32>(1.0 - f.y, 1.0 - f.x); }
    }
    let uv = vec2<f32>((col + f.x) / 3.0, (row + f.y) / 2.0);
    if (h_fov < 2.0 * PI - 1e-3) {
        // EAC 180: only the front half of the layout is stored (columns
        // mapped onto the full width).
        if (d.z > 0.0) { return INVALID; }
    }
    return uv;
}

// xy: coordinates on the screen, 0..1 inside it but NOT clamped or rejected
// outside (continuous across the edge, so derivatives are valid there); z: 1
// when the ray meets the screen's plane or cylinder at all, else 0.
fn flat_uv(origin: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    let o = (params.screen_inv * vec4<f32>(origin, 1.0)).xyz;
    let dir = normalize((params.screen_inv * vec4<f32>(d, 0.0)).xyz);
    let w = params.screen.x; let h = params.screen.y; let arc = params.screen.z;
    var p: vec3<f32>;
    var u: f32;
    if (arc < 1e-3) {
        if (dir.z >= -1e-6) { return vec3<f32>(INVALID, 0.0); }
        let t = -o.z / dir.z;
        if (t <= 0.0) { return vec3<f32>(INVALID, 0.0); }
        p = o + t * dir;
        u = p.x / w + 0.5;
    } else {
        // Concave cylinder of radius R centred at (0, 0, R), screen centre at origin.
        let r = w / arc;
        let oc = vec2<f32>(o.x, o.z - r);
        let dd = vec2<f32>(dir.x, dir.z);
        let qa = dot(dd, dd);
        let qb = 2.0 * dot(oc, dd);
        let qc = dot(oc, oc) - r * r;
        let disc = qb * qb - 4.0 * qa * qc;
        if (disc < 0.0) { return vec3<f32>(INVALID, 0.0); }
        let t = (-qb + sqrt(disc)) / (2.0 * qa);
        if (t <= 0.0) { return vec3<f32>(INVALID, 0.0); }
        p = o + t * dir;
        let phi = atan2(p.x, r - p.z);
        u = 0.5 + phi * r / w;
    }
    let v = 0.5 - p.y / h;
    return vec3<f32>(u, v, 1.0);
}

fn to_eye_rect(uv: vec2<f32>, eye: u32) -> vec2<f32> {
    let slot = f32(eye ^ params.mode.z);
    switch params.mode.y {
        case 1u: { return vec2<f32>(uv.x * 0.5 + slot * 0.5, uv.y); }
        case 2u: { return vec2<f32>(uv.x, uv.y * 0.5 + slot * 0.5); }
        default: { return uv; }
    }
}

// Where a point of the upright picture is in the stored frame.
fn stored_uv(uv: vec2<f32>) -> vec2<f32> {
    switch params.display.x {
        case 1u: { return vec2<f32>(uv.y, 1.0 - uv.x); }
        case 2u: { return 1.0 - uv; }
        case 3u: { return vec2<f32>(1.0 - uv.y, uv.x); }
        default: { return uv; }
    }
}

// rgb = m * yuv is linear in yuv (the offset is already subtracted in
// fetch_yuv, and the matrix is a plain 3x3), so averaging fetched samples
// and converting once equals converting each and averaging.
fn yuv_to_rgb(yuv: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.yuv_r.xyz, yuv), dot(params.yuv_g.xyz, yuv), dot(params.yuv_b.xyz, yuv));
}

fn sample_yuv(uv: vec2<f32>) -> vec3<f32> {
    return yuv_to_rgb(fetch_yuv(uv));
}

// The upright picture's size in video texels, the eye's share of it.
fn eye_texels() -> vec2<f32> {
    var size = params.tex_size.xy;
    if ((params.display.x & 1u) == 1u) { size = size.yx; }
    if (params.mode.y == 1u) { size.x = size.x * 0.5; }
    if (params.mode.y == 2u) { size.y = size.y * 0.5; }
    return max(size, vec2<f32>(1.0));
}

// A point of the screen clamped into the picture. For a stereo frame it stays
// `texels` luma texels (at least one, which is half a 4:2:0 chroma texel)
// inside the eye's own half, so the linear filter never blends in the other
// eye's border. The pyramid asks for its footprint's worth (VQ5).
fn clamp_to_eye_by(p: vec2<f32>, texels: f32) -> vec2<f32> {
    var m = vec2<f32>(0.0);
    if (params.mode.y != 0u) { m = min(vec2<f32>(max(texels, 1.0)) / eye_texels(), vec2<f32>(0.25)); }
    return clamp(p, m, vec2<f32>(1.0) - m);
}

fn clamp_to_eye(p: vec2<f32>) -> vec2<f32> {
    return clamp_to_eye_by(p, 1.0);
}

// The linear parts of to_eye_rect and stored_uv, for a step across the
// screen (the derivatives the pyramid's filtering takes): an eye's half of a
// stereo frame is half as wide or tall, and a turned frame swaps and flips
// its axes, without the offsets.
fn eye_rect_dir(d: vec2<f32>) -> vec2<f32> {
    switch params.mode.y {
        case 1u: { return vec2<f32>(d.x * 0.5, d.y); }
        case 2u: { return vec2<f32>(d.x, d.y * 0.5); }
        default: { return d; }
    }
}

fn stored_dir(d: vec2<f32>) -> vec2<f32> {
    switch params.display.x {
        case 1u: { return vec2<f32>(d.y, -d.x); }
        case 2u: { return -d; }
        case 3u: { return vec2<f32>(-d.y, d.x); }
        default: { return d; }
    }
}

// One tap at a point of the screen (0..1), clamped into the eye's picture.
fn fetch_tap(p: vec2<f32>, eye: u32) -> vec3<f32> {
    return fetch_yuv(stored_uv(to_eye_rect(clamp_to_eye(p), eye)));
}

// Four taps on a rotated grid inside the eye pixel's footprint on the screen
// (offsets of 1/8 and 3/8 of a pixel along its two screen-space axes), so
// every row and column of the pixel is sampled at a different position.
// Averaged in the video's own (gamma-encoded Y'CbCr) space, as the single
// tap was; not in linear light.
fn sample_yuv_aa(uv: vec2<f32>, dx: vec2<f32>, dy: vec2<f32>, eye: u32) -> vec3<f32> {
    let a = fetch_tap(uv + dx * -0.375 + dy * -0.125, eye);
    let b = fetch_tap(uv + dx * 0.125 + dy * -0.375, eye);
    let c = fetch_tap(uv + dx * 0.375 + dy * 0.125, eye);
    let e = fetch_tap(uv + dx * -0.125 + dy * 0.375, eye);
    return yuv_to_rgb((a + b + c + e) * 0.25);
}

// How many video texels one eye pixel spans on the screen, the larger of its
// two axes. `dx`/`dy` are the screen coordinate's derivatives.
fn texel_footprint(dx: vec2<f32>, dy: vec2<f32>) -> f32 {
    let size = eye_texels();
    return max(length(dx * size), length(dy * size));
}

// BT.2390's EETF on PQ code `e`, for a source peaking at PQ `src` and a
// display peaking at PQ `dst`: unchanged below the knee, a Hermite spline
// above it.
fn eetf(e: f32, src: f32, dst: f32) -> f32 {
    if (src <= dst) { return e; }
    let e1 = e / src;
    let max_lum = dst / src;
    let ks = max(1.5 * max_lum - 0.5, 0.0);
    if (e1 < ks) { return e; }
    let t = (min(e1, 1.0) - ks) / (1.0 - ks);
    let t2 = t * t;
    let t3 = t2 * t;
    let p = (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks)
        + (-2.0 * t3 + 3.0 * t2) * max_lum;
    return p * src;
}

// Nits, tone mapped through the brightest channel so hue is kept; 1.0 is
// the screen's white.
fn tone_map(c: vec3<f32>) -> vec3<f32> {
    let m = max3(c);
    if (m <= 0.0) { return vec3<f32>(0.0); }
    let mapped = pq_eotf1(eetf(pq_oetf1(m), params.hdr.x, params.hdr.y));
    return c * (mapped / (m * 203.0));
}

// The ACES reference gamut compression, sized for BT.2020 in BT.709 (see
// src/hdr.rs): each channel's distance below the brightest one is
// compressed past 0.8, bringing the most saturated BT.2020 colours to
// BT.709's edge and leaving most colours alone.
const GAMUT_THRESHOLD: f32 = 0.8;
const GAMUT_LIMIT: vec3<f32> = vec3<f32>(1.60, 1.09, 1.12);
const GAMUT_POWER: f32 = 1.2;

fn compress_distance(d: f32, limit: f32) -> f32 {
    if (d < GAMUT_THRESHOLD) { return d; }
    let s = (limit - GAMUT_THRESHOLD)
        / pow(pow((1.0 - GAMUT_THRESHOLD) / (limit - GAMUT_THRESHOLD), -GAMUT_POWER) - 1.0, 1.0 / GAMUT_POWER);
    let x = (d - GAMUT_THRESHOLD) / s;
    return GAMUT_THRESHOLD + s * x / pow(1.0 + pow(x, GAMUT_POWER), 1.0 / GAMUT_POWER);
}

fn gamut_compress(c: vec3<f32>) -> vec3<f32> {
    let ach = max3(c);
    if (ach <= 0.0) { return vec3<f32>(0.0); }
    let d = (ach - c) / ach;
    let cd = vec3<f32>(
        compress_distance(d.r, GAMUT_LIMIT.r),
        compress_distance(d.g, GAMUT_LIMIT.g),
        compress_distance(d.b, GAMUT_LIMIT.b));
    return ach - cd * ach;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
}

// A sample of the picture (the video's own colour values, YUV converted) as
// linear light for the screen: HDR to its tone curve, then the picture
// adjustments. Everything the picture shows goes through here, and so does
// the light it gives the room (`fs_glow`). No dither: that is the caller's.
fn display_linear(rgb: vec3<f32>) -> vec3<f32> {
    // HDR: to BT.709 nits, into its gamut, then the tone curve, which also
    // keeps every channel within the screen's white.
    var lin: vec3<f32>;
    let transfer = u32(params.extra.y + 0.5);
    let wide = params.extra.z > 0.5;
    if (transfer == 1u || transfer == 2u) {
        var nits = hdr_nits(rgb, transfer, wide);
        if (wide) { nits = gamut_compress(nits); }
        lin = tone_map(nits);
    } else {
        lin = srgb_to_linear(rgb);
        if (wide) {
            lin = gamut_compress(bt2020_to_709(lin));
            lin = lin / max(max3(lin), 1.0);
        }
    }

    // Picture adjustments, applied in a perceptual (gamma) space.
    var g = pow(max(lin, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    g = (g - 0.5) * params.picture.y + 0.5 + params.picture.x;
    let luma = dot(g, vec3<f32>(0.2126, 0.7152, 0.0722));
    g = mix(vec3<f32>(luma), g, params.picture.z);
    g = pow(max(g, vec3<f32>(0.0)), vec3<f32>(1.0 / max(params.picture.w, 0.05)));
    return srgb_to_linear(clamp(g, vec3<f32>(0.0), vec3<f32>(1.0)));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let eye = pc.eye;
    let near = params.inv_view_proj[eye] * vec4<f32>(in.ndc, 0.0, 1.0);
    let far = params.inv_view_proj[eye] * vec4<f32>(in.ndc, 1.0, 1.0);
    let origin = near.xyz / near.w;
    let world_dir = normalize(far.xyz / far.w - origin);

    if (params.screen.w < 0.5) {
        return vec4<f32>(0.0);
    }

    var uv: vec2<f32>;
    // False where the ray misses the picture outright (nothing to blend).
    var hit = true;
    // Fisheye: signed distance inside the image circle; large elsewhere.
    var inside = 1.0;
    let kind = params.mode.x;
    if (kind == 0u) {
        let f = flat_uv(origin, world_dir);
        uv = f.xy;
        hit = f.z > 0.5;
    } else {
        // In the room frame the video was centred in, so per-eye stereo
        // corrections stay the eyes' own; then the picture's rotation.
        let s = select(-0.5, 0.5, eye == 1u);
        var d = (params.anchor_inv * vec4<f32>(world_dir, 0.0)).xyz;
        d = rot_y(params.proj.w * s) * d;
        d = rot_x(params.lens.z * s) * d;
        d = rot_z(params.lens.w * s) * d;
        d = normalize((params.correction * vec4<f32>(d, 0.0)).xyz);
        d = off_centre(d, (params.correction * vec4<f32>(params.viewer.xyz, 0.0)).xyz,
                       params.viewer.w);
        d = zoom_dir(d, params.proj.z);
        if (kind == 1u) {
            uv = equirect_uv(d, params.proj.x, params.proj.y);
            hit = uv.x >= 0.0;
        } else if (kind == 2u) {
            // The centres belong to the images, so they follow a swap.
            let slot = select(0u, eye ^ params.mode.z, params.mode.y != 0u);
            let centre = 0.5 + select(params.centres.xy, params.centres.zw, slot == 1u);
            let f = fisheye_uv(d, params.proj.x, params.lens.x, params.lens.y, params.circle.xy, centre);
            uv = f.xy;
            inside = f.z;
        } else {
            uv = eac_uv(d, params.proj.x);
            hit = uv.x >= 0.0;
        }
    }

    // Screen-space derivatives, taken here: derivatives need uniform control
    // flow, and everything above branches only on uniforms, while the early
    // returns below depend on the pixel.
    let duv_dx = dpdx(uv);
    let duv_dy = dpdy(uv);
    let dinside = fwidth(inside);

    // Coverage of this pixel by the picture. Flat and curved screens are
    // edged by uv in 0..1 on both axes; a fisheye also by its image circle.
    // A pixel centred on the edge is half covered.
    var cov = 1.0;
    if (kind == 0u || kind == 2u) {
        if ((params.display.y & QUALITY_EDGE_AA) != 0u) {
            // display.w: the blend's width (low 16 bits) and curve (high), in
            // hundredths; 0 means the old one-pixel linear blend.
            let ew = max(f32(params.display.w & 0xffffu) / 100.0, 1.0);
            let eg = select(1.0, f32(params.display.w >> 16u) / 100.0, (params.display.w >> 16u) > 0u);
            let fw = max(abs(duv_dx) + abs(duv_dy), vec2<f32>(1e-6)) * ew;
            let e = clamp(min(uv, vec2<f32>(1.0) - uv) / fw + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
            // Smoothstep over the wider ramp: no corner where it meets full
            // cover or the background, which the lenses' resampling shows.
            let es = e * e * (vec2<f32>(3.0) - 2.0 * e);
            cov = pow(es.x * es.y, eg);
            if (kind == 2u) {
                cov = min(cov, clamp(inside / max(dinside, 1e-6) + 0.5, 0.0, 1.0));
            }
        } else if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || inside < 0.0) {
            cov = 0.0;
        }
    }
    // Also false for NaN.
    if (!hit || !(cov > 0.0)) {
        return vec4<f32>(0.0);
    }

    // Edge pixels centred just outside the picture read its border texel,
    // never the other eye's half of a stereo frame.
    let uvc = clamp_to_eye(uv);
    let tuv = stored_uv(to_eye_rect(uvc, eye));
    var rgb = sample_yuv(tuv);
    // How many video texels one eye pixel spans here: 1 is one to one, more is
    // minified. Supersampling and sharpening both key off it.
    let fp = texel_footprint(duv_dx, duv_dy);
    // VQ5: the share of the pyramid in the picture, 0 to 1 over 1.0 to 1.6
    // texels per pixel (0 where there is no pyramid for this frame).
    var pyr_t = 0.0;
    if (kind == 0u && (params.display.y & QUALITY_PYRAMID) != 0u && params.display.z > 0u) {
        pyr_t = smoothstep(1.0, 1.6, fp);
    }
    // Fades in from one tap to four as the picture gets minified, so a
    // tilted or curved screen shows no line where the mode would flip.
    // Where the pyramid takes over entirely the four taps aren't needed.
    if ((params.display.y & QUALITY_SUPERSAMPLE) != 0u && kind == 0u) {
        let ss = smoothstep(0.9, 1.3, fp);
        if (ss > 0.0 && pyr_t < 1.0) {
            rgb = mix(rgb, sample_yuv_aa(uvc, duv_dx, duv_dy, eye), ss);
        }
    }
    if (pyr_t > 0.0) {
        // The pyramid is of the stored frame and takes the footprint as
        // gradients: the screen's steps mapped into stored coordinates. The
        // point stays a footprint's width inside a stereo half, so the
        // filter reaches only the eye's own picture.
        let p = stored_uv(to_eye_rect(clamp_to_eye_by(uv, fp), eye));
        let gx = stored_dir(eye_rect_dir(duv_dx));
        let gy = stored_dir(eye_rect_dir(duv_dy));
        let codes = pyramid_raw(p, gx, gy) - params.yuv_offset.xyz;
        rgb = mix(rgb, yuv_to_rgb(codes), pyr_t);
    }

    // Chroma key (as OBS does it): pixels whose chroma is near the key
    // colour's turn see-through, with a soft edge, and the key colour's
    // fringe is desaturated out of what stays.
    var alpha = 1.0;
    if (params.key2.y > 0.5) {
        let c = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        let cbcr = vec2<f32>(dot(c, vec3<f32>(-0.1146, -0.3854, 0.5)), dot(c, vec3<f32>(0.5, -0.4542, -0.0458)));
        let d = distance(cbcr, params.key.xy) - params.key.z;
        alpha = pow(clamp(d / max(params.key.w, 1e-4), 0.0, 1.0), 1.5);
        let spill = pow(clamp(d / max(params.key2.x, 1e-4), 0.0, 1.0), 1.5);
        rgb = mix(vec3<f32>(dot(c, vec3<f32>(0.2126, 0.7152, 0.0722))), rgb, spill);
    }

    // Alpha-packed passthrough video (`_alpha` names): each eye's mask, scaled
    // down, sits in the red channel outside the fisheye circles, centred on
    // the frame's top middle for the left half and on its corner for the
    // right half, wrapping round the frame's edges.
    if (params.key2.z > 0.0) {
        let slot = f32(eye ^ params.mode.z);
        let k = params.key2.z;
        let m = fract(vec2<f32>((1.0 - slot) * 0.5 + (uvc.x - 0.5) * k * 0.5, (uvc.y - 0.5) * k));
        alpha *= clamp((sample_yuv(stored_uv(m)).r - 0.08) / 0.84, 0.0, 1.0);
    }

    // Sharpen: unsharp mask on luma. It fades out between 0.8 and 1.25 texels
    // per pixel (on a minified picture it would amplify aliasing and shimmer)
    // and never goes past the range of the texels around (no halos).
    let k = params.extra.x * (1.0 - smoothstep(0.8, 1.25, fp));
    if (k > 0.0) {
        let dx = vec2<f32>(params.tex_size.z, 0.0);
        let dy = vec2<f32>(0.0, params.tex_size.w);
        let yc = luma_at(tuv);
        let y0 = luma_at(tuv + dx);
        let y1 = luma_at(tuv - dx);
        let y2 = luma_at(tuv + dy);
        let y3 = luma_at(tuv - dy);
        let blur = (y0 + y1 + y2 + y3) * 0.25;
        let lo = min(min(yc, y0), min(min(y1, y2), y3));
        let hi = max(max(yc, y0), max(max(y1, y2), y3));
        let d = clamp((yc - blur) * k * 2.0, lo - yc, hi - yc);
        // Luma onto the colour's own scale (the matrix's Y column).
        rgb += vec3<f32>(d * params.yuv_r.x);
    }

    var col = display_linear(rgb);
    // Dither last, in the target's code space (dither.wgsl).
    if ((params.display.y & QUALITY_DITHER) != 0u) {
        col = dither_linear(col, in.pos.xy);
    }
    // Premultiplied: the pipeline blends this over whatever is behind the
    // picture (ONE, ONE_MINUS_SRC_ALPHA), the clear colour or a place. Pixels
    // the picture does not cover, and keyed-out ones, return less than full
    // alpha so that shows through; a partly covered edge pixel is a plain mix
    // towards it.
    return vec4<f32>(col * alpha * cov, alpha * cov);
}

// The light the picture gives its place: a 4x2 image, each pixel the mean of
// 8x8 taps of its cell of the picture (eye 0's half of a stereo frame), after
// `display_linear`, so it is what the screen shows. Drawn into a tiny target
// by its own pipelines (src/glow.rs); `fs_main` is not touched by it. Only
// `sample_yuv` is used, so planes.wgsl and planes_tp10.wgsl both serve it.
const GLOW_COLS: u32 = 4u;
const GLOW_ROWS: u32 = 2u;
const GLOW_TAPS: u32 = 8u;

@fragment
fn fs_glow(in: VsOut) -> @location(0) vec4<f32> {
    // The pixel's column and row are the cell: row 0 is the picture's top.
    let cell = vec2<f32>(floor(in.pos.xy));
    let cells = vec2<f32>(f32(GLOW_COLS), f32(GLOW_ROWS));
    let taps = f32(GLOW_TAPS);
    var sum = vec3<f32>(0.0);
    for (var j = 0u; j < GLOW_TAPS; j++) {
        for (var i = 0u; i < GLOW_TAPS; i++) {
            let p = (cell + (vec2<f32>(f32(i), f32(j)) + 0.5) / taps) / cells;
            let tap = sample_yuv(stored_uv(to_eye_rect(clamp_to_eye(p), 0u)));
            sum += display_linear(tap);
        }
    }
    return vec4<f32>(sum / (taps * taps), 1.0);
}
