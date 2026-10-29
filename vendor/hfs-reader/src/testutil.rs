//! Builder of synthetic classic HFS volumes for tests (feature `testutil`, local addition to the crate).
//!
//! The volume follows *Inside Macintosh: Files*: boot blocks, MDB at byte 1024, volume bitmap, allocation blocks
//! (file data first, then the extents overflow file and the catalog file), 512-byte B-tree nodes, catalog leaves
//! chained by `fLink` plus one index node when there is more than one leaf, directory and thread records, and the
//! overflow extent records of fragmented forks. It is written from the specification, not captured from a real
//! Macintosh volume, so a real disk image still deserves a manual try.

/// CNID of the volume root.
pub const ROOT: u32 = 2;
/// Modification date of everything (2023-11-14 22:13:20, seconds since 1904).
pub const MODIFIED: u32 = 3_782_844_800;
/// Creation date of everything (one day earlier).
pub const CREATED: u32 = MODIFIED - 86_400;

const NODE: usize = 512;
const AL_BL_ST: usize = 4; // sectors: 2 boot, MDB, volume bitmap

enum Kind {
    Dir,
    File { data: Vec<u8>, rsrc: Vec<u8>, ftype: [u8; 4], creator: [u8; 4], fragmented: bool, locked: bool },
}

struct Entry {
    parent: u32,
    cnid: u32,
    name: Vec<u8>,
    kind: Kind,
}

pub struct HfsImageBuilder {
    volume: Vec<u8>,
    block_size: usize,
    next_cnid: u32,
    entries: Vec<Entry>,
}

type Runs = Vec<(u16, u16)>;

impl HfsImageBuilder {
    /// `volume` is the MacRoman encoded volume name (at most 27 bytes).
    pub fn new(volume: &[u8]) -> Self {
        assert!(volume.len() <= 27);
        HfsImageBuilder { volume: volume.to_vec(), block_size: 1024, next_cnid: 16, entries: Vec::new() }
    }

    /// Allocation block size, a multiple of 512 (default 1024).
    pub fn block_size(&mut self, bytes: usize) -> &mut Self {
        assert!(bytes >= 512 && bytes.is_multiple_of(512));
        self.block_size = bytes;
        self
    }

    /// Add a folder below `parent` ([`ROOT`] or the CNID returned by an earlier call); returns its CNID.
    pub fn add_dir(&mut self, parent: u32, name: &[u8]) -> u32 {
        self.push(parent, name, Kind::Dir)
    }

    /// Add a file with a data fork and a resource fork (either may be empty); returns its CNID.
    pub fn add_file(&mut self, parent: u32, name: &[u8], data: &[u8], rsrc: &[u8]) -> u32 {
        self.push(
            parent,
            name,
            Kind::File { data: data.to_vec(), rsrc: rsrc.to_vec(), ftype: *b"TEXT", creator: *b"ttxt", fragmented: false, locked: false },
        )
    }

    /// Store the forks of the file one allocation block at a time with gaps between them, so that
    /// more than three extents are needed and the rest lands in the extents overflow file.
    pub fn fragment(&mut self, cnid: u32) -> &mut Self {
        if let Some(Kind::File { fragmented, .. }) = self.entries.iter_mut().find(|e| e.cnid == cnid).map(|e| &mut e.kind) {
            *fragmented = true;
        }
        self
    }

    fn push(&mut self, parent: u32, name: &[u8], kind: Kind) -> u32 {
        assert!(!name.is_empty() && name.len() <= 31);
        let cnid = self.next_cnid;
        self.next_cnid += 1;
        self.entries.push(Entry { parent, cnid, name: name.to_vec(), kind });
        cnid
    }

    /// The raw image (no container): the MDB is at byte 1024.
    pub fn build(&self) -> Vec<u8> {
        let bs = self.block_size;

        // ---- allocation: file forks first, then the two B-tree files
        let mut next_block = 0usize;
        let mut alloc = |blocks: usize, fragmented: bool| -> Runs {
            let mut runs: Runs = Vec::new();
            for i in 0..blocks {
                let b = next_block;
                next_block += if fragmented { 2 } else { 1 }; // a gap after every block
                match runs.last_mut() {
                    Some((s, c)) if !fragmented && *s as usize + *c as usize == b => *c += 1,
                    _ => runs.push((b as u16, 1)),
                }
                let _ = i;
            }
            runs
        };

        struct Placed<'a> {
            e: &'a Entry,
            data: Runs,
            rsrc: Runs,
        }
        let mut placed: Vec<Placed> = Vec::new();
        let mut blobs: Vec<(Runs, &[u8])> = Vec::new();
        for e in &self.entries {
            let (data, rsrc) = match &e.kind {
                Kind::Dir => (Vec::new(), Vec::new()),
                Kind::File { data, rsrc, fragmented, .. } => {
                    let d = alloc(data.len().div_ceil(bs), *fragmented);
                    let r = alloc(rsrc.len().div_ceil(bs), *fragmented);
                    blobs.push((d.clone(), data));
                    blobs.push((r.clone(), rsrc));
                    (d, r)
                }
            };
            placed.push(Placed { e, data, rsrc });
        }

        // ---- extents overflow records (key: fork, cnid, first block of the record within the fork)
        let mut overflow: Vec<(u8, u32, u16, [(u16, u16); 3])> = Vec::new();
        for p in &placed {
            for (fork, runs) in [(0x00u8, &p.data), (0xFFu8, &p.rsrc)] {
                let mut covered: usize = runs.iter().take(3).map(|r| r.1 as usize).sum();
                for chunk in runs.iter().skip(3).collect::<Vec<_>>().chunks(3) {
                    let mut rec = [(0u16, 0u16); 3];
                    for (i, r) in chunk.iter().enumerate() {
                        rec[i] = **r;
                    }
                    overflow.push((fork, p.e.cnid, covered as u16, rec));
                    covered += chunk.iter().map(|r| r.1 as usize).sum::<usize>();
                }
            }
        }
        overflow.sort_by_key(|o| (o.1, o.0, o.2));
        assert!(overflow.len() <= 20, "too many overflow records for one leaf");

        // ---- catalog records
        let mut recs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new(); // (key incl. length byte, record data)
        let root_children = self.entries.iter().filter(|e| e.parent == ROOT).count() as u16;
        recs.push((cat_key(1, &self.volume), dir_record(ROOT, root_children)));
        recs.push((cat_key(ROOT, b""), thread_record(1, &self.volume)));
        for p in &placed {
            let e = p.e;
            match &e.kind {
                Kind::Dir => {
                    let n = self.entries.iter().filter(|c| c.parent == e.cnid).count() as u16;
                    recs.push((cat_key(e.parent, &e.name), dir_record(e.cnid, n)));
                    recs.push((cat_key(e.cnid, b""), thread_record(e.parent, &e.name)));
                }
                Kind::File { data, rsrc, ftype, creator, locked, .. } => {
                    recs.push((
                        cat_key(e.parent, &e.name),
                        file_record(e.cnid, *ftype, *creator, *locked, data.len(), rsrc.len(), &p.data, &p.rsrc, bs),
                    ));
                }
            }
        }
        recs.sort_by(|a, b| sort_key(&a.0).cmp(&sort_key(&b.0)));

        // ---- B-tree files
        let ext_nodes = overflow_tree(&overflow);
        let (cat_nodes, cat_depth, cat_root, cat_leaves, cat_leaf_records) = catalog_tree(&recs);
        let ext_blocks = ext_nodes.len() * NODE / bs + usize::from(!(ext_nodes.len() * NODE).is_multiple_of(bs));
        let cat_blocks = cat_nodes.len() * NODE / bs + usize::from(!(cat_nodes.len() * NODE).is_multiple_of(bs));
        let ext_start = next_block;
        next_block += ext_blocks;
        let cat_start = next_block;
        next_block += cat_blocks;
        let total_blocks = next_block;
        assert!(total_blocks < 0xFFFF);
        let _ = (cat_depth, cat_root, cat_leaves, cat_leaf_records);

        // ---- image
        let mut img = vec![0u8; AL_BL_ST * 512 + total_blocks * bs + 1024];
        let at = |block: usize| AL_BL_ST * 512 + block * bs;
        for (runs, bytes) in &blobs {
            let mut off = 0;
            for &(s, c) in runs {
                let n = (c as usize * bs).min(bytes.len() - off);
                img[at(s as usize)..at(s as usize) + n].copy_from_slice(&bytes[off..off + n]);
                off += n;
            }
        }
        for (i, n) in ext_nodes.iter().enumerate() {
            img[at(ext_start) + i * NODE..at(ext_start) + (i + 1) * NODE].copy_from_slice(n);
        }
        for (i, n) in cat_nodes.iter().enumerate() {
            img[at(cat_start) + i * NODE..at(cat_start) + (i + 1) * NODE].copy_from_slice(n);
        }

        let m = &mut img[1024..1024 + 162];
        m[0..2].copy_from_slice(&0x4244u16.to_be_bytes());
        m[2..6].copy_from_slice(&CREATED.to_be_bytes());
        m[6..10].copy_from_slice(&MODIFIED.to_be_bytes());
        m[14..16].copy_from_slice(&3u16.to_be_bytes()); // drVBMSt
        m[18..20].copy_from_slice(&(total_blocks as u16).to_be_bytes());
        m[20..24].copy_from_slice(&(bs as u32).to_be_bytes());
        m[28..30].copy_from_slice(&(AL_BL_ST as u16).to_be_bytes());
        m[30..34].copy_from_slice(&self.next_cnid.to_be_bytes());
        m[36] = self.volume.len() as u8;
        m[37..37 + self.volume.len()].copy_from_slice(&self.volume);
        m[130..134].copy_from_slice(&((ext_nodes.len() * NODE) as u32).to_be_bytes());
        m[134..136].copy_from_slice(&(ext_start as u16).to_be_bytes());
        m[136..138].copy_from_slice(&(ext_blocks as u16).to_be_bytes());
        m[146..150].copy_from_slice(&((cat_nodes.len() * NODE) as u32).to_be_bytes());
        m[150..152].copy_from_slice(&(cat_start as u16).to_be_bytes());
        m[152..154].copy_from_slice(&(cat_blocks as u16).to_be_bytes());
        img
    }
}

/// Catalog key of a leaf record: `keyLen, reserved, parent ID, name (Pascal)`.
fn cat_key(parent: u32, name: &[u8]) -> Vec<u8> {
    let mut k = vec![(6 + name.len()) as u8, 0];
    k.extend_from_slice(&parent.to_be_bytes());
    k.push(name.len() as u8);
    k.extend_from_slice(name);
    k
}

/// Order of the catalog: parent ID, then the name (ASCII case folded; the reader does not depend on the order).
fn sort_key(key: &[u8]) -> (u32, Vec<u8>) {
    (u32::from_be_bytes([key[2], key[3], key[4], key[5]]), key[7..].to_ascii_uppercase())
}

fn dir_record(cnid: u32, valence: u16) -> Vec<u8> {
    let mut r = vec![0u8; 70];
    r[0] = 1;
    r[4..6].copy_from_slice(&valence.to_be_bytes());
    r[6..10].copy_from_slice(&cnid.to_be_bytes());
    r[10..14].copy_from_slice(&CREATED.to_be_bytes());
    r[14..18].copy_from_slice(&MODIFIED.to_be_bytes());
    r
}

fn thread_record(parent: u32, name: &[u8]) -> Vec<u8> {
    let mut r = vec![0u8; 14];
    r[0] = 3;
    r[10..14].copy_from_slice(&parent.to_be_bytes());
    r.push(name.len() as u8);
    r.extend_from_slice(name);
    r
}

#[allow(clippy::too_many_arguments)]
fn file_record(cnid: u32, ftype: [u8; 4], creator: [u8; 4], locked: bool, dlen: usize, rlen: usize, d: &Runs, r: &Runs, bs: usize) -> Vec<u8> {
    let mut f = vec![0u8; 98];
    f[0] = 2;
    f[2] = u8::from(locked);
    f[4..8].copy_from_slice(&ftype);
    f[8..12].copy_from_slice(&creator);
    f[20..24].copy_from_slice(&cnid.to_be_bytes());
    f[24..26].copy_from_slice(&d.first().map_or(0, |x| x.0).to_be_bytes());
    f[26..30].copy_from_slice(&(dlen as u32).to_be_bytes());
    f[30..34].copy_from_slice(&((d_blocks(d) * bs) as u32).to_be_bytes());
    f[34..36].copy_from_slice(&r.first().map_or(0, |x| x.0).to_be_bytes());
    f[36..40].copy_from_slice(&(rlen as u32).to_be_bytes());
    f[40..44].copy_from_slice(&((d_blocks(r) * bs) as u32).to_be_bytes());
    f[44..48].copy_from_slice(&CREATED.to_be_bytes());
    f[48..52].copy_from_slice(&MODIFIED.to_be_bytes());
    for (i, x) in d.iter().take(3).enumerate() {
        f[74 + i * 4..76 + i * 4].copy_from_slice(&x.0.to_be_bytes());
        f[76 + i * 4..78 + i * 4].copy_from_slice(&x.1.to_be_bytes());
    }
    for (i, x) in r.iter().take(3).enumerate() {
        f[86 + i * 4..88 + i * 4].copy_from_slice(&x.0.to_be_bytes());
        f[88 + i * 4..90 + i * 4].copy_from_slice(&x.1.to_be_bytes());
    }
    f
}

fn d_blocks(runs: &Runs) -> usize {
    runs.iter().map(|r| r.1 as usize).sum()
}

/// One B-tree node from its records; `kind` 0xFF = leaf, 0x00 = index, 1 = header.
fn node(kind: u8, height: u8, flink: u32, blink: u32, records: &[Vec<u8>]) -> [u8; NODE] {
    let mut n = [0u8; NODE];
    n[0..4].copy_from_slice(&flink.to_be_bytes());
    n[4..8].copy_from_slice(&blink.to_be_bytes());
    n[8] = kind;
    n[9] = height;
    n[10..12].copy_from_slice(&(records.len() as u16).to_be_bytes());
    let mut off = 14usize;
    for (i, r) in records.iter().enumerate() {
        n[off..off + r.len()].copy_from_slice(r);
        n[NODE - 2 * (i + 1)..NODE - 2 * i].copy_from_slice(&(off as u16).to_be_bytes());
        off += r.len();
    }
    n[NODE - 2 * (records.len() + 1)..NODE - 2 * records.len()].copy_from_slice(&(off as u16).to_be_bytes()); // free space
    n
}

fn header_node(depth: u16, root: u32, leaf_records: u32, first_leaf: u32, last_leaf: u32, total: u32) -> [u8; NODE] {
    let mut h = vec![0u8; 106];
    h[0..2].copy_from_slice(&depth.to_be_bytes());
    h[2..6].copy_from_slice(&root.to_be_bytes());
    h[6..10].copy_from_slice(&leaf_records.to_be_bytes());
    h[10..14].copy_from_slice(&first_leaf.to_be_bytes());
    h[14..18].copy_from_slice(&last_leaf.to_be_bytes());
    h[18..20].copy_from_slice(&(NODE as u16).to_be_bytes());
    h[20..22].copy_from_slice(&37u16.to_be_bytes());
    h[22..26].copy_from_slice(&total.to_be_bytes());
    let mut map = vec![0u8; 256];
    for i in 0..total as usize {
        map[i / 8] |= 0x80 >> (i % 8);
    }
    // the records land at the fixed positions of a header node: 14 (header), 120 (user data), 248 (map)
    node(1, 0, 0, 0, &[h, vec![0u8; 128], map])
}

/// Extents overflow file: header node and one leaf.
fn overflow_tree(records: &[(u8, u32, u16, [(u16, u16); 3])]) -> Vec<[u8; NODE]> {
    let recs: Vec<Vec<u8>> = records
        .iter()
        .map(|&(fork, cnid, start, ext)| {
            let mut r = vec![7u8, fork];
            r.extend_from_slice(&cnid.to_be_bytes());
            r.extend_from_slice(&start.to_be_bytes());
            for (s, c) in ext {
                r.extend_from_slice(&s.to_be_bytes());
                r.extend_from_slice(&c.to_be_bytes());
            }
            r
        })
        .collect();
    vec![header_node(1, 1, recs.len() as u32, 1, 1, 2), node(0xFF, 1, 0, 0, &recs)]
}

/// Catalog file: header node, leaves chained by `fLink`, one index node above them when there are several leaves.
/// Returns (nodes, depth, root node, leaf count, record count).
fn catalog_tree(recs: &[(Vec<u8>, Vec<u8>)]) -> (Vec<[u8; NODE]>, u16, u32, usize, usize) {
    // record = key (+ pad byte to an even length) + data
    let flat: Vec<Vec<u8>> = recs
        .iter()
        .map(|(k, d)| {
            let mut r = k.clone();
            if r.len() % 2 == 1 {
                r.push(0);
            }
            r.extend_from_slice(d);
            r
        })
        .collect();

    let mut leaves: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 14 + 2;
    for (i, r) in flat.iter().enumerate() {
        if used + r.len() + 2 > NODE {
            leaves.push(Vec::new());
            used = 14 + 2;
        }
        leaves.last_mut().unwrap().push(i);
        used += r.len() + 2;
    }
    let l = leaves.len();
    let mut nodes: Vec<[u8; NODE]> = Vec::new();
    let (depth, root) = if l == 1 { (1u16, 1u32) } else { (2u16, l as u32 + 1) };
    nodes.push(header_node(depth, root, recs.len() as u32, 1, l as u32, l as u32 + 1 + u32::from(l > 1)));
    for (i, idx) in leaves.iter().enumerate() {
        let rs: Vec<Vec<u8>> = idx.iter().map(|&j| flat[j].clone()).collect();
        let flink = if i + 1 < l { i as u32 + 2 } else { 0 };
        let blink = i as u32; // 0 for the first leaf
        nodes.push(node(0xFF, 1, flink, blink, &rs));
    }
    if l > 1 {
        let index: Vec<Vec<u8>> = leaves
            .iter()
            .enumerate()
            .map(|(i, idx)| {
                // index key: fixed length 37, the first key of the child; pointer to the child node
                let first = &recs[idx[0]].0;
                let mut k = vec![37u8, 0];
                k.extend_from_slice(&first[2..6]);
                let name = &first[7..];
                k.push(name.len() as u8);
                k.extend_from_slice(name);
                k.resize(38, 0);
                k.extend_from_slice(&(i as u32 + 1).to_be_bytes());
                k
            })
            .collect();
        nodes.push(node(0x00, 2, 0, 0, &index));
    }
    (nodes, depth, root, l, recs.len())
}
