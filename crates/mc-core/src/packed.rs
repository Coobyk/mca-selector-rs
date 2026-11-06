//! Packed bit arrays as used by block palettes and heightmaps.
//!
//! Minecraft packs entries into 64-bit words with the rule that an entry
//! never spans two words: each word holds `floor(64 / bits)` entries and the
//! remaining bits are padding.

/// A packed bit reader over `i64` words.
#[derive(Debug, Clone, Copy)]
pub struct PackedBits<'a> {
    data: &'a [i64],
    bits: u32,
    per_long: u32,
}

impl<'a> PackedBits<'a> {
    /// `data` is the raw long array, `bits` the entry width in bits
    /// (`bits <= 32`).
    pub fn new(data: &'a [i64], bits: u32) -> Self {
        debug_assert!((1..=32).contains(&bits));
        let per_long = 64 / bits;
        Self {
            data,
            bits,
            per_long,
        }
    }

    pub fn bits(&self) -> u32 {
        self.bits
    }

    /// Number of entries storable in `data`.
    pub fn capacity(&self) -> usize {
        self.data.len() * self.per_long as usize
    }

    /// Read the entry at `index`; returns 0 when out of range (missing data
    /// means "all air" for block states).
    pub fn get(&self, index: usize) -> u32 {
        let per_long = self.per_long as usize;
        let long = index / per_long;
        if long >= self.data.len() {
            return 0;
        }
        let shift = (index % per_long) * self.bits as usize;
        ((self.data[long] as u64) >> shift) as u32 & ((1 << self.bits) - 1)
    }
}

/// Palette bit width used by block states: at least 4 bits, otherwise the
/// number of bits needed to index the palette.
pub fn palette_bits(palette_len: usize) -> u32 {
    debug_assert!(palette_len > 0);
    let needed = usize::BITS - (palette_len - 1).leading_zeros();
    needed.max(4)
}

/// Heightmap entry width since 1.18: 9 bits (heights are -64..320).
pub const HEIGHTMAP_BITS: u32 = 9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_packing() {
        // 5 bits per entry, 12 entries per long.
        let mut data = vec![0i64; 2];
        let values: Vec<u32> = (0..20).map(|i| (i * 7 + 3) & 0x1f).collect();
        for (i, v) in values.iter().enumerate() {
            let long = i / 12;
            let shift = (i % 12) * 5;
            data[long] |= ((*v as i64) & 0x1f) << shift;
        }
        let bits = PackedBits::new(&data, 5);
        for (i, v) in values.iter().enumerate() {
            assert_eq!(bits.get(i), *v);
        }
        assert_eq!(bits.capacity(), 24);
    }

    #[test]
    fn palette_widths() {
        assert_eq!(palette_bits(1), 4);
        assert_eq!(palette_bits(16), 4);
        assert_eq!(palette_bits(17), 5);
        assert_eq!(palette_bits(256), 8);
        assert_eq!(palette_bits(257), 9);
    }

    #[test]
    fn heightmap_packing() {
        // 9 bits: 7 entries per long. Value 511 at index 6.
        let mut data = vec![0i64; 1];
        data[0] |= 511i64 << (6 * 9);
        let bits = PackedBits::new(&data, HEIGHTMAP_BITS);
        assert_eq!(bits.get(6), 511);
        assert_eq!(bits.get(7), 0);
    }
}
