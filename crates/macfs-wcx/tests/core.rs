//! Runs the plugin core and the exported WCX functions against synthetic HFSX
//! images (built with the `hfsplus` crate's test image builder).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use hfsplus::testutil::HfsPlusImageBuilder;
use macfs_wcx::archive::Archive;
use macfs_wcx::ffi::*;
use macfs_wcx::*;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "macfs-wcx-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn volume_image() -> Vec<u8> {
    HfsPlusImageBuilder::new()
        .add_file("hello.txt", b"hello hfs+", 0o644)
        .add_file("big.bin", &vec![0xAB; 600_000], 0o644)
        .add_file("a:b?.txt", b"illegal chars", 0o644)
        .add_file("Re\u{301}sume\u{301}.txt", b"nfd name", 0o644)
        .build()
}

/// Whole disk: protective MBR + GPT with one HFS+ partition at 1 MiB.
fn gpt_disk(volume: &[u8], sector: usize) -> Vec<u8> {
    let start_lba = (1 << 20) / sector;
    let end_lba = start_lba + volume.len() / sector - 1;
    let mut disk = vec![0u8; (1 << 20) + volume.len() + 64 * 1024];
    // GPT header at LBA 1
    let h = sector;
    disk[h..h + 8].copy_from_slice(b"EFI PART");
    disk[h + 72..h + 80].copy_from_slice(&2u64.to_le_bytes()); // entries at LBA 2
    disk[h + 80..h + 84].copy_from_slice(&8u32.to_le_bytes());
    disk[h + 84..h + 88].copy_from_slice(&128u32.to_le_bytes());
    // entry 0: Apple HFS+ type GUID 48465300-0000-11AA-AA11-00306543ECAC
    let e = 2 * sector;
    disk[e..e + 16].copy_from_slice(&[
        0x00, 0x53, 0x46, 0x48, 0x00, 0x00, 0xAA, 0x11, 0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC, 0xAC,
    ]);
    disk[e + 32..e + 40].copy_from_slice(&(start_lba as u64).to_le_bytes());
    disk[e + 40..e + 48].copy_from_slice(&(end_lba as u64).to_le_bytes());
    for (i, u) in "Macintosh HD".encode_utf16().enumerate() {
        disk[e + 56 + i * 2..e + 58 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    let off = start_lba * sector;
    disk[off..off + volume.len()].copy_from_slice(volume);
    disk
}

fn list(a: &mut Archive) -> Vec<(String, bool, u64)> {
    let mut v = Vec::new();
    while let Some(e) = a.next_header().unwrap() {
        v.push((e.disp.clone(), e.is_dir, e.size));
    }
    v
}

#[test]
fn bare_volume_list_and_extract() {
    let dir = temp_dir();
    let img = dir.join("vol.hfs");
    fs::write(&img, volume_image()).unwrap();

    let mut a = Archive::open(&img).unwrap();
    let items = list(&mut a);
    let names: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    assert!(names.contains(&"hello.txt"), "{names:?}");
    assert!(names.contains(&"a_b_.txt"), "{names:?}"); // sanitised
    assert!(names.contains(&"R\u{e9}sum\u{e9}.txt"), "{names:?}"); // NFC
    assert_eq!(items.iter().find(|i| i.0 == "big.bin").unwrap().2, 600_000);

    // extract everything and compare
    let mut a = Archive::open(&img).unwrap();
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    while let Some(e) = a.next_header().unwrap() {
        assert_eq!(a.process(PK_EXTRACT, Some(&out.join(&e.disp))), E_OK);
    }
    assert_eq!(fs::read(out.join("hello.txt")).unwrap(), b"hello hfs+");
    assert_eq!(fs::read(out.join("big.bin")).unwrap(), vec![0xAB; 600_000]);
    assert_eq!(fs::read(out.join("a_b_.txt")).unwrap(), b"illegal chars");
    assert_eq!(fs::read(out.join("R\u{e9}sum\u{e9}.txt")).unwrap(), b"nfd name");

    // test + skip must not create files
    let mut a = Archive::open(&img).unwrap();
    let e = a.next_header().unwrap().unwrap();
    assert_eq!(a.process(PK_TEST, None), E_OK, "{}", e.disp);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn gpt_disks_512_and_4096() {
    for sector in [512usize, 4096] {
        let dir = temp_dir();
        let disk = dir.join("disk.img");
        fs::write(&disk, gpt_disk(&volume_image(), sector)).unwrap();
        let mut a = Archive::open(&disk).unwrap();
        let names: Vec<String> = list(&mut a).into_iter().map(|i| i.0).collect();
        // a single HFS+ partition is shown at the root
        assert!(names.contains(&"hello.txt".to_string()), "sector {sector}: {names:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn devlink_with_offset_and_partition() {
    let dir = temp_dir();
    let disk = dir.join("disk.img");
    fs::write(&disk, gpt_disk(&volume_image(), 512)).unwrap();

    let link = dir.join("mac.hfsdev");
    fs::write(&link, format!("device = {}\npartition = 1\n", disk.display())).unwrap();
    let mut a = Archive::open(&link).unwrap();
    assert!(list(&mut a).iter().any(|i| i.0 == "hello.txt"));

    fs::write(&link, format!("device = {}\noffset = {}\n", disk.display(), 1 << 20)).unwrap();
    let mut a = Archive::open(&link).unwrap();
    assert!(list(&mut a).iter().any(|i| i.0 == "hello.txt"));

    fs::write(&link, format!("device = {}\noffset = 4096\n", disk.display())).unwrap();
    assert!(Archive::open(&link).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn non_hfs_is_rejected() {
    let dir = temp_dir();
    let f = dir.join("junk.img");
    fs::write(&f, vec![7u8; 100_000]).unwrap();
    assert!(Archive::open(&f).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

fn wide(p: &Path) -> Vec<u16> {
    to_wide_nul(&p.to_string_lossy())
}

#[test]
fn exported_wcx_functions() {
    let dir = temp_dir();
    let img = dir.join("vol.hfs");
    fs::write(&img, volume_image()).unwrap();

    unsafe {
        assert_eq!(GetPackerCaps(), PK_CAPS_BY_CONTENT);
        let w = wide(&img);
        assert_eq!(CanYouHandleThisFileW(w.as_ptr()), 1);
        let junk = dir.join("junk.img");
        fs::write(&junk, vec![1u8; 50_000]).unwrap();
        assert_eq!(CanYouHandleThisFileW(wide(&junk).as_ptr()), 0);

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

        let out = dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let mut dest = wide(&out);
        // TC passes the destination folder with a trailing backslash, or NULL + full name.
        let mut seen = 0;
        loop {
            let mut hd: Box<HeaderDataExW> = Box::new(std::mem::zeroed());
            let rc = ReadHeaderExW(h, &mut *hd);
            if rc == E_END_ARCHIVE {
                break;
            }
            assert_eq!(rc, E_OK);
            let n = hd.FileName.iter().position(|&c| c == 0).unwrap();
            let name = String::from_utf16_lossy(&hd.FileName[..n]);
            let full = wide(&out.join(&name));
            assert_eq!(ProcessFileW(h, PK_EXTRACT, std::ptr::null(), full.as_ptr()), E_OK);
            if name == "big.bin" {
                assert_eq!(hd.UnpSize, 600_000);
            }
            seen += 1;
        }
        assert!(seen >= 4);
        dest.clear();
        assert_eq!(CloseArchive(h), E_OK);
    }
    assert_eq!(fs::read(dir.join("out").join("hello.txt")).unwrap(), b"hello hfs+");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn progress_callback_can_abort() {
    unsafe extern "system" fn stop(_name: *mut u16, _size: i32) -> i32 {
        0
    }
    let dir = temp_dir();
    let img = dir.join("vol.hfs");
    fs::write(&img, volume_image()).unwrap();

    let mut a = Archive::open(&img).unwrap();
    a.progress = Some(stop);
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let mut aborted = false;
    while let Some(e) = a.next_header().unwrap() {
        let target = out.join(&e.disp);
        let rc = a.process(PK_EXTRACT, Some(&target));
        if rc == E_EABORTED {
            assert!(!target.exists(), "partial file must be removed");
            aborted = true;
            break;
        }
    }
    assert!(aborted);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn dmg_container_lists_and_extracts() {
    let dir = temp_dir();
    let dmg = dir.join("vol.dmg");
    let mut w = ::udif::DmgWriter::create(&dmg).unwrap().compression(::udif::CompressionMethod::Bzip2);
    w.add_partition("disk image (Apple_HFSX : 0)", &volume_image()).unwrap();
    w.finish().unwrap();

    let mut a = Archive::open(&dmg).unwrap();
    let items = list(&mut a);
    assert_eq!(items.iter().find(|i| i.0 == "big.bin").unwrap().2, 600_000);
    assert!(items.iter().any(|i| i.0 == "hello.txt"));

    let mut a = Archive::open(&dmg).unwrap();
    while let Some(e) = a.next_header().unwrap() {
        if e.disp == "hello.txt" {
            let out = dir.join("hello.out");
            assert_eq!(a.process(PK_EXTRACT, Some(&out)), E_OK);
            assert_eq!(fs::read(&out).unwrap(), b"hello hfs+");
        }
    }
}

#[test]
fn encrypted_dmg_is_reported_as_unknown_format() {
    let dir = temp_dir();
    let dmg = dir.join("secret.dmg");
    let mut data = b"encrcdsa".to_vec();
    data.resize(8192, 1);
    fs::write(&dmg, data).unwrap();
    let e = Archive::open(&dmg).err().expect("must fail");
    assert_eq!(e.code(), E_UNKNOWN_FORMAT);
}

#[test]
fn pkg_lists_and_extracts() {
    use macfs_core::testutil::{SAMPLE_BIN, sample_pkg};
    let dir = temp_dir();
    let pkg = dir.join("tool.pkg");
    fs::write(&pkg, sample_pkg(&vec![9u8; 40_000])).unwrap();

    unsafe {
        assert_eq!(CanYouHandleThisFileW(wide(&pkg).as_ptr()), 1);
    }
    let mut a = Archive::open(&pkg).unwrap();
    let items = list(&mut a);
    let names: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    for want in [
        "!XAR-TOC.xml",
        "!PKG-COMPONENTS.txt",
        "Installed Files",
        "Installed Files\\!CPIO-INDEX.txt",
        "Installed Files\\bin\\tool",
        "Installed Files\\share\\a_b_.txt",
        "Tool.pkg\\Payload",
    ] {
        assert!(names.contains(&want), "{want} missing in {names:?}");
    }
    assert_eq!(items.iter().find(|i| i.0 == "Installed Files\\share\\doc\\tool\\big.bin").unwrap().2, 40_000);

    let out = dir.join("out");
    let mut a = Archive::open(&pkg).unwrap();
    while let Some(e) = a.next_header().unwrap() {
        // Total Commander passes Windows paths; map the separator for the Linux test run
        assert_eq!(a.process(PK_EXTRACT, Some(&out.join(e.disp.replace('\\', "/")))), E_OK, "{}", e.disp);
    }
    assert_eq!(fs::read(out.join("Installed Files").join("bin").join("tool")).unwrap(), SAMPLE_BIN);
    assert_eq!(fs::read(out.join("Installed Files").join("share").join("doc").join("tool").join("big.bin")).unwrap(), vec![9u8; 40_000]);
    assert!(fs::read_to_string(out.join("!PKG-COMPONENTS.txt")).unwrap().contains("com.example.tool"));
    assert!(fs::read_to_string(out.join("!XAR-TOC.xml")).unwrap().contains("<toc>"));
    fs::remove_dir_all(&dir).unwrap();
}
