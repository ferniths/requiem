//! The REQUIEM ASCII banner, embedded at compile time.

use crate::theme::{Theme, Token};

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
///
/// Each line is painted individually so the SGR open/close sequences
/// land on the same line as the text they color — wrapping the whole
/// multi-line block in one sequence causes terminals to apply the
/// foreground only to the first and last lines.
pub fn render_themed(theme: &Theme) -> String {
    let lines: Vec<&str> = BANNER.lines().collect();
    theme.paint_lines(Token::Accent, &lines).join("\n")
}

/// The embedded banner text. Available after installation, independent of
/// the source repository.
pub fn banner() -> &'static str {
    BANNER
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{clear_color_mode_override, force_color_mode, ColorMode};

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

    #[test]
    fn themed_banner_colors_every_line() {
        // Each line must carry its own SGR open/close so the terminal
        // applies the accent color to every line, not just first/last.
        force_color_mode(ColorMode::TrueColor);
        let theme = Theme::default();
        let colored = render_themed(&theme);
        clear_color_mode_override();

        let lines: Vec<&str> = colored.lines().collect();
        assert_eq!(lines.len(), 15, "must still be 15 lines");
        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.starts_with("\x1b["),
                "line {} missing SGR open: {:?}",
                i,
                &line[..12.min(line.len())]
            );
            assert!(
                line.ends_with("\x1b[0m"),
                "line {} missing SGR reset: {:?}",
                i,
                &line[line.len().saturating_sub(6)..]
            );
        }
    }
}
