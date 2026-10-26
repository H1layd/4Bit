// Bloom: bright-pass downsample, then a separable blur (two entry points).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var lin: sampler;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    var o: VsOut;
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    o.pos = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(xy.x, 1.0 - xy.y);
    return o;
}

@fragment
fn bright(i: VsOut) -> @location(0) vec4<f32> {
    let t = 1.0 / vec2<f32>(textureDimensions(src));
    var c = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            c += textureSampleLevel(src, lin, i.uv + vec2<f32>(f32(x), f32(y)) * t * 1.5, 0.0).rgb;
        }
    }
    c /= 9.0;
    let l = max(c.r, max(c.g, c.b));
    return vec4<f32>(c * smoothstep(0.85, 1.6, l), 1.0);
}

fn blur(uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    let t = dir / vec2<f32>(textureDimensions(src));
    var w = array<f32, 5>(0.227, 0.194, 0.121, 0.054, 0.016);
    var c = textureSampleLevel(src, lin, uv, 0.0).rgb * w[0];
    for (var k = 1; k < 5; k++) {
        c += textureSampleLevel(src, lin, uv + t * f32(k) * 1.5, 0.0).rgb * w[k];
        c += textureSampleLevel(src, lin, uv - t * f32(k) * 1.5, 0.0).rgb * w[k];
    }
    return vec4<f32>(c, 1.0);
}

@fragment
fn blur_h(i: VsOut) -> @location(0) vec4<f32> { return blur(i.uv, vec2<f32>(1.0, 0.0)); }
@fragment
fn blur_v(i: VsOut) -> @location(0) vec4<f32> { return blur(i.uv, vec2<f32>(0.0, 1.0)); }
