#![allow(clippy::ptr_arg, clippy::manual_dangling_ptr, clippy::fn_to_numeric_cast)]

//! Tests the plugin through its real ABI: a mock "Salamander" built from the same
//! vtable layouts (entry object, general object, directory, progress dialog,
//! enumerator) drives `SalamanderPluginEntry` and the archiver methods.
//! The synthetic HFSX images come from the `hfsplus` crate's `testutil`.

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use apfs::testutil::ApfsImageBuilder;
use hfs_reader::testutil::{HfsImageBuilder, ROOT as HFS_ROOT};
use hfsplus::testutil::HfsPlusImageBuilder;
use macfs::{ImageFormat, MfsVolume};
use macfs_spl::spl::*;
use macfs_spl::win::{Answer, set_answer, take_messages};
use macfs_spl::{SalamanderPluginEntry, SalamanderPluginGetReqVer, SalamanderPluginGetSDKVer};

// ---------------------------------------------------------------------------
// mock CSalamanderGeneralAbstract (only the slots the plugin uses)
// ---------------------------------------------------------------------------

#[repr(C)]
struct MockGeneral {
    vtbl: *const usize,
}

unsafe extern "system" fn g_alloc(_this: *mut c_void, n: c_int) -> *mut c_void {
    let layout = Layout::from_size_align(n as usize + 16, 16).unwrap();
    unsafe {
        let p = alloc(layout);
        *(p as *mut usize) = n as usize + 16;
        p.add(16) as *mut c_void
    }
}
unsafe extern "system" fn g_free(_this: *mut c_void, p: *mut c_void) {
    if p.is_null() {
        return;
    }
    unsafe {
        let base = (p as *mut u8).sub(16);
        let size = *(base as *const usize);
        dealloc(base, Layout::from_size_align(size, 16).unwrap());
    }
}
unsafe extern "system" fn g_parent(_this: *mut c_void) -> HWND {
    null_mut()
}

fn general() -> *mut c_void {
    static G: OnceLock<usize> = OnceLock::new();
    *G.get_or_init(|| {
        let vt: &'static mut [usize; 40] = Box::leak(Box::new([0usize; 40]));
        vt[GENERAL_SLOT_GET_MSGBOX_PARENT] = g_parent as *const () as usize;
        vt[GENERAL_SLOT_ALLOC] = g_alloc as *const () as usize;
        vt[GENERAL_SLOT_FREE] = g_free as *const () as usize;
        Box::leak(Box::new(MockGeneral { vtbl: vt.as_ptr() })) as *mut MockGeneral as usize
    }) as *mut c_void
}

// ---------------------------------------------------------------------------
// mock CSalamanderPluginEntryAbstract / CSalamanderConnectAbstract
// ---------------------------------------------------------------------------

#[repr(C)]
struct MockEntry {
    vtbl: *const EntryVtbl,
    version: c_int,
    name: String,
    functions: u32,
    extensions: String,
    fs_name_null: bool,
}

unsafe extern "system" fn e_version(this: *mut Entry) -> c_int {
    unsafe { (*(this as *mut MockEntry)).version }
}
unsafe extern "system" fn e_parent(_this: *mut Entry) -> HWND {
    null_mut()
}
unsafe extern "system" fn e_set_basic(
    this: *mut Entry,
    name: *const c_char,
    functions: u32,
    _version: *const c_char,
    _copyright: *const c_char,
    _description: *const c_char,
    _reg: *const c_char,
    ext: *const c_char,
    fs: *const c_char,
) -> BOOL {
    unsafe {
        let m = &mut *(this as *mut MockEntry);
        m.name = CStr::from_ptr(name).to_string_lossy().into_owned();
        m.functions = functions;
        m.extensions = CStr::from_ptr(ext).to_string_lossy().into_owned();
        m.fs_name_null = fs.is_null();
    }
    TRUE
}
unsafe extern "system" fn e_general(_this: *mut Entry) -> *mut c_void {
    general()
}

static ENTRY_VTBL: EntryVtbl = EntryVtbl {
    GetVersion: e_version,
    GetParentWindow: e_parent,
    GetSalamanderDebug: null(),
    SetBasicPluginData: e_set_basic,
    GetSalamanderGeneral: e_general,
};

impl MockEntry {
    fn new(version: c_int) -> Box<MockEntry> {
        Box::new(MockEntry { vtbl: &ENTRY_VTBL, version, name: String::new(), functions: 0, extensions: String::new(), fs_name_null: false })
    }
}

#[repr(C)]
struct MockConnect {
    vtbl: *const ConnectVtbl,
    calls: Vec<(String, BOOL, BOOL)>,
}
unsafe extern "system" fn c_add_archiver(this: *mut Connect, ext: *const c_char, edit: BOOL, upd: BOOL) {
    unsafe {
        (*(this as *mut MockConnect)).calls.push((CStr::from_ptr(ext).to_string_lossy().into_owned(), edit, upd));
    }
}
static CONNECT_VTBL: ConnectVtbl = ConnectVtbl { AddCustomPacker: null(), AddCustomUnpacker: null(), AddPanelArchiver: c_add_archiver };

// ---------------------------------------------------------------------------
// mock CSalamanderDirectoryAbstract
// ---------------------------------------------------------------------------

/// Decode ANSI bytes the way Salamander's host would (emulated code page on Linux).
fn ansi(b: &[u8]) -> String {
    macfs_spl::win::from_ansi(b)
}

#[derive(Debug, Clone)]
struct Add {
    is_dir: bool,
    parent: String,
    name: String,
    ext: String,
    size: u64,
    attr: u32,
}

#[repr(C)]
struct MockDir {
    vtbl: *const DirVtbl,
    adds: Vec<Add>,
    valid: u32,
    cleared: bool,
    names: Vec<*mut c_char>,
}

unsafe extern "system" fn d_clear(this: *mut Dir, _pd: *mut c_void) {
    unsafe { (*(this as *mut MockDir)).cleared = true }
}
unsafe extern "system" fn d_valid(this: *mut Dir, v: u32) {
    unsafe { (*(this as *mut MockDir)).valid = v }
}
unsafe extern "system" fn d_flags(_this: *mut Dir, _f: u32) {}
unsafe fn record(this: *mut Dir, path: *const c_char, fd: *mut FileData, is_dir: bool) -> BOOL {
    unsafe {
        let m = &mut *(this as *mut MockDir);
        let f = std::ptr::read_unaligned(fd);
        let name_bytes = CStr::from_ptr(f.Name).to_bytes();
        assert_eq!(f.name_len(), name_bytes.len(), "NameLen must equal strlen(Name)");
        let name = ansi(name_bytes);
        assert_eq!((f.Bits >> 12) & 0xF, ICONOVERLAYINDEX_NOTUSED);
        assert!(f.DosName.is_null());
        assert!(f.Ext as usize >= f.Name as usize && f.Ext as usize <= f.Name as usize + name_bytes.len(), "Ext must point into Name");
        m.adds.push(Add {
            is_dir,
            parent: ansi(CStr::from_ptr(path).to_bytes()),
            name,
            ext: ansi(CStr::from_ptr(f.Ext).to_bytes()),
            size: f.Size.value(),
            attr: f.Attr,
        });
        m.names.push(f.Name); // freed in Drop, like Salamander would
        TRUE
    }
}
unsafe extern "system" fn d_add_file(this: *mut Dir, path: *const c_char, fd: *mut FileData, _pd: *mut c_void) -> BOOL {
    unsafe { record(this, path, fd, false) }
}
unsafe extern "system" fn d_add_dir(this: *mut Dir, path: *const c_char, fd: *mut FileData, _pd: *mut c_void) -> BOOL {
    unsafe { record(this, path, fd, true) }
}
static DIR_VTBL: DirVtbl = DirVtbl { Clear: d_clear, SetValidData: d_valid, SetFlags: d_flags, AddFile: d_add_file, AddDir: d_add_dir };

impl MockDir {
    fn new() -> Box<MockDir> {
        Box::new(MockDir { vtbl: &DIR_VTBL, adds: vec![], valid: 0, cleared: false, names: vec![] })
    }
    fn as_dir(&mut self) -> *mut Dir {
        self as *mut MockDir as *mut Dir
    }
    fn find(&self, name: &str) -> &Add {
        self.adds.iter().find(|a| a.name == name).unwrap_or_else(|| panic!("{name} not listed: {:?}", self.adds))
    }
}
impl Drop for MockDir {
    fn drop(&mut self) {
        for &n in &self.names {
            unsafe { g_free(null_mut(), n as *mut c_void) };
        }
    }
}

// ---------------------------------------------------------------------------
// mock CSalamanderForOperationsAbstract (progress dialog)
// ---------------------------------------------------------------------------

#[repr(C)]
struct MockOps {
    vtbl: *const OpsVtbl,
    opened: u32,
    closed: u32,
    two_bars: bool,
    total2: u64,
    added: u64,
    texts: Vec<String>,
    /// ProgressAddSize starts returning FALSE once this many bytes were reported.
    cancel_after: Option<u64>,
}
unsafe extern "system" fn o_open(this: *mut Ops, _t: *const c_char, two: BOOL, _p: HWND, _f: BOOL) {
    unsafe {
        let m = &mut *(this as *mut MockOps);
        m.opened += 1;
        m.two_bars = two != 0;
    }
}
unsafe extern "system" fn o_text(this: *mut Ops, txt: *const c_char, _d: BOOL) {
    unsafe { (*(this as *mut MockOps)).texts.push(CStr::from_ptr(txt).to_string_lossy().into_owned()) }
}
unsafe extern "system" fn o_total(this: *mut Ops, _t1: *const QuadWord, t2: *const QuadWord) {
    unsafe {
        let m = &mut *(this as *mut MockOps);
        if *t2 != QuadWord::MAX && m.two_bars && m.total2 == 0 {
            m.total2 = (*t2).value();
        }
    }
}
unsafe extern "system" fn o_set_size(_this: *mut Ops, _a: *const QuadWord, _b: *const QuadWord, _d: BOOL) -> BOOL {
    TRUE
}
unsafe extern "system" fn o_add(this: *mut Ops, n: c_int, _d: BOOL) -> BOOL {
    unsafe {
        let m = &mut *(this as *mut MockOps);
        m.added += n as u64;
        match m.cancel_after {
            Some(limit) if m.added >= limit => FALSE,
            _ => TRUE,
        }
    }
}
unsafe extern "system" fn o_enable(_this: *mut Ops, _e: BOOL) {}
unsafe extern "system" fn o_hwnd(_this: *mut Ops) -> HWND {
    null_mut()
}
unsafe extern "system" fn o_close(this: *mut Ops) {
    unsafe { (*(this as *mut MockOps)).closed += 1 }
}
static OPS_VTBL: OpsVtbl = OpsVtbl {
    OpenProgressDialog: o_open,
    ProgressDialogAddText: o_text,
    ProgressSetTotalSize: o_total,
    ProgressSetSize: o_set_size,
    ProgressAddSize: o_add,
    ProgressEnableCancel: o_enable,
    ProgressGetHWND: o_hwnd,
    CloseProgressDialog: o_close,
};
impl MockOps {
    fn new() -> Box<MockOps> {
        Box::new(MockOps { vtbl: &OPS_VTBL, opened: 0, closed: 0, two_bars: false, total2: 0, added: 0, texts: vec![], cancel_after: None })
    }
    fn as_ops(&mut self) -> *mut Ops {
        self as *mut MockOps as *mut Ops
    }
}

// ---------------------------------------------------------------------------
// mock SalEnumSelection
// ---------------------------------------------------------------------------

struct EnumState {
    items: Vec<(CString, bool, u64)>,
    pos: usize,
}
unsafe extern "system" fn enum_cb(
    _parent: HWND,
    enum_files: c_int,
    is_dir: *mut BOOL,
    size: *mut QuadWord,
    file_data: *mut *const FileData,
    param: *mut c_void,
    err: *mut c_int,
) -> *const c_char {
    unsafe {
        let st = &mut *(param as *mut EnumState);
        if enum_files == -1 {
            st.pos = 0;
            return null();
        }
        if !err.is_null() {
            *err = SALENUM_SUCCESS;
        }
        if !file_data.is_null() {
            *file_data = null();
        }
        let Some((name, dir, sz)) = st.items.get(st.pos) else { return null() };
        st.pos += 1;
        if !is_dir.is_null() {
            *is_dir = *dir as BOOL;
        }
        if !size.is_null() {
            *size = QuadWord::new(*sz);
        }
        name.as_ptr()
    }
}
fn enumerator(items: &[(&str, bool, u64)]) -> Box<EnumState> {
    Box::new(EnumState { items: items.iter().map(|(n, d, s)| (CString::new(*n).unwrap(), *d, *s)).collect(), pos: 0 })
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-spl-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    d
}

const NIHON: &str = "\u{65e5}\u{672c}.txt"; // not representable in the emulated ANSI code page
const CHUGOKU: &str = "\u{4e2d}\u{56fd}.txt";

fn flat_image() -> Vec<u8> {
    HfsPlusImageBuilder::new()
        .add_file("hello.txt", b"hello hfs+", 0o644)
        .add_file("big.bin", &vec![0xAB; 600_000], 0o644)
        .add_file(NIHON, b"nihon", 0o644)
        .add_file(CHUGOKU, b"chugoku", 0o644)
        .add_file("a:b?.txt", b"illegal", 0o644)
        .add_file("Re\u{301}sume\u{301}.txt", b"nfd", 0o644)
        .build()
}

fn small_image(tag: &str) -> Vec<u8> {
    HfsPlusImageBuilder::new().add_file("hello.txt", tag.as_bytes(), 0o644).build()
}

/// GPT disk with one HFS+ partition per image (512-byte sectors), 2 MiB apart.
fn gpt_disk(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let sector = 512usize;
    let stride = 2 << 20;
    let mut disk = vec![0u8; stride * (parts.len() + 1)];
    let h = sector;
    disk[h..h + 8].copy_from_slice(b"EFI PART");
    disk[h + 72..h + 80].copy_from_slice(&2u64.to_le_bytes());
    disk[h + 80..h + 84].copy_from_slice(&8u32.to_le_bytes());
    disk[h + 84..h + 88].copy_from_slice(&128u32.to_le_bytes());
    for (i, (label, vol)) in parts.iter().enumerate() {
        let start = stride * (i + 1);
        let e = 2 * sector + i * 128;
        disk[e..e + 16].copy_from_slice(&[0x00, 0x53, 0x46, 0x48, 0x00, 0x00, 0xAA, 0x11, 0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC, 0xAC]);
        disk[e + 32..e + 40].copy_from_slice(&((start / sector) as u64).to_le_bytes());
        disk[e + 40..e + 48].copy_from_slice(&(((start + vol.len()) / sector - 1) as u64).to_le_bytes());
        for (j, u) in label.encode_utf16().enumerate() {
            disk[e + 56 + j * 2..e + 58 + j * 2].copy_from_slice(&u.to_le_bytes());
        }
        disk[start..start + vol.len()].copy_from_slice(vol);
    }
    disk
}

/// The archiver interface as Salamander obtains it: entry -> plugin -> archiver.
fn archiver() -> &'static ArchiverVtbl {
    let mut e = MockEntry::new(103);
    unsafe {
        let obj = SalamanderPluginEntry(&mut *e as *mut MockEntry as *mut Entry);
        assert!(!obj.is_null());
        let pvt = &*(*(obj as *mut Obj<PluginVtbl>)).vtbl;
        let a = (pvt.GetInterfaceForArchiver)(obj);
        assert!(!a.is_null());
        &*(*(a as *const Obj<ArchiverVtbl>)).vtbl
    }
}

fn list(path: &PathBuf) -> (BOOL, Box<MockDir>) {
    let a = archiver();
    let mut dir = MockDir::new();
    let mut ops = MockOps::new();
    let c = CString::new(path.to_str().unwrap()).unwrap();
    let mut pd: *mut c_void = 1 as *mut c_void;
    let r = unsafe { (a.ListArchive)(null_mut(), ops.as_ops(), c.as_ptr(), dir.as_dir(), &mut pd) };
    assert!(pd.is_null(), "plugin must report no plugin data interface");
    (r, dir)
}

fn unpack(arc: &PathBuf, target: &PathBuf, root: &str, items: &[(&str, bool, u64)], ops: &mut MockOps) -> BOOL {
    let a = archiver();
    let c = CString::new(arc.to_str().unwrap()).unwrap();
    let t = CString::new(target.to_str().unwrap()).unwrap();
    let r = CString::new(root).unwrap();
    let mut st = enumerator(items);
    unsafe {
        (a.UnpackArchive)(null_mut(), ops.as_ops(), c.as_ptr(), null_mut(), t.as_ptr(), r.as_ptr(), enum_cb, &mut *st as *mut EnumState as *mut c_void)
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn entry_points_and_connect() {
    assert_eq!(SalamanderPluginGetReqVer(), 102);
    assert_eq!(SalamanderPluginGetSDKVer(), 103);

    // too old host -> refused with a message
    let _ = take_messages();
    let mut old = MockEntry::new(101);
    let r = unsafe { SalamanderPluginEntry(&mut *old as *mut MockEntry as *mut Entry) };
    assert!(r.is_null());
    assert!(take_messages().iter().any(|m| m.contains("requires")));

    // Altap Salamander 4.0 (102) and Open Salamander 5.0 (103) both accepted
    for v in [102, 103] {
        let mut e = MockEntry::new(v);
        let obj = unsafe { SalamanderPluginEntry(&mut *e as *mut MockEntry as *mut Entry) };
        assert!(!obj.is_null());
        assert_eq!(e.functions, FUNCTION_PANELARCHIVERVIEW);
        assert_eq!(e.extensions, "hfs;apfs;dmg;pkg;dsk;dc42;image;hfv;mfs;sparseimage;sparsebundle;hqx;macbin;macfsdev;hfsdev;apfsdev");
        assert!(e.fs_name_null);
        assert!(e.name.contains("HFS+"));

        unsafe {
            let pvt = &*(*(obj as *mut Obj<PluginVtbl>)).vtbl;
            let mut conn = MockConnect { vtbl: &CONNECT_VTBL, calls: vec![] };
            (pvt.Connect)(obj, null_mut(), &mut conn as *mut MockConnect as *mut Connect);
            assert_eq!(conn.calls, vec![("hfs;apfs;dmg;pkg;dsk;dc42;image;hfv;mfs;sparseimage;sparsebundle;hqx;macbin;macfsdev;hfsdev;apfsdev".to_string(), FALSE, FALSE)]);
            assert_eq!(
                (pvt.GetInterfaceForArchiver)(obj),
                &macfs_spl::plugin::ARCHIVER_OBJ as *const Obj<ArchiverVtbl> as *mut c_void
            );
            assert!((pvt.GetInterfaceForViewer)(obj).is_null());
            assert!((pvt.GetInterfaceForFS)(obj).is_null());
            assert!((pvt.GetInterfaceForMenuExt)(obj).is_null());
            assert_eq!((pvt.Release)(obj, null_mut(), FALSE), TRUE);
        }
    }
}
#[test]
fn list_flat_volume() {
    let dir = temp_dir();
    let img = dir.join("vol.hfs");
    fs::write(&img, flat_image()).unwrap();

    let (r, d) = list(&img);
    assert_eq!(r, TRUE);
    assert!(!d.cleared);
    assert_eq!(d.valid & VALID_DATA_SIZE, VALID_DATA_SIZE);
    assert_eq!(d.valid & 0x0002, 0, "DOS names are not provided");
    assert!(d.adds.iter().all(|a| a.parent.is_empty() && !a.is_dir));

    let hello = d.find("hello.txt");
    assert_eq!((hello.size, hello.ext.as_str(), hello.attr), (10, "txt", FILE_ATTRIBUTE_ARCHIVE));
    assert_eq!(d.find("big.bin").size, 600_000);
    assert_eq!(d.find("big.bin").ext, "bin");
    d.find("a_b_.txt"); // illegal characters replaced
    d.find("R\u{e9}sum\u{e9}.txt"); // NFC (and representable in the ANSI page)
    // two names that collapse to the same ANSI name stay distinguishable
    d.find("__.txt");
    d.find("__ (2).txt");
    fs::remove_dir_all(&dir).unwrap();
}
#[test]
fn list_multi_volume_puts_dirs_before_their_files() {
    let dir = temp_dir();
    let disk = dir.join("disk.img");
    fs::write(&disk, gpt_disk(&[("one", small_image("first")), ("two", small_image("second"))])).unwrap();

    let (r, d) = list(&disk);
    assert_eq!(r, TRUE);
    let kinds: Vec<(bool, &str, &str)> = d.adds.iter().map(|a| (a.is_dir, a.parent.as_str(), a.name.as_str())).collect();
    for part in ["Partition 1 - one", "Partition 2 - two"] {
        let dir_pos = kinds.iter().position(|k| *k == (true, "", part)).unwrap_or_else(|| panic!("{kinds:?}"));
        let file_pos = kinds.iter().position(|k| *k == (false, part, "hello.txt")).unwrap_or_else(|| panic!("{kinds:?}"));
        assert!(dir_pos < file_pos, "AddDir must precede AddFile into it");
    }
    assert_eq!(d.find("Partition 1 - one").attr, FILE_ATTRIBUTE_DIRECTORY);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn list_volume_inside_a_dmg() {
    let dir = temp_dir();
    let dmg = dir.join("vol.dmg");
    let mut w = ::udif::DmgWriter::create(&dmg).unwrap().compression(::udif::CompressionMethod::Lzfse);
    w.add_partition("disk image (Apple_HFSX : 0)", &flat_image()).unwrap();
    w.finish().unwrap();

    let (r, d) = list(&dmg);
    assert_eq!(r, TRUE);
    assert_eq!(d.find("hello.txt").size, 10);
    assert_eq!(d.find("big.bin").size, 600_000);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn list_encrypted_dmg_says_so() {
    let dir = temp_dir();
    let dmg = dir.join("secret.dmg");
    let mut data = b"encrcdsa".to_vec();
    data.resize(8192, 1);
    fs::write(&dmg, data).unwrap();
    let _ = take_messages();
    let (r, _d) = list(&dmg);
    assert_eq!(r, FALSE);
    assert!(take_messages().iter().any(|m| m.contains("encrypted")));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn list_rejects_non_hfs_with_message() {
    let dir = temp_dir();
    let junk = dir.join("junk.hfs");
    fs::write(&junk, vec![7u8; 100_000]).unwrap();
    let _ = take_messages();
    let (r, _d) = list(&junk);
    assert_eq!(r, FALSE);
    assert!(take_messages().iter().any(|m| m.contains("no HFS+")));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unpack_archive_keeps_full_unicode_names() {
    let dir = temp_dir();
    let img = dir.join("vol.hfs");
    fs::write(&img, flat_image()).unwrap();
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();

    // the panel shows ANSI-safe names, Salamander hands those back
    let items = [("hello.txt", false, 10), ("big.bin", false, 600_000), ("__.txt", false, 7), ("__ (2).txt", false, 5), ("a_b_.txt", false, 7)];
    let mut ops = MockOps::new();
    assert_eq!(unpack(&img, &out, "", &items, &mut ops), TRUE);

    assert_eq!(fs::read(out.join("hello.txt")).unwrap(), b"hello hfs+");
    assert_eq!(fs::read(out.join("big.bin")).unwrap(), vec![0xAB; 600_000]);
    // extracted under the real Unicode names, content matches the right file
    assert_eq!(fs::read(out.join(CHUGOKU)).unwrap(), b"chugoku");
    assert_eq!(fs::read(out.join(NIHON)).unwrap(), b"nihon");
    assert_eq!(fs::read(out.join("a_b_.txt")).unwrap(), b"illegal");

    assert_eq!((ops.opened, ops.closed, ops.two_bars), (1, 1, true));
    let expected_total: u64 = items.iter().map(|i| i.2 + 1).sum();
    assert_eq!(ops.total2, expected_total);
    assert!(ops.added >= 600_000 + 10 + 7 + 5 + 7);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unpack_archive_respects_archive_root() {
    let dir = temp_dir();
    let disk = dir.join("disk.img");
    fs::write(&disk, gpt_disk(&[("one", small_image("first")), ("two", small_image("second"))])).unwrap();

    // root = a folder: enumerated names are relative to it
    let out1 = dir.join("out1");
    fs::create_dir_all(&out1).unwrap();
    let mut ops = MockOps::new();
    assert_eq!(unpack(&disk, &out1, "Partition 2 - two", &[("hello.txt", false, 6)], &mut ops), TRUE);
    assert_eq!(fs::read(out1.join("hello.txt")).unwrap(), b"second");

    // root = archive root: the whole path is part of the name; directories are created
    let out2 = dir.join("out2");
    fs::create_dir_all(&out2).unwrap();
    let items = [("Partition 1 - one", true, 0), ("Partition 1 - one\\hello.txt", false, 5)];
    let mut ops = MockOps::new();
    assert_eq!(unpack(&disk, &out2, "", &items, &mut ops), TRUE);
    assert_eq!(fs::read(out2.join("Partition 1 - one").join("hello.txt")).unwrap(), b"first");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unpack_one_file_uses_the_panel_name_or_the_rename() {
    let dir = temp_dir();
    let img = dir.join("vol.hfs");
    fs::write(&img, flat_image()).unwrap();
    let a = archiver();
    let arc = CString::new(img.to_str().unwrap()).unwrap();

    let go = |name: &str, new_name: Option<&str>, target: &PathBuf| -> BOOL {
        let mut ops = MockOps::new();
        let n = CString::new(name).unwrap();
        let t = CString::new(target.to_str().unwrap()).unwrap();
        let nn = new_name.map(|s| CString::new(s).unwrap());
        let mut rn: BOOL = FALSE;
        unsafe {
            (a.UnpackOneFile)(
                null_mut(),
                ops.as_ops(),
                arc.as_ptr(),
                null_mut(),
                n.as_ptr(),
                null(),
                t.as_ptr(),
                nn.as_ref().map_or(null(), |c| c.as_ptr()),
                &mut rn,
            )
        }
    };

    let t1 = dir.join("t1");
    fs::create_dir_all(&t1).unwrap();
    // Salamander must find the file under the ANSI panel name it knows
    assert_eq!(go("__ (2).txt", None, &t1), TRUE);
    assert_eq!(fs::read(t1.join("__ (2).txt")).unwrap(), b"nihon");

    let t2 = dir.join("t2");
    fs::create_dir_all(&t2).unwrap();
    assert_eq!(go("hello.txt", Some("_renamed.txt"), &t2), TRUE);
    assert_eq!(fs::read(t2.join("_renamed.txt")).unwrap(), b"hello hfs+");

    // unknown name -> FALSE with a message
    let _ = take_messages();
    assert_eq!(go("nope.txt", None, &t2), FALSE);
    assert!(take_messages().iter().any(|m| m.contains("not found")));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancel_aborts_and_removes_the_partial_file() {
    let dir = temp_dir();
    let img = dir.join("vol.hfs");
    fs::write(&img, flat_image()).unwrap();
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();

    let mut ops = MockOps::new();
    ops.cancel_after = Some(1000);
    assert_eq!(unpack(&img, &out, "", &[("big.bin", false, 600_000), ("hello.txt", false, 10)], &mut ops), FALSE);
    assert!(!out.join("big.bin").exists(), "partial file must be removed");
    assert!(!out.join("hello.txt").exists(), "nothing after the cancel");
    assert_eq!(ops.closed, 1);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn overwrite_question_is_asked_once_and_honoured() {
    let dir = temp_dir();
    let img = dir.join("vol.hfs");
    fs::write(&img, flat_image()).unwrap();
    let items = [("hello.txt", false, 10)];

    for (answer, expect_ok, expect_content) in
        [(Answer::No, TRUE, &b"old"[..]), (Answer::Yes, TRUE, &b"hello hfs+"[..]), (Answer::Cancel, FALSE, &b"old"[..])]
    {
        let out = temp_dir();
        fs::write(out.join("hello.txt"), b"old").unwrap();
        set_answer(answer);
        let _ = take_messages();
        let mut ops = MockOps::new();
        assert_eq!(unpack(&img, &out, "", &items, &mut ops), expect_ok, "{answer:?}");
        assert_eq!(fs::read(out.join("hello.txt")).unwrap(), expect_content, "{answer:?}");
        assert_eq!(take_messages().iter().filter(|m| m.contains("already exists")).count(), 1);
        fs::remove_dir_all(&out).unwrap();
    }
    set_answer(Answer::Yes);
    fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// APFS (synthetic containers from `apfs::testutil`)
// ---------------------------------------------------------------------------

fn apfs_multi() -> Vec<u8> {
    let mut b = ApfsImageBuilder::new();
    b.volume("Macintosh HD").add_file("hello.txt", b"system");
    b.volume("Macintosh HD - Data")
        .snapshots(2)
        .add_dir("docs")
        .add_file("docs/note.txt", b"a note")
        .add_file("orig.bin", &vec![7u8; 5_000])
        .add_clone("copy.bin", "orig.bin");
    b.volume("Vault").encrypted().add_file("secret.txt", b"cipher");
    b.build()
}

#[test]
fn apfs_listing_reports_unsupported_features() {
    let dir = temp_dir();
    let img = dir.join("mac.apfs");
    fs::write(&img, apfs_multi()).unwrap();

    let (r, d) = list(&img);
    assert_eq!(r, TRUE);
    let kinds: Vec<(bool, &str, &str)> = d.adds.iter().map(|a| (a.is_dir, a.parent.as_str(), a.name.as_str())).collect();
    let data = "Partition 1 - Macintosh HD - Data";
    for want in [
        (true, "", "Partition 1 - Macintosh HD"),
        (false, "Partition 1 - Macintosh HD", "hello.txt"),
        (true, "", data),
        (true, data, "docs"),
        (false, data, "orig.bin"),
        (false, data, "copy.bin"),
        (false, data, "!APFS-NOTICE.txt"), // snapshots
        (false, data, "!APFS-CLONES.txt"), // clones
        (false, "Partition 1 - Vault", "!APFS-NOTICE.txt"), // encrypted
    ] {
        assert!(kinds.contains(&want), "{want:?} missing in {kinds:?}");
    }
    assert!(!kinds.iter().any(|k| k.2 == "secret.txt"), "encrypted files must not be listed: {kinds:?}");
    assert_eq!(kinds.iter().filter(|k| k.1 == "Partition 1 - Vault").count(), 1);
    assert!(!kinds.iter().any(|k| k.1 == "Partition 1 - Macintosh HD" && k.2.starts_with('!')), "clean volume has no notice");
    // directories are announced before their content, and the sizes of the notices are the real ones
    let dir_pos = kinds.iter().position(|k| *k == (true, data, "docs")).unwrap();
    let file_pos = kinds.iter().position(|k| *k == (false, "Partition 1 - Macintosh HD - Data\\docs", "note.txt")).unwrap();
    assert!(dir_pos < file_pos);
    assert!(d.find("!APFS-NOTICE.txt").size > 0 && d.find("!APFS-CLONES.txt").size > 0);
    assert_eq!(d.find("copy.bin").size, 5_000);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn apfs_unpack_including_the_notice_files() {
    let dir = temp_dir();
    let img = dir.join("mac.apfs");
    fs::write(&img, apfs_multi()).unwrap();
    let (_, d) = list(&img);
    let notice = d.adds.iter().find(|a| a.name == "!APFS-NOTICE.txt" && a.parent == "Partition 1 - Macintosh HD - Data").unwrap().size;
    let clones = d.find("!APFS-CLONES.txt").size;

    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let base = "Partition 1 - Macintosh HD - Data";
    let items = [
        ("copy.bin", false, 5_000),
        ("orig.bin", false, 5_000),
        ("docs", true, 0),
        ("docs\\note.txt", false, 6),
        ("!APFS-NOTICE.txt", false, notice),
        ("!APFS-CLONES.txt", false, clones),
    ];
    let mut ops = MockOps::new();
    assert_eq!(unpack(&img, &out, base, &items, &mut ops), TRUE);
    assert_eq!(fs::read(out.join("copy.bin")).unwrap(), vec![7u8; 5_000]);
    assert_eq!(fs::read(out.join("orig.bin")).unwrap(), vec![7u8; 5_000]);
    assert_eq!(fs::read(out.join("docs").join("note.txt")).unwrap(), b"a note");
    let n = fs::read_to_string(out.join("!APFS-NOTICE.txt")).unwrap();
    assert!(n.contains("2 snapshot(s)") && n.len() as u64 == notice, "{n}");
    let c = fs::read_to_string(out.join("!APFS-CLONES.txt")).unwrap();
    assert!(c.contains("cloned files") && c.len() as u64 == clones, "{c}");

    // the encrypted volume only has its notice
    let out2 = dir.join("out2");
    fs::create_dir_all(&out2).unwrap();
    let mut ops = MockOps::new();
    assert_eq!(unpack(&img, &out2, "Partition 1 - Vault", &[("!APFS-NOTICE.txt", false, 100)], &mut ops), TRUE);
    assert!(fs::read_to_string(out2.join("!APFS-NOTICE.txt")).unwrap().contains("is encrypted"));
    let mut ops = MockOps::new();
    let _ = take_messages();
    // not found -> the usual "skip this file?" question (answered Yes by the mock), nothing is written
    unpack(&img, &out2, "Partition 1 - Vault", &[("secret.txt", false, 6)], &mut ops);
    assert!(take_messages().iter().any(|m| m.contains("not found in the volume")));
    assert!(!out2.join("secret.txt").exists(), "ciphertext is never extracted");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn apfs_single_volume_sits_in_the_root() {
    let dir = temp_dir();
    let img = dir.join("data.apfs");
    let mut b = ApfsImageBuilder::new();
    b.volume("Data").add_file("hello.txt", b"hello apfs").add_file("Re\u{301}sume\u{301}.txt", b"nfd");
    fs::write(&img, b.build()).unwrap();
    let (r, d) = list(&img);
    assert_eq!(r, TRUE);
    assert!(d.adds.iter().all(|a| a.parent.is_empty() && !a.is_dir));
    assert_eq!(d.find("hello.txt").size, 10);
    d.find("R\u{e9}sum\u{e9}.txt");
    assert!(!d.adds.iter().any(|a| a.name.starts_with('!')), "no notice on a plain volume");

    // a container without any volume is reported, not silently shown empty
    let empty = dir.join("empty.apfs");
    fs::write(&empty, ApfsImageBuilder::new().build()).unwrap();
    let _ = take_messages();
    let (r, _) = list(&empty);
    assert_eq!(r, FALSE);
    assert!(take_messages().iter().any(|m| m.contains("no volume")));
    fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// classic HFS and MFS
// ---------------------------------------------------------------------------

#[test]
fn classic_hfs_and_mfs_images_list_and_unpack() {
    let dir = temp_dir();

    let mut b = HfsImageBuilder::new(b"Classic");
    let folder = b.add_dir(HFS_ROOT, b"System Folder");
    b.add_dir(HFS_ROOT, b"Empty");
    b.add_file(HFS_ROOT, b"Read Me", b"hello classic", b"");
    b.add_file(folder, b"Finder", b"data", b"resource fork");
    let hfs = dir.join("classic.hfs");
    fs::write(&hfs, b.build()).unwrap();

    let (r, d) = list(&hfs);
    assert_eq!(r, TRUE);
    assert_eq!(d.find("Read Me").size, 13);
    assert!(d.find("Empty").is_dir);
    assert!(d.find("System Folder").is_dir);
    assert_eq!(d.find("Finder").size, 4);
    assert_eq!(d.find("Finder.rsrc").size, 13);

    let target = dir.join("out");
    fs::create_dir_all(&target).unwrap();
    let mut ops = MockOps::new();
    let items = [("System Folder", true, 0u64), ("System Folder\\Finder.rsrc", false, 13), ("Read Me", false, 13)];
    assert_eq!(unpack(&hfs, &target, "", &items, &mut ops), TRUE);
    assert_eq!(fs::read(target.join("Read Me")).unwrap(), b"hello classic");
    assert_eq!(fs::read(target.join("System Folder").join("Finder.rsrc")).unwrap(), b"resource fork");

    let mut v = MfsVolume::format(MfsVolume::FLOPPY_400K, "Mac 128", ImageFormat::DiskCopy42).unwrap();
    v.add_file("Read Me", *b"TEXT", *b"MACA", b"hello from 1984", &[]).unwrap();
    let mut out = std::io::Cursor::new(Vec::new());
    v.save_to(&mut out).unwrap();
    let mfs = dir.join("mac128.dsk");
    fs::write(&mfs, out.into_inner()).unwrap();
    let (r, d) = list(&mfs);
    assert_eq!(r, TRUE);
    assert_eq!(d.find("Read Me").size, 15);
    fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// sparse images
// ---------------------------------------------------------------------------

#[test]
fn sparse_image_and_bundle_list_and_unpack() {
    let dir = temp_dir();
    let mut b = HfsImageBuilder::new(b"Sparse");
    b.add_file(HFS_ROOT, b"Read Me", b"hello sparse", b"");
    let mut raw = b.build();
    raw.resize((raw.len() + 100_000).next_multiple_of(512), 0);

    let img = dir.join("disk.sparseimage");
    fs::write(&img, macfs_core::testutil::sparse_image(&raw, 16)).unwrap();
    let (r, d) = list(&img);
    assert_eq!(r, TRUE);
    assert_eq!(d.find("Read Me").size, 12);

    let bundle = dir.join("Backup.sparsebundle");
    macfs_core::testutil::write_sparsebundle(&bundle, &raw, 32 * 1024);
    // Salamander hands over a file: the bundle is opened through the Info.plist inside it
    let (r, d) = list(&bundle.join("Info.plist"));
    assert_eq!(r, TRUE);
    assert_eq!(d.find("Read Me").size, 12);

    let target = dir.join("out");
    fs::create_dir_all(&target).unwrap();
    let mut ops = MockOps::new();
    assert_eq!(unpack(&img, &target, "", &[("Read Me", false, 12)], &mut ops), TRUE);
    assert_eq!(fs::read(target.join("Read Me")).unwrap(), b"hello sparse");
    fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// PKG (installer package)
// ---------------------------------------------------------------------------

#[test]
fn pkg_lists_toc_metadata_and_installed_files_and_unpacks() {
    use macfs_core::testutil::sample_pkg;
    let dir = temp_dir();
    let pkg = dir.join("tool.pkg");
    fs::write(&pkg, sample_pkg(&vec![7u8; 30_000])).unwrap();

    let (r, d) = list(&pkg);
    assert_eq!(r, TRUE);
    d.find("!XAR-TOC.xml");
    d.find("!PKG-COMPONENTS.txt");
    assert!(d.find("Installed Files").is_dir);
    d.find("PackageInfo");
    assert_eq!(d.find("Payload").parent, "Tool.pkg");
    assert_eq!(d.find("!CPIO-INDEX.txt").parent, "Installed Files");
    assert_eq!(d.find("tool-link").parent, "Installed Files\\bin");
    assert_eq!(d.find("big.bin").size, 30_000);
    d.find("a_b_.txt"); // illegal characters replaced

    // unpack a payload file and the CPIO index below the folder "Installed Files"
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let items = [("share\\doc\\tool\\big.bin", false, 30_000), ("Installed Files\\!CPIO-INDEX.txt", false, 100)];
    let mut ops = MockOps::new();
    assert_eq!(unpack(&pkg, &out, "Installed Files", &items[..1], &mut ops), TRUE);
    assert_eq!(fs::read(out.join("share").join("doc").join("tool").join("big.bin")).unwrap(), vec![7u8; 30_000]);
    let mut ops = MockOps::new();
    assert_eq!(unpack(&pkg, &out, "", &items[1..], &mut ops), TRUE);
    let idx = fs::read_to_string(out.join("Installed Files").join("!CPIO-INDEX.txt")).unwrap();
    assert!(idx.contains("bin/tool-link -> tool"), "{idx}");
    fs::remove_dir_all(&dir).unwrap();
}
