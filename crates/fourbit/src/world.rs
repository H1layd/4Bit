//! The world a game runs in: drawable objects, colliders (boxes for movement, exact shapes for rays), lamps.
//! Built from code (`block`, `lamp`, `place_model`) and/or from a scene file (`scene.rs`). Metres, Y up, +Z forward,
//! left-handed (like Unity).
use crate::collide::{Placed, Shape};
use crate::mesh::MeshData;
use crate::model::Model;
use glam::{Mat4, Quat, Vec3, Vec4};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, PartialEq, Debug)]
pub struct Material { pub tex: String, pub tint: Vec4, pub unlit: bool }
impl Material {
    /// `tex`: a name in assets/textures ("" = white), or a path ending in .png relative to assets.
    pub fn lit(tex: &str) -> Self { Self { tex: tex.into(), tint: Vec4::ONE, unlit: false } }
    pub fn tinted(tex: &str, r: f32, g: f32, b: f32) -> Self { Self { tex: tex.into(), tint: Vec4::new(r, g, b, 1.0), unlit: false } }
    pub fn unlit(tex: &str, r: f32, g: f32, b: f32) -> Self { Self { tex: tex.into(), tint: Vec4::new(r, g, b, 1.0), unlit: true } }
}

pub struct Obj {
    pub mesh: MeshData,
    pub mat: Material,
    pub model: Mat4,
    /// multiplied with the material tint (bulbs dim with their light)
    pub color: Vec4,
    pub visible: bool,
    /// moves at runtime: redrawn into lamp shadows every frame instead of being cached
    pub moving: bool,
    /// alpha cut-out threshold (< 0 = opaque)
    pub cutoff: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb { pub min: Vec3, pub max: Vec3 }
impl Aabb {
    pub fn from_center(c: Vec3, size: Vec3) -> Self { Self { min: c - size * 0.5, max: c + size * 0.5 } }
    /// Slab test from outside: distance and surface normal.
    pub fn ray(&self, o: Vec3, d: Vec3, max_t: f32) -> Option<(f32, Vec3)> {
        let (mut tmin, mut tmax, mut n) = (0.0f32, max_t, Vec3::ZERO);
        for a in 0..3 {
            if d[a].abs() < 1e-8 { if o[a] < self.min[a] || o[a] > self.max[a] { return None; } continue; }
            let inv = 1.0 / d[a];
            let (mut t0, mut t1) = ((self.min[a] - o[a]) * inv, (self.max[a] - o[a]) * inv);
            let mut nn = Vec3::ZERO; nn[a] = -d[a].signum();
            if t0 > t1 { std::mem::swap(&mut t0, &mut t1); }
            if t0 > tmin { tmin = t0; n = nn; }
            tmax = tmax.min(t1);
            if tmin > tmax { return None; }
        }
        if n == Vec3::ZERO { return None; }
        Some((tmin, n))
    }
    pub fn overlaps(&self, o: &Aabb) -> bool { self.max.cmpge(o.min).all() && self.min.cmple(o.max).all() }
}

#[derive(Clone, Debug)]
pub struct Light {
    pub pos: Vec3, pub color: Vec3, pub intensity: f32, pub range: f32,
    /// 0..1 noise flicker, and a broken lamp that blacks out now and then
    pub flicker: f32, pub broken: bool,
    pub seed: f32, pub blackout: f32,
    /// this frame's intensity (after flicker)
    pub current: f32,
    /// a bulb object dimmed with the light
    pub bulb: Option<usize>,
    /// light thrown back up by the floor under the lamp (0 = none)
    pub bounce: f32,
    /// reaches one storey up/down (an open stairwell)
    pub stair: bool,
    /// gets a cached shadow map when near (lamps shine down: 135° camera)
    pub shadows: bool,
}
impl Light {
    pub fn new(pos: Vec3, color: Vec3, intensity: f32, range: f32) -> Light {
        Light { pos, color, intensity, range, flicker: 0.0, broken: false, seed: pos.x * 7.1 + pos.z * 3.3, blackout: 0.0, current: intensity, bulb: None, bounce: 0.1, stair: false, shadows: true }
    }
}

/// A model loaded once from assets/models and shared by every placement: meshes (model space), texture paths,
/// the exact shape for rays.
pub struct ModelAsset { pub model: Model, pub textures: Vec<String>, pub shape: Arc<Shape>, pub bounds: (Vec3, Vec3) }

/// A model placed as its own movable objects (the scene editor's props).
#[derive(Clone, Debug)]
pub struct Placement { pub model: String, pub objs: Vec<usize>, pub colliders: std::ops::Range<usize>, pub shape: Option<usize> }

#[derive(Default)]
pub struct World {
    pub objs: Vec<Obj>,
    pub colliders: Vec<Aabb>,
    pub lights: Vec<Light>,
    pub shapes: Vec<Placed>,
    shape_of: HashMap<usize, usize>,
    /// colliders by 4 m ground cell
    grid: HashMap<(i32, i32), Vec<u32>>,
    /// colliders tested by every query (moving ones)
    pub always: Vec<usize>,
    /// objects whose mesh was rebuilt (the runtime re-uploads them)
    pub changed: Vec<usize>,
    assets: PathBuf,
    models: HashMap<String, Arc<ModelAsset>>,
    rng: u32,
}

impl World {
    pub fn new(assets: &Path) -> World { World { assets: assets.to_path_buf(), rng: 12345, ..Default::default() } }
    pub fn assets(&self) -> &Path { &self.assets }
    pub fn rand(&mut self) -> f32 { self.rng ^= self.rng << 13; self.rng ^= self.rng >> 17; self.rng ^= self.rng << 5; (self.rng % 100000) as f32 / 100000.0 }

    // ---------------------------------------------------------------- building
    /// A solid box: mesh (UVs tiled every `tile` m in world space) and a collider.
    pub fn block(&mut self, center: Vec3, size: Vec3, mat: &Material, tile: f32) -> usize {
        let mut m = MeshData::default();
        m.add_box(Vec3::ZERO, size, tile);
        self.colliders.push(Aabb::from_center(center, size));
        self.objs.push(Obj { mesh: m, mat: mat.clone(), model: Mat4::from_translation(center), color: Vec4::ONE, visible: true, moving: false, cutoff: -1.0 });
        self.objs.len() - 1
    }
    /// Any mesh, no collider.
    pub fn add_mesh(&mut self, mesh: MeshData, mat: &Material, model: Mat4) -> usize {
        self.objs.push(Obj { mesh, mat: mat.clone(), model, color: Vec4::ONE, visible: true, moving: false, cutoff: -1.0 });
        self.objs.len() - 1
    }
    /// A ceiling lamp: a small glowing shade and the light just under it.
    pub fn lamp(&mut self, pos: Vec3, color: Vec3, intensity: f32, range: f32) -> usize {
        let mut m = MeshData::default();
        m.add_box(Vec3::ZERO, Vec3::new(0.25, 0.08, 0.25), 1.0);
        let b = self.add_mesh(m, &Material::unlit("", color.x * 0.75, color.y * 0.75, color.z * 0.75), Mat4::from_translation(pos));
        let mut l = Light::new(pos - Vec3::Y * 0.15, color, intensity, range);
        l.bulb = Some(b);
        self.lights.push(l);
        self.lights.len() - 1
    }

    /// Every model under assets/models, by name (path without extension: "props/covered_car").
    pub fn model_names(&self) -> Vec<String> {
        fn walk(d: &Path, base: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() { walk(&p, base, out); }
                else if p.extension().is_some_and(|x| x == "wmdl") { out.push(p.strip_prefix(base).unwrap().with_extension("").to_string_lossy().replace('\\', "/")); }
            }
        }
        let mut v = vec![]; let base = self.assets.join("models"); walk(&base, &base, &mut v); v.sort(); v
    }
    /// A model by name, loaded once (meshes, textures, a ~5 mm exact shape for rays).
    pub fn model(&mut self, name: &str) -> Option<Arc<ModelAsset>> {
        if let Some(m) = self.models.get(name) { return Some(m.clone()); }
        let path = self.assets.join("models").join(format!("{name}.wmdl"));
        let model = Model::load(&path).map_err(|e| eprintln!("model {name}: {e}")).ok()?;
        let dir = Path::new("models").join(name).parent().unwrap().to_path_buf();
        let textures = model.meshes.iter().map(|m| if m.texture.is_empty() { String::new() } else { dir.join(&m.texture).to_string_lossy().replace('\\', "/") }).collect();
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for sm in &model.meshes { for v in &sm.verts { lo = lo.min(v.pos); hi = hi.max(v.pos); } }
        let a = Arc::new(ModelAsset { shape: Arc::new(model_shape(&model)), textures, bounds: (lo, hi), model });
        self.models.insert(name.into(), a.clone());
        Some(a)
    }

    /// Places a model as its own objects (movable later), with slice boxes and its exact shape when `solid`.
    /// Skinned models are placed in their rest pose.
    pub fn place_model(&mut self, name: &str, pos: Vec3, yaw: f32, scale: f32, solid: bool) -> Option<Placement> {
        let a = self.model(name)?;
        let transform = Mat4::from_scale_rotation_translation(Vec3::splat(scale), Quat::from_rotation_y(yaw.to_radians()), pos);
        let mut objs = vec![];
        for (sm, tex) in a.model.meshes.iter().zip(&a.textures) {
            let mut mesh = MeshData::default();
            for v in &sm.verts { mesh.push(v.pos, v.normal, v.uv); }
            mesh.indices = sm.indices.clone();
            objs.push(self.add_mesh(mesh, &Material::lit(tex), transform));
        }
        for &o in &objs { self.objs[o].moving = true; }
        let first = self.colliders.len();
        let shape = solid.then(|| {
            self.shapes.push(Placed::new(a.shape.clone(), transform));
            let s = self.shapes.len() - 1;
            for (lo, hi) in model_boxes(&a.model, transform) {
                // an empty slice stays a parked box far away: the slots keep their count when the model moves
                let b = if lo.x.is_finite() { Aabb { min: lo, max: hi } } else { Aabb { min: Vec3::splat(-1e5), max: Vec3::splat(-1e5) } };
                self.shape_of.insert(self.colliders.len(), s);
                self.colliders.push(b);
            }
            s
        });
        let p = Placement { model: name.into(), objs, colliders: first..self.colliders.len(), shape };
        self.reindex();
        Some(p)
    }
    pub fn move_model(&mut self, p: &Placement, pos: Vec3, yaw: f32, scale: f32) {
        let Some(a) = self.model(&p.model) else { return };
        let transform = Mat4::from_scale_rotation_translation(Vec3::splat(scale), Quat::from_rotation_y(yaw.to_radians()), pos);
        for &o in &p.objs { self.objs[o].model = transform; }
        if let Some(s) = p.shape { self.shapes[s] = Placed::new(a.shape.clone(), transform); }
        for (k, (lo, hi)) in model_boxes(&a.model, transform).into_iter().enumerate() {
            let i = p.colliders.start + k;
            if i < p.colliders.end { self.colliders[i] = if lo.x.is_finite() { Aabb { min: lo, max: hi } } else { Aabb { min: Vec3::splat(-1e5), max: Vec3::splat(-1e5) } }; }
        }
        self.reindex();
    }
    pub fn remove_model(&mut self, p: &Placement) {
        for &o in &p.objs { self.objs[o].visible = false; }
        for i in p.colliders.clone() { self.colliders[i] = Aabb { min: Vec3::splat(-1e5), max: Vec3::splat(-1e5) }; self.shape_of.remove(&i); }
        self.reindex();
    }

    // ---------------------------------------------------------------- queries
    fn cells(min: Vec3, max: Vec3) -> impl Iterator<Item = (i32, i32)> {
        let (x0, x1, z0, z1) = ((min.x / 4.0).floor() as i32, (max.x / 4.0).floor() as i32, (min.z / 4.0).floor() as i32, (max.z / 4.0).floor() as i32);
        (x0..=x1).flat_map(move |x| (z0..=z1).map(move |z| (x, z)))
    }
    /// Rebuilds the collider grid (after building, or when colliders changed).
    pub fn reindex(&mut self) {
        self.grid.clear();
        for (i, b) in self.colliders.iter().enumerate() {
            if self.always.contains(&i) || b.min.x < -1e4 { continue; }
            // huge boxes (a ground plane) go to the always list instead of filling thousands of cells
            if (b.max.x - b.min.x) * (b.max.z - b.min.z) > 4096.0 { continue; }
            for c in Self::cells(b.min, b.max) { self.grid.entry(c).or_default().push(i as u32); }
        }
    }
    fn big(&self, i: usize) -> bool { let b = &self.colliders[i]; b.min.x > -1e4 && (b.max.x - b.min.x) * (b.max.z - b.min.z) > 4096.0 }
    /// Colliders that may touch the box `min..max`.
    pub fn near(&self, min: Vec3, max: Vec3) -> Vec<usize> {
        let q = Aabb { min, max };
        let mut out: Vec<usize> = self.always.clone();
        out.extend((0..self.colliders.len()).filter(|&i| self.big(i)));
        for c in Self::cells(min, max) { if let Some(v) = self.grid.get(&c) { out.extend(v.iter().map(|&i| i as usize)); } }
        out.sort_unstable(); out.dedup();
        out.retain(|&i| self.colliders[i].overlaps(&q));
        out
    }
    fn refine(&self, i: usize, o: Vec3, d: Vec3, max: f32, memo: &mut Vec<(usize, Option<(f32, Vec3, f32)>)>) -> Option<Option<(f32, Vec3, f32)>> {
        let &s = self.shape_of.get(&i)?;
        if let Some(m) = memo.iter().find(|m| m.0 == s) { return Some(m.1); }
        let hit = self.shapes[s].ray(o, d, 0.0, max);
        memo.push((s, hit));
        Some(hit)
    }
    /// First hit along a ray (unit `d`): distance, normal, collider. Model boxes refine to the model's real surface.
    pub fn raycast_hit(&self, o: Vec3, d: Vec3, max: f32) -> Option<(f32, Vec3, usize)> {
        let e = o + d * max;
        let mut best: Option<(f32, Vec3, usize)> = None;
        let mut memo = vec![];
        for i in self.near(o.min(e), o.max(e)) {
            let Some((mut t, mut n)) = self.colliders[i].ray(o, d, max) else { continue };
            match self.refine(i, o, d, max, &mut memo) { Some(None) => continue, Some(Some((s, sn, _))) => { t = s; n = sn; } None => {} }
            if best.is_none_or(|b| t < b.0) { best = Some((t, n, i)); }
        }
        best
    }
    pub fn raycast(&self, o: Vec3, d: Vec3, max: f32) -> Option<(f32, Vec3)> { self.raycast_hit(o, d, max).map(|h| (h.0, h.1)) }
    /// The highest collider top under `p` within `drop` below it (ground for a character).
    pub fn support_height(&self, p: Vec3, drop: f32) -> Option<f32> {
        self.near(p - Vec3::new(0.0001, drop + 0.01, 0.0001), p + Vec3::new(0.0001, 0.01, 0.0001)).into_iter().map(|i| &self.colliders[i])
            .filter(|b| p.x >= b.min.x - 1e-4 && p.x <= b.max.x + 1e-4 && p.z >= b.min.z - 1e-4 && p.z <= b.max.z + 1e-4 && b.max.y <= p.y + 1e-4 && b.max.y >= p.y - drop)
            .map(|b| b.max.y).max_by(f32::total_cmp)
    }

    /// Flicker and blackouts; bulbs dim with their lamps.
    pub fn update_lights(&mut self, t: f32, dt: f32) {
        for i in 0..self.lights.len() {
            let (r, r2) = (self.rand(), self.rand());
            let l = &mut self.lights[i];
            let mut f = 1.0;
            if l.flicker > 0.0 { let n = crate::noise::perlin(l.seed, t * 9.0); f = 1.0 - l.flicker * n * n * 1.6; }
            if l.broken {
                if l.blackout > 0.0 { l.blackout -= dt; f *= 0.04; } else if r < dt * 0.35 { l.blackout = 0.04 + r2 * 0.66; }
            }
            l.current = l.intensity * f.max(0.0);
            if let Some(b) = l.bulb { let k = f.clamp(0.0, 1.0) * 1.3; if let Some(o) = self.objs.get_mut(b) { o.color = Vec4::new(k, k, k, 1.0); } }
        }
    }
}

/// A model's exact surface for rays: triangles simplified to ~5 mm in model space (a 90k-triangle prop needs no more).
fn model_shape(model: &Model) -> Shape {
    let mut tris = vec![];
    for m in &model.meshes {
        let pos: Vec<[f32; 3]> = m.verts.iter().map(|v| v.pos.into()).collect();
        let mut ix = m.indices.clone();
        if ix.len() > 3000 {
            if let Ok(a) = meshopt::VertexDataAdapter::new(bytemuck::cast_slice(&pos), 12, 0) {
                let s = meshopt::simplify(&ix, &a, 0, 0.005, meshopt::SimplifyOptions::ErrorAbsolute, None);
                if !s.is_empty() { ix = s; }
            }
        }
        for t in ix.chunks_exact(3) { tris.push([0, 1, 2].map(|k| m.verts[t[k] as usize].pos)); }
    }
    Shape::new(tris)
}

/// One box round a turned car is much wider than the car: the model is sliced along its long side (up to eight
/// 0.5 m slices), each boxed in world space. Points along every triangle edge go in (a long plank has vertices only
/// at its ends). An empty slice comes back infinite.
pub fn model_boxes(model: &Model, transform: Mat4) -> Vec<(Vec3, Vec3)> {
    let (mut mlo, mut mhi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for sm in &model.meshes { for v in &sm.verts { mlo = mlo.min(v.pos); mhi = mhi.max(v.pos); } }
    let ext = mhi - mlo;
    let axis = if ext.x >= ext.z { 0 } else { 2 };
    let n = ((ext[axis] / 0.5).ceil() as usize).clamp(1, 8);
    let mut boxes = vec![(Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)); n];
    let mut add = |q: Vec3| {
        let k = (((q[axis] - mlo[axis]) / ext[axis].max(1e-4)) * n as f32).floor().clamp(0.0, (n - 1) as f32) as usize;
        let w = transform.transform_point3(q);
        boxes[k].0 = boxes[k].0.min(w); boxes[k].1 = boxes[k].1.max(w);
    };
    for sm in &model.meshes { for t in sm.indices.chunks_exact(3) {
        let v = [0, 1, 2].map(|i| sm.verts[t[i] as usize].pos);
        for (a, b) in [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])] {
            let steps = ((a.distance(b) / 0.2).ceil() as usize).max(1);
            for i in 0..steps { add(a.lerp(b, i as f32 / steps as f32)); }
        }
    } }
    boxes
}
