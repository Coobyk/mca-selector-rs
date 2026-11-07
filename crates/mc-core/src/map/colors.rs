//! Color resolution replicating Xaero's World Map: vanilla map colors per
//! block, biome-dependent tints, and Xaero's exact brightness math.
//!
//! The tables in [`xaero_colors`] are generated from the (unobfuscated)
//! Minecraft jar; see `docs/generate_xaero_tables.py`.

pub mod xaero_colors;

/// Which biome color tints a block, mirroring vanilla
/// `BlockColors.createDefault()` as used by Xaero's tint provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TintKind {
    None,
    Grass,
    Foliage,
    DryFoliage,
    Water,
    Fixed(u32),
}

/// Block name (without `minecraft:`) -> vanilla map color, binary search.
pub fn block_map_color(name: &str) -> Option<u32> {
    xaero_colors::BLOCK_MAP_COLORS
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .map(|i| xaero_colors::BLOCK_MAP_COLORS[i].1)
        .filter(|c| *c != 0)
}

fn tint_kind(name: &str) -> TintKind {
    if xaero_colors::TINT_GRASS.binary_search(&name).is_ok() {
        return TintKind::Grass;
    }
    if xaero_colors::TINT_FOLIAGE.binary_search(&name).is_ok() {
        return TintKind::Foliage;
    }
    if xaero_colors::TINT_DRY_FOLIAGE.binary_search(&name).is_ok() {
        return TintKind::DryFoliage;
    }
    if xaero_colors::TINT_WATER.binary_search(&name).is_ok() {
        return TintKind::Water;
    }
    if let Ok(i) = xaero_colors::TINT_FIXED.binary_search_by(|(n, _)| (*n).cmp(name)) {
        return TintKind::Fixed(xaero_colors::TINT_FIXED[i].1 & 0xFFFFFF);
    }
    TintKind::None
}

/// The kind of tint a block receives (used to blend biome colors across
/// columns before applying them).
pub fn tint_kind_of(name: &str) -> TintKind {
    tint_kind(name)
}

/// Per-biome tint colors (grass, foliage, water), defaulting to plains for
/// unknown (modded) biomes.
pub fn biome_tint(biome: &str) -> [u32; 3] {
    xaero_colors::BIOME_TINTS
        .binary_search_by(|(n, ..)| (*n).cmp(biome))
        .ok()
        .map(|i| {
            let (_, g, f, w) = xaero_colors::BIOME_TINTS[i];
            [g, f, w]
        })
        .unwrap_or([0x91BD59, 0x77AB2F, 0x3F76E4])
}

/// Blocks that emit light (emissive >= 1), matched by name. Xaero brightens
/// these until their color channels sum to at least 407.
pub fn is_glowing(name: &str) -> bool {
    const GLOWING: &[&str] = &[
        "torch", "wall_torch", "redstone_torch", "lantern", "soul_lantern",
        "glowstone", "sea_lantern", "shroomlight", "magma_block", "lava",
        "campfire", "soul_campfire", "jack_o_lantern", "redstone_lamp",
        "redstone_wire", "end_rod", "crying_obsidian", "beacon", "conduit",
        "amethyst_cluster", "large_amethyst_bud", "medium_amethyst_bud",
        "small_amethyst_bud", "glow_lichen", "frogspawn", "light",
        "firefly_bush", "copper_bulb", "exposed_copper_bulb",
        "weathered_copper_bulb", "oxidized_copper_bulb", "respawn_anchor",
        "enchanting_table", "end_gateway", "end_portal", "nether_portal",
        "furnace", "blast_furnace", "smoker", "brain_coral", "fire",
        "soul_fire", "soul_torch", "soul_wall_torch", "redstone_wall_torch",
        "candle", "sea_pickle", "ochre_froglight", "verdant_froglight",
        "pearlescent_froglight",
    ];
    GLOWING.binary_search(&name).is_ok()
}

/// Blocks that are invisible on Xaero's map by default (plants and other
/// decorations; Xaero's "flowers" option is off by default). The column
/// continues below them.
pub fn is_invisible(name: &str) -> bool {
    const INVISIBLE: &[&str] = &[
        "allium",
        "azure_bluet",
        "beetroots",
        "blue_orchid",
        "brown_mushroom",
        "bush",
        "cactus_flower",
        "carrots",
        "cave_vines",
        "cave_vines_plant",
        "closed_eyeblossom",
        "cornflower",
        "crimson_fungus",
        "crimson_roots",
        "dandelion",
        "fern",
        "firefly_bush",
        "kelp",
        "kelp_plant",
        "large_fern",
        "lilac",
        "lily_of_the_valley",
        "nether_sprouts",
        "nether_wart",
        "open_eyeblossom",
        "orange_tulip",
        "oxeye_daisy",
        "peony",
        "pink_petals",
        "pink_tulip",
        "pitcher_plant",
        "poppy",
        "potatoes",
        "red_mushroom",
        "red_tulip",
        "rose_bush",
        "seagrass",
        "short_dry_grass",
        "short_grass",
        "sugar_cane",
        "sunflower",
        "sweet_berry_bush",
        "tall_dry_grass",
        "tall_grass",
        "tall_seagrass",
        "torchflower",
        "twisting_vines",
        "twisting_vines_plant",
        "warped_fungus",
        "weeping_vines",
        "weeping_vines_plant",
        "wheat",
        "white_tulip",
        "wildflowers",
        "wither_rose",
    ];
    INVISIBLE.binary_search(&name).is_ok()
}
