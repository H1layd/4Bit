//! {{NAME}} — made with 4bit. Start: `cargo run --release` (play) or `cargo run --release -- --editor` (scene editor).
//! The scene is `scenes/main.json`; code adds whatever the scene file can't express.
use fourbit::{glam::Vec3, Engine, Game, Key};

struct MyGame { crate_obj: Option<usize> }

impl Game for MyGame {
    fn init(&mut self, e: &mut Engine) {
        // something made in code next to the scene's things: a crate that turns
        if let Some(p) = e.world.place_model("props/wooden_crate_01", Vec3::new(2.0, 0.6, 6.0), 0.0, 1.0, false) { self.crate_obj = p.objs.first().copied(); }
        e.say("WASD — идти, мышь — смотреть, F — фонарь, E — привет", 5.0);
    }
    fn update(&mut self, e: &mut Engine, _dt: f32) {
        if e.input.pressed(Key::KeyE) { e.say("привет из кода игры", 2.0); }
        if let Some(o) = self.crate_obj {
            let t = e.time;
            e.world.objs[o].model = fourbit::glam::Mat4::from_rotation_translation(fourbit::glam::Quat::from_rotation_y(t * 0.8), Vec3::new(2.0, 0.6 + (t * 1.5).sin() * 0.1, 6.0));
        }
    }
}

fn main() { fourbit::run(MyGame { crate_obj: None }); }
