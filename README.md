# 4bit

A small Rust 3D engine (wgpu) with a scene editor, Source / Garry's Mod model import and a toolchain made to be
driven by people *and* AI agents.

- Lit, shadowed renderer: 32 lights per frame, 8 cached lamp shadows, torch, bounce light, light shafts, SSAO,
  automatic LODs, a low-resolution target with a "clean" or "lofi" post look.
- `.wmdl` skinned models with clips, imported from GMod / Source (`.mdl` + `.vtx` + `.vvd`, VTF textures).
- World: boxes and models with colliders (boxes for walking, exact triangle shapes for rays), lamps, a first-person
  controller.
- Scenes are JSON files; the in-game **scene editor** (`--editor`) has a hierarchy, inspector, gizmo, play / pause /
  step, undo, collider and lamp views.
- Every game answers `--shot out.png --camera x,y,z,yaw,pitch`: a picture of any place, for scripts, CI and AI.

## Quick start
```bash
cargo run --release -p fourbit-editor        # the 4bit hub: new project from a template, assets, models, builds
```
A game is a Rust crate with a `project.json`:
```rust
struct MyGame;
impl fourbit::Game for MyGame {
    fn update(&mut self, e: &mut fourbit::Engine, _dt: f32) {
        if e.input.pressed(fourbit::Key::KeyE) { e.say("hello", 2.0); }
    }
}
fn main() { fourbit::run(MyGame); }
```
See `docs/GUIDE.md` (people) and `docs/AI.md` (agents).

Not yet licensed: all rights reserved by the author until a licence is chosen.
