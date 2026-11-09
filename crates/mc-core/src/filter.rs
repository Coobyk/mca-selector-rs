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

impl Expr {
    pub fn matches(&self, c: &Chunk, dv: i32) -> bool {
        match self {
            Expr::Leaf { negated, filter } => filter.matches(c, dv) != *negated,
            Expr::Group { negated, op, parts } => {
                let r = match op {
                    Bool::And => parts.iter().all(|p| p.matches(c, dv)),
                    Bool::Or => parts.iter().any(|p| p.matches(c, dv)),
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
    Entities(Cmp, i64),
    TileEntities(Cmp, i64),
    Structures(Cmp, Vec<String>),
    Circle(Cmp, f64, f64, f64), // cx, cz, radius (block coords)
    Custom(String, Cmp, Tag),
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
            for key in ["Palette", "palette"] {
                let mut pal = sc.get(key).and_then(Tag::as_list);
                if pal.is_none() {
                    pal = sc
                        .get("block_states")
                        .and_then(Tag::as_compound)
                        .and_then(|b| b.get("palette"))
                        .and_then(Tag::as_list);
                }
                if let Some((_, l)) = pal {
                    for e in l {
                        if let Some(name) = palette_name_tag(e) {
                            out.push(name);
                        }
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
    pub fn matches(&self, c: &Chunk, dv: i32) -> bool {
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
            Filter::Entities(cmp, v) => {
                let n = count_list(data, &["Entities", "entities", "block_entities"]);
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
            Filter::Circle(cmp, cx, cz, r) => {
                let cpos = c.absolute_location.chunk_to_block();
                let dist = ((cpos.x as f64 - cx).powi(2) + (cpos.z as f64 - cz).powi(2)).sqrt();
                num_cmp(*cmp, dist as i64, *r as i64)
            }
            Filter::Custom(path, cmp, val) => {
                let cur = lookup_path(data, path);
                match (cur, val) {
                    (Some(t), _) => tag_cmp(*cmp, t, val),
                    (None, _) => matches!(cmp, Cmp::Ne),
                }
            }
        }
    }
}

fn lookup_path<'a>(data: &'a Compound, path: &str) -> Option<&'a Tag> {
    let mut cur: Option<&Tag> = None;
    let mut c: Option<&Compound> = Some(data);
    for p in path.split('.') {
        cur = c.and_then(|cc| cc.get(p));
        match cur {
            Some(Tag::Compound(cc)) => c = Some(cc),
            Some(t) => return Some(t),
            None => return None,
        }
    }
    cur
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
        "inhabitedtime" => Filter::InhabitedTime(cmp, num(value)?),
        "lastupdate" => Filter::LastUpdate(cmp, num(value)?),
        "timestamp" => Filter::Timestamp(cmp, num(value)?),
        "status" => Filter::Status(cmp, unquote(value)),
        "palette" => Filter::Palette(cmp, split_list(value)),
        "biome" | "biomes" => Filter::Biome(cmp, split_list(value)),
        "compression" => Filter::Compression(cmp, num(value)?),
        "lightpopulated" => Filter::LightPopulated(cmp, matches!(unquote(value).to_ascii_lowercase().as_str(), "true" | "1" | "yes")),
        "entities" | "entity" => Filter::Entities(cmp, num(value)?),
        "tileentities" | "blockentities" => Filter::TileEntities(cmp, num(value)?),
        "structures" | "structure" => Filter::Structures(cmp, split_list(value)),
        "circle" => {
            // circle intersects "x,z,r" or circle contains
            let parts: Vec<f64> = split_list(value)
                .iter()
                .filter_map(|p| p.parse::<f64>().ok())
                .collect();
            if parts.len() != 3 {
                return Err(format!("circle expects \"x,z,r\", got {value}"));
            }
            Filter::Circle(cmp, parts[0], parts[1], parts[2])
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
