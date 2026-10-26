//! Cached PCM voices, stereo distance cues and non-repeating footsteps.
use glam::Vec3;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::{collections::HashMap, io::Cursor, sync::Arc, time::Duration};

#[derive(Clone)]
struct Clip { samples: Arc<[f32]>, rate: u32 }
#[derive(Clone)]
struct Voice { clip: Clip, at: usize, gain: [f32; 2] }
impl Iterator for Voice {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let x = *self.clip.samples.get(self.at)? * self.gain[self.at % 2];
        self.at += 1; Some(x)
    }
}
impl Source for Voice {
    fn current_frame_len(&self) -> Option<usize> { None }
    fn channels(&self) -> u16 { 2 }
    fn sample_rate(&self) -> u32 { self.clip.rate }
    fn total_duration(&self) -> Option<Duration> { Some(Duration::from_secs_f64(self.clip.samples.len() as f64 / (2.0 * self.clip.rate as f64))) }
}
struct Loop { sink: Sink, pos: Vec3, volume: f32, range: f32 }
pub struct Sfx {
    _stream: Option<OutputStream>, handle: Option<OutputStreamHandle>,
    clips: HashMap<String, Clip>, loops: Vec<Loop>,
    pub listener: Vec3, pub right: Vec3, pub muted: bool,
    rng: u32, last_step: usize, last_take: HashMap<String, usize>, far_in: f32,
    /// 0..1, ears ringing after a shot: the background (loops, far events) drops away and comes back; one-shots
    /// (the shot itself, steps) are left as they are
    deaf: f32,
}
impl Sfx {
    pub fn new(dir: std::path::PathBuf, muted: bool) -> Self {
        let (stream, handle) = if muted { (None,None) } else { match OutputStream::try_default() {
            Ok((s,h)) => (Some(s),Some(h)), Err(e) => { eprintln!("audio: {e}"); (None,None) }
        }};
        let mut clips = HashMap::new();
        // Decode once at startup to keep file IO and decoding out of firing/footfall frames.
        if let Ok(files) = std::fs::read_dir(dir) {
            for entry in files.flatten() {
                let p = entry.path();
                if p.extension().is_none_or(|e| e != "wav") { continue; }
                let Ok(bytes) = std::fs::read(&p) else { continue };
                let Ok(d) = Decoder::new(Cursor::new(bytes)) else { eprintln!("audio: invalid {}",p.display()); continue };
                let (rate, channels) = (d.sample_rate(),d.channels() as usize);
                let src: Vec<f32> = d.convert_samples().collect();
                let samples: Vec<f32> = src.chunks(channels).flat_map(|f| [f[0],f[if channels > 1 { 1 } else { 0 }]]).collect();
                if let Some(name) = p.file_stem().and_then(|s| s.to_str()) { clips.insert(name.to_string(), Clip { samples: samples.into(), rate }); }
            }
        }
        Self { _stream: stream, handle, clips, loops: vec![], listener: Vec3::ZERO, right: Vec3::X, muted, rng: 1234567, last_step: 0, last_take: HashMap::new(), far_in: 15.0, deaf: 0.0 }
    }
    fn random(&mut self) -> f32 {
        self.rng ^= self.rng << 13; self.rng ^= self.rng >> 17; self.rng ^= self.rng << 5;
        self.rng as f32 / u32::MAX as f32
    }
    /// A sound by this name is loaded.
    pub fn has(&self, name: &str) -> bool { self.clips.contains_key(name) }
    pub fn play2d(&mut self, name: &str, volume: f32, pitch: f32) { self.pan(name,volume,pitch,0.0); }
    fn pan(&self, name: &str, volume: f32, pitch: f32, pan: f32) {
        if self.muted { return; }
        let (Some(h),Some(c)) = (&self.handle,self.clips.get(name)) else { return };
        let pan = pan.clamp(-0.85,0.85);
        let voice = Voice { clip: c.clone(), at: 0, gain: [volume * (1.0-pan).sqrt(), volume * (1.0+pan).sqrt()] };
        h.play_raw(voice.speed(pitch.clamp(0.25,3.0))).ok();
    }
    pub fn footstep(&mut self, surface: &str, side: f32, volume: f32, running: bool) {
        let i = (self.last_step + 1 + (self.random() * 4.0) as usize) % 5;
        self.last_step = i;
        let pitch = 0.97 + self.random() * 0.06 + if running { 0.02 } else { 0.0 };
        self.pan(&format!("step_{surface}_{i}"),volume,pitch,side * 0.13);
        self.pan(&format!("cloth_{}",i % 3),volume * 0.18,1.0,side * 0.08);
    }
    pub fn play_at(&mut self, name: &str, pos: Vec3, volume: f32, pitch: f32, max_dist: f32) {
        let delta = pos - self.listener;
        let range = max_dist * 10.0; // preserve existing callers' range unit
        let k = (1.0 - delta.length() / range.max(0.01)).clamp(0.0,1.0);
        if k > 0.0 { self.pan(name,volume * k * k,pitch,delta.normalize_or_zero().dot(self.right) * 0.7); }
    }
    /// Clip `<set>_<i>` picked at random, never the same take twice in a row; `None` if the set has no files.
    /// The next take of a sound set (`name_0`, `name_1`…), never the same one twice in a row.
    pub fn take(&mut self, set: &str) -> Option<String> {
        let n = (0..).take_while(|i| self.clips.contains_key(&format!("{set}_{i}"))).count();
        if n == 0 { return None; }
        let mut i = (self.random() * n as f32) as usize % n;
        if n > 1 && self.last_take.get(set) == Some(&i) { i = (i + 1 + (self.random() * (n - 1) as f32) as usize % (n - 1)) % n; }
        self.last_take.insert(set.to_string(), i);
        Some(format!("{set}_{i}"))
    }
    /// Surface sounds the Source way (surfaceprops): `impact_<surface>_<kind>_<i>.wav`, kind = bullet / hard / soft.
    /// A surface without recorded takes stays silent, so new sets work by dropping files in.
    pub fn impact(&mut self, surface: &str, kind: &str, pos: Vec3, volume: f32, pitch: f32, max_dist: f32) {
        let Some(name) = self.take(&format!("impact_{surface}_{kind}")) else { return };
        let pitch = pitch * (0.95 + self.random() * 0.1); // soundscript-style pitch spread hides the repeats
        self.play_at(&name, pos, volume, pitch, max_dist);
    }
    /// A random take of `<set>_<i>` straight to the ears (bodycam start-up chirp).
    pub fn play_any(&mut self, set: &str, volume: f32, pitch: f32) {
        if let Some(name) = self.take(set) { self.pan(&name, volume, pitch, 0.0); }
    }
    /// The district far off at night (petards, salutes, a skid, a plane): a random `far_<i>` every 25–70 s from a
    /// random side. The takes are already muffled by distance; through the walls they are only a murmur.
    pub fn distant(&mut self, dt: f32, indoors: bool) {
        self.far_in -= dt;
        if self.far_in > 0.0 { return; }
        self.far_in = 25.0 + self.random() * 45.0;
        let Some(name) = self.take("far") else { return };
        let (pan, pitch) = (self.random() * 1.4 - 0.7, 0.94 + self.random() * 0.1);
        self.pan(&name, if indoors { 0.07 } else { 0.22 } * (1.0 - self.deaf), pitch, pan);
    }
    pub fn start_loop(&mut self, name: &str, pos: Vec3, volume: f32, range: f32) -> Option<usize> {
        if self.muted { return None; }
        let (Some(h),Some(c)) = (&self.handle,self.clips.get(name)) else { return None };
        let sink = Sink::try_new(h).ok()?;
        sink.append(Voice { clip: c.clone(), at: 0, gain: [1.0,1.0] }.repeat_infinite()); sink.set_volume(0.0);
        self.loops.push(Loop { sink,pos,volume,range }); Some(self.loops.len()-1)
    }
    pub fn set_loop(&mut self, id: Option<usize>, volume: f32, pos: Vec3) {
        if let Some(l) = id.and_then(|i| self.loops.get_mut(i)) { l.volume=volume; l.pos=pos; }
    }
    /// A bang next to the head: the world goes dull at once (`amount` 0..1, e.g. more indoors than in the open).
    pub fn deafen(&mut self, amount: f32) { self.deaf = (self.deaf + amount).min(0.9); }
    pub fn update(&mut self,dt:f32) {
        self.deaf *= (-1.6 * dt).exp();
        for l in &self.loops {
            let k = (1.0-l.pos.distance(self.listener)/l.range).clamp(0.0,1.0);
            let target=if self.muted {0.0} else {l.volume*k*k*(1.0-0.85*self.deaf)};
            // it drops at once and comes back slowly
            let gain=l.sink.volume();let rate=if target<gain {40.0} else {12.0};
            l.sink.set_volume(gain+(target-gain)*(1.0-(-rate*dt).exp()));
        }
    }
}
