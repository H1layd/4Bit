//! Analytic two-bone IK. The pole chooses the bend plane; lengths never change.
use glam::Vec3;

pub fn joint(root: Vec3, target: Vec3, pole: Vec3, upper: f32, lower: f32) -> (Vec3, Vec3) {
    let upper = upper.max(1e-4);
    let lower = lower.max(1e-4);
    let delta = target - root;
    let axis = delta.try_normalize().unwrap_or(Vec3::NEG_Y);
    let d = delta.length().clamp((upper - lower).abs() + 1e-5, upper + lower - 1e-5);
    let along = (upper * upper - lower * lower + d * d) / (2.0 * d);
    let height = (upper * upper - along * along).max(0.0).sqrt();
    let hint = pole - root;
    let bend = (hint - axis * hint.dot(axis)).try_normalize().unwrap_or_else(|| {
        axis.cross(if axis.y.abs() < 0.9 { Vec3::Y } else { Vec3::X }).normalize()
    });
    (root + axis * along + bend * height, root + axis * d)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_lengths_and_reaches_target() {
        let target = Vec3::new(0.2, -0.35, 0.25);
        let (elbow, end) = joint(Vec3::ZERO, target, Vec3::NEG_Y, 0.3, 0.29);
        assert!((elbow.length() - 0.3).abs() < 1e-5);
        assert!((elbow.distance(end) - 0.29).abs() < 1e-5);
        assert!(end.distance(target) < 1e-5);
    }
    #[test]
    fn unreachable_and_degenerate_targets_stay_finite() {
        for target in [Vec3::ZERO, Vec3::Y * 10.0, Vec3::Y * 0.2] {
            let (k, end) = joint(Vec3::ZERO, target, Vec3::Y, 0.45, 0.42);
            assert!(k.is_finite() && end.is_finite());
            assert!((k.length() - 0.45).abs() < 1e-4);
            assert!((k.distance(end) - 0.42).abs() < 1e-4);
        }
    }
}

/// Twist helpers have their own bind axes; wrist skinning must meet the hand exactly.
pub fn twist(forearm: glam::Mat4, hand: glam::Mat4, inv_bind: [glam::Mat4; 3], share: f32) -> glam::Mat4 {
    let [helper_inv, forearm_inv, hand_inv] = inv_bind;
    let rest = helper_inv.inverse();
    let (_, a, _) = (forearm_inv * rest).to_scale_rotation_translation();
    let (_, b, _) = (hand_inv * rest).to_scale_rotation_translation();
    let (_, fa, _) = forearm.to_scale_rotation_translation();
    let (_, ha, _) = hand.to_scale_rotation_translation();
    let pos = forearm.w_axis.truncate().lerp(hand.w_axis.truncate(), share);
    glam::Mat4::from_rotation_translation((fa * a).slerp(ha * b, share), pos)
}

#[cfg(test)]
mod wrist_tests {
    use super::*;
    use glam::{Mat4, Quat};
    #[test]
    fn citizen_wrist_and_hand_share_the_same_skin_transform() {
        let Ok(m) = crate::model::Model::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/models/arms/arms.wmdl")) else { return };
        let rest = m.world(&m.rest_local());
        let inv = m.inverse_bind();
        for side in ["L","R"] {
            let [f,h,w] = ["Forearm","Hand","Wrist"].map(|n| m.bone(&format!("ValveBiped.Bip01_{side}_{n}")).unwrap());
            let hand = Mat4::from_rotation_translation(Quat::from_rotation_y(1.4) * Quat::from_rotation_z(-0.7),Vec3::new(0.1,-0.2,0.4));
            let helper = twist(rest[f],hand,[inv[w],inv[f],inv[h]],1.0);
            let actual = (helper * inv[w]).to_cols_array();
            let expected = (hand * inv[h]).to_cols_array();
            for (a,b) in actual.into_iter().zip(expected) { assert!((a-b).abs() < 1e-4, "{side}: cuff separated from hand"); }
        }
    }
}
