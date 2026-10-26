//! `4bit-mcp --project <dir>`: a Model Context Protocol server (JSON-RPC over stdio) for AI agents — Claude Code,
//! Codex, Cursor… It exposes the project's assets and scenes, edits scenes, imports models and takes screenshots that
//! come back as images, so an agent can *see* what it changed.
use fourbit_tools::tools;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 { s.push(if k <= c.len() { T[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' }); }
    }
    s
}

fn tool(name: &str, about: &str, props: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": about, "inputSchema": {"type": "object", "properties": props, "required": required}})
}
fn tools_list() -> Value {
    let scene = json!({"type": "string", "description": "scene file (default: the project's start scene), e.g. main.json"});
    json!([
        tool("guide", "How this engine and project work: API, scene format, conventions. Read first.", json!({}), &[]),
        tool("assets", "List the project's models (names usable in scenes), textures, sounds and scenes.", json!({}), &[]),
        tool("model_info", "A model's size in metres, bones, animation clips and meshes.", json!({"name": {"type": "string", "description": "model name, e.g. props/covered_car"}}), &["name"]),
        tool("scene_get", "A scene's entities and environment as JSON.", json!({"scene": scene}), &[]),
        tool("scene_add", "Add an entity. entity = {\"type\":\"model\",\"model\":\"props/x\",\"pos\":[x,y,z],\"yaw\":deg,\"scale\":1,\"solid\":true} | {\"type\":\"light\",\"pos\":[..],\"color\":[r,g,b],\"intensity\":1.5,\"range\":6,\"flicker\":0,\"shadows\":true} | {\"type\":\"box\",\"pos\":[..],\"size\":[x,y,z],\"texture\":\"grid\",\"tint\":[r,g,b],\"tile\":1} | {\"type\":\"spawn\",\"pos\":[..],\"yaw\":0}; optional \"name\". Metres, Y up, +Z forward, left-handed.",
             json!({"entity": {"type": "object"}, "scene": scene}), &["entity"]),
        tool("scene_set", "Change fields of the entity with this name (pos, yaw, scale, color, intensity, range, size, tint, texture…).", json!({"name": {"type": "string"}, "fields": {"type": "object"}, "scene": scene}), &["name", "fields"]),
        tool("scene_remove", "Remove the entity with this name.", json!({"name": {"type": "string"}, "scene": scene}), &["name"]),
        tool("scene_env", "Set the scene's environment: sky [r,g,b], key_dir [x,y,z], key [r,g,b], indoor [r,g,b], fog [r,g,b], fog_range [start,end], storey (m, 0 = none), zones [[min],[max]]…", json!({"env": {"type": "object"}, "scene": scene}), &["env"]),
        tool("screenshot", "Render the game and return the picture. camera = \"x,y,z,yaw,pitch\" (default: the player start). editor = true shows the editor panels. Compiles the game first (can take a minute the first time).",
             json!({"camera": {"type": "string"}, "scene": scene, "editor": {"type": "boolean"}}), &[]),
        tool("import", "Import a model file (.glb, .gltf, .mdl) into assets/models/<folder>/<name>/; returns its model name.", json!({"file": {"type": "string"}, "name": {"type": "string"}, "folder": {"type": "string"}}), &["file"]),
        tool("workshop", "Import models from a Garry's Mod Steam Workshop item (link or id); 'model' = substring of the .mdl names to import (empty: list them).", json!({"link": {"type": "string"}, "model": {"type": "string"}}), &["link"]),
        tool("check", "Broken references in scenes (missing models / textures, no spawn).", json!({}), &[]),
        tool("build", "Release package of the game into Builds/ (for this computer).", json!({}), &[]),
    ])
}

fn call(p: &fourbit::Project, name: &str, a: &Value) -> Result<Vec<Value>, String> {
    let s = |k: &str| a[k].as_str().filter(|x| !x.is_empty());
    let text = |v: Value| vec![json!({"type": "text", "text": serde_json::to_string_pretty(&v).unwrap()})];
    match name {
        "guide" => {
            let g = std::fs::read_to_string(p.root.join("AGENTS.md")).unwrap_or_else(|_| include_str!("../../../docs/AGENT_TEMPLATE.md").to_string());
            Ok(vec![json!({"type": "text", "text": g})])
        }
        "assets" => Ok(text(tools::assets(p))),
        "model_info" => tools::model_info(p, s("name").ok_or("name")?).map(text),
        "scene_get" => tools::scene_get(p, s("scene")).map(text),
        "scene_add" => tools::scene_add(p, s("scene"), &a["entity"]).map(text),
        "scene_set" => tools::scene_set(p, s("scene"), s("name").ok_or("name")?, &a["fields"]).map(text),
        "scene_remove" => tools::scene_remove(p, s("scene"), s("name").ok_or("name")?).map(text),
        "scene_env" => tools::scene_env(p, s("scene"), &a["env"]).map(text),
        "screenshot" => {
            let v = tools::screenshot(p, s("camera"), s("scene"), None, a["editor"].as_bool().unwrap_or(false))?;
            let path = PathBuf::from(v["image"].as_str().unwrap_or_default());
            // the picture itself, at most 1280 wide (agents read images, keep them light)
            let img = image::open(&path).map_err(|e| e.to_string())?;
            let img = if img.width() > 1280 { img.resize(1280, 1280, image::imageops::FilterType::Triangle) } else { img };
            let mut png = vec![];
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;
            let mut out = text(v);
            out.push(json!({"type": "image", "data": b64(&png), "mimeType": "image/png"}));
            Ok(out)
        }
        "import" => tools::import(p, &PathBuf::from(s("file").ok_or("file")?), s("name"), s("folder")).map(text),
        "workshop" => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name(if cfg!(windows) { "4bit.exe" } else { "4bit" });
            let mut c = std::process::Command::new(exe);
            c.arg("workshop").arg(s("link").ok_or("link")?);
            if let Some(m) = s("model") { c.arg(m); }
            let o = c.args(["--json", "--project"]).arg(&p.root).output().map_err(|e| e.to_string())?;
            let v: Value = serde_json::from_slice(&o.stdout).unwrap_or(json!({"output": String::from_utf8_lossy(&o.stdout), "log": String::from_utf8_lossy(&o.stderr)}));
            if let Some(e) = v["error"].as_str() { return Err(e.into()); }
            Ok(text(v))
        }
        "check" => Ok(text(tools::check(p))),
        "build" => tools::build(p).map(text),
        _ => Err(format!("нет инструмента {name}")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = args.iter().position(|a| a == "--project").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let project = fourbit::Project::find(dir);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let id = req.get("id").cloned();
        let method = req["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => Some(Ok(json!({"protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": {"tools": {}}, "serverInfo": {"name": "4bit", "version": env!("CARGO_PKG_VERSION")},
                "instructions": format!("4bit engine project \"{}\" at {}. Call `guide` first; `screenshot` shows the game.", project.name, project.root.display())}))),
            "tools/list" => Some(Ok(json!({"tools": tools_list()}))),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let r = call(&project, name, &req["params"]["arguments"]);
                // tool failures are results the agent can read, not protocol errors
                Some(Ok(match r { Ok(content) => json!({"content": content}), Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}) }))
            }
            "ping" => Some(Ok(json!({}))),
            m if m.starts_with("notifications/") => None,
            _ => Some(Err(json!({"code": -32601, "message": format!("unknown method {method}")}))),
        };
        let (Some(result), Some(id)) = (result, id) else { continue };
        let msg = match result { Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}), Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}) };
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }
}
