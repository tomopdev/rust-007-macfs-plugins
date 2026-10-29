//! Tests of the patched reader on volumes made by `testutil::HfsImageBuilder`.

use hfs_reader::testutil::{CREATED, HfsImageBuilder, MODIFIED, ROOT};
use hfs_reader::{HfsError, HfsVolume, decode_mac_roman};
use std::io::Cursor;

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

#[test]
fn lists_names_dates_sizes_forks_and_empty_dirs() {
    let mut b = HfsImageBuilder::new(b"Caf\x8E Disk");
    let docs = b.add_dir(ROOT, b"Docs");
    let empty = b.add_dir(ROOT, b"Empty");
    let deep = b.add_dir(docs, b"Deep/er");
    b.add_file(ROOT, b"Read Me", b"hello classic mac", b"");
    b.add_file(docs, b"Na\x95ve \xC9", b"naive", &pattern(3000, 5));
    b.add_file(deep, b"a", &pattern(2500, 1), b"R");
    let img = b.build();

    let mut v = HfsVolume::parse(&img).unwrap();
    assert_eq!(v.volume_name, "Caf\u{e9} Disk");
    assert_eq!(v.files.len(), 3);
    let mut dirs: Vec<_> = v.dirs.iter().map(|d| (d.rel_path.to_string_lossy().replace('\\', "/"), d.cnid)).collect();
    dirs.sort();
    assert_eq!(dirs, vec![("Docs".to_string(), docs), ("Docs/Deep:er".to_string(), deep), ("Empty".to_string(), empty)]);
    assert!(v.dirs.iter().all(|d| d.modify_date == MODIFIED && d.create_date == CREATED));

    let files = std::mem::take(&mut v.files);
    let by_name = |n: &str| files.iter().find(|f| f.name == n).unwrap_or_else(|| panic!("{n}"));
    let readme = by_name("Read Me");
    assert_eq!((readme.data_size(), readme.rsrc_size(), readme.modify_date), (17, 0, MODIFIED));
    assert_eq!(&readme.file_type, b"TEXT");
    assert_eq!(v.read_data_fork(readme).unwrap(), b"hello classic mac");
    assert_eq!(v.read_rsrc_fork(readme).unwrap(), b"");

    let naive = by_name("Na\u{ef}ve \u{2026}");
    assert_eq!(naive.rel_path.to_string_lossy().replace('\\', "/"), "Docs/Na\u{ef}ve \u{2026}");
    assert_eq!(v.read_data_fork(naive).unwrap(), b"naive");
    assert_eq!(v.read_rsrc_fork(naive).unwrap(), pattern(3000, 5));

    let a = by_name("a");
    assert_eq!(a.rel_path.to_string_lossy().replace('\\', "/"), "Docs/Deep:er/a");
    let mut out = Vec::new();
    assert_eq!(v.copy_data_fork(a, &mut out).unwrap(), 2500);
    assert_eq!(out, pattern(2500, 1));
}

#[test]
fn many_entries_use_several_leaves_and_an_index_node() {
    let mut b = HfsImageBuilder::new(b"Big");
    let d = b.add_dir(ROOT, b"Folder");
    let mut expect = Vec::new();
    for i in 0..28 {
        let name = format!("file number {i:03}");
        let data = pattern(i * 37 % 1500, i as u8);
        b.add_file(if i % 2 == 0 { ROOT } else { d }, name.as_bytes(), &data, b"");
        expect.push((name, data));
    }
    let img = b.build();
    let mut v = HfsVolume::parse(&img).unwrap();
    assert_eq!(v.files.len(), 28);
    let files = std::mem::take(&mut v.files);
    for (name, data) in expect {
        let f = files.iter().find(|f| f.name == name).unwrap();
        assert_eq!(v.read_data_fork(f).unwrap(), data, "{name}");
    }
}

#[test]
fn fragmented_forks_are_followed_through_the_overflow_file() {
    for bs in [512, 1024, 4096] {
        let mut b = HfsImageBuilder::new(b"Frag");
        b.block_size(bs);
        let data = pattern(bs * 8 + 100, 9);
        let rsrc = pattern(bs * 7, 77);
        let f = b.add_file(ROOT, b"fragmented", &data, &rsrc);
        b.fragment(f);
        let g = b.add_file(ROOT, b"next to it", &pattern(bs * 2, 3), b"");
        let _ = g;
        let img = b.build();
        let mut v = HfsVolume::parse(&img).unwrap();
        let files = std::mem::take(&mut v.files);
        let f = files.iter().find(|f| f.name == "fragmented").unwrap();
        assert_eq!(v.read_data_fork(f).unwrap(), data, "data, block size {bs}");
        assert_eq!(v.read_rsrc_fork(f).unwrap(), rsrc, "rsrc, block size {bs}");
        let g = files.iter().find(|f| f.name == "next to it").unwrap();
        assert_eq!(v.read_data_fork(g).unwrap(), pattern(bs * 2, 3));
    }
}

#[test]
fn disk_copy_42_header_is_stripped_by_parse() {
    let mut b = HfsImageBuilder::new(b"DC42");
    b.add_file(ROOT, b"f", b"inside dc42", b"");
    let raw = b.build();
    let mut dc = vec![0u8; 84];
    dc[0] = 4;
    dc[1..5].copy_from_slice(b"DC42");
    dc[64..68].copy_from_slice(&(raw.len() as u32).to_be_bytes());
    dc[82..84].copy_from_slice(&[1, 0]);
    dc.extend_from_slice(&raw);
    let mut v = HfsVolume::parse(&dc).unwrap();
    let f = std::mem::take(&mut v.files);
    assert_eq!(v.read_data_fork(&f[0]).unwrap(), b"inside dc42");
}

#[test]
fn rejects_other_formats_and_survives_damage() {
    let mut img = HfsImageBuilder::new(b"X").build();
    let mut plus = img.clone();
    plus[1024..1026].copy_from_slice(&0x482Bu16.to_be_bytes());
    assert!(matches!(HfsVolume::parse(&plus), Err(HfsError::Format(m)) if m.contains("HFS+")));
    let mut mfs = img.clone();
    mfs[1024..1026].copy_from_slice(&0xD2D7u16.to_be_bytes());
    assert!(matches!(HfsVolume::parse(&mfs), Err(HfsError::Format(m)) if m.contains("MFS")));
    assert!(HfsVolume::parse(&img[..1500]).is_err());

    // every possible single-byte corruption of the first 3 KiB must give an error or a volume, never a panic / hang
    let good = img.clone();
    for pos in (0..3072.min(good.len())).step_by(3) {
        for val in [0u8, 0xFF, 0x80] {
            img.copy_from_slice(&good);
            img[pos] = val;
            if let Ok(mut v) = HfsVolume::open(Cursor::new(&img)) {
                let files = std::mem::take(&mut v.files);
                for f in &files {
                    let _ = v.copy_data_fork(f, &mut std::io::sink());
                    let _ = v.copy_rsrc_fork(f, &mut std::io::sink());
                }
            }
        }
    }
}

#[test]
fn truncated_fork_is_an_error_not_short_data() {
    let mut b = HfsImageBuilder::new(b"T");
    b.add_file(ROOT, b"big", &pattern(5000, 1), b"");
    let mut img = b.build();
    let mut v = HfsVolume::open(Cursor::new(img.clone())).unwrap();
    let f = std::mem::take(&mut v.files);
    // cut the image inside the data of the file
    img.truncate(4 * 512 + 2000);
    let mut v2 = HfsVolume::open(Cursor::new(img));
    // the catalog lives behind the data, so the volume is already unusable
    assert!(v2.is_err() || v2.as_mut().map(|v| v.read_data_fork(&f[0]).is_err()).unwrap_or(true));
}

#[test]
fn mac_roman_decoding() {
    assert_eq!(decode_mac_roman(b"A\x80\xA5\xDB\xF0"), "A\u{c4}\u{2022}\u{20ac}\u{f8ff}");
    assert_eq!(decode_mac_roman(b"\x01a\x7f"), "_a_");
}
