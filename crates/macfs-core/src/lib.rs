//! Společné jádro pluginů `macfs-plugins`.
//!
//! * [`source`] – zdroj bajtů (soubor s obrazem nebo raw zařízení) s cache zarovnanou na sektory,
//! * [`detect`] – hledání svazků uvnitř zdroje (holý svazek, GPT, APM, MBR, `.hfsdev`),
//! * [`util`] – převody jmen (NFD → NFC, očista pro Windows), časů a cest.

pub mod detect;
pub mod source;
pub mod util;
