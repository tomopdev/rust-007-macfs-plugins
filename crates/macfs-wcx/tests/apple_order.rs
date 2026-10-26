//! Regression test for "N directories could not be read (damaged catalog)".
//!
//! On a real case-insensitive HFS+ volume the catalog is ordered by Apple's
//! FastUnicodeCompare: U+0000 folds to 0xFFFF, so the hard-link folder named
//! "\0\0\0\0HFS+ Private Data" sorts AFTER every ordinary name of its directory,
//! and ignorable characters (U+200C..200F, ...) are skipped when comparing.
//! The synthetic images of the `hfsplus` crate are HFSX (binary order), so this
//! test turns one into an HFS+ volume and rewrites the leaf node in Apple order.

use std::fs;
use std::path::PathBuf;

use hfsplus::testutil::HfsPlusImageBuilder;
use macfs_wcx::archive::Archive;
use macfs_wcx::ffi::{E_OK, PK_TEST};

const PRIVATE: &str = "\0\0\0\0HFS+ Private Data";

fn be16(b: &[u8], o: usize) -> usize {
    u16::from_be_bytes([b[o], b[o + 1]]) as usize
}

/// HFSX test image -> HFS+ image whose root-directory records follow Apple's order:
/// names beginning with NUL are moved behind all other records of the root (CNID 2).
fn apple_ordered(mut img: Vec<u8>) -> Vec<u8> {
    img[1024..1026].copy_from_slice(&0x482Bu16.to_be_bytes()); // "H+"
    img[1026..1028].copy_from_slice(&4u16.to_be_bytes()); // version 4
    let node = &mut img[3 * 4096..4 * 4096]; // catalog leaf node (block 3 of the builder's layout)
    assert_eq!(node[8], 0xFF, "leaf node expected");
    let n = be16(node, 10);
    let offs: Vec<usize> = (0..=n).map(|i| be16(node, 4096 - 2 * (i + 1))).collect();
    let recs: Vec<Vec<u8>> = (0..n).map(|i| node[offs[i]..offs[i + 1]].to_vec()).collect();

    let parent = |r: &[u8]| u32::from_be_bytes([r[2], r[3], r[4], r[5]]);
    let nul_first = |r: &[u8]| be16(r, 6) > 0 && r[8] == 0 && r[9] == 0;
    let (moved, mut kept): (Vec<_>, Vec<_>) = recs.into_iter().partition(|r| parent(r) == 2 && nul_first(r));
    assert!(!moved.is_empty(), "no NUL-named record found");
    let last_root = kept.iter().rposition(|r| parent(r) == 2).expect("root records");
    for (k, r) in moved.into_iter().enumerate() {
        kept.insert(last_root + 1 + k, r);
    }

    // rewrite records + offset table
    for b in &mut node[14..] {
        *b = 0;
    }
    let mut pos = 14;
    let mut new_offs = Vec::new();
    for r in &kept {
        new_offs.push(pos);
        node[pos..pos + r.len()].copy_from_slice(r);
        pos += r.len();
    }
    new_offs.push(pos);
    for (i, o) in new_offs.iter().enumerate() {
        node[4096 - 2 * (i + 1)..4096 - 2 * i].copy_from_slice(&(*o as u16).to_be_bytes());
    }
    img
}

fn temp_file(name: &str, data: &[u8]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-spl-order-{}-{}", std::process::id(), name));
    fs::create_dir_all(&d).unwrap();
    let f = d.join("vol.hfs");
    fs::write(&f, data).unwrap();
    f
}

#[test]
fn private_data_name_is_found_in_apple_order() {
    let img = HfsPlusImageBuilder::new()
        .add_file("apple.txt", b"A", 0o644)
        .add_file("hello.txt", b"H", 0o644)
        .add_file("zebra.txt", b"Z", 0o644)
        .add_file(PRIVATE, b"private", 0o644)
        .build();
    let file = temp_file("private", &apple_ordered(img));

    let mut a = Archive::open(&file).expect("open");
    let mut names = Vec::new();
    while let Some(e) = a.next_header().expect("listing") {
        // every item must be readable by path, including the NUL-prefixed one
        assert_eq!(a.process(PK_TEST, None), E_OK, "{}", e.disp);
        names.push(e.disp);
    }
    assert_eq!(names.len(), 4, "{names:?}");
    assert!(names.iter().any(|n| n == "____HFS+ Private Data"), "{names:?}");
    fs::remove_dir_all(file.parent().unwrap()).unwrap();
}
