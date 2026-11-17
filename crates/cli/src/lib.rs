//! REQUIEM CLI crate.
//!
//! The main entry point: parses arguments, loads config, initializes the
//! model client, and dispatches slash commands.

pub mod clipboard;
pub mod commands;
pub mod config;
pub mod history;
pub mod session;
pub mod setup;
