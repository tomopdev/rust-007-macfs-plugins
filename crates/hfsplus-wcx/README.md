# hfsplus-wcx – HFS+/HFSX plugin pro Total Commander (64 bit, jen čtení)

Součást workspace [macfs-plugins](../../README.md).

WCX plugin v Rustu (edition 2024) nad crate [`hfsplus` 0.4.0](https://crates.io/crates/hfsplus)
(včetně decmpfs komprese: zlib, LZVN, LZFSE – řeší přes něj `cmpfs`).

## Co umí

* otevře **obraz disku/oddílu** (`.hfs`, `.img`, `.raw`, `.iso` … cokoliv, co obsahuje HFS+/HFSX),
* automaticky najde HFS+ oddíly na **GPT**, **Apple Partition Map** i **MBR** discích a v klasickém
  HFS wrapperu; jeden oddíl se zobrazí rovnou v kořeni, víc oddílů jako složky `Partition N - název`,
* **skutečný oddíl/disk** přes malý textový soubor `*.hfsdev` (viz níže),
* pojmenování: NFD → NFC, nepovolené znaky Windows (`< > : " | ? *`, řídicí znaky) → `_`,
  koncové tečky/mezery a rezervovaná jména (CON, NUL …) se ošetří, kolize jmen dostanou ` (2)`,
* extrakce s průběhem a možností přerušení (nedokončený soubor se smaže), zachová datum změny,
  dlouhé cesty (`\\?\`).

## Build

Požadavky: Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`
a Visual Studio Build Tools (Desktop development with C++).

Z kořene workspace:

```powershell
.\build.ps1 -Plugin wcx           # testy + release build -> dist\hfsplus_wcx.wcx64 a dist\hfsplus_wcx.zip
.\build.ps1 -Plugin wcx -Native   # totéž s -C target-cpu=native (nepřenosné na starší CPU)
```

Ručně:

```powershell
cargo build --release --target x86_64-pc-windows-msvc -p hfsplus-wcx
copy target\x86_64-pc-windows-msvc\release\hfsplus_wcx.dll hfsplus_wcx.wcx64
```

Plugin musí mít příponu **`.wcx64`** (64bitový Total Commander načítá právě ty).
Ověření exportů: `dumpbin /exports hfsplus_wcx.wcx64` – mají být `OpenArchiveW`, `ReadHeaderExW`,
`ProcessFileW`, `CloseArchive`, `SetChangeVolProcW`, `SetProcessDataProcW`, `CanYouHandleThisFileW`,
`GetPackerCaps` (a ANSI zástupky `OpenArchive`, `ReadHeader`, `ProcessFile`).

## Instalace do Total Commanderu

**Ručně (nejjistější):**

1. Zkopíruj `hfsplus_wcx.wcx64` do trvalé složky, např. `C:\Portable\TC\Plugins\HfsPlusWcx\`.
2. *Konfigurace → Nastavení → Zásuvné moduly (Plugins) → Konfigurovat packer pluginy (WCX)*.
3. Do pole přípony zadej `hfsdev` (a podle potřeby další: `hfs`, `img`, `raw`), klikni
   *Nový typ…* a vyber `hfsplus_wcx.wcx64`. Potvrď.

**Přes instalátor:** otevři `dist\hfsplus_wcx.zip` v TC (Enter) a potvrď instalaci. (Pokud by instalátor
64bitový plugin nenašel, použij ruční postup.)

Soubor se pak otevírá Enter / Ctrl+PgDn jako archiv.

## Skutečný oddíl nebo disk: soubor `.hfsdev`

TC neumí předat jako „archiv“ cestu typu `\\.\PhysicalDrive2`, proto vytvoř textový soubor
(např. `macdisk.hfsdev`) a otevři ho:

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
* **APFS, CoreStorage/FileVault, šifrované a komprimované DMG (UDIF)** nejsou podporované
  (DMG nejdřív převeď na raw obraz).
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
