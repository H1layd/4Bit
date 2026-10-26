//! Model import into the engine's `.wmdl` (+ png textures next to it):
//! - **Source / Garry's Mod** `.mdl` (needs `.vvd` + `.vtx` beside it, `materials/` under the addon root),
//! - **glTF 2.0** `.gltf` / `.glb` (Blender, Sketchfab, Poly Haven, Mixamo via Blender): meshes, skeleton, weights,
//!   animations, base colour textures.
//! glTF is right-handed with -X right; the engine is left-handed with +X right (Unity's axes): X is mirrored and the
//! triangle winding flipped.
use crate::model::{BoneDef, Clip, Model, SkinMesh, SkinVert};
use glam::{Mat4, Quat, Vec2, Vec3};
use std::path::{Path, PathBuf};

/// What an import made (printed by the tools, returned as JSON by `4bit import --json` and the MCP server).
#[derive(Clone, Debug, Default)]
pub struct Report { pub output: PathBuf, pub bones: usize, pub meshes: Vec<(String, usize, String)>, pub clips: Vec<(String, usize, f32)>, pub warnings: Vec<String> }
impl Report {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"output": self.output, "bones": self.bones,
            "meshes": self.meshes.iter().map(|m| serde_json::json!({"name": m.0, "verts": m.1, "texture": m.2})).collect::<Vec<_>>(),
            "clips": self.clips.iter().map(|c| serde_json::json!({"name": c.0, "frames": c.1, "seconds": c.2})).collect::<Vec<_>>(),
            "warnings": self.warnings})
    }
    fn of(model: &Model, output: PathBuf, warnings: Vec<String>) -> Report {
        Report { output, bones: model.bones.len(), meshes: model.meshes.iter().map(|m| (format!("{} {}", m.body, m.material).trim().to_string(), m.verts.len(), m.texture.clone())).collect(),
                 clips: model.clips.iter().map(|c| (c.name.clone(), c.frames.len(), c.length())).collect(), warnings }
    }
}

/// Imports any supported file by its extension into `out_dir/<name>.wmdl`.
pub fn any(file: &Path, out_dir: &Path, name: &str, keep: &[String]) -> Result<Report, String> {
    match file.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("mdl") => source(file, out_dir, name, keep),
        Some("gltf") | Some("glb") => gltf(file, out_dir, name),
        e => Err(format!("не умею импортировать {:?} (есть: .mdl, .gltf, .glb)", e.unwrap_or(""))),
    }
}

// ---------------------------------------------------------------- Source
fn find_ci(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = root.to_path_buf();
    for part in rel.replace('\\', "/").split('/').filter(|p| !p.is_empty()) {
        let direct = cur.join(part);
        if direct.exists() { cur = direct; continue; }
        let low = part.to_lowercase();
        cur = std::fs::read_dir(&cur).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == low))?;
    }
    Some(cur)
}
fn vmt_basetexture(vmt: &str) -> Option<String> {
    for line in vmt.lines() {
        let l = line.trim();
        if l.to_lowercase().trim_start_matches('"').starts_with("$basetexture") {
            let rest = l.splitn(2, |c: char| c.is_whitespace()).nth(1)?.trim();
            return Some(rest.split("//").next()?.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Source `.mdl` → `.wmdl`. `keep`: bodygroup substrings to keep (empty = all).
pub fn source(mdl: &Path, out: &Path, name: &str, keep: &[String]) -> Result<Report, String> {
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let src = crate::source::mdl::load(mdl)?;
    let keep: Vec<String> = keep.iter().map(|k| k.to_lowercase()).collect();
    let keep_fn = |body: &str| keep.is_empty() || keep.iter().any(|k| body.to_lowercase().contains(k));
    let mut model = Model::from_source(&src, &keep_fn);
    let mut warnings = vec![];
    // the addon root is the folder holding both models/ and materials/
    let root = mdl.ancestors().find(|p| p.join("materials").is_dir());
    for m in &mut model.meshes {
        let Some(root) = root else { warnings.push("нет папки materials/ над моделью: без текстур".into()); break };
        let found = src.cdmaterials.iter().find_map(|cd| {
            let base = format!("materials/{}/{}", cd.trim_end_matches(['/', '\\']), m.material);
            find_ci(root, &format!("{base}.vmt")).map(|p| (p, true)).or_else(|| find_ci(root, &format!("{base}.vtf")).map(|p| (p, false)))
        });
        let vtf = match found {
            Some((p, true)) => std::fs::read_to_string(&p).ok().and_then(|t| vmt_basetexture(&t)).and_then(|bt| find_ci(root, &format!("materials/{bt}.vtf"))),
            Some((p, false)) => Some(p),
            None => None,
        };
        let Some(vtf) = vtf else { warnings.push(format!("нет текстуры для материала {}", m.material)); continue };
        let file = format!("{}.png", vtf.file_stem().unwrap().to_string_lossy().to_lowercase());
        if !out.join(&file).exists() {
            match crate::source::vtf::load(&vtf) {
                Ok(mut img) => {
                    while img.width() > 1024 { img = image::imageops::resize(&img, img.width() / 2, img.height() / 2, image::imageops::FilterType::Triangle); }
                    img.save(out.join(&file)).map_err(|e| e.to_string())?;
                }
                Err(e) => { warnings.push(format!("{}: {e}", vtf.display())); continue; }
            }
        }
        m.texture = file;
    }
    for c in &model.clips {
        let bad: Vec<_> = model.bones.iter().enumerate().filter(|(i, b)| b.pos.length() > 1e-3 && c.frames.iter().any(|f| f[*i].0.length() < 1e-5)).map(|(_, b)| b.name.as_str()).collect();
        if !bad.is_empty() { warnings.push(format!("клип {}: обнулённые кости {:?}", c.name, bad)); }
    }
    let path = out.join(format!("{name}.wmdl"));
    model.save(&path).map_err(|e| e.to_string())?;
    Ok(Report::of(&model, path, warnings))
}

// ---------------------------------------------------------------- glTF
fn mirror_v(v: [f32; 3]) -> Vec3 { Vec3::new(-v[0], v[1], v[2]) }
fn mirror_q(q: [f32; 4]) -> Quat { Quat::from_xyzw(q[0], -q[1], -q[2], q[3]).normalize() }
fn mirror_m(m: Mat4) -> Mat4 { let s = Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)); s * m * s }

/// glTF / GLB → `.wmdl`. Skinned meshes keep their skeleton (the first skin) and every animation (sampled at 30 fps);
/// static meshes are baked in their node transforms. Scale lives in the vertices (bones carry position + rotation).
pub fn gltf(file: &Path, out: &Path, name: &str) -> Result<Report, String> {
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let (doc, buffers, images) = gltf::import(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut warnings = vec![];
    let scene = doc.default_scene().or_else(|| doc.scenes().next()).ok_or("в файле нет сцены")?;
    // world matrix of every node (mirrored into the engine's axes)
    let mut world = vec![Mat4::IDENTITY; doc.nodes().count()];
    fn walk(n: gltf::Node, parent: Mat4, world: &mut [Mat4]) {
        let m = parent * Mat4::from_cols_array_2d(&n.transform().matrix());
        world[n.index()] = m;
        for c in n.children() { walk(c, m, world); }
    }
    for n in scene.nodes() { walk(n, Mat4::IDENTITY, &mut world); }
    // skeleton: the first skin's joints, parents among them
    let skin = doc.skins().next();
    let joints: Vec<usize> = skin.as_ref().map(|s| s.joints().map(|j| j.index()).collect()).unwrap_or_default();
    let mut parent_of = vec![None; doc.nodes().count()];
    for n in doc.nodes() { for c in n.children() { parent_of[c.index()] = Some(n.index()); } }
    let mut bones: Vec<BoneDef> = joints.iter().map(|&j| {
        let n = doc.nodes().nth(j).unwrap();
        let (t, r, _) = n.transform().decomposed();
        let parent = parent_of[j].and_then(|p| joints.iter().position(|&x| x == p)).map_or(-1, |p| p as i32);
        // a root joint carries the transforms of the nodes above it
        let (pos, rot) = if parent < 0 {
            let m = mirror_m(world[j]);
            let (_, r, t) = m.to_scale_rotation_translation();
            (t, r)
        } else { (mirror_v(t), mirror_q(r)) };
        BoneDef { name: n.name().map(String::from).unwrap_or_else(|| format!("joint_{j}")), parent, pos, rot }
    }).collect();
    if bones.is_empty() { bones.push(BoneDef { name: "root".into(), parent: -1, pos: Vec3::ZERO, rot: Quat::IDENTITY }); }
    // bones must come after their parents (the engine walks them in order)
    let mut order: Vec<usize> = vec![];
    while order.len() < bones.len() {
        let before = order.len();
        for i in 0..bones.len() { if !order.contains(&i) && (bones[i].parent < 0 || order.contains(&(bones[i].parent as usize))) { order.push(i); } }
        if order.len() == before { return Err("скелет с циклом".into()); }
    }
    let remap: Vec<usize> = (0..bones.len()).map(|i| order.iter().position(|&o| o == i).unwrap()).collect();
    let bones: Vec<BoneDef> = order.iter().map(|&i| { let mut b = bones[i].clone(); if b.parent >= 0 { b.parent = remap[b.parent as usize] as i32; } b }).collect();
    let joint_bone: Vec<usize> = (0..joints.len()).map(|k| remap[k]).collect();
    // rest pose in model space, to place skinned vertices
    let mut model = Model { bones, meshes: vec![], clips: vec![] };
    let rest = model.world(&model.rest_local());
    // skin matrices of the rest pose: joint rest (engine space) x inverse bind (mirrored) take a bind-pose vertex into
    // the model's rest pose, whatever pose the file was bound in
    let ibm: Vec<Mat4> = skin.as_ref().map(|s| {
        let r = s.reader(|b| Some(&buffers[b.index()]));
        r.read_inverse_bind_matrices().map(|m| m.map(|m| mirror_m(Mat4::from_cols_array_2d(&m))).collect()).unwrap_or_else(|| vec![Mat4::IDENTITY; joints.len()])
    }).unwrap_or_default();
    let bind: Vec<Mat4> = (0..joints.len()).map(|k| rest[joint_bone[k]] * ibm.get(k).copied().unwrap_or(Mat4::IDENTITY)).collect();
    let mut tex_saved: std::collections::HashMap<usize, String> = Default::default();
    for node in doc.nodes() {
        let Some(mesh) = node.mesh() else { continue };
        let skinned = node.skin().is_some() && !joints.is_empty();
        let place = mirror_m(world[node.index()]);
        for (pi, prim) in mesh.primitives().enumerate() {
            if prim.mode() != gltf::mesh::Mode::Triangles { warnings.push(format!("{}: не треугольники, пропущено", mesh.name().unwrap_or("mesh"))); continue; }
            let r = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(pos) = r.read_positions() else { continue };
            let pos: Vec<[f32; 3]> = pos.collect();
            let nrm: Vec<[f32; 3]> = r.read_normals().map(|n| n.collect()).unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; pos.len()]);
            let uv: Vec<[f32; 2]> = r.read_tex_coords(0).map(|t| t.into_f32().collect()).unwrap_or_else(|| vec![[0.0, 0.0]; pos.len()]);
            let js: Vec<[u16; 4]> = r.read_joints(0).map(|j| j.into_u16().collect()).unwrap_or_else(|| vec![[0; 4]; pos.len()]);
            let ws: Vec<[f32; 4]> = r.read_weights(0).map(|w| w.into_f32().collect()).unwrap_or_else(|| vec![[1.0, 0.0, 0.0, 0.0]; pos.len()]);
            let mut indices: Vec<u32> = r.read_indices().map(|i| i.into_u32().collect()).unwrap_or_else(|| (0..pos.len() as u32).collect());
            // mirroring X turns the faces inside out: swap two corners back
            for t in indices.chunks_exact_mut(3) { t.swap(1, 2); }
            let verts: Vec<SkinVert> = (0..pos.len()).map(|i| {
                let (p, n) = (mirror_v(pos[i]), mirror_v(nrm[i]));
                if skinned {
                    let joints4 = js[i].map(|j| joint_bone.get(j as usize).copied().unwrap_or(0) as u16);
                    let sum: f32 = ws[i].iter().sum::<f32>().max(1e-6);
                    let w = ws[i].map(|w| w / sum);
                    let mut m = Mat4::ZERO;
                    for k in 0..4 { if w[k] > 0.0 { m += bind.get(js[i][k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[k]; } }
                    if m == Mat4::ZERO { m = Mat4::IDENTITY; }
                    SkinVert { pos: m.transform_point3(p), normal: m.transform_vector3(n).normalize_or_zero(), uv: Vec2::new(uv[i][0], 1.0 - uv[i][1]), joints: joints4, weights: w }
                } else {
                    SkinVert { pos: place.transform_point3(p), normal: place.transform_vector3(n).normalize_or_zero(), uv: Vec2::new(uv[i][0], 1.0 - uv[i][1]), joints: [0; 4], weights: [1.0, 0.0, 0.0, 0.0] }
                }
            }).collect();
            // base colour texture → png beside the model
            let mut texture = String::new();
            if let Some(t) = prim.material().pbr_metallic_roughness().base_color_texture() {
                let src = t.texture().source().index();
                if let Some(f) = tex_saved.get(&src) { texture = f.clone(); } else if let Some(img) = images.get(src) {
                    let f = format!("{name}_{src}.png");
                    match to_rgba(img) { Some(i) => { let mut i = i; while i.width() > 2048 { i = image::imageops::resize(&i, i.width() / 2, i.height() / 2, image::imageops::FilterType::Triangle); } i.save(out.join(&f)).map_err(|e| e.to_string())?; tex_saved.insert(src, f.clone()); texture = f; }
                                   None => warnings.push(format!("текстура {src}: формат не поддержан")) }
                }
            } else if let Some(c) = Some(prim.material().pbr_metallic_roughness().base_color_factor()) {
                // a flat colour: a tiny texture of it
                let f = format!("{name}_c{}.png", prim.material().index().unwrap_or(pi));
                if !out.join(&f).exists() { image::RgbaImage::from_pixel(4, 4, image::Rgba(c.map(|x| (x.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8))).save(out.join(&f)).map_err(|e| e.to_string())?; }
                texture = f;
            }
            model.meshes.push(SkinMesh { body: mesh.name().unwrap_or("mesh").into(), material: prim.material().name().unwrap_or("").into(), texture, verts, indices });
        }
    }
    if model.meshes.is_empty() { return Err("в файле нет треугольных мешей".into()); }
    // animations: each sampled at 30 fps for the skeleton (channels on other nodes are ignored)
    for anim in doc.animations() {
        let mut tracks: Vec<(usize, Vec<f32>, Vec<Vec3>, bool)> = vec![];   // bone, times, values (pos) or rotation flag
        let mut rots: Vec<(usize, Vec<f32>, Vec<Quat>)> = vec![];
        let mut len = 0.0f32;
        for ch in anim.channels() {
            let Some(k) = joints.iter().position(|&j| j == ch.target().node().index()) else { continue };
            let bone = joint_bone[k];
            let r = ch.reader(|b| Some(&buffers[b.index()]));
            let Some(times) = r.read_inputs() else { continue };
            let times: Vec<f32> = times.collect();
            len = len.max(*times.last().unwrap_or(&0.0));
            match r.read_outputs() {
                Some(gltf::animation::util::ReadOutputs::Translations(t)) => tracks.push((bone, times, t.map(mirror_v).collect(), false)),
                Some(gltf::animation::util::ReadOutputs::Rotations(q)) => rots.push((bone, times, q.into_f32().map(mirror_q).collect())),
                _ => {}
            }
        }
        if tracks.is_empty() && rots.is_empty() { continue; }
        let fps = 30.0;
        let n = ((len * fps).ceil() as usize + 1).max(2);
        let rest_local = model.rest_local();
        let mut frames = vec![rest_local.clone(); n];
        let at = |times: &[f32], t: f32| -> (usize, usize, f32) {
            let i = times.partition_point(|&x| x <= t).max(1) - 1;
            let j = (i + 1).min(times.len() - 1);
            let k = if j > i { ((t - times[i]) / (times[j] - times[i]).max(1e-6)).clamp(0.0, 1.0) } else { 0.0 };
            (i, j, k)
        };
        for (f, frame) in frames.iter_mut().enumerate() {
            let t = f as f32 / fps;
            for (bone, times, vals, _) in &tracks {
                let (i, j, k) = at(times, t);
                // a root bone also carries its parents' transform (see the skeleton above): keep the rest offset frame
                frame[*bone].0 = if model.bones[*bone].parent < 0 { rest_local[*bone].0 + (vals[i].lerp(vals[j], k) - vals[0]) } else { vals[i].lerp(vals[j], k) };
            }
            for (bone, times, vals) in &rots {
                let (i, j, k) = at(times, t);
                let q = vals[i].slerp(vals[j], k);
                frame[*bone].1 = if model.bones[*bone].parent < 0 { rest_local[*bone].1 * vals[0].inverse() * q } else { q };
            }
        }
        model.clips.push(Clip { name: anim.name().map(String::from).unwrap_or_else(|| format!("clip{}", anim.index())), fps, looping: true, frames });
    }
    let path = out.join(format!("{name}.wmdl"));
    model.save(&path).map_err(|e| e.to_string())?;
    Ok(Report::of(&model, path, warnings))
}

fn to_rgba(img: &gltf::image::Data) -> Option<image::RgbaImage> {
    use gltf::image::Format;
    let (w, h) = (img.width, img.height);
    let px: Vec<u8> = match img.format {
        Format::R8G8B8A8 => img.pixels.clone(),
        Format::R8G8B8 => img.pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        Format::R8G8 => img.pixels.chunks_exact(2).flat_map(|p| [p[0], p[1], 0, 255]).collect(),
        Format::R8 => img.pixels.iter().flat_map(|&p| [p, p, p, 255]).collect(),
        _ => return None,
    };
    image::RgbaImage::from_raw(w, h, px)
}
