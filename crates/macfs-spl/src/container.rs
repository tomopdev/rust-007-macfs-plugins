//! Platform-neutral core: opens an image/disk, exposes its HFS+ volumes, APFS
//! containers and installer packages (PKG) as one directory tree and maps names between the three worlds involved:
//!
//! * `hfs`  – the raw on-disk name (needed to address the item in the volume),
//! * `disp` – what the Salamander panel shows: NFC, Windows-safe, and
//!   representable in the ANSI code page (the plugin API is `char*`),
//! * `real` – the full Unicode Windows-safe name used when extracting to disk.
//!
//! Both `disp` and `real` are made unique (case-insensitively) per directory, so
//! a panel path can always be mapped back to exactly one item.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::detect::{self, FsKind, LocateError};
use crate::util::{self, NameSet};
use crate::volume::{self, CLONES_NAME, MacVolume, Node as VolNode, NodeKind, VolumeError};
use crate::win;

/// Maximum length of a panel name in ANSI bytes (Salamander: `MAX_PATH - 5`).
pub const MAX_DISP_BYTES: usize = 250;

#[derive(Debug)]
pub enum OpenError {
    Io(io::Error),
    Config(String),
    /// Recognised container that cannot be handled (encrypted DMG...).
    Unsupported(String),
    NoVolume,
    Fs(VolumeError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Io(e) => write!(f, "cannot open the source: {e} (raw disks need Salamander run as administrator)"),
            OpenError::Config(m) => write!(f, "invalid .macfsdev / .hfsdev / .apfsdev file: {m}"),
            OpenError::Unsupported(m) => write!(f, "{m}"),
            OpenError::NoVolume => write!(f, "no HFS+/HFSX volume, APFS container or installer package (PKG) found (also looked inside a DMG)"),
            OpenError::Fs(e) => write!(f, "damaged volume: {e}"),
        }
    }
}

/// A directory of the tree: the virtual root of a multi-volume source, or a
/// directory inside one volume.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Node {
    vol: Option<usize>,
    path: String,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub vol: usize,
    /// Path of the item inside its volume (`/` separated, raw names).
    pub vol_path: String,
    pub disp: String,
    pub real: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    /// Modification time, unix seconds (UTC).
    pub mtime: Option<i64>,
}

impl Item {
    /// The directory this item represents (only meaningful for `is_dir`).
    pub fn node(&self) -> Node {
        Node { vol: Some(self.vol), path: self.vol_path.clone() }
    }
}

/// Result of mapping a panel path to the tree.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// `None` for the archive root.
    pub item: Option<Item>,
    /// Real (Unicode) names of every path component, outermost first.
    pub real_rel: Vec<String>,
}

pub struct Container {
    vols: Vec<MacVolume>,
    /// Panel / real name of every volume's folder in the virtual root (multi-volume case only).
    folders: Vec<(String, String)>,
    cache: HashMap<Node, Rc<Vec<Item>>>,
}

impl Container {
    pub fn open(path: &Path) -> Result<Container, OpenError> {
        let (src, parts) = detect::locate(path).map_err(|e| match e {
            LocateError::Io(e) => OpenError::Io(e),
            LocateError::Config(m) => OpenError::Config(m),
            LocateError::Unsupported(m) => OpenError::Unsupported(m),
        })?;
        if parts.is_empty() {
            return Err(OpenError::NoVolume);
        }
        let src = Arc::new(Mutex::new(src));
        let opened = volume::open_all(&src, &parts).map_err(OpenError::Fs)?;
        let (labels, vols): (Vec<String>, Vec<MacVolume>) = opened.into_iter().map(|o| (o.label, o.vol)).unzip();

        // folder names of the virtual root (several volumes only)
        let mut folders = Vec::new();
        if vols.len() > 1 {
            let mut real = NameSet::default();
            let mut disp = NameSet::default();
            for label in &labels {
                let base = util::sanitize_component(label);
                folders.push((disp.unique(win::ansi_safe(&base, MAX_DISP_BYTES)), real.unique(base)));
            }
        }
        Ok(Container { vols, folders, cache: HashMap::new() })
    }

    pub fn root(&self) -> Node {
        if self.vols.len() == 1 {
            Node { vol: Some(0), path: "/".into() }
        } else {
            Node { vol: None, path: String::new() }
        }
    }

    /// Entries of a directory (cached; use [`Container::list_uncached`] for one-shot walks).
    pub fn list(&mut self, node: &Node) -> Result<Rc<Vec<Item>>, VolumeError> {
        if let Some(hit) = self.cache.get(node) {
            return Ok(hit.clone());
        }
        let items = Rc::new(self.build(node)?);
        self.cache.insert(node.clone(), items.clone());
        Ok(items)
    }

    pub fn list_uncached(&mut self, node: &Node) -> Result<Vec<Item>, VolumeError> {
        self.build(node)
    }

    fn build(&mut self, node: &Node) -> Result<Vec<Item>, VolumeError> {
        let Some(vol) = node.vol else {
            // virtual root: one folder per volume
            return Ok(self
                .folders
                .iter()
                .enumerate()
                .map(|(i, (disp, real))| Item {
                    vol: i,
                    vol_path: "/".into(),
                    disp: disp.clone(),
                    real: real.clone(),
                    is_dir: true,
                    is_link: false,
                    size: 0,
                    mtime: None,
                })
                .collect());
        };

        let mut list: Vec<VolNode> = self.vols[vol].list_directory(&node.path)?;
        list.sort_by_cached_key(|d| d.name.to_lowercase());
        let mut real = NameSet::default();
        let mut disp = NameSet::default();
        Ok(list
            .into_iter()
            .map(|d| {
                let base = util::sanitize_component(&d.name);
                let is_dir = d.kind == NodeKind::Dir;
                Item {
                    vol,
                    vol_path: if node.path == "/" { format!("/{}", d.name) } else { format!("{}/{}", node.path, d.name) },
                    disp: disp.unique(win::ansi_safe(&base, MAX_DISP_BYTES)),
                    real: real.unique(base),
                    is_dir,
                    is_link: d.kind == NodeKind::Symlink,
                    size: if is_dir { 0 } else { d.size },
                    mtime: d.mtime,
                }
            })
            .collect())
    }

    fn clones_item(vol: usize) -> Item {
        let n = volume::clones_notice_node();
        let base = util::sanitize_component(&n.name);
        Item {
            vol,
            vol_path: format!("/{}", n.name),
            disp: win::ansi_safe(&base, MAX_DISP_BYTES),
            real: base,
            is_dir: false,
            is_link: false,
            size: n.size,
            mtime: None,
        }
    }

    /// Items that only become known after the whole tree has been walked (the "clones"
    /// notice of APFS volumes), each with the panel path of the directory it belongs to.
    pub fn late_items(&mut self) -> Vec<(String, Item)> {
        let mut out = Vec::new();
        for i in 0..self.vols.len() {
            if self.vols[i].late_clone_notice().is_some() {
                let parent = self.folders.get(i).map(|f| f.0.clone()).unwrap_or_default();
                out.push((parent, Self::clones_item(i)));
            }
        }
        out
    }

    /// Map a panel path (`\` separated, as shown in the panel) to its item.
    pub fn resolve(&mut self, panel_path: &str) -> Option<Resolved> {
        let mut node = self.root();
        let mut real_rel = Vec::new();
        let comps: Vec<&str> = panel_path.split('\\').filter(|c| !c.is_empty()).collect();
        let mut current: Option<Item> = None;
        for (i, comp) in comps.iter().enumerate() {
            let items = self.list(&node).ok()?;
            let lower = comp.to_lowercase();
            let found = items
                .iter()
                .find(|it| it.disp == *comp)
                .or_else(|| items.iter().find(|it| it.disp.to_lowercase() == lower))
                .cloned()
                // the "clones" notice is listed after the walk, so a fresh container has not seen it yet
                .or_else(|| match node.vol {
                    Some(v) if node.path == "/" && self.vols[v].kind() == FsKind::Apfs && lower == CLONES_NAME.to_lowercase() => {
                        Some(Self::clones_item(v))
                    }
                    _ => None,
                })?;
            real_rel.push(found.real.clone());
            if i + 1 < comps.len() {
                if !found.is_dir {
                    return None;
                }
                node = found.node();
            }
            current = Some(found);
        }
        Some(Resolved { item: current, real_rel })
    }

    /// Stream the contents of a file item (decmpfs-compressed files are decompressed).
    pub fn read_file(&mut self, item: &Item, out: &mut dyn Write) -> Result<u64, VolumeError> {
        self.vols[item.vol].read_file_to(&item.vol_path, out)
    }
}
