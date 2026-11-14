//! Port of `net.querz.mcaselector.changer` (FieldType / Field / ChangeParser)
//! and the per-field mutation logic upstream dispatches through
//! `version.ChunkFilter` implementations.
//!
//! Deviations from upstream:
//! - `Custom` is an NBT-path assignment (`path = SNBT`) instead of a Groovy
//!   script; `*` path segments iterate list elements.
//! - `Script` and `FixStructureReferences` are not implemented (Groovy-only /
//!   headless-only upstream).
//! - `Timestamp` and `Compression` mutate `Chunk` metadata directly (upstream
//!   writes MCA header timestamps / per-chunk compression fields), which is
//!   behaviorally equivalent.
//! - Upstream `ReplaceBlocks` matches source names via regex; we compare
//!   literally.
//! - Upstream crashes on some malformed chunks (missing status in FixStatus,
//!   palette without data in ReplaceBlocks); we skip those cases instead.
//! - The 2834-2843 heightmap reader uses upstream's transitional flat
//!   `palette`/`block_states` section layout, including its quirks.
//! - `LightPopulated` replicates upstream's `putLong` quirk for the legacy
//!   `LightPopulated` / `isLightOn` tags.

use crate::chunk::{self, ChunkRefs, PackedData, PackedSrc};
use crate::mca::CompressionType;
use crate::nbt::{self, Compound, Tag};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;



// ---------------------------------------------------------------- FieldType

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldType {
    DataVersion,
    InhabitedTime,
    Timestamp,
    Compression,
    LastUpdate,
    Status,
    Biome,
    ReplaceBlocks,
    DeleteEntities,
    DeleteSections,
    FixStatus,
    FixHeightmaps,
    DeleteStructureReference,
    PreventRetrogen,
    ForceBlend,
    RemoveLightingInfo,
    LightPopulated,
    Custom,
}

impl FieldType {
    pub const VALUES: [FieldType; 18] = [
        Self::DataVersion,
        Self::InhabitedTime,
        Self::Timestamp,
        Self::Compression,
        Self::LastUpdate,
        Self::Status,
        Self::Biome,
        Self::ReplaceBlocks,
        Self::DeleteEntities,
        Self::DeleteSections,
        Self::FixStatus,
        Self::FixHeightmaps,
        Self::DeleteStructureReference,
        Self::PreventRetrogen,
        Self::ForceBlend,
        Self::RemoveLightingInfo,
        Self::LightPopulated,
        Self::Custom,
    ];

    /// exact upstream display names (FieldType name strings)
    pub fn name(self) -> &'static str {
        match self {
            Self::DataVersion => "DataVersion",
            Self::InhabitedTime => "InhabitedTime",
            Self::Timestamp => "Timestamp",
            Self::Compression => "Compression",
            Self::LastUpdate => "LastUpdate",
            Self::Status => "Status",
            Self::Biome => "Biome",
            Self::ReplaceBlocks => "ReplaceBlocks",
            Self::DeleteEntities => "DeleteEntities",
            Self::DeleteSections => "DeleteSections",
            Self::FixStatus => "FixStatus",
            Self::FixHeightmaps => "FixHeightmaps",
            Self::DeleteStructureReference => "DeleteStructureReference",
            Self::PreventRetrogen => "PreventRetrogen",
            Self::ForceBlend => "ForceBlend",
            Self::RemoveLightingInfo => "RemoveLightingInfo",
            Self::LightPopulated => "LightPopulated",
            Self::Custom => "Custom",
        }
    }

    /// upstream FieldType.getByName (exact, case-sensitive)
    pub fn by_name(name: &str) -> Option<FieldType> {
        Self::VALUES.iter().copied().find(|t| t.name() == name)
    }

    /// upstream clearCache flag (Status, ReplaceBlocks, DeleteSections, FixStatus)
    pub fn requires_clear_cache(self) -> bool {
        matches!(
            self,
            Self::Status | Self::ReplaceBlocks | Self::DeleteSections | Self::FixStatus
        )
    }
}

/// true when any of the given fields requires clearing the image cache
/// (upstream Field.requiresClearCache).
pub fn requires_clear_cache(fields: &[Field]) -> bool {
    fields.iter().any(|f| f.field_type.requires_clear_cache())
}

// ------------------------------------------------------------------- Field

#[derive(Clone, Debug)]
enum FieldValue {
    Int(i32),
    Long(i64),
    Byte(i8),
    Flag,
    Compression(CompressionType),
    Status { name: String, namespaced: String },
    Biome(BiomeIdent),
    Ranges(Vec<(i32, i32)>),
    Structures(Vec<String>),
    /// (source name, replacement) pairs
    Replace(Vec<(String, BlockReplace)>),
    Custom { path: String, tag: Tag },
}

#[derive(Clone, Debug)]
struct BiomeIdent {
    id: Option<i32>,
    name: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReplaceKind {
    Name,
    State,
    NameTile,
    StateTile,
}

#[derive(Clone, Debug)]
struct BlockReplace {
    kind: ReplaceKind,
    state: Compound, // always carries "Name" for NAME kinds
    tile: Option<Compound>,
}

impl BlockReplace {
    /// the source name (map key) is stored separately; this is the target name
    fn target_name(&self) -> String {
        nbt::string_or(Some(&self.state), "Name", "").to_string()
    }

    /// upstream BlockReplaceData.getState(dataVersion)
    fn get_state(&self, dv: i32) -> Compound {
        let mut s = self.state.clone();
        if matches!(self.kind, ReplaceKind::Name | ReplaceKind::NameTile) {
            let key = if dv >= chunk::DV_26_3_S7 { "id" } else { "Name" };
            let n = nbt::string_or(Some(&self.state), "Name", "").to_string();
            s.insert(key.into(), Tag::String(n));
        }
        s
    }
}

/// one parsed `Type = value` entry (upstream changer.Field).
/// `value` keeps the raw (de-quoted) value text.
#[derive(Clone, Debug)]
pub struct Field {
    pub field_type: FieldType,
    pub value: String,
    parsed: FieldValue,
}

// ------------------------------------------------------------------ parser

struct Ptr<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Ptr<'a> {
    fn new(s: &'a str) -> Self {
        Ptr { s, i: 0 }
    }
    fn has_next(&self) -> bool {
        self.i < self.s.len()
    }
    fn cur(&self) -> char {
        self.s[self.i..].chars().next().unwrap()
    }
    fn skip_ws(&mut self) {
        while self.has_next() && self.cur().is_whitespace() {
            self.i += self.cur().len_utf8();
        }
    }
    fn expect(&mut self, c: char) -> Result<(), String> {
        self.skip_ws();
        if self.has_next() && self.cur() == c {
            self.i += c.len_utf8();
            Ok(())
        } else {
            Err(format!(
                "expected '{c}' but got {}",
                if self.has_next() {
                    format!("'{}'", self.cur())
                } else {
                    "EOF".into()
                }
            ))
        }
    }
    fn parse_simple(&mut self, valid: impl Fn(char) -> bool) -> &'a str {
        let start = self.i;
        while self.has_next() && valid(self.cur()) {
            self.i += self.cur().len_utf8();
        }
        &self.s[start..self.i]
    }
    /// upstream StringPointer.parseQuotedString('"')
    fn parse_quoted(&mut self) -> Result<String, String> {
        self.i += 1; // skip '"'
        let start = self.i;
        let mut sb: Option<String> = None;
        let mut escape = false;
        while self.has_next() {
            let c = self.cur();
            self.i += c.len_utf8();
            if escape {
                if c != '\\' && c != '"' {
                    return Err(format!("invalid escape of '{c}'"));
                }
                escape = false;
            } else {
                if c == '\\' {
                    escape = true;
                    if sb.is_none() {
                        sb = Some(self.s[start..self.i - 1].to_string());
                    }
                    continue;
                }
                if c == '"' {
                    return Ok(sb.unwrap_or_else(|| self.s[start..self.i - 1].to_string()));
                }
            }
            if let Some(buf) = &mut sb {
                buf.push(c);
            }
        }
        Err("missing end quote".into())
    }
}

fn is_field_char(c: char) -> bool {
    matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '+')
}

/// port of ChangeParser.parse: comma separated `Type = value` entries.
pub fn parse_fields(input: &str) -> Result<Vec<Field>, String> {
    let mut ptr = Ptr::new(input);
    let mut fields = Vec::new();
    loop {
        ptr.skip_ws();
        if !ptr.has_next() {
            break;
        }
        let name = ptr.parse_simple(is_field_char).to_string();
        let Some(field_type) = FieldType::by_name(&name) else {
            return Err(format!("invalid field name \"{name}\""));
        };
        ptr.expect('=')?;
        ptr.skip_ws();
        let value = if ptr.has_next() && ptr.cur() == '"' {
            ptr.parse_quoted()?
        } else {
            ptr.parse_simple(is_field_char).to_string()
        };
        let parsed = parse_value(field_type, &value)
            .map_err(|e| format!("invalid value for field \"{name}\": {e}"))?;
        fields.push(Field {
            field_type,
            value,
            parsed,
        });
        ptr.skip_ws();
        if !ptr.has_next() {
            break;
        }
        ptr.expect(',')?;
    }
    Ok(fields)
}

fn parse_value(ft: FieldType, s: &str) -> Result<FieldValue, String> {
    use FieldType as F;
    match ft {
        F::DataVersion => {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err("not a nonnegative integer".into());
            }
            s.parse::<i32>().map(FieldValue::Int).map_err(|e| e.to_string())
        }
        F::InhabitedTime | F::LastUpdate => {
            s.parse::<i64>().map(FieldValue::Long).map_err(|e| e.to_string())
        }
        F::Timestamp => s.parse::<i32>().map(FieldValue::Int).map_err(|e| e.to_string()),
        F::Compression => parse_compression(s).map(FieldValue::Compression),
        F::Status => parse_status(s),
        F::Biome => parse_biome(s),
        F::ReplaceBlocks => parse_replace(s),
        F::DeleteEntities
        | F::FixStatus
        | F::FixHeightmaps
        | F::PreventRetrogen
        | F::ForceBlend
        | F::RemoveLightingInfo => {
            // upstream BooleanField: only "1" and "true" are valid
            if s == "1" || s == "true" {
                Ok(FieldValue::Flag)
            } else {
                Err("expected \"1\" or \"true\"".into())
            }
        }
        F::DeleteSections => parse_ranges(s).map(FieldValue::Ranges),
        F::DeleteStructureReference => parse_structures(s),
        F::LightPopulated => match s {
            "1" => Ok(FieldValue::Byte(1)),
            "0" => Ok(FieldValue::Byte(0)),
            _ => Err("expected \"0\" or \"1\"".into()),
        },
        F::Custom => parse_custom(s),
    }
}

fn parse_compression(s: &str) -> Result<CompressionType, String> {
    if let Some(t) = CompressionType::from_name(s) {
        return Ok(t);
    }
    // upstream also accepts the raw (signed) byte value
    if let Ok(v) = s.parse::<i64>() {
        if let Some(t) = CompressionType::ALL
            .iter()
            .find(|t| v == t.to_byte() as i8 as i64)
        {
            return Ok(*t);
        }
    }
    Err("invalid compression".into())
}

fn status_names() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/mapping/registry/status.json"))
            .unwrap_or_default()
    })
}

/// port of StatusRegistry.StatusIdentifier
fn parse_status(s: &str) -> Result<FieldValue, String> {
    let names = status_names();
    if let Some(inner) = s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        return Ok(FieldValue::Status {
            name: inner.to_string(),
            namespaced: inner.to_string(),
        });
    }
    if let Some(rest) = s.strip_prefix("minecraft:") {
        if names.contains(rest) {
            return Ok(FieldValue::Status {
                name: rest.to_string(),
                namespaced: s.to_string(),
            });
        }
    } else if names.contains(s) {
        return Ok(FieldValue::Status {
            name: s.to_string(),
            namespaced: format!("minecraft:{s}"),
        });
    }
    Err("invalid status".into())
}

fn biome_ids() -> &'static HashMap<String, i64> {
    static M: OnceLock<HashMap<String, i64>> = OnceLock::new();
    M.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../resources/mapping/registry/biome_name_to_id.json"
        ))
        .unwrap_or_default()
    })
}

/// port of BiomeRegistry.BiomeIdentifier parsing
fn parse_biome(s: &str) -> Result<FieldValue, String> {
    let low = s.to_lowercase();
    let quoted = low.starts_with('\'') && low.ends_with('\'');
    let inner = if quoted {
        &low[1..low.len() - 1]
    } else {
        &low[..]
    };
    let name = if inner != "-1"
        && !inner.is_empty()
        && !inner.bytes().all(|b| b.is_ascii_digit())
        && !inner.starts_with("minecraft:")
    {
        format!("minecraft:{inner}")
    } else {
        inner.to_string()
    };
    let ids = biome_ids();
    if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
        let id: i32 = name.parse().map_err(|_| "invalid biome id".to_string())?;
        let known = ids.iter().find(|(_, &v)| v as i32 == id);
        if quoted || known.is_some() {
            Ok(FieldValue::Biome(BiomeIdent {
                id: Some(id),
                name: known.map(|(k, _)| k.clone()),
            }))
        } else {
            Err(format!("invalid biome id \"{s}\""))
        }
    } else if name == "-1" {
        Ok(FieldValue::Biome(BiomeIdent {
            id: Some(-1),
            name: Some("minecraft:invalid".into()),
        }))
    } else if quoted || ids.contains_key(&name) {
        Ok(FieldValue::Biome(BiomeIdent {
            id: ids.get(&name).map(|&v| v as i32),
            name: Some(name),
        }))
    } else {
        Err(format!("invalid biome name \"{s}\""))
    }
}

fn block_names() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::from_str::<Vec<String>>(include_str!(
            "../resources/mapping/registry/blocks.json"
        ))
        .unwrap_or_default()
        .into_iter()
        .map(|n| {
            n.strip_prefix("minecraft:")
                .map(str::to_string)
                .unwrap_or(n)
        })
        .collect()
    })
}

fn is_valid_block_name(namespaced: &str) -> bool {
    let bare = namespaced.strip_prefix("minecraft:").unwrap_or(namespaced);
    block_names().contains(bare)
}

/// upstream ReplaceBlocksField.parseBlockReplacement.
/// value: `from=to[,from=to]*` where `to` may be `name`, `'custom name'`,
/// `{snbt}` optionally followed by `;{snbt}`.
fn parse_replace(s: &str) -> Result<FieldValue, String> {
    let bytes = s.as_bytes();
    let mut list: Vec<(String, BlockReplace)> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // -- source name
        let eq = s[i..]
            .find('=')
            .ok_or_else(|| {
                "expected \"=\" before the end of a name=value pair".to_string()
            })?
            + i;
        let mut from = s[i..eq].trim().to_string();
        i = eq + 1;
        if from.len() >= 2 && from.starts_with('\'') && from.ends_with('\'') {
            from = from[1..from.len() - 1].to_string();
        } else {
            from = format!("minecraft:{from}");
            if !is_valid_block_name(&from) {
                return Err(format!("invalid source block name \"{from}\""));
            }
        }

        // -- target
        let mut state: Option<Compound> = None;
        let mut name: Option<String> = None;
        let mut tile: Option<Compound> = None;
        let t = s[i..].trim_start();
        i += s[i..].len() - t.len();
        match bytes.get(i) {
            Some(b'{') => {
                let (tag, n) = nbt::parse_snbt_partial(&s[i..])
                    .map_err(|e| format!("invalid block state: {e}"))?;
                let Tag::Compound(c) = tag else {
                    return Err("block state must be a compound".into());
                };
                state = Some(c);
                i += n;
            }
            Some(b'\'') => {
                i += 1;
                let q = s[i..].find('\'').ok_or("missing ending quotes")? + i;
                name = Some(s[i..q].to_string());
                i = q + 1;
            }
            _ => {
                let end = s[i..]
                    .find([',', ';'])
                    .map(|p| p + i)
                    .unwrap_or(s.len());
                let to = format!("minecraft:{}", s[i..end].trim());
                if !is_valid_block_name(&to) {
                    return Err(format!("invalid block name \"{to}\""));
                }
                name = Some(to);
                i = end;
            }
        }
        // optional ;tile
        let t = s[i..].trim_start();
        i += s[i..].len() - t.len();
        if bytes.get(i) == Some(&b';') {
            i += 1;
            let t = s[i..].trim_start();
            i += s[i..].len() - t.len();
            if bytes.get(i) != Some(&b'{') {
                return Err("expected '{' after ';'".into());
            }
            let (tag, n) = nbt::parse_snbt_partial(&s[i..])
                .map_err(|e| format!("invalid tile nbt: {e}"))?;
            let Tag::Compound(c) = tag else {
                return Err("tile nbt must be a compound".into());
            };
            tile = Some(c);
            i += n;
        }

        let kind = match (&state, &tile) {
            (Some(_), Some(_)) => ReplaceKind::StateTile,
            (Some(_), None) => ReplaceKind::State,
            (None, Some(_)) => ReplaceKind::NameTile,
            (None, None) => ReplaceKind::Name,
        };
        let mut st = state.unwrap_or_default();
        if let Some(n) = &name {
            st.insert("Name".into(), Tag::String(n.clone()));
        }
        list.push((
            from,
            BlockReplace {
                kind,
                state: st,
                tile,
            },
        ));

        let t = s[i..].trim_start();
        i += s[i..].len() - t.len();
        match bytes.get(i) {
            Some(b',') => i += 1,
            None => break,
            Some(_) => return Err("expected ',' or EOF".into()),
        }
    }
    if list.is_empty() {
        return Err("invalid block replacement".into());
    }
    Ok(FieldValue::Replace(list))
}

/// upstream RangeParser.parseRanges(s, ",")
fn parse_ranges(s: &str) -> Result<Vec<(i32, i32)>, String> {
    if s.trim() == "true" {
        return Ok(vec![(i32::MIN, i32::MAX)]);
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let t = part.trim();
        if t.is_empty() {
            return Err("invalid range".into());
        }
        let (a, b) = match t.split_once(':') {
            None => (t, None),
            Some((a, b)) => (a, Some(b)),
        };
        let from = if a.is_empty() {
            i32::MIN
        } else {
            a.parse::<i32>().map_err(|_| "invalid range".to_string())?
        };
        let to = match b {
            None => from,
            Some("") => i32::MAX,
            Some(b) => b.parse::<i32>().map_err(|_| "invalid range".to_string())?,
        };
        out.push((from, to));
    }
    Ok(out)
}

fn structure_names() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| crate::structures::structures().valid.clone())
}

/// upstream StructureRegistry.getAlts approximation:
/// {name, name minus "minecraft:", "minecraft:"+name}
fn structure_alts(name: &str) -> Vec<String> {
    let mut v = vec![name.to_string()];
    if let Some(b) = name.strip_prefix("minecraft:") {
        v.push(b.to_string());
    } else {
        v.push(format!("minecraft:{name}"));
    }
    v
}

fn parse_structures(s: &str) -> Result<FieldValue, String> {
    let valid = structure_names();
    let mut out = Vec::new();
    for part in s.split(',') {
        let t = part.trim();
        let quoted = t.starts_with('\'') && t.ends_with('\'');
        if !quoted && !valid.contains(t) && !valid.contains(&format!("minecraft:{t}")) {
            return Err(format!("invalid structure name \"{t}\""));
        }
        out.push(t.to_string());
    }
    Ok(FieldValue::Structures(out))
}

fn parse_custom(s: &str) -> Result<FieldValue, String> {
    let Some(eq) = s.find('=') else {
        return Err("invalid custom field format, expected \"path = SNBT\"".into());
    };
    let path = s[..eq].trim().to_string();
    let val = s[eq + 1..].trim();
    if path.is_empty() || val.is_empty() {
        return Err("invalid custom field format, expected \"path = SNBT\"".into());
    }
    let tag = nbt::parse_snbt(val)?;
    Ok(FieldValue::Custom { path, tag })
}

// -------------------------------------------------------------- public API

#[derive(Default, Debug)]
pub struct ChangeResult {
    /// fields that ran without error
    pub applied: usize,
    /// per-field error messages (fields that failed are skipped)
    pub errors: Vec<String>,
}

/// upstream ChunkData.applyFieldChanges: applies every field to this chunk
/// column. Fields that touch only NBT act on `region`; Timestamp/Compression
/// act on the `Chunk` metadata of all three; DeleteEntities additionally
/// acts on `entities`.
pub fn apply_change(refs: &mut ChunkRefs, fields: &[Field], force: bool) -> ChangeResult {
    let mut res = ChangeResult::default();
    for f in fields {
        match apply_field(refs, f, force) {
            Ok(()) => res.applied += 1,
            Err(e) => res.errors.push(format!("{}: {}", f.field_type.name(), e)),
        }
    }
    res
}

/// convenience wrapper for a single region chunk.
#[cfg(test)]
fn apply_change_region(chunk: &mut crate::mca::Chunk, fields: &[Field], force: bool) -> ChangeResult {
    let mut refs = ChunkRefs {
        region: Some(chunk),
        poi: None,
        entities: None,
    };
    apply_change(&mut refs, fields, force)
}

fn apply_field(refs: &mut ChunkRefs, f: &Field, force: bool) -> Result<(), String> {
    use FieldType as T;
    match (f.field_type, &f.parsed) {
        (T::DataVersion, FieldValue::Int(v)) => {
            for c in [&mut refs.region, &mut refs.poi, &mut refs.entities]
                .into_iter()
                .flatten()
            {
                let Some(d) = c.data.as_mut() else { continue };
                if !force && get_data_version_tag(d).is_none() {
                    continue;
                }
                set_data_version(d, *v);
            }
        }
        (T::InhabitedTime, FieldValue::Long(v)) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                let dv = nbt::data_version(Some(d));
                scalar_field(d, dv, "InhabitedTime", Tag::Long(*v), force);
            }
        }
        (T::Timestamp, FieldValue::Int(v)) => {
            for c in [&mut refs.region, &mut refs.poi, &mut refs.entities]
                .into_iter()
                .flatten()
            {
                c.timestamp = *v;
            }
        }
        (T::Compression, FieldValue::Compression(t)) => {
            for c in [&mut refs.region, &mut refs.poi, &mut refs.entities]
                .into_iter()
                .flatten()
            {
                c.compression = *t;
            }
        }
        (T::LastUpdate, FieldValue::Long(v)) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                let dv = nbt::data_version(Some(d));
                scalar_field(d, dv, "LastUpdate", Tag::Long(*v), force);
            }
        }
        (T::Status, FieldValue::Status { name, namespaced }) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                let dv = nbt::data_version(Some(d));
                if force || get_status(d, dv).is_some() {
                    set_status(d, dv, name, namespaced);
                }
            }
        }
        (T::Biome, FieldValue::Biome(b)) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                apply_biome(d, b, force);
            }
        }
        (T::ReplaceBlocks, FieldValue::Replace(r)) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                replace_blocks(d, r);
            }
        }
        (T::DeleteEntities, FieldValue::Flag) => {
            delete_entities(refs, None);
        }
        (T::DeleteSections, FieldValue::Ranges(r)) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                delete_sections(d, r);
            }
            delete_entities(refs, Some(r));
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                fix_heightmaps(d);
            }
        }
        (T::FixStatus, FieldValue::Flag) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                fix_status(d);
            }
        }
        (T::FixHeightmaps, FieldValue::Flag) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                fix_heightmaps(d);
            }
        }
        (T::DeleteStructureReference, FieldValue::Structures(names)) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                delete_structure(d, names);
            }
        }
        (T::PreventRetrogen, FieldValue::Flag) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                d.swap_remove("below_zero_retrogen");
                d.insert("Status".into(), Tag::String("full".into()));
            }
        }
        (T::ForceBlend, FieldValue::Flag) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                force_blend(d);
            }
        }
        (T::RemoveLightingInfo, FieldValue::Flag) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                remove_lighting(d);
            }
        }
        (T::LightPopulated, FieldValue::Byte(v)) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                light_populated(d, *v, force);
            }
        }
        (T::Custom, FieldValue::Custom { path, tag }) => {
            if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
                apply_custom(d, path, tag, force);
            }
        }
        _ => return Err("internal field/value mismatch".into()),
    }
    Ok(())
}

// -------------------------------------------------------------- tag access

fn level(root: &Compound) -> Option<&Compound> {
    nbt::level_from_root(root)
}

fn level_mut(root: &mut Compound) -> Option<&mut Compound> {
    root.get_mut("Level")?.as_compound_mut()
}

/// Helper.getDataVersionTag
fn get_data_version_tag(root: &Compound) -> Option<&Tag> {
    if let Some(t @ Tag::Int(_)) = root.get("DataVersion") {
        return Some(t);
    }
    level(root)?.get("DataVersion")
}

/// Helper.setDataVersion
fn set_data_version(root: &mut Compound, dv: i32) {
    if dv < chunk::DV_LEVEL_DATAVERSION {
        if let Some(l) = level_mut(root) {
            l.insert("DataVersion".into(), Tag::Int(dv));
        }
    }
    root.insert("DataVersion".into(), Tag::Int(dv));
}

/// simple scalar field living on Level (legacy) or root (flat).
fn scalar_field(root: &mut Compound, dv: i32, key: &str, tag: Tag, force: bool) {
    if !force && chunk::data_container(root, dv).and_then(|c| c.get(key)).is_none() {
        return;
    }
    if let Some(c) = chunk::data_container_mut(root, dv) {
        c.insert(key.into(), tag);
    }
}

fn get_status(root: &Compound, dv: i32) -> Option<String> {
    if dv < chunk::DV_18W06A {
        return None;
    }
    chunk::data_container(root, dv)
        .and_then(|c| c.get("Status"))
        .and_then(|t| t.as_string())
        .map(str::to_string)
}

fn set_status(root: &mut Compound, dv: i32, bare: &str, namespaced: &str) {
    let v = if dv >= chunk::DV_NAMESPACED_STATUS {
        namespaced.to_string()
    } else {
        bare.to_string()
    };
    if let Some(c) = chunk::data_container_mut(root, dv) {
        c.insert("Status".into(), Tag::String(v));
    }
}

fn as_list(t: &Tag) -> Option<&Vec<Tag>> {
    match t {
        Tag::List(_, l) => Some(l),
        _ => None,
    }
}

fn as_list_mut(t: &mut Tag) -> Option<&mut Vec<Tag>> {
    match t {
        Tag::List(_, l) => Some(l),
        _ => None,
    }
}

fn as_long_array(t: &Tag) -> Option<&Vec<i64>> {
    match t {
        Tag::LongArray(v) => Some(v),
        _ => None,
    }
}

fn as_byte_array(t: &Tag) -> Option<&Vec<u8>> {
    match t {
        Tag::ByteArray(v) => Some(v),
        _ => None,
    }
}

fn section_y(s: &Compound) -> i64 {
    nbt::int_from(Some(s), "Y", 0) as i64
}

fn in_ranges(y: i64, ranges: &[(i32, i32)]) -> bool {
    ranges.iter().any(|&(a, b)| a as i64 <= y && y <= b as i64)
}

// -------------------------------------------------------------- field impls

fn fix_status(root: &mut Compound) {
    let dv = nbt::data_version(Some(root));
    let Some(status) = get_status(root, dv) else {
        return; // upstream would NPE; we skip
    };
    if status != "empty" && status != "minecraft:empty" {
        return;
    }
    if chunk::sections_dv(root, dv).map(|s| !s.is_empty()).unwrap_or(false) {
        set_status(root, dv, "full", "minecraft:full");
    }
}

fn light_populated(root: &mut Compound, v: i8, force: bool) {
    let dv = nbt::data_version(Some(root));
    if dv < chunk::DV_18W06A {
        // legacy Level.LightPopulated; upstream writes it with putLong (bug)
        let exists = level(root).and_then(|l| l.get("LightPopulated")).is_some();
        if !force && !exists {
            return;
        }
        if let Some(l) = level_mut(root) {
            l.insert("LightPopulated".into(), Tag::Long(v as i64));
        }
    } else if dv < chunk::DV_19W02A {
        // ChunkFilter_18w45a: no-op
    } else if dv < chunk::DV_21W43A {
        let exists = level(root).and_then(|l| l.get("isLightOn")).is_some();
        if !force && !exists {
            return;
        }
        if let Some(l) = level_mut(root) {
            l.insert("isLightOn".into(), Tag::Long(v as i64)); // upstream putLong quirk
        }
    } else {
        if !force && root.get("isLightOn").is_none() {
            return;
        }
        root.insert("isLightOn".into(), Tag::Byte(v));
    }
}

fn remove_lighting(root: &mut Compound) {
    let dv = nbt::data_version(Some(root));
    if dv < chunk::DV_18W06A {
        if let Some(l) = level_mut(root) {
            l.swap_remove("LightPopulated");
        }
    } else if dv < chunk::DV_19W02A {
        // only section arrays are removed on this branch
    } else if dv < chunk::DV_21W43A {
        if let Some(l) = level_mut(root) {
            l.insert("isLightOn".into(), Tag::Byte(0));
        }
    } else {
        root.insert("isLightOn".into(), Tag::Byte(0));
    }
    if let Some(secs) = chunk::sections_dv_mut(root, dv) {
        for s in secs.iter_mut().filter_map(|t| t.as_compound_mut()) {
            s.swap_remove("BlockLight");
            s.swap_remove("SkyLight");
        }
    }
}

fn force_blend(root: &mut Compound) {
    let dv = nbt::data_version(Some(root));
    if dv < chunk::DV_21W43A {
        return;
    }
    if (chunk::DV_1_21_5..chunk::DV_25W09A).contains(&dv) {
        // 25w02a..25w09a: reset DataVersion to 1.21.4 instead
        set_data_version(root, chunk::DV_FORCE_BLEND_TARGET);
        return;
    }
    let Some(secs) = chunk::sections_dv(root, dv) else {
        return;
    };
    let mut min = i64::MAX;
    let mut max = i64::MIN;
    for s in secs.iter().filter_map(|t| t.as_compound()) {
        let y = section_y(s);
        min = min.min(y);
        max = max.max(y);
    }
    if min == i64::MAX {
        min = 0;
        max = 0;
    }
    min = min.min(-4);
    max = max.max(20);
    let mut bd = Compound::default();
    bd.insert("min_section".into(), Tag::Int(min as i32));
    bd.insert("max_section".into(), Tag::Int(max as i32));
    root.insert("blending_data".into(), Tag::Compound(bd));
    root.swap_remove("Heightmaps");
    root.swap_remove("isLightOn");
}

fn delete_structure(root: &mut Compound, names: &[String]) {
    let dv = nbt::data_version(Some(root));
    if let Some(c) = chunk::data_container_mut(root, dv) {
        let skey = if dv >= chunk::DV_21W43A {
            "structures"
        } else {
            "Structures"
        };
        if let Some(structs) = c.get_mut(skey).and_then(|t| t.as_compound_mut()) {
            for name in names {
                for alt in structure_alts(name) {
                    for k in ["References", "Starts", "starts", "references"] {
                        if let Some(sub) =
                            structs.get_mut(k).and_then(|t| t.as_compound_mut())
                        {
                            sub.swap_remove(&alt);
                        }
                    }
                }
            }
        }
    }
}

fn entity_y_in_ranges(e: &Compound, ranges: &[(i32, i32)]) -> bool {
    let Some((_, pos)) = e.get("Pos").and_then(|t| t.as_list()) else {
        return false;
    };
    if pos.len() != 3 {
        return false;
    }
    let Some(y) = pos[1].as_f64() else {
        return false;
    };
    in_ranges(y as i64 >> 4, ranges)
}

fn delete_from_list(list: &mut Vec<Tag>, ranges: Option<&[(i32, i32)]>) {
    match ranges {
        None => list.clear(),
        Some(r) => list.retain(|t| {
            !t.as_compound()
                .map(|c| entity_y_in_ranges(c, r))
                .unwrap_or(false)
        }),
    }
}

fn level_list_mut<'a>(root: &'a mut Compound, key: &str) -> Option<&'a mut Vec<Tag>> {
    as_list_mut(level_mut(root)?.get_mut(key)?)
}

/// ChunkFilter.Entities.deleteEntities
fn delete_entities(refs: &mut ChunkRefs, ranges: Option<&[(i32, i32)]>) {
    let dv = refs
        .region
        .as_ref()
        .and_then(|c| c.data.as_ref())
        .map(|d| nbt::data_version(Some(d)))
        .unwrap_or(0);
    if dv < chunk::DV_20W45A {
        // Level.Entities on the region chunk
        if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
            if let Some(list) = level_list_mut(d, "Entities") {
                delete_from_list(list, ranges);
            }
        }
    } else if dv < chunk::DV_21W43A {
        if let Some(d) = refs.entities.as_mut().and_then(|c| c.data.as_mut()) {
            if let Some(list) = level_list_mut(d, "Entities") {
                delete_from_list(list, ranges);
            }
        }
        // proto-entities: Level.Entities on region
        if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
            if let Some(list) = level_list_mut(d, "Entities") {
                delete_from_list(list, ranges);
            }
        }
    } else {
        if let Some(d) = refs.entities.as_mut().and_then(|c| c.data.as_mut()) {
            if let Some(list) = level_list_mut(d, "Entities") {
                delete_from_list(list, ranges);
            }
        }
        // upstream 21w43a quirk: proto-entities at Level.entities (lowercase)
        if let Some(d) = refs.region.as_mut().and_then(|c| c.data.as_mut()) {
            if let Some(list) = level_list_mut(d, "entities") {
                delete_from_list(list, ranges);
            }
        }
    }
}

fn delete_sections(root: &mut Compound, ranges: &[(i32, i32)]) {
    let dv = nbt::data_version(Some(root));
    if dv < chunk::DV_21W37A {
        // remove whole sections from Level.Sections
        if let Some(secs) = chunk::sections_dv_mut(root, dv) {
            secs.retain(|t| {
                !t.as_compound()
                    .map(|s| in_ranges(section_y(s), ranges))
                    .unwrap_or(false)
            });
        }
        return;
    }
    // 2834+: only "empty" the selected sections, gated on Status
    let status = chunk::data_container(root, dv)
        .and_then(|c| c.get("Status"))
        .and_then(|t| t.as_string())
        .unwrap_or("")
        .to_string();
    let target = if dv >= chunk::DV_NAMESPACED_STATUS {
        match status.as_str() {
            "minecraft:light" | "minecraft:spawn" | "minecraft:heightmaps"
            | "minecraft:full" => "minecraft:features",
            _ => return,
        }
    } else {
        match status.as_str() {
            "light" | "spawn" | "heightmaps" | "full" => "features",
            _ => return,
        }
    };
    if let Some(c) = chunk::data_container_mut(root, dv) {
        c.insert("Status".into(), Tag::String(target.into()));
    }
    if let Some(secs) = chunk::sections_dv_mut(root, dv) {
        for s in secs.iter_mut().filter_map(|t| t.as_compound_mut()) {
            if !in_ranges(section_y(s), ranges) {
                continue;
            }
            let bs = s
                .entry("block_states".into())
                .or_insert_with(|| Tag::Compound(Compound::default()));
            if let Some(bs) = bs.as_compound_mut() {
                bs.swap_remove("data");
                let mut p = Compound::default();
                p.insert("Name".into(), Tag::String("minecraft:air".into()));
                bs.insert("palette".into(), Tag::List(crate::nbt::TAG_COMPOUND, vec![Tag::Compound(p)]));
            }
            s.swap_remove("BlockLight");
        }
    }
}

fn apply_biome(root: &mut Compound, b: &BiomeIdent, force: bool) {
    let dv = nbt::data_version(Some(root));
    if dv < chunk::DV_18W06A {
        // Level.Biomes byte[256]
        let Some(id) = b.id else { return };
        let exists = level(root).and_then(|l| l.get("Biomes")).is_some();
        if !force && !exists {
            return;
        }
        let Some(l) = level_mut(root) else { return };
        if let Some(Tag::ByteArray(arr)) = l.get_mut("Biomes") {
            for v in arr.iter_mut() {
                *v = id as u8;
            }
        } else if force {
            l.insert("Biomes".into(), Tag::ByteArray(vec![id as u8; 256]));
        }
    } else if dv < chunk::DV_21W37A {
        // Level.Biomes int[]
        let Some(id) = b.id else { return };
        let exists = level(root).and_then(|l| l.get("Biomes")).is_some();
        if !force && !exists {
            return;
        }
        let Some(l) = level_mut(root) else { return };
        if let Some(Tag::IntArray(arr)) = l.get_mut("Biomes") {
            for v in arr.iter_mut() {
                *v = id;
            }
        } else if force {
            let n = if dv < chunk::DV_19W36A {
                256
            } else if dv < chunk::DV_21W06A {
                1024
            } else {
                // 21w06a: sectionRange.num() * 64
                let Some(range) = l
                    .get(chunk::sections_key(dv))
                    .and_then(as_list)
                    .and_then(|s| find_section_range_from(l, s))
                else {
                    return;
                };
                (range.1 - range.0 + 1).max(0) as usize * 64
            };
            l.insert("Biomes".into(), Tag::IntArray(vec![id; n]));
        }
    } else {
        // per-section biomes compounds
        let Some(name) = b.name.clone() else { return };
        if let Some(secs) = chunk::sections_dv_mut(root, dv) {
            for s in secs.iter_mut().filter_map(|t| t.as_compound_mut()) {
                if !force && !s.contains_key("biomes") {
                    continue;
                }
                let mut biomes = Compound::default();
                biomes.insert(
                    "palette".into(),
                    Tag::List(crate::nbt::TAG_STRING, vec![Tag::String(name.clone())]),
                );
                biomes.insert("data".into(), Tag::LongArray(vec![0]));
                s.insert("biomes".into(), Tag::Compound(biomes));
            }
        }
    }
}

/// Helper.findSectionRange: min/max section Y limited to -127..126;
/// root's yPos wins as the minimum when present. Returns None only when the
/// caller passed no valid context (mirrors upstream null checks).
fn find_section_range_from(root: &Compound, secs: &[Tag]) -> Option<(i32, i32)> {
    let mut min = i64::MAX;
    let mut max = i64::MIN;
    for t in secs {
        if let Some(s) = t.as_compound() {
            if s.len() > 1 && s.contains_key("Y") {
                let y = section_y(s);
                min = min.min(y);
                max = max.max(y);
            }
        }
    }
    if let Some(y) = root.get("yPos").and_then(|t| t.as_i64()) {
        min = y;
    }
    Some(if min > max {
        (-127, 126)
    } else {
        (min.max(-127) as i32, max.min(126) as i32)
    })
}

// ------------------------------------------------------------- heightmaps

struct HmCfg {
    light_blocking: HashSet<String>,
    world_surface: HashSet<String>,
    ocean_floor: HashSet<String>,
    motion_blocking: HashSet<String>,
    leaves: HashSet<String>,
}

fn load_hm(src: &str) -> HmCfg {
    let v: serde_json::Value = serde_json::from_str(src).unwrap_or(serde_json::Value::Null);
    let get = |k: &str| -> HashSet<String> {
        v.get(k)
            .and_then(|x| serde_json::from_value::<Vec<String>>(x.clone()).ok())
            .unwrap_or_default()
            .into_iter()
            .collect()
    };
    HmCfg {
        light_blocking: get("light_blocking"),
        world_surface: get("world_surface"),
        ocean_floor: get("ocean_floor"),
        motion_blocking: get("motion_blocking"),
        leaves: get("leaves"),
    }
}

fn hm_cfg(dv: i32) -> &'static HmCfg {
    static C: OnceLock<Vec<(i32, HmCfg)>> = OnceLock::new();
    let m = C.get_or_init(|| {
        let files: &[(i32, &str)] = &[
            (chunk::DV_18W19A, include_str!("../resources/mapping/java_1_13/heightmaps_18w19a.json")),
            (chunk::DV_18W43A, include_str!("../resources/mapping/java_1_14/heightmaps_18w43a.json")),
            (chunk::DV_19W34A, include_str!("../resources/mapping/java_1_15/heightmaps_19w34a.json")),
            (chunk::DV_20W06A, include_str!("../resources/mapping/java_1_16/heightmaps_20w06a.json")),
            (chunk::DV_20W45A, include_str!("../resources/mapping/java_1_17/heightmaps_20w45a.json")),
            (chunk::DV_21W37A, include_str!("../resources/mapping/java_1_18/heightmaps_21w37a.json")),
            (chunk::DV_22W11A, include_str!("../resources/mapping/java_1_19/heightmaps_22w11a.json")),
            (chunk::DV_23W12A, include_str!("../resources/mapping/java_1_20/heightmaps_23w12a.json")),
            (chunk::DV_24W18A, include_str!("../resources/mapping/java_1_21/heightmaps_24w18a.json")),
            (chunk::DV_25W32A, include_str!("../resources/mapping/java_1_21/heightmaps_25w32a.json")),
            (chunk::DV_26_1_S1, include_str!("../resources/mapping/java_26/heightmaps_26.1-snapshot-1.json")),
            (chunk::DV_26_2_S1, include_str!("../resources/mapping/java_26/heightmaps_26.2-snapshot-1.json")),
            (chunk::DV_26_3_S1, include_str!("../resources/mapping/java_26/heightmaps_26.3-snapshot-1.json")),
            (chunk::DV_26_3_S4, include_str!("../resources/mapping/java_26/heightmaps_26.3-snapshot-4.json")),
        ];
        files.iter().map(|(k, s)| (*k, load_hm(s))).collect()
    });
    chunk::dv_floor_key(m, dv).map(|(_, c)| c).unwrap_or(&m[0].1)
}

fn hm_18w06a() -> &'static (HashSet<String>, HashSet<String>) {
    static S: OnceLock<(HashSet<String>, HashSet<String>)> = OnceLock::new();
    S.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../resources/mapping/java_1_13/heightmaps_18w06a.json"
        ))
        .unwrap_or(serde_json::Value::Null);
        let get = |k: &str| {
            v.get(k)
                .and_then(|x| serde_json::from_value::<Vec<String>>(x.clone()).ok())
                .unwrap_or_default()
                .into_iter()
                .collect::<HashSet<String>>()
        };
        (get("light"), get("rain"))
    })
}

fn legacy_ids() -> &'static HashSet<i64> {
    static S: OnceLock<HashSet<i64>> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../resources/mapping/java_1_9/heightmaps_legacy.json"
        ))
        .unwrap_or_default()
    })
}

fn legacy_names() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::from_str::<Vec<String>>(include_str!(
            "../resources/mapping/java_1_13/heightmaps_legacy.json"
        ))
        .unwrap_or_default()
        .into_iter()
        .collect()
    })
}

/// 36-long interleaved 9-bit packing (1466..2528).
fn apply_height_map36(raw: &[i16; 256]) -> Vec<i64> {
    let mut data = vec![0i64; 36];
    let mut offset = 0i32;
    let mut index = 0usize;
    for i in 0..36 {
        let mut l: i64 = 0;
        for j in 0..8i32 {
            if index >= 256 {
                break;
            }
            let shift = 9 * j - offset;
            l = if shift < 0 {
                l.wrapping_add((raw[index] as i64) >> (-shift))
            } else {
                l.wrapping_add((raw[index] as i64) << shift)
            };
            index += 1;
        }
        offset += 1;
        if offset == 9 {
            offset = 0;
        }
        data[i] = l;
    }
    data
}

/// 37-long simple 9-bit packing (2529..2693).
fn apply_height_map37(raw: &[i16; 256]) -> Vec<i64> {
    let mut data = vec![0i64; 37];
    let mut index = 0usize;
    for i in 0..37 {
        let mut l: i64 = 0;
        for j in 0..7 {
            if index >= 256 {
                break;
            }
            l = l.wrapping_add((raw[index] as i64) << (9 * j));
            index += 1;
        }
        data[i] = l;
    }
    data
}

/// quirky applyHeightMap(raw, bits) used since 21w06a (j < bits-1).
fn apply_height_map_bits(raw: &[i16; 256], bits: usize) -> Vec<i64> {
    if bits == 0 || bits > 64 {
        return Vec::new();
    }
    let len = 256_usize.div_ceil(64 / bits);
    let mut data = vec![0i64; len];
    let mut index = 0usize;
    for i in 0..len {
        let mut l: i64 = 0;
        for j in 0..bits - 1 {
            if index >= 256 {
                break;
            }
            l = l.wrapping_add((raw[index] as i64) << (bits * j));
            index += 1;
        }
        data[i] = l;
    }
    data
}

// packed-block helpers -----------------------------------------------------
// both bit layouts live in chunk::PackedData / chunk::packed_set; these are
// the i64 conveniences this module's loops use.

fn get_pi(i: usize, arr: &[i64], dv: i32) -> i64 {
    PackedData::block_states(PackedSrc::Longs(arr), dv).get(i) as i64
}

fn set_pi(i: usize, v: i64, arr: &mut [i64], dv: i32) {
    chunk::packed_set(i, v as u64, arr, dv)
}

fn adjust_bits(
    palette_len: usize,
    old: Vec<i64>,
    mapping: Option<&HashMap<i64, i64>>,
    dv: i32,
) -> Vec<i64> {
    let mut new_bits = if palette_len == 0 {
        4
    } else {
        32 - (palette_len - 1).leading_zeros() as usize
    };
    new_bits = new_bits.max(4);
    let new_len = if dv >= chunk::DV_20W17A {
        (4096f64 / (64f64 / new_bits as f64).floor()).ceil() as usize
    } else {
        new_bits * 64
    };
    let mut new_arr = if new_bits == old.len() / 64 {
        old.clone()
    } else {
        vec![0i64; new_len]
    };
    for i in 0..4096 {
        let mut v = get_pi(i, &old, dv);
        if let Some(m) = mapping {
            v = *m.get(&v).unwrap_or(&v);
        }
        set_pi(i, v, &mut new_arr, dv);
    }
    new_arr
}

/// shared palette-entry name (chunk::palette_entry_name covers ""/"id"/"Name"
/// and bare-string entries).
fn pal_name(tag: &Tag) -> &str {
    chunk::palette_entry_name(tag)
}

/// scan sections top-down and fill a 256-entry height column array.
fn compute_raw_heights(
    palettes: &[(usize, Vec<Tag>, Vec<i64>)], // (rel-index, palette, states)
    matcher: &dyn Fn(&str) -> bool,
    dv: i32,
) -> [i16; 256] {
    let mut heightmap = [0i16; 256];
    for cx in 0..16usize {
        'col: for cz in 0..16usize {
            for (rel_i, palette, states) in palettes.iter().rev() {
                for cy in (0..16usize).rev() {
                    let bi = cy * 256 + cz * 16 + cx;
                    let pi = get_pi(bi, states, dv);
                    let Some(entry) = palette.get(pi as usize) else {
                        continue;
                    };
                    if matcher(pal_name(entry)) {
                        heightmap[cz * 16 + cx] =
                            (*rel_i as i32 * 16 + cy as i32 + 1) as i16;
                        continue 'col;
                    }
                }
            }
        }
    }
    heightmap
}

/// one heightmap pass: compute packed heights for a matcher (dv >= 1466).
fn heightmap_pass(
    root: &Compound,
    dv: i32,
    matcher: &dyn Fn(&str) -> bool,
) -> Option<Vec<i64>> {
    if dv >= chunk::DV_21W06A {
        let secs = chunk::sections_dv(root, dv)?;
        let scope = if dv >= chunk::DV_21W43A {
            root
        } else {
            level(root)?
        };
        let (from, to) = find_section_range_from(scope, secs)?;
        let num = (to - from + 1).max(0) as usize;
        if num == 0 {
            return Some(Vec::new());
        }
        let mut pals: Vec<(usize, Vec<Tag>, Vec<i64>)> = Vec::new();
        for t in secs {
            let Some(s) = t.as_compound() else { continue };
            if s.len() <= 1 || !s.contains_key("Y") {
                continue;
            }
            let rel = section_y(s) as i32 - from;
            if rel < 0 || rel as usize >= num {
                continue;
            }
            let (palette, states) = if (chunk::DV_21W37A..chunk::DV_21W43A).contains(&dv) {
                // upstream 21w37a heightmap quirk: flat palette/block_states
                (
                    s.get("palette").and_then(as_list).cloned(),
                    s.get("block_states").and_then(as_long_array).cloned(),
                )
            } else if dv >= chunk::DV_21W43A {
                let bs = s.get("block_states").and_then(|t| t.as_compound());
                (
                    bs.and_then(|c| c.get("palette")).and_then(as_list).cloned(),
                    bs.and_then(|c| c.get("data")).and_then(as_long_array).cloned(),
                )
            } else {
                (
                    s.get("Palette").and_then(as_list).cloned(),
                    s.get("BlockStates").and_then(as_long_array).cloned(),
                )
            };
            let (Some(p), Some(b)) = (palette, states) else {
                continue;
            };
            pals.push((rel as usize, p, b));
        }
        pals.sort_by_key(|(i, _, _)| *i);
        let raw = compute_raw_heights(&pals, matcher, dv);
        // Integer.numberOfLeadingZeros works on 32-bit ints
        let bits = 32u32.saturating_sub(((num * 16) as u32).leading_zeros()) as usize;
        return Some(apply_height_map_bits(&raw, bits));
    }
    // 1466..2693: fixed 16-section arrays at absolute Y
    let secs = chunk::sections_dv(root, dv)?;
    let mut pals: [Option<(Vec<Tag>, Vec<i64>)>; 16] = Default::default();
    for t in secs {
        let Some(s) = t.as_compound() else { continue };
        let y = section_y(s);
        if !(0..=15).contains(&y) {
            continue;
        }
        let p = s.get("Palette").and_then(as_list).cloned();
        let b = s.get("BlockStates").and_then(as_long_array).cloned();
        if let (Some(p), Some(b)) = (p, b) {
            pals[y as usize] = Some((p, b));
        }
    }
    let mut heightmap = [0i16; 256];
    for cx in 0..16usize {
        'col: for cz in 0..16usize {
            for (i, e) in pals.iter().enumerate().rev() {
                let Some((palette, states)) = e else { continue };
                for cy in (0..16usize).rev() {
                    let bi = cy * 256 + cz * 16 + cx;
                    let pi = get_pi(bi, states, dv);
                    let Some(entry) = palette.get(pi as usize) else {
                        continue;
                    };
                    if matcher(pal_name(entry)) {
                        heightmap[cz * 16 + cx] = (i * 16 + cy + 1) as i16;
                        continue 'col;
                    }
                }
            }
        }
    }
    Some(if dv >= chunk::DV_20W17A {
        apply_height_map37(&heightmap)
    } else {
        apply_height_map36(&heightmap)
    })
}

fn set_heightmap(root: &mut Compound, name: &str, data: Vec<i64>) {
    // upstream always writes via Level (root["Level"]["Heightmaps"]) — a no-op
    // for flattened chunks, replicated faithfully.
    if let Some(l) = level_mut(root) {
        if !l.contains_key("Heightmaps") {
            l.insert("Heightmaps".into(), Tag::Compound(Compound::default()));
        }
        if let Some(hms) = l.get_mut("Heightmaps").and_then(|t| t.as_compound_mut()) {
            hms.insert(name.into(), Tag::LongArray(data));
        }
    }
}

/// ChunkFilter.Heightmap: recompute all heightmaps (called by FixHeightmaps
/// and DeleteSections).
fn fix_heightmaps(root: &mut Compound) {
    let dv = nbt::data_version(Some(root));
    if dv < chunk::DV_17W47A {
        // legacy byte-block path -> Level.HeightMap int[256]
        let Some(secs) = chunk::sections_dv(root, dv) else {
            return;
        };
        let ids = legacy_ids();
        let mut blocks: [Option<Vec<u8>>; 16] = Default::default();
        for t in secs {
            let Some(s) = t.as_compound() else { continue };
            let y = section_y(s);
            if !(0..=15).contains(&y) {
                continue;
            }
            if let Some(b) = s.get("Blocks").and_then(as_byte_array) {
                blocks[y as usize] = Some(b.clone());
            }
        }
        let mut heightmap = [0i32; 256];
        for cx in 0..16usize {
            'col: for cz in 0..16usize {
                for i in (0..16usize).rev() {
                    let Some(b) = &blocks[i] else { continue };
                    for cy in (0..16usize).rev() {
                        let bi = cy * 256 + cz * 16 + cx;
                        if !ids.contains(&(b[bi] as i64)) {
                            heightmap[cz * 16 + cx] = (i * 16 + cy + 1) as i32;
                            continue 'col;
                        }
                    }
                }
            }
        }
        if let Some(l) = level_mut(root) {
            l.insert("HeightMap".into(), Tag::IntArray(heightmap.to_vec()));
        }
        return;
    }
    if dv < chunk::DV_18W06A {
        // 1451-1465: palette path writing int HeightMap
        let names = legacy_names();
        let m = |n: &str| {
            let bare = n.strip_prefix("minecraft:").unwrap_or(n);
            !names.contains(bare) && !names.contains(n)
        };
        if let Some(heightmap) = heightmap_raw_pass(root, dv, &m) {
            if let Some(l) = level_mut(root) {
                l.insert(
                    "HeightMap".into(),
                    Tag::IntArray(heightmap.iter().map(|&v| v as i32).collect()),
                );
            }
        }
        return;
    }
    if dv < chunk::DV_18W19A {
        // 18w06a: LIGHT + RAIN maps
        let (light, rain) = hm_18w06a();
        if let Some(d) = heightmap_pass(root, dv, &|n: &str| light.contains(n)) {
            set_heightmap(root, "LIGHT", d);
        }
        if let Some(d) = heightmap_pass(root, dv, &|n: &str| rain.contains(n)) {
            set_heightmap(root, "RAIN", d);
        }
        return;
    }
    let cfg = hm_cfg(dv);
    if dv < chunk::DV_1_13_PRE3 {
        // 18w19a..1_13_PRE2: worldSurface -> LIGHT_BLOCKING
        if let Some(d) =
            heightmap_pass(root, dv, &|n: &str| cfg.light_blocking.contains(n))
        {
            set_heightmap(root, "LIGHT_BLOCKING", d);
        }
    } else if dv < chunk::DV_18W45A {
        // 1_13_PRE3..18w43a: both LIGHT_BLOCKING and WORLD_SURFACE
        if let Some(d) =
            heightmap_pass(root, dv, &|n: &str| cfg.world_surface.contains(n))
        {
            set_heightmap(root, "LIGHT_BLOCKING", d.clone());
            set_heightmap(root, "WORLD_SURFACE", d);
        }
    } else {
        if let Some(d) =
            heightmap_pass(root, dv, &|n: &str| cfg.world_surface.contains(n))
        {
            set_heightmap(root, "WORLD_SURFACE", d);
        }
    }
    if let Some(d) = heightmap_pass(root, dv, &|n: &str| cfg.ocean_floor.contains(n)) {
        set_heightmap(root, "OCEAN_FLOOR", d);
    }
    if let Some(d) =
        heightmap_pass(root, dv, &|n: &str| cfg.motion_blocking.contains(n))
    {
        set_heightmap(root, "MOTION_BLOCKING", d);
    }
    let leaves = if cfg.leaves.is_empty() {
        &cfg.motion_blocking
    } else {
        &cfg.leaves
    };
    if let Some(d) = heightmap_pass(root, dv, &|n: &str| leaves.contains(n)) {
        set_heightmap(root, "MOTION_BLOCKING_NO_LEAVES", d);
    }
}

/// raw-heights variant of heightmap_pass (for the int[256] legacy target).
fn heightmap_raw_pass(
    root: &Compound,
    dv: i32,
    matcher: &dyn Fn(&str) -> bool,
) -> Option<Vec<i16>> {
    let secs = chunk::sections_dv(root, dv)?;
    let mut pals: Vec<(usize, Vec<Tag>, Vec<i64>)> = Vec::new();
    for t in secs {
        let Some(s) = t.as_compound() else { continue };
        let y = section_y(s);
        if !(0..=15).contains(&y) {
            continue;
        }
        let p = s.get("Palette").and_then(as_list).cloned();
        let b = s.get("BlockStates").and_then(as_long_array).cloned();
        if let (Some(p), Some(b)) = (p, b) {
            pals.push((y as usize, p, b));
        }
    }
    pals.sort_by_key(|(i, _, _)| *i);
    Some(compute_raw_heights(&pals, matcher, dv).to_vec())
}

// --------------------------------------------------------- replace blocks

/// find palette index of `state` (deep-equal), else append.
fn palette_index_of(palette: &mut Vec<Tag>, state: &Tag) -> usize {
    for (i, p) in palette.iter().enumerate() {
        if p == state {
            return i;
        }
    }
    palette.push(state.clone());
    palette.len() - 1
}

fn palette_contains_air(palette: &[Tag]) -> bool {
    palette
        .iter()
        .any(|t| chunk::is_air_name(chunk::palette_entry_name(t)))
}

fn air_entry(dv: i32) -> Tag {
    let mut c = Compound::default();
    if dv >= chunk::DV_26_3_S7 {
        c.insert("".into(), Tag::String("minecraft:air".into()));
    } else {
        c.insert("Name".into(), Tag::String("minecraft:air".into()));
    }
    Tag::Compound(c)
}

/// upstream cleanupPalette: drop unused entries, ensure air, repack bits.
fn cleanup_palette(states: Vec<i64>, palette: &mut Vec<Tag>, dv: i32) -> Vec<i64> {
    let mut used: HashMap<i64, i64> = HashMap::new();
    for i in 0..4096 {
        let pi = get_pi(i, &states, dv);
        used.insert(pi, pi);
    }
    let mut old_index = 0i64;
    let mut i = 0;
    while i < palette.len() {
        if !used.contains_key(&old_index) {
            palette.remove(i);
        } else {
            used.insert(old_index, i as i64);
            i += 1;
        }
        old_index += 1;
    }
    if !palette_contains_air(palette) {
        palette.push(air_entry(dv));
    }
    adjust_bits(palette.len(), states, Some(&used), dv)
}

fn read_block_states<'a>(
    section: &'a Compound,
    dv: i32,
) -> Option<(&'a Vec<Tag>, Option<Vec<i64>>)> {
    if dv >= chunk::DV_21W37A {
        let bs = section.get("block_states")?.as_compound()?;
        let p = bs.get("palette").and_then(as_list)?;
        let d = bs.get("data").and_then(as_long_array).cloned();
        Some((p, d))
    } else {
        let p = section.get("Palette").and_then(as_list)?;
        let d = section.get("BlockStates").and_then(as_long_array).cloned();
        Some((p, d))
    }
}

fn palette_mut<'a>(section: &'a mut Compound, dv: i32) -> Option<&'a mut Vec<Tag>> {
    if dv >= chunk::DV_21W37A {
        let bs = section.get_mut("block_states")?.as_compound_mut()?;
        as_list_mut(bs.get_mut("palette")?)
    } else {
        as_list_mut(section.get_mut("Palette")?)
    }
}

fn write_block_states(section: &mut Compound, dv: i32, states: Vec<i64>) {
    if dv >= chunk::DV_21W37A {
        if let Some(bs) = section
            .get_mut("block_states")
            .and_then(|t| t.as_compound_mut())
        {
            bs.insert("data".into(), Tag::LongArray(states));
        }
    } else {
        section.insert("BlockStates".into(), Tag::LongArray(states));
    }
}

fn complete_section(s: &mut Compound, y: i32, dv: i32) {
    s.insert("Y".into(), Tag::Byte(y as i8));
    if dv >= chunk::DV_21W37A {
        if !s.contains_key("block_states") {
            s.insert("block_states".into(), Tag::Compound(Compound::default()));
        }
        if let Some(bs) = s
            .get_mut("block_states")
            .and_then(|t| t.as_compound_mut())
        {
            if !bs.contains_key("data") {
                bs.insert("data".into(), Tag::LongArray(vec![0; 256]));
            }
            if !bs.contains_key("palette") {
                bs.insert("palette".into(), Tag::List(crate::nbt::TAG_COMPOUND, vec![air_entry(dv)]));
            }
        }
        if !s.contains_key("biomes") {
            let mut biomes = Compound::default();
            biomes.insert(
                "palette".into(),
                Tag::List(crate::nbt::TAG_STRING, vec![Tag::String("minecraft:the_void".into())]),
            );
            biomes.insert("data".into(), Tag::LongArray(vec![0]));
            s.insert("biomes".into(), Tag::Compound(biomes));
        }
    } else {
        if !s.contains_key("BlockStates") {
            s.insert("BlockStates".into(), Tag::LongArray(vec![0; 256]));
        }
        if !s.contains_key("Palette") {
            s.insert("Palette".into(), Tag::List(crate::nbt::TAG_COMPOUND, vec![air_entry(dv)]));
        }
    }
}

fn replace_blocks(root: &mut Compound, replace: &[(String, BlockReplace)]) {
    let dv = nbt::data_version(Some(root));
    if dv < chunk::DV_17W47A {
        replace_blocks_legacy(root, replace);
        return;
    }
    let Some(scope_dv) = (if dv >= chunk::DV_21W43A {
        Some(root as &Compound)
    } else {
        level(root)
    }) else {
        return;
    };
    let pos = nbt::int_from_opt(Some(scope_dv), "xPos")
        .zip(nbt::int_from_opt(Some(scope_dv), "zPos"));
    let Some((cx, cz)) = pos else { return };
    let base_x = cx * 16;
    let base_z = cz * 16;
    let skey = chunk::sections_key(dv);
    let Some(secs_ref) = scope_dv.get(skey).and_then(as_list) else {
        return;
    };
    let Some(range) = find_section_range_from(scope_dv, secs_ref) else {
        return;
    };

    // ---- mutable phase on the section list
    let tiles_key = if dv >= chunk::DV_21W43A {
        "block_entities"
    } else {
        "TileEntities"
    };
    // tile-entity ops deferred until after the sections borrow ends:
    // (x, y, z, tile-to-add-or-None-to-remove)
    let mut tile_ops: Vec<(i32, i32, i32, Option<Compound>)> = Vec::new();

    {
        let Some(c) = chunk::data_container_mut(root, dv) else {
            return;
        };
        let Some(secs) = as_list_mut(c.get_mut(skey).unwrap()) else {
            return;
        };

        // air-fill pass (replace contains "minecraft:air")
        if replace.iter().any(|(k, _)| k == "minecraft:air") {
            let mut map: HashMap<i64, Compound> = HashMap::new();
            for t in secs.drain(..) {
                if let Tag::Compound(s) = t {
                    map.insert(section_y(&s), s);
                }
            }
            for y in range.0..=range.1 {
                let y = y as i64;
                match map.get_mut(&y) {
                    None => {
                        let mut s = Compound::default();
                        complete_section(&mut s, y as i32, dv);
                        map.insert(y, s);
                    }
                    Some(s) => {
                        let missing = if dv >= chunk::DV_21W37A {
                            !s.contains_key("block_states")
                        } else {
                            !s.contains_key("Palette")
                        };
                        if missing {
                            complete_section(s, y as i32, dv);
                        }
                    }
                }
            }
            let mut heights: Vec<i64> = map.keys().copied().collect();
            heights.sort();
            *secs = heights
                .into_iter()
                .filter_map(|h| map.remove(&h).map(Tag::Compound))
                .collect();
        }

        for st in secs.iter_mut() {
            let Some(section) = st.as_compound_mut() else {
                continue;
            };
            let y = nbt::int_from(Some(section), "Y", range.0 - 1);
            if !(range.0..=range.1).contains(&y) {
                continue;
            }
            let Some((palette0, states0)) = read_block_states(section, dv) else {
                continue;
            };
            let mut states = match states0 {
                Some(s) => s,
                None if palette0.len() == 1 => vec![0i64; 256],
                None => continue, // upstream would NPE; skip
            };
            // collect edits first (immutable borrow of palette)
            let mut edits: Vec<(usize, usize, Tag)> = Vec::new(); // (block idx, replace idx, new state)
            for i in 0..4096 {
                let pi = get_pi(i, &states, dv);
                let Some(entry) = palette0.get(pi as usize) else {
                    continue;
                };
                let name = pal_name(entry);
                for (ri, (from, r)) in replace.iter().enumerate() {
                    if name == from.as_str() {
                        edits.push((i, ri, Tag::Compound(r.get_state(dv))));
                        break;
                    }
                }
            }
            if edits.is_empty() {
                continue;
            }
            // apply edits, growing the packed array when the palette crosses a
            // power-of-two boundary (upstream setBlockAt)
            {
                let Some(palette) = palette_mut(section, dv) else {
                    continue;
                };
                for (i, _, state) in &edits {
                    let pi = palette_index_of(palette, state);
                    if pi != 0 && pi & (pi - 1) == 0 {
                        states = adjust_bits(palette.len(), states, None, dv);
                    }
                    set_pi(*i, pi as i64, &mut states, dv);
                }
            }
            for (i, ri, _) in &edits {
                let (x, yy, z) = chunk::index_to_location(*i);
                let loc = (base_x + x, y * 16 + yy, base_z + z);
                tile_ops.push((loc.0, loc.1, loc.2, replace[*ri].1.tile.clone()));
            }
            let states = {
                let Some(palette) = palette_mut(section, dv) else {
                    continue;
                };
                cleanup_palette(states, palette, dv)
            };
            write_block_states(section, dv, states);
        }
    }

    // ---- deferred tile-entity updates on Level.TileEntities / root.block_entities
    if tile_ops.is_empty() {
        return;
    }
    let Some(c) = chunk::data_container_mut(root, dv) else {
        return;
    };
    if !matches!(c.get(tiles_key), Some(Tag::List(..))) {
        c.insert(tiles_key.into(), Tag::List(crate::nbt::TAG_COMPOUND, Vec::new()));
    }
    let Some(te) = c.get_mut(tiles_key).and_then(as_list_mut) else {
        return;
    };
    for (x, y, z, tile) in tile_ops {
        if let Some(mut t) = tile {
            t.insert("x".into(), Tag::Int(x));
            t.insert("y".into(), Tag::Int(y));
            t.insert("z".into(), Tag::Int(z));
            te.push(Tag::Compound(t));
        } else {
            te.retain(|t| {
                let Some(tc) = t.as_compound() else { return true };
                !(nbt::int_from(Some(tc), "x", 0) == x
                    && nbt::int_from(Some(tc), "y", 0) == y
                    && nbt::int_from(Some(tc), "z", 0) == z)
            });
        }
    }
}

fn legacy_block_map() -> &'static HashMap<String, Vec<(i16, HashSet<i8>)>> {
    static M: OnceLock<HashMap<String, Vec<(i16, HashSet<i8>)>>> = OnceLock::new();
    M.get_or_init(|| {
        let mut m: HashMap<String, Vec<(i16, HashSet<i8>)>> = HashMap::new();
        for line in
            include_str!("../resources/mapping/java_1_9/block_name_to_id.csv").lines()
        {
            let mut p = line.trim().split(';');
            let (Some(name), Some(id)) = (p.next(), p.next()) else {
                continue;
            };
            let Ok(id) = id.parse::<i16>() else { continue };
            let data: HashSet<i8> = p
                .next()
                .unwrap_or("")
                .split(',')
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse::<i8>().ok())
                .collect();
            m.entry(format!("minecraft:{name}"))
                .or_default()
                .push((id, data));
        }
        m
    })
}

fn nibble_get(arr: &[u8], i: usize) -> i8 {
    let b = arr[i >> 1];
    if i & 1 == 0 {
        (b & 0xF) as i8
    } else {
        ((b >> 4) & 0xF) as i8
    }
}

fn nibble_set(arr: &mut [u8], i: usize, v: i8) {
    let j = i >> 1;
    if i & 1 == 0 {
        arr[j] = (arr[j] & 0xF0) | (v as u8 & 0xF);
    } else {
        arr[j] = (arr[j] & 0x0F) | (((v as u8) << 4) & 0xF0);
    }
}

/// legacy (<1451) Level.Sections Blocks[4096]/Data[2048] replacement.
fn replace_blocks_legacy(root: &mut Compound, replace: &[(String, BlockReplace)]) {
    let mapping = legacy_block_map();
    let replaces_air = replace.iter().any(|(k, _)| k == "minecraft:air");
    let Some(l) = level_mut(root) else { return };
    if replaces_air {
        if !l.contains_key("Sections") {
            l.insert("Sections".into(), Tag::List(crate::nbt::TAG_COMPOUND, Vec::new()));
        }
        if let Some(secs) = l.get_mut("Sections").and_then(as_list_mut) {
            let present: HashSet<i64> = secs
                .iter()
                .filter_map(|t| t.as_compound().map(|s| section_y(s)))
                .collect();
            for y in 0..16i64 {
                if !present.contains(&y) {
                    let mut s = Compound::default();
                    s.insert("Y".into(), Tag::Byte(y as i8));
                    s.insert("Blocks".into(), Tag::ByteArray(vec![0; 4096]));
                    s.insert("Data".into(), Tag::ByteArray(vec![0; 2048]));
                    secs.push(Tag::Compound(s));
                }
            }
        }
    }
    let Some(secs) = l.get_mut("Sections").and_then(as_list_mut) else {
        return;
    };
    for st in secs.iter_mut() {
        let Some(s) = st.as_compound_mut() else { continue };
        let mut data_arr: Option<Vec<u8>> = s.get("Data").and_then(as_byte_array).cloned();
        let Some(blocks) = s.get_mut("Blocks").and_then(|t| match t {
            Tag::ByteArray(v) => Some(v),
            _ => None,
        }) else {
            continue;
        };
        for (from, r) in replace {
            let Some(variants) = mapping.get(from) else {
                continue;
            };
            // target id: first variant of the target name
            let Some((tid, tdset)) = mapping.get(&r.target_name()).and_then(|v| v.first())
            else {
                continue;
            };
            let tid = *tid;
            let tdata = tdset.iter().next().copied().unwrap_or(0);
            for i in 0..4096usize {
                let id = blocks[i] as i16;
                let meta = data_arr.as_ref().map(|d| nibble_get(d, i)).unwrap_or(0);
                for (vid, vdata) in variants {
                    if *vid == id && (vdata.is_empty() || vdata.contains(&meta)) {
                        blocks[i] = tid as u8;
                        if let Some(d) = data_arr.as_mut() {
                            nibble_set(d, i, tdata);
                        }
                        break;
                    }
                }
            }
        }
        if let Some(d) = data_arr {
            s.insert("Data".into(), Tag::ByteArray(d));
        }
    }
    // remove tile entities of replaced blocks
    let names: HashSet<String> = replace.iter().map(|(k, _)| k.clone()).collect();
    if let Some(te) = l.get_mut("TileEntities").and_then(as_list_mut) {
        te.retain(|t| {
            let Some(c) = t.as_compound() else { return true };
            let id = format!("minecraft:{}", nbt::string_or(Some(c), "id", ""));
            !names.contains(&id)
        });
    }
}

// ------------------------------------------------------------------ custom

/// set `path` (dotted, `*` iterates list elements) to `tag`.
/// change mode: only when every step resolves; force: create intermediates.
fn apply_custom(root: &mut Compound, path: &str, tag: &Tag, force: bool) {
    let segs: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return;
    }
    custom_walk(root, segs[0], &segs[1..], tag, force);
}

fn custom_walk(scope: &mut Compound, key: &str, rest: &[&str], tag: &Tag, force: bool) {
    if rest.is_empty() {
        if force || scope.contains_key(key) {
            scope.insert(key.into(), tag.clone());
        }
        return;
    }
    if !scope.contains_key(key) {
        if !force {
            return;
        }
        let intermediate = if rest[0] == "*" || rest[0].parse::<usize>().is_ok() {
            Tag::List(crate::nbt::TAG_COMPOUND, Vec::new())
        } else {
            Tag::Compound(Compound::default())
        };
        scope.insert(key.into(), intermediate);
    }
    let Some(t) = scope.get_mut(key) else {
        return;
    };
    custom_walk_tag(t, rest, tag, force);
}

fn custom_walk_tag(t: &mut Tag, segs: &[&str], tag: &Tag, force: bool) {
    let Some((seg, rest)) = segs.split_first() else {
        *t = tag.clone();
        return;
    };
    match t {
        Tag::Compound(c) => custom_walk(c, seg, rest, tag, force),
        Tag::List(_, l) => {
            if *seg == "*" {
                for e in l.iter_mut() {
                    custom_walk_tag(e, rest, tag, force);
                }
            } else if let Ok(i) = seg.parse::<usize>() {
                if i < l.len() {
                    custom_walk_tag(&mut l[i], rest, tag, force);
                }
            }
        }
        _ => {
            if force {
                let intermediate = if *seg == "*" || seg.parse::<usize>().is_ok() {
                    Tag::List(crate::nbt::TAG_COMPOUND, Vec::new())
                } else {
                    Tag::Compound(Compound::default())
                };
                *t = intermediate;
                custom_walk_tag(t, segs, tag, force);
            }
        }
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::Point2i;

    fn chunk(data: Compound) -> crate::mca::Chunk {
        let mut c = crate::mca::Chunk::new(Point2i::new(0, 0));
        c.data = Some(data);
        c
    }

    fn modern_root() -> Compound {
        // minimal 2844+ chunk root
        let mut s0 = Compound::default();
        s0.insert("Y".into(), Tag::Byte(0));
        let mut bs = Compound::default();
        bs.insert("palette".into(), Tag::List(crate::nbt::TAG_COMPOUND, vec![air_entry(2844)]));
        bs.insert("data".into(), Tag::LongArray(vec![0; 256]));
        s0.insert("block_states".into(), Tag::Compound(bs));
        s0.insert("BlockLight".into(), Tag::ByteArray(vec![0; 2048]));
        s0.insert("SkyLight".into(), Tag::ByteArray(vec![0; 2048]));
        let mut r = Compound::default();
        r.insert("DataVersion".into(), Tag::Int(3700));
        r.insert("xPos".into(), Tag::Int(0));
        r.insert("zPos".into(), Tag::Int(0));
        r.insert("Status".into(), Tag::String("full".into()));
        r.insert("isLightOn".into(), Tag::Byte(1));
        r.insert("sections".into(), Tag::List(crate::nbt::TAG_COMPOUND, vec![Tag::Compound(s0)]));
        r
    }

    #[test]
    fn parse_basic() {
        let f = parse_fields("DataVersion = 3700, Status = \"minecraft:full\"").unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].field_type, FieldType::DataVersion);
        assert_eq!(f[0].value, "3700");
        assert_eq!(f[1].field_type, FieldType::Status);
        assert_eq!(f[1].value, "minecraft:full");
    }

    #[test]
    fn parse_unquoted_namespaced_value_fails_like_upstream() {
        // upstream ChangeParser only accepts [a-zA-Z0-9-+] in unquoted values
        assert!(parse_fields("Status = minecraft:full").is_err());
    }

    #[test]
    fn parse_errors() {
        assert!(parse_fields("Foo = 1").is_err());
        assert!(parse_fields("DataVersion = -1").is_err());
        assert!(parse_fields("LightPopulated = 2").is_err());
        assert!(parse_fields("DeleteEntities = yes").is_err());
        assert!(parse_fields("DataVersion 3700").is_err());
    }

    #[test]
    fn parse_ranges_quoted() {
        let f = parse_fields("DeleteSections = \"1,5:7,12:\"").unwrap();
        let FieldValue::Ranges(r) = &f[0].parsed else {
            panic!()
        };
        assert_eq!(r[0], (1, 1));
        assert_eq!(r[1], (5, 7));
        assert_eq!(r[2], (12, i32::MAX));
        let f = parse_fields("DeleteSections = true").unwrap();
        let FieldValue::Ranges(r) = &f[0].parsed else {
            panic!()
        };
        assert_eq!(r[0], (i32::MIN, i32::MAX));
    }

    #[test]
    fn apply_dataversion_force_and_change() {
        let fields = parse_fields("DataVersion = 3800").unwrap();
        let mut c = chunk(modern_root());
        let r = apply_change_region(&mut c, &fields, false);
        assert_eq!(r.applied, 1);
        assert_eq!(c.data.as_ref().unwrap()["DataVersion"], Tag::Int(3800));
        // missing tag: change no-ops, force writes
        let mut c2 = chunk(modern_root());
        c2.data.as_mut().unwrap().swap_remove("DataVersion");
        apply_change_region(&mut c2, &fields, false);
        assert!(!c2.data.as_ref().unwrap().contains_key("DataVersion"));
        apply_change_region(&mut c2, &fields, true);
        assert_eq!(c2.data.as_ref().unwrap()["DataVersion"], Tag::Int(3800));
    }

    #[test]
    fn apply_status_modern() {
        // dv 3700 >= 3458 -> namespaced status value written at root
        let fields = parse_fields("Status = \"minecraft:empty\"").unwrap();
        let mut c = chunk(modern_root());
        apply_change_region(&mut c, &fields, false);
        assert_eq!(
            c.data.as_ref().unwrap()["Status"],
            Tag::String("minecraft:empty".into())
        );
        assert!(requires_clear_cache(&fields));
        // dv 3000 < 3458 -> bare status value
        let mut c = chunk(modern_root());
        c.data.as_mut().unwrap().insert("DataVersion".into(), Tag::Int(3000));
        apply_change_region(&mut c, &fields, false);
        assert_eq!(
            c.data.as_ref().unwrap()["Status"],
            Tag::String("empty".into())
        );
    }

    #[test]
    fn apply_custom_change_and_force() {
        let fields = parse_fields("Custom = \"foo.bar = 42\"").unwrap();
        let mut c = chunk(modern_root());
        apply_change_region(&mut c, &fields, false);
        assert!(!c.data.as_ref().unwrap().contains_key("foo"));
        apply_change_region(&mut c, &fields, true);
        let Tag::Compound(f) = &c.data.as_ref().unwrap()["foo"] else {
            panic!()
        };
        assert_eq!(f.get("bar"), Some(&Tag::Int(42)));
    }

    #[test]
    fn remove_lighting_modern() {
        let fields = parse_fields("RemoveLightingInfo = true").unwrap();
        let mut c = chunk(modern_root());
        apply_change_region(&mut c, &fields, false);
        let d = c.data.as_ref().unwrap();
        assert_eq!(d["isLightOn"], Tag::Byte(0));
        let Tag::List(_, secs) = &d["sections"] else {
            panic!()
        };
        let s = secs[0].as_compound().unwrap();
        assert!(!s.contains_key("BlockLight"));
        assert!(!s.contains_key("SkyLight"));
    }

    #[test]
    fn delete_sections_modern() {
        let fields = parse_fields("DeleteSections = 0").unwrap();
        let mut c = chunk(modern_root());
        // dv 3700 >= 3458 -> namespaced Status values
        c.data
            .as_mut()
            .unwrap()
            .insert("Status".into(), Tag::String("minecraft:full".into()));
        apply_change_region(&mut c, &fields, false);
        let d = c.data.as_ref().unwrap();
        // status was "minecraft:full" -> "minecraft:features", section emptied
        assert_eq!(d["Status"], Tag::String("minecraft:features".into()));
        let Tag::List(_, secs) = &d["sections"] else {
            panic!()
        };
        let s = secs[0].as_compound().unwrap();
        let bs = s.get("block_states").unwrap().as_compound().unwrap();
        assert!(!bs.contains_key("data"));
        assert!(!s.contains_key("BlockLight"));
    }

    #[test]
    fn delete_entities_legacy() {
        let mut level = Compound::default();
        let mut e = Compound::default();
        e.insert("id".into(), Tag::String("minecraft:pig".into()));
        e.insert(
            "Pos".into(),
            Tag::List(6, vec![Tag::Double(0.0), Tag::Double(64.0), Tag::Double(0.0)]),
        );
        level.insert("Entities".into(), Tag::List(crate::nbt::TAG_COMPOUND, vec![Tag::Compound(e)]));
        level.insert("DataVersion".into(), Tag::Int(1400));
        let mut root = Compound::default();
        root.insert("DataVersion".into(), Tag::Int(1400));
        root.insert("Level".into(), Tag::Compound(level));
        let mut c = chunk(root);
        let fields = parse_fields("DeleteEntities = true").unwrap();
        apply_change_region(&mut c, &fields, true);
        let d = c.data.as_ref().unwrap();
        let lvl = d["Level"].as_compound().unwrap();
        let Tag::List(_, ents) = &lvl["Entities"] else {
            panic!()
        };
        assert!(ents.is_empty());
    }

    #[test]
    fn compression_field_sets_metadata() {
        let fields = parse_fields("Compression = zlib").unwrap();
        let mut c = chunk(modern_root());
        apply_change_region(&mut c, &fields, false);
        assert_eq!(c.compression, CompressionType::Zlib);
        let fields = parse_fields("Compression = -127").unwrap();
        let mut c = chunk(modern_root());
        apply_change_region(&mut c, &fields, false);
        assert_eq!(c.compression, CompressionType::GzipExt);
    }
}
