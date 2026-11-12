//! Port of version.mapping: color configs (colors_*.json), block-state
//! bitsets, biome tints and the shared color_properties.json.

use crate::bitset::BitSet;
use crate::nbt::{Compound, Tag};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

pub const TRANSPARENT: u8 = 0b0000_0001;
pub const GRASS_TINT: u8 = 0b0000_0010;
pub const FOLIAGE_TINT: u8 = 0b0000_0100;
pub const WATER: u8 = 0b0000_1000;
pub const DRY_FOLIAGE_TINT: u8 = 0b1000_0000;
pub const TINTED: u8 = GRASS_TINT | FOLIAGE_TINT | WATER | DRY_FOLIAGE_TINT;

pub const MISSING_COLOR: u32 = 0x00FF_FFFF;

#[derive(Clone, Copy, Debug, Default)]
pub struct BlockColor {
    pub color: u32, // rgb without alpha
    pub properties: u8,
}

#[derive(Clone, Debug)]
pub enum StateColors {
    Single(BlockColor),
    States {
        states: HashMap<BitSet, BlockColor>,
        default: BlockColor,
    },
}

impl StateColors {
    pub fn color(&self, state: Option<&BitSet>) -> BlockColor {
        match self {
            StateColors::Single(c) => *c,
            StateColors::States { states, default } => state
                .and_then(|s| states.get(s))
                .copied()
                .unwrap_or(*default),
        }
    }
    pub fn iter_entries(&self) -> Vec<(Option<BitSet>, BlockColor)> {
        match self {
            StateColors::Single(c) => vec![(None, *c)],
            StateColors::States { states, .. } => {
                states.iter().map(|(k, v)| (Some(k.clone()), *v)).collect()
            }
        }
    }
}

/// BlockStates: property name -> value -> bit index.
#[derive(Debug)]
pub struct BlockStates {
    states: HashMap<String, HashMap<String, usize>>,
}

impl BlockStates {
    pub fn get_state(&self, properties: Option<&Compound>) -> Option<BitSet> {
        let props = properties?;
        let mut s = BitSet::new();
        for (k, v) in props {
            if let Some(map) = self.states.get(k) {
                if let Some(Tag::String(val)) = Some(v) {
                    if let Some(i) = map.get(val.as_str()) {
                        s.set(*i);
                    }
                }
            }
        }
        Some(s)
    }
}

/// Biome tints [grass, foliage, water, dry_foliage].
pub type BiomeTints = [u32; 4];

#[derive(Debug, Default)]
pub struct BiomeColors {
    pub biomes: HashMap<String, BiomeTints>,
}

/// tintCache: block -> biome -> StateColors
pub type TintCache = HashMap<String, HashMap<String, StateColors>>;

pub struct ColorConfig {
    pub states: BlockStates,
    pub colors: HashMap<String, StateColors>,
    pub tint_cache: TintCache,
}

impl ColorConfig {
    /// The block-state bitset is resolved by the caller — renderers compute
    /// each palette entry's state once instead of calling get_state (which
    /// allocates a BitSet) for every column.
    pub fn get_color_state(&self, name: &str, biome: &str, state: Option<&BitSet>) -> BlockColor {
        let block_color = self
            .colors
            .get(name)
            .map(|sc| sc.color(state))
            .unwrap_or(BlockColor {
                color: MISSING_COLOR,
                properties: 0,
            });
        if block_color.properties & TINTED != 0 {
            let tinted = self
                .tint_cache
                .get(name)
                .and_then(|m| m.get(biome))
                .map(|sc| sc.color(state));
            return tinted.unwrap_or_else(|| {
                self.tint_cache
                    .get(name)
                    .and_then(|m| m.get("minecraft:plains"))
                    .map(|sc| sc.color(None))
                    .unwrap_or(BlockColor {
                        color: MISSING_COLOR,
                        properties: 0,
                    })
            });
        }
        block_color
    }
}

pub fn apply_tint(color: u32, tint: u32) -> u32 {
    let nr = ((tint >> 16 & 0xFF) * (color >> 16 & 0xFF)) >> 8;
    let ng = ((tint >> 8 & 0xFF) * (color >> 8 & 0xFF)) >> 8;
    let nb = ((tint & 0xFF) * (color & 0xFF)) >> 8;
    (color & 0xFF00_0000) | nr << 16 | ng << 8 | nb
}

#[derive(Deserialize)]
struct RawColorConfig {
    states: HashMap<String, HashMap<String, usize>>,
    colors: HashMap<String, serde_json::Value>,
    tints: Option<HashMap<String, Vec<String>>>,
    #[serde(rename = "block_name_to_id")]
    _bnti: Option<serde_json::Value>,
}

fn parse_block_color(v: &serde_json::Value) -> BlockColor {
    match v {
        serde_json::Value::String(s) => BlockColor {
            color: u32::from_str_radix(s, 16).unwrap_or(MISSING_COLOR),
            properties: 0,
        },
        serde_json::Value::Object(o) => {
            let color = o
                .get("color")
                .and_then(|c| c.as_str())
                .and_then(|s| u32::from_str_radix(s, 16).ok())
                .unwrap_or(MISSING_COLOR);
            let props = o
                .get("properties")
                .and_then(|p| p.as_u64())
                .unwrap_or(0) as u8;
            BlockColor {
                color,
                properties: props,
            }
        }
        _ => BlockColor::default(),
    }
}

pub fn load_color_config(json: &str) -> ColorConfig {
    let raw: RawColorConfig = serde_json::from_str(json).expect("invalid color config json");
    let states = BlockStates { states: raw.states };
    let mut colors: HashMap<String, StateColors> = HashMap::new();
    for (name, v) in raw.colors {
        match &v {
            serde_json::Value::String(_) => {
                colors.insert(name, StateColors::Single(parse_block_color(&v)));
            }
            serde_json::Value::Object(o) if o.contains_key("states") || o.contains_key("default") => {
                let mut map = HashMap::new();
                if let Some(st) = o.get("states").and_then(|s| s.as_object()) {
                    for (k, cv) in st {
                        if let Some(bs) = BitSet::from_base64(k) {
                            map.insert(bs, parse_block_color(cv));
                        }
                    }
                }
                let default = o
                    .get("default")
                    .map(parse_block_color)
                    .unwrap_or_default();
                colors.insert(name, StateColors::States { states: map, default });
            }
            serde_json::Value::Object(_) => {
                colors.insert(name, StateColors::Single(parse_block_color(&v)));
            }
            _ => {}
        }
    }
    let mut tints = BiomeColors::default();
    if let Some(raw_tints) = raw.tints {
        for (k, arr) in raw_tints {
            let mut t = [0u32; 4];
            for (i, c) in arr.iter().enumerate().take(4) {
                t[i] = u32::from_str_radix(c, 16).unwrap_or(0);
            }
            tints.biomes.insert(k, t);
        }
    }
    let tint_cache = create_tint_cache(&colors, &tints);
    ColorConfig {
        states,
        colors,
        tint_cache,
    }
}

fn create_tint_cache(colors: &HashMap<String, StateColors>, tints: &BiomeColors) -> TintCache {
    let mut cache: TintCache = HashMap::new();
    for (name, sc) in colors {
        // tint the config's fallback entry as well: iter_entries only yields
        // the state-keyed colors, so without it a runtime state that matches
        // none of them (or the `.color(None)` plains fallback) resolves to
        // BlockColor::default() — opaque black patches on the map.
        let cfg_default = match sc {
            StateColors::Single(c) => *c,
            StateColors::States { default, .. } => *default,
        };
        for (state, bc) in sc.iter_entries() {
            let tint_idx = if bc.properties & GRASS_TINT != 0 {
                Some(0)
            } else if bc.properties & FOLIAGE_TINT != 0 {
                Some(1)
            } else if bc.properties & WATER != 0 {
                Some(2)
            } else if bc.properties & DRY_FOLIAGE_TINT != 0 {
                Some(3)
            } else {
                None
            };
            if let Some(ti) = tint_idx {
                let colored = cache.entry(name.clone()).or_default();
                for (biome, bt) in &tints.biomes {
                    let c = apply_tint(bc.color, bt[ti]);
                    match state {
                        None => {
                            colored
                                .insert(biome.clone(), StateColors::Single(BlockColor { color: c, properties: 0 }));
                        }
                        Some(ref st) => {
                            let e = colored
                                .entry(biome.clone())
                                .or_insert_with(|| StateColors::States {
                                    states: HashMap::new(),
                                    default: BlockColor {
                                        color: apply_tint(cfg_default.color, bt[ti]),
                                        properties: 0,
                                    },
                                });
                            if let StateColors::States { states, .. } = e {
                                states.insert(st.clone(), BlockColor { color: c, properties: 0 });
                            }
                        }
                    }
                }
            }
        }
    }
    cache
}

/// color_properties.json — global (not versioned) block property sets.
#[derive(Deserialize)]
pub struct ColorProperties {
    pub air: HashSet<String>,
    pub transparent: HashSet<String>,
    pub grass_tint: HashSet<String>,
    pub foliage_tint: HashSet<String>,
    pub dry_foliage_tint: HashSet<String>,
    pub water: HashSet<String>,
    pub foliage: HashSet<String>,
}

pub fn color_properties() -> &'static ColorProperties {
    static P: OnceLock<ColorProperties> = OnceLock::new();
    P.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/mapping/color_properties.json"))
            .expect("color_properties.json")
    })
}

// --- embedded per-version color configs -------------------------------------

macro_rules! embedded_cfg {
    ($name:ident, $path:literal) => {
        pub fn $name() -> &'static ColorConfig {
            static C: OnceLock<ColorConfig> = OnceLock::new();
            C.get_or_init(|| load_color_config(include_str!($path)))
        }
    };
}

embedded_cfg!(cfg_17w47a, "../resources/mapping/java_1_13/colors_17w47a.json");
embedded_cfg!(cfg_18w43a, "../resources/mapping/java_1_14/colors_18w43a.json");
embedded_cfg!(cfg_19w34a, "../resources/mapping/java_1_15/colors_19w34a.json");
embedded_cfg!(cfg_20w06a, "../resources/mapping/java_1_16/colors_20w06a.json");
embedded_cfg!(cfg_20w45a, "../resources/mapping/java_1_17/colors_20w45a.json");
embedded_cfg!(cfg_21w37a, "../resources/mapping/java_1_18/colors_21w37a.json");
embedded_cfg!(cfg_22w11a, "../resources/mapping/java_1_19/colors_22w11a.json");
embedded_cfg!(cfg_23w12a, "../resources/mapping/java_1_20/colors_23w12a.json");
embedded_cfg!(cfg_24w18a, "../resources/mapping/java_1_21/colors_24w18a.json");
embedded_cfg!(cfg_25w32a, "../resources/mapping/java_1_21/colors_25w32a.json");
embedded_cfg!(cfg_26_1_s1, "../resources/mapping/java_26/colors_26.1-snapshot-1.json");
embedded_cfg!(cfg_26_2_s1, "../resources/mapping/java_26/colors_26.2-snapshot-1.json");
embedded_cfg!(cfg_26_3_s1, "../resources/mapping/java_26/colors_26.3-snapshot-1.json");

// legacy (<=1.12) csv color mapping lives in version.rs (LegacyColorMapping).
