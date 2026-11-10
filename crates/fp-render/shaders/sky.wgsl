// A sky place's backdrop: a gradient from the horizon colour to the zenith,
// drawn last as one triangle at the far plane, so only the pixels no mesh
// covered are shaded. build.rs joins dither.wgsl and envcommon.wgsl in front.

struct SkyOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> SkyOut {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - 1.0;
    var o: SkyOut;
    // z = w puts it at depth 1.0, behind everything the room drew.
    o.pos = vec4<f32>(xy, 1.0, 1.0);
    o.ndc = xy;
    return o;
}

@fragment
fn fs_main(in: SkyOut) -> @location(0) vec4<f32> {
    let eye = pc.eye;
    let far = env.inv_room_view_proj[eye] * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize(far.xyz / far.w - env.eye_pos[eye].xyz);
    let t = smoothstep(0.0, 0.9, dir.y);
    let col = mix(env.sky_horizon.rgb, env.sky_zenith.rgb, t);
    return vec4<f32>(dither_linear(col, in.pos.xy), 1.0);
}
