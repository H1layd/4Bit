//! The runtime: a window, the engine state, the frame loop. A game implements [`Game`] and calls [`run`]:
//! ```no_run
//! struct MyGame;
//! impl fourbit::Game for MyGame {
//!     fn update(&mut self, e: &mut fourbit::Engine, dt: f32) { if e.input.pressed(fourbit::Key::KeyF) { e.say("hello", 2.0); } }
//! }
//! fn main() { fourbit::run(MyGame); }
//! ```
//! Command line (every 4bit game understands it):
//!   `--editor`                     open the scene editor
//!   `--scene <file>`               another scene than the project's start scene
//!   `--shot <out.png>`             render a few frames, save the picture, quit (scripts, CI, AI agents)
//!   `--camera x,y,z,yaw,pitch`     where `--shot` looks from (default: the player start)
//!   `--frames <n>`                 frames before the shot (default 20)
//!   `--project <dir>`              the project folder (default: found from the working dir / the executable)
use crate::audio::Sfx;
use crate::controller::FirstPerson;
use crate::input::Input;
use crate::render::{self as gfx, DrawCmd, Gfx, LightIn, MeshId, PostParams};
use crate::scene::Scene;
use crate::text::Hud;
use crate::world::World;
use glam::{Mat4, Quat, Vec3};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

/// What a game does. Every method has a default: an empty game still runs its scene with a first-person player.
pub trait Game: 'static {
    /// Once, after the scene is loaded: build code-made parts of the world, set up the player, load sounds.
    fn init(&mut self, _e: &mut Engine) {}
    /// Every simulated frame (not while the editor is editing or paused).
    fn update(&mut self, _e: &mut Engine, _dt: f32) {}
    /// Every frame before rendering: push extra draws (`e.draws`, `e.overlay_draws` for first-person hands/weapons).
    fn draw(&mut self, _e: &mut Engine) {}
}

/// The project: `project.json` in its folder, assets under `assets/`.
#[derive(Clone, Debug)]
pub struct Project {
    pub root: PathBuf,
    pub assets: PathBuf,
    pub name: String,
    pub start_scene: PathBuf,
    pub window: (u32, u32),
    /// lines of the low-resolution scene target (360 = chunky lo-fi, 720 = clean)
    pub frame_lines: u32,
    /// post look: "lofi" (fisheye, grain, crushed levels) or "clean"
    pub look: String,
    /// a first-person player walks the scene
    pub player: bool,
    pub flashlight: bool,
    pub json: Value,
}
impl Project {
    /// `--project`, `FOURBIT_PROJECT`, or the nearest folder with a project.json above the working dir / executable.
    pub fn find(arg: Option<PathBuf>) -> Project {
        let has = |p: &Path| p.join("project.json").is_file();
        let mut starts: Vec<PathBuf> = arg.into_iter().chain(std::env::var_os("FOURBIT_PROJECT").map(PathBuf::from)).collect();
        starts.extend(std::env::current_dir().ok());
        if let Ok(e) = std::env::current_exe() { starts.push(e); }
        let root = starts.iter().find_map(|s| s.ancestors().find(|p| has(p)).map(Path::to_path_buf))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        Project::load(&root)
    }
    pub fn load(root: &Path) -> Project {
        let json: Value = std::fs::read_to_string(root.join("project.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        let s = |k: &str, d: &str| json[k].as_str().unwrap_or(d).to_string();
        // a packaged game keeps its assets beside the executable (or in the .app's Resources)
        let mut assets = root.join(s("assets", "assets"));
        if !assets.is_dir() {
            if let Ok(e) = std::env::current_exe() {
                for c in [e.with_file_name("assets"), e.parent().unwrap().join("../Resources/assets")] { if c.is_dir() { assets = c; break; } }
            }
        }
        Project {
            name: s("name", "4bit game"),
            start_scene: root.join(s("start_scene", "scenes/main.json")),
            window: (json["window"]["width"].as_u64().unwrap_or(1280) as u32, json["window"]["height"].as_u64().unwrap_or(720) as u32),
            frame_lines: json["render"]["frame_lines"].as_u64().unwrap_or(480) as u32,
            look: json["render"]["look"].as_str().unwrap_or("clean").to_string(),
            player: json["player"]["enabled"].as_bool().unwrap_or(true),
            flashlight: json["player"]["flashlight"].as_bool().unwrap_or(false),
            root: root.to_path_buf(), assets, json,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Camera { pub pos: Vec3, pub yaw: f32, pub pitch: f32, pub fov: f32 }
impl Camera {
    pub fn rotation(&self) -> Quat { Quat::from_euler(glam::EulerRot::YXZ, self.yaw.to_radians(), self.pitch.to_radians(), 0.0) }
    pub fn forward(&self) -> Vec3 { self.rotation() * Vec3::Z }
}

/// Everything a game touches.
pub struct Engine {
    pub project: Project,
    pub gfx: Gfx,
    pub world: World,
    pub scene: Scene,
    pub input: Input,
    pub audio: Sfx,
    pub text: Hud,
    pub camera: Camera,
    /// the built-in first-person character (None = the game moves the camera itself)
    pub player: Option<FirstPerson>,
    /// a light carried by the camera (F toggles it in the built-in player)
    pub flashlight: bool,
    /// extra world draws this frame (cleared after rendering)
    pub draws: Vec<DrawCmd>,
    /// first-person draws (hands, weapons) in their own pass with `overlay_fov`, over the world
    pub overlay_draws: Vec<DrawCmd>,
    pub overlay_fov: f32,
    pub post: PostParams,
    pub time: f32,
    pub log: Vec<String>,
    pub quit: bool,
    pub(crate) meshes: Vec<MeshId>,
    pub(crate) editor: Option<crate::editor::Editor>,
    shadow_slots: [(Option<usize>, f32); gfx::LAMP_SLOTS],
    pub(crate) fps: f32,
}

/// The lo-fi look (bodycam: fisheye, grain, few levels) and a clean one.
pub fn post_look(name: &str, time: f32) -> PostParams {
    match name {
        "lofi" => PostParams { grade: [0.004, 0.55, 0.9, 0.92], tint: [0.97, 0.99, 0.97, 1.0], noise: [0.045, 0.018, 0.20, 1.25], misc: [1.0, 63.0, 0.22, 0.75], misc2: [0.0, time, 0.95, 0.10] },
        _ => PostParams { grade: [0.0, 1.0, 1.0, 1.0], tint: [1.0, 1.0, 1.0, 1.0], noise: [0.0, 0.0, 0.0, 0.35], misc: [1.0, 255.0, 0.0, 0.15], misc2: [0.0, time, 1.0, 0.08] },
    }
}

impl Engine {
    /// A line in the editor console and on stderr.
    pub fn log(&mut self, s: impl Into<String>) { let s = s.into(); eprintln!("[4bit] {s}"); self.log.push(s); if self.log.len() > 500 { self.log.remove(0); } }
    /// A message on screen for `seconds`.
    pub fn say(&mut self, s: &str, seconds: f32) { self.text.show(s, seconds); }
    /// The scene editor is editing or paused: the game does not simulate.
    pub fn editing(&self) -> bool { self.editor.as_ref().is_some_and(|e| e.frozen()) }

    /// GPU meshes for objects added (or rebuilt) since the last frame.
    fn upload(&mut self) {
        for o in std::mem::take(&mut self.world.changed) { if o < self.meshes.len() { self.meshes[o] = self.gfx.add_mesh(&self.world.objs[o].mesh); } }
        while self.meshes.len() < self.world.objs.len() { let o = self.meshes.len(); let id = self.gfx.add_mesh(&self.world.objs[o].mesh); self.meshes.push(id); }
    }

    fn render(&mut self, capture: bool) -> Option<image::RgbaImage> {
        let cam = self.camera;
        let rot = cam.rotation();
        let (fwd, up) = (rot * Vec3::Z, rot * Vec3::Y);
        let view = Mat4::look_to_lh(cam.pos, fwd, up);
        let proj = Mat4::perspective_lh(cam.fov.to_radians(), self.gfx.aspect(), 0.03, 400.0);
        // the most relevant lamps; the last few fade with their margin to the cut, so walking never pops a lamp
        const LAMPS: usize = gfx::MAX_LIGHTS - 2;
        let score = |l: &crate::world::Light| cam.pos.distance(l.pos) - l.range;
        let mut lights: Vec<(usize, &crate::world::Light)> = self.world.lights.iter().enumerate().filter(|(_, l)| l.current > 0.001).collect();
        lights.sort_by(|a, b| score(a.1).total_cmp(&score(b.1)));
        let cut = lights.get(LAMPS).map_or(f32::INFINITY, |(_, l)| score(l));
        lights.truncate(LAMPS);
        // cached lamp shadows: a lamp keeps its slot while among the best + 3, every hand-over fades
        let wanted: Vec<usize> = lights.iter().filter(|(_, l)| l.shadows).map(|(i, _)| *i).take(gfx::LAMP_SLOTS + 3).collect();
        let dt = 1.0 / self.fps.max(1.0);
        for k in 0..gfx::LAMP_SLOTS {
            let (cur, w) = self.shadow_slots[k];
            if cur.is_some_and(|c| wanted.contains(&c)) { self.shadow_slots[k].1 = (w + dt * 3.0).min(1.0); }
            else if w > 0.0 { self.shadow_slots[k].1 = (w - dt * 3.0).max(0.0); }
            else {
                let held: Vec<usize> = self.shadow_slots.iter().filter_map(|s| s.0).collect();
                self.shadow_slots[k] = (wanted.iter().copied().take(gfx::LAMP_SLOTS).find(|i| !held.contains(i)), 0.0);
            }
        }
        let lamp_cam = |l: &crate::world::Light| Mat4::perspective_lh(135_f32.to_radians(), 1.0, 0.3, l.range.max(0.5)) * Mat4::look_to_lh(l.pos, Vec3::NEG_Y, Vec3::Z);
        let mut lamp_vp = [Mat4::IDENTITY; gfx::LAMP_SLOTS];
        let mut lamps = [None; gfx::LAMP_SLOTS];
        for (k, s) in self.shadow_slots.iter().enumerate() {
            let Some(i) = s.0 else { continue };
            let Some(l) = self.world.lights.get(i) else { continue };
            lamp_vp[k] = lamp_cam(l);
            // new key = new static cache: the lamp and where it hangs
            let q = (l.pos * 100.0).round().as_ivec3();
            lamps[k] = Some((lamp_vp[k], (i as u64) << 40 ^ (q.x as u64 & 0x3fff) << 26 ^ (q.y as u64 & 0x1fff) << 13 ^ (q.z as u64 & 0x1fff)));
        }
        let mut ls: Vec<LightIn> = lights.iter().map(|&(i, l)| {
            let slot = self.shadow_slots.iter().position(|s| s.0 == Some(i) && s.1 > 0.0);
            LightIn { pos: l.pos, range: l.range, color: l.color * l.current * ((cut - score(l)) / 3.0).clamp(0.0, 1.0), stair: l.stair,
                      slot: slot.map_or(-1, |k| k as i32), weight: slot.map_or(0.0, |k| self.shadow_slots[k].1), bounce: l.bounce, halo: 1.0 }
        }).collect();
        let editing = self.editing();
        let lit = if editing { self.editor.as_ref().is_some_and(|e| e.headlight) } else { self.flashlight };
        let (flash_pos, flash_dir) = (cam.pos + rot * Vec3::new(0.12, -0.1, 0.0), fwd);
        if lit {
            if let Some((d, n)) = self.world.raycast(flash_pos, flash_dir, 9.0) {
                let k = (1.0 - d / 9.0).powi(2);
                ls.push(LightIn { pos: flash_pos + flash_dir * d + n * 0.25, range: 4.0 + d * 0.5, color: Vec3::from_slice(&gfx::torch_colour(false)) * 0.15 * k,
                                  stair: false, slot: -1, weight: 0.0, bounce: 0.0, halo: 0.0 });
            }
        }
        let globals = gfx::globals(view, proj, cam.pos, &ls, &lamp_vp, flash_pos, flash_dir, lit, false, self.time);
        let ov_proj = Mat4::perspective_lh(self.overlay_fov.to_radians(), self.gfx.aspect(), 0.03, 3.0);
        let mut ov_globals = gfx::globals(view, ov_proj, cam.pos, &ls, &lamp_vp, flash_pos, flash_dir, lit, false, self.time);
        ov_globals.cam_pos[3] = 0.0;
        ov_globals.ambient = [0.16, 0.17, 0.15, 1.0];
        let mut draws: Vec<DrawCmd> = self.world.objs.iter().zip(&self.meshes).filter(|(o, _)| o.visible).map(|(o, &m)| DrawCmd {
            mesh: m, tex: o.mat.tex.clone(), model: o.model, color: o.mat.tint * o.color, unlit: o.mat.unlit,
            fog: if o.mat.unlit { 0.6 } else { 1.0 }, cutoff: o.cutoff, moving: o.moving,
        }).collect();
        draws.append(&mut self.draws);
        let mut overlay = std::mem::take(&mut self.overlay_draws);
        if editing { overlay.clear(); }
        let mut post = self.post;
        post.misc2[1] = self.time;
        // the editor needs a straight projection (picking, gizmo)
        if editing { post.misc[2] = 0.0; }
        self.gfx.render(&[gfx::Pass { globals, clear_color: true, draws }, gfx::Pass { globals: ov_globals, clear_color: false, draws: overlay }], &post, &lamps, capture)
    }
}

struct Runner<G: Game> {
    game: G,
    engine: Option<Engine>,
    args: Vec<String>,
    last: Instant,
    shot: Option<(PathBuf, u32)>,
}

fn arg(args: &[String], k: &str) -> Option<String> { args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned() }

/// Runs a game: opens the window, loads the project and its scene, and loops until the window closes.
pub fn run<G: Game>(game: G) {
    let args: Vec<String> = std::env::args().collect();
    let shot = arg(&args, "--shot").map(|p| (PathBuf::from(p), arg(&args, "--frames").and_then(|f| f.parse().ok()).unwrap_or(20)));
    let el = EventLoop::new().expect("event loop");
    let mut r = Runner { game, engine: None, args, last: Instant::now(), shot };
    el.run_app(&mut r).expect("run");
}

impl<G: Game> Runner<G> {
    fn grab(e: &Engine, on: bool) {
        let w = &e.gfx.window;
        if on {
            if w.set_cursor_grab(CursorGrabMode::Locked).is_err() { let _ = w.set_cursor_grab(CursorGrabMode::Confined); }
        } else { let _ = w.set_cursor_grab(CursorGrabMode::None); }
        w.set_cursor_visible(!on);
    }
    fn start(&mut self, el: &ActiveEventLoop) {
        let project = Project::find(arg(&self.args, "--project").map(PathBuf::from));
        crate::render::FRAME_LINES.store(project.frame_lines.clamp(120, 2160), std::sync::atomic::Ordering::Relaxed);
        let window = Arc::new(el.create_window(Window::default_attributes().with_title(project.name.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(project.window.0 as f64, project.window.1 as f64))).expect("window"));
        let gfx = Gfx::new(window.clone(), project.assets.clone());
        let scene_path = arg(&self.args, "--scene").map(PathBuf::from).or_else(|| std::env::var_os("FOURBIT_SCENE").map(PathBuf::from)).unwrap_or(project.start_scene.clone());
        let mut world = World::new(&project.assets);
        let mut scene = Scene::load(&scene_path);
        scene.apply(&mut world);
        let font = std::fs::read(project.assets.join("fonts/ui.ttf")).unwrap_or_else(|_| epaint_default_fonts::UBUNTU_LIGHT.to_vec());
        let spawn = scene.spawn().unwrap_or((Vec3::ZERO, 0.0));
        let mut e = Engine {
            gfx, world, scene, input: Input::default(), audio: Sfx::new(project.assets.join("audio"), self.shot.is_some() || self.args.iter().any(|a| a == "--mute")),
            text: Hud::new(&font), camera: Camera { pos: spawn.0 + Vec3::Y * 1.65, yaw: spawn.1, pitch: 0.0, fov: 70.0 },
            player: project.player.then(|| FirstPerson::new(spawn.0, spawn.1)), flashlight: project.flashlight,
            draws: vec![], overlay_draws: vec![], overlay_fov: 62.0, post: post_look(&project.look, 0.0), time: 0.0, log: vec![], quit: false,
            meshes: vec![], editor: None, shadow_slots: [(None, 0.0); gfx::LAMP_SLOTS], fps: 60.0, project,
        };
        if let Some(env) = e.scene.env { e.gfx.env = env; }
        self.game.init(&mut e);
        e.world.reindex();
        // static objects get automatic LODs; moving ones (and anything added later) plain meshes
        let statics: Vec<usize> = (0..e.world.objs.len()).filter(|&i| !e.world.objs[i].moving).collect();
        let ids = e.gfx.add_static_meshes(&statics.iter().map(|&i| &e.world.objs[i].mesh).collect::<Vec<_>>());
        e.meshes = vec![usize::MAX; e.world.objs.len()];
        for (&i, id) in statics.iter().zip(ids) { e.meshes[i] = id; }
        for i in 0..e.meshes.len() { if e.meshes[i] == usize::MAX { e.meshes[i] = e.gfx.add_mesh(&e.world.objs[i].mesh); } }
        if let Some(c) = arg(&self.args, "--camera") {
            let v: Vec<f32> = c.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() >= 3 { e.camera.pos = Vec3::new(v[0], v[1], v[2]); e.camera.yaw = *v.get(3).unwrap_or(&0.0); e.camera.pitch = *v.get(4).unwrap_or(&0.0); e.player = None; }
        }
        if self.args.iter().any(|a| a == "--editor") || std::env::var_os("FOURBIT_EDITOR").is_some() {
            let cam = (e.camera.pos, e.camera.yaw, e.camera.pitch);
            e.editor = Some(crate::editor::Editor::new(&window, &mut e.gfx, cam));
            window.set_title(&format!("{} — 4bit", e.project.name));
            let msg = format!("сцена {}: {} объектов", e.scene.path.display(), e.scene.ents.len());
            e.log(msg);
        }
        self.engine = Some(e);
    }

    fn frame(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        let wall = (now - self.last).as_secs_f32();
        self.last = now;
        let Some(e) = self.engine.as_mut() else { return };
        let dt = if self.shot.is_some() { 1.0 / 60.0 } else { wall.min(0.05) };
        e.fps += (1.0 / wall.max(1e-4) - e.fps) * 0.05;
        e.time += dt;
        let (simulate, dt) = crate::editor::before(e, dt);
        if simulate {
            if let Some(p) = &mut e.player {
                if e.input.pressed(KeyCode::KeyF) { e.flashlight = !e.flashlight; }
                p.update(&e.input, &e.world, dt);
                e.camera.pos = p.eye_pos(); e.camera.yaw = p.yaw; e.camera.pitch = p.pitch;
            }
            self.game.update(e, dt);
            e.world.update_lights(e.time, dt);
            e.audio.listener = e.camera.pos;
            e.audio.right = e.camera.rotation() * Vec3::X;
            e.audio.update(dt);
        }
        self.game.draw(e);
        if e.editor.is_some() { let w = e.gfx.window.clone(); crate::editor::after(e, &w, wall); }
        e.upload();
        e.text.update(dt);
        if e.text.redraw(e.gfx.hud_size) { e.gfx.upload_hud(&e.text.canvas); }
        let capture = self.shot.as_mut().is_some_and(|s| { s.1 = s.1.saturating_sub(1); s.1 == 0 });
        let img = e.render(capture);
        if capture {
            let (path, _) = self.shot.take().unwrap();
            match img.map(|i| i.save(&path)) { Some(Ok(())) => println!("{}", path.display()), Some(Err(err)) => eprintln!("shot {}: {err}", path.display()), None => eprintln!("shot: no image") }
            el.exit();
        }
        e.input.flush();
        if e.quit { el.exit(); }
    }
}

impl<G: Game> ApplicationHandler for Runner<G> {
    fn resumed(&mut self, el: &ActiveEventLoop) { if self.engine.is_none() { self.start(el); } }
    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(e) = self.engine.as_mut() else { return };
        if !crate::editor::event(e, &event) {
            match event {
                WindowEvent::KeyboardInput { event: ref k, .. } => if let PhysicalKey::Code(c) = k.physical_key {
                    e.input.key(c, k.state == ElementState::Pressed);
                    if c == KeyCode::Escape && k.state == ElementState::Pressed { e.input.release(); Self::grab(e, false); }
                },
                WindowEvent::MouseInput { state, button, .. } => {
                    let down = state == ElementState::Pressed;
                    match button {
                        MouseButton::Left => {
                            if down && !e.input.locked && e.player.is_some() { e.input.locked = true; Self::grab(e, true); return; }
                            e.input.lmb = down; e.input.lmb_pressed |= down; e.input.lmb_released |= !down;
                        }
                        MouseButton::Right => e.input.rmb = down,
                        _ => {}
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => e.input.wheel += match delta { winit::event::MouseScrollDelta::LineDelta(_, y) => y, winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0 },
                WindowEvent::Focused(false) => { e.input.release(); Self::grab(e, false); }
                _ => {}
            }
        }
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => e.gfx.resize(s.width, s.height),
            WindowEvent::RedrawRequested => {
                self.frame(el);
                if let Some(e) = self.engine.as_mut() {
                    e.gfx.window.request_redraw();
                    if let Some(on) = e.editor.as_mut().and_then(|ed| ed.want_grab.take()) { if on { e.input.locked = true; } else { e.input.release(); } Self::grab(e, on); }
                }
            }
            _ => {}
        }
    }
    fn device_event(&mut self, _: &ActiveEventLoop, _: winit::event::DeviceId, event: DeviceEvent) {
        let (Some(e), DeviceEvent::MouseMotion { delta }) = (self.engine.as_mut(), event) else { return };
        let d = glam::Vec2::new(delta.0 as f32, delta.1 as f32);
        if let Some(ed) = e.editor.as_mut().filter(|ed| ed.frozen() && ed.rmb) { ed.look += d; }
        if e.input.locked { e.input.mouse += d; }
    }
    fn about_to_wait(&mut self, _: &ActiveEventLoop) { if let Some(e) = &self.engine { e.gfx.window.request_redraw(); } }
}
