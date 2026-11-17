//! REQUIEM TUI crate.
//!
//! Renders the startup banner and the live agent dashboard using
//! `ratatui` + `crossterm`. The banner is the exact ASCII art from
//! `REQUIEM-ASCII-BANNER.txt` embedded at compile time.

pub mod app;
pub mod banner;
pub mod color;
pub mod dashboard;
pub mod markdown;
pub mod theme;

pub use app::{App, CommandHints, DashState, UiAction, fmt_tokens};
pub use banner::{banner, render_banner, render_themed, BANNER};
pub use color::ColorMode;
pub use dashboard::{
    AgentRow, AgentStatus, DashboardRenderer, DashboardState, PlainRenderer, ThemedRenderer,
    render_task_dashboard, TaskDashboard, TaskRow, TaskStatus,
};
pub use markdown::{render as render_markdown, render_with_mode as render_markdown_with_mode};
pub use theme::{theme_from_name, Theme, THEME_NAMES, init_color_mode};
