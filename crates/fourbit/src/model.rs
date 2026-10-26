//! Engine model format (.wmdl): skeleton, skinned meshes and dense animation clips, in engine space
//! (left-handed, Y up, Z forward, metres). Written by `wibe-import`, read by the game.
use glam::{Mat4, Quat, Vec2, Vec3};

pub const INCH: f32 = 0.0254;

#[derive(Clone)]
pub struct BoneDef {
    pub name: String,
    pub parent: i32,
    pub pos: Vec3,
    pub rot: Quat,
}

#[derive(Clone, Copy, Default)]
pub struct SkinVert {
    pub pos: Vec3,
    pub normal: Vec3,
    pub uv: Vec2,
    pub joints: [u16; 4],
    pub weights: [f32; 4],
}

pub struct SkinMesh {
    pub body: String,
    pub material: String,
    pub texture: String, // png next to the .wmdl, "" if none
    pub verts: Vec<SkinVert>,
    pub indices: Vec<u32>,
}

impl SkinMesh {
    /// Older WMDLs reversed Source triangles twice. Repair that at load as well as for new imports.
    fn face_outward(&mut self) {
        let alignment: f32 = self.indices.chunks_exact(3).map(|t| {
            let [a,b,c] = [&self.verts[t[0] as usize], &self.verts[t[1] as usize], &self.verts[t[2] as usize]];
            (b.pos-a.pos).cross(c.pos-a.pos).dot(a.normal+b.normal+c.normal)
        }).sum();
        if alignment < 0.0 { for t in self.indices.chunks_exact_mut(3) { t.swap(1,2); } }
    }
}

pub struct Clip {
    pub name: String,
    pub fps: f32,
    pub looping: bool,
    pub frames: Vec<Vec<(Vec3, Quat)>>,
}

impl Clip {
    pub fn length(&self) -> f32 { (self.frames.len().max(2) - 1) as f32 / self.fps.max(1.0) }

    /// Local pose at time t (seconds), linear/slerp between frames.
    pub fn sample(&self, t: f32, out: &mut [(Vec3, Quat)]) {
        let n = self.frames.len();
        let f = (t * self.fps).max(0.0);
        let f = if self.looping && n > 1 { f % (n - 1) as f32 } else { f.min((n - 1) as f32) };
        let (i, k) = (f.floor() as usize, f.fract());
        let (a, b) = (&self.frames[i.min(n - 1)], &self.frames[(i + 1).min(n - 1)]);
        for (j, o) in out.iter_mut().enumerate() {
            *o = (a[j].0.lerp(b[j].0, k), a[j].1.slerp(b[j].1, k));
        }
    }
}

pub struct Model {
    pub bones: Vec<BoneDef>,
    pub meshes: Vec<SkinMesh>,
    pub clips: Vec<Clip>,
}

// Source: X forward, Y left, Z up (right-handed). Engine: X right, Y up, Z forward (left-handed).
fn conv_v(v: Vec3) -> Vec3 { Vec3::new(-v.y, v.z, v.x) }
fn conv_q(q: Quat) -> Quat {
    // improper basis change: the axis maps like a pseudovector (sign flip), the angle stays
    let a = conv_v(Vec3::new(q.x, q.y, q.z));
    Quat::from_xyzw(-a.x, -a.y, -a.z, q.w)
}

impl Model {
    pub fn from_source(m: &crate::source::mdl::Model, keep: &dyn Fn(&str) -> bool) -> Model {
        let bones: Vec<BoneDef> = m.bones.iter().map(|b| BoneDef {
            name: b.name.clone(), parent: b.parent, pos: conv_v(b.pos) * INCH, rot: conv_q(b.quat),
        }).collect();
        let meshes = m.meshes.iter().filter(|me| keep(&me.body)).map(|me| SkinMesh {
            body: me.body.clone(),
            material: me.material.clone(),
            texture: String::new(),
            verts: me.verts.iter().map(|v| {
                let mut joints = [0u16; 4];
                let mut weights = [0f32; 4];
                for k in 0..(v.nbones as usize).min(3) {
                    joints[k] = v.bones[k] as u16;
                    weights[k] = v.weights[k];
                }
                if v.nbones == 0 { weights[0] = 1.0; }
                SkinVert { pos: conv_v(v.pos) * INCH, normal: conv_v(v.normal), uv: Vec2::new(v.uv.x, 1.0 - v.uv.y), joints, weights }
            }).collect(),
            indices: me.indices.clone(), // Source VTX winding already matches the engine after the basis change
        }).collect();
        let clips = m.anims.iter().map(|a| Clip {
            name: a.name.clone(),
            fps: a.fps,
            looping: a.looping,
            frames: a.frames.iter().map(|f| f.iter().enumerate().map(|(i, (p, q))| {
                if a.animated[i] { (conv_v(*p) * INCH, conv_q(*q)) } else { (bones[i].pos, bones[i].rot) }
            }).collect()).collect(),
        }).collect();
        Model { bones, meshes, clips }
    }

    pub fn bone(&self, name: &str) -> Option<usize> { self.bones.iter().position(|b| b.name == name) }
    pub fn clip(&self, name: &str) -> Option<usize> { self.clips.iter().position(|c| c.name == name) }

    pub fn rest_local(&self) -> Vec<(Vec3, Quat)> { self.bones.iter().map(|b| (b.pos, b.rot)).collect() }

    /// Model-space bone matrices from local transforms (parents come before children in Source skeletons).
    pub fn world(&self, local: &[(Vec3, Quat)]) -> Vec<Mat4> {
        let mut w: Vec<Mat4> = Vec::with_capacity(local.len());
        for (i, b) in self.bones.iter().enumerate() {
            let m = Mat4::from_rotation_translation(local[i].1, local[i].0);
            w.push(if b.parent >= 0 { w[b.parent as usize] * m } else { m });
        }
        w
    }

    pub fn inverse_bind(&self) -> Vec<Mat4> { self.world(&self.rest_local()).iter().map(|m| m.inverse()).collect() }

    // ------------------------------------------------------------------ file io
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut w = W(Vec::new());
        w.0.extend_from_slice(b"WMDL1");
        w.u32(self.bones.len() as u32);
        for b in &self.bones { w.s(&b.name); w.i32(b.parent); w.v3(b.pos); w.q(b.rot); }
        w.u32(self.meshes.len() as u32);
        for m in &self.meshes {
            w.s(&m.body); w.s(&m.material); w.s(&m.texture);
            w.u32(m.verts.len() as u32);
            for v in &m.verts {
                w.v3(v.pos); w.v3(v.normal); w.f(v.uv.x); w.f(v.uv.y);
                for j in v.joints { w.0.extend_from_slice(&j.to_le_bytes()); }
                for k in v.weights { w.f(k); }
            }
            w.u32(m.indices.len() as u32);
            for i in &m.indices { w.u32(*i); }
        }
        w.u32(self.clips.len() as u32);
        for c in &self.clips {
            w.s(&c.name); w.f(c.fps); w.u32(c.looping as u32); w.u32(c.frames.len() as u32);
            for f in &c.frames { for (p, q) in f { w.v3(*p); w.q(*q); } }
        }
        std::fs::write(path, w.0)
    }

    pub fn load(path: &std::path::Path) -> Result<Model, String> {
        let d = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !d.starts_with(b"WMDL1") { return Err("not a wmdl".into()); }
        let mut r = Rd { d: &d, o: 5 };
        let nb = r.u32() as usize;
        let bones: Vec<BoneDef> = (0..nb).map(|_| BoneDef { name: r.s(), parent: r.i32(), pos: r.v3(), rot: r.q() }).collect();
        let meshes = (0..r.u32()).map(|_| {
            let (body, material, texture) = (r.s(), r.s(), r.s());
            let verts = (0..r.u32()).map(|_| SkinVert {
                pos: r.v3(), normal: r.v3(), uv: Vec2::new(r.f(), r.f()),
                joints: [r.u16(), r.u16(), r.u16(), r.u16()], weights: [r.f(), r.f(), r.f(), r.f()],
            }).collect();
            let indices = (0..r.u32()).map(|_| r.u32()).collect();
            let mut mesh = SkinMesh { body, material, texture, verts, indices };
            mesh.face_outward();
            mesh
        }).collect();
        let clips = (0..r.u32()).map(|_| {
            let (name, fps, looping, nf) = (r.s(), r.f(), r.u32() != 0, r.u32());
            Clip { name, fps, looping, frames: (0..nf).map(|_| (0..nb).map(|_| (r.v3(), r.q())).collect()).collect() }
        }).collect();
        Ok(Model { bones, meshes, clips })
    }
}

struct W(Vec<u8>);
impl W {
    fn u32(&mut self, v: u32) { self.0.extend_from_slice(&v.to_le_bytes()); }
    fn i32(&mut self, v: i32) { self.0.extend_from_slice(&v.to_le_bytes()); }
    fn f(&mut self, v: f32) { self.0.extend_from_slice(&v.to_le_bytes()); }
    fn v3(&mut self, v: Vec3) { self.f(v.x); self.f(v.y); self.f(v.z); }
    fn q(&mut self, q: Quat) { self.f(q.x); self.f(q.y); self.f(q.z); self.f(q.w); }
    fn s(&mut self, s: &str) { self.u32(s.len() as u32); self.0.extend_from_slice(s.as_bytes()); }
}

struct Rd<'a> { d: &'a [u8], o: usize }
impl Rd<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] { let a = self.d[self.o..self.o + N].try_into().unwrap(); self.o += N; a }
    fn u32(&mut self) -> u32 { u32::from_le_bytes(self.take()) }
    fn u16(&mut self) -> u16 { u16::from_le_bytes(self.take()) }
    fn i32(&mut self) -> i32 { i32::from_le_bytes(self.take()) }
    fn f(&mut self) -> f32 { f32::from_le_bytes(self.take()) }
    fn v3(&mut self) -> Vec3 { Vec3::new(self.f(), self.f(), self.f()) }
    fn q(&mut self) -> Quat { Quat::from_xyzw(self.f(), self.f(), self.f(), self.f()) }
    fn s(&mut self) -> String { let n = self.u32() as usize; let s = String::from_utf8_lossy(&self.d[self.o..self.o + n]).into_owned(); self.o += n; s }
}
