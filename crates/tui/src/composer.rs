//! Multiline prompt composer for the REQUIEM TUI.
//!
//! Replaces the old single-line input with a proper multiline text editor
//! that supports cursor movement, undo/redo, and correct paste handling.
//! The composer is used by `App` for the input area of the session screen.

use std::collections::VecDeque;
use std::time::Instant;

/// A single-line text buffer with cursor tracking.
#[derive(Debug, Clone, Default)]
pub struct Composer {
    /// The full text (may contain newlines).
    pub text: String,
    /// Char index of the cursor (0..=text.chars().count()).
    pub cursor: usize,
    /// Whether the text contains newlines (i.e. is multiline).
    pub multiline: bool,
    /// Timestamp of the last character insertion (for paste-flood detection).
    pub last_char_at: Option<Instant>,
    /// Undo stack (newest last).
    pub undo_stack: VecDeque<(String, usize)>,
    /// Redo stack.
    pub redo_stack: VecDeque<(String, usize)>,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Char count of the text.
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The cursor as a byte offset into `text`.
    pub fn cursor_byte(&self) -> usize {
        char_to_byte(&self.text, self.cursor)
    }

    /// Insert a single character at the cursor.
    pub fn insert_char(&mut self, c: char) {
        self.push_undo();
        let byte = self.cursor_byte();
        self.text.insert(byte, c);
        self.cursor += 1;
        self.multiline = self.text.contains('\n');
        self.last_char_at = Some(Instant::now());
    }

    /// Delete the character before the cursor.
    pub fn delete_before(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo();
        let idx = char_to_byte(&self.text, self.cursor - 1);
        self.text.remove(idx);
        self.cursor -= 1;
        self.multiline = self.text.contains('\n');
    }

    /// Delete the character at the cursor.
    pub fn delete_after(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        self.push_undo();
        let idx = char_to_byte(&self.text, self.cursor);
        self.text.remove(idx);
        self.multiline = self.text.contains('\n');
    }

    /// Move cursor left by one char.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move cursor right by one char.
    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    /// Move cursor to start.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to end.
    pub fn move_end(&mut self) {
        self.cursor = self.len();
    }

    /// Move cursor back one word.
    pub fn move_word_left(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cursor = i;
    }

    /// Move cursor forward one word.
    pub fn move_word_right(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        let mut i = self.cursor.min(n);
        while i < n && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        self.cursor = i;
    }

    /// Delete the word before the cursor.
    pub fn delete_word_left(&mut self) {
        self.push_undo();
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let byte = chars[..i].iter().map(|c| c.len_utf8()).sum();
        self.text.truncate(byte);
        self.cursor = i;
        self.multiline = self.text.contains('\n');
    }

    /// Clear text before the cursor (Ctrl+U).
    pub fn delete_to_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo();
        let byte = self.cursor_byte();
        // Keep text from cursor onwards, remove everything before it.
        let kept = self.text[byte..].to_string();
        self.text = kept;
        self.cursor = 0;
        self.multiline = self.text.contains('\n');
    }

    /// Clear text after the cursor (Ctrl+K).
    pub fn delete_to_end(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        self.push_undo();
        let byte = self.cursor_byte();
        self.text.truncate(byte);
        self.multiline = self.text.contains('\n');
    }

    /// Insert a newline at the cursor.
    pub fn insert_newline(&mut self) {
        self.push_undo();
        let byte = self.cursor_byte();
        self.text.insert(byte, '\n');
        self.cursor += 1;
        self.multiline = true;
    }

    /// Push the current state onto the undo stack.
    pub fn push_undo(&mut self) {
        self.undo_stack.push_back((self.text.clone(), self.cursor));
        if self.undo_stack.len() > 100 {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
    }

    /// Undo the last change.
    pub fn undo(&mut self) -> bool {
        if let Some((text, cursor)) = self.undo_stack.pop_back() {
            self.redo_stack.push_back((self.text.clone(), self.cursor));
            self.text = text;
            self.cursor = cursor;
            self.multiline = self.text.contains('\n');
            true
        } else {
            false
        }
    }

    /// Redo the last undone change.
    pub fn redo(&mut self) -> bool {
        if let Some((text, cursor)) = self.redo_stack.pop_back() {
            self.undo_stack.push_back((self.text.clone(), self.cursor));
            self.text = text;
            self.cursor = cursor;
            self.multiline = self.text.contains('\n');
            true
        } else {
            false
        }
    }

    /// Insert text at the cursor as a single undo operation.
    /// This is the paste entry point — newlines are preserved.
    pub fn insert_paste(&mut self, text: &str) {
        // Normalize line endings: CRLF → LF, CR → LF.
        let normalized = normalize_line_endings(text);
        self.push_undo();
        let byte = self.cursor_byte();
        self.text.insert_str(byte, &normalized);
        self.cursor = byte + normalized.chars().count();
        self.multiline = self.text.contains('\n');
        self.last_char_at = Some(Instant::now());
    }

    /// Clear all text and reset cursor.
    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.push_undo();
        }
        self.text.clear();
        self.cursor = 0;
        self.multiline = false;
        self.last_char_at = None;
    }

    /// Whether a paste flood is detected (large text, recent insertion).
    pub fn is_paste_flood(&self) -> bool {
        self.text.chars().count() >= 20
            && self
                .last_char_at
                .map(|t| t.elapsed() < std::time::Duration::from_millis(100))
                .unwrap_or(false)
    }

    /// The trimmed text (for submission).
    pub fn trimmed(&self) -> &str {
        self.text.trim()
    }

    /// The text as a single line (newlines replaced with spaces).
    /// Used for history and submission.
    pub fn flatten(&self) -> String {
        self.text.replace('\n', " ")
    }

    /// Compact placeholder for large multi-line pastes.
    ///
    /// Returns `None` for short pastes (less than 50 lines and less than
    /// 4 KB). For large content, returns a single-line summary such as
    /// `"[Pasted text · 240 lines · 18.4 KB]"`.
    pub fn paste_placeholder(&self) -> Option<String> {
        let line_count = self.text.lines().count();
        let byte_len = self.text.len();
        if line_count < 50 && byte_len < 4096 {
            return None;
        }
        let kb = byte_len as f64 / 1024.0;
        Some(format!("[Pasted text · {line_count} lines · {kb:.1} KB]"))
    }

    /// True when the composer content is large enough that a compact
    /// placeholder should be shown in the input area.
    pub fn should_show_placeholder(&self) -> bool {
        self.paste_placeholder().is_some()
    }
}

/// Normalize line endings: CRLF → LF, CR → LF.
pub fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Byte offset of char index `idx` in `s`.
fn char_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices().nth(idx).map(|(b, _)| b).unwrap_or(s.len())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_cursor() {
        let mut c = Composer::new();
        for ch in "hello".chars() {
            c.insert_char(ch);
        }
        assert_eq!(c.text, "hello");
        assert_eq!(c.cursor, 5);
        c.move_left();
        c.move_left();
        c.move_left();
        assert_eq!(c.cursor, 2);
        c.insert_char('X');
        assert_eq!(c.text, "heXllo");
        assert_eq!(c.cursor, 3);
    }

    #[test]
    fn delete_before_and_after() {
        let mut c = Composer::new();
        for ch in "abc".chars() {
            c.insert_char(ch);
        }
        c.delete_before();
        assert_eq!(c.text, "ab");
        assert_eq!(c.cursor, 2);
        c.move_left();
        c.delete_after();
        assert_eq!(c.text, "a");
        assert_eq!(c.cursor, 1);
    }

    #[test]
    fn word_movement() {
        let mut c = Composer::new();
        for ch in "hello world".chars() {
            c.insert_char(ch);
        }
        c.move_end();
        c.move_word_left();
        assert_eq!(c.cursor, 6); // start of "world"
        c.move_word_left();
        assert_eq!(c.cursor, 0); // start of "hello"
        c.move_end();
        c.move_word_left();
        c.move_word_right();
        assert_eq!(c.cursor, 11); // end
    }

    #[test]
    fn undo_redo() {
        let mut c = Composer::new();
        for ch in "abc".chars() {
            c.insert_char(ch);
        }
        c.delete_before(); // "ab"
        assert!(c.undo());
        assert_eq!(c.text, "abc");
        assert!(c.redo());
        assert_eq!(c.text, "ab");
        assert_eq!(c.cursor, 2);
    }

    #[test]
    fn paste_preserves_multiline() {
        let mut c = Composer::new();
        c.insert_paste("line1\nline2\nline3");
        assert_eq!(c.text, "line1\nline2\nline3");
        assert!(c.multiline);
        assert_eq!(c.cursor, 17);
    }

    #[test]
    fn paste_crlf_normalized() {
        let mut c = Composer::new();
        c.insert_paste("line1\r\nline2\r\nline3");
        assert_eq!(c.text, "line1\nline2\nline3");
    }

    #[test]
    fn paste_cr_only_normalized() {
        let mut c = Composer::new();
        c.insert_paste("line1\rline2\rline3");
        assert_eq!(c.text, "line1\nline2\nline3");
    }

    #[test]
    fn paste_in_middle() {
        let mut c = Composer::new();
        for ch in "abc".chars() {
            c.insert_char(ch);
        }
        c.move_home();
        c.insert_paste("XY");
        assert_eq!(c.text, "XYabc");
        assert_eq!(c.cursor, 2);
    }

    #[test]
    fn paste_is_single_undo() {
        let mut c = Composer::new();
        c.insert_paste("hello world");
        c.insert_paste("!");
        assert_eq!(c.text, "hello world!");
        c.undo();
        assert_eq!(c.text, "hello world");
        c.undo();
        assert!(c.text.is_empty());
    }

    #[test]
    fn delete_to_start_and_end() {
        let mut c = Composer::new();
        for ch in "abcdef".chars() {
            c.insert_char(ch);
        }
        c.move_left();
        c.move_left();
        c.delete_to_end(); // "abcd"
        assert_eq!(c.text, "abcd");
        c.delete_to_start(); // ""
        assert_eq!(c.text, "");
        assert_eq!(c.cursor, 0);
    }

    #[test]
    fn paste_flood_detection() {
        let mut c = Composer::new();
        let large = "a".repeat(100);
        c.insert_paste(&large);
        assert!(c.is_paste_flood());
        // After the 100ms window passes (simulated by clearing the timestamp),
        // it should not be a flood.
        c.last_char_at = None;
        assert!(!c.is_paste_flood());
    }

    #[test]
    fn flatten_replaces_newlines() {
        let mut c = Composer::new();
        c.insert_paste("line1\nline2");
        assert_eq!(c.flatten(), "line1 line2");
    }

    #[test]
    fn clear_resets_state() {
        let mut c = Composer::new();
        c.insert_paste("hello\nworld");
        c.clear();
        assert!(c.text.is_empty());
        assert_eq!(c.cursor, 0);
        assert!(!c.multiline);
        // Undo should restore.
        assert!(c.undo());
        assert_eq!(c.text, "hello\nworld");
    }

    #[test]
    fn placeholder_none_for_short_text() {
        let mut c = Composer::new();
        c.insert_paste("hello world");
        assert!(c.paste_placeholder().is_none());
        assert!(!c.should_show_placeholder());
    }

    #[test]
    fn placeholder_shown_for_many_lines() {
        let mut c = Composer::new();
        let text = (0..100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        c.insert_paste(&text);
        let ph = c.paste_placeholder().unwrap();
        assert!(ph.contains("100 lines"), "got: {ph}");
        assert!(ph.contains("KB"), "got: {ph}");
    }

    #[test]
    fn placeholder_shown_for_large_bytes() {
        let mut c = Composer::new();
        let text = "x".repeat(8000);
        c.insert_paste(&text);
        let ph = c.paste_placeholder().unwrap();
        assert!(ph.contains("KB"), "got: {ph}");
    }

    #[test]
    fn placeholder_preserves_content() {
        let mut c = Composer::new();
        let lines: Vec<String> = (0..60).map(|i| format!("fn f{i}() {{}}")).collect();
        let text = lines.join("\n");
        c.insert_paste(&text);
        // Placeholder is shown but the text is intact.
        assert!(c.should_show_placeholder());
        assert_eq!(c.text, text, "content must remain intact");
        // Flatten preserves all lines.
        assert_eq!(c.flatten(), text.replace('\n', " "));
    }
}
