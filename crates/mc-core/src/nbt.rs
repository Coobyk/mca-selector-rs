//! Port of net.querz.nbt: tag types, binary read/write, SNBT.
//!
//! `ByteArray` values may also carry raw big-endian bytes of IntArray /
//! LongArray payloads when parsed with `raw_arrays` — the renderers read
//! them back as BE i32/i64 without a decoding pass, like upstream's
//! `rawArrays(true)` reader.

use indexmap::IndexMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{self, Read, Write};

pub const TAG_END: u8 = 0;
pub const TAG_BYTE: u8 = 1;
pub const TAG_SHORT: u8 = 2;
pub const TAG_INT: u8 = 3;
pub const TAG_LONG: u8 = 4;
pub const TAG_FLOAT: u8 = 5;
pub const TAG_DOUBLE: u8 = 6;
pub const TAG_BYTE_ARRAY: u8 = 7;
pub const TAG_STRING: u8 = 8;
pub const TAG_LIST: u8 = 9;
pub const TAG_COMPOUND: u8 = 10;
pub const TAG_INT_ARRAY: u8 = 11;
pub const TAG_LONG_ARRAY: u8 = 12;

/// FxHash-family hasher for compound keys. NBT keys are short, trusted
/// strings hashed on every parse insert and every render-time `get` —
/// SipHash's DoS resistance isn't needed for local world data.
#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.hash = (self.hash.rotate_left(5) ^ w).wrapping_mul(FX_SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        // extra multiply so the high bits hashbrown uses for SIMD tags
        // carry entropy even for short keys
        self.hash.wrapping_mul(FX_SEED)
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut b = bytes;
        while b.len() >= 8 {
            self.add(u64::from_ne_bytes(b[..8].try_into().unwrap()));
            b = &b[8..];
        }
        if b.len() >= 4 {
            self.add(u64::from(u32::from_ne_bytes(b[..4].try_into().unwrap())));
            b = &b[4..];
        }
        for &x in b {
            self.add(u64::from(x));
        }
    }
    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.add(u64::from(n));
    }
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }
    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }
}

/// BuildHasher for [`FxHasher`].
pub type FxBuildHasher = BuildHasherDefault<FxHasher>;

pub type Compound = IndexMap<String, Tag, FxBuildHasher>;

#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// Raw bytes: either an NBT byte array or a raw-encoded int/long array.
    ByteArray(Vec<u8>),
    String(String),
    /// (element tag type, items)
    List(u8, Vec<Tag>),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn id(&self) -> u8 {
        match self {
            Tag::Byte(_) => TAG_BYTE,
            Tag::Short(_) => TAG_SHORT,
            Tag::Int(_) => TAG_INT,
            Tag::Long(_) => TAG_LONG,
            Tag::Float(_) => TAG_FLOAT,
            Tag::Double(_) => TAG_DOUBLE,
            Tag::ByteArray(_) => TAG_BYTE_ARRAY,
            Tag::String(_) => TAG_STRING,
            Tag::List(_, _) => TAG_LIST,
            Tag::Compound(_) => TAG_COMPOUND,
            Tag::IntArray(_) => TAG_INT_ARRAY,
            Tag::LongArray(_) => TAG_LONG_ARRAY,
        }
    }

    /// True when this ByteArray is actually raw long[] contents (set by
    /// raw_arrays parsing); the renderers rely on this.
    pub fn as_compound(&self) -> Option<&Compound> {
        match self {
            Tag::Compound(c) => Some(c),
            _ => None,
        }
    }
    pub fn as_compound_mut(&mut self) -> Option<&mut Compound> {
        match self {
            Tag::Compound(c) => Some(c),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<(u8, &Vec<Tag>)> {
        match self {
            Tag::List(t, l) => Some((*t, l)),
            _ => None,
        }
    }
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Tag::Byte(v) => Some(*v as i64),
            Tag::Short(v) => Some(*v as i64),
            Tag::Int(v) => Some(*v as i64),
            Tag::Long(v) => Some(*v as i64),
            _ => None,
        }
    }
    pub fn as_i32(&self) -> Option<i32> {
        self.as_i64().map(|v| v as i32)
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Tag::Byte(v) => Some(*v as f64),
            Tag::Short(v) => Some(*v as f64),
            Tag::Int(v) => Some(*v as f64),
            Tag::Long(v) => Some(*v as f64),
            Tag::Float(v) => Some(*v as f64),
            Tag::Double(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Tag::ByteArray(b) => Some(b),
            Tag::IntArray(v) => {
                // shouldn't happen in raw mode, but keep safe
                let _ = v;
                None
            }
            Tag::LongArray(_) => None,
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Compound helpers mirroring net.querz.mcaselector.version.Helper




pub fn int_from(c: Option<&Compound>, key: &str, def: i32) -> i32 {
    c.and_then(|c| c.get(key))
        .and_then(|t| t.as_i32())
        .unwrap_or(def)
}
pub fn int_from_opt(c: Option<&Compound>, key: &str) -> Option<i32> {
    c.and_then(|c| c.get(key)).and_then(|t| t.as_i32())
}
pub fn long_from_opt(c: Option<&Compound>, key: &str) -> Option<i64> {
    c.and_then(|c| c.get(key)).and_then(|t| t.as_i64())
}

pub fn string_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<&'a str> {
    c.and_then(|c| c.get(key)).and_then(|t| t.as_string())
}
pub fn string_or<'a>(c: Option<&'a Compound>, key: &str, def: &'a str) -> &'a str {
    string_from(c, key).unwrap_or(def)
}

/// sections list: returns None when missing or when it is a LongArray
/// (Helper.getSectionsFromCompound).
pub fn sections_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<(u8, &'a Vec<Tag>)> {
    match c.and_then(|c| c.get(key)) {
        Some(Tag::List(t, l)) => Some((*t, l)),
        _ => None,
    }
}

pub fn byte_array_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<&'a [u8]> {
    c.and_then(|c| c.get(key)).and_then(|t| t.as_bytes())
}



/// Level wrapper for pre-1.18 chunks (Helper.levelFromRoot).
/// format-agnostic section-list locator: `Sections`/`sections` at root or
/// under `Level`, in that priority order (all call sites need "any sections").
pub fn section_tags<'a>(root: &'a Compound) -> Option<&'a Vec<Tag>> {
    let level = level_from_root(root).unwrap_or(root);
    for (c, k) in [(level, "Sections"), (root, "sections"), (level, "sections")] {
        if let Some((_, l)) = c.get(k).and_then(Tag::as_list) {
            return Some(l);
        }
    }
    None
}

pub fn level_from_root<'a>(root: &'a Compound) -> Option<&'a Compound> {
    root.get("Level").and_then(Tag::as_compound)
}

/// Helper.getDataVersion
pub fn data_version(root: Option<&Compound>) -> i32 {
    let Some(root) = root else { return 100 };
    if !root.contains_key("DataVersion") {
        if let Some(level) = root.get("Level").and_then(Tag::as_compound) {
            return int_from_opt(Some(level), "DataVersion").unwrap_or(100);
        }
    }
    int_from(Some(root), "DataVersion", 100)
}

/// Read a BE i64 out of a raw byte slice (raw long[] data).
#[inline]

#[inline]
pub fn raw_i64_or(data: &[u8], idx: usize, def: i64) -> i64 {
    let o = idx * 8;
    if o + 8 <= data.len() {
        i64::from_be_bytes(data[o..o + 8].try_into().unwrap())
    } else {
        def
    }
}
#[inline]

#[inline]
pub fn raw_i32_or(data: &[u8], idx: usize, def: i32) -> i32 {
    let o = idx * 4;
    if o + 4 <= data.len() {
        i32::from_be_bytes(data[o..o + 4].try_into().unwrap())
    } else {
        def
    }
}

// ---------------------------------------------------------------------------
// Binary IO (big endian, Java DataInputStream semantics incl. modified UTF-8
// for name strings — MC never produces the long-form anyway)

pub struct NbtReader<R: Read> {
    r: R,
    pub raw_arrays: bool,
}

impl<R: Read> NbtReader<R> {
    pub fn new(r: R) -> Self {
        NbtReader { r, raw_arrays: false }
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.r.read_exact(buf)
    }
    fn u8(&mut self) -> io::Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn i8(&mut self) -> io::Result<i8> {
        Ok(self.u8()? as i8)
    }
    fn u16(&mut self) -> io::Result<u16> {
        let mut b = [0u8; 2];
        self.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }
    fn i16(&mut self) -> io::Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn i32(&mut self) -> io::Result<i32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(i32::from_be_bytes(b))
    }
    fn i64(&mut self) -> io::Result<i64> {
        let mut b = [0u8; 8];
        self.read_exact(&mut b)?;
        Ok(i64::from_be_bytes(b))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.i32()? as u32))
    }
    fn f64(&mut self) -> io::Result<f64> {
        Ok(f64::from_bits(self.i64()? as u64))
    }
    fn bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.read_exact(&mut v)?;
        Ok(v)
    }
    fn string(&mut self) -> io::Result<String> {
        let len = self.u16()? as usize;
        let raw = self.bytes(len)?;
        // String::from_utf8 reuses `raw`'s buffer on success (the common
        // case) — one allocation instead of copy-into-new-String.
        match String::from_utf8(raw) {
            Ok(s) => Ok(s),
            Err(e) => Ok(String::from_utf8_lossy(&e.into_bytes()).into_owned()),
        }
    }

    

    /// Reads the root tag; upstream NBTReader.read returns the root tag itself.
    pub fn read(&mut self) -> io::Result<Tag> {
        let ty = self.u8()?;
        if ty != TAG_COMPOUND {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected compound root, got {ty}"),
            ));
        }
        let _name = self.string()?;
        self.payload(TAG_COMPOUND)
    }

    pub fn payload(&mut self, ty: u8) -> io::Result<Tag> {
        Ok(match ty {
            TAG_BYTE => Tag::Byte(self.i8()?),
            TAG_SHORT => Tag::Short(self.i16()?),
            TAG_INT => Tag::Int(self.i32()?),
            TAG_LONG => Tag::Long(self.i64()?),
            TAG_FLOAT => Tag::Float(self.f32()?),
            TAG_DOUBLE => Tag::Double(self.f64()?),
            TAG_BYTE_ARRAY => {
                let n = self.i32()? as usize;
                Tag::ByteArray(self.bytes(n)?)
            }
            TAG_STRING => Tag::String(self.string()?),
            TAG_LIST => {
                let et = self.u8()?;
                let n = self.i32()?;
                let n = if n < 0 { 0 } else { n as usize };
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.payload(et)?);
                }
                Tag::List(et, items)
            }
            TAG_COMPOUND => {
                let mut map =
                    Compound::with_capacity_and_hasher(8, FxBuildHasher::default());
                loop {
                    let t = self.u8()?;
                    if t == TAG_END {
                        break;
                    }
                    let name = self.string()?;
                    let v = self.payload(t)?;
                    map.insert(name, v);
                }
                Tag::Compound(map)
            }
            TAG_INT_ARRAY => {
                let n = self.i32()? as usize;
                if self.raw_arrays {
                    Tag::ByteArray(self.bytes(n * 4)?)
                } else {
                    // one bulk read + word-wise BE decode instead of n reads
                    let raw = self.bytes(n * 4)?;
                    let mut v = Vec::with_capacity(n);
                    for c in raw.chunks_exact(4) {
                        v.push(i32::from_be_bytes(c.try_into().unwrap()));
                    }
                    Tag::IntArray(v)
                }
            }
            TAG_LONG_ARRAY => {
                let n = self.i32()? as usize;
                if self.raw_arrays {
                    Tag::ByteArray(self.bytes(n * 8)?)
                } else {
                    let raw = self.bytes(n * 8)?;
                    let mut v = Vec::with_capacity(n);
                    for c in raw.chunks_exact(8) {
                        v.push(i64::from_be_bytes(c.try_into().unwrap()));
                    }
                    Tag::LongArray(v)
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid tag type {ty}"),
                ))
            }
        })
    }
}

/// Serialize a named root tag (always writes a compound root like NBTWriter).
pub fn write_nbt<W: Write>(w: &mut W, name: &str, tag: &Tag) -> io::Result<()> {
    w.write_all(&[TAG_COMPOUND])?;
    w.write_all(&(name.len() as u16).to_be_bytes())?;
    w.write_all(name.as_bytes())?;
    write_payload(w, tag)
}

/// Serialize a compound borrowed as the root tag — same byte layout as
/// `write_nbt(w, name, &Tag::Compound(c))` without owning a Tag.
pub fn write_root_compound<W: Write>(w: &mut W, name: &str, c: &Compound) -> io::Result<()> {
    w.write_all(&[TAG_COMPOUND])?;
    w.write_all(&(name.len() as u16).to_be_bytes())?;
    w.write_all(name.as_bytes())?;
    write_compound_entries(w, c)
}

fn wstr<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    w.write_all(&(s.len() as u16).to_be_bytes())?;
    w.write_all(s.as_bytes())
}

fn write_payload<W: Write>(w: &mut W, tag: &Tag) -> io::Result<()> {
    match tag {
        Tag::Byte(v) => w.write_all(&[*v as u8]),
        Tag::Short(v) => w.write_all(&v.to_be_bytes()),
        Tag::Int(v) => w.write_all(&v.to_be_bytes()),
        Tag::Long(v) => w.write_all(&v.to_be_bytes()),
        Tag::Float(v) => w.write_all(&v.to_be_bytes()),
        Tag::Double(v) => w.write_all(&v.to_be_bytes()),
        Tag::ByteArray(b) => {
            // raw_arrays load turns int/long arrays into ByteArray too — those
            // paths are read-only (rendering). writing them back would emit a
            // byte[] tag with the wrong element width.
            w.write_all(&(b.len() as i32).to_be_bytes())?;
            w.write_all(b)
        }
        Tag::String(s) => wstr(w, s),
        Tag::List(et, items) => {
            // empty lists are written as TAG_End type like the Java lib? Java
            // writes the declared type; keep it.
            w.write_all(&[*et])?;
            w.write_all(&(items.len() as i32).to_be_bytes())?;
            for it in items {
                write_payload(w, it)?;
            }
            Ok(())
        }
        Tag::Compound(c) => write_compound_entries(w, c),
        Tag::IntArray(v) => {
            w.write_all(&(v.len() as i32).to_be_bytes())?;
            let mut b = Vec::with_capacity(v.len() * 4);
            for i in v {
                b.extend_from_slice(&i.to_be_bytes());
            }
            w.write_all(&b)
        }
        Tag::LongArray(v) => {
            w.write_all(&(v.len() as i32).to_be_bytes())?;
            let mut b = Vec::with_capacity(v.len() * 8);
            for i in v {
                b.extend_from_slice(&i.to_be_bytes());
            }
            w.write_all(&b)
        }
    }
}

fn write_compound_entries<W: Write>(w: &mut W, c: &Compound) -> io::Result<()> {
    for (k, v) in c {
        w.write_all(&[v.id()])?;
        wstr(w, k)?;
        write_payload(w, v)?;
    }
    w.write_all(&[TAG_END])
}

// ---------------------------------------------------------------------------
// SNBT (NBTUtil.toSNBT equivalent + a permissive parser for the editor)

pub fn to_snbt(tag: &Tag) -> String {
    let mut s = String::new();
    write_snbt(tag, &mut s);
    s
}

fn write_snbt(tag: &Tag, out: &mut String) {
    match tag {
        Tag::Byte(v) => out.push_str(&format!("{v}b")),
        Tag::Short(v) => out.push_str(&format!("{v}s")),
        Tag::Int(v) => out.push_str(&format!("{v}")),
        Tag::Long(v) => out.push_str(&format!("{v}l")),
        Tag::Float(v) => out.push_str(&format!("{v}f")),
        Tag::Double(v) => out.push_str(&format!("{v}d")),
        Tag::ByteArray(b) => {
            out.push_str("[B;");
            for (i, v) in b.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!("{}", *v as i8));
            }
            out.push(']');
        }
        Tag::String(s) => {
            out.push('"');
            for c in s.chars() {
                if c == '"' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
        }
        Tag::List(et, items) => {
            out.push('[');
            match *et {
                TAG_BYTE_ARRAY => {
                    // stored as raw bytes
                }
                _ => {}
            }
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_snbt(it, out);
            }
            out.push(']');
        }
        Tag::Compound(c) => {
            out.push('{');
            for (i, (k, v)) in c.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_snbt_name(k, out);
                out.push(':');
                write_snbt(v, out);
            }
            out.push('}');
        }
        Tag::IntArray(v) => {
            out.push_str("[I;");
            for (i, x) in v.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&x.to_string());
            }
            out.push(']');
        }
        Tag::LongArray(v) => {
            out.push_str("[L;");
            for (i, x) in v.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&x.to_string());
            }
            out.push(']');
        }
    }
}

fn write_snbt_name(name: &str, out: &mut String) {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '+');
    if plain {
        out.push_str(name);
    } else {
        out.push('"');
        for c in name.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
    }
}

// ---------------------------------------------------------------------------
// SNBT parser (port of net.querz.nbt.TagFactory.fromSNBT semantics)

pub fn parse_snbt(s: &str) -> Result<Tag, String> {
    let mut p = SnbtParser { b: s.as_bytes(), i: 0 };
    p.skip_ws();
    let t = p.value()?;
    p.skip_ws();
    if p.i != p.b.len() {
        return Err(format!("trailing characters at {}", p.i));
    }
    Ok(t)
}

struct SnbtParser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> SnbtParser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at {}", c as char, self.i))
        }
    }

    fn value(&mut self) -> Result<Tag, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.compound(),
            Some(b'[') => self.list_or_array(),
            Some(b'"') | Some(b'\'') => Ok(Tag::String(self.quoted()?)),
            Some(_) => self.scalar(),
            None => Err("unexpected end of input".into()),
        }
    }

    fn quoted(&mut self) -> Result<String, String> {
        let q = self.b[self.i];
        self.i += 1;
        let mut out = String::new();
        while self.i < self.b.len() {
            let c = self.b[self.i];
            self.i += 1;
            if c == q {
                return Ok(out);
            }
            if c == b'\\' && self.i < self.b.len() {
                let e = self.b[self.i];
                self.i += 1;
                out.push(match e {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    _ => e as char,
                });
            } else {
                // raw utf8 continuation: push byte-wise as char only for ascii;
                // rebuild via lossy for multibyte
                if c < 0x80 {
                    out.push(c as char);
                } else {
                    // collect the full utf8 sequence we already consumed partially
                    let start = self.i - 1;
                    let len = utf8_len(c);
                    let end = (start + len).min(self.b.len());
                    let s = std::str::from_utf8(&self.b[start..end]).unwrap_or("\u{FFFD}");
                    out.push_str(s);
                    self.i = end;
                }
            }
        }
        Err("unterminated string".into())
    }

    fn name(&mut self) -> Result<String, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'"') | Some(b'\'') => self.quoted(),
            _ => {
                let start = self.i;
                while self.i < self.b.len()
                    && !matches!(self.b[self.i], b':' | b',' | b'}' | b' ' | b'\t' | b'\n' | b'\r')
                {
                    self.i += 1;
                }
                Ok(std::str::from_utf8(&self.b[start..self.i])
                    .unwrap_or("")
                    .to_string())
            }
        }
    }

    fn compound(&mut self) -> Result<Tag, String> {
        self.eat(b'{')?;
        let mut c = Compound::default();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Tag::Compound(c));
        }
        loop {
            let name = self.name()?;
            self.eat(b':')?;
            let v = self.value()?;
            c.insert(name, v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Tag::Compound(c));
                }
                _ => return Err(format!("expected ',' or '}}' at {}", self.i)),
            }
        }
    }

    fn list_or_array(&mut self) -> Result<Tag, String> {
        self.eat(b'[')?;
        self.skip_ws();
        // typed arrays: [B; ...] [I; ...] [L; ...]
        if self.i + 1 < self.b.len() && self.b[self.i + 1] == b';' {
            let t = self.b[self.i];
            self.i += 2;
            let vals = self.number_list(b']')?;
            return match t {
                b'B' => Ok(Tag::ByteArray(vals.iter().map(|v| *v as i8 as u8).collect())),
                b'I' => Ok(Tag::IntArray(vals.iter().map(|v| *v as i32).collect())),
                b'L' => Ok(Tag::LongArray(vals.clone())),
                _ => Err(format!("invalid array type '{}'", t as char)),
            };
        }
        // plain list
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Tag::List(0, items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    let ty = items.first().map(Tag::id).unwrap_or(0);
                    return Ok(Tag::List(ty, items));
                }
                _ => return Err(format!("expected ',' or ']' at {}", self.i)),
            }
        }
    }

    fn number_list(&mut self, end: u8) -> Result<Vec<i64>, String> {
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(end) {
            self.i += 1;
            return Ok(out);
        }
        loop {
            self.skip_ws();
            let start = self.i;
            while self.i < self.b.len()
                && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
            {
                self.i += 1;
            }
            let tok = std::str::from_utf8(&self.b[start..self.i]).unwrap_or("");
            // strip optional type suffix
            let tok2 = tok.trim_end_matches(|c: char| c.is_ascii_alphabetic());
            let v: i64 = if tok2.contains('.') || tok2.contains('e') || tok2.contains('E') {
                tok2.parse::<f64>().map_err(|_| format!("bad number '{tok}'"))? as i64
            } else {
                tok2.parse::<i64>().map_err(|_| format!("bad number '{tok}'"))?
            };
            out.push(v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(c) if c == end => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => return Err(format!("expected ',' at {}", self.i)),
            }
        }
    }

    fn scalar(&mut self) -> Result<Tag, String> {
        let start = self.i;
        while self.i < self.b.len()
            && !matches!(self.b[self.i], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r' | b':')
        {
            self.i += 1;
        }
        let tok = std::str::from_utf8(&self.b[start..self.i]).unwrap_or("");
        snbt_scalar(tok)
    }
}

/// parse a scalar SNBT token: typed suffix (b/s/l/f/d) or inferred int/double/string.
pub fn snbt_scalar(tok: &str) -> Result<Tag, String> {
    if tok.is_empty() {
        return Err("empty value".into());
    }
    if let Some(v) = tok.strip_suffix('b').or_else(|| tok.strip_suffix('B')) {
        if let Ok(n) = v.parse::<i8>() {
            return Ok(Tag::Byte(n));
        }
    }
    if let Some(v) = tok.strip_suffix('s').or_else(|| tok.strip_suffix('S')) {
        if let Ok(n) = v.parse::<i16>() {
            return Ok(Tag::Short(n));
        }
    }
    if let Some(v) = tok.strip_suffix('l').or_else(|| tok.strip_suffix('L')) {
        if let Ok(n) = v.parse::<i64>() {
            return Ok(Tag::Long(n));
        }
    }
    if let Some(v) = tok.strip_suffix('f').or_else(|| tok.strip_suffix('F')) {
        if let Ok(n) = v.parse::<f32>() {
            return Ok(Tag::Float(n));
        }
    }
    if let Some(v) = tok.strip_suffix('d').or_else(|| tok.strip_suffix('D')) {
        if let Ok(n) = v.parse::<f64>() {
            return Ok(Tag::Double(n));
        }
    }
    if tok == "true" {
        return Ok(Tag::Byte(1));
    }
    if tok == "false" {
        return Ok(Tag::Byte(0));
    }
    if let Ok(n) = tok.parse::<i32>() {
        return Ok(Tag::Int(n));
    }
    if let Ok(n) = tok.parse::<f64>() {
        return Ok(Tag::Double(n));
    }
    Ok(Tag::String(tok.to_string()))
}

fn utf8_len(b: u8) -> usize {
    if b >= 0xF0 {
        4
    } else if b >= 0xE0 {
        3
    } else if b >= 0xC0 {
        2
    } else {
        1
    }
}



// ---------------------------------------------------------------------------
// path mutation: "a.b[0].c" (names and [index] list access)

/// split "a.b[0].c" into path segments: names and indices
fn path_segments(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in path.split('.') {
        let mut rest = part;
        while let Some(br) = rest.find('[') {
            if br > 0 {
                out.push(rest[..br].to_string());
            }
            let end = rest.find(']').unwrap_or(rest.len());
            out.push(rest[br + 1..end.min(rest.len())].to_string());
            rest = &rest[end.min(rest.len() - 1) + 1..];
        }
        if !rest.is_empty() {
            out.push(rest.to_string());
        }
    }
    out
}

/// navigate to a tag by path (immutable)
pub fn path_get<'a>(root: &'a Compound, path: &str) -> Option<&'a Tag> {
    let mut segs = path_segments(path);
    if segs.is_empty() {
        return None;
    }
    let first = segs.remove(0);
    let mut cur = root.get(&first)?;
    for seg in segs {
        match cur {
            Tag::Compound(c) => cur = c.get(&seg)?,
            Tag::List(_, l) => cur = l.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        }
    }
    Some(cur)
}

/// navigate to a tag by path (mutable)
pub fn path_get_mut<'a>(root: &'a mut Compound, path: &str) -> Option<&'a mut Tag> {
    let mut segs = path_segments(path);
    if segs.is_empty() {
        return None;
    }
    let first = segs.remove(0);
    let mut cur = root.get_mut(&first)?;
    for seg in segs {
        cur = match cur {
            Tag::Compound(c) => c.get_mut(&seg)?,
            Tag::List(_, l) => l.get_mut(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// remove a tag by path
pub fn path_remove(root: &mut Compound, path: &str) -> bool {
    let mut segs = path_segments(path);
    let Some(last) = segs.pop() else { return false };
    // navigate to parent container
    let parent_path = segs.join(".");
    let parent = if segs.is_empty() {
        None
    } else {
        path_get_mut(root, &parent_path)
    };
    match parent {
        None => root.shift_remove(&last).is_some(),
        Some(Tag::Compound(c)) => c.shift_remove(&last).is_some(),
        Some(Tag::List(_, l)) => match last.parse::<usize>() {
            Ok(i) if i < l.len() => {
                l.remove(i);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// insert `tag` under container at `path` with `name` (compound) or index (list)
pub fn path_insert(root: &mut Compound, path: &str, name: &str, tag: Tag) -> Result<(), String> {
    let container = if path.is_empty() {
        None
    } else {
        Some(path_get_mut(root, path).ok_or_else(|| format!("no tag at '{path}'"))?)
    };
    match container {
        None => {
            root.insert(name.to_string(), tag);
            Ok(())
        }
        Some(Tag::Compound(c)) => {
            c.insert(name.to_string(), tag);
            Ok(())
        }
        Some(Tag::List(_, l)) => {
            let i = name.parse::<usize>().unwrap_or(l.len()).min(l.len());
            l.insert(i, tag);
            Ok(())
        }
        _ => Err("target is not a container".into()),
    }
}


/// set the leaf tag at a dotted path, creating intermediate compounds.
/// `TAG_END` value deletes the leaf (upstream ChangeNbt "delete" semantics).
pub fn path_set(c: &mut Compound, path: &str, val: Tag) {
    let mut parts = path.split('.').peekable();
    let mut cur: &mut Compound = c;
    while let Some(p) = parts.next() {
        if parts.peek().is_none() {
            if val.id() == TAG_END {
                cur.remove(p);
            } else {
                cur.insert(p.to_string(), val.clone());
            }
            return;
        }
        let entry = cur
            .entry(p.to_string())
            .or_insert_with(|| Tag::Compound(Default::default()));
        match entry {
            Tag::Compound(cc) => cur = cc,
            _ => {
                *entry = Tag::Compound(Default::default());
                if let Tag::Compound(cc) = entry {
                    cur = cc;
                } else {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod snbt_tests {
    use super::*;

    #[test]
    fn snbt_roundtrip() {
        let src = r#"{a: 5b, b: "hello", c: {x: 1, y: 2.5d}, l: [1b, 2b], ia: [I; 3, 4], s: 'q'}"#;
        let t = parse_snbt(src).unwrap();
        let Tag::Compound(c) = &t else { panic!("not compound") };
        assert_eq!(c.get("a"), Some(&Tag::Byte(5)));
        assert_eq!(c.get("b"), Some(&Tag::String("hello".into())));
        assert_eq!(c.get("s"), Some(&Tag::String("q".into())));
        let Tag::List(_, l) = c.get("l").unwrap() else { panic!() };
        assert_eq!(l.len(), 2);
        assert_eq!(c.get("ia"), Some(&Tag::IntArray(vec![3, 4])));
        // roundtrip reparse
        let s2 = to_snbt(&t);
        let t2 = parse_snbt(&s2).unwrap();
        assert_eq!(t, t2);
    }

    #[test]
    fn snbt_scalars() {
        assert_eq!(parse_snbt("42").unwrap(), Tag::Int(42));
        assert_eq!(parse_snbt("42l").unwrap(), Tag::Long(42));
        assert_eq!(parse_snbt("1.5").unwrap(), Tag::Double(1.5));
        assert_eq!(parse_snbt("true").unwrap(), Tag::Byte(1));
        assert_eq!(parse_snbt("\"minecraft:stone\"").unwrap(), Tag::String("minecraft:stone".into()));
        assert!(parse_snbt("minecraft:stone").is_err()); // ':' not allowed unquoted
    }

    #[test]
    fn path_mutations() {
        let mut c = Compound::default();
        c.insert("a".into(), Tag::Compound({
            let mut x = Compound::default();
            x.insert("b".into(), Tag::Int(7));
            x.insert("l".into(), Tag::List(1, vec![Tag::Byte(1), Tag::Byte(2)]));
            x
        }));
        assert_eq!(path_get(&c, "a.b").unwrap().as_i64(), Some(7));
        assert_eq!(path_get(&c, "a.l[1]").unwrap().as_i64(), Some(2));
        *path_get_mut(&mut c, "a.b").unwrap() = Tag::Int(9);
        assert_eq!(path_get(&c, "a.b").unwrap().as_i64(), Some(9));
        path_insert(&mut c, "a", "n", Tag::String("v".into())).unwrap();
        assert!(path_get(&c, "a.n").is_some());
        assert!(path_remove(&mut c, "a.b"));
        assert!(path_get(&c, "a.b").is_none());
        assert!(path_remove(&mut c, "a.l[0]"));
        assert!(path_get(&c, "a.l[1]").is_none());
    }
}
