//! The Salamander plugin objects: `CPluginInterfaceAbstract` and
//! `CPluginInterfaceForArchiverAbstract` implemented as hand-built vtables.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::ptr::{self, null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::container::{Container, Item, OpenError};
use crate::spl::*;
use crate::util;
use crate::win::{self, Answer, Kind};

pub const PLUGIN_NAME: &str = "HFS+ (read-only)";
const ABOUT_TEXT: &str = "HFS+ / HFSX read-only archiver plugin\n\nWritten in Rust on top of the hfsplus 0.4.0 crate \
(including decmpfs transparent compression).\n\nOpens disk images and raw partitions; use a *.hfsdev link file for \
physical disks (see README).";

/// `CSalamanderGeneralAbstract*` received in `SalamanderPluginEntry`.
pub static GENERAL: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

/// A panic must never unwind into Salamander.
fn guard<T>(on_panic: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(on_panic)
}

// ---------------------------------------------------------------------------
// Access to CSalamanderGeneralAbstract by vtable slot
// ---------------------------------------------------------------------------

unsafe fn general_slot<T: Copy>(slot: usize) -> Option<(*mut c_void, T)> {
    let g = GENERAL.load(Ordering::Acquire);
    if g.is_null() {
        return None;
    }
    // SAFETY: `g` is the object pointer Salamander gave us; its first word is the vtable
    // pointer and the SDK guarantees `slot` exists (slots only grow at the end).
    unsafe {
        let vt = *(g as *const *const usize);
        Some((g, std::mem::transmute_copy::<usize, T>(&*vt.add(slot))))
    }
}

type AllocFn = unsafe extern "system" fn(*mut c_void, c_int) -> *mut c_void;
type FreeFn = unsafe extern "system" fn(*mut c_void, *mut c_void);
type ParentFn = unsafe extern "system" fn(*mut c_void) -> HWND;

/// Memory on Salamander's heap (names in `CFileData` must live there).
unsafe fn sal_alloc(n: usize) -> *mut u8 {
    // SAFETY: slot 28 is `Alloc(int)`.
    match unsafe { general_slot::<AllocFn>(GENERAL_SLOT_ALLOC) } {
        Some((g, f)) => unsafe { f(g, n as c_int) as *mut u8 },
        None => null_mut(),
    }
}

unsafe fn sal_free(p: *mut u8) {
    // SAFETY: slot 29 is `Free(void*)`.
    if let Some((g, f)) = unsafe { general_slot::<FreeFn>(GENERAL_SLOT_FREE) } {
        unsafe { f(g, p as *mut c_void) }
    }
}

fn msgbox_parent() -> HWND {
    // SAFETY: slot 3 is `GetMsgBoxParent()`.
    match unsafe { general_slot::<ParentFn>(GENERAL_SLOT_GET_MSGBOX_PARENT) } {
        Some((g, f)) => unsafe { f(g) },
        None => null_mut(),
    }
}

fn show(parent: HWND, kind: Kind, text: &str) -> Answer {
    win::message_box(parent, text, PLUGIN_NAME, kind)
}

unsafe fn ansi_arg(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: Salamander passes NUL-terminated strings.
        win::from_ansi(unsafe { CStr::from_ptr(p) }.to_bytes())
    }
}

fn cstring(s: &str) -> CString {
    CString::new(win::to_ansi(s)).unwrap_or_default()
}

fn join_panel(a: &str, b: &str) -> String {
    let (a, b) = (a.trim_matches('\\'), b.trim_matches('\\'));
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a}\\{b}"),
    }
}

// ---------------------------------------------------------------------------
// CPluginInterfaceAbstract
// ---------------------------------------------------------------------------

unsafe extern "system" fn p_about(_this: *mut c_void, parent: HWND) {
    guard((), || {
        show(parent, Kind::Info, ABOUT_TEXT);
    })
}
unsafe extern "system" fn p_release(_this: *mut c_void, _parent: HWND, _force: BOOL) -> BOOL {
    TRUE
}
unsafe extern "system" fn p_noop_cfg(_this: *mut c_void, _parent: HWND, _key: *mut c_void, _reg: *mut c_void) {}
unsafe extern "system" fn p_configuration(_this: *mut c_void, _parent: HWND) {}
unsafe extern "system" fn p_connect(_this: *mut c_void, _parent: HWND, salamander: *mut Connect) {
    guard((), || {
        if salamander.is_null() {
            return;
        }
        // Salamander recognises archives by extension only (no content detection).
        // SAFETY: valid host object; AddPanelArchiver is slot 2.
        unsafe { ((*(*salamander).vtbl).AddPanelArchiver)(salamander, c"hfs;hfsdev".as_ptr(), FALSE, FALSE) }
    })
}
unsafe extern "system" fn p_release_plugin_data(_this: *mut c_void, _pd: *mut c_void) {}
unsafe extern "system" fn p_get_archiver(_this: *mut c_void) -> *mut c_void {
    &ARCHIVER_OBJ as *const Obj<ArchiverVtbl> as *mut c_void
}
unsafe extern "system" fn p_get_null(_this: *mut c_void) -> *mut c_void {
    null_mut()
}
unsafe extern "system" fn p_event(_this: *mut c_void, _event: c_int, _param: u32) {}
unsafe extern "system" fn p_clear_history(_this: *mut c_void, _parent: HWND) {}
unsafe extern "system" fn p_path_change(_this: *mut c_void, _path: *const c_char, _sub: BOOL) {}
unsafe extern "system" fn p_password_event(_this: *mut c_void, _parent: HWND, _event: c_int) {}

pub static PLUGIN_VTBL: PluginVtbl = PluginVtbl {
    About: p_about,
    Release: p_release,
    LoadConfiguration: p_noop_cfg,
    SaveConfiguration: p_noop_cfg,
    Configuration: p_configuration,
    Connect: p_connect,
    ReleasePluginDataInterface: p_release_plugin_data,
    GetInterfaceForArchiver: p_get_archiver,
    GetInterfaceForViewer: p_get_null,
    GetInterfaceForMenuExt: p_get_null,
    GetInterfaceForFS: p_get_null,
    GetInterfaceForThumbLoader: p_get_null,
    Event: p_event,
    ClearHistory: p_clear_history,
    AcceptChangeOnPathNotification: p_path_change,
    PasswordManagerEvent: p_password_event,
};
pub static PLUGIN_OBJ: Obj<PluginVtbl> = Obj { vtbl: &PLUGIN_VTBL };

// ---------------------------------------------------------------------------
// CPluginInterfaceForArchiverAbstract
// ---------------------------------------------------------------------------

unsafe fn ops_call<R>(ops: *mut Ops, f: impl FnOnce(&OpsVtbl) -> R) -> R {
    // SAFETY: `ops` is the host object passed into the archiver method.
    f(unsafe { &*(*ops).vtbl })
}

fn open_container(arc: &str, parent: HWND) -> Option<Container> {
    match Container::open(Path::new(arc)) {
        Ok(c) => Some(c),
        Err(e) => {
            let hint = if matches!(e, OpenError::NoVolume) {
                "\n\nSalamander opens files by extension; the file must contain an HFS+/HFSX volume \
                 (directly, or on a GPT / Apple partition map / MBR disk)."
            } else {
                ""
            };
            show(parent, Kind::Error, &format!("{arc}\n\n{e}{hint}"));
            None
        }
    }
}

const VALID: u32 = VALID_DATA_EXTENSION
    | VALID_DATA_SIZE
    | VALID_DATA_TYPE
    | VALID_DATA_DATE
    | VALID_DATA_TIME
    | VALID_DATA_ATTRIBUTES
    | VALID_DATA_ISLINK;

/// Add one item to Salamander's directory object. `parent` is the panel path of its directory.
unsafe fn add_item(dir: *mut Dir, parent: &CStr, it: &Item) -> bool {
    let name = win::to_ansi(&it.disp);
    // SAFETY: Alloc returns `len + 1` writable bytes (or NULL).
    let buf = unsafe { sal_alloc(name.len() + 1) };
    if buf.is_null() {
        return false;
    }
    let ext_off = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if !it.is_dir => i + 1,
        _ => name.len(),
    };
    // SAFETY: `buf` has name.len() + 1 bytes.
    unsafe {
        ptr::copy_nonoverlapping(name.as_ptr(), buf, name.len());
        *buf.add(name.len()) = 0;
    }
    let mut fd = FileData {
        Name: buf as *mut c_char,
        // SAFETY: ext_off <= name.len() (points at the terminator at most).
        Ext: unsafe { buf.add(ext_off) } as *mut c_char,
        Size: QuadWord::new(it.size),
        Attr: if it.is_dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_ARCHIVE },
        LastWrite: filetime_from_unix(it.mtime),
        DosName: null_mut(),
        PluginData: 0,
        Bits: FileData::bits(name.len(), it.is_link),
    };
    // SAFETY: valid host object, valid FileData; on failure we own (and free) the name.
    let ok = unsafe {
        let vt = &*(*dir).vtbl;
        if it.is_dir {
            (vt.AddDir)(dir, parent.as_ptr(), &mut fd, null_mut())
        } else {
            (vt.AddFile)(dir, parent.as_ptr(), &mut fd, null_mut())
        }
    };
    if ok == 0 {
        unsafe { sal_free(buf) };
        return false;
    }
    true
}

unsafe extern "system" fn a_list(
    _this: *mut c_void,
    _ops: *mut Ops,
    file_name: *const c_char,
    dir: *mut Dir,
    plugin_data: *mut *mut c_void,
) -> BOOL {
    guard(FALSE, || {
        // SAFETY: host-provided pointers.
        unsafe {
            if !plugin_data.is_null() {
                *plugin_data = null_mut(); // no custom columns
            }
            let arc = ansi_arg(file_name);
            let parent = msgbox_parent();
            let Some(mut c) = open_container(&arc, parent) else { return FALSE };

            let clear = |dir: *mut Dir| ((*(*dir).vtbl).Clear)(dir, null_mut());
            ((*(*dir).vtbl).SetValidData)(dir, VALID);

            let mut stack = vec![(c.root(), String::new())];
            let mut first = true;
            let mut skipped: Vec<String> = Vec::new();
            while let Some((node, prefix)) = stack.pop() {
                let items = match c.list_uncached(&node) {
                    Ok(i) => i,
                    Err(e) if first => {
                        show(parent, Kind::Error, &format!("{arc}\n\ncannot read the volume: {e}"));
                        clear(dir);
                        return FALSE;
                    }
                    Err(e) => {
                        skipped.push(format!("\\{}: {e}", prefix));
                        continue;
                    }
                };
                first = false;
                let parent_path = cstring(&prefix);
                for it in &items {
                    if !add_item(dir, &parent_path, it) {
                        clear(dir);
                        return FALSE;
                    }
                    if it.is_dir {
                        stack.push((it.node(), join_panel(&prefix, &it.disp)));
                    }
                }
            }
            if !skipped.is_empty() {
                let mut text = format!(
                    "{} director{} could not be read and {} skipped:\n",
                    skipped.len(),
                    if skipped.len() == 1 { "y" } else { "ies" },
                    if skipped.len() == 1 { "was" } else { "were" }
                );
                for line in skipped.iter().take(5) {
                    text.push_str(&format!("\n{line}"));
                }
                if skipped.len() > 5 {
                    text.push_str(&format!("\n... and {} more", skipped.len() - 5));
                }
                show(parent, Kind::Warning, &text);
            }
            TRUE
        }
    })
}

// ---- extraction -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Overwrite {
    Ask,
    All,
    SkipAll,
}

#[derive(PartialEq, Eq, Debug)]
enum Outcome {
    Done,
    Skipped,
    Cancelled,
}

enum Fail {
    Cancelled,
    Error(String),
}

struct Extractor {
    c: Container,
    ops: *mut Ops,
    overwrite: Overwrite,
    skip_errors: bool,
    /// Single-file mode (view/edit): errors are reported once and abort.
    single: bool,
}

/// Writer that feeds the destination file and reports progress to Salamander.
struct ProgressWriter {
    out: BufWriter<File>,
    ops: *mut Ops,
    aborted: bool,
}

impl Write for ProgressWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(1 << 20);
        self.out.write_all(&buf[..n])?;
        // SAFETY: `ops` is the host object of the running operation.
        let go = unsafe { ops_call(self.ops, |v| (v.ProgressAddSize)(self.ops, n as c_int, TRUE)) };
        if go == 0 {
            self.aborted = true;
            return Err(io::Error::other("cancelled"));
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl Extractor {
    fn dialog_parent(&self) -> HWND {
        // SAFETY: host object; the progress dialog is open while extracting.
        let h = unsafe { ops_call(self.ops, |v| (v.ProgressGetHWND)(self.ops)) };
        if h.is_null() { msgbox_parent() } else { h }
    }

    fn add_progress(&self, n: u64) -> bool {
        // SAFETY: host object. Sizes are added in int-sized steps.
        unsafe {
            let mut left = n;
            while left > 0 {
                let step = left.min(1 << 30) as c_int;
                if ops_call(self.ops, |v| (v.ProgressAddSize)(self.ops, step, TRUE)) == 0 {
                    return false;
                }
                left -= step as u64;
            }
        }
        true
    }

    fn write_file(&mut self, it: &Item, dest: &Path) -> Result<(), Fail> {
        let file = File::create(dest).map_err(|e| Fail::Error(format!("{}: {e}", dest.display())))?;
        let mut w = ProgressWriter { out: BufWriter::with_capacity(1 << 20, file), ops: self.ops, aborted: false };
        let read = self.c.read_file(it, &mut w);
        let flushed = w.out.flush();
        let aborted = w.aborted;
        let result = match read {
            Err(_) if aborted => Err(Fail::Cancelled),
            Err(e) => Err(Fail::Error(e.to_string())),
            Ok(_) => flushed.map_err(|e| Fail::Error(format!("write error: {e}"))),
        };
        match result {
            Ok(()) => {
                if let (Some(t), Ok(file)) = (it.mtime, w.out.into_inner()) {
                    let _ = file.set_modified(util::unix_to_system_time(t));
                }
                Ok(())
            }
            Err(f) => {
                drop(w);
                let _ = fs::remove_file(dest); // never leave a truncated file behind
                Err(f)
            }
        }
    }

    /// Extract one file item; handles overwrite questions and errors.
    fn extract(&mut self, it: &Item, shown: &str, dest: PathBuf) -> Outcome {
        let dest = util::long_path(dest);
        let name = cstring(shown);
        // SAFETY: host object, NUL-terminated string.
        unsafe {
            ops_call(self.ops, |v| {
                (v.ProgressDialogAddText)(self.ops, name.as_ptr(), TRUE);
                (v.ProgressSetSize)(self.ops, &QuadWord::new(0), &QuadWord::MAX, TRUE);
                (v.ProgressSetTotalSize)(self.ops, &QuadWord::new(it.size + 1), &QuadWord::MAX);
            })
        };

        if dest.exists() {
            if self.overwrite == Overwrite::Ask {
                let text = format!(
                    "The file already exists:\n{}\n\nYes = overwrite it and all other existing files\nNo = skip all existing files\nCancel = abort",
                    dest.display()
                );
                match show(self.dialog_parent(), Kind::YesNoCancel, &text) {
                    Answer::Yes => self.overwrite = Overwrite::All,
                    Answer::No => self.overwrite = Overwrite::SkipAll,
                    _ => return Outcome::Cancelled,
                }
            }
            if self.overwrite == Overwrite::SkipAll {
                return if self.add_progress(it.size + 1) { Outcome::Skipped } else { Outcome::Cancelled };
            }
        }
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }

        match self.write_file(it, &dest) {
            Ok(()) => {
                if self.add_progress(1) { Outcome::Done } else { Outcome::Cancelled }
            }
            Err(Fail::Cancelled) => Outcome::Cancelled,
            Err(Fail::Error(msg)) => self.on_error(shown, &msg),
        }
    }

    fn on_error(&mut self, shown: &str, msg: &str) -> Outcome {
        if self.single {
            show(self.dialog_parent(), Kind::Error, &format!("Cannot unpack '{shown}':\n{msg}"));
            return Outcome::Cancelled;
        }
        if self.skip_errors {
            return Outcome::Skipped;
        }
        let text = format!("Cannot unpack '{shown}':\n{msg}\n\nYes = skip this file\nNo = skip all files with errors\nCancel = abort");
        match show(self.dialog_parent(), Kind::YesNoCancel, &text) {
            Answer::Yes => Outcome::Skipped,
            Answer::No => {
                self.skip_errors = true;
                Outcome::Skipped
            }
            _ => Outcome::Cancelled,
        }
    }
}

unsafe extern "system" fn a_unpack_archive(
    _this: *mut c_void,
    ops: *mut Ops,
    file_name: *const c_char,
    _plugin_data: *mut c_void,
    target_dir: *const c_char,
    archive_root: *const c_char,
    next: EnumSelection,
    next_param: *mut c_void,
) -> BOOL {
    guard(FALSE, || {
        // SAFETY: host-provided pointers and callbacks.
        unsafe {
            let arc = ansi_arg(file_name);
            let target = ansi_arg(target_dir);
            let root = ansi_arg(archive_root);
            let parent = msgbox_parent();
            let Some(c) = open_container(&arc, parent) else { return FALSE };

            // pass 1: total size for the progress bar
            let (mut is_dir, mut size, mut err) = (FALSE, QuadWord::default(), SALENUM_SUCCESS);
            let mut total = 0u64;
            while !next(parent, 1, &mut is_dir, &mut size, null_mut(), next_param, &mut err).is_null() {
                total += size.value() + 1;
            }
            if err == SALENUM_CANCEL {
                return FALSE;
            }

            ops_call(ops, |v| {
                (v.OpenProgressDialog)(ops, c"Unpacking HFS+ volume".as_ptr(), TRUE, null_mut(), FALSE);
                (v.ProgressSetTotalSize)(ops, &QuadWord::new(0), &QuadWord::new(total));
            });

            let mut ex = Extractor { c, ops, overwrite: Overwrite::Ask, skip_errors: false, single: false };
            let root_depth = root.split('\\').filter(|s| !s.is_empty()).count();
            let mut ok = true;

            next(null_mut(), -1, null_mut(), null_mut(), null_mut(), next_param, null_mut());
            loop {
                let p = next(null_mut(), 1, &mut is_dir, &mut size, null_mut(), next_param, null_mut());
                if p.is_null() {
                    break;
                }
                let rel = win::from_ansi(CStr::from_ptr(p).to_bytes());
                let full = join_panel(&root, &rel);
                let Some(res) = ex.c.resolve(&full) else {
                    if ex.on_error(&rel, "the item was not found in the volume") == Outcome::Cancelled {
                        ok = false;
                        break;
                    }
                    continue;
                };
                let Some(item) = res.item else { continue };
                let mut dest = PathBuf::from(&target);
                for comp in res.real_rel.iter().skip(root_depth) {
                    dest.push(comp);
                }
                if item.is_dir {
                    let _ = fs::create_dir_all(util::long_path(dest));
                    if !ex.add_progress(1) {
                        ok = false;
                        break;
                    }
                } else if ex.extract(&item, &rel, dest) == Outcome::Cancelled {
                    ok = false;
                    break;
                }
            }
            ops_call(ops, |v| (v.CloseProgressDialog)(ops));
            if ok { TRUE } else { FALSE }
        }
    })
}

unsafe extern "system" fn a_unpack_one(
    _this: *mut c_void,
    ops: *mut Ops,
    file_name: *const c_char,
    _plugin_data: *mut c_void,
    name_in_archive: *const c_char,
    _file_data: *const FileData,
    target_dir: *const c_char,
    new_file_name: *const c_char,
    _renaming_not_supported: *mut BOOL,
) -> BOOL {
    guard(FALSE, || {
        // SAFETY: host-provided pointers.
        unsafe {
            let arc = ansi_arg(file_name);
            let target = ansi_arg(target_dir);
            let name = ansi_arg(name_in_archive);
            let parent = msgbox_parent();
            let Some(mut c) = open_container(&arc, parent) else { return FALSE };

            let Some(item) = c.resolve(&name).and_then(|r| r.item).filter(|i| !i.is_dir) else {
                show(parent, Kind::Error, &format!("'{name}' was not found in the volume."));
                return FALSE;
            };
            // Salamander looks for the file under the name it knows: the panel (ANSI) name,
            // or `newFileName` when it asked for a rename.
            let file = if new_file_name.is_null() {
                name.rsplit('\\').next().unwrap_or(&name).to_string()
            } else {
                ansi_arg(new_file_name)
            };
            let dest = Path::new(&target).join(&file);

            ops_call(ops, |v| {
                (v.OpenProgressDialog)(ops, c"Unpacking HFS+ volume".as_ptr(), FALSE, null_mut(), FALSE);
                (v.ProgressSetTotalSize)(ops, &QuadWord::new(item.size + 1), &QuadWord::MAX);
            });
            let mut ex = Extractor { c, ops, overwrite: Overwrite::All, skip_errors: false, single: true };
            let outcome = ex.extract(&item, &name, dest.clone());
            ops_call(ops, |v| (v.CloseProgressDialog)(ops));
            if outcome == Outcome::Done { TRUE } else { FALSE }
        }
    })
}

unsafe extern "system" fn a_pack(
    _t: *mut c_void,
    _o: *mut Ops,
    _f: *const c_char,
    _r: *const c_char,
    _m: BOOL,
    _s: *const c_char,
    _n: *const c_void,
    _p: *mut c_void,
) -> BOOL {
    FALSE // read-only plugin
}
unsafe extern "system" fn a_delete(
    _t: *mut c_void,
    _o: *mut Ops,
    _f: *const c_char,
    _pd: *mut c_void,
    _r: *const c_char,
    _n: EnumSelection,
    _p: *mut c_void,
) -> BOOL {
    FALSE
}
unsafe extern "system" fn a_unpack_whole(
    _t: *mut c_void,
    _o: *mut Ops,
    _f: *const c_char,
    _m: *const c_char,
    _d: *const c_char,
    _del: BOOL,
    _v: *mut c_void,
) -> BOOL {
    FALSE // "custom unpacker" function is not registered
}
unsafe extern "system" fn a_can_close(_t: *mut c_void, _o: *mut Ops, _f: *const c_char, _force: BOOL, _panel: c_int) -> BOOL {
    TRUE
}
unsafe extern "system" fn a_cache_info(_t: *mut c_void, _tmp: *mut c_char, _own: *mut BOOL, _cache: *mut BOOL) -> BOOL {
    FALSE // default temp-copy handling
}
unsafe extern "system" fn a_delete_tmp(_t: *mut c_void, _f: *const c_char, _first: BOOL) {}
unsafe extern "system" fn a_premature(_t: *mut c_void, _parent: HWND, _n: c_int) -> BOOL {
    FALSE
}

pub static ARCHIVER_VTBL: ArchiverVtbl = ArchiverVtbl {
    ListArchive: a_list,
    UnpackArchive: a_unpack_archive,
    UnpackOneFile: a_unpack_one,
    PackToArchive: a_pack,
    DeleteFromArchive: a_delete,
    UnpackWholeArchive: a_unpack_whole,
    CanCloseArchive: a_can_close,
    GetCacheInfo: a_cache_info,
    DeleteTmpCopy: a_delete_tmp,
    PrematureDeleteTmpCopy: a_premature,
};
pub static ARCHIVER_OBJ: Obj<ArchiverVtbl> = Obj { vtbl: &ARCHIVER_VTBL };

// ---------------------------------------------------------------------------
// SalamanderPluginEntry
// ---------------------------------------------------------------------------

pub unsafe fn plugin_entry(entry: *mut Entry) -> *mut c_void {
    guard(null_mut(), || {
        if entry.is_null() {
            return null_mut();
        }
        // SAFETY: `entry` is the host's CSalamanderPluginEntryAbstract.
        unsafe {
            let vt = &*(*entry).vtbl;
            let parent = (vt.GetParentWindow)(entry);
            if (vt.GetVersion)(entry) < REQ_VERSION {
                show(parent, Kind::Error, "This plugin requires Altap Salamander 4.0 or Open Salamander 5.0 (x64) or later.");
                return null_mut();
            }
            GENERAL.store((vt.GetSalamanderGeneral)(entry), Ordering::Release);
            let ok = (vt.SetBasicPluginData)(
                entry,
                c"HFS+ (read-only)".as_ptr(),
                FUNCTION_PANELARCHIVERVIEW,
                c"0.1.0".as_ptr(),
                c"MIT licensed; hfsplus crate by Dil4rd".as_ptr(),
                c"Opens HFS+/HFSX disk images and partitions (read-only), including decmpfs compressed files.".as_ptr(),
                c"HFSPLUS".as_ptr(),
                c"hfs;hfsdev".as_ptr(),
                null(),
            );
            if ok == 0 {
                return null_mut();
            }
            &PLUGIN_OBJ as *const Obj<PluginVtbl> as *mut c_void
        }
    })
}
