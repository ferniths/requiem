//! REQUIEM CLI crate.
//!
//! The main entry point: parses arguments, loads config, initializes the
//! model client, and dispatches slash commands.

pub mod commands;
pub mod config;

pub use commands::CommandResult;
