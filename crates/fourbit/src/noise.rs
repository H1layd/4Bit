//! 2D gradient noise in 0..1, a stand-in for Unity's Mathf.PerlinNoise.
fn hash(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263);
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32 * std::f32::consts::TAU
}

fn grad(ix: i32, iy: i32, x: f32, y: f32) -> f32 {
    let a = hash(ix, iy);
    (x - ix as f32) * a.cos() + (y - iy as f32) * a.sin()
}

pub fn perlin(x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let fade = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (sx, sy) = (fade(x - x0 as f32), fade(y - y0 as f32));
    let n0 = grad(x0, y0, x, y) + (grad(x0 + 1, y0, x, y) - grad(x0, y0, x, y)) * sx;
    let n1 = grad(x0, y0 + 1, x, y) + (grad(x0 + 1, y0 + 1, x, y) - grad(x0, y0 + 1, x, y)) * sx;
    ((n0 + (n1 - n0) * sy) * 0.7071 + 0.5).clamp(0.0, 1.0)
}
