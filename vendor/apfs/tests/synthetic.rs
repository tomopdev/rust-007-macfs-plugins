//! LOCAL PATCH: tests of the volume selection / feature flags / clone detection
//! against synthetic containers built by `apfs::testutil`.

use std::io::Cursor;

use apfs::testutil::ApfsImageBuilder;
use apfs::{ApfsVolume, EntryKind, list_volumes};

fn two_volumes() -> Vec<u8> {
    let mut b = ApfsImageBuilder::new();
    b.volume("Macintosh HD")
        .add_dir("docs")
        .add_file("docs/hello.txt", b"hello apfs")
        .add_file("big.bin", &vec![0xAB; 600_000])
        .add_symlink("link", "docs/hello.txt")
        .add_file("Re\u{301}sume\u{301}.txt", b"nfd name");
    b.volume("Data").snapshots(3).add_file("x.txt", b"data volume");
    b.volume("Vault").encrypted().add_file("secret", b"?");
    b.build()
}

#[test]
fn lists_volumes_with_flags() {
    let mut img = Cursor::new(two_volumes());
    let slots = list_volumes(&mut img).unwrap();
    assert_eq!(slots.len(), 3);
    let info: Vec<_> = slots.iter().map(|s| s.info.as_ref().unwrap()).collect();
    assert_eq!(info[0].name, "Macintosh HD");
    assert!(!info[0].is_encrypted());
    assert_eq!(info[0].num_snapshots, 0);
    assert_eq!(info[1].name, "Data");
    assert_eq!(info[1].num_snapshots, 3);
    assert_eq!(info[2].name, "Vault");
    assert!(info[2].is_encrypted());
    assert_eq!(slots.iter().map(|s| s.index).collect::<Vec<_>>(), [0, 1, 2]);
}

#[test]
fn open_index_selects_the_volume() {
    let data = two_volumes();
    let mut v = ApfsVolume::open(Cursor::new(&data)).unwrap();
    assert_eq!(v.volume_info().name, "Macintosh HD");
    let names: Vec<String> = v.list_directory("/").unwrap().into_iter().map(|e| e.name).collect();
    assert!(names.contains(&"docs".to_string()), "{names:?}");

    let mut d = ApfsVolume::open_index(Cursor::new(&data), 1).unwrap();
    assert_eq!(d.volume_info().name, "Data");
    assert_eq!(d.read_file("/x.txt").unwrap(), b"data volume");

    assert!(ApfsVolume::open_index(Cursor::new(&data), 7).is_err());
}

#[test]
fn reads_files_dirs_and_links() {
    let mut v = ApfsVolume::open(Cursor::new(two_volumes())).unwrap();
    assert_eq!(v.read_file("/docs/hello.txt").unwrap(), b"hello apfs");
    assert_eq!(v.read_file("/big.bin").unwrap(), vec![0xAB; 600_000]);
    let root = v.list_directory("/").unwrap();
    let big = root.iter().find(|e| e.name == "big.bin").unwrap();
    assert_eq!((big.kind, big.size), (EntryKind::File, 600_000));
    let link = root.iter().find(|e| e.name == "link").unwrap();
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.size, "docs/hello.txt".len() as u64);
    assert_eq!(root.iter().find(|e| e.name == "docs").unwrap().kind, EntryKind::Directory);
    assert_eq!(big.modify_time, apfs::testutil::TEST_TIME_NS);
    assert!(root.iter().all(|e| !e.cloned));
}

#[test]
fn clones_are_flagged_and_read_through_shared_extents() {
    let mut b = ApfsImageBuilder::new();
    b.volume("C")
        .add_file("orig.bin", &vec![7u8; 10_000])
        .add_clone("copy.bin", "orig.bin")
        .add_file("plain.txt", b"plain");
    let mut v = ApfsVolume::open(Cursor::new(b.build())).unwrap();
    let root = v.list_directory("/").unwrap();
    let flag = |n: &str| root.iter().find(|e| e.name == n).unwrap().cloned;
    assert!(flag("orig.bin") && flag("copy.bin") && !flag("plain.txt"));
    assert_eq!(v.read_file("/copy.bin").unwrap(), vec![7u8; 10_000]);
    assert_eq!(v.read_file("/orig.bin").unwrap(), vec![7u8; 10_000]);
}

#[test]
fn sealed_flag_is_reported() {
    let mut b = ApfsImageBuilder::new();
    b.volume("System").sealed();
    let slots = list_volumes(&mut Cursor::new(b.build())).unwrap();
    assert!(slots[0].info.as_ref().unwrap().is_sealed());
}
