//! Block colors for map rendering.
//!
//! The palette follows Minecraft's map colors (the 16 base hues plus the
//! special terrain colors), and the block table maps block names to palette
//! entries. Grass and foliage blocks use their fixed plains-biome tints;
//! water uses the overworld water tint.

/// An RGB color with an alpha channel.
pub type Rgba = [u8; 4];

/// Base map colors, mirroring vanilla `MaterialColor`. Values are the
/// untinted base colors; shading multiplies them.
pub mod map_colors {
    use super::Rgba;

    pub const NONE: Rgba = [0, 0, 0, 255];
    pub const GRASS: Rgba = [0x7F, 0xB2, 0x38, 255];
    pub const SAND: Rgba = [0xF7, 0xE9, 0xA3, 255];
    pub const WOOL: Rgba = [0xFF, 0x00, 0x00, 255];
    pub const FIRE: Rgba = [0xFF, 0x00, 0x00, 255];
    pub const ICE: Rgba = [0xA0, 0xA0, 0xFF, 255];
    pub const METAL: Rgba = [0xA7, 0xA7, 0xA7, 255];
    pub const PLANT: Rgba = [0x00, 0x7C, 0x00, 255];
    pub const SNOW: Rgba = [0xFF, 0xFF, 0xFF, 255];
    pub const CLAY: Rgba = [0xA4, 0xA8, 0xB8, 255];
    pub const DIRT: Rgba = [0x96, 0x6D, 0x4D, 255];
    pub const STONE: Rgba = [0x7D, 0x7D, 0x7D, 255];
    pub const WATER: Rgba = [0x40, 0x50, 0xAE, 255];
    pub const WOOD: Rgba = [0x9C, 0x7F, 0x4E, 255];
    pub const QUARTZ: Rgba = [0xFF, 0xFC, 0xF5, 255];
    pub const ORANGE: Rgba = [0xD8, 0x7F, 0x33, 255];
    pub const MAGENTA: Rgba = [0xB2, 0x4C, 0xD8, 255];
    pub const LIGHT_BLUE: Rgba = [0x66, 0x99, 0xD8, 255];
    pub const YELLOW: Rgba = [0xE7, 0xC2, 0x94, 255];
    pub const LIME: Rgba = [0x41, 0xCD, 0x34, 255];
    pub const PINK: Rgba = [0xD5, 0xA7, 0xB4, 255];
    pub const GRAY: Rgba = [0x4C, 0x4C, 0x4C, 255];
    pub const LIGHT_GRAY: Rgba = [0x9D, 0xB3, 0xB3, 255];
    pub const CYAN: Rgba = [0x4A, 0x77, 0x8C, 255];
    pub const PURPLE: Rgba = [0x8C, 0x4F, 0x99, 255];
    pub const BLUE: Rgba = [0x33, 0x50, 0xA8, 255];
    pub const BROWN: Rgba = [0x79, 0x4E, 0x2F, 255];
    pub const GREEN: Rgba = [0x50, 0x7A, 0x32, 255];
    pub const RED: Rgba = [0xB2, 0x4E, 0x4E, 255];
    pub const BLACK: Rgba = [0x25, 0x25, 0x25, 255];
    pub const GOLD: Rgba = [0xE4, 0xC4, 0x8C, 255];
    pub const DIAMOND: Rgba = [0x74, 0xE4, 0xE4, 255];
    pub const LAPIS: Rgba = [0x21, 0x48, 0xC3, 255];
    pub const EMERALD: Rgba = [0x2C, 0xA4, 0x55, 255];
    pub const PODZOL: Rgba = [0x79, 0x6C, 0x38, 255];
    pub const NETHER: Rgba = [0x70, 0x30, 0x1E, 255];
    pub const TERRACOTTA_WHITE: Rgba = [0xD8, 0xAF, 0x93, 255];
    pub const TERRACOTTA_ORANGE: Rgba = [0xA2, 0x54, 0x26, 255];
    pub const TERRACOTTA_MAGENTA: Rgba = [0x9A, 0x51, 0x48, 255];
    pub const TERRACOTTA_LIGHT_BLUE: Rgba = [0x68, 0x74, 0x5E, 255];
    pub const TERRACOTTA_YELLOW: Rgba = [0xB9, 0x8A, 0x50, 255];
    pub const TERRACOTTA_LIME: Rgba = [0x77, 0x81, 0x3C, 255];
    pub const TERRACOTTA_PINK: Rgba = [0xA0, 0x4E, 0x40, 255];
    pub const TERRACOTTA_GRAY: Rgba = [0x39, 0x2A, 0x24, 255];
    pub const TERRACOTTA_LIGHT_GRAY: Rgba = [0x87, 0x66, 0x59, 255];
    pub const TERRACOTTA_CYAN: Rgba = [0x6B, 0x5C, 0x50, 255];
    pub const TERRACOTTA_PURPLE: Rgba = [0x7A, 0x44, 0x4B, 255];
    pub const TERRACOTTA_BLUE: Rgba = [0x4A, 0x3A, 0x38, 255];
    pub const TERRACOTTA_BROWN: Rgba = [0x4A, 0x32, 0x26, 255];
    pub const TERRACOTTA_GREEN: Rgba = [0x4B, 0x42, 0x32, 255];
    pub const TERRACOTTA_RED: Rgba = [0x8E, 0x3C, 0x2E, 255];
    pub const TERRACOTTA_BLACK: Rgba = [0x25, 0x17, 0x10, 255];
    pub const CRIMSON_NYLIUM: Rgba = [0x8E, 0x3C, 0x2E, 255];
    pub const CRIMSON_STEM: Rgba = [0x67, 0x2E, 0x28, 255];
    pub const CRIMSON_HYPHAE: Rgba = [0x81, 0x3F, 0x33, 255];
    pub const WARPED_NYLIUM: Rgba = [0x2B, 0x65, 0x5C, 255];
    pub const WARPED_STEM: Rgba = [0x3A, 0x8E, 0x8C, 255];
    pub const WARPED_HYPHAE: Rgba = [0x55, 0xA1, 0x9E, 255];
    pub const WARPED_WART_BLOCK: Rgba = [0x14, 0x93, 0x8B, 255];
}

/// Frequently tinted colors.
pub mod tints {
    pub const GRASS: [u8; 3] = [0x91, 0xBD, 0x59];
    pub const FOLIAGE: [u8; 3] = [0x77, 0xAB, 0x2F];
    pub const FOLIAGE_DARK: [u8; 3] = [0x48, 0xB5, 0x18];
    pub const BIRCH_FOLIAGE: [u8; 3] = [0x80, 0xA7, 0x55];
    pub const SPRUCE_FOLIAGE: [u8; 3] = [0x61, 0x99, 0x61];
    pub const WATER: [u8; 3] = [0x3F, 0x76, 0xE4];
}

/// Lookup with exact matches first, then family rules by suffix/prefix.
/// `name` is without the `minecraft:` prefix; unknown blocks fall back to a
/// deterministic color derived from the name so that they still read as
/// distinct terrain instead of a single gray.
pub fn block_color(name: &str) -> Rgba {
    if let Some(c) = exact(name) {
        return c;
    }
    for (needle, c) in RULES {
        if name.contains(needle) {
            return *c;
        }
    }
    fallback(name)
}

/// Multiply a color by a brightness factor (applied to terrain shading).
pub fn shade(c: Rgba, factor: f32) -> Rgba {
    let f = factor.clamp(0.0, 4.0);
    let mut out = [0u8; 4];
    for i in 0..3 {
        out[i] = (c[i] as f32 * f).round().clamp(0.0, 255.0) as u8;
    }
    out[3] = c[3];
    out
}

const fn rgba(c: [u8; 3]) -> Rgba {
    [c[0], c[1], c[2], 255]
}

use map_colors::*;

/// Blocks whose color is fixed and common enough to be worth listing.
fn exact(name: &str) -> Option<Rgba> {
    let c = match name {
        // Terrain
        "grass_block" | "tall_grass" | "fern" | "large_fern" | "grass" => rgba(tints::GRASS),
        "dirt" | "coarse_dirt" | "rooted_dirt" | "dirt_path" | "farmland" | "mud" => DIRT,
        "podzol" => PODZOL,
        "mycelium" => [0x8F, 0x77, 0x78, 255],
        "clay" => CLAY,
        "sand" | "suspicious_sand" => SAND,
        "red_sand" | "suspicious_red_sand" => ORANGE,
        "gravel" | "suspicious_gravel" => [0x7F, 0x7B, 0x77, 255],
        "stone" => STONE,
        "granite" => [0x9A, 0x6E, 0x59, 255],
        "diorite" => [0xBD, 0xBE, 0xC2, 255],
        "andesite" => [0x88, 0x89, 0x8A, 255],
        "deepslate" | "cobbled_deepslate" | "deepslate_bricks" | "deepslate_tiles"
        | "polished_deepslate" | "chiseled_deepslate" | "cracked_deepslate_bricks"
        | "cracked_deepslate_tiles" | "deepslate_brick_slab" => [0x51, 0x51, 0x52, 255],
        "tuff" | "tuff_bricks" | "chiseled_tuff" | "polished_tuff" => [0x6C, 0x6F, 0x66, 255],
        "calcite" => [0xDF, 0xE3, 0xE2, 255],
        "dripstone_block" => [0x87, 0x60, 0x53, 255],
        "cobblestone" | "mossy_cobblestone" | "infested_cobblestone" => [0x7A, 0x7A, 0x7A, 255],
        "bedrock" => [0x33, 0x33, 0x33, 255],
        "obsidian" => [0x0F, 0x0D, 0x15, 255],
        "crying_obsidian" => [0x25, 0x0F, 0x4A, 255],
        "netherrack" | "nether_bricks" | "cracked_nether_bricks" | "chiseled_nether_bricks"
        | "red_nether_bricks" => NETHER,
        "soul_sand" | "soul_soil" => [0x50, 0x3B, 0x2E, 255],
        "basalt" | "polished_basalt" | "smooth_basalt" => [0x4C, 0x4A, 0x4F, 255],
        "blackstone" | "polished_blackstone" | "polished_blackstone_bricks"
        | "chiseled_polished_blackstone" | "gilded_blackstone" => [0x27, 0x22, 0x28, 255],
        "end_stone" | "end_stone_bricks" => [0xDA, 0xDB, 0xA3, 255],
        "purpur_block" | "purpur_pillar" => [0xA9, 0x7F, 0xA9, 255],
        "prismarine" | "prismarine_bricks" => [0x63, 0x92, 0x8D, 255],
        "dark_prismarine" => [0x33, 0x60, 0x59, 255],
        "moss_block" => [0x5A, 0x72, 0x37, 255],
        "mud_bricks" | "packed_mud" => TERRACOTTA_GRAY,
        "snow" | "snow_block" | "powder_snow" => SNOW,
        "ice" => [0x7D, 0xAD, 0xFF, 255],
        "packed_ice" => [0x8C, 0xB4, 0xF4, 255],
        "blue_ice" => [0x74, 0xA8, 0xFD, 255],
        "frosted_ice" => [0x7D, 0xAD, 0xFF, 255],
        "water" | "flowing_water" | "bubble_column" => rgba(tints::WATER),
        "lava" | "flowing_lava" => [0xD4, 0x5A, 0x12, 255],
        "magma_block" => [0x8E, 0x39, 0x15, 255],
        "glowstone" | "sea_lantern" | "shroomlight" | "ochre_froglight"
        | "verdant_froglight" | "pearlescent_froglight" => [0xFF, 0xE9, 0x9D, 255],
        "glass" | "tinted_glass" => [0xC0, 0xD5, 0xDC, 255],

        // Ores
        "coal_ore" | "deepslate_coal_ore" => [0x27, 0x27, 0x27, 255],
        "iron_ore" | "deepslate_iron_ore" => [0xA0, 0x87, 0x71, 255],
        "copper_ore" | "deepslate_copper_ore" | "raw_copper_block" => [0xC0, 0x6B, 0x50, 255],
        "gold_ore" | "deepslate_gold_ore" | "nether_gold_ore" | "raw_gold_block" => GOLD,
        "redstone_ore" | "deepslate_redstone_ore" => RED,
        "emerald_ore" | "deepslate_emerald_ore" | "emerald_block" => EMERALD,
        "lapis_ore" | "deepslate_lapis_ore" | "lapis_block" => LAPIS,
        "diamond_ore" | "deepslate_diamond_ore" | "diamond_block" => DIAMOND,
        "nether_quartz_ore" | "quartz_block" | "quartz_bricks" | "smooth_quartz"
        | "quartz_pillar" | "chiseled_quartz_block" | "quartz_slab" => QUARTZ,
        "iron_block" | "raw_iron_block" => METAL,
        "coal_block" => [0x10, 0x10, 0x10, 255],
        "ancient_debris" => [0x5B, 0x3A, 0x33, 255],

        // Nylium and fungi
        "crimson_nylium" => CRIMSON_NYLIUM,
        "warped_nylium" => WARPED_NYLIUM,
        "crimson_stem" | "stripped_crimson_stem" | "crimson_planks" | "crimson_door"
        | "crimson_trapdoor" | "crimson_slab" | "crimson_fence" | "crimson_stairs" => {
            CRIMSON_STEM
        }
        "crimson_hyphae" | "stripped_crimson_hyphae" => CRIMSON_HYPHAE,
        "warped_stem" | "stripped_warped_stem" | "warped_planks" | "warped_door"
        | "warped_trapdoor" | "warped_slab" | "warped_fence" | "warped_stairs" => WARPED_STEM,
        "warped_hyphae" | "stripped_warped_hyphae" => WARPED_HYPHAE,
        "warped_wart_block" => WARPED_WART_BLOCK,
        "nether_wart_block" => [0x71, 0x08, 0x0D, 255],

        // Logs and wood (default brown; special woods below)
        "oak_log" | "spruce_log" | "birch_log" | "jungle_log" | "acacia_log"
        | "dark_oak_log" | "mangrove_log" | "cherry_log" | "pale_oak_log" => WOOD,
        "bamboo_block" => [0xB0, 0xA8, 0x4C, 255],

        // Plants / crops / misc
        "dead_bush" => BROWN,
        "pumpkin" | "carved_pumpkin" | "jack_o_lantern" => [0xC0, 0x76, 0x18, 255],
        "melon" => [0x77, 0xA6, 0x2F, 255],
        "hay_block" => YELLOW,
        "dried_kelp_block" => [0x2E, 0x33, 0x2A, 255],
        "sponge" | "wet_sponge" => [0xC3, 0xC3, 0x4B, 255],
        "sculk" | "sculk_catalyst" | "sculk_sensor" | "calibrated_sculk_sensor" => {
            [0x0B, 0x2B, 0x33, 255]
        }
        "amethyst_block" | "budding_amethyst" => [0x84, 0x5F, 0xC1, 255],
        "copper_block" | "exposed_copper" | "weathered_copper" | "oxidized_copper"
        | "cut_copper" | "exposed_cut_copper" | "weathered_cut_copper" | "oxidized_cut_copper" => {
            [0xC0, 0x6B, 0x50, 255]
        }
        "waxed_copper_block" | "waxed_cut_copper" => [0xC0, 0x6B, 0x50, 255],
        _ => return None,
    };
    Some(c)
}

/// Family rules matched by substring, in order of precedence.
static RULES: &[(&str, Rgba)] = &[
    // Leaves before wood, since "dark_oak_leaves" contains "oak".
    ("oak_leaves", rgba(tints::FOLIAGE)),
    ("dark_oak_leaves", [0x2E, 0x63, 0x1E, 255]),
    ("spruce_leaves", rgba(tints::SPRUCE_FOLIAGE)),
    ("birch_leaves", rgba(tints::BIRCH_FOLIAGE)),
    ("jungle_leaves", rgba(tints::FOLIAGE_DARK)),
    ("acacia_leaves", rgba(tints::FOLIAGE)),
    ("mangrove_leaves", [0x40, 0x7F, 0x28, 255]),
    ("cherry_leaves", [0xF2, 0xB3, 0xC9, 255]),
    ("pale_oak_leaves", [0x8E, 0x96, 0x84, 255]),
    ("azalea_leaves", [0x4E, 0x7A, 0x28, 255]),
    ("flowering_azalea_leaves", [0x6E, 0x8A, 0x3C, 255]),
    ("leaves", rgba(tints::FOLIAGE)),
    ("azalea", [0x4E, 0x7A, 0x28, 255]),
    ("vine", rgba(tints::FOLIAGE)),
    ("lily_pad", rgba(tints::FOLIAGE)),
    ("moss_carpet", [0x5A, 0x72, 0x37, 255]),
    ("_sapling", rgba(tints::FOLIAGE)),
    ("bush", rgba(tints::FOLIAGE)),
    // Concrete and concrete powder by dye color.
    ("white_concrete", SNOW),
    ("orange_concrete", ORANGE),
    ("magenta_concrete", MAGENTA),
    ("light_blue_concrete", LIGHT_BLUE),
    ("yellow_concrete", YELLOW),
    ("lime_concrete", LIME),
    ("pink_concrete", PINK),
    ("gray_concrete", GRAY),
    ("light_gray_concrete", LIGHT_GRAY),
    ("cyan_concrete", CYAN),
    ("purple_concrete", PURPLE),
    ("blue_concrete", BLUE),
    ("brown_concrete", BROWN),
    ("green_concrete", GREEN),
    ("red_concrete", RED),
    ("black_concrete", BLACK),
    ("white_concrete_powder", [0xE0, 0xE4, 0xE2, 255]),
    ("orange_concrete_powder", [0xE0, 0x8E, 0x59, 255]),
    ("magenta_concrete_powder", [0xE0, 0x92, 0xE0, 255]),
    ("light_blue_concrete_powder", [0x94, 0xB4, 0xE0, 255]),
    ("yellow_concrete_powder", [0xE5, 0xD9, 0xA3, 255]),
    ("lime_concrete_powder", [0x71, 0xAF, 0x4F, 255]),
    ("pink_concrete_powder", [0xD7, 0x8F, 0xA5, 255]),
    ("gray_concrete_powder", [0x57, 0x57, 0x59, 255]),
    ("light_gray_concrete_powder", [0x9C, 0xA3, 0xA4, 255]),
    ("cyan_concrete_powder", [0x59, 0x7D, 0x8B, 255]),
    ("purple_concrete_powder", [0x8F, 0x64, 0x9B, 255]),
    ("blue_concrete_powder", [0x4C, 0x64, 0xB0, 255]),
    ("brown_concrete_powder", [0x72, 0x55, 0x40, 255]),
    ("green_concrete_powder", [0x5E, 0x7B, 0x45, 255]),
    ("red_concrete_powder", [0x93, 0x57, 0x57, 255]),
    ("black_concrete_powder", [0x1D, 0x1D, 0x20, 255]),
    // Terracotta family.
    ("terracotta", TERRACOTTA_WHITE),
    // Wool and carpet by dye color (before generic wool fallback).
    ("white_wool", SNOW),
    ("orange_wool", ORANGE),
    ("magenta_wool", MAGENTA),
    ("light_blue_wool", LIGHT_BLUE),
    ("yellow_wool", YELLOW),
    ("lime_wool", LIME),
    ("pink_wool", PINK),
    ("gray_wool", GRAY),
    ("light_gray_wool", LIGHT_GRAY),
    ("cyan_wool", CYAN),
    ("purple_wool", PURPLE),
    ("blue_wool", BLUE),
    ("brown_wool", BROWN),
    ("green_wool", GREEN),
    ("red_wool", RED),
    ("black_wool", BLACK),
    ("_carpet", LIGHT_GRAY),
    // Wood families: planks, logs, stairs, slabs, fences, doors.
    ("spruce", [0x5C, 0x44, 0x29, 255]),
    ("birch", [0xD0, 0xC9, 0x9D, 255]),
    ("jungle", [0x92, 0x6E, 0x55, 255]),
    ("acacia", [0xAB, 0x66, 0x39, 255]),
    ("dark_oak", [0x40, 0x2D, 0x1A, 255]),
    ("mangrove", [0x77, 0x33, 0x2B, 255]),
    ("cherry", [0xE2, 0xA7, 0xA6, 255]),
    ("pale_oak", [0xCD, 0xC6, 0xB5, 255]),
    ("bamboo_", [0xC3, 0xB8, 0x51, 255]),
    // Stems (nether) and generic wood.
    ("stem", CRIMSON_STEM),
    ("_planks", WOOD),
    ("_log", WOOD),
    ("_wood", WOOD),
    ("_hyphae", CRIMSON_HYPHAE),
    // Ores embedded in stone keep the stone look with a hint of color.
    ("_ore", STONE),
    ("_bricks", STONE),
    ("bricks", [0x97, 0x63, 0x53, 255]),
    ("sandstone", SAND),
    ("red_sandstone", ORANGE),
    ("_stained_glass", LIGHT_BLUE),
    ("_glass", [0xC0, 0xD5, 0xDC, 255]),
    ("_shulker_box", PURPLE),
    ("_bed", RED),
    ("_chest", WOOD),
    ("_sign", WOOD),
    ("_hanging_sign", WOOD),
    ("_banner", RED),
    ("_coral", CYAN),
    ("_coral_block", CYAN),
    ("_coral_fan", CYAN),
    ("_coral_wall_fan", CYAN),
    ("sea_pickle", [0x53, 0x6D, 0x36, 255]),
    ("seagrass", [0x33, 0x61, 0x2F, 255]),
    ("kelp", [0x33, 0x61, 0x2F, 255]),
    ("_anvil", [0x49, 0x49, 0x49, 255]),
    ("_lantern", [0x6E, 0x59, 0x3F, 255]),
    ("_candle", [0xE0, 0xD8, 0xC0, 255]),
    ("_fungus", CRIMSON_NYLIUM),
    ("_roots", CRIMSON_NYLIUM),
    ("sprouts", WARPED_NYLIUM),
    ("_door", WOOD),
    ("_trapdoor", WOOD),
    ("_fence", WOOD),
    ("_gate", WOOD),
    ("_stairs", STONE),
    ("_slab", STONE),
    ("_wall", STONE),
    ("_pressure_plate", WOOD),
    ("_button", STONE),
    ("_torch", [0xE0, 0xC0, 0x60, 255]),
    ("_rail", [0x7A, 0x6C, 0x5C, 255]),
    ("_froglights", [0xFF, 0xE9, 0x9D, 255]),
];

/// Deterministic fallback for unknown blocks: a mid-brightness, slightly
/// desaturated color keyed by the name hash.
fn fallback(name: &str) -> Rgba {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    let h = (hash >> 24) as u16;
    let base = 90 + (h & 0x7f) as u8; // 90..=216
    let r = base;
    let g = (base as i32 + ((h >> 7) as i32 & 0x1f) - 16).clamp(0, 255) as u8;
    let b = (base as i32 + ((h >> 12) as i32 & 0x1f) - 16).clamp(0, 255) as u8;
    [r, g, b, 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_blocks() {
        assert_eq!(block_color("grass_block"), rgba(tints::GRASS));
        assert_eq!(block_color("water"), rgba(tints::WATER));
        assert_eq!(block_color("stone"), STONE);
    }

    #[test]
    fn family_rules() {
        assert_eq!(block_color("oak_planks"), WOOD);
        assert_eq!(block_color("spruce_stairs"), [0x5C, 0x44, 0x29, 255]);
        assert_eq!(block_color("jungle_leaves"), rgba(tints::FOLIAGE_DARK));
    }

    #[test]
    fn shading() {
        assert_eq!(shade([100, 100, 100, 255], 0.5), [50, 50, 50, 255]);
        assert_eq!(shade([200, 200, 200, 255], 2.0), [255, 255, 255, 255]);
    }

    #[test]
    fn fallback_is_deterministic() {
        assert_eq!(fallback("mystery_block"), fallback("mystery_block"));
        assert_ne!(fallback("mystery_block"), fallback("other_block"));
    }
}
