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
        Some(Tag::Compound(c)) => c
            .get("Properties")
            .or_else(|| c.get("properties"))
            .and_then(Tag::as_compound),
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
const X_WATER: u8 = 0b10_0000;

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
    // the aggregated name->flag map folds the per-set probes into one.
    let pf = cp.flags.get(name).copied().unwrap_or(0);
    let mut flags = pf & (F_TRANSPARENT | F_WATER | F_FOLIAGE);
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
        if pf & colors::FLAG_AIR != 0 {
            xflags |= X_AIR;
        }
        if let Some(m) = xc.xaero_meta.get(name) {
            if m.invis {
                xflags |= X_INVIS;
            }
            if m.overlay {
                xflags |= X_OVERLAY;
            }
            if m.water {
                xflags |= X_WATER;
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
    // -- per-palette-index pass masks (only when block.bits <= 6, so every
    // index the packed data can produce fits a u64 bit; a 0 mask simply
    // disables its fast path)
    /// xaero scan: index skips unconditionally when the column is entered
    /// (air | invis).
    x_skip: u64,
    /// subset of x_skip that sets underair — air but not invis (invis is
    /// tested first and leaves underair untouched). Doubles as the
    /// "can re-open underair" mask for the entered+!underair fast path;
    /// u64::MAX when the palette is too wide for masks so that path never
    /// skips (its x_skip-gated use is unreachable then anyway).
    x_air: u64,
    /// draw_chunk: transparent entries.
    trans_skip: u64,
    /// draw_caves: transparent | foliage (valid only before the first
    /// solid block — see the scan's `ignored` gate).
    caves_skip: u64,
    // -- non-spanning column-cursor precomputes (per-section constants;
    // keep the per-block step division-free)
    /// indexes per long (PackedData::ipl).
    col_ipl: u32,
    /// ceil(2^24 / ipl): `index * col_div >> 24` == index / ipl for the
    /// in-section index range (< 4096).
    col_div: u32,
    /// longs per 256-index down-step.
    col_step_li: u32,
    /// bit-shift decrement per step ((256 % ipl) * bits).
    col_step_shift: i32,
    /// shift add-back on wrap (ipl * bits).
    col_ipl_shift: i32,
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
    /// boxed so the per-chunk slot table stays small — the Vec is
    /// allocated and zeroed for every rendered chunk.
    built: std::cell::OnceCell<Box<IndexedSection<'a>>>,
}

impl<'a> SectionSlot<'a> {
    #[inline]
    fn get(&self) -> &IndexedSection<'a> {
        let b = self.built.get_or_init(|| {
            Box::new(build_section(
                self.src,
                self.modern,
                self.format,
                self.start_height,
                self.section_height,
                self.xaero,
            ))
        });
        &**b
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

/// tinted rgb bilinearly blended across the 4 surrounding 4x4x4 biome
/// cells — vanilla's BiomeColors smoothing: cell centers sit at +1.5
/// inside each cell, so block positions blend toward the neighboring
/// cell instead of switching tint at the cell boundary (the 4x4-block
/// tint grid otherwise visible in tinted water/grass).
/// `cy` is the block's y within the section — blending is horizontal
/// only; the block's own y cell is used for every corner sample.
#[allow(clippy::too_many_arguments)]
fn entry_rgb_blended<'a>(
    memo: &mut RgbMemo,
    cm: &ColorMap,
    e: &PalEntry<'_>,
    s: &IndexedSection<'a>,
    chunk_biomes: Option<&[u8]>,
    modern: bool,
    format: ChunkFormat,
    cx: usize,
    cy: i32,
    cz: usize,
    y_min: i32,
) -> u32 {
    let cv = e.cell.get_or_init(|| resolve_color(cm, e.name, e.props));
    if !cv.tinted {
        return cv.rgb;
    }
    let mut cell = |qx: usize, qz: usize| {
        memo.get(cm, e, cv, biome_at(
            s,
            chunk_biomes,
            modern,
            format,
            (qx << 2).min(15),
            cy,
            (qz << 2).min(15),
            y_min,
        ))
    };
    let (qx, qz) = (cx >> 2, cz >> 2);
    let (fx, fz) = ((cx & 3) as f32, (cz & 3) as f32);
    // weight of the neighboring cell toward the nearer boundary
    let (nx, wx) = if fx < 1.5 {
        (qx.saturating_sub(1), (1.5 - fx) / 4.0)
    } else {
        ((qx + 1).min(3), (fx - 1.5) / 4.0)
    };
    let (nz, wz) = if fz < 1.5 {
        (qz.saturating_sub(1), (1.5 - fz) / 4.0)
    } else {
        ((qz + 1).min(3), (fz - 1.5) / 4.0)
    };
    let c00 = cell(qx, qz);
    if wx == 0.0 && wz == 0.0 {
        return c00;
    }
    let c10 = cell(nx, qz);
    let c01 = cell(qx, nz);
    let c11 = cell(nx, nz);
    let ch = |c: u32, w: f32, r: &mut f32, g: &mut f32, b: &mut f32| {
        *r += (c >> 16 & 0xFF) as f32 * w;
        *g += (c >> 8 & 0xFF) as f32 * w;
        *b += (c & 0xFF) as f32 * w;
    };
    let (mut r, mut g, mut b) = (0f32, 0f32, 0f32);
    ch(c00, (1.0 - wx) * (1.0 - wz), &mut r, &mut g, &mut b);
    ch(c10, wx * (1.0 - wz), &mut r, &mut g, &mut b);
    ch(c01, (1.0 - wx) * wz, &mut r, &mut g, &mut b);
    ch(c11, wx * wz, &mut r, &mut g, &mut b);
    0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
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
    // per-index pass masks for the column scans' fast path — only dense
    // enough to cover every producible index when bits <= 6.
    let (x_skip, x_air, trans_skip, caves_skip) = if block.bits <= 6 {
        let mut xs = 0u64;
        let mut xa = 0u64;
        let mut ts = 0u64;
        let mut cs = 0u64;
        for (i, e) in entries.iter().enumerate().take(64) {
            let b = 1u64 << i;
            let xf = e.xflags;
            if xf & (X_INVIS | X_AIR) != 0 {
                xs |= b;
            }
            // air re-opens underair; invis is tested first and does not.
            if xf & X_AIR != 0 && xf & X_INVIS == 0 {
                xa |= b;
            }
            if e.flags & F_TRANSPARENT != 0 {
                ts |= b;
            }
            if e.flags & (F_TRANSPARENT | F_FOLIAGE) != 0 {
                cs |= b;
            }
        }
        (xs, xa, ts, cs)
    } else {
        (0, u64::MAX, 0, 0)
    };
    // non-spanning column cursor constants (unused when spanning/bits==0).
    let col_ipl = block.ipl() as u32;
    let step_rem = 256 % col_ipl as usize;
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
        x_skip,
        x_air,
        trans_skip,
        caves_skip,
        col_ipl,
        col_div: ((1u32 << 24) + col_ipl - 1) / col_ipl,
        col_step_li: (256 / col_ipl as usize) as u32,
        col_step_shift: (step_rem * block.bits) as i32,
        col_ipl_shift: (col_ipl as usize * block.bits) as i32,
    }
}

/// bitmask for `bits`-wide reads — matches chunk.rs's private helper.
#[inline]
fn bmask(bits: usize) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// spanning-layout read (< 20w17a) — chunk.rs's private helper duplicated
/// so the column cursor doesn't need a division-free PackedData::get.
#[inline]
fn get_spanning(src: &PackedSrc<'_>, bit: usize, bits: usize) -> u64 {
    let li = bit >> 6;
    let off = bit & 63;
    if off + bits > 64 {
        let prev = src.long(li) >> off;
        let next = src.long(li + 1) & bmask(off + bits - 64);
        (next << (64 - off)) | prev
    } else {
        (src.long(li) >> off) & bmask(bits)
    }
}

/// Incremental top-down column cursor (steps of 256 indexes — one section
/// row). For the non-spanning layout `prev` is a pair of integer updates:
/// the ipl constant and the 256/ipl split are precomputed per section, so
/// no division runs per block (PackedCursor::prev calls ipl() whenever
/// the in-long offset wraps). Spanning-layout sections fall back to an
/// absolute-index PackedData::get — those formats are legacy.
struct ColCursor<'a> {
    src: PackedSrc<'a>,
    /// absolute index — spanning layout only.
    index: usize,
    spanning: bool,
    bits: usize,
    li: usize,
    shift: i32,
    step_li: usize,
    step_shift: i32,
    ipl_shift: i32,
    mask: u64,
}

fn col_cursor<'a>(s: &IndexedSection<'a>, index: usize) -> ColCursor<'a> {
    let b = s.block;
    let mut c = ColCursor {
        src: b.src,
        index,
        spanning: b.spanning,
        bits: b.bits,
        li: 0,
        shift: 0,
        step_li: s.col_step_li as usize,
        step_shift: s.col_step_shift,
        ipl_shift: s.col_ipl_shift,
        mask: u64::MAX,
    };
    if !b.spanning {
        // index / ipl via the precomputed ceil(2^24/ipl) multiplier —
        // exact for all in-section indexes (< 4096 < 2^12).
        let li = ((index as u64 * s.col_div as u64) >> 24) as usize;
        c.li = li;
        c.shift = ((index - li * s.col_ipl as usize) * b.bits) as i32;
        c.mask = bmask(b.bits);
    }
    c
}

impl ColCursor<'_> {
    #[inline]
    fn get(&self) -> u64 {
        if self.spanning {
            get_spanning(&self.src, self.index * self.bits, self.bits)
        } else {
            (self.src.long(self.li) >> self.shift) & self.mask
        }
    }

    /// move one section row (256 indexes) down — saturates like
    /// PackedCursor::prev on corrupt data.
    #[inline]
    fn prev(&mut self) {
        if self.spanning {
            self.index = self.index.saturating_sub(256);
        } else {
            self.shift -= self.step_shift;
            if self.shift < 0 {
                self.shift += self.ipl_shift;
                self.li = self.li.saturating_sub(1);
            }
            self.li = self.li.saturating_sub(self.step_li);
        }
    }
}

pub struct RenderOut<'a> {
    pub pixel_buffer: &'a mut [u32],
    pub water_pixels: Option<&'a mut [u32]>,
    pub terrain_heights: &'a mut [i16],
    pub water_heights: Option<&'a mut [i16]>,
    pub water: bool,
    /// RenderSettings.shade == false: no overlay merge at all — every
    /// translucent block (water/leaves/glass) renders its own flat color
    /// as the surface pixel, matching upstream's flat unshaded render.
    pub flat: bool,
    pub height: i32,
    /// nether-style "cave dimension": scan past the ceiling and render the
    /// first floor below open space (Xaero caveModeType==2 semantics).
    pub full_cave: bool,
    /// RenderSettings.shade_water == false: water blocks are skipped
    /// entirely — the column continues and the underwater terrain renders
    /// as the surface. Unlike `flat`, water is hidden, not drawn opaque.
    pub no_water: bool,
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
    let oob = oob_entry();
    let water_entry = resolve_entry(WATER_DUMMY_NAME, None, None);
    let row = 512usize >> scale_bits;
    // top-down sequence of present sections — filtered once per chunk
    // instead of re-checking every Option slot per column.
    let mut present = Vec::with_capacity(start + 1);
    present.extend(indexed[..=start].iter().rev().flatten());

    with_memo(|memo| {
        for cx in (0..16usize).step_by(scale as usize) {
            'zloop: for cz in (0..16usize).step_by(scale as usize) {
                let pixel_index = (z + (cz >> scale_bits)) * row + (x + (cx >> scale_bits));
                let mut water_depth = false;
                for slot in &present {
                    let s = slot.get();
                    if s.all_transparent {
                        continue;
                    }
                    // packed-data cursor: stepping y by -1 subtracts exactly
                    // 256 indexes — tracked incrementally (ColCursor).
                    let uniform = s.block.bits == 0;
                    let top_index =
                        chunk::location_to_index(cx, s.start_height as usize, cz);
                    let mut cur = col_cursor(s, top_index);
                    for cy in (0..=s.start_height).rev() {
                        let e = if uniform {
                            s.entries.first().unwrap_or(&oob)
                        } else {
                            let pi = cur.get() as usize;
                            if cy > 0 {
                                cur.prev();
                            }
                            // transparent runs skip with a single mask test
                            // — no palette-entry load. (& 63 keeps the
                            // shift defined for wide-palette sections.)
                            if (s.trans_skip >> (pi & 63)) & 1 != 0 {
                                continue;
                            }
                            s.entries.get(pi).unwrap_or(&oob)
                        };
                        if e.flags & F_TRANSPARENT != 0 {
                            if uniform {
                                // every row holds the same entry — nothing
                                // else in this section can block the column.
                                break;
                            }
                            continue;
                        }
                        let bk =
                            || biome_at(s, chunk_biomes, modern, format, cx, cy, cz, y_min);
                        let y_abs = s.section_height + cy;
                        if e.flags & F_WATER != 0 && out.no_water {
                            // "water: off" hides water — the underwater
                            // terrain below renders as the surface.
                            if uniform {
                                break;
                            }
                            continue;
                        }
                        if out.water {
                            let wh = out.water_heights.as_deref_mut().unwrap();
                            if !water_depth {
                                out.pixel_buffer[pixel_index] = entry_rgb(memo, cm, e, bk);
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
                                    entry_rgb(memo, cm, &water_entry, bk);
                                if let Some(wp) = out.water_pixels.as_deref_mut() {
                                    wp[pixel_index] = entry_rgb(memo, cm, e, bk);
                                }
                                wh[pixel_index] = y_abs as i16;
                                out.terrain_heights[pixel_index] = (y_abs - 1) as i16;
                                continue 'zloop;
                            } else if let Some(wp) = out.water_pixels.as_deref_mut() {
                                wp[pixel_index] = entry_rgb(memo, cm, e, bk);
                            }
                        } else {
                            out.pixel_buffer[pixel_index] = entry_rgb(memo, cm, e, bk);
                        }
                        out.terrain_heights[pixel_index] = y_abs as i16;
                        continue 'zloop;
                    }
                }
            }
        }
    });
}

/// One merged overlay layer during the xaero column scan — same-state
/// translucent blocks merge, accumulating opacity (Xaero's Overlay +
/// OverlayBuilder, opacity capped at 15).
struct MergeOverlay {
    entry: usize, // PalEntry address — merge key (same palette slot+state)
    rgb: u32,
    alpha: u8,
    opacity: u8,
    /// merged column depth — drives the water coverage curve
    /// (upstream: ratio = 0.5 - 0.5/40 * depth).
    depth: u16,
    /// water-family fluid (water/bubble/contained fluid): the whole
    /// column merges into one overlay whose coverage scales with depth.
    water: bool,
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
    let oob = oob_entry();
    let water_entry = resolve_entry(WATER_DUMMY_NAME, None, Some(xc));
    let row = 512usize >> scale_bits;
    let full_cave = out.full_cave;
    let (Some(wp_buf), Some(wh_buf)) = (out.water_pixels.as_deref_mut(), out.water_heights.as_deref_mut()) else {
        return;
    };
    // top-down sequence of present sections — filtered once per chunk
    // instead of re-checking every Option slot per column.
    let mut present = Vec::with_capacity(start + 1);
    present.extend(indexed[..=start].iter().rev().flatten());

    with_memo(|memo| {
        for cx in (0..16usize).step_by(scale as usize) {
            'zloop: for cz in (0..16usize).step_by(scale as usize) {
                let pixel_index = (z + (cz >> scale_bits)) * row + (x + (cx >> scale_bits));
                // underair: "open space above" — in surface mode it stays
                // true; in full-cave mode the ceiling keeps it false until
                // the first air gap below it (Xaero's underair /
                // shouldEnterGround).
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
                            // water coverage follows column depth as an
                            // exponential: shallow keeps a clear water tint
                            // (floor ~46%) and the share decays toward a
                            // permanent ~15% — deep water never goes fully
                            // opaque, so the seabed always faintly shows.
                            let tr = if o.water {
                                (0.85 - 0.33 * (-(o.depth as f32) / 12.0).exp())
                                    .min(1.0)
                            } else {
                                o.alpha as f32 / 255.0
                            };
                            // daylight render: max(sun, light) = 15 ->
                            // brightness is 1, so the overlay contributes
                            // rgb*tr*rem.
                            let inten = tr * acc_tr;
                            ov_sum[0] += (o.rgb >> 16 & 0xFF) as f32 * inten;
                            ov_sum[1] += (o.rgb >> 8 & 0xFF) as f32 * inten;
                            ov_sum[2] += (o.rgb & 0xFF) as f32 * inten;
                            // submerged terrain keeps a little skylight so
                            // deep water shading doesn't go black — floor
                            // the sun loss at 5 for water columns.
                            sun = (sun - o.opacity as i32)
                                .max(if o.water { sun.min(5) } else { 0 });
                            acc_tr *= 1.0 - tr;
                        }
                    };
                }
                for slot in &present {
                    let s = slot.get();
                    if s.x_inert {
                        continue;
                    }
                    let uniform = s.block.bits == 0;
                    let top_index =
                        chunk::location_to_index(cx, s.start_height as usize, cz);
                    let mut cur = col_cursor(s, top_index);
                    for cy in (0..=s.start_height).rev() {
                        let e = if uniform {
                            s.entries.first().unwrap_or(&oob)
                        } else {
                            let pi = cur.get() as usize;
                            if cy > 0 {
                                cur.prev();
                            }
                            // entered columns pass straight through
                            // air/invis runs — one mask test replaces the
                            // palette-entry load + flag chain; air still
                            // re-opens underair for the cave scan. Below a
                            // sealed ceiling (entered && !underair) only
                            // air changes state, so everything else also
                            // skips on a single test.
                            if entered {
                                if !underair && (s.x_air >> (pi & 63)) & 1 == 0 {
                                    continue;
                                }
                                if (s.x_skip >> (pi & 63)) & 1 != 0 {
                                    underair =
                                        underair || (s.x_air >> (pi & 63)) & 1 != 0;
                                    continue;
                                }
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
                            if e.xflags & (X_AIR | X_OVERLAY) == 0
                                && e.flags & F_WATERLOGGED == 0
                            {
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
                        let water_ov = e.xflags & X_WATER != 0;
                        if water_ov && out.no_water {
                            // "water: off" hides water — the column keeps
                            // scanning so the underwater terrain renders as
                            // the surface.
                            if uniform {
                                break;
                            }
                            continue;
                        }
                        // overlay merge only when shading is on; "shade:
                        // off" renders every translucent block flat.
                        if e.xflags & X_OVERLAY != 0 && !out.flat && (out.water || !water_ov) {
                            if top_h == i32::MIN {
                                top_h = y_abs;
                            }
                            // water variants merge into the shared fluid
                            // overlay — depth must cover the whole column
                            // for the coverage curve to see it.
                            let key = if water_ov {
                                &water_entry as *const PalEntry as usize
                            } else {
                                e as *const PalEntry as usize
                            };
                            match cur_ov.as_mut() {
                                Some(o) if o.entry == key => {
                                    o.opacity = o.opacity.saturating_add(e.xop).min(15);
                                    o.depth = o.depth.saturating_add(1);
                                }
                                _ => {
                                    flush_ov!();
                                    let mut rgb = entry_rgb_blended(
                                        memo, cm, e, s, chunk_biomes, modern,
                                        format, cx, cy, cz, y_min,
                                    );
                                    if e.xflags & X_GLOW != 0 {
                                        let sum = ((rgb >> 16 & 0xFF)
                                            + (rgb >> 8 & 0xFF)
                                            + (rgb & 0xFF)) as f32;
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
                                        depth: 1,
                                        water: water_ov,
                                    });
                                }
                            }
                            if uniform {
                                // every remaining level repeats this overlay
                                if let Some(o) = cur_ov.as_mut() {
                                    o.opacity =
                                        o.opacity.saturating_add(e.xop * cy as u8).min(15);
                                    o.depth = o.depth.saturating_add(cy as u16);
                                }
                                break;
                            }
                            continue;
                        }
                        if e.flags & F_WATERLOGGED != 0 && !out.flat && out.water {
                            // contained fluid renders as a water overlay
                            // first (Xaero processes fluidState before the
                            // block itself)
                            if top_h == i32::MIN {
                                top_h = y_abs;
                            }
                            let key = &water_entry as *const PalEntry as usize;
                            match cur_ov.as_mut() {
                                Some(o) if o.entry == key => {
                                    o.opacity = o.opacity.saturating_add(1).min(15);
                                    o.depth = o.depth.saturating_add(1);
                                }
                                _ => {
                                    flush_ov!();
                                    cur_ov = Some(MergeOverlay {
                                        entry: key,
                                        rgb: entry_rgb_blended(
                                            memo, cm, &water_entry, s,
                                            chunk_biomes, modern, format,
                                            cx, cy, cz, y_min,
                                        ),
                                        alpha: water_entry.xalpha,
                                        opacity: 1,
                                        depth: 1,
                                        water: true,
                                    });
                                }
                            }
                        }
                        // surface block — composite the accumulated overlays.
                        let mut rgb = entry_rgb_blended(
                            memo, cm, e, s, chunk_biomes, modern, format,
                            cx, cy, cz, y_min,
                        );
                        flush_ov!();
                        let glowing = e.xflags & X_GLOW != 0;
                        if glowing {
                            let sum =
                                ((rgb >> 16 & 0xFF) + (rgb >> 8 & 0xFF) + (rgb & 0xFF)) as f32;
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
                            wh_buf[pixel_index] = ((top_h & 0xFFF) as i16)
                                | (((sun.max(0) & 0xF) as i16) << 12);
                        }
                        continue 'zloop;
                    }
                }
            }
        }
    });
}

/// (entry address, biome) -> rgb memo for the tinted path — carries the
/// palette entry's state bitset, so hits skip get_state + the string-keyed
/// color lookups. Direct-mapped (a slot collision just recomputes — the
/// result is identical) and generation-stamped: entries are keyed on
/// chunk-local pointers, so `begin` is bumped once per chunk scan and
/// stale generations can never hit. Kept in TLS so the table is allocated
/// once per worker thread, not per chunk.
const MEMO_SLOTS: usize = 4096;
struct RgbMemo {
    gen: u32,
    tags: Box<[u32; MEMO_SLOTS]>,
    keys: Box<[(usize, usize); MEMO_SLOTS]>,
    vals: Box<[u32; MEMO_SLOTS]>,
}
impl RgbMemo {
    fn new() -> Self {
        RgbMemo {
            gen: 0,
            tags: Box::new([0; MEMO_SLOTS]),
            keys: Box::new([(0, 0); MEMO_SLOTS]),
            vals: Box::new([0; MEMO_SLOTS]),
        }
    }
    /// start a new chunk scan: prior entries keyed chunk-local pointers.
    #[inline]
    fn begin(&mut self) {
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.tags.fill(0);
            self.gen = 1;
        }
    }
    #[inline]
    fn get(&mut self, cm: &ColorMap, e: &PalEntry<'_>, cv: &CellVal, bk: BiomeKey) -> u32 {
        let bk_key = match bk {
            BiomeKey::Name(s) => (s.as_ptr() as usize) ^ (s.len() << 20),
            BiomeKey::Idx(i) => (i as usize) << 40 | 1,
        };
        let key = (e as *const PalEntry<'_> as usize, bk_key);
        // two multiplies fold both words; the high bits carry the entropy.
        let h = (key.0 as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(23)
            ^ (key.1 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        let i = (((h >> 32) ^ h) as usize) & (MEMO_SLOTS - 1);
        if self.tags[i] == self.gen && self.keys[i] == key {
            return self.vals[i];
        }
        let c = cm.get_rgb_state(e.name, cv.state.as_ref(), bk);
        self.tags[i] = self.gen;
        self.keys[i] = key;
        self.vals[i] = c;
        c
    }
}

std::thread_local! {
    static RGB_MEMO: std::cell::RefCell<RgbMemo> = std::cell::RefCell::new(RgbMemo::new());
}

/// run `f` with the worker thread's memo, bumped to a fresh generation.
#[inline]
fn with_memo<R>(f: impl FnOnce(&mut RgbMemo) -> R) -> R {
    RGB_MEMO.with(|m| {
        let mut g = m.borrow_mut();
        g.begin();
        f(&mut *g)
    })
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
    with_memo(|memo| {
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
                let bk =
                    || biome_at(&s, chunk_biomes, modern, format, cx, cy as i32, cz, y_min);
                pixel_buffer[pixel_index] = entry_rgb(memo, cm, e, bk);
            }
        }
    });
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
    // top-down sequence of present sections — filtered once per chunk.
    let mut present = Vec::with_capacity(start + 1);
    present.extend(indexed[..=start].iter().rev().flatten());
    with_memo(|memo| {
        for cx in (0..16usize).step_by(scale as usize) {
            'zloop: for cz in (0..16usize).step_by(scale as usize) {
                let pixel_index =
                    (z + (cz >> scale_bits)) * (512 >> scale_bits) + (x + (cx >> scale_bits));
                let mut ignored = 0;
                let mut done_skipping = false;
                for slot in &present {
                    let s = slot.get();
                    if s.caves_inert {
                        continue;
                    }
                    let uniform = s.block.bits == 0;
                    let top_index =
                        chunk::location_to_index(cx, s.start_height as usize, cz);
                    let mut cur = col_cursor(s, top_index);
                    for cy in (0..=s.start_height).rev() {
                        let e = if uniform {
                            s.entries.first().unwrap_or(&oob)
                        } else {
                            let pi = cur.get() as usize;
                            if cy > 0 {
                                cur.prev();
                            }
                            // transparent/foliage runs skip on one mask
                            // test — but only before the first solid block
                            // (after it they flip done_skipping).
                            if ignored == 0 && (s.caves_skip >> (pi & 63)) & 1 != 0 {
                                continue;
                            }
                            s.entries.get(pi).unwrap_or(&oob)
                        };
                        if e.flags & (F_TRANSPARENT | F_FOLIAGE) == 0 {
                            if done_skipping {
                                let bk = || {
                                    biome_at(s, chunk_biomes, modern, format, cx, cy, cz, y_min)
                                };
                                pixel_buffer[pixel_index] = entry_rgb(memo, cm, e, bk);
                                terrain_heights[pixel_index] =
                                    (s.section_height + cy) as i16;
                                continue 'zloop;
                            }
                            ignored += 1;
                        } else if ignored > 0 {
                            done_skipping = true;
                        }
                        if uniform {
                            // every row holds the same entry — repeated rows
                            // can only repeat the first row's effect.
                            break;
                        }
                    }
                }
            }
        }
    });
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
