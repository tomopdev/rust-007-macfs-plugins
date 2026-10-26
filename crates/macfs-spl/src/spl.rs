//! Salamander plugin ABI (SDK `spl_base.h`, `spl_com.h`, `spl_arc.h`).
//!
//! The SDK interfaces are C++ abstract classes: an object is a pointer to a
//! pointer to a table of function pointers (one slot per `virtual` method, in
//! declaration order, no destructor). Structures use `#pragma pack(4)`.
//! The slot numbers below were taken from the SDK headers of Open Salamander 5.0
//! (plugin SDK version 103); vtables only ever grow at the end, so they are also
//! valid for Altap Salamander 4.0 (SDK version 102).
#![allow(non_snake_case, non_camel_case_types, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_void};

pub type BOOL = c_int;
pub type HWND = *mut c_void;
pub const TRUE: BOOL = 1;
pub const FALSE: BOOL = 0;

/// Oldest SDK version the plugin works with (Altap Salamander 4.0).
pub const REQ_VERSION: c_int = 102;
/// SDK version the plugin interfaces were written against (Open Salamander 5.0).
pub const SDK_VERSION: c_int = 103;

// ---- SetBasicPluginData `functions` ---------------------------------------
pub const FUNCTION_PANELARCHIVERVIEW: u32 = 0x0001;

// ---- CFileData validity mask -----------------------------------------------
pub const VALID_DATA_EXTENSION: u32 = 0x0001;
pub const VALID_DATA_SIZE: u32 = 0x0004;
pub const VALID_DATA_TYPE: u32 = 0x0008;
pub const VALID_DATA_DATE: u32 = 0x0010;
pub const VALID_DATA_TIME: u32 = 0x0020;
pub const VALID_DATA_ATTRIBUTES: u32 = 0x0040;
pub const VALID_DATA_ISLINK: u32 = 0x0100;

// ---- SalEnumSelection error codes -----------------------------------------
pub const SALENUM_SUCCESS: c_int = 0;
pub const SALENUM_CANCEL: c_int = 2;

pub const ICONOVERLAYINDEX_NOTUSED: u32 = 15;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;

/// `CQuadWord` (two DWORDs; also usable as a `unsigned __int64`).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct QuadWord {
    pub lo: u32,
    pub hi: u32,
}

impl QuadWord {
    pub const MAX: QuadWord = QuadWord { lo: u32::MAX, hi: u32::MAX };
    pub fn new(v: u64) -> Self {
        QuadWord { lo: v as u32, hi: (v >> 32) as u32 }
    }
    pub fn value(self) -> u64 {
        (self.hi as u64) << 32 | self.lo as u64
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FileTime {
    pub low: u32,
    pub high: u32,
}

/// `CFileData` (`#pragma pack(4)`). `Bits` holds the bit fields:
/// NameLen:9, Hidden:1, IsLink:1, IsOffline:1, IconOverlayIndex:4, then 8 flags
/// reserved for Salamander (all zero).
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct FileData {
    pub Name: *mut c_char,
    pub Ext: *mut c_char,
    pub Size: QuadWord,
    pub Attr: u32,
    pub LastWrite: FileTime,
    pub DosName: *mut c_char,
    pub PluginData: usize,
    pub Bits: u32,
}

impl FileData {
    pub fn bits(name_len: usize, is_link: bool) -> u32 {
        (name_len as u32 & 0x1FF) | ((is_link as u32) << 10) | (ICONOVERLAYINDEX_NOTUSED << 12)
    }
    pub fn name_len(&self) -> usize {
        (self.Bits & 0x1FF) as usize
    }
}

/// Unix seconds (UTC) -> Windows FILETIME.
pub fn filetime_from_unix(unix: Option<i64>) -> FileTime {
    let Some(u) = unix else { return FileTime::default() };
    let ticks = (u + 11_644_473_600).max(0) as u64 * 10_000_000;
    FileTime { low: ticks as u32, high: (ticks >> 32) as u32 }
}

// ---------------------------------------------------------------------------
// Host objects (implemented by Salamander, called by the plugin)
// ---------------------------------------------------------------------------

/// `CSalamanderPluginEntryAbstract`
#[repr(C)]
pub struct EntryVtbl {
    pub GetVersion: unsafe extern "system" fn(*mut Entry) -> c_int,
    pub GetParentWindow: unsafe extern "system" fn(*mut Entry) -> HWND,
    pub GetSalamanderDebug: *const c_void,
    pub SetBasicPluginData: unsafe extern "system" fn(
        this: *mut Entry,
        plugin_name: *const c_char,
        functions: u32,
        version: *const c_char,
        copyright: *const c_char,
        description: *const c_char,
        reg_key_name: *const c_char,
        extensions: *const c_char,
        fs_name: *const c_char,
    ) -> BOOL,
    pub GetSalamanderGeneral: unsafe extern "system" fn(*mut Entry) -> *mut c_void,
}

// SAFETY: vtables are immutable; the placeholder slots are never dereferenced.
unsafe impl Sync for EntryVtbl {}
unsafe impl Sync for ConnectVtbl {}

#[repr(C)]
pub struct Entry {
    pub vtbl: *const EntryVtbl,
}

/// `CSalamanderConnectAbstract` (only the slots the plugin uses).
#[repr(C)]
pub struct ConnectVtbl {
    pub AddCustomPacker: *const c_void,
    pub AddCustomUnpacker: *const c_void,
    pub AddPanelArchiver: unsafe extern "system" fn(this: *mut Connect, extensions: *const c_char, edit: BOOL, update_exts: BOOL),
}

#[repr(C)]
pub struct Connect {
    pub vtbl: *const ConnectVtbl,
}

/// `CSalamanderDirectoryAbstract` (only the slots the plugin uses).
#[repr(C)]
pub struct DirVtbl {
    pub Clear: unsafe extern "system" fn(this: *mut Dir, plugin_data: *mut c_void),
    pub SetValidData: unsafe extern "system" fn(this: *mut Dir, valid: u32),
    pub SetFlags: unsafe extern "system" fn(this: *mut Dir, flags: u32),
    pub AddFile: unsafe extern "system" fn(this: *mut Dir, path: *const c_char, file: *mut FileData, plugin_data: *mut c_void) -> BOOL,
    pub AddDir: unsafe extern "system" fn(this: *mut Dir, path: *const c_char, dir: *mut FileData, plugin_data: *mut c_void) -> BOOL,
}

#[repr(C)]
pub struct Dir {
    pub vtbl: *const DirVtbl,
}

/// `CSalamanderForOperationsAbstract`
#[repr(C)]
pub struct OpsVtbl {
    pub OpenProgressDialog: unsafe extern "system" fn(this: *mut Ops, title: *const c_char, two_bars: BOOL, parent: HWND, file_progress: BOOL),
    pub ProgressDialogAddText: unsafe extern "system" fn(this: *mut Ops, txt: *const c_char, delayed_paint: BOOL),
    pub ProgressSetTotalSize: unsafe extern "system" fn(this: *mut Ops, total1: *const QuadWord, total2: *const QuadWord),
    pub ProgressSetSize: unsafe extern "system" fn(this: *mut Ops, size1: *const QuadWord, size2: *const QuadWord, delayed_paint: BOOL) -> BOOL,
    pub ProgressAddSize: unsafe extern "system" fn(this: *mut Ops, size: c_int, delayed_paint: BOOL) -> BOOL,
    pub ProgressEnableCancel: unsafe extern "system" fn(this: *mut Ops, enable: BOOL),
    pub ProgressGetHWND: unsafe extern "system" fn(this: *mut Ops) -> HWND,
    pub CloseProgressDialog: unsafe extern "system" fn(this: *mut Ops),
}

#[repr(C)]
pub struct Ops {
    pub vtbl: *const OpsVtbl,
}

/// `SalEnumSelection`
pub type EnumSelection = unsafe extern "system" fn(
    parent: HWND,
    enum_files: c_int,
    is_dir: *mut BOOL,
    size: *mut QuadWord,
    file_data: *mut *const FileData,
    param: *mut c_void,
    error_occured: *mut c_int,
) -> *const c_char;

// `CSalamanderGeneralAbstract` has 248 methods; the plugin needs only these slots.
pub const GENERAL_SLOT_GET_MSGBOX_PARENT: usize = 3;
pub const GENERAL_SLOT_ALLOC: usize = 28;
pub const GENERAL_SLOT_FREE: usize = 29;

// ---------------------------------------------------------------------------
// Plugin objects (implemented here, called by Salamander)
// ---------------------------------------------------------------------------

/// `CPluginInterfaceAbstract` – 16 slots.
#[repr(C)]
pub struct PluginVtbl {
    pub About: unsafe extern "system" fn(this: *mut c_void, parent: HWND),
    pub Release: unsafe extern "system" fn(this: *mut c_void, parent: HWND, force: BOOL) -> BOOL,
    pub LoadConfiguration: unsafe extern "system" fn(this: *mut c_void, parent: HWND, reg_key: *mut c_void, registry: *mut c_void),
    pub SaveConfiguration: unsafe extern "system" fn(this: *mut c_void, parent: HWND, reg_key: *mut c_void, registry: *mut c_void),
    pub Configuration: unsafe extern "system" fn(this: *mut c_void, parent: HWND),
    pub Connect: unsafe extern "system" fn(this: *mut c_void, parent: HWND, salamander: *mut Connect),
    pub ReleasePluginDataInterface: unsafe extern "system" fn(this: *mut c_void, plugin_data: *mut c_void),
    pub GetInterfaceForArchiver: unsafe extern "system" fn(this: *mut c_void) -> *mut c_void,
    pub GetInterfaceForViewer: unsafe extern "system" fn(this: *mut c_void) -> *mut c_void,
    pub GetInterfaceForMenuExt: unsafe extern "system" fn(this: *mut c_void) -> *mut c_void,
    pub GetInterfaceForFS: unsafe extern "system" fn(this: *mut c_void) -> *mut c_void,
    pub GetInterfaceForThumbLoader: unsafe extern "system" fn(this: *mut c_void) -> *mut c_void,
    pub Event: unsafe extern "system" fn(this: *mut c_void, event: c_int, param: u32),
    pub ClearHistory: unsafe extern "system" fn(this: *mut c_void, parent: HWND),
    pub AcceptChangeOnPathNotification: unsafe extern "system" fn(this: *mut c_void, path: *const c_char, including_subdirs: BOOL),
    pub PasswordManagerEvent: unsafe extern "system" fn(this: *mut c_void, parent: HWND, event: c_int),
}

/// `CPluginInterfaceForArchiverAbstract` – 10 slots.
#[repr(C)]
pub struct ArchiverVtbl {
    pub ListArchive: unsafe extern "system" fn(
        this: *mut c_void,
        salamander: *mut Ops,
        file_name: *const c_char,
        dir: *mut Dir,
        plugin_data: *mut *mut c_void,
    ) -> BOOL,
    pub UnpackArchive: unsafe extern "system" fn(
        this: *mut c_void,
        salamander: *mut Ops,
        file_name: *const c_char,
        plugin_data: *mut c_void,
        target_dir: *const c_char,
        archive_root: *const c_char,
        next: EnumSelection,
        next_param: *mut c_void,
    ) -> BOOL,
    pub UnpackOneFile: unsafe extern "system" fn(
        this: *mut c_void,
        salamander: *mut Ops,
        file_name: *const c_char,
        plugin_data: *mut c_void,
        name_in_archive: *const c_char,
        file_data: *const FileData,
        target_dir: *const c_char,
        new_file_name: *const c_char,
        renaming_not_supported: *mut BOOL,
    ) -> BOOL,
    pub PackToArchive: unsafe extern "system" fn(
        this: *mut c_void,
        salamander: *mut Ops,
        file_name: *const c_char,
        archive_root: *const c_char,
        move_files: BOOL,
        source_path: *const c_char,
        next: *const c_void,
        next_param: *mut c_void,
    ) -> BOOL,
    pub DeleteFromArchive: unsafe extern "system" fn(
        this: *mut c_void,
        salamander: *mut Ops,
        file_name: *const c_char,
        plugin_data: *mut c_void,
        archive_root: *const c_char,
        next: EnumSelection,
        next_param: *mut c_void,
    ) -> BOOL,
    pub UnpackWholeArchive: unsafe extern "system" fn(
        this: *mut c_void,
        salamander: *mut Ops,
        file_name: *const c_char,
        mask: *const c_char,
        target_dir: *const c_char,
        del_archive_when_done: BOOL,
        archive_volumes: *mut c_void,
    ) -> BOOL,
    pub CanCloseArchive:
        unsafe extern "system" fn(this: *mut c_void, salamander: *mut Ops, file_name: *const c_char, force: BOOL, panel: c_int) -> BOOL,
    pub GetCacheInfo: unsafe extern "system" fn(this: *mut c_void, temp_path: *mut c_char, own_delete: *mut BOOL, cache_copies: *mut BOOL) -> BOOL,
    pub DeleteTmpCopy: unsafe extern "system" fn(this: *mut c_void, file_name: *const c_char, first_file: BOOL),
    pub PrematureDeleteTmpCopy: unsafe extern "system" fn(this: *mut c_void, parent: HWND, copies_count: c_int) -> BOOL,
}

/// An object whose first (and only) member is the vtable pointer.
#[repr(C)]
pub struct Obj<T> {
    pub vtbl: *const T,
}

// SAFETY: the vtables are immutable statics of function pointers.
unsafe impl<T> Sync for Obj<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn file_data_layout_matches_pack4_msvc() {
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(size_of::<FileData>(), 56);
            assert_eq!(offset_of!(FileData, Name), 0);
            assert_eq!(offset_of!(FileData, Ext), 8);
            assert_eq!(offset_of!(FileData, Size), 16);
            assert_eq!(offset_of!(FileData, Attr), 24);
            assert_eq!(offset_of!(FileData, LastWrite), 28);
            assert_eq!(offset_of!(FileData, DosName), 36);
            assert_eq!(offset_of!(FileData, PluginData), 44);
            assert_eq!(offset_of!(FileData, Bits), 52);
        }
        #[cfg(target_pointer_width = "32")]
        assert_eq!(size_of::<FileData>(), 40);
        assert_eq!(size_of::<QuadWord>(), 8);
    }

    #[test]
    fn bit_fields_and_time() {
        let b = FileData::bits(300, true);
        assert_eq!(b & 0x1FF, 300);
        assert_eq!((b >> 10) & 1, 1);
        assert_eq!((b >> 12) & 0xF, 15);
        assert_eq!(b >> 16, 0);
        let t = filetime_from_unix(Some(0));
        assert_eq!(((t.high as u64) << 32) | t.low as u64, 116_444_736_000_000_000);
        assert_eq!(QuadWord::new(0x1_0000_0002).value(), 0x1_0000_0002);
    }
}
