//! Loads mapping/registry/*.json — block/biome/entity/status/structure
//! name lists used by filters, the overlay system and dialogs.

use std::sync::OnceLock;

fn load(path: &str) -> serde_json::Value {
    serde_json::from_str(path).expect("registry json")
}

pub fn blocks() -> &'static serde_json::Value {
    static V: OnceLock<serde_json::Value> = OnceLock::new();
    V.get_or_init(|| load(include_str!("../resources/mapping/registry/blocks.json")))
}
pub fn biomes() -> &'static serde_json::Value {
    static V: OnceLock<serde_json::Value> = OnceLock::new();
    V.get_or_init(|| load(include_str!("../resources/mapping/registry/biomes.json")))
}
pub fn biome_name_to_id() -> &'static serde_json::Value {
    static V: OnceLock<serde_json::Value> = OnceLock::new();
    V.get_or_init(|| load(include_str!("../resources/mapping/registry/biome_name_to_id.json")))
}
pub fn entities() -> &'static serde_json::Value {
    static V: OnceLock<serde_json::Value> = OnceLock::new();
    V.get_or_init(|| load(include_str!("../resources/mapping/registry/entities.json")))
}
pub fn status() -> &'static serde_json::Value {
    static V: OnceLock<serde_json::Value> = OnceLock::new();
    V.get_or_init(|| load(include_str!("../resources/mapping/registry/status.json")))
}
pub fn structures_json() -> &'static serde_json::Value {
    static V: OnceLock<serde_json::Value> = OnceLock::new();
    V.get_or_init(|| load(include_str!("../resources/mapping/registry/structures.json")))
}
