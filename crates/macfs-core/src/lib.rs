//! Společné jádro pluginů `macfs-plugins`.
//!
//! * [`source`] – zdroj bajtů (soubor s obrazem, raw zařízení nebo DMG jako virtuální disk) s cache zarovnanou na sektory,
//! * [`udif`] – kontejner UDIF (DMG, jen čtení): rozpoznání, seznam oddílů, líné čtení dekomprimovaného virtuálního disku,
//! * [`detect`] – hledání svazků HFS+, kontejnerů APFS a instalačních balíčků PKG uvnitř zdroje (holý svazek, DMG, GPT, APM, MBR, `.macfsdev`/`.hfsdev`/`.apfsdev`),
//! * [`volume`] – jednotný pohled na jeden svazek (HFS+, APFS nebo balíček PKG), hlášení nepodporovaných featur APFS,
//! * [`pkg`] – instalační balíček macOS (XAR) jako virtuální svazek: TOC, metadata komponent, `Installed Files`; nad ním
//!   [`pbzx`] (PBZX s náhodným přístupem) a [`cpio`] (index a strom payloadu),
//! * [`util`] – převody jmen (NFD → NFC, očista pro Windows), časů a cest.

pub mod cpio;
pub mod detect;
pub mod pbzx;
pub mod pkg;
pub mod source;
pub mod udif;
pub mod util;
pub mod volume;

/// Stavitelé syntetických balíčků PKG pro testy (feature `testutil`).
#[cfg(feature = "testutil")]
pub mod testutil;
