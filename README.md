# macfs-plugins – souborové systémy macOS pro Total Commander a Salamander (jen čtení)

Rust workspace (edition 2024) s pluginy, které umožní prohlížet a rozbalovat svazky macOS ve Windows
správcích souborů. Aktuálně **HFS+/HFSX** (nad crate [`hfsplus` 0.4.0](https://crates.io/crates/hfsplus)
s lokálními patchi); další souborové systémy macOS přibudou v dalších fázích.

## Struktura

| Cesta | Obsah |
|---|---|
| `crates/macfs-core` | společné jádro: zdroj dat (obraz/raw disk), detekce svazků (GPT/APM/MBR/`.hfsdev`), převody jmen a časů |
| `crates/hfsplus-wcx` | plugin WCX pro Total Commander 11.x (64 bit) → [README](crates/hfsplus-wcx/README.md) |
| `crates/hfsplus-spl` | plugin `.spl` pro Altap Salamander 4.0 / Open Salamander 5.0 (64 bit) → [README](crates/hfsplus-spl/README.md) |
| `vendor/hfsplus` | crate `hfsplus` 0.4.0 s lokálními úpravami ([PATCHES.md](vendor/hfsplus/PATCHES.md)) |

## Build

Požadavky: Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools
(Desktop development with C++).

```powershell
.\build.ps1                  # testy celého workspace + oba pluginy -> dist\
.\build.ps1 -Plugin wcx      # jen Total Commander (dist\hfsplus_wcx.wcx64, dist\hfsplus_wcx.zip)
.\build.ps1 -Plugin spl      # jen Salamander (dist\hfsplus.spl)
.\build.ps1 -Native          # -C target-cpu=native (nepřenosné na starší CPU)
```

Testy (`cargo test --workspace`) běží i mimo Windows; pluginy se sestavují pro `x86_64-pc-windows-msvc`.
Instalace a použití jednotlivých pluginů jsou v jejich README.
