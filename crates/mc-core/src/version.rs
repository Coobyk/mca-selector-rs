//! Port of version.*: DataVersion-dispatched ChunkRenderers + ColorMappings.
//! Dispatch via floorEntry, like upstream's VersionHandler.

use crate::bitset::BitSet;
use crate::chunk::{self, PackedData, PackedSrc};
use crate::colors::{self, ColorConfig};
use crate::nbt::{self, Compound, Tag};

pub const DEFAULT_GRASS_TINT: u32 = 0x91bd59;
pub const DEFAULT_FOLIAGE_TINT: u32 = 0x77ab2f;
pub const DEFAULT_WATER_TINT: u32 = 0x3f76e4;

#[derive(Clone, Copy)]
pub enum BiomeKey<'a> {
    Idx(i32),
    Name(&'a str),
}

// --- legacy color model (java_1_9) -------------------------------------------

pub struct LegacyColorMapping {
    pub colors: std::collections::HashMap<i32, u32>, // (id<<4)|data -> argb
    pub grass_tints: [u32; 256],
    pub foliage_tints: [u32; 256],
    pub water_tints: [u32; 256],
    pub name_to_id: std::collections::HashMap<String, i32>,
}

pub fn legacy_colors() -> &'static LegacyColorMapping {
    static C: std::sync::OnceLock<LegacyColorMapping> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        let mut m = LegacyColorMapping {
            colors: std::collections::HashMap::new(),
            grass_tints: [DEFAULT_GRASS_TINT; 256],
            foliage_tints: [DEFAULT_FOLIAGE_TINT; 256],
            water_tints: [DEFAULT_WATER_TINT; 256],
            name_to_id: std::collections::HashMap::new(),
        };
        for line in include_str!("../resources/mapping/java_1_9/colors.csv").lines() {
            let mut it = line.split(';');
            if let (Some(a), Some(b), Some(c)) = (it.next(), it.next(), it.next()) {
                if let (Ok(id), Ok(data), Ok(col)) =
                    (a.parse::<i32>(), b.parse::<i32>(), u32::from_str_radix(c, 16))
                {
                    m.colors.insert((id << 4) | data, col | 0xFF00_0000);
                }
            }
        }
        for line in include_str!("../resources/mapping/java_1_9/biome_colors.csv").lines() {
            let mut it = line.split(';');
            if let (Some(a), Some(g), Some(f), Some(w)) = (it.next(), it.next(), it.next(), it.next())
            {
                if let (Ok(id), Ok(g), Ok(f), Ok(w)) = (
                    a.parse::<usize>(),
                    u32::from_str_radix(g, 16),
                    u32::from_str_radix(f, 16),
                    u32::from_str_radix(w, 16),
                ) {
                    m.grass_tints[id] = g;
                    m.foliage_tints[id] = f;
                    m.water_tints[id] = w;
                }
            }
        }
        for line in include_str!("../resources/mapping/java_1_9/block_name_to_id.csv").lines() {
            let mut it = line.split(';');
            if let (Some(name), Some(id)) = (it.next(), it.next()) {
                if let Ok(id) = id.parse::<i32>() {
                    m.name_to_id.insert(name.to_string(), id);
                }
            }
        }
        m
    })
}

fn legacy_is_grass(id: i32) -> bool {
    matches!(id, 2 | 31 | 104 | 105 | 175)
}
fn legacy_is_foliage(id: i32) -> bool {
    matches!(id, 18 | 106 | 161)
}
fn legacy_is_water(id: i32) -> bool {
    matches!(id, 8 | 9)
}
fn legacy_is_transparent(id: i32) -> bool {
    matches!(id, 0 | 166 | 217)
}

// --- color map dispatch ------------------------------------------------------

pub enum ColorMap {
    Null,
    Legacy(&'static LegacyColorMapping),
    Config(&'static ColorConfig),
    /// Xaero palette + per-version config as fallback for blocks the
    /// generated xaero table doesn't know.
    Xaero(&'static ColorConfig, &'static ColorConfig),
}

impl ColorMap {
    /// Modern path: palette-entry name + pre-resolved block-state bitset,
    /// (renderers resolve it once per palette entry).
    pub fn get_rgb_state(&self, name: &str, state: Option<&BitSet>, biome: BiomeKey) -> u32 {
        match self {
            ColorMap::Null => 0,
            ColorMap::Legacy(_) => 0xFF00_0000,
            ColorMap::Config(c) => {
                let biome_name = match biome {
                    BiomeKey::Name(s) => s,
                    BiomeKey::Idx(id) => biome_id_to_name(id),
                };
                c.get_color_state(name, biome_name, state).color | 0xFF00_0000
            }
            ColorMap::Xaero(x, fallback) => {
                let biome_name = match biome {
                    BiomeKey::Name(s) => s,
                    BiomeKey::Idx(id) => biome_id_to_name(id),
                };
                let bc = x.get_color_state(name, biome_name, state);
                if bc.color == colors::MISSING_COLOR {
                    fallback.get_color_state(name, biome_name, state).color | 0xFF00_0000
                } else {
                    bc.color | 0xFF00_0000
                }
            }
        }
    }

    /// the generated xaero table when this mapping uses it — entry
    /// resolution consults it for overlay/invisible/glow/short metadata.
    pub fn xaero_cfg(&self) -> Option<&'static ColorConfig> {
        match self {
            ColorMap::Xaero(x, _) => Some(x),
            _ => None,
        }
    }

    /// Legacy path: key = (blockId<<4)|data, biome = biome id.
    pub fn get_rgb_legacy(&self, key: i32, biome: i32) -> u32 {
        match self {
            ColorMap::Legacy(m) => {
                let color = m.colors.get(&key).copied().unwrap_or(0xFF00_0000);
                let id = key >> 4;
                let b = biome.clamp(0, 255) as usize;
                if legacy_is_grass(id) {
                    colors::apply_tint(color, m.grass_tints[b])
                } else if legacy_is_foliage(id) {
                    colors::apply_tint(color, m.foliage_tints[b])
                } else if legacy_is_water(id) {
                    colors::apply_tint(color, m.water_tints[b])
                } else {
                    color
                }
            }
            _ => 0,
        }
    }

}

/// floorEntry dispatch for the color mapping (VersionHandler.getImpl).
pub fn color_mapping(data_version: i32) -> ColorMap {
    const TABLE: &[(i32, fn() -> &'static ColorConfig)] = &[
        (chunk::DV_17W47A, colors::cfg_17w47a),
        (chunk::DV_18W43A, colors::cfg_18w43a),
        (chunk::DV_19W34A, colors::cfg_19w34a),
        (chunk::DV_20W06A, colors::cfg_20w06a),
        (chunk::DV_20W45A, colors::cfg_20w45a),
        (chunk::DV_21W37A, colors::cfg_21w37a),
        (chunk::DV_22W11A, colors::cfg_22w11a),
        (chunk::DV_23W12A, colors::cfg_23w12a),
        (chunk::DV_24W18A, colors::cfg_24w18a),
        (chunk::DV_25W32A, colors::cfg_25w32a),
        (chunk::DV_26_1_S1, colors::cfg_26_1_s1),
        (chunk::DV_26_2_S1, colors::cfg_26_2_s1),
        (chunk::DV_26_3_S1, colors::cfg_26_3_s1),
    ];
    if data_version < chunk::DV_15W32A {
        return ColorMap::Null;
    }
    if data_version < chunk::DV_17W47A {
        return ColorMap::Legacy(legacy_colors());
    }
    let (_, f) = chunk::dv_floor_key(TABLE, data_version).unwrap();
    ColorMap::Xaero(colors::cfg_xaero(), f())
}

// ---------------------------------------------------------------------------
// chunk formats

#[derive(Clone, Copy, PartialEq)]
pub enum ChunkFormat {
    Null,
    Legacy15w32a,
    P17w47a,
    P18w06a,
    P19w36a,
    P20w17a,
    P21w06a,
    P21w15a,
    P21w37a,
    P21w43a,
    P26_3s7,
}

pub fn chunk_format(data_version: i32) -> ChunkFormat {
    const TABLE: &[(i32, ChunkFormat)] = &[
        (chunk::DV_15W32A, ChunkFormat::Legacy15w32a),
        (chunk::DV_17W47A, ChunkFormat::P17w47a),
        (chunk::DV_18W06A, ChunkFormat::P18w06a),
        (chunk::DV_19W36A, ChunkFormat::P19w36a),
        (chunk::DV_20W17A, ChunkFormat::P20w17a),
        (chunk::DV_21W06A, ChunkFormat::P21w06a),
        (chunk::DV_21W15A, ChunkFormat::P21w15a),
        (chunk::DV_21W37A, ChunkFormat::P21w37a),
        (chunk::DV_21W43A, ChunkFormat::P21w43a),
        (chunk::DV_26_3_S7, ChunkFormat::P26_3s7),
    ];
    chunk::dv_floor_key(TABLE, data_version)
        .map(|(_, f)| *f)
        .unwrap_or(ChunkFormat::Null)
}

// ---------------------------------------------------------------------------
// helpers

fn palette_props<'a>(entry: Option<&'a Tag>) -> Option<&'a Compound> {
    match entry {
        Some(Tag::Compound(c)) => c.get("Properties").and_then(Tag::as_compound),
        _ => None,
    }
}

// --- resolved palette entries ------------------------------------------------
//
// The per-block lookups (name/properties hashing, transparent/water/foliage
// sets, waterlogged, state bitset) depend only on the palette entry. Name/props
// flags are resolved once per palette slot when a section is built; the
// color-mapping part (state bitset + rgb) is resolved lazily on first hit via
// PalEntry.cell, and whole sections are built lazily via SectionSlot.built.

const F_TRANSPARENT: u8 = 0b0001;
const F_WATER: u8 = 0b0010;
const F_WATERLOGGED: u8 = 0b0100;
const F_FOLIAGE: u8 = 0b1000;

// xaero-model flags (MapWriter column classification)
const X_INVIS: u8 = 0b0001;
const X_OVERLAY: u8 = 0b0010;
const X_GLOW: u8 = 0b0100;
const X_SHORT: u8 = 0b1000;
const X_AIR: u8 = 0b1_0000;

struct PalEntry<'a> {
    name: &'a str,
    /// block-state properties — kept so the color can be resolved lazily.
    props: Option<&'a Compound>,
    /// resolved color (+state for tinted entries); filled on first opaque hit.
    cell: std::cell::OnceCell<CellVal>,
    flags: u8,
    /// xaero-model flags (0 when the chunk's color map isn't ColorMap::Xaero).
    xflags: u8,
    /// overlay metadata — texture alpha and per-layer opacity.
    xalpha: u8,
    xop: u8,
}

/// The color-mapping half of palette resolution, deferred until a column
/// scan actually lands on the entry (unreached sections are not built at
/// all, and most slots of a reached section's palette are never painted).
struct CellVal {
    /// block-state bitset — only kept when `tinted` (tints need it).
    state: Option<BitSet>,
    /// resolved argb — valid when !tinted.
    rgb: u32,
    tinted: bool,
}

/// Eager half of resolution: the name/props-derived flags the column scan
/// and the whole-section skips test per visited block.
fn resolve_entry<'a>(
    name: &'a str,
    props: Option<&'a Compound>,
    xaero: Option<&ColorConfig>,
) -> PalEntry<'a> {
    let cp = colors::color_properties();
    let mut flags = 0u8;
    if cp.transparent.contains(name) {
        flags |= F_TRANSPARENT;
    }
    if cp.water.contains(name) {
        flags |= F_WATER;
    }
    if cp.foliage.contains(name) {
        flags |= F_FOLIAGE;
    }
    if props
        .and_then(|p| p.get("waterlogged"))
        .and_then(|t| t.as_string())
        .map(|s| s == "true")
        .unwrap_or(false)
    {
        flags |= F_WATERLOGGED;
    }
    let mut xflags = 0u8;
    let mut xalpha = 255u8;
    let mut xop = 0u8;
    if let Some(xc) = xaero {
        if matches!(name, "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air") {
            xflags |= X_AIR;
        }
        if let Some(m) = xc.xaero_meta.get(name) {
            if m.invis {
                xflags |= X_INVIS;
            }
            if m.overlay {
                xflags |= X_OVERLAY;
            }
            if m.glow {
                xflags |= X_GLOW;
            }
            if m.is_short(props) {
                xflags |= X_SHORT;
            }
            xalpha = m.alpha;
            xop = m.opacity.min(15);
        }
    }
    PalEntry {
        name,
        props,
        cell: std::cell::OnceCell::new(),
        flags,
        xflags,
        xalpha,
        xop,
    }
}

/// Color half of the old resolve_entry — same result, computed on first hit.
/// get_state (per-prop hash lookups + a BitSet) is skipped when the mapped
/// color is a state-independent Single that is not tinted, matching the old
/// code's `s.color(state)` where Single ignores `state` entirely.
fn resolve_color(cm: &ColorMap, name: &str, props: Option<&Compound>) -> CellVal {
    let c = match cm {
        ColorMap::Config(c) => *c,
        ColorMap::Xaero(x, fb) => {
            if x.colors.contains_key(name) {
                x
            } else {
                fb
            }
        }
        ColorMap::Null => {
            return CellVal {
                state: None,
                rgb: 0,
                tinted: false,
            }
        }
        ColorMap::Legacy(_) => {
            return CellVal {
                state: None,
                rgb: 0xFF00_0000,
                tinted: false,
            }
        }
    };
    let sc = c.colors.get(name);
    let (state, bc) = match sc {
        Some(colors::StateColors::Single(bc)) => {
            if bc.properties & colors::TINTED != 0 {
                // tint_cache lookups still consume the state.
                (c.states.get_state(props), *bc)
            } else {
                (None, *bc)
            }
        }
        Some(s) => {
            let st = c.states.get_state(props);
            let bc = s.color(st.as_ref());
            (st, bc)
        }
        None => (
            None,
            colors::BlockColor {
                color: colors::MISSING_COLOR,
                properties: 0,
            },
        ),
    };
    if bc.properties & colors::TINTED != 0 {
        // rgb is resolved per-biome at render time (memoized).
        CellVal {
            state,
            rgb: 0,
            tinted: true,
        }
    } else {
        CellVal {
            state: None,
            rgb: bc.color | 0xFF00_0000,
            tinted: false,
        }
    }
}

/// The entry for a missing/empty palette slot: name "" (not transparent,
/// not water, not foliage; the cell resolves to MISSING_COLOR -> opaque
/// white for Config mappings, 0 for Null, 0xFF000000 for Legacy).
fn oob_entry() -> PalEntry<'static> {
    PalEntry {
        name: "",
        props: None,
        cell: std::cell::OnceCell::new(),
        flags: 0,
        xflags: 0,
        xalpha: 255,
        xop: 0,
    }
}

struct IndexedSection<'a> {
    /// resolved block palette; entry i is block palette index i.
    entries: Vec<PalEntry<'a>>,
    /// packed block data; absent/short (bits == 0) => uniform section,
    /// every block reads entries[0].
    block: PackedData<'a>,
    /// resolved biome names for the 4x4x4 cells ([yq*16+zq*4+xq]) — built on
    /// the first tinted hit; an empty resolved table means the packed
    /// encoding is pathological and get_biome_packed must be used.
    biome_names: std::cell::OnceCell<Vec<&'a str>>,
    // raw biome palette kept for the pathological-bipl fallback path only.
    biome_palette: &'a [Tag],
    biome: PackedData<'a>,
    start_height: i32,
    section_height: i32,
    /// provable "every block maps to a transparent entry" => sections can be
    /// skipped wholesale by the column scan.
    all_transparent: bool,
    /// same for the caves renderer (transparent OR foliage never counts).
    caves_inert: bool,
    /// xaero scan: every entry is air or invisible => skippable.
    x_inert: bool,
}

/// An in-range section tag plus its slot geometry. The expensive part
/// (palette resolution, flag aggregation) is deferred until a column scan
/// reaches the section — sections below a fully-opaque surface are never
/// built at all. build_section is a pure function of the NBT, so deferral
/// is pixel-identical.
struct SectionSlot<'a> {
    src: &'a Compound,
    modern: bool,
    format: ChunkFormat,
    start_height: i32,
    section_height: i32,
    xaero: Option<&'static ColorConfig>,
    built: std::cell::OnceCell<IndexedSection<'a>>,
}

impl<'a> SectionSlot<'a> {
    #[inline]
    fn get(&self) -> &IndexedSection<'a> {
        self.built.get_or_init(|| {
            build_section(
                self.src,
                self.modern,
                self.format,
                self.start_height,
                self.section_height,
                self.xaero,
            )
        })
    }
}

/// biome lookup for one (x, cy, z) inside a section — mirrors the old
/// (get_biome_packed | biome_int) dispatch.
#[inline]
fn biome_at<'a>(
    s: &IndexedSection<'a>,
    chunk_biomes: Option<&[u8]>,
    modern: bool,
    format: ChunkFormat,
    cx: usize,
    cy: i32,
    cz: usize,
    y_min: i32,
) -> BiomeKey<'a> {
    if modern {
        let names = s.biome_names.get_or_init(|| build_biome_names(s));
        if !names.is_empty() {
            let bi = ((cy as usize >> 2) & 0xF) * 16 + ((cz >> 2) & 0xF) * 4 + ((cx >> 2) & 0xF);
            BiomeKey::Name(names.get(bi).copied().unwrap_or(""))
        } else {
            BiomeKey::Name(get_biome_packed(s, cx, cy as usize, cz))
        }
    } else {
        BiomeKey::Idx(biome_int(
            chunk_biomes,
            cx,
            s.section_height + cy,
            cz,
            format,
            y_min,
        ))
    }
}

/// fallback for pathological biome packings (bits >= 64).
fn get_biome_packed<'a>(s: &IndexedSection<'a>, x: usize, y: usize, z: usize) -> &'a str {
    if s.biome_palette.is_empty() || s.biome.bits == 0 {
        return s
            .biome_palette
            .first()
            .map(chunk::palette_entry_name)
            .unwrap_or("");
    }
    let index = (y >> 2 & 0xF) * 16 + (z >> 2 & 0xF) * 4 + (x >> 2 & 0xF);
    let pal_idx = s.biome.get(index) as usize;
    s.biome_palette
        .get(pal_idx)
        .map(chunk::palette_entry_name)
        .unwrap_or("")
}

/// Resolves the 64 biome cells once, on the first tinted hit. Returns an
/// empty Vec for pathological packings (bits >= 64) — biome_at then uses
/// get_biome_packed per call.
fn build_biome_names<'a>(s: &IndexedSection<'a>) -> Vec<&'a str> {
    if s.biome.bits >= 64 {
        return Vec::new();
    }
    fn name<'a>(e: Option<&'a Tag>) -> &'a str {
        e.map(chunk::palette_entry_name).unwrap_or("")
    }
    let mut v = Vec::with_capacity(64);
    if s.biome_palette.is_empty() {
        v.resize(64, "");
    } else if s.biome.bits == 0 {
        v.resize(64, name(s.biome_palette.first()));
    } else {
        for i in 0..64 {
            let pi = s.biome.get(i) as usize;
            v.push(name(s.biome_palette.get(pi)));
        }
    }
    v
}

/// rgb of a resolved entry; the color is resolved on first hit and biome
/// is only evaluated when the entry is tinted.
#[inline]
fn entry_rgb<'a>(
    memo: &mut RgbMemo,
    cm: &ColorMap,
    e: &PalEntry<'_>,
    bk: impl FnOnce() -> BiomeKey<'a>,
) -> u32 {
    let cv = e.cell.get_or_init(|| resolve_color(cm, e.name, e.props));
    if !cv.tinted {
        cv.rgb
    } else {
        memo.get(cm, e, cv, bk())
    }
}

/// small multiplicative hasher for pointer/integer keys — the memo is hit
/// once or more per rendered column and std's SipHash dominates otherwise.
#[derive(Default)]
struct PtrHasher(u64);
impl std::hash::Hasher for PtrHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            self.0 = self.0.rotate_left(23);
        }
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.0 = (self.0 ^ i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = self.0.rotate_left(23);
    }
}

fn biome_id_to_name(id: i32) -> &'static str {
    static M: std::sync::OnceLock<std::collections::HashMap<i32, &'static str>> =
        std::sync::OnceLock::new();
    let m = M.get_or_init(|| {
        crate::registry::biome_name_to_id()
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_i64().map(|i| (i as i32, k.as_str())))
                    .collect()
            })
            .unwrap_or_default()
    });
    m.get(&id).copied().unwrap_or("minecraft:plains")
}

fn biome_int(biomes: Option<&[u8]>, x: usize, y: i32, z: usize, format: ChunkFormat, y_min: i32) -> i32 {
    let Some(b) = biomes else { return 0 };
    match format {
        ChunkFormat::P17w47a => b.get(z * 16 + x).copied().unwrap_or(0) as i32 & 0xFF,
        ChunkFormat::P18w06a => nbt::raw_i32_or(b, z * 16 + x, 0),
        ChunkFormat::P19w36a | ChunkFormat::P20w17a => {
            nbt::raw_i32_or(b, (y >> 2 << 4) as usize + (z >> 2 << 2) + (x >> 2), 0)
        }
        ChunkFormat::P21w06a | ChunkFormat::P21w15a => {
            let i = ((y - y_min * 16) >> 2 << 4) + (z >> 2 << 2) as i32 + (x >> 2) as i32;
            if i < 0 {
                0
            } else {
                nbt::raw_i32_or(b, i as usize, 0)
            }
        }
        _ => 0,
    }
}

/// section internals are the 21w37a+ layout (per-section block_states/
/// biomes compounds). NOTE: the section *list* stays under Level until
/// 21w43a — locating it is chunk::sections' job.
pub fn is_modern(format: ChunkFormat) -> bool {
    matches!(
        format,
        ChunkFormat::P21w37a | ChunkFormat::P21w43a | ChunkFormat::P26_3s7
    )
}

/// spanning packed block-state layout (before the 20w17a non-spanning
/// layout); Legacy15w32a never reaches the packed paths.
fn is_spanning(format: ChunkFormat) -> bool {
    matches!(
        format,
        ChunkFormat::P17w47a | ChunkFormat::P18w06a | ChunkFormat::P19w36a
    )
}

fn index_sections<'a>(
    sections: Option<&'a Vec<Tag>>,
    format: ChunkFormat,
    height: i32,
    y_min: i32,
    xaero: Option<&'static ColorConfig>,
) -> Vec<Option<SectionSlot<'a>>> {
    let modern = is_modern(format);
    let (lo, hi) = if modern {
        (y_min, 1 + (height >> 4))
    } else {
        (0, 1 + (height.clamp(0, 255) >> 4))
    };
    let s_max = (hi - lo).max(0) as usize;
    let mut indexed: Vec<Option<SectionSlot>> = std::iter::repeat_with(|| None).take(s_max).collect();
    let Some(list) = sections else { return indexed };
    for t in list {
        let Some(s) = t.as_compound() else { continue };
        let y = s
            .get("Y")
            .and_then(|t| t.as_i64())
            .map(|v| v as i32)
            .unwrap_or(y_min - 1);
        if y < lo || y >= hi {
            continue;
        }
        let idx = (y - lo) as usize;
        if idx >= s_max {
            continue;
        }
        let abs_h = if modern { height - y_min * 16 } else { height.clamp(0, 255) };
        // section contents are built lazily (SectionSlot::get) on the first
        // column that reaches the section.
        indexed[idx] = Some(SectionSlot {
            src: s,
            modern,
            format,
            start_height: if (abs_h >> 4) == idx as i32 {
                abs_h & 0xF
            } else {
                15
            },
            section_height: (idx as i32 + lo) * 16,
            xaero,
            built: std::cell::OnceCell::new(),
        });
    }
    indexed
}

/// Extracts block/biome palettes + packed data refs from one section
/// compound and resolves every palette entry's flags. Colors, biome names
/// and packed-data decoding are deferred to first use.
fn build_section<'a>(
    s: &'a Compound,
    modern: bool,
    format: ChunkFormat,
    start_height: i32,
    section_height: i32,
    xaero: Option<&'static ColorConfig>,
) -> IndexedSection<'a> {
    fn raw<'a>(t: Option<&'a Tag>) -> PackedSrc<'a> {
        t.and_then(|t| t.as_bytes()).map(PackedSrc::Raw).unwrap_or_default()
    }
    let (palette, block_src): (&'a [Tag], PackedSrc<'a>) = if modern {
        let bs = s.get("block_states").and_then(Tag::as_compound);
        (
            bs.and_then(|b| b.get("palette"))
                .and_then(Tag::as_list)
                .map(|(_, l)| l.as_slice())
                .unwrap_or_default(),
            raw(bs.and_then(|b| b.get("data"))),
        )
    } else {
        (
            s.get("Palette")
                .and_then(Tag::as_list)
                .map(|(_, l)| l.as_slice())
                .unwrap_or_default(),
            raw(s.get("BlockStates")),
        )
    };
    let block = PackedData::with_bits(block_src, block_src.n_longs() >> 6, is_spanning(format));
    let (biome_palette, biome_src): (&'a [Tag], PackedSrc<'a>) = if modern {
        let bc = s.get("biomes").and_then(Tag::as_compound);
        (
            bc.and_then(|b| b.get("palette"))
                .and_then(Tag::as_list)
                .map(|(_, l)| l.as_slice())
                .unwrap_or_default(),
            raw(bc.and_then(|b| b.get("data"))),
        )
    } else {
        (&[], PackedSrc::default())
    };
    // biome packed bits = number of longs (upstream biomeBits).
    let biome = PackedData::with_bits(biome_src, biome_src.n_longs(), false);

    let entries: Vec<PalEntry> = palette
        .iter()
        .map(|t| resolve_entry(chunk::palette_entry_name(t), palette_props(Some(t)), xaero))
        .collect();
    // every palette index the packed data can produce must be in range for a
    // whole-section skip to be sound (oob entries resolve as opaque).
    let bits = block.bits;
    let no_oob = bits == 0 || (bits < 64 && entries.len() >= (1usize << bits));
    let all_transparent = no_oob
        && !entries.is_empty()
        && entries.iter().all(|e| e.flags & F_TRANSPARENT != 0);
    let caves_inert = no_oob
        && !entries.is_empty()
        && entries
            .iter()
            .all(|e| e.flags & (F_TRANSPARENT | F_FOLIAGE) != 0);
    let x_inert = xaero.is_some()
        && no_oob
        && !entries.is_empty()
        && entries
            .iter()
            .all(|e| e.xflags & (X_INVIS | X_AIR) != 0);
    IndexedSection {
        entries,
        block,
        biome_names: std::cell::OnceCell::new(),
        biome_palette,
        biome,
        start_height,
        section_height,
        all_transparent,
        caves_inert,
        x_inert,
    }
}

pub struct RenderOut<'a> {
    pub pixel_buffer: &'a mut [u32],
    pub water_pixels: Option<&'a mut [u32]>,
    pub terrain_heights: &'a mut [i16],
    pub water_heights: Option<&'a mut [i16]>,
    pub water: bool,
    pub height: i32,
    /// nether-style "cave dimension": scan past the ceiling and render the
    /// first floor below open space (Xaero caveModeType==2 semantics).
    pub full_cave: bool,
    /// set by draw_chunk when the xaero pipeline ran — the pixel buffers
    /// carry the xaero layout (water_* = overlay data, pixel alpha 0xFE
    /// marks glowing blocks), so the shade pass must be xaero_shade.
    pub xaero: bool,
}

const WATER_DUMMY_NAME: &str = "minecraft:water";

/// drawChunk — top-down surface render with water handling.
pub fn draw_chunk(
    root: &Compound,
    cm: &ColorMap,
    format: ChunkFormat,
    x: usize,
    z: usize,
    scale: i32,
    out: &mut RenderOut,
) {
    match format {
        ChunkFormat::Null => return,
        ChunkFormat::Legacy15w32a => return draw_chunk_legacy(root, cm, x, z, scale, out),
        _ => {}
    }
    if let Some(xc) = cm.xaero_cfg() {
        out.xaero = true;
        return draw_chunk_xaero(root, xc, cm, format, x, z, scale, out);
    }
    let Some((list, container)) = chunk::sections(root) else { return };
    let y_min = nbt::int_from(Some(container), "yPos", -4);
    let modern = is_modern(format);
    let height = out.height;
    let indexed = index_sections(Some(list), format, height, y_min, cm.xaero_cfg());
    if indexed.is_empty() {
        return;
    }
    let chunk_biomes = if modern {
        None
    } else {
        container.get("Biomes").and_then(|t| t.as_bytes())
    };
    let scale_bits = scale.ilog2() as i32;
    let abs_h = if modern {
        height - y_min * 16
    } else {
        height.clamp(0, 255)
    };
    let start = ((abs_h >> 4).min(indexed.len() as i32 - 1)).max(0) as usize;
    let mut memo = RgbMemo::default();
    let oob = oob_entry();
    let water_entry = resolve_entry(WATER_DUMMY_NAME, None, None);
    let row = 512usize >> scale_bits;

    for cx in (0..16usize).step_by(scale as usize) {
        'zloop: for cz in (0..16usize).step_by(scale as usize) {
            let pixel_index = (z + (cz >> scale_bits)) * row + (x + (cx >> scale_bits));
            let mut water_depth = false;
            // top-down section sequence; a rev+flatten iterator rebuilds the
            // same order per column without a per-chunk Vec allocation.
            for slot in indexed[..=start].iter().rev().flatten() {
                let s = slot.get();
                if s.all_transparent {
                    continue;
                }
                // packed-data cursor: stepping y by -1 subtracts exactly 256
                // indexes — the cursor tracks the long/bit offset
                // incrementally (chunk::PackedCursor).
                let uniform = s.block.bits == 0;
                let top_index = chunk::location_to_index(cx, s.start_height as usize, cz);
                let mut cur = s.block.cursor(top_index, 256);
                for cy in (0..=s.start_height).rev() {
                    let e = if uniform {
                        s.entries.first().unwrap_or(&oob)
                    } else {
                        let pi = cur.get() as usize;
                        if cy > 0 {
                            cur.prev();
                        }
                        s.entries.get(pi).unwrap_or(&oob)
                    };
                    if e.flags & F_TRANSPARENT != 0 {
                        if uniform {
                            // every row holds the same entry — nothing else
                            // in this section can block the column.
                            break;
                        }
                        continue;
                    }
                    let bk = || biome_at(s, chunk_biomes, modern, format, cx, cy, cz, y_min);
                    let y_abs = s.section_height + cy;
                    if out.water {
                        let wh = out.water_heights.as_deref_mut().unwrap();
                        if !water_depth {
                            out.pixel_buffer[pixel_index] = entry_rgb(&mut memo, cm, e, bk);
                            wh[pixel_index] = y_abs as i16;
                        }
                        if e.flags & F_WATER != 0 {
                            water_depth = true;
                            if uniform {
                                break;
                            }
                            continue;
                        } else if e.flags & F_WATERLOGGED != 0 {
                            out.pixel_buffer[pixel_index] =
                                entry_rgb(&mut memo, cm, &water_entry, bk);
                            if let Some(wp) = out.water_pixels.as_deref_mut() {
                                wp[pixel_index] = entry_rgb(&mut memo, cm, e, bk);
                            }
                            wh[pixel_index] = y_abs as i16;
                            out.terrain_heights[pixel_index] = (y_abs - 1) as i16;
                            continue 'zloop;
                        } else if let Some(wp) = out.water_pixels.as_deref_mut() {
                            wp[pixel_index] = entry_rgb(&mut memo, cm, e, bk);
                        }
                    } else {
                        out.pixel_buffer[pixel_index] = entry_rgb(&mut memo, cm, e, bk);
                    }
                    out.terrain_heights[pixel_index] = y_abs as i16;
                    continue 'zloop;
                }
            }
        }
    }
}

/// One merged overlay layer during the xaero column scan — same-state
/// translucent blocks merge, accumulating opacity (Xaero's Overlay +
/// OverlayBuilder, opacity capped at 15).
struct MergeOverlay {
    entry: usize, // PalEntry address — merge key (same palette slot+state)
    rgb: u32,
    alpha: u8,
    opacity: u8,
}

/// Xaero's MapWriter.loadPixel: translucent blocks (water/ice/stained
/// glass/…) stack as overlays above the surface block; the surface block
/// and its height feed the MapPixel shading pass (xaero_shade in render.rs).
/// Per column the scan writes:
///   pixel_buffer    = tinted surface rgb, alpha 0xFE = glowing, 0xFF normal
///   terrain_heights = effective surface height (short blocks -1)
///   water_heights   = packed (top_height & 0xFFF) | sun_left << 12
///   water_pixels    = acc_transparency<<24 | premultiplied overlay rgb
/// Overlays are composited in shade: rgb*(9+sun)/24*acc_tr + overlay_rgb.
fn draw_chunk_xaero(
    root: &Compound,
    xc: &'static ColorConfig,
    cm: &ColorMap,
    format: ChunkFormat,
    x: usize,
    z: usize,
    scale: i32,
    out: &mut RenderOut,
) {
    let Some((list, container)) = chunk::sections(root) else { return };
    let y_min = nbt::int_from(Some(container), "yPos", -4);
    let modern = is_modern(format);
    let height = out.height;
    let indexed = index_sections(Some(list), format, height, y_min, Some(xc));
    if indexed.is_empty() {
        return;
    }
    let chunk_biomes = if modern {
        None
    } else {
        container.get("Biomes").and_then(|t| t.as_bytes())
    };
    let scale_bits = scale.ilog2() as i32;
    let abs_h = if modern {
        height - y_min * 16
    } else {
        height.clamp(0, 255)
    };
    let start = ((abs_h >> 4).min(indexed.len() as i32 - 1)).max(0) as usize;
    let mut memo = RgbMemo::default();
    let oob = oob_entry();
    let water_entry = resolve_entry(WATER_DUMMY_NAME, None, Some(xc));
    let row = 512usize >> scale_bits;
    let full_cave = out.full_cave;
    let (Some(wp_buf), Some(wh_buf)) = (out.water_pixels.as_deref_mut(), out.water_heights.as_deref_mut()) else {
        return;
    };

    for cx in (0..16usize).step_by(scale as usize) {
        'zloop: for cz in (0..16usize).step_by(scale as usize) {
            let pixel_index = (z + (cz >> scale_bits)) * row + (x + (cx >> scale_bits));
            // underair: "open space above" — in surface mode it stays true;
            // in full-cave mode the ceiling keeps it false until the first
            // air gap below it (Xaero's underair/shouldEnterGround).
            let mut underair = true;
            let mut entered = !full_cave;
            let mut top_h = i32::MIN;
            // merged overlay accumulator + the running composite — a new
            // overlay type folds the current one into (sun, acc_tr, ov)
            // before starting a fresh merge record.
            let mut cur_ov: Option<MergeOverlay> = None;
            let mut sun = 15i32;
            let mut acc_tr = 1.0f32;
            let mut ov_sum = [0f32; 3];
            macro_rules! flush_ov {
                () => {
                    if let Some(o) = cur_ov.take() {
                        let tr = o.alpha as f32 / 255.0;
                        // daylight render: max(sun, light) = 15 -> brightness
                        // is 1, so the overlay contributes rgb*tr*rem.
                        let inten = tr * acc_tr;
                        ov_sum[0] += (o.rgb >> 16 & 0xFF) as f32 * inten;
                        ov_sum[1] += (o.rgb >> 8 & 0xFF) as f32 * inten;
                        ov_sum[2] += (o.rgb & 0xFF) as f32 * inten;
                        sun = (sun - o.opacity as i32).max(0);
                        acc_tr *= 1.0 - tr;
                    }
                };
            }
            for slot in indexed[..=start].iter().rev().flatten() {
                let s = slot.get();
                if s.x_inert {
                    continue;
                }
                let uniform = s.block.bits == 0;
                let top_index = chunk::location_to_index(cx, s.start_height as usize, cz);
                let mut cur = s.block.cursor(top_index, 256);
                for cy in (0..=s.start_height).rev() {
                    let e = if uniform {
                        s.entries.first().unwrap_or(&oob)
                    } else {
                        let pi = cur.get() as usize;
                        if cy > 0 {
                            cur.prev();
                        }
                        s.entries.get(pi).unwrap_or(&oob)
                    };
                    if e.xflags & X_INVIS != 0 {
                        if uniform {
                            break;
                        }
                        continue;
                    }
                    if !entered {
                        // first solid block closes the ceiling column;
                        // air/overlay/waterlogged blocks pass through.
                        if e.xflags & (X_AIR | X_OVERLAY) == 0 && e.flags & F_WATERLOGGED == 0 {
                            entered = true;
                            underair = false;
                        }
                        if uniform {
                            break;
                        }
                        continue;
                    }
                    if e.xflags & X_AIR != 0 {
                        underair = true;
                        if uniform {
                            break;
                        }
                        continue;
                    }
                    if !underair {
                        if uniform {
                            break;
                        }
                        continue;
                    }
                    let y_abs = s.section_height + cy;
                    if e.xflags & X_OVERLAY != 0 {
                        if top_h == i32::MIN {
                            top_h = y_abs;
                        }
                        let key = e as *const PalEntry as usize;
                        match cur_ov.as_mut() {
                            Some(o) if o.entry == key => {
                                o.opacity = o.opacity.saturating_add(e.xop).min(15);
                            }
                            _ => {
                                flush_ov!();
                                let mut rgb = entry_rgb(&mut memo, cm, e, || {
                                    biome_at(s, chunk_biomes, modern, format, cx, cy, cz, y_min)
                                });
                                if e.xflags & X_GLOW != 0 {
                                    let sum = ((rgb >> 16 & 0xFF) + (rgb >> 8 & 0xFF) + (rgb & 0xFF)) as f32;
                                    let b = (407.0 / sum).max(1.0);
                                    let r = ((rgb >> 16 & 0xFF) as f32 * b).min(255.0) as u32;
                                    let g = ((rgb >> 8 & 0xFF) as f32 * b).min(255.0) as u32;
                                    let bl = ((rgb & 0xFF) as f32 * b).min(255.0) as u32;
                                    rgb = r << 16 | g << 8 | bl;
                                }
                                cur_ov = Some(MergeOverlay {
                                    entry: key,
                                    rgb,
                                    alpha: e.xalpha,
                                    opacity: e.xop,
                                });
                            }
                        }
                        if uniform {
                            // every remaining level repeats this overlay
                            if let Some(o) = cur_ov.as_mut() {
                                o.opacity = o.opacity.saturating_add(e.xop * cy as u8).min(15);
                            }
                            break;
                        }
                        continue;
                    }
                    if e.flags & F_WATERLOGGED != 0 {
                        // contained fluid renders as a water overlay first
                        // (Xaero processes fluidState before the block itself)
                        if top_h == i32::MIN {
                            top_h = y_abs;
                        }
                        let key = &water_entry as *const PalEntry as usize;
                        match cur_ov.as_mut() {
                            Some(o) if o.entry == key => {
                                o.opacity = o.opacity.saturating_add(1).min(15);
                            }
                            _ => {
                                flush_ov!();
                                cur_ov = Some(MergeOverlay {
                                    entry: key,
                                    rgb: entry_rgb(&mut memo, cm, &water_entry, || {
                                        biome_at(s, chunk_biomes, modern, format, cx, cy, cz, y_min)
                                    }),
                                    alpha: water_entry.xalpha,
                                    opacity: 1,
                                });
                            }
                        }
                    }
                    // surface block — composite the accumulated overlays.
                    let mut rgb = entry_rgb(&mut memo, cm, e, || {
                        biome_at(s, chunk_biomes, modern, format, cx, cy, cz, y_min)
                    });
                    flush_ov!();
                    let glowing = e.xflags & X_GLOW != 0;
                    if glowing {
                        let sum = ((rgb >> 16 & 0xFF) + (rgb >> 8 & 0xFF) + (rgb & 0xFF)) as f32;
                        let b = (407.0 / sum).max(1.0);
                        let r = ((rgb >> 16 & 0xFF) as f32 * b).min(255.0) as u32;
                        let g = ((rgb >> 8 & 0xFF) as f32 * b).min(255.0) as u32;
                        let bl = ((rgb & 0xFF) as f32 * b).min(255.0) as u32;
                        rgb = r << 16 | g << 8 | bl;
                    }
                    out.pixel_buffer[pixel_index] =
                        (if glowing { 0xFE } else { 0xFF }) << 24 | (rgb & 0xFF_FFFF);
                    out.terrain_heights[pixel_index] =
                        (y_abs - if e.xflags & X_SHORT != 0 { 1 } else { 0 }) as i16;
                    if top_h == i32::MIN {
                        top_h = y_abs;
                    }
                    if acc_tr < 1.0 {
                        wp_buf[pixel_index] = ((acc_tr * 255.0) as u32) << 24
                            | (ov_sum[0].min(255.0) as u32) << 16
                            | (ov_sum[1].min(255.0) as u32) << 8
                            | ov_sum[2].min(255.0) as u32;
                        wh_buf[pixel_index] =
                            ((top_h & 0xFFF) as i16) | (((sun.max(0) & 0xF) as i16) << 12);
                    }
                    continue 'zloop;
                }
            }
        }
    }
}
/// carries its state bitset, so (entry address, biome) -> rgb can be memoized
/// instead of re-running get_state + the string-keyed color lookups.
#[derive(Default)]
struct RgbMemo {
    map: std::collections::HashMap<(usize, usize), u32, std::hash::BuildHasherDefault<PtrHasher>>,
}
impl RgbMemo {
    #[inline]
    fn get(&mut self, cm: &ColorMap, e: &PalEntry<'_>, cv: &CellVal, bk: BiomeKey) -> u32 {
        let bk_key = match bk {
            BiomeKey::Name(s) => (s.as_ptr() as usize) ^ (s.len() << 20),
            BiomeKey::Idx(i) => (i as usize) << 40 | 1,
        };
        let key = (e as *const PalEntry<'_> as usize, bk_key);
        if let Some(&c) = self.map.get(&key) {
            return c;
        }
        let c = cm.get_rgb_state(e.name, cv.state.as_ref(), bk);
        self.map.insert(key, c);
        c
    }
}

/// drawLayer — single Y layer.
pub fn draw_layer(
    root: &Compound,
    cm: &ColorMap,
    format: ChunkFormat,
    x: usize,
    z: usize,
    scale: i32,
    pixel_buffer: &mut [u32],
    height: i32,
) {
    if format == ChunkFormat::Null || format == ChunkFormat::Legacy15w32a {
        return;
    }
    let Some((list, container)) = chunk::sections(root) else { return };
    let y_min = nbt::int_from(Some(container), "yPos", -4);
    let modern = is_modern(format);
    let target_y = if modern { height >> 4 } else { height.clamp(0, 255) >> 4 };
    let mut target: Option<&Compound> = None;
    for t in list {
        let Some(s) = t.as_compound() else { continue };
        if s.get("Y").and_then(|t| t.as_i64()).map(|v| v as i32) == Some(target_y) {
            target = Some(s);
            break;
        }
    }
    let Some(section) = target else { return };
    let s = build_section(section, modern, format, 15, target_y * 16, cm.xaero_cfg());
    let chunk_biomes = if modern {
        None
    } else {
        container.get("Biomes").and_then(|t| t.as_bytes())
    };
    let scale_bits = scale.ilog2() as i32;
    let abs_h = if modern {
        height - y_min * 16
    } else {
        height.clamp(0, 255)
    };
    let cy = (abs_h & 0xF) as usize;
    let oob = oob_entry();
    let mut memo = RgbMemo::default();
    for cx in (0..16usize).step_by(scale as usize) {
        for cz in (0..16usize).step_by(scale as usize) {
            let index = chunk::location_to_index(cx, cy, cz);
            let e = if s.block.bits == 0 {
                s.entries.first().unwrap_or(&oob)
            } else {
                let pi = s.block.get(index) as usize;
                s.entries.get(pi).unwrap_or(&oob)
            };
            if e.flags & F_TRANSPARENT != 0 {
                continue;
            }
            let pixel_index =
                (z + (cz >> scale_bits)) * (512 >> scale_bits) + (x + (cx >> scale_bits));
            let bk = || biome_at(&s, chunk_biomes, modern, format, cx, cy as i32, cz, y_min);
            pixel_buffer[pixel_index] = entry_rgb(&mut memo, cm, e, bk);
        }
    }
}

/// drawCaves — first opaque non-foliage block below the surface blocks.
pub fn draw_caves(
    root: &Compound,
    cm: &ColorMap,
    format: ChunkFormat,
    x: usize,
    z: usize,
    scale: i32,
    pixel_buffer: &mut [u32],
    terrain_heights: &mut [i16],
    height: i32,
) {
    if format == ChunkFormat::Null || format == ChunkFormat::Legacy15w32a {
        return;
    }
    let (sections_opt, container) = match chunk::sections(root) {
        Some((l, c)) => (Some(l), c),
        None => (None, root),
    };
    let y_min = nbt::int_from(Some(container), "yPos", -4);
    let modern = is_modern(format);
    let indexed = index_sections(sections_opt, format, height, y_min, cm.xaero_cfg());
    let chunk_biomes = if modern {
        None
    } else {
        container.get("Biomes").and_then(|t| t.as_bytes())
    };
    let scale_bits = scale.ilog2() as i32;
    let abs_h = if modern {
        height - y_min * 16
    } else {
        height.clamp(0, 255)
    };
    let start = ((abs_h >> 4).min(indexed.len() as i32 - 1)).max(0) as usize;
    let oob = oob_entry();
    let mut memo = RgbMemo::default();
    for cx in (0..16usize).step_by(scale as usize) {
        'zloop: for cz in (0..16usize).step_by(scale as usize) {
            let pixel_index =
                (z + (cz >> scale_bits)) * (512 >> scale_bits) + (x + (cx >> scale_bits));
            let mut ignored = 0;
            let mut done_skipping = false;
            // same flattened top-down order for every column, no Vec needed.
            for slot in indexed[..=start].iter().rev().flatten() {
                let s = slot.get();
                if s.caves_inert {
                    continue;
                }
                let uniform = s.block.bits == 0;
                let top_index = chunk::location_to_index(cx, s.start_height as usize, cz);
                let mut cur = s.block.cursor(top_index, 256);
                for cy in (0..=s.start_height).rev() {
                    let e = if uniform {
                        s.entries.first().unwrap_or(&oob)
                    } else {
                        let pi = cur.get() as usize;
                        if cy > 0 {
                            cur.prev();
                        }
                        s.entries.get(pi).unwrap_or(&oob)
                    };
                    if e.flags & (F_TRANSPARENT | F_FOLIAGE) == 0 {
                        if done_skipping {
                            let bk =
                                || biome_at(s, chunk_biomes, modern, format, cx, cy, cz, y_min);
                            pixel_buffer[pixel_index] = entry_rgb(&mut memo, cm, e, bk);
                            terrain_heights[pixel_index] = (s.section_height + cy) as i16;
                            continue 'zloop;
                        }
                        ignored += 1;
                    } else if ignored > 0 {
                        done_skipping = true;
                    }
                    if uniform {
                        // every row holds the same entry — repeated rows can
                        // only repeat the first row's effect.
                        break;
                    }
                }
            }
        }
    }
}

// --- legacy (15w32a, <= 1.12) ------------------------------------------------

fn draw_chunk_legacy(
    root: &Compound,
    cm: &ColorMap,
    x: usize,
    z: usize,
    scale: i32,
    out: &mut RenderOut,
) {
    let Some((sections, level)) = chunk::sections(root) else { return };
    let height = out.height.clamp(0, 255);
    let y_max = 1 + (height >> 4);
    let mut indexed: Vec<Option<(&[u8], &[u8])>> = vec![None; y_max as usize];
    for t in sections {
        let Some(s) = t.as_compound() else { continue };
        let y = s.get("Y").and_then(|t| t.as_i64()).unwrap_or(-1) as i32;
        if y < 0 || y >= y_max {
            continue;
        }
        if s.contains_key("Blocks") && s.contains_key("Data") {
            indexed[y as usize] = Some((
                nbt::byte_array_from(Some(s), "Blocks").unwrap_or(&[]),
                nbt::byte_array_from(Some(s), "Data").unwrap_or(&[]),
            ));
        }
    }
    let biomes = nbt::byte_array_from(Some(level), "Biomes");
    let scale_bits = scale.ilog2() as i32;
    for cx in (0..16usize).step_by(scale as usize) {
        'zloop: for cz in (0..16usize).step_by(scale as usize) {
            let pixel_index =
                (z + (cz >> scale_bits)) * (512 >> scale_bits) + (x + (cx >> scale_bits));
            let biome = biomes
                .and_then(|b| b.get(cz * 16 + cx).copied())
                .unwrap_or(0) as i32;
            for i in (0..=(height >> 4)).rev() {
                let Some((blocks, block_data)) = indexed[i as usize] else {
                    continue;
                };
                let start = if (height >> 4) == i { height & 0xF } else { 15 };
                for cy in (0..=start).rev() {
                    let index = chunk::location_to_index(cx, cy as usize, cz);
                    let block = blocks.get(index).copied().unwrap_or(0) as i32;
                    let data = ((if index & 1 == 0 {
                        block_data.get(index >> 1).copied().unwrap_or(0)
                    } else {
                        block_data.get(index >> 1).copied().unwrap_or(0) >> 4
                    }) & 0x0F) as i32;
                    if legacy_is_transparent(block) {
                        continue;
                    }
                    let key = (block << 4) + data;
                    if out.water {
                        let wh = out.water_heights.as_deref_mut().unwrap();
                        out.pixel_buffer[pixel_index] = cm.get_rgb_legacy(key, biome);
                        wh[pixel_index] = (i * 16 + cy) as i16;
                        if legacy_is_water(block) {
                            if let Some(wp) = out.water_pixels.as_deref_mut() {
                                wp[pixel_index] = cm.get_rgb_legacy(key, biome);
                            }
                            continue 'zloop;
                        }
                    } else {
                        out.pixel_buffer[pixel_index] = cm.get_rgb_legacy(key, biome);
                    }
                    out.terrain_heights[pixel_index] = (i * 16 + cy) as i16;
                    continue 'zloop;
                }
            }
        }
    }
}
