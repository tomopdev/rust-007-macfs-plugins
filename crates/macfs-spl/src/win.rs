//! The only platform-specific layer of the plugin: ANSI code page conversion
//! and message boxes.
//!
//! The Salamander plugin API passes every file name as `char*` in the current
//! ANSI code page (ACP). On Windows this module uses the real ACP; on other
//! platforms (unit tests) it emulates an ACP that can represent U+0000..U+00FF
//! (Latin-1), which is enough to exercise all the "not representable" logic.

use std::ffi::c_void;

/// Which dialog to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Info,
    Error,
    Warning,
    /// Yes / No / Cancel question.
    YesNoCancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Ok,
    Yes,
    No,
    Cancel,
}

/// Convert to the ANSI code page; characters it cannot represent become `_`.
pub fn to_ansi(s: &str) -> Vec<u8> {
    imp::to_ansi(s)
}

/// Convert ANSI code page bytes to a string.
pub fn from_ansi(b: &[u8]) -> String {
    imp::from_ansi(b)
}

pub fn message_box(parent: *mut c_void, text: &str, title: &str, kind: Kind) -> Answer {
    imp::message_box(parent, text, title, kind)
}

/// `s` as the panel will show it: round-tripped through the ACP (unrepresentable
/// characters -> `_`) and shortened until it fits into `max_bytes` ANSI bytes.
pub fn ansi_safe(s: &str, max_bytes: usize) -> String {
    let mut s = s.to_string();
    loop {
        let bytes = to_ansi(&s);
        if bytes.len() <= max_bytes || s.is_empty() {
            return from_ansi(&bytes);
        }
        s.pop();
    }
}

#[cfg(windows)]
mod imp {
    use super::{Answer, Kind};
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Globalization::{CP_ACP, GetACP, MultiByteToWideChar, WC_NO_BEST_FIT_CHARS, WideCharToMultiByte};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDCANCEL, IDNO, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNOCANCEL, MessageBoxW,
    };

    pub fn to_ansi(s: &str) -> Vec<u8> {
        let wide: Vec<u16> = s.encode_utf16().collect();
        if wide.is_empty() {
            return Vec::new();
        }
        // SAFETY: all pointers reference live buffers of the stated lengths.
        unsafe {
            // UTF-8 ACP (65001) rejects WC_NO_BEST_FIT_CHARS and a default char; it can encode everything anyway.
            let utf8 = GetACP() == 65001;
            let mut used_default = 0i32;
            let (flags, def, used): (u32, *const u8, *mut i32) =
                if utf8 { (0, ptr::null(), ptr::null_mut()) } else { (WC_NO_BEST_FIT_CHARS, b"_\0".as_ptr(), &mut used_default) };
            let n = WideCharToMultiByte(CP_ACP, flags, wide.as_ptr(), wide.len() as i32, ptr::null_mut(), 0, def, used);
            if n <= 0 {
                return s.bytes().map(|b| if b.is_ascii() { b } else { b'_' }).collect();
            }
            let mut buf = vec![0u8; n as usize];
            let n2 = WideCharToMultiByte(CP_ACP, flags, wide.as_ptr(), wide.len() as i32, buf.as_mut_ptr(), n, def, used);
            buf.truncate(n2.max(0) as usize);
            buf
        }
    }

    pub fn from_ansi(b: &[u8]) -> String {
        if b.is_empty() {
            return String::new();
        }
        // SAFETY: pointers reference live buffers of the stated lengths.
        unsafe {
            let n = MultiByteToWideChar(CP_ACP, 0, b.as_ptr(), b.len() as i32, ptr::null_mut(), 0);
            if n <= 0 {
                return String::from_utf8_lossy(b).into_owned();
            }
            let mut w = vec![0u16; n as usize];
            let n2 = MultiByteToWideChar(CP_ACP, 0, b.as_ptr(), b.len() as i32, w.as_mut_ptr(), n);
            w.truncate(n2.max(0) as usize);
            String::from_utf16_lossy(&w)
        }
    }

    pub fn message_box(parent: *mut c_void, text: &str, title: &str, kind: Kind) -> Answer {
        let mut t: Vec<u16> = text.encode_utf16().collect();
        t.push(0);
        let mut c: Vec<u16> = title.encode_utf16().collect();
        c.push(0);
        let style = match kind {
            Kind::Info => MB_OK | MB_ICONINFORMATION,
            Kind::Error => MB_OK | MB_ICONERROR,
            Kind::Warning => MB_OK | MB_ICONWARNING,
            Kind::YesNoCancel => MB_YESNOCANCEL | MB_ICONWARNING,
        };
        // SAFETY: NUL-terminated buffers; `parent` is NULL or a window handle given by Salamander.
        let r = unsafe { MessageBoxW(parent, t.as_ptr(), c.as_ptr(), style) };
        match r {
            IDYES => Answer::Yes,
            IDNO => Answer::No,
            IDCANCEL => Answer::Cancel,
            _ => Answer::Ok,
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Answer, Kind};
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;

    thread_local! {
        static ANSWER: Cell<Answer> = const { Cell::new(Answer::Yes) };
        static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Test hook: answer given to the next `YesNoCancel` questions on this thread.
    pub fn set_answer(a: Answer) {
        ANSWER.with(|c| c.set(a));
    }

    /// Test hook: take (and clear) all message box texts shown on this thread.
    pub fn take_messages() -> Vec<String> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    pub fn to_ansi(s: &str) -> Vec<u8> {
        s.chars().map(|c| if (c as u32) <= 0xFF { c as u32 as u8 } else { b'_' }).collect()
    }

    pub fn from_ansi(b: &[u8]) -> String {
        b.iter().map(|&x| x as char).collect()
    }

    pub fn message_box(_parent: *mut c_void, text: &str, _title: &str, kind: Kind) -> Answer {
        LOG.with(|l| l.borrow_mut().push(text.to_string()));
        match kind {
            Kind::YesNoCancel => ANSWER.with(|c| c.get()),
            _ => Answer::Ok,
        }
    }
}

#[cfg(not(windows))]
pub use imp::{set_answer, take_messages};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_roundtrip_and_limits() {
        assert_eq!(ansi_safe("abc", 10), "abc");
        assert_eq!(ansi_safe("\u{65e5}\u{672c}.txt", 255), "__.txt");
        assert_eq!(ansi_safe("abcdef", 3), "abc");
        assert_eq!(from_ansi(&to_ansi("caf\u{e9}")), "caf\u{e9}");
    }
}
