//! Terminal color capability detection and low-level ANSI rendering.
//!
//! REQUIEM renders its own SGR escape sequences so that a single code path
//! covers truecolour (24-bit), 256-colour, 16-colour and colourless
//! terminals. `NO_COLOR` is always honoured, and `REQUIEM_COLOR` can force
//! a specific mode (`never`, `16`, `256`, `truecolor`, `auto`).

use std::sync::RwLock;

/// An RGB triple (0-255 per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parse `#RRGGBB` or `RRGGBB`.
    pub fn from_hex(hex: &str) -> Option<Rgb> {
        let hex = hex.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Rgb(r, g, b))
    }

    /// Relative luminance (0.0 = dark, 1.0 = bright).
    pub fn luma(self) -> f32 {
        let (r, g, b) = (self.0 as f32, self.1 as f32, self.2 as f32);
        (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
    }

    /// Squared perceptual-ish distance in RGB space.
    fn dist2(self, other: Rgb) -> i32 {
        let dr = self.0 as i32 - other.0 as i32;
        let dg = self.1 as i32 - other.1 as i32;
        let db = self.2 as i32 - other.2 as i32;
        // Weight green higher, as it dominates perceived brightness.
        2 * dr * dr + 4 * dg * dg + 3 * db * db
    }

    /// Mix two colors (t in 0.0..=1.0 toward `other`).
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let f = |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * t) as u8 };
        Rgb(f(self.0, other.0), f(self.1, other.1), f(self.2, other.2))
    }
}

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

/// The colour capabilities of the output terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Emit no SGR sequences at all.
    NoColor,
    /// 16 ANSI colours.
    Ansi16,
    /// 256-colour xterm palette.
    Ansi256,
    /// 24-bit truecolour.
    TrueColor,
}

/// The 16 base ANSI colours, in SGR foreground order (30-37, 90-97).
const ANSI16: [Rgb; 16] = [
    Rgb(0x00, 0x00, 0x00),   // 0 black
    Rgb(0xBB, 0x00, 0x00),   // 1 red
    Rgb(0x00, 0xBB, 0x00),   // 2 green
    Rgb(0xBB, 0xBB, 0x00),   // 3 yellow
    Rgb(0x00, 0x00, 0xBB),   // 4 blue
    Rgb(0xBB, 0x00, 0xBB),   // 5 magenta
    Rgb(0x00, 0xBB, 0xBB),   // 6 cyan
    Rgb(0xBB, 0xBB, 0xBB),   // 7 white
    Rgb(0x55, 0x55, 0x55),   // 8 bright black
    Rgb(0xFF, 0x55, 0x55),   // 9 bright red
    Rgb(0x55, 0xFF, 0x55),   // 10 bright green
    Rgb(0xFF, 0xFF, 0x55),   // 11 bright yellow
    Rgb(0x55, 0x55, 0xFF),   // 12 bright blue
    Rgb(0xFF, 0x55, 0xFF),   // 13 bright magenta
    Rgb(0x55, 0xFF, 0xFF),   // 14 bright cyan
    Rgb(0xFF, 0xFF, 0xFF),   // 15 bright white
];

/// Nearest base-16 ANSI colour index for an RGB value.
pub fn to_ansi16(rgb: Rgb) -> u8 {
    let mut best = 0u8;
    let mut best_d = i32::MAX;
    for (i, c) in ANSI16.iter().enumerate() {
        let d = rgb.dist2(*c);
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

/// Nearest xterm-256 palette index for an RGB value.
pub fn to_ansi256(rgb: Rgb) -> u8 {
    // Base 16 colours (0-15).
    let mut best = to_ansi16(rgb);
    let mut best_d = {
        let c = ANSI16[best as usize];
        rgb.dist2(c)
    };

    // 6×6×6 colour cube (16-231).
    let levels = [0u8, 95, 135, 175, 215, 255];
    for r in 0..6 {
        for g in 0..6 {
            for b in 0..6 {
                let c = Rgb(levels[r], levels[g], levels[b]);
                let d = rgb.dist2(c);
                if d < best_d {
                    best_d = d;
                    best = 16 + (36 * r + 6 * g + b) as u8;
                }
            }
        }
    }

    // Grayscale ramp (232-255).
    for i in 0..24u32 {
        let v = (8 + 10 * i) as u8;
        let d = rgb.dist2(Rgb(v, v, v));
        if d < best_d {
            best_d = d;
            best = (232 + i) as u8;
        }
    }
    best
}

/// Text attributes combinable with a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attr {
    Bold,
    Dim,
    Italic,
    Underline,
    Reverse,
    Strikethrough,
}

/// Foreground SGR parameter string for `rgb` under `mode`.
/// Returns `None` when colour is disabled.
#[cfg(test)]
pub(crate) fn fg_params(mode: ColorMode, rgb: Rgb) -> Option<String> {
    match mode {
        ColorMode::NoColor => None,
        ColorMode::TrueColor => Some(format!("38;2;{};{};{}", rgb.0, rgb.1, rgb.2)),
        ColorMode::Ansi256 => Some(format!("38;5;{}", to_ansi256(rgb))),
        ColorMode::Ansi16 => {
            let idx = to_ansi16(rgb);
            if idx < 8 {
                Some(format!("3{}", idx))
            } else {
                Some(format!("9{}", idx - 8))
            }
        }
    }
}

/// Foreground colour specification for a wrap.
#[derive(Debug, Clone, Copy)]
pub enum FgSpec {
    /// No foreground colour.
    None,
    /// Truecolour RGB (downgraded automatically on limited terminals).
    Rgb(Rgb),
    /// A hand-picked base-16 index (used by the token map so designed
    /// semantics survive on 16-colour terminals).
    Indexed16(u8),
}

impl FgSpec {
    fn rgb(self) -> Option<Rgb> {
        match self {
            FgSpec::None => None,
            FgSpec::Rgb(rgb) => Some(rgb),
            FgSpec::Indexed16(_) => None,
        }
    }

    fn hand_index(self) -> Option<u8> {
        match self {
            FgSpec::Indexed16(i) => Some(i),
            _ => None,
        }
    }
}

/// Wrap `text` in an SGR sequence with the given foreground and attributes.
/// In `NoColor` mode attributes that imply emphasis (bold/underline/reverse)
/// are still emitted so structure survives; Dim/Italic/Strikethrough are
/// dropped as they can look like garbage on plain terminals.
pub fn ansi_wrap(mode: ColorMode, fg: FgSpec, attrs: &[Attr], text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    match mode {
        ColorMode::NoColor => {
            let keep: Vec<Attr> = attrs
                .iter()
                .copied()
                .filter(|a| matches!(a, Attr::Bold | Attr::Underline | Attr::Reverse))
                .collect();
            if keep.is_empty() {
                text.to_string()
            } else {
                let params = keep
                    .iter()
                    .map(|a| attr_param(*a))
                    .collect::<Vec<&str>>()
                    .join(";");
                format!("\x1b[{params}m{text}\x1b[0m")
            }
        }
        mode => {
            let mut params: Vec<String> = attrs.iter().map(|a| attr_param(*a).to_string()).collect();
            if let Some(idx) = fg.hand_index() {
                if idx < 8 {
                    params.push(format!("3{idx}"));
                } else {
                    params.push(format!("9{}", idx - 8));
                }
            } else if let Some(rgb) = fg.rgb() {
                match mode {
                    ColorMode::TrueColor => {
                        params.push(format!("38;2;{};{};{}", rgb.0, rgb.1, rgb.2));
                    }
                    ColorMode::Ansi256 => params.push(format!("38;5;{}", to_ansi256(rgb))),
                    ColorMode::Ansi16 => {
                        let idx = to_ansi16(rgb);
                        if idx < 8 {
                            params.push(format!("3{idx}"));
                        } else {
                            params.push(format!("9{}", idx - 8));
                        }
                    }
                    ColorMode::NoColor => unreachable!(),
                }
            }
            if params.is_empty() {
                return text.to_string();
            }
            format!("\x1b[{}m{text}\x1b[0m", params.join(";"))
        }
    }
}

fn attr_param(a: Attr) -> &'static str {
    match a {
        Attr::Bold => "1",
        Attr::Dim => "2",
        Attr::Italic => "3",
        Attr::Underline => "4",
        Attr::Reverse => "7",
        Attr::Strikethrough => "9",
    }
}

// ── Global colour-mode resolution ───────────────────────────────────

static MODE_OVERRIDE: RwLock<Option<ColorMode>> = RwLock::new(None);

/// Force a specific colour mode (used by tests and the `REQUIEM_COLOR`
/// environment override at startup).
pub fn force_color_mode(mode: ColorMode) {
    *MODE_OVERRIDE.write().unwrap() = Some(mode);
}

/// Clear any forced mode override.
#[cfg(test)]
pub(crate) fn clear_color_mode_override() {
    *MODE_OVERRIDE.write().unwrap() = None;
}

/// The effective colour mode: the forced override if any, otherwise the
/// auto-detected capability of the current terminal.
pub fn color_mode() -> ColorMode {
    if let Some(m) = *MODE_OVERRIDE.read().unwrap() {
        return m;
    }
    auto_detect(is_stdout_terminal())
}

fn is_stdout_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Detect the best colour mode for this environment.
///
/// Precedence:
/// 1. `REQUIEM_COLOR` (never|no|0|16|256|truecolor|always|auto)
/// 2. `NO_COLOR` (non-empty) → NoColor
/// 3. non-TTY stdout → NoColor (piping must stay clean)
/// 4. `COLORTERM` truecolor/24bit → TrueColor
/// 5. `WT_SESSION` (Windows Terminal) → TrueColor
/// 6. `TERM` containing `256color` → Ansi256
/// 7. Windows ≥ 10 console or any modern terminal → TrueColor
/// 8. `TERM=dumb` → NoColor, otherwise Ansi16 as last resort
pub fn auto_detect(is_tty: bool) -> ColorMode {
    if let Ok(v) = std::env::var("REQUIEM_COLOR") {
        match v.trim().to_ascii_lowercase().as_str() {
            "never" | "no" | "none" | "0" => return ColorMode::NoColor,
            "16" => return ColorMode::Ansi16,
            "256" => return ColorMode::Ansi256,
            "truecolor" | "24bit" | "always" | "yes" | "1" => return ColorMode::TrueColor,
            _ => {} // "auto" or unknown → continue detection
        }
    }
    if std::env::var("NO_COLOR")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return ColorMode::NoColor;
    }
    if !is_tty {
        return ColorMode::NoColor;
    }
    if let Ok(ct) = std::env::var("COLORTERM") {
        let ct = ct.to_ascii_lowercase();
        if ct.contains("truecolor") || ct.contains("24bit") {
            return ColorMode::TrueColor;
        }
    }
    if std::env::var("WT_SESSION").is_ok() {
        return ColorMode::TrueColor;
    }
    let term = std::env::var("TERM").unwrap_or_default().to_ascii_lowercase();
    if term.contains("256color") {
        return ColorMode::Ansi256;
    }
    if term == "dumb" {
        return ColorMode::NoColor;
    }
    if cfg!(windows) {
        // Windows 10+ conhost and all major terminals handle 24-bit VT.
        ColorMode::TrueColor
    } else {
        ColorMode::Ansi16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parse_roundtrip() {
        let c = Rgb::from_hex("#D83A5E").unwrap();
        assert_eq!(c, Rgb(0xD8, 0x3A, 0x5E));
        assert_eq!(c.to_string(), "#D83A5E");
        assert!(Rgb::from_hex("D83A5E").is_some());
        assert!(Rgb::from_hex("#D83A5").is_none());
        assert!(Rgb::from_hex("#GGGGGG").is_none());
    }

    #[test]
    fn truecolor_wrap_emits_sgr() {
        let s = ansi_wrap(
            ColorMode::TrueColor,
            FgSpec::Rgb(Rgb(0xD8, 0x3A, 0x5E)),
            &[Attr::Bold],
            "REQUIEM",
        );
        assert_eq!(s, "\x1b[1;38;2;216;58;94mREQUIEM\x1b[0m");
    }

    #[test]
    fn nocolor_strips_everything() {
        let s = ansi_wrap(ColorMode::NoColor, FgSpec::Rgb(Rgb(255, 0, 0)), &[Attr::Bold], "x");
        assert_eq!(s, "\x1b[1mx\x1b[0m");
        let s = ansi_wrap(ColorMode::NoColor, FgSpec::Rgb(Rgb(255, 0, 0)), &[], "x");
        assert_eq!(s, "x");
        let s = ansi_wrap(ColorMode::NoColor, FgSpec::None, &[Attr::Dim], "x");
        assert_eq!(s, "x");
    }

    #[test]
    fn indexed16_hand_mapping() {
        let s = ansi_wrap(ColorMode::Ansi16, FgSpec::Indexed16(1), &[], "x");
        assert_eq!(s, "\x1b[31mx\x1b[0m");
        let s = ansi_wrap(ColorMode::Ansi16, FgSpec::Indexed16(9), &[], "x");
        assert_eq!(s, "\x1b[91mx\x1b[0m");
        let s = ansi_wrap(ColorMode::TrueColor, FgSpec::Indexed16(2), &[], "x");
        assert_eq!(s, "\x1b[32mx\x1b[0m");
    }

    #[test]
    fn ansi256_maps_strongly() {
        // Pure red must land on a red-ish cube cell.
        let idx = to_ansi256(Rgb(0xFF, 0x00, 0x00));
        assert_eq!(idx, 196);
        // Dark grey lands on the grayscale ramp.
        let idx = to_ansi256(Rgb(0x10, 0x10, 0x10));
        assert!(idx >= 232);
    }

    #[test]
    fn empty_text_stays_empty() {
        assert_eq!(ansi_wrap(ColorMode::TrueColor, FgSpec::Rgb(Rgb(1, 2, 3)), &[], ""), "");
    }

    #[test]
    fn force_mode_overrides_detection() {
        force_color_mode(ColorMode::Ansi256);
        assert_eq!(color_mode(), ColorMode::Ansi256);
        clear_color_mode_override();
    }

    #[test]
    fn detection_honours_no_color() {
        // SAFETY(test): single-threaded test binary for env mutation.
        std::env::set_var("NO_COLOR", "1");
        assert_eq!(auto_detect(true), ColorMode::NoColor);
        std::env::remove_var("NO_COLOR");
    }

    #[test]
    fn detection_prefers_requiem_color() {
        std::env::set_var("REQUIEM_COLOR", "256");
        assert_eq!(auto_detect(false), ColorMode::Ansi256);
        std::env::remove_var("REQUIEM_COLOR");
    }
}
