# macfs-spl – HFS+/HFSX, APFS, klasický HFS, MFS, DMG, sparse image, PKG a kontejnery AppleDouble/MacBinary/BinHex plugin pro Altap Salamander 4.0 / Open Salamander 5.0 (64 bit, jen čtení)

Součást workspace [macfs-plugins](../../README.md).

Plugin `.spl` v Rustu (edition 2024) nad crate [`hfsplus` 0.4.0](https://crates.io/crates/hfsplus)
(včetně decmpfs komprese: zlib, LZVN, LZFSE). Funkčně odpovídá WCX pluginu pro Total Commander:
obrazy disků, GPT / Apple Partition Map / MBR / HFS wrapper, `*.macfsdev` pro skutečné disky.

## Důležité: jména souborů jsou v API Salamanderu ANSI

Plugin SDK Open Salamanderu 5.0 (`spl_arc.h`, `spl_com.h`) předává a přijímá všechna jména
jako `const char*` v aktuální ANSI kódové stránce (u `CFileData::Name` navíc max. `MAX_PATH-5`
bajtů). „Unicode“ je tedy jen na straně hostitelské aplikace, ne v rozhraní pluginů. Důsledek:

* Jména z HFS+ se vždy převedou NFD → NFC (jak jsi chtěl) a očistí od znaků nepovolených ve Windows.
* **V panelu** se pak zobrazí jen to, co ANSI stránka umí (u cs-CZ Windows-1250). Neznámé znaky
  (např. čínština, emoji) se zobrazí jako `_`; kolize jmen dostanou ` (2)`.
* **Při rozbalení (F5 / Kopírovat)** plugin zapisuje na disk **plná Unicode jména** (NFC), takže
  `日本.txt` vznikne správně, i když v panelu byl `__.txt`.
* **Při F3/F4/Enter** (dočasná kopie pro prohlížeč/editor) Salamander hledá soubor pod jménem
  z panelu, proto se tam použije ANSI jméno.
* Cesta k samotnému archivu přichází také v ANSI – obraz ležící v adresáři s jménem mimo ANSI stránku
  nepůjde otevřít (omezení Salamanderu, ne pluginu).

## Co plugin dělá a nedělá

* Registruje se jako **panel archiver (jen prohlížení)** pro přípony `hfs`, `apfs`, `dmg`, `pkg`, `dsk`, `dc42`, `image`, `hfv`, `mfs`, `sparseimage`, `sparsebundle`, `macfsdev`, `hfsdev` a `apfsdev`.
  Salamander pozná archivy **jen podle přípony** (žádná detekce obsahem). Další přípony (`img`,
  `raw` …) přidej v *Options → Configuration → Archives Associations in Panels*.
  Pozor: `iso`/`img`/`dmg` mívá přiřazené plugin UnISO.
* Jeden HFS+ oddíl se zobrazí rovnou v kořeni, více oddílů jako složky `Partition N - název`.
* Rozbalení: průběhový dialog s tlačítkem Storno (nedokončený soubor se smaže), zachová datum
  změny. Existující soubory: plugin se **jednou** zeptá (Ano = přepsat všechny, Ne = přeskočit
  všechny existující, Storno = přerušit). Chyba čtení: Ano = přeskočit soubor, Ne = přeskočit
  všechny chybné, Storno = přerušit.
* Není implementováno: zápis/mazání, „custom unpacker“ (Alt+F9), konfigurační dialog, vlastní sloupce,
  lokalizace (hlášky jsou anglicky).
* Stejná omezení jako WCX verze: komprimované soubory se rozbalují celé do paměti, hard linky
  (Time Machine) se zobrazí jako prázdné soubory, žurnál se nepřehrává, CoreStorage a šifrované DMG nejsou podporované, symlink se rozbalí jako textový soubor s cílem.


## APFS

Kromě HFS+/HFSX plugin otevře i **kontejnery APFS** (obraz, GPT/APM/MBR disk, `.macfsdev`/`.apfsdev`/`.hfsdev`).
Používá vendorovaný čistě rustový crate [`apfs` 0.4.1](https://crates.io/crates/apfs) (`vendor/apfs`,
lokální úpravy v `vendor/apfs/PATCHES.md`: výběr svazku, příznaky svazku, detekce klonů).
Kontejner s víc svazky (System, Data, Preboot, Recovery, VM …) se zobrazí jako složky
`Partition N - název svazku`.

Crate neumí **šifrování, snapshoty ani vztahy klonů**. Plugin to před čtením zjistí a nahlásí
virtuálními textovými soubory, které se v seznamu chovají jako obyčejné soubory (jdou i rozbalit; v Salamanderu se místo dialogu používají právě tyto soubory):

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
* **Nepodporované:** šifrovaný DMG (AES) (plugin to při otevření oznámí), segmentované `.dmgpart`,
  starý ADC a NDIF/Disk Copy. DMG bez UDIF trailer (holý obraz s příponou `.dmg`) se čte jako obyčejný obraz.
* `.sparseimage` a `.sparsebundle` viz sekce níže.

## Klasický HFS a MFS (disketky a disky System 1 – Mac OS 9)

Plugin otevře i **klasické svazky Macintosh**, jen pro čtení: **HFS** (crate [`hfs-reader` 0.1.1](https://crates.io/crates/hfs-reader),
vendorovaný s lokálními úpravami v `vendor/hfs-reader/PATCHES.md`) a **MFS** (400K/800K disketa prvních Maců, crate
[`macfs` 0.2.0](https://crates.io/crates/macfs)). Poznají se podle obsahu (MDB na offsetu 1024), ne podle přípony:

* holý obraz disku, obraz **Disk Copy 4.2** (`.dsk`, `.image`, `.dc42`; 84bajtová hlavička se přeskočí) a klasický HFS
  oddíl na disku s **Apple Partition Map** (i uvnitř DMG). HFS wrapper kolem HFS+ je dál HFS+;
* jména jsou **MacRoman** (diakritika a symboly se zobrazí správně), `/` ve jménu (na klasickém Macu legální) se zobrazí jako `:`;
* **resource fork**: soubor s neprázdným resource forkem se zobrazí dvakrát – `Název` (data fork) a `Název.rsrc` (resource fork).
  Starý software má většinu obsahu právě v resource forku, a data fork je pak prázdný. Typ a creator kód se nezobrazují;
* prázdné složky se zobrazí, MFS je plochý (bez složek);
* HFS se čte líně (v paměti je jen katalog, soubory se streamují), MFS svazek se načte celý (je malý, nad 64 MiB se odmítne).

Omezení:

* **Datumy** klasických svazků jsou místní čas Macu, který je zapsal, bez časové zóny. Plugin je zobrazí, jako by byly v UTC,
  takže se v panelu mohou lišit o posun vaší časové zóny.
* Jen čtení; poškozený katalog HFS se neotevře (chyba), poškozený soubor se rozbalí jen s chybou čtení. Obrazy NDIF a Disk Copy 6
  (jiné než Disk Copy 4.2) plugin nezná.
* **Licence:** crate `hfs-reader` je GPL-3.0-or-later, takže plugin, který ho obsahuje, je při šíření dalším lidem také GPL-3.0-or-later
  (pro osobní použití je to jedno; viz `vendor/hfs-reader/PATCHES.md`).
* Obrazy z testů vznikají podle *Inside Macintosh* (`hfs_reader::testutil`, formátování MFS dělá crate `macfs`), **ne** z reálného disku –
  první skutečnou disketu nebo System disk vyzkoušej ručně.

## Sparse image a sparse bundle (`.sparseimage`, `.sparsebundle`)

Plugin čte i **sparse obrazy disků Apple**, jen pro čtení. Jsou to disky pevné velikosti rozdělené na pásma (bands);
pásmo, které se neukládá, se čte jako nuly. Uvnitř bývá HFS+ nebo APFS (typicky na GPT) a plugin je najde jako u DMG.

* **`.sparseimage`** (jeden soubor, signatura `sprs`): otevírá se přímo, pozná se podle obsahu.
* **`.sparsebundle`** (složka s `Info.plist` a `bands\<hex>`, např. záloha Time Machine na síťovém disku): Total Commander
  ani Salamander nepředají plugin složku, takže jedna z cest:
  1. otevřít **`Info.plist`** uvnitř balíku (`.sparsebundle` je pro Windows obyčejná složka). Plugin pozná podle obsahu, že je
     to sparse bundle; jiné `plist` soubory odmítne. V TC proto lze příponu `plist` pluginu bezpečně přiřadit,
     do Salamanderu `plist` nepřiřazuj (hlásil by chybu u každého plist) a použij následující možnost;
  2. soubor `.macfsdev` s `device = D:\Zalohy\Mac.sparsebundle` (cesta ke složce balíku).
* **Šifrované** obrazy (AES, `encrcdsa` / `cdsaencr`, u balíku v souboru `token`) se pozná a odmítne s vysvětlením.
* Obraz se čte líně, po pásmech; u balíku zůstává otevřených několik posledních souborů pásem. Time Machine zálohy mívají
  hard linky, které plugin u HFS+ zobrazuje jako prázdné soubory (viz Omezení).
* **Neověřeno na skutečných souborech:** formát `.sparseimage` je podle dokumentace libyal/libmodi (hlavička 4096 B, mapa pásem
  od offsetu 64, pásma za hlavičkou). U obrazů s víc než 1008 pásmy předpokládám, že se hlavička zvětší na násobek 4096 B;
  to jsem žádným skutečným souborem neověřil. Testy běží na syntetických obrazech, první skutečný `.sparseimage` / `.sparsebundle`
  vyzkoušej ručně.

## AppleDouble, MacBinary a BinHex (kontejnery souborů klasického Macu)

Plugin otevře i jednotlivé soubory, ve kterých je uložen soubor z Macu s oběma forky a metadaty. Obsah se ukáže jako
„archiv“ s několika položkami:

| Formát | Poznání | Obsah |
|---|---|---|
| **AppleDouble** (RFC 1740, soubory `._název` z macOS) a **AppleSingle** | magic `00 05 16 07` / `00 05 16 00` | `!APPLEDOUBLE-INFO.txt` / `!APPLESINGLE-INFO.txt`, resource fork jako `<název>.rsrc`, u AppleSingle i datový fork |
| **MacBinary I/II/III** (`.bin`, `.macbin`) | hlavička 128 bajtů (kontrola CRC u II/III) | `!MACBINARY-INFO.txt`, datový fork, `<název>.rsrc` |
| **BinHex 4.0** (`.hqx`) | řádek „(This file must be converted with BinHex 4.0)“ | `!BINHEX-INFO.txt`, datový fork, `<název>.rsrc`; CRC všech částí se kontroluje a výsledek je v info souboru |

`!…-INFO.txt` je čitelný výpis metadat: jméno, type/creator, příznaky Finderu (rozepsané), pozice ikony, data (vytvoření,
změna, záloha, přístup), velikosti forků, komentář, černobílá ikona jako ASCII-art, tabulka entries (AppleDouble), ProDOS/MS-DOS/AFP
informace a rozšířené atributy macOS (`com.apple.quarantine` apod.). Poškozená položka nebo špatné CRC nebrání otevření,
jen se to v info souboru uvede.

Poznámky: formát se pozná **podle obsahu**, přípona je jen pomůcka. Přípony `hqx` a `macbin` lze v Salamanderu
má plugin registrované; `bin` a `._*` ne (příliš obecné) – tyto soubory otevři přes `.macfsdev` (viz níže) (`device = C:\cesta\._soubor`). Dekódování atributů
(`ATTR`) vychází z dokumentace formátu a nebylo ověřeno na skutečných souborech z macOS; když se nepodaří, atributy se jen
ohlásí jako nedekódované. Datum v MacBinary/BinHex je místní čas Macu bez časové zóny. Obraz disku zabalený do MacBinary
se (zatím) jako disk neotevírá.

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

Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools (C++).

Z kořene workspace:

```powershell
.\build.ps1 -Plugin spl           # testy + release build -> dist\macfs.spl
```

Ručně: `cargo build --release --target x86_64-pc-windows-msvc -p macfs-spl` a soubor
`target\x86_64-pc-windows-msvc\release\macfs_spl.dll` přejmenuj na `macfs.spl`.
MSVC runtime je slinkovaný staticky. Kontrola exportů: `dumpbin /exports macfs.spl` –
musí být právě `SalamanderPluginEntry`, `SalamanderPluginGetReqVer`, `SalamanderPluginGetSDKVer`.

**Jen x64.** Plugin musí mít stejnou bitovost jako Salamander. 32bitová verze by na x86 MSVC
vyžadovala `.def` soubor (stdcall dekorace názvů), což tu není připravené.

## Instalace

1. Zkopíruj `macfs.spl` do `<Salamander>\plugins\macfs\`.
2. *Plugins → Plugins Manager → Add* a vyber soubor (v české lokalizaci stejné menu pod Moduly/Pluginy).
3. Zkontroluj přípony v *Options → Configuration → Archives Associations in Panels*.

## Skutečný disk nebo oddíl: soubor `.macfsdev`

Stejný formát jako u WCX verze. Textový soubor (např. `macdisk.macfsdev`):

```ini
device = \\.\PhysicalDrive2     # celý fyzický disk, oddíly se najdou podle GPT/APM/MBR
partition = 2                   # volitelně: jen 2. nalezený HFS+ oddíl
# offset = 209735680            # nebo přesný oddíl (bajty); length je volitelné
```

`\\.\PhysicalDriveN` a `\\.\X:` vyžadují Salamander spuštěný jako správce. Čte se jen pro čtení.

## Kompatibilita s verzemi (co je ověřené a co ne)

Rozložení vtable (pořadí `virtual` metod) je převzaté z hlaviček SDK **Open Salamander 5.0**
(SDK verze 103). Plugin hlásí `SalamanderPluginGetReqVer = 102` (Altap Salamander 4.0) a
`GetSDKVer = 103`; hostitel starší než 102 plugin odmítne s hláškou. Fungování ve 4.0 vychází
z tabulky verzí v `spl_vers.h` a z předpokladu, že se rozhraní rozšiřují jen na konci; hlavičky 4.0
jsem neměl, takže tohle **není ověřené**. Pokud ve 4.0 selže načtení, zkus Open Salamander 5.0.

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

`cargo test` na Linuxu: mock „Salamander“ postavený ze stejných vtable (entry, general, directory,
progress, enumerátor) volá exportované funkce a archiver metody nad syntetickými HFSX obrazy;
testuje se layout `CFileData` (pack 4), registrace, výpis (včetně pořadí AddDir před AddFile),
více oddílů, `archiveRoot`, rozbalení s plnými Unicode jmény, `UnpackOneFile` + přejmenování,
storno, dotaz na přepsání. **Skutečný Salamander ani Windows build tady spuštěn nebyl** – Windows
kód (ANSI konverze, `MessageBoxW`, čtení délky zařízení) je v `win.rs`/`source.rs` pod `cfg(windows)`.
Po prvním buildu otevři malý `.hfs` obraz a ověř výpis, F5, F3 a názvy s diakritikou.
