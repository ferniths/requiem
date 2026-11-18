//! Native clipboard read fallback for terminals that lack bracketed-paste
//! support.
//!
//! Bracketed paste is the primary path: terminals that support it deliver
//! clipboard content as a single paste event. When it is unavailable the
//! user can press `Alt+V` and this module reads the OS clipboard directly.
//!
//! The implementation uses [`arboard`], which talks to the platform
//! clipboard natively:
//!
//! * Windows — Win32 (`OpenClipboard`/`GetClipboardData`), with the
//!   clipboard handle and global memory held for exactly as long as the
//!   text is being copied. (The previous hand-rolled Win32 code closed the
//!   clipboard *before* reading the pointer, which was undefined behaviour.)
//! * Linux — X11 and Wayland (via `wl-clipboard-rs`), without shelling out
//!   to `xclip`/`wl-paste`.
//! * macOS — `NSPasteboard`.
//!
//! Reads happen on a short-lived worker thread with a timeout so a hung
//! clipboard owner can never freeze the TUI.

use std::sync::mpsc;
use std::time::Duration;

/// How long to wait for the platform clipboard before giving up.
const READ_TIMEOUT: Duration = Duration::from_millis(1500);

/// Returns `true` when a native clipboard read path exists on this platform.
///
/// `arboard` supports every desktop target REQUIEM builds for; availability
/// of an actual clipboard owner still depends on the running session, which
/// is why [`read_clipboard`] returns `None` instead of panicking.
pub fn is_supported() -> bool {
    cfg!(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd"
    ))
}

/// Read the text clipboard, normalized to LF line endings.
///
/// Returns `None` when the clipboard is empty, holds no text (e.g. an
/// image), no clipboard owner is available, or the read times out.
pub fn read_clipboard() -> Option<String> {
    if !is_supported() {
        return None;
    }

    // arboard can block while the owning application services the request
    // (notably on X11/Wayland). Do the read off-thread with a timeout.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_native());
    });
    rx.recv_timeout(READ_TIMEOUT).ok().flatten()
}

fn read_native() -> Option<String> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let text = clipboard.get_text().ok()?;
    if text.is_empty() {
        return None;
    }
    Some(normalize_line_endings(&text))
}

/// Normalize CRLF and lone CR to LF. Bracketed paste and native clipboard
/// text can arrive with platform line endings; the composer and transcript
/// only understand LF.
pub fn normalize_line_endings(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_crlf() {
        assert_eq!(normalize_line_endings("a\r\nb\r\n"), "a\nb\n");
    }

    #[test]
    fn normalizes_lone_cr() {
        assert_eq!(normalize_line_endings("a\rb"), "a\nb");
    }

    #[test]
    fn leaves_lf_untouched() {
        assert_eq!(normalize_line_endings("a\nb"), "a\nb");
    }

    #[test]
    fn preserves_unicode_and_indentation() {
        let text = "  fn main() {\r\n\tprintln!(\"héllo 🌍\");\r\n}";
        assert_eq!(
            normalize_line_endings(text),
            "  fn main() {\n\tprintln!(\"héllo 🌍\");\n}"
        );
    }

    #[test]
    fn supported_on_desktop_targets() {
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        assert!(is_supported());
    }

    #[test]
    fn read_never_panics() {
        // May return None in headless CI; must never panic or hang the test
        // (READ_TIMEOUT bounds the wait).
        let _ = read_clipboard();
    }
}
