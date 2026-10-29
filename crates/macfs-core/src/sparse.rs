//! Apple sparse disk images as one virtual disk, read-only: the **`.sparseimage`** file (signature `sprs`, one file with a
//! band map) and the **`.sparsebundle`** directory (`Info.plist` + `bands/<hex>` files, the format of Time Machine
//! network backups). Both describe a disk of a fixed size that is split into bands; a band that is not stored reads as zeros.
//!
//! Layout used here (libyal/libmodi "Mac OS disk image types", Apple `CopyHFSMeta/SparseBundle.c`):
//!
//! * `.sparseimage`: big-endian header, `"sprs"` at 0, sectors per band at 8, size of the disk in 512-byte sectors at 16,
//!   array of `u32` at 64 whose entry number *i* names the original band (1-based, 0 = unused) that is stored as the *i*-th
//!   band after the header; data of band slot *i* is at `header + i * band size`. The header is 4096 bytes; when the map
//!   does not fit into it (more than 1008 bands) the header is assumed to be rounded up to a multiple of 4096 – this case
//!   is **not** confirmed by any real image I could check.
//! * `.sparsebundle`: `Info.plist` with `band-size` and `size` (bytes), band *n* (covering bytes `n*band-size ..`) is the file
//!   `bands/<n in lowercase hex without padding>`; a missing file is a hole.
//!
//! Encrypted images (`encrcdsa` / `cdsaencr`, in a bundle the signature is in the `token` file) are recognised and refused.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::source::open_read_shared;

const SECTOR: u64 = 512;
const IMAGE_HEADER: u64 = 4096;
const MAP_OFFSET: u64 = 64;
/// Sanity limits: a band is at most 1 GiB, a disk at most 64 TiB, the band map at most 16 M entries.
const MAX_BAND: u64 = 1 << 30;
const MAX_DISK: u64 = 64 << 40;
const MAX_BANDS: u64 = 16 << 20;
/// Number of band files of a bundle that stay open.
const OPEN_BANDS: usize = 8;

/// What a path is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SparseKind {
    /// A `.sparseimage` file (signature `sprs`).
    Image,
    /// A sparse bundle; the path is the bundle directory (the user may have named `Info.plist` inside it).
    Bundle(PathBuf),
}

#[derive(Debug)]
pub enum SparseError {
    Io(io::Error),
    /// Recognised, but encrypted, damaged or otherwise not readable; the text says why.
    Unsupported(String),
}

impl fmt::Display for SparseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SparseError::Io(e) => write!(f, "I/O error: {e}"),
            SparseError::Unsupported(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for SparseError {}

impl From<io::Error> for SparseError {
    fn from(e: io::Error) -> Self {
        SparseError::Io(e)
    }
}

fn bad(m: impl Into<String>) -> SparseError {
    SparseError::Unsupported(m.into())
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn is_encrypted_signature(h: &[u8]) -> bool {
    h.starts_with(b"encrcdsa") || h.starts_with(b"cdsaencr")
}

// ---------------------------------------------------------------------------
// recognition
// ---------------------------------------------------------------------------

/// Content of `Info.plist` that matters.
#[derive(Debug, PartialEq, Eq)]
pub struct BundleInfo {
    pub band_size: u64,
    pub size: u64,
}

/// Read the `band-size` and `size` of an XML property list of a sparse bundle (`None` for any other plist).
/// The numbers may be `<integer>` (what macOS writes) or `<string>`.
pub fn parse_bundle_plist(xml: &[u8]) -> Option<BundleInfo> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut values: HashMap<String, String> = HashMap::new();
    let mut key: Option<String> = None;
    let mut tag = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => tag = e.name().as_ref().to_string(),
            Ok(Event::End(_)) => tag.clear(),
            Ok(Event::Text(t)) => {
                let text = t.xml10_content().trim().to_string();
                if text.is_empty() {
                    continue;
                }
                match tag.as_str() {
                    "key" => key = Some(text),
                    "integer" | "string" | "real" => {
                        if let Some(k) = key.take() {
                            values.insert(k, text);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    if !values.get("diskimage-bundle-type")?.contains("sparsebundle") {
        return None;
    }
    let num = |k: &str| values.get(k)?.parse::<u64>().ok();
    Some(BundleInfo { band_size: num("band-size")?, size: num("size")? })
}

fn read_bundle_info(dir: &Path) -> Option<BundleInfo> {
    let plist = dir.join("Info.plist");
    let meta = fs::metadata(&plist).ok()?;
    if !meta.is_file() || meta.len() > (1 << 20) {
        return None;
    }
    parse_bundle_plist(&fs::read(plist).ok()?)
}

/// Is `path` a sparse image, a sparse bundle (the directory, or `Info.plist` / `Info.bckup` / `token` inside it)
/// or something else (`None`)? Errors of a path that cannot be examined (raw devices) are not reported here:
/// opening reports them.
pub fn probe(path: &Path) -> Option<SparseKind> {
    let meta = fs::metadata(path).ok()?;
    if meta.is_dir() {
        return read_bundle_info(path).map(|_| SparseKind::Bundle(path.to_path_buf()));
    }
    if !meta.is_file() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if matches!(name.as_str(), "info.plist" | "info.bckup" | "token")
        && let Some(dir) = path.parent()
        && dir.join("bands").is_dir()
        && read_bundle_info(dir).is_some()
    {
        return Some(SparseKind::Bundle(dir.to_path_buf()));
    }
    let mut head = [0u8; 4];
    let mut f = open_read_shared(path).ok()?;
    f.read_exact(&mut head).ok()?;
    (&head == b"sprs").then_some(SparseKind::Image)
}

// ---------------------------------------------------------------------------
// the virtual disk
// ---------------------------------------------------------------------------

enum Store {
    Image {
        file: File,
        header: u64,
        /// original band -> slot in the file
        slots: HashMap<u64, u64>,
    },
    Bundle {
        dir: PathBuf,
        /// (band, file or `None` for a hole, use counter): the most recently used band files
        open: Vec<(u64, Option<File>, u64)>,
        tick: u64,
    },
}

pub struct SparseDisk {
    len: u64,
    band: u64,
    store: Store,
}

impl SparseDisk {
    /// Open a `.sparseimage` file.
    pub fn open_image(path: &Path) -> Result<Self, SparseError> {
        let mut file = open_read_shared(path)?;
        let file_len = file.metadata()?.len();
        let mut head = vec![0u8; IMAGE_HEADER as usize];
        file.read_exact(&mut head).map_err(|_| bad("the sparse image is shorter than its header"))?;
        if is_encrypted_signature(&head) {
            return Err(bad("encrypted sparse disk image (AES encrypted .sparseimage) is not supported"));
        }
        if &head[..4] != b"sprs" {
            return Err(bad("not a sparse disk image (no 'sprs' signature)"));
        }
        let band_sectors = u64::from(be32(&head, 8));
        let sectors = u64::from(be32(&head, 16));
        let band = band_sectors * SECTOR;
        if band_sectors == 0 || band > MAX_BAND || sectors == 0 || sectors * SECTOR > MAX_DISK {
            return Err(bad(format!("implausible sparse image geometry ({band_sectors} sectors per band, {sectors} sectors)")));
        }
        let nbands = sectors.div_ceil(band_sectors);
        if nbands > MAX_BANDS {
            return Err(bad("the sparse image has too many bands"));
        }
        let map_end = MAP_OFFSET + 4 * nbands;
        let header = if map_end <= IMAGE_HEADER { IMAGE_HEADER } else { map_end.div_ceil(IMAGE_HEADER) * IMAGE_HEADER };
        if header > IMAGE_HEADER {
            head.resize(header as usize, 0);
            file.seek(SeekFrom::Start(IMAGE_HEADER))?;
            file.read_exact(&mut head[IMAGE_HEADER as usize..]).map_err(|_| bad("the band map of the sparse image is truncated"))?;
        }
        let mut slots = HashMap::new();
        for i in 0..nbands {
            let v = u64::from(be32(&head, (MAP_OFFSET + 4 * i) as usize));
            if v == 0 {
                continue;
            }
            if v > nbands {
                return Err(bad(format!("the band map names band {v} of only {nbands}")));
            }
            if slots.insert(v - 1, i).is_some() {
                return Err(bad(format!("the band map names band {v} twice")));
            }
        }
        let _ = file_len; // bands that are cut off by the end of the file read as zeros
        Ok(SparseDisk { len: sectors * SECTOR, band, store: Store::Image { file, header, slots } })
    }

    /// Open a sparse bundle directory.
    pub fn open_bundle(dir: &Path) -> Result<Self, SparseError> {
        let info = read_bundle_info(dir).ok_or_else(|| bad("Info.plist of the sparse bundle is missing or unreadable"))?;
        if info.band_size == 0 || info.band_size > MAX_BAND || info.size == 0 || info.size > MAX_DISK {
            return Err(bad(format!("implausible sparse bundle geometry (band-size {}, size {})", info.band_size, info.size)));
        }
        if let Ok(mut t) = open_read_shared(&dir.join("token")) {
            let mut h = [0u8; 8];
            let n = t.read(&mut h)?;
            if is_encrypted_signature(&h[..n]) {
                return Err(bad("encrypted sparse bundle (AES encrypted) is not supported"));
            }
        }
        if !dir.join("bands").is_dir() {
            return Err(bad("the sparse bundle has no 'bands' folder"));
        }
        Ok(SparseDisk { len: info.size, band: info.band_size, store: Store::Bundle { dir: dir.to_path_buf(), open: Vec::new(), tick: 0 } })
    }

    /// Size of the virtual disk in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read up to `buf.len()` bytes at `off`; short only at the end of the disk. Holes read as zeros.
    pub fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut done = 0usize;
        let mut off = off;
        while done < buf.len() && off < self.len {
            let band = off / self.band;
            let inner = off % self.band;
            let n = ((buf.len() - done) as u64).min(self.band - inner).min(self.len - off) as usize;
            let out = &mut buf[done..done + n];
            out.fill(0);
            match &mut self.store {
                Store::Image { file, header, slots } => {
                    if let Some(&slot) = slots.get(&band) {
                        read_fill(file, *header + slot * self.band + inner, out)?;
                    }
                }
                Store::Bundle { dir, open, tick } => {
                    *tick += 1;
                    let idx = match open.iter().position(|e| e.0 == band) {
                        Some(i) => i,
                        None => {
                            let f = match open_read_shared(&dir.join("bands").join(format!("{band:x}"))) {
                                Ok(f) => Some(f),
                                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                                Err(e) => return Err(e),
                            };
                            if open.len() >= OPEN_BANDS {
                                let oldest = open.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i).unwrap_or(0);
                                open.swap_remove(oldest);
                            }
                            open.push((band, f, *tick));
                            open.len() - 1
                        }
                    };
                    open[idx].2 = *tick;
                    if let Some(f) = &mut open[idx].1 {
                        read_fill(f, inner, out)?;
                    }
                }
            }
            done += n;
            off += n as u64;
        }
        Ok(done)
    }
}

/// Read as much as the file has at `off` into `out` (the rest stays as it is, zeros).
fn read_fill(file: &mut File, off: u64, out: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(off))?;
    let mut got = 0;
    while got < out.len() {
        match file.read(&mut out[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_forms() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>band-size</key><integer>8388608</integer>
	<key>bundle-backingstore-version</key><integer>1</integer>
	<key>diskimage-bundle-type</key><string>com.apple.diskimage.sparsebundle</string>
	<key>size</key><string>4194304</string>
</dict></plist>"#;
        assert_eq!(parse_bundle_plist(xml), Some(BundleInfo { band_size: 8_388_608, size: 4_194_304 }));
        assert_eq!(parse_bundle_plist(b"<plist><dict><key>size</key><integer>1</integer></dict></plist>"), None);
        assert_eq!(parse_bundle_plist(b"garbage"), None);
    }
}
