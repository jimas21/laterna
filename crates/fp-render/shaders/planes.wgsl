// The video as uploaded or converted planes (I420, NV12 or P010), for
// scene.wgsl (build.rs joins them).

@group(0) @binding(2) var plane0: texture_2d<f32>;
@group(0) @binding(3) var plane1: texture_2d<f32>;
@group(0) @binding(4) var plane2: texture_2d<f32>;

// The video pyramid (src/pyramid.rs) and the sampler that reads it: the
// stored frame at half size and below, with raw codes in the channels, built
// once per video frame. planes_tp10.wgsl declares them too, for the TP10
// eye passes' descriptor buffer.
@group(0) @binding(20) var pyr: texture_2d<f32>;
@group(0) @binding(21) var samp_aniso: sampler;

// The stored frame's samples at `uv` as codes, 0..1 over the whole range:
// scaled but with the offsets still in (a UNORM image can hold them).
fn raw_yuv(uv: vec2<f32>) -> vec3<f32> {
    let y = textureSampleLevel(plane0, samp, uv, 0.0).r;
    var c: vec2<f32>;
    if (params.mode.w == 0u) {
        c = vec2<f32>(textureSampleLevel(plane1, samp, uv, 0.0).r, textureSampleLevel(plane2, samp, uv, 0.0).r);
    } else {
        c = textureSampleLevel(plane1, samp, uv, 0.0).rg;
    }
    return vec3<f32>(y, c) * params.yuv_offset.w;
}

// The stored frame's raw samples at `uv`, scaled and offset but not yet
// converted to RGB (scene.wgsl's `sample_yuv` and supersampling do that).
fn fetch_yuv(uv: vec2<f32>) -> vec3<f32> {
    return raw_yuv(uv) - params.yuv_offset.xyz;
}

fn luma_at(uv: vec2<f32>) -> f32 {
    return textureSampleLevel(plane0, samp, uv, 0.0).r * params.yuv_offset.w;
}

// The pyramid at stored coordinates `p`, filtered over the footprint the
// gradients `gx`, `gy` (per eye pixel, in the same coordinates) span: raw
// codes, offsets still in (fs_pyramid wrote them so). Explicit gradients need
// no uniform control flow.
fn pyramid_raw(p: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec3<f32> {
    return textureSampleGrad(pyr, samp_aniso, p, gx, gy).rgb;
}

// The pyramid's first level: one pixel per 2x2 luma texels of the stored
// frame. At a pixel's centre the bilinear luma tap falls on the corner the
// four texels share, so it averages them exactly, and the 4:2:0 chroma tap
// falls on a chroma texel's centre.
@fragment
fn fs_pyramid(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(raw_yuv(in.ndc * 0.5 + 0.5), 1.0);
}
