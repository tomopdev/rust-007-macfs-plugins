//! Společné jádro pluginů `macfs-plugins`.
//!
//! * [`source`] – zdroj bajtů (soubor s obrazem nebo raw zařízení) s cache zarovnanou na sektory,
//! * [`detect`] – hledání svazků HFS+ a kontejnerů APFS uvnitř zdroje (holý svazek, GPT, APM, MBR, `.hfsdev`/`.apfsdev`),
//! * [`volume`] – jednotný pohled na jeden svazek (HFS+ nebo APFS), hlášení nepodporovaných featur APFS,
//! * [`util`] – převody jmen (NFD → NFC, očista pro Windows), časů a cest.

pub mod detect;
pub mod source;
pub mod util;
pub mod volume;
