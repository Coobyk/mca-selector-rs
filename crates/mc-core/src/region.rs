//! Raw `.mca` region file access.
//!
//! A region file stores 32x32 chunks. It begins with two 4 KiB tables: the
//! sector table (offset in sectors and sector count per chunk) and the
//! modification timestamps. Chunk payloads start at sector boundaries and are
//! a 4-byte big-endian length, a compression type byte and the compressed
//! NBT data.

use std::fs::File;
use std::path::{Path, PathBuf};

use libdeflater::{DecompressionError, Decompressor};
use memmap2::Mmap;

use crate::error::{Error, Result};

pub const REGION_CHUNKS: usize = 32;
const HEADER_SECTORS: u32 = 2;
const SECTOR_SIZE: usize = 4096;

/// Decompress a zlib or gzip payload with libdeflate. The output size is
/// not stored in the stream, so the buffer grows on demand.
fn decompress(payload: &[u8], gzip: bool) -> Option<Vec<u8>> {
    let mut cap = 256 * 1024;
    loop {
        let mut out = vec![0u8; cap];
        let mut dec = Decompressor::new();
        let result = if gzip {
            dec.gzip_decompress(payload, &mut out)
        } else {
            dec.zlib_decompress(payload, &mut out)
        };
        match result {
            Ok(n) => {
                out.truncate(n);
                return Some(out);
            }
            Err(DecompressionError::InsufficientSpace) => {
                cap *= 4;
                if cap > 128 * 1024 * 1024 {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Gzip,
    Zlib,
    Uncompressed,
}

impl Compression {
    fn from_byte(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Gzip),
            2 => Ok(Self::Zlib),
            3 => Ok(Self::Uncompressed),
            other => Err(Error::UnknownCompression(other)),
        }
    }
}

/// A region file, memory-mapped for cheap random access to chunk payloads.
pub struct Region {
    map: Mmap,
    path: PathBuf,
}

impl Region {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| Error::Io {
            path: path.into(),
            source,
        })?;
        let map = unsafe { Mmap::map(&file).map_err(|source| Error::Io {
            path: path.into(),
            source,
        })? };
        if map.len() < HEADER_SECTORS as usize * SECTOR_SIZE {
            return Err(Error::TooShort {
                path: path.into(),
                expected: HEADER_SECTORS as usize * SECTOR_SIZE,
                got: map.len(),
            });
        }
        Ok(Self {
            map,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Byte length of the underlying file.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.len() == 0
    }

    /// Local chunk coordinates must be in `0..32`.
    fn index(x: u8, z: u8) -> Result<usize> {
        if x >= REGION_CHUNKS as u8 || z >= REGION_CHUNKS as u8 {
            return Err(Error::BadOffset {
                offset: u32::from(x) | (u32::from(z) << 8),
                path: PathBuf::new().into(),
            });
        }
        Ok(usize::from(z) * REGION_CHUNKS + usize::from(x))
    }

    /// Raw sector table entry: `(sector offset, sector count)`, both zero for
    /// an absent chunk.
    fn location(&self, x: u8, z: u8) -> Result<(u32, u8)> {
        let i = Self::index(x, z)?;
        let entry = u32::from_be_bytes([
            self.map[i * 4],
            self.map[i * 4 + 1],
            self.map[i * 4 + 2],
            self.map[i * 4 + 3],
        ]);
        Ok((entry >> 8, (entry & 0xff) as u8))
    }

    /// Modification timestamp of a chunk, in seconds since the epoch.
    pub fn chunk_timestamp(&self, x: u8, z: u8) -> Result<u32> {
        let i = Self::index(x, z)?;
        let base = SECTOR_SIZE + i * 4;
        Ok(u32::from_be_bytes([
            self.map[base],
            self.map[base + 1],
            self.map[base + 2],
            self.map[base + 3],
        ]))
    }

    pub fn has_chunk(&self, x: u8, z: u8) -> Result<bool> {
        Ok(self.location(x, z)?.0 != 0)
    }

    /// Decompressed NBT payload of one chunk.
    pub fn chunk_data(&self, x: u8, z: u8) -> Result<Vec<u8>> {
        let (offset_sectors, _count) = self.location(x, z)?;
        if offset_sectors == 0 {
            return Err(Error::MissingChunk {
                x: i32::from(x),
                z: i32::from(z),
            });
        }
        // The highest bit marks chunks stored outside the region file.
        if offset_sectors & 0x8000_0000 != 0 {
            return Err(Error::ExternalStorage {
                x: i32::from(x),
                z: i32::from(z),
            });
        }
        let offset = usize::try_from(offset_sectors).unwrap() * SECTOR_SIZE;
        if offset + 5 > self.map.len() {
            return Err(Error::BadOffset {
                offset: offset_sectors,
                path: self.path.clone().into(),
            });
        }
        let length = u32::from_be_bytes([
            self.map[offset],
            self.map[offset + 1],
            self.map[offset + 2],
            self.map[offset + 3],
        ]) as usize;
        if length < 1 || offset + 4 + length > self.map.len() {
            return Err(Error::BadLength {
                length: length as u32,
                path: self.path.clone().into(),
            });
        }
        let compression =
            Compression::from_byte(self.map[offset + 4])?;
        let payload = &self.map[offset + 5..offset + 4 + length];
        match compression {
            Compression::Uncompressed => Ok(payload.to_vec()),
            _ => decompress(payload, compression == Compression::Gzip)
                .ok_or(Error::Decompress),
        }
    }

    /// Visit every chunk present in the region with its local coordinates
    /// (0..32) and decompressed NBT payload.
    pub fn for_each_chunk(
        &self,
        mut f: impl FnMut(u8, u8, &[u8]) -> Result<()>,
    ) -> Result<()> {
        for z in 0..REGION_CHUNKS as u8 {
            for x in 0..REGION_CHUNKS as u8 {
                let (offset_sectors, _) = self.location(x, z)?;
                if offset_sectors == 0 || offset_sectors & 0x8000_0000 != 0 {
                    continue;
                }
                f(x, z, &self.chunk_data(x, z)?)?;
            }
        }
        Ok(())
    }
}

/// Region file coordinates parsed from a file name like `r.-2.-7.mca`.
pub fn parse_region_name(path: &Path) -> Option<(i32, i32)> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".mca")?;
    let rest = stem.strip_prefix('r')?;
    let rest = rest.strip_prefix('.')?;
    let (x, z) = rest.split_once('.')?;
    x.parse().ok().zip(z.parse().ok())
}
