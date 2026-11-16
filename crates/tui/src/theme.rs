//! REQUIEM design tokens and theme system.
//!
//! Every visual element in the application draws from this single source
//! of truth. No component hardcodes a color value.
//!
//! Uses the `colored` crate (16 ANSI colors) so the theme degrades
//! gracefully on terminals without truecolor support.

use colored::{Colorize, ColoredString};

/// A color token in the REQUIEM palette.
///
/// Each variant maps to one of the 16 ANSI colors. This keeps the theme
/// portable across Windows Terminal, PowerShell, CMD, and Linux terminals
/// without requiring 24-bit color support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// The REQUIEM accent — deep red, used for branding and emphasis.
    Accent,
    /// A dimmed accent for secondary emphasis.
    AccentDim,
    /// Success states (completed, verified, ok).
    Success,
    /// Warning states (queued, blocked, caution).
    Warning,
    /// Error states (failed, denied, unreachable).
    Error,
    /// Informational states (running, active, in-progress).
    Info,
    /// Panel borders and separators.
    Border,
    /// Primary text — maximum contrast.
    Text,
    /// Secondary text — labels, hints, descriptions.
    TextSecondary,
    /// Muted text — timestamps, counts, low-priority info.
    TextMuted,
}

impl Token {
    /// Render the token as the corresponding ANSI color.
    fn color(&self) -> colored::Color {
        match self {
            Token::Accent | Token::AccentDim => colored::Color::Red,
            Token::Success => colored::Color::Green,
            Token::Warning => colored::Color::Yellow,
            Token::Error => colored::Color::BrightRed,
            Token::Info => colored::Color::Cyan,
            Token::Border => colored::Color::BrightBlack,
            Token::Text => colored::Color::BrightWhite,
            Token::TextSecondary => colored::Color::BrightBlack,
            Token::TextMuted => colored::Color::BrightBlack,
        }
    }
}

/// The REQUIEM theme: a set of color tokens that every component
/// consumes. The default theme uses a crimson accent on a dark
/// background — atmospheric, technical, refined.
#[derive(Clone, Copy)]
pub struct Theme {
    pub accent: Token,
    pub success: Token,
    pub warning: Token,
    pub error: Token,
    pub info: Token,
    pub border: Token,
    pub text_primary: Token,
    pub text_secondary: Token,
    pub text_muted: Token,
}

impl Theme {
    /// The official REQUIEM theme.
    pub const fn default() -> Self {
        Self {
            accent: Token::Accent,
            success: Token::Success,
            warning: Token::Warning,
            error: Token::Error,
            info: Token::Info,
            border: Token::Border,
            text_primary: Token::Text,
            text_secondary: Token::TextSecondary,
            text_muted: Token::TextMuted,
        }
    }

    /// A high-contrast variant with brighter secondary/muted text.
    pub const fn high_contrast() -> Self {
        Self {
            accent: Token::Accent,
            success: Token::Success,
            warning: Token::Warning,
            error: Token::Error,
            info: Token::Info,
            border: Token::Text,
            text_primary: Token::Text,
            text_secondary: Token::Text,
            text_muted: Token::TextSecondary,
        }
    }

    /// A muted variant with softer, less stimulating colors.
    pub const fn muted_variant() -> Self {
        Self {
            accent: Token::AccentDim,
            success: Token::TextMuted,
            warning: Token::TextMuted,
            error: Token::Accent,
            info: Token::TextMuted,
            border: Token::Border,
            text_primary: Token::Text,
            text_secondary: Token::TextMuted,
            text_muted: Token::TextMuted,
        }
    }

    // ── Convenience methods ─────────────────────────────────────

    /// Render text in the accent color.
    pub fn accent(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.accent, text)
    }

    /// Render text in the accent color, bold.
    pub fn accent_bold(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.accent, text).bold()
    }

    /// Render text in the success color.
    pub fn success(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.success, text)
    }

    /// Render text in the success color, bold.
    pub fn success_bold(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.success, text).bold()
    }

    /// Render text in the warning color.
    pub fn warning(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.warning, text)
    }

    /// Render text in the error color.
    pub fn error(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.error, text)
    }

    /// Render text in the error color, bold.
    pub fn error_bold(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.error, text).bold()
    }

    /// Render text in the info color.
    pub fn info(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.info, text)
    }

    /// Render text in the border color.
    pub fn border(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.border, text)
    }

    /// Render text in muted color.
    pub fn muted(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.text_muted, text)
    }

    /// Render text in secondary color.
    pub fn secondary(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.text_secondary, text)
    }

    /// Render text in primary color.
    pub fn primary(&self, text: impl AsRef<str>) -> ColoredString {
        self.apply(self.text_primary, text)
    }

    /// The color token for a given status string.
    pub fn status_token(&self, status: &str) -> Token {
        match status.to_lowercase().as_str() {
            "running" | "active" => self.info,
            "done" | "complete" | "success" => self.success,
            "failed" | "error" => self.error,
            "waiting" | "blocked" | "queued" => self.warning,
            "idle" => self.text_muted,
            _ => self.text_secondary,
        }
    }

    /// Render a status indicator with the appropriate color.
    pub fn status(&self, status: &str) -> ColoredString {
        let token = self.status_token(status);
        self.apply(token, status)
    }

    // ── Internal ────────────────────────────────────────────────

    fn apply(&self, token: Token, text: impl AsRef<str>) -> ColoredString {
        let s = text.as_ref();
        match token.color() {
            colored::Color::Black => s.black(),
            colored::Color::Red => s.red(),
            colored::Color::Green => s.green(),
            colored::Color::Yellow => s.yellow(),
            colored::Color::Blue => s.blue(),
            colored::Color::Magenta => s.magenta(),
            colored::Color::Cyan => s.cyan(),
            colored::Color::White => s.white(),
            colored::Color::BrightBlack => s.bright_black(),
            colored::Color::BrightRed => s.bright_red(),
            colored::Color::BrightGreen => s.bright_green(),
            colored::Color::BrightYellow => s.bright_yellow(),
            colored::Color::BrightBlue => s.bright_blue(),
            colored::Color::BrightMagenta => s.bright_magenta(),
            colored::Color::BrightCyan => s.bright_cyan(),
            colored::Color::BrightWhite => s.bright_white(),
            _ => s.into(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default()
    }
}

/// Parse a theme name from config into a `Theme`.
pub fn theme_from_name(name: &str) -> Theme {
    match name {
        "high_contrast" => Theme::high_contrast(),
        "muted" => Theme::muted_variant(),
        _ => Theme::default(),
    }
}

/// The available theme variant names.
pub const THEME_NAMES: &[&str] = &["default", "high_contrast", "muted"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_has_accent() {
        let t = Theme::default();
        assert_eq!(t.accent, Token::Accent);
    }

    #[test]
    fn status_token_maps_correctly() {
        let t = Theme::default();
        assert_eq!(t.status_token("running"), t.info);
        assert_eq!(t.status_token("done"), t.success);
        assert_eq!(t.status_token("failed"), t.error);
        assert_eq!(t.status_token("waiting"), t.warning);
        assert_eq!(t.status_token("idle"), t.text_muted);
    }

    #[test]
    fn theme_from_name_falls_back_to_default() {
        let t = theme_from_name("nonexistent");
        assert_eq!(t.accent, Theme::default().accent);
    }

    #[test]
    fn accent_bold_produces_ansi() {
        let t = Theme::default();
        let s = t.accent_bold("REQUIEM").to_string();
        assert!(!s.is_empty());
    }

    #[test]
    fn all_themes_construct() {
        let _ = Theme::default();
        let _ = Theme::high_contrast();
        let _ = Theme::muted_variant();
    }

    #[test]
    fn theme_names_listed() {
        assert_eq!(THEME_NAMES.len(), 3);
        assert!(THEME_NAMES.contains(&"default"));
        assert!(THEME_NAMES.contains(&"high_contrast"));
        assert!(THEME_NAMES.contains(&"muted"));
    }
}
