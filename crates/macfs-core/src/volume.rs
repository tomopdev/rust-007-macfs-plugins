//! Uniform read-only view of one macOS volume, HFS+/HFSX or APFS (or an installer package, which is shown
//! like a volume, see [`crate::pkg`]), so the plugins do not care which of them they are walking.
//!
//! APFS support is built on the pure-Rust `apfs` crate (vendored, see
//! `vendor/apfs/PATCHES.md`). That reader has no encryption, no snapshots and no
//! clone relationships, so [`MacVolume`] looks at what the volume uses before it
//! is read and reports the unsupported features through two virtual text files
//! ([`NOTICE_NAME`] in the volume root, [`CLONES_NAME`] once a clone has been
//! seen), which show up in the panel like any other file and extract like one.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use apfs::{ApfsVolume, VolumeInfo};
use hfsplus::HfsVolume;

use crate::detect::{FsKind, Part};
use crate::pkg::PkgVolume;
use crate::source::{DiskSource, Slice};
use crate::util;

/// Virtual file with the list of APFS features of the volume that are not supported yet.
pub const NOTICE_NAME: &str = "!APFS-NOTICE.txt";
/// Virtual file reporting that the volume contains cloned files.
pub const CLONES_NAME: &str = "!APFS-CLONES.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
    Symlink,
}

/// One directory entry, independent of the file system.
#[derive(Debug, Clone)]
pub struct Node {
    /// Raw on-disk name (needed to address the item in the volume).
    pub name: String,
    pub kind: NodeKind,
    pub size: u64,
    /// Modification time, unix seconds (UTC).
    pub mtime: Option<i64>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
}

#[derive(Debug)]
pub enum VolumeError {
    Io(io::Error),
    /// Damaged structures, unsupported features, anything that is not plain I/O.
    Format(String),
    NotFound(String),
}

impl VolumeError {
    pub fn is_io(&self) -> bool {
        matches!(self, VolumeError::Io(_))
    }
}

impl fmt::Display for VolumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VolumeError::Io(e) => write!(f, "I/O error: {e}"),
            VolumeError::Format(m) => f.write_str(m),
            VolumeError::NotFound(p) => write!(f, "not found: {p}"),
        }
    }
}

impl std::error::Error for VolumeError {}

impl From<io::Error> for VolumeError {
    fn from(e: io::Error) -> Self {
        VolumeError::Io(e)
    }
}

impl From<hfsplus::HfsPlusError> for VolumeError {
    fn from(e: hfsplus::HfsPlusError) -> Self {
        match e {
            hfsplus::HfsPlusError::Io(e) => VolumeError::Io(e),
            hfsplus::HfsPlusError::FileNotFound(p) => VolumeError::NotFound(p),
            e => VolumeError::Format(e.to_string()),
        }
    }
}

impl From<apfs::ApfsError> for VolumeError {
    fn from(e: apfs::ApfsError) -> Self {
        match e {
            apfs::ApfsError::Io(e) => VolumeError::Io(e),
            apfs::ApfsError::FileNotFound(p) => VolumeError::NotFound(p),
            e => VolumeError::Format(e.to_string()),
        }
    }
}

enum Fs {
    Hfs(Box<HfsVolume<Slice>>),
    Apfs(Box<ApfsVolume<Slice>>),
    /// Installer package (XAR) with its virtual files and the payload tree.
    Pkg(Box<PkgVolume>),
    /// Volume that cannot be read (encrypted, damaged...): only the notice is shown.
    Unreadable,
}

pub struct MacVolume {
    fs: Fs,
    kind: FsKind,
    /// APFS volume name (empty for HFS+).
    name: String,
    /// Content of [`NOTICE_NAME`] (fixed when the volume is opened).
    notice: Option<String>,
    notice_listed: bool,
    clone_seen: bool,
    clones_listed: bool,
}

fn is_root(path: &str) -> bool {
    path.is_empty() || path == "/"
}

fn text_node(name: &str, text: &str) -> Node {
    Node { name: name.to_string(), kind: NodeKind::File, size: text.len() as u64, mtime: None }
}

/// The virtual [`CLONES_NAME`] file (the same for every volume).
pub fn clones_notice_node() -> Node {
    text_node(CLONES_NAME, CLONES_TEXT)
}

const CLONES_TEXT: &str = "This APFS volume contains cloned files (clonefile / copy-on-write copies, for example Finder duplicates).\r\n\
\r\n\
The contents of cloned files are read through the data they share with the original, so they list and extract\r\n\
like any other file. The clone relationship and the space sharing are not reported - that is not supported yet.\r\n";

/// Text of [`NOTICE_NAME`] for a volume that can be read but uses features the reader does not implement.
fn limitations_text(info: &VolumeInfo) -> Option<String> {
    let mut items: Vec<String> = Vec::new();
    if info.num_snapshots > 0 {
        items.push(format!(
            "Snapshots: the volume has {} snapshot(s). Only the current state of the volume is shown; the contents \
             of the snapshots are not accessible (not supported yet).",
            info.num_snapshots
        ));
    }
    if info.is_sealed() {
        items.push(
            "Sealed volume (signed system volume): the file tree uses hashed nodes that this reader was not written \
             for, so the listing may be incomplete or wrong (not supported yet)."
                .to_string(),
        );
    }
    if info.is_encryption_rolling() {
        items.push(
            "Encryption rolling is in progress: some data may still be encrypted and read as garbage (not supported yet)."
                .to_string(),
        );
    }
    if info.is_incomplete_restore() {
        items.push("The volume is marked as an incomplete restore; part of its data may be missing.".to_string());
    }
    if items.is_empty() {
        return None;
    }
    let mut t = format!("APFS volume \"{}\": features that this plugin does not support yet\r\n\r\n", info.name);
    for i in items {
        t.push_str("* ");
        t.push_str(&i);
        t.push_str("\r\n");
    }
    t.push_str("\r\nThe volume is read-only and only the parts the plugin understands are shown.\r\n");
    Some(t)
}

fn encrypted_text(info: &VolumeInfo) -> String {
    format!(
        "APFS volume \"{}\" is encrypted (FileVault, or hardware encryption of a T2 / Apple silicon Mac).\r\n\
         \r\n\
         Encrypted volumes are not supported yet: both the file names and the file data are ciphertext without the key,\r\n\
         so the contents of this volume are not shown.\r\n",
        info.name
    )
}

fn unreadable_text(name: &str, err: &dyn fmt::Display, sealed: bool) -> String {
    let mut t = format!("APFS volume \"{name}\" could not be opened: {err}\r\n");
    if sealed {
        t.push_str(
            "\r\nThis is a sealed (signed system) volume. Sealed volumes are not supported yet.\r\n",
        );
    }
    t
}

impl MacVolume {
    fn hfs(v: HfsVolume<Slice>) -> Self {
        MacVolume {
            fs: Fs::Hfs(Box::new(v)),
            kind: FsKind::HfsPlus,
            name: String::new(),
            notice: None,
            notice_listed: false,
            clone_seen: false,
            clones_listed: false,
        }
    }

    fn apfs(v: ApfsVolume<Slice>, notice: Option<String>) -> Self {
        let name = v.volume_info().name.clone();
        MacVolume {
            fs: Fs::Apfs(Box::new(v)),
            kind: FsKind::Apfs,
            name,
            notice,
            notice_listed: false,
            clone_seen: false,
            clones_listed: false,
        }
    }

    fn pkg(v: PkgVolume) -> Self {
        MacVolume {
            fs: Fs::Pkg(Box::new(v)),
            kind: FsKind::Pkg,
            name: String::new(),
            notice: None,
            notice_listed: false,
            clone_seen: false,
            clones_listed: false,
        }
    }

    fn unreadable(name: String, text: String) -> Self {
        MacVolume {
            fs: Fs::Unreadable,
            kind: FsKind::Apfs,
            name,
            notice: Some(text),
            notice_listed: false,
            clone_seen: false,
            clones_listed: false,
        }
    }

    pub fn kind(&self) -> FsKind {
        self.kind
    }

    /// APFS volume name (empty for HFS+).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `false` for an APFS volume that is only represented by its notice file.
    pub fn is_readable(&self) -> bool {
        !matches!(self.fs, Fs::Unreadable)
    }

    /// Entries of a directory (`/` separated raw path, `/` = root). The volume root also
    /// carries the virtual [`NOTICE_NAME`] file when the volume uses unsupported features.
    pub fn list_directory(&mut self, path: &str) -> Result<Vec<Node>, VolumeError> {
        let mut nodes = match &mut self.fs {
            Fs::Hfs(v) => v
                .list_directory(path)?
                .into_iter()
                .map(|d| Node {
                    kind: match d.kind {
                        hfsplus::EntryKind::Directory => NodeKind::Dir,
                        hfsplus::EntryKind::Symlink => NodeKind::Symlink,
                        hfsplus::EntryKind::File => NodeKind::File,
                    },
                    size: if d.kind == hfsplus::EntryKind::Directory { 0 } else { d.size },
                    mtime: util::hfs_to_unix(d.modify_date),
                    name: d.name,
                })
                .collect(),
            Fs::Apfs(v) => {
                let list = v.list_directory(path)?;
                if list.iter().any(|d| d.cloned) {
                    self.clone_seen = true;
                }
                list.into_iter()
                    .map(|d| Node {
                        kind: match d.kind {
                            apfs::EntryKind::Directory => NodeKind::Dir,
                            apfs::EntryKind::Symlink => NodeKind::Symlink,
                            apfs::EntryKind::File => NodeKind::File,
                        },
                        size: if d.kind == apfs::EntryKind::Directory { 0 } else { d.size },
                        mtime: (d.modify_time != 0).then(|| d.modify_time.div_euclid(1_000_000_000)),
                        name: d.name,
                    })
                    .collect()
            }
            Fs::Pkg(v) => v.list_directory(path)?,
            Fs::Unreadable if is_root(path) => Vec::new(),
            Fs::Unreadable => return Err(VolumeError::NotFound(path.to_string())),
        };
        if is_root(path)
            && let Some(t) = &self.notice
        {
            nodes.push(text_node(NOTICE_NAME, t));
            self.notice_listed = true;
        }
        Ok(nodes)
    }

    /// Cloned files seen while listing so far: returns the virtual [`CLONES_NAME`] file once,
    /// to be added to the volume root by the walker after it has finished.
    pub fn late_clone_notice(&mut self) -> Option<Node> {
        if self.clone_seen && !self.clones_listed {
            self.clones_listed = true;
            return Some(clones_notice_node());
        }
        None
    }

    /// Stream a file (decmpfs compressed files are decompressed; the virtual notice files are generated).
    pub fn read_file_to(&mut self, path: &str, out: &mut dyn Write) -> Result<u64, VolumeError> {
        let p = path.trim_start_matches('/');
        if p == NOTICE_NAME
            && let Some(t) = &self.notice
        {
            out.write_all(t.as_bytes())?;
            return Ok(t.len() as u64);
        }
        // Served for any APFS volume, not only after a clone was seen: a freshly opened volume
        // (extraction opens the source again) has not walked the tree yet.
        if p == CLONES_NAME && self.kind == FsKind::Apfs {
            out.write_all(CLONES_TEXT.as_bytes())?;
            return Ok(CLONES_TEXT.len() as u64);
        }
        match &mut self.fs {
            Fs::Hfs(v) => Ok(v.read_file_to(path, out)?),
            Fs::Apfs(v) => {
                let mut w: &mut dyn Write = out; // `apfs` wants a `&mut W` with a sized `W`
                Ok(v.read_file_to(path, &mut w)?)
            }
            Fs::Pkg(v) => v.read_file_to(path, out),
            Fs::Unreadable => Err(VolumeError::NotFound(path.to_string())),
        }
    }
}

/// A volume together with the folder name it gets when a source holds several volumes.
pub struct OpenedVolume {
    pub label: String,
    pub vol: MacVolume,
}

/// Open every volume of one partition: one for HFS+ (or a package), one per slot for an APFS container.
/// APFS volumes that cannot be read (encrypted, sealed, damaged) are still returned, as
/// [`MacVolume`]s that only show their notice file.
pub fn open_part(src: &Arc<Mutex<DiskSource>>, part: &Part) -> Result<Vec<MacVolume>, VolumeError> {
    let slice = || Slice::new(src.clone(), part.start, part.len);
    match part.kind {
        FsKind::HfsPlus => Ok(vec![MacVolume::hfs(HfsVolume::open(slice())?)]),
        FsKind::Pkg => Ok(vec![MacVolume::pkg(PkgVolume::open(src.clone(), part.start, part.len)?)]),
        FsKind::Apfs => {
            let slots = apfs::list_volumes(&mut slice())?;
            if slots.is_empty() {
                return Err(VolumeError::Format("the APFS container holds no volume".into()));
            }
            let mut out = Vec::new();
            for slot in slots {
                let label = format!("Volume {}", slot.index + 1);
                let info = match slot.info {
                    Ok(i) => i,
                    Err(e) => {
                        out.push(MacVolume::unreadable(label.clone(), unreadable_text(&label, &e, false)));
                        continue;
                    }
                };
                if info.is_encrypted() {
                    out.push(MacVolume::unreadable(info.name.clone(), encrypted_text(&info)));
                    continue;
                }
                match ApfsVolume::open_index(slice(), slot.index) {
                    Ok(v) => out.push(MacVolume::apfs(v, limitations_text(&info))),
                    Err(e) => out.push(MacVolume::unreadable(
                        info.name.clone(),
                        unreadable_text(&info.name, &e, info.is_sealed()),
                    )),
                }
            }
            Ok(out)
        }
    }
}

/// Open all volumes of all partitions of a source and give each a folder label
/// (`Partition 2 - Macintosh HD`). Partitions that cannot be opened are skipped; their first
/// error is returned only when nothing at all could be opened.
pub fn open_all(src: &Arc<Mutex<DiskSource>>, parts: &[Part]) -> Result<Vec<OpenedVolume>, VolumeError> {
    let mut out = Vec::new();
    let mut first_err = None;
    for (i, p) in parts.iter().enumerate() {
        match open_part(src, p) {
            Ok(vols) => {
                for (k, vol) in vols.into_iter().enumerate() {
                    let tag = match p.kind {
                        FsKind::HfsPlus | FsKind::Pkg => p.label.clone(),
                        FsKind::Apfs if !vol.name().is_empty() => vol.name().to_string(),
                        FsKind::Apfs => format!("Volume {}", k + 1),
                    };
                    let label =
                        if tag.is_empty() { format!("Partition {}", i + 1) } else { format!("Partition {} - {}", i + 1, tag) };
                    out.push(OpenedVolume { label, vol });
                }
            }
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    if out.is_empty() {
        return Err(first_err.unwrap_or_else(|| VolumeError::Format("no volume".into())));
    }
    Ok(out)
}
