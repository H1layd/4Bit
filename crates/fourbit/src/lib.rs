//! **4bit** — a small Rust 3D engine: wgpu renderer (lit, shadowed, low-res "lo-fi" target with a post pass), skinned
//! models (`.wmdl`) imported from Source / Garry's Mod, exact ray collisions, audio, input, a scene file and an
//! in-game scene editor. Games implement [`Game`] and call [`run`].
pub mod app;
pub mod audio;
pub mod controller;
pub mod editor;
pub mod look;
pub mod scene;
pub mod world;
pub mod collide;
pub mod ik;
pub mod import;
pub mod input;
pub mod mesh;
pub mod model;
pub mod noise;
pub mod render;
pub mod text;
pub mod source {
    pub mod mdl;
    pub mod vtf;
}

pub use app::{run, Camera, Engine, Game, Project};
pub use controller::FirstPerson;
pub use render::{DrawCmd, Env};
pub use scene::{Ent, Kind, Scene};
pub use world::{Light, Material, World};
/// Keyboard keys for `e.input.pressed(Key::KeyF)`.
pub use winit::keyboard::KeyCode as Key;
pub use {egui, glam};
