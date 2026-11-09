//! Port of io.mca: region file loading (MCAFile/Chunk/RegionChunk/Region).

use crate::nbt::{self, Compound, NbtReader, Tag};
use crate::point::Point2i;
use anyhow::{bail, Context as _, Result};
use flate2::read::{GzDecoder, ZlibDecoder};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

pub const HEADER_SIZE: usize = 8192;
pub const HEADER_OFFSETS_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressionType {
    None,
    Gzip,
    Zlib,
    Uncompressed,
    Lz4,
    NoneExt,
    GzipExt,
    ZlibExt,
    UncompressedExt,
    Lz4Ext,
}

impl CompressionType {
    pub fn from_byte(b: u8) -> Result<Self> {
        Ok(match b as i8 {
            0 => Self::None,
            1 => Self::Gzip,
            2 => Self::Zlib,
            3 => Self::Uncompressed,
            4 => Self::Lz4,
            -128 => Self::NoneExt,
            -127 => Self::GzipExt,
            -126 => Self::ZlibExt,
            -125 => Self::UncompressedExt,
            -124 => Self::Lz4Ext,
            _ => bail!("invalid compression type {}", b as i8),
        })
    }
    /// enum name (case-insensitive), e.g. "zlib" / "zlib_ext"
    pub fn from_name(n: &str) -> Option<Self> {
        Some(match n.to_ascii_lowercase().as_str() {
            "none" => Self::None,
            "gzip" => Self::Gzip,
            "zlib" => Self::Zlib,
            "uncompressed" => Self::Uncompressed,
            "lz4" => Self::Lz4,
            "none_ext" => Self::NoneExt,
            "gzip_ext" => Self::GzipExt,
            "zlib_ext" => Self::ZlibExt,
            "uncompressed_ext" => Self::UncompressedExt,
            "lz4_ext" => Self::Lz4Ext,
            _ => return None,
        })
    }
    pub fn is_external(&self) -> bool {
        (self.to_byte() as i8) < -120
    }
    pub fn to_byte(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::Gzip => 1,
            Self::Zlib => 2,
            Self::Uncompressed => 3,
            Self::Lz4 => 4,
            Self::NoneExt => 0x80,
            Self::GzipExt => 0x81,
            Self::ZlibExt => 0x82,
            Self::UncompressedExt => 0x83,
            Self::Lz4Ext => 0x84,
        }
    }
    pub fn external(&self) -> Self {
        match self {
            Self::None => Self::NoneExt,
            Self::Gzip => Self::GzipExt,
            Self::Zlib => Self::ZlibExt,
            Self::Uncompressed => Self::UncompressedExt,
            Self::Lz4 => Self::Lz4Ext,
            e => *e,
        }
    }
    fn base(&self) -> Self {
        match self {
            Self::None | Self::NoneExt => Self::None,
            Self::Gzip | Self::GzipExt => Self::Gzip,
            Self::Zlib | Self::ZlibExt => Self::Zlib,
            Self::Uncompressed | Self::UncompressedExt => Self::Uncompressed,
            Self::Lz4 | Self::Lz4Ext => Self::Lz4,
        }
    }
}

fn mcc_path(mca: &Path, chunk: Point2i) -> PathBuf {
    mca.with_file_name(format!("c.{}.{}.mcc", chunk.x, chunk.z))
}

/// net.jpountz.lz4.LZ4BlockInputStream framing:
/// repeated blocks of: compressed_len i32 BE, original_len i32 BE, data.
/// compressed_len high bit (0x80000000) => raw stored block.
fn lz4_decompress_blocks(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut c = Cursor::new(data);
    let mut hdr = [0u8; 4];
    loop {
        match c.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let compressed = i32::from_be_bytes(hdr);
        if compressed == 0 {
            break;
        }
        c.read_exact(&mut hdr)?;
        let original = i32::from_be_bytes(hdr);
        if compressed as u32 & 0x8000_0000 != 0 {
            let n = (compressed & 0x7FFF_FFFF) as usize;
            let start = c.position() as usize;
            out.extend_from_slice(&data[start..start + n]);
            c.set_position((start + n) as u64);
        } else {
            let n = compressed as usize;
            let start = c.position() as usize;
            let block = lz4_flex::block::decompress(&data[start..start + n], original as usize)
                .context("lz4 block decompress")?;
            out.extend_from_slice(&block);
            c.set_position((start + n) as u64);
        }
    }
    Ok(out)
}

/// lz4 block output framing for saving (LZ4BlockOutputStream, 64KiB blocks).
pub fn lz4_compress_blocks(data: &[u8]) -> Result<Vec<u8>> {
    const BLOCK: usize = 64 * 1024;
    let mut out = Vec::new();
    for chunk in data.chunks(BLOCK) {
        let c = lz4_flex::block::compress(chunk);
        if c.len() >= chunk.len() {
            out.extend_from_slice(&((chunk.len() as i32) | i32::MIN).to_be_bytes());
            out.extend_from_slice(&(chunk.len() as i32).to_be_bytes());
            out.extend_from_slice(chunk);
        } else {
            out.extend_from_slice(&(c.len() as i32).to_be_bytes());
            out.extend_from_slice(&(chunk.len() as i32).to_be_bytes());
            out.extend_from_slice(&c);
        }
    }
    out.extend_from_slice(&0i32.to_be_bytes());
    Ok(out)
}

fn decompress(compression: CompressionType, data: &[u8]) -> Result<Vec<u8>> {
    Ok(match compression.base() {
        CompressionType::Gzip => {
            let mut v = Vec::new();
            GzDecoder::new(data).read_to_end(&mut v)?;
            v
        }
        CompressionType::Zlib => {
            let mut v = Vec::new();
            ZlibDecoder::new(data).read_to_end(&mut v)?;
            v
        }
        CompressionType::Lz4 => lz4_decompress_blocks(data)?,
        _ => data.to_vec(),
    })
}

fn compress(compression: CompressionType, data: &[u8]) -> Result<Vec<u8>> {
    Ok(match compression.base() {
        CompressionType::Gzip => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut e, data)?;
            e.finish()?
        }
        CompressionType::Zlib => {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut e, data)?;
            e.finish()?
        }
        CompressionType::Lz4 => lz4_compress_blocks(data)?,
        _ => data.to_vec(),
    })
}

/// A single chunk's parsed contents (Chunk/RegionChunk).
#[derive(Clone, Debug)]
pub struct Chunk {
    pub absolute_location: Point2i,
    pub timestamp: i32,
    pub compression: CompressionType,
    pub data: Option<Compound>,
}

impl Chunk {
    pub fn new(loc: Point2i) -> Self {
        Chunk {
            absolute_location: loc,
            timestamp: 0,
            compression: CompressionType::Zlib,
            data: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.data.is_none()
    }
}

/// One region file (r.x.z.mca / poi / entities). Port of MCAFile.
pub struct McaFile {
    pub location: Point2i,
    pub file: PathBuf,
    pub timestamps: [i32; 1024],
    pub chunks: Vec<Option<Chunk>>,
    pub offsets: [i32; 1024],
    pub sectors: [u8; 1024],
}

/// 3-byte big-endian chunk-offset decode (header i-th entry)
fn offset_at(buf: &[u8], i: usize) -> i32 {
    let o = i * 4;
    ((buf[o] as i32) << 16) | ((buf[o + 1] as i32) << 8) | (buf[o + 2] as i32)
}

impl McaFile {
    pub fn new(file: PathBuf) -> Result<Self> {
        let location = crate::files::parse_mca_file_name(
            file.file_name().and_then(|s| s.to_str()).unwrap_or(""),
        )
        .with_context(|| format!("failed to parse region file name from {}", file.display()))?;
        Ok(McaFile {
            location,
            file,
            timestamps: [0; 1024],
            chunks: std::iter::repeat_with(|| None).take(1024).collect(),
            offsets: [0; 1024],
            sectors: [0; 1024],
        })
    }

    pub fn empty(location: Point2i, file: PathBuf) -> Self {
        McaFile {
            location,
            file,
            timestamps: [0; 1024],
            chunks: std::iter::repeat_with(|| None).take(1024).collect(),
            offsets: [0; 1024],
            sectors: [0; 1024],
        }
    }

    fn read_file(&self) -> Result<Vec<u8>> {
        Ok(std::fs::read(&self.file)?)
    }

    

    /// loadOffsets: only the offset table.
    pub fn load_offsets(&mut self) -> Result<()> {
        let data = self.read_file()?;
        if data.len() < HEADER_OFFSETS_SIZE {
            return Ok(());
        }
        for i in 0..1024 {
            self.offsets[i] = offset_at(&data, i);
        }
        Ok(())
    }

    fn parse_header(&mut self, hdr: &[u8]) {
        for i in 0..1024 {
            let o = i * 4;
            self.offsets[i] = offset_at(hdr, i);
            self.sectors[i] = hdr[o + 3];
            self.timestamps[i] = i32::from_be_bytes([
                hdr[4096 + o],
                hdr[4096 + o + 1],
                hdr[4096 + o + 2],
                hdr[4096 + o + 3],
            ]);
        }
    }

    /// load(raw): reads header then every chunk's NBT.
    /// `raw` => arrays parse as raw bytes (fast path used by the renderer).
    pub fn load(&mut self, raw: bool) -> Result<()> {
        let data = self.read_file()?;
        if data.len() <= HEADER_SIZE {
            return Ok(());
        }
        self.parse_header(&data[..HEADER_SIZE]);
        let origin = self.location.region_to_chunk();
        for i in 0..1024 {
            if self.offsets[i] == 0 {
                self.chunks[i] = None;
                continue;
            }
            let loc = origin.add_p(Point2i::from_chunk_index(i as i32));
            let pos = self.offsets[i] as usize * 4096;
            match Self::parse_chunk(&data, pos, loc, &self.file, raw) {
                Ok(Some(mut c)) => {
                    c.timestamp = self.timestamps[i];
                    self.chunks[i] = Some(c);
                }
                Ok(None) => self.chunks[i] = None,
                Err(e) => {
                    eprintln!("failed to load chunk at {loc}: {e}");
                    self.chunks[i] = None;
                }
            }
        }
        Ok(())
    }

    fn parse_chunk(
        data: &[u8],
        pos: usize,
        loc: Point2i,
        file: &Path,
        raw: bool,
    ) -> Result<Option<Chunk>> {
        if pos + 5 > data.len() {
            bail!("chunk offset out of bounds");
        }
        let length = i32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let compression = CompressionType::from_byte(data[pos + 4])?;
        let payload = if compression.is_external() {
            let mcc = mcc_path(file, loc);
            if !mcc.exists() {
                bail!("mcc file {} does not exist", mcc.display());
            }
            std::fs::read(&mcc)?
        } else {
            let end = (pos + 5 + length.saturating_sub(1)).min(data.len());
            data[pos + 5..end].to_vec()
        };
        let nbt_data = decompress(compression, &payload)?;
        let mut reader = NbtReader::new(Cursor::new(nbt_data));
        reader.raw_arrays = raw;
        let tag = reader.read()?;
        let mut chunk = Chunk::new(loc);
        chunk.compression = compression;
        chunk.data = tag.as_compound().cloned();
        Ok(Some(chunk))
    }

    pub fn load_single_chunk(&self, chunk: Point2i, raw: bool) -> Result<Option<Chunk>> {
        let data = self.read_file()?;
        if data.len() < HEADER_SIZE {
            return Ok(None);
        }
        let rel = chunk.as_relative_chunk();
        let idx = rel.z as usize * 32 + rel.x as usize;
        let offset = offset_at(&data, idx);
        let o = idx * 4;
        let timestamp = i32::from_be_bytes(data[4096 + o..4096 + o + 4].try_into().unwrap());
        let abs = self.location.region_to_chunk().add_p(rel);
        if offset <= 0 {
            return Ok(Some(Chunk::new(abs)));
        }
        let pos = offset as usize * 4096;
        match Self::parse_chunk(&data, pos, abs, &self.file, raw) {
            Ok(mut c) => {
                if let Some(c) = c.as_mut() {
                    c.timestamp = timestamp;
                }
                Ok(c)
            }
            Err(e) => Err(e),
        }
    }

    

    

    /// ChunkSet of present chunks (offsets > 0).
    pub fn offsets_chunk_set(&self) -> crate::selection::ChunkSet {
        let mut cs = crate::selection::ChunkSet::new();
        for i in 0..1024 {
            if self.offsets[i] > 0 {
                cs.set(i);
            }
        }
        cs
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.iter().all(|c| c.is_none())
    }

    pub fn delete_chunks(&mut self, selection: &crate::selection::ChunkSet) {
        for i in selection.iter() {
            self.timestamps[i] = 0;
            self.chunks[i] = None;
            self.sectors[i] = 0;
            self.offsets[i] = 0;
        }
    }

    /// save(raf): writes all chunks sequentially starting at sector 2.
    /// Returns false if no chunks were written.
    pub fn save_to<W: std::io::Write + std::io::Seek>(&self, w: &mut W) -> Result<bool> {
        use std::io::SeekFrom;
        let mut global_offset: i64 = 2;
        let mut last_written = 0usize;
        let mut offsets = [0i32; 1024];
        let mut sectors = [0u8; 1024];
        let mut timestamps = [0i32; 1024];

        w.seek(SeekFrom::Start((global_offset * 4096) as u64))?;
        for i in 0..1024 {
            w.seek(SeekFrom::Start((global_offset * 4096) as u64))?;
            let Some(chunk) = &self.chunks[i] else { continue };
            if chunk.is_empty() {
                continue;
            }
            last_written = write_chunk_data(w, chunk, &self.file, chunk.absolute_location)?;
            let secs = (last_written >> 12) + usize::from(last_written % 4096 != 0);
            offsets[i] = global_offset as i32;
            sectors[i] = secs as u8;
            timestamps[i] = chunk.timestamp;
            global_offset += secs as i64;
        }
        // header
        w.seek(SeekFrom::Start(0))?;
        for i in 0..1024 {
            w.write_all(&[
                (offsets[i] >> 16) as u8,
                (offsets[i] >> 8) as u8,
                offsets[i] as u8,
                sectors[i],
            ])?;
        }
        for t in timestamps {
            w.write_all(&t.to_be_bytes())?;
        }
        if last_written % 4096 != 0 {
            w.seek(SeekFrom::Start((global_offset * 4096 - 1) as u64))?;
            w.write_all(&[0])?;
        }
        w.seek(SeekFrom::End(0))?;
        Ok(global_offset != 2)
    }

    /// saveWithTempFile
    pub fn save_with_temp_file(&self, dest: &Path) -> Result<bool> {
        let tmp = dest.with_extension("mca.tmp");
        let result = {
            let mut f = std::fs::File::create(&tmp)?;
            self.save_to(&mut std::io::BufWriter::new(&mut f))?;
            f.sync_all().ok();
            // determine whether anything was written: re-open and check size > 8192
            drop(f);
            std::fs::metadata(&tmp).map(|m| m.len() > HEADER_SIZE as u64).unwrap_or(false)
        };
        if !result {
            let _ = std::fs::remove_file(&tmp);
            if dest.exists() {
                let _ = std::fs::remove_file(dest);
            }
        } else {
            std::fs::rename(&tmp, dest)?;
        }
        Ok(result)
    }

    /// deFragment: copy raw sectors of present chunks into a compacted file.
    pub fn defragment(&self, dest: &Path) -> Result<()> {
        let src = self.read_file()?;
        let tmp = self.file.with_extension("mca.defrag.tmp");
        {
            let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            use std::io::Write;
            let mut global_offset: i32 = 2;
            let mut skipped = 0usize;
            let mut offsets = [0i32; 1024];
            let mut sectors_out = [0u8; 1024];
            let mut out_buf: Vec<u8> = Vec::with_capacity(src.len());
            out_buf.resize(HEADER_SIZE, 0);
            for i in 0..1024 {
                if self.offsets[i] == 0 || self.sectors[i] == 0 {
                    skipped += 1;
                    continue;
                }
                let sectors = self.sectors[i] as usize;
                offsets[i] = global_offset;
                sectors_out[i] = sectors as u8;
                let start = self.offsets[i] as usize * 4096;
                let end = start + sectors * 4096;
                let end = end.min(src.len());
                if out_buf.len() < global_offset as usize * 4096 {
                    out_buf.resize(global_offset as usize * 4096, 0);
                }
                out_buf.extend_from_slice(&src[start..end]);
                global_offset += sectors as i32;
            }
            for i in 0..1024 {
                let o = i * 4;
                out_buf[o] = (offsets[i] >> 16) as u8;
                out_buf[o + 1] = (offsets[i] >> 8) as u8;
                out_buf[o + 2] = offsets[i] as u8;
                out_buf[o + 3] = sectors_out[i];
                let t = self.timestamps[i].to_be_bytes();
                out_buf[4096 + o..4096 + o + 4].copy_from_slice(&t);
            }
            w.write_all(&out_buf)?;
            w.flush()?;
            if skipped == 1024 {
                drop(w);
                let _ = std::fs::remove_file(&tmp);
                if dest == self.file {
                    let _ = std::fs::remove_file(dest);
                }
                return Ok(());
            }
        }
        std::fs::rename(&tmp, dest)?;
        Ok(())
    }

    /// RegionMCAFile.getStructures: chunk -> structure names from
    /// structures.Starts (references + POI are upstream-cached separately).
    pub fn structures(&self) -> std::collections::HashMap<i64, Vec<String>> {
        let mut map = std::collections::HashMap::new();
        for chunk in self.chunks.iter().flatten() {
            let Some(data) = &chunk.data else { continue };
            let mut names = Vec::new();
            // modern (21w43a+): structures.starts — every compound entry's key
            if let Some(starts) = data
                .get("structures")
                .and_then(Tag::as_compound)
                .and_then(|st| st.get("starts"))
                .and_then(Tag::as_compound)
            {
                for (name, tag) in starts {
                    if tag.as_compound().is_some() && !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
            // legacy (15w32a+): {Level|region}.Structures.Starts — compound with
            // size>1 and non-null "id" (upstream parseStructureStarts)
            let level: Option<&Compound> = data
                .get("Level")
                .or_else(|| data.get("region"))
                .and_then(Tag::as_compound)
                .or(Some(data));
            if let Some(starts) = level
                .and_then(|l| l.get("Structures"))
                .and_then(Tag::as_compound)
                .and_then(|st| st.get("Starts"))
                .and_then(Tag::as_compound)
            {
                for (_key, tag) in starts {
                    if let Some(c) = tag.as_compound() {
                        if c.len() > 1 {
                            if let Some(id) = c.get("id").and_then(Tag::as_string) {
                                let id = id.trim_start_matches("minecraft:");
                                if !names.iter().any(|n| n == id) {
                                    names.push(id.to_string());
                                }
                            }
                        }
                    }
                }
            }
            if !names.is_empty() {
                map.insert(chunk.absolute_location.as_long(), names);
            }
        }
        map
    }

    
}

/// write a single chunk at the writer's current position (length + type + data).
fn write_chunk_data<W: std::io::Write>(
    w: &mut W,
    chunk: &Chunk,
    mca_file: &Path,
    loc: Point2i,
) -> Result<usize> {
    let mut buf = Vec::with_capacity(8192);
    nbt::write_nbt(
        &mut Cursor::new(&mut buf),
        "",
        &Tag::Compound(chunk.data.clone().unwrap_or_default()),
    )?;
    let payload = compress(chunk.compression, &buf)?;
    if payload.len() > 1_044_475 {
        // external .mcc
        let dv = chunk
            .data
            .as_ref()
            .and_then(|d| nbt::int_from_opt(Some(d), "DataVersion"))
            .unwrap_or(0);
        if dv < 2203 {
            anyhow::bail!("chunk at {loc} is oversized and can't be saved (DataVersion < 2203)");
        }
        w.write_all(&1i32.to_be_bytes())?;
        w.write_all(&[chunk.compression.external().to_byte()])?;
        std::fs::write(mcc_path(mca_file, loc), &payload)?;
        Ok(5)
    } else {
        w.write_all(&((payload.len() + 1) as i32).to_be_bytes())?;
        w.write_all(&[chunk.compression.to_byte()])?;
        w.write_all(&payload)?;
        Ok(payload.len() + 5)
    }
}


#[derive(Clone, Debug)]
pub struct RegionDirectories {
    pub location: Point2i,
    pub region: Option<PathBuf>,
    pub poi: Option<PathBuf>,
    pub entities: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct WorldDirectories {
    pub region: Option<PathBuf>,
    pub poi: Option<PathBuf>,
    pub entities: Option<PathBuf>,
}



/// ChunkData bundles region+poi+entities chunks of one coordinate.
#[derive(Default)]
pub struct ChunkData<'a> {
    pub location: Point2i,
    pub region: Option<&'a Chunk>,
    pub poi: Option<&'a Chunk>,
    pub entities: Option<&'a Chunk>,
    pub selected: bool,
}
