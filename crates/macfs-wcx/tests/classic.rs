//! Classic HFS and MFS volumes through the WCX plugin core, on synthetic images
//! (`hfs_reader::testutil`, the `macfs` crate).

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use hfs_reader::testutil::{HfsImageBuilder, ROOT};
use macfs::{ImageFormat, MfsVolume};
use macfs_wcx::archive::Archive;
use macfs_wcx::ffi::*;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-wcx-classic-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    d
}

fn extract_all(img: &std::path::Path, out: &std::path::Path) -> Vec<(String, bool, u64)> {
    let mut a = Archive::open(img).unwrap();
    let mut v = Vec::new();
    while let Some(e) = a.next_header().unwrap() {
        assert_eq!(a.process(PK_EXTRACT, Some(&out.join(e.disp.replace('\\', "/")))), E_OK, "{}", e.disp);
        v.push((e.disp.clone(), e.is_dir, e.size));
    }
    v
}

#[test]
fn hfs_floppy_extracts_with_resource_forks() {
    let dir = temp_dir();
    let mut b = HfsImageBuilder::new(b"Disk 1");
    let f = b.add_dir(ROOT, b"Syst\x8Em Folder");
    b.add_dir(ROOT, b"Empty");
    b.add_file(f, b"Finder", b"data", &vec![0x5A; 3000]);
    b.add_file(ROOT, b"A/B", b"slash", b"");
    let img = dir.join("disk1.hfs");
    fs::write(&img, b.build()).unwrap();

    let out = dir.join("out");
    let items = extract_all(&img, &out);
    let names: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    for n in ["Empty", "Syst\u{e9}m Folder", "Syst\u{e9}m Folder\\Finder", "Syst\u{e9}m Folder\\Finder.rsrc", "A_B"] {
        assert!(names.contains(&n), "{n} in {names:?}");
    }
    assert!(out.join("Empty").is_dir());
    assert_eq!(fs::read(out.join("Syst\u{e9}m Folder").join("Finder")).unwrap(), b"data");
    assert_eq!(fs::read(out.join("Syst\u{e9}m Folder").join("Finder.rsrc")).unwrap(), vec![0x5A; 3000]);
    assert_eq!(fs::read(out.join("A_B")).unwrap(), b"slash");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn mfs_floppy_in_disk_copy_container_extracts() {
    let dir = temp_dir();
    let mut v = MfsVolume::format(MfsVolume::FLOPPY_800K, "System", ImageFormat::DiskCopy42).unwrap();
    v.add_file("Read Me", *b"TEXT", *b"MACA", b"hello from 1986", b"rsrc").unwrap();
    let mut out = Cursor::new(Vec::new());
    v.save_to(&mut out).unwrap();
    let img = dir.join("system.image");
    fs::write(&img, out.into_inner()).unwrap();

    let out = dir.join("out");
    let items = extract_all(&img, &out);
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(fs::read(out.join("Read Me")).unwrap(), b"hello from 1986");
    assert_eq!(fs::read(out.join("Read Me.rsrc")).unwrap(), b"rsrc");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn sparse_bundle_opens_through_info_plist_and_sparse_image_directly() {
    let dir = temp_dir();
    let mut b = HfsImageBuilder::new(b"Sparse");
    b.add_file(ROOT, b"Read Me", b"hello sparse", b"");
    let mut raw = b.build();
    raw.resize((raw.len() + 200_000).next_multiple_of(512), 0);

    let bundle = dir.join("Backup.sparsebundle");
    macfs_core::testutil::write_sparsebundle(&bundle, &raw, 16 * 1024);
    let img = dir.join("disk.sparseimage");
    fs::write(&img, macfs_core::testutil::sparse_image(&raw, 8)).unwrap();

    for (src, out) in [(bundle.join("Info.plist"), dir.join("out1")), (img, dir.join("out2"))] {
        let items = extract_all(&src, &out);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(fs::read(out.join("Read Me")).unwrap(), b"hello sparse");
    }
    // the plugin recognises it by content, so a user can associate `plist` safely: other plists are not archives
    let other = dir.join("Other.plist");
    fs::write(&other, "<plist><dict/></plist>").unwrap();
    assert!(Archive::open(&other).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn mac_file_containers_extract_forks_and_info() {
    use macfs_core::testutil::{apple_double, binhex, finder_info, macbinary};
    let dir = temp_dir();
    let data = vec![0x41u8; 900];
    let cases: Vec<(&str, Vec<u8>, &str, &str)> = vec![
        ("letter.hqx", binhex(b"Letter", b"TEXT", b"ttxt", 0, &data, b"RSRC").into_bytes(), "!BINHEX-INFO.txt", "Letter"),
        ("letter.bin", macbinary(b"Letter", b"TEXT", b"ttxt", 0, &data, b"RSRC", b""), "!MACBINARY-INFO.txt", "Letter"),
        ("._Letter", apple_double(false, &[(9, finder_info(b"TEXT", b"ttxt", 0)), (2, b"RSRC".to_vec())]), "!APPLEDOUBLE-INFO.txt", ""),
    ];
    for (file, bytes, info_name, data_name) in cases {
        let img = dir.join(file);
        fs::write(&img, bytes).unwrap();
        let out = dir.join(format!("out-{file}"));
        let items = extract_all(&img, &out);
        let names: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
        assert!(names.contains(&info_name), "{file}: {names:?}");
        let info = fs::read_to_string(out.join(info_name)).unwrap();
        assert!(info.contains("'TEXT'") && info.contains("'ttxt'"), "{file}\n{info}");
        assert_eq!(fs::read(out.join("Letter.rsrc")).unwrap(), b"RSRC", "{file}");
        if !data_name.is_empty() {
            assert_eq!(fs::read(out.join(data_name)).unwrap(), data, "{file}");
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}
