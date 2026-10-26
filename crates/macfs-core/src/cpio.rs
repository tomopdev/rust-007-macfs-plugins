//! CPIO archives (the decoded `Payload` / `Scripts` of installer packages): index and directory tree.
//!
//! Apple's installer payloads are `odc` (070707) archives; `newc` (070701) and `crc` (070702) are read as well.
//! [`scan`] walks the headers of a seekable stream and hops over the file data, so listing a payload only touches
//! the bytes that hold headers. The result is an [`Index`] (the entries in archive order, which is what the
//! `!CPIO-INDEX.txt` listing shows) that [`Tree`] turns into directories: CPIO archives may omit the entries of
//! parent directories, so those are made up. Nothing is ever written; extraction is the caller's `Seek` + `Read`
//! of `data_off..data_off + size`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Read, Seek, SeekFrom};

use crate::util;

const TRAILER: &str = "TRAILER!!!";
/// Longest entry name accepted (PATH_MAX is 1024 on macOS).
const MAX_NAME: u64 = 64 * 1024;
/// Longest symlink target read while indexing.
const MAX_LINK: u64 = 64 * 1024;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// POSIX.1 portable format, 76 byte headers, no padding.
    Odc,
    /// SVR4, hexadecimal fields, 4 byte alignment.
    Newc,
    /// SVR4 with a data checksum (not verified).
    Crc,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Odc => "odc (070707)",
            Format::Newc => "newc (070701)",
            Format::Crc => "crc (070702)",
        }
    }
}

/// One archive member.
#[derive(Debug, Clone)]
pub struct Entry {
    /// `/` separated, without leading `./` or `/`; never empty.
    pub path: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    /// Unix seconds.
    pub mtime: i64,
    /// Size of the data stored with this header.
    pub size: u64,
    /// Offset of the data in the stream.
    pub data_off: u64,
    /// Symlink target.
    pub target: Option<String>,
    /// Hard link without data of its own: the entry that carries the data (newc stores it with the last link).
    pub data_from: Option<usize>,
    dev: u64,
    ino: u64,
}

impl Entry {
    pub fn kind(&self) -> u32 {
        self.mode & S_IFMT
    }

    pub fn is_dir(&self) -> bool {
        self.kind() == S_IFDIR
    }

    pub fn is_symlink(&self) -> bool {
        self.kind() == S_IFLNK
    }

    pub fn is_file(&self) -> bool {
        self.kind() == S_IFREG
    }
}

#[derive(Debug)]
pub struct Index {
    pub format: Format,
    pub entries: Vec<Entry>,
    /// Why the scan ended before the trailer (damaged or cut-off archive); the entries before that are valid.
    pub warning: Option<String>,
}

impl Index {
    /// Size and data offset of what extracting the entry yields (hard links read the data of their owner).
    pub fn data_of(&self, i: usize) -> (u64, u64) {
        let e = &self.entries[self.entries[i].data_from.unwrap_or(i)];
        (e.data_off, e.size)
    }

    /// Size shown for the entry.
    pub fn size_of(&self, i: usize) -> u64 {
        if self.entries[i].is_dir() { 0 } else { self.data_of(i).1 }
    }
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

fn number(field: &[u8], radix: u32) -> io::Result<u64> {
    let s = std::str::from_utf8(field).map_err(|_| bad("CPIO header is not text"))?;
    u64::from_str_radix(s.trim_matches(|c| c == ' ' || c == '\0'), radix).map_err(|_| bad(format!("invalid CPIO header field {s:?}")))
}

/// Strip `./`, leading slashes and `.` components; `None` for the archive root.
fn normalize(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Read the header at `pos`. `Ok(None)` is the trailer.
fn header(r: &mut (impl Read + Seek), pos: u64, stream_len: u64) -> io::Result<Option<(Format, Entry, u64)>> {
    r.seek(SeekFrom::Start(pos))?;
    let mut h = [0u8; 110];
    let got = read_full(r, &mut h)?;
    if got < 76 {
        return Err(bad("CPIO header is cut off"));
    }
    let (format, fixed) = match &h[..6] {
        b"070707" => (Format::Odc, 76u64),
        b"070701" => (Format::Newc, 110),
        b"070702" => (Format::Crc, 110),
        m => return Err(bad(format!("invalid CPIO magic {:?} at offset {pos}", String::from_utf8_lossy(m)))),
    };
    if fixed > got as u64 {
        return Err(bad("CPIO header is cut off"));
    }
    let (mode, uid, gid, nlink, mtime, size, namesize, dev, ino);
    if format == Format::Odc {
        let f = |a: usize, n: usize| number(&h[a..a + n], 8);
        dev = f(6, 6)?;
        ino = f(12, 6)?;
        mode = f(18, 6)? as u32;
        uid = f(24, 6)? as u32;
        gid = f(30, 6)? as u32;
        nlink = f(36, 6)? as u32;
        mtime = f(48, 11)?;
        namesize = f(59, 6)?;
        size = f(65, 11)?;
    } else {
        let f = |i: usize| number(&h[6 + 8 * i..14 + 8 * i], 16);
        ino = f(0)?;
        mode = f(1)? as u32;
        uid = f(2)? as u32;
        gid = f(3)? as u32;
        nlink = f(4)? as u32;
        mtime = f(5)?;
        size = f(6)?;
        dev = (f(7)? << 32) | f(8)?;
        namesize = f(11)?;
    }
    if namesize == 0 || namesize > MAX_NAME {
        return Err(bad(format!("invalid CPIO name length {namesize} at offset {pos}")));
    }
    let name_off = pos + fixed;
    r.seek(SeekFrom::Start(name_off))?;
    let mut raw = vec![0u8; namesize as usize];
    r.read_exact(&mut raw).map_err(|_| bad("CPIO entry name is cut off"))?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let name = String::from_utf8_lossy(&raw[..end]).into_owned();

    let align = |v: u64| if format == Format::Odc { v } else { (v + 3) & !3 };
    let data_off = align(name_off + namesize);
    if name == TRAILER {
        return Ok(None);
    }
    let next = align(data_off.checked_add(size).ok_or_else(|| bad("CPIO entry size overflows"))?);
    if data_off + size > stream_len {
        return Err(bad(format!("data of CPIO entry {name:?} is cut off")));
    }
    let entry = Entry {
        path: normalize(&name).unwrap_or_default(),
        mode,
        uid,
        gid,
        nlink,
        mtime: mtime as i64,
        size,
        data_off,
        target: None,
        data_from: None,
        dev,
        ino,
    };
    Ok(Some((format, entry, next)))
}

/// Index a CPIO stream. Fails when not even the first header is valid; a later problem ends the index early
/// and is reported in [`Index::warning`].
pub fn scan(r: &mut (impl Read + Seek)) -> io::Result<Index> {
    let stream_len = r.seek(SeekFrom::End(0))?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut format = None;
    let (mut pos, mut warning) = (0u64, None);
    loop {
        if pos >= stream_len {
            warning = Some("the archive ends without the TRAILER!!! entry".to_string());
            break;
        }
        match header(r, pos, stream_len) {
            Ok(None) => break,
            Ok(Some((f, entry, next))) => {
                format.get_or_insert(f);
                pos = next;
                // the archive root ("." / "./") is only a placeholder
                if !entry.path.is_empty() {
                    entries.push(entry);
                }
            }
            Err(e) if format.is_none() => return Err(e),
            Err(e) => {
                warning = Some(e.to_string());
                break;
            }
        }
    }
    let Some(format) = format else { return Err(bad("the CPIO archive is empty")) };

    // symlink targets (the data of a symlink entry)
    for e in entries.iter_mut().filter(|e| e.is_symlink() && e.size > 0 && e.size <= MAX_LINK) {
        r.seek(SeekFrom::Start(e.data_off))?;
        let mut t = vec![0u8; e.size as usize];
        r.read_exact(&mut t)?;
        let end = t.iter().position(|&b| b == 0).unwrap_or(t.len());
        e.target = Some(String::from_utf8_lossy(&t[..end]).into_owned());
    }

    // Hard links: the members of a group (same device and inode, link count > 1) share one copy of the data,
    // which cpio stores with one of them; the others have size 0.
    let mut groups: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if e.is_file() && e.nlink > 1 {
            groups.entry((e.dev, e.ino)).or_default().push(i);
        }
    }
    for members in groups.values().filter(|m| m.len() > 1) {
        if let Some(&owner) = members.iter().find(|&&i| entries[i].size > 0) {
            for &i in members {
                if i != owner && entries[i].size == 0 {
                    entries[i].data_from = Some(owner);
                }
            }
        }
    }
    Ok(Index { format, entries, warning })
}

// ---------------------------------------------------------------------------
// Directory tree
// ---------------------------------------------------------------------------

/// One name in a directory.
#[derive(Debug, Clone)]
pub struct Child {
    pub name: String,
    /// `None`: a directory that only exists because other entries are inside it.
    pub entry: Option<usize>,
    pub is_dir: bool,
}

/// Where a path lives in the tree.
#[derive(Debug, Clone, Copy)]
struct Slot {
    entry: Option<usize>,
    is_dir: bool,
    /// Position of the name in its parent's list.
    pos: usize,
}

/// Directories of an [`Index`].
#[derive(Debug, Default)]
pub struct Tree {
    dirs: HashMap<String, Vec<Child>>,
    lookup: HashMap<String, Slot>,
}

impl Tree {
    pub fn build(index: &Index) -> Tree {
        let mut t = Tree::default();
        t.dirs.insert(String::new(), Vec::new());
        for (i, e) in index.entries.iter().enumerate() {
            let path = e.path.as_str();
            let mut start = 0;
            loop {
                let (end, last) = match path[start..].find('/') {
                    Some(p) => (start + p, false),
                    None => (path.len(), true),
                };
                let (parent, name, full) = (if start == 0 { "" } else { &path[..start - 1] }, &path[start..end], &path[..end]);
                let is_dir = !last || e.is_dir();
                let entry = last.then_some(i);
                match t.lookup.get_mut(full) {
                    // a later entry of the same path replaces the earlier one (extraction would overwrite it too);
                    // a name that is the parent of anything is a directory
                    Some(s) => {
                        let changed = (last && s.entry != entry) || (is_dir && !s.is_dir);
                        if last {
                            s.entry = entry;
                        }
                        s.is_dir |= is_dir;
                        if changed {
                            let c = &mut t.dirs.get_mut(parent).expect("parent was listed")[s.pos];
                            c.entry = s.entry;
                            c.is_dir = s.is_dir;
                        }
                    }
                    None => {
                        let list = t.dirs.entry(parent.to_string()).or_default();
                        t.lookup.insert(full.to_string(), Slot { entry, is_dir, pos: list.len() });
                        list.push(Child { name: name.to_string(), entry, is_dir });
                    }
                }
                if is_dir && !t.dirs.contains_key(full) {
                    t.dirs.insert(full.to_string(), Vec::new());
                }
                if last {
                    break;
                }
                start = end + 1;
            }
        }
        t
    }

    /// Entries of a directory (`""` = root); `None` when there is no such directory.
    pub fn list(&self, dir: &str) -> Option<&[Child]> {
        self.dirs.get(dir).map(Vec::as_slice)
    }

    /// Index of the entry at `path` (`None` for a made-up directory or an unknown path).
    pub fn entry(&self, path: &str) -> Option<usize> {
        self.lookup.get(path).and_then(|s| s.entry)
    }
}

// ---------------------------------------------------------------------------
// `cpio -tv` like listing
// ---------------------------------------------------------------------------

fn mode_string(mode: u32) -> String {
    let kind = match mode & S_IFMT {
        S_IFDIR => 'd',
        S_IFLNK => 'l',
        S_IFREG => '-',
        0o060000 => 'b',
        0o020000 => 'c',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '?',
    };
    let mut s = String::with_capacity(10);
    s.push(kind);
    for (shift, special, set, unset) in [(6, 0o4000, 's', 'S'), (3, 0o2000, 's', 'S'), (0, 0o1000, 't', 'T')] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(match (b & 1 != 0, mode & special != 0) {
            (true, true) => set,
            (false, true) => unset,
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// Text of the `!CPIO-INDEX.txt` file: one line per entry in archive order (CRLF, so it opens in Notepad).
pub fn index_text(index: &Index, heading: &str) -> String {
    let mut t = String::new();
    let _ = write!(t, "{heading}\r\nCPIO format: {}, {} entries\r\n", index.format.name(), index.entries.len());
    if let Some(w) = &index.warning {
        let _ = write!(t, "WARNING: the index is incomplete: {w}\r\n");
    }
    t.push_str("\r\nmode        uid/gid        size  modified (UTC)       path\r\n");
    for (i, e) in index.entries.iter().enumerate() {
        let when = util::format_utc(e.mtime);
        let _ = write!(
            t,
            "{} {:>5}/{:<5} {:>10}  {when}  {}",
            mode_string(e.mode),
            e.uid,
            e.gid,
            if e.is_dir() { 0 } else { index.data_of(i).1 },
            e.path
        );
        if let Some(target) = &e.target {
            let _ = write!(t, " -> {target}");
        }
        if let Some(owner) = e.data_from {
            let _ = write!(t, "  (hard link, data stored with {})", index.entries[owner].path);
        }
        t.push_str("\r\n");
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn odc(name: &str, mode: u32, data: &[u8], ino: u32, nlink: u32) -> Vec<u8> {
        let mut v = format!(
            "070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}",
            1, ino, mode, 501, 20, nlink, 0, 1_700_000_000u64, name.len() + 1, data.len()
        )
        .into_bytes();
        v.extend_from_slice(name.as_bytes());
        v.push(0);
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn odc_index_tree_and_links() {
        let mut a = Vec::new();
        a.extend(odc(".", 0o040755, b"", 1, 2));
        a.extend(odc("./usr/bin/tool", 0o100755, b"", 5, 2)); // hard link without data
        a.extend(odc("./usr/bin/tool2", 0o100755, b"BINARY", 5, 2)); // owner of the data
        a.extend(odc("./usr/bin/sh", 0o120755, b"/bin/zsh", 6, 1));
        a.extend(odc("TRAILER!!!", 0, b"", 0, 1));
        let idx = scan(&mut Cursor::new(&a)).unwrap();
        assert_eq!(idx.format, Format::Odc);
        assert_eq!(idx.entries.len(), 3); // "." is skipped
        assert!(idx.warning.is_none());
        assert_eq!(idx.entries[0].data_from, Some(1));
        assert_eq!(idx.size_of(0), 6);
        assert_eq!(idx.entries[2].target.as_deref(), Some("/bin/zsh"));

        let tree = Tree::build(&idx);
        let root = tree.list("").unwrap();
        assert_eq!(root.len(), 1);
        assert!(root[0].is_dir && root[0].entry.is_none()); // "usr" is made up
        let bin: Vec<_> = tree.list("usr/bin").unwrap().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(bin, ["tool", "tool2", "sh"]);
        assert_eq!(tree.entry("usr/bin/tool2"), Some(1));
        assert!(index_text(&idx, "x").contains("lrwxr-xr-x"));
    }

    #[test]
    fn truncated_archive_keeps_the_valid_part() {
        let mut a = odc("a", 0o100644, b"1234", 1, 1);
        a.extend(odc("b", 0o100644, b"5678", 2, 1));
        a.truncate(a.len() - 2);
        let idx = scan(&mut Cursor::new(&a)).unwrap();
        assert_eq!(idx.entries.len(), 1);
        assert!(idx.warning.is_some());
        assert!(scan(&mut Cursor::new(b"garbage".to_vec())).is_err());
    }
}
