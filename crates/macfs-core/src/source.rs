//! Byte source (image file, raw disk/partition device or a UDIF/DMG image seen as one
//! virtual disk) with a sector-aligned read cache, plus a bounded `Read + Seek` window
//! used as the HFS+ / APFS volume.
//!
//! Raw Windows devices (`\\.\PhysicalDriveN`, `\\.\X:`) only accept reads at
//! sector-aligned offsets and lengths. Everything therefore goes through
//! 128 KiB chunks read at 128 KiB-aligned offsets (a multiple of both 512 and
//! 4096 byte sectors), which also makes B-tree node reads cheap.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::sparse::SparseDisk;
use crate::udif::{UdifDisk, UdifImage};

const CHUNK: u64 = 128 * 1024;
const MAX_CHUNKS: usize = 128; // 16 MiB of cache

enum Backend {
    /// Image file or raw device.
    File(File),
    /// UDIF (DMG) image: the decompressed virtual disk, decoded block by block on demand.
    Udif(Box<UdifDisk>),
    /// Sparse image / sparse bundle: bands put together on demand, missing bands read as zeros.
    Sparse(Box<SparseDisk>),
}

pub struct DiskSource {
    backend: Backend,
    len: u64,
    tick: u64,
    cache: HashMap<u64, (Vec<u8>, u64)>,
    /// File name of the opened source (without the folder), when it came from a file.
    name: Option<String>,
}

impl DiskSource {
    /// Plain image file or raw device (no container format is looked for; see `detect::open_source`).
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = open_read_shared(path)?;
        let mut len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len == 0 {
            len = device_len(&file).unwrap_or(0);
        }
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "source is empty or its size is unknown"));
        }
        Ok(DiskSource { backend: Backend::File(file), len, tick: 0, cache: HashMap::new(), name: None })
    }

    /// UDIF image as one virtual disk: all its partitions at their sector positions
    /// (a partition map inside the image, if any, therefore works like on a real disk).
    pub fn open_udif(image: Arc<UdifImage>) -> io::Result<Self> {
        let disk = UdifDisk::open(image)?;
        let len = disk.len();
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the disk image is empty"));
        }
        Ok(DiskSource { backend: Backend::Udif(Box::new(disk)), len, tick: 0, cache: HashMap::new(), name: None })
    }

    /// Sparse image (`.sparseimage`) or sparse bundle as one virtual disk.
    pub fn open_sparse(disk: SparseDisk) -> io::Result<Self> {
        let len = disk.len();
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the disk image is empty"));
        }
        Ok(DiskSource { backend: Backend::Sparse(Box::new(disk)), len, tick: 0, cache: HashMap::new(), name: None })
    }

    /// The UDIF image behind this source (`None` for plain files and devices).
    pub fn udif(&self) -> Option<&Arc<UdifImage>> {
        match &self.backend {
            Backend::Udif(d) => Some(d.image()),
            Backend::File(_) | Backend::Sparse(_) => None,
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// File name of the source (set by `detect::open_source`).
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read up to `buf.len()` bytes at absolute offset `off`; short only at the end.
    pub fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut off = off;
        let mut done = 0usize;
        while done < buf.len() && off < self.len {
            let idx = off / CHUNK;
            let inner = (off % CHUNK) as usize;
            let chunk = self.chunk(idx)?;
            if inner >= chunk.len() {
                break;
            }
            let n = (buf.len() - done).min(chunk.len() - inner);
            buf[done..done + n].copy_from_slice(&chunk[inner..inner + n]);
            done += n;
            off += n as u64;
        }
        Ok(done)
    }

    pub fn read_exact_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        if self.read_at(off, buf)? == buf.len() {
            Ok(())
        } else {
            Err(io::ErrorKind::UnexpectedEof.into())
        }
    }

    fn chunk(&mut self, idx: u64) -> io::Result<&[u8]> {
        self.tick += 1;
        if !self.cache.contains_key(&idx) {
            let start = idx * CHUNK;
            let want = (self.len - start).min(CHUNK) as usize;
            let mut data = vec![0u8; want];
            let got = match &mut self.backend {
                Backend::File(file) => {
                    file.seek(SeekFrom::Start(start))?;
                    let mut got = 0;
                    while got < want {
                        let n = file.read(&mut data[got..])?;
                        if n == 0 {
                            break;
                        }
                        got += n;
                    }
                    got
                }
                Backend::Udif(disk) => disk.read_at(start, &mut data)?,
                Backend::Sparse(disk) => disk.read_at(start, &mut data)?,
            };
            data.truncate(got);
            if self.cache.len() >= MAX_CHUNKS {
                if let Some(oldest) = self.cache.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| *k) {
                    self.cache.remove(&oldest);
                }
            }
            self.cache.insert(idx, (data, self.tick));
        }
        let entry = self.cache.get_mut(&idx).expect("chunk was just inserted");
        entry.1 = self.tick;
        Ok(&entry.0)
    }
}

/// A `[start, start+len)` window of a [`DiskSource`] that behaves like its own file.
pub struct Slice {
    src: Arc<Mutex<DiskSource>>,
    start: u64,
    len: u64,
    pos: u64,
}

impl Slice {
    pub fn new(src: Arc<Mutex<DiskSource>>, start: u64, len: u64) -> Self {
        Slice { src, start, len, pos: 0 }
    }
}

impl Read for Slice {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.len.saturating_sub(self.pos);
        let n = (buf.len() as u64).min(remaining) as usize;
        if n == 0 {
            return Ok(0);
        }
        let mut src = self.src.lock().unwrap_or_else(|e| e.into_inner());
        let got = src.read_at(self.start + self.pos, &mut buf[..n])?;
        self.pos += got as u64;
        Ok(got)
    }
}

impl Seek for Slice {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

// ---------------------------------------------------------------------------
// Platform specifics
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub(crate) fn open_read_shared(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    OpenOptions::new().read(true).share_mode(7).open(path)
}

#[cfg(not(windows))]
pub(crate) fn open_read_shared(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

/// Size of a raw disk/volume handle (where `metadata().len()` is 0).
#[cfg(windows)]
fn device_len(file: &File) -> io::Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO};

    let mut info = GET_LENGTH_INFORMATION { Length: 0 };
    let mut returned = 0u32;
    // SAFETY: the handle is valid for the lifetime of `file`; the out buffer is a
    // properly sized GET_LENGTH_INFORMATION.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            IOCTL_DISK_GET_LENGTH_INFO,
            std::ptr::null(),
            0,
            &mut info as *mut GET_LENGTH_INFORMATION as *mut _,
            std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info.Length.max(0) as u64)
    }
}

#[cfg(not(windows))]
fn device_len(file: &File) -> io::Result<u64> {
    let mut f = file;
    f.seek(SeekFrom::End(0))
}
