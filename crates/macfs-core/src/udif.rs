//! UDIF (Apple DMG) containers: detection, partition list and a lazily decoded view of the
//! image as one virtual disk.
//!
//! A UDIF image is a data fork of (mostly compressed) blocks followed by an XML property list
//! (`blkx` entries = "partitions", each with a block map) and a 512 byte `koly` trailer. The
//! partitions sit at fixed sector positions of a virtual disk; for a typical image they are the
//! GPT / Apple partition map structures and the HFS+ or APFS partition(s), so the existing volume
//! detection (`detect::find_volumes`) works on that virtual disk unchanged.
//!
//! The `udif` crate parses the container, but it can
//! only decompress a whole partition into memory or into a writer. The plugins need random access
//! (list a directory of a 20 GB image without unpacking it), so [`UdifDisk`] decodes single block
//! runs on demand with the same codecs the crate uses and keeps the recently used ones in a cache.
//! Block checksums are not verified while browsing (the crate would have to read the whole data
//! fork first). The container is only ever read, never written.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use udif::{BlockType, DmgReader, DmgReaderOptions};

use crate::source::open_read_shared;

pub const SECTOR: u64 = 512;
/// Size of the `koly` trailer at the very end of every UDIF file.
pub const KOLY_SIZE: u64 = 512;

/// Largest block run decoded into memory at once (real images use 0.25 - 4 MiB).
const MAX_RUN_BYTES: u64 = 256 << 20;
/// Decoded block runs kept in memory.
const CACHE_BUDGET: usize = 64 << 20;

#[derive(Debug)]
pub enum UdifError {
    Io(io::Error),
    /// Damaged or inconsistent container.
    Format(String),
    /// Valid, but not something this plugin can handle.
    Unsupported(String),
}

impl fmt::Display for UdifError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UdifError::Io(e) => write!(f, "I/O error: {e}"),
            UdifError::Format(m) => write!(f, "damaged disk image: {m}"),
            UdifError::Unsupported(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for UdifError {}

impl From<io::Error> for UdifError {
    fn from(e: io::Error) -> Self {
        UdifError::Io(e)
    }
}

impl From<udif::DppError> for UdifError {
    fn from(e: udif::DppError) -> Self {
        match e {
            udif::DppError::Io(e) => UdifError::Io(e),
            udif::DppError::Unsupported(m) => UdifError::Unsupported(m),
            udif::DppError::UnsupportedCompression(c) => UdifError::Unsupported(format!("unsupported block type {c:#x}")),
            e => UdifError::Format(e.to_string()),
        }
    }
}

impl From<UdifError> for io::Error {
    fn from(e: UdifError) -> Self {
        match e {
            UdifError::Io(e) => e,
            e => io::Error::new(io::ErrorKind::InvalidData, e.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// What a file with a disk-image extension really is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageKind {
    /// Not a known container: an image to be searched for volumes as it is (raw HFS+/APFS image, disk dump...).
    Plain,
    /// UDIF container (ends with the `koly` trailer).
    Udif,
    /// A container recognised by its signature, but not supported (the text names it).
    Unsupported(&'static str),
}

/// Look at the first and last bytes of a file. Raw devices are never probed.
pub fn probe(path: &Path) -> io::Result<ImageKind> {
    let mut file = open_read_shared(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Ok(ImageKind::Plain);
    }
    let len = meta.len();
    if len >= KOLY_SIZE {
        let mut magic = [0u8; 4];
        file.seek(SeekFrom::Start(len - KOLY_SIZE))?;
        file.read_exact(&mut magic)?;
        if &magic == b"koly" {
            return Ok(ImageKind::Udif);
        }
    }
    let mut head = [0u8; 8];
    file.seek(SeekFrom::Start(0))?;
    let mut got = 0;
    while got < head.len() {
        match file.read(&mut head[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(match &head[..got] {
        h if h.starts_with(b"encrcdsa") || h.starts_with(b"cdsaencr") => ImageKind::Unsupported("encrypted disk image (AES encrypted DMG)"),
        h if h.starts_with(b"sprs") => ImageKind::Unsupported("sparse disk image (.sparseimage)"),
        _ => ImageKind::Plain,
    })
}

// ---------------------------------------------------------------------------
// Parsed container
// ---------------------------------------------------------------------------

/// One `blkx` entry ("partition") of the container.
#[derive(Debug, Clone)]
pub struct UdifPartition {
    /// Position in the property list (stable; used to address the partition).
    pub index: usize,
    pub id: i32,
    /// Name from the container, e.g. `disk image (Apple_HFS : 0)`.
    pub name: String,
    pub first_sector: u64,
    pub sectors: u64,
    /// Stored (compressed) size.
    pub compressed: u64,
}

impl UdifPartition {
    /// Byte offset of the partition on the virtual disk.
    pub fn offset(&self) -> u64 {
        self.first_sector * SECTOR
    }

    /// Uncompressed size in bytes.
    pub fn size(&self) -> u64 {
        self.sectors * SECTOR
    }
}

/// One contiguous piece of the virtual disk and where its data is stored.
#[derive(Debug, Clone, Copy)]
struct Run {
    /// Byte offset on the virtual disk.
    start: u64,
    len: u64,
    kind: BlockType,
    /// Stored data: offset relative to the data fork, and length.
    comp_off: u64,
    comp_len: u64,
}

/// Parsed container: partition list and the block map of the whole virtual disk.
#[derive(Debug)]
pub struct UdifImage {
    pub path: PathBuf,
    /// Modification time of the file, unix seconds (UTC).
    pub mtime: Option<i64>,
    pub partitions: Vec<UdifPartition>,
    data_fork_offset: u64,
    len: u64,
    /// Sorted by `start`, no overlaps.
    runs: Vec<Run>,
}

impl UdifImage {
    /// Parse the container. Checksums are not verified (see [`UdifImage::verify`]).
    pub fn open(path: &Path) -> Result<UdifImage, UdifError> {
        let file = open_read_shared(path)?;
        let mtime = file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        let reader = DmgReader::with_options(file, DmgReaderOptions { verify_checksums: false })?;
        let koly = reader.koly();
        if koly.segment_count > 1 {
            return Err(UdifError::Unsupported("segmented disk image (.dmgpart): open the complete image instead".into()));
        }
        let mut partitions = Vec::new();
        let mut runs: Vec<Run> = Vec::new();
        let overflow = || UdifError::Format("block map is out of range".into());
        for (index, p) in reader.partitions().iter().enumerate() {
            let bm = &p.block_map;
            partitions.push(UdifPartition {
                index,
                id: p.id,
                name: p.name.clone(),
                first_sector: bm.first_sector,
                sectors: bm.sector_count,
                compressed: bm.compressed_size(),
            });
            for r in &bm.block_runs {
                if matches!(r.block_type, BlockType::Comment | BlockType::End) || r.sector_count == 0 {
                    continue;
                }
                let start = bm.first_sector.checked_add(r.sector_number).and_then(|s| s.checked_mul(SECTOR)).ok_or_else(overflow)?;
                let len = r.sector_count.checked_mul(SECTOR).ok_or_else(overflow)?;
                start.checked_add(len).ok_or_else(overflow)?;
                runs.push(Run { start, len, kind: r.block_type, comp_off: r.compressed_offset, comp_len: r.compressed_length });
            }
        }
        // Overlapping runs only occur in damaged images; the first one wins.
        runs.sort_by_key(|r| r.start);
        let mut clean: Vec<Run> = Vec::with_capacity(runs.len());
        let mut end = 0u64;
        for r in runs {
            if r.start >= end {
                end = r.start + r.len;
                clean.push(r);
            }
        }
        let len = koly.sector_count.saturating_mul(SECTOR).max(end);
        Ok(UdifImage { path: path.to_path_buf(), mtime, partitions, data_fork_offset: koly.data_fork_offset, len, runs: clean })
    }

    /// Size of the virtual disk in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

// ---------------------------------------------------------------------------
// Virtual disk
// ---------------------------------------------------------------------------

/// Random access to the decompressed virtual disk of a [`UdifImage`].
pub struct UdifDisk {
    file: File,
    image: Arc<UdifImage>,
    tick: u64,
    cache: HashMap<usize, (Vec<u8>, u64)>,
    cached_bytes: usize,
}

impl UdifDisk {
    pub fn open(image: Arc<UdifImage>) -> io::Result<UdifDisk> {
        let file = open_read_shared(&image.path)?;
        Ok(UdifDisk { file, image, tick: 0, cache: HashMap::new(), cached_bytes: 0 })
    }

    pub fn image(&self) -> &Arc<UdifImage> {
        &self.image
    }

    pub fn len(&self) -> u64 {
        self.image.len
    }

    pub fn is_empty(&self) -> bool {
        self.image.len == 0
    }

    /// Read up to `buf.len()` bytes at `off`; short only at the end of the disk. Holes between
    /// the block runs read as zeros.
    pub fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let image = self.image.clone();
        let mut off = off;
        let mut done = 0usize;
        while done < buf.len() && off < image.len {
            let remaining = (buf.len() - done) as u64;
            let idx = image.runs.partition_point(|r| r.start + r.len <= off);
            let n;
            match image.runs.get(idx) {
                Some(r) if r.start <= off => {
                    let inner = off - r.start;
                    n = remaining.min(r.len - inner).min(image.len - off) as usize;
                    self.read_run(idx, inner, &mut buf[done..done + n])?;
                }
                next => {
                    let hole_end = next.map_or(image.len, |r| r.start).min(image.len);
                    n = remaining.min(hole_end - off) as usize;
                    buf[done..done + n].fill(0);
                }
            }
            done += n;
            off += n as u64;
        }
        Ok(done)
    }

    fn read_run(&mut self, idx: usize, inner: u64, out: &mut [u8]) -> io::Result<()> {
        let image = self.image.clone();
        let r = image.runs[idx];
        match r.kind {
            BlockType::ZeroFill | BlockType::Comment | BlockType::End => out.fill(0),
            BlockType::Raw | BlockType::Ignore => {
                // Stored as is: `comp_len` bytes, the rest of the run (if any) is zero.
                out.fill(0);
                let avail = r.comp_len.min(r.len);
                if inner < avail {
                    let n = (avail - inner).min(out.len() as u64) as usize;
                    self.file.seek(SeekFrom::Start(image.data_fork_offset + r.comp_off + inner))?;
                    self.file.read_exact(&mut out[..n])?;
                }
            }
            _ => {
                self.tick += 1;
                if !self.cache.contains_key(&idx) {
                    let data = self.decode_run(&image, &r)?;
                    self.evict(data.len());
                    self.cached_bytes += data.len();
                    self.cache.insert(idx, (data, self.tick));
                }
                let entry = self.cache.get_mut(&idx).expect("run was just cached");
                entry.1 = self.tick;
                let from = inner as usize;
                out.copy_from_slice(&entry.0[from..from + out.len()]);
            }
        }
        Ok(())
    }

    /// Drop the least recently used runs until `incoming` more bytes fit.
    fn evict(&mut self, incoming: usize) {
        while self.cached_bytes + incoming > CACHE_BUDGET {
            let Some(oldest) = self.cache.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| *k) else { break };
            if let Some((data, _)) = self.cache.remove(&oldest) {
                self.cached_bytes -= data.len();
            }
        }
    }

    fn decode_run(&mut self, image: &UdifImage, r: &Run) -> Result<Vec<u8>, UdifError> {
        if r.len > MAX_RUN_BYTES || r.comp_len > MAX_RUN_BYTES {
            return Err(UdifError::Unsupported(format!("block of {} MiB is too large to decode", r.len.max(r.comp_len) >> 20)));
        }
        let mut comp = vec![0u8; r.comp_len as usize];
        self.file.seek(SeekFrom::Start(image.data_fork_offset + r.comp_off))?;
        self.file.read_exact(&mut comp)?;
        let mut out = vec![0u8; r.len as usize];
        decode_block(r.kind, &comp, &mut out)?;
        Ok(out)
    }
}

/// Decompress one block run; the result must have exactly the length the block map declares.
pub fn decode_block(kind: BlockType, comp: &[u8], out: &mut [u8]) -> Result<(), UdifError> {
    fn fill<R: Read>(mut r: R, out: &mut [u8], what: &str) -> Result<(), UdifError> {
        let mut got = 0;
        while got < out.len() {
            match r.read(&mut out[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(UdifError::Format(format!("{what}: {e}"))),
            }
        }
        if got != out.len() {
            return Err(UdifError::Format(format!("{what} block decoded to {got} bytes, expected {}", out.len())));
        }
        Ok(())
    }
    match kind {
        BlockType::Zlib => fill(flate2::read::ZlibDecoder::new(comp), out, "zlib"),
        BlockType::Bzip2 => fill(bzip2::read::BzDecoder::new(comp), out, "bzip2"),
        BlockType::Xz => fill(lzma_rust2::XzReader::new(comp, false), out, "xz"),
        BlockType::Lzfse => {
            let mut v = Vec::with_capacity(out.len());
            let n = lzfse_rust::decode_bytes(comp, &mut v).map_err(|e| UdifError::Format(format!("LZFSE: {e:?}")))?;
            if n as usize != out.len() || v.len() != out.len() {
                return Err(UdifError::Format(format!("lzfse block decoded to {n} bytes, expected {}", out.len())));
            }
            out.copy_from_slice(&v);
            Ok(())
        }
        BlockType::Adc => Err(UdifError::Unsupported("ADC compressed disk image (very old Mac OS format)".into())),
        other => Err(UdifError::Format(format!("unexpected block type {other:?}"))),
    }
}
