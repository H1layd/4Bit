//! A first-person character: walk / run / crouch / jump, mouse look, collision against the world's boxes with step-up
//! and gravity. Good enough to walk any scene; games with their own movement just leave it off.
use crate::input::Input;
use crate::world::{Aabb, World};
use glam::{Quat, Vec2, Vec3};
use winit::keyboard::KeyCode;

pub struct FirstPerson {
    /// feet
    pub pos: Vec3,
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub height: f32,
    pub eye: f32,
    pub radius: f32,
    pub speed: f32,
    pub run: f32,
    pub jump: f32,
    pub step: f32,
    pub sensitivity: f32,
    pub grounded: bool,
    pub crouched: bool,
    pub fly: bool,
    /// head bob phase and amount (0 standing .. 1 running)
    pub bob: f32,
    pub moving: f32,
}

impl FirstPerson {
    pub fn new(pos: Vec3, yaw: f32) -> FirstPerson {
        FirstPerson { pos, velocity: Vec3::ZERO, yaw, pitch: 0.0, height: 1.8, eye: 1.65, radius: 0.3, speed: 3.2, run: 6.0, jump: 4.6, step: 0.36,
                      sensitivity: 0.09, grounded: false, crouched: false, fly: false, bob: 0.0, moving: 0.0 }
    }
    pub fn rotation(&self) -> Quat { Quat::from_euler(glam::EulerRot::YXZ, self.yaw.to_radians(), self.pitch.to_radians(), 0.0) }
    pub fn forward(&self) -> Vec3 { self.rotation() * Vec3::Z }
    /// Camera position: eye height, a small bob while walking.
    pub fn eye_pos(&self) -> Vec3 {
        let h = if self.crouched { self.eye * 0.6 } else { self.eye };
        self.pos + Vec3::Y * (h + (self.bob * 2.0).sin().abs() * 0.035 * self.moving)
    }

    pub fn update(&mut self, input: &Input, world: &World, dt: f32) {
        let dt = dt.min(0.05);
        if input.locked {
            let m = input.mouse * self.sensitivity;
            self.yaw += m.x;
            self.pitch = (self.pitch + m.y).clamp(-88.0, 88.0);
        }
        let flat = Quat::from_rotation_y(self.yaw.to_radians());
        let mut wish = Vec2::ZERO;
        for (k, d) in [(KeyCode::KeyW, Vec2::Y), (KeyCode::KeyS, Vec2::NEG_Y), (KeyCode::KeyD, Vec2::X), (KeyCode::KeyA, Vec2::NEG_X)] { if input.held(k) { wish += d; } }
        let wish = wish.normalize_or_zero();
        self.crouched = input.held(KeyCode::ControlLeft) || input.held(KeyCode::KeyC);
        let speed = if self.crouched { self.speed * 0.5 } else if input.held(KeyCode::ShiftLeft) { self.run } else { self.speed };
        if self.fly {
            let r = self.rotation();
            let mut v = r * Vec3::new(wish.x, 0.0, wish.y);
            if input.held(KeyCode::Space) { v.y += 1.0; }
            self.pos += v * speed * 2.0 * dt;
            self.velocity = Vec3::ZERO;
            return;
        }
        let target = flat * Vec3::new(wish.x, 0.0, wish.y) * speed;
        // quick on the ground, little control in the air
        let k = 1.0 - (-(if self.grounded { 14.0 } else { 2.0 }) * dt).exp();
        self.velocity.x += (target.x - self.velocity.x) * k;
        self.velocity.z += (target.z - self.velocity.z) * k;
        if self.grounded && input.pressed(KeyCode::Space) { self.velocity.y = self.jump; self.grounded = false; }
        self.velocity.y -= 9.81 * dt;
        let d = self.velocity * dt;
        self.slide(world, Vec3::new(d.x, 0.0, 0.0));
        self.slide(world, Vec3::new(0.0, 0.0, d.z));
        // vertical last: land on the highest top under the feet
        let before = self.pos.y;
        self.pos.y += d.y;
        if let Some(h) = self.blocked(world, self.pos) {
            if d.y <= 0.0 { self.pos.y = h; self.velocity.y = 0.0; self.grounded = true; } else { self.pos.y = before; self.velocity.y = 0.0; }
        } else {
            // snap down onto a step just below (walking down stairs without hopping)
            let snap = world.support_height(self.pos + Vec3::Y * 0.01, self.step);
            match snap { Some(h) if self.grounded && self.velocity.y <= 0.0 => { self.pos.y = h; self.velocity.y = 0.0; } _ => self.grounded = false }
        }
        let flat_speed = Vec2::new(self.velocity.x, self.velocity.z).length();
        self.moving += ((flat_speed / self.speed).min(1.5) * if self.grounded { 1.0 } else { 0.0 } - self.moving) * (1.0 - (-8.0 * dt).exp());
        self.bob += flat_speed * dt * 1.9;
        if self.pos.y < -200.0 { self.pos.y = 5.0; self.velocity = Vec3::ZERO; }
    }

    fn body(&self, p: Vec3) -> Aabb {
        let h = if self.crouched { self.height * 0.6 } else { self.height };
        Aabb { min: p + Vec3::new(-self.radius, 0.0, -self.radius), max: p + Vec3::new(self.radius, h, self.radius) }
    }
    /// The top of the highest box the body at `p` is in (None = free).
    fn blocked(&self, world: &World, p: Vec3) -> Option<f32> {
        let b = self.body(p);
        world.near(b.min, b.max).into_iter().map(|i| world.colliders[i]).filter(|c| c.overlaps(&b) && c.max.y - b.min.y > 1e-4).map(|c| c.max.y).max_by(f32::total_cmp)
    }
    /// Horizontal move; a ledge up to `step` high is climbed.
    fn slide(&mut self, world: &World, d: Vec3) {
        let to = self.pos + d;
        match self.blocked(world, to) {
            None => self.pos = to,
            Some(top) if self.grounded && top - self.pos.y <= self.step && self.blocked(world, Vec3::new(to.x, top + 0.001, to.z)).is_none() => { self.pos = Vec3::new(to.x, top + 0.001, to.z); }
            Some(_) => { if d.x != 0.0 { self.velocity.x = 0.0; } if d.z != 0.0 { self.velocity.z = 0.0; } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Material;
    #[test] fn walks_falls_and_climbs_a_step() {
        let mut w = World::new(std::path::Path::new("."));
        w.block(Vec3::new(0.0, -0.1, 0.0), Vec3::new(20.0, 0.2, 20.0), &Material::lit(""), 1.0);
        w.block(Vec3::new(0.0, 0.15, 3.0), Vec3::new(4.0, 0.3, 1.0), &Material::lit(""), 1.0);   // a 30 cm step
        w.block(Vec3::new(0.0, 1.0, 6.0), Vec3::new(4.0, 2.0, 0.4), &Material::lit(""), 1.0);    // a wall
        w.reindex();
        let mut p = FirstPerson::new(Vec3::new(0.0, 1.0, 0.0), 0.0);
        let mut input = Input::default();
        for _ in 0..60 { p.update(&input, &w, 1.0 / 60.0); }
        assert!(p.grounded && p.pos.y.abs() < 0.01, "landed: {:?}", p.pos);
        input.key(KeyCode::KeyW, true);
        for _ in 0..240 { p.update(&input, &w, 1.0 / 60.0); }
        assert!(p.pos.z > 4.5 && p.pos.z < 5.7, "stopped by the wall: {:?}", p.pos);
        assert!(p.pos.y.abs() < 0.01, "back on the floor after the step: {:?}", p.pos);
    }
}
