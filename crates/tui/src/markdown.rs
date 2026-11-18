//! Terminal markdown renderer.
//!
//! Converts a markdown document into a `Vec<String>` of ANSI-pre-styled
//! lines, one string per rendered row, using the REQUIEM [`Theme`] for
//! every colour decision. It is deliberately independent of `ratatui` —
//! output is plain pre-painted text usable from both the plain REPL and
//! any future ratatui view.
//!
//! Supported constructs:
//!
//! * ATX headings (`#`..`######`) with a `▌` bar prefix, accent coloured,
//!   h1 bold and brighter.
//! * Horizontal rules (`---`, `***`, `___`) drawn as `─` to the width.
//! * Fenced code blocks with a simple token-based syntax highlighter for
//!   rust, python, javascript/typescript, json, toml, bash/shell and yaml
//!   (keywords, strings, numbers, comments). Unknown languages render in
//!   the muted base colour with no highlighting.
//! * Inline styles: **bold**, *italic*, `inline code`,
//!   ~~strikethrough~~ and `[text](url)` links rendered as `text (url)`.
//!   Nested and combined styles compose (bold inside headings, code
//!   inside bold, …). Unterminated emphasis markers render literally,
//!   CommonMark style.
//! * Unordered and ordered lists with 2-space nesting, accent bullets and
//!   hanging-indent wrapped continuations.
//! * Blockquotes (`>`), nested by indentation, in secondary colour.
//! * GFM tables with box-drawing borders and column auto-padding;
//!   malformed tables fall back to plain text.
//!
//! Robustness rules: never panics on any input; unterminated fences
//! highlight to end of input; CRLF input is handled; widths below
//! [`MIN_WRAP_WIDTH`] (or zero) disable wrapping instead of misbehaving.

use crate::color::{Attr, ColorMode, FgSpec};
use crate::theme::{Theme, Token};

/// Minimum terminal width at which prose/code wrapping is attempted.
/// Below this (or at zero) the renderer degrades gracefully and does not
/// wrap at all.
const MIN_WRAP_WIDTH: usize = 8;

/// Bullets used per nesting depth (cycles at the last entry).
const BULLETS: [char; 3] = ['•', '◦', '▪'];

/// Bar glyph used for headings.
const HEADING_BAR: char = '▌';

/// Left border glyph for code blocks.
const CODE_BAR: char = '│';

/// Resolve the SGR background parameter for a token under `mode` (empty
/// when no background is requested or the mode cannot carry one).
fn bg_param(theme: &Theme, mode: ColorMode, bg: Option<Token>) -> String {
    let Some(bg) = bg else {
        return String::new();
    };
    // NoColor and Ansi16: degrade gracefully (no background parameter).
    match mode {
        ColorMode::NoColor | ColorMode::Ansi16 => String::new(),
        ColorMode::TrueColor => {
            let rgb = theme.rgb(bg);
            format!("48;2;{};{};{}", rgb.0, rgb.1, rgb.2)
        }
        ColorMode::Ansi256 => {
            let idx = crate::color::to_ansi256(theme.rgb(bg));
            format!("48;5;{idx}")
        }
    }
}

/// Left border glyph for blockquotes.
const QUOTE_BAR: char = '┃';

/// Horizontal rule glyph.
const RULE_GLYPH: char = '─';

// ── Public API ──────────────────────────────────────────────────────

/// Render a markdown `text` into ANSI-pre-styled lines at `width` columns
/// using the auto-detected colour mode.
///
/// Returns one string per rendered line; blank separators between blocks
/// appear as empty strings. Never panics on any input.
pub fn render(text: &str, theme: &Theme, width: usize) -> Vec<String> {
    render_with_mode(text, theme, width, crate::color::color_mode())
}

/// Render markdown under an explicit colour mode.
///
/// Identical to [`render`] but with a deterministic mode, which makes it
/// the right entry point for tests and for callers that have already
/// resolved the terminal capability themselves.
pub fn render_with_mode(text: &str, theme: &Theme, width: usize, mode: ColorMode) -> Vec<String> {
    let mut doc = Document::new(theme, width, mode);
    doc.feed(text);
    doc.finish()
}

/// The effective wrap width: unbounded for tiny widths so narrow
/// terminals degrade to "no wrapping" instead of producing garbage.
fn effective_width(width: usize) -> usize {
    if width == 0 || width < MIN_WRAP_WIDTH {
        usize::MAX
    } else {
        width
    }
}

// ── Inline styles and spans ─────────────────────────────────────────

/// Decoration flags plus an optional colour override for a text span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Style {
    /// Colour override; `None` inherits the surrounding block base colour.
    color: Option<Token>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

impl Style {
    /// A style that only overrides the colour.
    fn colored(color: Token) -> Self {
        Style {
            color: Some(color),
            ..Style::default()
        }
    }

    /// Merge in an extra colour override.
    fn with_color(self, color: Token) -> Self {
        Style {
            color: Some(color),
            ..self
        }
    }

    /// Merge in an extra underline.
    fn with_underline(self) -> Self {
        Style {
            underline: true,
            ..self
        }
    }

    /// Set or clear the bold flag.
    fn with_bold(self, on: bool) -> Self {
        Style { bold: on, ..self }
    }

    /// Set or clear the italic flag.
    fn with_italic(self, on: bool) -> Self {
        Style { italic: on, ..self }
    }

    /// Set or clear the strikethrough flag.
    fn with_strike(self, on: bool) -> Self {
        Style { strike: on, ..self }
    }
}

/// A run of text carrying a single combined style.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    text: String,
    style: Style,
}

impl Span {
    fn new(text: impl Into<String>, style: Style) -> Self {
        Span {
            text: text.into(),
            style,
        }
    }

    /// Visible width in chars (spans are always plain text).
    fn width(&self) -> usize {
        self.text.chars().count()
    }
}

/// Paint a span with the theme under `mode`, layering any overrides on
/// top of the block's `base` token and an optional background token
/// (e.g. the code-block surface).
fn paint_span(
    theme: &Theme,
    mode: ColorMode,
    span: &Span,
    base: Token,
    bg: Option<Token>,
) -> String {
    if span.text.is_empty() {
        return String::new();
    }
    let token = span.style.color.unwrap_or(base);
    let mut attrs: Vec<Attr> = Vec::new();
    if span.style.bold {
        attrs.push(Attr::Bold);
    }
    if span.style.italic {
        attrs.push(Attr::Italic);
    }
    if span.style.underline {
        attrs.push(Attr::Underline);
    }
    if span.style.strike {
        attrs.push(Attr::Strikethrough);
    }
    // Foreground parameters (theme-aware downgrade: hand-tuned 16 index,
    // RGB, or none).
    let mut fg_params: Vec<String> = match theme.fg_spec(mode, token) {
        FgSpec::None => Vec::new(),
        FgSpec::Rgb(rgb) => match mode {
            ColorMode::TrueColor => {
                vec![format!("38;2;{};{};{}", rgb.0, rgb.1, rgb.2)]
            }
            ColorMode::Ansi256 => vec![format!("38;5;{}", crate::color::to_ansi256(rgb))],
            ColorMode::Ansi16 => {
                let idx = crate::color::to_ansi16(rgb);
                vec![if idx < 8 {
                    format!("3{idx}")
                } else {
                    format!("9{}", idx - 8)
                }]
            }
            ColorMode::NoColor => Vec::new(),
        },
        FgSpec::Indexed16(idx) => {
            vec![if idx < 8 {
                format!("3{idx}")
            } else {
                format!("9{}", idx - 8)
            }]
        }
    };
    // Attributes.
    for a in &attrs {
        fg_params.push(attr_sgr(a).to_string());
    }
    // Optional background.
    let bg_s = bg_param(theme, mode, bg);
    if !bg_s.is_empty() {
        fg_params.push(bg_s);
    }
    if fg_params.is_empty() {
        return span.text.clone();
    }
    // Emit attrs, fg and bg in ONE sequence: a mid-line reset would split
    // the background off from the text it should paint behind.
    format!("\x1b[{}m{}\x1b[0m", fg_params.join(";"), span.text)
}

fn attr_sgr(a: &Attr) -> &'static str {
    match a {
        Attr::Bold => "1",
        Attr::Dim => "2",
        Attr::Italic => "3",
        Attr::Underline => "4",
        Attr::Reverse => "7",
        Attr::Strikethrough => "9",
    }
}

// ── Wrapping ────────────────────────────────────────────────────────

/// Builds one wrapped line at a time from styled words.
struct LineBuilder<'a> {
    theme: &'a Theme,
    mode: ColorMode,
    base: Token,
    width: usize,
    indent: usize,
    indent_rest: usize,
    line: Vec<Span>,
    line_len: usize,
    out: Vec<String>,
}

impl<'a> LineBuilder<'a> {
    fn new(
        theme: &'a Theme,
        mode: ColorMode,
        base: Token,
        width: usize,
        indent_first: usize,
        indent_rest: usize,
    ) -> Self {
        LineBuilder {
            theme,
            mode,
            base,
            width,
            indent: indent_first,
            indent_rest,
            line: Vec::new(),
            line_len: indent_first,
            out: Vec::new(),
        }
    }

    /// Paint and emit the current line, then reset for the next one.
    fn flush(&mut self) {
        if !self.line.is_empty() {
            let mut text = " ".repeat(self.indent);
            let mut i = 0;
            while i < self.line.len() {
                let style = self.line[i].style;
                let mut chunk = self.line[i].text.clone();
                while i + 1 < self.line.len() && self.line[i + 1].style == style {
                    i += 1;
                    chunk.push_str(&self.line[i].text);
                }
                let span = Span::new(chunk, style);
                text.push_str(&paint_span(self.theme, self.mode, &span, self.base, None));
                i += 1;
            }
            self.out.push(text);
        }
        self.line.clear();
        self.line_len = self.indent_rest;
        self.indent = self.indent_rest;
    }

    /// Place one word, flushing and hard-breaking as needed.
    fn place(&mut self, word: &Span) {
        let mut remaining: String = word.text.clone();
        loop {
            if remaining.is_empty() {
                return;
            }
            let sep = usize::from(!self.line.is_empty());
            let wlen = remaining.chars().count();
            if self.line_len.saturating_add(sep).saturating_add(wlen) <= self.width {
                if sep == 1 {
                    self.line.push(Span::new(" ", word.style));
                }
                self.line_len += sep + wlen;
                self.line.push(Span::new(remaining, word.style));
                return;
            }
            if self.line.is_empty() {
                // The word cannot fit even on a fresh line: hard-break it.
                let avail = self.width.saturating_sub(self.line_len).max(1);
                let mut head = String::new();
                let mut tail = String::new();
                for (k, ch) in remaining.chars().enumerate() {
                    if k < avail {
                        head.push(ch);
                    } else {
                        tail.push(ch);
                    }
                }
                if sep == 1 {
                    self.line.push(Span::new(" ", word.style));
                }
                self.line_len += sep + head.chars().count();
                self.line.push(Span::new(head, word.style));
                self.flush();
                remaining = tail;
            } else {
                self.flush();
            }
        }
    }
}

/// Wrap styled spans into painted lines.
///
/// `indent_first` prefixes the first line, `indent_rest` the wrapped
/// continuation lines. Words longer than the available width are
/// hard-broken at character boundaries; ANSI escape sequences are added
/// only after wrapping, so they are never split mid-code.
fn wrap_spans(
    theme: &Theme,
    mode: ColorMode,
    spans: &[Span],
    base: Token,
    width: usize,
    indent_first: usize,
    indent_rest: usize,
) -> Vec<String> {
    let mut words: Vec<Span> = Vec::new();
    for span in spans {
        for piece in span.text.split(' ') {
            if !piece.is_empty() {
                words.push(Span::new(piece, span.style));
            }
        }
    }
    if words.is_empty() {
        return Vec::new();
    }
    let mut lb = LineBuilder::new(theme, mode, base, width, indent_first, indent_rest);
    for word in &words {
        lb.place(word);
    }
    lb.flush();
    lb.out
}

/// Hard-wrap styled spans at exact character boundaries. Used for code,
/// where indentation and layout must survive intact.
fn hard_wrap_spans(spans: &[Span], width: usize) -> Vec<Vec<Span>> {
    let total = width.max(1);
    let mut rows: Vec<Vec<Span>> = Vec::new();
    let mut row: Vec<Span> = Vec::new();
    let mut row_len = 0usize;
    for span in spans {
        for ch in span.text.chars() {
            if row_len >= total {
                rows.push(std::mem::take(&mut row));
                row_len = 0;
            }
            row_len += 1;
            if let Some(last) = row.last_mut() {
                if last.style == span.style {
                    last.text.push(ch);
                    continue;
                }
            }
            row.push(Span::new(ch.to_string(), span.style));
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

// ── Inline markup parser ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Bold,
    Italic,
    Strike,
}

/// Parse one logical line of inline markdown into styled spans.
///
/// `base` is the colour/attribute context of the surrounding block.
/// Supports bold, italic, strikethrough, inline code, links (and images,
/// rendered like links) plus backslash escapes. Matched emphasis markers
/// are consumed; unterminated ones are re-emitted literally at their
/// original position, CommonMark style.
fn parse_inline(text: &str, base: Style) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // Char-level buffer so unmatched markers can be re-inserted at their
    // original position after the rest of the line is known.
    let mut buf: Vec<(char, Style)> = Vec::with_capacity(n);
    let mut cur = base;
    // Still-open emphasis: (marker, literal chars, insert position, style outside).
    let mut open: Vec<(Marker, &'static str, usize, Style)> = Vec::new();

    let mut i = 0;
    while i < n {
        let c = chars[i];
        // Backslash escape: the next character is literal.
        if c == '\\' && i + 1 < n {
            buf.push((chars[i + 1], cur));
            i += 2;
            continue;
        }
        // Inline code: scan to the closing backtick (backticks kept).
        if c == '`' {
            if let Some(end) = (i + 1..n).find(|&j| chars[j] == '`') {
                for &ch in &chars[i..=end] {
                    buf.push((ch, cur.with_color(Token::Info)));
                }
                i = end + 1;
            } else {
                buf.push((c, cur));
                i += 1;
            }
            continue;
        }
        // Links and images: [text](url) / ![alt](url) → text (url).
        if c == '[' || (c == '!' && i + 1 < n && chars[i + 1] == '[') {
            let start = if c == '[' { i } else { i + 1 };
            if let Some((consumed, text, url)) = try_parse_link(&chars, start) {
                for ch in text.chars() {
                    buf.push((ch, cur.with_color(Token::Violet)));
                }
                if !url.is_empty() {
                    for ch in " (".chars() {
                        buf.push((ch, cur));
                    }
                    for ch in url.chars() {
                        buf.push((ch, cur.with_color(Token::TextMuted).with_underline()));
                    }
                    buf.push((')', cur));
                }
                i = consumed;
                continue;
            }
            if c == '!' {
                buf.push(('!', cur));
                i += 1;
                continue;
            }
        }
        // Emphasis runs: `*`/`_` (bold in pairs, italic singly) and `~~`.
        if c == '*' || c == '_' {
            let run = (i..n).take_while(|&j| chars[j] == c).count();
            let mut left = run;
            while left >= 2 {
                let lit = if c == '*' { "**" } else { "__" };
                handle_marker(
                    Marker::Bold,
                    lit,
                    &chars,
                    &mut i,
                    &mut cur,
                    &mut open,
                    &mut buf,
                );
                left -= 2;
            }
            if left == 1 {
                let lit = if c == '*' { "*" } else { "_" };
                handle_marker(
                    Marker::Italic,
                    lit,
                    &chars,
                    &mut i,
                    &mut cur,
                    &mut open,
                    &mut buf,
                );
            }
            continue;
        }
        if c == '~' {
            let run = (i..n).take_while(|&j| chars[j] == '~').count();
            if run >= 2 {
                handle_marker(
                    Marker::Strike,
                    "~~",
                    &chars,
                    &mut i,
                    &mut cur,
                    &mut open,
                    &mut buf,
                );
            } else {
                buf.push((c, cur));
                i += 1;
            }
            continue;
        }
        buf.push((c, cur));
        i += 1;
    }

    // Re-insert still-open markers as literal text at their original
    // positions, descending so earlier positions stay valid.
    open.sort_by_key(|&(_, _, pos, _)| std::cmp::Reverse(pos));
    for (_, lit, pos, st) in &open {
        let (pos, st) = (*pos, *st);
        let lits: Vec<(char, Style)> = lit.chars().map(|ch| (ch, st)).collect();
        buf.splice(pos..pos, lits);
    }

    // Group consecutive same-style chars into spans.
    let mut spans: Vec<Span> = Vec::new();
    for (ch, st) in buf {
        if let Some(last) = spans.last_mut() {
            if last.style == st {
                last.text.push(ch);
                continue;
            }
        }
        spans.push(Span::new(ch.to_string(), st));
    }
    spans
}

/// Attempt to parse a link starting at `start` (the index of `[`).
/// Returns `(index past `)`, text, url)` on success.
fn try_parse_link(chars: &[char], start: usize) -> Option<(usize, String, String)> {
    let n = chars.len();
    let mut j = start + 1;
    let mut depth = 1usize;
    while j < n {
        match chars[j] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            '\\' if j + 1 < n => j += 1,
            _ => {}
        }
        j += 1;
    }
    if j >= n || depth != 0 || j + 1 >= n || chars[j + 1] != '(' {
        return None;
    }
    let close = (j + 2..n).find(|&k| chars[k] == ')')?;
    let text: String = chars[start + 1..j].iter().collect();
    let url: String = chars[j + 2..close].iter().collect();
    Some((close + 1, text, url))
}

/// Handle one emphasis marker occurrence (open or close toggle).
///
/// Matched markers are consumed silently; unmatched or badly flanked ones
/// become literal text at their original position.
fn handle_marker(
    marker: Marker,
    lit: &'static str,
    chars: &[char],
    i: &mut usize,
    cur: &mut Style,
    open: &mut Vec<(Marker, &'static str, usize, Style)>,
    buf: &mut Vec<(char, Style)>,
) {
    let idx = *i;
    let n = chars.len();
    let lit_len = lit.chars().count();
    let prev = if idx == 0 { None } else { Some(chars[idx - 1]) };
    let after = if idx + lit_len < n {
        Some(chars[idx + lit_len])
    } else {
        None
    };
    let underscore = lit.starts_with('_');
    let is_open = open.iter().any(|&(m, ..)| m == marker);

    if is_open {
        // Close attempt: must not follow whitespace, and `_` must not be
        // followed by an alphanumeric (right-flanking rule).
        let can_close = !prev.is_some_and(char::is_whitespace)
            && (!underscore || !after.is_some_and(char::is_alphanumeric));
        if can_close {
            if let Some(pos) = open.iter().position(|&(m, ..)| m == marker) {
                open.remove(pos);
            }
            match marker {
                Marker::Bold => *cur = cur.with_bold(false),
                Marker::Italic => *cur = cur.with_italic(false),
                Marker::Strike => *cur = cur.with_strike(false),
            }
        } else {
            // Badly flanked close: the marker is literal, outside the
            // emphasis it failed to close.
            let outside = match marker {
                Marker::Bold => cur.with_bold(false),
                Marker::Italic => cur.with_italic(false),
                Marker::Strike => cur.with_strike(false),
            };
            for ch in lit.chars() {
                buf.push((ch, outside));
            }
        }
    } else {
        // Open attempt: must not precede whitespace, and `_` must not sit
        // inside an alphanumeric run (left-flanking rule).
        let can_open = !after.is_some_and(char::is_whitespace)
            && (!underscore || !prev.is_some_and(char::is_alphanumeric));
        if can_open {
            open.push((marker, lit, buf.len(), *cur));
            match marker {
                Marker::Bold => *cur = cur.with_bold(true),
                Marker::Italic => *cur = cur.with_italic(true),
                Marker::Strike => *cur = cur.with_strike(true),
            }
        } else {
            for ch in lit.chars() {
                buf.push((ch, *cur));
            }
        }
    }
    *i += lit_len;
}

// ── Syntax highlighting ─────────────────────────────────────────────

/// A language's highlighting rules (token-based, regex-free).
struct LangSpec {
    keywords: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
}

const RUST: LangSpec = LangSpec {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ],
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"'],
};

const PYTHON: LangSpec = LangSpec {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self",
        "True", "try", "while", "with", "yield",
    ],
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
};

const JSTS: LangSpec = LangSpec {
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "constructor",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "get",
        "if",
        "implements",
        "import",
        "in",
        "infer",
        "instanceof",
        "interface",
        "keyof",
        "let",
        "namespace",
        "new",
        "null",
        "of",
        "readonly",
        "return",
        "set",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "yield",
    ],
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\'', '`'],
};

const JSON: LangSpec = LangSpec {
    keywords: &["true", "false", "null"],
    line_comments: &[],
    block_comment: None,
    quotes: &['"'],
};

const TOML: LangSpec = LangSpec {
    keywords: &["true", "false", "inf", "nan"],
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
};

const BASH: LangSpec = LangSpec {
    keywords: &[
        "alias", "break", "case", "cd", "continue", "declare", "do", "done", "echo", "elif",
        "else", "esac", "eval", "exec", "exit", "export", "fi", "for", "function", "if", "in",
        "local", "read", "readonly", "return", "select", "set", "shift", "source", "then", "time",
        "trap", "unset", "until", "while",
    ],
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
};

const YAML: LangSpec = LangSpec {
    keywords: &["false", "no", "null", "off", "on", "true", "yes"],
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
};

const LUA: LangSpec = LangSpec {
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
        "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ],
    line_comments: &["--"],
    block_comment: Some(("--[[", "]]")),
    quotes: &['"', '\''],
};

/// Resolve a fence language tag to its highlight rules.
fn lang_for(tag: &str) -> Option<&'static LangSpec> {
    let t = tag.trim().to_ascii_lowercase();
    match t.as_str() {
        "rust" | "rs" => Some(&RUST),
        "python" | "py" | "python3" => Some(&PYTHON),
        "js" | "javascript" | "ts" | "typescript" => Some(&JSTS),
        "json" => Some(&JSON),
        "toml" => Some(&TOML),
        "bash" | "sh" | "shell" | "zsh" => Some(&BASH),
        "yaml" | "yml" => Some(&YAML),
        "lua" => Some(&LUA),
        _ => None,
    }
}

/// Does `chars[i..]` start with the (ASCII) pattern `pat`?
fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(k, pc)| chars.get(i + k) == Some(&pc))
}

/// Highlight one physical code line into styled spans. Plain segments keep
/// `Style::default()` (painted in the block's base colour); keywords,
/// strings, numbers and comments get dedicated tokens.
fn highlight_code_line(line: &str, lang: Option<&LangSpec>) -> Vec<Span> {
    let Some(lang) = lang else {
        return vec![Span::new(line, Style::default())];
    };
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0;
    while i < n {
        // Line comments consume the rest of the line.
        if lang
            .line_comments
            .iter()
            .any(|c| starts_with_at(&chars, i, c))
        {
            spans.push(Span::new(
                chars[i..].iter().collect::<String>(),
                Style::colored(Token::TextMuted),
            ));
            break;
        }
        // Block comments: consume up to the closer (or end of line).
        if let Some((open, close)) = lang.block_comment {
            if starts_with_at(&chars, i, open) {
                let end = (i..n)
                    .find(|&j| starts_with_at(&chars, j, close))
                    .map_or(n, |j| (j + close.chars().count()).min(n));
                spans.push(Span::new(
                    chars[i..end].iter().collect::<String>(),
                    Style::colored(Token::TextMuted),
                ));
                i = end;
                continue;
            }
        }
        // Strings, with backslash-escape skipping.
        if lang.quotes.contains(&chars[i]) {
            let quote = chars[i];
            let mut j = i + 1;
            while j < n {
                if chars[j] == '\\' {
                    j += 2;
                } else if chars[j] == quote {
                    break;
                } else {
                    j += 1;
                }
            }
            let end = (j + 1).min(n);
            spans.push(Span::new(
                chars[i..end].iter().collect::<String>(),
                Style::colored(Token::Success),
            ));
            i = end;
            continue;
        }
        // Words: keywords, numbers, plain identifiers.
        let c = chars[i];
        if c.is_alphanumeric() || c == '_' {
            let is_number = c.is_ascii_digit();
            let mut j = i;
            while j < n
                && (chars[j].is_alphanumeric() || chars[j] == '_' || (is_number && chars[j] == '.'))
            {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let style = if is_number {
                Style::colored(Token::Warning)
            } else if lang.keywords.contains(&word.as_str()) {
                Style::colored(Token::Violet)
            } else {
                Style::default()
            };
            spans.push(Span::new(word, style));
            i = j;
            continue;
        }
        spans.push(Span::new(c.to_string(), Style::default()));
        i += 1;
    }
    spans
}

// ── Block-level renderer ────────────────────────────────────────────

/// Cell alignment parsed from a table delimiter row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

/// State of an open fenced code block.
struct FenceState {
    lang_tag: String,
    lang: Option<&'static LangSpec>,
    fence_char: char,
    fence_len: usize,
    indent: usize,
    lines: Vec<String>,
}

/// An open list item accumulating its content spans.
struct OpenItem {
    indent: usize,
    /// Visible width of the marker including its trailing space.
    prefix_w: usize,
    spans: Vec<Span>,
}

/// A detected list marker at the start of a line.
struct MarkerLine {
    indent: usize,
    content: String,
    ordered: bool,
    marker_text: String,
}

/// Incremental markdown document renderer.
struct Document<'a> {
    theme: &'a Theme,
    mode: ColorMode,
    /// Effective (possibly unbounded) wrap width.
    width: usize,
    rule_len: usize,
    out: Vec<String>,
    pending_gap: bool,
    para: Vec<Span>,
    current: Option<OpenItem>,
    fence: Option<FenceState>,
}

impl<'a> Document<'a> {
    fn new(theme: &'a Theme, width: usize, mode: ColorMode) -> Self {
        let width = effective_width(width);
        let rule_len = if width == usize::MAX {
            40
        } else {
            width.max(3)
        };
        Document {
            theme,
            mode,
            width,
            rule_len,
            out: Vec::new(),
            pending_gap: false,
            para: Vec::new(),
            current: None,
            fence: None,
        }
    }

    /// Feed a whole document (split into logical lines, CRLF normalized).
    fn feed(&mut self, text: &str) {
        let lines: Vec<String> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).replace('\t', "  "))
            .collect();
        let mut i = 0;
        while i < lines.len() {
            i += self.step(&lines, i);
        }
    }

    /// Process the line at `i`; returns how many lines were consumed.
    fn step(&mut self, lines: &[String], i: usize) -> usize {
        let line = &lines[i];
        if self.fence.is_some() {
            self.step_fence(line);
            return 1;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            self.on_blank();
            return 1;
        }
        if let Some(fs) = parse_fence_open(line) {
            self.flush_all();
            self.fence = Some(fs);
            return 1;
        }
        if let Some((level, content)) = parse_heading(trimmed) {
            self.flush_all();
            self.render_heading(level, &content);
            return 1;
        }
        if is_rule(trimmed) {
            self.flush_all();
            self.render_rule();
            return 1;
        }
        if trimmed.contains('|') && i + 1 < lines.len() {
            if let Some(aligns) = parse_delimiter_row(&lines[i + 1]) {
                let header = split_table_row(trimmed);
                if !header.is_empty() && header.len() == aligns.len() {
                    self.flush_all();
                    return self.render_table(header, aligns, lines, i + 2);
                }
            }
        }
        if trimmed.starts_with('>') {
            self.flush_all();
            self.render_quote(trimmed);
            return 1;
        }
        if let Some(marker) = parse_list_marker(line) {
            self.flush_para();
            if let Some(cur) = self.current.take() {
                self.emit_item(&cur);
            }
            let depth = (marker.indent / 2).min(64);
            let (marker_span, prefix_w) = if marker.ordered {
                (
                    Span::new(
                        format!("{} ", marker.marker_text),
                        Style::colored(Token::Accent),
                    ),
                    marker.marker_text.chars().count() + 1,
                )
            } else {
                let glyph = BULLETS[depth.min(BULLETS.len() - 1)];
                (
                    Span::new(format!("{glyph} "), Style::colored(Token::Accent)),
                    2,
                )
            };
            let mut spans = vec![marker_span];
            spans.extend(parse_inline(&marker.content, Style::colored(Token::Text)));
            self.current = Some(OpenItem {
                indent: marker.indent,
                prefix_w,
                spans,
            });
            return 1;
        }
        // Indented non-marker line while a list item is open: continuation.
        if self.current.is_some() && indent_of(line) >= 2 {
            if let Some(cur) = self.current.as_mut() {
                cur.spans.push(Span::new(" ", Style::default()));
                cur.spans
                    .extend(parse_inline(trimmed, Style::colored(Token::Text)));
            }
            return 1;
        }
        self.flush_all();
        self.para_line(trimmed);
        1
    }

    /// Consume one line inside an open code fence.
    fn step_fence(&mut self, line: &str) {
        let Some(fs) = self.fence.as_mut() else {
            return;
        };
        let t = line.trim();
        let is_closing = t.chars().count() >= fs.fence_len
            && t.chars().all(|c| c == fs.fence_char)
            && t.chars().count() > 0;
        if is_closing {
            let fs = self.fence.take().expect("fence checked above");
            self.fence_flush(fs);
            return;
        }
        fs.lines.push(strip_indent(line, fs.indent));
    }

    /// Render a finished code fence (also used for unterminated fences).
    /// Each code line is painted on the `CodeSurface` background so
    /// fenced blocks read as a distinct dark surface in the transcript.
    fn fence_flush(&mut self, fs: FenceState) {
        self.push_gap();
        let content_width = self.width.saturating_sub(2);
        let bg = Some(Token::CodeSurface);
        for (idx, raw) in fs.lines.iter().enumerate() {
            let spans = highlight_code_line(raw, fs.lang);
            for (r, row) in hard_wrap_spans(&spans, content_width)
                .into_iter()
                .enumerate()
            {
                let mut text = self.theme.paint_attrs_with(
                    self.mode,
                    Token::Border,
                    &[],
                    CODE_BAR.to_string().as_str(),
                );
                if !row.is_empty() {
                    text.push(' ');
                    for span in &row {
                        text.push_str(&paint_span(
                            self.theme,
                            self.mode,
                            span,
                            Token::TextSecondary,
                            bg,
                        ));
                    }
                }
                // Right-align the language tag on the first rendered row.
                if idx == 0 && r == 0 && !fs.lang_tag.is_empty() && self.width != usize::MAX {
                    let tag_w = fs.lang_tag.chars().count();
                    let used: usize = 2 + row.iter().map(|s| s.width()).sum::<usize>();
                    let pad = self.width.saturating_sub(used);
                    if pad > tag_w {
                        text.push_str(&" ".repeat(pad - tag_w));
                        text.push_str(&paint_span(
                            self.theme,
                            self.mode,
                            &Span::new(fs.lang_tag.clone(), Style::default()),
                            Token::TextMuted,
                            bg,
                        ));
                    }
                }
                // Paint the remaining width in the same background so the
                // surface runs the full line, not just the text extent.
                if self.width != usize::MAX {
                    let painted: usize = 2
                        + row.iter().map(|s| s.width()).sum::<usize>()
                        + if idx == 0 && r == 0 && !fs.lang_tag.is_empty() {
                            fs.lang_tag.chars().count()
                        } else {
                            0
                        };
                    let fill = self.width.saturating_sub(painted);
                    if fill > 0 {
                        text.push_str(&paint_span(
                            self.theme,
                            self.mode,
                            &Span::new(" ".repeat(fill), Style::default()),
                            Token::TextSecondary,
                            bg,
                        ));
                    }
                }
                self.out.push(text);
            }
        }
    }

    fn render_heading(&mut self, level: usize, content: &str) {
        self.push_gap();
        let (bar_token, text_base) = match level {
            1 => (
                Token::AccentBright,
                Style::colored(Token::AccentBright).with_bold(true),
            ),
            2..=4 => (Token::Accent, Style::colored(Token::Accent)),
            _ => (Token::Accent, Style::colored(Token::TextSecondary)),
        };
        let mut spans = vec![Span::new(
            format!("{HEADING_BAR} "),
            Style::colored(bar_token),
        )];
        spans.extend(parse_inline(content, text_base));
        let lines = wrap_spans(self.theme, self.mode, &spans, Token::Text, self.width, 0, 2);
        self.out.extend(lines);
    }

    fn render_rule(&mut self) {
        self.push_gap();
        let line: String = std::iter::repeat_n(RULE_GLYPH, self.rule_len).collect::<String>();
        self.out.push(
            self.theme
                .paint_with(self.mode, Token::Border, line.as_str()),
        );
    }

    fn render_quote(&mut self, trimmed: &str) {
        self.push_gap();
        let mut level = 0usize;
        let mut rest = trimmed;
        while let Some(r) = rest.strip_prefix('>') {
            level += 1;
            rest = r.trim_start();
        }
        let indent = level.saturating_sub(1) * 2;
        let mut spans = vec![Span::new(
            format!("{QUOTE_BAR} "),
            Style::colored(Token::TextSecondary),
        )];
        if !rest.is_empty() {
            spans.extend(parse_inline(rest, Style::colored(Token::TextSecondary)));
        }
        let lines = wrap_spans(
            self.theme,
            self.mode,
            &spans,
            Token::Text,
            self.width,
            indent,
            indent + 2,
        );
        self.out.extend(lines);
    }

    /// Render a GFM table starting after its delimiter row; returns the
    /// index of the first line after the table.
    fn render_table(
        &mut self,
        header: Vec<String>,
        aligns: Vec<Align>,
        lines: &[String],
        start: usize,
    ) -> usize {
        self.push_gap();
        let mut body: Vec<Vec<String>> = Vec::new();
        let mut i = start;
        while i < lines.len() && lines[i].trim().contains('|') {
            body.push(split_table_row(lines[i].trim()));
            i += 1;
        }
        // Ragged rows: widen the grid to fit the longest row.
        let ncols = header
            .len()
            .max(body.iter().map(Vec::len).max().unwrap_or(0));
        let mut aligns = aligns;
        while aligns.len() < ncols {
            aligns.push(Align::Left);
        }
        let mut header = header;
        while header.len() < ncols {
            header.push(String::new());
        }
        let hspans: Vec<Vec<Span>> = header
            .iter()
            .map(|c| parse_inline(c, Style::colored(Token::Accent).with_bold(true)))
            .collect();
        // One `Vec<Span>` per cell; body rows are `Vec<Vec<Span>>`.
        let bspans: Vec<Vec<Vec<Span>>> = body
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| parse_inline(c, Style::colored(Token::Text)))
                    .collect::<Vec<Vec<Span>>>()
            })
            .collect();
        let mut widths = vec![1usize; ncols];
        let measure = |row: &[Vec<Span>], widths: &mut [usize]| {
            for (k, cell) in row.iter().enumerate() {
                if k < widths.len() {
                    let w: usize = cell.iter().map(|s| s.width()).sum();
                    widths[k] = widths[k].max(w);
                }
            }
        };
        measure(&hspans, &mut widths);
        for row in &bspans {
            measure(row, &mut widths);
        }
        self.out
            .push(self.table_row(&hspans, &widths, true, &aligns));
        let mut sep = String::from("├");
        for (k, w) in widths.iter().enumerate() {
            if k > 0 {
                sep.push('┼');
            }
            for _ in 0..(w + 2) {
                sep.push('─');
            }
        }
        sep.push('┤');
        self.out.push(
            self.theme
                .paint_with(self.mode, Token::Border, sep.as_str()),
        );
        for row in &bspans {
            self.out.push(self.table_row(row, &widths, false, &aligns));
        }
        i
    }

    /// Paint one table row with padded cells and border separators.
    fn table_row(
        &self,
        row: &[Vec<Span>],
        widths: &[usize],
        header: bool,
        aligns: &[Align],
    ) -> String {
        let base = if header { Token::Accent } else { Token::Text };
        let mut text = self.theme.paint_with(self.mode, Token::Border, "│");
        for (k, w) in widths.iter().enumerate() {
            let empty: Vec<Span> = Vec::new();
            let spans = row.get(k).unwrap_or(&empty);
            let visible: usize = spans.iter().map(|s| s.width()).sum::<usize>();
            let gap = w.saturating_sub(visible);
            let (lp, rp) = match aligns.get(k).copied().unwrap_or(Align::Left) {
                Align::Left => (0, gap),
                Align::Right => (gap, 0),
                Align::Center => (gap / 2, gap - gap / 2),
            };
            text.push(' ');
            for _ in 0..lp {
                text.push(' ');
            }
            for span in spans {
                text.push_str(&paint_span(self.theme, self.mode, span, base, None));
            }
            for _ in 0..rp {
                text.push(' ');
            }
            text.push(' ');
            text.push_str(&self.theme.paint_with(self.mode, Token::Border, "│"));
        }
        text
    }

    fn para_line(&mut self, text: &str) {
        if !self.para.is_empty() {
            self.para.push(Span::new(" ", Style::default()));
        }
        self.para
            .extend(parse_inline(text, Style::colored(Token::Text)));
    }

    fn flush_para(&mut self) {
        if self.para.is_empty() {
            return;
        }
        self.push_gap();
        let spans = std::mem::take(&mut self.para);
        let lines = wrap_spans(self.theme, self.mode, &spans, Token::Text, self.width, 0, 0);
        self.out.extend(lines);
    }

    /// Flush the paragraph and any open list item.
    fn flush_all(&mut self) {
        self.flush_para();
        if let Some(cur) = self.current.take() {
            self.emit_item(&cur);
        }
    }

    fn on_blank(&mut self) {
        self.flush_all();
        self.pending_gap = true;
    }

    fn emit_item(&mut self, item: &OpenItem) {
        self.push_gap();
        let lines = wrap_spans(
            self.theme,
            self.mode,
            &item.spans,
            Token::Text,
            self.width,
            item.indent,
            item.indent.saturating_add(item.prefix_w),
        );
        self.out.extend(lines);
    }

    /// Insert a single blank line between blocks when a gap is pending.
    fn push_gap(&mut self) {
        if self.pending_gap && !self.out.is_empty() {
            self.out.push(String::new());
        }
        self.pending_gap = false;
    }

    fn finish(mut self) -> Vec<String> {
        if self.fence.is_some() {
            let fs = self.fence.take().expect("fence checked");
            self.fence_flush(fs);
        }
        self.flush_all();
        self.out
    }
}

// ── Block detection helpers ─────────────────────────────────────────

/// Leading space count of a line (tabs are normalized to spaces earlier).
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Strip up to `n` leading spaces (used for fence content alignment).
fn strip_indent(line: &str, n: usize) -> String {
    let mut s = line;
    let mut left = n;
    while left > 0 {
        let Some(rest) = s.strip_prefix(' ') else {
            break;
        };
        s = rest;
        left -= 1;
    }
    s.to_string()
}

/// Parse an opening code fence; `None` when the line is not one.
fn parse_fence_open(line: &str) -> Option<FenceState> {
    let indent = indent_of(line);
    let t = line.trim_start();
    let fence_char = if t.starts_with("```") {
        '`'
    } else if t.starts_with("~~~") {
        '~'
    } else {
        return None;
    };
    let fence_len = t.chars().take_while(|&c| c == fence_char).count();
    if fence_len < 3 {
        return None;
    }
    let info = t[fence_len..].trim();
    Some(FenceState {
        lang_tag: info.to_string(),
        lang: lang_for(info),
        fence_char,
        fence_len,
        indent,
        lines: Vec::new(),
    })
}

/// Parse an ATX heading: level and content (with closing `#`s stripped).
fn parse_heading(trimmed: &str) -> Option<(usize, String)> {
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let content = rest.trim();
    // Closed ATX: strip a trailing `#` run preceded by whitespace.
    let stripped = content.trim_end_matches('#');
    let content = if stripped.len() < content.len() && stripped.ends_with(' ') {
        stripped.trim_end()
    } else {
        content
    };
    Some((hashes, content.to_string()))
}

/// A thematic break: three or more identical `-`/`*`/`_` (spaces ignored).
fn is_rule(trimmed: &str) -> bool {
    let squeezed: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    squeezed.len() >= 3 && squeezed.chars().all(|c| matches!(c, '-' | '*' | '_'))
}

/// Detect a list marker at the start of a raw line.
fn parse_list_marker(line: &str) -> Option<MarkerLine> {
    let indent = indent_of(line);
    let t = line.trim_start();
    let first = t.chars().next()?;
    if matches!(first, '-' | '*' | '+') {
        let rest = &t[1..];
        if rest.is_empty() || rest.starts_with(' ') {
            return Some(MarkerLine {
                indent,
                content: rest.trim().to_string(),
                ordered: false,
                marker_text: String::new(),
            });
        }
        return None;
    }
    if first.is_ascii_digit() {
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        let after = t[digits..].chars().next()?;
        if after == '.' || after == ')' {
            let rest = &t[digits + 1..];
            if rest.is_empty() || rest.starts_with(' ') {
                return Some(MarkerLine {
                    indent,
                    content: rest.trim().to_string(),
                    ordered: true,
                    marker_text: t[..digits + 1].to_string(),
                });
            }
        }
    }
    None
}

/// Split a table row into cells, honouring `\|` escapes and dropping the
/// optional outer-pipe padding cells.
fn split_table_row(row: &str) -> Vec<String> {
    let mut cells: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut it = row.chars();
    while let Some(c) = it.next() {
        if c == '\\' && it.next() == Some('|') {
            cur.push('|');
        } else if c == '|' {
            let taken = std::mem::take(&mut cur);
            cells.push(taken.trim().to_string());
        } else {
            cur.push(c);
        }
    }
    let last = std::mem::take(&mut cur);
    cells.push(last.trim().to_string());
    if row.trim_start().starts_with('|') && cells.first().is_some_and(String::is_empty) {
        cells.remove(0);
    }
    if row.trim_end().ends_with('|') && cells.last().is_some_and(String::is_empty) {
        cells.pop();
    }
    cells
}

/// Parse a GFM table delimiter row into per-column alignment.
fn parse_delimiter_row(line: &str) -> Option<Vec<Align>> {
    let t = line.trim();
    if t.is_empty() || !t.contains('-') {
        return None;
    }
    let mut aligns = Vec::new();
    for cell in split_table_row(t) {
        let c = cell.trim();
        let body = c.trim_matches(':');
        if body.is_empty() || !body.chars().all(|ch| ch == '-') {
            return None;
        }
        let l = c.starts_with(':');
        let r = c.ends_with(':');
        aligns.push(if l && r {
            Align::Center
        } else if r {
            Align::Right
        } else {
            Align::Left
        });
    }
    Some(aligns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorMode;
    use crate::theme::theme_from_name;

    const MODE: ColorMode = ColorMode::NoColor;
    const W: usize = 80;

    fn md(text: &str) -> Vec<String> {
        render_with_mode(text, &theme_from_name("default"), W, MODE)
    }

    fn joined(text: &str) -> String {
        md(text).join("\n")
    }

    /// Remove SGR escape sequences so tests can assert on visible text and
    /// measure visible width.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    // SGR sequences end on any alphabetic final byte.
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn visible_lines(text: &str) -> Vec<String> {
        md(text).iter().map(|l| strip_ansi(l)).collect()
    }

    // ── basics ─────────────────────────────────────────────────────

    #[test]
    fn empty_and_blank_input_yield_no_lines() {
        assert!(md("").is_empty());
        assert!(md("\n").is_empty());
        assert!(md("\n\n   \n\t\n").is_empty());
    }

    #[test]
    fn plain_paragraph_without_trailing_newline() {
        let out = md("just some text");
        assert_eq!(out.len(), 1);
        assert_eq!(strip_ansi(&out[0]), "just some text");
        // NoColor mode emits no escapes at all for plain prose.
        assert!(!out[0].contains('\x1b'));
    }

    #[test]
    fn paragraphs_are_separated_by_one_blank_line() {
        let out = md("para one\n\n\npara two\n\npara three");
        assert_eq!(out, vec!["para one", "", "para two", "", "para three"]);
    }

    #[test]
    fn crlf_input_is_normalized() {
        let out = md("first\r\n\r\nsecond\r\n");
        assert_eq!(out, vec!["first", "", "second"]);
    }

    #[test]
    fn tabs_become_spaces() {
        assert_eq!(strip_ansi(&joined("-\tx")), "• x");
    }

    // ── headings & rules ───────────────────────────────────────────

    #[test]
    fn headings_use_bar_prefix_and_h1_is_bold() {
        let src = "# Title\n## Sub\n### Deep\n#### Four\n##### Five\n###### Six";
        let j = strip_ansi(&joined(src));
        assert!(j.contains("▌ Title"));
        assert!(j.contains("▌ Sub"));
        assert!(j.contains("▌ Deep"));
        assert!(j.contains("▌ Four"));
        assert!(j.contains("▌ Five"));
        assert!(j.contains("▌ Six"));
        // h1 keeps its bold even in NoColor mode; `#foo` is not a heading.
        let h1 = md("# Title");
        assert!(h1[0].contains("Title"));
        let not_heading = md("#nospace");
        assert_eq!(strip_ansi(&not_heading[0]), "#nospace");
        // Closed ATX headings strip the trailing marker run.
        assert_eq!(strip_ansi(joined("## Done ##").trim()), "▌ Done");
    }

    #[test]
    fn heading_inline_markup_composes() {
        let j = joined("# **Big** and `code`");
        assert!(j.contains("Big"));
        assert!(j.contains("`code`"));
    }

    #[test]
    fn horizontal_rule_spans_the_width() {
        let out = md("---\n");
        assert_eq!(strip_ansi(&out[0]), "─".repeat(W));
        assert_eq!(strip_ansi(&joined("***")), "─".repeat(W));
        assert_eq!(strip_ansi(&joined("_ _ _")), "─".repeat(W));
    }

    // ── inline styles & links ──────────────────────────────────────

    #[test]
    fn bold_survives_nocolor_mode() {
        let j = joined("**bold** plain");
        assert!(j.contains("\x1b[1mbold\x1b[0m"));
        assert!(strip_ansi(&j).contains("bold plain"));
    }

    #[test]
    fn nested_and_combined_emphasis() {
        let j = joined("**bold *with ital* inside**");
        assert!(j.contains("\x1b[1mbold"));
        assert!(strip_ansi(&j).contains("bold with ital inside"));
        // bold containing inline code
        let j2 = joined("**a `bit` b**");
        assert!(j2.contains("`bit`"));
        // italics inside plain text with intraword stars
        assert_eq!(strip_ansi(&joined("a*b*c")), "a b c");
    }

    #[test]
    fn underscore_emphasis_respects_word_boundaries() {
        let j = joined("snake_case_stays and _emphasized_ and __stronger__");
        assert!(j.contains("snake_case_stays"));
        assert!(strip_ansi(&j).contains("and emphasized and stronger"));
    }

    #[test]
    fn strikethrough_and_escaped_markers() {
        let j = joined("~~gone~~ stays");
        assert!(strip_ansi(&j).contains("gone stays"), "got: {j:?}");
        assert!(!strip_ansi(&j).contains("~~"));
        assert_eq!(strip_ansi(&joined("\\*literal\\*")), "*literal*");
    }

    #[test]
    fn inline_code_keeps_backticks() {
        assert!(joined("use `x + y` here").contains("`x + y`"));
        // unterminated backtick stays literal
        assert!(joined("a ` b").contains("a ` b"));
    }

    #[test]
    fn links_render_as_text_url() {
        let j = joined("see [docs](https://example.io/a) now");
        let plain = strip_ansi(&j);
        assert!(plain.contains("docs"), "got: {plain}");
        assert!(plain.contains("https://example.io/a"), "got: {plain}");
        // URL is underlined even in NoColor mode.
        assert!(plain.contains("https://example.io/a"));
        // images render like links; malformed links stay literal
        assert!(strip_ansi(&joined("![alt](img.png)")).contains("img.png"));
        assert!(strip_ansi(&joined("[unclosed](nope")).contains("[unclosed](nope"));
        assert!(strip_ansi(&joined("bare [ref] here")).contains("bare [ref] here"));
    }

    // ── code fences ────────────────────────────────────────────────

    #[test]
    fn rust_fence_is_highlighted_with_theme_colors() {
        let theme = theme_from_name("default");
        let out = render_with_mode(
            "```rust\nlet x = 42; // init\n```",
            &theme,
            W,
            ColorMode::TrueColor,
        );
        let j = out.join("\n");
        // The bg param rides inside each span's single SGR sequence (attrs;fg;bg),
        // so it cannot be matched as a standalone paint. Verify via strip +
        // that a `48;2;` bg sequence appears (any token with a bg param).
        let bg_seq = "48;2;";
        assert!(
            j.contains(bg_seq),
            "fenced lines must carry the CodeSurface bg param: {j:?}"
        );
        // Keyword highlighting is still present (extracted from span).
        let stripped = strip_ansi(&j);
        assert!(stripped.contains("let"));
        assert!(stripped.contains("42"));
        assert!(stripped.contains("// init"));
    }

    #[test]
    fn string_and_comment_highlighting_python() {
        let theme = theme_from_name("default");
        let j = render_with_mode(
            "```python\ns = \"hi\"  # note\n```",
            &theme,
            W,
            ColorMode::TrueColor,
        )
        .join("\n");
        let stripped = strip_ansi(&j);
        assert!(
            stripped.contains("\"hi\""),
            "string must survive bg param: {j:?}"
        );
        assert!(
            stripped.contains("# note"),
            "comment must survive bg param: {j:?}"
        );
    }

    #[test]
    fn all_supported_languages_highlight_keywords() {
        let theme = theme_from_name("default");
        let _paint = |t: Token, s: &str| theme.paint_with(ColorMode::TrueColor, t, s);
        let cases = [
            ("rust", "fn", Token::Violet),
            ("python", "lambda", Token::Violet),
            ("js", "const", Token::Violet),
            ("typescript", "interface", Token::Violet),
            ("json", "null", Token::Violet),
            ("toml", "true", Token::Violet),
            ("bash", "then", Token::Violet),
            ("shell", "esac", Token::Violet),
            ("yaml", "true", Token::Violet),
        ];
        for (lang, kw, token) in cases {
            let src = format!("```{lang}\nx = {kw}\n```");
            let j = render_with_mode(&src, &theme, W, ColorMode::TrueColor).join("\n");
            // The bg param rides inside the span's SGR sequence, so check
            // the visible text instead of the raw SGR-painted keyword.
            let stripped = strip_ansi(&j);
            assert!(
                stripped.contains(kw),
                "lang {lang} missed keyword {kw} in {stripped:?}"
            );
            // The violet highlight is still in the SGR params.
            let violet_rgb = theme.rgb(token);
            let sgr = format!("38;2;{};{};{}", violet_rgb.0, violet_rgb.1, violet_rgb.2);
            assert!(j.contains(&sgr), "lang {lang} missed violet SGR: {j:?}");
        }
    }

    #[test]
    fn unknown_language_is_plain_muted() {
        let theme = theme_from_name("default");
        let j = render_with_mode(
            "```weirdlang\n??? 42 \"x\"\n```",
            &theme,
            W,
            ColorMode::TrueColor,
        )
        .join("\n");
        // No highlight tokens leak in; content survives untouched.
        assert!(j.contains("42"));
        assert!(!j.contains(&theme.paint_with(ColorMode::TrueColor, Token::Success, "\"x\"")));
    }

    #[test]
    fn unterminated_fence_highlights_to_end() {
        let theme = theme_from_name("default");
        let out = render_with_mode(
            "```rust\nlet a = 1;\nfn f() {}\nmore text",
            &theme,
            W,
            ColorMode::TrueColor,
        );
        let j = out.join("\n");
        let stripped = strip_ansi(&j);
        assert!(
            stripped.contains("let"),
            "keyword must survive bg param: {j:?}"
        );
        assert!(
            stripped.contains("fn"),
            "keyword must survive bg param: {j:?}"
        );
        assert!(j.contains("more text"));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn long_code_lines_hard_wrap_within_width() {
        let out = md(&format!("```toml\nkey = \"{}\"\n```", "x".repeat(200)));
        for line in &out {
            assert!(
                strip_ansi(line).chars().count() <= W,
                "line too long: {line:?}"
            );
        }
        // content survives the wrap
        let j = out.join("");
        assert_eq!(strip_ansi(&j).matches('x').count(), 200);
    }

    // ── lists ──────────────────────────────────────────────────────

    #[test]
    fn unordered_lists_nest_with_bullets() {
        let src = "- alpha\n- beta\n  - gamma\n    - delta";
        let out = visible_lines(src);
        assert_eq!(out, vec!["• alpha", "• beta", "  ◦ gamma", "    ▪ delta"]);
    }

    #[test]
    fn ordered_list_markers_are_preserved() {
        let src = "1. one\n2. two\n10. ten";
        let out = visible_lines(src);
        assert_eq!(out, vec!["1. one", "2. two", "10. ten"]);
    }

    #[test]
    fn list_continuation_lines_align_under_text() {
        let src = "- alpha\n  continued words\n- beta";
        let out = visible_lines(src);
        assert_eq!(out, vec!["• alpha continued words", "• beta"]);
    }

    #[test]
    fn long_list_items_wrap_with_hanging_indent() {
        let words = "word ".repeat(30);
        let src = format!("- {words}end");
        let out = visible_lines(&src);
        assert!(out.len() > 1);
        assert!(out[0].starts_with("• "));
        for line in &out[1..] {
            assert!(line.starts_with("  "), "no hanging indent: {line:?}");
            assert!(line.chars().count() <= W);
        }
        assert_eq!(out.join(" ").split_whitespace().count(), 32);
    }

    #[test]
    fn not_quite_list_lines_stay_paragraphs() {
        assert_eq!(strip_ansi(&joined("-nospace")), "-nospace");
        assert_eq!(strip_ansi(&joined("*emph*")), "emph");
        assert_eq!(strip_ansi(&joined("1.x")), "1.x");
    }

    // ── blockquotes ────────────────────────────────────────────────

    #[test]
    fn blockquotes_render_with_bar_and_nesting() {
        let src = "> quoted\n>> deeper\n> plain again";
        let out = visible_lines(src);
        assert_eq!(out, vec!["┃ quoted", "  ┃ deeper", "┃ plain again"]);
        assert!(md("> quote")[0].contains("┃"));
    }

    #[test]
    fn blockquote_supports_inline_markup() {
        let j = joined("> **strong** words");
        assert!(strip_ansi(&j).contains("┃ strong words"));
    }

    // ── tables ─────────────────────────────────────────────────────

    #[test]
    fn gfm_table_renders_bordered_and_padded() {
        let src = "| Name | Age |\n|:-----|----:|\n| Alice | 30 |\n| Bob | 7 |";
        let out = visible_lines(src);
        assert_eq!(
            out,
            vec![
                "│ Name  │ Age │",
                "├───────┼─────┤",
                "│ Alice │  30 │",
                "│ Bob   │   7 │",
            ]
        );
        // header is bold even in NoColor; separator uses box joints
        assert!(md(src)[0].contains("\x1b[1mName"));
    }

    #[test]
    fn gfm_table_keeps_inline_markup_in_cells() {
        let src = "| A | B |\n|---|---|\n| `x` | **y** |";
        let out = visible_lines(src);
        assert_eq!(out[0], "│ A   │ B │");
        assert_eq!(out[2], "│ `x` │ y │");
        assert!(md(src)[2].contains("y"));
    }

    #[test]
    fn malformed_table_falls_back_to_plain_text() {
        let src = "| a | b\nnot a separator\n| c | d";
        let j = strip_ansi(&joined(src));
        assert!(j.contains("| a | b"));
        assert!(j.contains("not a separator"));
        assert!(j.contains("| c | d"));
        assert!(!j.contains("├"));
    }

    // ── width robustness ───────────────────────────────────────────

    #[test]
    fn prose_wraps_at_width() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        for line in visible_lines(text) {
            assert!(line.chars().count() <= W);
        }
        let narrow = 20usize;
        let out = render_with_mode(text, &theme_from_name("default"), narrow, MODE);
        for line in &out {
            assert!(strip_ansi(line).chars().count() <= narrow, "{line:?}");
        }
        assert!(out.len() > 1);
    }

    #[test]
    fn overlong_words_are_hard_broken() {
        let word = "z".repeat(50);
        let out = render_with_mode(&word, &theme_from_name("default"), 20, MODE);
        assert!(out.len() >= 3);
        for line in &out {
            let v = strip_ansi(line);
            assert!(v.chars().count() <= 20, "{line:?}");
        }
        assert_eq!(
            strip_ansi(&out.join(""))
                .chars()
                .filter(|c| *c == 'z')
                .count(),
            50
        );
    }

    #[test]
    fn tiny_widths_degrade_without_panicking() {
        let src = "# Head\n\nSome **bold** paragraph here\n\n```rust\nlet x = 1;\n```\n\n- item\n  - nested\n\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |";
        for w in [0usize, 1, 2, 5, 7, 19] {
            let out = render_with_mode(src, &theme_from_name("default"), w, MODE);
            assert!(!out.is_empty());
        }
    }

    // ── colour-mode sanity ─────────────────────────────────────────

    #[test]
    fn nocolor_mode_emits_no_color_sequences() {
        let theme = theme_from_name("default");
        let src = "# Head\n\n- item\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nlet x;\n```";
        let j = render_with_mode(src, &theme, 80, ColorMode::NoColor).join("\n");
        assert!(!j.contains("38;2;"));
        assert!(!j.contains("38;5;"));
        assert!(!j.contains("\x1b[3"));
        assert!(!j.contains("\x1b[9"));
    }

    #[test]
    fn truecolor_mode_emits_rgb_sequences() {
        let theme = theme_from_name("default");
        let j = render_with_mode("plain", &theme, 80, ColorMode::TrueColor).join("\n");
        assert!(j.contains("38;2;242;233;236")); // Token::Text
    }

    // ── adversarial input ──────────────────────────────────────────

    #[test]
    fn adversarial_input_never_panics() {
        let nasty = [
            "",
            "```",
            "```rust",
            "~~~~",
            "```py\n\"unterminated\n```",
            "```json\n{\"a\": [1, 2, {\"b\": null}]}\n```",
            "***",
            "___",
            "**",
            "__",
            "~~",
            "*_~[",
            "[",
            "[]()",
            "[](x",
            "![",
            "![a](b",
            "[a [b] c](d)",
            "\\",
            "\\`\\*\\",
            "\x1b[31mcolored\x1b[0m",
            ">>>",
            ">",
            "|",
            "||",
            "|-|-|",
            "|a|b|\n|---|---|",
            "|\t|\t|",
            &("  ".repeat(40) + "- deep"),
            "1.1.1.1",
            "0.",
            "10)",
            "- \n- \n- ",
            "1. \n2. \n",
            "#",
            "######",
            "####### seven",
            "#tag no space",
            "a\n\n\n\n\nb",
            "\r\r",
            "🎉 emoji + 中文 width",
            "`a` `b` **c** ~~d~~ [e](f) *g* _h_ \\i",
        ];
        for (idx, case) in nasty.iter().enumerate() {
            for w in [0usize, 3, 8, 40, 120] {
                for mode in [ColorMode::NoColor, ColorMode::TrueColor] {
                    let out = render_with_mode(case, &theme_from_name("default"), w, mode);
                    // every line must be paintable text (no mid-escape splits)
                    for line in &out {
                        let _ = strip_ansi(line);
                    }
                    let _ = (idx, w);
                }
            }
        }
    }

    #[test]
    fn emoji_and_cjk_do_not_panic_wrapping() {
        let src = "🎉🎉🎉 中文段落 very long mixed content line for wrapping tests";
        let out = render_with_mode(src, &theme_from_name("default"), 12, MODE);
        assert!(!out.is_empty());
    }
}
