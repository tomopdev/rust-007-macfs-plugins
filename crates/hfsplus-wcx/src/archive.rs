//! Platform-neutral plugin core: opens volumes, walks them lazily one header at
//! a time (what `ReadHeaderExW` needs) and extracts files (`ProcessFileW`).

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use hfsplus::{DirEntry, EntryKind, HfsPlusError, HfsVolume};

use crate::detect::{self, LocateError};
use crate::ffi::{self, ProcessDataProcW};
use crate::source::Slice;
use crate::util;

/// One item reported to Total Commander.
#[derive(Debug, Clone)]
pub struct Entry {
    vol: usize,
    /// Path inside the HFS+ volume (`/` separated, raw on-disk names).
    hfs_path: String,
    /// Path shown to Total Commander (`\` separated, Windows-safe names).
    pub disp: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time, unix seconds (UTC).
    pub mtime: Option<i64>,
}

struct Job {
    vol: usize,
    hfs_path: String,
    disp: String,
}

/// Why `Archive::open` failed.
#[derive(Debug)]
pub enum OpenError {
    Io(io::Error),
    /// `.hfsdev` file is malformed.
    Config(String),
    /// Nothing that looks like HFS+/HFSX was found.
    NoVolume,
    /// A volume was found but could not be parsed.
    Hfs(HfsPlusError),
}

impl OpenError {
    pub fn code(&self) -> i32 {
        match self {
            OpenError::Io(_) => ffi::E_EOPEN,
            OpenError::Config(_) => ffi::E_BAD_ARCHIVE,
            OpenError::NoVolume => ffi::E_UNKNOWN_FORMAT,
            OpenError::Hfs(HfsPlusError::Io(_)) => ffi::E_EREAD,
            OpenError::Hfs(_) => ffi::E_BAD_ARCHIVE,
        }
    }
}

pub struct Archive {
    /// Name of the opened source, copied into every header.
    pub arc_name: String,
    vols: Vec<HfsVolume<Slice>>,
    work: Vec<Job>,
    pending: VecDeque<Entry>,
    current: Option<Entry>,
    listed_any: bool,
    /// Directories that could not be read (damaged catalog) and were skipped.
    pub skipped_dirs: usize,
    pub progress: Option<ProcessDataProcW>,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Archive, OpenError> {
        let (src, parts) = detect::locate(path).map_err(|e| match e {
            LocateError::Io(e) => OpenError::Io(e),
            LocateError::Config(m) => OpenError::Config(m),
        })?;
        if parts.is_empty() {
            return Err(OpenError::NoVolume);
        }
        let src = Arc::new(Mutex::new(src));

        let mut vols = Vec::new();
        let mut labels = Vec::new();
        let mut first_err = None;
        for (i, p) in parts.iter().enumerate() {
            match HfsVolume::open(Slice::new(src.clone(), p.start, p.len)) {
                Ok(v) => {
                    vols.push(v);
                    labels.push(if p.label.is_empty() {
                        format!("Partition {}", i + 1)
                    } else {
                        format!("Partition {} - {}", i + 1, p.label)
                    });
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if vols.is_empty() {
            return Err(OpenError::Hfs(first_err.expect("at least one volume was tried")));
        }

        let mut work = Vec::new();
        let mut pending = VecDeque::new();
        if vols.len() == 1 {
            work.push(Job { vol: 0, hfs_path: "/".into(), disp: String::new() });
        } else {
            // Several volumes: one top-level folder per volume.
            let mut names = util::NameSet::default();
            for (i, label) in labels.iter().enumerate() {
                let disp = names.unique(util::sanitize_component(label));
                pending.push_back(Entry { vol: i, hfs_path: "/".into(), disp, is_dir: true, size: 0, mtime: None });
            }
        }

        Ok(Archive {
            arc_name: path.to_string_lossy().into_owned(),
            vols,
            work,
            pending,
            current: None,
            listed_any: false,
            skipped_dirs: 0,
            progress: None,
        })
    }

    /// Next item of the walk, `Ok(None)` at the end. Directories are reported
    /// before their contents.
    pub fn next_header(&mut self) -> Result<Option<Entry>, HfsPlusError> {
        loop {
            if let Some(e) = self.pending.pop_front() {
                if e.is_dir {
                    self.work.push(Job { vol: e.vol, hfs_path: e.hfs_path.clone(), disp: e.disp.clone() });
                }
                self.current = Some(e.clone());
                return Ok(Some(e));
            }
            let Some(job) = self.work.pop() else {
                self.current = None;
                return Ok(None);
            };
            match self.vols[job.vol].list_directory(&job.hfs_path) {
                Ok(list) => {
                    self.listed_any = true;
                    self.enqueue(&job, list);
                }
                // Nothing listed yet: the volume is unusable, report it.
                Err(e) if !self.listed_any => return Err(e),
                // Damaged subdirectory: skip it, keep the rest of the volume usable.
                Err(_) => self.skipped_dirs += 1,
            }
        }
    }

    fn enqueue(&mut self, job: &Job, mut list: Vec<DirEntry>) {
        list.sort_by_cached_key(|d| d.name.to_lowercase());
        let mut names = util::NameSet::default();
        for d in list {
            let shown = names.unique(util::sanitize_component(&d.name));
            let is_dir = d.kind == EntryKind::Directory;
            self.pending.push_back(Entry {
                vol: job.vol,
                hfs_path: if job.hfs_path == "/" {
                    format!("/{}", d.name)
                } else {
                    format!("{}/{}", job.hfs_path, d.name)
                },
                disp: if job.disp.is_empty() { shown } else { format!("{}\\{}", job.disp, shown) },
                is_dir,
                size: if is_dir { 0 } else { d.size },
                mtime: util::hfs_to_unix(d.modify_date),
            });
        }
    }

    /// `ProcessFile`: handle the item returned by the last `next_header`.
    /// Returns a WCX result code (0 = OK).
    pub fn process(&mut self, op: i32, dest: Option<&Path>) -> i32 {
        let Some(entry) = self.current.clone() else {
            return ffi::E_BAD_DATA;
        };
        if op == ffi::PK_SKIP {
            return ffi::E_OK;
        }
        if entry.is_dir {
            if op == ffi::PK_EXTRACT {
                let Some(d) = dest else { return ffi::E_ECREATE };
                if fs::create_dir_all(util::long_path(d.to_path_buf())).is_err() {
                    return ffi::E_ECREATE;
                }
            }
            return ffi::E_OK;
        }

        let extract_to = if op == ffi::PK_EXTRACT {
            let Some(d) = dest else { return ffi::E_ECREATE };
            let d = util::long_path(d.to_path_buf());
            if let Some(parent) = d.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match File::create(&d) {
                Ok(f) => Some((d, f)),
                Err(_) => return ffi::E_ECREATE,
            }
        } else {
            None
        };

        let mut sink = ProgressWriter {
            file: extract_to.as_ref().map(|(_, f)| BufWriter::with_capacity(1 << 20, f)),
            cb: self.progress,
            name: ffi::to_wide_nul(&entry.disp),
            aborted: false,
            write_failed: false,
        };
        let read = self.vols[entry.vol].read_file_to(&entry.hfs_path, &mut sink);
        let flushed = sink.flush();

        let code = match read {
            Ok(_) if flushed.is_err() => ffi::E_EWRITE,
            Ok(_) => ffi::E_OK,
            Err(_) if sink.aborted => ffi::E_EABORTED,
            Err(_) if sink.write_failed => ffi::E_EWRITE,
            Err(HfsPlusError::Io(_)) => ffi::E_EREAD,
            Err(_) => ffi::E_BAD_DATA,
        };
        drop(sink);

        if let Some((path, file)) = extract_to {
            if code == ffi::E_OK {
                if let Some(t) = entry.mtime {
                    let _ = file.set_modified(util::unix_to_system_time(t));
                }
            } else {
                drop(file);
                let _ = fs::remove_file(path); // never leave a truncated file behind
            }
        }
        code
    }
}

/// Writer that forwards to the destination file (if any) and reports progress
/// to Total Commander in at most 1 MiB steps.
struct ProgressWriter<'a> {
    file: Option<BufWriter<&'a File>>,
    cb: Option<ProcessDataProcW>,
    name: Vec<u16>,
    aborted: bool,
    write_failed: bool,
}

impl Write for ProgressWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(1 << 20);
        if let Some(f) = &mut self.file {
            if let Err(e) = f.write_all(&buf[..n]) {
                self.write_failed = true;
                return Err(e);
            }
        }
        if let Some(cb) = self.cb {
            // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the call;
            // the callback pointer was supplied by Total Commander.
            let keep_going = unsafe { cb(self.name.as_mut_ptr(), n as i32) };
            if keep_going == 0 {
                self.aborted = true;
                return Err(io::Error::other("aborted by user"));
            }
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}
