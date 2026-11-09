//! Port of overlay.Overlay / OverlayType / OverlayPool color grading.
//!
//! Each overlay parses one int per chunk and maps it onto an HSB hue gradient
//! (minHue blue .. maxHue red), producing a 32x32 ARGB image per region.

use crate::nbt::{self, Compound, Tag};
use crate::version;

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

    pub fn by_name(name: &str) -> Option<Self> {
        [
            Self::InhabitedTime, Self::Timestamp, Self::LastUpdate,
            Self::EntityAmount, Self::TileEntityAmount, Self::DataVersion,
            Self::AverageHeight, Self::BlockAmount, Self::Path, Self::Script,
        ]
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
        }
    }

    pub fn is_valid(&self) -> bool {
        matches!((self.min, self.max), (Some(a), Some(b)) if a < b)
    }

    

    /// setMin/setMax: plain int, or duration ("1d2h") for InhabitedTime/LastUpdate
    /// (seconds*20), or timestamp ("yyyy-MM-dd HH:mm:ss") for Timestamp.
    pub fn set_min_str(&mut self, raw: &str) -> bool {
        self.min = self.parse_bound(raw);
        self.is_valid()
    }

    pub fn set_max_str(&mut self, raw: &str) -> bool {
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
                let d = parse_duration(raw).ok()?;
                let max = i64::from(i32::MAX) / 20;
                if !(0..=max).contains(&d) {
                    return None;
                }
                Some((d * 20) as i32)
            }
            OverlayType::Timestamp => parse_timestamp(raw).ok(),
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
        let elements: Vec<String> = split_escaped(raw, '/', '\\');
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
        let last_parts = split_escaped(last, '.', '\\');
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
            OverlayType::EntityAmount => list_size(data, &["entities", "Entities"]),
            OverlayType::TileEntityAmount => {
                list_size(data, &["block_entities", "tile_entities", "TileEntities"])
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

fn list_size(data: &Compound, keys: &[&str]) -> i32 {
    let level = data.get("Level").and_then(Tag::as_compound);
    for k in keys {
        for root in [Some(data), level] {
            if let Some(l) = root.and_then(|r| r.get(*k)).and_then(Tag::as_list) {
                return l.1.len() as i32;
            }
        }
    }
    0
}



struct Sec {
    y: i32,
    palette: Vec<String>,
    data: Vec<u8>,
    bits: usize,
    ipl: usize,
}

fn sections_of(data: &Compound, format: version::ChunkFormat) -> Vec<Sec> {
    let modern = version::is_modern(format);
    let root = if modern {
        Some(data)
    } else {
        nbt::level_from_root(data)
    };
    let key = if modern { "sections" } else { "Sections" };
    let Some((_, list)) = nbt::sections_from(root, key) else { return Vec::new() };
    let mut out = Vec::new();
    for t in list {
        let Some(s) = t.as_compound() else { continue };
        let (palette_raw, data_raw) = if modern {
            let bs = s.get("block_states").and_then(Tag::as_compound);
            (
                bs.and_then(|b| b.get("palette")).and_then(Tag::as_list),
                bs.and_then(|b| b.get("data")).and_then(Tag::as_bytes),
            )
        } else {
            (
                s.get("Palette").and_then(Tag::as_list),
                s.get("BlockStates").and_then(Tag::as_bytes),
            )
        };
        let Some((_, plist)) = palette_raw else { continue };
        let Some(d) = data_raw else { continue };
        let palette: Vec<String> = plist
            .iter()
            .map(|e| version::palette_name(Some(e), format).to_string())
            .collect();
        let bits = d.len() >> 9;
        let ipl = if bits == 0 { 1 } else { 64 / bits };
        out.push(Sec {
            y: s.get("Y").and_then(Tag::as_i64).map(|v| v as i32).unwrap_or(-1),
            palette,
            data: d.to_vec(),
            bits,
            ipl,
        });
    }
    out
}

fn block_at<'a>(s: &'a Sec, index: usize) -> &'a str {
    if s.palette.is_empty() {
        return "";
    }
    if s.bits == 0 || s.data.is_empty() {
        return s.palette.first().map(|p| p.as_str()).unwrap_or("");
    }
    let pal_idx = version::packed_get(Some(&s.data), index, s.bits, s.ipl) as usize;
    s.palette.get(pal_idx).map(|p| p.as_str()).unwrap_or("")
}

fn name_is_empty(name: &str) -> bool {
    // upstream isEmpty: air/cave_air/barrier/structure_void (no Properties —
    // palette_name already strips them, so we conservatively treat plain-name
    // matches as empty)
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:barrier" | "minecraft:structure_void"
    )
}

/// ChunkFilter.Blocks.getAverageHeight: top-down scan per column, sum of
/// (sectionY*16 + cy) at first non-empty block / 256.
fn average_height(data: &Compound, dv: i32) -> i32 {
    let format = version::chunk_format(dv);
    let mut secs = sections_of(data, format);
    secs.sort_by(|a, b| b.y.cmp(&a.y));
    if secs.is_empty() {
        return 0;
    }
    let mut total = 0i64;
    for cx in 0..16 {
        'z: for cz in 0..16 {
            for s in &secs {
                for cy in (0..16).rev() {
                    let index = cy * 256 + cz * 16 + cx;
                    if !name_is_empty(block_at(s, index)) {
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
    let format = version::chunk_format(dv);
    let secs = sections_of(data, format);
    let mut result = 0i64;
    for s in &secs {
        let mut hit = vec![false; s.palette.len()];
        for (i, p) in s.palette.iter().enumerate() {
            if blocks.iter().any(|b| names_eq(p, b)) {
                hit[i] = true;
            }
        }
        if !hit.iter().any(|h| *h) {
            continue;
        }
        for k in 0..4096 {
            if s.bits == 0 || s.data.is_empty() {
                // single-palette section: all blocks are palette[0]
                if !s.palette.is_empty() && hit[0] {
                    result += 1;
                }
                break;
            }
            let pal_idx = version::packed_get(Some(&s.data), k, s.bits, s.ipl) as usize;
            if hit.get(pal_idx).copied().unwrap_or(false) {
                result += 1;
            }
        }
    }
    result as i32
}

fn names_eq(a: &str, b: &str) -> bool {
    let a = a.strip_prefix("minecraft:").unwrap_or(a);
    let b = b.strip_prefix("minecraft:").unwrap_or(b);
    a == b
}

/// BlockRegistry.parseBlockNames: comma/space/semicolon separated names.
fn parse_block_names(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == ';' || c == ' ' || c == '\n' || c == '\t')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.contains(':') {
                s.to_string()
            } else {
                format!("minecraft:{s}")
            }
        })
        .collect()
}

/// TextHelper.splitWithEscaping
fn split_escaped(s: &str, sep: char, esc: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == esc {
            if let Some(n) = chars.next() {
                cur.push(n);
            }
        } else if c == sep {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// TextHelper.parseDuration — returns seconds. e.g. "1d", "2h30m".
pub(crate) fn parse_duration(d: &str) -> Result<i64, String> {
    const UNITS: &[(&[&str], i64)] = &[
        (&["years", "year", "y"], 31536000),
        (&["months", "month"], 2592000),
        (&["days", "day", "d"], 86400),
        (&["hours", "hour", "h"], 3600),
        (&["minutes", "minute", "mins", "min"], 60),
        (&["seconds", "second", "secs", "sec", "s"], 1),
    ];
    let mut rest = d.trim().to_string();
    let mut total = 0i64;
    let mut matched_any = false;
    while !rest.is_empty() {
        let rest2 = rest.trim_start().to_string();
        rest = rest2;
        if rest.is_empty() {
            break;
        }
        // read leading number
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return Err(format!("invalid element in duration string: {rest}"));
        }
        let num: i64 = digits.parse().map_err(|_| rest.clone())?;
        let after: String = rest.chars().skip(digits.len()).collect();
        let unit_str: String = after
            .chars()
            .take_while(|c| c.is_alphabetic())
            .collect();
        if unit_str.is_empty() {
            return Err(format!("invalid element in duration string: {rest}"));
        }
        let mut matched = false;
        for (names, mult) in UNITS {
            if names.iter().any(|n| *n == unit_str) {
                total += num * mult;
                matched = true;
                matched_any = true;
                break;
            }
        }
        if !matched {
            return Err(format!("invalid element in duration string: {rest}"));
        }
        rest = after.chars().skip(unit_str.len()).collect();
    }
    if !matched_any {
        return Err(format!("could not parse anything from duration string: {d}"));
    }
    Ok(total)
}

/// TextHelper.parseTimestamp — "yyyy-MM-dd[ HH[:mm[:ss]]]" → epoch seconds.
pub(crate) fn parse_timestamp(t: &str) -> Result<i32, String> {
    let t = t.trim();
    let (date, time) = match t.split_once(' ') {
        Some((d, tm)) => (d, tm),
        None => (t, "00:00:00"),
    };
    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 {
        return Err("could not parse date time".into());
    }
    let y: i64 = dp[0].parse().map_err(|_| "bad year".to_string())?;
    let mo: i64 = dp[1].parse().map_err(|_| "bad month".to_string())?;
    let da: i64 = dp[2].parse().map_err(|_| "bad day".to_string())?;
    let tp: Vec<&str> = time.split(':').collect();
    let h: i64 = tp.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mi: i64 = tp.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let se: i64 = tp.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    // days since epoch (civil algorithm)
    let days = days_from_civil(y, mo, da);
    if days < 0 {
        return Err("could not parse date time".into());
    }
    Ok((days * 86400 + h * 3600 + mi * 60 + se) as i32)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// java.awt.Color.HSBtoRGB — returns 0xFFRRGGBB.
pub fn hsb_to_rgb(hue: f32, saturation: f32, brightness: f32) -> u32 {
    let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
    if saturation == 0.0 {
        r = (brightness * 255.0 + 0.5) as u32;
        g = r;
        b = r;
    } else {
        let h = (hue - hue.floor()) * 6.0;
        let f = h - h.floor();
        let p = brightness * (1.0 - saturation);
        let q = brightness * (1.0 - saturation * f);
        let t = brightness * (1.0 - saturation * (1.0 - f));
        let (rr, gg, bb) = match h as i32 {
            0 => (brightness, t, p),
            1 => (q, brightness, p),
            2 => (p, brightness, t),
            3 => (p, q, brightness),
            4 => (t, p, brightness),
            5 => (brightness, p, q),
            _ => (0.0, 0.0, 0.0),
        };
        r = (rr * 255.0 + 0.5) as u32;
        g = (gg * 255.0 + 0.5) as u32;
        b = (bb * 255.0 + 0.5) as u32;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
