//! LOCAL PATCH: builder of small synthetic APFS containers for tests
//! (feature `testutil`).
//!
//! The images follow the layout of Apple File System Reference (2020-06-22):
//! block 0 = `nx_superblock_t`, then the container object map and, per volume,
//! `apfs_superblock_t`, volume object map, catalog root node and data blocks.
//! Every object carries a valid Fletcher-64 checksum. The catalog of each
//! volume is ONE leaf node (which is also the root), so a volume can hold only a
//! few dozen items; the builder panics with a clear message when it overflows.
//!
//! Not a substitute for real macOS images: the layout comes from the
//! specification, not from something macOS wrote.

use crate::fletcher::fletcher64;

const BS: usize = 4096;
const NODE_HDR: usize = 56; // obj_phys_t (32) + btree_node_phys_t (24)
const INFO: usize = 40; // btree_info_t

const ROOT: u16 = 0x1;
const LEAF: u16 = 0x2;
const FIXED: u16 = 0x4;

const T_INODE: u64 = 3;
const T_XATTR: u64 = 4;
const T_EXTENT: u64 = 8;
const T_DREC: u64 = 9;

const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;

const WAS_CLONED: u64 = 0x10;
const WAS_EVER_CLONED: u64 = 0x400;

/// `apfs_fs_flags` of an unencrypted volume.
pub const FS_UNENCRYPTED: u64 = 0x1;
/// `apfs_fs_flags` of a hardware (effaceable) encrypted volume.
pub const FS_EFFACEABLE: u64 = 0x2;

/// Fixed time stamp of every item: 2024-02-29 12:34:56 UTC in nanoseconds.
pub const TEST_TIME_NS: i64 = 1_709_210_096 * 1_000_000_000;

#[derive(Debug, Clone)]
enum Kind {
    Dir,
    File { data: Vec<u8>, clone_of: Option<usize> },
    Symlink { target: String },
}

#[derive(Debug, Clone)]
struct Item {
    parent: usize, // index into items, usize::MAX = root
    name: String,
    kind: Kind,
    ever_cloned: bool,
}

/// One volume of the container.
#[derive(Debug, Clone)]
pub struct VolumeSpec {
    name: String,
    fs_flags: u64,
    incompat: u64,
    num_snapshots: u64,
    items: Vec<Item>,
}

impl VolumeSpec {
    fn new(name: &str) -> Self {
        VolumeSpec { name: name.into(), fs_flags: FS_UNENCRYPTED, incompat: 0x1, num_snapshots: 0, items: Vec::new() }
    }

    fn find(&self, path: &str) -> usize {
        let mut cur = usize::MAX;
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            cur = self
                .items
                .iter()
                .position(|i| i.parent == cur && i.name == comp)
                .unwrap_or_else(|| panic!("testutil: path component {comp:?} of {path:?} does not exist"));
        }
        cur
    }

    fn add(&mut self, path: &str, kind: Kind) -> usize {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.find(dir);
        self.items.push(Item { parent, name: name.into(), kind, ever_cloned: false });
        self.items.len() - 1
    }

    pub fn add_dir(&mut self, path: &str) -> &mut Self {
        self.add(path, Kind::Dir);
        self
    }

    pub fn add_file(&mut self, path: &str, data: &[u8]) -> &mut Self {
        self.add(path, Kind::File { data: data.to_vec(), clone_of: None });
        self
    }

    pub fn add_symlink(&mut self, path: &str, target: &str) -> &mut Self {
        self.add(path, Kind::Symlink { target: target.into() });
        self
    }

    /// A `clonefile` copy: its own inode, but the same data stream (and extents)
    /// as `source`; both inodes get the clone flags.
    pub fn add_clone(&mut self, path: &str, source: &str) -> &mut Self {
        let src = self.find(source);
        assert!(matches!(self.items[src].kind, Kind::File { .. }), "clone source must be a file");
        self.items[src].ever_cloned = true;
        self.add(path, Kind::File { data: Vec::new(), clone_of: Some(src) });
        self
    }

    /// Clear `APFS_FS_UNENCRYPTED` (and set the effaceable bit like T2 volumes do).
    pub fn encrypted(&mut self) -> &mut Self {
        self.fs_flags = FS_EFFACEABLE;
        self
    }

    pub fn snapshots(&mut self, n: u64) -> &mut Self {
        self.num_snapshots = n;
        self
    }

    /// Set `APFS_INCOMPAT_SEALED_VOLUME`.
    pub fn sealed(&mut self) -> &mut Self {
        self.incompat |= 0x20;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct ApfsImageBuilder {
    volumes: Vec<VolumeSpec>,
}

impl ApfsImageBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a volume and return it for filling.
    pub fn volume(&mut self, name: &str) -> &mut VolumeSpec {
        self.volumes.push(VolumeSpec::new(name));
        self.volumes.last_mut().expect("just pushed")
    }

    pub fn build(&self) -> Vec<u8> {
        Img::default().build(&self.volumes)
    }
}

// ---------------------------------------------------------------------------

#[derive(Default)]
struct Img {
    data: Vec<u8>,
}

fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

/// Fill the 32-byte object header and the Fletcher-64 checksum of a block.
fn seal(b: &mut [u8], oid: u64, type_and_flags: u32, subtype: u32) {
    put64(b, 8, oid);
    put64(b, 16, 1); // xid
    put32(b, 24, type_and_flags);
    put32(b, 28, subtype);
    let c = fletcher64(&b[8..]);
    b[..8].copy_from_slice(&c.to_le_bytes());
}

/// A root leaf node of a B-tree.
struct Rec {
    key: Vec<u8>,
    val: Vec<u8>,
    order: (u64, u64, Vec<u8>),
}

fn btree_info(b: &mut [u8], fixed: Option<(u32, u32)>, count: u64, longest_key: u32, longest_val: u32) {
    let o = BS - INFO;
    let (ks, vs) = fixed.unwrap_or((0, 0));
    put32(b, o, 0x1); // BTREE_PHYSICAL is irrelevant to the reader
    put32(b, o + 4, BS as u32);
    put32(b, o + 8, ks);
    put32(b, o + 12, vs);
    put32(b, o + 16, longest_key);
    put32(b, o + 20, longest_val);
    put64(b, o + 24, count);
    put64(b, o + 32, 1);
}

/// Single root-leaf node. `fixed` = (key size, value size) for fixed-size trees.
fn leaf_node(recs: &[Rec], fixed: Option<(u32, u32)>, oid: u64, tf: u32, subtype: u32) -> Vec<u8> {
    let mut b = vec![0u8; BS];
    let n = recs.len();
    let toc_len = n * if fixed.is_some() { 4 } else { 8 };
    let key_area = NODE_HDR + toc_len;
    let val_end = BS - INFO;

    put16(&mut b, 32, ROOT | LEAF | if fixed.is_some() { FIXED } else { 0 });
    put16(&mut b, 34, 0); // level
    put32(&mut b, 36, n as u32);
    put16(&mut b, 40, 0); // table space offset
    put16(&mut b, 42, toc_len as u16);

    let (mut koff, mut vpos) = (0usize, val_end);
    for (i, r) in recs.iter().enumerate() {
        assert!(
            key_area + koff + r.key.len() <= vpos - r.val.len(),
            "testutil: too many items for a single-node catalog ({n} records)"
        );
        b[key_area + koff..key_area + koff + r.key.len()].copy_from_slice(&r.key);
        vpos -= r.val.len();
        b[vpos..vpos + r.val.len()].copy_from_slice(&r.val);
        let voff = val_end - vpos;
        if fixed.is_some() {
            put16(&mut b, NODE_HDR + i * 4, koff as u16);
            put16(&mut b, NODE_HDR + i * 4 + 2, voff as u16);
        } else {
            put16(&mut b, NODE_HDR + i * 8, koff as u16);
            put16(&mut b, NODE_HDR + i * 8 + 2, r.key.len() as u16);
            put16(&mut b, NODE_HDR + i * 8 + 4, voff as u16);
            put16(&mut b, NODE_HDR + i * 8 + 6, r.val.len() as u16);
        }
        koff += r.key.len();
    }
    let lk = recs.iter().map(|r| r.key.len()).max().unwrap_or(0) as u32;
    let lv = recs.iter().map(|r| r.val.len()).max().unwrap_or(0) as u32;
    btree_info(&mut b, fixed, n as u64, lk, lv);
    seal(&mut b, oid, tf, subtype);
    b
}

fn omap_rec(oid: u64, paddr: u64) -> Rec {
    let mut key = vec![0u8; 16];
    put64(&mut key, 0, oid);
    put64(&mut key, 8, 1); // xid
    let mut val = vec![0u8; 16];
    put32(&mut val, 4, BS as u32);
    put64(&mut val, 8, paddr);
    Rec { key, val, order: (oid, 0, Vec::new()) }
}

fn omap_phys(tree_block: u64, oid: u64) -> Vec<u8> {
    let mut b = vec![0u8; BS];
    put32(&mut b, 40, 0x4000_0002); // om_tree_type: physical-ish btree
    put64(&mut b, 48, tree_block);
    seal(&mut b, oid, 0x0B, 0);
    b
}

fn jkey(oid: u64, t: u64) -> Vec<u8> {
    ((t << 60) | oid).to_le_bytes().to_vec()
}

impl Img {
    fn alloc(&mut self, n: usize) -> u64 {
        let blk = (self.data.len() / BS) as u64;
        self.data.resize(self.data.len() + n * BS, 0);
        blk
    }

    fn write(&mut self, blk: u64, bytes: &[u8]) {
        let o = blk as usize * BS;
        self.data[o..o + bytes.len()].copy_from_slice(bytes);
    }

    fn build(mut self, volumes: &[VolumeSpec]) -> Vec<u8> {
        assert!(volumes.len() <= 100);
        let nx = self.alloc(1); // block 0
        let c_omap = self.alloc(1);
        let c_tree = self.alloc(1);

        let mut vol_oids = Vec::new();
        let mut c_recs = Vec::new();
        for (i, v) in volumes.iter().enumerate() {
            let oid = 1026 + i as u64;
            let apsb = self.volume_blocks(v, i, oid);
            c_recs.push(omap_rec(oid, apsb));
            vol_oids.push(oid);
        }
        c_recs.sort_by_key(|r| r.order.0);
        let tree = leaf_node(&c_recs, Some((16, 16)), c_tree, 0x02, 0x0B);
        self.write(c_tree, &tree);
        self.write(c_omap, &omap_phys(c_tree, c_omap));

        // nx_superblock_t
        let mut b = vec![0u8; BS];
        put32(&mut b, 32, 0x4253_584E); // "NXSB"
        put32(&mut b, 36, BS as u32);
        put64(&mut b, 40, (self.data.len() / BS) as u64);
        b[72] = 0xA5; // uuid
        put64(&mut b, 88, 5000); // next_oid
        put64(&mut b, 96, 2); // next_xid
        put64(&mut b, 160, c_omap); // omap_oid
        put32(&mut b, 180, 100); // max_file_systems
        for (i, oid) in vol_oids.iter().enumerate() {
            put64(&mut b, 184 + i * 8, *oid);
        }
        seal(&mut b, 1, 0x1, 0);
        self.write(nx, &b);
        self.data
    }

    /// Lay out one volume; returns the block of its `apfs_superblock_t`.
    fn volume_blocks(&mut self, v: &VolumeSpec, index: usize, _oid: u64) -> u64 {
        let apsb = self.alloc(1);
        let v_omap = self.alloc(1);
        let v_tree = self.alloc(1);
        let cat = self.alloc(1);

        // inode numbers: root dir = 2, items start at 16
        let ino = |i: usize| 16 + i as u64;
        let parent_ino = |p: usize| if p == usize::MAX { 2 } else { ino(p) };

        // data blocks for every file that owns its data stream
        let mut extents: Vec<Option<(u64, usize)>> = vec![None; v.items.len()];
        for (i, it) in v.items.iter().enumerate() {
            if let Kind::File { data, clone_of: None } = &it.kind
                && !data.is_empty()
            {
                let nblk = data.len().div_ceil(BS);
                let blk = self.alloc(nblk);
                self.write(blk, data);
                extents[i] = Some((blk, nblk * BS));
            }
        }
        let stream_of = |i: usize| -> usize {
            match &v.items[i].kind {
                Kind::File { clone_of: Some(s), .. } => *s,
                _ => i,
            }
        };

        let mut recs: Vec<Rec> = Vec::new();
        let inode = |id: u64, parent: u64, private: u64, mode: u16, size: u64, flags: u64, nlink: i32| {
            let mut val = vec![0u8; 92];
            put64(&mut val, 0, parent);
            put64(&mut val, 8, private);
            for o in [16, 24, 32, 40] {
                put64(&mut val, o, TEST_TIME_NS as u64);
            }
            put64(&mut val, 48, flags);
            put32(&mut val, 56, nlink as u32);
            put32(&mut val, 72, 501);
            put32(&mut val, 76, 20);
            put16(&mut val, 80, mode);
            // xfields: one INO_EXT_TYPE_DSTREAM (j_dstream_t, 40 bytes)
            val.extend_from_slice(&1u16.to_le_bytes()); // xf_num_exts
            val.extend_from_slice(&(4u16 + 40).to_le_bytes()); // xf_used_data
            val.extend_from_slice(&[8, 0x02, 40, 0]); // x_type, x_flags, x_size
            let mut ds = [0u8; 40];
            ds[..8].copy_from_slice(&size.to_le_bytes());
            ds[8..16].copy_from_slice(&size.to_le_bytes());
            val.extend_from_slice(&ds);
            Rec { key: jkey(id, T_INODE), val, order: (id, T_INODE, Vec::new()) }
        };

        let root_children = v.items.iter().filter(|i| i.parent == usize::MAX).count() as i32;
        recs.push(inode(2, 1, 2, S_IFDIR | 0o755, 0, 0, root_children));

        let mut num_files = 0u64;
        let mut num_dirs = 1u64;
        let mut num_links = 0u64;
        for (i, it) in v.items.iter().enumerate() {
            let id = ino(i);
            let dt: u16;
            match &it.kind {
                Kind::Dir => {
                    num_dirs += 1;
                    dt = 4;
                    let n = v.items.iter().filter(|c| c.parent == i).count() as i32;
                    recs.push(inode(id, parent_ino(it.parent), id, S_IFDIR | 0o755, 0, 0, n));
                }
                Kind::File { data, clone_of } => {
                    num_files += 1;
                    dt = 8;
                    let s = stream_of(i);
                    let size = match clone_of {
                        Some(src) => match &v.items[*src].kind {
                            Kind::File { data, .. } => data.len() as u64,
                            _ => 0,
                        },
                        None => data.len() as u64,
                    };
                    let mut flags = 0;
                    if clone_of.is_some() {
                        flags |= WAS_CLONED | WAS_EVER_CLONED;
                    }
                    if it.ever_cloned {
                        flags |= WAS_EVER_CLONED;
                    }
                    // clones share the private_id (data stream id) of their source
                    recs.push(inode(id, parent_ino(it.parent), ino(s), S_IFREG | 0o644, size, flags, 1));
                    if let Some((blk, len)) = extents[s] {
                        let mut key = jkey(ino(s), T_EXTENT);
                        key.extend_from_slice(&0u64.to_le_bytes()); // logical address
                        let mut val = vec![0u8; 24];
                        put64(&mut val, 0, len as u64);
                        put64(&mut val, 8, blk);
                        // one extent record per data stream, even if two clones point at it
                        if !recs.iter().any(|r| r.key == key) {
                            recs.push(Rec { key, val, order: (ino(s), T_EXTENT, 0u64.to_le_bytes().to_vec()) });
                        }
                    }
                }
                Kind::Symlink { target } => {
                    num_links += 1;
                    dt = 10;
                    recs.push(inode(id, parent_ino(it.parent), id, S_IFLNK | 0o755, 0, 0, 1));
                    let name = b"com.apple.fs.symlink\0";
                    let mut key = jkey(id, T_XATTR);
                    key.extend_from_slice(&(name.len() as u16).to_le_bytes());
                    key.extend_from_slice(name);
                    let mut payload = target.as_bytes().to_vec();
                    payload.push(0);
                    let mut val = Vec::new();
                    val.extend_from_slice(&2u16.to_le_bytes()); // XATTR_EMBEDDED
                    val.extend_from_slice(&(payload.len() as u16).to_le_bytes());
                    val.extend_from_slice(&payload);
                    recs.push(Rec { key, val, order: (id, T_XATTR, name.to_vec()) });
                }
            }
            // directory record (hashed key layout: name_len_and_hash + NUL-terminated name)
            let mut key = jkey(parent_ino(it.parent), T_DREC);
            let mut name = it.name.as_bytes().to_vec();
            name.push(0);
            key.extend_from_slice(&(name.len() as u32).to_le_bytes());
            key.extend_from_slice(&name);
            let mut val = vec![0u8; 18];
            put64(&mut val, 0, id);
            put64(&mut val, 8, TEST_TIME_NS as u64);
            put16(&mut val, 16, dt);
            recs.push(Rec { key, val, order: (parent_ino(it.parent), T_DREC, name) });
        }
        recs.sort_by(|a, b| a.order.cmp(&b.order));

        let cat_node = leaf_node(&recs, None, 1024, 0x8000_0002, 0x0E);
        self.write(cat, &cat_node);

        // volume object map: virtual oid 1024 (catalog root) -> cat
        let tree = leaf_node(&[omap_rec(1024, cat)], Some((16, 16)), v_tree, 0x02, 0x0B);
        self.write(v_tree, &tree);
        self.write(v_omap, &omap_phys(v_tree, v_omap));

        // apfs_superblock_t
        let mut b = vec![0u8; BS];
        put32(&mut b, 32, 0x4253_5041); // "APSB"
        put32(&mut b, 36, index as u32);
        put64(&mut b, 56, v.incompat);
        put64(&mut b, 128, v_omap); // apfs_omap_oid
        put64(&mut b, 136, 1024); // apfs_root_tree_oid
        put64(&mut b, 176, 1000); // next_obj_id
        put64(&mut b, 184, num_files);
        put64(&mut b, 192, num_dirs);
        put64(&mut b, 200, num_links);
        put64(&mut b, 216, v.num_snapshots);
        b[240] = 0xB0 + index as u8; // uuid
        put64(&mut b, 264, v.fs_flags);
        let name = v.name.as_bytes();
        b[704..704 + name.len()].copy_from_slice(name);
        seal(&mut b, 1026 + index as u64, 0x8000_000D, 0);
        self.write(apsb, &b);
        apsb
    }
}
