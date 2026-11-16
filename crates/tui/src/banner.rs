//! The REQUIEM ASCII banner, embedded at compile time.

use crate::theme::Theme;

/// The exact banner from `REQUIEM-ASCII-BANNER.txt` (15 lines, 1498 bytes).
pub const BANNER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../REQUIEM-ASCII-BANNER.txt"
));

/// Render the banner to a `String` with optional color.
pub fn render_banner() -> String {
    BANNER.to_string()
}

/// A plain-text fallback for environments where ANSI colors are not
/// supported (e.g. piping to a file).
pub fn render_plain() -> String {
    BANNER.to_string()
}

/// Render the banner with the REQUIEM theme accent color.
pub fn render_themed(theme: &Theme) -> String {
    theme.accent(BANNER).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_is_15_lines() {
        let lines: Vec<&str> = BANNER.lines().collect();
        assert_eq!(
            lines.len(),
            15,
            "banner must be exactly 15 lines, got {}",
            lines.len()
        );
    }

    #[test]
    fn banner_starts_with_expected_prefix() {
        assert!(BANNER.starts_with("     ..      ..."));
    }

    #[test]
    fn render_banner_returns_non_empty() {
        let rendered = render_banner();
        assert!(!rendered.is_empty());
        assert_eq!(rendered, BANNER);
    }

    #[test]
    fn banner_contains_requiem_glyphs() {
        // The banner is ASCII art made of 8/8-style characters.
        assert!(BANNER.contains("8888"));
    }
}
