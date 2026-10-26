//! Installer packages (XAR + PBZX / CPIO) through the volume interface, on synthetic packages.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use macfs_core::detect::{self, FsKind};
use macfs_core::pkg::{INDEX_NAME, INSTALLED_NAME, PAYLOAD_ERROR_NAME, SUMMARY_NAME, TOC_NAME};
use macfs_core::testutil::*;
use macfs_core::volume::{self, MacVolume, Node, NodeKind, VolumeError};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_file(bytes: &[u8]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-core-pkg-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    let f = d.join("test.pkg");
    fs::write(&f, bytes).unwrap();
    f
}

fn open(bytes: &[u8]) -> MacVolume {
    let path = temp_file(bytes);
    let (src, parts) = detect::locate(&path).unwrap();
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert_eq!(parts[0].kind, FsKind::Pkg);
    let src = Arc::new(Mutex::new(src));
    let mut vols = volume::open_all(&src, &parts).unwrap();
    assert_eq!(vols.len(), 1);
    // the source file may go away on Windows only after the handle is closed; the volume owns it now
    vols.remove(0).vol
}

fn names(nodes: &[Node]) -> Vec<&str> {
    nodes.iter().map(|n| n.name.as_str()).collect()
}

fn read(v: &mut MacVolume, path: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let n = v.read_file_to(path, &mut out).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(n as usize, out.len());
    out
}

fn text(v: &mut MacVolume, path: &str) -> String {
    String::from_utf8(read(v, path)).unwrap()
}

fn big() -> Vec<u8> {
    (0..50_000u32).flat_map(|i| i.to_le_bytes()).collect()
}

#[test]
fn product_package_shows_xar_toc_metadata_and_installed_files() {
    let mut v = open(&sample_pkg(&big()));

    // ---- the top level: the XAR entries as stored + the virtual files and folder
    let root = v.list_directory("/").unwrap();
    let n = names(&root);
    for want in ["Distribution", "Resources", "Tool.pkg", TOC_NAME, SUMMARY_NAME, INSTALLED_NAME] {
        assert!(n.contains(&want), "{want} missing in {n:?}");
    }
    let installed = root.iter().find(|x| x.name == INSTALLED_NAME).unwrap();
    assert_eq!(installed.kind, NodeKind::Dir);
    let toc = text(&mut v, &format!("/{TOC_NAME}"));
    assert!(toc.starts_with("<?xml") && toc.contains("<name>Distribution</name>") && toc.contains("Tool.pkg"), "{toc}");
    assert_eq!(root.iter().find(|x| x.name == TOC_NAME).unwrap().size, toc.len() as u64);

    // ---- the component folder holds the raw XAR entries
    let comp = v.list_directory("/Tool.pkg").unwrap();
    assert_eq!(names(&comp), ["PackageInfo", "Bom", "Scripts", "Payload"]);
    assert_eq!(read(&mut v, "/Tool.pkg/Bom"), b"BOMStore");
    assert!(text(&mut v, "/Distribution").contains("Example &amp; Tool"));
    assert_eq!(read(&mut v, "/Resources/en.lproj/Localizable.strings"), b"\"a\" = \"b\";");
    assert!(read(&mut v, "/Tool.pkg/Payload").starts_with(b"pbzx"));
    assert!(v.list_directory("/Tool.pkg/Bom").is_err());

    // ---- the metadata of the components
    let summary = text(&mut v, &format!("/{SUMMARY_NAME}"));
    for want in [
        "Product archive",
        "Title: Example & Tool",
        "Product: com.example.suite 1.2.3",
        "Component 1: Tool.pkg",
        "com.example.tool",
        "1.2.3",
        "/usr/local",
        "6 files",
        "12 KB installed",
        "preinstall: ./preinstall",
        "com.example.tool.app 1.2 (./Tool.app)",
    ] {
        assert!(summary.contains(want), "{want:?} missing in\n{summary}");
    }
    assert_eq!(root.iter().find(|x| x.name == SUMMARY_NAME).unwrap().size, summary.len() as u64);

    // ---- Installed Files: the payload tree (one payload: no component folder) + the CPIO index
    let inst = v.list_directory(&format!("/{INSTALLED_NAME}")).unwrap();
    assert_eq!(names(&inst), ["bin", "share", INDEX_NAME]); // `share` is made up, "." is not an entry
    let bin = v.list_directory(&format!("/{INSTALLED_NAME}/bin")).unwrap();
    assert_eq!(names(&bin), ["tool", "tool-link"]);
    assert_eq!(bin[0].size, SAMPLE_BIN.len() as u64);
    assert_eq!(bin[0].mtime, Some(MTIME as i64));
    assert_eq!(bin[1].kind, NodeKind::Symlink);
    assert_eq!(read(&mut v, &format!("/{INSTALLED_NAME}/bin/tool")), SAMPLE_BIN);
    assert_eq!(read(&mut v, &format!("/{INSTALLED_NAME}/bin/tool-link")), b"tool"); // the target as text
    assert_eq!(read(&mut v, &format!("/{INSTALLED_NAME}/share/doc/tool/README.txt")), b"read me");
    assert_eq!(read(&mut v, &format!("/{INSTALLED_NAME}/share/a:b?.txt")), b"illegal chars"); // sanitised by the plugins
    // a file spanning many 4 KiB PBZX chunks
    assert_eq!(read(&mut v, &format!("/{INSTALLED_NAME}/share/doc/tool/big.bin")), big());
    // hard links read the data of the link that carries it
    assert_eq!(read(&mut v, &format!("/{INSTALLED_NAME}/share/hard-1.txt")), b"shared data");
    assert_eq!(read(&mut v, &format!("/{INSTALLED_NAME}/share/hard-2.txt")), b"shared data");

    let index = text(&mut v, &format!("/{INSTALLED_NAME}/{INDEX_NAME}"));
    for want in [
        "Payload of the package \"Tool.pkg\"",
        "install location: /usr/local",
        "odc (070707)",
        "-rwxr-xr-x",
        "lrwxr-xr-x",
        "bin/tool-link -> tool",
        "2023-11-14 22:13:20",
        "share/doc/tool/big.bin",
        "501/20",
    ] {
        assert!(index.contains(want), "{want:?} missing in\n{index}");
    }
    assert_eq!(inst.iter().find(|x| x.name == INDEX_NAME).unwrap().size, index.len() as u64);

    // unknown and misused paths
    assert!(matches!(v.list_directory("/nope"), Err(VolumeError::NotFound(_))));
    assert!(v.list_directory(&format!("/{INSTALLED_NAME}/bin/tool")).is_err());
    assert!(v.read_file_to(&format!("/{INSTALLED_NAME}/bin"), &mut Vec::new()).is_err());
    assert!(v.read_file_to(&format!("/{INSTALLED_NAME}/missing"), &mut Vec::new()).is_err());
}

#[test]
fn newc_payload_with_hard_link_data_on_the_last_link() {
    let payload = sample_payload(true, b"newc big file");
    let mut x = XarBuilder::new();
    x.file("PackageInfo", PACKAGE_INFO.as_bytes());
    x.file("Payload", &pbzx_stored(&payload, 1000));
    let mut v = open(&x.build());

    // a component package (no Distribution, payload at the archive root)
    let root = v.list_directory("/").unwrap();
    assert!(names(&root).contains(&INSTALLED_NAME));
    assert!(text(&mut v, &format!("/{SUMMARY_NAME}")).contains("Component package"));
    let base = format!("/{INSTALLED_NAME}");
    assert_eq!(read(&mut v, &format!("{base}/share/hard-1.txt")), b"shared data");
    assert_eq!(read(&mut v, &format!("{base}/share/hard-2.txt")), b"shared data");
    assert_eq!(read(&mut v, &format!("{base}/share/doc/tool/big.bin")), b"newc big file");
    let index = text(&mut v, &format!("{base}/{INDEX_NAME}"));
    assert!(index.contains("newc (070701)") && index.contains("hard link, data stored with share/hard-2.txt"), "{index}");
}

#[test]
fn payload_formats_and_xar_encodings() {
    let payload = sample_payload(false, b"x");
    let gz = gzip(&payload);
    let mut bz = Vec::new();
    {
        use std::io::Write;
        let mut e = bzip2::write::BzEncoder::new(&mut bz, bzip2::Compression::default());
        e.write_all(&payload).unwrap();
        e.finish().unwrap();
    }

    let cases: Vec<(&str, XarBuilder)> = {
        let mut v = Vec::new();
        let mut a = XarBuilder::new();
        a.file("Payload", &payload); // plain CPIO, read in place
        v.push(("plain cpio", a));
        let mut a = XarBuilder::new();
        a.file("Payload", &gz); // gzip compressed CPIO (older packages), decoded into a temporary file
        v.push(("gzip cpio", a));
        let mut a = XarBuilder::new();
        a.file("Payload", &bz);
        v.push(("bzip2 cpio", a));
        let mut a = XarBuilder::new();
        a.file_gzip("Payload", &pbzx_xz(&payload, 512)); // the XAR layer itself compresses the entry
        v.push(("xar gzip of pbzx", a));
        let mut a = XarBuilder::new();
        a.file_bzip2("Payload", &payload);
        v.push(("xar bzip2 of cpio", a));
        let mut a = XarBuilder::new();
        a.file("Payload", &pbzx_xz(&payload, 100_000)); // a single chunk
        v.push(("pbzx one chunk", a));
        v
    };
    for (what, x) in cases {
        let mut v = open(&x.build());
        let base = format!("/{INSTALLED_NAME}");
        let list = v.list_directory(&base).unwrap();
        assert_eq!(names(&list), ["bin", "share", INDEX_NAME], "{what}");
        assert_eq!(read(&mut v, &format!("{base}/bin/tool")), SAMPLE_BIN, "{what}");
        assert_eq!(read(&mut v, &format!("{base}/share/hard-2.txt")), b"shared data", "{what}");
    }
}

#[test]
fn several_components_get_a_folder_each() {
    let a = CpioBuilder::odc().add("./a.txt", FILE, b"from a").finish();
    let b = CpioBuilder::odc().add("./b.txt", FILE, b"from b").finish();
    let mut x = XarBuilder::new();
    x.file("Distribution", DISTRIBUTION.as_bytes());
    x.file("A.pkg/Payload", &pbzx_xz(&a, 4096));
    x.file("B.pkg/Payload", &pbzx_stored(&b, 4096));
    x.file("B.pkg/PackageInfo", b"<pkg-info identifier=\"com.example.b\" version=\"2\"/>");
    x.file("Scripts-only.pkg/Scripts", b"x");
    let mut v = open(&x.build());

    let base = format!("/{INSTALLED_NAME}");
    assert_eq!(names(&v.list_directory(&base).unwrap()), ["A.pkg", "B.pkg"]);
    assert_eq!(names(&v.list_directory(&format!("{base}/A.pkg")).unwrap()), ["a.txt", INDEX_NAME]);
    assert_eq!(read(&mut v, &format!("{base}/A.pkg/a.txt")), b"from a");
    assert_eq!(read(&mut v, &format!("{base}/B.pkg/b.txt")), b"from b");
    assert!(text(&mut v, &format!("{base}/B.pkg/{INDEX_NAME}")).contains("identifier: com.example.b"));
    assert!(v.list_directory(&format!("{base}/C.pkg")).is_err());
    let summary = text(&mut v, &format!("/{SUMMARY_NAME}"));
    assert!(summary.contains("Component 1: A.pkg") && summary.contains("Component 2: B.pkg"), "{summary}");
}

#[test]
fn unreadable_payload_is_reported_in_its_folder() {
    let mut x = XarBuilder::new();
    x.file("PackageInfo", PACKAGE_INFO.as_bytes());
    x.file("Payload", b"this is neither pbzx nor cpio");
    x.file("Other.pkg/Payload", &pbzx_stored(&CpioBuilder::odc().add("./ok", FILE, b"fine").finish(), 64));
    let mut v = open(&x.build());

    // the root is both the broken package and the folder with a second component
    let base = format!("/{INSTALLED_NAME}");
    let comps = v.list_directory(&base).unwrap();
    assert_eq!(comps.len(), 2, "{comps:?}");
    let broken = &comps[0].name;
    let inner = v.list_directory(&format!("{base}/{broken}")).unwrap();
    assert_eq!(names(&inner), [PAYLOAD_ERROR_NAME]);
    let msg = text(&mut v, &format!("{base}/{broken}/{PAYLOAD_ERROR_NAME}"));
    assert!(msg.contains("could not be read") && msg.contains("unrecognised payload format"), "{msg}");
    // the other component and the XAR entries are not affected
    assert_eq!(read(&mut v, &format!("{base}/Other.pkg/ok")), b"fine");
    assert_eq!(read(&mut v, "/Payload"), b"this is neither pbzx nor cpio");
}

#[test]
fn truncated_payload_keeps_the_entries_before_the_damage() {
    let mut payload = CpioBuilder::odc().add("./first.txt", FILE, b"complete").add("./second.txt", FILE, b"cut off here").finish();
    payload.truncate(payload.len() - 90); // inside the data of the second file
    let mut x = XarBuilder::new();
    x.file("Payload", &payload);
    let mut v = open(&x.build());
    let base = format!("/{INSTALLED_NAME}");
    assert_eq!(names(&v.list_directory(&base).unwrap()), ["first.txt", INDEX_NAME]);
    assert!(text(&mut v, &format!("{base}/{INDEX_NAME}")).contains("WARNING"));
    assert_eq!(read(&mut v, &format!("{base}/first.txt")), b"complete");
}

#[test]
fn virtual_names_never_hide_real_entries() {
    let payload = CpioBuilder::odc().add(&format!("./{INDEX_NAME}"), FILE, b"real index").finish();
    let mut x = XarBuilder::new();
    x.file(TOC_NAME, b"real toc");
    x.file(INSTALLED_NAME, b"real folder name");
    x.file("Payload", &payload);
    let mut v = open(&x.build());

    let root = v.list_directory("/").unwrap();
    let n = names(&root);
    assert_eq!(n.iter().filter(|x| **x == TOC_NAME).count(), 1);
    assert_eq!(read(&mut v, &format!("/{TOC_NAME}")), b"real toc");
    assert_eq!(read(&mut v, &format!("/{INSTALLED_NAME}")), b"real folder name");
    let virtual_toc = format!("{TOC_NAME} (MacFS)");
    assert!(n.contains(&virtual_toc.as_str()) && n.contains(&format!("{INSTALLED_NAME} (MacFS)").as_str()), "{n:?}");
    assert!(text(&mut v, &format!("/{virtual_toc}")).contains("<toc>"));

    let inst = format!("/{INSTALLED_NAME} (MacFS)");
    let list = v.list_directory(&inst).unwrap();
    assert_eq!(names(&list), [INDEX_NAME, &format!("{INDEX_NAME} (MacFS)")]);
    assert_eq!(read(&mut v, &format!("{inst}/{INDEX_NAME}")), b"real index");
    assert!(text(&mut v, &format!("{inst}/{INDEX_NAME} (MacFS)")).contains("CPIO format"));
}

#[test]
fn not_a_package_is_not_detected() {
    let path = temp_file(&vec![0x78u8; 50_000]);
    let (_, parts) = detect::locate(&path).unwrap();
    assert!(parts.is_empty());
    // a XAR header that promises a table of contents larger than the file
    let mut h = b"xar!".to_vec();
    h.extend_from_slice(&28u16.to_be_bytes());
    h.extend_from_slice(&1u16.to_be_bytes());
    h.extend_from_slice(&(1u64 << 40).to_be_bytes());
    h.extend_from_slice(&100u64.to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes());
    h.resize(4096, 0);
    let (_, parts) = detect::locate(&temp_file(&h)).unwrap();
    assert!(parts.is_empty());
}

#[test]
fn damaged_xar_is_a_format_error() {
    let mut bytes = sample_pkg(b"x");
    bytes[40] ^= 0xFF; // inside the zlib stream of the table of contents
    let path = temp_file(&bytes);
    let (src, parts) = detect::locate(&path).unwrap();
    let src = Arc::new(Mutex::new(src));
    match volume::open_all(&src, &parts) {
        Err(VolumeError::Format(m)) => assert!(m.contains("installer package"), "{m}"),
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("must fail"),
    }
}
