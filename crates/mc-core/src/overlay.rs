//! Port of overlay.Overlay / OverlayType / OverlayPool color grading.
//!
//! Each overlay parses one int per chunk and maps it onto an HSB hue gradient
//! (minHue blue .. maxHue red), producing a 32x32 ARGB image per region.

use crate::chunk::{self, PackedData, PackedSrc};
use crate::nbt::{self, Compound, Tag};
use crate::valuetext;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum OverlayType {
    InhabitedTime,
    Timestamp,
    LastUpdate,
    EntityAmount,
    TileEntityAmount,
    DataVersion,
    AverageHeight,
    BlockAmount,
    Path,
    Script,
}

impl OverlayType {
    /// upstream OverlayType.name
    pub fn name(self) -> &'static str {
        match self {
            Self::InhabitedTime => "InhabitedTime",
            Self::Timestamp => "Timestamp",
            Self::LastUpdate => "LastUpdate",
            Self::EntityAmount => "#Entities",
            Self::TileEntityAmount => "#TileEntities",
            Self::DataVersion => "DataVersion",
            Self::AverageHeight => "AverageHeight",
            Self::BlockAmount => "#Blocks",
            Self::Path => "Path",
            Self::Script => "Script",
        }
    }

    /// all overlay types in upstream OverlayType.values() order.
    pub const VALUES: [OverlayType; 10] = [
        Self::InhabitedTime, Self::Timestamp, Self::LastUpdate,
        Self::EntityAmount, Self::TileEntityAmount, Self::DataVersion,
        Self::AverageHeight, Self::BlockAmount, Self::Path, Self::Script,
    ];

    pub fn by_name(name: &str) -> Option<Self> {
        Self::VALUES
            .into_iter()
            .find(|t| t.name() == name || t.display_name() == name)
    }

    /// Overlay.name() shown to the user
    pub fn display_name(self) -> &'static str {
        match self {
            Self::EntityAmount => "EntityAmount",
            Self::TileEntityAmount => "TileEntityAmount",
            Self::BlockAmount => "Blocks",
            Self::Path => "Custom",
            _ => self.name().trim_start_matches('#'),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum PathNode {
    Name(String),
    Index(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub typ: OverlayType,
    pub active: bool,
    pub min: Option<i32>,
    pub max: Option<i32>,
    pub min_hue: f32,
    pub max_hue: f32,
    pub multi_values: Vec<String>,
    pub path_root: String,
    path: Vec<PathNode>,
    path_size: bool,
    pub raw_multi: String,
    /// raw text the min/max bounds were entered as (upstream Overlay.rawMin/
    /// rawMax) — durations/timestamps serialize back losslessly.
    pub raw_min: String,
    pub raw_max: String,
}

impl Overlay {
    pub fn new(typ: OverlayType) -> Self {
        Self {
            typ,
            active: false,
            min: None,
            max: None,
            min_hue: 0.666_666_7,
            max_hue: 0.0,
            multi_values: Vec::new(),
            path_root: String::new(),
            path: Vec::new(),
            path_size: false,
            raw_multi: String::new(),
            raw_min: String::new(),
            raw_max: String::new(),
        }
    }

    pub fn is_valid(&self) -> bool {
        matches!((self.min, self.max), (Some(a), Some(b)) if a < b)
    }

    /// setMin/setMax: plain int, or duration ("1d2h") for InhabitedTime/LastUpdate
    /// (seconds*20), or timestamp ("yyyy-MM-dd HH:mm:ss") for Timestamp.
    pub fn set_min_str(&mut self, raw: &str) -> bool {
        self.raw_min = raw.to_string();
        self.min = self.parse_bound(raw);
        self.is_valid()
    }

    pub fn set_max_str(&mut self, raw: &str) -> bool {
        self.raw_max = raw.to_string();
        self.max = self.parse_bound(raw);
        self.is_valid()
    }

    fn parse_bound(&self, raw: &str) -> Option<i32> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Ok(v) = raw.parse::<i32>() {
            return Some(v);
        }
        match self.typ {
            OverlayType::InhabitedTime | OverlayType::LastUpdate => {
                let d = valuetext::parse_duration(raw).ok()?;
                let max = i64::from(i32::MAX) / valuetext::TICKS_PER_SECOND;
                if !(0..=max).contains(&d) {
                    return None;
                }
                Some((d * valuetext::TICKS_PER_SECOND) as i32)
            }
            OverlayType::Timestamp => valuetext::parse_timestamp(raw).ok(),
            _ => None,
        }
    }

    /// setMultiValuesString — BlockAmount: block names; Path: root/a/b/[i]/x.size
    pub fn set_multi_values_str(&mut self, raw: &str) -> bool {
        self.raw_multi = raw.to_string();
        match self.typ {
            OverlayType::BlockAmount => {
                self.multi_values = parse_block_names(raw);
                !self.multi_values.is_empty()
            }
            OverlayType::Path => self.set_path(raw),
            _ => {
                self.multi_values = Vec::new();
                true
            }
        }
    }

    fn set_path(&mut self, raw: &str) -> bool {
        self.path.clear();
        self.path_size = false;
        self.path_root.clear();
        let elements: Vec<String> = valuetext::split_escaped(raw, '/', '\\');
        self.multi_values = elements.clone();
        if elements.len() < 2 {
            self.multi_values.clear();
            return false;
        }
        self.path_root = elements[0].clone();
        for e in &elements[1..elements.len() - 1] {
            if !self.parse_element(e) {
                self.multi_values.clear();
                return false;
            }
        }
        let last = &elements[elements.len() - 1];
        let last_parts = valuetext::split_escaped(last, '.', '\\');
        if last_parts.len() > 1 && last_parts.last().map(|s| s.as_str()) == Some("size") {
            self.path_size = true;
            let prefix = &last[..last.len() - 5];
            self.parse_element(prefix)
        } else {
            self.parse_element(last)
        }
    }

    fn parse_element(&mut self, e: &str) -> bool {
        if let Some(idx) = e.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            match idx.parse::<usize>() {
                Ok(i) => self.path.push(PathNode::Index(i)),
                Err(_) => {
                    self.multi_values.clear();
                    return false;
                }
            }
        } else {
            self.path.push(PathNode::Name(e.to_string()));
        }
        true
    }

    /// parseValue — one int per chunk. `data` is the region chunk's NBT,
    /// `timestamp` its region timestamp, `dv` the data version.
    pub fn parse_value(
        &self,
        data: &Compound,
        poi: Option<&Compound>,
        entities: Option<&Compound>,
        timestamp: i32,
        dv: i32,
    ) -> i32 {
        match self.typ {
            OverlayType::InhabitedTime => {
                nbt::long_from_opt(Some(data), "InhabitedTime").unwrap_or(0) as i32
            }
            OverlayType::Timestamp => timestamp,
            OverlayType::LastUpdate => {
                nbt::long_from_opt(Some(data), "LastUpdate").unwrap_or(0) as i32
            }
            OverlayType::EntityAmount => chunk::list_size(data, chunk::ENTITY_LIST_KEYS) as i32,
            OverlayType::TileEntityAmount => {
                chunk::list_size(data, chunk::TILE_ENTITY_LIST_KEYS) as i32
            }
            OverlayType::DataVersion => dv,
            OverlayType::AverageHeight => average_height(data, dv),
            OverlayType::BlockAmount => block_amount(data, &self.multi_values),
            OverlayType::Path => {
                let d = match self.path_root.as_str() {
                    "region" => data,
                    "poi" => match poi { Some(d) => d, None => return 0 },
                    "entities" => match entities { Some(d) => d, None => return 0 },
                    _ => return 0,
                };
                self.path_value(d)
            }
            OverlayType::Script => 0,
        }
    }

    fn path_value(&self, data: &Compound) -> i32 {
        let root = Tag::Compound(data.clone());
        let mut cur = &root;
        for node in &self.path {
            match (node, cur) {
                (PathNode::Name(n), Tag::Compound(c)) => match c.get(n.as_str()) {
                    Some(t) => cur = t,
                    None => return 0,
                },
                (PathNode::Index(i), Tag::List(_, l)) => match l.get(*i) {
                    Some(t) => cur = t,
                    None => return 0,
                },
                _ => return 0,
            }
        }
        match cur {
            Tag::Compound(c) if self.path_size => c.len() as i32,
            Tag::List(_, l) if self.path_size => l.len() as i32,
            t if !self.path_size => t.as_i64().unwrap_or(0) as i32,
            _ => 0,
        }
    }

    /// OverlayPool.getColorGrade
    pub fn color_grade(&self, value: i32) -> u32 {
        let (min, max) = match (self.min, self.max) {
            (Some(a), Some(b)) => (a, b),
            _ => return 0xFF00_0000,
        };
        if value <= min {
            return hsb_to_rgb(self.min_hue, 1.0, 1.0);
        }
        if value >= max {
            return hsb_to_rgb(self.max_hue, 1.0, 1.0);
        }
        let pct = (value - min) as f32 / (max - min) as f32;
        hsb_to_rgb(self.min_hue + pct * (self.max_hue - self.min_hue), 1.0, 1.0)
    }

    /// 32x32 ARGB overlay image for a region (upstream draws at alpha 0.5).
    pub fn image(&self, data: &[i32; 1024]) -> Vec<u32> {
        data.iter()
            .map(|&v| (self.color_grade(v) & 0x00FF_FFFF) | 0x8000_0000)
            .collect()
    }
}

/// OverlayPool: parse one value per chunk of a loaded region.
pub fn region_overlay_data(
    m: &crate::mca::McaFile,
    ov: &Overlay,
    poi: Option<&crate::mca::McaFile>,
    entities: Option<&crate::mca::McaFile>,
) -> [i32; 1024] {
    let mut out = [0i32; 1024];
    for (i, c) in m.chunks.iter().enumerate() {
        let Some(c) = c else { continue };
        let Some(d) = &c.data else { continue };
        let pd = poi.and_then(|f| f.chunks[i].as_ref()).and_then(|ch| ch.data.as_ref());
        let ed = entities.and_then(|f| f.chunks[i].as_ref()).and_then(|ch| ch.data.as_ref());
        let dv = nbt::data_version(Some(d));
        out[i] = ov.parse_value(d, pd, ed, c.timestamp, dv);
    }
    out
}

struct Sec<'a> {
    y: i32,
    palette: Vec<&'a str>,
    data: PackedData<'a>,
}

fn sections_of<'a>(data: &'a Compound, dv: i32) -> Vec<Sec<'a>> {
    let Some((list, _)) = chunk::sections(data) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for t in list {
        let Some(s) = t.as_compound() else { continue };
        let Some(plist) = chunk::block_palette(s) else { continue };
        let Some(d) = s
            .get("block_states")
            .and_then(Tag::as_compound)
            .and_then(|b| b.get("data"))
            .or_else(|| s.get("BlockStates"))
            .and_then(Tag::as_bytes)
        else {
            continue;
        };
        let palette: Vec<&str> = plist.iter().map(chunk::palette_entry_name).collect();
        out.push(Sec {
            y: s.get("Y").and_then(Tag::as_i64).map(|v| v as i32).unwrap_or(-1),
            palette,
            data: PackedData::block_states(PackedSrc::Raw(d), dv),
        });
    }
    out
}

fn block_at<'a>(s: &'a Sec<'a>, index: usize) -> &'a str {
    if s.palette.is_empty() {
        return "";
    }
    if s.data.bits == 0 {
        return s.palette.first().copied().unwrap_or("");
    }
    let pal_idx = s.data.get(index) as usize;
    s.palette.get(pal_idx).copied().unwrap_or("")
}

/// ChunkFilter.Blocks.getAverageHeight: top-down scan per column, sum of
/// (sectionY*16 + cy) at first non-empty block / 256.
fn average_height(data: &Compound, dv: i32) -> i32 {
    let mut secs = sections_of(data, dv);
    secs.sort_by(|a, b| b.y.cmp(&a.y));
    if secs.is_empty() {
        return 0;
    }
    let mut total = 0i64;
    for cx in 0..16 {
        'z: for cz in 0..16 {
            for s in &secs {
                for cy in (0..16).rev() {
                    let index = chunk::location_to_index(cx, cy, cz);
                    if !chunk::is_air_name(block_at(s, index)) {
                        total += s.y as i64 * 16 + cy as i64;
                        continue 'z;
                    }
                }
            }
        }
    }
    (total / 256) as i32
}

/// ChunkFilter.Blocks.getBlockAmount: count blocks matching names.
fn block_amount(data: &Compound, blocks: &[String]) -> i32 {
    // works for any dv via raw NBT (name compare on palette entries)
    let dv = nbt::data_version(Some(data));
    let secs = sections_of(data, dv);
    let mut result = 0i64;
    for s in &secs {
        let mut hit = vec![false; s.palette.len()];
        for (i, p) in s.palette.iter().enumerate() {
            if blocks.iter().any(|b| valuetext::names_eq(p, b)) {
                hit[i] = true;
            }
        }
        if !hit.iter().any(|h| *h) {
            continue;
        }
        for k in 0..4096 {
            if s.data.bits == 0 {
                // single-palette section: all blocks are palette[0]
                if !s.palette.is_empty() && hit[0] {
                    result += 1;
                }
                break;
            }
            let pal_idx = s.data.get(k) as usize;
            if hit.get(pal_idx).copied().unwrap_or(false) {
                result += 1;
            }
        }
    }
    result as i32
}

/// BlockRegistry.parseBlockNames: comma/space/semicolon separated names.
fn parse_block_names(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == ';' || c == ' ' || c == '\n' || c == '\t')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(valuetext::normalize_name)
        .collect()
}

/// java.awt.Color.HSBtoRGB — returns 0xFFRRGGBB.
pub fn hsb_to_rgb(hue: f32, saturation: f32, brightness: f32) -> u32 {
    let (rr, gg, bb) = if saturation == 0.0 {
        (brightness, brightness, brightness)
    } else {
        let h = (hue - hue.floor()) * 6.0;
        let f = h - h.floor();
        let p = brightness * (1.0 - saturation);
        let q = brightness * (1.0 - saturation * f);
        let t = brightness * (1.0 - saturation * (1.0 - f));
        match h as i32 {
            0 => (brightness, t, p),
            1 => (q, brightness, p),
            2 => (p, brightness, t),
            3 => (p, q, brightness),
            4 => (t, p, brightness),
            5 => (brightness, p, q),
            _ => (0.0, 0.0, 0.0),
        }
    };
    let r = (rr * 255.0 + 0.5) as u32;
    let g = (gg * 255.0 + 0.5) as u32;
    let b = (bb * 255.0 + 0.5) as u32;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

/// parse a single overlay spec line: `Type min max [data]`
/// e.g. `InhabitedTime 0 1000`, `Blocks 0 500 minecraft:stone`, `Custom 0 10 region/sections.size`
pub fn parse_overlay_line(line: &str) -> Result<Overlay, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty".into());
    }
    let mut it = line.splitn(4, char::is_whitespace).map(str::trim);
    let type_name = it.next().ok_or("missing type")?;
    let typ = OverlayType::by_name(type_name)
        .ok_or_else(|| format!("invalid overlay type \"{type_name}\""))?;
    let min = it.next().ok_or_else(|| format!("missing mandatory minimum value for \"{type_name}\" overlay"))?;
    let max = it.next().ok_or_else(|| format!("missing mandatory maximum value for \"{type_name}\" overlay"))?;
    let data = it.next().unwrap_or("");
    let mut o = Overlay::new(typ);
    o.set_min_str(min);
    o.set_max_str(max);
    o.set_multi_values_str(data);
    if !o.is_valid() {
        return Err(format!("failed to parse values for {type_name} overlay"));
    }
    o.active = true;
    Ok(o)
}

/// serialize one overlay to a single config line.
///
/// Format: `Type min max [data] ;min_hue=F;max_hue=F;active=B` — the
/// ` ;`-separated extension section carries the fields the plain
/// `Type min max [data]` line can't express (hue gradient, active flag).
/// Empty bounds are written as `-` so the positional layout is preserved.
pub fn serialize_overlay_line(o: &Overlay) -> String {
    let mut line = format!(
        "{} {} {}",
        o.typ.display_name(),
        if o.raw_min.is_empty() { "-" } else { o.raw_min.as_str() },
        if o.raw_max.is_empty() { "-" } else { o.raw_max.as_str() },
    );
    if !o.raw_multi.is_empty() {
        line.push(' ');
        line.push_str(&o.raw_multi);
    }
    line.push_str(&format!(
        " ;min_hue={};max_hue={};active={}",
        o.min_hue, o.max_hue, o.active
    ));
    line
}

/// parse a config line produced by [`serialize_overlay_line`].
///
/// Tolerant variant of [`parse_overlay_line`]: missing or unparseable bounds
/// (`-`/empty) produce a non-drawing overlay instead of an error, and the
/// optional ` ;key=value` extension restores hue range + active flag. A line
/// without the extension behaves like before, except `active` defaults to
/// false (config lines carry it explicitly).
pub fn parse_config_line(line: &str) -> Result<Overlay, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty".into());
    }
    // the extension section starts at the first " ;" whose remainder is a
    // pure `key=value;...` list — a " ;" inside the data value (e.g. block
    // lists) does not look like one and stays part of the base line.
    let (base, ext) = match line.find(" ;") {
        Some(i)
            if line[i + 2..]
                .split(';')
                .all(|t| t.trim().contains('=')) =>
        {
            (&line[..i], &line[i + 2..])
        }
        _ => (line, ""),
    };
    let mut it = base.splitn(4, char::is_whitespace).map(str::trim);
    let type_name = it.next().ok_or("missing type")?;
    let typ = OverlayType::by_name(type_name)
        .ok_or_else(|| format!("invalid overlay type \"{type_name}\""))?;
    let min = it.next().unwrap_or("");
    let max = it.next().unwrap_or("");
    let data = it.next().unwrap_or("");
    let mut o = Overlay::new(typ);
    if !min.is_empty() && min != "-" {
        o.set_min_str(min);
    }
    if !max.is_empty() && max != "-" {
        o.set_max_str(max);
    }
    o.set_multi_values_str(data);
    o.active = false;
    for kv in ext.split(';') {
        let Some((k, v)) = kv.trim().split_once('=') else { continue };
        match k.trim() {
            "min_hue" => {
                if let Ok(f) = v.trim().parse::<f32>() {
                    o.min_hue = f;
                }
            }
            "max_hue" => {
                if let Ok(f) = v.trim().parse::<f32>() {
                    o.max_hue = f;
                }
            }
            "active" => o.active = matches!(v.trim(), "true" | "1"),
            _ => {}
        }
    }
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::valuetext::{parse_duration, parse_timestamp};

    #[test]
    fn color_grade_clamps_and_interpolates() {
        let mut o = Overlay::new(OverlayType::DataVersion);
        o.set_min_str("0");
        o.set_max_str("100");
        assert!(o.is_valid());
        // <= min -> minHue blue (2/3), >= max -> maxHue red (0)
        assert_eq!(o.color_grade(-5), hsb_to_rgb(0.666_666_7, 1.0, 1.0));
        assert_eq!(o.color_grade(150), hsb_to_rgb(0.0, 1.0, 1.0));
        // midpoint hue = 1/3 -> green
        let mid = o.color_grade(50);
        assert_eq!(mid, hsb_to_rgb(0.333_333_34, 1.0, 1.0));
        // green: r=0,g=255,b=0
        assert_eq!(mid & 0x00FF_FFFF, 0x0000_FF00);
    }

    #[test]
    fn parse_line_block_amount() {
        let o = parse_overlay_line("Blocks 0 500 minecraft:stone,dirt").unwrap();
        assert_eq!(o.typ, OverlayType::BlockAmount);
        assert_eq!(o.min, Some(0));
        assert_eq!(o.max, Some(500));
        assert_eq!(
            o.multi_values,
            vec!["minecraft:stone".to_string(), "minecraft:dirt".to_string()]
        );
        assert!(o.is_valid() && o.active);
    }

    #[test]
    fn parse_line_path_overlay() {
        let o = parse_overlay_line("Custom 0 10 region/sections.size").unwrap();
        assert_eq!(o.typ, OverlayType::Path);
        assert!(o.path_size);
        assert_eq!(o.path_root, "region");
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert_eq!(parse_duration("1d2h30min").unwrap(), 86400 + 7200 + 1800);
        assert!(parse_duration("bogus").is_err());
        // InhabitedTime bound: duration -> ticks
        let mut o = Overlay::new(OverlayType::InhabitedTime);
        o.set_min_str("0");
        o.set_max_str("1h");
        assert_eq!(o.max, Some(72000));
    }

    #[test]
    fn timestamp_parsing() {
        // 1970-01-02 00:00:00 UTC -> 86400
        assert_eq!(parse_timestamp("1970-01-02").unwrap(), 86400);
        assert_eq!(parse_timestamp("1970-01-01 01:00:00").unwrap(), 3600);
    }

    #[test]
    fn invalid_min_max_rejected() {
        assert!(parse_overlay_line("DataVersion 100 0").is_err());
        assert!(parse_overlay_line("Bogus 0 10").is_err());
    }
}
