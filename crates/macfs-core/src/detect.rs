//! Locating HFS+/HFSX volumes and APFS containers inside a source: bare volume
//! (HFS+ also inside the classic HFS wrapper), GPT, Apple Partition Map and MBR
//! disks, UDIF (DMG) images (searched as the virtual disk they describe) and the
//! `.macfsdev` / `.hfsdev` / `.apfsdev` text files that point at raw devices.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::source::DiskSource;
use crate::udif::{self, ImageKind, UdifError, UdifImage};

/// Which file system a [`Part`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    /// HFS+ / HFSX volume.
    HfsPlus,
    /// APFS container (one or more volumes).
    Apfs,
}

/// One HFS+ volume or APFS container found in a source.
#[derive(Debug, Clone)]
pub struct Part {
    /// Partition name from the partition table (may be empty).
    pub label: String,
    pub start: u64,
    pub len: u64,
    pub kind: FsKind,
}

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Does an HFS+/HFSX volume start at `start`? (volume header lives at +1024)
pub fn hfs_at(src: &mut DiskSource, start: u64, len: u64, label: &str) -> Option<Part> {
    let mut h = [0u8; 512];
    src.read_exact_at(start.checked_add(1024)?, &mut h).ok()?;
    match be16(&h, 0) {
        0x482B | 0x4858 => Some(Part { label: label.to_string(), start, len, kind: FsKind::HfsPlus }),
        // Classic HFS master directory block wrapping an embedded HFS+ volume.
        0x4244 => {
            if be16(&h, 124) != 0x482B {
                return None;
            }
            let block = be32(&h, 20) as u64;
            let first_block_sector = be16(&h, 28) as u64;
            let embed_start = be16(&h, 126) as u64;
            let embed_count = be16(&h, 128) as u64;
            if block == 0 || embed_count == 0 {
                return None;
            }
            let s = start + first_block_sector * 512 + embed_start * block;
            let l = embed_count * block;
            let mut sig = [0u8; 2];
            src.read_exact_at(s.checked_add(1024)?, &mut sig).ok()?;
            (u16::from_be_bytes(sig) == 0x482B)
                .then(|| Part { label: label.to_string(), start: s, len: l, kind: FsKind::HfsPlus })
        }
        _ => None,
    }
}

/// Does an APFS container start at `start`? (`nx_superblock_t`: magic "NXSB" at +32,
/// block size at +36)
pub fn apfs_at(src: &mut DiskSource, start: u64, len: u64, label: &str) -> Option<Part> {
    let mut h = [0u8; 8];
    src.read_exact_at(start.checked_add(32)?, &mut h).ok()?;
    let block = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
    (&h[..4] == b"NXSB" && block.is_power_of_two() && (4096..=65536).contains(&block))
        .then(|| Part { label: label.to_string(), start, len, kind: FsKind::Apfs })
}

/// HFS+/HFSX volume or APFS container at `start`.
pub fn fs_at(src: &mut DiskSource, start: u64, len: u64, label: &str) -> Option<Part> {
    hfs_at(src, start, len, label).or_else(|| apfs_at(src, start, len, label))
}

/// All HFS+ volumes and APFS containers of a source. A bare volume at offset 0
/// wins; otherwise GPT, then Apple Partition Map, then MBR.
pub fn find_volumes(src: &mut DiskSource) -> Vec<Part> {
    let total = src.len();
    if let Some(p) = fs_at(src, 0, total, "") {
        return vec![p];
    }
    let mut parts = gpt(src);
    if parts.is_empty() {
        parts = apm(src);
    }
    if parts.is_empty() {
        parts = mbr(src);
    }
    parts
}

fn gpt(src: &mut DiskSource) -> Vec<Part> {
    for sector in [512u64, 4096] {
        let mut h = [0u8; 92];
        if src.read_exact_at(sector, &mut h).is_err() || &h[0..8] != b"EFI PART" {
            continue;
        }
        let entries_lba = le64(&h, 72);
        let count = le32(&h, 80).min(1024) as usize;
        let size = le32(&h, 84) as usize;
        if !(128..=1024).contains(&size) || count == 0 {
            continue;
        }
        let mut table = vec![0u8; count * size];
        if src.read_exact_at(entries_lba.saturating_mul(sector), &mut table).is_err() {
            continue;
        }
        let mut out = Vec::new();
        for i in 0..count {
            let e = &table[i * size..(i + 1) * size];
            if e[..16].iter().all(|&b| b == 0) {
                continue;
            }
            let (first, last) = (le64(e, 32), le64(e, 40));
            if last < first {
                continue;
            }
            let start = first.saturating_mul(sector);
            let len = (last - first + 1).saturating_mul(sector);
            let units: Vec<u16> = e[56..128.min(size)]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            let name = String::from_utf16_lossy(&units);
            if let Some(p) = fs_at(src, start, len, &name) {
                out.push(p);
            }
        }
        return out;
    }
    Vec::new()
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn apm(src: &mut DiskSource) -> Vec<Part> {
    let mut b = [0u8; 512];
    if src.read_exact_at(0, &mut b).is_err() || be16(&b, 0) != 0x4552 {
        return Vec::new();
    }
    let bs = (be16(&b, 2) as u64).max(512);
    let mut first = [0u8; 512];
    if src.read_exact_at(bs, &mut first).is_err() || be16(&first, 0) != 0x504D {
        return Vec::new();
    }
    let count = be32(&first, 4).min(256) as u64;
    let mut out = Vec::new();
    for i in 1..=count {
        let mut e = [0u8; 128];
        if src.read_exact_at(i * bs, &mut e).is_err() || be16(&e, 0) != 0x504D {
            continue;
        }
        let start = be32(&e, 8) as u64 * bs;
        let len = be32(&e, 12) as u64 * bs;
        let name = cstr(&e[16..48]);
        if let Some(p) = fs_at(src, start, len, &name) {
            out.push(p);
        }
    }
    out
}

fn mbr(src: &mut DiskSource) -> Vec<Part> {
    let mut b = [0u8; 512];
    if src.read_exact_at(0, &mut b).is_err() || b[510] != 0x55 || b[511] != 0xAA {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..4 {
        let e = &b[446 + 16 * i..446 + 16 * (i + 1)];
        // empty, extended, GPT protective
        if matches!(e[4], 0x00 | 0x05 | 0x0F | 0x85 | 0xEE) {
            continue;
        }
        let start = le32(e, 8) as u64 * 512;
        let len = le32(e, 12) as u64 * 512;
        if let Some(p) = fs_at(src, start, len, "") {
            out.push(p);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// .macfsdev / .hfsdev / .apfsdev link files
// ---------------------------------------------------------------------------

/// Contents of a `.macfsdev` / `.hfsdev` / `.apfsdev` file (same format, the extension is only a hint for the user).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DevSpec {
    pub device: PathBuf,
    pub offset: Option<u64>,
    pub length: Option<u64>,
    /// 1-based index among the volumes found in the partition table.
    pub partition: Option<usize>,
}

/// Link file to a device (`.macfsdev`; `.hfsdev` and `.apfsdev` are the older names of the same format)?
pub fn is_devlink(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("macfsdev") || e.eq_ignore_ascii_case("hfsdev") || e.eq_ignore_ascii_case("apfsdev")
    })
}

fn parse_num(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => s.parse::<u64>(),
    };
    r.map_err(|_| format!("invalid number: {s}"))
}

/// Decode a text file: UTF-8 (with or without BOM) or UTF-16 LE/BE with BOM.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let u: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let u: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Parse `key = value` lines (`#` / `;` comments): `device`, `offset`, `length`, `partition`.
/// `device` may also name a disk image file (including a DMG).
pub fn parse_devlink(text: &str) -> Result<DevSpec, String> {
    let mut spec = DevSpec::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("expected key=value: {line}"));
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "device" | "path" => spec.device = PathBuf::from(v.trim_matches('"')),
            "offset" => spec.offset = Some(parse_num(v)?),
            "length" => spec.length = Some(parse_num(v)?),
            "partition" => {
                let n = parse_num(v)? as usize;
                if n == 0 {
                    return Err("partition is 1-based".into());
                }
                spec.partition = Some(n);
            }
            other => return Err(format!("unknown key: {other}")),
        }
    }
    if spec.device.as_os_str().is_empty() {
        return Err("missing device=".into());
    }
    Ok(spec)
}

/// Why a source could not be opened.
#[derive(Debug)]
pub enum LocateError {
    Io(io::Error),
    Config(String),
    /// A container that is recognised but cannot be handled (encrypted or sparse DMG, damaged UDIF...); the text says why.
    Unsupported(String),
}

impl From<UdifError> for LocateError {
    fn from(e: UdifError) -> Self {
        match e {
            UdifError::Io(e) => LocateError::Io(e),
            e => LocateError::Unsupported(e.to_string()),
        }
    }
}

/// Open an image file, raw device or UDIF (DMG) image as a byte source. A UDIF container is
/// presented as the virtual disk it describes; encrypted and sparse images are refused with an explanation.
pub fn open_source(path: &Path) -> Result<DiskSource, LocateError> {
    match udif::probe(path).map_err(LocateError::Io)? {
        ImageKind::Plain => DiskSource::open(path).map_err(LocateError::Io),
        ImageKind::Udif => Ok(DiskSource::open_udif(Arc::new(UdifImage::open(path)?)).map_err(LocateError::Io)?),
        ImageKind::Unsupported(what) => Err(LocateError::Unsupported(format!("{what} is not supported"))),
    }
}

/// Open `path` (image file, DMG or `.macfsdev` / `.hfsdev` / `.apfsdev` link) and find its HFS+ volumes / APFS containers.
pub fn locate(path: &Path) -> Result<(DiskSource, Vec<Part>), LocateError> {
    if !is_devlink(path) {
        let mut src = open_source(path)?;
        let parts = find_volumes(&mut src);
        return Ok((src, parts));
    }
    let bytes = std::fs::read(path).map_err(LocateError::Io)?;
    let spec = parse_devlink(&decode_text(&bytes)).map_err(LocateError::Config)?;
    let mut src = open_source(&spec.device)?;
    let mut parts = match spec.offset {
        Some(off) => {
            let len = spec.length.unwrap_or_else(|| src.len().saturating_sub(off));
            fs_at(&mut src, off, len, "").into_iter().collect()
        }
        None => find_volumes(&mut src),
    };
    if let Some(n) = spec.partition {
        parts = parts.get(n - 1).cloned().into_iter().collect();
    }
    Ok((src, parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devlink_parsing() {
        let s = parse_devlink("# comment\ndevice = \\\\.\\PhysicalDrive2\npartition=2\noffset = 0x10\n").unwrap();
        assert_eq!(s.device, PathBuf::from("\\\\.\\PhysicalDrive2"));
        assert_eq!(s.partition, Some(2));
        assert_eq!(s.offset, Some(16));
        assert!(parse_devlink("offset=1").is_err());
        assert!(parse_devlink("device=x\nfoo=1").is_err());
        assert!(is_devlink(Path::new("a.apfsdev")) && is_devlink(Path::new("a.HFSDEV")) && is_devlink(Path::new("a.macfsdev")));
        assert!(!is_devlink(Path::new("a.img")) && !is_devlink(Path::new("a.dmg")));
    }

    #[test]
    fn utf16_bom() {
        let mut b = vec![0xFF, 0xFE];
        for u in "device=x".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_text(&b), "device=x");
    }
}
