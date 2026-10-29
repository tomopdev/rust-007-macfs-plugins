//! Read-only parser for classic HFS disk images (lokálně upravená kopie crate `hfs-reader` 0.1.1, viz `PATCHES.md`).
//!
//! Accepts the HFS MDB signature (`0x4244`). MFS (`0xD2D7`) and HFS+ (`0x482B`) are rejected
//! with explicit errors. The image is read through any `Read + Seek` (a partition slice, a file,
//! a `Cursor` over a byte slice): only the catalog and the extents overflow B-tree are loaded
//! into memory, the forks of files are read on demand and can be streamed.
//!
//! # Usage
//!
//! ```no_run
//! use hfs_reader::HfsVolume;
//!
//! let image_bytes = std::fs::read("my_disk.dsk").unwrap();
//! let mut vol = HfsVolume::parse(&image_bytes).unwrap();
//!
//! let files = std::mem::take(&mut vol.files);
//! for file in &files {
//!     let data = vol.read_data_fork(file).unwrap();
//!     let rsrc = vol.read_rsrc_fork(file).unwrap();
//!     println!("{}: data={} rsrc={}", file.rel_path.display(), data.len(), rsrc.len());
//! }
//! ```

use std::{
    collections::{HashMap, HashSet},
    fmt,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[cfg(feature = "testutil")]
pub mod testutil;

// Each extent descriptor is (first_alloc_block, block_count).
type Extents = [(u16, u16); 3];

/// Node size of the classic HFS B-trees (fixed by the format).
const NODE_SIZE: usize = 512;
/// Upper bound of a B-tree file that is loaded into memory (a catalog of a 2 GB volume is a few tens of MB).
const MAX_TREE_BYTES: u32 = 256 << 20;

/// Error of the reader: an I/O failure of the underlying image or damaged / unsupported structures.
#[derive(Debug)]
pub enum HfsError {
    Io(io::Error),
    Format(String),
}

impl fmt::Display for HfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HfsError::Io(e) => write!(f, "I/O error: {e}"),
            HfsError::Format(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for HfsError {}

impl From<io::Error> for HfsError {
    fn from(e: io::Error) -> Self {
        // A short image is damaged data, not a failing device.
        if e.kind() == io::ErrorKind::UnexpectedEof {
            HfsError::Format("the image is truncated".to_string())
        } else {
            HfsError::Io(e)
        }
    }
}

impl From<String> for HfsError {
    fn from(m: String) -> Self {
        HfsError::Format(m)
    }
}

impl From<&str> for HfsError {
    fn from(m: &str) -> Self {
        HfsError::Format(m.to_string())
    }
}

// MacRoman 0x80..=0xFF -> Unicode.
const MAC_ROMAN_HIGH: [char; 128] = [
    '\u{00c4}', '\u{00c5}', '\u{00c7}', '\u{00c9}', '\u{00d1}', '\u{00d6}', '\u{00dc}', '\u{00e1}', '\u{00e0}', '\u{00e2}', '\u{00e4}', '\u{00e3}',
    '\u{00e5}', '\u{00e7}', '\u{00e9}', '\u{00e8}', '\u{00ea}', '\u{00eb}', '\u{00ed}', '\u{00ec}', '\u{00ee}', '\u{00ef}', '\u{00f1}', '\u{00f3}',
    '\u{00f2}', '\u{00f4}', '\u{00f6}', '\u{00f5}', '\u{00fa}', '\u{00f9}', '\u{00fb}', '\u{00fc}', '\u{2020}', '\u{00b0}', '\u{00a2}', '\u{00a3}',
    '\u{00a7}', '\u{2022}', '\u{00b6}', '\u{00df}', '\u{00ae}', '\u{00a9}', '\u{2122}', '\u{00b4}', '\u{00a8}', '\u{2260}', '\u{00c6}', '\u{00d8}',
    '\u{221e}', '\u{00b1}', '\u{2264}', '\u{2265}', '\u{00a5}', '\u{00b5}', '\u{2202}', '\u{2211}', '\u{220f}', '\u{03c0}', '\u{222b}', '\u{00aa}',
    '\u{00ba}', '\u{03a9}', '\u{00e6}', '\u{00f8}', '\u{00bf}', '\u{00a1}', '\u{00ac}', '\u{221a}', '\u{0192}', '\u{2248}', '\u{2206}', '\u{00ab}',
    '\u{00bb}', '\u{2026}', '\u{00a0}', '\u{00c0}', '\u{00c3}', '\u{00d5}', '\u{0152}', '\u{0153}', '\u{2013}', '\u{2014}', '\u{201c}', '\u{201d}',
    '\u{2018}', '\u{2019}', '\u{00f7}', '\u{25ca}', '\u{00ff}', '\u{0178}', '\u{2044}', '\u{20ac}', '\u{2039}', '\u{203a}', '\u{fb01}', '\u{fb02}',
    '\u{2021}', '\u{00b7}', '\u{201a}', '\u{201e}', '\u{2030}', '\u{00c2}', '\u{00ca}', '\u{00c1}', '\u{00cb}', '\u{00c8}', '\u{00cd}', '\u{00ce}',
    '\u{00cf}', '\u{00cc}', '\u{00d3}', '\u{00d4}', '\u{f8ff}', '\u{00d2}', '\u{00da}', '\u{00db}', '\u{00d9}', '\u{0131}', '\u{02c6}', '\u{02dc}',
    '\u{00af}', '\u{02d8}', '\u{02d9}', '\u{02da}', '\u{00b8}', '\u{02dd}', '\u{02db}', '\u{02c7}',
];

/// Decode a MacRoman name from the catalog. Control characters become `_`; `/` is kept
/// (it is a legal character of a classic HFS name, only `:` is the path separator there).
pub fn decode_mac_roman(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x00..=0x1f | 0x7f => '_',
            0x20..=0x7e => b as char,
            _ => MAC_ROMAN_HIGH[(b - 0x80) as usize],
        })
        .collect()
}

// Accumulates raw catalog data for a file before path-building.  Directory
// names for parent CNIDs may appear after their children in tree order, so
// everything is collected first and paths are resolved in a second pass.
struct RawFile {
    cnid: u32,
    parent_cnid: u32,
    name: String,
    flags: u8,
    file_type: [u8; 4],
    creator: [u8; 4],
    data_logical: u32,
    data_ext: Extents,
    rsrc_logical: u32,
    rsrc_ext: Extents,
    create_date: u32,
    modify_date: u32,
}

struct RawDir {
    cnid: u32,
    parent_cnid: u32,
    name: String,
    create_date: u32,
    modify_date: u32,
}

/// A file entry decoded from the HFS catalog B-tree.
pub struct HfsFileEntry {
    /// Relative path from the volume root (names MacRoman-decoded, `/` in a name replaced by `:`).
    pub rel_path: PathBuf,
    /// CNID of the file.
    pub cnid: u32,
    /// CNID of the parent directory (2 = volume root).
    pub parent_cnid: u32,
    /// Name of the file (MacRoman decoded, `/` kept as stored).
    pub name: String,
    /// Mac OS four-character file type (e.g. `b"APPL"`).
    pub file_type: [u8; 4],
    /// Mac OS four-character creator code (e.g. `b"MACS"`).
    pub creator: [u8; 4],
    /// `filFlags` (bit 0 = locked).
    pub flags: u8,
    /// Creation date, seconds since 1904-01-01 in the local time of the Mac that made it (0 = not set).
    pub create_date: u32,
    /// Modification date, same encoding as `create_date`.
    pub modify_date: u32,

    pub(crate) data_logical: u32,
    pub(crate) data_extents: Vec<Extents>,
    pub(crate) rsrc_logical: u32,
    pub(crate) rsrc_extents: Vec<Extents>,
}

impl HfsFileEntry {
    /// Logical length of the data fork.
    pub fn data_size(&self) -> u32 {
        self.data_logical
    }

    /// Logical length of the resource fork.
    pub fn rsrc_size(&self) -> u32 {
        self.rsrc_logical
    }
}

/// A directory entry decoded from the HFS catalog B-tree.
pub struct HfsDirEntry {
    /// Relative path from the volume root.
    pub rel_path: PathBuf,
    pub cnid: u32,
    pub parent_cnid: u32,
    pub name: String,
    pub create_date: u32,
    pub modify_date: u32,
}

/// A parsed HFS volume.
pub struct HfsVolume<R> {
    img: R,
    block_size: u32,
    first_alloc_sector: u32,
    /// Volume name from the MDB (drVN), MacRoman decoded.
    pub volume_name: String,
    /// All files found in the catalog, in catalog order.
    pub files: Vec<HfsFileEntry>,
    /// All directories found in the catalog (excluding the volume root), empty ones included.
    pub dirs: Vec<HfsDirEntry>,
}

impl<'a> HfsVolume<Cursor<&'a [u8]>> {
    /// Parse an HFS disk image held in memory.  Both raw sector dumps and Disk Copy 4.2
    /// containers are accepted.
    #[allow(clippy::missing_errors_doc)]
    pub fn parse(img: &'a [u8]) -> Result<Self, HfsError> {
        // DC42 images carry a magic value at bytes 82–83.  Strip the header so
        // the rest of the parser sees a plain sector dump.
        let img = if img.get(82..84) == Some(&[0x01, 0x00]) {
            const DC42_HEADER: usize = 84;
            let data_size = img
                .get(64..68)
                .ok_or("DC42 image too small to read data_size field")
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)?;
            img.get(DC42_HEADER..DC42_HEADER.saturating_add(data_size))
                .ok_or("DC42 image truncated: sector data extends past end of file")?
        } else {
            img
        };
        Self::open(Cursor::new(img))
    }
}

impl<R: Read + Seek> HfsVolume<R> {
    /// Open a raw HFS volume (the MDB is at byte 1024 of `img`).
    #[allow(clippy::missing_errors_doc)]
    pub fn open(mut img: R) -> Result<Self, HfsError> {
        let mut mdb = [0u8; 162];
        img.seek(SeekFrom::Start(1024))?;
        img.read_exact(&mut mdb)?;

        let sig = u16::from_be_bytes([mdb[0], mdb[1]]);
        if sig != 0x4244 {
            if sig == 0x482B {
                return Err("Image is HFS+, not HFS — only classic HFS is supported".into());
            }
            if sig == 0xD2D7 {
                return Err("Image is MFS, not HFS — only classic HFS is supported".into());
            }
            return Err(format!("Not an HFS volume (MDB signature {sig:#06x})").into());
        }

        let block_size = u32::from_be_bytes([mdb[20], mdb[21], mdb[22], mdb[23]]);
        if block_size == 0 || !block_size.is_multiple_of(512) {
            return Err(format!("Invalid HFS allocation block size {block_size}").into());
        }
        let first_alloc_sector = u32::from(u16::from_be_bytes([mdb[28], mdb[29]]));

        let vn_len = (mdb[36] as usize).min(27);
        let volume_name = decode_mac_roman(&mdb[37..37 + vn_len]);

        // MDB offsets per Inside Macintosh: Files:
        //   130  drXTFlSize  extents overflow B-tree logical size
        //   134  drXTExtRec  extents overflow B-tree extent record
        //   146  drCTFlSize  catalog B-tree logical size
        //   150  drCTExtRec  catalog B-tree extent record
        let ext_logical = u32::from_be_bytes([mdb[130], mdb[131], mdb[132], mdb[133]]);
        let ext_extents = read_extent_record(&mdb, 134);
        let cat_logical = u32::from_be_bytes([mdb[146], mdb[147], mdb[148], mdb[149]]);
        let cat_extents = read_extent_record(&mdb, 150);

        let mut vol = HfsVolume {
            img,
            block_size,
            first_alloc_sector,
            volume_name,
            files: Vec::new(),
            dirs: Vec::new(),
        };

        if ext_logical > MAX_TREE_BYTES || cat_logical > MAX_TREE_BYTES {
            return Err("The catalog or extents B-tree is implausibly large".into());
        }
        let ext_data = vol.read_raw(&[ext_extents], ext_logical)?;
        let overflow = ExtentsOverflow::parse(&ext_data);

        let cat_data = vol.read_raw(&[cat_extents], cat_logical)?;
        vol.parse_catalog(&cat_data, &overflow)?;

        Ok(vol)
    }

    /// Read the data fork of a file entry into memory.
    #[allow(clippy::missing_errors_doc)]
    pub fn read_data_fork(&mut self, entry: &HfsFileEntry) -> Result<Vec<u8>, HfsError> {
        let mut out = Vec::new();
        self.copy_fork(&entry.data_extents, entry.data_logical, &mut out)?;
        Ok(out)
    }

    /// Read the resource fork of a file entry into memory.  Returns an empty `Vec` when
    /// the resource fork is absent or zero-length.
    #[allow(clippy::missing_errors_doc)]
    pub fn read_rsrc_fork(&mut self, entry: &HfsFileEntry) -> Result<Vec<u8>, HfsError> {
        let mut out = Vec::new();
        self.copy_fork(&entry.rsrc_extents, entry.rsrc_logical, &mut out)?;
        Ok(out)
    }

    /// Stream the data fork of a file entry; returns the number of bytes written.
    #[allow(clippy::missing_errors_doc)]
    pub fn copy_data_fork(&mut self, entry: &HfsFileEntry, out: &mut dyn Write) -> Result<u64, HfsError> {
        self.copy_fork(&entry.data_extents, entry.data_logical, out)
    }

    /// Stream the resource fork of a file entry; returns the number of bytes written.
    #[allow(clippy::missing_errors_doc)]
    pub fn copy_rsrc_fork(&mut self, entry: &HfsFileEntry, out: &mut dyn Write) -> Result<u64, HfsError> {
        self.copy_fork(&entry.rsrc_extents, entry.rsrc_logical, out)
    }

    fn byte_offset_of_block(&self, block: u32) -> u64 {
        u64::from(self.first_alloc_sector) * 512 + u64::from(block) * u64::from(self.block_size)
    }

    /// Copy `logical_size` bytes described by `extents` to `out` in pieces of at most 1 MiB.
    /// The fork is damaged (an error) when its extents cover fewer bytes than its logical size.
    fn copy_fork(&mut self, extents: &[Extents], logical_size: u32, out: &mut dyn Write) -> Result<u64, HfsError> {
        const PIECE: usize = 1 << 20;
        let total = u64::from(logical_size);
        let mut left = total;
        let mut buf = Vec::new();
        'outer: for ext_set in extents {
            for &(start_block, block_count) in ext_set {
                if left == 0 || block_count == 0 {
                    break 'outer;
                }
                let mut off = self.byte_offset_of_block(u32::from(start_block));
                let mut run = (u64::from(block_count) * u64::from(self.block_size)).min(left);
                self.img.seek(SeekFrom::Start(off))?;
                while run > 0 {
                    let n = run.min(PIECE as u64) as usize;
                    buf.resize(n, 0);
                    self.img.read_exact(&mut buf).map_err(|e| match e.kind() {
                        io::ErrorKind::UnexpectedEof => HfsError::Format(format!(
                            "Extent [{start_block}+{block_count}] is out of bounds (the image ends before byte {})",
                            off + n as u64
                        )),
                        _ => HfsError::Io(e),
                    })?;
                    out.write_all(&buf)?;
                    off += n as u64;
                    run -= n as u64;
                    left -= n as u64;
                }
            }
        }
        if left != 0 {
            return Err(format!("The extents of the fork cover {} of its {total} bytes", total - left).into());
        }
        Ok(total)
    }

    fn read_raw(&mut self, extents: &[Extents], logical_size: u32) -> Result<Vec<u8>, HfsError> {
        let mut out = Vec::with_capacity(logical_size as usize);
        self.copy_fork(extents, logical_size, &mut out)?;
        Ok(out)
    }

    #[allow(clippy::too_many_lines)]
    fn parse_catalog(&mut self, cat_data: &[u8], overflow: &ExtentsOverflow) -> Result<(), HfsError> {
        if cat_data.len() < NODE_SIZE {
            return Err("Catalog B-tree is too small to be valid".into());
        }

        // Node 0 is the header node; the root node index lives at header+2.
        let root_node = u32::from_be_bytes([cat_data[16], cat_data[17], cat_data[18], cat_data[19]]);

        let mut raw_files: Vec<RawFile> = Vec::new();
        let mut raw_dirs: Vec<RawDir> = Vec::new();

        let mut stack: Vec<u32> = vec![root_node];
        let mut visited: HashSet<u32> = HashSet::new();

        while let Some(node_idx) = stack.pop() {
            if node_idx == 0 || !visited.insert(node_idx) {
                continue;
            }
            let node_off = node_idx as usize * NODE_SIZE;
            let Some(node) = cat_data.get(node_off..node_off + NODE_SIZE) else {
                continue;
            };

            let node_kind = node[8];
            let num_records = (u16::from_be_bytes([node[10], node[11]]) as usize).min((NODE_SIZE - 14) / 2);

            // Record offsets are packed at the node tail, last record first
            // (record 0's offset is at byte 510).
            for i in 0..num_records {
                let ptr_off = NODE_SIZE - 2 * (i + 1);
                let rec_off = u16::from_be_bytes([node[ptr_off], node[ptr_off + 1]]) as usize;
                let Some(rec) = node.get(rec_off..) else {
                    continue;
                };
                if rec.is_empty() {
                    continue;
                }

                let key_len = rec[0] as usize;
                if key_len < 6 || rec.len() < 1 + key_len {
                    continue;
                }
                let parent_cnid = u32::from_be_bytes([rec[2], rec[3], rec[4], rec[5]]);
                let name_len = rec[6] as usize;
                // The name must lie inside the key.
                let name_bytes = rec.get(7..7 + name_len.min(key_len - 6)).unwrap_or(&[]);
                let name = decode_mac_roman(name_bytes);

                // HFS catalog keys have an odd key_len (37), so no padding byte is
                // needed.  We apply the general rule anyway for correctness.
                let data_off = 1 + key_len + usize::from(key_len.is_multiple_of(2));

                match node_kind {
                    0xFF => {
                        let Some(rec_data) = rec.get(data_off..) else {
                            continue;
                        };
                        if rec_data.len() < 2 {
                            continue;
                        }
                        // Record type is stored little-endian.
                        let rec_type = i16::from_le_bytes([rec_data[0], rec_data[1]]);
                        match rec_type {
                            1 => {
                                // Directory record; CNID at offset 6, dates 10 (created) and 14 (modified).
                                if rec_data.len() < 18 {
                                    continue;
                                }
                                raw_dirs.push(RawDir {
                                    cnid: be32(rec_data, 6),
                                    parent_cnid,
                                    name,
                                    create_date: be32(rec_data, 10),
                                    modify_date: be32(rec_data, 14),
                                });
                            }

                            2 => {
                                // File record.  Field offsets per Inside Macintosh: Files:
                                //   2   filFlags               4   FInfo (type+creator)
                                //  20   CNID                  26   data logical len
                                //  36   rsrc logical len      44   created / 48 modified
                                //  74   data extents          86   rsrc extents
                                if rec_data.len() < 98 {
                                    continue;
                                }
                                raw_files.push(RawFile {
                                    cnid: be32(rec_data, 20),
                                    parent_cnid,
                                    name,
                                    flags: rec_data[2],
                                    file_type: [rec_data[4], rec_data[5], rec_data[6], rec_data[7]],
                                    creator: [rec_data[8], rec_data[9], rec_data[10], rec_data[11]],
                                    data_logical: be32(rec_data, 26),
                                    data_ext: read_extent_record(rec_data, 74),
                                    rsrc_logical: be32(rec_data, 36),
                                    rsrc_ext: read_extent_record(rec_data, 86),
                                    create_date: be32(rec_data, 44),
                                    modify_date: be32(rec_data, 48),
                                });
                            }

                            _ => {} // thread records
                        }
                    }

                    0x00 => {
                        // Index node — payload is a 4-byte child node index.
                        let Some(ptr_data) = rec.get(data_off..data_off + 4) else {
                            continue;
                        };
                        stack.push(be32(ptr_data, 0));
                    }

                    _ => {}
                }
            }

            // Follow fLink to catch sibling nodes not reachable via an index record.
            if node_kind == 0x00 || node_kind == 0xFF {
                let flink = be32(node, 0);
                if flink != 0 {
                    stack.push(flink);
                }
            }
        }

        // CNID -> (parent, name) of every directory, then paths by walking up (depth-limited: a loop of damaged parents ends).
        let mut dir_info: HashMap<u32, (u32, String)> = HashMap::new();
        for d in &raw_dirs {
            dir_info.insert(d.cnid, (d.parent_cnid, d.name.clone()));
        }
        let mut path_cache: HashMap<u32, Option<PathBuf>> = HashMap::new();
        path_cache.insert(2, Some(PathBuf::new())); // the volume root
        let mut dir_path = |cnid: u32| -> Option<PathBuf> {
            if let Some(p) = path_cache.get(&cnid) {
                return p.clone();
            }
            let mut parts: Vec<String> = Vec::new();
            let mut cur = cnid;
            let mut ok = false;
            for _ in 0..64 {
                if cur == 2 {
                    ok = true;
                    break;
                }
                let Some((parent, name)) = dir_info.get(&cur) else { break };
                parts.push(name.replace('/', ":"));
                cur = *parent;
            }
            parts.reverse();
            let p = ok.then(|| parts.iter().collect::<PathBuf>());
            path_cache.insert(cnid, p.clone());
            p
        };

        for d in raw_dirs {
            // Directories that do not lead to the root (orphans of a damaged catalog) are dropped.
            let Some(parent) = dir_path(d.parent_cnid) else { continue };
            self.dirs.push(HfsDirEntry {
                rel_path: parent.join(d.name.replace('/', ":")),
                cnid: d.cnid,
                parent_cnid: d.parent_cnid,
                name: d.name,
                create_date: d.create_date,
                modify_date: d.modify_date,
            });
        }

        for rf in raw_files {
            let Some(parent) = dir_path(rf.parent_cnid) else { continue };
            let data_extents = overflow.get_extents(rf.cnid, 0x00, &rf.data_ext, rf.data_logical, self.block_size);
            let rsrc_extents = overflow.get_extents(rf.cnid, 0xFF, &rf.rsrc_ext, rf.rsrc_logical, self.block_size);
            self.files.push(HfsFileEntry {
                rel_path: parent.join(rf.name.replace('/', ":")),
                cnid: rf.cnid,
                parent_cnid: rf.parent_cnid,
                name: rf.name,
                file_type: rf.file_type,
                creator: rf.creator,
                flags: rf.flags,
                create_date: rf.create_date,
                modify_date: rf.modify_date,
                data_logical: rf.data_logical,
                data_extents,
                rsrc_logical: rf.rsrc_logical,
                rsrc_extents,
            });
        }

        Ok(())
    }
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

struct ExtentsOverflow {
    entries: HashMap<(u32, u8, u32), Extents>,
}

impl ExtentsOverflow {
    fn parse(data: &[u8]) -> Self {
        let mut entries = HashMap::new();
        if data.len() < NODE_SIZE {
            return Self { entries };
        }

        // First leaf node index is at header record offset 10 (bytes 24..28 of node 0).
        let first_leaf = be32(data, 24);

        let mut node_idx = first_leaf;
        let mut visited: HashSet<u32> = HashSet::new();

        while node_idx != 0 && visited.insert(node_idx) {
            let off = node_idx as usize * NODE_SIZE;
            let Some(node) = data.get(off..off + NODE_SIZE) else {
                break;
            };
            let flink = be32(node, 0);
            let num_records = (u16::from_be_bytes([node[10], node[11]]) as usize).min((NODE_SIZE - 14) / 2);

            for i in 0..num_records {
                let ptr_off = NODE_SIZE - 2 * (i + 1);
                let rec_off = u16::from_be_bytes([node[ptr_off], node[ptr_off + 1]]) as usize;
                let Some(rec) = node.get(rec_off..) else {
                    continue;
                };
                if rec.is_empty() {
                    continue;
                }

                let key_len = rec[0] as usize;
                if key_len < 7 || rec.len() < 1 + key_len + 12 {
                    continue;
                }
                let fork_type = rec[1];
                let cnid = be32(rec, 2);
                let start_block = u32::from(u16::from_be_bytes([rec[6], rec[7]]));

                let data_off = 1 + key_len + usize::from(key_len.is_multiple_of(2));
                let Some(ext_data) = rec.get(data_off..data_off + 12) else {
                    continue;
                };
                entries.insert((cnid, fork_type, start_block), read_extent_record(ext_data, 0));
            }

            node_idx = flink;
        }

        Self { entries }
    }

    fn get_extents(&self, cnid: u32, fork_type: u8, catalog_ext: &Extents, logical_size: u32, block_size: u32) -> Vec<Extents> {
        let mut result = vec![*catalog_ext];
        if block_size == 0 {
            return result;
        }

        let catalog_blocks: u32 = catalog_ext.iter().map(|e| u32::from(e.1)).sum();
        let blocks_needed = logical_size.div_ceil(block_size);
        if catalog_blocks >= blocks_needed {
            return result;
        }

        // Each overflow record is keyed by the block where the previous set ended.
        let mut covered = catalog_blocks;
        while let Some(&ext) = self.entries.get(&(cnid, fork_type, covered)) {
            result.push(ext);
            let added: u32 = ext.iter().map(|e| u32::from(e.1)).sum();
            if added == 0 {
                break;
            }
            covered += added;
            if covered >= blocks_needed {
                break;
            }
        }
        result
    }
}

/// Read three HFS extent descriptors from `data` at `offset`.
/// Each descriptor is two big-endian u16 values: (first_alloc_block, block_count).
fn read_extent_record(data: &[u8], offset: usize) -> Extents {
    let mut e = [(0u16, 0u16); 3];
    for (i, slot) in e.iter_mut().enumerate() {
        let o = offset + i * 4;
        if o + 4 <= data.len() {
            *slot = (u16::from_be_bytes([data[o], data[o + 1]]), u16::from_be_bytes([data[o + 2], data[o + 3]]));
        }
    }
    e
}

/// Sanitise a path so it can be safely joined onto a host filesystem path.
/// Strips `.`, `..`, and empty components.
#[must_use]
pub fn sanitise_path(raw: &Path) -> PathBuf {
    use std::path::Component::Normal;
    raw.components().filter(|c| matches!(c, Normal(_))).collect()
}
