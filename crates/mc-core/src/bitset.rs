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
    /// Java BitSet.toByteArray -> little endian bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let n = self.len.div_ceil(8).max(1);
        let mut out = vec![0u8; n];
        for i in 0..self.len.max(n * 8) {
            if self.get(i) {
                out[i >> 3] |= 1 << (i & 7);
            }
        }
        out
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
    pub fn to_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.to_bytes())
    }
    pub fn from_base64(s: &str) -> Option<Self> {
        let b = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
        Some(Self::from_bytes(&b))
    }
}
