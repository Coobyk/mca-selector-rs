//! Port of version.*: DataVersion-dispatched ChunkRenderers + ColorMappings.
//! Dispatch via floorEntry, like upstream's VersionHandler.

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
                    BiomeKey::Idx(_) => "minecraft:plains",
                };
                c.get_color(name, biome_name, props).color | 0xFF00_0000
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

struct IndexedSection<'a> {
    palette: Vec<&'a Tag>,
    data: Option<&'a [u8]>,
    bits: usize,
    indexes_per_long: usize,
    biome_palette: Vec<&'a Tag>,
    biome_data: Option<&'a [u8]>,
    biome_bits: usize,
    biome_indexes_per_long: usize,
    start_height: i32,
    section_height: i32,
}

fn get_block<'a>(
    s: &'a IndexedSection<'a>,
    x: usize,
    y: usize,
    z: usize,
) -> Option<&'a Tag> {
    if s.palette.is_empty() {
        return None;
    }
    if s.bits == 0 || s.data.is_none() {
        return s.palette.first().copied();
    }
    let index = y * 256 + z * 16 + x;
    let pal_idx = packed_get(s.data, index, s.bits, s.indexes_per_long) as usize;
    s.palette.get(pal_idx).copied()
}

fn get_biome_packed<'a>(s: &'a IndexedSection<'a>, x: usize, y: usize, z: usize, format: ChunkFormat) -> &'a str {
    if s.biome_palette.is_empty() {
        return "";
    }
    if s.biome_bits == 0 || s.biome_data.is_none() {
        return palette_name(s.biome_palette.first().copied(), format);
    }
    let index = (y >> 2 & 0xF) * 16 + (z >> 2 & 0xF) * 4 + (x >> 2 & 0xF);
    let pal_idx = packed_get(s.biome_data, index, s.biome_bits, s.biome_indexes_per_long) as usize;
    palette_name(s.biome_palette.get(pal_idx).copied(), format)
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

fn is_modern(format: ChunkFormat) -> bool {
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
) -> Vec<Option<IndexedSection<'a>>> {
    let modern = is_modern(format);
    let (lo, hi) = if modern {
        (y_min, 1 + (height >> 4))
    } else {
        (0, 1 + (height.clamp(0, 255) >> 4))
    };
    let s_max = (hi - lo).max(0) as usize;
    let mut indexed: Vec<Option<IndexedSection>> = std::iter::repeat_with(|| None).take(s_max).collect();
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
        let (palette, data) = if modern {
            let bs = s.get("block_states").and_then(Tag::as_compound);
            (
                bs.and_then(|b| b.get("palette"))
                    .and_then(Tag::as_list)
                    .map(|(_, l)| l.iter().collect())
                    .unwrap_or_default(),
                bs.and_then(|b| b.get("data")).and_then(|t| t.as_bytes()),
            )
        } else {
            (
                s.get("Palette")
                    .and_then(Tag::as_list)
                    .map(|(_, l)| l.iter().collect())
                    .unwrap_or_default(),
                s.get("BlockStates").and_then(|t| t.as_bytes()),
            )
        };
        let bits = data.map(|d| d.len() >> 9).unwrap_or(0);
        let ipl = if bits == 0 { 1 } else { (64.0 / bits as f64) as usize };
        let (biome_palette, biome_data) = if modern {
            let bc = s.get("biomes").and_then(Tag::as_compound);
            (
                bc.and_then(|b| b.get("palette"))
                    .and_then(Tag::as_list)
                    .map(|(_, l)| l.iter().collect())
                    .unwrap_or_default(),
                bc.and_then(|b| b.get("data")).and_then(|t| t.as_bytes()),
            )
        } else {
            (Vec::new(), None)
        };
        let bb = biome_data.map(|d| d.len() >> 3).unwrap_or(0);
        let bipl = if bb == 0 { 1 } else { (64.0 / bb as f64) as usize };
        let abs_h = if modern { height - y_min * 16 } else { height.clamp(0, 255) };
        indexed[idx] = Some(IndexedSection {
            palette,
            data,
            bits,
            indexes_per_long: ipl,
            biome_palette,
            biome_data,
            biome_bits: bb,
            biome_indexes_per_long: bipl,
            start_height: if (abs_h >> 4) == idx as i32 {
                abs_h & 0xF
            } else {
                15
            },
            section_height: (idx as i32 + lo) * 16,
        });
    }
    indexed
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

    for cx in (0..16usize).step_by(scale as usize) {
        'zloop: for cz in (0..16usize).step_by(scale as usize) {
            let pixel_index =
                (z + (cz >> scale_bits)) * (512 >> scale_bits) + (x + (cx >> scale_bits));
            let mut water_depth = false;
            for s in indexed[..=start].iter().rev().flatten() {
                for cy in (0..=s.start_height).rev() {
                    let entry = get_block(s, cx, cy as usize, cz);
                    let name = palette_name(entry, format);
                    if cm.is_transparent(name) {
                        continue;
                    }
                    let props = palette_props(entry);
                    let bk = if modern {
                        BiomeKey::Name(get_biome_packed(s, cx, cy as usize, cz, format))
                    } else {
                        BiomeKey::Idx(biome_int(
                            chunk_biomes,
                            cx,
                            s.section_height + cy,
                            cz,
                            format,
                            y_min,
                        ))
                    };
                    if out.water {
                        let wh = out.water_heights.as_deref_mut().unwrap();
                        if !water_depth {
                            out.pixel_buffer[pixel_index] = cm.get_rgb(name, props, bk);
                            wh[pixel_index] = (s.section_height + cy) as i16;
                        }
                        if cm.is_water(name) {
                            water_depth = true;
                            continue;
                        } else if cm.is_waterlogged(props) {
                            out.pixel_buffer[pixel_index] =
                                cm.get_rgb(WATER_DUMMY_NAME, None, bk);
                            if let Some(wp) = out.water_pixels.as_deref_mut() {
                                wp[pixel_index] = cm.get_rgb(name, props, bk);
                            }
                            wh[pixel_index] = (s.section_height + cy) as i16;
                            out.terrain_heights[pixel_index] =
                                (s.section_height + cy - 1) as i16;
                            continue 'zloop;
                        } else if let Some(wp) = out.water_pixels.as_deref_mut() {
                            wp[pixel_index] = cm.get_rgb(name, props, bk);
                        }
                    } else {
                        out.pixel_buffer[pixel_index] = cm.get_rgb(name, props, bk);
                    }
                    out.terrain_heights[pixel_index] = (s.section_height + cy) as i16;
                    continue 'zloop;
                }
            }
        }
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
    let (palette, data) = if modern {
        let bs = section.get("block_states").and_then(Tag::as_compound);
        (
            bs.and_then(|b| b.get("palette"))
                .and_then(Tag::as_list)
                .map(|(_, l)| l.iter().collect())
                .unwrap_or_default(),
            bs.and_then(|b| b.get("data")).and_then(|t| t.as_bytes()),
        )
    } else {
        (
            section
                .get("Palette")
                .and_then(Tag::as_list)
                .map(|(_, l)| l.iter().collect())
                .unwrap_or_default(),
            section.get("BlockStates").and_then(|t| t.as_bytes()),
        )
    };
    let bits = data.map(|d| d.len() >> 9).unwrap_or(0);
    let ipl = if bits == 0 { 1 } else { (64.0 / bits as f64) as usize };
    let (biome_palette, biome_data) = if modern {
        let bc = section.get("biomes").and_then(Tag::as_compound);
        (
            bc.and_then(|b| b.get("palette"))
                .and_then(Tag::as_list)
                .map(|(_, l)| l.iter().collect())
                .unwrap_or_default(),
            bc.and_then(|b| b.get("data")).and_then(|t| t.as_bytes()),
        )
    } else {
        (Vec::new(), None)
    };
    let bb = biome_data.map(|d| d.len() >> 3).unwrap_or(0);
    let s = IndexedSection {
        palette,
        data,
        bits,
        indexes_per_long: ipl,
        biome_palette,
        biome_data,
        biome_bits: bb,
        biome_indexes_per_long: if bb == 0 { 1 } else { (64.0 / bb as f64) as usize },
        start_height: 15,
        section_height: target_y * 16,
    };
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
    for cx in (0..16usize).step_by(scale as usize) {
        for cz in (0..16usize).step_by(scale as usize) {
            let entry = get_block(&s, cx, cy, cz);
            let name = palette_name(entry, format);
            if cm.is_transparent(name) {
                continue;
            }
            let props = palette_props(entry);
            let pixel_index =
                (z + (cz >> scale_bits)) * (512 >> scale_bits) + (x + (cx >> scale_bits));
            let bk = if modern {
                BiomeKey::Name(get_biome_packed(&s, cx, cy, cz, format))
            } else {
                BiomeKey::Idx(biome_int(
                    chunk_biomes,
                    cx,
                    s.section_height + cy as i32,
                    cz,
                    format,
                    y_min,
                ))
            };
            pixel_buffer[pixel_index] = cm.get_rgb(name, props, bk);
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
    for cx in (0..16usize).step_by(scale as usize) {
        'zloop: for cz in (0..16usize).step_by(scale as usize) {
            let pixel_index =
                (z + (cz >> scale_bits)) * (512 >> scale_bits) + (x + (cx >> scale_bits));
            let mut ignored = 0;
            let mut done_skipping = false;
            for s in indexed[..=start].iter().rev().flatten() {
                for cy in (0..=s.start_height).rev() {
                    let entry = get_block(s, cx, cy as usize, cz);
                    let name = palette_name(entry, format);
                    if !cm.is_transparent(name) && !cm.is_foliage(name) {
                        if done_skipping {
                            let props = palette_props(entry);
                            let bk = if modern {
                                BiomeKey::Name(get_biome_packed(s, cx, cy as usize, cz, format))
                            } else {
                                BiomeKey::Idx(biome_int(
                                    chunk_biomes,
                                    cx,
                                    s.section_height + cy,
                                    cz,
                                    format,
                                    y_min,
                                ))
                            };
                            pixel_buffer[pixel_index] = cm.get_rgb(name, props, bk);
                            terrain_heights[pixel_index] = (s.section_height + cy) as i16;
                            continue 'zloop;
                        }
                        ignored += 1;
                    } else if ignored > 0 {
                        done_skipping = true;
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
