//! Project tools with JSON results: one implementation behind the `4bit` command line, the MCP server and the hub.
use fourbit::model::Model;
use fourbit::scene::{ent_from_json, ent_to_json, Scene};
use fourbit::Project;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

pub type R = Result<Value, String>;

fn walk(dir: &Path, base: &Path, ext: &[&str], out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, base, ext, out); }
        else if p.extension().is_some_and(|x| ext.iter().any(|e| x.eq_ignore_ascii_case(e))) {
            out.push(p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
        }
    }
}
fn list(dir: &Path, ext: &[&str], strip_ext: bool) -> Vec<String> {
    let mut v = vec![];
    walk(dir, dir, ext, &mut v);
    if strip_ext { for s in &mut v { if let Some(i) = s.rfind('.') { s.truncate(i); } } }
    v.sort();
    v
}

/// Every model, texture, sound and scene of the project.
pub fn assets(p: &Project) -> Value {
    let a = &p.assets;
    let tex: Vec<String> = list(&a.join("textures"), &["png"], true).into_iter().filter(|t| !t.ends_with("_normal")).collect();
    json!({"project": p.name, "root": p.root, "assets": a,
           "models": list(&a.join("models"), &["wmdl"], true), "textures": tex, "sounds": list(&a.join("audio"), &["wav"], true),
           "scenes": list(&p.root.join("scenes"), &["json"], false)})
}

/// A model's bones, clips, meshes and size.
pub fn model_info(p: &Project, name: &str) -> R {
    let path = p.assets.join("models").join(format!("{name}.wmdl"));
    let m = Model::load(&path)?;
    let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
    for sm in &m.meshes { for v in &sm.verts { lo = lo.min(v.pos); hi = hi.max(v.pos); } }
    Ok(json!({"name": name, "file": path,
        "size_m": [hi.x - lo.x, hi.y - lo.y, hi.z - lo.z], "min": [lo.x, lo.y, lo.z], "max": [hi.x, hi.y, hi.z],
        "bones": m.bones.iter().map(|b| json!({"name": b.name, "parent": b.parent})).collect::<Vec<_>>(),
        "clips": m.clips.iter().map(|c| json!({"name": c.name, "frames": c.frames.len(), "fps": c.fps, "seconds": c.length(), "looping": c.looping})).collect::<Vec<_>>(),
        "meshes": m.meshes.iter().map(|s| json!({"body": s.body, "material": s.material, "texture": s.texture, "verts": s.verts.len(), "triangles": s.indices.len() / 3})).collect::<Vec<_>>()}))
}

pub fn scene_path(p: &Project, scene: Option<&str>) -> PathBuf {
    match scene { Some(s) if !s.is_empty() => { let q = PathBuf::from(s); if q.is_absolute() { q } else if q.starts_with("scenes") { p.root.join(q) } else { p.root.join("scenes").join(q) } } _ => p.start_scene.clone() }
}
pub fn scene_get(p: &Project, scene: Option<&str>) -> R {
    let path = scene_path(p, scene);
    let s = Scene::load(&path);
    let mut v = s.to_json();
    v["file"] = json!(path);
    Ok(v)
}
/// Adds an entity given as scene JSON (`{"type": "model", "model": "...", "pos": [..]}`); a unique name is chosen.
pub fn scene_add(p: &Project, scene: Option<&str>, ent: &Value) -> R {
    let path = scene_path(p, scene);
    let mut s = Scene::load(&path);
    let mut e = ent_from_json(ent).ok_or("сущность: нужны type (model/light/box/spawn) и pos [x,y,z]; у model — model")?;
    if let fourbit::Kind::Model { model, .. } = &e.kind {
        if !p.assets.join("models").join(format!("{model}.wmdl")).is_file() { return Err(format!("нет модели {model} (см. assets)")); }
    }
    let base = e.name.clone();
    let mut k = 1;
    while s.ents.iter().any(|x| x.name == e.name) { k += 1; e.name = format!("{base} {k}"); }
    s.ents.push(e);
    s.save().map_err(|e| e.to_string())?;
    Ok(json!({"added": ent_to_json(s.ents.last().unwrap()), "file": path, "count": s.ents.len()}))
}
/// Changes fields of the entity named `name` (any of pos, yaw, scale, color, intensity, range, size, tint, …).
pub fn scene_set(p: &Project, scene: Option<&str>, name: &str, fields: &Value) -> R {
    let path = scene_path(p, scene);
    let mut s = Scene::load(&path);
    let i = s.ents.iter().position(|x| x.name == name).ok_or(format!("нет сущности {name}"))?;
    let mut v = ent_to_json(&s.ents[i]);
    if let (Some(o), Some(f)) = (v.as_object_mut(), fields.as_object()) { for (k, x) in f { o.insert(k.clone(), x.clone()); } }
    s.ents[i] = ent_from_json(&v).ok_or("поля не подходят")?;
    s.save().map_err(|e| e.to_string())?;
    Ok(json!({"changed": ent_to_json(&s.ents[i]), "file": path}))
}
pub fn scene_remove(p: &Project, scene: Option<&str>, name: &str) -> R {
    let path = scene_path(p, scene);
    let mut s = Scene::load(&path);
    let n = s.ents.len();
    s.ents.retain(|x| x.name != name);
    if s.ents.len() == n { return Err(format!("нет сущности {name}")); }
    s.save().map_err(|e| e.to_string())?;
    Ok(json!({"removed": name, "count": s.ents.len()}))
}
/// Sets the scene's environment (sky, key, fog… as in the scene file's "env").
pub fn scene_env(p: &Project, scene: Option<&str>, env: &Value) -> R {
    let path = scene_path(p, scene);
    let mut s = Scene::load(&path);
    let mut v = s.env.map(|e| fourbit::scene::env_to_json(&e)).unwrap_or(json!({}));
    if let (Some(o), Some(f)) = (v.as_object_mut(), env.as_object()) { for (k, x) in f { o.insert(k.clone(), x.clone()); } }
    s.env = Some(fourbit::scene::env_from_json(&v));
    s.save().map_err(|e| e.to_string())?;
    Ok(json!({"env": fourbit::scene::env_to_json(&s.env.unwrap()), "file": path}))
}

/// Imports a model file into assets/models/<folder>/<name>/<name>.wmdl.
pub fn import(p: &Project, file: &Path, name: Option<&str>, folder: Option<&str>) -> R {
    let name = name.map(String::from).unwrap_or_else(|| crate::workshop::clean_name(&file.file_stem().unwrap_or_default().to_string_lossy()));
    let folder = folder.unwrap_or("imported");
    let out = p.assets.join("models").join(folder).join(&name);
    let rep = fourbit::import::any(file, &out, &name, &[])?;
    let mut v = rep.to_json();
    v["model"] = json!(format!("{folder}/{name}/{name}"));
    Ok(v)
}

/// Broken references: scene models that don't exist, box textures that don't exist, model textures missing.
pub fn check(p: &Project) -> Value {
    let mut problems = vec![];
    let scenes = list(&p.root.join("scenes"), &["json"], false);
    for sc in &scenes {
        let s = Scene::load(&p.root.join("scenes").join(sc));
        for e in &s.ents {
            match &e.kind {
                fourbit::Kind::Model { model, .. } => if !p.assets.join("models").join(format!("{model}.wmdl")).is_file() { problems.push(json!({"scene": sc, "entity": e.name, "problem": format!("нет модели {model}")})); },
                fourbit::Kind::Box { texture, .. } => if !texture.is_empty() && !p.assets.join("textures").join(format!("{texture}.png")).is_file() && !p.assets.join(texture).is_file() {
                    problems.push(json!({"scene": sc, "entity": e.name, "problem": format!("нет текстуры {texture}")}));
                },
                _ => {}
            }
        }
        if !s.ents.iter().any(|e| e.kind == fourbit::Kind::Spawn) { problems.push(json!({"scene": sc, "problem": "нет точки старта (spawn): игрок появится в 0,0,0"})); }
    }
    for m in list(&p.assets.join("models"), &["wmdl"], true) {
        let path = p.assets.join("models").join(format!("{m}.wmdl"));
        match Model::load(&path) {
            Ok(model) => for s in &model.meshes { if !s.texture.is_empty() && !path.parent().unwrap().join(&s.texture).is_file() { problems.push(json!({"model": m, "problem": format!("нет текстуры {}", s.texture)})); } },
            Err(e) => problems.push(json!({"model": m, "problem": e})),
        }
    }
    if !p.start_scene.is_file() { problems.push(json!({"problem": format!("нет стартовой сцены {}", p.start_scene.display())})); }
    json!({"ok": problems.is_empty(), "scenes": scenes.len(), "problems": problems})
}

/// The game's executable after `cargo build --release` (its crate name from Cargo.toml).
pub fn game_exe(p: &Project) -> Option<PathBuf> {
    let toml = std::fs::read_to_string(p.root.join("Cargo.toml")).ok()?;
    let name = toml.lines().skip_while(|l| l.trim() != "[package]").find_map(|l| l.trim().strip_prefix("name").map(|r| r.trim_start_matches([' ', '=']).trim().trim_matches('"').to_string()))?;
    let dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or(p.root.join("target"));
    Some(dir.join("release").join(if cfg!(windows) { format!("{name}.exe") } else { name }))
}
/// `cargo build --release` in the project; the compiler's complaints come back as the error.
pub fn compile(p: &Project) -> Result<PathBuf, String> {
    let o = Command::new("cargo").args(["build", "--release", "--message-format", "short"]).current_dir(&p.root).output().map_err(|e| format!("cargo: {e} (нужен Rust: https://rustup.rs)"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let lines: Vec<&str> = err.lines().filter(|l| l.contains("error") || l.contains("-->")).take(40).collect();
        return Err(format!("сборка не прошла:\n{}", lines.join("\n")));
    }
    game_exe(p).filter(|e| e.exists()).ok_or("не нашёл собранную игру".into())
}
/// A picture of the game from a camera (`x,y,z,yaw,pitch`) or from the player start; with `editor` the editor's panels.
pub fn screenshot(p: &Project, camera: Option<&str>, scene: Option<&str>, out: Option<&Path>, editor: bool) -> R {
    let exe = compile(p)?;
    let out = out.map(Path::to_path_buf).unwrap_or_else(|| {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        p.root.join("screenshots").join(format!("shot_{stamp}.png"))
    });
    if let Some(d) = out.parent() { std::fs::create_dir_all(d).ok(); }
    let mut cmd = Command::new(&exe);
    cmd.current_dir(&p.root).arg("--project").arg(&p.root).arg("--shot").arg(&out);
    if let Some(c) = camera { cmd.args(["--camera", c]); }
    if let Some(s) = scene { cmd.arg("--scene").arg(scene_path(p, Some(s))); }
    if editor { cmd.arg("--editor"); }
    let o = cmd.output().map_err(|e| e.to_string())?;
    if !out.is_file() { return Err(format!("снимок не получился: {}", String::from_utf8_lossy(&o.stderr).lines().rev().take(6).collect::<Vec<_>>().join(" / "))); }
    Ok(json!({"image": out, "camera": camera, "log": String::from_utf8_lossy(&o.stderr).lines().filter(|l| l.contains("[4bit]") || l.contains("error")).collect::<Vec<_>>()}))
}

/// Release package for this computer: Builds/<name>-<platform>/ with the game and its assets (+ a .app on macOS), zipped.
pub fn build(p: &Project) -> R {
    let exe = compile(p)?;
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let safe = crate::projects::crate_name(&p.name);
    let builds = p.root.join("Builds");
    std::fs::create_dir_all(&builds).map_err(|e| e.to_string())?;
    let copy_assets = |to: &Path| -> Result<usize, String> {
        let mut n = 0;
        let mut files = vec![]; walk(&p.assets, &p.assets, &["png", "wmdl", "wav", "ttf", "tsv", "json", "txt", "md"], &mut files);
        for f in files { let d = to.join(&f); std::fs::create_dir_all(d.parent().unwrap()).map_err(|e| e.to_string())?; std::fs::copy(p.assets.join(&f), &d).map_err(|e| e.to_string())?; n += 1; }
        Ok(n)
    };
    let (folder, n) = if cfg!(target_os = "macos") {
        let app = builds.join(format!("{}.app", p.name));
        std::fs::remove_dir_all(&app).ok();
        let c = app.join("Contents");
        std::fs::create_dir_all(c.join("MacOS")).map_err(|e| e.to_string())?;
        std::fs::copy(&exe, c.join("MacOS").join(&safe)).map_err(|e| e.to_string())?;
        let n = copy_assets(&c.join("Resources/assets"))?;
        for f in ["project.json"] { std::fs::copy(p.root.join(f), c.join("MacOS").join(f)).ok(); }
        copy_scenes(p, &c.join("MacOS/scenes"))?;
        std::fs::write(c.join("Info.plist"), format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleExecutable</key><string>{safe}</string><key>CFBundleName</key><string>{}</string>
<key>CFBundleIdentifier</key><string>dev.4bit.{safe}</string><key>CFBundlePackageType</key><string>APPL</string><key>NSHighResolutionCapable</key><true/></dict></plist>
"#, p.name)).map_err(|e| e.to_string())?;
        Command::new("codesign").args(["--force", "--deep", "--sign", "-"]).arg(&app).output().ok();
        (app, n)
    } else {
        let dir = builds.join(format!("{safe}-{platform}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::copy(&exe, dir.join(exe.file_name().unwrap())).map_err(|e| e.to_string())?;
        std::fs::copy(p.root.join("project.json"), dir.join("project.json")).ok();
        copy_scenes(p, &dir.join("scenes"))?;
        (dir.clone(), copy_assets(&dir.join("assets"))?)
    };
    let zip = builds.join(format!("{safe}-{platform}.zip"));
    std::fs::remove_file(&zip).ok();
    let zipped = if cfg!(target_os = "macos") { Command::new("ditto").args(["-c", "-k", "--keepParent"]).arg(&folder).arg(&zip).status() }
                 else { Command::new("tar").args(["-a", "-c", "-f"]).arg(&zip).arg("-C").arg(&builds).arg(folder.file_name().unwrap()).status() };
    Ok(json!({"folder": folder, "zip": zipped.ok().filter(|s| s.success()).map(|_| zip), "assets": n, "platform": platform}))
}
fn copy_scenes(p: &Project, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for s in list(&p.root.join("scenes"), &["json"], false) { let d = to.join(&s); std::fs::create_dir_all(d.parent().unwrap()).ok(); std::fs::copy(p.root.join("scenes").join(&s), d).map_err(|e| e.to_string())?; }
    Ok(())
}
