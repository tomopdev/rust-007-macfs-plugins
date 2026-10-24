#![allow(clippy::missing_safety_doc)] // exports follow the SDK ABI; contracts are in the SDK headers

//! Open Salamander 5.0 / Altap Salamander 4.0 plugin (`.spl`) for read-only access
//! to HFS+/HFSX volumes and APFS containers, implemented in Rust against the C++ plugin SDK ABI.
//!
//! The SDK interfaces are abstract C++ classes; `spl.rs` reproduces their vtable
//! layout by hand. Exported symbols (same as the SDK's `.def` file):
//! `SalamanderPluginEntry`, `SalamanderPluginGetReqVer`, `SalamanderPluginGetSDKVer`.

pub mod container;
pub mod plugin;
pub mod spl;
pub mod win;

// sdílené moduly z macfs-core (zpřístupněné pod původními cestami)
pub use macfs_core::{detect, source, util, volume};

use std::ffi::{c_int, c_void};

/// Plugin entry point: returns the `CPluginInterfaceAbstract*` or NULL to refuse loading.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn SalamanderPluginEntry(entry: *mut spl::Entry) -> *mut c_void {
    // SAFETY: forwarded unchanged; Salamander passes its CSalamanderPluginEntryAbstract.
    unsafe { plugin::plugin_entry(entry) }
}

/// Oldest Salamander version (SDK numbering) this plugin can run in.
#[unsafe(no_mangle)]
pub extern "system" fn SalamanderPluginGetReqVer() -> c_int {
    spl::REQ_VERSION
}

/// SDK version the plugin's interfaces were written against; lets newer Salamander
/// versions call every method the plugin implements.
#[unsafe(no_mangle)]
pub extern "system" fn SalamanderPluginGetSDKVer() -> c_int {
    spl::SDK_VERSION
}
