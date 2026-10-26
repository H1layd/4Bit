//! CPU-side mesh building: boxes with world-space tiled UVs, low-poly pipes, quads.
use glam::{Quat, Vec2, Vec3};

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

#[derive(Default, Clone)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn push(&mut self, p: Vec3, n: Vec3, uv: Vec2) -> u32 {
        self.vertices.push(Vertex { pos: p.into(), normal: n.into(), uv: uv.into() });
        self.vertices.len() as u32 - 1
    }

    /// Planar quad a-b-c-d (any winding; drawn double-sided), UVs tiled in its own plane every `tile` metres.
    pub fn add_quad4(&mut self, q: [Vec3; 4], tile: f32) {
        let n = (q[1] - q[0]).cross(q[2] - q[0]).normalize_or_zero();
        let u = (q[1] - q[0]).normalize_or_zero();
        let v = n.cross(u);
        let s = self.vertices.len() as u32;
        for p in q { self.push(p, n, Vec2::new(p.dot(u), p.dot(v)) / tile); }
        self.indices.extend_from_slice(&[s, s + 1, s + 2, s, s + 2, s + 3]);
    }


    pub fn append(&mut self, other: &MeshData) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    /// Box centred at `center` (world), UVs tiled in world space every `tile` metres. One quad per face: light and fog
    /// are per pixel, so the old 0.8 m split (from the vertex-lit Unity build) only multiplied the triangle count.
    pub fn add_box(&mut self, center: Vec3, size: Vec3, tile: f32) {
        let cell = f32::INFINITY;
        let h = size * 0.5;
        let mut face = |normal: Vec3, up: Vec3| {
            let right = normal.cross(up);
            let lu = size.dot(right).abs();
            let lv = size.dot(up).abs();
            let ln = h.dot(normal).abs();
            let nu = ((lu / cell).ceil() as usize).max(1);
            let nv = ((lv / cell).ceil() as usize).max(1);
            let start = self.vertices.len() as u32;
            for j in 0..=nv {
                for i in 0..=nu {
                    let p = normal * ln + right * (i as f32 / nu as f32 - 0.5) * lu + up * (j as f32 / nv as f32 - 0.5) * lv;
                    let w = p + center;
                    self.push(w, normal, Vec2::new(w.dot(right), w.dot(up)) / tile);
                }
            }
            for j in 0..nv {
                for i in 0..nu {
                    let a = start + (j * (nu + 1) + i) as u32;
                    let b = a + 1;
                    let c = a + nu as u32 + 1;
                    let d = c + 1;
                    self.indices.extend_from_slice(&[a, c, d, a, d, b]);
                }
            }
        };
        face(Vec3::X, Vec3::Y);
        face(Vec3::NEG_X, Vec3::Y);
        face(Vec3::Z, Vec3::Y);
        face(Vec3::NEG_Z, Vec3::Y);
        face(Vec3::Y, Vec3::Z);
        face(Vec3::NEG_Y, Vec3::NEG_Z);
    }

    /// Rounded architectural edges have real normals/silhouettes instead of coplanar trim overlays.
    pub fn add_beveled_box(&mut self, center:Vec3,size:Vec3,tile:f32,bevel:f32) {
        let h=size*0.5;let b=bevel.min(h.min_element()*0.45);
        for (normal,up) in [(Vec3::X,Vec3::Y),(Vec3::NEG_X,Vec3::Y),(Vec3::Z,Vec3::Y),(Vec3::NEG_Z,Vec3::Y),(Vec3::Y,Vec3::Z),(Vec3::NEG_Y,Vec3::NEG_Z)] {
            let right=normal.cross(up);let hu=h.dot(right).abs();let hv=h.dot(up).abs();let hn=h.dot(normal).abs();let start=self.vertices.len() as u32;
            for v in [-hv,-hv+b,hv-b,hv] { for u in [-hu,-hu+b,hu-b,hu] {
                let p=normal*hn+right*u+up*v;let inner=p.clamp(-h+Vec3::splat(b),h-Vec3::splat(b));let n=(p-inner).normalize();let w=center+inner+n*b;
                self.push(w,n,Vec2::new(w.dot(right),w.dot(up))/tile);
            }}
            for j in 0..3 { for i in 0..3 { let a=start+j*4+i;self.indices.extend_from_slice(&[a,a+4,a+5,a,a+5,a+1]); }}
        }
    }

    /// Six-sided cylinder from a to b.
    pub fn add_pipe(&mut self, a: Vec3, b: Vec3, radius: f32) {
        let sides = 16;
        let len = a.distance(b);
        let rot = Quat::from_rotation_arc(Vec3::Y, (b - a).normalize());
        let mid = (a + b) * 0.5;
        let segs = ((len / 1.0).ceil() as usize).max(1);
        let start = self.vertices.len() as u32;
        for s in 0..=segs {
            for i in 0..=sides {
                let ang = i as f32 / sides as f32 * std::f32::consts::TAU;
                let dir = Vec3::new(ang.cos(), 0.0, ang.sin());
                let p = dir * radius + Vec3::Y * (s as f32 / segs as f32 - 0.5) * len;
                self.push(mid + rot * p, rot * dir, Vec2::new(i as f32 / sides as f32, s as f32 / segs as f32 * len / 1.5));
            }
        }
        for s in 0..segs {
            for i in 0..sides {
                let a0 = start + (s * (sides + 1) + i) as u32;
                let a1 = a0 + 1;
                let b0 = a0 + sides as u32 + 1;
                let b1 = b0 + 1;
                self.indices.extend_from_slice(&[a0, b0, b1, a0, b1, a1]);
            }
        }
    }

    /// Flat quad facing `normal`, rotated by `spin` degrees around it. UVs 0..1.
    pub fn add_quad(&mut self, pos: Vec3, normal: Vec3, size: Vec2, spin: f32) {
        let up_hint = if normal.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
        // same basis as Unity's Quaternion.LookRotation(-normal) * Euler(0,0,spin) on a default quad
        let fwd = -normal;
        let right0 = up_hint.cross(fwd).normalize();
        let up0 = fwd.cross(right0);
        let r = Quat::from_axis_angle(fwd, spin.to_radians());
        let right = r * right0 * size.x * 0.5;
        let up = r * up0 * size.y * 0.5;
        let c = pos + normal * 0.006;
        let s = self.vertices.len() as u32;
        // Unity convention: v goes up (the shader flips to image space)
        self.push(c - right - up, normal, Vec2::new(0.0, 0.0));
        self.push(c + right - up, normal, Vec2::new(1.0, 0.0));
        self.push(c + right + up, normal, Vec2::new(1.0, 1.0));
        self.push(c - right + up, normal, Vec2::new(0.0, 1.0));
        self.indices.extend_from_slice(&[s, s + 2, s + 1, s, s + 3, s + 2]);
    }
}
