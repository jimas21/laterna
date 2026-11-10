// Dither for 8-bit render targets, shared: build.rs joins this file in front
// of scene.wgsl (and scene_tp10), and any pass that writes a gradient into an
// sRGB target (the environments room pass) should end with `dither_linear`.
//
// The noise is added in the target's own code space, never on linear light:
// the colour is encoded with the real sRGB curve, nudged by up to half a code,
// and decoded again, so the target's encode puts back code + noise. One
// noise value serves all three channels (no colour speckle). The pattern is
// fixed per pixel (no frame index), so it cannot shimmer or crawl.

// Interleaved gradient noise (Jimenez, 2014), roughly uniform in [0, 1).
fn dither_ign(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

// Linear light to the sRGB code (0..1), the exact piecewise curve.
fn dither_encode(l: vec3<f32>) -> vec3<f32> {
    let hi = 1.055 * pow(max(l, vec3<f32>(0.0031308)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(l * 12.92, hi, l > vec3<f32>(0.0031308));
}

// The sRGB code (0..1) back to linear light.
fn dither_decode(c: vec3<f32>) -> vec3<f32> {
    let hi = pow((max(c, vec3<f32>(0.04045)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(c / 12.92, hi, c > vec3<f32>(0.04045));
}

// `lin` (linear light, about to be written to an sRGB-format target) with
// +-0.5 of an 8-bit code of noise. `frag` is the fragment's pixel position
// (`@builtin(position).xy`); callers that draw both eyes should offset it per
// eye so the two eyes' noise differs. Pure black and white keep their codes.
fn dither_linear(lin: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    let code = dither_encode(clamp(lin, vec3<f32>(0.0), vec3<f32>(1.0)));
    let noise = (dither_ign(frag) - 0.5) / 255.0;
    return dither_decode(clamp(code + vec3<f32>(noise), vec3<f32>(0.0), vec3<f32>(1.0)));
}
