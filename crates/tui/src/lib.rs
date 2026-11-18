//! REQUIEM TUI crate.
//!
//! Renders the startup banner and the live agent dashboard using
//! `ratatui` + `crossterm`. The banner is the exact ASCII art from
//! `REQUIEM-ASCII-BANNER.txt` embedded at compile time.

pub mod app;
pub mod banner;
pub mod color;
pub mod composer;
pub mod dashboard;
pub mod markdown;
pub mod theme;

pub use app::{fmt_tokens, App, CommandHints, DashFocus, DashState, HistoryEntry, UiAction};
pub use banner::{banner, render_banner, render_themed, BANNER};
pub use color::ColorMode;
pub use dashboard::{
    render_task_dashboard, AgentRow, AgentStatus, DashboardRenderer, DashboardState, PlainRenderer,
    TaskDashboard, TaskRow, TaskStatus, ThemedRenderer,
};
pub use markdown::{render as render_markdown, render_with_mode as render_markdown_with_mode};
pub use theme::{init_color_mode, theme_from_name, Theme, THEME_NAMES};
