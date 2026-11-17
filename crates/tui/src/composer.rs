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
    ///
    /// Standard editor behavior: skip trailing whitespace, then skip the
    /// preceding word. All content after the cursor is preserved intact.
    /// If the cursor is at the start of a word (after whitespace), the
    /// whitespace is included in the deletion.
    pub fn delete_word_left(&mut self) {
        self.push_undo();
        let chars: Vec<char> = self.text.chars().collect();
        let cursor_char = self.cursor.min(chars.len());
        let mut i = cursor_char;
        // Pass 1: skip trailing whitespace.
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        // Pass 2: skip the word.
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let removed_count = cursor_char - i;
        if removed_count == 0 {
            self.redo_stack.clear();
            return;
        }
        let before: String = chars[..i].iter().collect();
        let after: String = chars[cursor_char..].iter().collect();
        self.text = format!("{before}{after}");
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

// ── Soft wrapping ─────────────────────────────────────────────────────

/// A visual line produced by soft-wrapping a logical line of text to a
/// given width. Carries the starting char index so the cursor can be
/// mapped back to a (row, col) position.
#[derive(Debug, Clone)]
pub struct WrappedLine {
    /// The visible text for this row.
    pub text: String,
    /// Char index in the original logical line where this visual row starts.
    pub start_char: usize,
}

/// Soft-wrap a single logical line (no embedded `\n`) to `width` columns.
/// Long words are broken mid-word rather than overflowing.
pub fn soft_wrap_line(line: &str, width: usize) -> Vec<WrappedLine> {
    let width = width.max(1);
    if line.is_empty() {
        return vec![WrappedLine {
            text: String::new(),
            start_char: 0,
        }];
    }
    let chars: Vec<char> = line.chars().collect();
    let total = chars.len();
    if total <= width {
        return vec![WrappedLine {
            text: line.to_string(),
            start_char: 0,
        }];
    }

    let mut result = Vec::new();
    let mut pos = 0;
    while pos < total {
        // Find the last space within the window; break there if found.
        let window_end = (pos + width).min(total);
        let mut break_at = window_end;
        for i in (pos + 1..window_end).rev() {
            if chars[i].is_whitespace() {
                break_at = i;
                break;
            }
        }
        // If no space found, hard-break at window_end.
        let end = if break_at > pos + 1 {
            break_at
        } else {
            window_end
        };
        let text: String = chars[pos..end].iter().collect();
        result.push(WrappedLine {
            text,
            start_char: pos,
        });
        pos = end;
    }
    result
}

/// Compute the visual row and column of the cursor for a given composer
/// state and terminal width. Returns `(row, col)` where row is the index
/// into the vector of all visual rows and col is the character position
/// within that visual row's text.
///
/// The `all_visual_rows` is built by first splitting the text on `\n`
/// (logical lines), then soft-wrapping each logical line.
pub fn cursor_visual_position(text: &str, cursor_char: usize, width: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    let cursor = cursor_char.min(total);

    // Find which logical line the cursor is on.
    let mut char_in_line = 0;
    for (i, c) in chars.iter().enumerate() {
        if i >= cursor {
            break;
        }
        if *c == '\n' {
            char_in_line = 0;
        } else {
            char_in_line += 1;
        }
    }

    // Build visual rows for just this logical line.
    let line_start = {
        // Find the start of the current logical line.
        let mut ls = 0;
        for (i, c) in chars.iter().enumerate() {
            if i >= cursor {
                break;
            }
            if *c == '\n' {
                ls = i + 1;
            }
        }
        ls
    };
    let line_end = chars[line_start..]
        .iter()
        .position(|c| *c == '\n')
        .map(|p| line_start + p)
        .unwrap_or(total);
    let logical_text: String = chars[line_start..line_end].iter().collect();
    let wrapped = soft_wrap_line(&logical_text, width);

    // Count total visual rows before this logical line.
    let mut rows_before = 0;
    for i in 0..line_start {
        if chars[i] == '\n' {
            let prev_start = {
                let mut ps = 0;
                for (j, c) in chars.iter().enumerate().take(i) {
                    if *c == '\n' {
                        ps = j + 1;
                    }
                }
                ps
            };
            let prev_end = i;
            let prev_text: String = chars[prev_start..prev_end].iter().collect();
            rows_before += soft_wrap_line(&prev_text, width).len();
        }
    }

    // Find which visual row within this logical line the cursor maps to.
    let mut row_in_line = 0;
    let mut col = char_in_line;
    for wl in &wrapped {
        if char_in_line < wl.start_char + wl.text.chars().count() {
            row_in_line = wrapped
                .iter()
                .position(|w| std::ptr::eq(w, wl))
                .unwrap_or(0);
            col = char_in_line - wl.start_char;
            break;
        }
        row_in_line += 1;
        if row_in_line == wrapped.len() {
            col = 0;
            break;
        }
    }

    (rows_before + row_in_line, col)
}

/// Number of visual rows the composer text will occupy at a given width.
pub fn total_visual_rows(text: &str, width: usize) -> usize {
    if text.is_empty() {
        return 1;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut rows = 0;
    let mut line_start = 0;
    for i in 0..=chars.len() {
        let is_end = i == chars.len() || chars[i] == '\n';
        if is_end {
            let line_text: String = chars[line_start..i].iter().collect();
            rows += soft_wrap_line(&line_text, width).len();
            line_start = i + 1;
        }
    }
    rows.max(1)
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

    // ── Phase 1 regression tests ──────────────────────────────────

    #[test]
    fn ctrl_w_preserves_text_after_cursor() {
        let mut c = Composer::new();
        c.insert_paste("hello world foo bar");
        c.move_end();
        // Cursor at end. Ctrl+W deletes "bar" (word before cursor).
        c.delete_word_left();
        assert_eq!(
            c.text, "hello world foo ",
            "Ctrl+W must not truncate after cursor"
        );
        assert_eq!(c.cursor, 16);
    }

    #[test]
    fn ctrl_w_preserves_multiline_content() {
        let mut c = Composer::new();
        c.insert_paste("line1\nline2\nline3");
        c.move_end();
        c.delete_word_left();
        // Should be "line1\nline2\n" (removed "line3")
        assert_eq!(c.text, "line1\nline2\n");
    }

    #[test]
    fn ctrl_w_at_word_boundary_middle() {
        let mut c = Composer::new();
        c.insert_paste("abc def ghi");
        // Position cursor on 'g' (index 8).
        c.move_home();
        for _ in 0..8 {
            c.move_right();
        }
        c.delete_word_left();
        // "abc def ghi": pass1 skips space→i=7, pass2 skips "def"→i=4.
        // Delete chars 4..8 = "def ". before="abc ", after="ghi". Result: "abc ghi".
        assert_eq!(c.text, "abc ghi");
        assert_eq!(c.cursor, 4);
    }

    #[test]
    fn soft_wrap_short_line_unchanged() {
        let wrapped = soft_wrap_line("hello", 80);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].text, "hello");
    }

    #[test]
    fn soft_wrap_long_word_breaks_mid_word() {
        let long_word = "a".repeat(100);
        let wrapped = soft_wrap_line(&long_word, 40);
        assert!(wrapped.len() >= 3);
        // Total chars preserved.
        let total: usize = wrapped.iter().map(|w| w.text.chars().count()).sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn soft_wrap_breaks_at_spaces() {
        let text = "word1 word2 word3 word4";
        let wrapped = soft_wrap_line(text, 10);
        // Each visual line should be at most 10 chars (including trailing space).
        for wl in &wrapped {
            assert!(
                wl.text.chars().count() <= 10,
                "line too long: {:?}",
                wl.text
            );
        }
        // Concatenating all wrapped lines should reproduce the original text.
        let reconstructed: String = wrapped.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            reconstructed, text,
            "wrapped lines must reconstruct the original"
        );
    }

    #[test]
    fn cursor_visual_position_on_wrapped_line() {
        let text = "aaaaaaaaaa bbbbbbbbbb"; // 10 a's, space, 10 b's
        let width = 12;
        // Cursor at char 15 (middle of b's).
        let (row, col) = cursor_visual_position(text, 15, width);
        assert_eq!(row, 1, "should be on second visual row");
        assert!(col > 0, "should have a column offset");
    }

    #[test]
    fn total_visual_rows_empty_text() {
        assert_eq!(total_visual_rows("", 80), 1);
    }

    #[test]
    fn total_visual_rows_single_line() {
        assert_eq!(total_visual_rows("hello world", 80), 1);
    }

    #[test]
    fn total_visual_rows_multiline() {
        assert_eq!(total_visual_rows("line1\nline2\nline3", 80), 3);
    }

    #[test]
    fn total_visual_rows_wrapped() {
        let long = "x".repeat(100);
        let rows = total_visual_rows(&long, 40);
        assert!(
            rows >= 3,
            "100 chars at width 40 should produce ≥ 3 rows, got {rows}"
        );
    }

    #[test]
    fn unicode_cursor_movement() {
        let mut c = Composer::new();
        c.insert_paste("hello 🌍 world");
        let len = c.len();
        assert_eq!(len, "hello 🌍 world".chars().count());
        c.move_end();
        c.move_left();
        c.move_left();
        c.delete_before();
        c.delete_before();
        // Removed "or" → "hello 🌍 wld"
        assert_eq!(c.text, "hello 🌍 wld");
    }

    #[test]
    fn cjk_cursor_movement() {
        let mut c = Composer::new();
        c.insert_paste("你好世界");
        assert_eq!(c.len(), 4);
        c.move_home();
        c.move_right();
        c.move_right();
        assert_eq!(c.cursor, 2);
        c.delete_before();
        // Removed "好" → "你世界"
        assert_eq!(c.text, "你世界");
    }

    #[test]
    fn multiline_paste_never_submits() {
        let mut c = Composer::new();
        c.insert_paste("line1\nline2\nline3");
        // The text is intact — no flattening, no auto-submit.
        assert_eq!(c.text, "line1\nline2\nline3");
        assert!(c.multiline);
    }

    #[test]
    fn large_code_block_paste_intact() {
        let code = r#"
fn main() {
    let x = 42;
    println!("value: {}", x);
    for i in 0..10 {
        if i % 2 == 0 {
            println!("even: {i}");
        }
    }
}
"#;
        let mut c = Composer::new();
        c.insert_paste(code);
        assert_eq!(c.text, code, "code block must survive paste intact");
        assert!(c.multiline);
        // Undo the entire paste as one operation.
        assert!(c.undo());
        assert!(c.text.is_empty());
    }
}
