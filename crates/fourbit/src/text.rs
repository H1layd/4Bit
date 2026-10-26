//! Text overlay drawn on the CPU (fontdue) into an RGBA canvas. The text is faint, fades in slowly and looks
//! crunchy (low-res, unsmoothed, speckled) - static, no animated glitches. Redrawn only while it fades.
const FADE_IN: f32 = 0.9;
/// The F prompt answers a look, with its tick sound: a slow fade read as lag.
const PROMPT_FADE_IN: f32 = 0.12;
const FADE_OUT: f32 = 0.7;
const MAX_ALPHA: f32 = 0.8;
const GLITCH_HZ: f32 = 12.0;

struct Line { text: String, age: f32, left: f32 }

pub struct Hud {
    font: fontdue::Font,
    pub prompt: Option<String>,
    /// F3: frame stats along the top
    pub stats: Option<String>,
    prompt_line: Option<Line>,
    message: Option<Line>,
    time: f32,
    last: String,
    pub canvas: Vec<u8>,
    size: (u32, u32),
}

fn hash(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x9e3779b1) ^ b.wrapping_mul(0x85ebca6b);
    h ^= h >> 15; h = h.wrapping_mul(0x2c1b3c6d); h ^= h >> 13;
    (h & 0xffff) as f32 / 65535.0
}

impl Hud {
    pub fn new(font_bytes: &[u8]) -> Self {
        let font = fontdue::Font::from_bytes(font_bytes, fontdue::FontSettings::default()).expect("font");
        Hud { font, prompt: None, stats: None, prompt_line: None, message: None, time: 0.0, last: "\u{1}".into(), canvas: vec![], size: (0, 0) }
    }

    pub fn show(&mut self, text: &str, seconds: f32) {
        // the same text again keeps fading in from where it is instead of restarting
        let age = self.message.as_ref().filter(|m| m.text == text).map_or(0.0, |m| m.age);
        self.message = Some(Line { text: text.to_string(), age, left: seconds });
    }

    pub fn update(&mut self, dt: f32) {
        self.time += dt;
        if let Some(m) = &mut self.message {
            m.age += dt;
            m.left -= dt;
            if m.left <= -FADE_OUT { self.message = None; }
        }
        // the interaction prompt fades in too, and out when you look away
        match (&self.prompt, &mut self.prompt_line) {
            (Some(p), Some(l)) if &l.text == p => { l.age += dt; l.left = 1.0; }
            (Some(p), _) => self.prompt_line = Some(Line { text: p.clone(), age: 0.0, left: 1.0 }),
            (None, Some(l)) => { l.left -= dt / FADE_OUT; if l.left <= 0.0 { self.prompt_line = None; } }
            (None, None) => {}
        }
    }

    fn alpha(l: &Line, fade_out_by_left: bool) -> f32 {
        let a = (l.age / if fade_out_by_left { FADE_IN } else { PROMPT_FADE_IN }).clamp(0.0, 1.0);
        let a = a * a * (3.0 - 2.0 * a);
        let out = if fade_out_by_left { (1.0 + l.left / FADE_OUT).clamp(0.0, 1.0).min(if l.left < 0.0 { 1.0 } else { 1.0 }) } else { l.left.clamp(0.0, 1.0) };
        a * out * MAX_ALPHA
    }

    /// Redraws the canvas when the text, its fade or the glitch frame changed. Returns true when it needs uploading.
    pub fn redraw(&mut self, size: (u32, u32)) -> bool {
        let tick = (self.time * GLITCH_HZ) as u32;
        let pa = self.prompt_line.as_ref().map_or(0.0, |l| Self::alpha(l, false));
        let ma = self.message.as_ref().map_or(0.0, |l| Self::alpha(l, true));
        let key = format!("{:?}|{:?}|{:?}|{:?}|{:.2}|{:.2}", self.prompt_line.as_ref().map(|l| &l.text), self.message.as_ref().map(|m| &m.text),
            size, self.stats, pa, ma);
        if key == self.last { return false; }
        self.last = key;
        self.size = size;
        self.canvas = vec![0; (size.0 * size.1 * 4) as usize];
        let fs = size.1 as f32 / 30.0;
        if let Some(l) = &self.prompt_line {
            let t = l.text.clone();
            self.text(&t, size.1 as f32 * 0.58, fs, [220, 220, 200], pa, tick, 1);
        }
        if let Some(s) = self.stats.clone() {
            for (k, line) in s.lines().enumerate() { self.text(line, fs * (1.3 + k as f32 * 1.1), fs * 0.8, [190, 230, 170], 0.95, tick, 3); }
        }
        if let Some(m) = &self.message {
            let t = m.text.clone();
            self.text(&t, size.1 as f32 * 0.78, fs * 1.3, [235, 225, 205], ma, tick, 2);
        }
        true
    }

    /// Crunchy, not glitchy: the text is rasterised at half its size without smoothing, blown up with
    /// nearest-neighbour pixels, alpha crushed to a few levels with fixed (not moving) speckle, a dark smear under it.
    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, s: &str, y: f32, px: f32, col: [u8; 3], alpha: f32, _tick: u32, salt: u32) {
        if alpha <= 0.003 { return; }
        const SCALE: i32 = 2;
        let small = px / SCALE as f32;
        let glyphs: Vec<_> = s.chars().map(|c| self.font.rasterize(c, small)).collect();
        let width: f32 = glyphs.iter().map(|(m, _)| m.advance_width).sum();
        // low-res mask of the whole line
        let (lw, lh) = (width.ceil() as i32 + 4, (small * 1.6).ceil() as i32 + 4);
        let mut mask = vec![0f32; (lw * lh) as usize];
        let mut x = 2.0f32;
        for (m, bmp) in &glyphs {
            let ox = x as i32 + m.xmin;
            let oy = (small * 1.2) as i32 - m.height as i32 - m.ymin + 2;
            for gy in 0..m.height { for gx in 0..m.width {
                let (mx, my) = (ox + gx as i32, oy + gy as i32);
                if mx < 0 || my < 0 || mx >= lw || my >= lh { continue; }
                let v = bmp[gy * m.width + gx] as f32 / 255.0;
                let i = (my * lw + mx) as usize;
                mask[i] = mask[i].max(v);
            } }
            x += m.advance_width;
        }
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let x0 = (w - lw * SCALE) / 2;
        let y0 = y as i32;
        for my in 0..lh { for mx in 0..lw {
            let v = mask[(my * lw + mx) as usize];
            // crushed to 3 levels with a light fixed speckle: chewed edges, the strokes still read
            let n = hash((mx as u32).wrapping_add(salt * 977), my as u32);
            let q = if v + (n - 0.5) * 0.12 > 0.5 { 1.0 } else if v + (n - 0.5) * 0.12 > 0.25 { 0.6 } else { 0.0 };
            let shadow = {
                let (sx, sy) = (mx - 1, my - 1);
                if sx >= 0 && sy >= 0 { mask[(sy * lw + sx) as usize] } else { 0.0 }
            };
            if q == 0.0 && shadow < 0.4 { continue; }
            let tint = 0.82 + n * 0.18;
            for dy in 0..SCALE { for dx in 0..SCALE {
                let (pxi, pyi) = (x0 + mx * SCALE + dx, y0 + my * SCALE + dy);
                if pxi < 0 || pyi < 0 || pxi >= w || pyi >= h { continue; }
                let o = ((pyi * w + pxi) * 4) as usize;
                if q > 0.0 {
                    for ch in 0..3 { self.canvas[o + ch] = (col[ch] as f32 * tint) as u8; }
                    self.canvas[o + 3] = self.canvas[o + 3].max((q * alpha * 255.0) as u8);
                } else {
                    // smeared dark copy one low-res pixel down-right
                    for ch in 0..3 { self.canvas[o + ch] = 0; }
                    self.canvas[o + 3] = self.canvas[o + 3].max((0.7 * alpha * 255.0) as u8);
                }
            } }
        } }
    }
}
