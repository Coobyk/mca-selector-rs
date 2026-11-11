//! Port of version.mapping.registry.StructureRegistry data.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Deserialize)]
pub struct StructureData {
    pub display: String,
    pub name: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default, rename = "allNames")]
    pub all_names: Vec<String>,
    #[serde(default, rename = "maxScale")]
    pub max_scale: f32,
}

pub struct StructureRegistry {
    /// canonical structure name -> StructureData
    pub icons: HashMap<String, StructureData>,
    /// display name -> canonical minecraft:name
    pub display_names: Vec<(String, String)>,
    /// any valid name/alt -> canonical name
    pub valid: HashMap<String, String>,
}

pub fn structures() -> &'static StructureRegistry {
    static R: OnceLock<StructureRegistry> = OnceLock::new();
    R.get_or_init(|| {
        let list: Vec<StructureData> = serde_json::from_str(include_str!(
            "../resources/mapping/registry/structures.json"
        ))
        .expect("structures.json");
        let mut r = StructureRegistry {
            icons: HashMap::new(),
            display_names: Vec::new(),
            valid: HashMap::new(),
        };
        for s in list {
            let canon = format!("minecraft:{}", s.name);
            r.display_names.push((s.display.clone(), canon.clone()));
            for name in &s.all_names {
                if name.chars().any(|c| c.is_ascii_uppercase()) {
                    r.valid.insert(name.clone(), canon.clone());
                } else {
                    r.valid.insert(format!("minecraft:{name}"), canon.clone());
                    r.valid.insert(name.clone(), canon.clone());
                }
            }
            r.icons.insert(canon, s);
        }
        r.display_names
            .sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        r
    })
}
