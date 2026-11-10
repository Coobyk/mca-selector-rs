//! Port of version.*: DataVersion-dispatched ChunkRenderers + ColorMappings.
//! Dispatch via floorEntry, like upstream's VersionHandler.

use crate::bitset::BitSet;
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
}

impl ColorMap {
    /// Modern path: palette-entry name + properties, biome name or legacy id.
    pub fn get_rgb(
        &self,
        name: &str,
        props: Option<&Compound>,
        biome: BiomeKey,
    ) -> u32 {
        match self {
            ColorMap::Null => 0,
            ColorMap::Legacy(_) => 0xFF00_0000,
            ColorMap::Config(c) => {
                let biome_name = match biome {
                    BiomeKey::Name(s) => s,
                    BiomeKey::Idx(id) => biome_id_to_name(id),
                };
                c.get_color(name, biome_name, props).color | 0xFF00_0000
            }
        }
    }

    /// get_rgb with the block-state bitset pre-resolved by the caller
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

    pub fn is_foliage(&self, name: &str) -> bool {
        colors::color_properties().foliage.contains(name)
    }
    pub fn is_transparent(&self, name: &str) -> bool {
        colors::color_properties().transparent.contains(name)
    }
    pub fn is_water(&self, name: &str) -> bool {
        colors::color_properties().water.contains(name)
    }
    pub fn is_waterlogged(&self, props: Option<&Compound>) -> bool {
        props
            .and_then(|p| p.get("waterlogged"))
            .and_then(|t| t.as_string())
            .map(|s| s == "true")
            .unwrap_or(false)
    }
}

/// floorEntry dispatch for the color mapping (VersionHandler.getImpl).
pub fn color_mapping(data_version: i32) -> ColorMap {
    const TABLE: &[(i32, fn() -> &'static ColorConfig)] = &[
        (1451, colors::cfg_17w47a),
        (1901, colors::cfg_18w43a),
        (2200, colors::cfg_19w34a),
        (2504, colors::cfg_20w06a),
        (2681, colors::cfg_20w45a),
        (2834, colors::cfg_21w37a),
        (3080, colors::cfg_22w11a),
        (3207, colors::cfg_23w12a),
        (3940, colors::cfg_24w18a),
        (4536, colors::cfg_25w32a),
        (4764, colors::cfg_26_1_s1),
        (4883, colors::cfg_26_2_s1),
        (4998, colors::cfg_26_3_s1),
    ];
    if data_version < 100 {
        return ColorMap::Null;
    }
    if data_version < 1451 {
        return ColorMap::Legacy(legacy_colors());
    }
    let mut i = 0;
    for (k, _) in TABLE {
        if *k <= data_version {
            i += 1;
        } else {
            break;
        }
    }
    ColorMap::Config(TABLE[i - 1].1())
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
        (100, ChunkFormat::Legacy15w32a),
        (1451, ChunkFormat::P17w47a),
        (1466, ChunkFormat::P18w06a),
        (2203, ChunkFormat::P19w36a),
        (2529, ChunkFormat::P20w17a),
        (2694, ChunkFormat::P21w06a),
        (2709, ChunkFormat::P21w15a),
        (2834, ChunkFormat::P21w37a),
        (2844, ChunkFormat::P21w43a),
        (5009, ChunkFormat::P26_3s7),
    ];
    let mut fmt = ChunkFormat::Null;
    for (k, f) in TABLE {
        if *k <= data_version {
            fmt = *f;
        } else {
            break;
        }
    }
    fmt
}

// ---------------------------------------------------------------------------
// helpers

#[inline]
pub(crate) fn packed_get(data: Option<&[u8]>, index: usize, bits: usize, indexes_per_long: usize) -> u64 {
    let Some(d) = data else { return 0 };
    let li = index / indexes_per_long;
    let start_bit = (index % indexes_per_long) * bits;
    let v = nbt::raw_i64_or(d, li, 0) as u64;
    (v >> start_bit) & ((1u64 << bits) - 1)
}

pub(crate) fn palette_name<'a>(entry: Option<&'a Tag>, format: ChunkFormat) -> &'a str {
    match entry {
        Some(Tag::Compound(c)) => {
            if format == ChunkFormat::P26_3s7 {
                c.get("")
                    .or_else(|| c.get("id"))
                    .and_then(|t| t.as_string())
                    .unwrap_or("")
            } else {
                nbt::string_or(Some(c), "Name", "")
            }
        }
        Some(Tag::String(s)) => s.as_str(),
        _ => "",
    }
}

fn palette_props<'a>(entry: Option<&'a Tag>) -> Option<&'a Compound> {
    match entry {
        Some(Tag::Compound(c)) => c.get("Properties").and_then(Tag::as_compound),
        _ => None,
    }
}

// --- resolved palette entries ------------------------------------------------
//
// The old inner loop re-did all of this per block visit: Compound lookups for
// "Name"/"Properties" (IndexMap string hashing), HashSet<String> lookups for
// transparent/water/foliage and a waterlogged string compare, plus a BitSet
// allocation inside get_state for every column. All of it depends only on the
// palette entry. The name/props flags are resolved once per palette slot when
// a section is built; the color-mapping part (state bitset + rgb) is resolved
// lazily on first hit via PalEntry.cell, and whole sections are built lazily
// via SectionSlot.built.

const F_TRANSPARENT: u8 = 0b0001;
const F_WATER: u8 = 0b0010;
const F_WATERLOGGED: u8 = 0b0100;
const F_FOLIAGE: u8 = 0b1000;

struct PalEntry<'a> {
    name: &'a str,
    /// block-state properties — kept so the color can be resolved lazily.
    props: Option<&'a Compound>,
    /// resolved color (+state for tinted entries); filled on first opaque hit.
    cell: std::cell::OnceCell<CellVal>,
    flags: u8,
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
fn resolve_entry<'a>(name: &'a str, props: Option<&'a Compound>) -> PalEntry<'a> {
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
    PalEntry {
        name,
        props,
        cell: std::cell::OnceCell::new(),
        flags,
    }
}

/// Color half of the old resolve_entry — same result, computed on first hit.
/// get_state (per-prop hash lookups + a BitSet) is skipped when the mapped
/// color is a state-independent Single that is not tinted, matching the old
/// code's `s.color(state)` where Single ignores `state` entirely.
fn resolve_color(cm: &ColorMap, name: &str, props: Option<&Compound>) -> CellVal {
    match cm {
        ColorMap::Config(c) => {
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
        ColorMap::Null => CellVal {
            state: None,
            rgb: 0,
            tinted: false,
        },
        ColorMap::Legacy(_) => CellVal {
            state: None,
            rgb: 0xFF00_0000,
            tinted: false,
        },
    }
}

/// What the old code produced for a missing/empty palette entry: name ""
/// (not transparent, not water, not foliage; the cell resolves to
/// MISSING_COLOR -> opaque white for Config mappings, 0 for Null,
/// 0xFF000000 for Legacy).
fn oob_entry() -> PalEntry<'static> {
    PalEntry {
        name: "",
        props: None,
        cell: std::cell::OnceCell::new(),
        flags: 0,
    }
}

struct IndexedSection<'a> {
    /// resolved block palette; entry i is block palette index i.
    entries: Vec<PalEntry<'a>>,
    /// raw packed block data (BE longs); absent/short => uniform section,
    /// every block reads entries[0]. Longs are read on demand — the column
    /// scan only touches a handful of words per column.
    data: Option<&'a [u8]>,
    bits: usize,
    indexes_per_long: usize,
    /// resolved biome names for the 4x4x4 cells ([yq*16+zq*4+xq]) — built on
    /// the first tinted hit; an empty resolved table means the packed
    /// encoding is pathological and get_biome_packed must be used.
    biome_names: std::cell::OnceCell<Vec<&'a str>>,
    // raw biome palette kept for the pathological-bipl fallback path only.
    biome_palette: &'a [Tag],
    biome_data: Option<&'a [u8]>,
    biome_bits: usize,
    biome_indexes_per_long: usize,
    start_height: i32,
    section_height: i32,
    /// provable "every block maps to a transparent entry" => sections can be
    /// skipped wholesale by the column scan.
    all_transparent: bool,
    /// same for the caves renderer (transparent OR foliage never counts).
    caves_inert: bool,
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
        let names = s
            .biome_names
            .get_or_init(|| build_biome_names(s, format));
        if !names.is_empty() {
            let bi = ((cy as usize >> 2) & 0xF) * 16 + ((cz >> 2) & 0xF) * 4 + ((cx >> 2) & 0xF);
            BiomeKey::Name(names.get(bi).copied().unwrap_or(""))
        } else {
            BiomeKey::Name(get_biome_packed(s, cx, cy as usize, cz, format))
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

/// fallback for pathological biome packings (biome_indexes_per_long == 0);
/// identical to the old per-call implementation.
fn get_biome_packed<'a>(s: &IndexedSection<'a>, x: usize, y: usize, z: usize, format: ChunkFormat) -> &'a str {
    if s.biome_palette.is_empty() {
        return "";
    }
    if s.biome_bits == 0 || s.biome_data.is_none() {
        return palette_name(s.biome_palette.first(), format);
    }
    let index = (y >> 2 & 0xF) * 16 + (z >> 2 & 0xF) * 4 + (x >> 2 & 0xF);
    let pal_idx = packed_get(s.biome_data, index, s.biome_bits, s.biome_indexes_per_long) as usize;
    palette_name(s.biome_palette.get(pal_idx), format)
}

/// Resolves the 64 biome cells once, on the first tinted hit. Returns an
/// empty Vec for pathological packings (bb >= 64) — biome_at then uses
/// get_biome_packed per call, matching the old eager code including its
/// panic behaviour on corrupt data.
fn build_biome_names<'a>(s: &IndexedSection<'a>, format: ChunkFormat) -> Vec<&'a str> {
    if s.biome_bits >= 64 {
        return Vec::new();
    }
    let mut v = Vec::with_capacity(64);
    if s.biome_palette.is_empty() {
        v.resize(64, "");
    } else if s.biome_bits == 0 || s.biome_data.is_none() {
        v.resize(64, palette_name(s.biome_palette.first(), format));
    } else {
        for i in 0..64 {
            let pi = packed_get(s.biome_data, i, s.biome_bits, s.biome_indexes_per_long) as usize;
            v.push(palette_name(s.biome_palette.get(pi), format));
        }
    }
    v
}

/// Read one BE u64 out of raw packed long[] data; 0 when out of range —
/// the same result the old decoded-Vec `longs.get(li).unwrap_or(0)` gave
/// (chunks_exact(8) never emitted a partial tail long either).
#[inline]
fn packed_u64(data: Option<&[u8]>, li: usize) -> u64 {
    match data {
        Some(d) if li < d.len() >> 3 => {
            u64::from_be_bytes(d[li * 8..li * 8 + 8].try_into().unwrap())
        }
        _ => 0,
    }
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

fn sections_and_level<'a>(
    root: &'a Compound,
    format: ChunkFormat,
) -> (Option<&'a Compound>, Option<(u8, &'a Vec<Tag>)>, i32) {
    match format {
        ChunkFormat::P21w37a | ChunkFormat::P21w43a | ChunkFormat::P26_3s7 => {
            let y_min = nbt::int_from(Some(root), "yPos", -4);
            (Some(root), nbt::sections_from(Some(root), "sections"), y_min)
        }
        _ => {
            let level = nbt::level_from_root(root);
            let secs = nbt::sections_from(level, "Sections");
            let y_min = nbt::int_from(level.or(Some(root)), "yPos", -4);
            (level, secs, y_min)
        }
    }
}

/// modern chunk NBT layout (sections/biomes at root, not under Level).
pub fn is_modern(format: ChunkFormat) -> bool {
    matches!(
        format,
        ChunkFormat::P21w37a | ChunkFormat::P21w43a | ChunkFormat::P26_3s7
    )
}

fn index_sections<'a>(
    sections: Option<(u8, &'a Vec<Tag>)>,
    format: ChunkFormat,
    height: i32,
    y_min: i32,
) -> Vec<Option<SectionSlot<'a>>> {
    let modern = is_modern(format);
    let (lo, hi) = if modern {
        (y_min, 1 + (height >> 4))
    } else {
        (0, 1 + (height.clamp(0, 255) >> 4))
    };
    let s_max = (hi - lo).max(0) as usize;
    let mut indexed: Vec<Option<SectionSlot>> = std::iter::repeat_with(|| None).take(s_max).collect();
    let Some((_, list)) = sections else { return indexed };
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
) -> IndexedSection<'a> {
    let (palette, data): (&'a [Tag], Option<&'a [u8]>) = if modern {
        let bs = s.get("block_states").and_then(Tag::as_compound);
        (
            bs.and_then(|b| b.get("palette"))
                .and_then(Tag::as_list)
                .map(|(_, l)| l.as_slice())
                .unwrap_or_default(),
            bs.and_then(|b| b.get("data")).and_then(|t| t.as_bytes()),
        )
    } else {
        (
            s.get("Palette")
                .and_then(Tag::as_list)
                .map(|(_, l)| l.as_slice())
                .unwrap_or_default(),
            s.get("BlockStates").and_then(|t| t.as_bytes()),
        )
    };
    let bits = data.map(|d| d.len() >> 9).unwrap_or(0);
    let ipl = if bits == 0 { 1 } else { (64.0 / bits as f64) as usize };
    let (biome_palette, biome_data): (&'a [Tag], Option<&'a [u8]>) = if modern {
        let bc = s.get("biomes").and_then(Tag::as_compound);
        (
            bc.and_then(|b| b.get("palette"))
                .and_then(Tag::as_list)
                .map(|(_, l)| l.as_slice())
                .unwrap_or_default(),
            bc.and_then(|b| b.get("data")).and_then(|t| t.as_bytes()),
        )
    } else {
        (&[], None)
    };
    let bb = biome_data.map(|d| d.len() >> 3).unwrap_or(0);
    let bipl = if bb == 0 { 1 } else { (64.0 / bb as f64) as usize };

    let entries: Vec<PalEntry> = palette
        .iter()
        .map(|t| resolve_entry(palette_name(Some(t), format), palette_props(Some(t))))
        .collect();
    // every palette index the packed data can produce must be in range for a
    // whole-section skip to be sound (oob entries resolve as opaque).
    let no_oob =
        bits == 0 || (bits < 64 && entries.len() >= (1usize << bits));
    let all_transparent = no_oob
        && !entries.is_empty()
        && entries.iter().all(|e| e.flags & F_TRANSPARENT != 0);
    let caves_inert = no_oob
        && !entries.is_empty()
        && entries
            .iter()
            .all(|e| e.flags & (F_TRANSPARENT | F_FOLIAGE) != 0);
    IndexedSection {
        entries,
        data,
        bits,
        indexes_per_long: ipl,
        biome_names: std::cell::OnceCell::new(),
        biome_palette,
        biome_data,
        biome_bits: bb,
        biome_indexes_per_long: bipl,
        start_height,
        section_height,
        all_transparent,
        caves_inert,
    }
}

pub struct RenderOut<'a> {
    pub pixel_buffer: &'a mut [u32],
    pub water_pixels: Option<&'a mut [u32]>,
    pub terrain_heights: &'a mut [i16],
    pub water_heights: Option<&'a mut [i16]>,
    pub water: bool,
    pub height: i32,
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
    let (level, sections, y_min) = sections_and_level(root, format);
    let modern = is_modern(format);
    let height = out.height;
    let indexed = index_sections(sections, format, height, y_min);
    if indexed.is_empty() {
        return;
    }
    let chunk_biomes = if modern {
        None
    } else {
        level.and_then(|l| l.get("Biomes")).and_then(|t| t.as_bytes())
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
    let water_entry = resolve_entry(WATER_DUMMY_NAME, None);
    let row = 512usize >> scale_bits;

    for cx in (0..16usize).step_by(scale as usize) {
        'zloop: for cz in (0..16usize).step_by(scale as usize) {
            let pixel_index = (z + (cz >> scale_bits)) * row + (x + (cx >> scale_bits));
            let mut water_depth = false;
            let col = cz * 16 + cx;
            // top-down section sequence; a rev+flatten iterator rebuilds the
            // same order per column without a per-chunk Vec allocation.
            for slot in indexed[..=start].iter().rev().flatten() {
                let s = slot.get();
                if s.all_transparent {
                    continue;
                }
                // packed-data cursor: block index = y*256 + z*16 + x, so
                // stepping y by -1 always subtracts exactly 256 — the long
                // index / bit offset can be updated incrementally without a
                // division per block.
                let uniform = s.bits == 0;
                let ipl = s.indexes_per_long;
                let top_index = s.start_height as usize * 256 + col;
                let mut li = top_index / ipl;
                let mut rem = top_index % ipl;
                let step_li = 256 / ipl;
                let step_rem = 256 % ipl;
                let mask = (1u64 << s.bits) - 1;
                for cy in (0..=s.start_height).rev() {
                    let e = if uniform {
                        s.entries.first().unwrap_or(&oob)
                    } else {
                        let v = packed_u64(s.data, li);
                        let pi = ((v >> (rem * s.bits)) & mask) as usize;
                        if cy > 0 {
                            if rem < step_rem {
                                rem += ipl;
                                li -= 1;
                            }
                            rem -= step_rem;
                            li -= step_li;
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


/// per-chunk rgb memo for tinted blocks: each resolved PalEntry already
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
    let (level, sections, y_min) = sections_and_level(root, format);
    let modern = is_modern(format);
    let target_y = if modern { height >> 4 } else { height.clamp(0, 255) >> 4 };
    let Some((_, list)) = sections else { return };
    let mut target: Option<&Compound> = None;
    for t in list {
        let Some(s) = t.as_compound() else { continue };
        if s.get("Y").and_then(|t| t.as_i64()).map(|v| v as i32) == Some(target_y) {
            target = Some(s);
            break;
        }
    }
    let Some(section) = target else { return };
    let s = build_section(section, modern, format, 15, target_y * 16);
    let chunk_biomes = if modern {
        None
    } else {
        level.and_then(|l| l.get("Biomes")).and_then(|t| t.as_bytes())
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
    let mask = (1u64 << s.bits) - 1;
    for cx in (0..16usize).step_by(scale as usize) {
        for cz in (0..16usize).step_by(scale as usize) {
            let index = cy * 256 + cz * 16 + cx;
            let e = if s.bits == 0 {
                s.entries.first().unwrap_or(&oob)
            } else {
                let v = packed_u64(s.data, index / s.indexes_per_long);
                let pi = ((v >> ((index % s.indexes_per_long) * s.bits)) & mask) as usize;
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
    let (level, sections, y_min) = sections_and_level(root, format);
    let modern = is_modern(format);
    let indexed = index_sections(sections, format, height, y_min);
    let chunk_biomes = if modern {
        None
    } else {
        level.and_then(|l| l.get("Biomes")).and_then(|t| t.as_bytes())
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
            let col = cz * 16 + cx;
            // same flattened top-down order for every column, no Vec needed.
            for slot in indexed[..=start].iter().rev().flatten() {
                let s = slot.get();
                if s.caves_inert {
                    continue;
                }
                let uniform = s.bits == 0;
                let ipl = s.indexes_per_long;
                let top_index = s.start_height as usize * 256 + col;
                let mut li = top_index / ipl;
                let mut rem = top_index % ipl;
                let step_li = 256 / ipl;
                let step_rem = 256 % ipl;
                let mask = (1u64 << s.bits) - 1;
                for cy in (0..=s.start_height).rev() {
                    let e = if uniform {
                        s.entries.first().unwrap_or(&oob)
                    } else {
                        let v = packed_u64(s.data, li);
                        let pi = ((v >> (rem * s.bits)) & mask) as usize;
                        if cy > 0 {
                            if rem < step_rem {
                                rem += ipl;
                                li -= 1;
                            }
                            rem -= step_rem;
                            li -= step_li;
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
    let Some(level) = nbt::level_from_root(root) else { return };
    let Some((_, sections)) = nbt::sections_from(Some(level), "Sections") else {
        return;
    };
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
                    let index = (cy as usize) * 256 + cz * 16 + cx;
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
