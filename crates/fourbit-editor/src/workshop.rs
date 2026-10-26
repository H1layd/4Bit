//! Steam Workshop (Garry's Mod, app 4000) -> extracted addon -> engine models.
//! Details come from the public Steam API (no key). Old items have a direct `file_url`; newer ones are only served
//! to Steam clients, so those go through Valve's steamcmd (anonymous login), installed on first use. The download is a
//! GMA archive, sometimes LZMA-compressed (`*.bin`). Models are converted by `fourbit::import::source`.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

pub const GMOD: u64 = 4000;

#[derive(Clone, Debug, Default)]
pub struct Item {
    pub id: u64,
    pub title: String,
    pub description: String,
    pub creator: String,
    pub size: u64,
    pub file_url: String,
    pub preview_url: String,
    pub app: u64,
}

/// A downloaded and extracted addon.
#[derive(Clone, Debug, Default)]
pub struct Addon {
    pub dir: PathBuf,
    pub models: Vec<PathBuf>,
    pub sounds: Vec<PathBuf>,
    pub textures: usize,
}

pub enum Msg {
    Log(String),
    Details(Result<Item, String>),
    Preview(Vec<u8>),
    Addon(Result<Addon, String>),
    Imported(Result<Vec<String>, String>),
}

/// `https://steamcommunity.com/sharedfiles/filedetails/?id=123`, `...?id=123&searchtext=`, or just `123`.
pub fn parse_id(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Ok(n) = s.parse() { return Some(n); }
    let at = s.find("id=")? + 3;
    s[at..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
}

fn agent() -> ureq::Agent { ureq::AgentBuilder::new().timeout_connect(std::time::Duration::from_secs(15)).build() }

pub fn details(id: u64) -> Result<Item, String> {
    let body = agent().post("https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/")
        .send_form(&[("itemcount", "1"), ("publishedfileids[0]", &id.to_string())])
        .map_err(|e| format!("steam api: {e}"))?.into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let d = &v["response"]["publishedfiledetails"][0];
    if d["result"].as_i64() != Some(1) { return Err("предмет не найден или скрыт".into()); }
    let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
    let n = |k: &str| d[k].as_u64().or_else(|| d[k].as_str().and_then(|x| x.parse().ok())).unwrap_or(0);
    Ok(Item { id, title: s("title"), description: s("description"), creator: s("creator"), size: n("file_size"),
              file_url: s("file_url"), preview_url: s("preview_url"), app: n("consumer_app_id").max(n("creator_app_id")) })
}

pub fn fetch(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let r = agent().get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let mut out = Vec::new();
    r.into_reader().take(limit).read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Where 4bit keeps its downloads and steamcmd (inside the ignored build dir of the project).
/// Where 4bit keeps Workshop downloads and steamcmd: per user, shared by every project.
pub fn cache(_project: &Path) -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default());
    home.join(".4bit/cache")
}

/// Download + extract an item in a worker thread; progress goes to `tx`.
pub fn download(project: PathBuf, item: Item, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let r = download_inner(&project, &item, &tx);
        let _ = tx.send(Msg::Addon(r));
    });
}

fn download_inner(project: &Path, item: &Item, tx: &Sender<Msg>) -> Result<Addon, String> {
    let log = |s: String| { let _ = tx.send(Msg::Log(s)); };
    let dir = cache(project).join("workshop").join(item.id.to_string());
    let data = if let Some(local) = installed(item.id) {
        log(format!("уже скачан Steam: {}", local.display()));
        read_any(&local)?
    } else if !item.file_url.is_empty() {
        log(format!("скачиваю напрямую ({:.1} МБ)…", item.size as f64 / 1e6));
        fetch(&item.file_url, 2 << 30)?
    } else {
        log("прямой ссылки нет — качаю через steamcmd (анонимно)…".into());
        let found = steamcmd(project, item.id, &log)?;
        read_any(&found)?
    };
    let gma = if data.starts_with(b"GMAD") { data } else {
        log("распаковываю LZMA…".into());
        let mut out = Vec::new();
        lzma_rs::lzma_decompress(&mut std::io::Cursor::new(&data), &mut out).map_err(|e| format!("lzma: {e:?}"))?;
        if !out.starts_with(b"GMAD") { return Err("это не аддон GMod (нет заголовка GMAD)".into()); }
        out
    };
    if dir.exists() { let _ = std::fs::remove_dir_all(&dir); }
    let n = extract_gma(&gma, &dir)?;
    log(format!("распаковано файлов: {n}"));
    Ok(scan(&dir))
}

/// The item already in a local Steam library (GMod installed with the item subscribed).
fn installed(id: u64) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?);
    let roots = [home.join("Library/Application Support/Steam"), home.join(".steam/steam"), home.join(".local/share/Steam"),
                 PathBuf::from("C:/Program Files (x86)/Steam")];
    roots.iter().map(|r| r.join(format!("steamapps/workshop/content/{GMOD}/{id}"))).find_map(|d| first_archive(&d))
}

fn first_archive(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    found.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    found.into_iter().next()
}

fn read_any(p: &Path) -> Result<Vec<u8>, String> { std::fs::read(p).map_err(|e| format!("{}: {e}", p.display())) }

fn steamcmd(project: &Path, id: u64, log: &dyn Fn(String)) -> Result<PathBuf, String> {
    let root = cache(project).join("steamcmd");
    let (url, exe) = if cfg!(target_os = "windows") {
        ("https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip", root.join("steamcmd.exe"))
    } else if cfg!(target_os = "macos") {
        ("https://steamcdn-a.akamaihd.net/client/installer/steamcmd_osx.tar.gz", root.join("steamcmd.sh"))
    } else {
        ("https://steamcdn-a.akamaihd.net/client/installer/steamcmd_linux.tar.gz", root.join("steamcmd.sh"))
    };
    if !exe.exists() {
        log("ставлю steamcmd (Valve)…".into());
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let archive = root.join(if url.ends_with(".zip") { "steamcmd.zip" } else { "steamcmd.tar.gz" });
        std::fs::write(&archive, fetch(url, 64 << 20)?).map_err(|e| e.to_string())?;
        // bsdtar on macOS/Windows 10+ reads both zip and tar.gz
        let ok = std::process::Command::new("tar").arg("-xf").arg(&archive).current_dir(&root).status().map_err(|e| e.to_string())?;
        if !ok.success() { return Err("не удалось распаковать steamcmd".into()); }
    }
    log("steamcmd: вход anonymous, загрузка (первый запуск обновляет steamcmd, это долго)…".into());
    let out = std::process::Command::new(&exe)
        .args(["+login", "anonymous", "+workshop_download_item", &GMOD.to_string(), &id.to_string(), "+quit"])
        .current_dir(&root).output().map_err(|e| format!("steamcmd: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines().filter(|l| l.contains("Success") || l.contains("ERROR") || l.contains("Downloaded")) { log(line.trim().to_string()); }
    // steamcmd puts content beside itself, or into the per-user Steam dir on macOS
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let places = [root.join(format!("steamapps/workshop/content/{GMOD}/{id}")),
                  home.join(format!("Library/Application Support/Steam/steamapps/workshop/content/{GMOD}/{id}")),
                  home.join(format!(".steam/steam/steamapps/workshop/content/{GMOD}/{id}"))];
    places.iter().find_map(|d| first_archive(d)).ok_or_else(|| {
        let tail: Vec<&str> = text.lines().rev().take(4).collect();
        format!("steamcmd не скачал предмет: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" / "))
    })
}

/// GMA (`GMAD` v1..3): header, required content, name/description/author, file table, then the files in order.
pub fn extract_gma(d: &[u8], dest: &Path) -> Result<usize, String> {
    let mut at = 4usize;
    let take = |at: &mut usize, n: usize| -> Result<&[u8], String> {
        let s = d.get(*at..*at + n).ok_or("обрезанный GMA")?; *at += n; Ok(s)
    };
    let string = |at: &mut usize| -> Result<String, String> {
        let end = d[*at..].iter().position(|&b| b == 0).ok_or("обрезанный GMA")?;
        let s = String::from_utf8_lossy(&d[*at..*at + end]).into_owned(); *at += end + 1; Ok(s)
    };
    let version = take(&mut at, 1)?[0];
    take(&mut at, 16)?; // steamid, timestamp
    if version > 1 { while !string(&mut at)?.is_empty() {} }
    string(&mut at)?; string(&mut at)?; string(&mut at)?;
    take(&mut at, 4)?;
    let mut files = Vec::new();
    loop {
        let num = u32::from_le_bytes(take(&mut at, 4)?.try_into().unwrap());
        if num == 0 { break; }
        let name = string(&mut at)?;
        let size = i64::from_le_bytes(take(&mut at, 8)?.try_into().unwrap()) as usize;
        take(&mut at, 4)?;
        files.push((name, size));
    }
    for (name, size) in &files {
        let data = take(&mut at, *size)?;
        let rel = name.replace('\\', "/").to_lowercase();
        // never write outside the addon folder
        if rel.split('/').any(|p| p == ".." || p.is_empty()) || rel.starts_with('/') { continue; }
        let p = dest.join(&rel);
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&p, data).map_err(|e| e.to_string())?;
    }
    Ok(files.len())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out); } else { out.push(p); }
    }
}

pub fn scan(dir: &Path) -> Addon {
    let mut all = Vec::new();
    walk(dir, &mut all);
    let ext = |p: &Path| p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut models: Vec<PathBuf> = all.iter().filter(|p| ext(p) == "mdl").cloned().collect();
    models.sort();
    let mut sounds: Vec<PathBuf> = all.iter().filter(|p| matches!(ext(p).as_str(), "wav")).cloned().collect();
    sounds.sort();
    let textures = all.iter().filter(|p| ext(p) == "vtf").count();
    Addon { dir: dir.to_path_buf(), models, sounds, textures }
}

/// A file name the engine can use: lowercase ascii, digits and `_`.
pub fn clean_name(s: &str) -> String {
    let mut n: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    while n.contains("__") { n = n.replace("__", "_"); }
    n.trim_matches('_').to_string()
}

pub struct Job { pub mdl: PathBuf, pub name: String, pub keep: String }

/// Converts the chosen models into the project (`assets/models/workshop/<name>/`), copies chosen sounds and credits
/// the item in assets/models/WORKSHOP.md (Workshop assets keep their authors' terms).
pub fn import(assets: PathBuf, item: Item, jobs: Vec<Job>, sounds: Vec<PathBuf>, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let r = (|| -> Result<Vec<String>, String> {
            let mut done = Vec::new();
            for j in &jobs {
                let out = assets.join("models/workshop").join(&j.name);
                let _ = tx.send(Msg::Log(format!("импорт {} → models/workshop/{}", j.mdl.file_name().unwrap().to_string_lossy(), j.name)));
                let keep: Vec<String> = j.keep.split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
                let rep = fourbit::import::source(&j.mdl, &out, &j.name, &keep)?;
                let _ = tx.send(Msg::Log(format!("  {} костей, {} мешей, клипы: {}", rep.bones, rep.meshes.len(), rep.clips.iter().map(|c| c.0.as_str()).collect::<Vec<_>>().join(" "))));
                for w in &rep.warnings { let _ = tx.send(Msg::Log(format!("  ! {w}"))); }
                done.push(format!("workshop/{}/{}", j.name, j.name));
            }
            std::fs::create_dir_all(assets.join("audio")).ok();
            for s in &sounds {
                let name = format!("ws{}_{}.wav", item.id, clean_name(&s.file_stem().unwrap().to_string_lossy()));
                std::fs::copy(s, assets.join("audio").join(&name)).map_err(|e| e.to_string())?;
                let _ = tx.send(Msg::Log(format!("звук → audio/{name}")));
            }
            credit(&assets, &item, &done, sounds.len())?;
            Ok(done)
        })();
        let _ = tx.send(Msg::Imported(r));
    });
}

fn credit(assets: &Path, item: &Item, models: &[String], sounds: usize) -> Result<(), String> {
    use std::io::Write;
    let path = assets.join("models/WORKSHOP.md");
    let new = !path.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
    if new { writeln!(f, "# Steam Workshop (Garry's Mod) assets imported with 4bit\n").ok(); }
    writeln!(f, "- **{}** — https://steamcommunity.com/sharedfiles/filedetails/?id={} (author steamid {}): {}{}",
             item.title, item.id, item.creator, models.join(", "), if sounds > 0 { format!(", {sounds} sounds") } else { String::new() }).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn ids_from_links() {
        assert_eq!(parse_id("https://steamcommunity.com/sharedfiles/filedetails/?id=104691717&searchtext=gun"), Some(104691717));
        assert_eq!(parse_id(" 2131057232 "), Some(2131057232));
        assert_eq!(parse_id("nothing"), None);
    }
    #[test] fn gma_round_trip() {
        let mut d = b"GMAD\x03".to_vec();
        d.extend([0u8; 16]); d.push(0); // no required content
        for s in ["name", "desc", "author"] { d.extend(s.as_bytes()); d.push(0); }
        d.extend(1i32.to_le_bytes());
        d.extend(1u32.to_le_bytes()); d.extend(b"models/Test.MDL\0"); d.extend(3i64.to_le_bytes()); d.extend(0u32.to_le_bytes());
        d.extend(2u32.to_le_bytes()); d.extend(b"../evil.txt\0"); d.extend(1i64.to_le_bytes()); d.extend(0u32.to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend(b"abcX");
        let dir = std::env::temp_dir().join(format!("4bit-gma-{}", std::process::id()));
        assert_eq!(extract_gma(&d, &dir).unwrap(), 2);
        assert_eq!(std::fs::read(dir.join("models/test.mdl")).unwrap(), b"abc");
        assert!(!dir.parent().unwrap().join("evil.txt").exists());
        assert_eq!(scan(&dir).models.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(clean_name("My Gun (v2)"), "my_gun_v2");
    }
}
