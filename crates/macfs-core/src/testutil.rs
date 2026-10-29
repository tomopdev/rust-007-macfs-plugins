//! Builders of synthetic installer packages for the tests of the core and of both plugins (feature `testutil`):
//! a XAR archive, CPIO archives (odc / newc) and PBZX streams. They follow the formats' specifications;
//! they are not captured from a real macOS package, so a real one should still be tried by hand.

use std::io::Write;

use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};

/// Modification time of every CPIO entry (2023-11-14 22:13:20 UTC).
pub const MTIME: u64 = 1_700_000_000;

// ---------------------------------------------------------------------------
// CPIO
// ---------------------------------------------------------------------------

pub const DIR: u32 = 0o040755;
pub const FILE: u32 = 0o100644;
pub const EXEC: u32 = 0o100755;
pub const LINK: u32 = 0o120755;

pub struct CpioBuilder {
    newc: bool,
    buf: Vec<u8>,
    next_ino: u32,
}

impl CpioBuilder {
    /// POSIX.1 archive, as Apple's installers use.
    pub fn odc() -> Self {
        CpioBuilder { newc: false, buf: Vec::new(), next_ino: 100 }
    }

    /// SVR4 archive with hexadecimal fields and 4 byte alignment.
    pub fn newc() -> Self {
        CpioBuilder { newc: true, buf: Vec::new(), next_ino: 100 }
    }

    /// Add an entry with its own inode.
    pub fn add(&mut self, path: &str, mode: u32, data: &[u8]) -> &mut Self {
        let ino = self.next_ino;
        self.next_ino += 1;
        let nlink = if mode & 0o170000 == 0o040000 { 2 } else { 1 };
        self.raw(path, mode, data, ino, nlink)
    }

    /// Add an entry with explicit inode and link count (hard links share both).
    pub fn raw(&mut self, path: &str, mode: u32, data: &[u8], ino: u32, nlink: u32) -> &mut Self {
        let name = format!("{path}\0");
        if self.newc {
            let h = format!(
                "070701{ino:08X}{mode:08X}{:08X}{:08X}{nlink:08X}{MTIME:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
                501, 20, data.len(), 1, 2, 0, 0, name.len(), 0
            );
            self.buf.extend_from_slice(h.as_bytes());
            self.buf.extend_from_slice(name.as_bytes());
            while !self.buf.len().is_multiple_of(4) {
                self.buf.push(0);
            }
            self.buf.extend_from_slice(data);
            while !self.buf.len().is_multiple_of(4) {
                self.buf.push(0);
            }
        } else {
            let h = format!(
                "070707{:06o}{ino:06o}{mode:06o}{:06o}{:06o}{nlink:06o}{:06o}{MTIME:011o}{:06o}{:011o}",
                1,
                501,
                20,
                0,
                name.len(),
                data.len()
            );
            self.buf.extend_from_slice(h.as_bytes());
            self.buf.extend_from_slice(name.as_bytes());
            self.buf.extend_from_slice(data);
        }
        self
    }

    /// The archive with its `TRAILER!!!` entry (the builder is empty afterwards).
    pub fn finish(&mut self) -> Vec<u8> {
        self.raw("TRAILER!!!", 0, b"", 0, 1);
        std::mem::take(&mut self.buf)
    }
}

/// A PBZX stream whose chunks are all stored uncompressed.
pub fn pbzx_stored(cpio: &[u8], chunk: usize) -> Vec<u8> {
    let mut v = b"pbzx".to_vec();
    v.extend_from_slice(&(chunk as u64).to_be_bytes());
    for part in cpio.chunks(chunk) {
        v.extend_from_slice(&(part.len() as u64).to_be_bytes());
        v.extend_from_slice(&(part.len() as u64).to_be_bytes());
        v.extend_from_slice(part);
    }
    v
}

/// A PBZX stream with XZ compressed chunks of (at most) `chunk` bytes.
pub fn pbzx_xz(cpio: &[u8], chunk: usize) -> Vec<u8> {
    let mut w = pbzx::PbzxWriter::new(Vec::new()).chunk_size(chunk);
    w.write_cpio(cpio).expect("write PBZX");
    w.finish().expect("finish PBZX")
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).expect("gzip");
    e.finish().expect("gzip")
}

// ---------------------------------------------------------------------------
// XAR
// ---------------------------------------------------------------------------

#[derive(Default)]
struct XNode {
    name: String,
    /// `(offset, stored length, size, encoding)`; `None` for directories.
    data: Option<(u64, u64, u64, &'static str)>,
    children: Vec<XNode>,
}

/// Builds a XAR archive (the container of flat packages).
#[derive(Default)]
pub struct XarBuilder {
    root: XNode,
    heap: Vec<u8>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl XarBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn node(&mut self, path: &str) -> &mut XNode {
        let mut n = &mut self.root;
        for part in path.split('/') {
            let pos = match n.children.iter().position(|c| c.name == part) {
                Some(p) => p,
                None => {
                    n.children.push(XNode { name: part.to_string(), ..XNode::default() });
                    n.children.len() - 1
                }
            };
            n = &mut n.children[pos];
        }
        n
    }

    fn add(&mut self, path: &str, stored: &[u8], size: usize, enc: &'static str) -> &mut Self {
        let offset = self.heap.len() as u64;
        self.heap.extend_from_slice(stored);
        self.node(path).data = Some((offset, stored.len() as u64, size as u64, enc));
        self
    }

    /// A file stored as it is (`application/octet-stream`); parent folders are created.
    pub fn file(&mut self, path: &str, data: &[u8]) -> &mut Self {
        self.add(path, data, data.len(), "application/octet-stream")
    }

    /// A file the XAR layer compresses with gzip.
    pub fn file_gzip(&mut self, path: &str, data: &[u8]) -> &mut Self {
        self.add(path, &gzip(data), data.len(), "application/x-gzip")
    }

    /// A file the XAR layer compresses with bzip2.
    pub fn file_bzip2(&mut self, path: &str, data: &[u8]) -> &mut Self {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        e.write_all(data).expect("bzip2");
        let stored = e.finish().expect("bzip2");
        self.add(path, &stored, data.len(), "application/x-bzip2")
    }

    /// An empty folder.
    pub fn dir(&mut self, path: &str) -> &mut Self {
        self.node(path);
        self
    }

    fn write_node(n: &XNode, id: &mut u64, out: &mut String) {
        *id += 1;
        out.push_str(&format!("<file id=\"{id}\"><name>{}</name>", esc(&n.name)));
        match n.data {
            Some((off, len, size, enc)) => out.push_str(&format!(
                "<type>file</type><data><length>{len}</length><offset>{off}</offset><size>{size}</size><encoding style=\"{enc}\"/></data>"
            )),
            None => out.push_str("<type>directory</type>"),
        }
        for c in &n.children {
            Self::write_node(c, id, out);
        }
        out.push_str("</file>");
    }

    /// The TOC XML as stored (before zlib).
    pub fn toc_xml(&self) -> String {
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xar><toc><creation-time>2023-11-14T22:13:20</creation-time>");
        let mut id = 0;
        for c in &self.root.children {
            Self::write_node(c, &mut id, &mut xml);
        }
        xml.push_str("</toc></xar>");
        xml
    }

    pub fn build(&self) -> Vec<u8> {
        let toc = self.toc_xml();
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(toc.as_bytes()).expect("zlib");
        let z = z.finish().expect("zlib");
        let mut out = Vec::new();
        out.extend_from_slice(&0x7861_7221u32.to_be_bytes());
        out.extend_from_slice(&28u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(z.len() as u64).to_be_bytes());
        out.extend_from_slice(&(toc.len() as u64).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&z);
        out.extend_from_slice(&self.heap);
        out
    }
}

// ---------------------------------------------------------------------------
// Ready-made packages
// ---------------------------------------------------------------------------

pub const PACKAGE_INFO: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<pkg-info format-version=\"2\" identifier=\"com.example.tool\" version=\"1.2.3\" install-location=\"/usr/local\" auth=\"root\">\n\
    <payload numberOfFiles=\"6\" installKBytes=\"12\"/>\n\
    <bundle path=\"./Tool.app\" id=\"com.example.tool.app\" CFBundleShortVersionString=\"1.2\"/>\n\
    <scripts><preinstall file=\"./preinstall\"/><postinstall file=\"./postinstall\"/></scripts>\n\
</pkg-info>\n";

pub const DISTRIBUTION: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<installer-gui-script minSpecVersion=\"1\">\n\
    <title>Example &amp; Tool</title>\n\
    <product id=\"com.example.suite\" version=\"1.2.3\"/>\n\
    <pkg-ref id=\"com.example.tool\" version=\"1.2.3\" installKBytes=\"12\">#Tool.pkg</pkg-ref>\n\
    <choices-outline><line choice=\"default\"/></choices-outline>\n\
</installer-gui-script>\n";

/// Content of `bin/tool` in [`sample_payload`].
pub const SAMPLE_BIN: &[u8] = b"#!/bin/sh\necho hello from the package\n";

/// A typical payload: made-up parent directories (only `bin/` is an entry), a script, a symlink, a hard link pair
/// and a file with a name that is not valid on Windows. `big` is a payload file spanning several PBZX chunks.
pub fn sample_payload(newc: bool, big: &[u8]) -> Vec<u8> {
    let mut c = if newc { CpioBuilder::newc() } else { CpioBuilder::odc() };
    c.add(".", DIR, b"");
    c.add("./bin", DIR, b"");
    c.add("./bin/tool", EXEC, SAMPLE_BIN);
    c.add("./bin/tool-link", LINK, b"tool");
    c.add("./share/doc/tool/README.txt", FILE, b"read me");
    c.add("./share/doc/tool/big.bin", FILE, big);
    c.add("./share/a:b?.txt", FILE, b"illegal chars");
    // hard links: odc stores the data with every link, newc only with the last one
    if newc {
        c.raw("./share/hard-1.txt", FILE, b"", 900, 2);
        c.raw("./share/hard-2.txt", FILE, b"shared data", 900, 2);
    } else {
        c.raw("./share/hard-1.txt", FILE, b"shared data", 900, 2);
        c.raw("./share/hard-2.txt", FILE, b"", 900, 2);
    }
    c.finish()
}

/// A product archive: `Distribution`, `Resources/`, and the component `Tool.pkg` with `PackageInfo`, `Bom`,
/// `Scripts` and a `Payload` stored as PBZX (XZ chunks of 4 KiB, so the payload spans several chunks).
pub fn sample_pkg(big: &[u8]) -> Vec<u8> {
    let payload = sample_payload(false, big);
    let mut x = XarBuilder::new();
    x.file("Distribution", DISTRIBUTION.as_bytes());
    x.file("Resources/en.lproj/Localizable.strings", b"\"a\" = \"b\";");
    x.file("Tool.pkg/PackageInfo", PACKAGE_INFO.as_bytes());
    x.file("Tool.pkg/Bom", b"BOMStore");
    x.file("Tool.pkg/Scripts", &pbzx_stored(&CpioBuilder::odc().finish(), 1024));
    x.file("Tool.pkg/Payload", &pbzx_xz(&payload, 4096));
    x.build()
}

// ---------------------------------------------------------------------------
// sparse images
// ---------------------------------------------------------------------------

/// `.sparseimage` made of `raw` (a disk whose size is a multiple of 512): bands of `band_sectors` sectors that are all zeros are
/// left out (holes), the stored bands are put into the file in reverse order so that the band map is really needed.
/// A map that does not fit into 4096 bytes makes the header grow to a multiple of 4096 (the assumption of the reader).
pub fn sparse_image(raw: &[u8], band_sectors: u32) -> Vec<u8> {
    assert!(raw.len().is_multiple_of(512));
    let band = band_sectors as usize * 512;
    let nbands = raw.len().div_ceil(band);
    let header = (64 + 4 * nbands).div_ceil(4096).max(1) * 4096;
    let stored: Vec<usize> = (0..nbands).filter(|&b| raw[b * band..((b + 1) * band).min(raw.len())].iter().any(|&x| x != 0)).rev().collect();
    let mut out = vec![0u8; header];
    out[0..4].copy_from_slice(b"sprs");
    out[4..8].copy_from_slice(&3u32.to_be_bytes());
    out[8..12].copy_from_slice(&band_sectors.to_be_bytes());
    out[12..16].copy_from_slice(&1u32.to_be_bytes());
    out[16..20].copy_from_slice(&((raw.len() / 512) as u32).to_be_bytes());
    for (slot, &b) in stored.iter().enumerate() {
        out[64 + 4 * slot..68 + 4 * slot].copy_from_slice(&(b as u32 + 1).to_be_bytes());
        let mut data = raw[b * band..((b + 1) * band).min(raw.len())].to_vec();
        data.resize(band, 0);
        out.extend_from_slice(&data);
    }
    out
}

/// Sparse bundle folder `dir` (created) made of `raw`: `Info.plist`, `Info.bckup`, empty `token`, `bands/<hex>` without the all-zero bands.
pub fn write_sparsebundle(dir: &std::path::Path, raw: &[u8], band_bytes: usize) {
    std::fs::create_dir_all(dir.join("bands")).unwrap();
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>CFBundleInfoDictionaryVersion</key>\n\t<string>6.0</string>\n\t<key>band-size</key>\n\t<integer>{band_bytes}</integer>\n\t<key>bundle-backingstore-version</key>\n\t<integer>1</integer>\n\t<key>diskimage-bundle-type</key>\n\t<string>com.apple.diskimage.sparsebundle</string>\n\t<key>size</key>\n\t<integer>{}</integer>\n</dict>\n</plist>\n",
        raw.len()
    );
    std::fs::write(dir.join("Info.plist"), &plist).unwrap();
    std::fs::write(dir.join("Info.bckup"), &plist).unwrap();
    std::fs::write(dir.join("token"), b"").unwrap();
    for (b, chunk) in raw.chunks(band_bytes).enumerate() {
        if chunk.iter().any(|&x| x != 0) {
            std::fs::write(dir.join("bands").join(format!("{b:x}")), chunk).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// Macintosh file containers (AppleDouble, MacBinary, BinHex)
// ---------------------------------------------------------------------------

/// Mac time (seconds since 1904) used by the container builders: 2023-11-14 22:13:20.
pub const MAC_TIME: u32 = 3_782_844_800;

/// MacBinary III file: header with CRC and `mBIN`, forks and comment padded to 128 bytes.
pub fn macbinary(name: &[u8], ftype: &[u8; 4], creator: &[u8; 4], flags: u16, data: &[u8], rsrc: &[u8], comment: &[u8]) -> Vec<u8> {
    let mut h = [0u8; 128];
    h[1] = name.len() as u8;
    h[2..2 + name.len()].copy_from_slice(name);
    h[65..69].copy_from_slice(ftype);
    h[69..73].copy_from_slice(creator);
    h[73] = (flags >> 8) as u8;
    h[83..87].copy_from_slice(&(data.len() as u32).to_be_bytes());
    h[87..91].copy_from_slice(&(rsrc.len() as u32).to_be_bytes());
    h[91..95].copy_from_slice(&(MAC_TIME - 86_400).to_be_bytes());
    h[95..99].copy_from_slice(&MAC_TIME.to_be_bytes());
    h[99..101].copy_from_slice(&(comment.len() as u16).to_be_bytes());
    h[101] = flags as u8;
    h[102..106].copy_from_slice(b"mBIN");
    h[122] = 130;
    h[123] = 129;
    let crc = crate::macfile::crc16(0, &h[..124]);
    h[124..126].copy_from_slice(&crc.to_be_bytes());
    let mut out = h.to_vec();
    for part in [data, rsrc, comment] {
        out.extend_from_slice(part);
        out.resize(out.len().next_multiple_of(128), 0);
    }
    out
}

/// MacBinary I file (no CRC, zero filled reserved area).
pub fn macbinary1(name: &[u8], ftype: &[u8; 4], creator: &[u8; 4], data: &[u8], rsrc: &[u8]) -> Vec<u8> {
    let mut v = macbinary(name, ftype, creator, 0, data, rsrc, b"");
    v[99..128].fill(0);
    v
}

/// BinHex 4.0 text of a file (CRCs, run-length encoding of runs of equal bytes, 64 characters per line).
pub fn binhex(name: &[u8], ftype: &[u8; 4], creator: &[u8; 4], flags: u16, data: &[u8], rsrc: &[u8]) -> String {
    use crate::macfile::crc16;
    let mut raw = vec![name.len() as u8];
    raw.extend_from_slice(name);
    raw.push(0);
    raw.extend_from_slice(ftype);
    raw.extend_from_slice(creator);
    raw.extend_from_slice(&flags.to_be_bytes());
    raw.extend_from_slice(&(data.len() as u32).to_be_bytes());
    raw.extend_from_slice(&(rsrc.len() as u32).to_be_bytes());
    let c = crc16(0, &raw);
    raw.extend_from_slice(&c.to_be_bytes());
    for fork in [data, rsrc] {
        raw.extend_from_slice(fork);
        let c = crc16(0, fork);
        raw.extend_from_slice(&c.to_be_bytes());
    }
    // run-length encoding: 0x90 is escaped as 0x90 0x00, a run of 4..=255 equal bytes becomes byte, 0x90, count
    let mut enc = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        let mut n = 1;
        while i + n < raw.len() && raw[i + n] == b && n < 255 {
            n += 1;
        }
        if n >= 4 {
            enc.push(b);
            if b == 0x90 {
                enc.push(0);
            }
            enc.extend_from_slice(&[0x90, n as u8]);
            i += n;
        } else {
            enc.push(b);
            if b == 0x90 {
                enc.push(0);
            }
            i += 1;
        }
    }
    const ALPHABET: &[u8; 64] = b"!\"#$%&'()*+,-012345689@ABCDEFGHIJKLMNPQRSTUVXYZ[`abcdefhijklmpqr";
    let mut chars = String::new();
    let (mut bits, mut nbits) = (0u32, 0);
    for &b in &enc {
        bits = bits << 8 | u32::from(b);
        nbits += 8;
        while nbits >= 6 {
            nbits -= 6;
            chars.push(ALPHABET[(bits >> nbits & 63) as usize] as char);
        }
        bits &= (1 << nbits) - 1;
    }
    if nbits > 0 {
        chars.push(ALPHABET[(bits << (6 - nbits) & 63) as usize] as char);
    }
    let mut out = String::from("(This file must be converted with BinHex 4.0)\r\n\r\n:");
    let all = chars;
    for (k, line) in all.as_bytes().chunks(64).enumerate() {
        if k > 0 {
            out.push_str("\r\n");
        }
        out.push_str(std::str::from_utf8(line).unwrap());
    }
    out.push_str(":\r\n");
    out
}

/// Entries of an AppleDouble / AppleSingle file: (entry id, data).
pub fn apple_double(single: bool, entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&(if single { 0x0005_1600u32 } else { 0x0005_1607 }).to_be_bytes());
    h.extend_from_slice(&0x0002_0000u32.to_be_bytes());
    h.extend_from_slice(b"Mac OS X        ");
    h.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    let mut off = 26 + 12 * entries.len();
    for (id, d) in entries {
        h.extend_from_slice(&id.to_be_bytes());
        h.extend_from_slice(&(off as u32).to_be_bytes());
        h.extend_from_slice(&(d.len() as u32).to_be_bytes());
        off += d.len();
    }
    for (_, d) in entries {
        h.extend_from_slice(d);
    }
    h
}

/// Finder Info entry (32 bytes) as in a `._` file.
pub fn finder_info(ftype: &[u8; 4], creator: &[u8; 4], flags: u16) -> Vec<u8> {
    let mut f = vec![0u8; 32];
    f[..4].copy_from_slice(ftype);
    f[4..8].copy_from_slice(creator);
    f[8..10].copy_from_slice(&flags.to_be_bytes());
    f
}
