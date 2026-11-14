//! Port of io.mca: region file loading (MCAFile/Chunk/RegionChunk/Region).

use crate::nbt::{self, Compound, SliceReader, Tag};
use crate::point::Point2i;
use anyhow::{bail, Context as _, Result};
use flate2::bufread::{GzDecoder, ZlibDecoder};
use rayon::prelude::*;
use std::borrow::Cow;
use std::cell::RefCell;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
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
/// `into` variant decodes each block straight into `out`'s tail instead of
/// an intermediate Vec per block.
fn lz4_decompress_blocks_into(data: &[u8], out: &mut Vec<u8>) -> Result<()> {
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
        if original < 0 {
            bail!("invalid lz4 block size {original}");
        }
        if compressed as u32 & 0x8000_0000 != 0 {
            let n = (compressed & 0x7FFF_FFFF) as usize;
            let start = c.position() as usize;
            out.extend_from_slice(&data[start..start + n]);
            c.set_position((start + n) as u64);
        } else {
            let n = compressed as usize;
            let start = c.position() as usize;
            let base = out.len();
            out.resize(base + original as usize, 0);
            let written =
                lz4_flex::block::decompress_into(&data[start..start + n], &mut out[base..])
                    .context("lz4 block decompress")?;
            out.truncate(base + written);
            c.set_position((start + n) as u64);
        }
    }
    Ok(())
}

fn lz4_decompress_blocks(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len().saturating_mul(3).max(4096));
    lz4_decompress_blocks_into(data, &mut out)?;
    Ok(out)
}

/// lz4 block output framing for saving (LZ4BlockOutputStream, 64KiB blocks).
fn lz4_compress_blocks(data: &[u8]) -> Result<Vec<u8>> {
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

/// Borrowing variant of [`decompress`]: uncompressed payloads are returned as
/// a view into `data` (zero-copy), compressed ones come back owned.
/// `bufread` decoders inflate straight from the source slice with no
/// intermediate buffer copy.
fn decompress_cow(compression: CompressionType, data: &[u8]) -> Result<Cow<'_, [u8]>> {
    Ok(match compression.base() {
        CompressionType::Gzip => {
            let mut v = Vec::with_capacity(data.len().saturating_mul(4).max(4096));
            GzDecoder::new(data).read_to_end(&mut v)?;
            Cow::Owned(v)
        }
        CompressionType::Zlib => {
            let mut v = Vec::with_capacity(data.len().saturating_mul(4).max(4096));
            ZlibDecoder::new(data).read_to_end(&mut v)?;
            Cow::Owned(v)
        }
        CompressionType::Lz4 => Cow::Owned(lz4_decompress_blocks(data)?),
        _ => Cow::Borrowed(data),
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

pub(crate) fn decompress(compression: CompressionType, data: &[u8]) -> Result<Vec<u8>> {
    Ok(decompress_cow(compression, data)?.into_owned())
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
    /// set by `load(true)` — arrays were parsed as undecoded byte blobs and
    /// would serialize back corrupted, so all write paths refuse.
    pub raw_mode: bool,
    /// chunks that failed to parse on the last `load` (upstream logs and
    /// skips them; we count them so callers can surface the number).
    pub skipped: usize,
}

/// 3-byte big-endian chunk-offset decode (header i-th entry)
fn offset_at(buf: &[u8], i: usize) -> i32 {
    let o = i * 4;
    ((buf[o] as i32) << 16) | ((buf[o + 1] as i32) << 8) | (buf[o + 2] as i32)
}

// -- raw region-file image cache ------------------------------------------------

/// Bounded cache of raw `.mca` bytes shared by repeated `load` calls.
/// Entries are keyed by path, validated by (mtime, len) on every access and
/// evicted oldest-first. Parsed `McaFile`s can't be shared — jobs consume
/// owned files and `chunk.data` is mutated by callers — but the raw image is
/// immutable, so `Arc<Vec<u8>>` handouts are cheap and safe.
struct RawFileCache {
    map: std::collections::HashMap<PathBuf, RawFileEntry>,
    order: std::collections::VecDeque<PathBuf>,
    bytes: usize,
}

struct RawFileEntry {
    mtime_ms: i64,
    len: u64,
    data: Arc<Vec<u8>>,
}

const RAW_CACHE_MAX_ENTRIES: usize = 24;
const RAW_CACHE_MAX_BYTES: usize = 128 * 1024 * 1024;

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
            skipped: 0,
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
            skipped: 0,
        }
    }

    /// Read the region file image. Repeated loads of the same file — the
    /// image + overlay jobs for one region, re-renders at a new zoom level,
    /// or a defragment right after a render — hit a small bounded cache
    /// instead of re-reading several MB per job. Entries are validated by
    /// (mtime, len) so any rewrite automatically misses, and are dropped
    /// explicitly by the write paths below.
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
    /// instead of the whole (multi-MB) region file. Callers like defragment()
    /// require all three header tables.
    pub fn load_offsets(&mut self) -> Result<()> {
        let mut hdr = [0u8; HEADER_SIZE];
        let mut f = std::fs::File::open(&self.file)?;
        match f.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        self.parse_header(&hdr);
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
    /// `raw` => arrays parse as raw bytes (fast path used by the renderer)
    /// and marks the file unserializable (`raw_mode`).
    pub fn load(&mut self, raw: bool) -> Result<()> {
        let data = self.read_file()?;
        self.raw_mode = raw;
        self.skipped = 0;
        if data.len() <= HEADER_SIZE {
            return Ok(());
        }
        self.parse_header(&data[..HEADER_SIZE]);
        let origin = self.location.region_to_chunk();
        let data: &[u8] = data.as_slice();
        let offsets = &self.offsets;
        let timestamps = &self.timestamps;
        let file = &self.file;
        let failed = std::sync::atomic::AtomicUsize::new(0);
        let parse = |i: usize| -> Option<Chunk> {
            if offsets[i] == 0 {
                return None;
            }
            let loc = origin.add_p(Point2i::from_chunk_index(i as i32));
            let pos = offsets[i] as usize * 4096;
            match Self::parse_chunk(data, pos, loc, file, raw) {
                Ok(mut c) => {
                    c.timestamp = timestamps[i];
                    Some(c)
                }
                Err(_) => {
                    failed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    None
                }
            }
        };
        // decompress+parse is the dominant region-load cost and chunks are
        // independent: spread them over the rayon pool. collect() preserves
        // index order so chunks[i] still corresponds to header slot i.
        // Nearly-empty regions stay sequential — the process-pool workers
        // already saturate all cores, so cross-thread dispatch of a handful
        // of tiny chunk tasks is pure overhead.
        const PAR_CHUNK_THRESHOLD: usize = 64;
        // cap splitting at ~32-chunk tasks: 1024 single-chunk tasks mostly
        // buy scheduling overhead at chunk-parse cost granularity.
        const PAR_MIN_BATCH: usize = 32;
        let present = offsets.iter().filter(|&&o| o != 0).count();
        self.chunks = if present >= PAR_CHUNK_THRESHOLD {
            (0..1024)
                .into_par_iter()
                .with_min_len(PAR_MIN_BATCH)
                .map(parse)
                .collect()
        } else {
            (0..1024).map(parse).collect()
        };
        self.skipped = failed.load(std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Parse the `[len i32][type u8][payload]` record starting at `pos`.
    /// A non-compound root yields a Chunk with data=None (never Ok(None)).
    fn parse_chunk(
        data: &[u8],
        pos: usize,
        loc: Point2i,
        file: &Path,
        raw: bool,
    ) -> Result<Chunk> {
        if pos + 5 > data.len() {
            bail!("chunk offset out of bounds");
        }
        let length = i32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let compression = CompressionType::from_byte(data[pos + 4])?;
        // borrow the payload straight out of the file image — decoders read
        // from the slice and uncompressed chunks parse in place (no copies).
        let payload: Cow<'_, [u8]> = if compression.is_external() {
            let mcc = mcc_path(file, loc);
            // one syscall instead of exists()+read; missing file keeps the
            // same bail message as before.
            match std::fs::read(&mcc) {
                Ok(b) => Cow::Owned(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    bail!("mcc file {} does not exist", mcc.display())
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            let end = (pos + 5 + length.saturating_sub(1)).min(data.len());
            Cow::Borrowed(&data[pos + 5..end])
        };
        // decompress into the per-thread scratch buffer and parse — the
        // scratch borrow (and any payload borrow for uncompressed chunks)
        // lives only until the owned Tag tree is built inside the closure.
        let tag = DECOMPRESS_SCRATCH.with(|cell| -> Result<Tag> {
            let mut scratch = cell.borrow_mut();
            let nbt_data = decompress_into(compression, &payload, &mut scratch)?;
            let mut reader = SliceReader::new(nbt_data);
            reader.raw_arrays = raw;
            // raw (render) loads don't need block_entities/tick schedules/
            // upgrade_data materialized: keep them as raw spans — every
            // Compound accessor resolves them on first touch, so all
            // readers still see a complete tree. load(false) parses fully
            // (mutation/write-back paths need owned tags up front).
            if raw {
                reader.lazy_keep = Some(crate::nbt::RENDER_LAZY_KEEP);
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

    pub fn load_single_chunk(&self, chunk: Point2i, raw: bool) -> Result<Option<Chunk>> {
        let rel = chunk.as_relative_chunk();
        let idx = rel.as_chunk_index();
        let abs = self.location.region_to_chunk().add_p(rel);
        // only the 8KiB header + the target chunk's sectors are needed —
        // the old version pulled the whole multi-MB region file into memory.
        let mut f = std::fs::File::open(&self.file)?;
        let mut hdr = [0u8; HEADER_SIZE];
        let mut n = 0usize;
        while n < HEADER_SIZE {
            match f.read(&mut hdr[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        if n < HEADER_SIZE {
            return Ok(None);
        }
        let offset = offset_at(&hdr, idx);
        let o = idx * 4;
        let timestamp = i32::from_be_bytes(hdr[4096 + o..4096 + o + 4].try_into().unwrap());
        if offset <= 0 {
            // no chunk recorded — same as load() leaving the slot None
            return Ok(None);
        }
        let pos = offset as usize * 4096;
        // parse_chunk wants a buffer laid out as [len i32][type u8][payload]
        // starting at `pos`; build exactly that slice instead of the file.
        let file_len = f.metadata()?.len() as usize;
        f.seek(SeekFrom::Start(pos as u64))?;
        let mut head = [0u8; 5];
        if f.read_exact(&mut head).is_err() {
            // pos+5 beyond EOF — same failure parse_chunk produced when the
            // offset pointed outside the whole-file image.
            bail!("chunk offset out of bounds");
        }
        let length = i32::from_be_bytes(head[..4].try_into().unwrap()) as usize;
        let payload_len = length
            .saturating_sub(1)
            .min(file_len.saturating_sub(pos + 5));
        let mut data = Vec::with_capacity(5 + payload_len);
        data.extend_from_slice(&head);
        (&mut f).take(payload_len as u64).read_to_end(&mut data)?;
        let mut c = Self::parse_chunk(&data, 0, abs, &self.file, raw)?;
        c.timestamp = timestamp;
        Ok(Some(c))
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
fn write_chunk_data<W: std::io::Write>(
    w: &mut W,
    chunk: &Chunk,
    mca_file: &Path,
    loc: Point2i,
) -> Result<usize> {
    let mut buf = Vec::with_capacity(8192);
    let empty = Compound::default();
    // borrow the compound — avoids a deep clone of the whole chunk per save.
    nbt::write_root_compound(
        &mut Cursor::new(&mut buf),
        "",
        chunk.data.as_ref().unwrap_or(&empty),
    )?;
    let payload = compress(chunk.compression, &buf)?;
    if payload.len() > 1_044_475 {
        // external .mcc
        let dv = chunk
            .data
            .as_ref()
            .and_then(|d| nbt::int_from_opt(Some(d), "DataVersion"))
            .unwrap_or(0);
        if dv < crate::chunk::DV_19W36A {
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


#[derive(Clone, Debug, Default)]
pub struct WorldDirectories {
    pub region: Option<PathBuf>,
    pub poi: Option<PathBuf>,
    pub entities: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
