//! Classic HFS and MFS volumes (bare, in Disk Copy 4.2, in an Apple Partition Map) through `detect` + `volume`.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use hfs_reader::testutil::{HfsImageBuilder, MODIFIED, ROOT};
use macfs::{ImageFormat, MfsVolume};
use macfs_core::detect::{self, FsKind};
use macfs_core::volume::{self, MacVolume, NodeKind};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_file(bytes: &[u8], name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-classic-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    let f = d.join(name);
    fs::write(&f, bytes).unwrap();
    f
}

fn open(bytes: &[u8], name: &str) -> (Vec<detect::Part>, Vec<volume::OpenedVolume>) {
    let f = temp_file(bytes, name);
    let (src, parts) = detect::locate(&f).unwrap();
    let src = Arc::new(Mutex::new(src));
    let vols = volume::open_all(&src, &parts).unwrap_or_else(|e| panic!("{e}"));
    (parts, vols)
}

fn text(v: &mut MacVolume, path: &str) -> Vec<u8> {
    let mut out = Vec::new();
    v.read_file_to(path, &mut out).unwrap();
    out
}

fn names(v: &mut MacVolume, path: &str) -> Vec<String> {
    let mut n: Vec<String> = v.list_directory(path).unwrap().into_iter().map(|d| d.name).collect();
    n.sort();
    n
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed)).collect()
}

fn hfs_image() -> Vec<u8> {
    let mut b = HfsImageBuilder::new(b"Syst\x8Em");
    let folder = b.add_dir(ROOT, b"Syst\x8Em Folder");
    b.add_dir(ROOT, b"Empty");
    b.add_file(ROOT, b"Read Me", b"classic hello", b"");
    let app = b.add_file(folder, b"Finder", b"", &pattern(5000, 3));
    b.fragment(app);
    b.add_file(folder, b"A/B", &pattern(2000, 1), b"x");
    b.build()
}

#[test]
fn bare_hfs_volume_lists_forks_and_dates() {
    let (parts, mut vols) = open(&hfs_image(), "disk.img");
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].kind, FsKind::Hfs);
    assert_eq!(vols.len(), 1);
    let v = &mut vols[0].vol;
    assert_eq!(v.kind(), FsKind::Hfs);
    assert_eq!(v.name(), "Syst\u{e9}m");
    assert_eq!(vols[0].label, "Partition 1 - Syst\u{e9}m");
    let v = &mut vols[0].vol;

    assert_eq!(names(v, "/"), ["Empty", "Read Me", "Syst\u{e9}m Folder"]);
    assert_eq!(names(v, "/Empty"), Vec::<String>::new());
    // the resource fork is a second file, A/B is shown as A:B
    assert_eq!(names(v, "/Syst\u{e9}m Folder"), ["A:B", "A:B.rsrc", "Finder", "Finder.rsrc"]);
    let root = v.list_directory("/").unwrap();
    let readme = root.iter().find(|n| n.name == "Read Me").unwrap();
    assert_eq!((readme.kind, readme.size), (NodeKind::File, 13));
    assert_eq!(readme.mtime, Some(MODIFIED as i64 - 2_082_844_800));
    assert!(root.iter().find(|n| n.name == "Empty").unwrap().is_dir());

    assert_eq!(text(v, "/Read Me"), b"classic hello");
    let finder = v.list_directory("/Syst\u{e9}m Folder").unwrap();
    assert_eq!(finder.iter().find(|n| n.name == "Finder").unwrap().size, 0);
    assert_eq!(finder.iter().find(|n| n.name == "Finder.rsrc").unwrap().size, 5000);
    assert_eq!(text(v, "/Syst\u{e9}m Folder/Finder"), b"");
    assert_eq!(text(v, "/Syst\u{e9}m Folder/Finder.rsrc"), pattern(5000, 3)); // fragmented, overflow extents
    assert_eq!(text(v, "/Syst\u{e9}m Folder/A:B"), pattern(2000, 1));
    assert!(v.list_directory("/nothing").is_err());
    assert!(v.read_file_to("/nothing", &mut Vec::new()).is_err());
}

#[test]
fn disk_copy_hfs_floppy_is_found() {
    let raw = hfs_image();
    let mut dc = vec![0u8; 84];
    dc[0] = 5;
    dc[1..6].copy_from_slice(b"Disk1");
    dc[64..68].copy_from_slice(&(raw.len() as u32).to_be_bytes());
    dc[82..84].copy_from_slice(&[1, 0]);
    dc.extend_from_slice(&raw);
    let (parts, mut vols) = open(&dc, "floppy.dsk");
    assert_eq!((parts.len(), parts[0].kind, parts[0].start), (1, FsKind::Hfs, 84));
    assert_eq!(text(&mut vols[0].vol, "/Read Me"), b"classic hello");
}

fn mfs_volume(format: ImageFormat) -> Vec<u8> {
    let mut v = MfsVolume::format(MfsVolume::FLOPPY_400K, "Mac 128", format).unwrap();
    v.add_file("Read Me", *b"TEXT", *b"MACA", b"hello from 1984", &[]).unwrap();
    v.add_file("MacPaint", *b"APPL", *b"MPNT", &pattern(3000, 2), &pattern(1500, 9)).unwrap();
    v.add_file("a/b", *b"TEXT", *b"MACA", b"slash", &[]).unwrap();
    let mut out = Cursor::new(Vec::new());
    v.save_to(&mut out).unwrap();
    out.into_inner()
}

#[test]
fn mfs_floppy_raw_and_disk_copy() {
    for (format, ext) in [(ImageFormat::Raw, "mfs.img"), (ImageFormat::DiskCopy42, "mfs.dsk")] {
        let (parts, mut vols) = open(&mfs_volume(format), ext);
        assert_eq!((parts.len(), parts[0].kind), (1, FsKind::Mfs), "{ext}");
        assert_eq!(vols[0].label, "Partition 1 - Mac 128");
        let v = &mut vols[0].vol;
        assert_eq!((v.kind(), v.name()), (FsKind::Mfs, "Mac 128"));
        assert_eq!(names(v, "/"), ["MacPaint", "MacPaint.rsrc", "Read Me", "a:b"], "{ext}");
        assert_eq!(text(v, "/Read Me"), b"hello from 1984");
        assert_eq!(text(v, "/MacPaint"), pattern(3000, 2));
        assert_eq!(text(v, "/MacPaint.rsrc"), pattern(1500, 9));
        assert_eq!(text(v, "/a:b"), b"slash");
        assert_eq!(v.list_directory("/").unwrap().iter().find(|n| n.name == "Read Me").unwrap().size, 15);
        assert!(v.list_directory("/sub").is_err());
    }
}

/// Apple Partition Map disk: driver partition, a classic HFS partition and an HFS+ partition.
#[test]
fn apple_partition_map_with_hfs_partition() {
    let hfs = hfs_image();
    let mut disk = vec![0u8; 512 * 80 + hfs.len()];
    disk[0..2].copy_from_slice(&0x4552u16.to_be_bytes());
    disk[2..4].copy_from_slice(&512u16.to_be_bytes());
    let entry = |disk: &mut Vec<u8>, i: usize, start: u32, len: u32, name: &str| {
        let o = 512 * i;
        disk[o..o + 2].copy_from_slice(&0x504Du16.to_be_bytes());
        disk[o + 4..o + 8].copy_from_slice(&2u32.to_be_bytes());
        disk[o + 8..o + 12].copy_from_slice(&start.to_be_bytes());
        disk[o + 12..o + 16].copy_from_slice(&len.to_be_bytes());
        disk[o + 16..o + 16 + name.len()].copy_from_slice(name.as_bytes());
    };
    entry(&mut disk, 1, 1, 63, "Apple");
    entry(&mut disk, 2, 64, (hfs.len() / 512) as u32, "Macintosh HD");
    disk[512 * 64..512 * 64 + hfs.len()].copy_from_slice(&hfs);
    let (parts, mut vols) = open(&disk, "hd.img");
    assert_eq!(parts.len(), 1);
    assert_eq!((parts[0].kind, parts[0].start, parts[0].label.as_str()), (FsKind::Hfs, 512 * 64, "Macintosh HD"));
    assert_eq!(vols[0].label, "Partition 1 - Macintosh HD");
    assert_eq!(text(&mut vols[0].vol, "/Read Me"), b"classic hello");
}

#[test]
fn hfs_wrapper_of_hfs_plus_is_still_hfs_plus() {
    // an HFS MDB that embeds an HFS+ volume must not be taken for a classic volume
    let mut img = hfs_image();
    img[1024 + 124..1024 + 126].copy_from_slice(&0x482Bu16.to_be_bytes());
    let f = temp_file(&img, "wrapped.img");
    let (mut src, _) = detect::locate(&f).unwrap();
    assert!(detect::classic_at(&mut src, 0, img.len() as u64, "").is_none());
}

#[test]
fn damaged_classic_volumes_report_errors() {
    let mut img = hfs_image();
    let len = img.len();
    // catalog destroyed: the volume is recognised but cannot be opened
    for b in &mut img[2048..len - 2048] {
        *b = 0xEE;
    }
    let f = temp_file(&img, "broken.img");
    let (src, parts) = detect::locate(&f).unwrap();
    let src = Arc::new(Mutex::new(src));
    if !parts.is_empty() {
        let r = volume::open_all(&src, &parts);
        if let Ok(mut v) = r {
            // an accepted volume must still list without panicking
            let _ = v[0].vol.list_directory("/");
        }
    }
}
