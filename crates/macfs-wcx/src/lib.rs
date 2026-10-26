//! Total Commander WCX (packer) plugin for macOS file systems and archives: HFS+/HFSX volumes and APFS
//! containers, also inside UDIF disk images (DMG), and installer packages (PKG). Everything is read-only.
//!
//! Built as a 64-bit DLL and renamed to `*.wcx64`. Only the Unicode (`...W`)
//! entry points are implemented; the ANSI ones exist solely so Total Commander
//! recognises the DLL as a packer plugin and report "not supported".
#![allow(clippy::missing_safety_doc)] // the exports follow the WCX C ABI; contracts are in wcxhead.h

pub mod archive;
pub mod ffi;

// sdílené moduly z macfs-core (zpřístupněné pod původními cestami)
pub use macfs_core::{detect, source, udif, util, volume};

use std::ffi::{c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use archive::Archive;
use ffi::*;

/// Callback registered before any archive exists (some TC versions call
/// `SetProcessDataProcW` with a NULL handle).
static GLOBAL_PROGRESS: AtomicUsize = AtomicUsize::new(0);

/// A panic must never unwind into Total Commander.
fn guard<T>(on_panic: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(on_panic)
}

/// # Safety
/// `h` must be null or a handle produced by `OpenArchiveW` and not yet closed.
unsafe fn archive_mut<'a>(h: Handle) -> Option<&'a mut Archive> {
    // SAFETY: guaranteed by the caller.
    unsafe { (h as *mut Archive).as_mut() }
}

// ---------------------------------------------------------------------------
// Required exports
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "system" fn OpenArchiveW(data: *mut OpenArchiveDataW) -> Handle {
    guard(ptr::null_mut(), || {
        // SAFETY: TC passes a valid tOpenArchiveDataW.
        let Some(d) = (unsafe { data.as_mut() }) else { return ptr::null_mut() };
        if d.ArcName.is_null() {
            d.OpenResult = E_EOPEN;
            return ptr::null_mut();
        }
        // SAFETY: ArcName is a NUL-terminated wide string.
        let path = util::wide_to_path(unsafe { wide_slice(d.ArcName) });
        match Archive::open(&path) {
            Ok(mut a) => {
                let cb = GLOBAL_PROGRESS.load(Ordering::Relaxed);
                if cb != 0 {
                    // SAFETY: the value was stored from a valid ProcessDataProcW.
                    a.progress = Some(unsafe { std::mem::transmute::<usize, ProcessDataProcW>(cb) });
                }
                d.OpenResult = E_OK;
                Box::into_raw(Box::new(a)) as Handle
            }
            Err(e) => {
                d.OpenResult = e.code();
                ptr::null_mut()
            }
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ReadHeaderExW(h: Handle, out: *mut HeaderDataExW) -> c_int {
    guard(E_BAD_DATA, || {
        // SAFETY: `h` comes from OpenArchiveW; `out` is a valid tHeaderDataExW.
        let (Some(a), false) = (unsafe { archive_mut(h) }, out.is_null()) else {
            return E_BAD_DATA;
        };
        match a.next_header() {
            Ok(Some(e)) => {
                // SAFETY: `out` is valid and writable; all-zero is a valid bit pattern.
                unsafe {
                    ptr::write_bytes(out, 0, 1);
                    let hd = &mut *out;
                    copy_to_wide(&mut hd.ArcName, &a.arc_name);
                    copy_to_wide(&mut hd.FileName, &e.disp);
                    hd.UnpSize = e.size as u32;
                    hd.UnpSizeHigh = (e.size >> 32) as u32;
                    hd.PackSize = hd.UnpSize;
                    hd.PackSizeHigh = hd.UnpSizeHigh;
                    hd.FileTime = util::dos_time(e.mtime);
                    hd.FileAttr = if e.is_dir { ATTR_DIRECTORY } else { 0 };
                }
                E_OK
            }
            Ok(None) => E_END_ARCHIVE,
            Err(_) => E_BAD_DATA,
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ProcessFileW(
    h: Handle,
    operation: c_int,
    dest_path: *const u16,
    dest_name: *const u16,
) -> c_int {
    guard(E_BAD_DATA, || {
        // SAFETY: `h` comes from OpenArchiveW.
        let Some(a) = (unsafe { archive_mut(h) }) else { return E_BAD_DATA };
        // DestPath may be NULL (then DestName is the full path) or the target folder.
        let dest: Option<PathBuf> = if operation == PK_EXTRACT {
            // SAFETY: non-null pointers are NUL-terminated wide strings.
            let name = (!dest_name.is_null()).then(|| unsafe { wide_slice(dest_name) });
            let dir = (!dest_path.is_null()).then(|| unsafe { wide_slice(dest_path) });
            match (dir, name) {
                (Some(d), Some(n)) => {
                    let mut full = d.to_vec();
                    let sep = u16::from(b'\\');
                    if full.last() != Some(&sep) && n.first() != Some(&sep) {
                        full.push(sep);
                    }
                    full.extend_from_slice(n);
                    Some(util::wide_to_path(&full))
                }
                (None, Some(n)) => Some(util::wide_to_path(n)),
                (Some(d), None) => Some(util::wide_to_path(d)),
                (None, None) => None,
            }
        } else {
            None
        };
        a.process(operation, dest.as_deref())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn CloseArchive(h: Handle) -> c_int {
    guard(E_ECLOSE, || {
        if !h.is_null() {
            // SAFETY: the handle was created by Box::into_raw in OpenArchiveW and is closed once.
            drop(unsafe { Box::from_raw(h as *mut Archive) });
        }
        E_OK
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SetChangeVolProcW(_h: Handle, _proc: Option<ChangeVolProcW>) {}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SetProcessDataProcW(h: Handle, proc: Option<ProcessDataProcW>) {
    guard((), || {
        GLOBAL_PROGRESS.store(proc.map_or(0, |p| p as usize), Ordering::Relaxed);
        // SAFETY: `h` is null or comes from OpenArchiveW.
        if let Some(a) = unsafe { archive_mut(h) } {
            a.progress = proc;
        }
    })
}

/// Lets Total Commander ask "is this one of yours?" for files whose extension
/// is associated with the plugin.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn CanYouHandleThisFileW(file_name: *const u16) -> c_int {
    guard(0, || {
        if file_name.is_null() {
            return 0;
        }
        // SAFETY: NUL-terminated wide string.
        let path = util::wide_to_path(unsafe { wide_slice(file_name) });
        if detect::is_devlink(&path) {
            // Accept always: opening reports the real problem (admin rights, bad device...).
            return 1;
        }
        match detect::locate(&path) {
            Ok((_, parts)) => c_int::from(!parts.is_empty()),
            Err(_) => 0,
        }
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn GetPackerCaps() -> c_int {
    PK_CAPS_BY_CONTENT
}

// ---------------------------------------------------------------------------
// ANSI stubs: exported only so TC recognises the DLL; the W versions do the work.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "system" fn OpenArchive(data: *mut OpenArchiveDataA) -> Handle {
    // SAFETY: TC passes a valid tOpenArchiveData (or null).
    if let Some(d) = unsafe { data.as_mut() } {
        d.OpenResult = E_NOT_SUPPORTED;
    }
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ReadHeader(_h: Handle, _data: *mut c_void) -> c_int {
    E_NOT_SUPPORTED
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ProcessFile(
    _h: Handle,
    _operation: c_int,
    _dest_path: *mut c_char,
    _dest_name: *mut c_char,
) -> c_int {
    E_NOT_SUPPORTED
}
