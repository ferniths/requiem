//! REQUIEM context crate.
//!
//! Manages the message history sent to the model: budgets tokens,
//! compresses old tool results, and deduplicates repeated content.

pub mod budget;
pub mod compress;

pub use budget::ContextBudget;
pub use compress::Compressor;

use requiem_model::request::ChatMessage;

/// The result of a context-management pass over the message history.
pub struct ManagedContext {
    /// The messages to send to the model.
    pub messages: Vec<ChatMessage>,
    /// Whether any compression was applied.
    pub compressed: bool,
    /// Tokens freed by compression.
    pub tokens_freed: u32,
}

/// A trait for strategies that decide how to shape the context window.
pub trait ContextStrategy: Send + Sync {
    /// Apply the strategy to the current history.
    fn apply(&self, history: Vec<ChatMessage>, budget: &ContextBudget) -> ManagedContext;
}
