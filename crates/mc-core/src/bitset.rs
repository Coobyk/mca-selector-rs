//! Minimal BitSet with Java semantics (little-endian long words), used for
//! block-state keys in the color mapping tables.

use base64::Engine;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BitSet {
    pub words: Vec<u64>,
    pub len: usize,
}

impl BitSet {
    pub fn new() -> Self {
        BitSet::default()
    }
    pub fn set(&mut self, i: usize) {
        let w = i >> 6;
        if self.words.len() <= w {
            self.words.resize(w + 1, 0);
        }
        self.words[w] |= 1u64 << (i & 63);
        if i >= self.len {
            self.len = i + 1;
        }
    }
    pub fn get(&self, i: usize) -> bool {
        let w = i >> 6;
        w < self.words.len() && (self.words[w] >> (i & 63)) & 1 == 1
    }
    /// BitSet.valueOf(byte[]).
    pub fn from_bytes(b: &[u8]) -> Self {
        let mut s = BitSet::new();
        for i in 0..b.len() * 8 {
            if (b[i >> 3] >> (i & 7)) & 1 == 1 {
                s.set(i);
            }
        }
        s
    }

    pub fn from_base64(s: &str) -> Option<Self> {
        let b = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
        Some(Self::from_bytes(&b))
    }
}

// ---------------------------------------------------------------------------
// fast hasher for the color tables

/// Multiplicative (FxHash-family) hasher for the mapping tables' string
/// and BitSet keys — short, trusted keys hashed on the render hot path,
/// so SipHash's DoS resistance isn't needed.
#[derive(Default)]
pub struct FxHasher {
    h: u64,
}

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.h = (self.h.rotate_left(5) ^ w).wrapping_mul(FX_SEED);
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        // extra multiply so the high bits hashbrown uses for SIMD tags
        // carry entropy even for short keys
        self.h.wrapping_mul(FX_SEED)
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
        if b.len() >= 2 {
            self.add(u64::from(u16::from_ne_bytes(b[..2].try_into().unwrap())));
            b = &b[2..];
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

pub type FxMap<K, V> =
    std::collections::HashMap<K, V, std::hash::BuildHasherDefault<FxHasher>>;
pub type FxSet<K> = std::collections::HashSet<K, std::hash::BuildHasherDefault<FxHasher>>;
