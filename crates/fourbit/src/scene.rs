//! Scene files (`scenes/<name>.json`): what the editor places in the world — models, lamps, boxes, the player start —
//! and the scene's lighting (`env`: sky, key light, fog, indoor zones). Plain JSON, easy for people, scripts and AI:
//! ```json
//! {"env": {"sky": [0.2,0.22,0.26], "key_dir": [-0.4,0.6,-0.5], "key": [0.55,0.48,0.4], "fog": [0.1,0.11,0.13], "fog_range": [30,160]},
//!  "entities": [
//!   {"name": "car", "type": "model", "model": "props/covered_car", "pos": [0,0,5], "yaw": 30, "scale": 1, "solid": true},
//!   {"name": "lamp", "type": "light", "pos": [0,3,5], "color": [1,0.85,0.6], "intensity": 1.5, "range": 8, "flicker": 0, "shadows": true},
//!   {"name": "floor", "type": "box", "pos": [0,-0.1,0], "size": [40,0.2,40], "texture": "", "tint": [0.5,0.5,0.5], "tile": 2},
//!   {"name": "start", "type": "spawn", "pos": [0,0,0], "yaw": 0}]}
//! ```
use crate::render::Env;
use crate::world::{Light, Material, Placement, World};
use glam::{Mat4, Vec3};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Model { model: String, solid: bool },
    Light { color: Vec3, intensity: f32, range: f32, flicker: f32, shadows: bool },
    Box { size: Vec3, texture: String, tint: Vec3, tile: f32 },
    Spawn,
}
impl Kind {
    pub fn label(&self) -> &'static str { match self { Kind::Model { .. } => "модель", Kind::Light { .. } => "лампа", Kind::Box { .. } => "коробка", Kind::Spawn => "старт" } }
}

/// What an entity became in the world.
#[derive(Clone, Debug, Default)]
pub struct Live { pub placement: Option<Placement>, pub light: Option<usize>, pub obj: Option<usize>, pub collider: Option<usize> }

#[derive(Clone, Debug)]
pub struct Ent { pub name: String, pub kind: Kind, pub pos: Vec3, pub yaw: f32, pub scale: f32, pub live: Live }
impl Ent {
    pub fn new(name: impl Into<String>, kind: Kind, pos: Vec3) -> Ent { Ent { name: name.into(), kind, pos, yaw: 0.0, scale: 1.0, live: Live::default() } }
}

#[derive(Default)]
pub struct Scene { pub path: PathBuf, pub ents: Vec<Ent>, pub env: Option<Env>, pub dirty: bool }

fn v3(v: &Value) -> Option<Vec3> { let a = v.as_array()?; Some(Vec3::new(a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32)) }
fn f(v: &Value, d: f32) -> f32 { v.as_f64().map_or(d, |x| x as f32) }
fn r(x: f32) -> f64 { (x as f64 * 1000.0).round() / 1000.0 }
fn arr(v: Vec3) -> Value { json!([r(v.x), r(v.y), r(v.z)]) }

pub fn env_from_json(e: &Value) -> Env {
    let mut env = Env::default();
    if let Some(v) = v3(&e["sky"]) { env.sky_ambient = v; }
    if let Some(v) = v3(&e["key_dir"]) { env.key_dir = v; }
    if let Some(v) = v3(&e["key"]) { env.key_colour = v; }
    env.key_wrap = f(&e["key_wrap"], env.key_wrap);
    if let Some(v) = v3(&e["indoor"]) { env.indoor_ambient = v; }
    if let Some(v) = v3(&e["fog"]) { env.fog_outdoor = v; }
    if let Some(a) = e["fog_range"].as_array() { env.fog_outdoor_range = (f(&a[0], 30.0), f(a.get(1).unwrap_or(&json!(160)), 160.0)); }
    if let Some(v) = v3(&e["fog_indoor"]) { env.fog_indoor = v; }
    if let Some(a) = e["fog_indoor_range"].as_array() { env.fog_indoor_range = (f(&a[0], 4.0), f(a.get(1).unwrap_or(&json!(40)), 40.0)); }
    env.storey = f(&e["storey"], 0.0);
    env.zone_top = f(&e["zone_top"], 1e6);
    for (k, z) in e["zones"].as_array().into_iter().flatten().take(4).enumerate() {
        if let (Some(a), Some(b)) = (v3(&z[0]), v3(&z[1])) { env.zones[k] = (a.min(b), a.max(b)); env.zone_count = k + 1; }
    }
    env
}
pub fn env_to_json(e: &Env) -> Value {
    json!({"sky": arr(e.sky_ambient), "key_dir": arr(e.key_dir), "key": arr(e.key_colour), "key_wrap": r(e.key_wrap), "indoor": arr(e.indoor_ambient),
           "fog": arr(e.fog_outdoor), "fog_range": [r(e.fog_outdoor_range.0), r(e.fog_outdoor_range.1)],
           "fog_indoor": arr(e.fog_indoor), "fog_indoor_range": [r(e.fog_indoor_range.0), r(e.fog_indoor_range.1)],
           "storey": r(e.storey), "zone_top": r(e.zone_top.min(1e6)),
           "zones": e.zones[..e.zone_count].iter().map(|z| json!([arr(z.0), arr(z.1)])).collect::<Vec<_>>()})
}

impl Scene {
    /// A missing file is an empty scene; a broken one is reported and loaded as empty (never fatal).
    pub fn load(path: &Path) -> Scene {
        let mut s = Scene { path: path.to_path_buf(), ..Default::default() };
        let Ok(text) = std::fs::read_to_string(path) else { return s };
        match serde_json::from_str::<Value>(&text) { Ok(v) => s.read(&v), Err(e) => eprintln!("scene {}: {e}", path.display()) }
        s
    }
    pub fn read(&mut self, v: &Value) {
        if v["env"].is_object() { self.env = Some(env_from_json(&v["env"])); }
        for e in v["entities"].as_array().into_iter().flatten() { if let Some(ent) = ent_from_json(e) { self.ents.push(ent); } }
    }
    pub fn to_json(&self) -> Value {
        let mut v = json!({"entities": self.ents.iter().map(ent_to_json).collect::<Vec<_>>()});
        if let Some(e) = &self.env { v["env"] = env_to_json(e); }
        v
    }
    pub fn save(&mut self) -> std::io::Result<()> {
        if let Some(d) = self.path.parent() { std::fs::create_dir_all(d)?; }
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.to_json()).unwrap_or_default() + "\n")?;
        self.dirty = false;
        Ok(())
    }
    /// The player start (first spawn entity): position and yaw.
    pub fn spawn(&self) -> Option<(Vec3, f32)> { self.ents.iter().find(|e| e.kind == Kind::Spawn).map(|e| (e.pos, e.yaw)) }

    /// Puts every entity into the world.
    pub fn apply(&mut self, world: &mut World) { for i in 0..self.ents.len() { self.place(world, i); } }
    /// Puts entity `i` into the world (after a load, or when the editor adds one).
    pub fn place(&mut self, world: &mut World, i: usize) {
        let e = &mut self.ents[i];
        match &e.kind {
            Kind::Model { model, solid } => e.live.placement = world.place_model(model, e.pos, e.yaw, e.scale, *solid),
            Kind::Light { color, intensity, range, flicker, shadows } => {
                let mut l = Light::new(e.pos, *color, *intensity, *range);
                l.flicker = *flicker; l.shadows = *shadows;
                world.lights.push(l);
                e.live.light = Some(world.lights.len() - 1);
            }
            Kind::Box { size, texture, tint, tile } => {
                let o = world.block(e.pos, *size * e.scale, &Material::tinted(texture, tint.x, tint.y, tint.z), *tile);
                world.objs[o].moving = true;
                e.live.obj = Some(o); e.live.collider = Some(world.colliders.len() - 1);
                world.reindex();
            }
            Kind::Spawn => {}
        }
    }
    /// Pushes an entity's edited fields into the world.
    pub fn sync(&mut self, world: &mut World, i: usize) {
        let e = &self.ents[i];
        if let Some(p) = &e.live.placement { world.move_model(p, e.pos, e.yaw, e.scale); }
        match &e.kind {
            Kind::Light { color, intensity, range, flicker, shadows } => if let Some(l) = e.live.light {
                let w = &mut world.lights[l];
                w.pos = e.pos; w.color = *color; w.intensity = *intensity; w.range = *range; w.flicker = *flicker; w.shadows = *shadows;
            },
            Kind::Box { size, texture, tint, tile } => {
                if let (Some(o), Some(c)) = (e.live.obj, e.live.collider) {
                    // a resized box needs a new mesh: rebuilt here, uploaded by the caller (`changed_mesh`)
                    let s = *size * e.scale;
                    let mut m = crate::mesh::MeshData::default(); m.add_box(Vec3::ZERO, s, *tile);
                    let old = world.colliders[c].max - world.colliders[c].min;
                    if (old - s).abs().max_element() > 1e-4 { world.objs[o].mesh = m; world.changed.push(o); }
                    let ob = &mut world.objs[o];
                    ob.mat = Material::tinted(texture, tint.x, tint.y, tint.z);
                    ob.model = Mat4::from_translation(e.pos);
                    world.colliders[c] = crate::world::Aabb::from_center(e.pos, s);
                    world.reindex();
                }
            }
            _ => {}
        }
        self.dirty = true;
    }
    /// Takes entity `i` out of the world and the scene.
    pub fn remove(&mut self, world: &mut World, i: usize) -> Ent {
        let e = self.ents.remove(i);
        if let Some(p) = &e.live.placement { world.remove_model(p); }
        if let Some(l) = e.live.light { let w = &mut world.lights[l]; w.intensity = 0.0; w.current = 0.0; w.range = 0.01; }
        if let Some(o) = e.live.obj { world.objs[o].visible = false; }
        if let Some(c) = e.live.collider { world.colliders[c] = crate::world::Aabb { min: Vec3::splat(-1e5), max: Vec3::splat(-1e5) }; world.reindex(); }
        self.dirty = true;
        e
    }
}

pub fn ent_from_json(e: &Value) -> Option<Ent> {
    let pos = v3(&e["pos"])?;
    let kind = match e["type"].as_str()? {
        "model" | "prop" => Kind::Model { model: e["model"].as_str()?.to_string(), solid: e["solid"].as_bool().unwrap_or(true) },
        "light" => Kind::Light { color: v3(&e["color"]).unwrap_or(Vec3::ONE), intensity: f(&e["intensity"], 1.0), range: f(&e["range"], 6.0),
                                 flicker: f(&e["flicker"], 0.0), shadows: e["shadows"].as_bool().unwrap_or(true) },
        "box" => Kind::Box { size: v3(&e["size"]).unwrap_or(Vec3::ONE), texture: e["texture"].as_str().unwrap_or("").into(), tint: v3(&e["tint"]).unwrap_or(Vec3::splat(0.6)), tile: f(&e["tile"], 1.0) },
        "spawn" => Kind::Spawn,
        _ => return None,
    };
    Some(Ent { name: e["name"].as_str().unwrap_or(kind.label()).to_string(), kind, pos, yaw: f(&e["yaw"], 0.0), scale: f(&e["scale"], 1.0), live: Live::default() })
}
pub fn ent_to_json(e: &Ent) -> Value {
    let mut v = match &e.kind {
        Kind::Model { model, solid } => json!({"type": "model", "model": model, "solid": solid}),
        Kind::Light { color, intensity, range, flicker, shadows } => json!({"type": "light", "color": arr(*color), "intensity": r(*intensity), "range": r(*range), "flicker": r(*flicker), "shadows": shadows}),
        Kind::Box { size, texture, tint, tile } => json!({"type": "box", "size": arr(*size), "texture": texture, "tint": arr(*tint), "tile": r(*tile)}),
        Kind::Spawn => json!({"type": "spawn"}),
    };
    v["name"] = json!(e.name); v["pos"] = arr(e.pos); v["yaw"] = json!(r(e.yaw)); v["scale"] = json!(r(e.scale));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn round_trip() {
        let mut s = Scene::default();
        s.ents.push(Ent::new("car", Kind::Model { model: "props/covered_car".into(), solid: true }, Vec3::new(1.0, 0.0, 2.5)));
        s.ents.push(Ent::new("lamp", Kind::Light { color: Vec3::new(1.0, 0.8, 0.6), intensity: 2.0, range: 5.0, flicker: 0.1, shadows: false }, Vec3::Y));
        s.ents.push(Ent::new("floor", Kind::Box { size: Vec3::new(10.0, 0.2, 10.0), texture: "grid".into(), tint: Vec3::ONE, tile: 2.0 }, Vec3::ZERO));
        s.ents.push(Ent::new("start", Kind::Spawn, Vec3::new(0.0, 0.0, -3.0)));
        let mut env = Env::default(); env.zone_count = 1; env.zones[0] = (Vec3::ZERO, Vec3::ONE); env.storey = 3.0;
        s.env = Some(env);
        let v = s.to_json();
        let mut t = Scene::default(); t.read(&v);
        assert_eq!(t.ents.len(), 4);
        for (a, b) in s.ents.iter().zip(&t.ents) { assert_eq!(a.kind, b.kind); assert_eq!(a.pos, b.pos); assert_eq!(a.name, b.name); }
        let e = t.env.unwrap(); assert_eq!(e.zone_count, 1); assert_eq!(e.storey, 3.0);
        assert_eq!(t.spawn().unwrap().0, Vec3::new(0.0, 0.0, -3.0));
        assert!(Scene::load(Path::new("/nonexistent/s.json")).ents.is_empty());
    }
}
