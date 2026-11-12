#!/usr/bin/env python3
"""
xaero_palette.py — generate a Xaero-World-Map-equivalent block color palette
for the offline map renderer in mca-selector-rs.

Pipeline (mirrors Xaero's color resolver):
  block -> blockstates json -> model(s) -> merge parent chain ->
  largest UP-facing quad (after state rotation) -> average sprite -> ARGB.
  No UP quad -> model's #particle texture.

Input:
  --jar    vanilla client jar (assets/minecraft/{blockstates,models,textures})
  --blocks prismarine blocks.json
  --ref    an existing colors_*.json used ONLY to copy `tints` verbatim
Output:
  --out    colors.json with {version, states, colors, tints, xaero_meta}

Stdlib + PIL only. Deterministic: sorted keys, stable enumeration.
"""

import argparse
import base64
import io
import itertools
import json
import os
import sys
import zipfile
from collections import Counter, defaultdict

try:
    from PIL import Image
except ImportError:
    sys.stderr.write("PIL (Pillow) is required: pip install Pillow\n")
    raise

NS = "minecraft"

# ---------------------------------------------------------------- rotation --

# Direction maps for 90-degree rotations (vanilla ModelRotation semantics).
DIRECTIONS = ("up", "down", "north", "south", "east", "west")

X_ROT = {
    90:  {"up": "north", "north": "down", "down": "south", "south": "up"},
    180: {"up": "down", "down": "up", "north": "south", "south": "north"},
    270: {"up": "south", "south": "down", "down": "north", "north": "up"},
}
Y_ROT = {
    90:  {"north": "west", "west": "south", "south": "east", "east": "north"},
    180: {"north": "south", "south": "north", "east": "west", "west": "east"},
    270: {"north": "east", "east": "south", "south": "west", "west": "north"},
}
# element-level z rotation (same handedness as the x table)
Z_ROT = {
    90:  {"up": "east", "east": "down", "down": "west", "west": "up"},
    180: {"up": "down", "down": "up", "east": "west", "west": "east"},
    270: {"up": "west", "west": "down", "down": "east", "east": "up"},
}
AXIS_ROT = {"x": X_ROT, "y": Y_ROT, "z": Z_ROT}


def rot_dir(direction, x=0, y=0, el=None):
    """Rotate a face direction: element rotation, then state x, then state y."""
    d = direction
    if el is not None:
        axis, ang = el
        d = AXIS_ROT[axis].get(ang, {}).get(d, d)
    d = X_ROT.get(x, {}).get(d, d)
    d = Y_ROT.get(y, {}).get(d, d)
    return d


# ------------------------------------------------------------- name lists --

SKIP_BLOCKS = {"air", "cave_air", "void_air"}

# name -> tint kind used for `properties` bits in the colors table
# mirrors vanilla BlockColors.create() (decompiled 1.21.4): grass providers
TINT_GRASS = {
    "grass_block", "short_grass", "tall_grass", "fern", "large_fern",
    "sugar_cane", "pink_petals",
}
TINT_FOLIAGE = {
    "oak_leaves", "jungle_leaves", "acacia_leaves", "dark_oak_leaves",
    "mangrove_leaves", "vine",
}
TINT_WATER = {"water", "bubble_column", "water_cauldron"}
TINT_DRY = {"leaf_litter"}
# fixed (non-biome) tints baked into the emitted color
STATIC_TINT = {
    "birch_leaves": 0x80A755,
    "spruce_leaves": 0x619961,
    "lily_pad": 0x208030,
    "attached_pumpkin_stem": 0xE0C71C,
    "attached_melon_stem": 0xE0C71C,
}
# models carry a tintindex quad but vanilla registers no color provider for
# these — the raw texture color is used
KNOWN_UNTINTED = {"cherry_leaves", "pale_oak_leaves"}

# property bit values (colors.rs: GRASS_TINT|FOLIAGE_TINT|WATER|DRY_FOLIAGE)
P_GRASS, P_FOLIAGE, P_WATER, P_DRY = 2, 4, 8, 128
PROP_BIT = {"grass": P_GRASS, "foliage": P_FOLIAGE, "water": P_WATER, "dry": P_DRY}

# overlay classes for xaero_meta
OV_FLUID = {"water", "bubble_column", "lava"}
OV_GLASS_EXACT = {
    "ice", "frosted_ice", "tinted_glass", "honey_block", "slime_block",
    "nether_portal", "end_gateway", "end_portal", "beacon",
}

def overlay_class(name):
    if name in OV_FLUID:
        return "f"
    if name in OV_GLASS_EXACT:
        return "g"
    if name.endswith("_stained_glass") or name.endswith("_stained_glass_pane"):
        return "g"
    return None

# ---- invisible blocks (Xaero never renders these) ----
INVIS_EXACT = {
    "glass", "glass_pane", "barrier", "structure_void", "light",
    "wheat", "carrots", "potatoes", "beetroots", "nether_wart",
    "torchflower_crop", "pitcher_crop",
    "melon_stem", "pumpkin_stem", "attached_melon_stem", "attached_pumpkin_stem",
    "sweet_berry_bush", "dead_bush",
    "seagrass", "kelp", "kelp_plant",
    "cave_vines", "cave_vines_plant", "hanging_roots",
    "small_dripleaf", "big_dripleaf", "big_dripleaf_stem", "spore_blossom",
    "twisting_vines", "twisting_vines_plant", "weeping_vines", "weeping_vines_plant",
    "mangrove_propagule", "bamboo_sapling", "cobweb",
    "moving_piston", "piston_head",
    "lantern", "soul_lantern", "conduit", "lever",
    "tripwire", "tripwire_hook", "flower_pot",
    "short_grass", "fern",
    # MapColor.NONE — Xaero's hasVanillaColor() skips these too:
    # flat redstone/decor blocks that only exist on top of a surface.
    "redstone_wire", "repeater", "comparator", "ladder", "lightning_rod",
}
INVIS_SUFFIXES = (
    "_sign",            # *_sign, *_wall_sign, *_hanging_sign, *_wall_hanging_sign
    "_banner",          # *_banner, *_wall_banner
    "_skull", "_head",
    "_candle", "_candle_cake",
    "_sapling",
    "_wall_torch",
    "_coral_fan", "_coral_wall_fan",
    "_rail",            # rail, powered_rail, detector_rail, activator_rail
    "_button",
)
INVIS_EXACT |= {"torch", "candle"}

# blocks that stay visible even though transparent+empty (spec'd exceptions
# plus judgement calls for flat visible ground covers)
AUTO_INVIS_EXCLUDE_EXACT = {
    "snow", "lily_pad", "leaf_litter", "dragon_egg", "sniffer_egg",
    "powder_snow",
}
AUTO_INVIS_EXCLUDE_SUFFIXES = (
    "_pressure_plate", "_carpet", "_trapdoor",
)

# flower-class blocks rendered when flowers=on (our default: on) —
# vanilla FlowerBlock / FlowerBedBlock / TallFlowerBlock / flower-tagged.
# Plain plants (tall_grass, ferns, mushrooms, seagrass, crops) are NOT here:
# Xaero's isInvisible() drops non-flower PlantBlocks entirely.
FLOWERS_EXACT = {
    "dandelion", "poppy", "allium",
    "azure_bluet", "oxeye_daisy", "cornflower", "lily_of_the_valley",
    "rose_bush", "peony", "lilac", "sunflower", "torchflower",
    "pink_petals", "wildflowers",
    "wither_rose", "open_eyeblossom", "closed_eyeblossom", "pitcher_plant",
}
FLOWERS_SUFFIXES = ("_orchid", "_tulip")

def is_flower(name):
    return name in FLOWERS_EXACT or name.endswith(FLOWERS_SUFFIXES)

def is_invis(name, block):
    if name in INVIS_EXACT or name.endswith(INVIS_SUFFIXES) or name.startswith("potted_"):
        return True
    if name in AUTO_INVIS_EXCLUDE_EXACT or name.endswith(AUTO_INVIS_EXCLUDE_SUFFIXES):
        return False
    if overlay_class(name) is not None or is_flower(name):
        return False
    if block.get("transparent") and block.get("boundingBox") == "empty":
        return True
    return False

# vanilla redstone wire power ramp (power 0..15), baked — not a biome tint
WIRE_COLORS = [
    0x4B0000, 0x550000, 0x5F0000, 0x690000, 0x740000, 0x7E0000, 0x880000,
    0x930000, 0x9D0000, 0xA80000, 0xB20000, 0xBC0000, 0xC70000, 0xD10000,
    0xDC0000, 0xE60000,
]

# fluid fallbacks when no blockstates/model produces a quad
FLUID_TEXTURE = {"water": "block/water_still", "bubble_column": "block/water_still",
                 "lava": "block/lava_still"}
WATER_ALPHA = 0xB2  # Xaero water alpha (~178)

MISSING_RGB = 0xFF00FF  # magenta sentinel for unresolved colors


def tint_multiply(rgb, tint):
    """Same math as the runtime (colors.rs apply_tint): (a*b)>>8 per channel."""
    r = (((tint >> 16) & 0xFF) * ((rgb >> 16) & 0xFF)) >> 8
    g = (((tint >> 8) & 0xFF) * ((rgb >> 8) & 0xFF)) >> 8
    b = ((tint & 0xFF) * (rgb & 0xFF)) >> 8
    return (r << 16) | (g << 8) | b


def natkey(v):
    """Deterministic value ordering: numerics first, then lexicographic."""
    return (0, int(v)) if str(v).lstrip("-").isdigit() else (1, str(v))


# ------------------------------------------------------------------ assets --

class Assets:
    def __init__(self, jar_path):
        self.zf = zipfile.ZipFile(jar_path)
        self.names = set(self.zf.namelist())
        self._json_cache = {}
        self._tex_cache = {}

    def has(self, path):
        return path in self.names

    def read_json(self, path):
        if path in self._json_cache:
            return self._json_cache[path]
        try:
            data = self.zf.read(path)
        except KeyError:
            self._json_cache[path] = None
            return None
        self._json_cache[path] = json.loads(data.decode("utf-8"))
        return self._json_cache[path]

    def texture_avg(self, tex_path):
        """Average first ts x ts region of the sprite -> (a, r, g, b) or None."""
        if tex_path in self._tex_cache:
            return self._tex_cache[tex_path]
        result = None
        try:
            raw = self.zf.read(tex_path)
        except KeyError:
            raw = None
        if raw is not None:
            img = Image.open(io.BytesIO(raw)).convert("RGBA")
            w, h = img.size
            ts = min(w, h)
            diff = max(1, min(4, ts // 8))
            n = sa = sr = sg = sb = 0
            px = img.load()
            for yy in range(0, ts, diff):
                for xx in range(0, ts, diff):
                    r, g, b, a = px[xx, yy]
                    if (r == 0 and g == 0 and b == 0) or a <= 10:
                        continue
                    sa += a
                    sr += r
                    sg += g
                    sb += b
                    n += 1
            if n:
                result = (sa // n, sr // n, sg // n, sb // n)
            else:
                result = (0, 0, 0, 0)
        self._tex_cache[tex_path] = result
        return result


def split_ns(ref):
    if ":" in ref:
        ns, rel = ref.split(":", 1)
        return ns, rel
    return NS, ref


def model_path(ref):
    ns, rel = split_ns(ref)
    return "assets/%s/models/%s.json" % (ns, rel)


def texture_path(ref):
    ns, rel = split_ns(ref)
    if "/" not in rel:
        rel = "block/" + rel
    return "assets/%s/textures/%s.png" % (ns, rel)


class ResolvedModel:
    __slots__ = ("textures", "elements")

    def __init__(self, textures, elements):
        self.textures = textures
        self.elements = elements

    def tex(self, ref, depth=0):
        """Resolve '#name' chains through merged textures -> bare texture ref."""
        seen = 0
        while isinstance(ref, str) and ref.startswith("#") and seen < 8:
            ref = self.textures.get(ref[1:])
            seen += 1
        return ref


def resolve_model(assets, ref, _cache={}):
    """Merge a model's parent chain: child textures override parent; elements
    come from the nearest model in the chain that defines them."""
    if ref in _cache:
        return _cache[ref]
    chain = []
    cur_ref = ref
    seen = set()
    while cur_ref is not None and cur_ref not in seen:
        seen.add(cur_ref)
        j = assets.read_json(model_path(cur_ref))
        if j is None:
            break
        chain.append(j)
        cur_ref = j.get("parent")
    textures = {}
    elements = None
    for j in reversed(chain):            # root -> child: child wins
        textures.update(j.get("textures") or {})
    for j in chain:                       # child -> root: nearest elements
        if "elements" in j:
            elements = j["elements"]
            break
    m = ResolvedModel(textures, elements or [])
    _cache[ref] = m
    return m


# ------------------------------------------------------------- blockstates --

def parse_variant_key(key):
    if key == "":
        return {}
    return dict(part.split("=", 1) for part in key.split(","))


def pick_apply(apply):
    """An apply entry is a model object or a weighted list; take max weight."""
    if isinstance(apply, list):
        if not apply:
            return None
        return max(apply, key=lambda a: a.get("weight", 1))
    return apply


def when_matches(when, props):
    if not when:
        return True
    if isinstance(when, list):
        return any(when_matches(clause, props) for clause in when)
    if "OR" in when:
        return any(when_matches(clause, props) for clause in when["OR"])
    for k, v in when.items():
        allowed = str(v).split("|")
        if props.get(k) not in allowed:
            return False
    return True


def block_prop_space(block, bs):
    """Full property space for a block: prismarine states + anything seen in
    the blockstates file (variant keys / multipart when clauses)."""
    space = {}
    for st in block.get("states") or []:
        name = st["name"]
        if st.get("values"):
            space[name] = [str(v) for v in st["values"]]
        elif st.get("type") == "bool":
            space[name] = ["false", "true"]
        else:
            space[name] = [str(i) for i in range(st.get("num_values", 0))]
    if bs and "variants" in bs:
        for key in bs["variants"]:
            for k, v in parse_variant_key(key).items():
                space.setdefault(k, [])
                if v not in space[k]:
                    space[k].append(v)
    if bs and "multipart" in bs:
        def harvest(w):
            if "OR" in w:
                for c in w["OR"]:
                    harvest(c)
                return
            for k, v in w.items():
                for val in str(v).split("|"):
                    space.setdefault(k, [])
                    if val not in space[k]:
                        space[k].append(val)
        for part in bs["multipart"]:
            harvest(part.get("when") or {})
        # bool-ish props seen with only one value get the counterpart so that
        # the cartesian product covers real states
        for k, vals in space.items():
            if vals and all(v in ("true", "false") for v in vals) and len(vals) == 1:
                vals.append("false" if vals[0] == "true" else "true")
    return {k: sorted(v, key=natkey) for k, v in space.items()}


def applies_for_state(bs, props, warnings):
    """Return list of (model_ref, x_rot, y_rot) for one block state."""
    out = []
    if "variants" in bs:
        best = None
        best_score = -1
        for key, val in bs["variants"].items():
            kprops = parse_variant_key(key)
            # variant keys enumerate the full property space; accept any
            # candidate whose stated props all match (partial = fallback)
            if all(props.get(k) == v for k, v in kprops.items()):
                score = len(kprops)
                if score > best_score or (score == best_score and key == ""):
                    best, best_score = val, score
        if best is None:
            warnings.append("no variant matched %s" % props)
            return []
        a = pick_apply(best)
        if a and a.get("model"):
            out.append((a["model"], a.get("x", 0), a.get("y", 0)))
    elif "multipart" in bs:
        for part in bs["multipart"]:
            if when_matches(part.get("when"), props):
                a = pick_apply(part.get("apply"))
                if a and a.get("model"):
                    out.append((a["model"], a.get("x", 0), a.get("y", 0)))
    return out


def element_rotation(el):
    r = el.get("rotation")
    if not r:
        return None
    ang = int(round(r.get("angle", 0))) % 360
    if ang == 0:
        return None
    axis = r.get("axis", "y")
    if ang in (90, 180, 270) and axis in AXIS_ROT:
        return (axis, ang)
    return None  # 22.5/45/etc: not axis-aligned; ignore (kept faces only)


def quad_color(assets, applies, missing_tex, warnings):
    """Pick the largest UP quad across all model applies; fall back to the
    first model's #particle texture. Returns (argb_tuple, tintindex, is_quad,
    tex_name) or None."""
    best = None            # (area, tex_ref, tintindex, model)
    particle = None
    particle_model = None
    for ref, x, y in applies:
        m = resolve_model(assets, ref)
        if particle is None:
            ptex = m.tex("#particle")
            if ptex:
                particle, particle_model = ptex, m
        for el in m.elements:
            fr, to = el.get("from", [0, 0, 0]), el.get("to", [16, 16, 16])
            area = (to[0] - fr[0]) * (to[2] - fr[2])
            elrot = element_rotation(el)
            for dname, face in (el.get("faces") or {}).items():
                if rot_dir(dname, x=x, y=y, el=elrot) != "up":
                    continue
                tex_ref = face.get("texture")
                tint = face.get("tintindex", -1)
                if best is None or area > best[0]:
                    best = (area, tex_ref, tint, m)
    def avg_for(model, tex_ref):
        if not tex_ref:
            return None, None
        tref = model.tex(tex_ref) if isinstance(tex_ref, str) and tex_ref.startswith("#") else tex_ref
        if not tref:
            missing_tex.add(str(tex_ref))
            return None, tex_ref
        path = texture_path(tref)
        if not assets.has(path):
            missing_tex.add(tref)
            return None, tref
        return assets.texture_avg(path), tref

    if best is not None:
        avg, tname = avg_for(best[3], best[1])
        if avg is None and particle:
            # unresolvable quad texture -> try the particle sprite
            avg, tname = avg_for(particle_model, particle)
        if avg is None:
            return None
        if avg[1] == 0 and avg[2] == 0 and avg[3] == 0 and particle:
            # quad texture averaged to black: retry once with particle sprite
            avg2, tname2 = avg_for(particle_model, particle)
            if avg2 is not None:
                return avg2, best[2], True, tname2
        return avg, best[2], True, tname
    if particle:
        avg, tname = avg_for(particle_model, particle)
        if avg is None:
            return None
        return avg, 0, False, tname
    return None


# --------------------------------------------------------------- classify --

def classify_tint(name, tintindex, is_quad, reports):
    """-> (kind, prop_bit, baked_rgb_or_None).
    kind: grass|foliage|water|dry|birch|spruce|mangrove|lily_pad|unknown|None"""
    if name in STATIC_TINT:
        if not is_quad or tintindex >= 0 or name == "lily_pad":
            kind = name.rsplit("_leaves", 1)[0] if name.endswith("_leaves") else name
            return (kind, 0, STATIC_TINT[name])
        return (None, 0, None)
    if name in TINT_GRASS:
        if tintindex >= 0 or not is_quad:
            return ("grass", P_GRASS, None)
        reports["listed_no_tint"].add(name)
        return (None, 0, None)
    if name in TINT_FOLIAGE:
        if tintindex >= 0 or not is_quad:
            return ("foliage", P_FOLIAGE, None)
        reports["listed_no_tint"].add(name)
        return (None, 0, None)
    if name in TINT_WATER:
        return ("water", P_WATER, None)
    if name in TINT_DRY:
        return ("dry", P_DRY, None)
    if is_quad and tintindex >= 0 and name not in KNOWN_UNTINTED:
        reports["unknown_tint"].add(name)
        return ("unknown", 0, None)
    return (None, 0, None)


# -------------------------------------------------------------------- main --

def state_bitset_b64(props, index):
    """Java BitSet.valueOf little-endian bytes, standard base64."""
    bits = bytearray()
    for k, v in props.items():
        bit = index.get(k, {}).get(v)
        if bit is None:
            continue
        i = bit >> 3
        while len(bits) <= i:
            bits.append(0)
        bits[i] |= 1 << (bit & 7)
    return base64.b64encode(bytes(bits)).decode("ascii")


def color_value(rgb, props):
    hexs = "%06x" % rgb
    if props:
        return {"color": hexs, "properties": props}
    return hexs


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--jar", default="/tmp/mc_assets/client.jar")
    ap.add_argument("--blocks", default="/tmp/mc_assets/blocks.json")
    ap.add_argument("--ref", default=None,
                    help="colors json to copy `tints` from (auto-detected)")
    ap.add_argument("--out", default=os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "crates/mc-core/resources/mapping/java_xaero/colors.json"))
    args = ap.parse_args()

    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    ref_candidates = [args.ref] if args.ref else [
        os.path.join(here, "crates/mc-core/resources/mapping/java_1_21_3/colors.json"),
        os.path.join(here, "crates/mc-core/resources/mapping/java_1_21/colors_24w18a.json"),
        os.path.join(here, "crates/mc-core/resources/mapping/java_1_21/colors_25w32a.json"),
    ]
    tints = None
    ref_used = None
    for cand in ref_candidates:
        if cand and os.path.isfile(cand):
            with open(cand) as f:
                tints = json.load(f).get("tints")
            if tints:
                ref_used = cand
                break
    if tints is None:
        sys.exit("no reference colors json found for `tints` copy")
    print("tints copied from %s (%d biomes)" % (ref_used, len(tints)))

    with open(args.blocks) as f:
        blocks = {b["name"]: b for b in json.load(f)}

    assets = Assets(args.jar)
    reports = {
        "unknown_tint": set(),
        "listed_no_tint": set(),
        "missing_blockstates": [],
        "missing_model": set(),
        "no_color": [],
    }
    missing_tex = set()
    warnings = []

    # ---- pass 1: per-state raw color info for every block ----
    # block -> {props_tuple: (rgb, alpha, prop_bit, tint_kind)}
    block_states = {}
    block_space = {}
    default_state_color = {}

    for name in sorted(blocks):
        if name in SKIP_BLOCKS:
            continue
        block = blocks[name]
        bs = assets.read_json("assets/minecraft/blockstates/%s.json" % name)

        # special cases first: fluids / redstone
        if name in FLUID_TEXTURE or (bs is None):
            space = block_prop_space(block, bs)
            block_space[name] = space
            per_state = {}
            tex = FLUID_TEXTURE.get(name)
            if tex:
                avg = assets.texture_avg(texture_path(tex)) or (0, 0, 0, 0)
            else:
                avg = (0, 0, 0, 0)
                if bs is None:
                    reports["missing_blockstates"].append(name)
            rgb = (avg[1] << 16) | (avg[2] << 8) | avg[3]
            if name == "lava" and rgb == 0:
                rgb = 0xCF5B16
            kind, pbit, baked = classify_tint(name, 0, False, reports)
            if baked is not None:
                rgb = tint_multiply(rgb, baked)
            if rgb == 0 and name not in FLUID_TEXTURE:
                rgb = MISSING_RGB
            alpha = WATER_ALPHA if name in ("water", "bubble_column") else avg[0]
            for combo in itertools.product(*space.values()) if space else [()]:
                props = dict(zip(space.keys(), combo))
                per_state[tuple(sorted(props.items()))] = (
                    rgb, alpha, pbit, kind)
            default_state_color[name] = (rgb, alpha, pbit, kind)
            block_states[name] = per_state
            continue

        space = block_prop_space(block, bs)
        block_space[name] = space
        per_state = {}
        value_lists = [space[k] for k in sorted(space)]
        key_names = sorted(space)
        all_applies = []
        if "multipart" in bs:
            for part in bs["multipart"]:
                a = pick_apply(part.get("apply"))
                if a and a.get("model"):
                    all_applies.append((a["model"], a.get("x", 0), a.get("y", 0)))
        else:
            for val in (bs.get("variants") or {}).values():
                a = pick_apply(val)
                if a and a.get("model"):
                    all_applies.append((a["model"], a.get("x", 0), a.get("y", 0)))
        for combo in itertools.product(*value_lists) if value_lists else [()]:
            props = dict(zip(key_names, combo))
            applies = applies_for_state(bs, props, warnings)
            if not applies:
                # state no multipart part matches (e.g. wall up=false with all
                # sides none — unreachable in saves): color it from any part's
                # particle sprite so it never renders magenta.
                res = quad_color(assets, all_applies, missing_tex, warnings)
                if res is not None:
                    (a, r, g, b), tintidx, is_quad, tname = res
                    per_state[tuple(sorted(props.items()))] = (
                        (r << 16) | (g << 8) | b, a, 0, None)
                    continue
                reports["missing_model"].add(name)
                per_state[tuple(sorted(props.items()))] = (
                    MISSING_RGB, 255, 0, None)
                continue
            res = quad_color(assets, applies, missing_tex, warnings)
            if res is None:
                per_state[tuple(sorted(props.items()))] = (
                    MISSING_RGB, 255, 0, None)
                continue
            (a, r, g, b), tintidx, is_quad, tname = res
            rgb = (r << 16) | (g << 8) | b
            if rgb == 0:
                rgb = MISSING_RGB
            if name == "redstone_wire":
                p = int(props.get("power", 0))
                per_state[tuple(sorted(props.items()))] = (
                    WIRE_COLORS[p], a, 0, None)
                continue
            if name in ("pumpkin_stem", "melon_stem"):
                # vanilla BlockColors: axk.a(age*32, 255-age*8, age*4) on the
                # stem texture — age 7 matches the attached-stem 0xE0C71C
                age = int(props.get("age", 0))
                ramp = (min(age * 32, 255) << 16) | \
                    (max(255 - age * 8, 0) << 8) | min(age * 4, 255)
                per_state[tuple(sorted(props.items()))] = (
                    tint_multiply(rgb, ramp), a, 0, None)
                continue
            kind, pbit, baked = classify_tint(name, tintidx, is_quad, reports)
            if baked is not None:
                rgb = tint_multiply(rgb, baked)
            per_state[tuple(sorted(props.items()))] = (rgb, a, pbit, kind)
        if not per_state:  # empty property space product -> single default
            per_state[()] = (MISSING_RGB, 255, 0, None)
        block_states[name] = per_state

        # default-state color for meta `a`: each prop's first value
        dprops = {k: space[k][0] for k in key_names if space[k]}
        default_state_color[name] = per_state[
            tuple(sorted(dprops.items()))]

    # ---- pass 2: which props actually change the emitted color ----
    relevant_props = set()
    for name, per_state in block_states.items():
        space = block_space[name]
        if len(per_state) <= 1:
            continue
        for pname in space:
            grouped = defaultdict(set)
            for pkey, info in per_state.items():
                rest = tuple((k, v) for k, v in pkey if k != pname)
                grouped[rest].add((info[0], info[2]))
            if any(len(s) > 1 for s in grouped.values()):
                relevant_props.add(pname)

    # ---- build the global states index (only color-relevant props) ----
    index = {}
    all_values = defaultdict(set)
    for name, space in block_space.items():
        for p, vals in space.items():
            if p in relevant_props:
                all_values[p].update(vals)
    bit = 0
    for pname in sorted(all_values):
        index[pname] = {}
        for v in sorted(all_values[pname], key=natkey):
            index[pname][v] = bit
            bit += 1
    total_bits = bit

    # ---- emit colors + meta ----
    colors = {}
    meta = {}
    n_state_keys = 0
    n_invis = 0
    invis_list = []
    ov_count = 0

    for name in sorted(block_states):
        per_state = block_states[name]
        full = "minecraft:" + name
        block = blocks[name]

        # map state -> bitset b64 -> colorinfo (dedupe: same b64 => same color)
        info_of_b64 = {}
        for pkey, info in per_state.items():
            props = dict(pkey)
            b64 = state_bitset_b64(props, index)
            if b64 in info_of_b64 and info_of_b64[b64] != (info[0], info[2]):
                warnings.append("bitset collision %s %s" % (name, pkey))
            info_of_b64[b64] = (info[0], info[2])

        distinct = sorted(set(info_of_b64.values()))
        entry = None
        if len(distinct) == 1:
            rgb, pbit = distinct[0]
            entry = color_value(rgb, pbit)
        else:
            # default = modal colorinfo (minimizes emitted keys)
            counts = Counter(info_of_b64.values())
            default_info = counts.most_common(1)[0][0]
            states_out = {}
            for b64, info in sorted(info_of_b64.items()):
                if info == default_info:
                    continue
                states_out[b64] = color_value(info[0], info[1])
            n_state_keys += len(states_out)
            entry = {"states": states_out,
                     "default": color_value(default_info[0], default_info[1])}
        colors[full] = entry

        # ---- xaero_meta ----
        drgb, dalpha, dpbit, dkind = default_state_color[name]
        m = {}
        a = dalpha
        ov = overlay_class(name)
        if name in ("water", "bubble_column"):
            a = WATER_ALPHA
        m["a"] = a
        m["op"] = int(block.get("filterLight", 0))
        if int(block.get("emitLight", 0)) >= 1:
            m["glow"] = True
        if ov:
            m["ov"] = ov
            ov_count += 1
        if is_invis(name, block):
            m["invis"] = True
            n_invis += 1
            invis_list.append(name)
        if is_flower(name):
            m["fl"] = True
        if dkind:
            m["tint"] = dkind
        # short blocks — Xaero isShort: collision shape maxY < 0.25
        if name == "snow":
            m["short_layers"] = 1  # layer 1 = 2/16 < 0.25; layer 2 = 0.25 not <
        elif name.endswith("_trapdoor"):
            m["short_if"] = {"open": "false", "half": "bottom"}
        elif name.endswith("_carpet") or name.endswith("_pressure_plate") \
                or name in ("lily_pad", "pink_petals", "wildflowers",
                            "leaf_litter"):
            m["short"] = True
        meta[full] = m

    out = {
        "version": "xaero-1.21.4",
        "states": index,
        "colors": colors,
        "tints": tints,
        "xaero_meta": meta,
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        json.dump(out, f, indent=2, sort_keys=False)
    size = os.path.getsize(args.out)

    # ---------------------------------------------------------- self-check --
    print("\n===== xaero_palette self-check =====")
    print("output: %s (%d bytes)" % (args.out, size))
    print("blocks emitted: %d, state-keyed entries: %d, index bits: %d"
          % (len(colors), n_state_keys, total_bits))
    print("invis: %d, overlays: %d" % (n_invis, ov_count))

    def show(full):
        v = colors[full]
        return json.dumps(v) if not isinstance(v, str) else v

    def state_of(name, props):
        space = block_space[name]
        full = {k: space[k][0] for k in space}
        full.update(props)
        return block_states[name][tuple(sorted(full.items()))]

    warnings_out = []
    def check(label, ok, detail):
        print(("  OK  " if ok else "  WARN") + " %s: %s" % (label, detail))
        if not ok:
            warnings_out.append(label)

    gb = colors["minecraft:grass_block"]
    gbx = state_of("grass_block", {"snowy": "false"})
    check("grass_block", True, "entry=%s raw(snowy=false)=%06x"
          % (show("minecraft:grass_block"), gbx[0]))

    oy = state_of("oak_log", {"axis": "y"})
    ox = state_of("oak_log", {"axis": "x"})
    check("oak_log", oy[0] != ox[0],
          "axis=y #%06x vs axis=x #%06x (should differ)" % (oy[0], ox[0]))

    water_info = block_states["water"][tuple(sorted({"level": "0"}.items()))]
    check("water", 170 <= meta["minecraft:water"]["a"] <= 190,
          "rgb=%06x a=%d (expect a~178-182, rgb~gray)"
          % (water_info[0], meta["minecraft:water"]["a"]))

    ice = colors.get("minecraft:ice")
    ice_a = meta.get("minecraft:ice", {}).get("a")
    check("ice", ice_a is not None and ice_a > 100,
          "color=%s a=%s (expect a~190)" % (ice, ice_a))

    lava_info = block_states["lava"][tuple(sorted({"level": "0"}.items()))]
    check("lava", lava_info[0] > 0x800000,
          "rgb=%06x (expect bright orange ~#CF5B16)" % lava_info[0])

    print("\nunknown tints: %s" % sorted(reports["unknown_tint"]))
    print("listed-but-untinted quads: %s" % sorted(reports["listed_no_tint"]))
    print("missing blockstates: %s" % sorted(set(reports["missing_blockstates"])))
    print("blocks w/o model/quad: %s" % sorted(reports["missing_model"]))
    print("missing textures: %s" % sorted(missing_tex))
    if warnings:
        print("warnings (first 20): %s" % warnings[:20])
    print("invis list: %s" % invis_list)

    if len(colors) < 500:
        sys.exit("FATAL: suspiciously few blocks emitted (%d)" % len(colors))
    print("\nSELF-CHECK %s" % ("PASSED" if not warnings_out
                                 else "finished with %d warnings" % len(warnings_out)))


if __name__ == "__main__":
    main()
