//! Společné jádro pluginů `macfs-plugins`.
//!
//! * [`source`] – zdroj bajtů (soubor s obrazem, raw zařízení nebo DMG jako virtuální disk) s cache zarovnanou na sektory,
//! * [`udif`] – kontejner UDIF (DMG, jen čtení): rozpoznání, seznam oddílů, líné čtení dekomprimovaného virtuálního disku,
//! * [`detect`] – hledání svazků HFS+ a kontejnerů APFS uvnitř zdroje (holý svazek, DMG, GPT, APM, MBR, `.macfsdev`/`.hfsdev`/`.apfsdev`),
//! * [`volume`] – jednotný pohled na jeden svazek (HFS+ nebo APFS), hlášení nepodporovaných featur APFS,
//! * [`util`] – převody jmen (NFD → NFC, očista pro Windows), časů a cest.

pub mod detect;
pub mod source;
pub mod udif;
pub mod util;
pub mod volume;
