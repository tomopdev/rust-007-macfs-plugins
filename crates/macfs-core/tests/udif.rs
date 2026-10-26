//! UDIF (DMG) support of the core (read-only): lazy virtual disk and volume detection inside images.
//! Images are built with the `udif` crate itself.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use apfs::testutil::ApfsImageBuilder;
use hfsplus::testutil::HfsPlusImageBuilder;
use macfs_core::detect::{self, FsKind, LocateError};
use macfs_core::source::DiskSource;
use macfs_core::udif::{self as core_udif, ImageKind, SECTOR, UdifImage};
use macfs_core::volume;
use udif::CompressionMethod;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("macfs-udif-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    d
}

fn hfs_volume() -> Vec<u8> {
    HfsPlusImageBuilder::new().add_file("hello.txt", b"hello from a dmg", 0o644).add_file("big.bin", &vec![0xAB; 300_000], 0o644).build()
}

fn apfs_container() -> Vec<u8> {
    let mut b = ApfsImageBuilder::new();
    b.volume("Data").add_file("note.txt", b"apfs in a dmg");
    b.build()
}

/// Deterministic noise (a stand-in for data that does not compress).
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn make_dmg(path: &Path, method: CompressionMethod, parts: &[(&str, &[u8])]) {
    let mut w = udif::DmgWriter::create(path).unwrap().compression(method).chunk_size(64 * 1024);
    for (name, data) in parts {
        w.add_partition(name, data).unwrap();
    }
    w.finish().unwrap();
}

/// The virtual disk the container must present: partitions back to back, each padded to whole sectors.
fn expected_disk(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut v = Vec::new();
    for (_, d) in parts {
        v.extend_from_slice(d);
        v.resize(v.len().div_ceil(SECTOR as usize) * SECTOR as usize, 0);
    }
    v
}

fn read_all(src: &mut DiskSource) -> Vec<u8> {
    let mut out = vec![0u8; src.len() as usize];
    let mut off = 0;
    while off < out.len() {
        let end = (off + 100_000).min(out.len());
        src.read_exact_at(off as u64, &mut out[off..end]).unwrap();
        off = end;
    }
    out
}

fn text(v: &mut volume::MacVolume, path: &str) -> String {
    let mut out = Vec::new();
    v.read_file_to(path, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn every_block_type_reads_back_the_raw_disk() {
    let vol = hfs_volume();
    let rnd = noise(200_001, 7); // not a multiple of the sector size
    let parts: [(&str, &[u8]); 3] = [("table", &rnd[..70_000]), ("volume", &vol), ("tail", &rnd)];
    for method in [CompressionMethod::Raw, CompressionMethod::Zlib, CompressionMethod::Bzip2, CompressionMethod::Lzfse] {
        let dir = temp_dir();
        let f = dir.join("t.dmg");
        make_dmg(&f, method, &parts);
        let mut src = detect::open_source(&f).unwrap();
        assert!(src.udif().is_some());
        let expected = expected_disk(&parts);
        assert_eq!(src.len(), expected.len() as u64, "{method:?}");
        assert!(read_all(&mut src) == expected, "{method:?}: whole disk differs");

        // unaligned reads across run boundaries, and reads past the end
        let mut x = 12345u64;
        for _ in 0..200 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            let off = (x >> 33) % (expected.len() as u64 + 100);
            let len = ((x >> 8) % 90_000) as usize + 1;
            let mut buf = vec![0xEEu8; len];
            let got = src.read_at(off, &mut buf).unwrap();
            let want = (expected.len() as u64).saturating_sub(off).min(len as u64) as usize;
            assert_eq!(got, want, "{method:?} off={off} len={len}");
            assert!(buf[..got] == expected[off as usize..off as usize + got], "{method:?} off={off} len={len}");
        }
    }
}

#[test]
fn xz_blocks_decode() {
    use lzma_rust2::{XzOptions, XzWriter};
    use std::io::Write;
    let data = noise(4096, 3).into_iter().map(|b| b & 0x0F).collect::<Vec<u8>>();
    let mut enc = XzWriter::new(Vec::new(), XzOptions::with_preset(6)).unwrap();
    enc.write_all(&data).unwrap();
    let comp = enc.finish().unwrap();
    let mut out = vec![0u8; data.len()];
    core_udif::decode_block(::udif::BlockType::Xz, &comp, &mut out).unwrap();
    assert_eq!(out, data);
    // a block that decodes to something else than the block map says is an error, not silently padded
    let mut longer = vec![0u8; data.len() + 512];
    assert!(core_udif::decode_block(::udif::BlockType::Xz, &comp, &mut longer).is_err());
    assert!(core_udif::decode_block(::udif::BlockType::Adc, &comp, &mut out).is_err());
}

#[test]
fn volume_inside_a_dmg_is_listed_and_read() {
    let dir = temp_dir();
    let f = dir.join("v.dmg");
    let vol = hfs_volume();
    make_dmg(&f, CompressionMethod::Zlib, &[("disk image (Apple_HFS : 0)", &vol)]);
    let (src, parts) = detect::locate(&f).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].kind, FsKind::HfsPlus);
    let src = Arc::new(Mutex::new(src));
    let mut vols = volume::open_all(&src, &parts).unwrap();
    assert_eq!(text(&mut vols[0].vol, "/hello.txt"), "hello from a dmg");
    let mut big = Vec::new();
    vols[0].vol.read_file_to("/big.bin", &mut big).unwrap();
    assert!(big == vec![0xAB; 300_000]);
}

#[test]
fn partition_map_inside_the_dmg_is_followed() {
    // GPT disk with an HFS+ and an APFS partition, stored as ONE blkx entry (as some tools do) ...
    let (hfs, apfs) = (hfs_volume(), apfs_container());
    let stride = 2usize << 20;
    let mut disk = vec![0u8; stride * 3];
    disk[512..520].copy_from_slice(b"EFI PART");
    disk[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
    disk[512 + 80..512 + 84].copy_from_slice(&8u32.to_le_bytes());
    disk[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
    for (i, (label, vol)) in [("Mac HFS", &hfs), ("Mac APFS", &apfs)].into_iter().enumerate() {
        let start = stride * (i + 1);
        let e = 1024 + i * 128;
        disk[e..e + 16].copy_from_slice(&[1; 16]);
        disk[e + 32..e + 40].copy_from_slice(&((start / 512) as u64).to_le_bytes());
        disk[e + 40..e + 48].copy_from_slice(&(((start + vol.len()) / 512 - 1) as u64).to_le_bytes());
        for (j, u) in label.encode_utf16().enumerate() {
            disk[e + 56 + j * 2..e + 58 + j * 2].copy_from_slice(&u.to_le_bytes());
        }
        disk[start..start + vol.len()].copy_from_slice(vol);
    }
    let dir = temp_dir();
    let f = dir.join("gpt.dmg");
    make_dmg(&f, CompressionMethod::Lzfse, &[("whole disk", &disk)]);
    let (_src, parts) = detect::locate(&f).unwrap();
    let kinds: Vec<_> = parts.iter().map(|p| (p.label.as_str(), p.kind)).collect();
    assert_eq!(kinds, vec![("Mac HFS", FsKind::HfsPlus), ("Mac APFS", FsKind::Apfs)]);
}

#[test]
fn partition_list_and_positions() {
    let dir = temp_dir();
    let f = dir.join("p.dmg");
    let (a, b) = (noise(100_000, 1), hfs_volume());
    make_dmg(&f, CompressionMethod::Zlib, &[("first", &a), ("second", &b)]);
    let img = UdifImage::open(&f).unwrap();
    assert_eq!(img.partitions.len(), 2);
    assert_eq!(img.partitions[1].first_sector, (a.len() as u64).div_ceil(SECTOR));
    assert_eq!(img.partitions[1].name, "second");
}

#[test]
fn unsupported_containers_are_named() {
    let dir = temp_dir();
    for (magic, what) in [(&b"encrcdsa"[..], "encrypted"), (b"cdsaencr", "encrypted"), (b"sprs\0\0\0\0", "sparse")] {
        let f = dir.join("x.dmg");
        let mut data = magic.to_vec();
        data.resize(4096, 0);
        fs::write(&f, data).unwrap();
        assert!(matches!(core_udif::probe(&f).unwrap(), ImageKind::Unsupported(w) if w.contains(what)), "{what}");
        match detect::locate(&f) {
            Err(LocateError::Unsupported(m)) => assert!(m.contains(what), "{m}"),
            other => panic!("{:?}", other.map(|_| ())),
        }
    }
}

#[test]
fn a_dmg_that_is_not_udif_is_read_as_a_plain_image() {
    let dir = temp_dir();
    let f = dir.join("raw.dmg");
    fs::write(&f, hfs_volume()).unwrap();
    assert_eq!(core_udif::probe(&f).unwrap(), ImageKind::Plain);
    let (src, parts) = detect::locate(&f).unwrap();
    assert!(src.udif().is_none());
    assert_eq!(parts.len(), 1);
}
