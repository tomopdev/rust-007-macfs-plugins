//! AppleDouble / AppleSingle, MacBinary and BinHex files through `detect` + `volume`.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use macfs_core::detect::{self, FsKind};
use macfs_core::testutil::{MAC_TIME, apple_double, binhex, finder_info, macbinary, macbinary1};
use macfs_core::volume::{self, MacVolume};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_file(bytes: &[u8], name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-macfile-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    let f = d.join(name);
    fs::write(&f, bytes).unwrap();
    f
}

fn open(bytes: &[u8], file: &str) -> (FsKind, MacVolume) {
    let f = temp_file(bytes, file);
    let (src, parts) = detect::locate(&f).unwrap();
    assert_eq!(parts.len(), 1, "{file}");
    let kind = parts[0].kind;
    let src = Arc::new(Mutex::new(src));
    let mut v = volume::open_all(&src, &parts).unwrap();
    (kind, v.remove(0).vol)
}

/// Info text with runs of blanks collapsed (the label column width is not under test).
fn norm(bytes: Vec<u8>) -> String {
    let text = String::from_utf8(bytes).unwrap();
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.chars() {
        if c == ' ' {
            if !prev_space {
                out.push(c);
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

fn names(v: &mut MacVolume) -> Vec<String> {
    let mut n: Vec<String> = v.list_directory("/").unwrap().into_iter().map(|n| n.name).collect();
    n.sort();
    n
}

fn read(v: &mut MacVolume, path: &str) -> Vec<u8> {
    let mut out = Vec::new();
    v.read_file_to(path, &mut out).unwrap();
    out
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(11).wrapping_add(seed)).collect()
}

#[test]
fn macbinary_iii_shows_forks_and_info() {
    let data = pattern(1000, 1);
    let rsrc = pattern(300, 2);
    let img = macbinary(b"Caf\x8E Doc", b"TEXT", b"ttxt", 0x4100, &data, &rsrc, b"my comment");
    let (kind, mut v) = open(&img, "doc.bin");
    assert_eq!(kind, FsKind::MacBinary);
    assert_eq!(v.name(), "Caf\u{e9} Doc");
    assert_eq!(names(&mut v), ["!MACBINARY-INFO.txt", "Caf\u{e9} Doc", "Caf\u{e9} Doc.rsrc"]);
    assert_eq!(read(&mut v, "/Caf\u{e9} Doc"), data);
    assert_eq!(read(&mut v, "/Caf\u{e9} Doc.rsrc"), rsrc);
    let info = norm(read(&mut v, "/!MACBINARY-INFO.txt"));
    for want in ["MacBinary III file", "Caf\u{e9} Doc", "'TEXT' / 'ttxt'", "0x4100 (invisible, has been inited)", "data 1000 bytes, resource 300 bytes", "2023-11-14 22:13:20", "my comment", "Header CRC: OK"] {
        assert!(info.contains(want), "{want} in\n{info}");
    }
    let nodes = v.list_directory("/").unwrap();
    assert_eq!(nodes.iter().find(|n| n.name == "Caf\u{e9} Doc").unwrap().mtime, Some(MAC_TIME as i64 - 2_082_844_800));
}

#[test]
fn macbinary_i_and_resource_only_files() {
    let img = macbinary1(b"Old", b"APPL", b"MACS", b"", &pattern(500, 3));
    let (kind, mut v) = open(&img, "old.macbin");
    assert_eq!(kind, FsKind::MacBinary);
    // no data fork: only the resource fork and the info are listed
    assert_eq!(names(&mut v), ["!MACBINARY-INFO.txt", "Old.rsrc"]);
    let info = norm(read(&mut v, "/!MACBINARY-INFO.txt"));
    assert!(info.contains("MacBinary I file") && info.contains("MacBinary I has none"));
    // an empty file still shows its (empty) data fork
    let (_, mut v) = open(&macbinary(b"Empty", b"????", b"????", 0, b"", b"", b""), "e.bin");
    assert_eq!(names(&mut v), ["!MACBINARY-INFO.txt", "Empty"]);
}

#[test]
fn macbinary_is_not_confused_with_other_files() {
    let f = temp_file(&vec![0u8; 4096], "zeros.bin");
    assert!(detect::locate(&f).unwrap().1.is_empty());
    let f = temp_file(&pattern(5000, 9), "noise.bin");
    assert!(detect::locate(&f).unwrap().1.is_empty());
    // fork lengths that do not fit into the file
    let mut img = macbinary(b"X", b"TEXT", b"ttxt", 0, b"abc", b"", b"");
    img[83..87].copy_from_slice(&1_000_000u32.to_be_bytes());
    let f = temp_file(&img, "lying.bin");
    assert!(detect::locate(&f).unwrap().1.is_empty());
}

#[test]
fn binhex_decodes_forks_and_checks_crcs() {
    // runs of equal bytes and 0x90 bytes exercise the run-length coding
    let mut data = pattern(700, 5);
    data.extend_from_slice(&[0x90, 0x90, 0x90, 0x90, 0x90, 7, 7, 7, 7, 7, 7, 7, 7, 0x90, 1, 2]);
    data.extend(vec![0u8; 400]);
    let rsrc = pattern(257, 6);
    let hqx = binhex(b"Read Me", b"TEXT", b"ttxt", 0x0100, &data, &rsrc);
    let (kind, mut v) = open(hqx.as_bytes(), "readme.hqx");
    assert_eq!(kind, FsKind::BinHex);
    assert_eq!(names(&mut v), ["!BINHEX-INFO.txt", "Read Me", "Read Me.rsrc"]);
    assert_eq!(read(&mut v, "/Read Me"), data);
    assert_eq!(read(&mut v, "/Read Me.rsrc"), rsrc);
    let info = norm(read(&mut v, "/!BINHEX-INFO.txt"));
    for want in ["BinHex 4.0 file", "'TEXT' / 'ttxt'", "Header CRC: OK", "Data fork CRC: OK", "Resource fork CRC: OK"] {
        assert!(info.contains(want), "{want} in\n{info}");
    }

    // damage in the data fork: still opens, the CRC says so
    let mut bad = hqx.clone().into_bytes();
    let at = bad.len() / 2;
    bad[at] = if bad[at] == b'a' { b'b' } else { b'a' };
    let (_, mut v) = open(&bad, "damaged.hqx");
    let info = norm(read(&mut v, "/!BINHEX-INFO.txt"));
    assert!(info.contains("MISMATCH"), "{info}");

    // preamble before the marker, no trailing colon -> refused with a message
    let mixed = format!("From: someone\r\n\r\n{hqx}");
    assert_eq!(open(mixed.as_bytes(), "mail.hqx").0, FsKind::BinHex);
    let cut = &hqx[..hqx.len() - 6];
    let f = temp_file(cut.as_bytes(), "cut.hqx");
    let (src, parts) = detect::locate(&f).unwrap();
    let src = Arc::new(Mutex::new(src));
    assert!(volume::open_all(&src, &parts).is_err());
}

#[test]
fn apple_double_header_file_describes_the_metadata() {
    let mut dates = Vec::new();
    for v in [100_000_000i32, 100_000_060, i32::MIN, 100_000_120] {
        dates.extend_from_slice(&v.to_be_bytes());
    }
    let mut icon = vec![0u8; 128];
    icon[0..4].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    icon[4..8].copy_from_slice(&0x8000_0001u32.to_be_bytes());
    let rsrc = pattern(2048, 1);
    let img = apple_double(
        false,
        &[
            (3, b"Photo \x8E.jpg".to_vec()),
            (4, b"scanned in 1999".to_vec()),
            (5, icon),
            (8, dates),
            (9, finder_info(b"JPEG", b"8BIM", 0x0400)),
            (10, vec![0, 0, 0, 1]),
            (11, vec![0, 0xC3, 0, 0x06, 0, 0, 0x20, 0]),
            (12, vec![0, 0x21]),
            (14, vec![0, 0, 0, 0x41]),
            (2, rsrc.clone()),
        ],
    );
    let (kind, mut v) = open(&img, "._Photo.jpg");
    assert_eq!(kind, FsKind::AppleDouble);
    assert_eq!(v.name(), "Photo \u{e9}.jpg");
    assert_eq!(names(&mut v), ["!APPLEDOUBLE-INFO.txt", "Photo \u{e9}.jpg.rsrc"]);
    assert_eq!(read(&mut v, "/Photo \u{e9}.jpg.rsrc"), rsrc);
    let info = norm(read(&mut v, "/!APPLEDOUBLE-INFO.txt"));
    for want in [
        "AppleDouble file (RFC 1740)",
        "Photo \u{e9}.jpg",
        "'JPEG' / '8BIM'",
        "custom icon",
        "scanned in 1999",
        "2003-03-03 09:46:40 UTC", // 2000-01-01 + 100_000_000 s
        "Locked: yes",
        "Protected: no",
        "access 0x00C3, file type 0x0006, aux type 0x00002000",
        "read-only, archive",
        "invisible, backup needed",
        "Icon (black and white",
        "AppleDouble header file",
        "Resource Fork",
    ] {
        assert!(info.contains(want), "{want} in\n{info}");
    }
    assert!(info.contains("########################################"), "icon art");
    assert!(!info.contains("Backup:"), "unknown date (0x80000000) is not shown");
}

#[test]
fn apple_single_carries_the_data_fork_and_unnamed_files_use_the_file_name() {
    let data = pattern(3000, 4);
    let img = apple_double(true, &[(1, data.clone()), (9, finder_info(b"TEXT", b"ttxt", 0)), (3, b"notes.txt".to_vec())]);
    let (_, mut v) = open(&img, "notes.as");
    assert_eq!(names(&mut v), ["!APPLESINGLE-INFO.txt", "notes.txt"]);
    assert_eq!(read(&mut v, "/notes.txt"), data);

    let img = apple_double(false, &[(9, finder_info(b"TEXT", b"ttxt", 0)), (2, b"r".to_vec())]);
    let (_, mut v) = open(&img, "._report.doc");
    assert_eq!(v.name(), "report.doc");
    let info = norm(read(&mut v, "/!APPLEDOUBLE-INFO.txt"));
    assert!(info.contains("taken from the name of the opened file"));
}

#[test]
fn macos_extended_attributes_are_listed_and_broken_ones_do_not_matter() {
    // Finder Info (32 bytes) + ATTR block, entry offsets are absolute file offsets
    let attrs: [(&str, &[u8]); 2] = [("com.apple.quarantine", b"0083;5f000000;Safari;"), ("com.apple.metadata:kMDItemWhereFroms", &[0xFF, 0xFE, 0x00, 0x01, 0x80])];
    let finder = finder_info(b"TEXT", b"ttxt", 0);
    let mut head_len = 26 + 12 * 2; // two entries: 9 and 2
    let entry_len = |n: usize| (11 + n + 3) & !3;
    let table: usize = attrs.iter().map(|(n, _)| entry_len(n.len() + 1)).sum();
    let attr_hdr = 36 + table;
    let finder_total = 32 + attr_hdr;
    let mut values_at = head_len + finder_total;
    let mut block = Vec::new();
    block.extend_from_slice(b"ATTR");
    block.extend_from_slice(&[0; 4]); // debug tag
    block.extend_from_slice(&0u32.to_be_bytes()); // total size (not used by the reader)
    block.extend_from_slice(&0u32.to_be_bytes());
    block.extend_from_slice(&0u32.to_be_bytes());
    block.extend_from_slice(&[0; 12]);
    block.extend_from_slice(&0u16.to_be_bytes());
    block.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    let mut values = Vec::new();
    for (name, val) in attrs {
        block.extend_from_slice(&(values_at as u32).to_be_bytes());
        block.extend_from_slice(&(val.len() as u32).to_be_bytes());
        block.extend_from_slice(&0u16.to_be_bytes());
        block.push(name.len() as u8 + 1);
        block.extend_from_slice(name.as_bytes());
        block.push(0);
        while !block.len().is_multiple_of(4) {
            block.push(0);
        }
        values.extend_from_slice(val);
        values_at += val.len();
    }
    assert_eq!(block.len(), attr_hdr);
    let mut entry9 = finder;
    entry9.extend_from_slice(&block);
    entry9.extend_from_slice(&values);
    head_len += 0;
    let _ = head_len;
    let img = apple_double(false, &[(9, entry9), (2, b"rsrc".to_vec())]);
    let (_, mut v) = open(&img, "._file.txt");
    let info = norm(read(&mut v, "/!APPLEDOUBLE-INFO.txt"));
    assert!(info.contains("Extended attributes (macOS)"), "{info}");
    assert!(info.contains("com.apple.quarantine\r\n 0083;5f000000;Safari;"), "{info}");
    assert!(info.contains("com.apple.metadata:kMDItemWhereFroms\r\n 5 bytes of binary data"), "{info}");

    // garbage behind the Finder Info: reported, not decoded
    let mut bad = finder_info(b"TEXT", b"ttxt", 0);
    bad.extend_from_slice(&[0xAA; 40]);
    let (_, mut v) = open(&apple_double(false, &[(9, bad)]), "._x");
    let info = norm(read(&mut v, "/!APPLEDOUBLE-INFO.txt"));
    assert!(info.contains("could not be decoded"), "{info}");
}

#[test]
fn damaged_apple_double_is_survived() {
    let img = apple_double(false, &[(9, finder_info(b"TEXT", b"ttxt", 0)), (2, vec![1; 100])]);
    for cut in [26usize, 40, img.len() - 50] {
        let f = temp_file(&img[..cut], "._cut");
        if let Ok((src, parts)) = detect::locate(&f) {
            let src = Arc::new(Mutex::new(src));
            if let Ok(mut v) = volume::open_all(&src, &parts) {
                let _ = v[0].vol.list_directory("/");
            }
        }
    }
    // an entry that points outside the file is ignored and mentioned
    let mut bad = apple_double(false, &[(9, finder_info(b"TEXT", b"ttxt", 0)), (2, vec![1; 100])]);
    bad[26 + 12 + 8..26 + 12 + 12].copy_from_slice(&5000u32.to_be_bytes());
    let (_, mut v) = open(&bad, "._out");
    let info = norm(read(&mut v, "/!APPLEDOUBLE-INFO.txt"));
    assert!(info.contains("lies outside the file"), "{info}");
}
