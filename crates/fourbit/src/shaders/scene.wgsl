// World shading, HDR: wrapped-Lambert + normalized Blinn-Phong specular from point lights and the hand flash
// (with its shadow map), normal maps via a screen-space cotangent frame, distance fog.
// Writes colour (HDR) and the world normal (for SSAO).
const MAX_LIGHTS: u32 = 32u;

struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    light_vp: mat4x4<f32>,            // flash shadow camera
    cam_pos: vec4<f32>,
    light_pos: array<vec4<f32>, 32>,   // xyz pos, w range
    light_col: array<vec4<f32>, 32>,   // rgb colour * intensity, w 2 = stairwell lamp
    light_info: array<vec4<f32>, 32>,  // x lamp shadow slot (-1 none), y its weight, z floor bounce, w halo
    flash_pos: vec4<f32>,
    flash_dir: vec4<f32>,
    flash_col: vec4<f32>,
    flash_params: vec4<f32>,          // range, beam (1 = phone LED, 0 = reflector torch), -, on
    ambient: vec4<f32>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>,
    lamp_vp: array<mat4x4<f32>, 8>,   // lamp shadow cameras, one per slot
    slot_light: array<vec4<f32>, 2>,  // light index of each lamp shadow slot, -1 = free
    // environment (project settings): interior zones, storey height, sky, fog
    zone_min: array<vec4<f32>, 4>,
    zone_max: array<vec4<f32>, 4>,
    zones: vec4<f32>,                 // x zone count (0 = all outdoors), y storey height (0 = no storeys), z above this y = outdoors
    sky_amb: vec4<f32>,               // outdoor ambient rgb
    key_dir: vec4<f32>,               // sun / moon direction (towards it), w wrap
    key_col: vec4<f32>,               // its colour
    in_amb: vec4<f32>,                // indoor ambient rgb, w rim strength
    fog_out: vec4<f32>,               // outdoor fog rgb, w start
    fog_in: vec4<f32>,                // indoor fog rgb, w start
    fog_end: vec4<f32>,               // x outdoor end, y indoor end
};

struct Draw {
    model: mat4x4<f32>,
    color: vec4<f32>,
    params: vec4<f32>,                // x > .5 = unlit, x < 0 = texel grid; y fog, z cutoff, w roughness
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var nrm: texture_2d<f32>;
@group(1) @binding(2) var samp: sampler;
@group(2) @binding(0) var<uniform> d: Draw;
@group(3) @binding(0) var shadow_map: texture_depth_2d;
@group(3) @binding(1) var shadow_samp: sampler_comparison;
@group(3) @binding(2) var lamp_shadows: texture_depth_2d_array;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wpos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct FsOut {
    @location(0) color: vec4<f32>,
    @location(1) normal: vec4<f32>,
};

@vertex
fn vs(v: VsIn) -> VsOut {
    var o: VsOut;
    let w = d.model * vec4<f32>(v.pos, 1.0);
    o.wpos = w.xyz;
    // Cofactors give the inverse-transpose normal for non-uniformly scaled body limbs.
    let a = d.model[0].xyz; let b = d.model[1].xyz; let c = d.model[2].xyz;
    o.normal = normalize(mat3x3<f32>(cross(b,c),cross(c,a),cross(a,b)) * v.normal);
    o.uv = vec2<f32>(v.uv.x, 1.0 - v.uv.y);   // meshes use Unity's v-up convention
    o.clip = g.view_proj * w;
    return o;
}

// 12-tap Poisson disc, turned per world position (not per pixel/frame): soft edges that don't crawl or saw-tooth
// when the torch sways with the camera
const POISSON = array<vec2<f32>, 12>(vec2<f32>(-0.326,-0.406), vec2<f32>(-0.840,-0.074), vec2<f32>(-0.696,0.457), vec2<f32>(-0.203,0.621),
    vec2<f32>(0.962,-0.195), vec2<f32>(0.473,-0.480), vec2<f32>(0.519,0.767), vec2<f32>(0.185,-0.893), vec2<f32>(0.507,0.064),
    vec2<f32>(0.896,0.412), vec2<f32>(-0.322,-0.933), vec2<f32>(-0.792,-0.598));

fn flash_shadow(wpos: vec3<f32>, n: vec3<f32>) -> f32 {
    let p = g.light_vp * vec4<f32>(wpos + n * 0.03, 1.0);
    if (p.w <= 0.0) { return 1.0; }
    let ndc = p.xyz / p.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) { return 1.0; }
    let a = fract(sin(dot(floor(wpos * 37.0), vec3<f32>(12.9898, 78.233, 37.719))) * 43758.5453) * 6.2832;
    let r = mat2x2<f32>(cos(a), sin(a), -sin(a), cos(a)) * (2.2 / 2048.0);
    var s = 0.0;
    for (var k = 0; k < 12; k++) {
        s += textureSampleCompareLevel(shadow_map, shadow_samp, uv + r * POISSON[k], ndc.z - 0.0009);
    }
    return s / 12.0;
}

fn lamp_visibility(i: u32, p: vec3<f32>, n: vec3<f32>) -> f32 {
    let info = g.light_info[i];
    if (info.x < -0.5 || info.y <= 0.0) { return 1.0; }
    let slot = i32(info.x + 0.5);
    let q = g.lamp_vp[slot] * vec4<f32>(p + n * 0.015, 1.0);
    let ndc = q.xyz / q.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    // outside the shadow camera there is no information: lit, not black (walls beside/above a lamp went dark)
    if (q.w <= 0.0 || any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z > 1.0) { return 1.0; }
    var result = 0.0;
    let a = fract(sin(dot(floor(p * 31.0), vec3<f32>(12.9898, 78.233, 37.719))) * 43758.5453) * 6.2832;
    let r = mat2x2<f32>(cos(a), sin(a), -sin(a), cos(a)) * (2.0 / 512.0);
    for (var k = 0; k < 12; k++) {
        result += textureSampleCompareLevel(lamp_shadows, shadow_samp, uv + r * POISSON[k], slot, ndc.z - 0.0015);
    }
    // fade the shadow out towards the edge of its map and while a lamp hands its shadow slot over (no popping)
    let edge = smoothstep(0.0, 0.15, min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y)));
    return mix(1.0, result / 12.0, edge * info.y);
}

// Diffuse and specular are kept apart: diffuse is tinted by the albedo, a dielectric highlight is not
// (wet-looking plastic came from multiplying the highlight by the texture colour).
struct Light2 { diff: vec3<f32>, spec: vec3<f32> };

// one light: wrapped Lambert + GGX highlight with Schlick Fresnel
fn shade(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, col: vec3<f32>, wrap: f32, rough: f32) -> Light2 {
    let ndl = dot(n, l);
    let h = normalize(l + v);
    let a = max(rough * rough, 0.02);
    let a2 = a * a;
    let nh = clamp(dot(n, h), 0.0, 1.0);
    let dd = nh * nh * (a2 - 1.0) + 1.0;
    let ggx = a2 / (3.14159 * dd * dd);
    let fres = 0.04 + 0.96 * pow(1.0 - clamp(dot(h, v), 0.0, 1.0), 5.0);
    let vis = 0.25 / max(clamp(dot(n, v), 0.05, 1.0) * clamp(ndl, 0.05, 1.0), 0.05);
    var o: Light2;
    o.diff = col * (clamp(ndl, 0.0, 1.0) * (1.0 - wrap) + wrap);
    o.spec = col * min(ggx * fres * vis, 24.0) * clamp(ndl, 0.0, 1.0);
    return o;
}

// Torch beam profile, as a real reflector torch draws it on a wall: a hot centre, a darker band, the bright ring of
// the reflector, the main beam fading out by ~33°, a dim wide spill to 45°, and the unevenness of a cheap reflector
// (lumps round the ring, faint radial streaks) that makes it read as light instead of a perfect disc.
// The phone's LED behind its little lens: one wide soft lobe (no reflector rings), a slightly brighter core, faint
// ripples of the Fresnel lens and the square-ish footprint of the LED die.
fn led_profile(th: f32, phi: f32) -> f32 {
    let core = exp(-pow(th / 11.0, 2.0)) * 0.55;
    let lobe = smoothstep(44.0, 6.0, th) * 1.05;
    let ripple = 1.0 + 0.045 * sin(th * 1.6);
    let die = 1.0 + 0.05 * cos(phi * 4.0) * smoothstep(5.0, 25.0, th);
    return (core + lobe) * ripple * die;
}
fn torch_profile(th: f32, lump: f32) -> f32 {
    let hot = exp(-pow(th / 6.5, 2.0));
    let dark = 1.0 - 0.18 * exp(-pow((th - 9.0) / 1.5, 2.0));
    let ring = exp(-pow((th - 12.5) / 2.2, 2.0)) * 0.35 * lump;
    let body = smoothstep(33.0, 14.0, th) * 0.8;
    let spill = smoothstep(45.0, 24.0, th) * 0.07;
    return (hot + ring + body) * dark + spill;
}
fn torch_cookie(fdir: vec3<f32>) -> f32 {
    let d = g.flash_dir.xyz;
    let c = dot(fdir, d);
    if (c < 0.7071) { return 0.0; }   // cos 45°: outside the spill, before any trigonometry
    let th = degrees(acos(min(c, 1.0)));
    let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(d.y) > 0.95);
    let rx = normalize(cross(up, d));
    let phi = atan2(dot(fdir, cross(d, rx)), dot(fdir, rx));
    if (g.flash_params.y > 0.5) { return led_profile(th, phi); }
    let lump = 1.0 + 0.10 * sin(phi * 5.0 + 0.7) + 0.06 * sin(phi * 11.0 + 2.1) + 0.04 * sin(phi * 23.0);
    let streak = 1.0 + 0.06 * pow(abs(sin(phi * 9.0 + th * 0.15)), 8.0);
    return torch_profile(th, lump) * streak;
}

fn hash3(p: vec3<f32>) -> f32 { return fract(sin(dot(p, vec3<f32>(12.9898, 78.233, 37.719))) * 43758.5453); }

// Interior zones: inside any box (and below zones.z) is indoors; with no zones everything is outdoors.
fn exterior(p: vec3<f32>) -> bool {
    let n = u32(g.zones.x);
    if (n == 0u || p.y > g.zones.z) { return true; }
    for (var k = 0u; k < 4u; k++) {
        if (k >= n) { break; }
        if (all(p >= g.zone_min[k].xyz) && all(p <= g.zone_max[k].xyz)) { return false; }
    }
    return true;
}
// Storey index of a height (lamps don't light through slabs); one storey when storeys are off.
fn band(y: f32) -> f32 { let h = g.zones.y; return select(0.0, floor((y + h) / h), h > 0.0); }

// `volume`: smoke and dust scatter light from every side, so lights reach them without the cosine and without a
// highlight (lit like a surface they only showed where a lamp happened to face the billboard)
fn lighting(wpos: vec3<f32>, n: vec3<f32>, rough: f32, volume: bool) -> Light2 {
    let wrap = select(0.0, 1.0, volume);
    let gloss = select(1.0, 0.0, volume);
    let v = normalize(g.cam_pos.xyz - wpos);
    // indoor base, a little brighter from above; edges catch it (rim) so forms read in the dark
    var amb = g.in_amb.rgb * (0.75 + 0.25*n.y);
    if (exterior(wpos+n*0.08)) {
        // sky: brighter from above; the key (sun or moon) wraps round forms so they read
        let key = normalize(g.key_dir.xyz);
        let sky = 0.75 + 0.15*max(n.y,0.0) + 0.15*max(dot(n,normalize(key + vec3<f32>(0.05,0.0,0.0))),0.0);
        amb = g.sky_amb.rgb*sky;
        amb += g.key_col.rgb * clamp(dot(n, key) * (1.0 - g.key_dir.w) + g.key_dir.w, 0.0, 1.0);
    }
    if (g.cam_pos.w<0.5) { amb=g.ambient.rgb; }
    let rim = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 4.0);
    var light: Light2;
    light.diff = amb;
    light.spec = vec3<f32>(0.07,0.10,0.16) * g.in_amb.w * rim * (1.0 - rough * 0.5) * gloss;
    let count = u32(g.fog_params.z);
    let f_out = exterior(wpos + n * 0.08);
    for (var i = 0u; i < MAX_LIGHTS; i++) {
        if (i >= count) { break; }
        let lp = g.light_pos[i].xyz;
        let range = max(g.light_pos[i].w, 1e-3);
        let dv = lp - wpos;
        let dist = max(length(dv), 1e-3);
        if (dist > range * 1.3 + 2.5) { continue; }
        // no light through slabs and walls: a lamp lights its own floor; street light inside / room light outside
        // only as much as a window lets through (shadowed lamps also have their map; the rest rely on this)
        var gate = 1.0;
        let l_out = exterior(lp - vec3<f32>(0.0, 0.3, 0.0));
        if (l_out != f_out) { gate = 0.1; }
        else if (!l_out) {
            let bands = abs(band(lp.y) - band(wpos.y + n.y * 0.05));
            // stairwell lamps (w = 2) reach one storey up/down through the open well, not out into the halls
            let open = g.light_col[i].w > 1.5 && bands < 1.5 && length((lp - wpos).xz) < 3.9;
            if (bands > 0.5 && !open) { gate = 0.0; }
        }
        if (gate <= 0.0) { continue; }
        var att = clamp(1.0 - dist / range, 0.0, 1.0);
        att *= att * gate;
        if (att > 0.0) {
            // smoke hugging a wall sat in the shadow maps' acne (a soot stain); a few cm of smoke need no shadow
            let vis = select(lamp_visibility(i, wpos, n), 1.0, volume);
            let s = shade(n, v, dv / dist, g.light_col[i].rgb * att * vis, max(0.2, wrap), rough);
            light.diff += s.diff; light.spec += s.spec * gloss;
        }
        // the floor under a lamp throws some of it back up: ceilings, the undersides of flights and the upper walls
        // get a soft fill instead of black (concrete indoors, snow outdoors, set per lamp); diffuse only, no shadow
        let bounce = g.light_info[i].z;
        if (bounce > 0.0) {
            let h = g.zones.y;
            let floor_y = select(select(max(lp.y - 2.4, 0.0), max(floor(lp.y / h) * h, lp.y - 2.4), h > 0.0), 0.0, l_out) + 0.05;
            let bv = vec3<f32>(lp.x, floor_y, lp.z) - wpos;
            let bd = max(length(bv), 1e-3);
            var batt = clamp(1.0 - bd / (range * 1.3), 0.0, 1.0);
            batt *= batt * gate;
            light.diff += g.light_col[i].rgb * bounce * batt * max(clamp(dot(n, bv / bd), 0.0, 1.0) * 0.55 + 0.45, wrap);
        }
    }
    let fd = wpos - g.flash_pos.xyz;
    let fdist = max(length(fd), 1e-3);
    let fdir = fd / fdist;
    let beam = torch_cookie(fdir);
    var fatt = clamp(1.0 - fdist / g.flash_params.x, 0.0, 1.0);
    fatt *= fatt;
    if (beam * fatt * g.flash_params.w > 0.0) {
        let sh = select(flash_shadow(wpos, n), 1.0, volume);
        let s = shade(n, v, -fdir, g.flash_col.rgb * beam * fatt * g.flash_params.w, max(0.1, wrap), rough);
        light.diff += s.diff * sh;
        // snow sparkle: ice grains on the ground flash back at the torch. Only snow: on concrete walls the same
        // sub-pixel glints read as white noise at 360 lines
        let snow = select(0.0, smoothstep(0.7, 0.9, n.y), f_out);
        let glint = step(0.9935, hash3(floor(wpos * 140.0))) * snow * clamp(dot(n, -fdir), 0.0, 1.0);
        light.spec += (s.spec + g.flash_col.rgb * beam * fatt * glint * 1.2) * sh * gloss;
    }
    return light;
}

// Opaque surfaces go through fs_opaque: no discard anywhere in it, so a tile GPU (Apple) can drop hidden fragments
// before shading them (one discard in the shader turned that off for the whole world: every layer was lit).
struct Surf { o: FsOut, a: f32 };

@fragment
fn fs(i: VsOut, @builtin(front_facing) front: bool) -> FsOut {
    let s = surface(i, front);
    if (s.a < d.params.z) { discard; }
    // negative fog = own body: nothing of it closer than |fog| metres to the lens (thighs in a crouch, shoulders)
    if (d.params.y < 0.0 && (distance(i.wpos, g.cam_pos.xyz) < -d.params.y || !front)) { discard; }
    return s.o;
}

@fragment
fn fs_opaque(i: VsOut, @builtin(front_facing) front: bool) -> FsOut { return surface(i, front).o; }

fn surface(i: VsOut, front: bool) -> Surf {
    // Quantise material detail, not geometry. Original gradients retain mip filtering in the distance.
    let dx=dpdx(i.uv);let dy=dpdy(i.uv);
    let dims=vec2<f32>(textureDimensions(tex));
    let grid=dims*min(1.0,max(-d.params.x,1.0)/max(dims.x,dims.y));
    let uv=select(i.uv,(floor(i.uv*grid)+0.5)/grid,d.params.x<0.0);
    let t = textureSampleGrad(tex, samp, uv, dx, dy);
    let surface = textureSampleGrad(nrm, samp, uv, dx, dy);
    let tn = surface.xyz * 2.0 - 1.0;
    // cotangent frame from screen derivatives (no tangents in the vertex data)
    let dp1 = dpdx(i.wpos);
    let dp2 = dpdy(i.wpos);
    let duv1 = dpdx(i.uv);
    let duv2 = dpdy(i.uv);
    var n = normalize(i.normal);
    if (!front) { n = -n; }
    let dp2perp = cross(dp2, n);
    let dp1perp = cross(n, dp1);
    let tt = dp2perp * duv1.x + dp1perp * duv2.x;
    let bb = dp2perp * duv1.y + dp1perp * duv2.y;
    let m = max(dot(tt, tt), dot(bb, bb));
    if (m > 1e-20) {
        let inv = inverseSqrt(m);
        n = normalize(tt * inv * tn.x - bb * inv * tn.y + n * tn.z);
    }

    var c = t.rgb * d.color.rgb;
    if (d.params.x < 0.5) {
        // roughness above 1.5 marks smoke and dust (render::roughness)
        let l = lighting(i.wpos, n, clamp(d.params.w*surface.a,0.09,1.0), d.params.w > 1.5);
        c = c * l.diff + l.spec;
    }
    let outside=exterior(i.wpos+n*0.08);
    let fog_start=select(g.fog_in.w,g.fog_out.w,outside);let fog_end=select(g.fog_end.y,g.fog_end.x,outside);
    let fog=clamp((distance(i.wpos,g.cam_pos.xyz)-fog_start)/max(fog_end-fog_start,0.01),0.0,1.0);
    let fog_color=select(g.fog_in.rgb,g.fog_out.rgb,outside);
    c=mix(c,fog_color,fog*abs(d.params.y));
    var o: FsOut;
    o.color = vec4<f32>(c, t.a * d.color.a);
    // normal alpha tells fx.wgsl what the pixel is: 1 lit world surface, 0 sky / unlit, 0.25 the viewmodel pass
    o.normal = vec4<f32>(n * 0.5 + 0.5, select(select(1.0, 0.0, d.params.x > 0.5), 0.25, g.cam_pos.w < 0.5));
    return Surf(o, t.a);
}
