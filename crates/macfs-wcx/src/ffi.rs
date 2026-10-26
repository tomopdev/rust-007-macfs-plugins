//! Raw WCX (Total Commander packer plugin) ABI: structs, constants, callbacks.
//!
//! Layouts follow `wcxhead.h`. They are `#[repr(C)]`, so on x64 they have the
//! same natural alignment as the C compiler gives them.
#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::{c_char, c_int, c_uint, c_void};

/// Opaque archive handle handed to Total Commander.
pub type Handle = *mut c_void;

// ---- error codes returned by the plugin -----------------------------------
pub const E_OK: c_int = 0;
pub const E_END_ARCHIVE: c_int = 10;
pub const E_NO_MEMORY: c_int = 11;
pub const E_BAD_DATA: c_int = 12;
pub const E_BAD_ARCHIVE: c_int = 13;
pub const E_UNKNOWN_FORMAT: c_int = 14;
pub const E_EOPEN: c_int = 15;
pub const E_ECREATE: c_int = 16;
pub const E_ECLOSE: c_int = 17;
pub const E_EREAD: c_int = 18;
pub const E_EWRITE: c_int = 19;
pub const E_SMALL_BUF: c_int = 20;
pub const E_EABORTED: c_int = 21;
pub const E_NO_FILES: c_int = 22;
pub const E_TOO_MANY_FILES: c_int = 23;
pub const E_NOT_SUPPORTED: c_int = 24;

// ---- ProcessFile operations ------------------------------------------------
pub const PK_SKIP: c_int = 0;
pub const PK_TEST: c_int = 1;
pub const PK_EXTRACT: c_int = 2;

// ---- OpenArchive modes -----------------------------------------------------
pub const PK_OM_LIST: c_int = 0;
pub const PK_OM_EXTRACT: c_int = 1;

// ---- GetPackerCaps flags ---------------------------------------------------
/// Plugin can detect its archives by content (`CanYouHandleThisFile*` is called).
pub const PK_CAPS_BY_CONTENT: c_int = 64;

// ---- FileAttr bits (Windows attributes) --------------------------------------
pub const ATTR_DIRECTORY: c_int = 0x10;

/// `int __stdcall ProcessDataProcW(WCHAR *FileName, int Size)`; returns 0 to abort.
pub type ProcessDataProcW = unsafe extern "system" fn(file_name: *mut u16, size: c_int) -> c_int;
/// `int __stdcall ChangeVolProcW(WCHAR *ArcName, int Mode)`.
pub type ChangeVolProcW = unsafe extern "system" fn(arc_name: *mut u16, mode: c_int) -> c_int;

/// `tOpenArchiveDataW`
#[repr(C)]
pub struct OpenArchiveDataW {
    pub ArcName: *mut u16,
    pub OpenMode: c_int,
    pub OpenResult: c_int,
    pub CmtBuf: *mut u16,
    pub CmtBufSize: c_int,
    pub CmtSize: c_int,
    pub CmtState: c_int,
}

/// `tOpenArchiveData` (ANSI) – only used by the "not supported" stub.
#[repr(C)]
pub struct OpenArchiveDataA {
    pub ArcName: *mut c_char,
    pub OpenMode: c_int,
    pub OpenResult: c_int,
    pub CmtBuf: *mut c_char,
    pub CmtBufSize: c_int,
    pub CmtSize: c_int,
    pub CmtState: c_int,
}

/// `tHeaderDataExW`
#[repr(C)]
pub struct HeaderDataExW {
    pub ArcName: [u16; 1024],
    pub FileName: [u16; 1024],
    pub Flags: c_int,
    pub PackSize: c_uint,
    pub PackSizeHigh: c_uint,
    pub UnpSize: c_uint,
    pub UnpSizeHigh: c_uint,
    pub HostOS: c_int,
    pub FileCRC: c_int,
    pub FileTime: c_int,
    pub UnpVer: c_int,
    pub Method: c_int,
    pub FileAttr: c_int,
    pub CmtBuf: *mut c_char,
    pub CmtBufSize: c_int,
    pub CmtSize: c_int,
    pub CmtState: c_int,
    pub Reserved: [c_char; 1024],
}

// ---- helpers ---------------------------------------------------------------

/// Copy `s` into a fixed UTF-16 buffer, truncating and always NUL-terminating.
pub fn copy_to_wide(dst: &mut [u16], s: &str) {
    if dst.is_empty() {
        return;
    }
    let mut i = 0;
    for unit in s.encode_utf16() {
        if i + 1 >= dst.len() {
            break;
        }
        dst[i] = unit;
        i += 1;
    }
    dst[i] = 0;
}

/// Borrow a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must be non-null and point to a NUL-terminated UTF-16 string that stays
/// valid for the returned lifetime.
pub unsafe fn wide_slice<'a>(p: *const u16) -> &'a [u16] {
    let mut len = 0usize;
    // SAFETY: guaranteed by the caller (NUL-terminated).
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        std::slice::from_raw_parts(p, len)
    }
}

/// NUL-terminated UTF-16 copy of `s`.
pub fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}
