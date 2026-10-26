//! glTF import: a generated 2 m bar on two joints (root at x = +1 in glTF), a texture, a "bend" clip.
use fourbit::model::Model;
use glam::Vec3;

#[test]
fn skinned_gltf_comes_in_mirrored_with_its_skeleton_and_clip() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/bar.gltf");
    let out = std::env::temp_dir().join(format!("4bit-gltf-{}", std::process::id()));
    let r = fourbit::import::any(&src, &out, "bar", &[]).expect("import");
    assert_eq!(r.bones, 2);
    assert_eq!(r.clips.len(), 1);
    assert_eq!(r.clips[0].0, "bend");
    let m = Model::load(&r.output).unwrap();
    assert!(!m.meshes[0].texture.is_empty() && out.join(&m.meshes[0].texture).exists(), "texture written");
    // rest pose: the bar stands at x = -1 (glTF +1 mirrored), from y 0 to 2
    let (lo, hi) = m.meshes[0].verts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), v| (a.min(v.pos), b.max(v.pos)));
    assert!((lo.x + 1.1).abs() < 0.02 && (hi.x + 0.9).abs() < 0.02, "x {lo} {hi}");
    assert!(lo.y.abs() < 1e-3 && (hi.y - 2.0).abs() < 1e-3, "y {lo} {hi}");
    let w = m.world(&m.rest_local());
    assert!((w[1].w_axis.truncate() - Vec3::new(-1.0, 1.0, 0.0)).length() < 1e-3, "tip joint {:?}", w[1].w_axis);
    // half way through "bend" the tip joint is turned 90°: the top of the bar leaves the vertical
    let mut pose = m.rest_local();
    m.clips[0].sample(0.5, &mut pose);
    let w2 = m.world(&pose);
    let inv = m.inverse_bind();
    let top = m.meshes[0].verts.iter().max_by(|a, b| a.pos.y.total_cmp(&b.pos.y)).unwrap();
    let mut mat = glam::Mat4::ZERO;
    for k in 0..4 { mat += w2[top.joints[k] as usize] * inv[top.joints[k] as usize] * top.weights[k]; }
    let p = mat.transform_point3(top.pos);
    assert!(p.y < 1.3 && (p.x + 1.0).abs() > 0.7, "bent top {p}");
    std::fs::remove_dir_all(&out).ok();
}
