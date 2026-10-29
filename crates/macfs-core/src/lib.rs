//! Společné jádro pluginů `macfs-plugins`.
//!
//! * [`source`] – zdroj bajtů (soubor s obrazem, raw zařízení nebo DMG jako virtuální disk) s cache zarovnanou na sektory,
//! * [`macfile`] – kontejnery jednoho souboru Macintosh (AppleDouble/AppleSingle, MacBinary, BinHex) jako malý virtuální svazek s popisem metadat,
//! * [`sparse`] – Apple sparse images (`.sparseimage`) a sparse bundles (`.sparsebundle`) jako virtuální disk,
//! * [`udif`] – kontejner UDIF (DMG, jen čtení): rozpoznání, seznam oddílů, líné čtení dekomprimovaného virtuálního disku,
//! * [`detect`] – hledání svazků HFS+, klasického HFS, MFS (také v kontejneru Disk Copy 4.2), kontejnerů APFS a instalačních balíčků PKG uvnitř zdroje (holý svazek, DMG, GPT, APM, MBR, `.macfsdev`/`.hfsdev`/`.apfsdev`),
//! * [`classic`] – klasické svazky Macintosh: HFS (crate `hfs-reader`) a MFS (crate `macfs`) jako strom virtuálních uzlů,
//! * [`volume`] – jednotný pohled na jeden svazek (HFS+, klasický HFS, MFS, APFS nebo balíček PKG), hlášení nepodporovaných featur APFS,
//! * [`pkg`] – instalační balíček macOS (XAR) jako virtuální svazek: TOC, metadata komponent, `Installed Files`; nad ním
//!   [`pbzx`] (PBZX s náhodným přístupem) a [`cpio`] (index a strom payloadu),
//! * [`util`] – převody jmen (NFD → NFC, očista pro Windows), časů a cest.

pub mod classic;
pub mod cpio;
pub mod detect;
pub mod macfile;
pub mod pbzx;
pub mod pkg;
pub mod source;
pub mod sparse;
pub mod udif;
pub mod util;
pub mod volume;

/// Stavitelé syntetických balíčků PKG pro testy (feature `testutil`).
#[cfg(feature = "testutil")]
pub mod testutil;
