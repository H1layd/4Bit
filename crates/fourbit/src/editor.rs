//! The scene editor (`--editor`): the game's own renderer underneath, panels on top. Hierarchy, inspector (entity or
//! the scene's environment), console with stats; a fly camera (hold the right button + WASD/QE, wheel forward), click
//! to select, an axis gizmo to move; Play / Pause / Step / Stop; collider and lamp views; undo (Ctrl+Z).
use crate::app::Engine;
use crate::look;
use crate::scene::{Ent, Kind};
use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode { Edit, Play, Paused }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sel { Ent(usize), Light(usize), Player }

struct Drag { axis: Option<usize>, start: Vec3, grab: Vec3 }

pub struct Editor {
    pub ctx: egui::Context,
    pub state: egui_winit::State,
    pub mode: Mode,
    step: bool,
    pub cam: (Vec3, f32, f32),
    pub look: Vec2,
    pub rmb: bool,
    pub sel: Option<Sel>,
    pub show_colliders: bool, pub show_lights: bool, pub headlight: bool,
    time_scale: f32, cam_speed: f32,
    add_model: String, models: Vec<String>, filter: String,
    drag: Option<Drag>,
    pub want_grab: Option<bool>,
    vp: Mat4, eye: Vec3,
    undo: Vec<serde_json::Value>,
    /// an inspector edit in progress (one undo step per gesture, not per frame)
    editing_field: bool,
}

impl Editor {
    pub fn new(window: &winit::window::Window, gfx: &mut crate::render::Gfx, cam: (Vec3, f32, f32)) -> Editor {
        let ctx = egui::Context::default();
        look::apply(&ctx, None);
        let state = egui_winit::State::new(ctx.clone(), egui::ViewportId::ROOT, window, Some(window.scale_factor() as f32), None, None);
        gfx.ui = Some(crate::render::UiLayer { renderer: egui_wgpu::Renderer::new(&gfx.device, gfx.surface_format(), None, 1, false), jobs: vec![], delta: Default::default(), ppp: 1.0 });
        Editor { ctx, state, mode: Mode::Edit, step: false, cam, look: Vec2::ZERO, rmb: false, sel: None, show_colliders: false, show_lights: true, headlight: true,
                 time_scale: 1.0, cam_speed: 5.0, add_model: String::new(), models: vec![], filter: String::new(), drag: None, want_grab: None,
                 vp: Mat4::IDENTITY, eye: Vec3::ZERO, undo: vec![], editing_field: false }
    }
    pub fn frozen(&self) -> bool { self.mode != Mode::Play }
    pub fn wants_pointer(&self) -> bool { self.ctx.wants_pointer_input() || self.ctx.is_pointer_over_area() }
    pub fn wants_keyboard(&self) -> bool { self.ctx.wants_keyboard_input() }
    fn cam_rot(&self) -> Quat { Quat::from_euler(glam::EulerRot::YXZ, self.cam.1.to_radians(), self.cam.2.to_radians(), 0.0) }
}

/// Window events: the panels first; while editing the right button flies and the cursor is never grabbed.
/// Returns true when the event is handled here (the game must not see it).
pub fn event(e: &mut Engine, event: &WindowEvent) -> bool {
    let Some(ed) = e.editor.as_mut() else { return false };
    let window = e.gfx.window.clone();
    let eaten = ed.state.on_window_event(&window, event).consumed;
    let input = matches!(event, WindowEvent::KeyboardInput { .. } | WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. });
    if input && !e.input.locked && (eaten || ed.wants_pointer() && !matches!(event, WindowEvent::KeyboardInput { .. })) {
        if let WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Right, .. } = event { ed.rmb = false; }
        return true;
    }
    if ed.frozen() {
        match event {
            WindowEvent::MouseInput { state, button, .. } => {
                let down = *state == ElementState::Pressed;
                match button { MouseButton::Right => ed.rmb = down, MouseButton::Left => { e.input.lmb = down; e.input.lmb_pressed |= down; e.input.lmb_released |= !down; } _ => {} }
                return true;
            }
            WindowEvent::KeyboardInput { event: k, .. } => { if let PhysicalKey::Code(c) = k.physical_key { e.input.key(c, k.state == ElementState::Pressed); } return true; }
            WindowEvent::MouseWheel { .. } => return false,
            _ => {}
        }
    }
    false
}

/// Before the game's update: (simulate this frame, dt). Edit / pause run the fly camera and the lamps only.
pub fn before(e: &mut Engine, dt: f32) -> (bool, f32) {
    let Some(ed) = e.editor.as_mut() else { return (true, dt) };
    if ed.mode == Mode::Play { return (true, dt * ed.time_scale); }
    if std::mem::take(&mut ed.step) { return (true, dt * ed.time_scale); }
    let look = std::mem::take(&mut ed.look);
    if ed.rmb {
        ed.cam.1 += look.x * 0.12;
        ed.cam.2 = (ed.cam.2 + look.y * 0.12).clamp(-89.0, 89.0);
        let r = ed.cam_rot();
        let mut v = Vec3::ZERO;
        for (k, d) in [(KeyCode::KeyW, Vec3::Z), (KeyCode::KeyS, Vec3::NEG_Z), (KeyCode::KeyD, Vec3::X), (KeyCode::KeyA, Vec3::NEG_X)] { if e.input.held(k) { v += r * d; } }
        for (k, d) in [(KeyCode::KeyE, Vec3::Y), (KeyCode::KeyQ, Vec3::NEG_Y)] { if e.input.held(k) { v += d; } }
        let fast = if e.input.held(KeyCode::ShiftLeft) { 3.0 } else { 1.0 };
        ed.cam.0 += v.normalize_or_zero() * ed.cam_speed * fast * dt;
    }
    if !ed.wants_pointer() && e.input.wheel != 0.0 { let r = ed.cam_rot(); ed.cam.0 += r * Vec3::Z * e.input.wheel * 0.6; }
    e.camera.pos = ed.cam.0; e.camera.yaw = ed.cam.1; e.camera.pitch = ed.cam.2;
    e.world.update_lights(e.time, dt);
    (false, dt)
}

// ---------------------------------------------------------------- geometry helpers
fn ray(vp: Mat4, ndc: Vec2) -> (Vec3, Vec3) {
    let inv = vp.inverse();
    let (a, b) = (inv * Vec4::new(ndc.x, ndc.y, 0.0, 1.0), inv * Vec4::new(ndc.x, ndc.y, 1.0, 1.0));
    let (a, b) = (a.truncate() / a.w, b.truncate() / b.w);
    (a, (b - a).normalize_or_zero())
}
fn project(vp: Mat4, p: Vec3, s: egui::Rect) -> Option<egui::Pos2> {
    let c = vp * p.extend(1.0);
    if c.w <= 0.01 { return None; }
    let n = c.truncate() / c.w;
    Some(egui::pos2(s.left() + (n.x * 0.5 + 0.5) * s.width(), s.top() + (0.5 - n.y * 0.5) * s.height()))
}
fn ray_sphere(o: Vec3, d: Vec3, c: Vec3, r: f32) -> Option<f32> {
    let oc = o - c; let b = oc.dot(d); let h = b * b - (oc.length_squared() - r * r);
    (h >= 0.0).then(|| -b - h.sqrt()).filter(|t| *t > 0.0)
}
fn ray_box(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let inv = Vec3::ONE / d;
    let (a, b) = ((lo - o) * inv, (hi - o) * inv);
    let (t0, t1) = (a.min(b).max_element(), a.max(b).min_element());
    (t1 >= t0.max(0.0)).then_some(t0.max(0.0))
}
fn dist_to_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let (ab, ap) = (b - a, p - a);
    let t = (ap.dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}
fn box_lines(p: &egui::Painter, vp: Mat4, s: egui::Rect, lo: Vec3, hi: Vec3, stroke: egui::Stroke) {
    let c = [Vec3::new(lo.x, lo.y, lo.z), Vec3::new(hi.x, lo.y, lo.z), Vec3::new(hi.x, lo.y, hi.z), Vec3::new(lo.x, lo.y, hi.z),
             Vec3::new(lo.x, hi.y, lo.z), Vec3::new(hi.x, hi.y, lo.z), Vec3::new(hi.x, hi.y, hi.z), Vec3::new(lo.x, hi.y, hi.z)];
    let q: Vec<Option<egui::Pos2>> = c.iter().map(|&x| project(vp, x, s)).collect();
    for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 0), (4, 5), (5, 6), (6, 7), (7, 4), (0, 4), (1, 5), (2, 6), (3, 7)] {
        if let (Some(a), Some(b)) = (q[a], q[b]) { p.line_segment([a, b], stroke); }
    }
}
fn ent_bounds(e: &Engine, ent: &Ent) -> (Vec3, Vec3) {
    match &ent.kind {
        Kind::Box { size, .. } => { let h = *size * ent.scale * 0.5; (ent.pos - h, ent.pos + h) }
        Kind::Spawn => (ent.pos - Vec3::new(0.3, 0.0, 0.3), ent.pos + Vec3::new(0.3, 1.8, 0.3)),
        Kind::Light { .. } => (ent.pos - Vec3::splat(0.25), ent.pos + Vec3::splat(0.25)),
        Kind::Model { .. } => {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            if let Some(p) = &ent.live.placement {
                for &o in &p.objs { let ob = &e.world.objs[o]; for v in ob.mesh.vertices.iter().step_by(7) { let q = ob.model.transform_point3(v.pos.into()); lo = lo.min(q); hi = hi.max(q); } }
            }
            if lo.x <= hi.x { (lo, hi) } else { (ent.pos - Vec3::splat(0.3), ent.pos + Vec3::splat(0.3)) }
        }
    }
}
fn sel_pos(e: &Engine, s: Sel) -> Option<Vec3> {
    match s { Sel::Ent(i) => e.scene.ents.get(i).map(|x| x.pos), Sel::Light(i) => e.world.lights.get(i).map(|l| l.pos), Sel::Player => e.player.as_ref().map(|p| p.pos) }
}
fn set_sel_pos(e: &mut Engine, s: Sel, p: Vec3) {
    match s {
        Sel::Ent(i) => { e.scene.ents[i].pos = p; e.scene.sync(&mut e.world, i); }
        Sel::Light(i) => e.world.lights[i].pos = p,
        Sel::Player => if let Some(pl) = &mut e.player { pl.pos = p; },
    }
}
fn scene_light_ids(e: &Engine) -> Vec<usize> { e.scene.ents.iter().filter_map(|x| x.live.light).collect() }

// ---------------------------------------------------------------- actions (also used by tools)
/// Remembers the scene for Ctrl+Z.
pub fn snapshot(e: &mut Engine) {
    let v = e.scene.to_json();
    if let Some(ed) = e.editor.as_mut() { ed.undo.push(v); if ed.undo.len() > 60 { ed.undo.remove(0); } }
}
/// Back to the last snapshot: every entity leaves the world and the snapshot's come back.
pub fn undo(e: &mut Engine) {
    let Some(v) = e.editor.as_mut().and_then(|ed| ed.undo.pop()) else { return };
    while !e.scene.ents.is_empty() { e.scene.remove(&mut e.world, 0); }
    let mut s = crate::scene::Scene::default();
    s.read(&v);
    e.scene.ents = s.ents;
    e.scene.apply(&mut e.world);
    if let Some(env) = s.env { e.scene.env = Some(env); e.gfx.env = env; }
    e.scene.dirty = true;
    if let Some(ed) = e.editor.as_mut() { ed.sel = None; }
    e.log("отменено");
}
/// Adds an entity 3 m in front of the editor camera (on the floor for models and boxes) and selects it.
pub fn add(e: &mut Engine, kind: Kind, name: String) -> usize {
    snapshot(e);
    let (c, r) = e.editor.as_ref().map_or((e.camera.pos, e.camera.rotation()), |ed| (ed.cam.0, ed.cam_rot()));
    let fwd = r * Vec3::Z;
    let mut at = c + fwd * 3.0;
    if let Some((t, _)) = e.world.raycast(c, fwd, 30.0) { at = c + fwd * (t - 0.3).max(0.5); }
    if !matches!(kind, Kind::Light { .. }) { if let Some(y) = e.world.support_height(at + Vec3::Y * 0.5, 6.0) { at.y = y; } }
    if let Kind::Box { size, .. } = &kind { at.y += size.y * 0.5; }
    e.scene.ents.push(Ent::new(name.clone(), kind, at));
    let i = e.scene.ents.len() - 1;
    e.scene.place(&mut e.world, i);
    e.scene.dirty = true;
    if let Some(ed) = e.editor.as_mut() { ed.sel = Some(Sel::Ent(i)); }
    e.log(format!("добавлен {name} в {:.2} {:.2} {:.2}", at.x, at.y, at.z));
    i
}
pub fn save(e: &mut Engine) {
    if let Some(env) = &e.scene.env { let _ = env; }
    let r = e.scene.save();
    let path = e.scene.path.display().to_string();
    match r { Ok(()) => e.log(format!("сцена сохранена: {path}")), Err(err) => e.log(format!("не сохранилось: {err}")) }
}

// ---------------------------------------------------------------- frame
pub fn after(e: &mut Engine, window: &winit::window::Window, _wall: f32) {
    let Some(ed) = e.editor.as_mut() else { return };
    if ed.models.is_empty() { ed.models = e.world.model_names(); ed.add_model = ed.models.first().cloned().unwrap_or_default(); }
    let proj = Mat4::perspective_lh(e.camera.fov.to_radians(), e.gfx.aspect(), 0.03, 400.0);
    let r = e.camera.rotation();
    ed.vp = proj * Mat4::look_to_lh(e.camera.pos, r * Vec3::Z, r * Vec3::Y);
    ed.eye = e.camera.pos;
    if !ed.wants_keyboard() && ed.frozen() {
        let ctrl = e.input.held(KeyCode::SuperLeft) || e.input.held(KeyCode::ControlLeft);
        if ctrl && e.input.pressed(KeyCode::KeyS) { save(e); }
        if ctrl && e.input.pressed(KeyCode::KeyZ) { undo(e); }
        if ctrl && e.input.pressed(KeyCode::KeyD) { if let Some(Sel::Ent(i)) = e.editor.as_ref().unwrap().sel { duplicate(e, i); } }
        if e.input.pressed(KeyCode::Delete) || e.input.pressed(KeyCode::Backspace) {
            if let Some(Sel::Ent(i)) = e.editor.as_ref().unwrap().sel { snapshot(e); let x = e.scene.remove(&mut e.world, i); e.editor.as_mut().unwrap().sel = None; e.log(format!("удалён {}", x.name)); }
        }
        if e.input.pressed(KeyCode::KeyF) && !ctrl {
            let s = e.editor.as_ref().unwrap().sel;
            if let Some(p) = s.and_then(|s| sel_pos(e, s)) { let ed = e.editor.as_mut().unwrap(); ed.cam.0 = p - ed.cam_rot() * Vec3::Z * 3.0 + Vec3::Y * 0.5; }
        }
    }
    let ed = e.editor.as_mut().unwrap();
    let raw = ed.state.take_egui_input(window);
    let ctx = ed.ctx.clone();
    let full = ctx.run(raw, |ctx| panels(e, ctx));
    let ed = e.editor.as_mut().unwrap();
    ed.state.handle_platform_output(window, full.platform_output);
    let jobs = ctx.tessellate(full.shapes, full.pixels_per_point);
    if let Some(ui) = &mut e.gfx.ui {
        ui.jobs = jobs; ui.ppp = full.pixels_per_point;
        ui.delta.set.extend(full.textures_delta.set); ui.delta.free.extend(full.textures_delta.free);
    }
}

fn duplicate(e: &mut Engine, i: usize) {
    let src = e.scene.ents[i].clone();
    let j = add(e, src.kind.clone(), format!("{} копия", src.name));
    let x = &mut e.scene.ents[j]; x.pos = src.pos + Vec3::X * 0.5; x.yaw = src.yaw; x.scale = src.scale;
    e.scene.sync(&mut e.world, j);
}

fn panels(e: &mut Engine, ctx: &egui::Context) {
    egui::TopBottomPanel::top("ed_top").exact_height(44.0).frame(egui::Frame::new()).show(ctx, |ui| {
        let r = ui.max_rect();
        look::gradient(ui.painter(), r, look::BAR_TOP, look::BAR_BOTTOM);
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, look::BAR_EDGE));
        toolbar(e, ui);
    });
    egui::TopBottomPanel::bottom("ed_console").exact_height(150.0).frame(egui::Frame::new().fill(look::PAGE).inner_margin(egui::Margin::same(6))).show(ctx, |ui| console(e, ui));
    egui::SidePanel::left("ed_tree").resizable(false).exact_width(250.0).frame(egui::Frame::new().fill(look::PAGE).inner_margin(egui::Margin::same(8))).show(ctx, |ui| hierarchy(e, ui));
    egui::SidePanel::right("ed_inspector").resizable(false).exact_width(300.0).frame(egui::Frame::new().fill(look::PAGE).inner_margin(egui::Margin::same(8))).show(ctx, |ui| inspector(e, ui));
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| viewport(e, ui, ctx.screen_rect()));
}

fn toolbar(e: &mut Engine, ui: &mut egui::Ui) {
    let dirty = e.scene.dirty;
    let (mut do_save, mut do_undo) = (false, false);
    let mut msg = None;
    let has_undo = !e.editor.as_ref().unwrap().undo.is_empty();
    let ed = e.editor.as_mut().unwrap();
    ui.horizontal_centered(|ui| {
        ui.add_space(8.0);
        look::logo(ui, 24.0);
        ui.add_space(10.0);
        match ed.mode {
            Mode::Edit => if look::shiny(ui, "▶ Играть", look::GO, true).clicked() { ed.mode = Mode::Play; ed.want_grab = Some(true); msg = Some("игра: старт"); },
            _ => if look::shiny(ui, "■ Стоп", egui::Color32::from_rgb(200, 50, 40), true).clicked() { ed.mode = Mode::Edit; ed.want_grab = Some(false); msg = Some("игра: стоп"); },
        }
        match ed.mode {
            Mode::Play => if ui.button("Пауза").clicked() { ed.mode = Mode::Paused; ed.want_grab = Some(false); },
            Mode::Paused => if ui.button("Дальше").clicked() { ed.mode = Mode::Play; ed.want_grab = Some(true); },
            Mode::Edit => {}
        }
        if ed.mode != Mode::Play && ui.button("Шаг").on_hover_text("один кадр игры").clicked() { ed.step = true; }
        if ui.add_enabled(has_undo, egui::Button::new("Отменить")).on_hover_text("Ctrl+Z").clicked() { do_undo = true; }
        ui.separator();
        ui.label("время"); ui.add(egui::Slider::new(&mut ed.time_scale, 0.0..=2.0).max_decimals(2));
        ui.separator();
        ui.checkbox(&mut ed.show_colliders, "коллайдеры");
        ui.checkbox(&mut ed.show_lights, "лампы");
        ui.checkbox(&mut ed.headlight, "свет камеры");
        ui.label("камера"); ui.add(egui::Slider::new(&mut ed.cam_speed, 1.0..=30.0).max_decimals(0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if look::shiny(ui, if dirty { "Сохранить *" } else { "Сохранить" }, egui::Color32::from_rgb(40, 110, 210), true).clicked() { do_save = true; }
        });
    });
    if let Some(m) = msg { e.log(m); }
    if do_save { save(e); }
    if do_undo { undo(e); }
}

fn console(e: &mut Engine, ui: &mut egui::Ui) {
    let (calls, tris) = e.gfx.last_drawn;
    let mode = e.editor.as_ref().unwrap().mode;
    ui.horizontal(|ui| {
        look::heading(ui, "Консоль");
        ui.label(egui::RichText::new(format!("{:.0} fps · {} вызовов · {} тыс. треуг. · объектов {} · коллайдеров {} · ламп {} · {:?}",
            e.fps, calls, tris / 1000, e.world.objs.len(), e.world.colliders.len(), e.world.lights.len(), mode)).small().color(look::GREY));
    });
    egui::Frame::new().fill(egui::Color32::WHITE).stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(170))).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for l in &e.log { ui.label(egui::RichText::new(l).monospace().size(11.0)); }
        });
    });
}

fn hierarchy(e: &mut Engine, ui: &mut egui::Ui) {
    look::heading(ui, &format!("Сцена: {}", e.scene.path.file_name().map_or("—".into(), |n| n.to_string_lossy().into_owned())));
    let mut add_kind: Option<(Kind, String)> = None;
    {
        let ed = e.editor.as_mut().unwrap();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("add_model").selected_text(&ed.add_model).width(150.0).height(400.0).show_ui(ui, |ui| {
                for n in &ed.models { ui.selectable_value(&mut ed.add_model, n.clone(), n.as_str()); }
            });
            if ui.add_enabled(!ed.add_model.is_empty(), egui::Button::new("+ модель")).clicked() {
                add_kind = Some((Kind::Model { model: ed.add_model.clone(), solid: true }, ed.add_model.rsplit('/').next().unwrap_or("").to_string()));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("+ лампа").clicked() { add_kind = Some((Kind::Light { color: Vec3::new(1.0, 0.85, 0.6), intensity: 1.5, range: 6.0, flicker: 0.0, shadows: true }, "лампа".into())); }
            if ui.button("+ коробка").clicked() { add_kind = Some((Kind::Box { size: Vec3::ONE, texture: String::new(), tint: Vec3::splat(0.6), tile: 1.0 }, "коробка".into())); }
            if ui.button("+ старт").clicked() { add_kind = Some((Kind::Spawn, "старт".into())); }
        });
    }
    if let Some((k, n)) = add_kind { add(e, k, n); }
    ui.add_space(4.0);
    let sel = e.editor.as_ref().unwrap().sel;
    let mut pick = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        if e.player.is_some() && look::link(ui, "Игрок", sel == Some(Sel::Player)).clicked() { pick = Some(Sel::Player); }
        for (i, x) in e.scene.ents.iter().enumerate() {
            let icon = match x.kind { Kind::Light { .. } => "☀", Kind::Model { .. } => "■", Kind::Box { .. } => "□", Kind::Spawn => "▲" };
            if look::link(ui, &format!("{icon} {}", x.name), sel == Some(Sel::Ent(i))).clicked() { pick = Some(Sel::Ent(i)); }
        }
        let owned = scene_light_ids(e);
        let code: Vec<usize> = (0..e.world.lights.len()).filter(|i| !owned.contains(i)).collect();
        if !code.is_empty() {
            ui.add_space(6.0);
            let ed = e.editor.as_mut().unwrap();
            egui::CollapsingHeader::new(egui::RichText::new(format!("Лампы из кода ({})", code.len())).color(look::MAROON).strong()).show(ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut ed.filter).hint_text(egui::RichText::new("номер").color(look::GREY)).desired_width(80.0));
                for i in code {
                    let n = format!("лампа #{i}");
                    if !ed.filter.is_empty() && !n.contains(&ed.filter) { continue; }
                    let d = e.world.lights[i].pos.distance(ed.cam.0);
                    if look::link(ui, &format!("{n}  ({d:.0} м)"), sel == Some(Sel::Light(i))).clicked() { pick = Some(Sel::Light(i)); }
                }
            });
        }
    });
    if let Some(p) = pick { e.editor.as_mut().unwrap().sel = Some(p); }
}

fn v3(ui: &mut egui::Ui, label: &str, v: &mut Vec3, speed: f64) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| { ui.label(label); for (c, k) in [("x", 0), ("y", 1), ("z", 2)] { ch |= ui.add(egui::DragValue::new(&mut v[k]).speed(speed).max_decimals(3).prefix(format!("{c} "))).changed(); } });
    ch
}
fn colour(ui: &mut egui::Ui, label: &str, v: &mut Vec3) -> bool {
    let mut c = [v.x.min(1.0), v.y.min(1.0), v.z.min(1.0)];
    let mut ch = false;
    ui.horizontal(|ui| { ui.label(label); if ui.color_edit_button_rgb(&mut c).changed() { *v = Vec3::from(c); ch = true; } });
    ch
}

fn inspector(e: &mut Engine, ui: &mut egui::Ui) {
    look::heading(ui, "Инспектор");
    let sel = e.editor.as_ref().unwrap().sel;
    let mut changed = false;
    let mut actions: (bool, bool) = (false, false);
    look::post().show(ui, |ui| {
        ui.set_width(ui.available_width());
        match sel {
            Some(Sel::Ent(i)) if i < e.scene.ents.len() => {
                let x = &mut e.scene.ents[i];
                ui.horizontal(|ui| { ui.label("имя"); changed |= ui.text_edit_singleline(&mut x.name).changed(); });
                ui.label(egui::RichText::new(x.kind.label()).small().color(look::GREY));
                changed |= v3(ui, "позиция", &mut x.pos, 0.02);
                if !matches!(x.kind, Kind::Light { .. }) { changed |= ui.add(egui::Slider::new(&mut x.yaw, -180.0..=180.0).text("поворот °")).changed(); }
                match &mut x.kind {
                    Kind::Model { model, solid } => {
                        ui.label(egui::RichText::new(format!("модель: {model}")).small());
                        changed |= ui.add(egui::DragValue::new(&mut x.scale).speed(0.01).range(0.05..=50.0).prefix("масштаб ")).changed();
                        ui.label(if *solid { "твёрдая (коллайдер)" } else { "без коллайдера" });
                    }
                    Kind::Light { color, intensity, range, flicker, shadows } => {
                        changed |= colour(ui, "цвет", color);
                        changed |= ui.add(egui::Slider::new(intensity, 0.0..=8.0).text("яркость")).changed();
                        changed |= ui.add(egui::Slider::new(range, 0.5..=40.0).text("радиус, м")).changed();
                        changed |= ui.add(egui::Slider::new(flicker, 0.0..=1.0).text("мерцание")).changed();
                        changed |= ui.checkbox(shadows, "тени").changed();
                    }
                    Kind::Box { size, texture, tint, tile } => {
                        changed |= v3(ui, "размер", size, 0.02);
                        ui.horizontal(|ui| { ui.label("текстура"); changed |= ui.text_edit_singleline(texture).changed(); });
                        changed |= colour(ui, "цвет", tint);
                        changed |= ui.add(egui::DragValue::new(tile).speed(0.05).range(0.05..=50.0).prefix("плитка, м ")).changed();
                    }
                    Kind::Spawn => { ui.label(egui::RichText::new("здесь появляется игрок").small()); }
                }
                ui.horizontal(|ui| { actions.0 = ui.button("Дублировать").on_hover_text("Ctrl+D").clicked(); actions.1 = ui.button("Удалить").on_hover_text("Del").clicked(); });
            }
            Some(Sel::Light(i)) if i < e.world.lights.len() => {
                ui.label(egui::RichText::new(format!("лампа #{i} из кода игры: правки живые, но в сцену не сохраняются")).small());
                let l = &mut e.world.lights[i];
                v3(ui, "позиция", &mut l.pos, 0.02);
                colour(ui, "цвет", &mut l.color);
                ui.add(egui::Slider::new(&mut l.intensity, 0.0..=8.0).text("яркость"));
                ui.add(egui::Slider::new(&mut l.range, 0.5..=40.0).text("радиус, м"));
                ui.label(egui::RichText::new(format!("сейчас {:.2}", l.current)).small().color(look::GREY));
            }
            Some(Sel::Player) => if let Some(p) = &mut e.player {
                v3(ui, "ноги", &mut p.pos, 0.02);
                ui.add(egui::Slider::new(&mut p.speed, 0.5..=10.0).text("шаг, м/с"));
                ui.add(egui::Slider::new(&mut p.run, 1.0..=20.0).text("бег, м/с"));
                ui.add(egui::Slider::new(&mut p.jump, 0.0..=12.0).text("прыжок"));
                ui.checkbox(&mut p.fly, "полёт (ноклип)");
            },
            _ => {
                // nothing selected: the scene's environment
                ui.label(egui::RichText::new("Окружение сцены").strong().color(look::MAROON));
                let mut env = e.scene.env.unwrap_or(e.gfx.env);
                let mut ch = false;
                ch |= colour(ui, "небо", &mut env.sky_ambient);
                ch |= colour(ui, "солнце/луна", &mut env.key_colour);
                ch |= v3(ui, "направление", &mut env.key_dir, 0.01);
                ch |= colour(ui, "внутри", &mut env.indoor_ambient);
                ch |= colour(ui, "туман", &mut env.fog_outdoor);
                ch |= ui.add(egui::Slider::new(&mut env.fog_outdoor_range.0, 0.0..=200.0).text("туман от, м")).changed();
                ch |= ui.add(egui::Slider::new(&mut env.fog_outdoor_range.1, 1.0..=600.0).text("туман до, м")).changed();
                ch |= ui.add(egui::Slider::new(&mut env.storey, 0.0..=6.0).text("высота этажа")).on_hover_text("0 = нет этажей; иначе лампы не светят сквозь перекрытия").changed();
                if ch { e.scene.env = Some(env); e.gfx.env = env; e.scene.dirty = true; }
                ui.add_space(4.0);
                ui.label(egui::RichText::new("клик по объекту — выбрать").small().color(look::GREY));
            }
        }
    });
    // one undo step per gesture: taken when an edit starts, not every frame of a drag
    let ed = e.editor.as_mut().unwrap();
    let gesture = ui.ctx().input(|i| i.pointer.any_down() || i.keys_down.len() > 0);
    if changed && !ed.editing_field { ed.editing_field = true; let mut before = e.scene.to_json(); let _ = &mut before; snapshot(e); }
    let ed = e.editor.as_mut().unwrap();
    if !gesture { ed.editing_field = false; }
    if changed { if let Some(Sel::Ent(i)) = sel { e.scene.sync(&mut e.world, i); } }
    if actions.0 { if let Some(Sel::Ent(i)) = sel { duplicate(e, i); } }
    if actions.1 { if let Some(Sel::Ent(i)) = sel { snapshot(e); let x = e.scene.remove(&mut e.world, i); e.editor.as_mut().unwrap().sel = None; e.log(format!("удалён {}", x.name)); } }
    ui.add_space(6.0);
    ui.label(egui::RichText::new("ПКМ + WASD/QE — лететь, Shift — быстрее, колесо — вперёд; клик — выбрать; стрелки гизмо — двигать (Ctrl — шаг 0.25 м); F — к объекту; Ctrl+D — копия; Del — удалить; Ctrl+Z — отменить; Ctrl+S — сохранить").small().color(look::GREY));
}

fn viewport(e: &mut Engine, ui: &mut egui::Ui, screen: egui::Rect) {
    let rect = ui.max_rect();
    let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    let p = ui.painter_at(rect);
    if !e.editor.as_ref().unwrap().frozen() {
        if resp.clicked() && !e.input.locked { e.editor.as_mut().unwrap().want_grab = Some(true); }
        return;
    }
    let ed = e.editor.as_ref().unwrap();
    let (vp, eye, sel) = (ed.vp, ed.eye, ed.sel);
    if ed.show_colliders {
        for &i in e.world.near(eye - Vec3::splat(25.0), eye + Vec3::splat(25.0)).iter().take(3000) {
            let b = e.world.colliders[i];
            if b.min.x < -1e4 { continue; }
            box_lines(&p, vp, screen, b.min, b.max, egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(80, 255, 120, 110)));
        }
    }
    if ed.show_lights {
        for (i, l) in e.world.lights.iter().enumerate() {
            if l.pos.distance(eye) > 80.0 { continue; }
            let Some(s) = project(vp, l.pos, screen) else { continue };
            let col = egui::Color32::from_rgb((l.color.x.min(1.0) * 255.0) as u8, (l.color.y.min(1.0) * 255.0) as u8, (l.color.z.min(1.0) * 255.0) as u8);
            let on = sel == Some(Sel::Light(i)) || e.scene.ents.iter().enumerate().any(|(k, x)| x.live.light == Some(i) && sel == Some(Sel::Ent(k)));
            p.circle(s, if on { 7.0 } else { 5.0 }, col, egui::Stroke::new(1.0, egui::Color32::BLACK));
            if on { if let Some(q) = project(vp, l.pos + (e.camera.rotation() * Vec3::X) * l.range, screen) { p.circle_stroke(s, (q - s).length(), egui::Stroke::new(1.0, col)); } }
        }
    }
    for x in &e.scene.ents { if x.kind == Kind::Spawn { if let Some(s) = project(vp, x.pos, screen) { p.text(s, egui::Align2::CENTER_BOTTOM, "▲ старт", egui::FontId::proportional(12.0), egui::Color32::from_rgb(255, 210, 60)); } } }
    if let Some(Sel::Ent(i)) = sel { if let Some(x) = e.scene.ents.get(i) { let (lo, hi) = ent_bounds(e, x); box_lines(&p, vp, screen, lo, hi, egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 210, 60))); } }
    // gizmo
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let mut hover_axis = None;
    if let Some(at) = sel.and_then(|s| sel_pos(e, s)) {
        let len = (at.distance(eye) * 0.15).max(0.05);
        let axes = [(Vec3::X, egui::Color32::from_rgb(230, 50, 50)), (Vec3::Y, egui::Color32::from_rgb(40, 190, 60)), (Vec3::Z, egui::Color32::from_rgb(50, 90, 240))];
        if let Some(o) = project(vp, at, screen) {
            for (k, (a, col)) in axes.iter().enumerate() {
                let Some(q) = project(vp, at + *a * len, screen) else { continue };
                let hot = pointer.is_some_and(|m| dist_to_segment(m, o, q) < 7.0) || e.editor.as_ref().unwrap().drag.as_ref().is_some_and(|d| d.axis == Some(k));
                if hot && hover_axis.is_none() { hover_axis = Some(k); }
                p.line_segment([o, q], egui::Stroke::new(if hot { 4.0 } else { 2.5 }, *col));
                p.circle_filled(q, 4.5, *col);
            }
            p.rect_filled(egui::Rect::from_center_size(o, egui::vec2(9.0, 9.0)), 1.0, egui::Color32::from_rgb(255, 210, 60));
        }
        if resp.drag_started() {
            if let Some(m) = pointer {
                let near_centre = project(vp, at, screen).is_some_and(|o| (o - m).length() < 9.0);
                if hover_axis.is_some() || near_centre { snapshot(e); e.editor.as_mut().unwrap().drag = Some(Drag { axis: if near_centre { None } else { hover_axis }, start: at, grab: Vec3::ZERO }); }
            }
        }
        let ed = e.editor.as_mut().unwrap();
        if let (Some(d), Some(m)) = (&ed.drag, pointer) {
            let ndc = Vec2::new((m.x - screen.left()) / screen.width() * 2.0 - 1.0, 1.0 - (m.y - screen.top()) / screen.height() * 2.0);
            let (ro, rd) = ray(vp, ndc);
            let target = match d.axis {
                Some(k) => {
                    // the point on the axis closest to the mouse ray
                    let a = [Vec3::X, Vec3::Y, Vec3::Z][k];
                    let w = d.start - ro;
                    let (b, dd, ee) = (a.dot(rd), a.dot(w), rd.dot(w));
                    let den = 1.0 - b * b;
                    (den.abs() > 1e-4).then(|| d.start + a * ((b * ee - dd) / den))
                }
                None => (rd.y.abs() > 1e-4).then(|| ro + rd * ((d.start.y - ro.y) / rd.y)),
            };
            if let Some(mut t) = target {
                let mut d = ed.drag.take().unwrap();
                if d.grab == Vec3::ZERO { d.grab = t - d.start; }
                t -= d.grab;
                if e.input.held(KeyCode::ControlLeft) || e.input.held(KeyCode::SuperLeft) { t = (t / 0.25).round() * 0.25; }
                ed.drag = Some(d);
                set_sel_pos(e, sel.unwrap(), t);
            }
        }
    }
    if resp.drag_stopped() { if let Some(d) = e.editor.as_mut().unwrap().drag.take() { if let Some(now) = sel.and_then(|s| sel_pos(e, s)) { e.log(format!("перемещено: {:.2} {:.2} {:.2} → {:.2} {:.2} {:.2}", d.start.x, d.start.y, d.start.z, now.x, now.y, now.z)); } } }
    let show_lights = e.editor.as_ref().unwrap().show_lights;
    if resp.clicked() && hover_axis.is_none() {
        if let Some(m) = pointer {
            let ndc = Vec2::new((m.x - screen.left()) / screen.width() * 2.0 - 1.0, 1.0 - (m.y - screen.top()) / screen.height() * 2.0);
            let (ro, rd) = ray(vp, ndc);
            let mut best: Option<(f32, Sel)> = None;
            let mut consider = |t: Option<f32>, s: Sel| if let Some(t) = t { if best.is_none_or(|b| t < b.0) { best = Some((t, s)); } };
            for (i, x) in e.scene.ents.iter().enumerate() {
                let (lo, hi) = ent_bounds(e, x);
                consider(if matches!(x.kind, Kind::Light { .. }) { ray_sphere(ro, rd, x.pos, 0.3) } else { ray_box(ro, rd, lo, hi) }, Sel::Ent(i));
            }
            let owned = scene_light_ids(e);
            if show_lights { for (i, l) in e.world.lights.iter().enumerate() { if !owned.contains(&i) { consider(ray_sphere(ro, rd, l.pos, 0.25), Sel::Light(i)); } } }
            if let Some(pl) = &e.player { consider(ray_sphere(ro, rd, pl.pos + Vec3::Y * 0.9, 0.45), Sel::Player); }
            // the level stands in front of what is behind it (boxes the scene owns are picked as entities)
            let wall = e.world.raycast(ro, rd, 400.0).map(|h| h.0).unwrap_or(f32::MAX);
            e.editor.as_mut().unwrap().sel = best.filter(|b| b.0 <= wall + 0.3).map(|b| b.1);
        }
    }
    let mode = e.editor.as_ref().unwrap().mode;
    p.text(rect.left_top() + egui::vec2(8.0, 6.0), egui::Align2::LEFT_TOP, match mode { Mode::Edit => "Сцена", Mode::Paused => "Пауза", Mode::Play => "Игра" }, egui::FontId::proportional(13.0), egui::Color32::from_white_alpha(200));
}
