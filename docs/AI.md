# 4bit and AI agents

Every new project comes with:
- `AGENTS.md` — the engine API, the scene format, the commands and how to verify a change (Codex, Cursor and most
  agents read it); `CLAUDE.md` points Claude Code to it.
- `.mcp.json` — registers the **4bit MCP server** for the project (Claude Code picks it up when opened in the folder).

## MCP server
`target/release/4bit-mcp --project <project dir>` speaks MCP over stdio. Tools:

| tool | what |
|---|---|
| `guide` | the project's AGENTS.md (read first) |
| `assets` | models (names usable in scenes), textures, sounds, scenes |
| `model_info` | size in metres, bones, clips, meshes |
| `scene_get` / `scene_add` / `scene_set` / `scene_remove` / `scene_env` | read and edit scenes (JSON entities) |
| `screenshot` | builds the game, renders from `camera = "x,y,z,yaw,pitch"`, **returns the image** (optionally with the editor's panels) |
| `import` | `.glb` / `.gltf` / `.mdl` into the project |
| `workshop` | Garry's Mod Workshop item → project models |
| `check` | broken references |
| `build` | release package |

Connecting by hand:
```bash
claude mcp add 4bit -- /path/to/4bit/target/release/4bit-mcp --project /path/to/game      # Claude Code
```
Codex (`~/.codex/config.toml`):
```toml
[mcp_servers.4bit]
command = "/path/to/4bit/target/release/4bit-mcp"
args = ["--project", "/path/to/game"]
```
Cursor: Settings → MCP → add the same command.

## The same without MCP
`4bit <command> --json` (see `4bit help`) — assets, model, scene (add / set / remove / env), import, workshop, check,
shot, run, build. Every game also answers `--shot out.png --camera x,y,z,yaw,pitch [--scene file] [--editor]`.

## A good loop for an agent
1. `guide`, `assets`, `scene_get` — know what exists.
2. Change the scene (`scene_add` / `scene_set`) or the code (`src/main.rs`).
3. `screenshot` from a camera that shows the change — look at it; fix; repeat.
4. `check`, `cargo build --release` without warnings, commit.
