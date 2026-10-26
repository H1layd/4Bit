use std::collections::HashSet;
use winit::keyboard::KeyCode;

#[derive(Default)]
pub struct Input {
    held: HashSet<KeyCode>,
    pressed: HashSet<KeyCode>,
    pub mouse: glam::Vec2,
    pub lmb: bool,
    pub rmb: bool,
    pub lmb_pressed: bool,
    pub lmb_released: bool,
    /// wheel notches this frame (+ = away from the player)
    pub wheel: f32,
    pub locked: bool,
}

impl Input {
    pub fn key(&mut self, k: KeyCode, down: bool) {
        if down {
            if self.held.insert(k) { self.pressed.insert(k); }
        } else {
            self.held.remove(&k);
        }
    }
    pub fn held(&self, k: KeyCode) -> bool { self.held.contains(&k) }
    pub fn pressed(&self, k: KeyCode) -> bool { self.pressed.contains(&k) }
    pub fn ctrl(&self) -> bool {
        self.held(KeyCode::ControlLeft) || self.held(KeyCode::ControlRight) || self.held(KeyCode::SuperLeft)
    }
    /// Forget a press this frame (the phone screen took it).
    pub fn consume(&mut self, k: KeyCode) { self.pressed.remove(&k); }
    pub fn release(&mut self) {
        self.held.clear(); self.pressed.clear(); self.mouse = glam::Vec2::ZERO;
        self.lmb = false; self.rmb = false; self.lmb_pressed = false; self.lmb_released = false; self.locked = false; self.wheel = 0.0;
    }
    /// End of frame: forget one-shot events.
    pub fn flush(&mut self) {
        self.pressed.clear();
        self.mouse = glam::Vec2::ZERO;
        self.lmb_pressed = false;
        self.lmb_released = false;
        self.wheel = 0.0;
    }
}
