#!/usr/bin/env python3
"""Generate Rust tables replicating Xaero's World Map coloring from the
Minecraft 26.3 jar: block -> vanilla map color, biome -> grass/foliage/water
tints, and the block tint-kind list (mirrors vanilla BlockColors.createDefault)."""

# NOTE: the Blocks.java regex below only sees explicit `mapColor(MapColor.*)`
# calls; every block registered with `ofLegacyCopy`/`mapColor(Block)` (all
# stairs, slabs, logs, fences, ...) is missed, and item ids leak in as false
# positives. The checked-in BLOCK_MAP_COLORS was therefore regenerated from a
# per-state `getMapColor().col` export (facts.json + gameplay-facts.json of a
# prepared Horizon 26.3 client), which resolves those derivations. Regenerating
# from Blocks.java alone will reintroduce ~600 missing entries.

import json
import os
import re
import struct
import subprocess
import zlib

MC = "/tmp/xaero-analysis/mc"
DEC = f"{MC}/dec"
OUT = "/home/coobyk/Projects/mca-selector-rs/crates/mc-core/src/map/xaero_colors.rs"

# ---------------------------------------------------------------- map colors
mapcolor_int = {}
src = open(f"{DEC}/net/minecraft/world/level/material/MapColor.java").read()
for m in re.finditer(r"MapColor (\w+) = new MapColor\(\d+, (\d+|0x[0-9A-Fa-f]+)\)", src):
    mapcolor_int[m.group(1)] = int(m.group(2), 0)
print(f"MapColor constants: {len(mapcolor_int)}")

# ------------------------------------------------------- block id -> name
javap = subprocess.run(
    ["javap", "-c", "-p", f"{MC}/net/minecraft/references/BlockItemIds.class"],
    capture_output=True, text=True).stdout
id_name = {}
lines = javap.splitlines()
last_ldc = None
for ln in lines:
    s = ln.strip()
    m = re.search(r'ldc(?:_w|_2_w)?\s+.*// String (\S+)', s)
    if m:
        last_ldc = m.group(1)
    m2 = re.search(r'putstatic.*Field (\w+):', s)
    if m2 and last_ldc:
        id_name[m2.group(1)] = last_ldc
        last_ldc = None
javap2 = subprocess.run(
    ["javap", "-c", "-p", f"{MC}/net/minecraft/references/BlockIds.class"],
    capture_output=True, text=True).stdout
for ln in javap2.splitlines():
    s2 = ln.strip()
    m = re.search(r'ldc(?:_w|_2_w)?\s+.*// String (\S+)', s2)
    if m:
        last_ldc = m.group(1)
    m2 = re.search(r'putstatic.*Field (\w+):', s2)
    if m2 and last_ldc:
        id_name.setdefault(m2.group(1), last_ldc)
        last_ldc = None
print(f"BlockItemIds+BlockIds: {len(id_name)}")

# --------------------------------------------------- block -> map color
blocks_src = open(f"{DEC}/net/minecraft/world/level/block/Blocks.java").read()
blocks = {}
pat = re.compile(
    r"public static final Block (\w+) = Blocks\.register\((?:\(ResourceKey<Block>\))?\s*Block(?:Item)?Ids\.(\w+),")
starts = list(pat.finditer(blocks_src))
for i, m in enumerate(starts):
    const, id_const = m.group(1), m.group(2)
    end = starts[i + 1].start() if i + 1 < len(starts) else len(blocks_src)
    stmt = blocks_src[m.start():end]
    if "leavesProperties(" in stmt:
        name = id_name.get(id_const, id_const.lower())
        blocks[name] = mapcolor_int["PLANT"]
        continue
    mc_m = re.search(r"mapColor\((?:[^()]|\([^()]*\))*?MapColor\.(\w+)", stmt)
    if mc_m:
        name = id_name.get(id_const)
        if name is None:
            name = id_const.lower()
        color = mapcolor_int.get(mc_m.group(1))
        if color:
            blocks[name] = color
print(f"blocks with map color: {len(blocks)}")

# ------------------------------------------------------------- biome tints
def load_png(path):
    data = open(path, "rb").read()
    # minimal PNG decode (truecolor 8-bit)
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    pos = 8
    w = h = None
    idat = b""
    while pos < len(data):
        ln = struct.unpack(">I", data[pos:pos+4])[0]
        typ = data[pos+4:pos+8]
        chunk = data[pos+8:pos+8+ln]
        if typ == b"IHDR":
            w, h, depth, ctype = struct.unpack(">IIBB", chunk[:10])
            assert depth == 8 and ctype == 2, (depth, ctype)
        elif typ == b"IDAT":
            idat += chunk
        pos += 12 + ln
    raw = zlib.decompress(idat)
    stride = w * 3
    rows = []
    prev = bytearray(stride)
    p = 0
    for _ in range(h):
        f = raw[p]; p += 1
        row = bytearray(raw[p:p+stride]); p += stride
        if f == 0:
            pass
        elif f == 1:
            for i in range(3, stride):
                row[i] = (row[i] + row[i-3]) & 255
        elif f == 2:
            for i in range(stride):
                row[i] = (row[i] + prev[i]) & 255
        elif f == 3:
            for i in range(stride):
                a = row[i-3] if i >= 3 else 0
                row[i] = (row[i] + ((a + prev[i]) >> 1)) & 255
        elif f == 4:
            for i in range(stride):
                a = row[i-3] if i >= 3 else 0
                b = prev[i]
                c = prev[i-3] if i >= 3 else 0
                pa, pb, pc = abs(b-c), abs(a-c), abs(a+b-2*c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                row[i] = (row[i] + pr) & 255
        else:
            raise Exception(f"filter {f}")
        rows.append(bytes(row))
        prev = row
    return w, h, rows

def colormap_sample(path, temp, rain):
    w, h, rows = load_png(path)
    x = int((1.0 - temp) * 255.0)
    y = int((1.0 - rain * temp) * 255.0)
    x = max(0, min(255, x)); y = max(0, min(255, y))
    row = rows[y]
    return (row[x*3] << 16) | (row[x*3+1] << 8) | row[x*3+2]

grass_cmap = f"{MC}/assets/minecraft/textures/colormap/grass.png"
foliage_cmap = f"{MC}/assets/minecraft/textures/colormap/foliage.png"

MODIFIER_DARK_FOREST = lambda base: ((base & 0xFEFEFE) + 2634762) >> 1
SWAMP_GRASS = 0x6A7039  # the majority noise variant of the swamp modifier

def biome_effects(j, temp, rain):
    eff = j.get("effects", {})
    def as_int(v, default):
        if v is None:
            return default
        if isinstance(v, str):
            return int(v.replace("#", ""), 16)
        return int(v)
    grass = as_int(eff.get("grass_color"), None)
    foliage = as_int(eff.get("foliage_color"), None)
    water = as_int(eff.get("water_color"), 0x3F76E4)
    modifier = eff.get("grass_color_modifier", "none")
    if grass is None:
        grass = colormap_sample(grass_cmap, temp, rain)
    if foliage is None:
        foliage = colormap_sample(foliage_cmap, temp, rain)
    if modifier == "dark_forest":
        grass = MODIFIER_DARK_FOREST(grass)
    elif modifier == "swamp":
        grass = SWAMP_GRASS
    return grass, foliage, water

biomes = {}
biome_dir = f"{MC}/data/minecraft/worldgen/biome"
for fn in os.listdir(biome_dir):
    if not fn.endswith(".json"):
        continue
    name = fn[:-5]
    j = json.load(open(f"{biome_dir}/{fn}"))
    temp = max(0.0, min(1.0, j.get("temperature", 0.5)))
    rain = max(0.0, min(1.0, j.get("downfall", 0.5)))
    grass, foliage, water = biome_effects(j, temp, rain)
    biomes[name] = (grass, foliage, water)
print(f"biomes: {len(biomes)}")

# ------------------------------------------------------------ tint kinds
TINT_GRASS = ["grass_block", "short_grass", "fern", "potted_fern", "bush",
              "large_fern", "tall_grass", "sugar_cane"]
TINT_FOLIAGE = ["oak_leaves", "jungle_leaves", "acacia_leaves", "dark_oak_leaves",
                "vine", "mangrove_leaves"]
TINT_DRY = ["leaf_litter"]
TINT_FIXED = {"spruce_leaves": 0x619961, "birch_leaves": 0x80A755,
              "lily_pad": 0x203560}
TINT_WATER = ["water", "bubble_column"]

# --------------------------------------------------------------- emit rust
def rs_str(s):
    return '"' + s + '"'

lines = []
lines.append("//! Generated from the Minecraft 26.3 jar (unobfuscated): vanilla map")
lines.append("//! colors per block, per-biome grass/foliage/water tints, and tint")
lines.append("//! kinds mirroring `BlockColors.createDefault`. Regenerate with the")
lines.append("//! script in `docs/` (xaero parity work).")
lines.append("")
lines.append("#![allow(clippy::all)]")
lines.append("")

lines.append("/// Block name (without `minecraft:`) -> vanilla map color RGB.")
lines.append("/// Blocks missing from this list have no map color: they are")
lines.append("/// invisible to the map and the column continues downwards.")
lines.append("pub static BLOCK_MAP_COLORS: &[(&str, u32)] = &[")
for name in sorted(blocks):
    lines.append(f"    ({rs_str(name)}, 0x{blocks[name]:06X}),")
lines.append("];")
lines.append("")

lines.append("/// Biome name -> (grass, foliage, water) tint colors.")
lines.append("pub static BIOME_TINTS: &[(&str, u32, u32, u32)] = &[")
for name in sorted(biomes):
    g, f, w = biomes[name]
    lines.append(f"    ({rs_str(name)}, 0x{g:06X}, 0x{f:06X}, 0x{w:06X}),")
lines.append("];")
lines.append("")

lines.append("/// Blocks tinted by the biome grass color.")
lines.append("pub static TINT_GRASS: &[&str] = &[")
for n in sorted(TINT_GRASS):
    lines.append(f"    {rs_str(n)},")
lines.append("];")
lines.append("")
lines.append("/// Blocks tinted by the biome foliage color.")
lines.append("pub static TINT_FOLIAGE: &[&str] = &[")
for n in sorted(TINT_FOLIAGE):
    lines.append(f"    {rs_str(n)},")
lines.append("];")
lines.append("")
lines.append("/// Blocks tinted by the biome dry-foliage color.")
lines.append("pub static TINT_DRY_FOLIAGE: &[&str] = &[")
for n in sorted(TINT_DRY):
    lines.append(f"    {rs_str(n)},")
lines.append("];")
lines.append("")
lines.append("/// Blocks tinted by the biome water color.")
lines.append("pub static TINT_WATER: &[&str] = &[")
for n in sorted(TINT_WATER):
    lines.append(f"    {rs_str(n)},")
lines.append("];")
lines.append("")
lines.append("/// Blocks with a fixed world tint.")
lines.append("pub static TINT_FIXED: &[(&str, u32)] = &[")
for n, c in sorted(TINT_FIXED.items()):
    lines.append(f"    ({rs_str(n)}, 0x{c:06X}),")
lines.append("];")

os.makedirs(os.path.dirname(OUT), exist_ok=True)
open(OUT, "w").write("\n".join(lines) + "\n")
print(f"wrote {OUT}")

# sanity spot checks
for probe in ["grass_block", "stone", "water", "oak_leaves", "sand", "snow"]:
    print(probe, hex(blocks.get(probe, 0)))
plains = biomes.get("plains")
print("plains:", [hex(v) for v in plains] if plains else None)
desert = biomes.get("desert")
print("desert:", [hex(v) for v in desert] if desert else None)
