//! Writing Anvil region files.
//!
//! `RegionWriter` lays out chunk payloads sequentially from sector 2 and
//! fills the sector table and timestamp table at the end, producing a
//! standard `.mca` file that vanilla and mod loaders can read.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::error::{Error, Result};
use crate::region::REGION_CHUNKS;

const SECTOR_SIZE: usize = 4096;
const HEADER_SECTORS: usize = 2;

/// Incremental region file writer.
pub struct RegionWriter<W: Write> {
    out: W,
    /// Payload bytes buffered so far, in write order.
    payloads: Vec<(u8, u8, Vec<u8>, u32)>, // (lx, lz, zlib data, timestamp)
}

impl RegionWriter<BufWriter<File>> {
    /// Create a writer for the region file at `path` (e.g.
    /// `<world>/region/r.0.0.mca`), creating parent directories.
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| Error::Io {
                path: dir.into(),
                source,
            })?;
        }
        let file = File::create(path).map_err(|source| Error::Io {
            path: path.into(),
            source,
        })?;
        Ok(Self {
            out: BufWriter::new(file),
            payloads: Vec::new(),
        })
    }
}

impl<W: Write> RegionWriter<W> {
    /// Queue one chunk (raw, uncompressed NBT). Local coords in 0..32.
    pub fn write_chunk(&mut self, lx: u8, lz: u8, nbt: &[u8], timestamp: u32) -> Result<()> {
        if lx >= REGION_CHUNKS as u8 || lz >= REGION_CHUNKS as u8 {
            return Err(Error::BadOffset {
                offset: u32::from(lx) | (u32::from(lz) << 8),
                path: PathBuf::new().into(),
            });
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(nbt).map_err(|source| Error::Io {
            path: PathBuf::new().into(),
            source,
        })?;
        let compressed = enc.finish().map_err(|source| Error::Io {
            path: PathBuf::new().into(),
            source,
        })?;
        self.payloads.push((lx, lz, compressed, timestamp));
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.payloads.len()
    }

    /// Finish the file: lay out payloads from sector 2, write the sector
    /// table and timestamps. Consumes the writer.
    pub fn finish(mut self) -> Result<usize> {
        // Sort by local index for a deterministic layout (optional, tidy).
        self.payloads
            .sort_by_key(|(lx, lz, _, _)| usize::from(*lz) * REGION_CHUNKS + usize::from(*lx));

        let mut sector_table = vec![0u8; SECTOR_SIZE];
        let mut timestamp_table = vec![0u8; SECTOR_SIZE];
        let mut body: Vec<u8> = Vec::new();
        let mut next_sector = HEADER_SECTORS as u32;

        for (lx, lz, compressed, timestamp) in &self.payloads {
            let sector_count =
                ((compressed.len() + 5 + SECTOR_SIZE - 1) / SECTOR_SIZE) as u32;
            let index = usize::from(*lz) * REGION_CHUNKS + usize::from(*lx);
            let entry =
                (next_sector << 8) | (sector_count.min(255) as u32 & 0xff);
            sector_table[index * 4..index * 4 + 4].copy_from_slice(&entry.to_be_bytes());
            timestamp_table[index * 4..index * 4 + 4]
                .copy_from_slice(&timestamp.to_be_bytes());

            body.extend_from_slice(&((compressed.len() + 1) as u32).to_be_bytes());
            body.push(2); // zlib
            body.extend_from_slice(compressed);
            // Pad to a sector boundary.
            let used = body.len() % SECTOR_SIZE;
            if used != 0 {
                body.extend(std::iter::repeat_n(0u8, SECTOR_SIZE - used));
            }
            next_sector += sector_count;
        }

        self.out
            .write_all(&sector_table)
            .and_then(|_| self.out.write_all(&timestamp_table))
            .and_then(|_| self.out.write_all(&body))
            .and_then(|_| self.out.flush())
            .map_err(|source| Error::Io {
                path: PathBuf::new().into(),
                source,
            })?;
        Ok(self.payloads.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::Region;

    #[test]
    fn roundtrip_region() {
        let dir = std::env::temp_dir().join("mca-selector-rs-test-region");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("r.0.0.mca");
        let mut writer = RegionWriter::create(&path).unwrap();
        // A minimal, valid-ish NBT blob is not required by the writer; use
        // recognizable bytes.
        writer.write_chunk(0, 0, b"chunk-0-0", 111).unwrap();
        writer.write_chunk(31, 31, b"chunk-31-31", 222).unwrap();
        writer.write_chunk(5, 3, b"chunk-5-3", 333).unwrap();
        assert_eq!(writer.count(), 3);
        writer.finish().unwrap();

        let region = Region::open(&path).unwrap();
        assert!(region.has_chunk(0, 0).unwrap());
        assert!(region.has_chunk(31, 31).unwrap());
        assert!(!region.has_chunk(1, 1).unwrap());
        assert_eq!(region.chunk_data(0, 0).unwrap(), b"chunk-0-0");
        assert_eq!(region.chunk_data(31, 31).unwrap(), b"chunk-31-31");
        assert_eq!(region.chunk_data(5, 3).unwrap(), b"chunk-5-3");
        assert_eq!(region.chunk_timestamp(5, 3).unwrap(), 333);
    }
}
