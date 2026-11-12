//! Port of net.querz.nbt: tag types, binary read/write, SNBT.
//!
//! `ByteArray` values may also carry raw big-endian bytes of IntArray /
//! LongArray payloads when parsed with `raw_arrays` — the renderers read
//! them back as BE i32/i64 without a decoding pass, like upstream's
//! `rawArrays(true)` reader.

use std::hash::{BuildHasherDefault, Hasher};
use std::io::{self, Write};
use std::sync::OnceLock;

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

/// Insertion-ordered string → tag map: a flat Vec, not a hash map. NBT
/// compounds are tiny (palette entries have 1-3 keys, the chunk root ~25)
/// — a memcmp scan beats hashing+probing on lookups, and parse inserts
/// become a plain push. The API mirrors IndexMap: `insert` replaces in
/// place, iteration is insertion order, equality is order-insensitive.
///
/// A `Slot::Lazy` entry carries raw NBT payload bytes instead of a parsed
/// tag (the render-load path skips whole root subtrees). Every accessor
/// resolves it on first touch — `OnceLock` lets even shared `&` reads
/// materialize — so laziness is unobservable outside this type.
#[derive(Clone, Debug, Default)]
pub struct Compound {
    entries: Vec<(String, Slot)>,
}

#[derive(Debug)]
enum Slot {
    Tag(Tag),
    Lazy(LazyTag),
}

/// A named subtree's unresolved payload (type + raw bytes, no header).
/// Materializes on demand; only ever lives inside `Compound` slots.
struct LazyTag {
    ty: u8,
    /// the parser's raw_arrays mode at load — materialization must use it
    /// too so int/long arrays keep their raw-byte encoding.
    raw_arrays: bool,
    /// NBT payload bytes, tag type and name already consumed.
    bytes: Box<[u8]>,
    parsed: OnceLock<Box<Tag>>,
}

impl LazyTag {
    fn parse(&self) -> Tag {
        let mut r = SliceReader::new(&self.bytes);
        r.raw_arrays = self.raw_arrays;
        // the span was bounds/type-validated when it was skipped, so this
        // can't fail structurally; fall back defensively anyway.
        r.payload(self.ty)
            .unwrap_or_else(|_| Tag::Compound(Compound::default()))
    }
    #[inline]
    fn resolve(&self) -> &Tag {
        self.parsed.get_or_init(|| Box::new(self.parse()))
    }
    fn into_tag(self) -> Tag {
        let LazyTag { ty, raw_arrays, bytes, parsed } = self;
        match parsed.into_inner() {
            Some(t) => *t,
            None => {
                let mut r = SliceReader::new(&bytes);
                r.raw_arrays = raw_arrays;
                r.payload(ty)
                    .unwrap_or_else(|_| Tag::Compound(Compound::default()))
            }
        }
    }
}

impl Clone for LazyTag {
    fn clone(&self) -> Self {
        LazyTag {
            ty: self.ty,
            raw_arrays: self.raw_arrays,
            bytes: self.bytes.clone(),
            parsed: match self.parsed.get() {
                Some(t) => OnceLock::from(t.clone()),
                None => OnceLock::new(),
            },
        }
    }
}

impl std::fmt::Debug for LazyTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyTag")
            .field("ty", &self.ty)
            .field("bytes", &self.bytes.len())
            .field("resolved", &self.parsed.get().is_some())
            .finish()
    }
}

impl Clone for Slot {
    fn clone(&self) -> Self {
        match self {
            Slot::Tag(t) => Slot::Tag(t.clone()),
            Slot::Lazy(l) => Slot::Lazy(l.clone()),
        }
    }
}

impl Slot {
    /// borrow the parsed tag, materializing a lazy entry in place (through
    /// `&self` — the OnceLock stores it inside the slot).
    #[inline]
    fn resolve(&self) -> &Tag {
        match self {
            Slot::Tag(t) => t,
            Slot::Lazy(l) => l.resolve(),
        }
    }
    /// `&mut` resolve swaps the slot for the parsed tag outright.
    fn resolve_mut(&mut self) -> &mut Tag {
        if let Slot::Lazy(l) = self {
            *self = Slot::Tag(l.parse());
        }
        match self {
            Slot::Tag(t) => t,
            Slot::Lazy(_) => unreachable!(),
        }
    }
    fn into_tag(self) -> Tag {
        match self {
            Slot::Tag(t) => t,
            Slot::Lazy(l) => l.into_tag(),
        }
    }
}

impl Compound {
    pub fn new() -> Self {
        Compound { entries: Vec::new() }
    }
    pub fn with_capacity(n: usize) -> Self {
        Compound { entries: Vec::with_capacity(n) }
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn reserve(&mut self, n: usize) {
        self.entries.reserve(n);
    }

    #[inline]
    fn find(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|(k, _)| k.as_str() == key)
    }

    pub fn get<Q: AsRef<str> + ?Sized>(&self, key: &Q) -> Option<&Tag> {
        self.find(key.as_ref())
            .map(|i| self.entries[i].1.resolve())
    }
    pub fn get_mut<Q: AsRef<str> + ?Sized>(&mut self, key: &Q) -> Option<&mut Tag> {
        let i = self.find(key.as_ref())?;
        Some(self.entries[i].1.resolve_mut())
    }
    pub fn get_index(&self, index: usize) -> Option<(&String, &Tag)> {
        self.entries.get(index).map(|(k, s)| (k, s.resolve()))
    }
    pub fn get_index_mut(&mut self, index: usize) -> Option<(&String, &mut Tag)> {
        self.entries
            .get_mut(index)
            .map(|(k, s)| (&*k, s.resolve_mut()))
    }
    pub fn first(&self) -> Option<(&String, &Tag)> {
        self.get_index(0)
    }
    pub fn last(&self) -> Option<(&String, &Tag)> {
        self.get_index(self.len().saturating_sub(1))
    }
    pub fn contains_key<Q: AsRef<str> + ?Sized>(&self, key: &Q) -> bool {
        self.find(key.as_ref()).is_some()
    }

    /// IndexMap insert semantics: replace in place when the key exists,
    /// else append. Returns the old value.
    pub fn insert(&mut self, key: String, v: Tag) -> Option<Tag> {
        match self.find(&key) {
            Some(i) => Some(
                std::mem::replace(&mut self.entries[i].1, Slot::Tag(v)).into_tag(),
            ),
            None => {
                self.entries.push((key, Slot::Tag(v)));
                None
            }
        }
    }

    /// deferred-parse entry (SliceReader lazy_root mode); same
    /// replace/append semantics as `insert`.
    pub(crate) fn insert_lazy(&mut self, key: String, ty: u8, payload: &[u8], raw_arrays: bool) {
        let slot = Slot::Lazy(LazyTag {
            ty,
            raw_arrays,
            bytes: payload.into(),
            parsed: OnceLock::new(),
        });
        match self.find(&key) {
            Some(i) => self.entries[i].1 = slot,
            None => self.entries.push((key, slot)),
        }
    }

    /// IndexMap::swap_remove — removes the entry, moving the last into
    /// its slot.
    pub fn swap_remove<Q: AsRef<str> + ?Sized>(&mut self, key: &Q) -> Option<Tag> {
        self.find(key.as_ref())
            .map(|i| self.entries.swap_remove(i).1.into_tag())
    }
    /// IndexMap::shift_remove — removes preserving order.
    pub fn shift_remove<Q: AsRef<str> + ?Sized>(&mut self, key: &Q) -> Option<Tag> {
        self.find(key.as_ref())
            .map(|i| self.entries.remove(i).1.into_tag())
    }
    /// order-preserving remove (same as shift_remove).
    pub fn remove<Q: AsRef<str> + ?Sized>(&mut self, key: &Q) -> Option<Tag> {
        self.shift_remove(key)
    }
    pub fn pop(&mut self) -> Option<(String, Tag)> {
        self.entries.pop().map(|(k, s)| (k, s.into_tag()))
    }
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&String, &mut Tag) -> bool,
    {
        self.entries.retain_mut(|(k, s)| f(k, s.resolve_mut()));
    }
    pub fn entry(&mut self, key: String) -> Entry<'_> {
        match self.find(&key) {
            Some(index) => Entry::Occupied(OccupiedEntry { map: self, index }),
            None => Entry::Vacant(VacantEntry { map: self, key }),
        }
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter { inner: self.entries.iter() }
    }
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        IterMut { inner: self.entries.iter_mut() }
    }
    pub fn keys(&self) -> Keys<'_> {
        Keys { inner: self.entries.iter() }
    }
    pub fn values(&self) -> Values<'_> {
        Values { inner: self.entries.iter() }
    }
    pub fn values_mut(&mut self) -> ValuesMut<'_> {
        ValuesMut { inner: self.entries.iter_mut() }
    }
}

impl PartialEq for Compound {
    /// order-insensitive map equality, like IndexMap.
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(k, v)| other.get(k).map(|ov| ov == v).unwrap_or(false))
    }
}

/// IndexMap::entry counterpart.
pub enum Entry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a>),
}

pub struct OccupiedEntry<'a> {
    map: &'a mut Compound,
    index: usize,
}

pub struct VacantEntry<'a> {
    map: &'a mut Compound,
    key: String,
}

impl<'a> Entry<'a> {
    pub fn or_insert(self, default: Tag) -> &'a mut Tag {
        self.or_insert_with(|| default)
    }
    pub fn or_insert_with<F: FnOnce() -> Tag>(self, f: F) -> &'a mut Tag {
        match self {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(f()),
        }
    }
    pub fn and_modify<F: FnOnce(&mut Tag)>(self, f: F) -> Self {
        match self {
            Entry::Occupied(mut o) => {
                f(o.get_mut());
                Entry::Occupied(o)
            }
            e @ Entry::Vacant(_) => e,
        }
    }
    pub fn key(&self) -> &String {
        match self {
            Entry::Occupied(o) => o.key(),
            Entry::Vacant(v) => &v.key,
        }
    }
}

impl<'a> OccupiedEntry<'a> {
    pub fn key(&self) -> &String {
        &self.map.entries[self.index].0
    }
    pub fn get(&self) -> &Tag {
        self.map.entries[self.index].1.resolve()
    }
    pub fn get_mut(&mut self) -> &mut Tag {
        self.map.entries[self.index].1.resolve_mut()
    }
    pub fn into_mut(self) -> &'a mut Tag {
        self.map.entries[self.index].1.resolve_mut()
    }
    /// replace the value, returning the old one.
    pub fn insert(&mut self, v: Tag) -> Tag {
        std::mem::replace(&mut self.map.entries[self.index].1, Slot::Tag(v)).into_tag()
    }
    pub fn remove(self) -> Tag {
        self.map.entries.remove(self.index).1.into_tag()
    }
}

impl<'a> VacantEntry<'a> {
    pub fn key(&self) -> &String {
        &self.key
    }
    pub fn insert(self, v: Tag) -> &'a mut Tag {
        self.map.entries.push((self.key, Slot::Tag(v)));
        self.map.entries.last_mut().unwrap().1.resolve_mut()
    }
}

// -- iterators ---------------------------------------------------------------

pub struct Iter<'a> {
    inner: std::slice::Iter<'a, (String, Slot)>,
}
impl<'a> Iterator for Iter<'a> {
    type Item = (&'a String, &'a Tag);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, s)| (k, s.resolve()))
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.inner.nth(n).map(|(k, s)| (k, s.resolve()))
    }
}
impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(k, s)| (k, s.resolve()))
    }
}
impl ExactSizeIterator for Iter<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}
impl std::iter::FusedIterator for Iter<'_> {}
impl Clone for Iter<'_> {
    fn clone(&self) -> Self {
        Iter { inner: self.inner.clone() }
    }
}

pub struct IterMut<'a> {
    inner: std::slice::IterMut<'a, (String, Slot)>,
}
impl<'a> Iterator for IterMut<'a> {
    type Item = (&'a String, &'a mut Tag);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, s)| (&*k, s.resolve_mut()))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl DoubleEndedIterator for IterMut<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(k, s)| (&*k, s.resolve_mut()))
    }
}
impl ExactSizeIterator for IterMut<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}
impl std::iter::FusedIterator for IterMut<'_> {}

pub struct Keys<'a> {
    inner: std::slice::Iter<'a, (String, Slot)>,
}
impl<'a> Iterator for Keys<'a> {
    type Item = &'a String;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, _)| k)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl DoubleEndedIterator for Keys<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(k, _)| k)
    }
}
impl ExactSizeIterator for Keys<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}
impl std::iter::FusedIterator for Keys<'_> {}

pub struct Values<'a> {
    inner: std::slice::Iter<'a, (String, Slot)>,
}
impl<'a> Iterator for Values<'a> {
    type Item = &'a Tag;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, s)| s.resolve())
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl DoubleEndedIterator for Values<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, s)| s.resolve())
    }
}
impl ExactSizeIterator for Values<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}
impl std::iter::FusedIterator for Values<'_> {}

pub struct ValuesMut<'a> {
    inner: std::slice::IterMut<'a, (String, Slot)>,
}
impl<'a> Iterator for ValuesMut<'a> {
    type Item = &'a mut Tag;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, s)| s.resolve_mut())
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl DoubleEndedIterator for ValuesMut<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, s)| s.resolve_mut())
    }
}
impl ExactSizeIterator for ValuesMut<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}
impl std::iter::FusedIterator for ValuesMut<'_> {}

pub struct IntoIter {
    inner: std::vec::IntoIter<(String, Slot)>,
}
impl Iterator for IntoIter {
    type Item = (String, Tag);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, s)| (k, s.into_tag()))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl DoubleEndedIterator for IntoIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(k, s)| (k, s.into_tag()))
    }
}
impl ExactSizeIterator for IntoIter {
    fn len(&self) -> usize {
        self.inner.len()
    }
}
impl std::iter::FusedIterator for IntoIter {}

impl<'a> IntoIterator for &'a Compound {
    type Item = (&'a String, &'a Tag);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}
impl<'a> IntoIterator for &'a mut Compound {
    type Item = (&'a String, &'a mut Tag);
    type IntoIter = IterMut<'a>;
    fn into_iter(self) -> IterMut<'a> {
        self.iter_mut()
    }
}
impl IntoIterator for Compound {
    type Item = (String, Tag);
    type IntoIter = IntoIter;
    fn into_iter(self) -> IntoIter {
        IntoIter { inner: self.entries.into_iter() }
    }
}

impl FromIterator<(String, Tag)> for Compound {
    fn from_iter<I: IntoIterator<Item = (String, Tag)>>(it: I) -> Self {
        let mut c = Compound::new();
        c.extend(it);
        c
    }
}
impl Extend<(String, Tag)> for Compound {
    fn extend<I: IntoIterator<Item = (String, Tag)>>(&mut self, it: I) {
        for (k, v) in it {
            self.insert(k, v);
        }
    }
}
impl From<Vec<(String, Tag)>> for Compound {
    fn from(v: Vec<(String, Tag)>) -> Self {
        v.into_iter().collect()
    }
}

// -- Index impls (IndexMap allows map["key"] and map[i]) ---------------------

impl std::ops::Index<&str> for Compound {
    type Output = Tag;
    fn index(&self, key: &str) -> &Tag {
        self.get(key).expect("compound key not found")
    }
}
impl std::ops::Index<&String> for Compound {
    type Output = Tag;
    fn index(&self, key: &String) -> &Tag {
        self.get(key.as_str()).expect("compound key not found")
    }
}
impl std::ops::Index<String> for Compound {
    type Output = Tag;
    fn index(&self, key: String) -> &Tag {
        self.get(&key).expect("compound key not found")
    }
}
impl std::ops::Index<usize> for Compound {
    type Output = Tag;
    fn index(&self, i: usize) -> &Tag {
        self.get_index(i).map(|(_, v)| v).expect("compound index out of bounds")
    }
}
impl std::ops::IndexMut<&str> for Compound {
    fn index_mut(&mut self, key: &str) -> &mut Tag {
        self.get_mut(key).expect("compound key not found")
    }
}
impl std::ops::IndexMut<String> for Compound {
    fn index_mut(&mut self, key: String) -> &mut Tag {
        self.get_mut(&key).expect("compound key not found")
    }
}
impl std::ops::IndexMut<usize> for Compound {
    fn index_mut(&mut self, i: usize) -> &mut Tag {
        self.get_index_mut(i)
            .map(|(_, v)| v)
            .expect("compound index out of bounds")
    }
}

/// Root entries that stay eagerly parsed under `load(true)` (render mode):
/// everything the renderers and structure/overlay reads touch
/// unconditionally. All other compound/list subtrees on the chunk root
/// (block_entities, tick schedules, post_processing, upgrade_data, …) are
/// stored as raw spans and materialized on first access.
pub const RENDER_LAZY_KEEP: &[&str] = &[
    "Level", "region", "sections", "Sections", "structures", "Structures",
];

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

pub fn byte_array_from<'a>(c: Option<&'a Compound>, key: &str) -> Option<&'a [u8]> {
    c.and_then(|c| c.get(key)).and_then(|t| t.as_bytes())
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
pub fn raw_i64_or(data: &[u8], idx: usize, def: i64) -> i64 {
    let o = idx * 8;
    if o + 8 <= data.len() {
        i64::from_be_bytes(data[o..o + 8].try_into().unwrap())
    } else {
        def
    }
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

#[inline]
fn eof() -> io::Error {
    // same error Read::read_exact produces on a short input
    io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer")
}

/// Slice-cursor NBT reader: scalars are a pointer bump + `from_be_bytes`,
/// array/string payloads borrow the input then `to_vec`. Used on the hot
/// region-load path and for standalone .dat files.
pub(crate) struct SliceReader<'a> {
    s: &'a [u8],
    pub raw_arrays: bool,
    /// render-load mode: names of root-compound entries to parse eagerly.
    /// Unlisted compound/list subtrees on the root are stored as raw
    /// payload spans (see `Compound::insert_lazy`) and parsed on first
    /// access instead of at load.
    pub lazy_keep: Option<&'static [&'static str]>,
    depth: usize,
}

impl<'a> SliceReader<'a> {
    pub fn new(s: &'a [u8]) -> Self {
        SliceReader { s, raw_arrays: false, lazy_keep: None, depth: 0 }
    }

    /// advance the cursor by `n`, returning the skipped-over bytes
    #[inline]
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.s.len() < n {
            return Err(eof());
        }
        let (head, tail) = self.s.split_at(n);
        self.s = tail;
        Ok(head)
    }
    #[inline]
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    #[inline]
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    #[inline]
    fn i16(&mut self) -> io::Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    #[inline]
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    #[inline]
    fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    #[inline]
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.i32()? as u32))
    }
    #[inline]
    fn f64(&mut self) -> io::Result<f64> {
        Ok(f64::from_bits(self.i64()? as u64))
    }
    fn string(&mut self) -> io::Result<String> {
        let len = self.u16()? as usize;
        // from_utf8_lossy borrows on success — into_owned() is the only copy
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    /// Reads the root tag (upstream NBTReader.read).
    pub fn read(&mut self) -> io::Result<Tag> {
        let ty = self.u8()?;
        if ty != TAG_COMPOUND {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected compound root, got {ty}"),
            ));
        }
        // root name is discarded — skip it without materialising a String
        let len = self.u16()? as usize;
        self.take(len)?;
        self.payload(TAG_COMPOUND)
    }

    fn payload(&mut self, ty: u8) -> io::Result<Tag> {
        Ok(match ty {
            TAG_BYTE => Tag::Byte(self.u8()? as i8),
            TAG_SHORT => Tag::Short(self.i16()?),
            TAG_INT => Tag::Int(self.i32()?),
            TAG_LONG => Tag::Long(self.i64()?),
            TAG_FLOAT => Tag::Float(self.f32()?),
            TAG_DOUBLE => Tag::Double(self.f64()?),
            TAG_BYTE_ARRAY => {
                let n = self.i32()? as usize;
                Tag::ByteArray(self.take(n)?.to_vec())
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
                // most chunk compounds are tiny (palette entries, block_states,
                // biomes have 1-3 keys) — size for that, not the ~8-key root
                let mut map = Compound::with_capacity(4);
                self.depth += 1;
                // lazify only direct children of the chunk root
                let lazy = self.depth == 1 && self.lazy_keep.is_some();
                loop {
                    let t = self.u8()?;
                    if t == TAG_END {
                        break;
                    }
                    let name = self.string()?;
                    if lazy
                        && (t == TAG_COMPOUND || t == TAG_LIST)
                        && !self.lazy_keep.unwrap_or(&[]).contains(&name.as_str())
                    {
                        let raw = self.skip(t)?;
                        map.insert_lazy(name, t, raw, self.raw_arrays);
                        continue;
                    }
                    let v = self.payload(t)?;
                    map.insert(name, v);
                }
                self.depth -= 1;
                Tag::Compound(map)
            }
            TAG_INT_ARRAY => {
                let n = self.i32()? as usize;
                let raw = self.take(n * 4)?;
                if self.raw_arrays {
                    Tag::ByteArray(raw.to_vec())
                } else {
                    let mut v = Vec::with_capacity(n);
                    for c in raw.chunks_exact(4) {
                        v.push(i32::from_be_bytes(c.try_into().unwrap()));
                    }
                    Tag::IntArray(v)
                }
            }
            TAG_LONG_ARRAY => {
                let n = self.i32()? as usize;
                let raw = self.take(n * 8)?;
                if self.raw_arrays {
                    Tag::ByteArray(raw.to_vec())
                } else {
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

    /// advance over a whole payload of type `ty`, returning its bytes
    /// (post type/name header). Same validation as `payload`, no allocs.
    fn skip(&mut self, ty: u8) -> io::Result<&'a [u8]> {
        let start = self.s;
        self.skip_payload(ty)?;
        Ok(&start[..start.len() - self.s.len()])
    }

    fn skip_payload(&mut self, ty: u8) -> io::Result<()> {
        match ty {
            TAG_BYTE => {
                self.take(1)?;
            }
            TAG_SHORT => {
                self.take(2)?;
            }
            TAG_INT | TAG_FLOAT => {
                self.take(4)?;
            }
            TAG_LONG | TAG_DOUBLE => {
                self.take(8)?;
            }
            TAG_BYTE_ARRAY => {
                let n = self.i32()? as usize;
                self.take(n)?;
            }
            TAG_STRING => {
                let n = self.u16()? as usize;
                self.take(n)?;
            }
            TAG_LIST => {
                let et = self.u8()?;
                let n = self.i32()?;
                let n = if n < 0 { 0 } else { n as usize };
                for _ in 0..n {
                    self.skip_payload(et)?;
                }
            }
            TAG_COMPOUND => loop {
                let t = self.u8()?;
                if t == TAG_END {
                    break;
                }
                let n = self.u16()? as usize;
                self.take(n)?;
                self.skip_payload(t)?;
            },
            TAG_INT_ARRAY => {
                let n = self.i32()? as usize;
                self.take(n * 4)?;
            }
            TAG_LONG_ARRAY => {
                let n = self.i32()? as usize;
                self.take(n * 8)?;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid tag type {ty}"),
                ))
            }
        }
        Ok(())
    }
}

/// gzip-or-plain NBT blob -> root compound (player .dat files).
pub(crate) fn read_compressed(bytes: &[u8]) -> Option<Compound> {
    let raw = if bytes.starts_with(&[0x1f, 0x8b]) {
        crate::mca::decompress(crate::mca::CompressionType::Gzip, bytes).ok()?
    } else {
        bytes.to_vec()
    };
    match SliceReader::new(&raw).read().ok()? {
        Tag::Compound(c) => Some(c),
        _ => None,
    }
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
        Tag::List(_, items) => {
            out.push('[');
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

/// Parse an SNBT value starting at the beginning of `s` (after leading
/// whitespace); returns the tag and the number of bytes consumed. Trailing
/// input is allowed — needed by the `ReplaceBlocks` field parser where an
/// SNBT compound may be followed by `;` or `,`.
pub fn parse_snbt_partial(s: &str) -> Result<(Tag, usize), String> {
    let mut p = SnbtParser { b: s.as_bytes(), i: 0 };
    p.skip_ws();
    let t = p.value()?;
    Ok((t, p.i))
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

/// split "a.b[0].c" into path segments: names and [index] list access.
/// Inside brackets the content is literal — `.` stays part of the segment
/// and `]` is addressable as `\]` (e.g. `x[a.b]` reads key "a.b").
fn path_segments(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => out.push(std::mem::take(&mut cur)),
            '[' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut seg = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(&n) = chars.peek() {
                                if n == ']' || n == '\\' {
                                    chars.next();
                                    seg.push(n);
                                    continue;
                                }
                            }
                            seg.push(c);
                        }
                        ']' => break,
                        _ => seg.push(c),
                    }
                }
                out.push(seg);
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
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
    if segs.is_empty() {
        return root.shift_remove(&last).is_some();
    }
    // navigate the parent segments directly — re-joining them with "." would
    // reparse segments that legitimately contain dots
    let first = segs.remove(0);
    let mut cur = match root.get_mut(&first) {
        Some(t) => t,
        None => return false,
    };
    for seg in segs {
        cur = match cur {
            Tag::Compound(c) => match c.get_mut(&seg) {
                Some(t) => t,
                None => return false,
            },
            Tag::List(_, l) => match seg.parse::<usize>().ok().and_then(|i| l.get_mut(i)) {
                Some(t) => t,
                None => return false,
            },
            _ => return false,
        };
    }
    match cur {
        Tag::Compound(c) => c.shift_remove(&last).is_some(),
        Tag::List(_, l) => match last.parse::<usize>() {
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

    #[test]
    fn lazy_root_materializes() {
        // a chunk-root-shaped tree: a scalar, a kept list, and subtrees the
        // lazy keep list skips (block_entities as a list, upgrade_data as a
        // compound — lazified regardless of name).
        let mut item = Compound::default();
        item.insert("id".into(), Tag::String("minecraft:stone".into()));
        item.insert("Count".into(), Tag::Byte(3));
        let mut be = Compound::default();
        be.insert("id".into(), Tag::String("minecraft:chest".into()));
        be.insert("Items".into(), Tag::List(TAG_COMPOUND, vec![Tag::Compound(item)]));
        let mut ud = Compound::default();
        ud.insert("Indices".into(), Tag::LongArray(vec![1, 2, 3]));
        let mut sec = Compound::default();
        sec.insert("Y".into(), Tag::Byte(4));
        sec.insert("BlockLight".into(), Tag::ByteArray(vec![7u8; 2048]));
        let mut root = Compound::default();
        root.insert("DataVersion".into(), Tag::Int(3955));
        root.insert("sections".into(), Tag::List(TAG_COMPOUND, vec![Tag::Compound(sec)]));
        root.insert("block_entities".into(), Tag::List(TAG_COMPOUND, vec![Tag::Compound(be)]));
        root.insert("upgrade_data".into(), Tag::Compound(ud));

        let mut buf = Vec::new();
        write_root_compound(&mut buf, "", &root).unwrap();

        // raw_arrays stays false so the lazified LongArray round-trips as a
        // real long[]; render mode would set both flags (LazyTag carries the
        // flag through materialization).
        let mut r = SliceReader::new(&buf);
        r.lazy_keep = Some(RENDER_LAZY_KEEP);
        let Tag::Compound(parsed) = r.read().unwrap() else { panic!("not compound") };

        // whole-tree equality resolves the skipped spans transparently
        assert_eq!(parsed, root);
        // scalars stay eager; skipped subtrees materialize on access
        assert_eq!(parsed.get("DataVersion"), Some(&Tag::Int(3955)));
        assert_eq!(parsed.get("block_entities"), root.get("block_entities"));
        // reserializing a lazy tree yields byte-identical output
        let mut buf2 = Vec::new();
        write_root_compound(&mut buf2, "", &parsed).unwrap();
        assert_eq!(buf2, buf);
    }
}
