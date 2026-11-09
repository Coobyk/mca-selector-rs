//! Port of filter.*: query parser + per-filter matches() for the
//! FilterChunks feature. Grammar: `field cmp value [AND|OR field cmp value]*`
//! with parenthesized groups and `NOT`.

use crate::mca::Chunk;
use crate::nbt::{self, Compound, Tag};
use crate::point::Point2i;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Contains,
    ContainsNot,
    Intersects,
    IntersectsNot,
}

impl Cmp {
    fn parse(s: &str) -> Option<Cmp> {
        Some(match s.to_ascii_lowercase().as_str() {
            "=" | "==" => Cmp::Eq,
            "!=" | "≠" => Cmp::Ne,
            "<" => Cmp::Lt,
            ">" => Cmp::Gt,
            "<=" | "≤" | "=<" => Cmp::Le,
            ">=" | "≥" | "=>" => Cmp::Ge,
            "contains" | "⊃" => Cmp::Contains,
            "!contains" | "contains_not" | "⊅" => Cmp::ContainsNot,
            "intersects" | "∩" => Cmp::Intersects,
            "!intersects" | "intersects_not" => Cmp::IntersectsNot,
            _ => return None,
        })
    }
    pub fn negate(&self) -> Cmp {
        match self {
            Cmp::Eq => Cmp::Ne,
            Cmp::Ne => Cmp::Eq,
            Cmp::Lt => Cmp::Ge,
            Cmp::Gt => Cmp::Le,
            Cmp::Le => Cmp::Gt,
            Cmp::Ge => Cmp::Lt,
            Cmp::Contains => Cmp::ContainsNot,
            Cmp::ContainsNot => Cmp::Contains,
            Cmp::Intersects => Cmp::IntersectsNot,
            Cmp::IntersectsNot => Cmp::Intersects,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Bool {
    And,
    Or,
}

pub enum Expr {
    Leaf {
        negated: bool,
        filter: Filter,
    },
    Group {
        negated: bool,
        op: Bool,
        parts: Vec<Expr>,
    },
}

/// upstream RegionMatcher.MatchType — whether a whole region can be
/// classified without loading it (FULL: every chunk matches,
/// NONE: none matches, PARTIAL: needs per-chunk evaluation).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchType {
    None,
    Partial,
    Full,
}

impl MatchType {
    fn invert(self) -> Self {
        match self {
            MatchType::Full => MatchType::None,
            MatchType::None => MatchType::Full,
            m => m,
        }
    }
}

impl Expr {
    /// AND -> min of parts (NONE dominates), OR -> max (FULL dominates),
    /// negation flips FULL/NONE.
    pub fn matches_region(&self, region: crate::point::Point2i, ctx: &mut FilterCtx) -> MatchType {
        match self {
            Expr::Leaf { negated, filter } => {
                let m = filter.matches_region(region, ctx);
                if *negated { m.invert() } else { m }
            }
            Expr::Group { negated, op, parts } => {
                let m = match op {
                    Bool::And => parts
                        .iter()
                        .map(|p| p.matches_region(region, ctx))
                        .min()
                        .unwrap_or(MatchType::Full),
                    Bool::Or => parts
                        .iter()
                        .map(|p| p.matches_region(region, ctx))
                        .max()
                        .unwrap_or(MatchType::None),
                };
                if *negated { m.invert() } else { m }
            }
        }
    }
}

/// Evaluation context for filters that need filesystem access
/// (Border neighbor lookups, entities dimension, csv selections, player data).
pub struct FilterCtx {
    pub region_dir: Option<std::path::PathBuf>,
    pub entities_dir: Option<std::path::PathBuf>,
    chunk_sets: std::collections::HashMap<i64, Option<crate::selection::ChunkSet>>,
    entities_files: std::collections::HashMap<i64, Option<std::rc::Rc<crate::mca::McaFile>>>,
    csvs: std::collections::HashMap<std::path::PathBuf, Option<crate::selection::Selection>>,
    player_sets: std::collections::HashMap<String, std::collections::HashSet<i64>>,
}

impl FilterCtx {
    pub fn new(
        region_dir: Option<std::path::PathBuf>,
        entities_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            region_dir,
            entities_dir,
            chunk_sets: std::collections::HashMap::new(),
            entities_files: std::collections::HashMap::new(),
            csvs: std::collections::HashMap::new(),
            player_sets: std::collections::HashMap::new(),
        }
    }

    /// preload the chunk set of the current region (avoid a header reload)
    pub fn insert_chunk_set(&mut self, region: i64, cs: crate::selection::ChunkSet) {
        self.chunk_sets.insert(region, Some(cs));
    }

    /// region header chunk set for Border lookups (cached).
    fn region_chunk_set(&mut self, region: i64) -> Option<&crate::selection::ChunkSet> {
        if !self.chunk_sets.contains_key(&region) {
            let cs = self.region_dir.as_ref().and_then(|dir| {
                let rp = crate::point::Point2i::from_long(region);
                let p = dir.join(crate::files::mca_file_name(rp));
                let mut m = crate::mca::McaFile::new(p).ok()?;
                m.load_offsets().ok()?;
                Some(m.offsets_chunk_set())
            });
            self.chunk_sets.insert(region, cs);
        }
        self.chunk_sets.get(&region)?.as_ref()
    }

    /// the loaded entities-dim mca file for a region (cached).
    fn entities_file(&mut self, region: i64) -> Option<std::rc::Rc<crate::mca::McaFile>> {
        if !self.entities_files.contains_key(&region) {
            let f = self.entities_dir.as_ref().and_then(|dir| {
                let rp = crate::point::Point2i::from_long(region);
                let p = dir.join(crate::files::mca_file_name(rp));
                if !p.exists() {
                    return None;
                }
                let mut m = crate::mca::McaFile::new(p).ok()?;
                m.load(false).ok()?;
                Some(std::rc::Rc::new(m))
            });
            self.entities_files.insert(region, f);
        }
        self.entities_files.get(&region)?.clone()
    }

    fn csv_selection(&mut self, path: &std::path::Path) -> Option<&crate::selection::Selection> {
        if !self.csvs.contains_key(path) {
            let sel = crate::selection::Selection::read_csv(path).ok();
            self.csvs.insert(path.to_path_buf(), sel);
        }
        self.csvs.get(path)?.as_ref()
    }

    /// chunk-set of player locations (or spawns) for a playerdata dir+dimension.
    fn player_set(&mut self, dir: &std::path::Path, dimension: &str, spawn: bool) -> &std::collections::HashSet<i64> {
        let key = format!("{}:{}:{}", dir.display(), dimension, spawn);
        if !self.player_sets.contains_key(&key) {
            let set = load_player_chunks(dir, dimension, spawn);
            self.player_sets.insert(key.clone(), set);
        }
        self.player_sets.get(&key).unwrap()
    }
}

impl Expr {
    pub fn matches(&self, c: &Chunk, dv: i32, ctx: &mut FilterCtx) -> bool {
        match self {
            Expr::Leaf { negated, filter } => filter.matches(c, dv, ctx) != *negated,
            Expr::Group { negated, op, parts } => {
                let r = match op {
                    Bool::And => parts.iter().all(|p| p.matches(c, dv, ctx)),
                    Bool::Or => parts.iter().any(|p| p.matches(c, dv, ctx)),
                };
                r != *negated
            }
        }
    }
}

#[derive(Debug)]
pub enum Filter {
    XPos(Cmp, i32),
    ZPos(Cmp, i32),
    YPos(Cmp, i32),
    DataVersion(Cmp, i32),
    InhabitedTime(Cmp, i64),
    LastUpdate(Cmp, i64),
    Timestamp(Cmp, i64),
    Status(Cmp, String),
    Palette(Cmp, Vec<String>),
    Biome(Cmp, Vec<String>),
    Compression(Cmp, i64),
    LightPopulated(Cmp, bool),
    TileEntities(Cmp, i64),
    Structures(Cmp, Vec<String>),
    Circle(Cmp, Vec<(i32, i32, i32)>), // (cx, cz, r) in block coords, any-of
    /// Entities: name list contains/intersects (entity dimension file or
    /// legacy Level.Entities)
    EntityNames(Cmp, Vec<String>),
    /// #Entities: entity count (entities file, falls back to Level.Entities)
    EntityAmount(Cmp, i64),
    /// #ProtoEntities: count of the region chunk's proto `entities` list
    ProtoEntityAmount(Cmp, i64),
    /// Border: count of missing neighbor chunks (9 when status != full)
    Border(Cmp, i64),
    /// Selection: csv file -> chunk-in-selection equality
    SelectionFile(Cmp, std::path::PathBuf),
    /// PlayerLocation / PlayerSpawn: "<playerdata dir>:<dimension>"
    PlayerLocation { cmp: Cmp, dir: std::path::PathBuf, dimension: String, spawn: bool },
    Custom(String, Cmp, Tag),
}

impl Filter {
    /// upstream per-filter matchesRegion (only some filters implement it).
    pub fn matches_region(&self, region: crate::point::Point2i, ctx: &mut FilterCtx) -> MatchType {
        match self {
            Filter::XPos(cmp, v) | Filter::ZPos(cmp, v) => {
                let base = region.region_to_chunk();
                let c = if matches!(self, Filter::XPos(..)) { base.x } else { base.z };
                let mut n = 0;
                for i in 0..32 {
                    if num_cmp(*cmp, (c + i) as i64, *v as i64) {
                        n += 1;
                    }
                }
                match n {
                    0 => MatchType::None,
                    32 => MatchType::Full,
                    _ => MatchType::Partial,
                }
            }
            Filter::SelectionFile(cmp, path) => {
                let Some(sel) = ctx.csv_selection(path).cloned() else {
                    return MatchType::None;
                };
                let n = sel
                    .selected_chunks(region)
                    .map(|c| c.map(|s| s.size()).unwrap_or(1024))
                    .unwrap_or(0);
                match cmp {
                    Cmp::Eq => match n {
                        0 => MatchType::None,
                        1024 => MatchType::Full,
                        _ => MatchType::Partial,
                    },
                    Cmp::Ne => match n {
                        0 => MatchType::Full,
                        1024 => MatchType::None,
                        _ => MatchType::Partial,
                    },
                    _ => MatchType::None,
                }
            }
            Filter::Circle(cmp, circles) => {
                if *cmp != Cmp::Contains {
                    return MatchType::Partial;
                }
                let tl = region.region_to_chunk();
                let corners = [
                    tl,
                    tl.add(0, 31),
                    tl.add(31, 31),
                    tl.add(31, 0),
                ];
                for &(cx, cz, r) in circles {
                    let center = crate::point::Point2i::new(cx, cz).block_to_chunk();
                    // center in this region, or a corner inside the circle
                    if center.chunk_to_region() == region {
                        return MatchType::Partial;
                    }
                    for c in corners {
                        let d2 = ((c.x - center.x) as i64).pow(2) + ((c.z - center.z) as i64).pow(2);
                        if d2 <= (r as i64 / 16).pow(2) {
                            return MatchType::Partial;
                        }
                    }
                    // circle overlaps a region edge (axis extremes inside region)
                    let ext = r / 16;
                    let extremes = [
                        center.add(-ext, 0),
                        center.add(ext, 0),
                        center.add(0, -ext),
                        center.add(0, ext),
                    ];
                    for e in extremes {
                        if e.chunk_to_region() == region {
                            return MatchType::Partial;
                        }
                    }
                }
                MatchType::None
            }
            Filter::PlayerLocation { dir, dimension, spawn, .. } => {
                let set = ctx.player_set(dir, dimension, *spawn).clone();
                if set.iter().any(|&l| crate::point::Point2i::from_long(l).chunk_to_region() == region) {
                    MatchType::Partial
                } else {
                    MatchType::None
                }
            }
            _ => MatchType::Partial,
        }
    }
}

fn num_cmp(cmp: Cmp, a: i64, b: i64) -> bool {
    match cmp {
        Cmp::Eq => a == b,
        Cmp::Ne => a != b,
        Cmp::Lt => a < b,
        Cmp::Gt => a > b,
        Cmp::Le => a <= b,
        Cmp::Ge => a >= b,
        _ => false,
    }
}

fn set_cmp(cmp: Cmp, a: &[String], b: &[String]) -> bool {
    match cmp {
        Cmp::Contains => b.iter().all(|x| a.iter().any(|y| y == x || names_eq(y, x))),
        Cmp::ContainsNot => b.iter().all(|x| !a.iter().any(|y| y == x || names_eq(y, x))),
        Cmp::Intersects => b.iter().any(|x| a.iter().any(|y| y == x || names_eq(y, x))),
        Cmp::IntersectsNot => !b.iter().any(|x| a.iter().any(|y| y == x || names_eq(y, x))),
        Cmp::Eq => b.iter().all(|x| a.iter().any(|y| names_eq(y, x))),
        Cmp::Ne => !b.iter().all(|x| a.iter().any(|y| names_eq(y, x))),
        _ => false,
    }
}

fn names_eq(a: &str, b: &str) -> bool {
    a == b
        || (!a.contains(':') && format!("minecraft:{a}") == b)
        || (!b.contains(':') && a == format!("minecraft:{b}"))
}

fn level<'a>(root: &'a Compound) -> &'a Compound {
    nbt::level_from_root(root).unwrap_or(root)
}

fn get_sections<'a>(data: &'a Compound) -> Option<&'a Vec<Tag>> {
    // both Level.Sections and root.sections
    let l = level(data);
    if let Some(l) = l.get("Sections").and_then(Tag::as_list) {
        return Some(l.1);
    }
    if let Some(l) = data.get("sections").and_then(Tag::as_list) {
        return Some(l.1);
    }
    l.get("sections").and_then(Tag::as_list).map(|t| t.1)
}

fn palette_names(data: &Compound) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(secs) = get_sections(data) {
        for s in secs {
            let Some(sc) = s.as_compound() else { continue };
            let pal = ["Palette", "palette"]
                .iter()
                .find_map(|k| sc.get(*k).and_then(Tag::as_list))
                .or_else(|| {
                    sc.get("block_states")
                        .and_then(Tag::as_compound)
                        .and_then(|b| b.get("palette"))
                        .and_then(Tag::as_list)
                });
            if let Some((_, l)) = pal {
                for e in l {
                    if let Some(name) = palette_name_tag(e) {
                        out.push(name);
                    }
                }
            }
        }
    }
    out
}

fn palette_name_tag(e: &Tag) -> Option<String> {
    match e {
        Tag::Compound(c) => c
            .get("Name")
            .or_else(|| c.get(""))
            .or_else(|| c.get("id"))
            .and_then(|t| t.as_string())
            .map(|s| s.to_string()),
        Tag::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn biome_names(data: &Compound) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(secs) = get_sections(data) {
        for s in secs {
            let Some(sc) = s.as_compound() else { continue };
            let mut biomes = sc.get("biomes").and_then(Tag::as_compound);
            if let Some(pal) = biomes
                .and_then(|b| b.get("palette"))
                .and_then(Tag::as_list)
            {
                for e in pal.1 {
                    if let Some(n) = palette_name_tag(e) {
                        out.push(n);
                    }
                }
            }
        }
    }
    // legacy Biomes byte/int array -> skip name resolution here
    out
}

fn count_list(data: &Compound, keys: &[&str]) -> i64 {
    let mut n = 0;
    for k in keys {
        for c in [Some(data), nbt::level_from_root(data)] {
            if let Some(Some((_, l))) = c.map(|cc| cc.get(*k).and_then(Tag::as_list)) {
                n += l.len() as i64;
            }
        }
    }
    n
}

fn structures(data: &Compound) -> Vec<String> {
    let mut out = Vec::new();
    for c in [Some(data), nbt::level_from_root(data)] {
        let Some(cc) = c else { continue };
        if let Some(st) = cc.get("structures").and_then(Tag::as_compound) {
            if let Some(starts) = st.get("starts").and_then(Tag::as_compound) {
                for (n, v) in starts {
                    let valid = v
                        .as_compound()
                        .and_then(|s| s.get("id"))
                        .and_then(|t| t.as_string())
                        .map(|id| !id.is_empty() && id != "INVALID")
                        .unwrap_or(false);
                    if valid {
                        out.push(n.clone());
                    }
                }
            }
            if let Some(refs) = st.get("References").and_then(Tag::as_compound) {
                for (n, _) in refs {
                    out.push(n.clone());
                }
            }
        }
    }
    out
}

impl Filter {
    pub fn matches(&self, c: &Chunk, dv: i32, ctx: &mut FilterCtx) -> bool {
        let Some(data) = &c.data else { return false };
        let l = level(data);
        match self {
            Filter::XPos(cmp, v) => {
                let x = nbt::int_from(Some(l), "xPos", 0);
                num_cmp(*cmp, x as i64, *v as i64)
            }
            Filter::ZPos(cmp, v) => {
                let z = nbt::int_from(Some(l), "zPos", 0);
                num_cmp(*cmp, z as i64, *v as i64)
            }
            Filter::YPos(cmp, v) => {
                let y = nbt::int_from(Some(l), "yPos", 0);
                num_cmp(*cmp, y as i64, *v as i64)
            }
            Filter::DataVersion(cmp, v) => num_cmp(*cmp, dv as i64, *v as i64),
            Filter::InhabitedTime(cmp, v) => {
                num_cmp(*cmp, nbt::long_from_opt(Some(l), "InhabitedTime").unwrap_or(0), *v)
            }
            Filter::LastUpdate(cmp, v) => {
                num_cmp(*cmp, nbt::long_from_opt(Some(l), "LastUpdate").unwrap_or(0), *v)
            }
            Filter::Timestamp(cmp, v) => num_cmp(*cmp, c.timestamp as i64, *v),
            Filter::Status(cmp, v) => {
                let s = nbt::string_or(Some(l), "Status", "");
                match cmp {
                    Cmp::Eq => names_eq(s, v),
                    Cmp::Ne => !names_eq(s, v),
                    _ => false,
                }
            }
            Filter::Palette(cmp, v) => {
                let names = palette_names(data);
                set_cmp(*cmp, &names, v)
            }
            Filter::Biome(cmp, v) => {
                let names = biome_names(data);
                set_cmp(*cmp, &names, v)
            }
            Filter::Compression(cmp, v) => {
                num_cmp(*cmp, c.compression.to_byte() as i64, *v)
            }
            Filter::LightPopulated(cmp, v) => {
                let lp = l
                    .get("isLightOn")
                    .and_then(|t| t.as_i64())
                    .map(|b| b != 0)
                    .unwrap_or(false);
                match cmp {
                    Cmp::Eq => lp == *v,
                    Cmp::Ne => lp != *v,
                    _ => false,
                }
            }
            Filter::EntityNames(cmp, v) => {
                let ids = entity_ids(c, ctx);
                set_cmp(*cmp, &ids, v)
            }
            Filter::EntityAmount(cmp, v) => {
                let n = entity_count(c, ctx);
                num_cmp(*cmp, n, *v)
            }
            Filter::ProtoEntityAmount(cmp, v) => {
                let n = count_list(data, &["entities", "Entities"]);
                num_cmp(*cmp, n, *v)
            }
            Filter::TileEntities(cmp, v) => {
                let n = count_list(data, &["TileEntities", "block_entities", "tile_entities"]);
                num_cmp(*cmp, n, *v)
            }
            Filter::Structures(cmp, v) => {
                let names = structures(data);
                set_cmp(*cmp, &names, v)
            }
            Filter::Circle(cmp, circles) => {
                let pos = c.absolute_location.chunk_to_block();
                let inside = circles.iter().any(|&(cx, cz, r)| {
                    let dx = pos.x as i64 - cx as i64;
                    let dz = pos.z as i64 - cz as i64;
                    dx * dx + dz * dz <= (r as i64) * (r as i64)
                });
                match cmp {
                    Cmp::Eq | Cmp::Contains => inside,
                    Cmp::Ne | Cmp::ContainsNot => !inside,
                    _ => false,
                }
            }
            Filter::Border(cmp, v) => {
                // 9 when no data or status != full (upstream getNumber)
                let status = nbt::string_or(Some(l), "Status", "");
                let n = if status != "full" {
                    9
                } else {
                    border_missing_neighbors(c, ctx)
                };
                num_cmp(*cmp, n, *v)
            }
            Filter::SelectionFile(cmp, path) => {
                let pos = c.absolute_location;
                let inside = ctx
                    .csv_selection(path)
                    .map(|s| s.is_chunk_selected(pos))
                    .unwrap_or(false);
                match cmp {
                    Cmp::Eq => inside,
                    Cmp::Ne => !inside,
                    _ => false,
                }
            }
            Filter::PlayerLocation { cmp, dir, dimension, spawn } => {
                let inside = ctx
                    .player_set(dir, dimension, *spawn)
                    .contains(&c.absolute_location.as_long());
                match cmp {
                    Cmp::Contains | Cmp::Eq => inside,
                    Cmp::ContainsNot | Cmp::Ne => !inside,
                    _ => false,
                }
            }
            Filter::Custom(path, cmp, val) => {
                let cur = nbt::path_get(data, path);
                match (cur, val) {
                    (Some(t), _) => tag_cmp(*cmp, t, val),
                    (None, _) => matches!(cmp, Cmp::Ne),
                }
            }
        }
    }
}

/// Border: count of missing neighbor chunks (0-8).
fn border_missing_neighbors(c: &Chunk, ctx: &mut FilterCtx) -> i64 {
    let pos = c.absolute_location;
    let mut count = 0i64;
    for (dx, dz) in [
        (0, -1), (1, 0), (0, 1), (-1, 0),
        (-1, -1), (1, -1), (1, 1), (-1, 1),
    ] {
        let n = pos.add(dx, dz);
        let region = n.chunk_to_region().as_long();
        let present = ctx
            .region_chunk_set(region)
            .map(|cs| {
                let rel = n.as_relative_chunk();
                cs.get(rel.z as usize * 32 + rel.x as usize)
            })
            .unwrap_or(false);
        if !present {
            count += 1;
        }
    }
    count
}

/// entities dimension chunk data for the same position as `c` (1.17+).
fn entities_chunk<'m>(c: &Chunk, ctx: &'m mut FilterCtx) -> Option<(std::rc::Rc<crate::mca::McaFile>, usize)> {
    let region = c.absolute_location.chunk_to_region().as_long();
    let m = ctx.entities_file(region)?;
    let rel = c.absolute_location.as_relative_chunk();
    let i = rel.z as usize * 32 + rel.x as usize;
    m.chunks.get(i)?.as_ref()?;
    Some((m, i))
}

/// entity ids: entities-dim chunk `Entities` list, else legacy Level.Entities.
fn entity_ids(c: &Chunk, ctx: &mut FilterCtx) -> Vec<String> {
    if let Some((m, i)) = entities_chunk(c, ctx) {
        if let Some(d) = m.chunks[i].as_ref().and_then(|c| c.data.as_ref()) {
            if let Some(ids) = list_names(d, &["Entities", "entities"], "id") {
                return ids;
            }
        }
    }
    // legacy: region Level.Entities
    let Some(data) = &c.data else { return Vec::new() };
    if let Some(level) = data.get("Level").and_then(Tag::as_compound) {
        if let Some(ids) = list_names(level, &["Entities"], "id") {
            return ids;
        }
    }
    list_names(data, &["Entities"], "id").unwrap_or_default()
}

fn list_names(data: &Compound, keys: &[&str], field: &str) -> Option<Vec<String>> {
    for k in keys {
        if let Some((_, l)) = data.get(*k).and_then(Tag::as_list) {
            return Some(
                l.iter()
                    .filter_map(|e| e.as_compound())
                    .filter_map(|e| e.get(field).and_then(Tag::as_string))
                    .map(|s| s.to_string())
                    .collect(),
            );
        }
    }
    None
}

fn entity_count(c: &Chunk, ctx: &mut FilterCtx) -> i64 {
    entity_ids(c, ctx).len() as i64
}

/// load player chunk positions from a playerdata directory.
/// spawn: SpawnX/SpawnZ/SpawnDimension; else Pos/Dimension.
fn load_player_chunks(
    dir: &std::path::Path,
    dimension: &str,
    spawn: bool,
) -> std::collections::HashSet<i64> {
    use std::collections::HashSet;
    let mut out = HashSet::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".dat") {
            continue;
        }
        let Ok(bytes) = std::fs::read(e.path()) else { continue };
        let root = read_player_dat(&bytes);
        let Some(root) = root else { continue };
        let (x, z, dim) = if spawn {
            let x = root.get("SpawnX").and_then(Tag::as_i64);
            let z = root.get("SpawnZ").and_then(Tag::as_i64);
            let dim = root.get("SpawnDimension");
            match (x, z) {
                (Some(x), Some(z)) => (x, z, dim),
                _ => continue,
            }
        } else {
            let pos = root.get("Pos").and_then(Tag::as_list);
            let dim = root.get("Dimension").or_else(|| root.get("dimension"));
            match pos {
                Some((_, l)) if l.len() >= 3 => {
                    (l[0].as_f64().unwrap_or(0.0) as i64, l[2].as_f64().unwrap_or(0.0) as i64, dim)
                }
                _ => continue,
            }
        };
        if !dimension_matches(dim, dimension) {
            continue;
        }
        let p = crate::point::Point2i::new(x as i32, z as i32);
        out.insert(p.block_to_chunk().as_long());
    }
    out
}

fn read_player_dat(bytes: &[u8]) -> Option<Compound> {
    // player .dat files are gzip-compressed NBT
    let raw = if bytes.starts_with(&[0x1f, 0x8b]) {
        crate::mca::decompress(crate::mca::CompressionType::Gzip, bytes).ok()?
    } else {
        bytes.to_vec()
    };
    let mut r = crate::nbt::NbtReader::new(&raw[..]);
    match r.read().ok()? {
        Tag::Compound(c) => Some(c),
        _ => None,
    }
}

fn dimension_matches(dim: Option<&Tag>, want: &str) -> bool {
    let want_num: Option<i64> = want.parse().ok();
    match dim {
        Some(Tag::Int(v)) => want_num.map(|w| w == *v as i64).unwrap_or(false),
        Some(Tag::String(s)) => {
            let w = match want {
                "0" | "overworld" | "minecraft:overworld" => "minecraft:overworld",
                "-1" | "the_nether" | "nether" | "minecraft:the_nether" => "minecraft:the_nether",
                "1" | "the_end" | "end" | "minecraft:the_end" => "minecraft:the_end",
                other => other,
            };
            s == w || s.strip_prefix("minecraft:") == Some(w.strip_prefix("minecraft:").unwrap_or(w))
        }
        _ => want_num.is_none() && want.is_empty(),
    }
}



fn tag_cmp(cmp: Cmp, a: &Tag, b: &Tag) -> bool {
    match cmp {
        Cmp::Eq => a == b || tag_str(a) == tag_str(b),
        Cmp::Ne => !(a == b || tag_str(a) == tag_str(b)),
        Cmp::Lt | Cmp::Gt | Cmp::Le | Cmp::Ge => {
            let (x, y) = (a.as_f64().unwrap_or(0.0), b.as_f64().unwrap_or(0.0));
            match cmp {
                Cmp::Lt => x < y,
                Cmp::Gt => x > y,
                Cmp::Le => x <= y,
                Cmp::Ge => x >= y,
                _ => false,
            }
        }
        _ => false,
    }
}

fn tag_str(t: &Tag) -> String {
    match t {
        Tag::String(s) => s.clone(),
        Tag::Int(v) => v.to_string(),
        Tag::Long(v) => v.to_string(),
        Tag::Double(v) => v.to_string(),
        Tag::Float(v) => v.to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// parser

struct Tok(Vec<String>);

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            cur.push(c);
            if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                cur.push(c);
            }
            '(' | ')' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            }
            ' ' | '\t' | '\n' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// parse a filter query into an Expr. Supports AND/OR/NOT, parens,
/// `field cmp value`, value lists "a,b,c", quoted strings.
pub fn parse(query: &str) -> Result<Expr, String> {
    let toks = tokenize(query);
    let mut i = 0;
    let expr = parse_expr(&toks, &mut i)?;
    if i != toks.len() {
        return Err(format!("unexpected token: {}", toks[i]));
    }
    Ok(expr)
}

fn parse_expr(t: &[String], i: &mut usize) -> Result<Expr, String> {
    // OR has lowest precedence
    let mut parts = vec![parse_and(t, i)?];
    while *i < t.len() && t[*i].eq_ignore_ascii_case("or") {
        *i += 1;
        parts.push(parse_and(t, i)?);
    }
    if parts.len() == 1 {
        Ok(parts.remove(0))
    } else {
        Ok(Expr::Group {
            negated: false,
            op: Bool::Or,
            parts,
        })
    }
}

fn parse_and(t: &[String], i: &mut usize) -> Result<Expr, String> {
    let mut parts = vec![parse_term(t, i)?];
    while *i < t.len() && t[*i].eq_ignore_ascii_case("and") {
        *i += 1;
        parts.push(parse_term(t, i)?);
    }
    if parts.len() == 1 {
        Ok(parts.remove(0))
    } else {
        Ok(Expr::Group {
            negated: false,
            op: Bool::And,
            parts,
        })
    }
}

fn parse_term(t: &[String], i: &mut usize) -> Result<Expr, String> {
    let mut negated = false;
    while *i < t.len() && (t[*i].eq_ignore_ascii_case("not") || t[*i] == "!") {
        negated = !negated;
        *i += 1;
    }
    if *i < t.len() && t[*i] == "(" {
        *i += 1;
        let mut e = parse_expr(t, i)?;
        if *i >= t.len() || t[*i] != ")" {
            return Err("expected ')'".into());
        }
        *i += 1;
        if negated {
            match &mut e {
                Expr::Leaf { negated: n, .. } | Expr::Group { negated: n, .. } => *n = !*n,
            }
        }
        return Ok(e);
    }
    // field cmp value
    let field = take(t, i).ok_or("expected field name")?;
    let cmp_s = take(t, i).ok_or("expected comparator")?;
    let cmp = Cmp::parse(&cmp_s).ok_or_else(|| format!("invalid comparator {cmp_s}"))?;
    let value = take(t, i).ok_or("expected value")?;
    let filter = make_filter(&field, cmp, &value)?;
    Ok(Expr::Leaf { negated, filter })
}

fn take(t: &[String], i: &mut usize) -> Option<String> {
    if *i < t.len() {
        let v = t[*i].clone();
        *i += 1;
        Some(v)
    } else {
        None
    }
}

fn unquote(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(unquote).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

fn num(s: &str) -> Result<i64, String> {
    unquote(s).parse().map_err(|_| format!("expected number, got {s}"))
}

fn make_filter(field: &str, cmp: Cmp, value: &str) -> Result<Filter, String> {
    let f = field.to_ascii_lowercase();
    Ok(match f.as_str() {
        "xpos" => Filter::XPos(cmp, num(value)? as i32),
        "zpos" => Filter::ZPos(cmp, num(value)? as i32),
        "ypos" => Filter::YPos(cmp, num(value)? as i32),
        "dataversion" => Filter::DataVersion(cmp, num(value)? as i32),
        "inhabitedtime" => Filter::InhabitedTime(
            cmp,
            num(value).or_else(|_| {
                crate::overlay::parse_duration(&unquote(value)).map(|d| d * 20)
            })?,
        ),
        "lastupdate" => Filter::LastUpdate(
            cmp,
            num(value).or_else(|_| {
                crate::overlay::parse_duration(&unquote(value)).map(|d| d * 20)
            })?,
        ),
        "timestamp" => Filter::Timestamp(
            cmp,
            num(value).or_else(|_| {
                crate::overlay::parse_timestamp(&unquote(value)).map(|t| t as i64)
            })?,
        ),
        "status" => Filter::Status(cmp, unquote(value)),
        "palette" => Filter::Palette(cmp, split_list(value)),
        "biome" | "biomes" => Filter::Biome(cmp, split_list(value)),
        "compression" => Filter::Compression(cmp, compression_value(value)?),
        "lightpopulated" => Filter::LightPopulated(cmp, matches!(unquote(value).to_ascii_lowercase().as_str(), "true" | "1" | "yes")),
        "entities" | "entity" => {
            // name list (EntityFilter): contains/intersects on entity ids
            Filter::EntityNames(cmp, entity_names(value)?)
        }
        "#entities" | "entityamount" | "num_entities" => {
            Filter::EntityAmount(cmp, num(value)?)
        }
        "#protoentities" | "protoentities" => {
            Filter::ProtoEntityAmount(cmp, num(value)?)
        }
        "tileentities" | "blockentities" | "#tileentities" => {
            Filter::TileEntities(cmp, num(value)?)
        }
        "border" => Filter::Border(cmp, num(value)?),
        "selection" => {
            let p = std::path::PathBuf::from(unquote(value));
            if !p.is_file() {
                return Err(format!("selection file not found: {}", p.display()));
            }
            Filter::SelectionFile(cmp, p)
        }
        "playerlocation" => player_filter(cmp, value, false)?,
        "playerspawn" => player_filter(cmp, value, true)?,
        "structures" | "structure" => Filter::Structures(cmp, split_list(value)),
        "circle" => {
            // Circle contains "x;z;r[,x2;z2;r2,...]"
            let raw = unquote(value);
            let mut circles = Vec::new();
            for cdef in raw.split(',') {
                let parts: Vec<&str> = cdef.trim().split(';').collect();
                if parts.len() != 3 {
                    return Err(format!("circle expects \"x;z;r\", got \"{cdef}\""));
                }
                let x: i32 = parts[0].trim().parse().map_err(|_| format!("bad circle x in \"{cdef}\""))?;
                let z: i32 = parts[1].trim().parse().map_err(|_| format!("bad circle z in \"{cdef}\""))?;
                let r: i32 = parts[2].trim().parse().map_err(|_| format!("bad circle r in \"{cdef}\""))?;
                circles.push((x, z, r));
            }
            if circles.is_empty() {
                return Err("circle expects at least one \"x;z;r\"".into());
            }
            Filter::Circle(cmp, circles)
        }
        _ => {
            // Custom: arbitrary NBT path
            let val = if let Ok(v) = unquote(value).parse::<i64>() {
                Tag::Long(v)
            } else if let Ok(v) = unquote(value).parse::<f64>() {
                Tag::Double(v)
            } else {
                Tag::String(unquote(value))
            };
            Filter::Custom(field.to_string(), cmp, val)
        }
    })
}

/// CompressionFilter.setFilterValue: enum name (case-insensitive) or byte.
fn compression_value(raw: &str) -> Result<i64, String> {
    let r = unquote(raw).trim().to_string();
    if let Some(c) = crate::mca::CompressionType::from_name(&r) {
        return Ok(c.to_byte() as i8 as i64);
    }
    r.parse::<i64>()
        .map_err(|_| format!("invalid compression type \"{r}\""))
}

/// EntityFilter.setFilterValue: comma-separated names; `foo:`/`foo` -> minecraft:foo
fn entity_names(raw: &str) -> Result<Vec<String>, String> {
    let unq = unquote(raw);
    let names: Vec<String> = unq
        .replace(' ', "")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|n| {
            let n = n.trim_matches('\'').to_string();
            if n.contains(':') {
                n
            } else {
                format!("minecraft:{n}")
            }
        })
        .collect();
    if names.is_empty() {
        return Err("empty entity list".into());
    }
    Ok(names)
}

/// PlayerLocation/Spawn: "<dir>:<dimension>"
fn player_filter(cmp: Cmp, raw: &str, spawn: bool) -> Result<Filter, String> {
    let unq = unquote(raw);
    let (dir, dim) = unq
        .rsplit_once(':')
        .ok_or_else(|| format!("expected \"<playerdata dir>:<dimension>\", got \"{unq}\""))?;
    let dir = std::path::PathBuf::from(dir);
    if !dir.is_dir() {
        return Err(format!("player data directory not found: {}", dir.display()));
    }
    Ok(Filter::PlayerLocation {
        cmp,
        dir,
        dimension: dim.to_string(),
        spawn,
    })
}
