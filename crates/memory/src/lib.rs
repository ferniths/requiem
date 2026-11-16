//! REQUIEM memory crate.
//!
//! SQLite-backed session persistence: saves agent runs, token usage,
//! and task state so a session can be resumed after a crash or restart.

pub mod session;

pub use session::{Session, SessionRecord};
