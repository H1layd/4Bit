//! wgpu renderer. Per frame:
//!   flash shadow map (world draws) -> scene passes into a low-res HDR target (+ normals, depth)
//!   -> half-res fx (SSAO + dust in the flash beam) -> bloom (bright pass + blur) -> post shader (+HUD) to the window.
use crate::mesh::{MeshData, Vertex};
use glam::{Mat4, Vec3, Vec4};
use std::collections::HashMap;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;
mod materials;

/// Lines of the low-resolution scene target (the project's `render.frame_lines`; 360 = chunky lo-fi).
pub static FRAME_LINES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(360);
const SHADOW_SIZE: u32 = 2048;
const DRAW_SLOT: u64 = 256;
const GLOBAL_SLOT: u64 = 4096;
/// lights per frame (lamps, muzzle flash, torch bounce) and lamps with a cached shadow map
pub const MAX_LIGHTS: usize = 32;
pub const LAMP_SLOTS: usize = 8;
const LAMP_SIZE: u32 = 512;
/// shadow cameras: torch + lamp slots, one 256-byte uniform slot each (shadow.wgsl reads the Globals prefix)
const CAM_SLOT: u64 = 256;
const MAX_DRAWS: u64 = 4096;
const MAX_PASSES: u64 = 4;
const HDR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub inv_view_proj: [[f32; 4]; 4],
    pub light_vp: [[f32; 4]; 4],
    pub cam_pos: [f32; 4],
    pub light_pos: [[f32; 4]; MAX_LIGHTS],
    pub light_col: [[f32; 4]; MAX_LIGHTS],
    /// x: lamp shadow slot (-1 = none), y: shadow weight (slot hand-over fade), z: floor bounce, w: halo in the haze
    pub light_info: [[f32; 4]; MAX_LIGHTS],
    pub flash_pos: [f32; 4],
    pub flash_dir: [f32; 4],
    pub flash_col: [f32; 4],
    pub flash_params: [f32; 4],
    pub ambient: [f32; 4],
    pub fog_color: [f32; 4],
    pub fog_params: [f32; 4],
    pub lamp_vp: [[[f32; 4]; 4]; LAMP_SLOTS],
    /// per lamp shadow slot: the index of its light (-1 = free), so the light shafts only visit shadowed lamps
    pub slot_light: [[f32; 4]; LAMP_SLOTS / 4],
    // environment, filled from `Gfx::env` at render
    pub zone_min: [[f32; 4]; 4],
    pub zone_max: [[f32; 4]; 4],
    pub zones: [f32; 4],
    pub sky_amb: [f32; 4],
    pub key_dir: [f32; 4],
    pub key_col: [f32; 4],
    pub in_amb: [f32; 4],
    pub fog_out: [f32; 4],
    pub fog_in: [f32; 4],
    pub fog_end: [f32; 4],
}

/// Where the world is indoors and what the sky does: the project's lighting settings.
/// Interior zones (up to 4 boxes) get the indoor ambient and fog and keep outdoor lamps out (and the other way round);
/// a storey height keeps lamps from lighting through floor slabs (0 = no storeys).
#[derive(Clone, Copy, Debug)]
pub struct Env {
    pub zones: [(Vec3, Vec3); 4], pub zone_count: usize, pub storey: f32, pub zone_top: f32,
    pub sky_ambient: Vec3, pub key_dir: Vec3, pub key_colour: Vec3, pub key_wrap: f32,
    pub indoor_ambient: Vec3, pub rim: f32,
    pub fog_outdoor: Vec3, pub fog_outdoor_range: (f32, f32), pub fog_indoor: Vec3, pub fog_indoor_range: (f32, f32),
}
impl Default for Env {
    /// A dim overcast evening: everything outdoors, soft sky, a low warm key.
    fn default() -> Env {
        Env { zones: [(Vec3::ZERO, Vec3::ZERO); 4], zone_count: 0, storey: 0.0, zone_top: 1e6,
              sky_ambient: Vec3::new(0.20, 0.22, 0.26), key_dir: Vec3::new(-0.4, 0.6, -0.5), key_colour: Vec3::new(0.55, 0.48, 0.40), key_wrap: 0.2,
              indoor_ambient: Vec3::new(0.12, 0.12, 0.11), rim: 1.0,
              fog_outdoor: Vec3::new(0.10, 0.11, 0.13), fog_outdoor_range: (30.0, 160.0), fog_indoor: Vec3::new(0.02, 0.02, 0.02), fog_indoor_range: (4.0, 40.0) }
    }
}
impl Env {
    fn write(&self, g: &mut Globals) {
        for k in 0..4 { g.zone_min[k] = self.zones[k].0.extend(0.0).into(); g.zone_max[k] = self.zones[k].1.extend(0.0).into(); }
        g.zones = [self.zone_count.min(4) as f32, self.storey, self.zone_top, 0.0];
        g.sky_amb = self.sky_ambient.extend(1.0).into();
        g.key_dir = self.key_dir.normalize_or_zero().extend(self.key_wrap).into();
        g.key_col = self.key_colour.extend(1.0).into();
        g.in_amb = self.indoor_ambient.extend(self.rim).into();
        g.fog_out = self.fog_outdoor.extend(self.fog_outdoor_range.0).into();
        g.fog_in = self.fog_indoor.extend(self.fog_indoor_range.0).into();
        g.fog_end = [self.fog_outdoor_range.1, self.fog_indoor_range.1, 0.0, 0.0];
    }
}

/// One light for `globals`: a lamp, the muzzle flash or the torch's bounce off what it hits.
#[derive(Clone, Copy)]
pub struct LightIn { pub pos: Vec3, pub range: f32, pub color: Vec3, pub stair: bool, pub slot: i32, pub weight: f32, pub bounce: f32, pub halo: f32 }

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowCam { view_proj: [[f32; 4]; 4], inv_view_proj: [[f32; 4]; 4], light_vp: [[f32; 4]; 4] }

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    model: [[f32; 4]; 4],
    color: [f32; 4],
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PostParams {
    pub grade: [f32; 4],
    pub tint: [f32; 4],
    pub noise: [f32; 4],
    pub misc: [f32; 4],  // fade, levels, fisheye, sharpen
    pub misc2: [f32; 4], // analog, time, exposure, bloom
}

pub type MeshId = usize;

/// egui over the game frame (the scene editor): its renderer, and this frame's triangles and texture changes.
pub struct UiLayer { pub renderer: egui_wgpu::Renderer, pub jobs: Vec<egui::ClippedPrimitive>, pub delta: egui::TexturesDelta, pub ppp: f32 }

/// `DrawCmd::fog` at or below this: drawn into lamp shadow maps only (the player's own arms and head)
pub const SHADOW_ONLY: f32 = -99.0;

#[derive(Clone)]
pub struct DrawCmd {
    pub mesh: MeshId,
    pub tex: String,
    pub model: Mat4,
    pub color: Vec4,
    pub unlit: bool,
    pub fog: f32,
    pub cutoff: f32,
    /// can move (lift cars and doors, the body, the pistol): drawn into the lamp shadows every frame instead of
    /// being baked into their static caches
    pub moving: bool,
}

pub struct Pass {
    pub globals: Globals,
    pub clear_color: bool,
    pub draws: Vec<DrawCmd>,
}

struct GpuMesh {
    bounds:[Vec3;2],dynamic:std::cell::Cell<bool>,
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    count: u32,
    /// levels of detail in `ib`: first index, index count, geometric error in metres (LOD 0 = the mesh, error 0)
    lods: Vec<(u32, u32, f32)>,
}

/// Screen error allowed for a coarser LOD, as error / distance (~1.5 px of the 360-line frame); shadows take 3x.
const LOD_VIEW: f32 = 0.005;
const LOD_SHADOW: f32 = 0.015;

/// Simplified index lists for a static mesh: errors in metres, a level kept only if it drops a quarter of the triangles.
/// Chunk borders stay put (neighbouring chunks never open a crack); every level is reordered for the vertex cache.
fn build_lods(m: &MeshData) -> (Vec<u32>, Vec<(u32, u32, f32)>) {
    let opt = |ix: &[u32]| meshopt::optimize_vertex_cache(ix, m.vertices.len());
    let mut all = opt(&m.indices);
    let mut lods = vec![(0, all.len() as u32, 0.0)];
    if m.indices.len() < 3000 { return (all, lods); }
    let Ok(adapter) = meshopt::VertexDataAdapter::new(bytemuck::cast_slice(&m.vertices), std::mem::size_of::<Vertex>(), 0) else { return (all, lods) };
    let mut prev = m.indices.len();
    for err in [0.01f32, 0.04, 0.15] {
        let mut got = 0.0;
        let ix = meshopt::simplify(&m.indices, &adapter, 0, err, meshopt::SimplifyOptions::LockBorder | meshopt::SimplifyOptions::ErrorAbsolute, Some(&mut got));
        if ix.is_empty() || ix.len() * 4 > prev * 3 { continue; }
        prev = ix.len();
        let ix = opt(&ix);
        lods.push((all.len() as u32, ix.len() as u32, got.max(err * 0.25)));
        all.extend(ix);
    }
    (all, lods)
}

/// Where a camera sits, from its view-projection (the near-plane centre: close enough for picking a LOD).
fn eye_of(vp: Mat4) -> Vec3 { let p = vp.inverse() * glam::Vec4::new(0.0, 0.0, 0.0, 1.0); p.truncate() / p.w }

/// Roughness by texture: metal / plastic / gun parts shine, plaster and concrete don't.
fn roughness(tex: &str) -> f32 {
    if tex.starts_with("fx_smoke") { return 2.0; } // not a surface: scene.wgsl lights it as a volume
    if tex.starts_with("cellar") || tex.starts_with("hall_") || tex=="facade" || tex.starts_with("snow") { 0.95 }
    else if tex.starts_with("models/props/") {0.95}
    else if tex.starts_with("models/citizen/") { 0.88 }
    else if tex.starts_with("models/") { 0.35 }
    else if tex.contains("metal") || tex.contains("lift_panel") || tex.contains("elevator") { 0.3 }
    else if tex.contains("floor") || tex.contains("lift_wall") || tex.contains("door") { 0.55 }
    else { 0.85 }
}

impl GpuMesh {
    /// The coarsest level whose error stays under `k` of the distance from `eye` (world) to the mesh.
    fn lod(&self,model:&Mat4,eye:Vec3,k:f32)->(u32,u32) {
        let full=(0,self.count);
        if self.lods.len()<2||self.dynamic.get() {return full;}
        let [lo,hi]=self.bounds;
        let scale=model.x_axis.truncate().length();
        let c=model.transform_point3((lo+hi)*0.5);
        let d=(c.distance(eye)-(hi-lo).length()*0.5*scale).max(0.05);
        self.lods.iter().rev().find(|l|l.2*scale<k*d).map_or(full,|l|(l.0,l.1))
    }
    fn visible(&self,matrix:Mat4)->bool {
        if self.dynamic.get() {return true;}
        let [lo,hi]=self.bounds;
        let corners=[Vec3::new(lo.x,lo.y,lo.z),Vec3::new(hi.x,lo.y,lo.z),Vec3::new(lo.x,hi.y,lo.z),Vec3::new(hi.x,hi.y,lo.z),Vec3::new(lo.x,lo.y,hi.z),Vec3::new(hi.x,lo.y,hi.z),Vec3::new(lo.x,hi.y,hi.z),Vec3::new(hi.x,hi.y,hi.z)].map(|v|matrix*v.extend(1.0));
        !(corners.iter().all(|p|p.x < -p.w)||corners.iter().all(|p|p.x>p.w)||corners.iter().all(|p|p.y < -p.w)||corners.iter().all(|p|p.y>p.w)||corners.iter().all(|p|p.z<0.0)||corners.iter().all(|p|p.z>p.w))
    }
}

/// `FOURBIT_PROFILE=gpu`: GPU time of each pass and of the whole frame (timestamp queries) (timestamp queries) and triangles per pass, every 60 frames.
struct Prof { qs: wgpu::QuerySet, resolve: wgpu::Buffer, read: wgpu::Buffer, period: f32, ms: [f64; STAMPS as usize], frame: f64, tris: [u64; 3], cpu: [f64; 3], frames: u32 }
/// begin/end timestamps of every pass (Metal only writes them at pass boundaries)
const STAMPS: u32 = 20;
const STAMP_GROUPS: [(&str, &[usize]); 7] = [("torch shadow", &[0]), ("lamp shadows", &[1]), ("world", &[3]), ("viewmodel", &[4]),
    ("fx", &[5]), ("bloom", &[6, 7, 8]), ("post", &[9])];

struct Targets {
    frame: wgpu::Texture,
    normal: wgpu::Texture,
    depth: wgpu::Texture,
    vm_depth: wgpu::Texture,
    fx: wgpu::Texture,
    bloom_a: wgpu::Texture,
    bloom_b: wgpu::Texture,
    hud: wgpu::Texture,
    frame_size: (u32, u32),
    hud_size: (u32, u32),
}

pub struct Gfx {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scene_pipe: wgpu::RenderPipeline,
    opaque_pipe: wgpu::RenderPipeline,
    flash_pipe: wgpu::RenderPipeline,
    smoke_pipe: wgpu::RenderPipeline,
    shadow_pipe: wgpu::RenderPipeline,
    fx_pipe: wgpu::RenderPipeline,
    bright_pipe: wgpu::RenderPipeline,
    blur_h_pipe: wgpu::RenderPipeline,
    blur_v_pipe: wgpu::RenderPipeline,
    post_pipe: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    draw_buf: wgpu::Buffer,
    draw_bg: wgpu::BindGroup,
    shadow_draw_bg: wgpu::BindGroup,
    tex_layout: wgpu::BindGroupLayout,
    post_layout: wgpu::BindGroupLayout,
    fx_layout: wgpu::BindGroupLayout,
    bloom_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    clamp_sampler: wgpu::Sampler,
    cmp_sampler: wgpu::Sampler,
    shadow_view: wgpu::TextureView,
    shadow_bg: wgpu::BindGroup,
    cam_buf: wgpu::Buffer,
    cam_bg: wgpu::BindGroup,
    /// lamp shadows: static geometry rendered once per lamp into `lamp_static`, copied each frame into `lamp_live`
    /// where only moving casters (body, lift cars, doors) are drawn on top
    lamp_static: wgpu::Texture,
    lamp_live: wgpu::Texture,
    lamp_static_layers: Vec<wgpu::TextureView>,
    lamp_live_layers: Vec<wgpu::TextureView>,
    lamp_array: wgpu::TextureView,
    lamp_keys: [Option<u64>; LAMP_SLOTS],
    draw_data: Vec<u8>,
    textures: HashMap<String, wgpu::BindGroup>,
    styles: HashMap<String, materials::Style>,
    meshes: Vec<GpuMesh>,
    t: Targets,
    post_buf: wgpu::Buffer,
    post_bg: wgpu::BindGroup,
    fx_bg: wgpu::BindGroup,
    bright_bg: wgpu::BindGroup,
    blur_h_bg: wgpu::BindGroup,
    blur_v_bg: wgpu::BindGroup,
    pub frame_size: (u32, u32),
    pub hud_size: (u32, u32),
    asset_dir: std::path::PathBuf,
    /// CPU-drawn textures rewritten in place (`update_texture`): the phone screen
    dynamic: HashMap<String, (wgpu::Texture, (u32, u32))>,
    /// copy the world (pass 0, before the hands) into the "phone_cam" texture this frame
    pub phone_cam: bool,
    phone_cam_tex: Option<wgpu::Texture>,
    /// snapshot of this frame's world inside a rect (u0, v0, u1, v1 of the frame) into "phone_shot"; the picture comes
    /// back in `phone_shot_png` after the frame
    pub phone_shot: Option<[f32; 4]>,
    phone_shot_tex: Option<(wgpu::Texture, (u32, u32, u32, u32))>,
    pub phone_shot_png: Option<image::RgbaImage>,
    prof: Option<Prof>,
    /// the scene editor's egui layer, drawn over the frame
    pub ui: Option<UiLayer>,
    /// lighting settings of the world (zones, sky, fog)
    pub env: Env,
    /// draw calls and triangles of the scene passes, this frame / the last one (F3)
    drawn: (u32, u64), pub last_drawn: (u32, u64),
}

fn view(t: &wgpu::Texture) -> wgpu::TextureView { t.create_view(&Default::default()) }

impl Gfx {
    pub fn new(window: Arc<Window>, asset_dir: std::path::PathBuf) -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no GPU adapter");
        let timing = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        // FOURBIT_PROFILE=gpu: timestamp queries (waits for the GPU every frame); FOURBIT_PROFILE=1 only times the frame stages
        let profile = std::env::var("FOURBIT_PROFILE").is_ok_and(|v| v == "gpu") && adapter.features().contains(timing);
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor { label: None, required_features: if profile { timing } else { wgpu::Features::empty() },
                required_limits: wgpu::Limits::default(), memory_hints: wgpu::MemoryHints::Performance },
            None,
        ))
        .expect("device");
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // FOURBIT_NOVSYNC=1: uncapped, so the frame time printed by FOURBIT_PROFILE is the real cost
            present_mode: if std::env::var_os("FOURBIT_NOVSYNC").is_some() { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::AutoVsync },
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // ---- layouts
        let uniform = |binding, vis, dynamic| wgpu::BindGroupLayoutEntry {
            binding, visibility: vis,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: dynamic, min_binding_size: None },
            count: None,
        };
        let tex_e = |binding| wgpu::BindGroupLayoutEntry {
            binding, visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let depth_e = |binding| wgpu::BindGroupLayoutEntry {
            binding, visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let samp_e = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(ty), count: None,
        };
        let vf = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let filt = wgpu::SamplerBindingType::Filtering;
        let cmp = wgpu::SamplerBindingType::Comparison;
        let bgl = |entries: &[wgpu::BindGroupLayoutEntry]| device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries });
        let globals_layout = bgl(&[uniform(0, vf, true)]);
        let tex_layout = bgl(&[tex_e(0), tex_e(1), samp_e(2, filt)]);
        let draw_layout = bgl(&[uniform(0, vf, true)]);
        let depth_array = |binding| wgpu::BindGroupLayoutEntry {
            binding, visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2Array, multisampled: false },
            count: None,
        };
        let shadow_layout = bgl(&[depth_e(0), samp_e(1, cmp), depth_array(2)]);
        let fx_layout = bgl(&[depth_e(0), tex_e(1), depth_e(2), samp_e(3, cmp), depth_array(4)]);
        let bloom_layout = bgl(&[tex_e(0), samp_e(1, filt)]);
        let post_layout = bgl(&[tex_e(0), tex_e(1), samp_e(2, filt), uniform(3, wgpu::ShaderStages::FRAGMENT, false), tex_e(4), tex_e(5)]);

        // ---- pipelines
        let module = |name: &str, src: &str| device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(name), source: wgpu::ShaderSource::Wgsl(src.into()) });
        let scene_sh = module("scene", include_str!("../shaders/scene.wgsl"));
        let shadow_sh = module("shadow", include_str!("../shaders/shadow.wgsl"));
        let fx_sh = module("fx", include_str!("../shaders/fx.wgsl"));
        let bloom_sh = module("bloom", include_str!("../shaders/bloom.wgsl"));
        let post_sh = module("post", include_str!("../shaders/post.wgsl"));
        let layout = |groups: &[&wgpu::BindGroupLayout]| device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None, bind_group_layouts: groups, push_constant_ranges: &[] });
        let vbuf = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2],
        }];
        let depth_state = |write| Some(wgpu::DepthStencilState {
            format: DEPTH, depth_write_enabled: write, depth_compare: wgpu::CompareFunction::Less,
            stencil: Default::default(), bias: Default::default(),
        });
        let target = |f| Some(wgpu::ColorTargetState { format: f, blend: None, write_mask: wgpu::ColorWrites::ALL });
        let scene_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene"),
            layout: Some(&layout(&[&globals_layout, &tex_layout, &draw_layout, &shadow_layout])),
            vertex: wgpu::VertexState { module: &scene_sh, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &vbuf },
            fragment: Some(wgpu::FragmentState {
                module: &scene_sh, entry_point: Some("fs"), compilation_options: Default::default(),
                targets: &[target(HDR), target(HDR)],
            }),
            primitive: wgpu::PrimitiveState { front_face: wgpu::FrontFace::Cw, cull_mode: None, ..Default::default() },
            depth_stencil: depth_state(true),
            multisample: Default::default(), multiview: None, cache: None,
        });
        let opaque_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene opaque"),
            layout: Some(&layout(&[&globals_layout, &tex_layout, &draw_layout, &shadow_layout])),
            vertex: wgpu::VertexState { module: &scene_sh, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &vbuf },
            fragment: Some(wgpu::FragmentState {
                module: &scene_sh, entry_point: Some("fs_opaque"), compilation_options: Default::default(),
                targets: &[target(HDR), target(HDR)],
            }),
            primitive: wgpu::PrimitiveState { front_face: wgpu::FrontFace::Cw, cull_mode: None, ..Default::default() },
            depth_stencil: depth_state(true),
            multisample: Default::default(), multiview: None, cache: None,
        });
        let flash_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flash additive"),
            layout: Some(&layout(&[&globals_layout, &tex_layout, &draw_layout, &shadow_layout])),
            vertex: wgpu::VertexState { module: &scene_sh, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &vbuf },
            fragment: Some(wgpu::FragmentState { module: &scene_sh, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[
                Some(wgpu::ColorTargetState { format: HDR, blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::SrcAlpha, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
                    alpha: wgpu::BlendComponent::OVER,
                }), write_mask: wgpu::ColorWrites::ALL }),
                Some(wgpu::ColorTargetState { format: HDR, blend: None, write_mask: wgpu::ColorWrites::empty() }),
            ] }),
            primitive: wgpu::PrimitiveState { front_face: wgpu::FrontFace::Cw, cull_mode: None, ..Default::default() },
            depth_stencil: depth_state(false), multisample: Default::default(), multiview: None, cache: None,
        });
        // `fx_*` textures (smoke): lit like the scene, blended over it, no depth or normal writes (SSAO and the shafts
        // keep seeing the walls behind); the game draws them last
        let smoke_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("smoke"),
            layout: Some(&layout(&[&globals_layout, &tex_layout, &draw_layout, &shadow_layout])),
            vertex: wgpu::VertexState { module: &scene_sh, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &vbuf },
            fragment: Some(wgpu::FragmentState { module: &scene_sh, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[
                Some(wgpu::ColorTargetState { format: HDR, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL }),
                Some(wgpu::ColorTargetState { format: HDR, blend: None, write_mask: wgpu::ColorWrites::empty() }),
            ] }),
            primitive: wgpu::PrimitiveState { front_face: wgpu::FrontFace::Cw, cull_mode: None, ..Default::default() },
            depth_stencil: depth_state(false), multisample: Default::default(), multiview: None, cache: None,
        });
        let shadow_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow"),
            layout: Some(&layout(&[&globals_layout, &draw_layout])),
            vertex: wgpu::VertexState { module: &shadow_sh, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &vbuf },
            fragment: None,
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH, depth_write_enabled: true, depth_compare: wgpu::CompareFunction::Less, stencil: Default::default(),
                bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.0, clamp: 0.0 },
            }),
            multisample: Default::default(), multiview: None, cache: None,
        });
        let fullscreen = |name: &str, sh: &wgpu::ShaderModule, entry: &str, groups: &[&wgpu::BindGroupLayout], fmt| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(name),
                layout: Some(&layout(groups)),
                vertex: wgpu::VertexState { module: sh, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState { module: sh, entry_point: Some(entry), compilation_options: Default::default(), targets: &[target(fmt)] }),
                primitive: Default::default(), depth_stencil: None, multisample: Default::default(), multiview: None, cache: None,
            })
        };
        let fx_pipe = fullscreen("fx", &fx_sh, "fs", &[&globals_layout, &fx_layout], HDR);
        let bright_pipe = fullscreen("bright", &bloom_sh, "bright", &[&bloom_layout], HDR);
        let blur_h_pipe = fullscreen("blur_h", &bloom_sh, "blur_h", &[&bloom_layout], HDR);
        let blur_v_pipe = fullscreen("blur_v", &bloom_sh, "blur_v", &[&bloom_layout], HDR);
        let post_pipe = fullscreen("post", &post_sh, "fs", &[&post_layout], format);

        // ---- buffers + samplers
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None, size: GLOBAL_SLOT * MAX_PASSES, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let draw_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None, size: DRAW_SLOT * MAX_DRAWS, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let ubind = |layout: &wgpu::BindGroupLayout, buf: &wgpu::Buffer, size: usize| device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: buf, offset: 0, size: wgpu::BufferSize::new(size as u64) }) }],
        });
        let globals_bg = ubind(&globals_layout, &globals_buf, std::mem::size_of::<Globals>());
        let draw_bg = ubind(&draw_layout, &draw_buf, std::mem::size_of::<DrawUniform>());
        let cam_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None, size: CAM_SLOT * (1 + LAMP_SLOTS as u64), usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let cam_bg = ubind(&globals_layout, &cam_buf, std::mem::size_of::<ShadowCam>());
        let shadow_draw_bg = ubind(&draw_layout, &draw_buf, std::mem::size_of::<DrawUniform>());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat, address_mode_v: wgpu::AddressMode::Repeat, address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 4, ..Default::default()
        });
        let clamp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default()
        });
        let cmp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual), ..Default::default()
        });
        let shadow_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow"), size: wgpu::Extent3d { width: SHADOW_SIZE, height: SHADOW_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: DEPTH,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] });
        let shadow_view = view(&shadow_tex);
        let lamp_tex = |label, usage| device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label), size: wgpu::Extent3d { width: LAMP_SIZE, height: LAMP_SIZE, depth_or_array_layers: LAMP_SLOTS as u32 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: DEPTH, usage, view_formats: &[] });
        let lamp_static = lamp_tex("lamp shadow static", wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
        let lamp_live = lamp_tex("lamp shadow", wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING);
        let layers = |t: &wgpu::Texture| (0..LAMP_SLOTS as u32).map(|k| t.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2), base_array_layer: k, array_layer_count: Some(1), ..Default::default() })).collect::<Vec<_>>();
        let (lamp_static_layers, lamp_live_layers) = (layers(&lamp_static), layers(&lamp_live));
        let lamp_array = lamp_live.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let shadow_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &shadow_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&shadow_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&cmp_sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&lamp_array) },
            ],
        });
        let post_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None, size: std::mem::size_of::<PostParams>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });

        let t = Self::targets(&device, config.width, config.height);
        let (post_bg, fx_bg, bright_bg, blur_h_bg, blur_v_bg) =
            Self::groups(&device, &t, &post_layout, &fx_layout, &bloom_layout, &clamp_sampler, &cmp_sampler, &shadow_view, &lamp_array, &post_buf);
        let (frame_size, hud_size) = (t.frame_size, t.hud_size);
        let prof = profile.then(|| Prof {
            qs: device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: STAMPS }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 8 * STAMPS as u64, usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false }),
            read: device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 8 * STAMPS as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
            period: queue.get_timestamp_period(), ms: [0.0; STAMPS as usize], frame: 0.0, tris: [0; 3], cpu: [0.0; 3], frames: 0,
        });
        let mut g = Gfx {
            window, surface, device, queue, config, scene_pipe, opaque_pipe, flash_pipe, smoke_pipe, shadow_pipe, fx_pipe, bright_pipe, blur_h_pipe, blur_v_pipe, post_pipe,
            globals_buf, globals_bg, draw_buf, draw_bg, shadow_draw_bg, tex_layout, post_layout, fx_layout, bloom_layout,
            sampler, clamp_sampler, cmp_sampler, shadow_view, shadow_bg, cam_buf, cam_bg, lamp_static, lamp_live, lamp_static_layers,
            lamp_live_layers, lamp_array, lamp_keys: [None; LAMP_SLOTS], draw_data: vec![0; (DRAW_SLOT * MAX_DRAWS) as usize], textures: HashMap::new(), styles: materials::load(&asset_dir), meshes: vec![], t, post_buf, post_bg,
            fx_bg, bright_bg, blur_h_bg, blur_v_bg, frame_size, hud_size, asset_dir, prof, drawn: (0, 0), last_drawn: (0, 0), ui: None, env: Env::default(),
            dynamic: HashMap::new(), phone_cam: false, phone_cam_tex: None, phone_shot: None, phone_shot_tex: None, phone_shot_png: None,
        };
        g.texture("");
        g
    }

    fn targets(device: &wgpu::Device, w: u32, h: u32) -> Targets {
        let fh = FRAME_LINES.load(std::sync::atomic::Ordering::Relaxed);
        let fw = ((fh as f32 * w as f32 / h.max(1) as f32).round() as u32).max(64);
        let mk = |w: u32, h: u32, format, usage| device.create_texture(&wgpu::TextureDescriptor {
            label: None, size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] });
        let rt = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let hh = 720u32;
        let hw = ((hh as f32 * w as f32 / h.max(1) as f32).round() as u32).max(64);
        Targets {
            frame: mk(fw, fh, HDR, rt | wgpu::TextureUsages::COPY_SRC),
            normal: mk(fw, fh, HDR, rt),
            depth: mk(fw, fh, DEPTH, rt),
            vm_depth: mk(fw, fh, DEPTH, rt),
            fx: mk(fw / 2, fh / 2, HDR, rt),
            bloom_a: mk(fw / 4, fh / 4, HDR, rt),
            bloom_b: mk(fw / 4, fh / 4, HDR, rt),
            hud: mk(hw, hh, wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST),
            frame_size: (fw, fh),
            hud_size: (hw, hh),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn groups(device: &wgpu::Device, t: &Targets, post_l: &wgpu::BindGroupLayout, fx_l: &wgpu::BindGroupLayout, bloom_l: &wgpu::BindGroupLayout,
              lin: &wgpu::Sampler, cmp: &wgpu::Sampler, shadow: &wgpu::TextureView, lamps: &wgpu::TextureView, post_buf: &wgpu::Buffer)
              -> (wgpu::BindGroup, wgpu::BindGroup, wgpu::BindGroup, wgpu::BindGroup, wgpu::BindGroup) {
        let views: Vec<_> = [&t.frame, &t.hud, &t.fx, &t.bloom_a, &t.depth, &t.normal, &t.bloom_b].into_iter().map(view).collect();
        let tv = |i: usize| wgpu::BindingResource::TextureView(&views[i]);
        let e = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let bg = |layout, entries: &[wgpu::BindGroupEntry]| device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout, entries });
        let post = bg(post_l, &[e(0, tv(0)), e(1, tv(1)), e(2, wgpu::BindingResource::Sampler(lin)),
            e(3, post_buf.as_entire_binding()), e(4, tv(2)), e(5, tv(3))]);
        let fx = bg(fx_l, &[e(0, tv(4)), e(1, tv(5)), e(2, wgpu::BindingResource::TextureView(shadow)),
            e(3, wgpu::BindingResource::Sampler(cmp)), e(4, wgpu::BindingResource::TextureView(lamps))]);
        let bright = bg(bloom_l, &[e(0, tv(0)), e(1, wgpu::BindingResource::Sampler(lin))]);
        let blur_h = bg(bloom_l, &[e(0, tv(3)), e(1, wgpu::BindingResource::Sampler(lin))]);
        let blur_v = bg(bloom_l, &[e(0, tv(6)), e(1, wgpu::BindingResource::Sampler(lin))]);
        (post, fx, bright, blur_h, blur_v)
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 { return; }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        self.t = Self::targets(&self.device, w, h);
        let (a, b, c, d, e) = Self::groups(&self.device, &self.t, &self.post_layout, &self.fx_layout, &self.bloom_layout,
            &self.clamp_sampler, &self.cmp_sampler, &self.shadow_view, &self.lamp_array, &self.post_buf);
        (self.post_bg, self.fx_bg, self.bright_bg, self.blur_h_bg, self.blur_v_bg) = (a, b, c, d, e);
        self.frame_size = self.t.frame_size;
        self.hud_size = self.t.hud_size;
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat { self.config.format }
    pub fn aspect(&self) -> f32 { self.frame_size.0 as f32 / self.frame_size.1 as f32 }

    pub fn add_mesh(&mut self, m: &MeshData) -> MeshId {
        let vb = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None, contents: bytemuck::cast_slice(&m.vertices), usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST });
        let ib = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None, contents: bytemuck::cast_slice(&m.indices), usage: wgpu::BufferUsages::INDEX });
        let mut lo=Vec3::splat(f32::INFINITY);let mut hi=Vec3::splat(f32::NEG_INFINITY);
        for v in &m.vertices {let p=Vec3::from(v.pos);lo=lo.min(p);hi=hi.max(p);}
        self.meshes.push(GpuMesh { vb, ib, count: m.indices.len() as u32,bounds:[lo,hi],dynamic:std::cell::Cell::new(false),lods:vec![(0,m.indices.len() as u32,0.0)] });
        self.meshes.len() - 1
    }

    /// Static meshes with automatic levels of detail, simplified on every core.
    pub fn add_static_meshes(&mut self, ms: &[&MeshData]) -> Vec<MeshId> {
        let n = std::thread::available_parallelism().map_or(4, |n| n.get());
        let mut built: Vec<Option<(Vec<u32>, Vec<(u32, u32, f32)>)>> = (0..ms.len()).map(|_| None).collect();
        std::thread::scope(|sc| {
            for (part, out) in ms.chunks(ms.len().div_ceil(n).max(1)).zip(built.chunks_mut(ms.len().div_ceil(n).max(1))) {
                sc.spawn(move || for (m, o) in part.iter().zip(out) { *o = Some(build_lods(m)); });
            }
        });
        ms.iter().zip(built).map(|(m, b)| {
            let (indices, lods) = b.unwrap();
            let vb = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None, contents: bytemuck::cast_slice(&m.vertices), usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST });
            let ib = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None, contents: bytemuck::cast_slice(&indices), usage: wgpu::BufferUsages::INDEX });
            let mut lo=Vec3::splat(f32::INFINITY);let mut hi=Vec3::splat(f32::NEG_INFINITY);
            for v in &m.vertices {let p=Vec3::from(v.pos);lo=lo.min(p);hi=hi.max(p);}
            self.meshes.push(GpuMesh { vb, ib, count: lods[0].1, bounds:[lo,hi], dynamic:std::cell::Cell::new(false), lods });
            self.meshes.len() - 1
        }).collect()
    }
    /// Triangles of every level over all static meshes (for the stats screen).
    pub fn lod_stats(&self) -> (u64, u64) {
        self.meshes.iter().fold((0, 0), |(a, b), m| (a + m.count as u64 / 3, b + m.lods.last().map_or(0, |l| l.1 as u64 / 3)))
    }

    /// New vertex data for a mesh created with the same vertex count (CPU-skinned meshes).
    pub fn update_mesh(&self, id: MeshId, verts: &[Vertex]) {
        self.meshes[id].dynamic.set(true);
        self.queue.write_buffer(&self.meshes[id].vb, 0, bytemuck::cast_slice(verts));
    }

    /// Bind group for assets/textures/<name>.png ("" = white). Mipmapped, loaded once.
    fn texture_path(&self,name:&str)->std::path::PathBuf {
        if let Some(style)=self.styles.get(name) {self.asset_dir.join(&style.path)}
        else if name.ends_with(".png") {self.asset_dir.join(name)}
        else {self.asset_dir.join(format!("textures/{name}.png"))}
    }

    pub fn texture(&mut self, name: &str) {
        if self.textures.contains_key(name) { return; }
        // Some imported meshes repeat an atlas under several names; share its GPU upload.
        if let Some(style)=self.styles.get(name) {
            let shared=self.styles.iter().find_map(|(key,s)| {
                if s.path==style.path {self.textures.get(key).cloned()} else {None}
            });
            if let Some(bg)=shared {self.textures.insert(name.into(),bg);return;}
        }
        let img = if name.is_empty() { None } else {
            let path = self.texture_path(name);
            if std::env::var_os("FOURBIT_MATERIAL_AUDIT").is_some() {
                eprintln!("material {}: {} [{}]",name,path.display(),if self.styles.contains_key(name) {"authored"} else {"legacy"});
            }
            match image::open(&path) {
                Ok(i) => Some(i.to_rgba8()),
                Err(e) => { eprintln!("texture {}: {e}", path.display()); None }
            }
        };
        let img = img.unwrap_or_else(|| image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255])));
        self.texture_image(name, img);
    }

    fn upload(&self, label: &str, img: &image::RgbaImage) -> wgpu::Texture {
        let (w, h) = img.dimensions();
        let mips = 32 - w.max(h).leading_zeros();
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: mips, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        let mut level = img.clone();
        for mip in 0..mips {
            let (lw, lh) = level.dimensions();
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: mip, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &level,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * lw), rows_per_image: Some(lh) },
                wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
            );
            level = image::imageops::resize(&level, (lw / 2).max(1), (lh / 2).max(1), image::imageops::FilterType::Triangle);
        }
        tex
    }

    /// Authored normal maps store roughness in alpha; older/code-generated images use a height fallback.
    pub fn texture_image(&mut self, name: &str, img: image::RgbaImage) {
        let tex = self.upload(name, &img);
        let path=self.texture_path(name);
        let normal=path.with_file_name(format!("{}_normal.png",path.file_stem().unwrap().to_string_lossy()));
        let nmap = image::open(normal).ok().map(|i|i.to_rgba8()).unwrap_or_else(|| height_normals(&img, if name.is_empty() || name.starts_with("muzzle") || name.starts_with("models/citizen/") { 0.0 } else { 0.75 }));
        let ntex = self.upload(name, &nmap);
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view(&tex)) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view(&ntex)) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.textures.insert(name.to_string(), bg);
    }

    /// Bind group of an unmipped texture with a flat normal map (live pictures: the phone screen and camera).
    fn bind_plain(&self, tex: &wgpu::Texture) -> wgpu::BindGroup {
        let flat = self.upload("flat normal", &image::RgbaImage::from_pixel(1, 1, image::Rgba([128, 128, 255, 255])));
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view(tex)) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view(&flat)) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.clamp_sampler) },
            ],
        })
    }
    fn plain_texture(&self, size: (u32, u32), format: wgpu::TextureFormat, extra: wgpu::TextureUsages) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: None, size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | extra, view_formats: &[] })
    }

    /// Rewrites a CPU-drawn texture in place (created on first use, no mips).
    pub fn update_texture(&mut self, name: &str, img: &image::RgbaImage) {
        let size = img.dimensions();
        if self.dynamic.get(name).is_none_or(|d| d.1 != size) {
            let tex = self.plain_texture(size, wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureUsages::empty());
            let bg = self.bind_plain(&tex);
            self.textures.insert(name.to_string(), bg);
            self.dynamic.insert(name.to_string(), (tex, size));
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.dynamic[name].0, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            img, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * size.0), rows_per_image: Some(size.1) },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 });
    }

    pub fn upload_hud(&self, rgba: &[u8]) {
        let (w, h) = self.hud_size;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.t.hud, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    /// Draws the passes and presents. Pass 0 is the world: it also feeds the flash shadow map, SSAO and the dust.
    /// With `capture`, also returns the final image (RGBA, window size).
    fn tw(&self, pass: u32) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.prof.as_ref().map(|p| wgpu::RenderPassTimestampWrites { query_set: &p.qs, beginning_of_pass_write_index: Some(2 * pass), end_of_pass_write_index: Some(2 * pass + 1) })
    }

    /// Shadow casters among the world draws: `Some(true)` for static geometry (not `moving`, not skinned), which the
    /// lamp caches keep, `Some(false)` for moving casters redrawn every frame; unlit decals and bulbs don't cast.
    fn caster(&self, d: &DrawCmd) -> Option<bool> {
        (!d.unlit && !d.tex.starts_with("fx_")).then(|| !d.moving && !self.meshes[d.mesh].dynamic.get() && d.fog > SHADOW_ONLY + 1.0)
    }

    /// Draws `pass0` casters chosen by `keep` into a depth view from shadow camera `cam`. Returns triangles drawn.
    #[allow(clippy::too_many_arguments)]
    fn shadow_pass(&self, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, clear: bool, cam: usize, vp: Mat4, pass0: &Pass,
                   keep: impl Fn(&DrawCmd) -> bool, tw: Option<wgpu::RenderPassTimestampWrites>) -> u64 {
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow"), color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: target, depth_ops: Some(wgpu::Operations { load: if clear { wgpu::LoadOp::Clear(1.0) } else { wgpu::LoadOp::Load }, store: wgpu::StoreOp::Store }), stencil_ops: None,
            }),
            timestamp_writes: tw, occlusion_query_set: None,
        });
        rp.set_pipeline(&self.shadow_pipe);
        rp.set_bind_group(0, &self.cam_bg, &[(cam as u64 * CAM_SLOT) as u32]);
        let eye = eye_of(vp);
        let mut tris = 0;
        for (k, d) in pass0.draws.iter().enumerate().take(MAX_DRAWS as usize) {
            if !keep(d) { continue; }
            let m = &self.meshes[d.mesh];
            if !m.visible(vp * d.model) { continue; }
            rp.set_bind_group(1, &self.shadow_draw_bg, &[(k as u64 * DRAW_SLOT) as u32]);
            rp.set_vertex_buffer(0, m.vb.slice(..));
            rp.set_index_buffer(m.ib.slice(..), wgpu::IndexFormat::Uint32);
            let (a, n) = m.lod(&d.model, eye, LOD_SHADOW);
            rp.draw_indexed(a..a + n, 0, 0..1);
            tris += n as u64 / 3;
        }
        tris
    }

    /// `lamps`: per shadow slot, the lamp's camera and a key that changes when the lamp (or where it hangs) changes;
    /// a new key re-renders that slot's static cache.
    pub fn render(&mut self, passes: &[Pass], post: &PostParams, lamps: &[Option<(Mat4, u64)>; LAMP_SLOTS], capture: bool) -> Option<image::RgbaImage> {
        let t_prep = std::time::Instant::now();
        self.last_drawn = std::mem::take(&mut self.drawn);
        let mut tris = [0u64; 3];
        let fs = self.frame_size;
        if self.phone_cam && self.phone_cam_tex.as_ref().is_none_or(|t| (t.width(), t.height()) != fs) {
            let tex = self.plain_texture(fs, HDR, wgpu::TextureUsages::empty());
            self.textures.insert("phone_cam".into(), self.bind_plain(&tex));
            self.phone_cam_tex = Some(tex);
        }
        let shot = self.phone_shot.take().map(|[u0, v0, u1, v1]| {
            let px = |u: f32, n: u32| ((u.clamp(0.0, 1.0) * n as f32) as u32).min(n - 1);
            let (x0, y0) = (px(u0.min(u1), fs.0), px(v0.min(v1), fs.1));
            (x0, y0, (px(u0.max(u1), fs.0) - x0).max(1), (px(v0.max(v1), fs.1) - y0).max(1))
        });
        if let Some(r) = shot {
            let tex = self.plain_texture((r.2, r.3), HDR, wgpu::TextureUsages::COPY_SRC);
            self.textures.insert("phone_shot".into(), self.bind_plain(&tex));
            self.phone_shot_tex = Some((tex, r));
        }
        for p in passes { for d in &p.draws { self.texture(&d.tex); } }
        for (i, p) in passes.iter().enumerate() {
            let mut gl = p.globals; self.env.write(&mut gl);
            self.queue.write_buffer(&self.globals_buf, i as u64 * GLOBAL_SLOT, bytemuck::bytes_of(&gl));
        }
        let mut slot = 0u64;
        let mut data = std::mem::take(&mut self.draw_data);
        for p in passes {
            for d in &p.draws {
                if slot >= MAX_DRAWS { break; }
                // shadow-only parts (the body's arms and head) get their uniforms too: the lamp maps need them
                let u = DrawUniform {
                    model: d.model.to_cols_array_2d(),
                    color: d.color.into(),
                    // Negative x carries the authored texture texel grid; signs and FX stay sharp.
                    params: [if d.unlit {1.0} else {-self.styles.get(&d.tex).map_or(0.0,|s|s.texels)}, d.fog, d.cutoff, self.styles.get(&d.tex).map_or_else(||roughness(&d.tex),|s|s.roughness)],
                };
                let o = (slot * DRAW_SLOT) as usize;
                data[o..o + std::mem::size_of::<DrawUniform>()].copy_from_slice(bytemuck::bytes_of(&u));
                slot += 1;
            }
        }
        self.queue.write_buffer(&self.draw_buf, 0, &data[..(slot * DRAW_SLOT) as usize]);
        self.draw_data = data;
        self.queue.write_buffer(&self.post_buf, 0, bytemuck::bytes_of(post));
        let mut cams = vec![0u8; (CAM_SLOT as usize) * (1 + LAMP_SLOTS)];
        let cam = |m: Mat4| ShadowCam { view_proj: m.to_cols_array_2d(), inv_view_proj: Mat4::IDENTITY.to_cols_array_2d(), light_vp: m.to_cols_array_2d() };
        let torch_vp = passes.first().map_or(Mat4::IDENTITY, |p| Mat4::from_cols_array_2d(&p.globals.light_vp));
        for (k, m) in std::iter::once(Some(torch_vp)).chain(lamps.iter().map(|l| l.map(|l| l.0))).enumerate() {
            if let Some(m) = m { cams[k * CAM_SLOT as usize..][..std::mem::size_of::<ShadowCam>()].copy_from_slice(bytemuck::bytes_of(&cam(m))); }
        }
        self.queue.write_buffer(&self.cam_buf, 0, &cams);

        let t_acquire = std::time::Instant::now();
        // FOURBIT_OFFSCREEN=1 (benchmarks): draw into a texture and wait for the GPU, never present: no vsync and no
        // throttling of a covered window, so the frame time is the real CPU + GPU cost
        let offscreen = std::env::var_os("FOURBIT_OFFSCREEN").is_some();
        let out = if offscreen { None } else { match self.surface.get_current_texture() {
            Ok(t) => Some(t),
            Err(_) => { self.surface.configure(&self.device, &self.config); return None; }
        } };
        let off_tex = offscreen.then(|| self.device.create_texture(&wgpu::TextureDescriptor {
            label: None, size: wgpu::Extent3d { width: self.config.width, height: self.config.height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[] }));
        let t_encode = std::time::Instant::now();
        let out_view = match (&out, &off_tex) { (Some(o), _) => view(&o.texture), (None, Some(t)) => view(t), _ => unreachable!() };
        let mut enc = self.device.create_command_encoder(&Default::default());

        if let Some(p0) = passes.first() {
            // torch: everything lit except the body's own head/arms (the torch sits at the eye, they would black out the view)
            tris[0] += self.shadow_pass(&mut enc, &self.shadow_view, true, 0, torch_vp, p0, |d| !d.unlit && !d.tex.starts_with("fx_") && d.fog > SHADOW_ONLY + 1.0, self.tw(0));
            // lamps: a new lamp in a slot renders its static cache once; every frame the cache is copied and only
            // moving casters are drawn on top
            let active: Vec<usize> = (0..LAMP_SLOTS).filter(|&k| lamps[k].is_some()).collect();
            for (n, &k) in active.iter().enumerate() {
                let (vp, key) = lamps[k].unwrap();
                let (first, last) = (n == 0, n + 1 == active.len());
                let tw = self.prof.as_ref().filter(|_| first || last).map(|p| wgpu::RenderPassTimestampWrites { query_set: &p.qs,
                    beginning_of_pass_write_index: first.then_some(2), end_of_pass_write_index: last.then_some(3) });
                if self.lamp_keys[k] != Some(key) {
                    tris[1] += self.shadow_pass(&mut enc, &self.lamp_static_layers[k], true, 1 + k, vp, p0, |d| self.caster(d) == Some(true), None);
                    self.lamp_keys[k] = Some(key);
                }
                let layer = |t| wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: k as u32 }, aspect: wgpu::TextureAspect::All };
                enc.copy_texture_to_texture(layer(&self.lamp_static), layer(&self.lamp_live), wgpu::Extent3d { width: LAMP_SIZE, height: LAMP_SIZE, depth_or_array_layers: 1 });
                tris[1] += self.shadow_pass(&mut enc, &self.lamp_live_layers[k], false, 1 + k, vp, p0, |d| self.caster(d) == Some(false), tw);
            }
        }

        let (frame_v, normal_v, depth_v) = (view(&self.t.frame), view(&self.t.normal), view(&self.t.depth));
        let vm_depth_v = view(&self.t.vm_depth);
        let mut slot = 0u64;
        for (i, p) in passes.iter().enumerate() {
            let load = if p.clear_color { wgpu::LoadOp::Clear(wgpu::Color { r:self.env.fog_outdoor.x as f64,g:self.env.fog_outdoor.y as f64,b:self.env.fog_outdoor.z as f64,a:1.0 }) } else { wgpu::LoadOp::Load };
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment { view: &frame_v, resolve_target: None, ops: wgpu::Operations { load, store: wgpu::StoreOp::Store } }),
                    Some(wgpu::RenderPassColorAttachment { view: &normal_v, resolve_target: None, ops: wgpu::Operations { load, store: wgpu::StoreOp::Store } }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: if i == 0 { &depth_v } else { &vm_depth_v }, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None,
                }),
                timestamp_writes: self.tw(3 + i.min(1) as u32),
                occlusion_query_set: None,
            });
            // cut-outs and the own body need discard; everything else stays on the opaque pipeline (no discard
            // anywhere in it, so a tile GPU drops hidden fragments before shading them)
            let mut bound = u8::MAX;
            let eye = Vec3::new(p.globals.cam_pos[0], p.globals.cam_pos[1], p.globals.cam_pos[2]);
            rp.set_bind_group(0, &self.globals_bg, &[(i as u64 * GLOBAL_SLOT) as u32]);
            rp.set_bind_group(3, &self.shadow_bg, &[]);
            for d in &p.draws {
                if slot >= MAX_DRAWS { break; }
                let m = &self.meshes[d.mesh];
                if d.fog <= SHADOW_ONLY + 1.0 || !m.visible(Mat4::from_cols_array_2d(&p.globals.view_proj)*d.model) {slot+=1;continue;}
                // 0 additive flashes, 1 cut-outs / own body (discard), 2 opaque, 3 blended smoke (`fx_*`)
                let kind = if d.tex.starts_with("muzzle") { 0 } else if d.tex.starts_with("fx_") { 3 } else if d.cutoff >= 0.0 || d.fog < 0.0 { 1 } else { 2 };
                if kind != bound { rp.set_pipeline([&self.flash_pipe, &self.scene_pipe, &self.opaque_pipe, &self.smoke_pipe][kind as usize]); bound = kind; }
                rp.set_bind_group(1, &self.textures[&d.tex], &[]);
                rp.set_bind_group(2, &self.draw_bg, &[(slot * DRAW_SLOT) as u32]);
                rp.set_vertex_buffer(0, m.vb.slice(..));
                rp.set_index_buffer(m.ib.slice(..), wgpu::IndexFormat::Uint32);
                let (a, n) = if i == 0 { m.lod(&d.model, eye, LOD_VIEW) } else { (0, m.count) };
                rp.draw_indexed(a..a + n, 0, 0..1);
                tris[2] += n as u64 / 3;
                self.drawn.0 += 1; self.drawn.1 += n as u64 / 3;
                slot += 1;
            }
            drop(rp);
            // the phone's camera sees the world without the hands and the phone (no picture of its own screen)
            if i == 0 {
                fn whole(t: &wgpu::Texture, x: u32, y: u32) -> wgpu::TexelCopyTextureInfo<'_> { wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All } }
                if let Some(t) = self.phone_cam_tex.as_ref().filter(|_| self.phone_cam) {
                    enc.copy_texture_to_texture(whole(&self.t.frame, 0, 0), whole(t, 0, 0), wgpu::Extent3d { width: fs.0, height: fs.1, depth_or_array_layers: 1 });
                }
                if let (Some((t, r)), Some(_)) = (&self.phone_shot_tex, shot) {
                    enc.copy_texture_to_texture(whole(&self.t.frame, r.0, r.1), whole(t, 0, 0), wgpu::Extent3d { width: r.2, height: r.3, depth_or_array_layers: 1 });
                }
            }
        }

        // screen effects
        let fullscreen = |enc: &mut wgpu::CommandEncoder, target: &wgpu::Texture, pipe: &wgpu::RenderPipeline, groups: &[(&wgpu::BindGroup, &[u32])], tw: Option<wgpu::RenderPassTimestampWrites>| {
            let v = view(target);
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &v, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, timestamp_writes: tw, occlusion_query_set: None,
            });
            rp.set_pipeline(pipe);
            for (k, (bg, off)) in groups.iter().enumerate() { rp.set_bind_group(k as u32, *bg, off); }
            rp.draw(0..3, 0..1);
        };
        // World depth survives the weapon pass; its pixels are masked in the normal target.
        fullscreen(&mut enc, &self.t.fx, &self.fx_pipe, &[(&self.globals_bg, &[0]), (&self.fx_bg, &[])], self.tw(5));
        fullscreen(&mut enc, &self.t.bloom_a, &self.bright_pipe, &[(&self.bright_bg, &[])], self.tw(6));
        fullscreen(&mut enc, &self.t.bloom_b, &self.blur_h_pipe, &[(&self.blur_h_bg, &[])], self.tw(7));
        fullscreen(&mut enc, &self.t.bloom_a, &self.blur_v_pipe, &[(&self.blur_v_bg, &[])], self.tw(8));

        // post to the window, and to a readback texture when capturing
        let cap_tex = capture.then(|| self.device.create_texture(&wgpu::TextureDescriptor {
            label: None, size: wgpu::Extent3d { width: self.config.width, height: self.config.height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[] }));
        let mut targets = vec![out_view];
        if let Some(t) = &cap_tex { targets.push(view(t)); }
        for (k, v) in targets.iter().enumerate() {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("post"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: v, resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, timestamp_writes: if k == 0 { self.tw(9) } else { None }, occlusion_query_set: None,
            });
            rp.set_pipeline(&self.post_pipe);
            rp.set_bind_group(0, &self.post_bg, &[]);
            rp.draw(0..3, 0..1);
        }
        // the editor's panels over the finished frame (and into a capture: screenshots show the editor as it is)
        let mut ui_cmds = vec![];
        if let Some(ui) = &mut self.ui {
            for (id, d) in &ui.delta.set { ui.renderer.update_texture(&self.device, &self.queue, *id, d); }
            let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [self.config.width, self.config.height], pixels_per_point: ui.ppp };
            ui_cmds = ui.renderer.update_buffers(&self.device, &self.queue, &mut enc, &ui.jobs, &sd);
            for v in &targets {
                let rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("editor ui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: v, resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                    depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None,
                });
                ui.renderer.render(&mut rp.forget_lifetime(), &ui.jobs, &sd);
            }
        }
        if let Some(p) = &self.prof {
            enc.resolve_query_set(&p.qs, 0..STAMPS, &p.resolve, 0);
            enc.copy_buffer_to_buffer(&p.resolve, 0, &p.read, 0, 8 * STAMPS as u64);
        }
        let (w, h) = (self.config.width, self.config.height);
        let padded = (4 * w).div_ceil(256) * 256;
        let readback = cap_tex.as_ref().map(|t| {
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None, size: (padded * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            buf
        });
        let shot_buf = shot.and(self.phone_shot_tex.as_ref()).map(|(t, r)| {
            let padded = (8 * r.2).div_ceil(256) * 256;
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None, size: (padded * r.3) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(r.3) } },
                wgpu::Extent3d { width: r.2, height: r.3, depth_or_array_layers: 1 });
            (buf, padded, r.2, r.3)
        });
        let t_submit = std::time::Instant::now();
        self.queue.submit(ui_cmds.into_iter().chain([enc.finish()]));
        if let Some(ui) = &mut self.ui { for id in std::mem::take(&mut ui.delta.free) { ui.renderer.free_texture(&id); } ui.delta.set.clear(); }
        if let Some(out) = out { self.window.pre_present_notify(); out.present(); } else { self.device.poll(wgpu::Maintain::Wait); }
        if let Some(p) = &mut self.prof {
            // profiling only: waits for the GPU every frame, so the frame rate itself is not representative
            let t_done = t_submit;
            let sl = p.read.slice(..);
            sl.map_async(wgpu::MapMode::Read, |_| {});
            self.device.poll(wgpu::Maintain::Wait);
            let ts: Vec<u64> = bytemuck::cast_slice(&sl.get_mapped_range()).to_vec();
            p.read.unmap();
            for k in 0..STAMPS as usize / 2 { p.ms[k] += ts[2 * k + 1].saturating_sub(ts[2 * k]) as f64 * p.period as f64 / 1e6; }
            // passes overlap on tile GPUs and Metal's pass timestamps don't compare across passes: the frame is timed
            // from submit until the GPU is idle again
            p.frame += t_submit.elapsed().as_secs_f64() * 1000.0;
            for k in 0..3 { p.tris[k] += tris[k]; }
            for (k, (a, b)) in [(t_prep, t_acquire), (t_acquire, t_encode), (t_encode, t_done)].into_iter().enumerate() { p.cpu[k] += (b - a).as_secs_f64() * 1000.0; }
            p.frames += 1;
            if p.frames == 60 {
                let f = 60.0;
                let gpu = p.frame;
                eprintln!("gpu frame ms {:.2}, passes {} | tris k: torch {} lamps {} view {} | cpu ms prep {:.2} acquire {:.2} encode {:.2}", gpu / f,
                    STAMP_GROUPS.iter().map(|(n, ks)| format!("{n} {:.2}", ks.iter().map(|&k| p.ms[k]).sum::<f64>() / f)).collect::<Vec<_>>().join(", "),
                    p.tris[0] / 60_000, p.tris[1] / 60_000, p.tris[2] / 60_000, p.cpu[0] / f, p.cpu[1] / f, p.cpu[2] / f);
                p.ms = [0.0; STAMPS as usize]; p.frame = 0.0; p.tris = [0; 3]; p.cpu = [0.0; 3]; p.frames = 0;
            }
        }

        if let Some((buf, padded, w, h)) = shot_buf {
            let sl = buf.slice(..);
            sl.map_async(wgpu::MapMode::Read, |_| {});
            self.device.poll(wgpu::Maintain::Wait);
            let raw = sl.get_mapped_range();
            let ch = |o: usize| (half_to_f32(u16::from_le_bytes([raw[o], raw[o + 1]])).clamp(0.0, 1.0) * 255.0) as u8;
            self.phone_shot_png = Some(image::RgbaImage::from_fn(w, h, |x, y| {
                let o = (y * padded + x * 8) as usize;
                image::Rgba([ch(o), ch(o + 2), ch(o + 4), 255])
            }));
        }
        let buf = readback?;
        let sl = buf.slice(..);
        sl.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let raw = sl.get_mapped_range();
        let bgr = matches!(self.config.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
        let mut img = image::RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let o = (y * padded + x * 4) as usize;
                let (r, b) = if bgr { (raw[o + 2], raw[o]) } else { (raw[o], raw[o + 2]) };
                img.put_pixel(x, y, image::Rgba([r, raw[o + 1], b, 255]));
            }
        }
        Some(img)
    }
}

/// IEEE half -> f32 (the world target is Rgba16Float)
fn half_to_f32(h: u16) -> f32 {
    let (s, e, m) = ((h >> 15) as u32, ((h >> 10) & 0x1f) as u32, (h & 0x3ff) as u32);
    let v = if e == 0 { m as f32 * 2f32.powi(-24) } else if e == 31 { f32::INFINITY } else { f32::from_bits(((e + 112) << 23) | (m << 13)) };
    if s == 1 { -v } else { v }
}

/// Tangent-space normal map from image brightness (Sobel on a blurred luminance).
fn height_normals(img: &image::RgbaImage, strength: f32) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    if strength <= 0.0 || w < 4 || h < 4 { return image::RgbaImage::from_pixel(w.max(1), h.max(1), image::Rgba([128, 128, 255, 255])); }
    let lum: Vec<f32> = img.pixels().map(|p| (p[0] as f32 * 0.3 + p[1] as f32 * 0.59 + p[2] as f32 * 0.11) / 255.0).collect();
    let at = |x: i32, y: i32| lum[(y.rem_euclid(h as i32) as u32 * w + x.rem_euclid(w as i32) as u32) as usize];
    let mut out = image::RgbaImage::new(w, h);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let dx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
            let dy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
            let n = Vec3::new(-dx * strength, -dy * strength, 1.0).normalize();
            out.put_pixel(x as u32, y as u32, image::Rgba([((n.x * 0.5 + 0.5) * 255.0) as u8, ((n.y * 0.5 + 0.5) * 255.0) as u8, ((n.z * 0.5 + 0.5) * 255.0) as u8, 255]));
        }
    }
    out
}

/// Torch colour: the phone's LED is near-white with the yellow of its phosphor, the old chest torch a warm bulb.
pub fn torch_colour(led: bool) -> [f32; 3] { if led { [2.15, 2.05, 1.8] } else { [2.3, 1.85, 1.25] } }

/// Lights/flash/fog block for one pass. The flash shadow camera sits at the flash looking along it; `lamp_vp` are
/// the cameras of the lamp shadow slots; `led` picks the phone LED's beam over the reflector torch's.
#[allow(clippy::too_many_arguments)]
pub fn globals(view: Mat4, proj: Mat4, cam: Vec3, lights: &[LightIn], lamp_vp: &[Mat4; LAMP_SLOTS], flash_pos: Vec3, flash_dir: Vec3, flash_on: bool, led: bool, time: f32) -> Globals {
    let mut lp = [[0.0; 4]; MAX_LIGHTS];
    let mut lc = [[0.0; 4]; MAX_LIGHTS];
    let mut li = [[0.0; 4]; MAX_LIGHTS];
    let mut slot_light = [[-1.0; 4]; LAMP_SLOTS / 4];
    for (i, l) in lights.iter().take(MAX_LIGHTS).enumerate() {
        if l.slot >= 0 { slot_light[l.slot as usize / 4][l.slot as usize % 4] = i as f32; }
        lp[i] = [l.pos.x, l.pos.y, l.pos.z, l.range];
        lc[i] = [l.color.x, l.color.y, l.color.z, if l.stair { 2.0 } else { 1.0 }];
        li[i] = [l.slot as f32, l.weight, l.bounce, l.halo];
    }
    let up = if flash_dir.y.abs() > 0.95 { Vec3::Z } else { Vec3::Y };
    // 90°: the torch's dim spill reaches 45° off its axis and must not leak through walls outside the map
    let light_vp = Mat4::perspective_lh(90f32.to_radians(), 1.0, 0.05, 12.0) * Mat4::look_to_lh(flash_pos, flash_dir, up);
    let vp = proj * view;
    Globals {
        view_proj: vp.to_cols_array_2d(),
        inv_view_proj: vp.inverse().to_cols_array_2d(),
        light_vp: light_vp.to_cols_array_2d(),
        cam_pos: [cam.x, cam.y, cam.z, 1.0],
        light_pos: lp,
        light_col: lc,
        light_info: li,
        flash_pos: [flash_pos.x, flash_pos.y, flash_pos.z, 1.0],
        flash_dir: [flash_dir.x, flash_dir.y, flash_dir.z, 0.0],
        flash_col: { let [r, g, b] = torch_colour(led); [r, g, b, 1.0] },
        // range, beam (1 = phone LED, 0 = reflector torch), unused, on
        flash_params: [9.0, if led { 1.0 } else { 0.0 }, 0.0, if flash_on { 1.0 } else { 0.0 }],
        ambient: [0.05, 0.055, 0.045, 1.0],
        fog_color: [0.008, 0.009, 0.007, 1.0],
        fog_params: [2.0, 17.0, lights.len().min(MAX_LIGHTS) as f32, time],
        lamp_vp: lamp_vp.map(|m| m.to_cols_array_2d()),
        slot_light,
        zone_min: [[0.0; 4]; 4], zone_max: [[0.0; 4]; 4], zones: [0.0; 4], sky_amb: [0.0; 4], key_dir: [0.0; 4], key_col: [0.0; 4],
        in_amb: [0.0; 4], fog_out: [0.0; 4], fog_in: [0.0; 4], fog_end: [0.0; 4],
    }
}
