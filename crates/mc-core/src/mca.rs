//! Port of io.mca: region file loading (MCAFile/Chunk/RegionChunk/Region).

#[cfg(not(target_arch = "wasm32"))]
use crate::nbt;
use crate::nbt::{Compound, SliceReader, Tag};
use crate::point::Point2i;
use crate::selection::ChunkSet;
use anyhow::{bail, Context as _, Result};
use flate2::bufread::{GzDecoder, ZlibDecoder};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;
use std::borrow::Cow;
use std::cell::RefCell;
use std::io::Read;
#[cfg(not(target_arch = "wasm32"))]
use std::io::{Cursor, Seek, SeekFrom};
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

pub const HEADER_SIZE: usize = 8192;

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
    /// every variant — one enumeration for tables that must cover all
    /// compression types (changer's parse, filter validation).
    pub const ALL: [CompressionType; 10] = [
        Self::None, Self::Gzip, Self::Zlib, Self::Uncompressed, Self::Lz4,
        Self::NoneExt, Self::GzipExt, Self::ZlibExt, Self::UncompressedExt, Self::Lz4Ext,
    ];

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
        (self.to_byte() & 0x80) != 0
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

#[cfg(not(target_arch = "wasm32"))]
fn read_sidecar(file: &Path, loc: Point2i) -> Result<Cow<'static, [u8]>> {
    let mcc = mcc_path(file, loc);
    match std::fs::read(&mcc) {
        Ok(bytes) => Ok(Cow::Owned(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("mcc file {} does not exist", mcc.display())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn mcc_path(mca: &Path, chunk: Point2i) -> PathBuf {
    mca.with_file_name(format!("c.{}.{}.mcc", chunk.x, chunk.z))
}

const LZ4_MAGIC: &[u8; 8] = b"LZ4Block";
const LZ4_HEADER_SIZE: usize = 21;
const LZ4_RAW: u8 = 0x10;
const LZ4_COMPRESSED: u8 = 0x20;
const LZ4_BLOCK_SIZE: usize = 64 * 1024;

fn lz4_checksum(data: &[u8]) -> u32 {
    // lz4-java's StreamingXXHash32.asChecksum() exposes the low 28 bits.
    twox_hash::XxHash32::oneshot(0x9747_b28c, data) & 0x0fff_ffff
}

fn lz4_append_block(payload: &[u8], original: usize, raw: bool, out: &mut Vec<u8>) -> Result<()> {
    if raw {
        anyhow::ensure!(payload.len() == original, "invalid raw lz4 block size");
        out.extend_from_slice(payload);
    } else {
        let base = out.len();
        out.resize(base + original, 0);
        let written = lz4_flex::block::decompress_into(payload, &mut out[base..])
            .context("lz4 block decompress")?;
        anyhow::ensure!(written == original, "incorrect lz4 block size");
    }
    Ok(())
}

fn lz4_decompress_blocks_into(mut data: &[u8], out: &mut Vec<u8>) -> Result<()> {
    if !data.starts_with(LZ4_MAGIC) {
        return lz4_decompress_legacy(data, out);
    }
    loop {
        let (header, rest) = data.split_at_checked(LZ4_HEADER_SIZE).context("truncated lz4 header")?;
        anyhow::ensure!(header.starts_with(LZ4_MAGIC), "invalid lz4 block magic");
        let token = header[8];
        let method = token & 0xf0;
        anyhow::ensure!(matches!(method, LZ4_RAW | LZ4_COMPRESSED), "invalid lz4 block method");
        let compressed = u32::from_le_bytes(header[9..13].try_into().unwrap()) as usize;
        let original = u32::from_le_bytes(header[13..17].try_into().unwrap()) as usize;
        let checksum = u32::from_le_bytes(header[17..21].try_into().unwrap());
        if compressed == 0 && original == 0 {
            anyhow::ensure!(checksum == 0, "invalid lz4 end marker");
            return Ok(());
        }
        anyhow::ensure!(compressed != 0 && original != 0 && original <= 1 << (10 + (token & 0xf)),
            "invalid lz4 block size");
        let (payload, remaining) = rest.split_at_checked(compressed).context("truncated lz4 block")?;
        let base = out.len();
        lz4_append_block(payload, original, method == LZ4_RAW, out)?;
        anyhow::ensure!(lz4_checksum(&out[base..]) == checksum, "incorrect lz4 checksum");
        data = remaining;
    }
}

// Older Rust builds wrote big-endian lengths instead of Java's block header.
fn lz4_decompress_legacy(mut data: &[u8], out: &mut Vec<u8>) -> Result<()> {
    loop {
        let (size, rest) = data.split_at_checked(4).context("truncated legacy lz4 header")?;
        let size = u32::from_be_bytes(size.try_into().unwrap());
        if size == 0 {
            return Ok(());
        }
        let (original, rest) = rest.split_at_checked(4).context("truncated legacy lz4 header")?;
        let original = u32::from_be_bytes(original.try_into().unwrap()) as usize;
        let len = (size & 0x7fff_ffff) as usize;
        anyhow::ensure!(len != 0 && original != 0 && original <= LZ4_BLOCK_SIZE,
            "invalid legacy lz4 block size");
        let (payload, remaining) = rest.split_at_checked(len).context("truncated legacy lz4 block")?;
        lz4_append_block(payload, original, size & 0x8000_0000 != 0, out)?;
        data = remaining;
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn lz4_decompress_blocks(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len().saturating_mul(3).max(4096));
    lz4_decompress_blocks_into(data, &mut out)?;
    Ok(out)
}

/// lz4 block output framing for saving (LZ4BlockOutputStream, 64KiB blocks).
#[cfg(not(target_arch = "wasm32"))]
fn lz4_compress_blocks(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut write = |method, payload: &[u8], original: &[u8]| {
        out.extend_from_slice(LZ4_MAGIC);
        out.push(method | 6); // log2(64 KiB) - 10
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(original.len() as u32).to_le_bytes());
        out.extend_from_slice(&(if original.is_empty() { 0 } else { lz4_checksum(original) }).to_le_bytes());
        out.extend_from_slice(payload);
    };
    for chunk in data.chunks(LZ4_BLOCK_SIZE) {
        let c = lz4_flex::block::compress(chunk);
        if c.len() >= chunk.len() {
            write(LZ4_RAW, chunk, chunk);
        } else {
            write(LZ4_COMPRESSED, &c, chunk);
        }
    }
    write(LZ4_RAW, &[], &[]);
    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
fn decompress(compression: CompressionType, data: &[u8]) -> Result<Vec<u8>> {
    Ok(match compression.base() {
        CompressionType::Gzip => {
            let mut v = Vec::with_capacity(data.len().saturating_mul(4).max(4096));
            GzDecoder::new(data).read_to_end(&mut v)?;
            v
        }
        CompressionType::Zlib => {
            let mut v = Vec::with_capacity(data.len().saturating_mul(4).max(4096));
            ZlibDecoder::new(data).read_to_end(&mut v)?;
            v
        }
        CompressionType::Lz4 => lz4_decompress_blocks(data)?,
        _ => data.to_vec(),
    })
}

/// Scratch variant of [`decompress_cow`]: compressed payloads inflate into
/// `scratch` (which the caller reuses across chunks) and uncompressed ones
/// are still returned as a view into `data`. The returned slice borrows
/// either `data` or `scratch` — scratch stays borrowed until the NBT parse
/// consuming it is done.
fn decompress_into<'a>(
    compression: CompressionType,
    data: &'a [u8],
    scratch: &'a mut Vec<u8>,
) -> Result<&'a [u8]> {
    Ok(match compression.base() {
        CompressionType::Gzip => {
            scratch.clear();
            scratch.reserve(data.len().saturating_mul(4).max(4096));
            GzDecoder::new(data).read_to_end(scratch)?;
            &scratch[..]
        }
        CompressionType::Zlib => {
            scratch.clear();
            scratch.reserve(data.len().saturating_mul(4).max(4096));
            ZlibDecoder::new(data).read_to_end(scratch)?;
            &scratch[..]
        }
        CompressionType::Lz4 => {
            scratch.clear();
            lz4_decompress_blocks_into(data, scratch)?;
            &scratch[..]
        }
        _ => data,
    })
}

// Per-worker decompression buffer. Region loads inflate up to 1024 chunks;
// reusing one Vec per thread turns ~1024 grow-to-size allocations per
// region into ~one per worker.
thread_local! {
    static DECOMPRESS_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}



#[cfg(not(target_arch = "wasm32"))]
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
    timestamps: [i32; 1024],
    pub chunks: Vec<Option<Chunk>>,
    offsets: [i32; 1024],
    sectors: [u8; 1024],
    /// set by `load(true)` — arrays were parsed as undecoded byte blobs and
    /// would serialize back corrupted, so all write paths refuse.
    pub raw_mode: bool,
}

/// 3-byte big-endian chunk-offset decode (header i-th entry)
fn offset_at(buf: &[u8], i: usize) -> i32 {
    let o = i * 4;
    ((buf[o] as i32) << 16) | ((buf[o + 1] as i32) << 8) | (buf[o + 2] as i32)
}

/// the 8KiB header's three tables: offsets, sector counts, timestamps.
fn header_tables(hdr: &[u8; HEADER_SIZE]) -> ([i32; 1024], [u8; 1024], [i32; 1024]) {
    let mut offsets = [0i32; 1024];
    let mut sectors = [0u8; 1024];
    let mut timestamps = [0i32; 1024];
    for i in 0..1024 {
        let o = i * 4;
        offsets[i] = offset_at(hdr, i);
        sectors[i] = hdr[o + 3];
        timestamps[i] = i32::from_be_bytes(hdr[4096 + o..4096 + o + 4].try_into().unwrap());
    }
    (offsets, sectors, timestamps)
}

/// Read occupied slots from an Anvil header without decoding any chunks.
pub fn region_chunk_set(data: &[u8]) -> Result<ChunkSet> {
    anyhow::ensure!(data.len() >= HEADER_SIZE, "truncated region header");
    let mut chunks = ChunkSet::new();
    for i in 0..1024 {
        if offset_at(data, i) != 0 {
            chunks.set(i);
        }
    }
    Ok(chunks)
}

/// Copy retained chunk records and timestamps, without decoding their NBT.
/// The returned set identifies retained records that need their .mcc sidecars.
pub fn filter_region_bytes(data: &[u8], keep: &ChunkSet) -> Result<(Vec<u8>, ChunkSet)> {
    anyhow::ensure!(data.len() >= HEADER_SIZE, "truncated region header");
    let mut output = vec![0; HEADER_SIZE];
    let mut external = ChunkSet::new();
    for i in keep.iter() {
        let entry = i * 4;
        let offset = offset_at(data, i) as usize;
        if offset == 0 {
            continue;
        }
        let sectors = data[entry + 3];
        let start = offset.checked_mul(4096).context("chunk offset out of bounds")?;
        let len = sectors as usize * 4096;
        let end = start.checked_add(len).context("chunk offset out of bounds")?;
        anyhow::ensure!(start >= HEADER_SIZE && sectors != 0 && end <= data.len(),
            "invalid sector range for chunk slot {i}");
        let record = &data[start..end];
        let payload_len = u32::from_be_bytes(record[..4].try_into().unwrap()) as usize;
        anyhow::ensure!(payload_len > 0 && payload_len <= len - 4,
            "invalid record length for chunk slot {i}");
        if record[4] & 0x80 != 0 {
            external.set(i);
        }
        let new_offset = output.len() / 4096;
        anyhow::ensure!(new_offset <= 0xff_ffff, "region offset exceeds three bytes");
        output[entry..entry + 3].copy_from_slice(&(new_offset as u32).to_be_bytes()[1..]);
        output[entry + 3] = sectors;
        let timestamp = 4096 + entry;
        output[timestamp..timestamp + 4].copy_from_slice(&data[timestamp..timestamp + 4]);
        output.extend_from_slice(record);
    }
    Ok((output, external))
}

/// read one chunk's `[len i32][type u8][payload]` record at sector-offset
/// `pos` — synthesizes the buffer layout `parse_chunk` expects without
/// pulling the whole region file into memory.
#[cfg(not(target_arch = "wasm32"))]
fn read_chunk_record(f: &mut std::fs::File, pos: usize) -> Result<Vec<u8>> {
    let file_len = f.metadata()?.len() as usize;
    f.seek(SeekFrom::Start(pos as u64))?;
    let mut head = [0u8; 5];
    f.read_exact(&mut head)
        .map_err(|_| anyhow::anyhow!("chunk offset out of bounds"))?;
    let length = i32::from_be_bytes(head[..4].try_into().unwrap()) as usize;
    let payload_len = length.saturating_sub(1).min(file_len.saturating_sub(pos + 5));
    let mut data = Vec::with_capacity(5 + payload_len);
    data.extend_from_slice(&head);
    (&mut *f).take(payload_len as u64).read_to_end(&mut data)?;
    Ok(data)
}


// -- raw region-file image cache ------------------------------------------------

/// Bounded cache of raw `.mca` bytes shared by repeated `load` calls.
/// Entries are keyed by path, validated by (mtime, len) on every access and
/// evicted oldest-first. Parsed `McaFile`s can't be shared — jobs consume
/// owned files and `chunk.data` is mutated by callers — but the raw image is
/// immutable, so `Arc<Vec<u8>>` handouts are cheap and safe.
#[cfg(not(target_arch = "wasm32"))]
struct RawFileCache {
    map: std::collections::HashMap<PathBuf, RawFileEntry>,
    order: std::collections::VecDeque<PathBuf>,
    bytes: usize,
}

#[cfg(not(target_arch = "wasm32"))]
struct RawFileEntry {
    mtime_ms: i64,
    len: u64,
    data: Arc<Vec<u8>>,
}

#[cfg(not(target_arch = "wasm32"))]
const RAW_CACHE_MAX_ENTRIES: usize = 24;
#[cfg(not(target_arch = "wasm32"))]
const RAW_CACHE_MAX_BYTES: usize = 128 * 1024 * 1024;

#[cfg(not(target_arch = "wasm32"))]
fn raw_file_cache() -> &'static parking_lot::Mutex<RawFileCache> {
    static C: std::sync::OnceLock<parking_lot::Mutex<RawFileCache>> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        parking_lot::Mutex::new(RawFileCache {
            map: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
            bytes: 0,
        })
    })
}

/// drop any cached image for `path` (called after writes/renames).
#[cfg(not(target_arch = "wasm32"))]
fn invalidate_file_cache(path: &Path) {
    let mut c = raw_file_cache().lock();
    if let Some(e) = c.map.remove(path) {
        c.bytes = c.bytes.saturating_sub(e.len as usize);
        c.order.retain(|p| p != path);
    }
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
            raw_mode: false,
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
            raw_mode: false,
        }
    }

    /// Read the region file image. Repeated loads of the same file — the
    /// image + overlay jobs for one region, re-renders at a new zoom level,
    /// or a defragment right after a render — hit a small bounded cache
    /// instead of re-reading several MB per job. Entries are validated by
    /// (mtime, len) so any rewrite automatically misses, and are dropped
    /// explicitly by the write paths below.
    #[cfg(not(target_arch = "wasm32"))]
    fn read_file(&self) -> Result<Arc<Vec<u8>>> {
        let stamp = std::fs::metadata(&self.file).ok().and_then(|m| {
            let ms = m
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as i64;
            Some((ms, m.len()))
        });
        if let Some((mtime_ms, len)) = stamp {
            let c = raw_file_cache().lock();
            if let Some(e) = c.map.get(&self.file) {
                if e.mtime_ms == mtime_ms && e.len == len {
                    return Ok(e.data.clone());
                }
            }
        }
        let data = Arc::new(std::fs::read(&self.file)?);
        if let Some((mtime_ms, len)) = stamp {
            let mut c = raw_file_cache().lock();
            if let Some(old) = c.map.insert(
                self.file.clone(),
                RawFileEntry { mtime_ms, len, data: data.clone() },
            ) {
                c.bytes = c.bytes.saturating_sub(old.len as usize);
                c.order.retain(|p| p != &self.file);
            }
            c.order.push_back(self.file.clone());
            c.bytes = c.bytes.saturating_add(len as usize);
            while c.map.len() > RAW_CACHE_MAX_ENTRIES || c.bytes > RAW_CACHE_MAX_BYTES {
                match c.order.pop_front() {
                    Some(k) => {
                        if let Some(e) = c.map.remove(&k) {
                            c.bytes = c.bytes.saturating_sub(e.len as usize);
                        }
                    }
                    None => break,
                }
            }
        }
        Ok(data)
    }

    /// loadOffsets: read just the 8KiB header (offsets + sectors + timestamps)
    /// instead of the whole (multi-MB) region file — the Border filter's
    /// neighbor probes only need the header tables.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_offsets(&mut self) -> Result<()> {
        let mut hdr = [0u8; HEADER_SIZE];
        let mut f = std::fs::File::open(&self.file)?;
        match f.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        self.parse_header(hdr[..HEADER_SIZE].try_into().unwrap());
        Ok(())
    }

    fn parse_header(&mut self, hdr: &[u8; HEADER_SIZE]) {
        (self.offsets, self.sectors, self.timestamps) = header_tables(hdr);
    }

    /// load(raw): reads header then every chunk's NBT.
    /// `raw` => arrays parse as raw bytes (fast path used by the renderer)
    /// and marks the file unserializable (`raw_mode`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(&mut self, raw: bool) -> Result<()> {
        let data = self.read_file()?;
        self.load_bytes(&data, raw, &read_sidecar)
    }

    /// Parse a region image. Sidecar keys are absolute chunk coordinates.
    pub fn from_bytes(
        file: PathBuf,
        data: &[u8],
        raw: bool,
        sidecars: &std::collections::HashMap<Point2i, Vec<u8>>,
    ) -> Result<Self> {
        anyhow::ensure!(data.len() >= HEADER_SIZE, "truncated region header");
        let mut region = Self::new(file)?;
        region.load_bytes(data, raw, &|_, loc| {
            sidecars.get(&loc).map(|bytes| Cow::Borrowed(bytes.as_slice()))
                .with_context(|| format!("missing sidecar c.{}.{}.mcc", loc.x, loc.z))
        })?;
        Ok(region)
    }

    fn load_bytes<'a>(
        &mut self,
        data: &[u8],
        raw: bool,
        sidecar: &(impl Fn(&Path, Point2i) -> Result<Cow<'a, [u8]>> + Sync),
    ) -> Result<()> {
        self.raw_mode = raw;
        if data.len() <= HEADER_SIZE {
            return Ok(());
        }
        self.parse_header(data[..HEADER_SIZE].try_into().unwrap());
        let origin = self.location.region_to_chunk();
        let offsets = &self.offsets;
        let timestamps = &self.timestamps;
        let file = &self.file;
        let parse = |i: usize| -> Option<Chunk> {
            if offsets[i] == 0 {
                return None;
            }
            let loc = origin.add_p(Point2i::from_chunk_index(i as i32));
            let pos = (offsets[i] as usize).checked_mul(4096)?;
            match Self::parse_chunk(data, pos, loc, file, raw, sidecar) {
                Ok(mut c) => {
                    c.timestamp = timestamps[i];
                    Some(c)
                }
                Err(_) => None,
            }
        };
        // decompress+parse is the dominant region-load cost and chunks are
        // independent: spread them over the rayon pool. collect() preserves
        // index order so chunks[i] still corresponds to header slot i.
        // Nearly-empty regions stay sequential — the process-pool workers
        // already saturate all cores, so cross-thread dispatch of a handful
        // of tiny chunk tasks is pure overhead.
        #[cfg(not(target_arch = "wasm32"))]
        {
            const PAR_CHUNK_THRESHOLD: usize = 64;
        // cap splitting at ~32-chunk tasks: 1024 single-chunk tasks mostly
        // buy scheduling overhead at chunk-parse cost granularity.
            const PAR_MIN_BATCH: usize = 32;
            let present = offsets.iter().filter(|&&o| o != 0).count();
            if present >= PAR_CHUNK_THRESHOLD {
                self.chunks = (0..1024)
                    .into_par_iter()
                    .with_min_len(PAR_MIN_BATCH)
                    .map(parse)
                    .collect();
                return Ok(());
            }
        }
        self.chunks = (0..1024).map(parse).collect();
        Ok(())
    }

    /// Parse the `[len i32][type u8][payload]` record starting at `pos`.
    /// A non-compound root yields a Chunk with data=None (never Ok(None)).
    fn parse_chunk<'a>(
        data: &[u8],
        pos: usize,
        loc: Point2i,
        file: &Path,
        raw: bool,
        sidecar: &impl Fn(&Path, Point2i) -> Result<Cow<'a, [u8]>>,
    ) -> Result<Chunk> {
        if data.len().saturating_sub(pos) < 5 {
            bail!("chunk offset out of bounds");
        }
        let length = i32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let compression = CompressionType::from_byte(data[pos + 4])?;
        // borrow the payload straight out of the file image — decoders read
        // from the slice and uncompressed chunks parse in place (no copies).
        let payload: Cow<'_, [u8]> = if compression.is_external() {
            sidecar(file, loc)?
        } else {
            let end = (pos + 5).saturating_add(length.saturating_sub(1)).min(data.len());
            Cow::Borrowed(&data[pos + 5..end])
        };
        // decompress into the per-thread scratch buffer and parse — the
        // scratch borrow (and any payload borrow for uncompressed chunks)
        // lives only until the owned Tag tree is built inside the closure.
        let tag = DECOMPRESS_SCRATCH.with(|cell| -> Result<Tag> {
            let mut scratch = cell.borrow_mut();
            let nbt_data = decompress_into(compression, &payload, &mut scratch)?;
            // raw (render) loads don't need block_entities/tick schedules/
            // upgrade_data materialized: keep them as raw spans — every
            // Compound accessor resolves them on first touch, so all
            // readers still see a complete tree. load(false) parses fully
            // (mutation/write-back paths need owned tags up front).
            let mut reader = SliceReader::new(nbt_data).raw_arrays(raw);
            if raw {
                reader = reader.lazy_keep(crate::nbt::RENDER_LAZY_KEEP);
            }
            Ok(reader.read()?)
        })?;
        let mut chunk = Chunk::new(loc);
        chunk.compression = compression;
        // move the root compound out — as_compound().cloned() would deep-copy
        // the entire NBT tree just to drop the original right after.
        chunk.data = match tag {
            Tag::Compound(c) => Some(c),
            _ => None,
        };
        Ok(chunk)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_single_chunk(&self, chunk: Point2i, raw: bool) -> Result<Option<Chunk>> {
        let rel = chunk.as_relative_chunk();
        let idx = rel.as_chunk_index();
        let abs = self.location.region_to_chunk().add_p(rel);
        // only the 8KiB header + the target chunk's sectors are needed —
        // the old version pulled the whole multi-MB region file into memory.
        let mut f = std::fs::File::open(&self.file)?;
        let mut hdr = [0u8; HEADER_SIZE];
        if f.read_exact(&mut hdr).is_err() {
            return Ok(None);
        }
        let (offsets, _, timestamps) = header_tables(&hdr);
        if offsets[idx] <= 0 {
            // no chunk recorded — same as load() leaving the slot None
            return Ok(None);
        }
        let pos = offsets[idx] as usize * 4096;
        let data = read_chunk_record(&mut f, pos)?;
        let mut c = Self::parse_chunk(&data, 0, abs, &self.file, raw, &read_sidecar)?;
        c.timestamp = timestamps[idx];
        Ok(Some(c))
    }

    /// Load only the listed header-slot indices — the renderer's margin
    /// strips need a region's edge chunks without paying for a full
    /// decompress: one file open, the 8KiB header, then each chunk's own
    /// sectors (same read path as load_single_chunk).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_sparse(&mut self, indices: &[usize], raw: bool) -> Result<()> {
        let mut f = std::fs::File::open(&self.file)?;
        let mut hdr = [0u8; HEADER_SIZE];
        f.read_exact(&mut hdr)?;
        self.raw_mode = raw;
        self.parse_header(hdr[..HEADER_SIZE].try_into().unwrap());
        let origin = self.location.region_to_chunk();
        let mut chunks: Vec<Option<Chunk>> = (0..1024).map(|_| None).collect();
        for &i in indices {
            if i >= 1024 || self.offsets[i] == 0 {
                continue;
            }
            let pos = self.offsets[i] as usize * 4096;
            let loc = origin.add_p(Point2i::from_chunk_index(i as i32));
            let Ok(data) = read_chunk_record(&mut f, pos) else { continue };
            if let Ok(mut c) = Self::parse_chunk(&data, 0, loc, &self.file, raw, &read_sidecar) {
                c.timestamp = self.timestamps[i];
                chunks[i] = Some(c);
            }
        }
        self.chunks = chunks;
        Ok(())
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

    /// Refuse to serialize a raw-loaded file: its arrays are undecoded byte
    /// blobs that would write back as corrupted ByteArrays (silent data loss).
    #[cfg(not(target_arch = "wasm32"))]
    fn ensure_serializable(&self) -> Result<()> {
        if self.raw_mode {
            bail!(
                "refusing to write {}: file was loaded with raw=true",
                self.file.display()
            );
        }
        Ok(())
    }

    /// save(raf): writes all chunks sequentially starting at sector 2.
    /// Returns false if no chunks were written.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_to<W: std::io::Write + std::io::Seek>(&self, w: &mut W) -> Result<bool> {
        self.ensure_serializable()?;
        use std::io::SeekFrom;
        let mut global_offset: i64 = 2;
        let mut last_written = 0usize;
        let mut offsets = [0i32; 1024];
        let mut sectors = [0u8; 1024];
        let mut timestamps = [0i32; 1024];

        for i in 0..1024 {
            let Some(chunk) = &self.chunks[i] else { continue };
            if chunk.is_empty() {
                continue;
            }
            // seek only right before a write (skips the padding gap); empty
            // slots no longer cost an lseek each.
            w.seek(SeekFrom::Start((global_offset * 4096) as u64))?;
            last_written = write_chunk_data(w, chunk, &self.file)?;
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
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_with_temp_file(&self, dest: &Path) -> Result<bool> {
        self.ensure_serializable()?;
        let tmp = dest.with_extension("mca.tmp");
        let result = {
            let mut f = std::fs::File::create(&tmp)?;
            self.save_to(&mut std::io::BufWriter::new(&mut f))?;
            f.sync_all()?;
            // determine whether anything was written: re-open and check size > 8192
            drop(f);
            std::fs::metadata(&tmp).map(|m| m.len() > HEADER_SIZE as u64).unwrap_or(false)
        };
        if !result {
            let _ = std::fs::remove_file(&tmp); // temp cleanup: best-effort
            if dest.exists() {
                // upstream deletes the file when a save produced nothing
                // (all chunks deleted); a lingering file is harmless
                let _ = std::fs::remove_file(dest);
            }
        } else {
            std::fs::rename(&tmp, dest)?;
        }
        invalidate_file_cache(dest);
        Ok(result)
    }

    /// deFragment: copy raw sectors of present chunks into a compacted file.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn defragment(&self, dest: &Path) -> Result<()> {
        // copies sectors, not NBT — but a raw-loaded file must never reach a
        // write path, so the same guard applies. (load_offsets users never
        // set raw_mode, so header-only defragment is unaffected.)
        self.ensure_serializable()?;
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
                let _ = std::fs::remove_file(&tmp); // temp cleanup: best-effort
                if dest == self.file {
                    // same empty-region delete as save_with_temp_file
                    let _ = std::fs::remove_file(dest);
                }
                invalidate_file_cache(dest);
                return Ok(());
            }
        }
        std::fs::rename(&tmp, dest)?;
        invalidate_file_cache(dest);
        Ok(())
    }

    /// RegionMCAFile.getStructures: chunk -> structure names via
    /// `chunk::structure_starts` (upstream ChunkFilter.parseStructureStarts;
    /// references + POI are upstream-cached separately).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn structures(&self) -> crate::cache::StructuresData {
        let mut out = crate::cache::StructuresData::default();
        for chunk in self.chunks.iter().flatten() {
            let Some(data) = &chunk.data else { continue };
            let names = crate::chunk::structure_starts(data);
            if !names.is_empty() {
                out.chunks.insert(chunk.absolute_location.as_long(), names);
            }
            out.boxes.extend(crate::chunk::structure_boxes(data));
        }
        out
    }
}

/// write a single chunk at the writer's current position (length + type + data).
#[cfg(not(target_arch = "wasm32"))]
fn write_chunk_data<W: std::io::Write>(
    w: &mut W,
    chunk: &Chunk,
    mca_file: &Path,
) -> Result<usize> {
    let mut buf = Vec::with_capacity(8192);
    let empty = Compound::default();
    // borrow the compound — avoids a deep clone of the whole chunk per save.
    nbt::write_root_compound(&mut Cursor::new(&mut buf), chunk.data.as_ref().unwrap_or(&empty))?;
    let payload = compress(chunk.compression, &buf)?;
    if payload.len() > 1_044_475 {
        // external .mcc
        let dv = chunk
            .data
            .as_ref()
            .and_then(|d| nbt::int_from_opt(Some(d), "DataVersion"))
            .unwrap_or(0);
        if dv < crate::chunk::DV_19W36A {
            anyhow::bail!("chunk at {} is oversized and can't be saved (DataVersion < 2203)", chunk.absolute_location);
        }
        w.write_all(&1i32.to_be_bytes())?;
        w.write_all(&[chunk.compression.external().to_byte()])?;
        std::fs::write(mcc_path(mca_file, chunk.absolute_location), &payload)?;
        Ok(5)
    } else {
        w.write_all(&((payload.len() + 1) as i32).to_be_bytes())?;
        w.write_all(&[chunk.compression.to_byte()])?;
        w.write_all(&payload)?;
        Ok(payload.len() + 5)
    }
}


#[derive(Clone, Debug, Default)]
pub struct WorldDirectories {
    pub region: Option<PathBuf>,
    pub poi: Option<PathBuf>,
    pub entities: Option<PathBuf>,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_java_lz4_blocks() {
        // lz4-java 1.8.0 LZ4BlockOutputStream with 64-byte blocks.
        let blocks = b"LZ4Block\x20\x0c\0\0\0\x40\0\0\0\x9b\x35\x30\x04\x2f\x0a\0\x01\0\x26\x50\0\0\0\0\0\
                       LZ4Block\x10\x04\0\0\0\x04\0\0\0\x10\x7f\xce\x07\0\0\0\0\
                       LZ4Block\x10\0\0\0\0\0\0\0\0\0\0\0\0";
        let mut expected = [0; 68];
        expected[0] = 10;
        assert_eq!(decompress(CompressionType::Lz4, blocks).unwrap(), expected);
        let mut corrupt = *blocks;
        corrupt[17] ^= 1;
        assert!(decompress(CompressionType::Lz4, &corrupt).is_err());
        let legacy = b"\x80\0\0\x04\0\0\0\x04\x0a\0\0\0\0\0\0\0";
        assert_eq!(decompress(CompressionType::Lz4, legacy).unwrap(), [10, 0, 0, 0]);
        let encoded = compress(CompressionType::Lz4, &expected).unwrap();
        assert!(encoded.starts_with(LZ4_MAGIC));
        assert_eq!(decompress(CompressionType::Lz4, &encoded).unwrap(), expected);
    }

    #[test]
    fn in_memory_region_filter_preserves_timestamps_and_external_payloads() {
        let mut data = vec![0u8; HEADER_SIZE + 8192];
        let payload = [10, 0, 0, 0]; // empty compound
        data[..8].copy_from_slice(&[0, 0, 2, 1, 0, 0, 3, 1]);
        data[4096..4104].copy_from_slice(&[0, 0, 0, 42, 0, 0, 0, 99]);
        data[8192..8201].copy_from_slice(&[0, 0, 0, 5, 3, 10, 0, 0, 0]);
        data[12288..12293].copy_from_slice(&[0, 0, 0, 1, 0x83]);
        let sidecars = [(Point2i::new(-31, 64), payload.to_vec())].into();
        let file = PathBuf::from("r.-1.2.mca");
        let original = McaFile::from_bytes(file.clone(), &data, false, &sidecars).unwrap();
        assert_eq!(original.chunks[0].as_ref().unwrap().timestamp, 42);
        assert_eq!(original.chunks[1].as_ref().unwrap().timestamp, 99);

        let mut keep = region_chunk_set(&data).unwrap();
        keep.clear(0);
        let (filtered, external) = filter_region_bytes(&data, &keep).unwrap();
        assert_eq!(external, keep);
        assert_eq!(region_chunk_set(&filtered).unwrap(), keep);
        assert_eq!(filtered.len(), 12288);
        let result = McaFile::from_bytes(file, &filtered, false, &sidecars).unwrap();
        assert!(result.chunks[0].is_none());
        assert_eq!(result.chunks[1].as_ref().unwrap().timestamp, 99);
        assert!(filter_region_bytes(&data[..data.len() - 1], &keep).is_err());
    }

    /// regression: load_offsets must fill sectors[] and timestamps[] too —
    /// when it only filled offsets[], defragment() read sectors==0 for every
    /// chunk and deleted the region file it was asked to compact.
    #[test]
    fn load_offsets_fills_all_header_tables() {
        let mut hdr = [0u8; HEADER_SIZE];
        // chunk slot 5: offset=2 sectors, count=3 sectors
        hdr[20] = 0;
        hdr[21] = 0;
        hdr[22] = 2;
        hdr[23] = 3;
        // timestamp slot 5 = 0x01020304
        hdr[4096 + 20] = 1;
        hdr[4096 + 21] = 2;
        hdr[4096 + 22] = 3;
        hdr[4096 + 23] = 4;
        let path = std::env::temp_dir().join("r.9.9.mca");
        std::fs::File::create(&path).unwrap().write_all(&hdr).unwrap();
        let mut m = McaFile::new(path.clone()).unwrap();
        m.load_offsets().unwrap();
        assert_eq!(m.offsets[5], 2);
        assert_eq!(m.sectors[5], 3);
        assert_eq!(m.timestamps[5], 0x01020304);
        std::fs::remove_file(&path).ok();
    }
}

/// gzip-or-plain NBT blob -> root compound (player .dat files).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_compressed(bytes: &[u8]) -> Option<nbt::Compound> {
    let raw = if bytes.starts_with(&[0x1f, 0x8b]) {
        decompress(CompressionType::Gzip, bytes).ok()?
    } else {
        bytes.to_vec()
    };
    match nbt::SliceReader::new(&raw).read().ok()? {
        nbt::Tag::Compound(c) => Some(c),
        _ => None,
    }
}
