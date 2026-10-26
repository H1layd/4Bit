//! `4bit <command>`: the hub's tools from a terminal or a script. `--json` prints machine-readable results;
//! `--project <dir>` picks the project (default: found from the working directory).
use crate::tools;
use serde_json::{json, Value};
use std::path::PathBuf;

pub const HELP: &str = "4bit — движок и его инструменты

  4bit                                   окно 4bit (проекты, ассеты, модели, анимации, мастерская, сборка)
  4bit new <имя> [--template first_person] [--dir <папка>]   новый проект
  4bit assets                            модели, текстуры, звуки, сцены проекта
  4bit model <имя модели>                кости, клипы, меши, размер
  4bit scene [--scene <файл>]            сущности сцены
  4bit scene add model <модель> x,y,z [--yaw 0] [--scale 1] [--name ..] [--nosolid]
  4bit scene add light x,y,z [--color 1,0.85,0.6] [--intensity 1.5] [--range 6] [--name ..]
  4bit scene add box x,y,z --size 1,1,1 [--texture grid] [--tint 0.6,0.6,0.6] [--name ..]
  4bit scene add spawn x,y,z [--yaw 0]
  4bit scene set <имя> '{\"pos\":[0,0,3]}'       поменять поля сущности
  4bit scene remove <имя>
  4bit scene env '{\"sky\":[0.2,0.22,0.26],\"fog_range\":[20,120]}'
  4bit import <файл.glb|.gltf|.mdl> [--name n] [--folder props]
  4bit workshop <ссылка|id> [подстрока модели]   Garry's Mod Workshop → модели проекта
  4bit check                             битые ссылки в сценах, нет текстур
  4bit shot [out.png] [--camera x,y,z,yaw,pitch] [--scene ..] [--editor]   снимок игры
  4bit run [--editor]                    собрать и запустить игру
  4bit build                             релизная сборка в Builds/

  общие: --project <папка> --json";

fn opt(args: &[String], k: &str) -> Option<String> { args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned() }
fn has(args: &[String], k: &str) -> bool { args.iter().any(|a| a == k) }
fn v3(s: &str) -> Option<Value> { let v: Vec<f64> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect(); (v.len() == 3).then(|| json!(v)) }
/// Positional arguments (flags and their values removed).
fn positional(args: &[String]) -> Vec<String> {
    const WITH_VALUE: [&str; 14] = ["--project", "--template", "--dir", "--scene", "--yaw", "--scale", "--name", "--color", "--intensity", "--range", "--size", "--texture", "--tint", "--camera"];
    let mut out = vec![]; let mut skip = false;
    for (i, a) in args.iter().enumerate() {
        if skip { skip = false; continue; }
        if WITH_VALUE.contains(&a.as_str()) || a == "--folder" { skip = true; continue; }
        if a.starts_with("--") { continue; }
        if i > 0 { out.push(a.clone()); }
    }
    out
}

/// Runs a command; `None` when `args` is not a command (the hub window opens instead).
pub fn run(args: &[String]) -> Option<i32> {
    let cmd = args.get(1)?.as_str();
    if !["new", "assets", "model", "scene", "import", "workshop", "check", "shot", "run", "build", "help", "--help", "-h"].contains(&cmd) { return None; }
    if matches!(cmd, "help" | "--help" | "-h") { println!("{HELP}"); return Some(0); }
    let json_out = has(args, "--json");
    let pos = positional(args);
    let project = || fourbit::Project::find(opt(args, "--project").map(PathBuf::from));
    let scene = opt(args, "--scene");
    let r: tools::R = match cmd {
        "new" => (|| {
            let name = pos.get(1).ok_or("4bit new <имя>")?;
            let engine = crate::projects::engine_root().ok_or("не нашёл папку движка 4bit (FOURBIT_HOME)")?;
            let parent = opt(args, "--dir").map(PathBuf::from).unwrap_or_else(crate::projects::default_parent);
            let dir = crate::projects::create(&engine, &opt(args, "--template").unwrap_or("first_person".into()), &parent, name)?;
            Ok(json!({"project": dir, "next": format!("cd \"{}\" && cargo run --release", dir.display())}))
        })(),
        "assets" => Ok(tools::assets(&project())),
        "model" => pos.get(1).ok_or("4bit model <имя>".to_string()).and_then(|m| tools::model_info(&project(), m)),
        "scene" => match pos.get(1).map(String::as_str) {
            None => tools::scene_get(&project(), scene.as_deref()),
            Some("add") => (|| {
                let kind = pos.get(2).ok_or("model / light / box / spawn")?.as_str();
                let mut e = json!({"type": kind});
                let at = |i: usize| pos.get(i).and_then(|s| v3(s)).ok_or("позиция x,y,z");
                match kind {
                    "model" => { e["model"] = json!(pos.get(3).ok_or("имя модели")?); e["pos"] = at(4)?; e["solid"] = json!(!has(args, "--nosolid")); }
                    _ => e["pos"] = at(3)?,
                }
                for (k, f) in [("--yaw", "yaw"), ("--scale", "scale"), ("--intensity", "intensity"), ("--range", "range")] { if let Some(x) = opt(args, k).and_then(|x| x.parse::<f64>().ok()) { e[f] = json!(x); } }
                for (k, f) in [("--color", "color"), ("--size", "size"), ("--tint", "tint")] { if let Some(x) = opt(args, k).and_then(|x| v3(&x)) { e[f] = x; } }
                if let Some(t) = opt(args, "--texture") { e["texture"] = json!(t); }
                e["name"] = json!(opt(args, "--name").unwrap_or_else(|| match kind { "model" => pos[3].rsplit('/').next().unwrap_or("model").to_string(), k => k.to_string() }));
                tools::scene_add(&project(), scene.as_deref(), &e)
            })(),
            Some("set") => (|| { let n = pos.get(2).ok_or("имя")?; let f: Value = serde_json::from_str(pos.get(3).ok_or("JSON полей")?).map_err(|e| e.to_string())?; tools::scene_set(&project(), scene.as_deref(), n, &f) })(),
            Some("remove") => pos.get(2).ok_or("имя".to_string()).and_then(|n| tools::scene_remove(&project(), scene.as_deref(), n)),
            Some("env") => (|| { let f: Value = serde_json::from_str(pos.get(2).ok_or("JSON окружения")?).map_err(|e| e.to_string())?; tools::scene_env(&project(), scene.as_deref(), &f) })(),
            Some(x) => Err(format!("scene {x}? (add / set / remove / env)")),
        },
        "import" => pos.get(1).ok_or("4bit import <файл>".to_string()).and_then(|f| tools::import(&project(), &PathBuf::from(f), opt(args, "--name").as_deref(), opt(args, "--folder").as_deref())),
        "workshop" => (|| {
            let id = pos.get(1).and_then(|a| crate::workshop::parse_id(a)).ok_or("4bit workshop <ссылка|id> [подстрока]")?;
            let only = pos.get(2).cloned().unwrap_or_default().to_lowercase();
            let p = project();
            let item = crate::workshop::details(id)?;
            eprintln!("{} ({:.1} МБ)", item.title, item.size as f64 / 1e6);
            let (tx, rx) = std::sync::mpsc::channel();
            let wait = |rx: &std::sync::mpsc::Receiver<crate::workshop::Msg>| loop { match rx.recv() { Ok(crate::workshop::Msg::Log(l)) => eprintln!("{l}"), Ok(m) => return Some(m), Err(_) => return None } };
            crate::workshop::download(p.root.clone(), item.clone(), tx.clone());
            let addon = match wait(&rx) { Some(crate::workshop::Msg::Addon(r)) => r?, _ => return Err("загрузка прервалась".into()) };
            let models: Vec<String> = addon.models.iter().map(|m| m.strip_prefix(&addon.dir).unwrap_or(m).to_string_lossy().into_owned()).collect();
            let jobs: Vec<crate::workshop::Job> = addon.models.iter().filter(|m| !only.is_empty() && m.to_string_lossy().to_lowercase().contains(&only))
                .map(|m| crate::workshop::Job { mdl: m.clone(), name: crate::workshop::clean_name(&m.file_stem().unwrap().to_string_lossy()), keep: String::new() }).collect();
            if jobs.is_empty() { return Ok(json!({"title": item.title, "models_in_addon": models, "imported": [], "hint": "укажи подстроку имени модели"})); }
            crate::workshop::import(p.assets.clone(), item.clone(), jobs, vec![], tx);
            match wait(&rx) { Some(crate::workshop::Msg::Imported(r)) => Ok(json!({"title": item.title, "imported": r?, "models_in_addon": models})), _ => Err("импорт прервался".into()) }
        })(),
        "check" => Ok(tools::check(&project())),
        "shot" => tools::screenshot(&project(), opt(args, "--camera").as_deref(), scene.as_deref(), pos.get(1).map(PathBuf::from).as_deref(), has(args, "--editor")),
        "run" => (|| { let p = project(); let exe = tools::compile(&p)?; let mut c = std::process::Command::new(exe); c.current_dir(&p.root).arg("--project").arg(&p.root); if has(args, "--editor") { c.arg("--editor"); } let s = c.status().map_err(|e| e.to_string())?; Ok(json!({"exit": s.code()})) })(),
        "build" => tools::build(&project()),
        _ => unreachable!(),
    };
    match r {
        Ok(v) => { if json_out { println!("{}", serde_json::to_string_pretty(&v).unwrap()); } else { print_human(&v); } Some(0) }
        Err(e) => { if json_out { println!("{}", json!({"error": e})); } else { eprintln!("ошибка: {e}"); } Some(1) }
    }
}

fn print_human(v: &Value) {
    match v {
        Value::Object(o) => for (k, x) in o {
            match x {
                Value::Array(a) if a.iter().all(|y| y.is_string()) => println!("{k} ({}): {}", a.len(), a.iter().map(|y| y.as_str().unwrap()).collect::<Vec<_>>().join(", ")),
                Value::Array(a) => { println!("{k} ({}):", a.len()); for y in a { println!("  {}", serde_json::to_string(y).unwrap()); } }
                Value::String(s) => println!("{k}: {s}"),
                _ => println!("{k}: {x}"),
            }
        },
        _ => println!("{v}"),
    }
}
