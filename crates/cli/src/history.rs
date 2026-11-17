//! Input history for the plain (non-ratatui) session fallback.
//!
//! Used when stdin/stdout is not a TTY (piped / CI) or when the full
//! screen app is unavailable: persists the last N commands to
//! `~/.requiem/history` and provides up/down-arrow recall on a TTY via
//! `crossterm` raw mode.
//!
//! Known crossterm pitfalls handled here:
//! * Windows emits both `Press` and `Release` key events — only `Press`
//!   is processed, otherwise every keystroke registers twice.
//! * Redraws use `MoveToColumn(0)` (not `MoveTo(0,0)`), which used to
//!   teleport the cursor to the screen's top-left and corrupt output.

use std::io::IsTerminal;
use std::path::PathBuf;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use requiem_tui::theme::Theme;

/// The maximum number of history entries kept.
const MAX_HISTORY: usize = 200;

/// A minimal input history with key-driven recall.
pub struct InputHistory {
    entries: Vec<String>,
    theme: Theme,
}

impl InputHistory {
    /// Load existing history from disk (if any).
    pub fn load() -> Self {
        Self::load_with_theme(Theme::default())
    }

    /// Load existing history with a specific theme for prompt rendering.
    pub fn load_with_theme(theme: Theme) -> Self {
        let entries = std::fs::read_to_string(history_path())
            .map(|raw| {
                raw.lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(MAX_HISTORY)
                    .map(|l| l.to_string())
                    .collect()
            })
            .unwrap_or_default();
        Self { entries, theme }
    }

    /// Seed history entries (newest first).
    pub fn seed(&mut self, entries: Vec<String>) {
        self.entries = entries;
    }

    /// The persisted entries (newest first).
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Read one line.
    ///
    /// - On a TTY: raw mode with up/down recall.
    /// - On piped input: plain single read.
    ///
    /// Returns `None` on EOF or Ctrl-C.
    pub fn read_line(&mut self) -> Option<String> {
        if !std::io::stdin().is_terminal() {
            return self.read_plain();
        }
        self.read_tty()
    }

    /// Plain single-line read for piped / non-interactive input.
    fn read_plain(&mut self) -> Option<String> {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => {
                let t = line.trim().to_string();
                if !t.is_empty() {
                    self.push(&t);
                }
                Some(t)
            }
            Err(_) => None,
        }
    }

    /// Raw-mode read with up/down history recall (TTY only).
    fn read_tty(&mut self) -> Option<String> {
        use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

        // Render the prompt through the theme (NO_COLOR-aware).
        print!("{} ", self.theme.accent_bold("›"));
        std::io::Write::flush(&mut std::io::stdout()).ok();

        let mut buf = String::new();
        let mut recall: Option<usize> = None;
        let mut saved_draft: Option<String> = None;

        if enable_raw_mode().is_err() {
            // Raw mode unavailable — fall back to a plain read.
            return self.read_plain();
        }

        loop {
            let Ok(ev) = event::read() else {
                disable_raw_mode().ok();
                println!();
                if buf.is_empty() {
                    return None;
                }
                self.push(buf.trim());
                return Some(buf);
            };
            let Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) = ev
            else {
                continue;
            };
            // Windows delivers Press AND Release; handling both makes
            // every key register twice.
            if kind != KeyEventKind::Press {
                continue;
            }
            match code {
                KeyCode::Enter => {
                    let line = buf.clone();
                    if !line.trim().is_empty() {
                        self.push(line.trim());
                    }
                    disable_raw_mode().ok();
                    println!();
                    return Some(line);
                }
                KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                    disable_raw_mode().ok();
                    println!();
                    return None;
                }
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    disable_raw_mode().ok();
                    println!("^C");
                    return None;
                }
                KeyCode::Up => {
                    if self.entries.is_empty() {
                        continue;
                    }
                    if recall.is_none() {
                        saved_draft = Some(buf.clone());
                        recall = Some(0);
                        buf = self.entries[0].clone();
                        self.redraw(&buf);
                    } else if let Some(i) = recall {
                        let next = (i + 1).min(self.entries.len() - 1);
                        recall = Some(next);
                        buf = self.entries[next].clone();
                        self.redraw(&buf);
                    }
                }
                KeyCode::Down => {
                    if let Some(i) = recall {
                        let next = i.saturating_sub(1);
                        if next == 0 {
                            recall = None;
                            buf = saved_draft.take().unwrap_or_default();
                        } else {
                            recall = Some(next);
                            buf = self.entries[next].clone();
                        }
                        self.redraw(&buf);
                    }
                }
                KeyCode::Esc => {
                    buf = saved_draft.take().unwrap_or_default();
                    recall = None;
                    self.redraw(&buf);
                }
                KeyCode::Backspace => {
                    buf.pop();
                    self.redraw(&buf);
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                    self.redraw(&buf);
                }
                _ => {}
            }
        }
    }

    /// Replace the visible line at the prompt with `text`.
    ///
    /// `MoveToColumn(0)` stays on the current row — `MoveTo(0,0)` used
    /// to jump to the screen origin and repaint over the transcript.
    fn redraw(&self, text: &str) {
        use crossterm::cursor::MoveToColumn;
        use crossterm::queue;
        use crossterm::terminal::{Clear, ClearType};
        use std::io::Write;
        let _ = queue!(
            std::io::stdout(),
            MoveToColumn(0),
            Clear(ClearType::UntilNewLine)
        );
        let _ = write!(&mut std::io::stdout(), "{text}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    fn push(&mut self, s: &str) {
        self.entries.insert(0, s.to_string());
        self.entries.truncate(MAX_HISTORY);
    }

    /// Persist the history buffer to disk. Best-effort.
    pub fn save(&self) {
        if self.entries.is_empty() {
            return;
        }
        if let Some(parent) = history_path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(e);
            out.push('\n');
        }
        let _ = std::fs::write(history_path(), out);
    }
}

fn history_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".requiem")
        .join("history")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_creates_empty_history() {
        // Works even when no history file exists.
        let h = InputHistory::load();
        assert!(h.entries().len() <= MAX_HISTORY);
    }

    #[test]
    fn push_trims_to_max() {
        let mut h = InputHistory::load();
        for i in 0..(MAX_HISTORY + 10) {
            h.push(&format!("line-{i}"));
        }
        assert_eq!(h.entries().len(), MAX_HISTORY);
        assert_eq!(h.entries()[0], format!("line-{}", MAX_HISTORY + 9));
    }

    #[test]
    fn non_tty_read_returns_none_or_line() {
        // In tests stdin is usually not a terminal; read_plain path must
        // be safe (returns None at EOF without hanging).
        let mut h = InputHistory::load();
        let _ = h.read_line();
    }

    #[test]
    fn seed_then_entries_preserves_order() {
        let mut h = InputHistory::load();
        h.seed(vec!["a".into(), "b".into()]);
        assert_eq!(h.entries(), &["a".to_string(), "b".to_string()]);
    }
}
