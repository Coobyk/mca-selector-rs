/// Zero-copy NBT reader for the chunk fields the map needs.
//
// This deliberately replaces serde-based parsing on the hot path: a chunk
// NBT carries megabytes of data we never look at, and skipping it by tag
// arithmetic is an order of magnitude faster than deserializing it.

use std::convert::TryInto;

use crate::packed::PackedBits;

/// A section's block palette (names only; properties don't affect map
/// colors) and its packed block data.
pub struct FastSection {
    pub y: i8,
    /// Block names, `minecraft:` prefix stripped.
    pub names: Vec<Box<str>>,
    pub data: Option<Vec<i64>>,
    /// Biome palette (4x4x4 cells per section), names with namespace.
    pub biomes: Option<FastBiomes>,
}

/// Section biome storage: a palette of biome names plus the packed 4x4x4
/// cell data (index `(y&15)>>2 * 16 + (z&15)>>2 * 4 + (x&15)>>2`).
#[derive(Debug, Clone)]
pub struct FastBiomes {
    pub palette: Vec<Box<str>>,
    pub data: Option<Vec<i64>>,
}

impl FastBiomes {
    /// Biome name at block coordinates local to the section.
    pub fn at(&self, x: usize, y: usize, z: usize) -> &str {
        let cell = ((y & 15) >> 2) * 16 + ((z & 15) >> 2) * 4 + ((x & 15) >> 2);
        match &self.data {
            None => &self.palette[0],
            Some(data) => {
                let bits = self.bits();
                let packed = PackedBits::new(data, bits);
                let idx = packed.get(cell) as usize;
                self.palette.get(idx).map(|s| &**s).unwrap_or("")
            }
        }
    }

    fn bits(&self) -> u32 {
        // Vanilla biome palettes: ceil(log2(len)) with a 1-bit minimum.
        let len = self.palette.len().max(2);
        ((usize::BITS - (len - 1).leading_zeros()).max(1)).min(16)
    }
}

/// The subset of a chunk NBT used for map rendering.
pub struct FastChunk {
    pub data_version: i32,
    pub x_pos: i32,
    pub z_pos: i32,
    pub sections: Vec<FastSection>,
    /// Heightmaps of interest, 37 packed longs each (9 bits per column).
    pub motion_blocking: Option<Vec<i64>>,
    pub ocean_floor: Option<Vec<i64>>,
}

/// Parse a decompressed chunk NBT payload. Returns `None` for malformed
/// data; unknown fields are skipped, not errors.
///
/// Falls back to a full serde parse for the rare chunks whose encoding the
/// fast reader does not cover yet (a few tenths of a percent).
pub fn parse_chunk(bytes: &[u8]) -> Option<FastChunk> {
    match parse_chunk_traced(bytes) {
        Ok(c) => Some(c),
        Err(reason) => {
            if std::env::var_os("MC_CORE_PARSE_DEBUG").is_some() {
                eprintln!("fast parse failed ({reason}); using fallback");
            }
            parse_chunk_slow(bytes)
        }
    }
}

/// Reference implementation over `fastnbt::Value` for encodings the fast
/// reader misses. Not performance sensitive: this path is rare.
fn parse_chunk_slow(bytes: &[u8]) -> Option<FastChunk> {
    let root = fastnbt::from_bytes::<std::collections::BTreeMap<String, fastnbt::Value>>(bytes)
        .ok()?;
    let sections_val = root.get("sections")?;
    let mut sections = Vec::new();
    if let fastnbt::Value::List(list) = sections_val {
        for s in list {
            let fastnbt::Value::Compound(c) = s else {
                continue;
            };
            let y = match c.get("Y") {
                Some(fastnbt::Value::Byte(y)) => *y,
                _ => continue,
            };
            let Some(fastnbt::Value::Compound(bs)) = c.get("block_states") else {
                continue;
            };
            let mut names = Vec::new();
            match bs.get("palette") {
                Some(fastnbt::Value::List(entries)) => {
                    for e in entries {
                        let name = match e {
                            fastnbt::Value::String(s) => s.clone(),
                            fastnbt::Value::Compound(ec) => {
                                let pick = |k: &str| match ec.get(k) {
                                    Some(fastnbt::Value::String(s)) => Some(s.clone()),
                                    _ => None,
                                };
                                pick("Name")
                                    .or_else(|| pick("id"))
                                    .or_else(|| pick(""))?
                            }
                            _ => continue,
                        };
                        names.push(strip_prefix(&name).into());
                    }
                }
                _ => continue,
            }
            let data = match bs.get("data") {
                Some(fastnbt::Value::LongArray(a)) => Some(a.clone().into_inner()),
                _ => None,
            };
            sections.push(FastSection { y, names, data, biomes: None });
        }
    }
    let longs = |k: &str| match root.get("Heightmaps") {
        Some(fastnbt::Value::Compound(hm)) => match hm.get(k) {
            Some(fastnbt::Value::LongArray(a)) => Some(a.clone().into_inner()),
            _ => None,
        },
        _ => None,
    };
    let int = |k: &str| match root.get(k) {
        Some(fastnbt::Value::Int(v)) => *v,
        _ => 0,
    };
    Some(FastChunk {
        data_version: int("DataVersion"),
        x_pos: int("xPos"),
        z_pos: int("zPos"),
        sections,
        motion_blocking: longs("MOTION_BLOCKING"),
        ocean_floor: longs("OCEAN_FLOOR"),
    })
}

/// Like [`parse_chunk`] but reports where parsing diverged.
pub fn parse_chunk_traced(bytes: &[u8]) -> Result<FastChunk, String> {
    let mut r = R {
        b: bytes,
        p: 0,
        depth: 0,
        fail: None,
    };
    let chunk = parse_loop(&mut r);
    match chunk {
        Some(c) => Ok(c),
        None => Err(match r.fail {
            Some((pos, msg)) => format!("at byte {pos}: {msg}"),
            None => format!("at byte {}: unknown parse failure", r.p),
        }),
    }
}

fn parse_loop(r: &mut R) -> Option<FastChunk> {
    // Root: TAG_Compound + (usually empty) name.
    if r.u8()? != 10 {
        if r.fail.is_none() {
            r.fail = Some((0, "root is not a compound".into()));
        }
        return None;
    }
    let name_len = r.u16()? as usize;
    r.take(name_len)?;

    let mut chunk = FastChunk {
        data_version: 0,
        x_pos: 0,
        z_pos: 0,
        sections: Vec::new(),
        motion_blocking: None,
        ocean_floor: None,
    };

    loop {
        let tag = r.u8()?;
        if tag == 0 {
            break;
        }
        let name = r.name()?;
        match (name, tag) {
            ("DataVersion", 3) => chunk.data_version = r.i32()?,
            ("xPos", 3) => chunk.x_pos = r.i32()?,
            ("zPos", 3) => chunk.z_pos = r.i32()?,
            ("sections", 9) => parse_sections(r, &mut chunk)?,
            ("Heightmaps", 10) => parse_heightmaps(r, &mut chunk)?,
            _ => r.skip_payload(tag)?,
        }
    }
    Some(chunk)
}

/// Re-run the parse to collect the failure reason (used by diagnostics).
pub fn parse_failure_reason(bytes: &[u8]) -> Option<String> {
    let mut r = R {
        b: bytes,
        p: 0,
        depth: 0,
        fail: None,
    };
    run_parse(&mut r)?;
    r.fail.map(|(pos, msg)| format!("at byte {pos}: {msg}"))
}

fn run_parse(r: &mut R) -> Option<()> {
    parse_loop(r).map(|_| ())
}

fn parse_sections(r: &mut R, chunk: &mut FastChunk) -> Option<()> {
    let elem = r.u8()?;
    let len = r.i32()?;
    if len <= 0 {
        return Some(());
    }
    for _ in 0..len {
        if elem == 0 {
            return None;
        }
        if elem != 10 {
            r.skip_payload(elem)?;
            continue;
        }
        parse_section(r, chunk)?;
    }
    Some(())
}

fn parse_section(r: &mut R, chunk: &mut FastChunk) -> Option<()> {
    let mut y: i8 = 0;
    let mut names: Option<Vec<Box<str>>> = None;
    let mut data: Option<Vec<i64>> = None;
    let mut biomes: Option<FastBiomes> = None;
    loop {
        let tag = r.u8()?;
        if tag == 0 {
            break;
        }
        let name = r.name()?;
        match (name, tag) {
            ("Y", 1) => y = r.u8()? as i8,
            ("block_states", 10) => {
                parse_block_states(r, &mut names, &mut data)?;
            }
            ("biomes", 10) => {
                biomes = Some(parse_biomes(r)?);
            }
            _ => r.skip_payload(tag)?,
        }
    }
    if let Some(names) = names {
        chunk.sections.push(FastSection {
            y,
            names,
            data,
            biomes,
        });
    }
    Some(())
}

/// Section biome storage parser: `{palette: [names], data: longs}`.
fn parse_biomes(r: &mut R) -> Option<FastBiomes> {
    let mut palette: Option<Vec<Box<str>>> = None;
    let mut data: Option<Vec<i64>> = None;
    loop {
        let tag = r.u8()?;
        if tag == 0 {
            break;
        }
        let name = r.name()?;
        match (name, tag) {
            ("palette", 9) => {
                let elem = r.u8()?;
                let len = r.i32()?;
                if len < 0 {
                    return None;
                }
                let mut out = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    out.push(Box::from(r.string()?.as_str()));
                }
                palette = Some(out);
            }
            ("data", 12) => {
                data = Some(r.long_array()?);
            }
            _ => r.skip_payload(tag)?,
        }
    }
    Some(FastBiomes {
        palette: palette?,
        data,
    })
}

fn parse_block_states(
    r: &mut R,
    names: &mut Option<Vec<Box<str>>>,
    data: &mut Option<Vec<i64>>,
) -> Option<()> {
    loop {
        let tag = r.u8()?;
        if tag == 0 {
            break;
        }
        let name = r.name()?;
        match (name, tag) {
            ("palette", 9) => {
                *names = Some(parse_palette(r)?);
            }
            ("data", 12) => {
                *data = Some(r.long_array()?);
            }
            _ => r.skip_payload(tag)?,
        }
    }
    Some(())
}

/// Palette entries across formats: plain name strings (1.21.5+), compounds
/// `{Name}` (old), `{"": name, ...}` (snapshot), `{id, properties}` (new).
fn parse_palette(r: &mut R) -> Option<Vec<Box<str>>> {
    let elem = r.u8()?;
    let len = r.i32()?;
    if len < 0 {
        return None;
    }
    let mut out = Vec::with_capacity(len as usize);
    for _ in 0..len {
        let name = match elem {
            8 => Box::from(r.string()?.as_str()),
            10 => {
                let mut found: Option<Box<str>> = None;
                loop {
                    let tag = r.u8()?;
                    if tag == 0 {
                        break;
                    }
                    let field = r.name()?;
                    match (field, tag) {
                        ("Name", 8) | ("id", 8) | ("", 8) => {
                            found = Some(r.string()?.into());
                        }
                        ("Properties", 10) | ("properties", 10) => {
                            r.skip_payload(10)?;
                        }
                        _ => r.skip_payload(tag)?,
                    }
                }
                found?
            }
            _ => return None,
        };
        out.push(strip_prefix(&name).into());
    }
    Some(out)
}

fn strip_prefix(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn parse_heightmaps(r: &mut R, chunk: &mut FastChunk) -> Option<()> {
    loop {
        let tag = r.u8()?;
        if tag == 0 {
            break;
        }
        let name = r.name()?;
        match (name, tag) {
            ("MOTION_BLOCKING", 12) => chunk.motion_blocking = Some(r.long_array()?),
            ("OCEAN_FLOOR", 12) => chunk.ocean_floor = Some(r.long_array()?),
            _ => r.skip_payload(tag)?,
        }
    }
    Some(())
}

struct R<'a> {
    b: &'a [u8],
    p: usize,
    depth: u32,
    /// First failure: (position, reason), for diagnostics.
    fail: Option<(usize, String)>,
}

impl<'a> R<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.p)?;
        self.p += 1;
        Some(v)
    }

    fn u16(&mut self) -> Option<u16> {
        let v = self.take(2)?;
        Some(u16::from_be_bytes([v[0], v[1]]))
    }

    fn i32(&mut self) -> Option<i32> {
        let v = self.take(4)?;
        Some(i32::from_be_bytes([v[0], v[1], v[2], v[3]]))
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.p.saturating_add(n) > self.b.len() {
            if self.fail.is_none() {
                self.fail = Some((self.p, format!("read of {n} bytes overruns buffer")));
            }
            return None;
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Some(s)
    }

    fn name(&mut self) -> Option<&'a str> {
        let len = self.u16()? as usize;
        let s = self.take(len)?;
        std::str::from_utf8(s).ok()
    }

    fn string(&mut self) -> Option<String> {
        Some(self.name()?.to_string())
    }

    fn long_array(&mut self) -> Option<Vec<i64>> {
        let len = self.i32()?;
        if len < 0 {
            return None;
        }
        let bytes = self.take(len as usize * 8)?;
        Some(
            bytes
                .chunks_exact(8)
                .map(|c| i64::from_be_bytes(c.try_into().unwrap()))
                .collect(),
        )
    }

    /// Skip the payload of any tag. Recursion depth is bounded by NBT
    /// nesting, which is small in practice.
    fn skip_payload(&mut self, tag: u8) -> Option<()> {
        self.depth += 1;
        if self.depth > 32 {
            return None;
        }
        let r = (|| {
            match tag {
                1 | 5 => self.take(1).map(|_| ()),
                2 | 6 => self.take(2).map(|_| ()),
                3 => self.take(4).map(|_| ()),
                4 => self.take(8).map(|_| ()),
                7 => {
                    let len = self.i32()?;
                    if len < 0 {
                        return None;
                    }
                    self.take(len as usize).map(|_| ())
                }
                8 => {
                    let len = self.u16()? as usize;
                    self.take(len).map(|_| ())
                }
                9 => {
                    let elem = self.u8()?;
                    let len = self.i32()?;
                    if len < 0 {
                        return None;
                    }
                    for _ in 0..len {
                        if elem == 0 {
                            return None;
                        }
                        self.skip_payload(elem)?;
                    }
                    Some(())
                }
                10 => loop {
                    let tag = self.u8()?;
                    if tag == 0 {
                        return Some(());
                    }
                    let len = self.u16()? as usize;
                    self.take(len)?;
                    self.skip_payload(tag)?;
                },
                11 => {
                    let len = self.i32()?;
                    if len < 0 {
                        return None;
                    }
                    self.take(len as usize * 4).map(|_| ())
                }
                12 => {
                    let len = self.i32()?;
                    if len < 0 {
                        return None;
                    }
                    self.take(len as usize * 8).map(|_| ())
                }
                other => {
                    if self.fail.is_none() {
                        self.fail = Some((self.p, format!("unexpected tag {other}")));
                    }
                    None
                }
            }
        })();
        self.depth -= 1;
        r
    }
}
