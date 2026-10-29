# macfs-plugins

*[English](README.en.md)*

**Prohlížení a rozbalování souborových systémů, obrazů disků a archivů macOS v Total Commanderu a Altap/Open Salamanderu. Jen pro čtení, napsáno v Rustu.**

![license](https://img.shields.io/badge/licence-GPL--3.0--or--later-blue)
![rust](https://img.shields.io/badge/Rust-edition%202024-orange)
![platform](https://img.shields.io/badge/platforma-Windows%20x64-lightgrey)
![mode](https://img.shields.io/badge/režim-jen%20pro%20čtení-green)

Pluginy ukážou obraz disku z Macu (DMG, sparse image, surový disk nebo oddíl) jako obyčejný archiv: procházíš složky,
prohlížíš soubory (F3) a rozbaluješ je (F5). Nic se nikdy nezapisuje.

* `macfs_wcx.wcx64` – plugin **WCX** pro Total Commander 11.x (64 bit)
* `macfs.spl` – plugin pro **Altap Salamander 4.0 / Open Salamander 5.0** (64 bit)

Oba pluginy sdílejí stejné jádro (`macfs-core`), takže umí totéž.

> **Stav:** projekt vznikl a byl testován v Linuxu na **syntetických** obrazech (generátory v testech) a na pluginovém
> rozhraní přes zkušební „hostitele“. Sestavení pro Windows (MSVC) a běh v reálném Total Commanderu / Salamanderu **nebyly v
> místě vývoje ověřeny** – viz [Omezení](#omezení-a-neověřené-věci).

## Obsah

- [Iniciální přispěvatelé](#iniciální-přispěvatelé)
- [Co je implementováno](#co-je-implementováno)
- [Instalace](#instalace)
- [Použití](#použití)
- [Sestavení](#sestavení)
- [Struktura repozitáře](#struktura-repozitáře)
- [Jak to funguje](#jak-to-funguje)
- [Testy](#testy)
- [Omezení a neověřené věci](#omezení-a-neověřené-věci)
- [Co zatím není implementováno (roadmap)](#co-zatím-není-implementováno-roadmap)
- [Přispívání](#přispívání)
- [Licence](#licence)

## Iniciální přispěvatelé

Nápad, analýza, manální testování: tomopdev@outlook.com
Architektura, programování a automatické testování: Claude Sonnet 5.5
Časové období úvodní implmenetace: Říjen 2026


## Co je implementováno

| Oblast | Formát | Stav | Poznámka |
|---|---|---|---|
| Souborové systémy | **HFS+ / HFSX** | ✅ | crate `hfsplus` 0.4.0 s lokálními patchi, včetně transparentní komprese `decmpfs` |
| | **APFS** | ✅ (bez šifrování) | crate `apfs` 0.4.1; šifrované svazky, snapshoty a vztahy klonů plugin jen **hlásí** |
| | **klasický HFS** | ✅ | disky i disketky System 1 – Mac OS 9, crate `hfs-reader` 0.1.1 (lokálně upravený), resource forky jako `<název>.rsrc` |
| | **MFS** | ✅ | disketky 400K/800K prvních Maců, crate `macfs` 0.2.0 |
| Obrazy a kontejnery | **DMG (UDIF)** | ✅ | líné čtení; komprese zlib, bzip2, LZFSE, LZMA; ADC (velmi staré DMG) se hlásí jako nepodporované |
| | **Disk Copy 4.2** | ✅ | hlavička kolem HFS/MFS disket |
| | **sparse image** (`.sparseimage`) | ✅ (bez šifrování) | |
| | **sparse bundle** (`.sparsebundle`) | ✅ (bez šifrování) | otevírá se přes `Info.plist` nebo `.macfsdev` |
| | surové obrazy, **GPT / APM / MBR** | ✅ | oddíly se najdou automaticky |
| | surový disk / oddíl | ✅ | přes textový soubor `.macfsdev` |
| Balíčky | **PKG** (XAR + PBZX/CPIO) | ✅ | TOC, metadata komponent, seznam nainstalovaných souborů, položky XAR |
| Kontejnery souborů | **AppleDouble / AppleSingle** (RFC 1740) | ✅ | čitelný výpis metadat (`!APPLEDOUBLE-INFO.txt`), resource fork, rozšířené atributy `._` souborů* |
| | **MacBinary I/II/III** | ✅ | `!MACBINARY-INFO.txt`, oba forky, komentář |
| | **BinHex 4.0** (`.hqx`) | ✅ | kontrola CRC, `!BINHEX-INFO.txt`, oba forky |

\* dekódování rozšířených atributů macOS (`ATTR`) není ověřeno na skutečných souborech.

Konvence klasických Macových formátů: jména se dekódují z MacRoman, `/` se zobrazí jako `:`, resource fork je zvláštní
soubor `<název>.rsrc`, soubory se shodnými jmény se číslují.

## Instalace

**Total Commander** – zip `dist\macfs_wcx.zip` obsahuje `pluginst.inf`; stačí ho otevřít v TC a potvrdit instalaci.
Přípony (např. `dmg`, `hfs`, `sparseimage`, `hqx`) se přiřadí v *Konfigurace → Nastavení → Zabalování → Konfigurovat
pakovací programy*. Plugin pozná formát podle obsahu, takže jde ručně otevřít i soubor s jinou příponou (**Ctrl+PgDn**).

**Salamander** – `macfs.spl` zkopíruj do složky `plugins\macfs\` a v *Plugins → Plugins Manager* ho přidej. Plugin se
registruje na přípony `hfs;apfs;dmg;pkg;dsk;dc42;image;hfv;mfs;sparseimage;sparsebundle;hqx;macbin;macfsdev;hfsdev;apfsdev`.

Podrobnosti, kompatibilita a omezení jednotlivých pluginů: [macfs-wcx](crates/macfs-wcx/README.md), [macfs-spl](crates/macfs-spl/README.md).

## Použití

* **Obraz nebo svazek**: dvojklik / Ctrl+PgDn na `.dmg`, `.sparseimage`, `.hfs`, `.img` … Pokud obsahuje víc svazků (APM/GPT),
  každý je ve vlastní složce.
* **Sparse bundle**: otevři `Info.plist` uvnitř složky `.sparsebundle`, nebo vytvoř `.macfsdev`.
* **Skutečný disk či oddíl** (typicky disk z Macu v USB adaptéru): textový soubor `macdisk.macfsdev`

  ```ini
  device = \\.\PhysicalDrive2
  partition = 2          ; volitelné, 1 = první nalezený svazek
  # offset = 209735680   ; nebo přesný oddíl v bajtech
  ```

  Čtení surového disku vyžaduje práva správce. Disk se **nikdy** nezapisuje.
* **Soubory z Macu s forky**: otevři `.hqx`, `.bin`/`.macbin` nebo `._soubor`; uvidíš `!…-INFO.txt` s metadaty a oba forky.
* **PKG**: otevři `.pkg`; kromě XAR položek je `!PKG-COMPONENTS.txt` a `Installed Files`.

## Sestavení

Požadavky: Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools (Desktop development with C++).

```powershell
.\build.ps1                  # testy celého workspace + oba pluginy -> dist\
.\build.ps1 -Plugin wcx      # jen Total Commander (dist\macfs_wcx.wcx64, dist\macfs_wcx.zip)
.\build.ps1 -Plugin spl      # jen Salamander (dist\macfs.spl)
.\build.ps1 -Native          # -C target-cpu=native (nepřenosné na starší CPU)
```

Testy (`cargo test --workspace`) běží i v Linuxu/macOS; pluginy se sestavují pro `x86_64-pc-windows-msvc`.

## Struktura repozitáře

| Cesta | Obsah |
|---|---|
| `crates/macfs-core` | společné jádro: zdroj dat s cache, detekce svazků (GPT/APM/MBR, `.macfsdev`), jednotný pohled na svazek, UDIF, klasické HFS/MFS (`classic`), sparse formáty (`sparse`), PKG (`pkg`), kontejnery souborů (`macfile`) |
| `crates/macfs-wcx` | plugin Total Commanderu |
| `crates/macfs-spl` | plugin Salamanderu |
| `vendor/hfsplus`, `vendor/apfs`, `vendor/hfs-reader` | upstream cratey s lokálními úpravami (popsány v `PATCHES.md`), nejsou členy workspace |
| `build.ps1` | build a testy ve Windows |
| `LICENSE`, `THIRD-PARTY-LICENSES.md` | licence projektu a použitých knihoven |

## Jak to funguje

Soubor se otevře jako `DiskSource` (soubor, DMG, sparse image/bundle; čtení po blocích 128 KiB s cache). `detect` v něm najde
svazky (holý souborový systém, GPT/APM/MBR, Disk Copy 4.2) a případně rozpozná kontejner souboru (AppleDouble, BinHex,
MacBinary). Každý svazek se zpřístupní jako `MacVolume` s jednotným rozhraním *vypiš složku / přečti soubor*. Pluginy jsou
na konkrétním formátu nezávislé, jen překládají toto rozhraní na API Total Commanderu, resp. Salamanderu. Čtení je líné:
DMG se dekomprimuje po blocích, soubory se streamují.

## Testy

```bash
cargo test --workspace
cargo test --manifest-path vendor/hfs-reader/Cargo.toml --features testutil   # vendorovaný hfs-reader
```

Obrazy pro testy se generují v kódu (`macfs-core::testutil`, `hfs_reader::testutil`), v repozitáři nejsou žádné binární vzorky.
Testy tedy ověřují shodu s **naší interpretací** formátů, ne s reálnými soubory z Macu.

## Omezení a neověřené věci

* Sestavení pro Windows a běh v reálném TC / Salamanderu nebyl ověřen; testy běží v Linuxu na „hostitelských“ simulacích API.
* Data klasických svazků, MacBinary a BinHex jsou **místní čas Macu bez časové zóny**; pluginy je předají jako UTC, takže mohou být
  posunuta o rozdíl časového pásma.
* APFS: šifrované svazky, snapshoty, vztahy klonů a zapečetěné svazky se jen hlásí. HFS+: žurnál se nepřehrává, hard linky
  (Time Machine) se nepřekládají.
* Šifrované DMG a šifrované sparse image/bundle se neotevřou (plugin to ohlásí).
* Sparse image s více než 1008 pásmy (hlavička delší než 4 KiB) vychází z dokumentace a není ověřena na reálném souboru.
* Rozšířené atributy v `._` souborech (blok `ATTR`) jsou dekódovány tolerantně a neověřeno.
* MacBinary obsahující obraz disku se zobrazí jako soubor, ne jako disk.
* BinHex se dekóduje do paměti, proto je omezen na 512 MiB.

## Co zatím není implementováno (roadmap)

Odhady pracnosti jsou **hrubé, neověřené** (jeden zkušený vývojář, včetně testů a dokumentace, bez vzorků ze skutečných médií;
u každého se čas může změnit podle dostupnosti reálných obrazů).

| Formát / funkce | Co by to obnášelo | Odhad |
|---|---|---|
| **CoreStorage** (Fusion Drive, FileVault 1, logické svazky OS X 10.7–10.14) – nešifrovaný | parsování metadat LVG/LVF/LV, sestavení virtuálního svazku (i více fyzických disků), pak se použije stávající HFS+ | 4–7 dní |
| CoreStorage – **šifrovaný** (FileVault 1) | AES-XTS, rozbalení klíčů z hesla/obnovovacího klíče, UX pro zadání hesla | 2–3 týdny |
| **ProDOS** (Apple II / IIgs), **Apple DOS 3.3**, **SOS** (Apple III) | každý souborový systém zvlášť; jednoduché struktury, ale málo dokumentovaných vzorků | 2–4 dny každý |
| kontejnery disket Apple II (`.2mg`, `.nib`, `.woz`, `.do/.po`) | prokládání sektorů, dekódování nibble/WOZ | +3–5 dní |
| **NDIF / Disk Copy 6** (`.img`, `.smi` – obraz v resource forku) | čtení resource forku (MacBinary/AppleDouble), bloky `bcem` (komprese ADC/RLE) | 4–6 dní |
| **StuffIt** `.sit` (klasický) | parser archivu a dekomprese (metody 0, 1 RLE, 2 LZW, 3 Huffman, 5 LZAH, 13 …) | 1–2 týdny |
| StuffIt 5 / X, šifrované archivy | další metody (Arsenic atd.), heslo | +2–4 týdny |
| HFS+ **přehrání žurnálu** | čtení journalu a aplikace transakcí v paměti | ≈ 3 dny |
| Time Machine **hard linky** adresářů | překlad `.HFS+ Private Directory Data` | 3–4 dny |
| APFS **snapshoty** | výběr snapshotu, zobrazení jako složky | 1–2 týdny |
| APFS **šifrování** | klíče z hesla/obnovovacího klíče, AES-XTS | týdny |
| APFS **zapečetěné svazky** | práce s integritními stromy a složenými svazky (System + Data) | ≈ 1 týden |
| **UFS** (starší Mac OS X, NeXT) | další souborový systém | ≈ 1 týden |
| MacBinary / BinHex s **obrazem disku** uvnitř | předání forku jako zdroje do `detect` | 1–2 dny |

Pokud některý z nich potřebuješ a máš reálný vzorek, otevři issue.

## Přispívání

Issues a pull requesty jsou vítány. Před odesláním spusť `cargo test --workspace` a `cargo clippy --workspace --all-targets`.
Nejcennější je reálný vzorek obrazu nebo souboru, který plugin neotevře nebo zobrazí špatně (nejlépe malý, anonymizovaný).
Změny ve `vendor/*` zapisuj do příslušného `PATCHES.md`.

## Licence

Projekt je šířen pod **GNU General Public License v3.0 nebo novější** ([LICENSE](LICENSE)).

Důvod: crate `hfs-reader` (klasický HFS) je GPL-3.0-or-later a pluginy ho obsahují, výsledné binárky tedy musí být také GPL-3.0-or-later.
Ostatní použité knihovny jsou MIT / Apache-2.0 / Zlib / BSD / Unicode-3.0 apod., tedy s GPLv3 slučitelné; úplný seznam je v
[THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).

## Poděkování

Crate `hfsplus`, `apfs`, `udif`, `xara` (Dil4rd), `macfs` (John-K), `hfs-reader` (autoři tailtalk) a specifikace RFC 1740, libyal/libmodi.
