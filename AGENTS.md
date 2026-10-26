# 4bit engine — guide for coding agents working on the engine itself

(Agents working on a *game* read that game's AGENTS.md, generated from `docs/AGENT_TEMPLATE.md`.)

## Layout
| path | what |
|---|---|
| `crates/fourbit/src/render/` | wgpu renderer: low-res HDR target → fx (SSAO, haze, shafts) → bloom → post; lamp shadow caches; `Env` (zones, storeys, sky, fog); LODs; egui `UiLayer` |
| `crates/fourbit/src/shaders/` | `scene.wgsl` (opaque `fs_opaque` has no discard — keep it so), `fx.wgsl`, `post.wgsl`, `shadow.wgsl`, `bloom.wgsl` |
| `crates/fourbit/src/app.rs` | `Game` trait, `Engine`, `run`, `Project` (project.json), command line (`--editor --shot --camera --scene --project`) |
| `crates/fourbit/src/world.rs` | objects, colliders (grid), exact model shapes for rays, lamps, model library |
| `crates/fourbit/src/scene.rs`, `editor.rs` | JSON scenes; the in-game scene editor |
| `crates/fourbit/src/import.rs` | Source `.mdl` and glTF → `.wmdl` |
| `crates/fourbit/src/{model,collide,controller,audio,input,text,mesh,ik,noise,look}.rs` | the rest |
| `crates/fourbit-editor/src/` | `4bit` hub window (`main.rs`, `preview.rs`), `cli.rs`, `tools.rs` (shared JSON tools), `mcp.rs` (`4bit-mcp`), `projects.rs`, `workshop.rs` |
| `templates/<name>/` | project templates (`{{NAME}}`, `{{CRATE}}`, `{{ENGINE}}` are filled in); `TEMPLATE.txt` = description |
| `docs/` | `GUIDE.md` (people, Russian), `AI.md`, `AGENT_TEMPLATE.md` (the game projects' AGENTS.md) |

## Conventions
Left-handed, Y up, +Z forward, metres (Unity's axes). User-facing text is Russian, short. Dense code, short comments
that say *why*. No new warnings.

## Verify
`cargo build --release && cargo test --release`; then a template project end to end:
`target/release/4bit new T --dir /tmp/p && cd "/tmp/p/T" && CARGO_TARGET_DIR=<this>/target <this>/target/release/4bit shot /tmp/t.png`
— look at the picture. For editor changes: `--editor --shot`.
