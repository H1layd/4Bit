//! The 4bit look: mid-2000s web. Cream "Yotsuba" pages and post boxes, maroon headings and green names from the
//! imageboards, the four-colour serif logo and plain blue links of the old search page, Web 2.0 glossy bars,
//! rounded shiny buttons and a "beta" badge.
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use std::sync::Arc;

pub const PAGE: Color32 = Color32::from_rgb(255, 255, 238);
pub const POST: Color32 = Color32::from_rgb(240, 224, 214);
pub const POST_EDGE: Color32 = Color32::from_rgb(217, 191, 183);
pub const MAROON: Color32 = Color32::from_rgb(128, 0, 0);
pub const NAME: Color32 = Color32::from_rgb(17, 119, 67);
pub const LINK: Color32 = Color32::from_rgb(0, 0, 238);
pub const TEXT: Color32 = Color32::from_rgb(0, 0, 0);
pub const GREY: Color32 = Color32::from_rgb(119, 119, 119);
pub const MARK: Color32 = Color32::from_rgb(255, 255, 153);
pub const BAR_TOP: Color32 = Color32::from_rgb(255, 255, 255);
pub const BAR_BOTTOM: Color32 = Color32::from_rgb(214, 226, 242);
pub const BAR_EDGE: Color32 = Color32::from_rgb(165, 187, 218);
pub const GO: Color32 = Color32::from_rgb(98, 178, 58);
const LOGO: [Color32; 4] = [Color32::from_rgb(51, 105, 232), Color32::from_rgb(213, 15, 37), Color32::from_rgb(238, 178, 17), Color32::from_rgb(0, 153, 37)];

pub fn apply(ctx: &egui::Context, font: Option<Vec<u8>>) {
    let mut fonts = egui::FontDefinitions::default();
    // Arial first: the period's web font, and full Cyrillic
    if let Some(b) = font {
        fonts.font_data.insert("arial".into(), Arc::new(egui::FontData::from_owned(b)));
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] { fonts.families.entry(fam).or_default().insert(0, "arial".into()); }
    }
    ctx.set_fonts(fonts);
    let mut v = egui::Visuals::light();
    v.panel_fill = PAGE;
    v.window_fill = PAGE;
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = POST;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = LINK;
    // selection and the filled part of sliders: a Web 2.0 blue (the yellow mark is drawn by `link` itself)
    v.selection.bg_fill = Color32::from_rgb(120, 160, 230);
    v.slider_trailing_fill = true;
    v.selection.stroke = Stroke::new(1.0, MAROON);
    let r = egui::CornerRadius::same(4);
    for (w, fill, edge) in [(&mut v.widgets.inactive, Color32::from_rgb(241, 241, 241), Color32::from_rgb(168, 168, 168)),
                            (&mut v.widgets.hovered, Color32::from_rgb(250, 250, 250), Color32::from_rgb(120, 140, 180)),
                            (&mut v.widgets.active, Color32::from_rgb(222, 228, 240), Color32::from_rgb(90, 110, 160))] {
        w.bg_fill = fill; w.weak_bg_fill = fill; w.bg_stroke = Stroke::new(1.0, edge); w.corner_radius = r;
        w.fg_stroke = Stroke::new(1.0, TEXT);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, POST_EDGE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(6.0, 4.0);
        s.spacing.button_padding = Vec2::new(8.0, 3.0);
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
        s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(11.0));
    });
}

/// Vertical gradient, the Web 2.0 gloss.
pub fn gradient(p: &egui::Painter, r: Rect, top: Color32, bottom: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(r.left_top(), top); m.colored_vertex(r.right_top(), top);
    m.colored_vertex(r.right_bottom(), bottom); m.colored_vertex(r.left_bottom(), bottom);
    m.add_triangle(0, 1, 2); m.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(m));
}

/// "4bit" in the four search-page colours, serif-heavy, with a soft drop shadow, and a red "beta" badge.
pub fn logo(ui: &mut egui::Ui, size: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(size * 2.55, size * 1.15), egui::Sense::hover());
    let p = ui.painter();
    let font = egui::FontId::proportional(size);
    let mut x = r.left();
    for (ch, col) in "4bit".chars().zip(LOGO) {
        let s = ch.to_string();
        let g = p.layout_no_wrap(s.clone(), font.clone(), col);
        let at = Pos2::new(x, r.top());
        p.text(at + Vec2::new(1.5, 1.5), egui::Align2::LEFT_TOP, &s, font.clone(), Color32::from_black_alpha(55));
        p.text(at, egui::Align2::LEFT_TOP, &s, font.clone(), col);
        x += g.size().x - size * 0.02;
    }
    let badge = Rect::from_min_size(Pos2::new(x + 2.0, r.top() + size * 0.05), Vec2::new(size * 0.95, size * 0.42));
    p.rect_filled(badge, 3.0, Color32::from_rgb(214, 36, 36));
    p.rect_filled(Rect::from_min_max(badge.min, Pos2::new(badge.max.x, badge.center().y)), 3.0, Color32::from_rgb(236, 92, 80));
    p.text(badge.center(), egui::Align2::CENTER_CENTER, "beta", egui::FontId::proportional(size * 0.3), Color32::WHITE);
}

/// A shiny pill button (Web 2.0 "sign up now" style).
pub fn shiny(ui: &mut egui::Ui, label: &str, base: Color32, enabled: bool) -> egui::Response {
    let font = egui::FontId::proportional(14.0);
    let g = ui.painter().layout_no_wrap(label.to_string(), font.clone(), Color32::WHITE);
    let size = Vec2::new(g.size().x + 26.0, 26.0);
    let (r, resp) = ui.allocate_exact_size(size, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let p = ui.painter();
    let base = if !enabled { Color32::from_gray(170) } else if resp.hovered() { lighten(base, 18) } else { base };
    p.rect_filled(r, 13.0, darken(base, 30));
    let inner = r.shrink(1.0);
    p.rect_filled(inner, 12.0, base);
    p.rect_filled(Rect::from_min_max(inner.min, Pos2::new(inner.max.x, inner.center().y)), egui::CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 }, lighten(base, 40));
    p.text(r.center() + Vec2::new(0.0, 1.0), egui::Align2::CENTER_CENTER, label, font.clone(), Color32::from_black_alpha(70));
    p.text(r.center(), egui::Align2::CENTER_CENTER, label, font, Color32::WHITE);
    resp
}

fn lighten(c: Color32, k: u8) -> Color32 { Color32::from_rgb(c.r().saturating_add(k), c.g().saturating_add(k), c.b().saturating_add(k)) }
fn darken(c: Color32, k: u8) -> Color32 { Color32::from_rgb(c.r().saturating_sub(k), c.g().saturating_sub(k), c.b().saturating_sub(k)) }

/// An imageboard post box.
pub fn post() -> egui::Frame {
    egui::Frame::new().fill(POST).stroke(Stroke::new(1.0, POST_EDGE)).inner_margin(egui::Margin::same(7)).corner_radius(0)
}

/// Maroon board-style heading.
pub fn heading(ui: &mut egui::Ui, s: &str) { ui.label(egui::RichText::new(s).color(MAROON).strong().size(15.0)); }

/// A plain old link: blue, underlined, a pale yellow mark when it is the open one.
pub fn link(ui: &mut egui::Ui, s: &str, on: bool) -> egui::Response {
    let t = egui::RichText::new(s).color(if on { TEXT } else { LINK }).underline();
    let t = if on { t.background_color(MARK).strong() } else { t };
    ui.add(egui::Label::new(t).sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Old search-page tabs: links, the open one in black bold.
pub fn tabs(ui: &mut egui::Ui, names: &[&str], at: &mut usize) {
    ui.horizontal(|ui| {
        for (i, n) in names.iter().enumerate() {
            let on = *at == i;
            let t = if on { egui::RichText::new(*n).strong().color(TEXT) } else { egui::RichText::new(*n).underline().color(LINK) };
            if ui.add(egui::Label::new(t).sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() { *at = i; }
        }
    });
}
