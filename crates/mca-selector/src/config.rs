//! Port of config.GlobalConfig / WorldConfig / ConfigProvider with
//! serde-json persistence at the same paths as upstream.

use mc_core::files;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MAX_SCALE: f32 = 63.9999;
pub const MIN_SCALE: f32 = 0.05;
pub const MAX_ZOOM_LEVEL: i32 = 32;
pub const IMAGE_POOL_SIZE: f32 = 2.5;
pub const MAX_RECENT_FILES: usize = 16;

/// ARGB colors as f32 components, matching upstream Color defaults.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Color { r, g, b, a }
    }
    pub fn to_argb(&self) -> u32 {
        let a = (self.a.clamp(0.0, 1.0) * 255.0) as u32;
        let r = (self.r.clamp(0.0, 1.0) * 255.0) as u32;
        let g = (self.g.clamp(0.0, 1.0) * 255.0) as u32;
        let b = (self.b.clamp(0.0, 1.0) * 255.0) as u32;
        a << 24 | r << 16 | g << 8 | b
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentWorld {
    pub world: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalConfig {
    pub region_selection_color: Color,
    pub chunk_selection_color: Color,
    pub paste_chunks_color: Color,
    pub process_threads: i32,
    pub write_threads: i32,
    pub max_loaded_files: i32,
    pub mc_saves_dir: String,
    pub show_chunk_grid: bool,
    pub show_region_grid: bool,
    pub show_coordinates: bool,
    /// structure id -> draw icon (SelectStructures submenu toggles)
    pub show_structure_icons: bool,

    pub coordinate_style: String,
    pub debug: bool,
    /// upstream FilterChunksDialog keeps the last filter query for the session;
    /// we persist it (GlobalConfig has no dedicated filter file).
    pub filter_query: String,
    /// configured overlays, serialized via mc_core::overlay::serialize_overlay_line
    /// (upstream OverlayConfig).
    pub overlays: Vec<String>,
    pub recent_worlds: BTreeMap<i64, RecentWorld>,
    pub structure_icons: BTreeMap<String, bool>,
}

fn num_cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

impl Default for GlobalConfig {
    fn default() -> Self {
        GlobalConfig {
            region_selection_color: Color::new(1.0, 0.45, 0.0, 0.8),
            chunk_selection_color: Color::new(1.0, 0.45, 0.0, 0.8),
            paste_chunks_color: Color::new(0.0, 1.0, 0.0, 0.8),
            process_threads: (num_cpus() as i32 - 2).max(4),
            write_threads: (num_cpus() as i32).clamp(1, 4),
            max_loaded_files: 1,
            mc_saves_dir: mc_saves_dir(),
            show_chunk_grid: true,
            show_region_grid: true,
            show_coordinates: false,
            show_structure_icons: false,
            coordinate_style: "DYNAMIC".into(),
            debug: false,
            filter_query: String::new(),
            overlays: Vec::new(),
            recent_worlds: BTreeMap::new(),
            structure_icons: BTreeMap::new(),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl GlobalConfig {
    fn path() -> PathBuf {
        files::base_config_dir().join("settings.json")
    }
    pub fn load() -> Self {
        let p = Self::path();
        let mut cfg: GlobalConfig = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).unwrap_or_default())
            .unwrap_or_default();
        // structure icon toggles: default all on
        for (display, id) in &mc_core::structures::structures().display_names {
            cfg.structure_icons.entry(id.clone()).or_insert(true);
            let _ = display;
        }
        cfg
    }
    pub fn save(&self) {
        if let Ok(j) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), j);
        }
    }
    pub fn add_recent_world(&mut self, world: PathBuf) {
        if let Some((k, _)) = self
            .recent_worlds
            .iter()
            .find(|(_, w)| w.world == world)
            .map(|(k, w)| (*k, w.clone()))
        {
            self.recent_worlds.remove(&k);
        } else if self.recent_worlds.len() >= MAX_RECENT_FILES {
            let first = *self.recent_worlds.keys().next().unwrap();
            self.recent_worlds.remove(&first);
        }
        self.recent_worlds
            .insert(now_ms(), RecentWorld { world });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldConfig {
    pub render_height: i32,
    pub render_layer_only: bool,
    pub render_caves: bool,
    pub shade: bool,
    pub shade_water: bool,
    pub shade_altitude: bool,
    pub smooth_rendering: bool,
    pub smooth_overlays: bool,
    pub tile_map_background: String,
    pub show_nonexistent_regions: bool,
    pub structure_icon_size: i32,
    pub structure_icon_border_size: i32,
    pub render_header_only_zoom_level: i32,
    #[serde(skip)]
    pub zoom_level_lod_color: u32,
}

impl Default for WorldConfig {
    fn default() -> Self {
        WorldConfig {
            render_height: 319,
            render_layer_only: false,
            render_caves: false,
            shade: true,
            shade_water: true,
            shade_altitude: true,
            smooth_rendering: false,
            smooth_overlays: true,
            tile_map_background: "BLACK".into(),
            show_nonexistent_regions: true,
            structure_icon_size: 32,
            structure_icon_border_size: 1,
            render_header_only_zoom_level: 16,
            zoom_level_lod_color: 0xFF00_A300
                | ((0.64 * 255.0) as u32) << 8
                | 0xFF00_0000,
        }
    }
}

impl WorldConfig {
    fn path(world_cache: &Path) -> PathBuf {
        world_cache.join("world_settings.json")
    }
    pub fn load(world_cache: &Path) -> Self {
        std::fs::read_to_string(Self::path(world_cache))
            .ok()
            .and_then(|s| serde_json::from_str(&s).unwrap_or_default())
            .unwrap_or_default()
    }
    pub fn save(&self, world_cache: &Path) {
        let _ = std::fs::create_dir_all(world_cache);
        if let Ok(j) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(world_cache), j);
        }
    }
}

fn mc_saves_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let saves = Path::new(&home).join(".minecraft/saves");
    if saves.is_dir() {
        saves.to_string_lossy().into_owned()
    } else {
        home
    }
}

/// Global config singleton.
pub static GLOBAL: std::sync::RwLock<Option<GlobalConfig>> = std::sync::RwLock::new(None);
/// World config (per opened world).
pub static WORLD: std::sync::RwLock<Option<WorldState>> = std::sync::RwLock::new(None);

#[derive(Clone)]
pub struct WorldState {
    pub dirs: mc_core::mca::WorldDirectories,
    pub dimensions: Vec<PathBuf>,
    pub uuid: String,
    pub cache_dir: PathBuf,
    pub cfg: WorldConfig,
}

impl WorldState {
    pub fn region_dir(&self) -> &Path {
        self.dirs.region.as_deref().unwrap()
    }

    /// build-limit slider range for the active dimension: nether/end-style
    /// dims are 0..255, overworld(-like) dims -64..319. Derived from the
    /// dimension dir name — level.dat dimension types aren't parsed yet.
    pub fn height_range(&self) -> (i32, i32) {
        let name = self
            .region_dir()
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.contains("nether")
            || name.contains("end")
            || matches!(name.as_str(), "dim-1" | "dim1")
        {
            (0, 255)
        } else {
            (-64, 319)
        }
    }
}

pub fn global() -> GlobalConfig {
    GLOBAL.read().unwrap().clone().unwrap_or_default()
}
pub fn set_global(cfg: GlobalConfig) {
    cfg.save();
    *GLOBAL.write().unwrap() = Some(cfg);
}
pub fn world() -> Option<WorldState> {
    WORLD.read().unwrap().clone()
}
pub fn set_world(w: Option<WorldState>) {
    *WORLD.write().unwrap() = w;
}

/// Open a world directory and set the active WorldState.
pub fn open_world(dirs: mc_core::mca::WorldDirectories, dimensions: Vec<PathBuf>) -> WorldState {
    let region_dir = dirs.region.clone().unwrap();
    let cache_dir = files::world_cache_dir(&region_dir);
    let uuid = files::world_uuid(&region_dir);
    let cfg = WorldConfig::load(&cache_dir);
    let ws = WorldState {
        dirs,
        dimensions,
        uuid,
        cache_dir,
        cfg,
    };
    set_world(Some(ws.clone()));
    ws
}
