# macfs-plugins – souborové systémy a archivy macOS pro Total Commander a Salamander (jen čtení)

Rust workspace (edition 2024) s pluginy, které umožní prohlížet a rozbalovat svazky macOS ve Windows
správcích souborů. Aktuálně **HFS+/HFSX** (nad crate [`hfsplus` 0.4.0](https://crates.io/crates/hfsplus)
s lokálními patchi) a **APFS** (nad crate [`apfs` 0.4.1](https://crates.io/crates/apfs), bez šifrování, snapshotů a vztahů klonů – ty plugin jen hlásí); **obrazy DMG (UDIF)** s HFS+/APFS uvnitř; další souborové systémy macOS přibudou v dalších fázích. Vše je jen pro čtení (UDIF kontejner ani souborové systémy v něm se nezapisují).

## Struktura

| Cesta | Obsah |
|---|---|
| `crates/macfs-core` | společné jádro: zdroj dat (obraz/raw disk), kontejner UDIF (DMG: líné čtení dekomprimovaného virtuálního disku nad crate [`udif` 0.4.2](https://crates.io/crates/udif)), detekce svazků (holý svazek/DMG/GPT/APM/MBR/`.macfsdev`/`.hfsdev`/`.apfsdev`), jednotný pohled na svazek HFS+/APFS, hlášení nepodporovaných featur APFS, převody jmen a časů |
| `crates/macfs-wcx` | plugin WCX pro Total Commander 11.x (64 bit) → [README](crates/macfs-wcx/README.md) |
| `crates/macfs-spl` | plugin `.spl` pro Altap Salamander 4.0 / Open Salamander 5.0 (64 bit) → [README](crates/macfs-spl/README.md) |
| `vendor/apfs` | crate `apfs` 0.4.1 s lokálními úpravami ([PATCHES.md](vendor/apfs/PATCHES.md)) |
| `vendor/hfsplus` | crate `hfsplus` 0.4.0 s lokálními úpravami ([PATCHES.md](vendor/hfsplus/PATCHES.md)) |

## Build

Požadavky: Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools
(Desktop development with C++).

```powershell
.\build.ps1                  # testy celého workspace + oba pluginy -> dist\
.\build.ps1 -Plugin wcx      # jen Total Commander (dist\macfs_wcx.wcx64, dist\macfs_wcx.zip)
.\build.ps1 -Plugin spl      # jen Salamander (dist\macfs.spl)
.\build.ps1 -Native          # -C target-cpu=native (nepřenosné na starší CPU)
```

Testy (`cargo test --workspace`) běží i mimo Windows; pluginy se sestavují pro `x86_64-pc-windows-msvc`.
Instalace a použití jednotlivých pluginů jsou v jejich README.
