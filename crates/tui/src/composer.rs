//! Multiline prompt composer for the REQUIEM TUI.
//!
//! Replaces the old single-line input with a proper multiline text editor
//! that supports cursor movement, undo/redo, and correct paste handling.
//! The composer is used by `App` for the input area of the session screen.
//!
//! Wrapping is display-width based (see [`unicode_width`]): CJK glyphs and
//! emoji occupy two cells, combining marks zero, and tabs advance to the
//! next 4-column stop. `visual_rows` is the single source of truth for
//! both layout height and rendering, so they can never disagree.

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

/// Tab stop width for display purposes.
pub const TAB_STOP: usize = 4;

/// A single-line text buffer with cursor tracking.
#[derive(Debug, Clone, Default)]
pub struct Composer {
    /// The full text (may contain newlines).
    pub text: String,
    /// Char index of the cursor (0..=text.chars().count()).
    pub cursor: usize,
    /// Whether the text contains newlines (i.e. is multiline).
    pub multiline: bool,
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
        self.clamp_cursor();
        self.push_undo();
        let byte = self.cursor_byte();
        self.text.insert(byte, c);
        self.cursor += 1;
        self.multiline = self.text.contains('\n');
    }

    /// Keep the cursor inside the buffer (defensive against stale state).
    fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.len());
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
        self.clamp_cursor();
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
        self.clamp_cursor();
        self.push_undo();
        let byte = self.cursor_byte();
        self.text.insert_str(byte, &normalized);
        // `cursor` is a char index, not a byte offset: advance by the
        // number of characters inserted, never by `byte + len`.
        self.cursor += normalized.chars().count();
        self.multiline = self.text.contains('\n');
    }

    /// Clear all text and reset cursor.
    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.push_undo();
        }
        self.text.clear();
        self.cursor = 0;
        self.multiline = false;
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
        let line_count = self.text.lines().count().max(1);
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

// ── Display-width soft wrapping ──────────────────────────────────────

/// One visual row of the composer after display-width soft wrapping.
#[derive(Debug, Clone, PartialEq)]
pub struct VisualRow {
    /// The rendered text (tabs already expanded to spaces).
    pub text: String,
    /// Char index in the whole composer text where this row starts.
    pub start_char: usize,
    /// Char index in the whole composer text just past this row.
    pub end_char: usize,
    /// Display width of `text` in terminal cells.
    pub width: usize,
}

/// A display cluster: a base character plus any zero-width followers
/// (combining marks, variation selectors, ZWJ sequences).
#[derive(Debug, Clone, Copy)]
struct Cluster {
    start: usize,
    end: usize,
    width: usize,
    is_space: bool,
}

/// Whether a char is zero-width (combining mark, joiner, selector).
fn is_zero_width(c: char) -> bool {
    !c.is_control() && matches!(UnicodeWidthChar::width(c), Some(0) | None)
}

/// Whether `c` continues the cluster started by `prev`.
fn is_joiner(prev: char, c: char) -> bool {
    c == '\u{200D}' || prev == '\u{200D}' || c == '\u{FE0F}' || is_zero_width(c)
}

/// Width of one char when placed at display column `col`.
pub fn char_width_at(c: char, col: usize) -> usize {
    match c {
        '\t' => TAB_STOP - (col % TAB_STOP),
        _ => UnicodeWidthChar::width(c).unwrap_or(0),
    }
}

/// Split a logical line into display clusters.
fn clusters(chars: &[char]) -> Vec<Cluster> {
    let mut out: Vec<Cluster> = Vec::with_capacity(chars.len());
    let mut i = 0;
    let mut col = 0;
    while i < chars.len() {
        let start = i;
        let c = chars[i];
        let mut width = char_width_at(c, col);
        i += 1;
        // Attach combining marks and ZWJ sequences to the base glyph.
        while i < chars.len() && is_joiner(chars[i - 1], chars[i]) {
            let w = char_width_at(chars[i], col + width);
            width = width.max(w);
            i += 1;
        }
        col += width;
        out.push(Cluster {
            start,
            end: i,
            width,
            is_space: c == ' ' || c == '\t',
        });
    }
    out
}

/// Render a char range with tabs expanded to their cluster width.
fn render_range(chars: &[char], start: usize, end: usize, cl: &[Cluster]) -> String {
    let mut out = String::new();
    for c in &cl[start..end] {
        for &ch in &chars[c.start..c.end] {
            if ch == '\t' {
                out.push_str(&" ".repeat(c.width));
            } else {
                out.push(ch);
            }
        }
    }
    out
}

/// Split `text` into logical lines, preserving a trailing empty line so
/// that a final newline produces a visible empty row.
fn logical_lines(text: &str) -> Vec<(usize, Vec<char>)> {
    let mut out: Vec<(usize, Vec<char>)> = Vec::new();
    let mut current: Vec<char> = Vec::new();
    let mut offset = 0usize;
    for c in text.chars() {
        if c == '\n' {
            let line_len = current.len();
            out.push((offset, std::mem::take(&mut current)));
            offset += line_len + 1;
        } else {
            current.push(c);
        }
    }
    out.push((offset, current));
    out
}

/// Soft-wrap one logical line into display rows, preferring break points
/// at spaces. Wide glyphs never split; the char ranges of consecutive
/// rows cover the whole line with no gaps.
fn wrap_line(chars: &[char], offset: usize, avail: usize) -> Vec<VisualRow> {
    let cl = clusters(chars);
    if cl.is_empty() {
        return vec![VisualRow {
            text: String::new(),
            start_char: offset,
            end_char: offset,
            width: 0,
        }];
    }
    let avail = avail.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    while start < cl.len() {
        let mut end = start;
        let mut w = 0usize;
        let mut last_space: Option<usize> = None;
        while end < cl.len() {
            let cw = cl[end].width;
            // Always admit the first cluster of a row (progress guarantee),
            // even if a single wide glyph is wider than the available width.
            if end > start && w + cw > avail {
                break;
            }
            w += cw;
            end += 1;
            if cl[end - 1].is_space {
                last_space = Some(end);
            }
            if w >= avail {
                break;
            }
        }
        let mut chosen = end;
        if chosen < cl.len() {
            if let Some(sp) = last_space {
                if sp > start {
                    chosen = sp;
                }
            }
            if chosen == start {
                chosen = end.max(start + 1);
            }
        }
        let text = render_range(chars, start, chosen, &cl);
        let width: usize = cl[start..chosen].iter().map(|c| c.width).sum();
        rows.push(VisualRow {
            text,
            start_char: offset + cl[start].start,
            end_char: offset + cl[chosen - 1].end,
            width,
        });
        start = chosen;
    }
    rows
}

/// Wrap the whole composer text at `width` display columns.
///
/// This is the single canonical wrapping function: layout height and the
/// rendered rows both come from it, so they can never disagree.
pub fn visual_rows(text: &str, width: usize) -> Vec<VisualRow> {
    let width = width.max(1);
    let mut rows: Vec<VisualRow> = Vec::new();
    for (offset, chars) in logical_lines(text) {
        rows.extend(wrap_line(&chars, offset, width));
    }
    if rows.is_empty() {
        rows.push(VisualRow {
            text: String::new(),
            start_char: 0,
            end_char: 0,
            width: 0,
        });
    }
    rows
}

/// Number of visual rows the composer text occupies at `width` columns.
pub fn total_visual_rows(text: &str, width: usize) -> usize {
    visual_rows(text, width).len()
}

/// Compute the `(row, display_column)` of the cursor.
///
/// `cursor_char` is a char index into `text`. The returned column is in
/// terminal cells, so wide glyphs (CJK, emoji) and expanded tabs map
/// correctly.
pub fn cursor_visual_position(text: &str, cursor_char: usize, width: usize) -> (usize, usize) {
    let rows = visual_rows(text, width);
    let total_chars = text.chars().count();
    let cursor = cursor_char.min(total_chars);

    let mut row_idx = 0;
    for (i, row) in rows.iter().enumerate() {
        if cursor < row.end_char {
            row_idx = i;
            break;
        }
        if cursor == row.end_char {
            row_idx = if i + 1 < rows.len() && rows[i + 1].start_char == cursor {
                i + 1
            } else {
                i
            };
            break;
        }
        row_idx = i;
    }

    let row = &rows[row_idx];
    let line: Vec<char> = text
        .chars()
        .skip(row.start_char)
        .take(row.end_char - row.start_char)
        .collect();
    let target = cursor.saturating_sub(row.start_char);
    let mut col = 0usize;
    for c in clusters(&line) {
        if c.start >= target {
            break;
        }
        if target < c.end {
            break;
        }
        col += c.width;
    }
    (row_idx, col)
}

/// Split a rendered row at display column `col` into
/// `(before, cursor_cluster, after)`. The cursor cluster may be empty
/// when the caret is at the end of the row.
pub fn split_at_display_col(text: &str, col: usize) -> (String, String, String) {
    let chars: Vec<char> = text.chars().collect();
    let cl = clusters(&chars);
    let mut w = 0usize;
    for c in &cl {
        if w + c.width > col {
            let before: String = chars[..c.start].iter().collect();
            let at: String = chars[c.start..c.end].iter().collect();
            let after: String = chars[c.end..].iter().collect();
            return (before, at, after);
        }
        w += c.width;
    }
    (text.to_string(), String::new(), String::new())
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
    fn paste_after_unicode_places_cursor_by_char_index() {
        // The old bug did `byte + normalized.chars().count()`, mixing byte
        // and char indices. Pasting after multibyte text exposed it.
        let mut c = Composer::new();
        c.insert_paste("héllo 🌍 世界");
        let typed_before = c.len();
        c.insert_paste("X");
        assert_eq!(c.text, "héllo 🌍 世界X");
        assert_eq!(c.cursor, typed_before + 1);
        // Cursor must be usable immediately: deleting removes the pasted X.
        c.delete_before();
        assert_eq!(c.text, "héllo 🌍 世界");
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
    fn wrap_short_line_unchanged() {
        let rows = visual_rows("hello", 80);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "hello");
        assert_eq!(rows[0].width, 5);
    }

    #[test]
    fn wrap_long_word_breaks_mid_word() {
        let long_word = "a".repeat(100);
        let rows = visual_rows(&long_word, 40);
        assert!(rows.len() >= 3);
        let total: usize = rows.iter().map(|w| w.text.chars().count()).sum();
        assert_eq!(total, 100);
        for row in &rows {
            assert!(row.width <= 40, "row wider than limit: {}", row.width);
        }
    }

    #[test]
    fn wrap_breaks_at_spaces_and_reconstructs() {
        let text = "word1 word2 word3 word4";
        let rows = visual_rows(text, 10);
        for row in &rows {
            assert!(row.width <= 10, "row too wide: {:?}", row.text);
        }
        let reconstructed: String = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(reconstructed, text, "rows must reconstruct the original");
    }

    #[test]
    fn wrap_preserves_trailing_newline_as_empty_row() {
        let rows = visual_rows("abc\n", 40);
        assert_eq!(rows.len(), 2, "final newline must yield an empty row");
        assert_eq!(rows[0].text, "abc");
        assert_eq!(rows[1].text, "");
        assert_eq!(total_visual_rows("abc\n", 40), 2);
    }

    #[test]
    fn wrap_wide_glyphs_use_two_columns() {
        // Four CJK glyphs = 8 display cells.
        let rows = visual_rows("你好世界", 4);
        assert_eq!(rows.len(), 2, "8 cells at width 4 → 2 rows");
        assert_eq!(rows[0].text, "你好");
        assert_eq!(rows[0].width, 4);
        assert_eq!(rows[1].text, "世界");
    }

    #[test]
    fn wrap_never_splits_wide_glyph() {
        // An odd width must not split a 2-cell glyph.
        let rows = visual_rows("你好世", 3);
        assert_eq!(rows[0].text, "你");
        assert_eq!(rows[1].text, "好");
        assert_eq!(rows[2].text, "世");
    }

    #[test]
    fn wrap_tabs_expand_to_stops() {
        // Tab at column 0 expands to 4 spaces.
        let rows = visual_rows("\tx", 40);
        assert_eq!(rows[0].text, "    x");
        assert_eq!(rows[0].width, 5);
    }

    #[test]
    fn wrap_emoji_width() {
        let rows = visual_rows("🌍🌍", 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "🌍");
    }

    #[test]
    fn wrap_combining_marks_stay_with_base() {
        // "e" + combining acute = one cluster, width 1.
        let text = "e\u{0301}e\u{0301}e\u{0301}";
        let rows = visual_rows(text, 2);
        assert_eq!(rows.len(), 2, "3 clusters at width 2 → 2 rows");
        assert_eq!(rows[0].text, "e\u{0301}e\u{0301}");
        assert_eq!(rows[0].width, 2);
    }

    #[test]
    fn cursor_maps_after_wide_glyphs() {
        // Cursor after two CJK glyphs on row 0, then wraps.
        let text = "你好世界";
        let (row, col) = cursor_visual_position(text, 2, 4);
        // At a soft-wrap boundary the caret moves to the start of the next
        // visual row (it cannot sit past the last cell of a full row).
        assert_eq!((row, col), (1, 0));
        let (row, col) = cursor_visual_position(text, 3, 4);
        assert_eq!((row, col), (1, 2));
    }

    #[test]
    fn cursor_at_end_of_trailing_newline() {
        let text = "ab\n";
        let (row, col) = cursor_visual_position(text, 3, 40);
        assert_eq!((row, col), (1, 0));
    }

    #[test]
    fn split_at_display_col_handles_wide_glyphs() {
        let (before, at, after) = split_at_display_col("你好", 2);
        assert_eq!(before, "你");
        assert_eq!(at, "好");
        assert_eq!(after, "");
        // Past the end → nothing to place the cursor on.
        let (before, at, after) = split_at_display_col("ab", 5);
        assert_eq!(before, "ab");
        assert!(at.is_empty() && after.is_empty());
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
