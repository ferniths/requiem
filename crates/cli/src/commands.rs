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

/// List all registered commands for help display, grouped by category.
pub fn list_commands() -> Vec<(&'static str, &'static str)> {
    vec![
        // Modes
        ("ask", "switch to Ask mode (read-only)"),
        ("plan", "switch to Plan mode"),
        ("code", "switch to Code mode"),
        ("auto", "switch to Auto mode"),
        ("debug", "switch to Debug mode"),
        ("research", "switch to Research mode"),
        // Model
        ("model", "show or test the configured model"),
        // Session
        ("setup", "re-run the interactive setup wizard"),
        ("theme", "list available theme variants with previews"),
        // Info
        ("help", "show this help"),
    ]
}

/// Grouped help display for the REPL.
pub fn list_commands_grouped() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            "Modes",
            vec![
                ("ask", "read-only inspection and answers"),
                ("plan", "investigate and produce a plan"),
                ("code", "implement, delegate, build, test"),
                ("auto", "fully autonomous execution"),
                ("debug", "reproduce, root-cause, fix, verify"),
                ("research", "parallel research and vault writes"),
            ],
        ),
        (
            "Model",
            vec![("model", "show or test the configured model")],
        ),
        (
            "Session",
            vec![
                ("setup", "re-run the interactive setup wizard"),
                ("theme", "list available theme variants"),
            ],
        ),
        (
            "Info",
            vec![
                ("help", "show this help"),
                ("exit", "leave the session"),
            ],
        ),
    ]
}
