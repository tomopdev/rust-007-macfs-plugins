# hfsplus-spl – HFS+/HFSX plugin pro Altap Salamander 4.0 / Open Salamander 5.0 (64 bit, jen čtení)

Součást workspace [macfs-plugins](../../README.md).

Plugin `.spl` v Rustu (edition 2024) nad crate [`hfsplus` 0.4.0](https://crates.io/crates/hfsplus)
(včetně decmpfs komprese: zlib, LZVN, LZFSE). Funkčně odpovídá WCX pluginu pro Total Commander:
obrazy disků, GPT / Apple Partition Map / MBR / HFS wrapper, `*.hfsdev` pro skutečné disky.

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

* Registruje se jako **panel archiver (jen prohlížení)** pro přípony `hfs` a `hfsdev`.
  Salamander pozná archivy **jen podle přípony** (žádná detekce obsahem). Další přípony (`img`,
  `raw`, `dmg` po převodu na raw …) přidej v *Options → Configuration → Archives Associations in Panels*.
  Pozor: `iso`/`img`/`dmg` mívá přiřazené plugin UnISO.
* Jeden HFS+ oddíl se zobrazí rovnou v kořeni, více oddílů jako složky `Partition N - název`.
* Rozbalení: průběhový dialog s tlačítkem Storno (nedokončený soubor se smaže), zachová datum
  změny. Existující soubory: plugin se **jednou** zeptá (Ano = přepsat všechny, Ne = přeskočit
  všechny existující, Storno = přerušit). Chyba čtení: Ano = přeskočit soubor, Ne = přeskočit
  všechny chybné, Storno = přerušit.
* Není implementováno: zápis/mazání, „custom unpacker“ (Alt+F9), konfigurační dialog, vlastní sloupce,
  lokalizace (hlášky jsou anglicky).
* Stejná omezení jako WCX verze: komprimované soubory se rozbalují celé do paměti, hard linky
  (Time Machine) se zobrazí jako prázdné soubory, žurnál se nepřehrává, APFS/CoreStorage/DMG
  (UDIF) nejsou podporované, symlink se rozbalí jako textový soubor s cílem.

## Build

Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools (C++).

Z kořene workspace:

```powershell
.\build.ps1 -Plugin spl           # testy + release build -> dist\hfsplus.spl
```

Ručně: `cargo build --release --target x86_64-pc-windows-msvc -p hfsplus-spl` a soubor
`target\x86_64-pc-windows-msvc\release\hfsplus_spl.dll` přejmenuj na `hfsplus.spl`.
MSVC runtime je slinkovaný staticky. Kontrola exportů: `dumpbin /exports hfsplus.spl` –
musí být právě `SalamanderPluginEntry`, `SalamanderPluginGetReqVer`, `SalamanderPluginGetSDKVer`.

**Jen x64.** Plugin musí mít stejnou bitovost jako Salamander. 32bitová verze by na x86 MSVC
vyžadovala `.def` soubor (stdcall dekorace názvů), což tu není připravené.

## Instalace

1. Zkopíruj `hfsplus.spl` do `<Salamander>\plugins\hfsplus\`.
2. *Plugins → Plugins Manager → Add* a vyber soubor (v české lokalizaci stejné menu pod Moduly/Pluginy).
3. Zkontroluj přípony v *Options → Configuration → Archives Associations in Panels*.

## Skutečný disk nebo oddíl: soubor `.hfsdev`

Stejný formát jako u WCX verze. Textový soubor (např. `macdisk.hfsdev`):

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
