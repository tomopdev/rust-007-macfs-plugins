//! macOS installer packages (`.pkg`, flat packages = XAR archives) as a read-only virtual volume.
//!
//! What the panel shows when a package is opened:
//!
//! * every entry of the XAR archive as it is stored (`Distribution`, `Resources`, and for product archives one
//!   `Name.pkg` folder per component with `PackageInfo`, `Bom`, `Payload`, `Scripts`),
//! * [`TOC_NAME`] – the decompressed XAR table of contents (`toc.xml`),
//! * [`SUMMARY_NAME`] – the metadata of the components in a readable form (identifier, version, install location,
//!   file counts and sizes, scripts, bundles; the title and product of the `Distribution`),
//! * the folder [`INSTALLED_NAME`] – the files the package installs, read from the `Payload` of each component
//!   (a folder per component when there are several). Its root also holds [`INDEX_NAME`], a `cpio -tv` like listing
//!   of the payload: modes, owners, sizes, dates, symlink targets and hard links as they are stored in the CPIO archive.
//!
//! The XAR container is parsed by the `xara` crate. Entry data is read through [`Slice`] windows of the source,
//! so nothing is unpacked as a whole. A payload is a PBZX stream (random access through [`PbzxReader`], chunks decoded
//! on demand), a plain CPIO archive (read in place) or - older packages - a gzip / bzip2 / xz compressed CPIO archive,
//! which cannot be read randomly and is therefore decoded once into an anonymous temporary file.
//! A payload that cannot be read does not fail the package: its folder then holds [`PAYLOAD_ERROR_NAME`].

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use xara::{XarArchive, XarError, XarFile, XarFileData, XarFileType, XarHeader};

use crate::cpio::{self, Index, Tree};
use crate::pbzx::{self, PbzxReader};
use crate::source::{DiskSource, Slice};
use crate::util::NameSet;
use crate::volume::{Node, NodeKind, VolumeError};

/// Virtual file: the decompressed XAR table of contents.
pub const TOC_NAME: &str = "!XAR-TOC.xml";
/// Virtual file: the metadata of the package and its components.
pub const SUMMARY_NAME: &str = "!PKG-COMPONENTS.txt";
/// Virtual folder: the files the package installs (the payloads).
pub const INSTALLED_NAME: &str = "Installed Files";
/// Virtual file in the root of a payload: the CPIO index.
pub const INDEX_NAME: &str = "!CPIO-INDEX.txt";
/// Virtual file that replaces a payload which could not be read.
pub const PAYLOAD_ERROR_NAME: &str = "!PAYLOAD-ERROR.txt";

/// Largest XAR table of contents accepted (decompressed).
const MAX_TOC: u64 = 256 << 20;
/// Largest `PackageInfo` / `Distribution` file parsed.
const MAX_XML: u64 = 8 << 20;
/// Largest payload decoded into the temporary file (gzip / bzip2 / xz compressed payloads only).
const MAX_SPOOL: u64 = 64 << 30;
const COPY_BUFFER: usize = 256 << 10;

pub fn is_xar(head: &[u8]) -> bool {
    head.starts_with(b"xar!")
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Damaged data is a format error, everything else an I/O error (the plugins report the two differently).
fn io_err(e: io::Error) -> VolumeError {
    if e.kind() == io::ErrorKind::InvalidData { VolumeError::Format(e.to_string()) } else { VolumeError::Io(e) }
}

fn xar_err(e: XarError) -> VolumeError {
    match e {
        XarError::Io(e) => io_err(e),
        e => VolumeError::Format(format!("damaged installer package: {e}")),
    }
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}
type Stream = Box<dyn ReadSeek>;

// ---------------------------------------------------------------------------
// Package metadata (PackageInfo, Distribution)
// ---------------------------------------------------------------------------

struct Elem {
    name: String,
    parent: String,
    attrs: Vec<(String, String)>,
    text: String,
}

impl Elem {
    fn attr(&self, key: &str) -> &str {
        self.attrs.iter().find(|(k, _)| k == key).map_or("", |(_, v)| v.as_str())
    }
}

/// All elements of an XML document with their attributes and text (what is wanted of a `PackageInfo`).
/// A document that turns out to be malformed ends the list early.
fn elements(xml: &[u8]) -> Vec<Elem> {
    fn make(e: &BytesStart<'_>, parent: &str) -> Elem {
        let attrs = e
            .attributes()
            .flatten()
            .map(|a| {
                (
                    a.key.local_name().as_ref().to_string(),
                    a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_default(),
                )
            })
            .collect();
        Elem { name: e.local_name().as_ref().to_string(), parent: parent.to_string(), attrs, text: String::new() }
    }

    let mut reader = Reader::from_reader(xml);
    let (mut buf, mut out, mut stack): (Vec<u8>, Vec<Elem>, Vec<usize>) = (Vec::new(), Vec::new(), Vec::new());
    loop {
        let parent = stack.last().map_or("", |&i| out[i].name.as_str()).to_string();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                out.push(make(&e, &parent));
                stack.push(out.len() - 1);
            }
            Ok(Event::Empty(e)) => out.push(make(&e, &parent)),
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Text(t)) => {
                if let Some(&i) = stack.last() {
                    out[i].text.push_str(&t.xml10_content());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let (Some(&i), Ok(c)) = (stack.last(), quick_xml::escape::unescape(&format!("&{};", r.xml10_content()))) {
                    out[i].text.push_str(&c);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

#[derive(Default)]
struct PackageInfo {
    identifier: String,
    version: String,
    install_location: String,
    auth: String,
    files: Option<u64>,
    kbytes: Option<u64>,
    bundles: Vec<String>,
    scripts: Vec<String>,
}

fn parse_package_info(xml: &[u8]) -> PackageInfo {
    let mut p = PackageInfo::default();
    for e in elements(xml) {
        match e.name.as_str() {
            "pkg-info" => {
                p.identifier = e.attr("identifier").to_string();
                p.version = e.attr("version").to_string();
                p.install_location = e.attr("install-location").to_string();
                p.auth = e.attr("auth").to_string();
            }
            "payload" => {
                p.files = e.attr("numberOfFiles").parse().ok();
                p.kbytes = e.attr("installKBytes").parse().ok();
            }
            "bundle" => {
                let mut s = e.attr("id").to_string();
                let v = e.attr("CFBundleShortVersionString");
                if !v.is_empty() {
                    let _ = write!(s, " {v}");
                }
                if !e.attr("path").is_empty() {
                    let _ = write!(s, " ({})", e.attr("path"));
                }
                p.bundles.push(s);
            }
            n if e.parent == "scripts" => p.scripts.push(if e.attr("file").is_empty() { n.to_string() } else { format!("{n}: {}", e.attr("file")) }),
            _ => {}
        }
    }
    p
}

// ---------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------

/// A package inside the XAR archive: a folder with a `PackageInfo` and/or a `Payload`
/// (the archive root for a plain component package).
struct Component {
    /// XAR path of the folder (`""` = archive root).
    dir: String,
    /// Name of the component's folder under [`INSTALLED_NAME`] (used when there is more than one payload).
    folder: String,
    payload: Option<usize>,
    scripts: Option<usize>,
    bom: Option<usize>,
    info: PackageInfo,
}

impl Component {
    fn label(&self) -> &str {
        if self.dir.is_empty() { "(package)" } else { &self.dir }
    }
}

/// A payload that has been indexed.
struct Payload {
    stream: Stream,
    index: Index,
    tree: Tree,
    text: String,
    index_name: String,
}

enum Loaded {
    Ok(Box<Payload>),
    /// The payload cannot be read; the text goes into [`PAYLOAD_ERROR_NAME`].
    Failed { text: String, name: String },
}

struct Names {
    toc: String,
    summary: String,
    installed: String,
}

pub struct PkgVolume {
    src: Arc<Mutex<DiskSource>>,
    start: u64,
    len: u64,
    header: XarHeader,
    files: Vec<XarFile>,
    by_path: HashMap<String, usize>,
    /// Entries at the top of the XAR archive.
    root: Vec<usize>,
    /// Offset of the XAR heap in the package file.
    heap: u64,
    names: Names,
    components: Vec<Component>,
    summary: Option<String>,
    loaded: HashMap<usize, Loaded>,
}

fn join(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { format!("{a}/{b}") }
}

fn components_of(path: &str) -> Vec<&str> {
    path.split('/').filter(|c| !c.is_empty()).collect()
}

/// `base`, or `base (MacFS)` when a real entry already has that name.
fn free_name(base: &str, taken: &HashSet<String>) -> String {
    if taken.contains(&base.to_lowercase()) { format!("{base} (MacFS)") } else { base.to_string() }
}

/// Copy with a big buffer (the plugins report progress per write).
fn copy(r: &mut dyn Read, out: &mut dyn Write) -> io::Result<u64> {
    let mut buf = vec![0u8; COPY_BUFFER];
    let mut total = 0u64;
    loop {
        match r.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => {
                out.write_all(&buf[..n])?;
                total += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Anonymous temporary file (removed when closed, also if the process dies).
fn spool_file() -> io::Result<File> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("macfs-pkg-{}-{}.tmp", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let mut o = OpenOptions::new();
    o.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(0x0400_0000); // FILE_FLAG_DELETE_ON_CLOSE
    }
    let f = o.open(&path)?;
    #[cfg(not(windows))]
    let _ = std::fs::remove_file(&path);
    Ok(f)
}

/// Decode a stream into a temporary file that can be read randomly.
fn spool(r: impl Read) -> io::Result<File> {
    let mut f = spool_file()?;
    {
        let mut w = BufWriter::with_capacity(1 << 20, &f);
        let n = copy(&mut r.take(MAX_SPOOL + 1), &mut w)?;
        w.flush()?;
        if n > MAX_SPOOL {
            return Err(bad("the decoded payload is too large"));
        }
    }
    f.seek(SeekFrom::Start(0))?;
    Ok(f)
}

fn read_head(s: &mut Stream, n: usize) -> io::Result<Vec<u8>> {
    s.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match s.read(&mut buf[got..])? {
            0 => break,
            k => got += k,
        }
    }
    buf.truncate(got);
    s.seek(SeekFrom::Start(0))?;
    Ok(buf)
}

/// A CPIO archive that can be read randomly, from whatever the payload is stored as.
fn sniff(mut base: Stream) -> io::Result<Stream> {
    let head = read_head(&mut base, 6)?;
    if pbzx::is_pbzx(&head) {
        return Ok(Box::new(PbzxReader::open(base)?));
    }
    let decoder: Box<dyn Read> = if head.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(base))
    } else if head.starts_with(b"BZh") {
        Box::new(bzip2::read::BzDecoder::new(base))
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Box::new(lzma_rust2::XzReader::new(base, false))
    } else if head.starts_with(b"07070") {
        return Ok(base);
    } else {
        return Err(bad("unrecognised payload format (expected PBZX, gzip / bzip2 / xz compressed CPIO, or CPIO)"));
    };
    let mut decoded: Stream = Box::new(spool(decoder)?);
    if !read_head(&mut decoded, 5)?.starts_with(b"07070") {
        return Err(bad("the decoded payload is not a CPIO archive"));
    }
    Ok(decoded)
}

impl PkgVolume {
    /// Open the XAR archive at `start..start + len` of the source.
    pub fn open(src: Arc<Mutex<DiskSource>>, start: u64, len: u64) -> Result<PkgVolume, VolumeError> {
        let mut slice = Slice::new(src.clone(), start, len);
        let header = xara::header::parse_header(&mut slice).map_err(xar_err)?;
        if header.toc_uncompressed_len > MAX_TOC || header.toc_compressed_len > len {
            return Err(VolumeError::Format("damaged installer package: the table of contents is implausibly large".into()));
        }
        slice.seek(SeekFrom::Start(0))?;
        let archive = XarArchive::open(&mut slice).map_err(xar_err)?;
        let files = archive.files().to_vec();
        drop(archive);
        let heap = u64::from(header.header_size) + header.toc_compressed_len;

        let mut by_path = HashMap::new();
        for (i, f) in files.iter().enumerate() {
            by_path.entry(f.path.clone()).or_insert(i);
        }
        let root: Vec<usize> = files.iter().enumerate().filter(|(_, f)| f.parent.is_none()).map(|(i, _)| i).collect();
        let taken: HashSet<String> = root.iter().map(|&i| files[i].name.to_lowercase()).collect();
        let names = Names {
            toc: free_name(TOC_NAME, &taken),
            summary: free_name(SUMMARY_NAME, &taken),
            installed: free_name(INSTALLED_NAME, &taken),
        };

        let mut vol = PkgVolume {
            src,
            start,
            len,
            header,
            files,
            by_path,
            root,
            heap,
            names,
            components: Vec::new(),
            summary: None,
            loaded: HashMap::new(),
        };
        vol.find_components();
        Ok(vol)
    }

    /// Folders with a `Payload` or a `PackageInfo` are the component packages.
    fn find_components(&mut self) {
        let mut dirs: Vec<String> = Vec::new();
        for f in &self.files {
            if f.file_type == XarFileType::File && (f.name == "Payload" || f.name == "PackageInfo") {
                let dir = f.parent.map(|p| self.files[p].path.clone()).unwrap_or_default();
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
        }
        let mut folders = NameSet::default();
        let mut comps = Vec::new();
        for dir in dirs {
            let find = |n: &str| self.by_path.get(&join(&dir, n)).copied().filter(|&i| self.files[i].file_type == XarFileType::File);
            let info = find("PackageInfo")
                .and_then(|i| self.read_small(i).ok())
                .map(|xml| parse_package_info(&xml))
                .unwrap_or_default();
            let base = dir.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("Package").to_string();
            comps.push(Component {
                folder: folders.unique(base),
                payload: find("Payload"),
                scripts: find("Scripts"),
                bom: find("Bom"),
                info,
                dir,
            });
        }
        self.components = comps;
    }

    // ---- reading XAR entries ------------------------------------------------

    fn window(&self, d: &XarFileData) -> io::Result<Slice> {
        let off = self.heap.checked_add(d.offset).filter(|o| o.checked_add(d.length).is_some_and(|e| e <= self.len));
        match off {
            Some(off) => Ok(Slice::new(self.src.clone(), self.start + off, d.length)),
            None => Err(bad("an entry of the installer package lies outside the file")),
        }
    }

    /// Decoded content of an entry (exactly its declared size; a shorter stream is reported by the caller).
    fn entry_reader(&self, f: &XarFile) -> io::Result<Box<dyn Read>> {
        let d = f.data.as_ref().ok_or_else(|| bad("the XAR entry has no data"))?;
        let raw = self.window(d)?;
        let r: Box<dyn Read> = match d.encoding.as_str() {
            "application/octet-stream" => Box::new(raw),
            "application/x-gzip" => Box::new(flate2::read::GzDecoder::new(raw)),
            "application/zlib" | "application/x-zlib" => Box::new(flate2::read::ZlibDecoder::new(raw)),
            "application/x-bzip2" => Box::new(bzip2::read::BzDecoder::new(raw)),
            "application/x-xz" => Box::new(lzma_rust2::XzReader::new(raw, false)),
            other => return Err(bad(format!("unsupported XAR encoding {other}"))),
        };
        Ok(Box::new(r.take(d.size)))
    }

    fn read_small(&self, idx: usize) -> io::Result<Vec<u8>> {
        let f = &self.files[idx];
        let mut buf = Vec::new();
        if f.data.as_ref().is_some_and(|d| d.size > MAX_XML) {
            return Err(bad("XML file is too large"));
        }
        if f.data.is_some() {
            self.entry_reader(f)?.read_to_end(&mut buf)?;
        }
        Ok(buf)
    }

    fn copy_entry(&self, idx: usize, out: &mut dyn Write) -> io::Result<u64> {
        let f = &self.files[idx];
        match f.file_type {
            XarFileType::Symlink => {
                let t = f.link.as_deref().unwrap_or("");
                out.write_all(t.as_bytes())?;
                Ok(t.len() as u64)
            }
            _ if f.data.is_none() => Ok(0), // an empty file
            _ => {
                let size = f.data.as_ref().map_or(0, |d| d.size);
                let n = copy(&mut self.entry_reader(f)?, out)?;
                if n != size {
                    return Err(bad(format!("the entry {} is cut off ({n} of {size} bytes)", f.path)));
                }
                Ok(n)
            }
        }
    }

    fn toc_bytes(&self) -> io::Result<Vec<u8>> {
        let raw = Slice::new(self.src.clone(), self.start + u64::from(self.header.header_size), self.header.toc_compressed_len);
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(raw).take(self.header.toc_uncompressed_len).read_to_end(&mut out)?;
        Ok(out)
    }

    // ---- payloads -----------------------------------------------------------

    fn open_stream(&self, idx: usize) -> io::Result<Stream> {
        let f = &self.files[idx];
        let d = f.data.as_ref().ok_or_else(|| bad("the payload entry has no data"))?;
        let base: Stream = if d.encoding == "application/octet-stream" && d.length == d.size {
            Box::new(self.window(d)?)
        } else {
            Box::new(spool(self.entry_reader(f)?)?)
        };
        sniff(base)
    }

    fn try_load(&self, ci: usize) -> io::Result<Payload> {
        let c = &self.components[ci];
        let mut stream = self.open_stream(c.payload.expect("only components with a payload are loaded"))?;
        let index = cpio::scan(&mut stream)?;
        let tree = Tree::build(&index);
        let mut heading = format!("Payload of the package \"{}\"", c.label());
        for (what, v) in [("identifier", &c.info.identifier), ("version", &c.info.version), ("install location", &c.info.install_location)] {
            if !v.is_empty() {
                let _ = write!(heading, ", {what}: {v}");
            }
        }
        let text = cpio::index_text(&index, &heading);
        let taken: HashSet<String> = tree.list("").unwrap_or(&[]).iter().map(|c| c.name.to_lowercase()).collect();
        Ok(Payload { stream, index, tree, text, index_name: free_name(INDEX_NAME, &taken) })
    }

    fn ensure(&mut self, ci: usize) -> &mut Loaded {
        if !self.loaded.contains_key(&ci) {
            let l = match self.try_load(ci) {
                Ok(p) => Loaded::Ok(Box::new(p)),
                Err(e) => Loaded::Failed {
                    text: format!(
                        "The payload of the package \"{}\" could not be read: {e}\r\n\r\nThe other files of the package are not affected.\r\n",
                        self.components[ci].label()
                    ),
                    name: PAYLOAD_ERROR_NAME.to_string(),
                },
            };
            self.loaded.insert(ci, l);
        }
        self.loaded.get_mut(&ci).expect("just inserted")
    }

    fn payload_components(&self) -> Vec<usize> {
        (0..self.components.len()).filter(|&i| self.components[i].payload.is_some()).collect()
    }

    /// Component and the path inside its payload for a path below [`INSTALLED_NAME`].
    fn component_for<'a>(&self, rest: &'a [&'a str]) -> Option<(usize, &'a [&'a str])> {
        let pc = self.payload_components();
        match pc.len() {
            0 => None,
            1 => Some((pc[0], rest)),
            _ => {
                let first = rest.first()?;
                let ci = pc.into_iter().find(|&i| self.components[i].folder == *first)?;
                Some((ci, &rest[1..]))
            }
        }
    }

    fn has_installed(&self) -> bool {
        self.components.iter().any(|c| c.payload.is_some())
    }

    // ---- summary ------------------------------------------------------------

    fn build_summary(&self) -> String {
        let mut t = String::new();
        let h = &self.header;
        let _ = write!(
            t,
            "XAR archive: version {}, header {} bytes, table of contents {} bytes (zlib) / {} bytes, checksum {:?}\r\n",
            h.version, h.header_size, h.toc_compressed_len, h.toc_uncompressed_len, h.checksum_algo
        );
        match self.by_path.get("Distribution").copied() {
            Some(i) => {
                t.push_str("Product archive (it has a Distribution file)\r\n");
                match self.read_small(i) {
                    Ok(xml) => {
                        let els = elements(&xml);
                        if let Some(title) = els.iter().find(|e| e.name == "title") {
                            let _ = write!(t, "Title: {}\r\n", title.text.trim());
                        }
                        if let Some(p) = els.iter().find(|e| e.name == "product") {
                            let _ = write!(t, "Product: {} {}\r\n", p.attr("id"), p.attr("version"));
                        }
                        let refs: Vec<&Elem> = els.iter().filter(|e| e.name == "pkg-ref" && !e.text.trim().is_empty()).collect();
                        if !refs.is_empty() {
                            t.push_str("Package references:\r\n");
                            for r in refs {
                                let _ = write!(t, "  {} {} -> {}", r.attr("id"), r.attr("version"), r.text.trim());
                                if !r.attr("installKBytes").is_empty() {
                                    let _ = write!(t, " ({} KB installed)", r.attr("installKBytes"));
                                }
                                t.push_str("\r\n");
                            }
                        }
                    }
                    Err(e) => {
                        let _ = write!(t, "The Distribution file could not be read: {e}\r\n");
                    }
                }
            }
            None if !self.components.is_empty() => t.push_str("Component package (no Distribution file)\r\n"),
            None => t.push_str("Not an installer package: there is no PackageInfo, Payload or Distribution\r\n"),
        }

        for (n, c) in self.components.iter().enumerate() {
            let _ = write!(t, "\r\nComponent {}: {}\r\n", n + 1, c.label());
            let rows: [(&str, &str); 4] =
                [("Identifier", &c.info.identifier), ("Version", &c.info.version), ("Install location", &c.info.install_location), ("Authorization", &c.info.auth)];
            for (k, v) in rows {
                if !v.is_empty() {
                    let _ = write!(t, "  {k:<17} {v}\r\n");
                }
            }
            if let Some(p) = c.payload {
                let stored = self.files[p].data.as_ref().map_or(0, |d| d.size);
                let _ = write!(t, "  {:<17} {} ({stored} bytes)", "Payload", self.files[p].path);
                if let Some(n) = c.info.files {
                    let _ = write!(t, ", {n} files");
                }
                if let Some(k) = c.info.kbytes {
                    let _ = write!(t, ", {k} KB installed");
                }
                t.push_str("\r\n");
            } else {
                let _ = write!(t, "  {:<17} none\r\n", "Payload");
            }
            if let Some(s) = c.scripts {
                let _ = write!(t, "  {:<17} {}", "Scripts", self.files[s].path);
                if !c.info.scripts.is_empty() {
                    let _ = write!(t, " ({})", c.info.scripts.join(", "));
                }
                t.push_str("\r\n");
            }
            if let Some(b) = c.bom {
                let _ = write!(t, "  {:<17} {}\r\n", "Bom", self.files[b].path);
            }
            for b in c.info.bundles.iter().take(20) {
                let _ = write!(t, "  {:<17} {b}\r\n", "Bundle");
            }
            if c.info.bundles.len() > 20 {
                let _ = write!(t, "  {:<17} ... and {} more\r\n", "", c.info.bundles.len() - 20);
            }
        }
        t
    }

    fn summary_text(&mut self) -> &str {
        if self.summary.is_none() {
            self.summary = Some(self.build_summary());
        }
        self.summary.as_deref().expect("just built")
    }

    // ---- the volume interface -----------------------------------------------

    fn xar_node(f: &XarFile) -> Node {
        let (kind, size) = match f.file_type {
            XarFileType::Directory => (NodeKind::Dir, 0),
            XarFileType::Symlink => (NodeKind::Symlink, f.link.as_ref().map_or(0, |l| l.len() as u64)),
            XarFileType::File => (NodeKind::File, f.data.as_ref().map_or(0, |d| d.size)),
        };
        Node { name: f.name.clone(), kind, size, mtime: None }
    }

    fn text_node(name: &str, text: &str) -> Node {
        Node { name: name.to_string(), kind: NodeKind::File, size: text.len() as u64, mtime: None }
    }

    /// Entries of a directory (`/` separated path, `/` = root).
    pub fn list_directory(&mut self, path: &str) -> Result<Vec<Node>, VolumeError> {
        let comps = components_of(path);
        let not_found = || VolumeError::NotFound(path.to_string());
        let Some(first) = comps.first() else {
            let mut nodes: Vec<Node> = self.root.iter().map(|&i| Self::xar_node(&self.files[i])).collect();
            nodes.push(Node { name: self.names.toc.clone(), kind: NodeKind::File, size: self.header.toc_uncompressed_len, mtime: None });
            let (name, text_len) = (self.names.summary.clone(), self.summary_text().len() as u64);
            nodes.push(Node { name, kind: NodeKind::File, size: text_len, mtime: None });
            if self.has_installed() {
                nodes.push(Node { name: self.names.installed.clone(), kind: NodeKind::Dir, size: 0, mtime: None });
            }
            return Ok(nodes);
        };

        if *first == self.names.installed && self.has_installed() {
            let rest = &comps[1..];
            if self.payload_components().len() > 1 && rest.is_empty() {
                return Ok(self
                    .payload_components()
                    .into_iter()
                    .map(|i| Node { name: self.components[i].folder.clone(), kind: NodeKind::Dir, size: 0, mtime: None })
                    .collect());
            }
            let (ci, inner) = self.component_for(rest).ok_or_else(not_found)?;
            let dir = inner.join("/");
            return match self.ensure(ci) {
                Loaded::Failed { text, name } if inner.is_empty() => Ok(vec![Self::text_node(name, text)]),
                Loaded::Failed { .. } => Err(not_found()),
                Loaded::Ok(p) => {
                    let kids = p.tree.list(&dir).ok_or_else(not_found)?;
                    let mut nodes: Vec<Node> = kids
                        .iter()
                        .map(|c| {
                            let e = c.entry.map(|i| &p.index.entries[i]);
                            Node {
                                name: c.name.clone(),
                                kind: match e {
                                    _ if c.is_dir => NodeKind::Dir,
                                    Some(e) if e.is_symlink() => NodeKind::Symlink,
                                    _ => NodeKind::File,
                                },
                                size: match (c.entry, c.is_dir) {
                                    (Some(i), false) => p.index.size_of(i),
                                    _ => 0,
                                },
                                mtime: e.map(|e| e.mtime).filter(|&t| t > 0),
                            }
                        })
                        .collect();
                    if inner.is_empty() {
                        nodes.push(Self::text_node(&p.index_name, &p.text));
                    }
                    Ok(nodes)
                }
            };
        }

        let key = comps.join("/");
        let i = *self.by_path.get(&key).ok_or_else(not_found)?;
        let f = &self.files[i];
        if f.file_type != XarFileType::Directory {
            return Err(not_found());
        }
        Ok(f.children.iter().map(|&c| Self::xar_node(&self.files[c])).collect())
    }

    /// Stream a file: a XAR entry (decoded), a generated text, or a file of a payload.
    pub fn read_file_to(&mut self, path: &str, out: &mut dyn Write) -> Result<u64, VolumeError> {
        let comps = components_of(path);
        let not_found = || VolumeError::NotFound(path.to_string());
        let write_text = |out: &mut dyn Write, t: &str| -> Result<u64, VolumeError> {
            out.write_all(t.as_bytes())?;
            Ok(t.len() as u64)
        };
        match comps.as_slice() {
            [n] if *n == self.names.toc => {
                let toc = self.toc_bytes().map_err(io_err)?;
                out.write_all(&toc)?;
                Ok(toc.len() as u64)
            }
            [n] if *n == self.names.summary => {
                let t = self.summary_text().to_string();
                write_text(out, &t)
            }
            [n, rest @ ..] if *n == self.names.installed && self.has_installed() => {
                let (ci, inner) = self.component_for(rest).ok_or_else(not_found)?;
                match self.ensure(ci) {
                    Loaded::Failed { text, name } if inner == [name.as_str()] => {
                        let t = text.clone();
                        write_text(out, &t)
                    }
                    Loaded::Failed { .. } => Err(not_found()),
                    Loaded::Ok(p) => {
                        if inner == [p.index_name.as_str()] {
                            return write_text(out, &p.text);
                        }
                        let i = p.tree.entry(&inner.join("/")).ok_or_else(not_found)?;
                        if p.index.entries[i].is_dir() {
                            return Err(not_found());
                        }
                        let (off, size) = p.index.data_of(i);
                        p.stream.seek(SeekFrom::Start(off)).map_err(io_err)?;
                        let n = copy(&mut (&mut p.stream).take(size), out).map_err(io_err)?;
                        if n != size {
                            return Err(VolumeError::Format(format!("the file {} is cut off ({n} of {size} bytes)", p.index.entries[i].path)));
                        }
                        Ok(n)
                    }
                }
            }
            _ => {
                let i = *self.by_path.get(&comps.join("/")).ok_or_else(not_found)?;
                if self.files[i].file_type == XarFileType::Directory {
                    return Err(not_found());
                }
                self.copy_entry(i, out).map_err(io_err)
            }
        }
    }
}
