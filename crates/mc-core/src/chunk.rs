//! The chunk NBT model for modern Anvil (1.18+, `DataVersion >= 2724`).
//!
//! Only the fields needed for map rendering and the selection UI are
//! deserialized; serde skips everything else.

use fastnbt::from_bytes;
use serde::Deserialize;

use crate::error::Result;
use crate::packed::{palette_bits, PackedBits, HEIGHTMAP_BITS};

/// Section Y size, and the horizontal size of a chunk.
pub const CHUNK_BITS: usize = 16;
pub const SECTION_BLOCKS: usize = 16;
/// Number of sections in a 1.18+ chunk (-64..320).
pub const SECTION_COUNT: usize = 24;

#[derive(Debug, Deserialize)]
pub struct Chunk {
    #[serde(rename = "DataVersion")]
    pub data_version: i32,
    #[serde(rename = "xPos")]
    pub x_pos: i32,
    #[serde(rename = "zPos")]
    pub z_pos: i32,
    /// `yPos` exists since 1.18; older chunks place it in the file name.
    #[serde(rename = "yPos", default)]
    pub y_pos: i32,
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "LastUpdate", default)]
    pub last_update: i64,
    #[serde(rename = "sections")]
    pub sections: Vec<Section>,
    #[serde(rename = "Heightmaps")]
    pub heightmaps: Option<Heightmaps>,
    /// Filled by [`Chunk::from_nbt`] from the region file, not the NBT.
    #[serde(skip)]
    pub inhabited_time: i64,
}

#[derive(Debug, Deserialize)]
pub struct Section {
    #[serde(rename = "Y")]
    pub y: i8,
    #[serde(rename = "block_states")]
    pub block_states: Option<BlockStates>,
}

#[derive(Debug, Deserialize)]
pub struct BlockStates {
    pub palette: Vec<PaletteEntry>,
    pub data: Option<fastnbt::LongArray>,
}

/// One palette entry. Three encodings exist across versions:
///
/// - pre-1.21.5: compound `{Name: string, Properties: {string: string}}`;
/// - 1.21.5: plain block name string;
/// - current: compound with the name under the empty key and the block
///   properties as sibling string keys, e.g. `{"": "oak_stairs", facing: "east"}`.
///
/// All are normalized into `name` + optional `properties`.
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    pub name: String,
    pub properties: Option<std::collections::HashMap<String, String>>,
}

impl<'de> serde::Deserialize<'de> for PaletteEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = fastnbt::Value::deserialize(deserializer)?;
        match value {
            fastnbt::Value::String(name) => Ok(Self {
                name,
                properties: None,
            }),
            fastnbt::Value::Compound(map) => {
                if let Some(fastnbt::Value::String(name)) = map.get("Name") {
                    let name = name.clone();                    // Old format: {Name, Properties: {..}}.
                    let properties = match map.get("Properties") {
                        Some(fastnbt::Value::Compound(props)) => props
                            .iter()
                            .filter_map(|(k, v)| match v {
                                fastnbt::Value::String(v) => Some((k.clone(), v.clone())),
                                _ => None,
                            })
                            .collect::<std::collections::HashMap<_, _>>(),
                        _ => return Ok(Self { name, properties: None }),
                    };
                    Ok(Self {
                        name,
                        properties: Some(properties),
                    })
                } else if let Some(fastnbt::Value::String(name)) = map.get("id") {
                    let name = name.clone();
                    // Modern format: {id: name, properties: {prop: value}}.
                    let properties = match map.get("properties") {
                        Some(fastnbt::Value::Compound(props)) => props
                            .iter()
                            .filter_map(|(k, v)| match v {
                                fastnbt::Value::String(v) => Some((k.clone(), v.clone())),
                                _ => None,
                            })
                            .collect::<std::collections::HashMap<_, _>>(),
                        _ => return Ok(Self { name, properties: None }),
                    };
                    let properties = if properties.is_empty() {
                        None
                    } else {
                        Some(properties)
                    };
                    Ok(Self {
                        name,
                        properties,
                    })
                } else if let Some(fastnbt::Value::String(name)) = map.get("") {
                    let name = name.clone();
                    // Newest format: {"" : name, prop: value, ...}.
                    let properties = map
                        .iter()
                        .filter(|(k, _)| !k.is_empty())
                        .filter_map(|(k, v)| match v {
                            fastnbt::Value::String(v) => Some((k.clone(), v.clone())),
                            _ => None,
                        })
                        .collect::<std::collections::HashMap<_, _>>();
                    let properties = if properties.is_empty() {
                        None
                    } else {
                        Some(properties)
                    };
                    Ok(Self {
                        name,
                        properties,
                    })
                } else {
                    Err(serde::de::Error::custom(format!(
                        "unrecognized palette entry: keys {:?}",
                        map.keys().take(6).collect::<Vec<_>>()
                    )))
                }
            }
            other => Err(serde::de::Error::custom(format!(
                "unrecognized palette entry: {other:?}"
            ))),
        }
    }
}

impl PaletteEntry {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn properties(&self) -> Option<&std::collections::HashMap<String, String>> {
        self.properties.as_ref()
    }

    /// Full block state string, e.g. `oak_stairs[facing=east]`, the form
    /// used by block color tables.
    pub fn state_string(&self) -> String {
        let name = &self.name;
        let Some(props) = &self.properties else {
            return name.clone();
        };
        if props.is_empty() {
            return name.clone();
        }
        // Sorted for a canonical form.
        let mut entries: Vec<(&String, &String)> = props.iter().collect();
        entries.sort();
        let joined = entries
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{}[{}]", name, joined)
    }
}

/// Heightmap long arrays keyed by type (`MOTION_BLOCKING`, `WORLD_SURFACE`, ...).
#[derive(Debug, Deserialize)]
pub struct Heightmaps(pub std::collections::HashMap<String, fastnbt::LongArray>);

impl Heightmaps {
    pub fn get(&self, key: &str) -> Option<&fastnbt::LongArray> {
        self.0.get(key)
    }

    /// Read height of the column `(x, z)` from a packed heightmap array.
    /// Heights are stored as `y - world_min_y` (`y + 64` in 1.18); this
    /// returns the plain world Y.
    pub fn height_at(map: &[i64], x: usize, z: usize) -> i32 {
        debug_assert!(x < 16 && z < 16);
        let bits = PackedBits::new(map, HEIGHTMAP_BITS);
        let raw = bits.get(z * 16 + x);
        raw as i32 - 64
    }
}

impl Chunk {
    pub fn from_nbt(bytes: &[u8]) -> Result<Chunk> {
        Ok(from_bytes(bytes)?)
    }

    /// Decode the NBT into a generic [`fastnbt::Value`] and format the
    /// deserialization error with the root's keys, for debugging malformed
    /// or unexpected chunk data.
    pub fn debug_nbt_error(bytes: &[u8]) -> String {
        match from_bytes::<Chunk>(bytes) {
            Ok(_) => "parses fine".to_string(),
            Err(e) => match from_bytes::<std::collections::BTreeMap<String, fastnbt::Value>>(bytes) {
                Ok(root) => format!(
                    "{e}; root keys: {:?}",
                    root.keys().take(40).collect::<Vec<_>>()
                ),
                Err(e2) => format!("{e}; raw root decode failed: {e2}"),
            },
        }
    }

    pub fn min_y(&self) -> i32 {
        // Chunk yPos field when present, otherwise the 1.18 default.
        if self.sections.iter().any(|s| s.y != 0) || self.y_pos != 0 {
            self.y_pos * SECTION_BLOCKS as i32
        } else {
            -64
        }
    }

    /// Find a section by its Y index (in sections, i.e. -4..4 for 1.18+).
    pub fn section(&self, y: i8) -> Option<&Section> {
        self.sections.iter().find(|s| s.y == y)
    }

    /// Index of the topmost non-empty section, scanning downwards.
    pub fn top_section(&self) -> Option<&Section> {
        self.sections
            .iter()
            .filter(|s| {
                s.block_states
                    .as_ref()
                    .is_some_and(|b| b.palette.iter().any(|p| p.name != "minecraft:air"))
            })
            .max_by_key(|s| s.y)
    }

    /// Look up one block in a section's packed data. Returns the palette
    /// index (which indexes the section's palette).
    pub fn block_palette_index(section: &Section, x: usize, y: usize, z: usize) -> u32 {
        let Some(states) = &section.block_states else {
            return 0;
        };
        let bits = palette_bits(states.palette.len());
        match &states.data {
            // Fully-uniform sections (single-entry palette) have no data array.
            None => 0,
            Some(data) => {
                let packed = PackedBits::new(data, bits);
                packed.get((y * 16 + z) * 16 + x)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heightmap_reading() {
        // 9-bit packing, 7 entries per long; raw value is y + 64.
        let mut map = vec![0i64; 37];
        let idx = 3 * 16 + 5;
        map[idx / 7] |= ((321 + 64) as i64) << ((idx % 7) * 9);
        assert_eq!(Heightmaps::height_at(&map, 5, 3), 321);
    }
}
