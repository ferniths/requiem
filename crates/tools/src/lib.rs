//! REQUIEM tools crate.
//!
//! Concrete tool implementations that the orchestration layer calls when
//! an agent requests a tool. Each tool implements the `Tool` trait and
//! returns a `serde_json::Value` result.

pub mod file_ops;
pub mod git;
pub mod obsidian;
pub mod search;
pub mod subagent;
pub mod terminal;

pub use file_ops::{FileDeleteTool, FileEditTool, FileReadTool, FileWriteTool};
pub use git::GitTool;
pub use obsidian::ObsidianTool;
pub use search::{GlobSearchTool, GrepSearchTool};
pub use subagent::make_subagent_tool;
pub use terminal::TerminalTool;

use serde_json::Value;

/// The error type returned by all tools.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// A filesystem error.
    #[error("file error: {0}")]
    File(String),

    /// The edit target string was not found in the file.
    #[error("text not found in file: {0}")]
    EditNotFound(String),

    /// A terminal command failed.
    #[error("terminal error: {0}")]
    Terminal(String),

    /// A search operation failed.
    #[error("search error: {0}")]
    Search(String),

    /// An argument was invalid.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

/// Convenience result alias for tool implementations.
pub type Result<T> = std::result::Result<T, ToolError>;

/// A tool that an agent can call.
///
/// Each tool exposes its name, a human-readable description, a JSON schema
/// for its arguments, and a synchronous `execute` method. The orchestration
/// layer wraps `execute` in an async context when needed.
pub trait Tool: Send + Sync {
    /// The tool's name (used in model API `tool_calls`).
    fn name(&self) -> &str;

    /// A short description shown to the model so it knows when to use the tool.
    fn description(&self) -> &str;

    /// The JSON schema describing the tool's arguments.
    fn schema(&self) -> Value;

    /// Build the `ToolDef` this tool advertises to the model.
    fn tool_def(&self) -> requiem_model::request::ToolDef {
        requiem_model::request::ToolDef::new(self.name(), self.description(), self.schema())
    }

    /// Execute the tool with the given arguments.
    fn execute(&self, args: &Value) -> Result<Value>;
}

#[cfg(test)]
pub mod tests {
    use async_trait::async_trait;
    use requiem_core::error::RequiemError;
    use requiem_orchestration::agent_runtime::ToolExecutor;

    /// A no-op executor that always returns a success JSON.
    pub struct NoopExecutor;

    #[async_trait]
    impl ToolExecutor for NoopExecutor {
        async fn execute(
            &self,
            _name: &str,
            _arguments: &serde_json::Value,
        ) -> Result<serde_json::Value, RequiemError> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    pub fn noop_executor() -> NoopExecutor {
        NoopExecutor
    }
}
