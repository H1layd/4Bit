//! Projects: create one from a template, remember recent ones. A project is a Rust crate with a `project.json`,
//! `assets/`, `scenes/`, `src/main.rs`, plus instructions and an MCP config so an AI agent can work in it at once.
use std::path::{Path, PathBuf};

/// The 4bit checkout this hub runs from (has `crates/fourbit` and `templates/`).
pub fn engine_root() -> Option<PathBuf> {
    let ok = |p: &Path| p.join("crates/fourbit/Cargo.toml").is_file() && p.join("templates").is_dir();
    std::env::var_os("FOURBIT_HOME").map(PathBuf::from).filter(|p| ok(p))
        .or_else(|| std::env::current_exe().ok().and_then(|e| e.ancestors().find(|p| ok(p)).map(Path::to_path_buf)))
        .or_else(|| std::env::current_dir().ok().and_then(|d| d.ancestors().find(|p| ok(p)).map(Path::to_path_buf)))
}

pub fn templates(engine: &Path) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(engine.join("templates")).into_iter().flatten().flatten().filter(|e| e.path().join("project.json").is_file())
        .map(|e| { let n = e.file_name().to_string_lossy().into_owned(); let about = std::fs::read_to_string(e.path().join("TEMPLATE.txt")).unwrap_or_default(); (n, about.trim().to_string()) }).collect();
    v.sort();
    v
}

fn home() -> PathBuf { PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default()) }
pub fn default_parent() -> PathBuf { home().join("4bit Projects") }

pub fn recent() -> Vec<PathBuf> {
    let text = std::fs::read_to_string(home().join(".4bit/recent.json")).unwrap_or_default();
    let v: Vec<PathBuf> = serde_json::from_str::<Vec<String>>(&text).unwrap_or_default().into_iter().map(PathBuf::from).filter(|p| p.join("project.json").is_file()).collect();
    v
}
pub fn remember(p: &Path) {
    let mut v = recent();
    v.retain(|x| x != p);
    v.insert(0, p.to_path_buf());
    v.truncate(12);
    let dir = home().join(".4bit");
    std::fs::create_dir_all(&dir).ok();
    std::fs::write(dir.join("recent.json"), serde_json::to_string_pretty(&v.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>()).unwrap()).ok();
}

/// A crate name from a title: lowercase ascii, digits and '_' (Cyrillic is transliterated).
pub fn crate_name(title: &str) -> String {
    const TR: [(&str, &str); 33] = [("а","a"),("б","b"),("в","v"),("г","g"),("д","d"),("е","e"),("ё","e"),("ж","zh"),("з","z"),("и","i"),("й","y"),("к","k"),("л","l"),("м","m"),("н","n"),("о","o"),("п","p"),("р","r"),("с","s"),("т","t"),("у","u"),("ф","f"),("х","h"),("ц","c"),("ч","ch"),("ш","sh"),("щ","sch"),("ъ",""),("ы","y"),("ь",""),("э","e"),("ю","yu"),("я","ya")];
    let mut s = String::new();
    for ch in title.to_lowercase().chars() {
        let c = ch.to_string();
        if let Some((_, t)) = TR.iter().find(|(r, _)| *r == c) { s.push_str(t); }
        else if ch.is_ascii_alphanumeric() { s.push(ch); } else { s.push('_'); }
    }
    let mut s = s.split('_').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("_");
    if s.is_empty() { s = "game".into(); }
    if s.chars().next().unwrap().is_ascii_digit() { s = format!("g_{s}"); }
    s
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let n = e.file_name().to_string_lossy().into_owned();
        if n == "target" || n == ".DS_Store" || n.starts_with("._") { continue; }
        if src.is_dir() { copy_dir(&src, &dst)?; } else { std::fs::copy(&src, &dst)?; }
    }
    Ok(())
}

/// Creates `parent/<title>` from `template`; the game depends on this engine checkout by path.
pub fn create(engine: &Path, template: &str, parent: &Path, title: &str) -> Result<PathBuf, String> {
    let dir = parent.join(title.trim());
    if dir.exists() { return Err(format!("папка уже есть: {}", dir.display())); }
    copy_dir(&engine.join("templates").join(template), &dir).map_err(|e| e.to_string())?;
    let krate = crate_name(title);
    let engine_dep = format!("path = {:?}", engine.join("crates/fourbit").to_string_lossy());
    for f in ["project.json", "Cargo.toml", "src/main.rs"] {
        let p = dir.join(f);
        if let Ok(t) = std::fs::read_to_string(&p) {
            std::fs::write(&p, t.replace("{{NAME}}", title.trim()).replace("{{CRATE}}", &krate).replace("{{ENGINE}}", &engine_dep)).map_err(|e| e.to_string())?;
        }
    }
    std::fs::remove_file(dir.join("TEMPLATE.txt")).ok();
    // the engine's lock file pins the same dependency versions (no surprise updates on first build)
    if let Ok(lock) = std::fs::read(engine.join("Cargo.lock")) { std::fs::write(dir.join("Cargo.lock"), lock).ok(); }
    write_ai_files(engine, &dir, title.trim(), &krate).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(".gitignore"), "target/\nBuilds/\n.DS_Store\n").ok();
    remember(&dir);
    Ok(dir)
}

/// Instructions for coding agents (Codex, Claude, Cursor…) and the MCP config, so an agent opened in the project
/// knows the engine and has its tools.
pub fn write_ai_files(engine: &Path, dir: &Path, title: &str, krate: &str) -> std::io::Result<()> {
    let guide = include_str!("../../../docs/AGENT_TEMPLATE.md").replace("{{NAME}}", title).replace("{{CRATE}}", krate)
        .replace("{{ENGINE}}", &engine.to_string_lossy());
    std::fs::write(dir.join("AGENTS.md"), &guide)?;
    std::fs::write(dir.join("CLAUDE.md"), "# Claude Code\n\nRead and follow @AGENTS.md.\n")?;
    let mcp_bin = engine.join("target/release").join(if cfg!(windows) { "4bit-mcp.exe" } else { "4bit-mcp" });
    let mcp = serde_json::json!({"mcpServers": {"4bit": {"command": mcp_bin, "args": ["--project", dir]}}});
    std::fs::write(dir.join(".mcp.json"), serde_json::to_string_pretty(&mcp).unwrap() + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn crate_names() {
        assert_eq!(crate_name("Мой Хоррор 2"), "moy_horror_2");
        assert_eq!(crate_name("  Cool  Game! "), "cool_game");
        assert_eq!(crate_name("3D"), "g_3d");
    }
}
