// Final pass: low-res frame -> screen. Bodycam lens, oversharpen, grading, grain, chroma noise,
// line jitter, vignette, dither. Port of PSXPost.shader. HUD is composited on top, untouched by the effects.
struct Post {
    grade: vec4<f32>,     // black crush, saturation, contrast, exposure
    tint: vec4<f32>,
    noise: vec4<f32>,     // grain, chroma noise, jitter, vignette
    misc: vec4<f32>,      // fade, levels, fisheye, sharpen
    misc2: vec4<f32>,     // analog, time, exposure, bloom
};

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var hud: texture_2d<f32>;
@group(0) @binding(2) var lin: sampler;
@group(0) @binding(3) var<uniform> p: Post;
@group(0) @binding(4) var fx: texture_2d<f32>;     // rgb = flash beam dust, a = ambient occlusion
@group(0) @binding(5) var bloom: texture_2d<f32>;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    var o: VsOut;
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    o.pos = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(xy.x, 1.0 - xy.y);
    return o;
}

fn hash(q: vec2<f32>) -> f32 { return fract(sin(dot(q, vec2<f32>(12.9898, 78.233))) * 43758.5453); }

fn bayer(i: i32) -> f32 {
    var b = array<f32, 16>(0., 8., 2., 10., 12., 4., 14., 6., 3., 11., 1., 9., 15., 7., 13., 5.);
    return b[i];
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    let a = x * (2.51 * x + 0.03);
    let b = x * (2.43 * x + 0.59) + 0.14;
    return clamp(a / b, vec3<f32>(0.0), vec3<f32>(1.0));
}

// HDR scene + AO + beam dust + bloom, tonemapped: everything after this works on display values like before
fn texel(px: vec2<f32>, res: vec2<f32>) -> vec3<f32> {
    let q = clamp(vec2<i32>(px), vec2<i32>(0), vec2<i32>(res) - 1);
    let uv = (vec2<f32>(q) + 0.5) / res;
    let f = textureSampleLevel(fx, lin, uv, 0.0);
    var c = textureLoad(frame, q, 0).rgb * mix(1.0, f.a, 0.85) + f.rgb;
    c += textureSampleLevel(bloom, lin, uv, 0.0).rgb * p.misc2.w;
    return aces(c * p.misc2.z);
}

@fragment
fn fs(i: VsOut) -> @location(0) vec4<f32> {
    let res = vec2<f32>(textureDimensions(frame));
    let analog = p.misc2.x;
    let t = floor(p.misc2.y * 24.0);
    var uv = i.uv;
    let aspect = res.x / res.y;
    let cc = uv - 0.5;
    let r2 = dot(cc * vec2<f32>(aspect, 1.0), cc * vec2<f32>(aspect, 1.0));
    let r2max = 0.25 * aspect * aspect + 0.25;
    uv = 0.5 + cc * (1.0 + p.misc.z * r2) / (1.0 + p.misc.z * r2max);
    let row = floor(uv.y * res.y);
    let band = step(0.985 - analog * 0.065, hash(vec2<f32>(floor(uv.y * 18.0), t)));
    let roll = fract(p.misc2.y * 0.07);
    let bar = smoothstep(0.06, 0.0, abs(uv.y - roll));
    uv.x += (hash(vec2<f32>(row, t)) - 0.5) * p.noise.z * (1.0 + band * (6.0 + 4.0 * analog) + bar * 14.0 * analog) / res.x;
    uv.x += sin(uv.y * 40.0 + p.misc2.y * 3.0) * 0.0012 * analog;

    let px = floor(uv * res);
    let shift = (2.0 + band * 4.0) * analog;
    var c = vec3<f32>(texel(px + vec2<f32>(shift, 0.0), res).r, texel(px, res).g, texel(px - vec2<f32>(shift, 0.0), res).b);
    let blur = (texel(px + vec2<f32>(1.0, 0.0), res) + texel(px - vec2<f32>(1.0, 0.0), res)
              + texel(px + vec2<f32>(0.0, 1.0), res) + texel(px - vec2<f32>(0.0, 1.0), res)
              + texel(px + vec2<f32>(2.0, 2.0), res) + texel(px - vec2<f32>(2.0, 2.0), res)) / 6.0;
    c = max(vec3<f32>(0.0), c + (c - blur) * p.misc.w);
    c += texel(px - vec2<f32>(7.0, 0.0), res) * 0.12 * analog;
    c *= 1.0 - bar * 0.25 * analog;
    c += bar * (hash(px + t) - 0.5) * 0.35 * analog;

    c *= p.grade.w;
    c = clamp((c - p.grade.x) / (1.0 - p.grade.x), vec3<f32>(0.0), vec3<f32>(1.0));
    let l = dot(c, vec3<f32>(0.299, 0.587, 0.114));
    c = mix(vec3<f32>(l), c, p.grade.y);
    c = clamp((c - 0.5) * p.grade.z + 0.5, vec3<f32>(0.0), vec3<f32>(1.0));
    c *= p.tint.rgb;
    // split toning like the references: shadows sink into cold blue-grey, the torch keeps its warmth
    let lum = dot(c, vec3<f32>(0.299, 0.587, 0.114));
    c *= mix(vec3<f32>(0.96, 0.99, 1.05), vec3<f32>(1.02, 1.0, 0.97), smoothstep(0.05, 0.55, lum));

    let gr = hash(px + t * 1.37) - 0.5;
    c += gr * p.noise.x * (0.35 + l);
    c += (vec3<f32>(hash(px + t + 3.1), hash(px + t + 7.7), hash(px + t + 11.3)) - 0.5) * p.noise.y;

    let dd = i.uv - 0.5;
    c *= clamp(1.0 - dot(dd, dd) * p.noise.w, 0.0, 1.0);

    let bi = i32(px.x % 4.0) + i32(px.y % 4.0) * 4;
    let dth = bayer(bi) / 16.0 - 0.5;
    c = floor(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)) * p.misc.y + dth + 0.5) / p.misc.y;
    c *= p.misc.x;

    let h = textureSample(hud, lin, i.uv);
    return vec4<f32>(mix(c, h.rgb, h.a), 1.0);
}
