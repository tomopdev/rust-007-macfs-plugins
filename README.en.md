# macfs-plugins

**Browse and extract macOS file systems, disk images and archives in Total Commander and Altap/Open Salamander. Read-only, written in Rust.**

*[Česky](README.cs.md)*

![license](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)
![rust](https://img.shields.io/badge/Rust-edition%202024-orange)
![platform](https://img.shields.io/badge/platform-Windows%20x64-lightgrey)
![mode](https://img.shields.io/badge/mode-read--only-green)

The plugins present a Mac disk image (DMG, sparse image, raw disk or partition) as an ordinary archive: you browse
folders, view files (F3) and extract them (F5). Nothing is ever written.

* `macfs_wcx.wcx64` – **WCX** plugin for Total Commander 11.x (64-bit)
* `macfs.spl` – plugin for **Altap Salamander 4.0 / Open Salamander 5.0** (64-bit)

Both plugins share the same core (`macfs-core`), so they support the same formats.

> **Status:** developed and tested on Linux with **synthetic** images (generators in the tests) and with a mock plugin host.
> The Windows (MSVC) build and running inside real Total Commander / Salamander have **not been verified** where the
> project was developed – see [Limitations](#limitations-and-unverified-items).

## Contents

- [Initial contributors](#initial-contributors)
- [What is implemented](#what-is-implemented)
- [Installation](#installation)
- [Usage](#usage)
- [Building](#building)
- [Repository layout](#repository-layout)
- [How it works](#how-it-works)
- [Tests](#tests)
- [Limitations and unverified items](#limitations-and-unverified-items)
- [Not implemented yet (roadmap)](#not-implemented-yet-roadmap)
- [Contributing](#contributing)
- [License](#license)


## Initial contributors

Idea, analysis, manual testing: tomopdev@outlook.com
Architecture, programming, automated testing: Claude Sonnet 5.5
Time scope of initial implementation: October 2026


## What is implemented

| Area | Format | Status | Notes |
|---|---|---|---|
| File systems | **HFS+ / HFSX** | ✅ | crate `hfsplus` 0.4.0 with local patches, including `decmpfs` transparent compression |
| | **APFS** | ✅ (unencrypted) | crate `apfs` 0.4.1; encrypted volumes, snapshots and clone relations are only **reported** |
| | **classic HFS** | ✅ | System 1 – Mac OS 9 disks and floppies, crate `hfs-reader` 0.1.1 (locally modified), resource forks as `<name>.rsrc` |
| | **MFS** | ✅ | 400K/800K floppies of the first Macs, crate `macfs` 0.2.0 |
| Images and containers | **DMG (UDIF)** | ✅ | lazy reading; zlib, bzip2, LZFSE, LZMA compression; ADC (very old DMGs) is reported as unsupported |
| | **Disk Copy 4.2** | ✅ | header around HFS/MFS floppies |
| | **sparse image** (`.sparseimage`) | ✅ (unencrypted) | |
| | **sparse bundle** (`.sparsebundle`) | ✅ (unencrypted) | opened via `Info.plist` or `.macfsdev` |
| | raw images, **GPT / APM / MBR** | ✅ | partitions are found automatically |
| | raw disk / partition | ✅ | via a `.macfsdev` text file |
| Packages | **PKG** (XAR + PBZX/CPIO) | ✅ | TOC, component metadata, list of installed files, XAR entries |
| File containers | **AppleDouble / AppleSingle** (RFC 1740) | ✅ | readable metadata listing (`!APPLEDOUBLE-INFO.txt`), resource fork, macOS extended attributes of `._` files* |
| | **MacBinary I/II/III** | ✅ | `!MACBINARY-INFO.txt`, both forks, comment |
| | **BinHex 4.0** (`.hqx`) | ✅ | CRC verification, `!BINHEX-INFO.txt`, both forks |

\* decoding of macOS extended attributes (`ATTR`) has not been verified against real files.

Conventions for classic Mac formats: names are decoded from MacRoman, `/` is shown as `:`, the resource fork is a separate
file `<name>.rsrc`, files with identical names are numbered.

## Installation

**Total Commander** – the zip `dist\macfs_wcx.zip` contains `pluginst.inf`; just open it in TC and confirm the installation.
Associate extensions (e.g. `dmg`, `hfs`, `sparseimage`, `hqx`) in *Configuration → Options → Packer → Configure packer extensions*.
The plugin recognises formats by content, so a file with any extension can be opened manually (**Ctrl+PgDn**).

**Salamander** – copy `macfs.spl` into `plugins\macfs\` and add it in *Plugins → Plugins Manager*. The plugin registers
for the extensions `hfs;apfs;dmg;pkg;dsk;dc42;image;hfv;mfs;sparseimage;sparsebundle;hqx;macbin;macfsdev;hfsdev;apfsdev`.

Details, compatibility and limitations of each plugin: [macfs-wcx](crates/macfs-wcx/README.md), [macfs-spl](crates/macfs-spl/README.md) (Czech).

## Usage

* **Image or volume**: double-click / Ctrl+PgDn on `.dmg`, `.sparseimage`, `.hfs`, `.img` … If it holds several volumes (APM/GPT),
  each appears in its own folder.
* **Sparse bundle**: open `Info.plist` inside the `.sparsebundle` folder, or create a `.macfsdev`.
* **Real disk or partition** (typically a Mac disk in a USB adapter): a text file `macdisk.macfsdev`

  ```ini
  device = \\.\PhysicalDrive2
  partition = 2          ; optional, 1 = first volume found
  # offset = 209735680   ; or an exact partition, in bytes
  ```

  Reading a raw disk needs administrator rights. The disk is **never** written.
* **Mac files with forks**: open `.hqx`, `.bin`/`.macbin` or `._file`; you get `!…-INFO.txt` with the metadata plus both forks.
  `bin` and `._*` are too generic to register, so use Ctrl+PgDn or a `.macfsdev` with `device = C:\path\._file`.
* **PKG**: open `.pkg`; besides the XAR entries there are `!PKG-COMPONENTS.txt` and `Installed Files`.

## Building

Requirements: Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools (Desktop development with C++).

```powershell
.\build.ps1                  # tests of the whole workspace + both plugins -> dist\
.\build.ps1 -Plugin wcx      # Total Commander only (dist\macfs_wcx.wcx64, dist\macfs_wcx.zip)
.\build.ps1 -Plugin spl      # Salamander only (dist\macfs.spl)
.\build.ps1 -Native          # -C target-cpu=native (not portable to older CPUs)
```

Tests (`cargo test --workspace`) also run on Linux/macOS; the plugins are built for `x86_64-pc-windows-msvc`.

## Repository layout

| Path | Contents |
|---|---|
| `crates/macfs-core` | shared core: cached data source, volume detection (GPT/APM/MBR, `.macfsdev`), unified volume view, UDIF, classic HFS/MFS (`classic`), sparse formats (`sparse`), PKG (`pkg`), file containers (`macfile`) |
| `crates/macfs-wcx` | Total Commander plugin |
| `crates/macfs-spl` | Salamander plugin |
| `vendor/hfsplus`, `vendor/apfs`, `vendor/hfs-reader` | upstream crates with local patches (described in `PATCHES.md`), not workspace members |
| `build.ps1` | Windows build and tests |
| `LICENSE`, `THIRD-PARTY-LICENSES.md` | project license and licenses of the dependencies |

## How it works

A file is opened as a `DiskSource` (file, DMG, sparse image/bundle; reads in 128 KiB blocks with a cache). `detect` finds the
volumes in it (bare file system, GPT/APM/MBR, Disk Copy 4.2) or recognises a file container (AppleDouble, BinHex, MacBinary).
Each volume is exposed as a `MacVolume` with one interface: *list a folder / read a file*. The plugins are independent of the
concrete format and only translate that interface to the Total Commander / Salamander API. Reading is lazy: DMGs are
decompressed block by block and files are streamed.

## Tests

```bash
cargo test --workspace
cargo test --manifest-path vendor/hfs-reader/Cargo.toml --features testutil   # vendored hfs-reader
```

Test images are generated in code (`macfs-core::testutil`, `hfs_reader::testutil`); the repository contains no binary samples.
The tests therefore verify agreement with **our interpretation** of the formats, not with real Mac files.

## Limitations and unverified items

* The Windows build and running in real TC / Salamander have not been verified; tests run on Linux against mock host simulations.
* Dates of classic volumes, MacBinary and BinHex are the **local time of the Mac without a time zone**; the plugins pass them on as UTC,
  so they may be shifted by the time zone offset.
* APFS: encrypted volumes, snapshots, clone relations and sealed volumes are only reported. HFS+: the journal is not replayed,
  directory hard links (Time Machine) are not translated.
* Encrypted DMGs and encrypted sparse images/bundles cannot be opened (the plugin says so).
* Sparse images with more than 1008 bands (header longer than 4 KiB) follow the documentation and are unverified on a real file.
* Extended attributes in `._` files (`ATTR` block) are decoded tolerantly and unverified.
* A MacBinary file containing a disk image is shown as a file, not as a disk.
* BinHex is decoded into memory, so it is limited to 512 MiB.

## Not implemented yet (roadmap)

Effort estimates are **rough and unverified** (one experienced developer, including tests and documentation, without samples
from real media; the real time depends on the availability of real images).

| Format / feature | What it would take | Estimate |
|---|---|---|
| **CoreStorage** (Fusion Drive, FileVault 1, logical volumes of OS X 10.7–10.14) – unencrypted | parse LVG/LVF/LV metadata, assemble the virtual volume (possibly from several physical disks), then reuse the existing HFS+ | 4–7 days |
| CoreStorage – **encrypted** (FileVault 1) | AES-XTS, key unwrapping from password/recovery key, password UX | 2–3 weeks |
| **ProDOS** (Apple II / IIgs), **Apple DOS 3.3**, **SOS** (Apple III) | each file system separately; simple structures but few documented samples | 2–4 days each |
| Apple II floppy containers (`.2mg`, `.nib`, `.woz`, `.do/.po`) | sector interleaving, nibble/WOZ decoding | +3–5 days |
| **NDIF / Disk Copy 6** (`.img`, `.smi` – image in the resource fork) | resource fork reading (MacBinary/AppleDouble), `bcem` blocks (ADC/RLE compression) | 4–6 days |
| **StuffIt** `.sit` (classic) | archive parser and decompression (methods 0, 1 RLE, 2 LZW, 3 Huffman, 5 LZAH, 13 …) | 1–2 weeks |
| StuffIt 5 / X, encrypted archives | further methods (Arsenic etc.), password | +2–4 weeks |
| HFS+ **journal replay** | read the journal and apply transactions in memory | ≈ 3 days |
| Time Machine directory **hard links** | translate `.HFS+ Private Directory Data` | 3–4 days |
| APFS **snapshots** | snapshot selection, shown as folders | 1–2 weeks |
| APFS **encryption** | keys from password/recovery key, AES-XTS | weeks |
| APFS **sealed volumes** | integrity trees and composite volumes (System + Data) | ≈ 1 week |
| **UFS** (older Mac OS X, NeXT) | another file system | ≈ 1 week |
| MacBinary / BinHex with a **disk image** inside | pass the fork as a source to `detect` | 1–2 days |

If you need one of these and have a real sample, please open an issue.

## Contributing

Issues and pull requests are welcome. Before submitting, run `cargo test --workspace` and `cargo clippy --workspace --all-targets`.
The most valuable contribution is a real image or file the plugin fails to open or shows incorrectly (preferably small and anonymised).
Record changes under `vendor/*` in the respective `PATCHES.md`.

## License

The project is distributed under the **GNU General Public License v3.0 or later** ([LICENSE](LICENSE)).

Reason: the crate `hfs-reader` (classic HFS) is GPL-3.0-or-later and the plugins contain it, so the resulting binaries must be
GPL-3.0-or-later as well. The other dependencies are MIT / Apache-2.0 / Zlib / BSD / Unicode-3.0 etc., i.e. GPLv3-compatible;
the full list is in [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).

## Acknowledgements

The crates `hfsplus`, `apfs`, `udif`, `xara` (Dil4rd), `macfs` (John-K), `hfs-reader` (the tailtalk authors), the RFC 1740 specification and libyal/libmodi.
