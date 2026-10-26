// Half-res screen effects from depth + normals: SSAO (a); light scattered in the air (rgb): dust in the flash beam,
// light shafts of the shadowed lamps (marched through their shadow maps: cones under the bulbs, bars of shadow from
// railings and door frames) and the glow every bulb / street lamp throws into the haze around it (GoldSrc halos).
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    light_vp: mat4x4<f32>,
    cam_pos: vec4<f32>,
    light_pos: array<vec4<f32>, 32>,
    light_col: array<vec4<f32>, 32>,
    light_info: array<vec4<f32>, 32>,
    flash_pos: vec4<f32>,
    flash_dir: vec4<f32>,
    flash_col: vec4<f32>,
    flash_params: vec4<f32>,
    ambient: vec4<f32>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>,
    lamp_vp: array<mat4x4<f32>, 8>,
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
@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var depth: texture_depth_2d;
@group(1) @binding(1) var normals: texture_2d<f32>;
@group(1) @binding(2) var shadow_map: texture_depth_2d;
@group(1) @binding(3) var shadow_samp: sampler_comparison;
@group(1) @binding(4) var lamp_shadows: texture_depth_2d_array;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    var o: VsOut;
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    o.pos = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(xy.x, 1.0 - xy.y);
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
    let c = dot(fdir, g.flash_dir.xyz);
    if (c < 0.7071) { return 0.0; }
    // dust in the beam only needs the rings, not the reflector's lumps
    let th = degrees(acos(min(c, 1.0)));
    return select(torch_profile(th, 1.0), led_profile(th, 0.0), g.flash_params.y > 0.5);
}

fn hash(q: vec2<f32>) -> f32 { return fract(sin(dot(q, vec2<f32>(12.9898, 78.233))) * 43758.5453); }

fn world_at(uv: vec2<f32>, z: f32) -> vec3<f32> {
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    let w = g.inv_view_proj * ndc;
    return w.xyz / w.w;
}

fn depth_at(uv: vec2<f32>) -> f32 {
    let size = vec2<f32>(textureDimensions(depth));
    let q = clamp(vec2<i32>(uv * size), vec2<i32>(0), vec2<i32>(size) - 1);
    return textureLoad(depth, q, 0);
}

fn in_shadow(p: vec3<f32>) -> f32 {
    let c = g.light_vp * vec4<f32>(p, 1.0);
    if (c.w <= 0.0) { return 0.0; }
    let ndc = c.xyz / c.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) { return 1.0; }
    return textureSampleCompareLevel(shadow_map, shadow_samp, uv, ndc.z - 0.002);
}

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

/// In-scattering of every lamp along the eye ray (closed-form point-light integral over [0, len]):
/// a soft glow around bulbs that grows with the haze, cut where a slab or wall stands between eye and lamp.
fn halos(dir: vec3<f32>, len: f32) -> vec3<f32> {
    let o = g.cam_pos.xyz;
    let eye_out = exterior(o);
    let eye_band = band(o.y);
    var sum = vec3<f32>(0.0);
    let count = u32(g.fog_params.z);
    for (var i = 0u; i < 32u; i++) {
        if (i >= count) { break; }
        // the glow at the bulb; shadowed lamps add their lit cone through the march below
        let halo = g.light_info[i].w;
        if (halo <= 0.0) { continue; }
        let lp = g.light_pos[i].xyz;
        let l_out = exterior(lp - vec3<f32>(0.0, 0.3, 0.0));
        if (l_out != eye_out) { continue; }
        if (!l_out) {
            let bands = abs(band(lp.y) - eye_band);
            let open = g.light_col[i].w > 1.5 && bands < 1.5;
            if (bands > 0.5 && !open) { continue; }
        }
        let rel = lp - o;
        let b = dot(rel, dir);
        let h = sqrt(max(dot(rel, rel) - b * b, 0.0)) + 0.06;
        let s = (atan((len - b) / h) - atan(-b / h)) / h;
        // haze is thicker outdoors (frost in the air) and the glow fades over the lamp's own reach
        let density = select(0.012, 0.022, l_out);
        let reach = clamp(1.0 - length(rel) / (g.light_pos[i].w * 3.0), 0.0, 1.0);
        sum += g.light_col[i].rgb * min(s, 25.0) * density * reach * halo;
    }
    return sum;
}

/// Light of the shadowed lamps scattered along the eye ray. Only lamps whose reach the ray crosses are visited, and
/// each is marched inside its own sphere (10 steps, one shadow tap each): cheap, and dense where the shafts are.
/// The lamp's reach shapes the lit volume (unlike the 1/r² halo it reads all the way down to the floor), the storey
/// rules of the surfaces keep it out of other floors. Air above a lamp (outside its 135° shadow camera) stays dark:
/// lamps shine down, and a lit sphere round every street lamp turned the whole sky into yellow haze.
fn shafts(dir: vec3<f32>, len: f32, jitter: f32) -> vec3<f32> {
    let o = g.cam_pos.xyz;
    let reach = min(len, select(16.0, 30.0, exterior(o)));
    var sum = vec3<f32>(0.0);
    for (var s = 0u; s < 8u; s++) {
        let li = g.slot_light[s / 4u][s % 4u];
        if (li < 0.0) { continue; }
        let i = u32(li);
        let info = g.light_info[i];
        if (info.y <= 0.0) { continue; }
        let lp = g.light_pos[i].xyz;
        let r = g.light_pos[i].w;
        let rel = lp - o;
        let b = dot(rel, dir);
        let disc = b * b - (dot(rel, rel) - r * r);
        if (disc <= 0.0) { continue; }
        let t0 = max(b - sqrt(disc), 0.0);
        let t1 = min(b + sqrt(disc), reach);
        if (t1 <= t0) { continue; }
        let l_out = exterior(lp - vec3<f32>(0.0, 0.3, 0.0));
        let l_band = band(lp.y);
        let stair = g.light_col[i].w > 1.5;
        let slot = i32(s);
        let dt = (t1 - t0) / 10.0;
        var acc = 0.0;
        for (var k = 0; k < 10; k++) {
            let x = o + dir * (t0 + dt * (f32(k) + jitter));
            var att = clamp(1.0 - distance(lp, x) / r, 0.0, 1.0);
            if (exterior(x) != l_out) { continue; }
            if (!l_out && abs(l_band - band(x.y)) > 0.5 && !(stair && length((lp - x).xz) < 3.9)) { continue; }
            let q = g.lamp_vp[slot] * vec4<f32>(x, 1.0);
            if (q.w <= 0.0) { continue; }
            let ndc = q.xyz / q.w;
            let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
            if (any(uv <= vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) || ndc.z >= 1.0) { continue; }
            // soft cone edge: the shade's rim, not the shadow camera's square
            let rim = smoothstep(0.0, 0.25, 0.5 - length(uv - 0.5));
            let vis = rim * mix(1.0, textureSampleCompareLevel(lamp_shadows, shadow_samp, uv, slot, ndc.z - 0.002), info.y);
            acc += att * att * vis;
        }
        // frost in the yard air scatters more than the stairwell dust
        sum += g.light_col[i].rgb * acc * dt * select(0.009, 0.008, l_out);
    }
    return sum;
}

@fragment
fn fs(i: VsOut) -> @location(0) vec4<f32> {
    let z = depth_at(i.uv);
    let p = world_at(i.uv, z);
    let size = vec2<f32>(textureDimensions(normals));
    let nq = clamp(vec2<i32>(i.uv * size), vec2<i32>(0), vec2<i32>(size) - 1);
    let ns = textureLoad(normals, nq, 0);
    // the viewmodel (hands, weapon) sits right at the lens: no air in front of it to light up (marching the sky's
    // 80 m over it washed a pistol out into a ghost)
    if (ns.a > 0.1 && ns.a < 0.5) { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }
    if (ns.a < 0.5) {
        // sky / nothing drawn: lamps still glow against it
        let far = world_at(i.uv, 0.9999);
        let sky = normalize(far - g.cam_pos.xyz);
        return vec4<f32>(halos(sky, 80.0) + shafts(sky, 80.0, hash(i.pos.xy + fract(g.fog_params.w) * 61.0)), 1.0);
    }
    let n = normalize(ns.xyz * 2.0 - 1.0);
    let jitter = hash(i.pos.xy + fract(g.fog_params.w) * 61.0);

    // ---- SSAO: hemisphere samples around p, projected back and compared with the depth buffer
    var ao = 1.0;
    if (z < 1.0 && ns.a > 0.5) {
        var occ = 0.0;
        let radius = 0.3;
        for (var k = 0; k < 10; k++) {
            let fk = f32(k);
            let a = (fk + jitter) * 2.39996;
            let r = sqrt((fk + 0.5) / 10.0);
            var dir = vec3<f32>(cos(a) * r, sin(a) * r, sqrt(max(1.0 - r * r, 0.0)));
            // build a basis around n
            let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.y) > 0.9);
            let tx = normalize(cross(up, n));
            let ty = cross(n, tx);
            let s = p + (tx * dir.x + ty * dir.y + n * dir.z) * radius * (0.3 + 0.7 * fract(jitter + fk * 0.37));
            let c = g.view_proj * vec4<f32>(s, 1.0);
            let suv = vec2<f32>(c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5);
            let q = world_at(suv, depth_at(suv));
            let dq = distance(q, g.cam_pos.xyz);
            let ds = distance(s, g.cam_pos.xyz);
            let range = smoothstep(0.0, 1.0, radius / max(abs(distance(p, q)), 1e-3));
            occ += select(0.0, 1.0, dq < ds - 0.02) * range;
        }
        ao = 1.0 - occ / 10.0;
    }

    // ---- dust in the flash beam: march camera -> surface, sample cone * shadow
    var fog = vec3<f32>(0.0);
    if (g.flash_params.w > 0.0) {
        let to = p - g.cam_pos.xyz;
        let len = min(length(to), 12.0);
        let dir = to / max(length(to), 1e-3);
        let steps = 16;
        let dt = len / f32(steps);
        for (var k = 0; k < steps; k++) {
            let x = g.cam_pos.xyz + dir * dt * (f32(k) + jitter);
            let fd = x - g.flash_pos.xyz;
            let fdist = max(length(fd), 1e-3);
            let cone = torch_cookie(fd / fdist);
            var att = clamp(1.0 - fdist / g.flash_params.x, 0.0, 1.0);
            att *= att;
            if (cone * att > 0.001) { fog += g.flash_col.rgb * cone * att * in_shadow(x) * dt; }
        }
        fog *= 0.035;
    }
    let to_p = p - g.cam_pos.xyz;
    fog += halos(to_p / max(length(to_p), 1e-3), length(to_p));
    fog += shafts(to_p / max(length(to_p), 1e-3), length(to_p), jitter);
    return vec4<f32>(fog, ao);
}
