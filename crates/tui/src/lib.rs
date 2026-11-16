//! REQUIEM TUI crate.
//!
//! Renders the startup banner and the live agent dashboard using
//! `ratatui` + `crossterm`. The banner is the exact ASCII art from
//! `REQUIEM-ASCII-BANNER.txt` embedded at compile time.

pub mod banner;
pub mod dashboard;
pub mod theme;

pub use banner::{render_banner, render_themed, BANNER};
pub use dashboard::{
    AgentRow, AgentStatus, DashboardRenderer, DashboardState, PlainRenderer, ThemedRenderer,
};
pub use theme::{theme_from_name, Theme, THEME_NAMES};
