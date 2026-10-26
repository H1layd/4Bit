//! Model preview: a .wmdl rendered into an offscreen texture that egui shows; CPU-skinned clips, orbit camera.
use fourbit::model::Model;
use glam::{Mat4, Quat, Vec3};
use std::path::{Path, PathBuf};
use wgpu::util::DeviceExt;

pub const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vert { pos: [f32; 3], normal: [f32; 3], uv: [f32; 2] }

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms { view_proj: [[f32; 4]; 4], light: [f32; 4], eye: [f32; 4] }

const SHADER: &str = r#"
struct U { view_proj: mat4x4<f32>, light: vec4<f32>, eye: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
struct V { @builtin(position) clip: vec4<f32>, @location(0) n: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) p: vec3<f32> };
@vertex fn vs(@location(0) pos: vec3<f32>, @location(1) n: vec3<f32>, @location(2) uv: vec2<f32>) -> V {
    var o: V; o.clip = u.view_proj * vec4(pos, 1.0); o.n = n; o.uv = vec2(uv.x, 1.0 - uv.y); o.p = pos; return o;
}
@fragment fn fs(i: V, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    // Source diffuse alpha is usually a gloss mask, not cut-out
    let c = textureSample(tex, samp, i.uv);
    var n = normalize(i.n); if (!front) { n = -n; }
    let l = normalize(u.light.xyz);
    let key = max(dot(n, l), 0.0);
    let rim = pow(1.0 - max(dot(n, normalize(u.eye.xyz - i.p)), 0.0), 3.0) * 0.25;
    let sky = 0.32 + 0.18 * n.y;
    // textures and lighting are gamma-space (like the game); egui expects linear values from a native texture
    return vec4(pow(clamp(c.rgb * (key * 0.85 + sky) + rim, vec3(0.0), vec3(1.0)), vec3(2.2)), 1.0);
}
"#;

struct Part { verts: Vec<fourbit::model::SkinVert>, buf: wgpu::Buffer, idx: wgpu::Buffer, count: u32, tex: wgpu::BindGroup, pub name: String, pub shown: bool }

pub struct Preview {
    pipeline: wgpu::RenderPipeline,
    tex_layout: wgpu::BindGroupLayout,
    ubuf: wgpu::Buffer,
    ugroup: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    pub color: Option<(wgpu::Texture, wgpu::TextureView, wgpu::Texture, wgpu::TextureView, [u32; 2])>,
    pub egui_tex: Option<egui::TextureId>,
    pub model: Option<Loaded>,
    pub yaw: f32, pub pitch: f32, pub dist: f32,
    /// the last frame's camera (for drawing the skeleton over the picture)
    pub view_proj: Mat4,
}

pub struct Loaded {
    pub path: PathBuf,
    pub model: Model,
    parts: Vec<Part>,
    inv_bind: Vec<Mat4>,
    pub centre: Vec3,
    pub radius: f32,
    pub clip: Option<usize>,
    pub time: f32,
    pub playing: bool,
    pub speed: f32,
    dirty: bool,
    /// frame the camera on the next skinned pose (viewmodels are bound far from where their clips hold them)
    fit: bool,
    /// bone world matrices of the pose on screen
    pub pose: Vec<Mat4>,
}
impl Loaded {
    pub fn parts(&mut self) -> impl Iterator<Item = (&String, &mut bool)> { self.parts.iter_mut().map(|p| (&p.name, &mut p.shown)) }
    pub fn set_clip(&mut self, c: Option<usize>) { self.clip = c; self.time = 0.0; self.dirty = true; self.fit = true; }
    pub fn scrub(&mut self, t: f32) { self.time = t; self.dirty = true; }
    pub fn tris(&self) -> u32 { self.parts.iter().map(|p| p.count / 3).sum() }
    /// The clips were edited: redraw the pose (and keep the clip index in range).
    pub fn touched(&mut self) { if self.clip.is_some_and(|c| c >= self.model.clips.len()) { self.clip = None; } self.dirty = true; }
}

impl Preview {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("preview"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let ulayout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }] });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None }] });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&ulayout, &tex_layout], push_constant_ranges: &[] });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("preview"), layout: Some(&layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vert>() as u64, step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2] }] },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: COLOR, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH, depth_write_enabled: true, depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(), multiview: None, cache: None,
        });
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let ugroup = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &ulayout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() }] });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat, address_mode_v: wgpu::AddressMode::Repeat, ..Default::default() });
        Preview { pipeline, tex_layout, ubuf, ugroup, sampler, color: None, egui_tex: None, model: None, view_proj: Mat4::IDENTITY, yaw: 200.0, pitch: 12.0, dist: 1.0 }
    }

    pub fn load(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, path: &Path) -> Result<(), String> {
        let model = Model::load(path)?;
        let dir = path.parent().unwrap();
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        let mut parts = Vec::new();
        for m in &model.meshes {
            for v in &m.verts { lo = lo.min(v.pos); hi = hi.max(v.pos); }
            let img = image::open(dir.join(&m.texture)).map(|i| i.to_rgba8()).unwrap_or_else(|_| image::RgbaImage::from_pixel(2, 2, image::Rgba([190, 190, 185, 255])));
            let tex = device.create_texture_with_data(queue, &wgpu::TextureDescriptor { label: None,
                size: wgpu::Extent3d { width: img.width(), height: img.height(), depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
                dimension: wgpu::TextureDimension::D2, format: COLOR, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] },
                wgpu::util::TextureDataOrder::LayerMajor, &img);
            let view = tex.create_view(&Default::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &self.tex_layout, entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }] });
            let verts: Vec<Vert> = m.verts.iter().map(|v| Vert { pos: v.pos.into(), normal: v.normal.into(), uv: v.uv.into() }).collect();
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST });
            let idx = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&m.indices), usage: wgpu::BufferUsages::INDEX });
            let name = if m.material.is_empty() { m.body.clone() } else { format!("{} · {}", m.body, m.material) };
            parts.push(Part { verts: m.verts.clone(), buf, idx, count: m.indices.len() as u32, tex: group, name, shown: true });
        }
        if parts.is_empty() { lo = Vec3::ZERO; hi = Vec3::ONE * 0.1; }
        let inv_bind = model.inverse_bind();
        let centre = (lo + hi) * 0.5;
        let radius = ((hi - lo).length() * 0.5).max(0.01);
        let clip = model.clip("idle").or(if model.clips.is_empty() { None } else { Some(0) });
        self.dist = radius * 2.6;
        self.model = Some(Loaded { path: path.to_path_buf(), model, parts, inv_bind, centre, radius, clip, time: 0.0, playing: clip.is_some(), speed: 1.0, dirty: true, fit: true, pose: vec![] });
        Ok(())
    }

    /// Advance the clip and CPU-skin the meshes into their vertex buffers.
    pub fn animate(&mut self, queue: &wgpu::Queue, dt: f32) {
        let Some(l) = &mut self.model else { return };
        let Some(c) = l.clip.filter(|&c| c < l.model.clips.len()) else {
            if l.dirty {
                l.dirty = false;
                for p in &l.parts { write_rest(queue, p); }
                if std::mem::take(&mut l.fit) {
                    let (lo, hi) = bounds(l.parts.iter().flat_map(|p| p.verts.iter().map(|v| v.pos)));
                    l.centre = (lo + hi) * 0.5; l.radius = ((hi - lo).length() * 0.5).max(0.01); self.dist = l.radius * 2.6;
                }
            }
            return;
        };
        let len = l.model.clips[c].length().max(1e-3);
        if l.playing { l.time = (l.time + dt * l.speed).rem_euclid(len); l.dirty = true; }
        if !l.dirty { return; }
        l.dirty = false;
        let mut pose = l.model.rest_local();
        l.model.clips[c].sample(l.time, &mut pose);
        let world = l.model.world(&pose);
        l.pose = world.clone();
        let skin: Vec<Mat4> = world.iter().zip(&l.inv_bind).map(|(w, i)| *w * *i).collect();
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &l.parts {
            let verts: Vec<Vert> = p.verts.iter().map(|v| {
                let mut m = Mat4::ZERO;
                for k in 0..4 { if v.weights[k] > 0.0 { m += skin.get(v.joints[k] as usize).copied().unwrap_or(Mat4::IDENTITY) * v.weights[k]; } }
                if m == Mat4::ZERO { m = Mat4::IDENTITY; }
                Vert { pos: m.transform_point3(v.pos).into(), normal: m.transform_vector3(v.normal).normalize_or_zero().into(), uv: v.uv.into() }
            }).collect();
            if l.fit { for v in &verts { lo = lo.min(Vec3::from(v.pos)); hi = hi.max(Vec3::from(v.pos)); } }
            queue.write_buffer(&p.buf, 0, bytemuck::cast_slice(&verts));
        }
        if std::mem::take(&mut l.fit) && lo.x <= hi.x {
            l.centre = (lo + hi) * 0.5; l.radius = ((hi - lo).length() * 0.5).max(0.01); self.dist = l.radius * 2.6;
        }
    }

    /// Render into a target of `size` pixels; returns true when the texture was (re)created and egui must re-register it.
    pub fn render(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, enc: &mut wgpu::CommandEncoder, size: [u32; 2]) -> bool {
        let size = [size[0].clamp(16, 4096), size[1].clamp(16, 4096)];
        let mut fresh = false;
        if self.color.as_ref().map(|c| c.4) != Some(size) {
            let mk = |format, usage| device.create_texture(&wgpu::TextureDescriptor { label: None,
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
                dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] });
            let c = mk(COLOR, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
            let d = mk(DEPTH, wgpu::TextureUsages::RENDER_ATTACHMENT);
            let (cv, dv) = (c.create_view(&Default::default()), d.create_view(&Default::default()));
            self.color = Some((c, cv, d, dv, size));
            fresh = true;
        }
        let (centre, radius) = self.model.as_ref().map_or((Vec3::ZERO, 1.0), |l| (l.centre, l.radius));
        let rot = Quat::from_euler(glam::EulerRot::YXZ, self.yaw.to_radians(), self.pitch.to_radians(), 0.0);
        let eye = centre - rot * Vec3::Z * self.dist;
        let view = Mat4::look_at_lh(eye, centre, Vec3::Y);
        let proj = Mat4::perspective_lh(40f32.to_radians(), size[0] as f32 / size[1] as f32, (self.dist - radius * 2.0).max(radius * 0.01).max(0.001), self.dist + radius * 4.0);
        self.view_proj = proj * view;
        let light = (rot * Vec3::new(-0.5, 0.8, -0.6)).normalize();
        let u = Uniforms { view_proj: (proj * view).to_cols_array_2d(), light: light.extend(0.0).into(), eye: eye.extend(1.0).into() };
        queue.write_buffer(&self.ubuf, 0, bytemuck::bytes_of(&u));
        let (_, cv, _, dv, _) = self.color.as_ref().unwrap();
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("preview"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: cv, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.62, g: 0.65, b: 0.70, a: 1.0 }), store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: dv,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
            timestamp_writes: None, occlusion_query_set: None });
        if let Some(l) = &self.model {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.ugroup, &[]);
            for p in l.parts.iter().filter(|p| p.shown) {
                pass.set_bind_group(1, &p.tex, &[]);
                pass.set_vertex_buffer(0, p.buf.slice(..));
                pass.set_index_buffer(p.idx.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..p.count, 0, 0..1);
            }
        }
        fresh
    }
}

fn bounds(it: impl Iterator<Item = Vec3>) -> (Vec3, Vec3) {
    it.fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(p), b.max(p)))
}

fn write_rest(queue: &wgpu::Queue, p: &Part) {
    let verts: Vec<Vert> = p.verts.iter().map(|v| Vert { pos: v.pos.into(), normal: v.normal.into(), uv: v.uv.into() }).collect();
    queue.write_buffer(&p.buf, 0, bytemuck::cast_slice(&verts));
}
