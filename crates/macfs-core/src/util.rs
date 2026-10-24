//! Name sanitising, time conversion and path helpers.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use unicode_normalization::UnicodeNormalization;

// ---------------------------------------------------------------------------
// Wide string <-> path
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub fn wide_to_path(w: &[u16]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_wide(w))
}

#[cfg(not(windows))]
pub fn wide_to_path(w: &[u16]) -> PathBuf {
    PathBuf::from(String::from_utf16_lossy(w))
}

/// Windows only: prefix long absolute paths with `\\?\` so they escape MAX_PATH.
#[cfg(windows)]
pub fn long_path(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().into_owned();
    if s.len() < 240 || s.starts_with(r"\\?\") {
        return p;
    }
    let b = s.as_bytes();
    if b.len() > 2 && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        PathBuf::from(format!(r"\\?\{}", s.replace('/', "\\")))
    } else if let Some(rest) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{}", rest.replace('/', "\\")))
    } else {
        p
    }
}

#[cfg(not(windows))]
pub fn long_path(p: PathBuf) -> PathBuf {
    p
}

// ---------------------------------------------------------------------------
// File names
// ---------------------------------------------------------------------------

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Turn one HFS+ name component into something Windows can store:
/// NFC normalisation (HFS+ stores NFD), illegal characters -> `_`,
/// no trailing dots/spaces, no reserved device names.
pub fn sanitize_component(name: &str) -> String {
    let mut out: String = name
        .nfc()
        .map(|c| match c {
            '\u{0}'..='\u{1f}' | '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c => c,
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        return "_".to_string();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    out
}

/// Case-insensitive uniqueness within one directory listing
/// (`a.txt`, `A.txt` -> `a.txt`, `A (2).txt`).
#[derive(Default)]
pub struct NameSet(HashSet<String>);

impl NameSet {
    pub fn unique(&mut self, name: String) -> String {
        if self.0.insert(name.to_lowercase()) {
            return name;
        }
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name.as_str(), ""),
        };
        let mut n = 2u32;
        loop {
            let cand = format!("{stem} ({n}){ext}");
            if self.0.insert(cand.to_lowercase()) {
                return cand;
            }
            n += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Seconds between 1904-01-01 (HFS+ epoch) and 1970-01-01.
pub const HFS_TO_UNIX: i64 = 2_082_844_800;

/// HFS+ timestamp (UTC seconds since 1904) -> unix seconds; `0` means "not set".
pub fn hfs_to_unix(secs: u32) -> Option<i64> {
    if secs == 0 { None } else { Some(secs as i64 - HFS_TO_UNIX) }
}

pub fn unix_to_system_time(unix: i64) -> SystemTime {
    if unix >= 0 {
        UNIX_EPOCH + Duration::from_secs(unix as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(unix.unsigned_abs())
    }
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Pack a calendar time into the DOS `date<<16 | time` format Total Commander wants.
/// Years outside 1980..=2107 are clamped.
pub fn pack_dos(year: i64, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> i32 {
    if year < 1980 {
        return (0x21u32 << 16) as i32; // 1980-01-01 00:00
    }
    let year = year.min(2107);
    let date = (((year - 1980) as u32) << 9) | (month << 5) | day;
    let time = (hour << 11) | (min << 5) | (sec / 2);
    ((date << 16) | time) as i32
}

/// Local wall-clock time of a UTC unix timestamp: Windows time zone rules for
/// the actual date (DST aware); plain UTC on other platforms (tests).
#[cfg(windows)]
fn to_local(unix: i64) -> i64 {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    let ticks = ((unix + 11_644_473_600) as u64) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: valid pointers to properly initialised structs; null time zone = current zone.
    let ok = unsafe {
        FileTimeToSystemTime(&ft, &mut utc) != 0
            && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
    };
    if !ok {
        return unix;
    }
    let days = days_from_civil(local.wYear as i64, local.wMonth as u32, local.wDay as u32);
    days * 86_400 + local.wHour as i64 * 3600 + local.wMinute as i64 * 60 + local.wSecond as i64
}

#[cfg(not(windows))]
fn to_local(unix: i64) -> i64 {
    unix
}

#[cfg(windows)]
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `FileTime` field of the WCX header (DOS date/time, local time).
pub fn dos_time(unix: Option<i64>) -> i32 {
    let Some(u) = unix else { return pack_dos(1980, 1, 1, 0, 0, 0) };
    let t = to_local(u);
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400) as u32;
    pack_dos(y, m, d, s / 3600, (s % 3600) / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(sanitize_component("a:b?c"), "a_b_c");
        assert_eq!(sanitize_component("trail. "), "trail");
        assert_eq!(sanitize_component("NUL.txt"), "_NUL.txt");
        assert_eq!(sanitize_component("e\u{301}"), "\u{e9}"); // NFD -> NFC
        assert_eq!(sanitize_component("\r\r"), "__");
        let mut s = NameSet::default();
        assert_eq!(s.unique("a.txt".into()), "a.txt");
        assert_eq!(s.unique("A.TXT".into()), "A (2).TXT");
    }

    #[test]
    fn dos() {
        // 2024-02-29 12:34:56 UTC
        let unix = 1_709_210_096;
        let v = dos_time(Some(unix)) as u32;
        assert_eq!(v >> 25, 44); // 2024 - 1980
        assert_eq!((v >> 21) & 0xF, 2);
        assert_eq!((v >> 16) & 0x1F, 29);
        assert_eq!((v >> 11) & 0x1F, 12);
        assert_eq!((v >> 5) & 0x3F, 34);
        assert_eq!(v & 0x1F, 28);
        assert_eq!(hfs_to_unix(2_082_844_800), Some(0));
    }
}
