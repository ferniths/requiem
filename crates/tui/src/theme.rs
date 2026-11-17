//! REQUIEM design tokens and theme system.
//!
//! Every visual element in the application draws from this single source
//! of truth. No component hardcodes a color value.
//!
//! The palette is a dark, atmospheric, crimson-oriented identity: deep
//! near-black surfaces, crimson accents with bright red/rose highlights,
//! muted grey typography, and carefully differentiated status colors.
//! Colors are defined as truecolour RGB and automatically degrade to the
//! 256-colour palette, the 16 base colours, or plain text (see `color`).

use ratatui::style::{Color, Modifier, Style};

use crate::color::{ansi_wrap, color_mode, force_color_mode, Attr, ColorMode, Rgb};

/// A semantic color token in the REQUIEM palette.
///
/// Components never reference concrete RGB values — they ask for a token
/// and the active theme resolves it (with automatic capability downgrade).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// Deep near-black background.
    Background,
    /// Secondary panel surface.
    Surface,
    /// Raised surface (popups, selected rows, headers).
    SurfaceRaised,
    /// The REQUIEM accent — primary crimson.
    Accent,
    /// Brighter crimson for highlights, live indicators, focus.
    AccentBright,
    /// Deep crimson for subdued brand elements.
    AccentDeep,
    /// Subtle violet — secondary accent for special states.
    Violet,
    /// Primary text — maximum contrast.
    Text,
    /// Secondary text — labels, hints, descriptions.
    TextSecondary,
    /// Muted text — timestamps, counts, low-priority info.
    TextMuted,
    /// Panel borders and separators.
    Border,
    /// Focused/active borders.
    BorderActive,
    /// Informational states (running, active, in-progress).
    Info,
    /// Success states (completed, verified, ok).
    Success,
    /// Warning states (queued, blocked, caution).
    Warning,
    /// Error states (failed, denied, unreachable).
    Error,
}

/// The REQUIEM theme: a complete semantic palette.
///
/// The default theme is the official REQUIEM identity — crimson on
/// near-black, refined and technical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    name: &'static str,
    colors: [(Token, Rgb); 16],
}

impl Theme {
    /// The official REQUIEM theme.
    pub const fn default() -> Self {
        Self::with_colors(
            "default",
            [
                (Token::Background, Rgb(0x0B, 0x09, 0x0D)),
                (Token::Surface, Rgb(0x15, 0x10, 0x18)),
                (Token::SurfaceRaised, Rgb(0x20, 0x15, 0x1D)),
                (Token::Accent, Rgb(0xD8, 0x3A, 0x5E)),
                (Token::AccentBright, Rgb(0xFF, 0x52, 0x78)),
                (Token::AccentDeep, Rgb(0x8C, 0x1D, 0x3A)),
                (Token::Violet, Rgb(0x9D, 0x7B, 0xD8)),
                (Token::Text, Rgb(0xF2, 0xE9, 0xEC)),
                (Token::TextSecondary, Rgb(0xB9, 0xA6, 0xAE)),
                (Token::TextMuted, Rgb(0x76, 0x63, 0x6C)),
                (Token::Border, Rgb(0x3A, 0x2C, 0x34)),
                (Token::BorderActive, Rgb(0x6E, 0x41, 0x52)),
                (Token::Info, Rgb(0x65, 0xC9, 0xD6)),
                (Token::Success, Rgb(0x75, 0xD6, 0x9A)),
                (Token::Warning, Rgb(0xE7, 0xB8, 0x6D)),
                (Token::Error, Rgb(0xFF, 0x64, 0x7C)),
            ],
        )
    }

    /// A high-contrast variant with brighter typography and borders.
    pub const fn high_contrast() -> Self {
        Self::with_colors(
            "high_contrast",
            [
                (Token::Background, Rgb(0x0B, 0x09, 0x0D)),
                (Token::Surface, Rgb(0x15, 0x10, 0x18)),
                (Token::SurfaceRaised, Rgb(0x24, 0x18, 0x20)),
                (Token::Accent, Rgb(0xE8, 0x4C, 0x6E)),
                (Token::AccentBright, Rgb(0xFF, 0x6E, 0x8C)),
                (Token::AccentDeep, Rgb(0xA6, 0x2A, 0x48)),
                (Token::Violet, Rgb(0xB0, 0x92, 0xE8)),
                (Token::Text, Rgb(0xFF, 0xFF, 0xFF)),
                (Token::TextSecondary, Rgb(0xDC, 0xCE, 0xD4)),
                (Token::TextMuted, Rgb(0xA6, 0x93, 0x9C)),
                (Token::Border, Rgb(0x5C, 0x47, 0x51)),
                (Token::BorderActive, Rgb(0x9A, 0x5C, 0x70)),
                (Token::Info, Rgb(0x7C, 0xDC, 0xE8)),
                (Token::Success, Rgb(0x8A, 0xE4, 0xAC)),
                (Token::Warning, Rgb(0xF2, 0xC8, 0x82)),
                (Token::Error, Rgb(0xFF, 0x7C, 0x90)),
            ],
        )
    }

    /// A muted variant with softer, less stimulating colors.
    pub const fn muted_variant() -> Self {
        Self::with_colors(
            "muted",
            [
                (Token::Background, Rgb(0x0D, 0x0B, 0x0E)),
                (Token::Surface, Rgb(0x16, 0x12, 0x18)),
                (Token::SurfaceRaised, Rgb(0x1E, 0x16, 0x1C)),
                (Token::Accent, Rgb(0xA8, 0x40, 0x5C)),
                (Token::AccentBright, Rgb(0xC8, 0x54, 0x70)),
                (Token::AccentDeep, Rgb(0x74, 0x24, 0x3C)),
                (Token::Violet, Rgb(0x86, 0x70, 0xB2)),
                (Token::Text, Rgb(0xD9, 0xCD, 0xD2)),
                (Token::TextSecondary, Rgb(0x9E, 0x8C, 0x94)),
                (Token::TextMuted, Rgb(0x6E, 0x5D, 0x66)),
                (Token::Border, Rgb(0x30, 0x26, 0x2C)),
                (Token::BorderActive, Rgb(0x58, 0x3E, 0x4A)),
                (Token::Info, Rgb(0x7F, 0xA9, 0xB0)),
                (Token::Success, Rgb(0x7F, 0xB8, 0x94)),
                (Token::Warning, Rgb(0xC0, 0xA4, 0x76)),
                (Token::Error, Rgb(0xD0, 0x6B, 0x7A)),
            ],
        )
    }

    const fn with_colors(name: &'static str, colors: [(Token, Rgb); 16]) -> Self {
        Self { name, colors }
    }

    /// The theme's variant name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Resolve a token to its RGB value.
    pub fn rgb(&self, token: Token) -> Rgb {
        self.colors
            .iter()
            .find(|(t, _)| *t == token)
            .map(|(_, c)| *c)
            .unwrap_or(Rgb(0xFF, 0x00, 0xFF))
    }

    /// Resolve a token to a concrete 16-colour ANSI index (for terminals
    /// without higher colour depth). Hand-tuned for the REQUIEM palette so
    /// semantic distinctions survive the downgrade (e.g. accent stays the
    /// calmer base red while errors take the bright red).
    pub fn ansi16_index(&self, token: Token) -> u8 {
        match token {
            Token::Background | Token::Surface | Token::SurfaceRaised => 0,
            Token::Accent | Token::AccentDeep => 1,  // red
            Token::AccentBright | Token::Error => 9, // bright red
            Token::Violet => 13,                     // bright magenta
            Token::Text => 15,                       // bright white
            Token::TextSecondary => 7,               // white
            Token::TextMuted | Token::Border => 8,   // bright black (grey)
            Token::BorderActive => 5,                // magenta
            Token::Info => 6,                        // cyan
            Token::Success => 2,                     // green
            Token::Warning => 3,                     // yellow
        }
    }

    // ── ANSI string rendering ───────────────────────────────────────

    /// Paint `text` in a token color under an explicit colour mode.
    pub fn paint_with(&self, mode: ColorMode, token: Token, text: impl AsRef<str>) -> String {
        ansi_wrap(mode, self.fg_spec(mode, token), &[], text.as_ref())
    }

    /// Paint `text` with attributes under an explicit colour mode.
    pub fn paint_attrs_with(
        &self,
        mode: ColorMode,
        token: Token,
        attrs: &[Attr],
        text: impl AsRef<str>,
    ) -> String {
        ansi_wrap(mode, self.fg_spec(mode, token), attrs, text.as_ref())
    }

    /// The foreground spec for a token under a mode: the hand-tuned 16-colour
    /// index on Ansi16 terminals, RGB otherwise.
    fn fg_spec(&self, mode: ColorMode, token: Token) -> crate::color::FgSpec {
        match mode {
            ColorMode::NoColor => crate::color::FgSpec::None,
            ColorMode::Ansi16 => crate::color::FgSpec::Indexed16(self.ansi16_index(token)),
            _ => crate::color::FgSpec::Rgb(self.rgb(token)),
        }
    }

    /// Paint `text` in a token color using the auto-detected mode.
    pub fn paint(&self, token: Token, text: impl AsRef<str>) -> String {
        self.paint_with(color_mode(), token, text)
    }

    /// Convenience: accent-colored text.
    pub fn accent(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::Accent, text)
    }

    /// Convenience: accent-colored bold text.
    pub fn accent_bold(&self, text: impl AsRef<str>) -> String {
        self.paint_attrs_with(color_mode(), Token::Accent, &[Attr::Bold], text)
    }

    /// Convenience: success-colored text.
    pub fn success(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::Success, text)
    }

    /// Convenience: success-colored bold text.
    pub fn success_bold(&self, text: impl AsRef<str>) -> String {
        self.paint_attrs_with(color_mode(), Token::Success, &[Attr::Bold], text)
    }

    /// Convenience: warning-colored text.
    pub fn warning(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::Warning, text)
    }

    /// Convenience: error-colored text.
    pub fn error(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::Error, text)
    }

    /// Convenience: error-colored bold text.
    pub fn error_bold(&self, text: impl AsRef<str>) -> String {
        self.paint_attrs_with(color_mode(), Token::Error, &[Attr::Bold], text)
    }

    /// Convenience: info-colored text.
    pub fn info(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::Info, text)
    }

    /// Convenience: violet (secondary accent) text.
    pub fn violet(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::Violet, text)
    }

    /// Convenience: border-colored text.
    pub fn border(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::Border, text)
    }

    /// Convenience: muted text.
    pub fn muted(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::TextMuted, text)
    }

    /// Convenience: secondary text.
    pub fn secondary(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::TextSecondary, text)
    }

    /// Convenience: primary text.
    pub fn primary(&self, text: impl AsRef<str>) -> String {
        self.paint(Token::Text, text)
    }

    /// Convenience: bold text in the primary color.
    pub fn primary_bold(&self, text: impl AsRef<str>) -> String {
        self.paint_attrs_with(color_mode(), Token::Text, &[Attr::Bold], text)
    }

    /// The color token for a given status string.
    pub fn status_token(&self, status: &str) -> Token {
        match status.to_lowercase().as_str() {
            "running" | "active" | "in-progress" => Token::Info,
            "done" | "complete" | "completed" | "success" | "ok" => Token::Success,
            "failed" | "error" | "failure" => Token::Error,
            "waiting" | "blocked" | "queued" | "pending" | "claimed" => Token::Warning,
            "available" => Token::Violet,
            "cancelled" | "idle" => Token::TextMuted,
            _ => Token::TextSecondary,
        }
    }

    /// Render a status indicator with the appropriate color.
    pub fn status(&self, status: &str) -> String {
        let token = self.status_token(status);
        self.paint(token, status)
    }

    /// A filled/hollow dot glyph for a status string, colored appropriately.
    pub fn status_dot(&self, status: &str) -> String {
        let token = self.status_token(status);
        let glyph = match status.to_lowercase().as_str() {
            "running" | "active" => "▶",
            "done" | "complete" | "completed" | "success" | "ok" => "●",
            "failed" | "error" | "failure" => "✗",
            "waiting" | "blocked" | "queued" | "pending" => "◇",
            "claimed" => "◆",
            "cancelled" => "⊘",
            _ => "○",
        };
        self.paint(token, glyph)
    }

    // ── ratatui bridge ──────────────────────────────────────────────

    /// Convert a token to a ratatui color, honouring the effective mode.
    pub fn ratatui_color(&self, token: Token) -> Color {
        self.ratatui_color_with(color_mode(), token)
    }

    /// Convert a token to a ratatui color under an explicit mode.
    pub fn ratatui_color_with(&self, mode: ColorMode, token: Token) -> Color {
        let rgb = self.rgb(token);
        match mode {
            ColorMode::NoColor => Color::Reset,
            ColorMode::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
            ColorMode::Ansi256 => Color::Indexed(crate::color::to_ansi256(rgb)),
            ColorMode::Ansi16 => {
                let idx = crate::color::to_ansi16(rgb);
                Color::Indexed(idx)
            }
        }
    }

    /// A ratatui style for a token.
    pub fn style(&self, token: Token) -> Style {
        Style::new().fg(self.ratatui_color(token))
    }

    /// A ratatui style for a token with a bold modifier.
    pub fn style_bold(&self, token: Token) -> Style {
        Style::new()
            .fg(self.ratatui_color(token))
            .add_modifier(Modifier::BOLD)
    }

    /// A ratatui style for a token with a dim modifier.
    pub fn style_dim(&self, token: Token) -> Style {
        Style::new()
            .fg(self.ratatui_color(token))
            .add_modifier(Modifier::DIM)
    }

    /// The background style for panels (kept subtle: no hard fills unless
    /// the terminal supports truecolour).
    pub fn panel_style(&self) -> Style {
        Style::new().fg(self.ratatui_color(Token::Border))
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

/// Force the global colour mode (startup hook for the CLI).
pub fn init_color_mode() {
    force_color_mode(color_mode());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorMode;

    #[test]
    fn default_theme_tokens_resolve() {
        let t = Theme::default();
        assert_eq!(t.rgb(Token::Accent), Rgb(0xD8, 0x3A, 0x5E));
        assert_eq!(t.rgb(Token::Background), Rgb(0x0B, 0x09, 0x0D));
        assert_eq!(t.name(), "default");
    }

    #[test]
    fn status_token_maps_correctly() {
        let t = Theme::default();
        assert_eq!(t.status_token("running"), Token::Info);
        assert_eq!(t.status_token("done"), Token::Success);
        assert_eq!(t.status_token("failed"), Token::Error);
        assert_eq!(t.status_token("waiting"), Token::Warning);
        assert_eq!(t.status_token("blocked"), Token::Warning);
        assert_eq!(t.status_token("available"), Token::Violet);
        assert_eq!(t.status_token("idle"), Token::TextMuted);
    }

    #[test]
    fn theme_from_name_falls_back_to_default() {
        assert_eq!(theme_from_name("nonexistent").name(), "default");
        assert_eq!(theme_from_name("high_contrast").name(), "high_contrast");
        assert_eq!(theme_from_name("muted").name(), "muted");
    }

    #[test]
    fn ansi16_downgrade_is_stable() {
        let t = Theme::default();
        // Crimson → red(1); success green → green(2); text → white-ish (7 or 15).
        assert_eq!(t.ansi16_index(Token::Accent), 1);
        assert_eq!(t.ansi16_index(Token::Success), 2);
        assert_eq!(t.ansi16_index(Token::Warning), 3);
        assert_eq!(t.ansi16_index(Token::Info), 6);
        assert!(matches!(t.ansi16_index(Token::Text), 7 | 15));
        assert_eq!(t.ansi16_index(Token::TextMuted), 8);
    }

    #[test]
    fn truecolor_rendering() {
        let t = Theme::default();
        let s = t.paint_with(ColorMode::TrueColor, Token::Accent, "requiem");
        assert_eq!(s, "\x1b[38;2;216;58;94mrequiem\x1b[0m");
        let b = t.paint_attrs_with(ColorMode::TrueColor, Token::Accent, &[Attr::Bold], "REQ");
        assert_eq!(b, "\x1b[1;38;2;216;58;94mREQ\x1b[0m");
    }

    #[test]
    fn ansi16_rendering() {
        let t = Theme::default();
        let s = t.paint_with(ColorMode::Ansi16, Token::Accent, "requiem");
        assert_eq!(s, "\x1b[31mrequiem\x1b[0m");
    }

    #[test]
    fn ansi256_rendering() {
        let t = Theme::default();
        let s = t.paint_with(ColorMode::Ansi256, Token::Accent, "x");
        assert!(s.starts_with("\x1b[38;5;"), "got {s}");
        assert!(s.ends_with("mx\x1b[0m"));
    }

    #[test]
    fn nocolor_rendering_is_plain() {
        let t = Theme::default();
        // Colours are stripped, but bold survives so structure remains.
        assert_eq!(
            t.paint_with(ColorMode::NoColor, Token::Accent, "plain"),
            "plain"
        );
        assert_eq!(
            t.paint_attrs_with(ColorMode::NoColor, Token::Accent, &[Attr::Bold], "plain"),
            "\x1b[1mplain\x1b[0m"
        );
    }

    #[test]
    fn status_dot_glyphs_differ_per_status() {
        let t = Theme::default();
        assert_ne!(t.status_dot("running"), t.status_dot("done"));
        assert_ne!(t.status_dot("failed"), t.status_dot("waiting"));
    }

    #[test]
    fn ratatui_bridge_truecolor() {
        let t = Theme::default();
        assert_eq!(
            t.ratatui_color_with(ColorMode::TrueColor, Token::Accent),
            Color::Rgb(0xD8, 0x3A, 0x5E)
        );
        assert_eq!(
            t.ratatui_color_with(ColorMode::NoColor, Token::Accent),
            Color::Reset
        );
        let idx = t.ratatui_color_with(ColorMode::Ansi256, Token::Accent);
        assert!(matches!(idx, Color::Indexed(_)));
    }

    #[test]
    fn theme_names_listed() {
        assert_eq!(THEME_NAMES.len(), 3);
        for n in THEME_NAMES {
            assert_eq!(theme_from_name(n).name(), *n);
        }
    }

    #[test]
    fn variants_differ_visibly() {
        let d = Theme::default();
        let h = Theme::high_contrast();
        let m = Theme::muted_variant();
        assert_ne!(d.rgb(Token::Text), h.rgb(Token::Text));
        assert_ne!(d.rgb(Token::Accent), m.rgb(Token::Accent));
    }
}
