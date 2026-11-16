//! Slash-command dispatch.
//!
//! Each command is a struct implementing the `Command` trait. The
//! dispatcher matches the input string to a command and runs it.

/// The result of running a command.
pub enum CommandResult {
    /// The command completed successfully with output.
    Ok(String),
    /// The command failed.
    Err(String),
    /// The command is not recognized.
    Unknown(String),
}

/// A slash command that the user can invoke.
pub trait Command: Send + Sync {
    /// The command name without the leading `/`.
    fn name(&self) -> &str;

    /// A short help line.
    fn help(&self) -> &str;

    /// Execute the command with optional arguments.
    fn run(&self, args: &str) -> CommandResult;
}

/// List all registered commands for help display.
pub fn list_commands() -> Vec<(&'static str, &'static str)> {
    vec![
        ("ask", "switch to Ask mode (read-only)"),
        ("plan", "switch to Plan mode"),
        ("code", "switch to Code mode"),
        ("auto", "switch to Auto mode"),
        ("debug", "switch to Debug mode"),
        ("research", "switch to Research mode"),
        ("model", "show or test the configured model"),
        ("help", "show this help"),
    ]
}
