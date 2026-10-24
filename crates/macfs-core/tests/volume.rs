//! Detection of APFS containers and the shared `MacVolume` layer, on synthetic images
//! (`apfs::testutil`, `hfsplus::testutil`).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use apfs::testutil::ApfsImageBuilder;
use hfsplus::testutil::HfsPlusImageBuilder;
use macfs_core::detect::{self, FsKind};
use macfs_core::volume::{self, CLONES_NAME, NOTICE_NAME, NodeKind, VolumeError};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-core-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    d
}

/// GPT disk (512-byte sectors), one partition per image, 2 MiB apart.
fn gpt_disk(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let sector = 512usize;
    let stride = 2 << 20;
    let mut disk = vec![0u8; stride * (parts.len() + 1)];
    let h = sector;
    disk[h..h + 8].copy_from_slice(b"EFI PART");
    disk[h + 72..h + 80].copy_from_slice(&2u64.to_le_bytes());
    disk[h + 80..h + 84].copy_from_slice(&8u32.to_le_bytes());
    disk[h + 84..h + 88].copy_from_slice(&128u32.to_le_bytes());
    for (i, (label, vol)) in parts.iter().enumerate() {
        let start = stride * (i + 1);
        let e = 2 * sector + i * 128;
        disk[e..e + 16].copy_from_slice(&[0x01; 16]); // type GUID is not looked at: detection is by content
        disk[e + 32..e + 40].copy_from_slice(&((start / sector) as u64).to_le_bytes());
        disk[e + 40..e + 48].copy_from_slice(&(((start + vol.len()) / sector - 1) as u64).to_le_bytes());
        for (j, u) in label.encode_utf16().enumerate() {
            disk[e + 56 + j * 2..e + 58 + j * 2].copy_from_slice(&u.to_le_bytes());
        }
        disk[start..start + vol.len()].copy_from_slice(vol);
    }
    disk
}

fn container() -> Vec<u8> {
    let mut b = ApfsImageBuilder::new();
    b.volume("Macintosh HD")
        .add_dir("docs")
        .add_file("docs/hello.txt", b"hello apfs")
        .add_file("big.bin", &vec![0xAB; 600_000]);
    b.volume("Macintosh HD - Data")
        .snapshots(3)
        .add_file("orig.bin", &vec![7u8; 10_000])
        .add_clone("copy.bin", "orig.bin");
    b.volume("Vault").encrypted().add_file("secret.txt", b"cipher");
    b.volume("System").sealed().add_file("kernel", b"k");
    b.build()
}

fn open(img: &[u8]) -> (PathBuf, Vec<volume::OpenedVolume>, Vec<detect::Part>) {
    let dir = temp_dir();
    let f = dir.join("disk.img");
    fs::write(&f, img).unwrap();
    let (src, parts) = detect::locate(&f).unwrap();
    let src = Arc::new(Mutex::new(src));
    let vols = volume::open_all(&src, &parts).unwrap_or_else(|e| panic!("{e}"));
    (dir, vols, parts)
}

fn text(v: &mut volume::MacVolume, path: &str) -> String {
    let mut out = Vec::new();
    v.read_file_to(path, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn bare_container_is_detected_and_every_volume_opens() {
    let (dir, vols, parts) = open(&container());
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].kind, FsKind::Apfs);
    let labels: Vec<&str> = vols.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Partition 1 - Macintosh HD", "Partition 1 - Macintosh HD - Data", "Partition 1 - Vault", "Partition 1 - System"]
    );
    assert!(vols.iter().all(|v| v.vol.kind() == FsKind::Apfs));
    assert!(vols[0].vol.is_readable() && vols[1].vol.is_readable() && !vols[2].vol.is_readable());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn plain_volume_lists_and_reads_without_notice() {
    let (dir, mut vols, _) = open(&container());
    let v = &mut vols[0].vol;
    let root = v.list_directory("/").unwrap();
    let names: Vec<&str> = root.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"docs") && names.contains(&"big.bin"), "{names:?}");
    assert!(!names.contains(&NOTICE_NAME), "volume without unsupported features has no notice: {names:?}");
    let big = root.iter().find(|n| n.name == "big.bin").unwrap();
    assert_eq!((big.kind, big.size, big.mtime), (NodeKind::File, 600_000, Some(1_709_210_096)));
    assert_eq!(text(v, "/docs/hello.txt"), "hello apfs");
    assert!(v.late_clone_notice().is_none());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn snapshots_are_reported_in_the_notice_file() {
    let (dir, mut vols, _) = open(&container());
    let v = &mut vols[1].vol;
    let root = v.list_directory("/").unwrap();
    let notice = root.iter().find(|n| n.name == NOTICE_NAME).expect("notice file");
    let t = text(v, "/!APFS-NOTICE.txt");
    assert_eq!(notice.size, t.len() as u64);
    assert!(t.contains("Macintosh HD - Data") && t.contains("3 snapshot(s)") && t.contains("not supported yet"), "{t}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn clones_read_correctly_and_are_reported_after_the_walk() {
    let (dir, mut vols, _) = open(&container());
    let v = &mut vols[1].vol;
    assert!(v.late_clone_notice().is_none(), "nothing seen yet");
    v.list_directory("/").unwrap();
    assert_eq!(v.read_file_to("/copy.bin", &mut Vec::new()).unwrap(), 10_000);
    let late = v.late_clone_notice().expect("clones seen");
    assert_eq!((late.name.as_str(), late.kind), (CLONES_NAME, NodeKind::File));
    assert!(v.late_clone_notice().is_none(), "reported once");
    let t = text(v, "/!APFS-CLONES.txt");
    assert_eq!(late.size, t.len() as u64);
    assert!(t.contains("cloned files") && t.contains("not supported yet"));
    let mut data = Vec::new();
    v.read_file_to("/copy.bin", &mut data).unwrap();
    assert_eq!(data, vec![7u8; 10_000]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn encrypted_volume_is_not_read_only_reported() {
    let (dir, mut vols, _) = open(&container());
    let v = &mut vols[2].vol;
    assert_eq!(v.name(), "Vault");
    let root = v.list_directory("/").unwrap();
    assert_eq!(root.len(), 1, "only the notice, never the (ciphertext) files: {root:?}");
    assert_eq!(root[0].name, NOTICE_NAME);
    let t = text(v, "/!APFS-NOTICE.txt");
    assert!(t.contains("is encrypted") && t.contains("not supported yet"), "{t}");
    assert!(matches!(v.list_directory("/sub"), Err(VolumeError::NotFound(_))));
    assert!(matches!(v.read_file_to("/secret.txt", &mut Vec::new()), Err(VolumeError::NotFound(_))));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn sealed_volume_is_flagged() {
    let (dir, mut vols, _) = open(&container());
    let v = &mut vols[3].vol;
    let t = {
        v.list_directory("/").unwrap();
        text(v, "/!APFS-NOTICE.txt")
    };
    assert!(t.contains("Sealed volume"), "{t}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn gpt_disk_with_apfs_and_hfs_partitions() {
    let hfs = HfsPlusImageBuilder::new().add_file("hello.txt", b"from hfs+", 0o644).build();
    let mut b = ApfsImageBuilder::new();
    b.volume("Solo").add_file("hello.txt", b"from apfs");
    let disk = gpt_disk(&[("Old Mac", hfs), ("ignored GPT name", b.build())]);
    let (dir, mut vols, parts) = open(&disk);
    assert_eq!(parts.iter().map(|p| p.kind).collect::<Vec<_>>(), [FsKind::HfsPlus, FsKind::Apfs]);
    let labels: Vec<&str> = vols.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(labels, ["Partition 1 - Old Mac", "Partition 2 - Solo"]);
    assert_eq!(text(&mut vols[0].vol, "/hello.txt"), "from hfs+");
    assert_eq!(text(&mut vols[1].vol, "/hello.txt"), "from apfs");
    // HFS+ volumes never get APFS notices
    assert!(vols[0].vol.list_directory("/").unwrap().iter().all(|n| n.name != NOTICE_NAME));
    assert!(vols[0].vol.late_clone_notice().is_none());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn junk_and_empty_container_are_rejected() {
    let dir = temp_dir();
    let f = dir.join("junk.img");
    fs::write(&f, vec![9u8; 100_000]).unwrap();
    let (_, parts) = detect::locate(&f).unwrap();
    assert!(parts.is_empty());

    let f = dir.join("empty.img");
    fs::write(&f, ApfsImageBuilder::new().build()).unwrap();
    let (src, parts) = detect::locate(&f).unwrap();
    assert_eq!(parts.len(), 1);
    let src = Arc::new(Mutex::new(src));
    assert!(volume::open_all(&src, &parts).is_err());
    fs::remove_dir_all(&dir).unwrap();
}
