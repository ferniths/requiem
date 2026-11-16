//! REQUIEM TUI crate.
//!
//! Renders the startup banner and the live agent dashboard using
//! `ratatui` + `crossterm`. The banner is the exact ASCII art from
//! `REQUIEM-ASCII-BANNER.txt` embedded at compile time.

pub mod banner;
pub mod dashboard;

pub use banner::{render_banner, BANNER};
pub use dashboard::{AgentRow, AgentStatus, DashboardRenderer, DashboardState, PlainRenderer};
