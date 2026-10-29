//! Classic Macintosh volumes: HFS (hard disks and 800K/1.4M floppies of System 2 - Mac OS 9, crate `hfs-reader`)
//! and MFS (the flat 400K/800K floppies of the first Macs, crate `macfs`). Both are shown through one tree of
//! virtual nodes, read-only.
//!
//! * **Names** are MacRoman decoded; `/` (legal in a classic name) is shown as `:` like the Finder does.
//! * **Resource forks** are shown as a second file `<name>.rsrc` next to the file (classic software keeps most
//!   of its content there) when the fork is not empty, the data fork keeps the plain name.
//! * **Dates** of classic volumes are the local time of the Mac that wrote them and have no time zone; they are
//!   passed on as if they were UTC like the HFS+ ones, so a plugin that converts UTC to local time shows them
//!   shifted by the offset of the local time zone.
//! * HFS is read lazily from the volume (only the catalog is in memory), MFS volumes are small and are loaded whole.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};

use hfs_reader::{HfsError, HfsVolume};
use macfs::{Fork, MfsError, MfsVolume};

use crate::util;
use crate::volume::{Node, NodeKind, VolumeError};

/// Suffix of the virtual file that carries the resource fork.
pub const RSRC_SUFFIX: &str = ".rsrc";
/// MFS images are at most a few MB; anything bigger is not an MFS volume (and would be loaded into memory).
pub const MAX_MFS_BYTES: u64 = 64 << 20;

impl From<HfsError> for VolumeError {
    fn from(e: HfsError) -> Self {
        match e {
            HfsError::Io(e) => VolumeError::Io(e),
            HfsError::Format(m) => VolumeError::Format(m),
        }
    }
}

impl From<MfsError> for VolumeError {
    fn from(e: MfsError) -> Self {
        match e {
            MfsError::Io { source, .. } => VolumeError::Io(source),
            MfsError::FileNotFound(n) => VolumeError::NotFound(n),
            e => VolumeError::Format(e.to_string()),
        }
    }
}

/// Where the bytes of a virtual file come from: index into the files of the volume, `rsrc` selects the fork.
#[derive(Clone, Copy)]
struct Source {
    index: usize,
    rsrc: bool,
}

enum Inner {
    Hfs(Box<HfsVolume<crate::source::Slice>>, Vec<hfs_reader::HfsFileEntry>),
    Mfs(Box<MfsVolume>, Vec<String>),
}

struct Dir {
    nodes: Vec<Node>,
}

/// One classic volume: the listing is built when the volume is opened.
pub struct ClassicVolume {
    inner: Inner,
    name: String,
    /// Directory path (`""` = root, `a/b`) -> its entries.
    dirs: HashMap<String, Dir>,
    /// File path -> source of its bytes.
    files: HashMap<String, Source>,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

/// Name of an entry as shown: `/` -> `:`, unique inside the directory (case sensitive here, the
/// plugin makes the names unique for Windows afterwards).
pub(crate) fn shown(name: &str, taken: &mut HashSet<String>) -> String {
    let mut base = name.replace('/', ":");
    if base.is_empty() {
        base.push('_');
    }
    let mut n = base.clone();
    let mut k = 2;
    while !taken.insert(n.clone()) {
        n = format!("{base} ({k})");
        k += 1;
    }
    n
}

pub(crate) fn rsrc_name(base: &str, taken: &mut HashSet<String>) -> String {
    let mut n = format!("{base}{RSRC_SUFFIX}");
    let mut k = 2;
    while !taken.insert(n.clone()) {
        n = format!("{base}{RSRC_SUFFIX} ({k})");
        k += 1;
    }
    n
}

fn file_node(name: String, size: u64, mtime: u32) -> Node {
    Node { name, kind: NodeKind::File, size, mtime: util::hfs_to_unix(mtime) }
}

impl ClassicVolume {
    /// HFS volume (the MDB is at byte 1024 of `r`).
    pub fn open_hfs(r: crate::source::Slice) -> Result<Self, VolumeError> {
        let mut vol = HfsVolume::open(r)?;
        let files = std::mem::take(&mut vol.files);
        let dirs_in = std::mem::take(&mut vol.dirs);

        // parent CNID -> children; the path of every directory is found by walking down from the root (CNID 2)
        let mut sub: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, d) in dirs_in.iter().enumerate() {
            sub.entry(d.parent_cnid).or_default().push(i);
        }
        let mut in_dir: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, f) in files.iter().enumerate() {
            in_dir.entry(f.parent_cnid).or_default().push(i);
        }

        let mut dirs: HashMap<String, Dir> = HashMap::new();
        let mut file_map: HashMap<String, Source> = HashMap::new();
        let mut visited: HashSet<u32> = HashSet::new();
        let mut stack: Vec<(u32, String)> = vec![(2, String::new())];
        while let Some((cnid, path)) = stack.pop() {
            if !visited.insert(cnid) {
                continue; // a loop in a damaged catalog
            }
            let mut taken = HashSet::new();
            let mut nodes = Vec::new();
            for &i in sub.get(&cnid).map(Vec::as_slice).unwrap_or(&[]) {
                let d = &dirs_in[i];
                let name = shown(&d.name, &mut taken);
                nodes.push(Node { name: name.clone(), kind: NodeKind::Dir, size: 0, mtime: util::hfs_to_unix(d.modify_date) });
                stack.push((d.cnid, join(&path, &name)));
            }
            for &i in in_dir.get(&cnid).map(Vec::as_slice).unwrap_or(&[]) {
                let f = &files[i];
                let name = shown(&f.name, &mut taken);
                nodes.push(file_node(name.clone(), u64::from(f.data_size()), f.modify_date));
                file_map.insert(join(&path, &name), Source { index: i, rsrc: false });
                if f.rsrc_size() > 0 {
                    let rn = rsrc_name(&name, &mut taken);
                    nodes.push(file_node(rn.clone(), u64::from(f.rsrc_size()), f.modify_date));
                    file_map.insert(join(&path, &rn), Source { index: i, rsrc: true });
                }
            }
            dirs.insert(path, Dir { nodes });
        }

        Ok(ClassicVolume { name: vol.volume_name.clone(), inner: Inner::Hfs(Box::new(vol), files), dirs, files: file_map })
    }

    /// MFS volume (a raw image or a Disk Copy 4.2 container, read completely).
    pub fn open_mfs<R: Read + Seek>(r: R) -> Result<Self, VolumeError> {
        let vol = MfsVolume::open(r)?;
        let mut taken = HashSet::new();
        let mut nodes = Vec::new();
        let mut names = Vec::new();
        let mut file_map = HashMap::new();
        for (i, f) in vol.files().enumerate() {
            let name = shown(&f.name, &mut taken);
            nodes.push(file_node(name.clone(), u64::from(f.data_len), f.modified.0));
            file_map.insert(name.clone(), Source { index: i, rsrc: false });
            if f.rsrc_len > 0 {
                let rn = rsrc_name(&name, &mut taken);
                nodes.push(file_node(rn.clone(), u64::from(f.rsrc_len), f.modified.0));
                file_map.insert(rn, Source { index: i, rsrc: true });
            }
            names.push(f.name);
        }
        let mut dirs = HashMap::new();
        dirs.insert(String::new(), Dir { nodes });
        Ok(ClassicVolume { name: vol.info().name, inner: Inner::Mfs(Box::new(vol), names), dirs, files: file_map })
    }

    /// Volume name from the MDB.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn list_directory(&self, path: &str) -> Result<Vec<Node>, VolumeError> {
        let key = path.trim_matches('/');
        self.dirs.get(key).map(|d| d.nodes.clone()).ok_or_else(|| VolumeError::NotFound(path.to_string()))
    }

    pub fn read_file_to(&mut self, path: &str, out: &mut dyn Write) -> Result<u64, VolumeError> {
        let key = path.trim_matches('/');
        let Some(&Source { index, rsrc }) = self.files.get(key) else {
            return Err(VolumeError::NotFound(path.to_string()));
        };
        match &mut self.inner {
            Inner::Hfs(v, files) => {
                let e = &files[index];
                Ok(if rsrc { v.copy_rsrc_fork(e, out)? } else { v.copy_data_fork(e, out)? })
            }
            Inner::Mfs(v, names) => {
                let data = v.read_fork(&names[index], if rsrc { Fork::Resource } else { Fork::Data })?;
                out.write_all(&data)?;
                Ok(data.len() as u64)
            }
        }
    }
}
