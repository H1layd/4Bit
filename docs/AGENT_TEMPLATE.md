# {{NAME}} — guide for coding agents (Codex, Claude, Cursor…)

A game on the **4bit** engine (Rust, wgpu). Engine checkout: `{{ENGINE}}` (read its `docs/` and `crates/fourbit/src/`
when you need details; don't edit it unless asked). This file is the single source of truth for working here.

## Layout
| path | what |
|---|---|
| `project.json` | name, start scene, window, `render.frame_lines` (lo-res target height), `render.look` (`clean` / `lofi`), `player` |
| `src/main.rs` | the game: a type implementing `fourbit::Game`, `fn main() { fourbit::run(..) }` |
| `scenes/*.json` | scenes: entities (`model`, `light`, `box`, `spawn`) + `env` (sky, key light, fog, indoor zones) |
| `assets/models/<path>.wmdl` | models (+ png textures beside them); a model's name is its path without `.wmdl`, e.g. `props/covered_car` |
| `assets/textures/<name>.png` | textures for boxes / materials (`Material::lit("name")`); `<name>_normal.png` = its normal map |
| `assets/audio/<name>.wav` | sounds: `e.audio.play2d("name", vol, pitch)`, `e.audio.play_at("name", pos, vol, pitch, max_dist)` |

## Conventions
- Metres, **left-handed, Y up, +Z forward, +X right** (Unity's axes). Yaw in degrees around +Y (0 = looking +Z).
- Text the player sees: short, lowercase unless the project says otherwise.
- Prefer the scene file for placing things; code for behaviour and anything procedural.

## Engine API (what you use most)
```rust
use fourbit::{glam::{Vec3, Mat4, Quat}, Engine, Game, Key, Material, Light, Kind, Ent};
impl Game for MyGame {
    fn init(&mut self, e: &mut Engine)            // once: build code-made parts, set e.player / e.camera
    fn update(&mut self, e: &mut Engine, dt: f32)  // every simulated frame
    fn draw(&mut self, e: &mut Engine)             // push e.draws / e.overlay_draws (first-person hands, weapons)
}
e.input.pressed(Key::KeyE) / held(..) / e.input.mouse / lmb_pressed     // input
e.player: Option<FirstPerson>  (pos = feet, yaw, pitch, speed, run, jump, fly)   // built-in walker; None = move e.camera yourself
e.camera (pos, yaw, pitch, fov)  e.flashlight: bool  e.say("text", secs)  e.log("line")  e.time
e.world.block(center, size, &Material::tinted("grid", r, g, b), tile)   // solid box (mesh + collider)
e.world.place_model("props/covered_car", pos, yaw_deg, scale, solid) -> Option<Placement>   // movable model
e.world.move_model(&placement, pos, yaw, scale) / remove_model(&placement)
e.world.lamp(pos, colour, intensity, range)       // ceiling lamp with a bulb; e.world.lights[i] to change it
e.world.raycast(origin, dir, max) -> Option<(dist, normal)>   // exact on models, boxes elsewhere
e.world.support_height(p, drop) -> Option<y>      // ground under a point
e.world.objs[i].model = Mat4                      // move any object (set objs[i].moving = true for it)
e.scene.ents / e.scene.place(&mut e.world, i) / e.scene.sync(..) / e.scene.save()   // the scene in code
e.gfx.env  (sky_ambient, key_dir, key_colour, fog_outdoor, fog_outdoor_range, zones, storey)
```

## Commands (from this folder)
```bash
cargo run --release                         # play
cargo run --release -- --editor             # scene editor (RMB+WASD fly, click select, gizmo, Ctrl+S save, Ctrl+Z undo)
cargo run --release -- --shot out.png --camera x,y,z,yaw,pitch [--scene scenes/x.json] [--frames 20]
{{ENGINE}}/target/release/4bit assets --json            # every model / texture / sound / scene
{{ENGINE}}/target/release/4bit model props/covered_car --json   # bones, clips, meshes, size
{{ENGINE}}/target/release/4bit scene --json             # the start scene's entities
{{ENGINE}}/target/release/4bit scene add model props/covered_car 0,0,5 --yaw 30
{{ENGINE}}/target/release/4bit scene add light 0,3,5 --color 1,0.85,0.6 --range 8
{{ENGINE}}/target/release/4bit scene remove <name>
{{ENGINE}}/target/release/4bit import <file.glb|.gltf|.mdl> [--name n] [--folder props] --json
{{ENGINE}}/target/release/4bit workshop <steam link|id> <model substring>   # Garry's Mod Workshop import
{{ENGINE}}/target/release/4bit check --json             # broken references in scenes, missing textures
{{ENGINE}}/target/release/4bit build                    # release package into Builds/
```
Every command takes `--project <dir>` (default: this folder) and prints JSON with `--json`.

## MCP
`.mcp.json` registers the `4bit` MCP server for this project: tools `assets`, `model_info`, `scene_get`, `scene_add`,
`scene_set`, `scene_remove`, `screenshot`, `import`, `check`, `build`, `run_log`. Prefer them over editing files by hand
when they cover the task; `screenshot` is how you *see* the result.

## How to verify a change
1. `cargo build --release` without errors (no new warnings).
2. Take screenshots of what you changed (`--shot` / the `screenshot` tool) from cameras that show it, **look at them**.
3. `4bit check` reports nothing.
4. Commit with a clear message.

## Assets
Prefer free / CC0 assets (Poly Haven, Kenney, ambientCG) and credit them in a `SOURCES.md` beside them. Workshop
imports are credited automatically in `assets/models/WORKSHOP.md`; they keep their authors' terms (usually: not for
sale).
