//! {{NAME}} — made with 4bit. `cargo run --release` plays, `cargo run --release -- --editor` opens the scene editor.
use fourbit::{Engine, Game};

struct MyGame;

impl Game for MyGame {
    fn init(&mut self, _e: &mut Engine) {}
    fn update(&mut self, _e: &mut Engine, _dt: f32) {}
}

fn main() { fourbit::run(MyGame); }
