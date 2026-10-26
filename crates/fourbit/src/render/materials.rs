//! The texture pack redirects albedos without changing model UVs or collision material names.
use std::{collections::HashMap, path::Path};

pub struct Style {
    pub path: String,
    pub roughness: f32,
    pub texels: f32,
}

pub fn load(assets: &Path) -> HashMap<String, Style> {
    let mut styles=HashMap::new();
    // optional: assets/textures/materials.tsv (or a pack's own folder): name, albedo path, roughness, texel grid
    let Some(text)=["textures/materials.tsv","textures/wibe/materials.tsv"].iter().find_map(|p|std::fs::read_to_string(assets.join(p)).ok()) else {return styles};
    for (i,line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {continue;}
        let fields:Vec<_>=line.split('\t').collect();
        if fields.len()!=4 {eprintln!("material pack line {}: expected four tab-separated fields",i+1);continue;}
        let (Ok(roughness),Ok(texels))=(fields[2].parse::<f32>(),fields[3].parse::<f32>()) else {eprintln!("material pack line {}: invalid surface parameters",i+1);continue;};
        styles.insert(fields[0].into(),Style {path:fields[1].into(),roughness:roughness.clamp(0.09,1.0),texels:texels.clamp(0.0,2048.0)});
    }
    styles
}
