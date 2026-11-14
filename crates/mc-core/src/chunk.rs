//! Chunk-data layout: the one owner of *where* chunk NBT keeps things —
//! data-version boundaries, the sections/entity/structure locators,
//! palette-entry naming and the packed block-state bit layouts
//! (upstream `version.Helper` + the `ChunkFilter` dispatch internals).

use crate::mca::Chunk;
use crate::nbt::{Compound, Tag};

// ---- data-version boundaries (upstream MCVersionImplementation) ----

/// default chunk `yPos` when the tag is absent (pre-1.18 layout).
pub const Y_POS_DEFAULT: i32 = -4;

pub const DV_15W32A: i32 = 100; // first supported format: Level.Sections byte blocks
pub const DV_17W47A: i32 = 1451; // flattened palette sections (Level.Sections: Palette/BlockStates)
pub const DV_18W06A: i32 = 1466; // Level.Status, int biomes, Heightmaps compound
pub const DV_18W19A: i32 = 1484; // heightmap config change
pub const DV_1_13_PRE3: i32 = 1503; // heightmap config change
pub const DV_18W43A: i32 = 1901; // color/heightmap config change
pub const DV_18W45A: i32 = 1908; // heightmap config change
pub const DV_19W02A: i32 = 1921; // isLightOn byte replaces LightPopulated
pub const DV_19W34A: i32 = 2200; // color/heightmap config change
pub const DV_19W36A: i32 = 2203; // 3d int biomes (Level.Biomes int[1024])
pub const DV_20W06A: i32 = 2504; // color/heightmap config change
pub const DV_20W17A: i32 = 2529; // non-spanning packed block-state layout
pub const DV_20W45A: i32 = 2681; // entities moved to entities/*.mca
pub const DV_21W06A: i32 = 2694; // sectionRange-based heightmaps
pub const DV_21W15A: i32 = 2709; // chunk format change
pub const DV_21W37A: i32 = 2834; // per-section biomes/block_states compounds (still under Level)
pub const DV_21W43A: i32 = 2844; // Level flattened into root
pub const DV_22W11A: i32 = 3080; // color/heightmap config change
pub const DV_23W12A: i32 = 3207; // color/heightmap config change
pub const DV_NAMESPACED_STATUS: i32 = 3458; // Status written with the "minecraft:" prefix
pub const DV_24W18A: i32 = 3940; // color/heightmap config change
pub const DV_FORCE_BLEND_TARGET: i32 = 4189; // ForceBlend resets DataVersion to this inside the gap
pub const DV_1_21_5: i32 = 4317; // blending_data dropped (temporarily)
pub const DV_25W09A: i32 = 4324; // back to blending_data
pub const DV_25W32A: i32 = 4536; // color/heightmap config change
pub const DV_26_1_S1: i32 = 4764; // color/heightmap config change
pub const DV_26_2_S1: i32 = 4883; // color/heightmap config change
pub const DV_26_3_S1: i32 = 4998; // color/heightmap config change
pub const DV_26_3_S4: i32 = 5003; // heightmap config change
pub const DV_26_3_S7: i32 = 5009; // string palettes / "" and "id" name keys
/// below this DataVersion is also written into Level (Helper.setDataVersion)
pub const DV_LEVEL_DATAVERSION: i32 = 111;

/// Largest-key dispatch (upstream VersionHandler's floorEntry): the
/// entry of a sorted-ascending `&[(K, V)]` table with the greatest
/// key <= `dv`.
pub fn dv_floor_key<'a, K: Copy + PartialOrd, V>(table: &'a [(K, V)], dv: K) -> Option<(K, &'a V)> {
    table
        .iter()
        .rev()
        .find(|(k, _)| *k <= dv)
        .map(|(k, v)| (*k, v))
}

// ---- sections / data container -------------------------------------

/// The compound holding the chunk payload: the `Level` wrapper below
/// dv 2844 (21w43a), the root itself from then on.
pub fn data_container(root: &Compound, dv: i32) -> Option<&Compound> {
    if dv >= DV_21W43A {
        Some(root)
    } else {
        crate::nbt::level_from_root(root)
    }
}

pub fn data_container_mut(root: &mut Compound, dv: i32) -> Option<&mut Compound> {
    if dv >= DV_21W43A {
        Some(root)
    } else {
        root.get_mut("Level")?.as_compound_mut()
    }
}

/// Key of the section list inside the data container.
pub fn sections_key(dv: i32) -> &'static str {
    if dv >= DV_21W43A {
        "sections"
    } else {
        "Sections"
    }
}


/// The section list, located by presence: root `sections` (>= 21w43a),
/// then `Level.Sections` (< 21w43a — including the 2834-2843 transition
/// where section internals are already modern), then `Level.sections`
/// (transitional lowercase). Returns the list items and the compound
/// they were found in (Level or root).
pub fn sections<'a>(root: &'a Compound) -> Option<(&'a Vec<Tag>, &'a Compound)> {
    if let Some((_, l)) = root.get("sections").and_then(Tag::as_list) {
        return Some((l, root));
    }
    let level = crate::nbt::level_from_root(root)?;
    let l = ["Sections", "sections"]
        .iter()
        .find_map(|k| level.get(*k).and_then(Tag::as_list))?;
    Some((l.1, level))
}

/// The section list by data-version dispatch — the write-side variant
/// for code that must address the layout a chunk claims to be.
pub fn sections_dv<'a>(root: &'a Compound, dv: i32) -> Option<&'a Vec<Tag>> {
    data_container(root, dv)?
        .get(sections_key(dv))
        .and_then(Tag::as_list)
        .map(|(_, l)| l)
}

pub fn sections_dv_mut<'a>(root: &'a mut Compound, dv: i32) -> Option<&'a mut Vec<Tag>> {
    match data_container_mut(root, dv)?.get_mut(sections_key(dv))? {
        Tag::List(_, l) => Some(l),
        _ => None,
    }
}

/// The block-state palette of one section, any layout:
/// `Palette` (legacy) -> `block_states.palette` (modern) -> `palette`
/// (21w37a transitional flat layout).
pub fn block_palette(section: &Compound) -> Option<&Vec<Tag>> {
    section
        .get("Palette")
        .and_then(Tag::as_list)
        .map(|(_, l)| l)
        .or_else(|| {
            section
                .get("block_states")
                .and_then(Tag::as_compound)
                .and_then(|b| b.get("palette"))
                .and_then(Tag::as_list)
                .map(|(_, l)| l)
        })
        .or_else(|| {
            section
                .get("palette")
                .and_then(Tag::as_list)
                .map(|(_, l)| l)
        })
}

/// The biome palette of one section (`biomes.palette`); only 21w37a+
/// sections carry per-section biomes.
pub fn biome_palette(section: &Compound) -> Option<&Vec<Tag>> {
    section
        .get("biomes")
        .and_then(Tag::as_compound)
        .and_then(|b| b.get("palette"))
        .and_then(Tag::as_list)
        .map(|(_, l)| l)
}

// ---- entity / tile-entity / structure lists -------------------------

/// Entity-list keys in lookup order (modern lowercase first).
pub const ENTITY_LIST_KEYS: &[&str] = &["entities", "Entities"];

/// Tile-entity-list keys in lookup order (modern first).
pub const TILE_ENTITY_LIST_KEYS: &[&str] = &["block_entities", "tile_entities", "TileEntities"];

/// Length of the first list found among `keys` — each key is tried at
/// root, then under `Level` (upstream picks exactly one source; summing
/// would double-count).
pub fn list_size(data: &Compound, keys: &[&str]) -> i64 {
    let level = crate::nbt::level_from_root(data);
    for k in keys {
        for c in [Some(data), level] {
            if let Some((_, l)) = c.and_then(|cc| cc.get(*k)).and_then(Tag::as_list) {
                return l.len() as i64;
            }
        }
    }
    0
}

/// Upstream `ChunkFilter.parseStructureStarts`, both layouts merged:
/// modern root `structures.starts` contributes every compound entry's
/// *key*; legacy `Level.Structures.Starts` contributes the `id` value
/// of every compound with size > 1. (The `region` wrapper fallback is
/// for poi chunks.)
pub fn structure_starts(data: &Compound) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(starts) = data
        .get("structures")
        .and_then(Tag::as_compound)
        .and_then(|st| st.get("starts"))
        .and_then(Tag::as_compound)
    {
        for (name, tag) in starts {
            if tag.as_compound().is_some() && !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    let level = data
        .get("Level")
        .or_else(|| data.get("region"))
        .and_then(Tag::as_compound)
        .or(Some(data));
    if let Some(starts) = level
        .and_then(|l| l.get("Structures"))
        .and_then(Tag::as_compound)
        .and_then(|st| st.get("Starts"))
        .and_then(Tag::as_compound)
    {
        for tag in starts.values() {
            let Some(c) = tag.as_compound() else { continue };
            if c.len() <= 1 {
                continue;
            }
            if let Some(id) = c.get("id").and_then(Tag::as_string) {
                let id = id.trim_start_matches("minecraft:");
                if !names.iter().any(|n| n == id) {
                    names.push(id.to_string());
                }
            }
        }
    }
    names
}

/// Structure starts with bounding boxes: `structures.starts.<id>.BB`
/// (modern) and `Level.Structures.Starts.<id>.BB` (legacy) — an IntArray of
/// [minX, minY, minZ, maxX, maxY, maxZ] in absolute block coords. Returns
/// (structure name, BB) pairs; a start lives in exactly one chunk, so
/// collecting per-region yields each box once.
pub fn structure_boxes(data: &Compound) -> Vec<(String, [i32; 6])> {
    fn bb(c: &Compound) -> Option<[i32; 6]> {
        let Tag::IntArray(a) = c.get("BB")? else { return None };
        if a.len() == 6 {
            Some([a[0], a[1], a[2], a[3], a[4], a[5]])
        } else {
            None
        }
    }
    let mut out: Vec<(String, [i32; 6])> = Vec::new();
    if let Some(starts) = data
        .get("structures")
        .and_then(Tag::as_compound)
        .and_then(|st| st.get("starts"))
        .and_then(Tag::as_compound)
    {
        for (name, tag) in starts {
            let Some(b) = tag.as_compound().and_then(bb) else { continue };
            let name = name.strip_prefix("minecraft:").unwrap_or(name);
            out.push((name.to_string(), b));
        }
    }
    let level = data
        .get("Level")
        .or_else(|| data.get("region"))
        .and_then(Tag::as_compound)
        .or(Some(data));
    if let Some(starts) = level
        .and_then(|l| l.get("Structures"))
        .and_then(Tag::as_compound)
        .and_then(|st| st.get("Starts"))
        .and_then(Tag::as_compound)
    {
        for c in starts.values().filter_map(Tag::as_compound) {
            if c.len() <= 1 {
                continue;
            }
            let (Some(id), Some(b)) = (c.get("id").and_then(Tag::as_string), bb(c)) else {
                continue;
            };
            let id = id.trim_start_matches("minecraft:");
            if !out.iter().any(|(n, _)| n == id) {
                out.push((id.to_string(), b));
            }
        }
    }
    out
}

// ---- palette entry names --------------------------------------------

/// Palette-entry block name: `""` then `"id"` for the 26.3 layout
/// (upstream Helper.getBlockID), `"Name"` for the legacy layout; a bare
/// string tag is the name itself.
pub fn palette_entry_name(entry: &Tag) -> &str {
    match entry {
        Tag::Compound(c) => ["", "id", "Name"]
            .iter()
            .find_map(|k| c.get(*k).and_then(Tag::as_string))
            .unwrap_or(""),
        Tag::String(s) => s.as_str(),
        _ => "",
    }
}

/// Air names (color_properties.json "air" set): the upstream isEmpty
/// check used by AverageHeight/Blocks filters.
pub fn is_air_name(name: &str) -> bool {
    crate::colors::color_properties().air.contains(name)
}

// ---- block index <-> (x, y, z) within a section ----------------------

/// Block index within a section: `y*256 + z*16 + x`.
pub const fn location_to_index(x: usize, y: usize, z: usize) -> usize {
    y * 256 + z * 16 + x
}

/// Inverse of [`location_to_index`] -> (x, y, z).
pub const fn index_to_location(i: usize) -> (i32, i32, i32) {
    ((i & 0xF) as i32, ((i >> 8) & 0xF) as i32, ((i >> 4) & 0xF) as i32)
}

// ---- packed palette-index data ---------------------------------------

/// Borrowed packed backing: raw BE long bytes (raw_arrays parse) or an
/// already-decoded i64 slice.
#[derive(Clone, Copy)]
pub enum PackedSrc<'a> {
    Raw(&'a [u8]),
    Longs(&'a [i64]),
}

impl<'a> PackedSrc<'a> {
    /// BE long at `i` (0 out of range — the decoded-Vec
    /// `get(i).unwrap_or(0)` semantic).
    #[inline]
    pub fn long(&self, i: usize) -> u64 {
        match self {
            PackedSrc::Raw(d) => match d.get(i * 8..i * 8 + 8) {
                Some(b) => u64::from_be_bytes(b.try_into().unwrap()),
                None => 0,
            },
            PackedSrc::Longs(v) => v.get(i).copied().unwrap_or(0) as u64,
        }
    }

    #[inline]
    pub fn n_longs(&self) -> usize {
        match self {
            PackedSrc::Raw(d) => d.len() >> 3,
            PackedSrc::Longs(v) => v.len(),
        }
    }
}

impl Default for PackedSrc<'_> {
    fn default() -> Self {
        PackedSrc::Raw(&[])
    }
}

#[inline]
pub(crate) fn mask(bits: usize) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// Spanning-layout read (< 20w17a): `bits` at absolute bit position
/// `bit` may cross a long boundary. Out-of-range longs read as 0.
#[inline]
pub(crate) fn get_spanning(src: &PackedSrc<'_>, bit: usize, bits: usize) -> u64 {
    let li = bit >> 6;
    let off = bit & 63;
    if off + bits > 64 {
        let prev = src.long(li) >> off;
        let next = src.long(li + 1) & mask(off + bits - 64);
        (next << (64 - off)) | prev
    } else {
        (src.long(li) >> off) & mask(bits)
    }
}

/// Read view over one packed palette-index array covering both layouts:
/// spanning (dv < 20w17a) and non-spanning (64/bits indexes per long,
/// none straddling a boundary).
#[derive(Clone, Copy, Default)]
pub struct PackedData<'a> {
    pub src: PackedSrc<'a>,
    pub bits: usize,
    pub spanning: bool,
}

impl<'a> PackedData<'a> {
    /// Block-state packing rules: bits derived from the array length,
    /// spanning below dv 2529.
    pub fn block_states(src: PackedSrc<'a>, dv: i32) -> Self {
        PackedData {
            src,
            bits: src.n_longs() >> 6,
            spanning: dv < DV_20W17A,
        }
    }

    /// Explicit-bit-width packing (biome data derives `bits` as the
    /// number of longs — upstream biomeBits).
    pub fn with_bits(src: PackedSrc<'a>, bits: usize, spanning: bool) -> Self {
        PackedData { src, bits, spanning }
    }

    /// indexes per long for the non-spanning layout.
    #[inline]
    pub fn ipl(&self) -> usize {
        if self.bits == 0 {
            1
        } else {
            (64 / self.bits).max(1)
        }
    }

    /// palette index at `index` — 0 for empty data and out-of-range
    /// longs.
    #[inline]
    pub fn get(&self, index: usize) -> u64 {
        let bits = self.bits;
        if bits == 0 {
            return 0;
        }
        if self.spanning {
            get_spanning(&self.src, index * bits, bits)
        } else {
            let ipl = self.ipl();
            let li = index / ipl;
            (self.src.long(li) >> ((index % ipl) * bits)) & mask(bits)
        }
    }
}


/// Writable packed access over an i64 backing (the changer re-encodes
/// block states as owned long arrays). Both layouts, bits derived from
/// the array length.
pub fn packed_set(index: usize, v: u64, arr: &mut [i64], dv: i32) {
    let bits = arr.len() >> 6;
    if bits == 0 {
        return;
    }
    if dv < DV_20W17A {
        // spanning layout
        let bit = index * bits;
        let li = bit >> 6;
        let off = bit & 63;
        if off + bits > 64 {
            let val = v & mask(bits);
            arr[li] =
                ((arr[li] as u64 & !(mask(64 - off) << off)) | (val << off)) as i64;
            let hbits = off + bits - 64;
            arr[li + 1] =
                ((arr[li + 1] as u64 & !mask(hbits)) | (val >> (64 - off))) as i64;
        } else {
            let m = mask(bits) << off;
            arr[li] = ((arr[li] as u64 & !m) | ((v << off) & m)) as i64;
        }
    } else {
        let ipl = 64 / bits;
        let li = index / ipl;
        let off = (index % ipl) * bits;
        let m = mask(bits) << off;
        arr[li] = ((arr[li] as u64 & !m) | ((v << off) & m)) as i64;
    }
}

// ---- the region/poi/entities chunk triple ----------------------------

/// The region/poi/entities triple of one chunk column (upstream ChunkData
/// as seen by the changer).
#[derive(Default)]
pub struct ChunkRefs<'a> {
    pub region: Option<&'a mut Chunk>,
    pub poi: Option<&'a mut Chunk>,
    pub entities: Option<&'a mut Chunk>,
}
