//! Macintosh file containers that hold one file with both forks and its Finder metadata, shown as a small virtual volume:
//!
//! * **AppleDouble / AppleSingle** ([RFC 1740](https://datatracker.ietf.org/doc/html/rfc1740); the `._name` files of macOS,
//!   `.AppleDouble` folders, `.adouble`),
//! * **MacBinary** I, II and III (`.bin`, `.macbin`),
//! * **BinHex 4.0** (`.hqx`).
//!
//! The volume holds
//!
//! * `!APPLEDOUBLE-INFO.txt` / `!APPLESINGLE-INFO.txt` / `!MACBINARY-INFO.txt` / `!BINHEX-INFO.txt` – readable description of
//!   the metadata (name, type and creator, Finder flags, dates, forks, comment, icon, extended attributes, checks of the checksums),
//! * the data fork under the name of the file (when it has one) and
//! * the resource fork as `<name>.rsrc` (when it has one).
//!
//! The same convention as for classic volumes ([`crate::classic`]): MacRoman names, `/` -> `:`, dates of the classic formats are
//! local time of the creating Mac and are passed on as if they were UTC.

use std::io::Write;
use std::sync::{Arc, Mutex};

use hfs_reader::decode_mac_roman;

use crate::classic::{rsrc_name, shown};
use crate::detect::FsKind;
use crate::source::DiskSource;
use crate::util;
use crate::volume::{Node, NodeKind, VolumeError};

/// BinHex has to be decoded as a whole; anything above this is refused.
pub const MAX_BINHEX_BYTES: usize = 512 << 20;
const APPLE_DOUBLE_MAGIC: u32 = 0x0005_1607;
const APPLE_SINGLE_MAGIC: u32 = 0x0005_1600;
/// 2000-01-01 00:00:00 UTC as unix time (epoch of the AppleDouble dates).
const Y2000: i64 = 946_684_800;
const MAX_ENTRIES: usize = 1024;

// ---------------------------------------------------------------------------
// checksum
// ---------------------------------------------------------------------------

/// CRC-16/XMODEM (CCITT polynomial 0x1021, no reflection) as used by MacBinary II/III and BinHex; `crc` is the running value (start with 0).
pub fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

// ---------------------------------------------------------------------------
// description text
// ---------------------------------------------------------------------------

/// What the containers say about the file; [`Meta::render`] makes the info text of it.
#[derive(Default)]
struct Meta {
    name: String,
    file_type: Option<[u8; 4]>,
    creator: Option<[u8; 4]>,
    flags: Option<u16>,
    location: Option<(i16, i16)>,
    folder: Option<i16>,
    created: Option<i64>,
    modified: Option<i64>,
    backup: Option<i64>,
    access: Option<i64>,
    /// the dates are local time of the creating Mac (classic formats) and not UTC
    local_dates: bool,
    data_len: Option<u64>,
    rsrc_len: Option<u64>,
    comment: Option<String>,
    locked: Option<bool>,
    protected: Option<bool>,
    /// further lines (`label: value`), in order
    extra: Vec<(String, String)>,
    /// free text blocks that follow the table (icon, entry list, extended attributes)
    blocks: Vec<String>,
    notes: Vec<String>,
}

fn code(c: [u8; 4]) -> String {
    let t: String = c.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '?' }).collect();
    format!("'{t}'")
}

const FINDER_FLAGS: [(u16, &str); 11] = [
    (0x8000, "alias"),
    (0x4000, "invisible"),
    (0x2000, "has bundle"),
    (0x1000, "name locked"),
    (0x0800, "stationery"),
    (0x0400, "custom icon"),
    (0x0100, "has been inited"),
    (0x0080, "no INITs"),
    (0x0040, "shared"),
    (0x0020, "requires switch launch"),
    (0x0001, "on desktop"),
];

fn flag_names(f: u16) -> String {
    let mut v: Vec<String> = FINDER_FLAGS.iter().filter(|(m, _)| f & m != 0).map(|(_, n)| n.to_string()).collect();
    let color = (f >> 1) & 7;
    if color != 0 {
        v.push(format!("label colour {color}"));
    }
    if v.is_empty() { "none".to_string() } else { v.join(", ") }
}

impl Meta {
    fn render(&self, title: &str, container: &[(&str, String)]) -> String {
        let mut t = format!("{title}\r\n{}\r\n\r\n", "=".repeat(title.len()));
        let mut row = |k: &str, v: &str| t.push_str(&format!("{:<20}{v}\r\n", format!("{k}:")));
        row("Name", &self.name);
        for (k, v) in container {
            row(k, v);
        }
        if let (Some(a), Some(b)) = (self.file_type, self.creator) {
            row("Type / creator", &format!("{} / {}", code(a), code(b)));
        }
        if let Some(f) = self.flags {
            row("Finder flags", &format!("0x{f:04X} ({})", flag_names(f)));
        }
        if let Some((v, h)) = self.location {
            row("Icon position", &format!("v {v}, h {h}"));
        }
        if let Some(f) = self.folder {
            row("Folder (window)", &f.to_string());
        }
        let when = |u: i64| {
            let s = util::format_utc(u);
            if self.local_dates { format!("{s} (local time of the Mac, no time zone)") } else { format!("{s} UTC") }
        };
        for (k, v) in [("Created", self.created), ("Modified", self.modified), ("Backup", self.backup), ("Accessed", self.access)] {
            if let Some(u) = v {
                row(k, &when(u));
            }
        }
        if let (Some(d), Some(r)) = (self.data_len, self.rsrc_len) {
            row("Forks", &format!("data {d} bytes, resource {r} bytes"));
        } else if let Some(d) = self.data_len {
            row("Data fork", &format!("{d} bytes"));
        } else if let Some(r) = self.rsrc_len {
            row("Resource fork", &format!("{r} bytes"));
        }
        if let Some(l) = self.locked {
            row("Locked", if l { "yes" } else { "no" });
        }
        if let Some(p) = self.protected {
            row("Protected", if p { "yes" } else { "no" });
        }
        for (k, v) in &self.extra {
            row(k, v);
        }
        if let Some(c) = &self.comment {
            t.push_str(&format!("\r\nComment\r\n-------\r\n{}\r\n", c.replace('\r', "\r\n").replace("\r\n\n", "\r\n")));
        }
        for b in &self.blocks {
            t.push_str("\r\n");
            t.push_str(b);
        }
        if !self.notes.is_empty() {
            t.push_str("\r\nNotes\r\n-----\r\n");
            for n in &self.notes {
                t.push_str(&format!("* {n}\r\n"));
            }
        }
        t
    }
}

fn mac_date(secs: u32) -> Option<i64> {
    util::hfs_to_unix(secs)
}

// ---------------------------------------------------------------------------
// the virtual volume
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Data {
    /// Bytes of the source.
    Range { start: u64, len: u64 },
    /// Bytes of a decoded buffer.
    Mem { buf: Arc<Vec<u8>>, start: usize, len: usize },
    Text(String),
}

struct Item {
    name: String,
    mtime: Option<i64>,
    data: Data,
}

impl Item {
    fn size(&self) -> u64 {
        match &self.data {
            Data::Range { len, .. } => *len,
            Data::Mem { len, .. } => *len as u64,
            Data::Text(t) => t.len() as u64,
        }
    }
}

/// One file container as a volume with a flat list of virtual files.
pub struct ForkVolume {
    src: Arc<Mutex<DiskSource>>,
    name: String,
    items: Vec<Item>,
}

impl ForkVolume {
    /// Name of the file inside the container.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn list_directory(&self, path: &str) -> Result<Vec<Node>, VolumeError> {
        if !(path.is_empty() || path == "/") {
            return Err(VolumeError::NotFound(path.to_string()));
        }
        Ok(self.items.iter().map(|i| Node { name: i.name.clone(), kind: NodeKind::File, size: i.size(), mtime: i.mtime }).collect())
    }

    pub fn read_file_to(&mut self, path: &str, out: &mut dyn Write) -> Result<u64, VolumeError> {
        let key = path.trim_matches('/');
        let item = self.items.iter().find(|i| i.name == key).ok_or_else(|| VolumeError::NotFound(path.to_string()))?;
        match &item.data {
            Data::Text(t) => out.write_all(t.as_bytes())?,
            Data::Mem { buf, start, len } => out.write_all(&buf[*start..*start + *len])?,
            Data::Range { start, len } => {
                let mut off = *start;
                let mut left = *len;
                let mut buf = vec![0u8; 1 << 20];
                while left > 0 {
                    let n = left.min(buf.len() as u64) as usize;
                    {
                        let mut s = self.src.lock().unwrap_or_else(|e| e.into_inner());
                        s.read_exact_at(off, &mut buf[..n])?;
                    }
                    out.write_all(&buf[..n])?;
                    off += n as u64;
                    left -= n as u64;
                }
            }
        }
        Ok(item.size())
    }

    /// Open a file container found by [`crate::detect`] (`start`/`len`: the container inside the source).
    pub fn open(src: &Arc<Mutex<DiskSource>>, kind: FsKind, start: u64, len: u64) -> Result<Self, VolumeError> {
        let source_name = src.lock().unwrap_or_else(|e| e.into_inner()).name().map(str::to_string);
        let (name, items) = match kind {
            FsKind::AppleDouble => open_apple_double(src, start, len, source_name)?,
            FsKind::MacBinary => open_mac_binary(src, start, len)?,
            FsKind::BinHex => open_binhex(src, start, len)?,
            _ => return Err(VolumeError::Format("not a file container".into())),
        };
        Ok(ForkVolume { src: src.clone(), name, items })
    }
}

fn read_source(src: &Arc<Mutex<DiskSource>>, off: u64, n: usize) -> Result<Vec<u8>, VolumeError> {
    let mut b = vec![0u8; n];
    src.lock().unwrap_or_else(|e| e.into_inner()).read_exact_at(off, &mut b)?;
    Ok(b)
}

/// Info text, data fork and resource fork as virtual files with unique names.
fn assemble(info_name: &str, info: String, name: &str, mtime: Option<i64>, data: Option<Data>, rsrc: Option<Data>) -> Vec<Item> {
    let mut taken = std::collections::HashSet::new();
    let mut items = Vec::new();
    items.push(Item { name: shown(info_name, &mut taken), mtime: None, data: Data::Text(info) });
    let base = shown(name, &mut taken);
    if let Some(d) = data {
        items.push(Item { name: base.clone(), mtime, data: d });
    }
    if let Some(r) = rsrc {
        let rn = rsrc_name(&base, &mut taken);
        items.push(Item { name: rn, mtime, data: r });
    }
    items
}

// ---------------------------------------------------------------------------
// AppleDouble / AppleSingle
// ---------------------------------------------------------------------------

/// Is there an AppleDouble / AppleSingle header at the start of `h`? (magic and version 1 or 2)
pub fn is_apple_double(h: &[u8]) -> bool {
    h.len() >= 26 && matches!(be32(h, 0), APPLE_DOUBLE_MAGIC | APPLE_SINGLE_MAGIC) && matches!(be32(h, 4), 0x0001_0000 | 0x0002_0000)
}

struct Entry {
    id: u32,
    off: u64,
    len: u64,
}

fn entry_name(id: u32) -> &'static str {
    match id {
        1 => "Data Fork",
        2 => "Resource Fork",
        3 => "Real Name",
        4 => "Comment",
        5 => "Icon, B&W",
        6 => "Icon, Colour",
        7 => "File Info (version 1)",
        8 => "File Dates Info",
        9 => "Finder Info",
        10 => "Macintosh File Info",
        11 => "ProDOS File Info",
        12 => "MS-DOS File Info",
        13 => "Short Name",
        14 => "AFP File Info",
        15 => "Directory ID",
        _ => "(unknown)",
    }
}

fn open_apple_double(src: &Arc<Mutex<DiskSource>>, start: u64, len: u64, source_name: Option<String>) -> Result<(String, Vec<Item>), VolumeError> {
    let head = read_source(src, start, 26.min(len as usize))?;
    if !is_apple_double(&head) {
        return Err(VolumeError::Format("not an AppleDouble / AppleSingle file".into()));
    }
    let single = be32(&head, 0) == APPLE_SINGLE_MAGIC;
    let version = be32(&head, 4);
    let count = be16(&head, 24) as usize;
    if count > MAX_ENTRIES {
        return Err(VolumeError::Format(format!("the header claims {count} entries")));
    }
    let table = read_source(src, start + 26, (count * 12).min(len.saturating_sub(26) as usize))?;
    let mut meta = Meta::default();
    let mut entries = Vec::new();
    for c in table.chunks_exact(12) {
        let e = Entry { id: be32(c, 0), off: u64::from(be32(c, 4)), len: u64::from(be32(c, 8)) };
        if e.off + e.len > len {
            meta.notes.push(format!("entry {} ({}) lies outside the file (offset {}, length {}) and is ignored", e.id, entry_name(e.id), e.off, e.len));
        } else {
            entries.push(e);
        }
    }
    if table.len() < count * 12 {
        meta.notes.push("the entry table is cut off".into());
    }
    let find = |id: u32| entries.iter().find(|e| e.id == id);
    let bytes = |e: &Entry, max: u64| read_source(src, start + e.off, e.len.min(max) as usize);

    let real_name = match find(3) {
        Some(e) if e.len > 0 => Some(decode_mac_roman(&bytes(e, 255)?)),
        _ => None,
    };
    meta.name = real_name
        .clone()
        .or_else(|| source_name.as_deref().map(|n| n.strip_prefix("._").unwrap_or(n).to_string()))
        .unwrap_or_else(|| "(unnamed)".into());
    if real_name.is_none() {
        meta.notes.push("the file has no Real Name entry; the name is taken from the name of the opened file".into());
    }
    if let Some(e) = find(4) {
        meta.comment = Some(decode_mac_roman(&bytes(e, 4096)?));
    }
    if let Some(e) = find(8)
        && e.len >= 16
    {
        let d = bytes(e, 16)?;
        let date = |o: usize| {
            let v = be32(&d, o) as i32;
            (v != i32::MIN).then(|| i64::from(v) + Y2000)
        };
        (meta.created, meta.modified, meta.backup, meta.access) = (date(0), date(4), date(8), date(12));
    }
    if let Some(e) = find(9) {
        let d = bytes(e, 1 << 20)?;
        if d.len() >= 16 {
            meta.file_type = Some([d[0], d[1], d[2], d[3]]);
            meta.creator = Some([d[4], d[5], d[6], d[7]]);
            meta.flags = Some(be16(&d, 8));
            meta.location = Some((be16(&d, 10) as i16, be16(&d, 12) as i16));
            meta.folder = Some(be16(&d, 14) as i16);
        }
        if d.len() >= 32 {
            let script = d[16 + 8] as i8;
            let xflags = d[16 + 9];
            if script != 0 || xflags != 0 {
                meta.extra.push(("Script / ext. flags".into(), format!("{script} / 0x{xflags:02X}")));
            }
            let comment_id = be16(&d, 16 + 10);
            if comment_id != 0 {
                meta.extra.push(("Comment ID".into(), comment_id.to_string()));
            }
        }
        if d.len() > 32 {
            // macOS keeps extended attributes behind the 32 bytes of Finder Info
            match parse_xattrs(src, start, len, e.off) {
                Some(block) => meta.blocks.push(block),
                None => meta.notes.push(format!(
                    "the Finder Info entry has {} bytes (32 are Finder Info): the rest is probably macOS extended attributes, which could not be decoded",
                    d.len()
                )),
            }
        }
    }
    if let Some(e) = find(10)
        && e.len >= 4
    {
        let d = bytes(e, 4)?;
        meta.locked = Some(d[3] & 1 != 0);
        meta.protected = Some(d[3] & 2 != 0);
    }
    if let Some(e) = find(11)
        && e.len >= 8
    {
        let d = bytes(e, 8)?;
        meta.extra.push(("ProDOS".into(), format!("access 0x{:04X}, file type 0x{:04X}, aux type 0x{:08X}", be16(&d, 0), be16(&d, 2), be32(&d, 4))));
    }
    if let Some(e) = find(12)
        && e.len >= 2
    {
        let d = bytes(e, 2)?;
        let a = d[1];
        let names: Vec<&str> = [(1, "read-only"), (2, "hidden"), (4, "system"), (8, "volume label"), (0x10, "subdirectory"), (0x20, "archive")]
            .iter()
            .filter(|(m, _)| a & m != 0)
            .map(|(_, n)| *n)
            .collect();
        meta.extra.push(("MS-DOS attributes".into(), format!("0x{a:02X} ({})", if names.is_empty() { "normal".into() } else { names.join(", ") })));
    }
    if let Some(e) = find(14)
        && e.len >= 4
    {
        let a = bytes(e, 4)?[3];
        let names: Vec<&str> = [(1, "invisible"), (2, "multi-user"), (4, "system"), (0x40, "backup needed")].iter().filter(|(m, _)| a & m != 0).map(|(_, n)| *n).collect();
        meta.extra.push(("AFP attributes".into(), format!("0x{a:02X} ({})", if names.is_empty() { "none".into() } else { names.join(", ") })));
    }
    if let Some(e) = find(13)
        && e.len > 0
    {
        meta.extra.push(("Short name".into(), decode_mac_roman(&bytes(e, 255)?)));
    }
    if let Some(e) = find(15)
        && e.len >= 4
    {
        meta.extra.push(("Directory ID".into(), be32(&bytes(e, 4)?, 0).to_string()));
    }
    let data_len = find(1).map(|e| e.len);
    let rsrc_len = find(2).map(|e| e.len);
    meta.data_len = data_len;
    meta.rsrc_len = rsrc_len;
    if data_len.is_none() && !single {
        meta.notes.push("AppleDouble header file: the data fork is a separate file, which usually has the same name without the '._' prefix".into());
    }
    if let Some(e) = find(5)
        && e.len >= 128
    {
        let d = bytes(e, 128)?;
        let mut b = String::from("Icon (black and white, 32 x 32)\r\n-------------------------------\r\n");
        for row in d.chunks_exact(4).take(32) {
            let line: String = row.iter().flat_map(|byte| (0..8).rev().map(move |i| if byte >> i & 1 != 0 { "##" } else { "  " })).collect();
            b.push_str(line.trim_end());
            b.push_str("\r\n");
        }
        meta.blocks.push(b);
    }
    if let Some(e) = find(6) {
        meta.notes.push(format!("a colour icon of {} bytes is stored (not shown)", e.len));
    }
    let mut table = String::from("Entries of the file\r\n-------------------\r\n");
    for e in &entries {
        table.push_str(&format!("{:>3}  {:<22} offset {:>8}  length {:>8}\r\n", e.id, entry_name(e.id), e.off, e.len));
    }
    meta.blocks.push(table);

    let kind = if single { "AppleSingle" } else { "AppleDouble" };
    let title = format!("{kind} file (RFC 1740)");
    let info = meta.render(&title, &[("Format version", format!("{}.{}", version >> 16, version & 0xFFFF))]);
    let range = |e: &Entry| Data::Range { start: start + e.off, len: e.len };
    let data = find(1).filter(|e| e.len > 0 || rsrc_len.unwrap_or(0) == 0 && single).map(range);
    let rsrc = find(2).filter(|e| e.len > 0).map(range);
    let info_name = if single { "!APPLESINGLE-INFO.txt" } else { "!APPLEDOUBLE-INFO.txt" };
    let mtime = meta.modified;
    Ok((meta.name.clone(), assemble(info_name, info, &meta.name, mtime, data, rsrc)))
}

/// macOS extended attributes behind the Finder Info of an AppleDouble file (`ATTR` header at +32 of the entry, entries with absolute
/// file offsets). `None` when the structure does not look like it.
fn parse_xattrs(src: &Arc<Mutex<DiskSource>>, start: u64, len: u64, finder_off: u64) -> Option<String> {
    let base = finder_off + 32;
    let h = read_source(src, start + base, 36).ok()?;
    if &h[..4] != b"ATTR" {
        return None;
    }
    let count = be16(&h, 34) as usize;
    if count == 0 || count > 256 {
        return None;
    }
    let mut out = String::from("Extended attributes (macOS)\r\n---------------------------\r\n");
    let mut pos = base + 36;
    for _ in 0..count {
        let e = read_source(src, start + pos, 11).ok()?;
        let (voff, vlen, nlen) = (u64::from(be32(&e, 0)), u64::from(be32(&e, 4)), e[10] as usize);
        if nlen == 0 || voff + vlen > len {
            return None;
        }
        let name = read_source(src, start + pos + 11, nlen).ok()?;
        let name = String::from_utf8_lossy(name.strip_suffix(&[0]).unwrap_or(&name)).into_owned();
        let value = read_source(src, start + voff, vlen.min(512) as usize).ok()?;
        let shown = match std::str::from_utf8(&value) {
            Ok(t) if t.chars().all(|c| !c.is_control() || c == '\n' || c == '\t') => t.replace('\n', " | "),
            _ => format!("{} bytes of binary data", vlen),
        };
        out.push_str(&format!("{name}\r\n    {shown}{}\r\n", if vlen > 512 { " ..." } else { "" }));
        pos += ((11 + nlen as u64) + 3) & !3;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// MacBinary
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub struct MbHeader {
    pub name: Vec<u8>,
    pub file_type: [u8; 4],
    pub creator: [u8; 4],
    pub data_len: u32,
    pub rsrc_len: u32,
    pub created: u32,
    pub modified: u32,
    pub comment_len: u16,
    pub flags: u16,
    /// 1, 2 or 3
    pub generation: u8,
    pub crc_ok: bool,
    pub secondary_len: u16,
    pub location: (i16, i16),
    pub folder: i16,
    pub protected: bool,
    pub script: u8,
    pub ext_flags: u8,
}

fn pad128(n: u64) -> u64 {
    n.div_ceil(128) * 128
}

/// Check a 128 byte block for a MacBinary header and decode it; `file_len` bounds the fork lengths.
pub fn parse_macbinary(h: &[u8], file_len: u64) -> Option<MbHeader> {
    if h.len() < 128 || h[0] != 0 || h[74] != 0 || h[82] != 0 {
        return None;
    }
    let nlen = h[1] as usize;
    if !(1..=63).contains(&nlen) || h[2..2 + nlen].iter().any(|&b| b < 0x20) {
        return None;
    }
    let (data_len, rsrc_len) = (be32(h, 83), be32(h, 87));
    if data_len > 0x7FFF_FFFF || rsrc_len > 0x7FFF_FFFF {
        return None;
    }
    let secondary_len = be16(h, 120);
    let body = 128 + pad128(u64::from(secondary_len)) + u64::from(data_len);
    // the forks must be inside the file; padding of the last block may be missing
    let rsrc_off = 128 + pad128(u64::from(secondary_len)) + pad128(u64::from(data_len));
    if body > file_len || rsrc_off + u64::from(rsrc_len) > file_len {
        return None;
    }
    let crc_ok = crc16(0, &h[..124]) == be16(h, 124);
    let generation = if crc_ok && h[122] >= 129 {
        if &h[102..106] == b"mBIN" || h[122] >= 130 { 3 } else { 2 }
    } else if h[99..126].iter().all(|&b| b == 0) {
        1
    } else {
        return None;
    };
    Some(MbHeader {
        name: h[2..2 + nlen].to_vec(),
        file_type: [h[65], h[66], h[67], h[68]],
        creator: [h[69], h[70], h[71], h[72]],
        data_len,
        rsrc_len,
        created: be32(h, 91),
        modified: be32(h, 95),
        comment_len: if generation >= 2 { be16(h, 99) } else { 0 },
        flags: u16::from(h[73]) << 8 | if generation >= 2 { u16::from(h[101]) } else { 0 },
        generation,
        crc_ok,
        secondary_len: if generation >= 2 { secondary_len } else { 0 },
        location: (be16(h, 75) as i16, be16(h, 77) as i16),
        folder: be16(h, 79) as i16,
        protected: h[81] & 1 != 0,
        script: if generation == 3 { h[106] } else { 0 },
        ext_flags: if generation == 3 { h[107] } else { 0 },
    })
}

fn open_mac_binary(src: &Arc<Mutex<DiskSource>>, start: u64, len: u64) -> Result<(String, Vec<Item>), VolumeError> {
    let h = read_source(src, start, 128)?;
    let m = parse_macbinary(&h, len).ok_or_else(|| VolumeError::Format("not a MacBinary file".into()))?;
    let secondary = pad128(u64::from(m.secondary_len));
    let data_off = start + 128 + secondary;
    let rsrc_off = data_off + pad128(u64::from(m.data_len));
    let comment_off = rsrc_off + pad128(u64::from(m.rsrc_len));
    let name = decode_mac_roman(&m.name);

    let mut meta = Meta { name: name.clone(), local_dates: true, ..Meta::default() };
    meta.file_type = Some(m.file_type);
    meta.creator = Some(m.creator);
    meta.flags = Some(m.flags);
    meta.location = Some(m.location);
    meta.folder = Some(m.folder);
    meta.created = mac_date(m.created);
    meta.modified = mac_date(m.modified);
    meta.data_len = Some(u64::from(m.data_len));
    meta.rsrc_len = Some(u64::from(m.rsrc_len));
    meta.protected = Some(m.protected);
    if m.generation == 3 && (m.script != 0 || m.ext_flags != 0) {
        meta.extra.push(("Script / ext. flags".into(), format!("{} / 0x{:02X}", m.script as i8, m.ext_flags)));
    }
    if m.comment_len > 0 && comment_off + u64::from(m.comment_len) <= start + len {
        meta.comment = Some(decode_mac_roman(&read_source(src, comment_off, m.comment_len as usize)?));
    }
    if m.secondary_len > 0 {
        meta.notes.push(format!("a secondary header of {} bytes is present (skipped)", m.secondary_len));
    }
    if m.generation >= 2 && !m.crc_ok {
        meta.notes.push("the CRC of the header does not match".into());
    }
    let roman = ["I", "II", "III"][m.generation as usize - 1];
    let info = meta.render(
        &format!("MacBinary {roman} file"),
        &[("Header CRC", if m.generation == 1 { "(MacBinary I has none)".into() } else if m.crc_ok { "OK".into() } else { "MISMATCH".into() })],
    );
    let data = (m.data_len > 0 || m.rsrc_len == 0).then_some(Data::Range { start: data_off, len: u64::from(m.data_len) });
    let rsrc = (m.rsrc_len > 0).then_some(Data::Range { start: rsrc_off, len: u64::from(m.rsrc_len) });
    Ok((name.clone(), assemble("!MACBINARY-INFO.txt", info, &name, mac_date(m.modified), data, rsrc)))
}

// ---------------------------------------------------------------------------
// BinHex 4.0
// ---------------------------------------------------------------------------

const BINHEX_ALPHABET: &[u8; 64] = b"!\"#$%&'()*+,-012345689@ABCDEFGHIJKLMNPQRSTUVXYZ[`abcdefhijklmpqr";
const BINHEX_MARKER: &[u8] = b"must be converted with BinHex";

/// Does the start of a file look like BinHex 4.0 (the "(This file must be converted with BinHex 4.0)" line)?
pub fn is_binhex(head: &[u8]) -> bool {
    head.windows(BINHEX_MARKER.len()).any(|w| w == BINHEX_MARKER)
}

/// Text of a `.hqx` file -> bytes of the 8-bit stream (6-bit decoding only, no run-length expansion).
fn binhex_sextets(text: &[u8]) -> Result<Vec<u8>, String> {
    let mut rev = [255u8; 256];
    for (i, &c) in BINHEX_ALPHABET.iter().enumerate() {
        rev[c as usize] = i as u8;
    }
    // the data are between the first ':' after the marker line and the next ':'
    let from = text.windows(BINHEX_MARKER.len()).position(|w| w == BINHEX_MARKER).map_or(0, |p| p + BINHEX_MARKER.len());
    let open = text[from..].iter().position(|&b| b == b':').ok_or("no ':' that starts the BinHex data")? + from + 1;
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut closed = false;
    for &c in &text[open..] {
        match c {
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            b':' => {
                closed = true;
                break;
            }
            _ => {}
        }
        let v = rev[c as usize];
        if v == 255 {
            return Err(format!("invalid character 0x{c:02X} in the BinHex data"));
        }
        bits = bits << 6 | u32::from(v);
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
        if out.len() > MAX_BINHEX_BYTES {
            return Err("the BinHex file decodes to more than the supported size".into());
        }
    }
    if !closed {
        return Err("the BinHex data are cut off (no closing ':')".into());
    }
    Ok(out)
}

/// Undo the run-length encoding (0x90 marker).
fn binhex_rle(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        i += 1;
        if b != 0x90 {
            out.push(b);
            continue;
        }
        let n = *raw.get(i).ok_or("the BinHex data end after a run-length marker")?;
        i += 1;
        if n == 0 {
            out.push(0x90);
        } else {
            let last = *out.last().ok_or("a run-length marker without a byte to repeat")?;
            out.extend(std::iter::repeat_n(last, n as usize - 1));
        }
        if out.len() > MAX_BINHEX_BYTES {
            return Err("the BinHex file decodes to more than the supported size".into());
        }
    }
    Ok(out)
}

fn open_binhex(src: &Arc<Mutex<DiskSource>>, start: u64, len: u64) -> Result<(String, Vec<Item>), VolumeError> {
    if len as usize > MAX_BINHEX_BYTES * 4 / 3 + 65_536 {
        return Err(VolumeError::Format("the BinHex file is too big".into()));
    }
    let text = read_source(src, start, len as usize)?;
    let d = Arc::new(binhex_rle(&binhex_sextets(&text).map_err(VolumeError::Format)?).map_err(VolumeError::Format)?);
    let bad = || VolumeError::Format("the BinHex header is cut off".into());
    let nlen = *d.first().ok_or_else(bad)? as usize;
    let hdr_end = 1 + nlen + 1 + 4 + 4 + 2 + 4 + 4;
    if nlen == 0 || nlen > 63 || d.len() < hdr_end + 2 {
        return Err(bad());
    }
    let name = decode_mac_roman(&d[1..1 + nlen]);
    let p = 1 + nlen + 1;
    let (ftype, creator) = ([d[p], d[p + 1], d[p + 2], d[p + 3]], [d[p + 4], d[p + 5], d[p + 6], d[p + 7]]);
    let flags = be16(&d, p + 8);
    let (dlen, rlen) = (be32(&d, p + 10) as usize, be32(&d, p + 14) as usize);
    let header_ok = crc16(0, &d[..hdr_end + 2]) == 0;
    let data_start = hdr_end + 2;
    let rsrc_start = data_start + dlen + 2;
    let end = rsrc_start + rlen + 2;
    let mut meta = Meta { name: name.clone(), local_dates: true, ..Meta::default() };
    meta.file_type = Some(ftype);
    meta.creator = Some(creator);
    meta.flags = Some(flags);
    meta.data_len = Some(dlen as u64);
    meta.rsrc_len = Some(rlen as u64);
    let (mut data_ok, mut rsrc_ok) = (None, None);
    if d.len() >= end {
        data_ok = Some(crc16(0, &d[data_start..data_start + dlen + 2]) == 0);
        rsrc_ok = Some(crc16(0, &d[rsrc_start..rsrc_start + rlen + 2]) == 0);
    } else {
        meta.notes.push(format!("the file is cut off: {} of {end} decoded bytes are present", d.len()));
    }
    let word = |ok: Option<bool>| match ok {
        Some(true) => "OK",
        Some(false) => "MISMATCH",
        None => "not checked",
    };
    let info = meta.render(
        "BinHex 4.0 file",
        &[
            ("Header CRC", if header_ok { "OK" } else { "MISMATCH" }.into()),
            ("Data fork CRC", word(data_ok).into()),
            ("Resource fork CRC", word(rsrc_ok).into()),
        ],
    );
    // a cut-off fork is shortened to what is there
    let avail = |from: usize, want: usize| d.len().saturating_sub(from).min(want);
    let data = (dlen > 0 || rlen == 0).then(|| Data::Mem { buf: d.clone(), start: data_start.min(d.len()), len: avail(data_start, dlen) });
    let rsrc = (rlen > 0).then(|| Data::Mem { buf: d.clone(), start: rsrc_start.min(d.len()), len: avail(rsrc_start, rlen) });
    Ok((name.clone(), assemble("!BINHEX-INFO.txt", info, &name, None, data, rsrc)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc16(0, b"123456789"), 0x31C3); // CRC-16/XMODEM
        let mut v = b"hello".to_vec();
        let c = crc16(0, &v);
        v.extend_from_slice(&c.to_be_bytes());
        assert_eq!(crc16(0, &v), 0);
    }

    #[test]
    fn rle() {
        assert_eq!(binhex_rle(&[1, 0x90, 0, 2]).unwrap(), [1, 0x90, 2]);
        assert_eq!(binhex_rle(&[7, 0x90, 4]).unwrap(), [7, 7, 7, 7]);
        assert_eq!(binhex_rle(&[7, 0x90, 1]).unwrap(), [7]);
        assert!(binhex_rle(&[0x90, 3]).is_err());
    }

    #[test]
    fn flags() {
        assert_eq!(flag_names(0), "none");
        assert_eq!(flag_names(0x4100), "invisible, has been inited");
    }
}
