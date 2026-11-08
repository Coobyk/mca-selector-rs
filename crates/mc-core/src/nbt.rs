//! Port of net.querz.nbt: tag types, binary read/write, SNBT.
//!
//! `ByteArray` values may also carry raw big-endian bytes of IntArray /
//! LongArray payloads when parsed with `raw_arrays` — the renderers read
//! them back as BE i32/i64 without a decoding pass, like upstream's
//! `rawArrays(true)` reader.

use indexmap::IndexMap;
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

pub type Compound = IndexMap<String, Tag>;

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

pub fn compound_get<'a>(c: &'a Compound, key: &str) -> Option<&'a Tag> {
    c.get(key)
}
pub fn compound_get_mut<'a>(c: &'a mut Compound, key: &str) -> Option<&'a mut Tag> {
    c.get_mut(key)
}
pub fn tag_compound<'a>(t: Option<&'a Tag>, key: &str) -> Option<&'a Tag> {
    t.and_then(Tag::as_compound)?.get(key)
}
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
pub fn byte_from_opt(c: Option<&Compound>, key: &str) -> Option<i8> {
    c.and_then(|c| c.get(key)).and_then(|t| match t {
        Tag::Byte(b) => Some(*b),
        _ => t.as_i64().map(|v| v as i8),
    })
}
pub fn string_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<&'a str> {
    c.and_then(|c| c.get(key)).and_then(|t| t.as_string())
}
pub fn string_or<'a>(c: Option<&'a Compound>, key: &str, def: &'a str) -> &'a str {
    string_from(c, key).unwrap_or(def)
}
pub fn list_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<(u8, &'a Vec<Tag>)> {
    c.and_then(|c| c.get(key)).and_then(Tag::as_list)
}
/// sections list: returns None when missing or when it is a LongArray
/// (Helper.getSectionsFromCompound).
pub fn sections_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<(u8, &'a Vec<Tag>)> {
    match c.and_then(|c| c.get(key)) {
        Some(Tag::List(t, l)) => Some((*t, l)),
        _ => None,
    }
}
pub fn compound_list<'a>(l: &'a (u8, Vec<Tag>)) -> impl Iterator<Item = &'a Compound> {
    l.1.iter().filter_map(|t| t.as_compound())
}
pub fn byte_array_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<&'a [u8]> {
    c.and_then(|c| c.get(key)).and_then(|t| t.as_bytes())
}
pub fn long_array_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<&'a [i64]> {
    match c.and_then(|c| c.get(key)) {
        Some(Tag::LongArray(v)) => Some(v),
        _ => None,
    }
}
pub fn int_array_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<&'a [i32]> {
    match c.and_then(|c| c.get(key)) {
        Some(Tag::IntArray(v)) => Some(v),
        _ => None,
    }
}

/// Level wrapper for pre-1.18 chunks (Helper.levelFromRoot).
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
pub fn raw_i64(data: &[u8], idx: usize) -> i64 {
    let o = idx * 8;
    i64::from_be_bytes(data[o..o + 8].try_into().unwrap())
}
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
pub fn raw_i32(data: &[u8], idx: usize) -> i32 {
    let o = idx * 4;
    i32::from_be_bytes(data[o..o + 4].try_into().unwrap())
}
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
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }

    /// Reads a named root tag; returns (name, tag). Errors if root is not a compound.
    pub fn read_named(&mut self) -> io::Result<(String, Tag)> {
        let ty = self.u8()?;
        if ty == TAG_END {
            return Ok((String::new(), Tag::Compound(Compound::default())));
        }
        let name = self.string()?;
        let tag = self.payload(ty)?;
        Ok((name, tag))
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
                let mut map = Compound::default();
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
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push(self.i32()?);
                    }
                    Tag::IntArray(v)
                }
            }
            TAG_LONG_ARRAY => {
                let n = self.i32()? as usize;
                if self.raw_arrays {
                    Tag::ByteArray(self.bytes(n * 8)?)
                } else {
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push(self.i64()?);
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
        Tag::Compound(c) => {
            for (k, v) in c {
                w.write_all(&[v.id()])?;
                wstr(w, k)?;
                write_payload(w, v)?;
            }
            w.write_all(&[TAG_END])
        }
        Tag::IntArray(v) => {
            w.write_all(&(v.len() as i32).to_be_bytes())?;
            for i in v {
                w.write_all(&i.to_be_bytes())?;
            }
            Ok(())
        }
        Tag::LongArray(v) => {
            w.write_all(&(v.len() as i32).to_be_bytes())?;
            for i in v {
                w.write_all(&i.to_be_bytes())?;
            }
            Ok(())
        }
    }
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
