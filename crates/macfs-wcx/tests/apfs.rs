//! APFS through the WCX plugin core and exports, on synthetic containers
//! (`apfs::testutil`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use apfs::testutil::ApfsImageBuilder;
use macfs_wcx::archive::{Archive, OpenError};
use macfs_wcx::ffi::*;
use macfs_wcx::*;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-wcx-apfs-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    d
}

fn single_volume() -> Vec<u8> {
    let mut b = ApfsImageBuilder::new();
    b.volume("Data")
        .add_dir("docs")
        .add_file("docs/hello.txt", b"hello apfs")
        .add_file("big.bin", &vec![0xAB; 600_000])
        .add_file("a:b?.txt", b"illegal chars")
        .add_file("Re\u{301}sume\u{301}.txt", b"nfd name");
    b.build()
}

fn multi_volume() -> Vec<u8> {
    let mut b = ApfsImageBuilder::new();
    b.volume("Macintosh HD").add_file("hello.txt", b"system");
    b.volume("Macintosh HD - Data")
        .snapshots(2)
        .add_file("orig.bin", &vec![7u8; 5_000])
        .add_clone("copy.bin", "orig.bin");
    b.volume("Vault").encrypted().add_file("secret.txt", b"cipher");
    b.build()
}

fn list(a: &mut Archive) -> Vec<(String, bool, u64)> {
    let mut v = Vec::new();
    while let Some(e) = a.next_header().unwrap() {
        v.push((e.disp.clone(), e.is_dir, e.size));
    }
    v
}

fn extract_all(img: &Path, out: &Path) -> Vec<String> {
    let mut a = Archive::open(img).unwrap();
    let mut names = Vec::new();
    while let Some(e) = a.next_header().unwrap() {
        // `\` separates the components in the panel path; on the Linux test host it must become `/`
        assert_eq!(a.process(PK_EXTRACT, Some(&out.join(e.disp.replace('\\', "/")))), E_OK, "{}", e.disp);
        names.push(e.disp);
    }
    names
}

#[test]
fn single_volume_is_shown_at_the_root_and_extracts() {
    let dir = temp_dir();
    let img = dir.join("data.apfs");
    fs::write(&img, single_volume()).unwrap();

    let items = list(&mut Archive::open(&img).unwrap());
    let names: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    for n in ["docs", "docs\\hello.txt", "big.bin", "a_b_.txt", "R\u{e9}sum\u{e9}.txt"] {
        assert!(names.contains(&n), "{n}: {names:?}");
    }
    assert!(!names.iter().any(|n| n.contains("APFS-")), "no notices on a plain volume: {names:?}");
    assert_eq!(items.iter().find(|i| i.0 == "big.bin").unwrap().2, 600_000);

    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    extract_all(&img, &out);
    assert_eq!(fs::read(out.join("docs").join("hello.txt")).unwrap(), b"hello apfs");
    assert_eq!(fs::read(out.join("big.bin")).unwrap(), vec![0xAB; 600_000]);
    assert_eq!(fs::read(out.join("a_b_.txt")).unwrap(), b"illegal chars");
    assert_eq!(fs::read(out.join("R\u{e9}sum\u{e9}.txt")).unwrap(), b"nfd name");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn multi_volume_container_with_notices() {
    let dir = temp_dir();
    let img = dir.join("mac.apfs");
    fs::write(&img, multi_volume()).unwrap();

    let items = list(&mut Archive::open(&img).unwrap());
    let names: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    for n in [
        "Partition 1 - Macintosh HD",
        "Partition 1 - Macintosh HD\\hello.txt",
        "Partition 1 - Macintosh HD - Data\\orig.bin",
        "Partition 1 - Macintosh HD - Data\\copy.bin",
        "Partition 1 - Macintosh HD - Data\\!APFS-NOTICE.txt", // snapshots
        "Partition 1 - Macintosh HD - Data\\!APFS-CLONES.txt", // clones, listed after the walk
        "Partition 1 - Vault\\!APFS-NOTICE.txt",               // encrypted
    ] {
        assert!(names.contains(&n), "{n}: {names:?}");
    }
    // the encrypted volume shows nothing but its notice
    assert!(!names.iter().any(|n| n.contains("secret.txt")), "{names:?}");
    assert_eq!(names.iter().filter(|n| n.starts_with("Partition 1 - Vault\\")).count(), 1);
    // the system volume is clean
    assert!(!names.iter().any(|n| n.starts_with("Partition 1 - Macintosh HD\\!")), "{names:?}");
    let clones_pos = names.iter().position(|n| n.ends_with("!APFS-CLONES.txt")).unwrap();
    assert_eq!(clones_pos, names.len() - 1, "clones notice comes after the walk");

    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    extract_all(&img, &out);
    let data = out.join("Partition 1 - Macintosh HD - Data");
    assert_eq!(fs::read(data.join("copy.bin")).unwrap(), vec![7u8; 5_000]);
    assert_eq!(fs::read(data.join("orig.bin")).unwrap(), vec![7u8; 5_000]);
    let notice = fs::read_to_string(data.join("!APFS-NOTICE.txt")).unwrap();
    assert!(notice.contains("2 snapshot(s)") && notice.contains("not supported yet"), "{notice}");
    let clones = fs::read_to_string(data.join("!APFS-CLONES.txt")).unwrap();
    assert!(clones.contains("cloned files"), "{clones}");
    let vault = fs::read_to_string(out.join("Partition 1 - Vault").join("!APFS-NOTICE.txt")).unwrap();
    assert!(vault.contains("is encrypted"), "{vault}");
    assert_eq!(fs::read(out.join("Partition 1 - Macintosh HD").join("hello.txt")).unwrap(), b"system");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn apfsdev_link_file() {
    let dir = temp_dir();
    let img = dir.join("mac.img");
    fs::write(&img, multi_volume()).unwrap();
    let link = dir.join("mac.apfsdev");
    fs::write(&link, format!("device = {}\n", img.display())).unwrap();
    let names: Vec<String> = list(&mut Archive::open(&link).unwrap()).into_iter().map(|i| i.0).collect();
    assert!(names.contains(&"Partition 1 - Macintosh HD\\hello.txt".to_string()), "{names:?}");

    // offset 0 + explicit partition index are honoured for APFS too
    fs::write(&link, format!("device = {}\noffset = 0\n", img.display())).unwrap();
    assert!(Archive::open(&link).is_ok());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn container_without_volumes_is_a_bad_archive() {
    let dir = temp_dir();
    let img = dir.join("empty.apfs");
    fs::write(&img, ApfsImageBuilder::new().build()).unwrap();
    let Err(e) = Archive::open(&img) else { panic!("an empty container must not open") };
    assert!(matches!(e, OpenError::Fs(_)));
    assert_eq!(e.code(), E_BAD_ARCHIVE);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn exported_functions_recognise_apfs() {
    let dir = temp_dir();
    let img = dir.join("mac.apfs");
    fs::write(&img, multi_volume()).unwrap();
    let w = to_wide_nul(&img.to_string_lossy());
    unsafe {
        assert_eq!(CanYouHandleThisFileW(w.as_ptr()), 1);
        let mut data = OpenArchiveDataW {
            ArcName: w.as_ptr() as *mut u16,
            OpenMode: PK_OM_EXTRACT,
            OpenResult: -1,
            CmtBuf: std::ptr::null_mut(),
            CmtBufSize: 0,
            CmtSize: 0,
            CmtState: 0,
        };
        let h = OpenArchiveW(&mut data);
        assert!(!h.is_null());
        assert_eq!(data.OpenResult, E_OK);
        let mut seen = 0;
        loop {
            let mut hd: Box<HeaderDataExW> = Box::new(std::mem::zeroed());
            let rc = ReadHeaderExW(h, &mut *hd);
            if rc == E_END_ARCHIVE {
                break;
            }
            assert_eq!(rc, E_OK);
            assert_eq!(ProcessFileW(h, PK_SKIP, std::ptr::null(), std::ptr::null()), E_OK);
            seen += 1;
        }
        assert!(seen >= 8, "{seen}");
        assert_eq!(CloseArchive(h), E_OK);
    }
    fs::remove_dir_all(&dir).unwrap();
}
