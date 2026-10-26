//! Exact shapes for ray queries: a triangle BVH per model (built once, in model space), instanced by a transform.
//! Boxes stay the broad phase and the walkable volume; rays (bullets, interaction, camera) refine to the real surface,
//! so a shot past the rim of a bin or under a car's bumper goes on instead of stopping in the air of its box.
use glam::{Mat4, Vec3};

#[derive(Clone, Copy)]
struct Node { lo: Vec3, hi: Vec3, /// leaf: first triangle; inner: right child (left is the next node)
    at: u32, /// triangles in a leaf, 0 for an inner node
    n: u32 }

pub struct Shape { nodes: Vec<Node>, tris: Vec<[Vec3; 3]> }

impl Shape {
    pub fn new(mut tris: Vec<[Vec3; 3]>) -> Shape {
        let mut nodes = Vec::with_capacity(tris.len() / 2 + 1);
        if !tris.is_empty() { let n = tris.len(); build(&mut nodes, &mut tris, 0, n); }
        Shape { nodes, tris }
    }
    pub fn triangles(&self) -> usize { self.tris.len() }

    /// Nearest hit with t in (min, max]: distance along `d` (not normalised: t is in units of `d`) and the face normal
    /// turned against the ray.
    pub fn ray(&self, o: Vec3, d: Vec3, min: f32, max: f32) -> Option<(f32, Vec3)> {
        if self.nodes.is_empty() { return None; }
        let inv = Vec3::ONE / d;
        let mut best: Option<(f32, Vec3)> = None;
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let idx = stack[sp];
            let node = self.nodes[idx as usize];
            let far = best.map_or(max, |b| b.0);
            if !slab(o, inv, node.lo, node.hi, min, far) { continue; }
            if node.n > 0 {
                for t in &self.tris[node.at as usize..(node.at + node.n) as usize] {
                    if let Some(h) = tri(o, d, t) { if h > min && h <= best.map_or(max, |b| b.0) {
                        let mut nrm = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
                        if nrm.dot(d) > 0.0 { nrm = -nrm; }
                        best = Some((h, nrm));
                    } }
                }
            } else if sp + 2 <= stack.len() {
                // the left child always follows its parent; `at` is the right one
                stack[sp] = node.at; stack[sp + 1] = idx + 1;
                sp += 2;
            }
        }
        best
    }
}

fn build(nodes: &mut Vec<Node>, tris: &mut [[Vec3; 3]], start: usize, end: usize) -> u32 {
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let (mut clo, mut chi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for t in &tris[start..end] {
        for v in t { lo = lo.min(*v); hi = hi.max(*v); }
        let c = (t[0] + t[1] + t[2]) / 3.0;
        clo = clo.min(c); chi = chi.max(c);
    }
    let me = nodes.len() as u32;
    nodes.push(Node { lo, hi, at: start as u32, n: (end - start) as u32 });
    if end - start <= 4 { return me; }
    let ext = chi - clo;
    let axis = if ext.x >= ext.y && ext.x >= ext.z { 0 } else if ext.y >= ext.z { 1 } else { 2 };
    let mid = (start + end) / 2;
    tris[start..end].select_nth_unstable_by(mid - start, |a, b| {
        let ca = a[0][axis] + a[1][axis] + a[2][axis];
        let cb = b[0][axis] + b[1][axis] + b[2][axis];
        ca.total_cmp(&cb)
    });
    let left = build(nodes, tris, start, mid);
    debug_assert_eq!(left, me + 1);
    let right = build(nodes, tris, mid, end);
    nodes[me as usize].n = 0;
    nodes[me as usize].at = right;
    me
}

fn slab(o: Vec3, inv: Vec3, lo: Vec3, hi: Vec3, min: f32, max: f32) -> bool {
    let a = (lo - o) * inv;
    let b = (hi - o) * inv;
    let t0 = a.min(b).max_element().max(min);
    let t1 = a.max(b).min_element().min(max);
    t0 <= t1
}

/// Möller–Trumbore, both faces.
fn tri(o: Vec3, d: Vec3, t: &[Vec3; 3]) -> Option<f32> {
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 { return None; }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) { return None; }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 { return None; }
    Some(e2.dot(q) * inv)
}

/// A shape placed in the world.
pub struct Placed { pub shape: std::sync::Arc<Shape>, pub to_world: Mat4, pub to_local: Mat4 }
impl Placed {
    pub fn new(shape: std::sync::Arc<Shape>, to_world: Mat4) -> Placed { Placed { shape, to_world, to_local: to_world.inverse() } }
    /// World ray (`d` unit length): first hit after `min`, and the next one after it (where a bullet leaves the solid).
    pub fn ray(&self, o: Vec3, d: Vec3, min: f32, max: f32) -> Option<(f32, Vec3, f32)> {
        let (lo, ld) = (self.to_local.transform_point3(o), self.to_local.transform_vector3(d));
        let (t, n) = self.shape.ray(lo, ld, min, max)?;
        let exit = self.shape.ray(lo, ld, t + 1e-4, max).map_or(t, |h| h.0);
        Some((t, self.to_world.transform_vector3(n).normalize_or_zero(), exit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cube() -> Shape {
        let v = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
        let q = |a: Vec3, b: Vec3, c: Vec3, d: Vec3| [[a, b, c], [a, c, d]];
        let (l, h) = (-0.5, 0.5);
        let mut t = vec![];
        for f in [q(v(l,l,l),v(h,l,l),v(h,h,l),v(l,h,l)), q(v(l,l,h),v(h,l,h),v(h,h,h),v(l,h,h)), q(v(l,l,l),v(l,h,l),v(l,h,h),v(l,l,h)),
                  q(v(h,l,l),v(h,h,l),v(h,h,h),v(h,l,h)), q(v(l,l,l),v(h,l,l),v(h,l,h),v(l,l,h)), q(v(l,h,l),v(h,h,l),v(h,h,h),v(l,h,h))] { t.extend(f); }
        // many small triangles so the tree has inner nodes
        for i in 0..200 { let x = 3.0 + i as f32 * 0.01; t.push([v(x, 0.0, 0.0), v(x + 0.005, 0.0, 0.0), v(x, 0.005, 0.0)]); }
        Shape::new(t)
    }
    #[test] fn rays_hit_the_real_faces_and_find_the_way_out() {
        let s = std::sync::Arc::new(cube());
        let p = Placed::new(s, Mat4::from_scale_rotation_translation(Vec3::splat(2.0), glam::Quat::from_rotation_y(0.3), Vec3::new(0.0, 0.0, 10.0)));
        let (t, n, exit) = p.ray(Vec3::new(0.0, 0.0, 0.0), Vec3::Z, 0.0, 100.0).unwrap();
        assert!(t > 8.8 && t < 9.1, "{t}");
        assert!(n.z < -0.9, "{n}");
        assert!(exit > 10.8 && exit < 11.2, "{exit}");
        // beside it: nothing
        assert!(p.ray(Vec3::new(2.0, 0.0, 0.0), Vec3::Z, 0.0, 100.0).is_none());
    }
}
