//! Port of util.point.Point2i / Point2f.
//!
//! All shift-based conversions use arithmetic shifts, matching Java's `>>`.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point2i {
    pub x: i32,
    pub z: i32,
}

impl Point2i {
    pub const fn new(x: i32, z: i32) -> Self {
        Point2i { x, z }
    }

    /// new Point2i(long)
    pub fn from_long(l: i64) -> Self {
        Point2i {
            x: (l >> 32) as i32,
            z: l as i32,
        }
    }

    /// new Point2i(chunkIndex): x = i & 0x1F, z = i >> 5
    pub fn from_chunk_index(i: i32) -> Self {
        Point2i {
            x: i & 0x1F,
            z: i >> 5,
        }
    }

    pub fn as_long(&self) -> i64 {
        ((self.x as i64) << 32) | (self.z as i64 & 0xFFFF_FFFF)
    }

    /// relative chunk index inside a region, x | z<<5
    pub fn as_chunk_index(&self) -> usize {
        (self.x as usize & 0x1F) + (self.z as usize & 0x1F) * 32
    }

    /// absolute chunk -> chunk within its region (0..32)
    pub fn as_relative_chunk(&self) -> Point2i {
        Point2i::new(self.x & 0x1F, self.z & 0x1F)
    }

    pub fn block_to_chunk(&self) -> Point2i {
        Point2i::new(self.x >> 4, self.z >> 4)
    }

    pub fn block_to_region(&self) -> Point2i {
        Point2i::new(self.x >> 9, self.z >> 9)
    }

    pub fn chunk_to_region(&self) -> Point2i {
        Point2i::new(self.x >> 5, self.z >> 5)
    }

    pub fn chunk_to_block(&self) -> Point2i {
        Point2i::new(self.x << 4, self.z << 4)
    }

    pub fn region_to_block(&self) -> Point2i {
        Point2i::new(self.x << 9, self.z << 9)
    }

    pub fn region_to_chunk(&self) -> Point2i {
        Point2i::new(self.x << 5, self.z << 5)
    }

    pub fn add(&self, x: i32, z: i32) -> Point2i {
        Point2i::new(self.x + x, self.z + z)
    }

    pub fn add_p(&self, p: Point2i) -> Point2i {
        Point2i::new(self.x + p.x, self.z + p.z)
    }

    pub fn sub(&self, p: Point2i) -> Point2i {
        Point2i::new(self.x - p.x, self.z - p.z)
    }

    pub fn sub_v(&self, x: i32, z: i32) -> Point2i {
        Point2i::new(self.x - x, self.z - z)
    }

    pub fn div(&self, d: i32) -> Point2i {
        Point2i::new(self.x / d, self.z / d)
    }

    pub fn to_point2f(&self) -> Point2f {
        Point2f::new(self.x as f32, self.z as f32)
    }
}

impl std::fmt::Display for Point2i {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}, {}", self.x, self.z)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point2f {
    pub x: f32,
    pub z: f32,
}

impl Point2f {
    pub const ZERO: Point2f = Point2f { x: 0.0, z: 0.0 };

    pub const fn new(x: f32, z: f32) -> Self {
        Point2f { x, z }
    }

    pub fn add(&self, x: f32, z: f32) -> Point2f {
        Point2f::new(self.x + x, self.z + z)
    }

    pub fn add_p(&self, p: Point2f) -> Point2f {
        Point2f::new(self.x + p.x, self.z + p.z)
    }

    pub fn sub(&self, p: Point2f) -> Point2f {
        Point2f::new(self.x - p.x, self.z - p.z)
    }

    pub fn mul(&self, m: f32) -> Point2f {
        Point2f::new(self.x * m, self.z * m)
    }

    pub fn to_point2i(&self) -> Point2i {
        Point2i::new(self.x as i32, self.z as i32)
    }
}

