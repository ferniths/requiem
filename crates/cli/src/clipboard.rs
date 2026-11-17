//! Native clipboard read fallback for terminals that lack bracketed-paste
//! support.
//!
//! On Windows the user can press `Alt+V` to read the clipboard directly
//! from the Win32 API. On other platforms bracketed paste is the primary
//! mechanism; this module is a no-op.

/// Returns `true` when the platform supports a native clipboard read path.
pub fn is_supported() -> bool {
    cfg!(windows)
}

/// Read the text clipboard. Returns `None` when the clipboard is empty,
/// contains no text, or the platform does not support native reads.
///
/// On Windows this calls `GetClipboardData(CF_UNICODETEXT)` via the
/// Win32 API. The returned string is normalised to LF line endings.
pub fn read_clipboard() -> Option<String> {
    #[cfg(windows)]
    {
        windows_clipboard()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// ── Windows implementation ───────────────────────────────────────────

#[cfg(windows)]
fn windows_clipboard() -> Option<String> {
    use std::ffi::c_void;

    extern "system" {
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(uFormat: u32) -> *mut c_void;
        fn GlobalLock(hMem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(hMem: *mut c_void) -> i32;
        fn OpenClipboard(hwnd: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
    }

    const CF_UNICODETEXT: u32 = 13;

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let ok = IsClipboardFormatAvailable(CF_UNICODETEXT);
        let handle = if ok != 0 {
            GetClipboardData(CF_UNICODETEXT)
        } else {
            std::ptr::null_mut()
        };
        CloseClipboard();

        let ptr = if handle.is_null() {
            return None;
        } else {
            GlobalLock(handle)
        };
        if !ptr.is_null() {
            GlobalUnlock(handle);
        }

        if ptr.is_null() {
            return None;
        }

        let wide = ptr as *const u16;
        let mut count = 0;
        while *wide.add(count) != 0 {
            count += 1;
        }
        let chars: Vec<u16> = std::slice::from_raw_parts(wide, count).to_vec();
        let text = String::from_utf16_lossy(&chars);
        Some(text.replace("\r\n", "\n").replace('\r', "\n"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    #[test]
    fn not_supported_off_windows() {
        assert!(!super::is_supported());
        assert!(super::read_clipboard().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn supported_on_windows() {
        assert!(super::is_supported());
    }
}
