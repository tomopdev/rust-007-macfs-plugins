//! Sparse images (`.sparseimage`) and sparse bundles through `detect` + `volume`, on a classic HFS volume
//! (`hfs_reader::testutil`) cut into bands by `macfs_core::testutil`.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use hfs_reader::testutil::{HfsImageBuilder, ROOT};
use macfs_core::detect::{self, FsKind, LocateError};
use macfs_core::testutil::{sparse_image, write_sparsebundle};
use macfs_core::volume;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-sparse-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    d
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed)).collect()
}

/// Classic HFS disk with some real content and a long run of zeros (holes) behind it.
fn disk(extra_zeros: usize) -> Vec<u8> {
    let mut b = HfsImageBuilder::new(b"Sparse");
    let d = b.add_dir(ROOT, b"Folder");
    b.add_file(ROOT, b"small", b"hello sparse", b"");
    b.add_file(d, b"big", &pattern(70_000, 4), b"rsrc");
    let mut raw = b.build();
    raw.resize((raw.len() + extra_zeros).next_multiple_of(512), 0);
    raw
}

fn check_volume(path: &std::path::Path) {
    let (src, parts) = detect::locate(path).unwrap_or_else(|e| panic!("{path:?}: {e:?}"));
    assert_eq!(parts.len(), 1, "{path:?}");
    assert_eq!(parts[0].kind, FsKind::Hfs);
    let src = Arc::new(Mutex::new(src));
    let mut vols = volume::open_all(&src, &parts).unwrap();
    let v = &mut vols[0].vol;
    let mut names: Vec<String> = v.list_directory("/").unwrap().into_iter().map(|n| n.name).collect();
    names.sort();
    assert_eq!(names, ["Folder", "small"]);
    let mut out = Vec::new();
    v.read_file_to("/small", &mut out).unwrap();
    assert_eq!(out, b"hello sparse");
    out.clear();
    v.read_file_to("/Folder/big", &mut out).unwrap();
    assert_eq!(out, pattern(70_000, 4));
    out.clear();
    v.read_file_to("/Folder/big.rsrc", &mut out).unwrap();
    assert_eq!(out, b"rsrc");
}

#[test]
fn sparseimage_is_read_through_its_band_map() {
    let dir = temp_dir();
    let raw = disk(300_000);
    for band_sectors in [8u32, 2048] {
        let f = dir.join(format!("disk{band_sectors}.sparseimage"));
        fs::write(&f, sparse_image(&raw, band_sectors)).unwrap();
        check_volume(&f);
        // the whole virtual disk, also over the holes, equals the original
        let (mut src, _) = detect::locate(&f).unwrap();
        assert_eq!(src.len(), raw.len() as u64);
        let mut all = vec![0u8; raw.len()];
        src.read_exact_at(0, &mut all).unwrap();
        assert!(all == raw, "content differs, band {band_sectors}");
    }
}

#[test]
fn a_band_map_longer_than_the_header_makes_it_grow() {
    let dir = temp_dir();
    let raw = disk(700_000); // > 1008 bands of one sector
    let f = dir.join("many.sparseimage");
    let img = sparse_image(&raw, 1);
    assert!(img[..4096].iter().skip(64).any(|&b| b != 0) && img.len() > 8192);
    fs::write(&f, img).unwrap();
    check_volume(&f);
}

#[test]
fn sparsebundle_folder_info_plist_and_devlink_all_open() {
    let dir = temp_dir();
    let bundle = dir.join("Backup.sparsebundle");
    let raw = disk(2_000_000);
    write_sparsebundle(&bundle, &raw, 64 * 1024);
    assert!(bundle.join("bands").join("0").is_file());
    check_volume(&bundle);
    check_volume(&bundle.join("Info.plist"));
    check_volume(&bundle.join("token"));
    let link = dir.join("backup.macfsdev");
    fs::write(&link, format!("device = {}\n", bundle.display())).unwrap();
    check_volume(&link);

    // hole bands are zeros, band files that are shorter than a band are padded with zeros
    let (mut src, _) = detect::locate(&bundle).unwrap();
    assert_eq!(src.len(), raw.len() as u64);
    let mut tail = vec![1u8; 4096];
    src.read_exact_at(raw.len() as u64 - 4096, &mut tail).unwrap();
    assert!(tail.iter().all(|&b| b == 0));
    let mut crossing = vec![0u8; 100_000];
    src.read_exact_at(60_000, &mut crossing).unwrap();
    assert!(crossing == raw[60_000..160_000]);
}

#[test]
fn encrypted_and_damaged_sparse_images_are_reported() {
    let dir = temp_dir();
    let raw = disk(10_000);

    let bundle = dir.join("Enc.sparsebundle");
    write_sparsebundle(&bundle, &raw, 4096);
    fs::write(bundle.join("token"), b"encrcdsa\0\0\0\0").unwrap();
    match detect::locate(&bundle) {
        Err(LocateError::Unsupported(m)) => assert!(m.contains("encrypted"), "{m}"),
        other => panic!("{:?}", other.map(|_| ())),
    }

    let enc = dir.join("enc.sparseimage");
    let mut head = b"encrcdsa".to_vec();
    head.resize(4096, 0);
    fs::write(&enc, head).unwrap();
    assert!(matches!(detect::locate(&enc), Err(LocateError::Unsupported(m)) if m.contains("encrypted")));

    // the same band named twice
    let mut img = sparse_image(&raw, 8);
    let first = img[64..68].to_vec();
    img[68..72].copy_from_slice(&first);
    let dup = dir.join("dup.sparseimage");
    fs::write(&dup, img).unwrap();
    assert!(matches!(detect::locate(&dup), Err(LocateError::Unsupported(m)) if m.contains("twice")));

    // zero geometry
    let zero = dir.join("zero.sparseimage");
    let mut z = b"sprs".to_vec();
    z.resize(4096, 0);
    fs::write(&zero, z).unwrap();
    assert!(matches!(detect::locate(&zero), Err(LocateError::Unsupported(_))));

    // cut-off file: reads as zeros past the end, never panics
    let cut = dir.join("cut.sparseimage");
    let img = sparse_image(&raw, 8);
    fs::write(&cut, &img[..img.len() - 3000]).unwrap();
    if let Ok((mut src, _)) = detect::locate(&cut) {
        let mut b = vec![0u8; raw.len()];
        let _ = src.read_exact_at(0, &mut b);
    }

    // a plist that is not a sparse bundle is just a file
    let other = dir.join("Other");
    fs::create_dir_all(other.join("bands")).unwrap();
    fs::write(other.join("Info.plist"), "<plist><dict><key>size</key><integer>1</integer></dict></plist>").unwrap();
    let (_, parts) = detect::locate(&other.join("Info.plist")).unwrap();
    assert!(parts.is_empty());
}
