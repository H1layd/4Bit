//! 4bit — the hub window: projects (new from a template, recent), the project's assets, a model viewer with
//! animation tools, model import (glTF / GLB / Source, drag a file onto the window), Garry's Mod Workshop import,
//! release builds, and starting the game or its scene editor. `4bit <command>` runs the same tools from a terminal.
mod preview;

use fourbit::look;
use fourbit::model::{Clip, Model};
use fourbit_tools::{cli, projects, tools, workshop};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};
use workshop::{Addon, Item, Job, Msg};

#[derive(Clone, PartialEq)]
enum Sel { None, Model(String), Texture(String), Sound(String), Scene(String) }

#[derive(Default)]
struct Index { models: Vec<String>, textures: Vec<String>, sounds: Vec<String>, scenes: Vec<String> }
impl Index {
    fn of(p: &fourbit::Project) -> Index {
        let v = tools::assets(p);
        let l = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        Index { models: l("models"), textures: l("textures"), sounds: l("sounds"), scenes: l("scenes") }
    }
}

struct Ws { input: String, item: Option<Item>, thumb: Option<egui::TextureHandle>, addon: Option<Addon>, picks: Vec<(bool, String, String)>, sound_picks: Vec<bool>, busy: Option<&'static str> }

/// Work in a thread: its log lines and its result.
enum Job2 { Line(String), Done(Result<Value, String>, &'static str) }

struct Anim { clip: usize, rename: String, trim: (usize, usize), from: String, dirty: bool, skeleton: bool }

struct Gpu { surface: wgpu::Surface<'static>, device: wgpu::Device, queue: wgpu::Queue, config: wgpu::SurfaceConfiguration }

struct Hub {
    window: Arc<Window>,
    gpu: Gpu,
    egui: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    preview: preview::Preview,
    view_px: [u32; 2],
    engine: Option<PathBuf>,
    templates: Vec<(String, String)>,
    recent: Vec<PathBuf>,
    new_name: String, new_parent: String, new_template: usize, open_path: String,
    project: Option<fourbit::Project>,
    index: Index,
    filter: String,
    sel: Sel,
    tex_view: Option<(String, egui::TextureHandle, [usize; 2])>,
    error: Option<String>,
    tab: usize,
    ws: Ws,
    tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>,
    jtx: mpsc::Sender<Job2>, jrx: mpsc::Receiver<Job2>,
    busy: Option<&'static str>,
    log: Vec<String>,
    import_path: String, import_name: String, import_folder: String,
    anim: Anim,
    last_build: Option<PathBuf>,
    audio: Option<(rodio::OutputStream, rodio::Sink)>,
    last: std::time::Instant,
}

impl Hub {
    fn new(window: Arc<Window>, open: Option<PathBuf>) -> Hub {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, compatible_surface: Some(&surface), force_fallback_adapter: false })).expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: None, required_features: wgpu::Features::empty(), required_limits: wgpu::Limits::default(), memory_hints: wgpu::MemoryHints::Performance }, None)).expect("device");
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration { usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format, width: size.width.max(1), height: size.height.max(1), present_mode: wgpu::PresentMode::AutoVsync, alpha_mode: caps.alpha_modes[0], view_formats: vec![], desired_maximum_frame_latency: 2 };
        surface.configure(&device, &config);
        let ctx = egui::Context::default();
        look::apply(&ctx, None);
        let egui = egui_winit::State::new(ctx, egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, Some(device.limits().max_texture_dimension_2d as usize));
        let renderer = egui_wgpu::Renderer::new(&device, format, None, 1, false);
        let preview = preview::Preview::new(&device);
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        let engine = projects::engine_root();
        let templates = engine.as_ref().map(|e| projects::templates(e)).unwrap_or_default();
        let mut hub = Hub { window, gpu: Gpu { surface, device, queue, config }, egui, renderer, preview, view_px: [640, 480], engine, templates, recent: projects::recent(),
            new_name: "Моя игра".into(), new_parent: projects::default_parent().to_string_lossy().into_owned(), new_template: 0, open_path: String::new(),
            project: None, index: Index::default(), filter: String::new(), sel: Sel::None, tex_view: None, error: None, tab: 0,
            ws: Ws { input: String::new(), item: None, thumb: None, addon: None, picks: vec![], sound_picks: vec![], busy: None },
            tx, rx, jtx, jrx, busy: None, log: vec![], import_path: String::new(), import_name: String::new(), import_folder: "imported".into(),
            anim: Anim { clip: 0, rename: String::new(), trim: (0, 0), from: String::new(), dirty: false, skeleton: true }, last_build: None, audio: None, last: std::time::Instant::now() };
        if let Some(p) = open.filter(|p| p.join("project.json").is_file()) { hub.open(&p); }
        hub
    }

    fn open(&mut self, root: &Path) {
        let p = fourbit::Project::load(root);
        projects::remember(root);
        self.recent = projects::recent();
        self.index = Index::of(&p);
        self.window.set_title(&format!("{} — 4bit", p.name));
        self.log.push(format!("открыт проект {} ({})", p.name, root.display()));
        self.project = Some(p);
        self.sel = Sel::None;
        if let Some(m) = self.index.models.first().cloned() { self.select(Sel::Model(m)); }
    }
    fn assets(&self) -> PathBuf { self.project.as_ref().map(|p| p.assets.clone()).unwrap_or_default() }

    fn select(&mut self, sel: Sel) {
        self.error = None;
        match &sel {
            Sel::Model(m) => {
                let path = self.assets().join("models").join(format!("{m}.wmdl"));
                match self.preview.load(&self.gpu.device, &self.gpu.queue, &path) {
                    Ok(()) => { self.anim = Anim { clip: 0, rename: String::new(), trim: (0, 0), from: String::new(), dirty: false, skeleton: self.anim.skeleton }; self.sync_anim(); }
                    Err(e) => self.error = Some(e),
                }
            }
            Sel::Texture(t) => match image::open(self.assets().join("textures").join(format!("{t}.png"))) {
                Ok(img) => {
                    let img = img.to_rgba8();
                    let size = [img.width() as usize, img.height() as usize];
                    let h = self.egui.egui_ctx().load_texture(t.clone(), egui::ColorImage::from_rgba_unmultiplied(size, &img), egui::TextureOptions::NEAREST);
                    self.tex_view = Some((t.clone(), h, size));
                }
                Err(e) => self.error = Some(e.to_string()),
            },
            Sel::Sound(s) => { let p = self.assets().join("audio").join(format!("{s}.wav")); self.play(&p); }
            _ => {}
        }
        self.sel = sel;
    }
    fn sync_anim(&mut self) {
        if let Some(l) = &self.preview.model {
            let c = self.anim.clip.min(l.model.clips.len().saturating_sub(1));
            self.anim.clip = c;
            if let Some(clip) = l.model.clips.get(c) { self.anim.rename = clip.name.clone(); self.anim.trim = (0, clip.frames.len().saturating_sub(1)); }
        }
    }
    fn play(&mut self, p: &Path) {
        if self.audio.is_none() { if let Ok((s, h)) = rodio::OutputStream::try_default() { if let Ok(sink) = rodio::Sink::try_new(&h) { self.audio = Some((s, sink)); } } }
        let Some((_, sink)) = &self.audio else { return };
        sink.stop();
        match std::fs::File::open(p).map_err(|e| e.to_string()).and_then(|f| rodio::Decoder::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())) { Ok(d) => sink.append(d), Err(e) => self.error = Some(e) }
    }
    /// Work off the UI thread; `what` names it in the log.
    fn job(&mut self, what: &'static str, f: impl FnOnce(mpsc::Sender<Job2>) -> Result<Value, String> + Send + 'static) {
        if self.busy.is_some() { return; }
        self.busy = Some(what);
        self.log.push(format!("{what}…"));
        let tx = self.jtx.clone();
        std::thread::spawn(move || { let r = f(tx.clone()); let _ = tx.send(Job2::Done(r, what)); });
    }
    fn launch(&mut self, editor: bool, scene: Option<String>) {
        let Some(p) = self.project.clone() else { return };
        self.job(if editor { "сборка и запуск редактора сцены" } else { "сборка и запуск игры" }, move |tx| {
            let exe = tools::compile(&p)?;
            let mut c = std::process::Command::new(exe);
            c.current_dir(&p.root).arg("--project").arg(&p.root);
            if editor { c.arg("--editor"); }
            if let Some(s) = scene { c.arg("--scene").arg(tools::scene_path(&p, Some(&s))); }
            c.spawn().map_err(|e| e.to_string())?;
            let _ = tx.send(Job2::Line("запущено".into()));
            Ok(Value::Null)
        });
    }

    fn poll(&mut self) {
        while let Ok(m) = self.jrx.try_recv() {
            match m {
                Job2::Line(l) => self.log.push(l),
                Job2::Done(r, what) => {
                    self.busy = None;
                    match r {
                        Ok(v) => {
                            if !v.is_null() { self.log.push(format!("{what}: {}", serde_json::to_string(&v).unwrap_or_default().chars().take(400).collect::<String>())); }
                            if let Some(f) = v["folder"].as_str() { self.last_build = Some(PathBuf::from(f)); }
                            if let Some(m) = v["model"].as_str() { if let Some(p) = &self.project { self.index = Index::of(p); } self.select(Sel::Model(m.to_string())); }
                        }
                        Err(e) => { self.log.push(format!("{what}: ошибка: {e}")); self.error = Some(e); }
                    }
                }
            }
        }
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(s) => self.log.push(s),
                Msg::Details(r) => { self.ws.busy = None; match r {
                    Ok(item) => {
                        self.log.push(format!("мастерская: {} ({:.1} МБ)", item.title, item.size as f64 / 1e6));
                        if !item.preview_url.is_empty() { let (url, tx) = (item.preview_url.clone(), self.tx.clone()); std::thread::spawn(move || { if let Ok(b) = workshop::fetch(&url, 8 << 20) { let _ = tx.send(Msg::Preview(b)); } }); }
                        self.ws.item = Some(item); self.ws.addon = None; self.ws.thumb = None;
                    }
                    Err(e) => self.log.push(format!("ошибка: {e}")),
                } }
                Msg::Preview(b) => if let Ok(img) = image::load_from_memory(&b) {
                    let img = img.thumbnail(512, 512).to_rgba8();
                    self.ws.thumb = Some(self.egui.egui_ctx().load_texture("ws_thumb", egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img), Default::default()));
                },
                Msg::Addon(r) => { self.ws.busy = None; match r {
                    Ok(a) => {
                        self.log.push(format!("в аддоне: моделей {}, текстур {}, звуков {}", a.models.len(), a.textures, a.sounds.len()));
                        let few = a.models.len() <= 4;
                        self.ws.picks = a.models.iter().map(|m| (few, workshop::clean_name(&m.file_stem().unwrap().to_string_lossy()), String::new())).collect();
                        self.ws.sound_picks = vec![false; a.sounds.len()];
                        self.ws.addon = Some(a);
                    }
                    Err(e) => self.log.push(format!("ошибка: {e}")),
                } }
                Msg::Imported(r) => { self.ws.busy = None; match r {
                    Ok(done) => { self.log.push(format!("импортировано: {}", done.join(", "))); if let Some(p) = &self.project { self.index = Index::of(p); } if let Some(f) = done.first() { self.select(Sel::Model(f.clone())); } }
                    Err(e) => self.log.push(format!("ошибка: {e}")),
                } }
            }
        }
        if self.log.len() > 500 { let n = self.log.len() - 500; self.log.drain(..n); }
    }

    fn ui(&mut self, ctx: &egui::Context) {
        let mut pick: Option<Sel> = None;
        egui::TopBottomPanel::top("top").exact_height(64.0).frame(egui::Frame::new()).show(ctx, |ui| {
            let r = ui.max_rect();
            look::gradient(ui.painter(), r, look::BAR_TOP, look::BAR_BOTTOM);
            ui.painter().hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, look::BAR_EDGE));
            ui.horizontal_centered(|ui| {
                ui.add_space(14.0);
                look::logo(ui, 34.0);
                ui.add_space(18.0);
                if let Some(p) = self.project.clone() {
                    ui.vertical(|ui| {
                        ui.add_space(9.0);
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(egui::RichText::new("поиск ассетов").color(look::GREY)).desired_width(300.0).font(egui::FontId::proportional(15.0)));
                            if ui.button("Мне повезёт!").clicked() && !self.index.models.is_empty() {
                                let k = (self.last.elapsed().as_nanos() as usize ^ self.log.len() * 7919) % self.index.models.len();
                                pick = Some(Sel::Model(self.index.models[k].clone()));
                            }
                        });
                        ui.label(egui::RichText::new(format!("{} · {}", p.name, p.root.display())).small().color(look::GREY));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(14.0);
                        let free = self.busy.is_none();
                        if look::shiny(ui, "Играть ▶", look::GO, free).clicked() { self.launch(false, None); }
                        ui.add_space(6.0);
                        if look::shiny(ui, "Редактор сцены", egui::Color32::from_rgb(40, 110, 210), free).clicked() { self.launch(true, None); }
                        ui.add_space(8.0);
                        if look::link(ui, "проекты", false).clicked() { self.project = None; self.window.set_title("4bit"); }
                        if look::link(ui, "обновить", false).clicked() { self.index = Index::of(&p); }
                    });
                } else {
                    ui.label(egui::RichText::new("движок для 3D-игр на Rust").color(look::GREY));
                }
            });
        });
        egui::TopBottomPanel::bottom("log").resizable(true).default_height(120.0).frame(egui::Frame::new().fill(look::PAGE).inner_margin(egui::Margin::same(6))).show(ctx, |ui| {
            ui.horizontal(|ui| { look::heading(ui, "Журнал"); if let Some(b) = self.busy { ui.spinner(); ui.label(b); } });
            log_box(ui, &self.log, "log");
        });
        if self.project.is_none() { egui::CentralPanel::default().frame(egui::Frame::new().fill(look::PAGE).inner_margin(egui::Margin::same(24))).show(ctx, |ui| self.projects_page(ui)); return; }
        egui::SidePanel::left("assets").resizable(false).exact_width(260.0).frame(egui::Frame::new().fill(look::PAGE).inner_margin(egui::Margin::same(8))).show(ctx, |ui| {
            let f = self.filter.to_lowercase();
            let hit = |s: &str| f.is_empty() || s.to_lowercase().contains(&f);
            egui::ScrollArea::vertical().show(ui, |ui| {
                look::heading(ui, &format!("Сцены ({})", self.index.scenes.len()));
                for s in &self.index.scenes { if hit(s) && look::link(ui, s, self.sel == Sel::Scene(s.clone())).clicked() { pick = Some(Sel::Scene(s.clone())); } }
                ui.add_space(6.0);
                look::heading(ui, &format!("Модели ({})", self.index.models.len()));
                for m in &self.index.models { if hit(m) && look::link(ui, m, self.sel == Sel::Model(m.clone())).clicked() { pick = Some(Sel::Model(m.clone())); } }
                ui.add_space(6.0);
                egui::CollapsingHeader::new(egui::RichText::new(format!("Текстуры ({})", self.index.textures.len())).color(look::MAROON).strong()).show(ui, |ui| {
                    for t in &self.index.textures { if hit(t) && look::link(ui, t, self.sel == Sel::Texture(t.clone())).clicked() { pick = Some(Sel::Texture(t.clone())); } }
                });
                egui::CollapsingHeader::new(egui::RichText::new(format!("Звуки ({})", self.index.sounds.len())).color(look::MAROON).strong()).show(ui, |ui| {
                    for s in &self.index.sounds { if hit(s) && look::link(ui, s, self.sel == Sel::Sound(s.clone())).clicked() { pick = Some(Sel::Sound(s.clone())); } }
                });
            });
        });
        egui::SidePanel::right("side").resizable(false).exact_width(370.0).frame(egui::Frame::new().fill(look::PAGE).inner_margin(egui::Margin::same(8))).show(ctx, |ui| {
            look::tabs(ui, &["Модель", "Импорт", "Мастерская", "Сборка"], &mut self.tab);
            ui.add_space(2.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, look::BAR_EDGE));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().id_salt("side").show(ui, |ui| match self.tab { 0 => self.model_tab(ui), 1 => self.import_tab(ui), 2 => self.workshop_tab(ui), _ => self.build_tab(ui) });
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(look::PAGE).inner_margin(egui::Margin::same(8))).show(ctx, |ui| self.central(ui));
        if let Some(s) = pick { self.select(s); }
    }

    fn projects_page(&mut self, ui: &mut egui::Ui) {
        let Some(engine) = self.engine.clone() else {
            ui.colored_label(look::MAROON, "Не нашёл папку движка (crates/fourbit и templates/). Запусти 4bit из его папки или задай FOURBIT_HOME.");
            return;
        };
        ui.columns(2, |c| {
            look::heading(&mut c[0], "Новый проект");
            look::post().show(&mut c[0], |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| { ui.label("название"); ui.add(egui::TextEdit::singleline(&mut self.new_name).desired_width(260.0)); });
                ui.horizontal(|ui| { ui.label("папка"); ui.add(egui::TextEdit::singleline(&mut self.new_parent).desired_width(300.0)); });
                ui.label(egui::RichText::new(format!("будет: {}", Path::new(&self.new_parent).join(self.new_name.trim()).display())).small().color(look::GREY));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("шаблон").strong());
                for (i, (n, about)) in self.templates.iter().enumerate() {
                    ui.radio_value(&mut self.new_template, i, egui::RichText::new(n).strong());
                    if !about.is_empty() { ui.label(egui::RichText::new(about).small().color(look::GREY)); }
                }
                ui.add_space(8.0);
                let ok = !self.new_name.trim().is_empty() && !self.templates.is_empty();
                if look::shiny(ui, "Создать проект", look::GO, ok).clicked() {
                    let t = self.templates[self.new_template.min(self.templates.len() - 1)].0.clone();
                    match projects::create(&engine, &t, Path::new(&self.new_parent), &self.new_name) {
                        Ok(dir) => { self.log.push(format!("создан {}", dir.display())); self.open(&dir); }
                        Err(e) => self.error = Some(e),
                    }
                }
                ui.label(egui::RichText::new("в проекте сразу есть AGENTS.md / CLAUDE.md и .mcp.json — ИИ-агент, открытый в его папке, видит движок и его инструменты").small().color(look::GREY));
            });
            look::heading(&mut c[1], "Последние проекты");
            look::post().show(&mut c[1], |ui| {
                ui.set_width(ui.available_width());
                if self.recent.is_empty() { ui.label(egui::RichText::new("пока нет").color(look::GREY)); }
                for p in self.recent.clone() {
                    let name = fourbit::Project::load(&p).name;
                    if look::link(ui, &name, false).clicked() { self.open(&p); }
                    ui.label(egui::RichText::new(p.display().to_string()).small().color(look::GREY));
                    ui.add_space(3.0);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.open_path).hint_text(egui::RichText::new("путь к папке проекта").color(look::GREY)).desired_width(260.0));
                    if ui.button("Открыть").clicked() {
                        let p = PathBuf::from(self.open_path.trim());
                        if p.join("project.json").is_file() { self.open(&p); } else { self.error = Some("там нет project.json".into()); }
                    }
                });
            });
        });
        if let Some(e) = &self.error { ui.colored_label(egui::Color32::from_rgb(180, 20, 20), e); }
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.error { ui.colored_label(egui::Color32::from_rgb(180, 20, 20), e); }
        match self.sel.clone() {
            Sel::Model(name) => {
                let avail = ui.available_size();
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(avail.x, (avail.y - 70.0).max(60.0)), egui::Sense::click_and_drag());
                let ppp = ui.ctx().pixels_per_point();
                self.view_px = [(rect.width() * ppp) as u32, (rect.height() * ppp) as u32];
                if let Some(id) = self.preview.egui_tex { ui.painter().image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE); }
                if resp.dragged() { let d = resp.drag_delta(); self.preview.yaw += d.x * 0.4; self.preview.pitch = (self.preview.pitch + d.y * 0.4).clamp(-89.0, 89.0); }
                if resp.hovered() { let s = ui.input(|i| i.smooth_scroll_delta.y); if s != 0.0 { self.preview.dist *= (-s * 0.002).exp(); } }
                // skeleton over the picture
                if self.anim.skeleton { if let Some(l) = &self.preview.model {
                    let vp = self.preview.view_proj;
                    let at = |m: &glam::Mat4| { let c = vp * m.w_axis; (c.w > 0.0).then(|| egui::pos2(rect.left() + (c.x / c.w * 0.5 + 0.5) * rect.width(), rect.top() + (0.5 - c.y / c.w * 0.5) * rect.height())) };
                    for (i, b) in l.model.bones.iter().enumerate() {
                        if b.parent < 0 || i >= l.pose.len() { continue; }
                        if let (Some(a), Some(c)) = (at(&l.pose[i]), at(&l.pose[b.parent as usize])) { ui.painter().line_segment([a, c], egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 140, 0))); ui.painter().circle_filled(a, 2.0, egui::Color32::from_rgb(255, 220, 0)); }
                    }
                } }
                ui.painter().text(rect.left_top() + egui::vec2(10.0, 8.0), egui::Align2::LEFT_TOP, format!("{name} — тяни: вращать, колесо: приблизить"), egui::FontId::proportional(12.0), egui::Color32::from_gray(70));
                let Some(l) = &mut self.preview.model else { return };
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("{} костей · {} треуг. · {} клипов · {:.2} м", l.model.bones.len(), l.tris(), l.model.clips.len(), l.radius * 2.0)).small());
                    if ui.small_button("в центр").clicked() { self.preview.dist = l.radius * 2.6; self.preview.yaw = 200.0; self.preview.pitch = 12.0; }
                    ui.checkbox(&mut self.anim.skeleton, "скелет");
                });
                ui.horizontal(|ui| {
                    let names: Vec<String> = l.model.clips.iter().map(|c| c.name.clone()).collect();
                    let cur = l.clip.and_then(|c| names.get(c).cloned()).unwrap_or_else(|| "поза покоя".into());
                    egui::ComboBox::from_id_salt("clip").selected_text(cur).width(180.0).height(420.0).show_ui(ui, |ui| {
                        if ui.selectable_label(l.clip.is_none(), "поза покоя").clicked() { l.set_clip(None); }
                        for (i, n) in names.iter().enumerate() { if ui.selectable_label(l.clip == Some(i), n).clicked() { l.set_clip(Some(i)); l.playing = true; self.anim.clip = i; } }
                    });
                    if let Some(c) = l.clip {
                        if ui.button(if l.playing { "пауза" } else { "играть" }).clicked() { l.playing = !l.playing; }
                        let clip = &l.model.clips[c];
                        let (len, fps) = (clip.length(), clip.fps);
                        let mut t = l.time;
                        let r = ui.add(egui::Slider::new(&mut t, 0.0..=len).suffix(" с").max_decimals(2));
                        if r.dragged() || r.clicked() { l.playing = false; l.scrub(t); }
                        ui.label(format!("кадр {}", (l.time * fps).round()));
                        ui.add(egui::Slider::new(&mut l.speed, 0.1..=2.0).text("скорость").max_decimals(1));
                    }
                });
            }
            Sel::Texture(_) => if let Some((n, h, size)) = &self.tex_view {
                ui.label(format!("{n} · {}×{}", size[0], size[1]));
                let avail = ui.available_size();
                let k = (avail.x / size[0] as f32).min(avail.y / size[1] as f32).min(4.0);
                ui.centered_and_justified(|ui| ui.image((h.id(), egui::vec2(size[0] as f32 * k, size[1] as f32 * k))));
            },
            Sel::Sound(s) => { ui.heading(&s); if ui.button("ещё раз").clicked() { let p = self.assets().join("audio").join(format!("{s}.wav")); self.play(&p); } }
            Sel::Scene(s) => {
                let Some(p) = self.project.clone() else { return };
                look::heading(ui, &format!("Сцена {s}"));
                let v = tools::scene_get(&p, Some(&s)).unwrap_or(Value::Null);
                let ents = v["entities"].as_array().cloned().unwrap_or_default();
                ui.label(format!("{} сущностей", ents.len()));
                if look::shiny(ui, "Открыть в редакторе", egui::Color32::from_rgb(40, 110, 210), self.busy.is_none()).clicked() { self.launch(true, Some(s.clone())); }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for e in ents { ui.label(egui::RichText::new(format!("{} — {} {}", e["name"].as_str().unwrap_or(""), e["type"].as_str().unwrap_or(""), e["model"].as_str().unwrap_or(""))).small()); }
                });
            }
            Sel::None => { ui.centered_and_justified(|ui| ui.label("выбери ассет слева")); }
        }
    }

    /// Animation tools for the model on screen: clips (rename, loop, fps, duplicate, delete, trim), clips from another
    /// model with the same bone names, and saving the .wmdl.
    fn model_tab(&mut self, ui: &mut egui::Ui) {
        let models = self.index.models.clone();
        let Some(l) = &mut self.preview.model else { ui.label(egui::RichText::new("выбери модель слева").color(look::GREY)); return };
        look::heading(ui, &l.path.file_stem().unwrap_or_default().to_string_lossy());
        look::post().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("Анимации").strong());
            if l.model.clips.is_empty() { ui.label(egui::RichText::new("у модели нет клипов").small().color(look::GREY)); }
            let mut chosen = None;
            for (i, c) in l.model.clips.iter().enumerate() {
                if ui.selectable_label(self.anim.clip == i, format!("{} · {} кадров · {:.2} с{}", c.name, c.frames.len(), c.length(), if c.looping { " · цикл" } else { "" })).clicked() { chosen = Some(i); }
            }
            if let Some(i) = chosen { self.anim.clip = i; l.set_clip(Some(i)); l.playing = true; if let Some(c) = l.model.clips.get(i) { self.anim.rename = c.name.clone(); self.anim.trim = (0, c.frames.len().saturating_sub(1)); } }
            let c = self.anim.clip;
            if c < l.model.clips.len() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.anim.rename).desired_width(150.0));
                    if ui.button("переименовать").clicked() && !self.anim.rename.trim().is_empty() { l.model.clips[c].name = self.anim.rename.trim().to_string(); self.anim.dirty = true; }
                });
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut l.model.clips[c].looping, "цикл").changed() { self.anim.dirty = true; }
                    if ui.add(egui::DragValue::new(&mut l.model.clips[c].fps).range(1.0..=240.0).prefix("кадров/с ")).changed() { self.anim.dirty = true; }
                });
                let n = l.model.clips[c].frames.len().saturating_sub(1);
                ui.horizontal(|ui| {
                    ui.label("кадры");
                    ui.add(egui::DragValue::new(&mut self.anim.trim.0).range(0..=n));
                    ui.label("—");
                    ui.add(egui::DragValue::new(&mut self.anim.trim.1).range(0..=n));
                    if ui.button("в новый клип").on_hover_text("вырезать кусок клипа в отдельный").clicked() && self.anim.trim.1 > self.anim.trim.0 {
                        let src = &l.model.clips[c];
                        let clip = Clip { name: format!("{}_{}_{}", src.name, self.anim.trim.0, self.anim.trim.1), fps: src.fps, looping: src.looping, frames: src.frames[self.anim.trim.0..=self.anim.trim.1].to_vec() };
                        l.model.clips.push(clip); self.anim.dirty = true;
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("дублировать").clicked() { let src = &l.model.clips[c]; let d = Clip { name: format!("{} копия", src.name), fps: src.fps, looping: src.looping, frames: src.frames.clone() }; l.model.clips.push(d); self.anim.dirty = true; }
                    if ui.button("удалить").clicked() { l.model.clips.remove(c); self.anim.clip = 0; l.touched(); self.anim.dirty = true; }
                });
            }
            ui.separator();
            ui.label(egui::RichText::new("Клипы из другой модели").strong()).on_hover_text("кости сопоставляются по именам (GMod, Mixamo — один скелет на много моделей)");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("from").selected_text(if self.anim.from.is_empty() { "модель…" } else { &self.anim.from }).width(200.0).height(400.0).show_ui(ui, |ui| {
                    for m in &models { ui.selectable_value(&mut self.anim.from, m.clone(), m.as_str()); }
                });
                if ui.add_enabled(!self.anim.from.is_empty(), egui::Button::new("добавить")).clicked() {
                    let base = l.path.ancestors().find(|a| a.ends_with("models")).map(Path::to_path_buf).unwrap_or_default();
                    match Model::load(&base.join(format!("{}.wmdl", self.anim.from))) {
                        Ok(src) => { let n = merge_clips(&mut l.model, &src); self.log.push(format!("добавлено клипов: {n} из {}", self.anim.from)); self.anim.dirty |= n > 0; }
                        Err(e) => self.error = Some(e),
                    }
                }
            });
            ui.separator();
            if look::shiny(ui, if self.anim.dirty { "Сохранить модель *" } else { "Сохранить модель" }, egui::Color32::from_rgb(40, 110, 210), self.anim.dirty).clicked() {
                // one backup of the original next to it, then the edit
                let bak = l.path.with_extension("wmdl.bak");
                if !bak.exists() { std::fs::copy(&l.path, &bak).ok(); }
                match l.model.save(&l.path) { Ok(()) => { self.anim.dirty = false; self.log.push(format!("сохранено {} (оригинал: {})", l.path.display(), bak.display())); } Err(e) => self.error = Some(e.to_string()) }
            }
        });
        ui.add_space(6.0);
        egui::CollapsingHeader::new(egui::RichText::new(format!("Кости ({})", l.model.bones.len())).color(look::MAROON).strong()).show(ui, |ui| {
            for b in &l.model.bones {
                let depth = { let mut d = 0; let mut q = b.parent; while q >= 0 && d < 64 { d += 1; q = l.model.bones[q as usize].parent; } d };
                ui.label(egui::RichText::new(format!("{}{}", "  ".repeat(depth.min(12)), b.name)).monospace().size(11.0));
            }
        });
        egui::CollapsingHeader::new(egui::RichText::new("Меши").color(look::MAROON).strong()).show(ui, |ui| { for (n, on) in l.parts() { ui.checkbox(on, n.as_str()); } });
    }

    fn import_tab(&mut self, ui: &mut egui::Ui) {
        look::heading(ui, "Импорт модели");
        ui.label(egui::RichText::new(".glb / .gltf (Blender, Sketchfab, Poly Haven, Mixamo через Blender) или .mdl (Source / GMod). Можно просто перетащить файл в окно.").small().color(look::GREY));
        look::post().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::TextEdit::singleline(&mut self.import_path).hint_text(egui::RichText::new("путь к файлу").color(look::GREY)).desired_width(f32::INFINITY));
            ui.horizontal(|ui| { ui.label("имя"); ui.add(egui::TextEdit::singleline(&mut self.import_name).hint_text(egui::RichText::new("как у файла").color(look::GREY)).desired_width(120.0)); ui.label("папка"); ui.add(egui::TextEdit::singleline(&mut self.import_folder).desired_width(90.0)); });
            if look::shiny(ui, "Импортировать", look::GO, self.busy.is_none() && !self.import_path.trim().is_empty()).clicked() { self.import_now(); }
        });
    }
    fn import_now(&mut self) {
        let Some(p) = self.project.clone() else { return };
        let (file, name, folder) = (PathBuf::from(self.import_path.trim()), self.import_name.trim().to_string(), self.import_folder.trim().to_string());
        self.job("импорт", move |_| tools::import(&p, &file, (!name.is_empty()).then_some(name.as_str()), (!folder.is_empty()).then_some(folder.as_str())));
    }

    fn workshop_tab(&mut self, ui: &mut egui::Ui) {
        look::heading(ui, "Мастерская Steam · Garry's Mod");
        ui.label(egui::RichText::new("вставь ссылку на предмет или его id").small().color(look::GREY));
        let busy = self.ws.busy.is_some();
        ui.horizontal(|ui| {
            let edit = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut self.ws.input).hint_text(egui::RichText::new("steamcommunity.com/sharedfiles/filedetails/?id=…").color(look::GREY)).desired_width(ui.available_width() - 64.0));
            if ui.add_enabled(!busy, egui::Button::new("найти")).clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                match workshop::parse_id(&self.ws.input) {
                    Some(id) => { self.ws.busy = Some("ищу…"); let tx = self.tx.clone(); std::thread::spawn(move || { let _ = tx.send(Msg::Details(workshop::details(id))); }); }
                    None => self.log.push("не вижу id в ссылке".into()),
                }
            }
        });
        if let Some(b) = self.ws.busy { ui.horizontal(|ui| { ui.spinner(); ui.label(b); }); }
        let Some(item) = self.ws.item.clone() else { return };
        look::post().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| { ui.label(egui::RichText::new(&item.title).color(egui::Color32::from_rgb(15, 12, 93)).strong()); ui.label(egui::RichText::new("Аноним").color(look::NAME).strong()); ui.label(egui::RichText::new(format!("No.{}", item.id)).small()); });
            if let Some(t) = &self.ws.thumb { ui.add(egui::Image::new(t).max_width(250.0)); }
            ui.label(egui::RichText::new(format!("Файл: {:.1} МБ", item.size as f64 / 1e6)).small());
            if self.ws.addon.is_none() && ui.add_enabled(!busy, egui::Button::new("скачать и распаковать")).clicked() {
                self.ws.busy = Some("качаю…");
                workshop::download(self.project.as_ref().map(|p| p.root.clone()).unwrap_or_default(), item.clone(), self.tx.clone());
            }
            if let Some(a) = self.ws.addon.clone() {
                for (i, m) in a.models.iter().enumerate() {
                    let rel = m.strip_prefix(&a.dir).unwrap_or(m).to_string_lossy().into_owned();
                    let (on, name, keep) = &mut self.ws.picks[i];
                    ui.checkbox(on, egui::RichText::new(rel).small());
                    if *on { ui.horizontal(|ui| { ui.label("имя"); ui.add(egui::TextEdit::singleline(name).desired_width(120.0)); ui.label("части"); ui.add(egui::TextEdit::singleline(keep).desired_width(80.0)); }); }
                }
                if !a.sounds.is_empty() { egui::CollapsingHeader::new(format!("звуки ({})", a.sounds.len())).show(ui, |ui| { for (i, s) in a.sounds.iter().enumerate() { ui.checkbox(&mut self.ws.sound_picks[i], egui::RichText::new(s.file_name().unwrap().to_string_lossy()).small()); } }); }
                let n = self.ws.picks.iter().filter(|p| p.0).count() + self.ws.sound_picks.iter().filter(|&&p| p).count();
                if ui.add_enabled(!busy && n > 0, egui::Button::new(format!("импортировать ({n})"))).clicked() {
                    let jobs = a.models.iter().zip(&self.ws.picks).filter(|(_, p)| p.0).map(|(m, p)| Job { mdl: m.clone(), name: workshop::clean_name(&p.1), keep: p.2.clone() }).collect();
                    let sounds = a.sounds.iter().zip(&self.ws.sound_picks).filter(|(_, &on)| on).map(|(s, _)| s.clone()).collect();
                    self.ws.busy = Some("импортирую…");
                    workshop::import(self.assets(), item.clone(), jobs, sounds, self.tx.clone());
                }
                ui.label(egui::RichText::new("→ assets/models/workshop/<имя>; автор — в models/WORKSHOP.md").small().color(look::GREY));
            }
        });
    }

    fn build_tab(&mut self, ui: &mut egui::Ui) {
        look::heading(ui, "Сборка игры");
        ui.label(egui::RichText::new(format!("для этого компьютера ({} {}): игра, ассеты и сцены в Builds/, плюс zip", std::env::consts::OS, std::env::consts::ARCH)).small().color(look::GREY));
        look::post().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if look::shiny(ui, "Собрать!", egui::Color32::from_rgb(40, 110, 210), self.busy.is_none()).clicked() {
                if let Some(p) = self.project.clone() { self.job("сборка", move |_| tools::build(&p)); }
            }
            if let Some(f) = self.last_build.clone() { if look::link(ui, &format!("открыть {}", f.display()), false).clicked() { open_folder(f.parent().unwrap_or(&f)); } }
            ui.separator();
            if ui.button("Проверить проект").on_hover_text("битые ссылки в сценах, нет текстур").clicked() {
                if let Some(p) = &self.project { let v = tools::check(p); self.log.push(format!("проверка: {}", if v["ok"].as_bool() == Some(true) { "всё в порядке".to_string() } else { serde_json::to_string(&v["problems"]).unwrap_or_default() })); }
            }
            if ui.button("Обновить файлы для ИИ").on_hover_text("AGENTS.md, CLAUDE.md, .mcp.json под эту версию движка").clicked() {
                if let (Some(p), Some(e)) = (&self.project, &self.engine) { match projects::write_ai_files(e, &p.root, &p.name, &projects::crate_name(&p.name)) { Ok(()) => self.log.push("файлы для ИИ обновлены".into()), Err(err) => self.error = Some(err.to_string()) } }
            }
        });
    }

    fn frame(&mut self) {
        let now = std::time::Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;
        self.poll();
        let raw = self.egui.take_egui_input(&self.window);
        let ctx = self.egui.egui_ctx().clone();
        let full = ctx.run(raw, |ctx| self.ui(ctx));
        self.egui.handle_platform_output(&self.window, full.platform_output);
        let jobs = ctx.tessellate(full.shapes, full.pixels_per_point);
        let g = &self.gpu;
        for (id, d) in &full.textures_delta.set { self.renderer.update_texture(&g.device, &g.queue, *id, d); }
        let out = match g.surface.get_current_texture() { Ok(o) => o, Err(_) => { g.surface.configure(&g.device, &g.config); return; } };
        let view = out.texture.create_view(&Default::default());
        let mut enc = g.device.create_command_encoder(&Default::default());
        if matches!(self.sel, Sel::Model(_)) && self.project.is_some() {
            self.preview.animate(&g.queue, dt);
            let fresh = self.preview.render(&g.device, &g.queue, &mut enc, self.view_px);
            let cv = &self.preview.color.as_ref().unwrap().1;
            match self.preview.egui_tex {
                None => self.preview.egui_tex = Some(self.renderer.register_native_texture(&g.device, cv, wgpu::FilterMode::Linear)),
                Some(id) if fresh => self.renderer.update_egui_texture_from_wgpu_texture(&g.device, cv, wgpu::FilterMode::Linear, id),
                _ => {}
            }
        }
        let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [g.config.width, g.config.height], pixels_per_point: full.pixels_per_point };
        let extra = self.renderer.update_buffers(&g.device, &g.queue, &mut enc, &jobs, &sd);
        {
            let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None });
            self.renderer.render(&mut pass.forget_lifetime(), &jobs, &sd);
        }
        g.queue.submit(extra.into_iter().chain([enc.finish()]));
        self.window.pre_present_notify();
        out.present();
        for id in &full.textures_delta.free { self.renderer.free_texture(id); }
        self.window.request_redraw();
    }
}

/// Copies every clip of `src` into `dst`, bone by name; bones `src` lacks keep `dst`'s rest pose. Returns clips added.
fn merge_clips(dst: &mut Model, src: &Model) -> usize {
    let map: Vec<Option<usize>> = dst.bones.iter().map(|b| src.bones.iter().position(|s| s.name == b.name)).collect();
    if map.iter().all(|m| m.is_none()) { return 0; }
    let rest = dst.rest_local();
    let mut n = 0;
    for c in &src.clips {
        let frames = c.frames.iter().map(|f| map.iter().enumerate().map(|(i, m)| m.map_or(rest[i], |k| f[k])).collect()).collect();
        let mut name = c.name.clone();
        while dst.clips.iter().any(|d| d.name == name) { name.push('+'); }
        dst.clips.push(Clip { name, fps: c.fps, looping: c.looping, frames });
        n += 1;
    }
    n
}

fn log_box(ui: &mut egui::Ui, lines: &[String], id: &str) {
    egui::Frame::new().fill(egui::Color32::WHITE).stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(170))).inner_margin(egui::Margin::same(5)).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(id).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for l in lines.iter().rev().take(300).collect::<Vec<_>>().into_iter().rev() { ui.label(egui::RichText::new(l).monospace().size(11.0)); }
        });
    });
}
fn open_folder(p: &Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else if cfg!(windows) { "explorer" } else { "xdg-open" };
    let _ = std::process::Command::new(opener).arg(p).spawn();
}

struct App { hub: Option<Hub>, open: Option<PathBuf> }
impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.hub.is_some() { return; }
        let w = Arc::new(el.create_window(Window::default_attributes().with_title("4bit").with_inner_size(winit::dpi::LogicalSize::new(1440.0, 900.0))).expect("window"));
        self.hub = Some(Hub::new(w, self.open.take()));
    }
    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(h) = &mut self.hub else { return };
        let _ = h.egui.on_window_event(&h.window, &event);
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => { h.gpu.config.width = s.width.max(1); h.gpu.config.height = s.height.max(1); h.gpu.surface.configure(&h.gpu.device, &h.gpu.config); }
            // a model file dropped on the window is imported into the open project
            WindowEvent::DroppedFile(p) => if h.project.is_some() { h.import_path = p.to_string_lossy().into_owned(); h.tab = 1; h.import_now(); },
            WindowEvent::RedrawRequested => h.frame(),
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = cli::run(&args) { std::process::exit(code); }
    let open = args.get(1).map(PathBuf::from).or_else(|| {
        let here = std::env::current_dir().ok()?;
        here.ancestors().find(|p| p.join("project.json").is_file()).map(Path::to_path_buf)
    });
    let el = EventLoop::new().expect("event loop");
    el.run_app(&mut App { hub: None, open }).expect("run");
}
