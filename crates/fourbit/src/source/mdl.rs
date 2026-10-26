//! Source engine model reader (MDL v44-49 + VVD + VTX). Produces skeleton, LOD0 meshes and every sequence's
//! first-blend animation decoded to dense per-frame local bone transforms, all still in Source space
//! (X forward, Y left, Z up, inches). Conversion to engine space happens in `model::from_source`.
use glam::{Quat, Vec2, Vec3};
use std::path::Path;

pub struct Bone {
    pub name: String,
    pub parent: i32,
    pub pos: Vec3,
    pub quat: Quat,
    pub rot: Vec3, // RadianEuler
    pub posscale: Vec3,
    pub rotscale: Vec3,
}

pub struct Vert {
    pub pos: Vec3,
    pub normal: Vec3,
    pub uv: Vec2,
    pub bones: [u8; 3],
    pub weights: [f32; 3],
    pub nbones: u8,
}

pub struct Mesh {
    pub body: String,   // bodypart model name
    pub material: String,
    pub verts: Vec<Vert>,
    pub indices: Vec<u32>,
}

pub struct Anim {
    pub name: String,
    pub fps: f32,
    pub looping: bool,
    /// frames[f][bone] = (pos, quat) local to the parent bone
    pub frames: Vec<Vec<(Vec3, Quat)>>,
    /// bones this sequence actually animates (weightlist > 0 and present in the anim data)
    pub animated: Vec<bool>,
}

pub struct Model {
    pub bones: Vec<Bone>,
    pub meshes: Vec<Mesh>,
    pub anims: Vec<Anim>,
    pub cdmaterials: Vec<String>,
}

struct R<'a>(&'a [u8]);
impl R<'_> {
    fn i32(&self, o: usize) -> i32 { i32::from_le_bytes(self.0[o..o + 4].try_into().unwrap()) }
    fn u16(&self, o: usize) -> u16 { u16::from_le_bytes(self.0[o..o + 2].try_into().unwrap()) }
    fn i16(&self, o: usize) -> i16 { i16::from_le_bytes(self.0[o..o + 2].try_into().unwrap()) }
    fn f32(&self, o: usize) -> f32 { f32::from_le_bytes(self.0[o..o + 4].try_into().unwrap()) }
    fn u8(&self, o: usize) -> u8 { self.0[o] }
    fn v3(&self, o: usize) -> Vec3 { Vec3::new(self.f32(o), self.f32(o + 4), self.f32(o + 8)) }
    fn str(&self, o: usize) -> String {
        let end = self.0[o..].iter().position(|&b| b == 0).map_or(self.0.len(), |e| o + e);
        String::from_utf8_lossy(&self.0[o..end]).into_owned()
    }
}

fn half(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f32;
    if e == 0 { return s * m * 2f32.powi(-24); }
    if e == 31 { return s * f32::INFINITY; }
    s * (1.0 + m / 1024.0) * 2f32.powi(e - 15)
}

/// Source AngleQuaternion(RadianEuler)
pub fn euler_quat(a: Vec3) -> Quat {
    let (sr, cr) = (a.x * 0.5).sin_cos();
    let (sp, cp) = (a.y * 0.5).sin_cos();
    let (sy, cy) = (a.z * 0.5).sin_cos();
    Quat::from_xyzw(
        sr * cp * cy - cr * sp * sy,
        cr * sp * cy + sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
        cr * cp * cy + sr * sp * sy,
    )
}

/// ExtractAnimValue: run-length compressed shorts.
fn anim_value(r: &R, mut p: usize, frame: usize, scale: f32) -> f32 {
    let mut k = frame as i32;
    loop {
        let valid = r.u8(p) as i32;
        let total = r.u8(p + 1) as i32;
        if total == 0 { return 0.0; } // corrupt / empty
        if total > k {
            let idx = if valid > k { k + 1 } else { valid };
            return r.i16(p + idx as usize * 2) as f32 * scale;
        }
        k -= total;
        p += (valid as usize + 1) * 2;
    }
}

pub fn load(mdl_path: &Path) -> Result<Model, String> {
    let data = std::fs::read(mdl_path).map_err(|e| format!("{}: {e}", mdl_path.display()))?;
    let r = R(&data);
    if &data[0..4] != b"IDST" { return Err("not an MDL".into()); }
    let version = r.i32(4);
    if !(44..=49).contains(&version) { return Err(format!("MDL version {version} not supported")); }

    // ---- bones
    let (nb, bi) = (r.i32(156) as usize, r.i32(160) as usize);
    let mut bones = Vec::with_capacity(nb);
    for i in 0..nb {
        let o = bi + i * 216;
        bones.push(Bone {
            name: r.str(o + r.i32(o) as usize),
            parent: r.i32(o + 4),
            pos: r.v3(o + 32),
            quat: Quat::from_xyzw(r.f32(o + 44), r.f32(o + 48), r.f32(o + 52), r.f32(o + 56)),
            rot: r.v3(o + 60),
            posscale: r.v3(o + 72),
            rotscale: r.v3(o + 84),
        });
    }

    // ---- materials: texture names, cd paths, skin family 0
    let tex_names: Vec<String> = (0..r.i32(204) as usize).map(|i| {
        let o = r.i32(208) as usize + i * 64;
        r.str(o + r.i32(o) as usize)
    }).collect();
    let cdmaterials: Vec<String> = (0..r.i32(212) as usize).map(|i| r.str(r.i32(r.i32(216) as usize + i * 4) as usize)).collect();
    let numskinref = r.i32(220) as usize;
    let skin = |m: usize| -> usize { if m < numskinref { r.i16(r.i32(228) as usize + m * 2) as usize } else { m } };

    // ---- meshes: MDL structure + VVD vertices + VTX LOD0 strips
    let base = mdl_path.with_extension("");
    let vvd = std::fs::read(base.with_extension("vvd")).map_err(|e| format!("vvd: {e}"))?;
    let vtx = ["dx90.vtx", "vtx", "dx80.vtx", "sw.vtx"].iter()
        .find_map(|e| std::fs::read(base.with_extension(e)).ok()).ok_or("no .vtx")?;
    let vv = R(&vvd);
    let all_verts = {
        let num = vv.i32(16) as usize; // LOD0 count
        let (nfix, fixstart, vstart) = (vv.i32(48) as usize, vv.i32(52) as usize, vv.i32(56) as usize);
        let read = |i: usize| {
            let o = vstart + i * 48;
            Vert {
                weights: [vv.f32(o), vv.f32(o + 4), vv.f32(o + 8)],
                bones: [vv.u8(o + 12), vv.u8(o + 13), vv.u8(o + 14)],
                nbones: vv.u8(o + 15),
                pos: vv.v3(o + 16),
                normal: vv.v3(o + 28),
                uv: Vec2::new(vv.f32(o + 40), vv.f32(o + 44)),
            }
        };
        let mut out = vec![];
        if nfix == 0 {
            for i in 0..num { out.push(read(i)); }
        } else {
            for f in 0..nfix {
                let o = fixstart + f * 12;
                if vv.i32(o) >= 0 {
                    let (src, n) = (vv.i32(o + 4) as usize, vv.i32(o + 8) as usize);
                    for i in src..src + n { out.push(read(i)); }
                }
            }
        }
        out
    };
    let vt = R(&vtx);
    let mut meshes = vec![];
    let (nbp, bpi) = (r.i32(232) as usize, r.i32(236) as usize);
    let vtx_bp = vt.i32(32) as usize;
    for b in 0..nbp {
        let bo = bpi + b * 16;
        let (nm, mi) = (r.i32(bo + 4) as usize, r.i32(bo + 12) as usize);
        let vbo = vtx_bp + b * 8;
        let vtx_models = vbo + vt.i32(vbo + 4) as usize;
        for m in 0..nm {
            let mo = bo + mi + m * 148;
            let mname = r.str(mo);
            let model_vstart = r.i32(mo + 84) as usize / 48;
            let (nmesh, meshi) = (r.i32(mo + 72) as usize, r.i32(mo + 76) as usize);
            let vmo = vtx_models + m * 8;
            if vt.i32(vmo) == 0 { continue; }
            let lod0 = vmo + vt.i32(vmo + 4) as usize;
            let vmesh0 = lod0 + vt.i32(lod0 + 4) as usize;
            for k in 0..nmesh.min(vt.i32(lod0) as usize) {
                let me = mo + meshi + k * 116;
                let material = tex_names.get(skin(r.i32(me) as usize)).cloned().unwrap_or_default();
                let mesh_vstart = model_vstart + r.i32(me + 12) as usize;
                let vm = vmesh0 + k * 9;
                let (nsg, sgo) = (vt.i32(vm) as usize, vt.i32(vm + 4) as usize);
                let mut out = Mesh { body: mname.clone(), material, verts: vec![], indices: vec![] };
                for s in 0..nsg {
                    let sg = vm + sgo + s * 25;
                    let (nv, vo, ni, io) = (vt.i32(sg) as usize, vt.i32(sg + 4) as usize, vt.i32(sg + 8) as usize, vt.i32(sg + 12) as usize);
                    let first = out.verts.len() as u32;
                    for v in 0..nv {
                        let orig = vt.u16(sg + vo + v * 9 + 4) as usize;
                        let src = &all_verts[(mesh_vstart + orig).min(all_verts.len() - 1)];
                        out.verts.push(Vert { pos: src.pos, normal: src.normal, uv: src.uv, bones: src.bones, weights: src.weights, nbones: src.nbones });
                    }
                    for i in 0..ni {
                        out.indices.push(first + vt.u16(sg + io + i * 2) as u32);
                    }
                }
                if !out.indices.is_empty() { meshes.push(out); }
            }
        }
    }

    // ---- animations (per sequence, first blend)
    let (na, ai) = (r.i32(180) as usize, r.i32(184) as usize);
    let decode = |a: usize| -> (f32, usize, Vec<Vec<(Vec3, Quat)>>, Vec<bool>) {
        let o = ai + a * 100;
        let fps = r.f32(o + 8);
        let flags = r.i32(o + 12);
        let nframes = (r.i32(o + 16) as usize).max(1);
        let (animblock, animindex) = (r.i32(o + 52), r.i32(o + 56));
        let (sectionindex, sectionframes) = (r.i32(o + 80) as usize, r.i32(o + 84) as usize);
        let rest: Vec<(Vec3, Quat)> = bones.iter().map(|b| (b.pos, b.quat)).collect();
        let mut animated = vec![false; nb];
        let mut frames = vec![rest.clone(); nframes];
        if flags & 0x20 != 0 || animblock != 0 { return (fps, nframes, frames, animated); } // all zeros / external .ani
        for (f, frame) in frames.iter_mut().enumerate() {
            // sections: each holds `sectionframes` frames of its own anim chain
            let (mut p, local) = if sectionframes > 0 && sectionindex != 0 {
                let s = f / sectionframes;
                let so = o + sectionindex + s * 8;
                if r.i32(so) != 0 { continue; }
                (o + r.i32(so + 4) as usize, f - s * sectionframes)
            } else {
                (o + animindex as usize, f)
            };
            if animindex == 0 && sectionindex == 0 { break; }
            loop {
                let bone = r.u8(p) as usize;
                let aflags = r.u8(p + 1);
                let next = r.i16(p + 2);
                if bone < nb {
                    let b = &bones[bone];
                    let d = p + 4;
                    let delta = aflags & 0x10 != 0;
                    let mut q = if delta { Quat::IDENTITY } else { b.quat };
                    let mut pos = if delta { Vec3::ZERO } else { b.pos };
                    let mut rot_size = 0;
                    if aflags & 0x02 != 0 {
                        // Quaternion48
                        let x = (r.u16(d) as f32 - 32768.0) / 32768.0;
                        let y = (r.u16(d + 2) as f32 - 32768.0) / 32768.0;
                        let zw = r.u16(d + 4);
                        let z = ((zw & 0x7fff) as f32 - 16384.0) / 16384.0;
                        let mut w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
                        if zw & 0x8000 != 0 { w = -w; }
                        q = Quat::from_xyzw(x, y, z, w);
                        rot_size = 6;
                    } else if aflags & 0x20 != 0 {
                        // Quaternion64
                        let v = u64::from_le_bytes(data[d..d + 8].try_into().unwrap());
                        let x = ((v & 0x1fffff) as f32 - 1048576.0) / 1048576.5;
                        let y = (((v >> 21) & 0x1fffff) as f32 - 1048576.0) / 1048576.5;
                        let z = (((v >> 42) & 0x1fffff) as f32 - 1048576.0) / 1048576.5;
                        let mut w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
                        if v >> 63 != 0 { w = -w; }
                        q = Quat::from_xyzw(x, y, z, w);
                        rot_size = 8;
                    } else if aflags & 0x08 != 0 {
                        let mut e = if delta { Vec3::ZERO } else { b.rot };
                        for j in 0..3 {
                            let off = r.i16(d + j * 2);
                            if off != 0 { e[j] += anim_value(&r, d + off as usize, local, b.rotscale[j]); }
                        }
                        q = euler_quat(e);
                        rot_size = 6;
                    }
                    let pd = d + rot_size;
                    if aflags & 0x01 != 0 {
                        pos = Vec3::new(half(r.u16(pd)), half(r.u16(pd + 2)), half(r.u16(pd + 4)));
                    } else if aflags & 0x04 != 0 {
                        for j in 0..3 {
                            let off = r.i16(pd + j * 2);
                            if off != 0 { pos[j] += anim_value(&r, pd + off as usize, local, b.posscale[j]); }
                        }
                    }
                    if !delta { frame[bone] = (pos, q.normalize()); }
                    animated[bone] = true;
                }
                if next == 0 { break; }
                p = (p as isize + next as isize) as usize;
            }
        }
        (fps, nframes, frames, animated)
    };

    let (ns, si) = (r.i32(188) as usize, r.i32(192) as usize);
    let mut anims = vec![];
    for s in 0..ns {
        let o = si + s * 212;
        let label = r.str(o + r.i32(o + 4) as usize);
        let seq_flags = r.i32(o + 12);
        let anim_idx = r.i16(o + r.i32(o + 60) as usize) as usize;
        if anim_idx >= na { continue; }
        let (fps, _, frames, mut animated) = decode(anim_idx);
        let wl = r.i32(o + 156) as usize;
        if wl != 0 {
            for (bone, a) in animated.iter_mut().enumerate() {
                if r.f32(o + wl + bone * 4) <= 0.0 { *a = false; }
            }
        }
        anims.push(Anim { name: label, fps, looping: seq_flags & 1 != 0, frames, animated });
    }
    Ok(Model { bones, meshes, anims, cdmaterials })
}
