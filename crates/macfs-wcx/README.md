# macfs-wcx – HFS+/HFSX, APFS, DMG a PKG plugin pro Total Commander (64 bit, jen čtení)

Součást workspace [macfs-plugins](../../README.md).

WCX plugin v Rustu (edition 2024) nad crate [`hfsplus` 0.4.0](https://crates.io/crates/hfsplus)
(včetně decmpfs komprese: zlib, LZVN, LZFSE – řeší přes něj `cmpfs`).

## Co umí

* otevře **obraz disku/oddílu** (`.hfs`, `.img`, `.raw`, `.iso` … cokoliv, co obsahuje HFS+/HFSX),
* automaticky najde HFS+ oddíly na **GPT**, **Apple Partition Map** i **MBR** discích a v klasickém
  HFS wrapperu; jeden oddíl se zobrazí rovnou v kořeni, víc oddílů jako složky `Partition N - název`,
* **skutečný oddíl/disk** přes malý textový soubor `*.macfsdev` (viz níže),
* pojmenování: NFD → NFC, nepovolené znaky Windows (`< > : " | ? *`, řídicí znaky) → `_`,
  koncové tečky/mezery a rezervovaná jména (CON, NUL …) se ošetří, kolize jmen dostanou ` (2)`,
* extrakce s průběhem a možností přerušení (nedokončený soubor se smaže), zachová datum změny,
  dlouhé cesty (`\\?\`).


## APFS

Kromě HFS+/HFSX plugin otevře i **kontejnery APFS** (obraz, GPT/APM/MBR disk, `.macfsdev`/`.apfsdev`/`.hfsdev`).
Používá vendorovaný čistě rustový crate [`apfs` 0.4.1](https://crates.io/crates/apfs) (`vendor/apfs`,
lokální úpravy v `vendor/apfs/PATCHES.md`: výběr svazku, příznaky svazku, detekce klonů).
Kontejner s víc svazky (System, Data, Preboot, Recovery, VM …) se zobrazí jako složky
`Partition N - název svazku`.

Crate neumí **šifrování, snapshoty ani vztahy klonů**. Plugin to před čtením zjistí a nahlásí
virtuálními textovými soubory, které se v seznamu chovají jako obyčejné soubory (jdou i rozbalit):

* `!APFS-NOTICE.txt` v kořeni svazku – svazek má snapshoty (zobrazuje se jen aktuální stav), je zapečetěný
  (sealed system volume) nebo má nedokončený restore / probíhající přešifrování;
* šifrovaný svazek (FileVault, hardwarové šifrování T2 / Apple silicon) se **nečte** – je v něm jen
  `!APFS-NOTICE.txt` (bez klíče jsou jména i data šifrovaná);
* `!APFS-CLONES.txt` – svazek obsahuje klonované soubory. Data klonů se čtou přes sdílené extenty
  původního souboru, takže se vypíší i rozbalí normálně; nehlásí se jen vazba klon–originál.
* svazek, který nejde otevřít (poškozený, sealed), má v sobě `!APFS-NOTICE.txt` s chybou.

Syntetické APFS obrazy z testů vznikají podle specifikace Applu, **ne** z obrazu zapsaného macOS –
první skutečný obraz vyzkoušej ručně (výpis, rozbalení, soubor s diakritikou).

## DMG (UDIF)

Obrazy Apple DMG se otevírají jako ostatní obrazy; plugin je pozná podle trailer „koly“ (ne podle přípony)
a čte je jako jeden virtuální disk: oddíly leží na svých sektorech, takže GPT / Apple Partition Map uvnitř
fungují stejně jako na skutečném disku (jeden svazek se zobrazí v kořeni, víc jako složky `Partition N - název`).
Podporované bloky: raw, zlib (UDZO), bzip2 (UDBZ), LZFSE (ULFO), XZ (ULMO). Bloky se dekódují až při čtení, takže se
velký DMG nerozbaluje celý a výpis je rychlý. Kontrolní součty (CRC32) se při prohlížení neověřují.

* **Jen čtení.** DMG ani HFS+/APFS uvnitř se nezapisují; plugin nové kontejnery nevytváří.
* **Nepodporované:** šifrovaný DMG (AES) a `.sparseimage` (plugin to při otevření oznámí), segmentované `.dmgpart`,
  starý ADC a NDIF/Disk Copy. DMG bez UDIF trailer (holý obraz s příponou `.dmg`) se čte jako obyčejný obraz.
* Obrazy `.sparsebundle` jsou složky, ne soubory; otevři v nich jednotlivé bands až po převodu na DMG/raw.

## PKG (instalační balíčky macOS)

Plugin otevře i **instalační balíčky `.pkg`** (flat packages = archiv XAR; poznají se podle obsahu, ne podle přípony;
funguje i pro `.xar`). Zobrazí se:

* položky archivu XAR tak, jak jsou uložené (`Distribution`, `Resources\`, u product balíčků složka `Název.pkg` s `PackageInfo`,
  `Bom`, `Scripts`, `Payload`),
* `!XAR-TOC.xml` – dekomprimovaný XAR TOC,
* `!PKG-COMPONENTS.txt` – metadata balíčku a komponent (titul a produkt z `Distribution`, u komponent identifikátor, verze,
  install location, počet souborů a velikost, skripty, bundly),
* složka **`Installed Files`** – soubory z payloadů komponent (při víc komponentách podsložka na komponentu) a v jejím
  kořeni `!CPIO-INDEX.txt` – výpis CPIO jako `cpio -tv` (práva, uid/gid, velikost, datum, cíle symlinků, hard linky).
  Cesty jsou relativní k *install location* komponenty (ta je v indexu a v `!PKG-COMPONENTS.txt`).

Payload `pbzx` (XZ bloky) se čte líně přes tabulku bloků: výpis dekóduje jen bloky s hlavičkami CPIO a rozbalení souboru
jen jeho bloky, takže se velký payload nerozbaluje do paměti (crate `pbzx` dekóduje celý payload do paměti; proto jen XAR
parsuje crate [`xara`](https://crates.io/crates/xara) a PBZX/CPIO řeší `macfs-core`). Čte se i holé CPIO (odc/newc/crc) a starší
gzip/bzip2/xz CPIO – ty se musí jednou dekódovat do anonymní dočasné složky (`%TEMP%`, smaže se při zavření).
Hard linky (data u jednoho z odkazů) se vyhodnocují; symlink se rozbalí jako textový soubor s cílem.
Payload, který nejde přečíst, nepoloží balíček: ve složce je místo něj `!PAYLOAD-ERROR.txt` s důvodem.

* Jen **čtení**; kontrolní součty XAR/BOM se neověřují, datum položek XAR (v TOC) se nezobrazuje (u payloadu ano).
* Nepodporované kódování položky XAR (jiné než raw/gzip/zlib/bzip2/xz) se hlásí chybou u dané položky.
* Archiv `Scripts` se zobrazuje jako soubor (pbzx/gzip CPIO), nerozbaluje se do složky.
* Syntetické balíčky z testů vznikají podle specifikace, **ne** z reálného balíčku z macOS – první skutečný `.pkg` vyzkoušej ručně.

## Build

Požadavky: Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`
a Visual Studio Build Tools (Desktop development with C++).

Z kořene workspace:

```powershell
.\build.ps1 -Plugin wcx           # testy + release build -> dist\macfs_wcx.wcx64 a dist\macfs_wcx.zip
.\build.ps1 -Plugin wcx -Native   # totéž s -C target-cpu=native (nepřenosné na starší CPU)
```

Ručně:

```powershell
cargo build --release --target x86_64-pc-windows-msvc -p macfs-wcx
copy target\x86_64-pc-windows-msvc\release\macfs_wcx.dll macfs_wcx.wcx64
```

Plugin musí mít příponu **`.wcx64`** (64bitový Total Commander načítá právě ty).
Ověření exportů: `dumpbin /exports macfs_wcx.wcx64` – mají být `OpenArchiveW`, `ReadHeaderExW`,
`ProcessFileW`, `CloseArchive`, `SetChangeVolProcW`, `SetProcessDataProcW`, `CanYouHandleThisFileW`,
`GetPackerCaps` (a ANSI zástupky `OpenArchive`, `ReadHeader`, `ProcessFile`).

## Instalace do Total Commanderu

**Ručně (nejjistější):**

1. Zkopíruj `macfs_wcx.wcx64` do trvalé složky, např. `C:\Portable\TC\Plugins\MacFsWcx\`.
2. *Konfigurace → Nastavení → Zásuvné moduly (Plugins) → Konfigurovat packer pluginy (WCX)*.
3. Do pole přípony zadej `dmg` (a podle potřeby další: `pkg`, `macfsdev`, `hfs`, `img`, `raw`), klikni
   *Nový typ…* a vyber `macfs_wcx.wcx64`. Potvrď.

**Přes instalátor:** otevři `dist\macfs_wcx.zip` v TC (Enter) a potvrď instalaci. (Pokud by instalátor
64bitový plugin nenašel, použij ruční postup.)

Soubor se pak otevírá Enter / Ctrl+PgDn jako archiv.

## Skutečný oddíl nebo disk: soubor `.macfsdev`

TC neumí předat jako „archiv“ cestu typu `\\.\PhysicalDrive2`, proto vytvoř textový soubor
(např. `macdisk.macfsdev`) a otevři ho:

```ini
# celý fyzický disk – oddíly se najdou podle GPT/APM/MBR
device = \\.\PhysicalDrive2

# volitelně vybrat jen jeden nalezený HFS+ oddíl (1 = první)
partition = 2

# nebo oddíl zadat přesně (bajty; dec nebo 0x…); length je volitelné
# offset = 209735680
# length = 500107862016
```

* `\\.\PhysicalDriveN` (číslo je v Správě disků) i `\\.\X:` vyžadují **spuštění Total Commanderu
  jako správce**. Čte se výhradně pro čtení, nic se nezapisuje.
* Windows HFS+ nemontuje, takže disk bývá bez písmene – proto `PhysicalDriveN`.

## Omezení

* Jen **čtení**, žádné vytváření/úpravy (`GetPackerCaps` hlásí jen detekci podle obsahu).
* **Komprimované (decmpfs) soubory** se dekomprimují celé do paměti → velmi velké komprimované soubory
  zaberou tolik RAM, kolik mají po rozbalení.
* **Hard linky** (typicky Time Machine) crate nevyhodnocuje: odkaz se zobrazí jako prázdný soubor, data
  jsou ve složce `____HFS+ Private Data` / `.HFS+ Private Directory Data_` jako `iNode…`.
* Žurnál (journal) se nepřehrává – po neuklidněném odpojení může být obraz mírně nekonzistentní.
* **CoreStorage/FileVault (HFS+)** a šifrované DMG nejsou podporované (viz sekce DMG).
* Položky, jejichž původní název na disku obsahuje znak `/` (starý Mac OS), nelze přes crate adresovat
  a extrakce selže.
* Čas souboru v seznamu se převádí z UTC do lokálního času podle pravidel Windows.

## Lokální oprava crate `hfsplus` (adresář `vendor/hfsplus`)

Crate `hfsplus` 0.4.0 řadí/porovnává jména na case-insensitive HFS+ svazcích jinak než Apple
(`FastUnicodeCompare`): znak U+0000 se musí skládat na 0xFFFF (řadí se jako poslední),
ignorovatelné znaky (U+200C–200F, U+202A–202E, U+206A–206F, U+FEFF) se přeskakují a tabulka
case-foldingu nezná řečtinu/cyrilici. Důsledek: složka `\0\0\0\0HFS+ Private Data`
(kterou má každý macOS svazek s hard linky) se nenašla a plugin hlásil „directory could not be
read“. Projekt proto používá lokální kopii crate s opravou v `unicode.rs` (viz `vendor/hfsplus/PATCHES.md`);
chování pro HFSX (binární porovnání) se nemění. Regresní test: `tests/apple_order.rs` (přeuspořádá
katalog syntetického obrazu do Applova pořadí; s původním crate selhává na `file not found`).
Hard linky samotné (soubory `iNode…` v té složce) plugin stále nevyhodnocuje.

## Testy

`cargo test` staví syntetické HFSX obrazy (feature `testutil` crate `hfsplus`) a ověřuje výpis,
extrakci, GPT (512 B i 4 KiB sektory), `.hfsdev`, přerušení a exportované WCX funkce.
Spouštěly se na Linuxu; kód specifický pro Windows (čtení délky zařízení přes
`IOCTL_DISK_GET_LENGTH_INFO`, převod na lokální čas, `\\?\`) je pod `cfg(windows)` a otestuj ho
na svém stroji.
