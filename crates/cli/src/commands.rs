//! Slash-command registry for the interactive session.
//!
//! One static table: name, group, description and argument hints. The
//! session UI uses it for autocomplete and `did-you-mean` suggestions,
//! and the dispatcher validates arguments against it.

/// The complete command table, grouped in display order.
pub const COMMANDS: &[CommandSpec] = &[
    // ── Modes ───────────────────────────────────────────────────────
    CommandSpec::new("ask", "Modes", "read-only inspection and answers"),
    CommandSpec::new("plan", "Modes", "investigate and produce a plan"),
    CommandSpec::new("code", "Modes", "implement, delegate, build, test"),
    CommandSpec::new("auto", "Modes", "fully autonomous execution"),
    CommandSpec::new("debug", "Modes", "reproduce, root-cause, fix, verify"),
    CommandSpec::new("research", "Modes", "parallel research and vault writes"),
    // ── Run control ─────────────────────────────────────────────────
    CommandSpec::new("stop", "Run control", "cancel the running task"),
    // ── View ────────────────────────────────────────────────────────
    CommandSpec::new("dashboard", "View", "open the live dashboard overlay"),
    CommandSpec::new("agents", "View", "list agents and their state"),
    CommandSpec::new("tasks", "View", "list tasks and their status"),
    CommandSpec::new("status", "View", "session status: mode, model, tokens, tasks"),
    CommandSpec::new("events", "View", "recent runtime events"),
    // ── Config ──────────────────────────────────────────────────────
    CommandSpec::new("model", "Config", "show or test the configured model")
        .with_args(&["--test"]),
    CommandSpec::new("theme", "Config", "list themes or switch: /theme [name]")
        .with_args(&["[name]"]),
    CommandSpec::new("setup", "Config", "re-run the interactive setup wizard"),
    CommandSpec::new("vault", "Config", "list configured Obsidian vaults"),
    CommandSpec::new("permissions", "Config", "show the permission rules for the current mode"),
    CommandSpec::new("budget", "Config", "show token and task budget usage"),
    // ── Memory ──────────────────────────────────────────────────────
    CommandSpec::new("sessions", "Memory", "list past sessions"),
    CommandSpec::new("resume", "Memory", "resume a past session: /resume <id>")
        .with_args(&["<session-id>"]),
    CommandSpec::new("brain", "Memory", "search the Obsidian brain: /brain <query>")
        .with_args(&["<query>"]),
    // ── Help ────────────────────────────────────────────────────────
    CommandSpec::new("help", "Help", "show this help"),
    CommandSpec::new("exit", "Help", "leave the session (alias: /quit)"),
];

/// One registered slash command.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// The command name without the leading `/`.
    pub name: &'static str,
    /// The display group in `/help`.
    pub group: &'static str,
    /// A short help line.
    pub description: &'static str,
    /// Argument hints (may be empty).
    pub args: &'static [&'static str],
}

impl CommandSpec {
    const fn new(name: &'static str, group: &'static str, description: &'static str) -> Self {
        Self {
            name,
            group,
            description,
            args: &[],
        }
    }

    const fn with_args(mut self, args: &'static [&'static str]) -> Self {
        self.args = args;
        self
    }
}

/// All command names (without the leading `/`), sorted.
pub fn command_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = COMMANDS.iter().map(|c| c.name).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Look up a command by name.
pub fn lookup(name: &str) -> Option<&'static CommandSpec> {
    let name = name.trim_start_matches('/');
    COMMANDS.iter().find(|c| c.name == name)
}

/// A one-line description for a command, if known.
pub fn command_description(name: &str) -> Option<&'static str> {
    lookup(name).map(|c| c.description)
}

/// Grouped command listing for `/help`.
pub fn list_commands_grouped() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    let mut groups: Vec<(&'static str, Vec<(&'static str, &'static str)>)> = Vec::new();
    for cmd in COMMANDS {
        match groups.iter_mut().find(|(g, _)| *g == cmd.group) {
            Some((_, cmds)) => cmds.push((cmd.name, cmd.description)),
            None => groups.push((cmd.group, vec![(cmd.name, cmd.description)])),
        }
    }
    groups
}

/// Flat listing (name, description) for plain help output.
pub fn list_commands() -> Vec<(&'static str, &'static str)> {
    COMMANDS
        .iter()
        .map(|c| (c.name, c.description))
        .collect()
}

/// Suggest commands that start with the given (lowercased) prefix.
/// An empty prefix returns the first few commands.
pub fn suggest_commands(prefix: &str) -> Vec<&'static str> {
    let prefix = prefix.to_lowercase();
    let names = command_names();
    if prefix.is_empty() {
        return names.iter().take(6).copied().collect();
    }
    names
        .iter()
        .copied()
        .filter(|n| n.starts_with(&prefix))
        .take(5)
        .collect()
}

/// Argument hints for a known command. Empty for commands that take none.
pub fn suggest_args(name: &str) -> Vec<&'static str> {
    lookup(name).map(|c| c.args.to_vec()).unwrap_or_default()
}

/// Whether a command accepts the given argument token (used by the
/// dispatcher to warn about unsupported args). Unknown tokens on
/// free-form commands (like `/theme <name>`) are accepted.
pub fn accepts_arg(name: &str, arg: &str) -> bool {
    let Some(spec) = lookup(name) else {
        return false;
    };
    if spec.args.is_empty() {
        return false;
    }
    let arg_lower = arg.to_lowercase();
    spec.args.iter().any(|hint| {
        let hint = hint.trim_matches(|c| c == '[' || c == ']' || c == '<' || c == '>');
        // Bracketed hints are free-form values, not literal flags.
        hint.starts_with('-') || hint.starts_with("--") || arg_lower.contains(hint) || hint == arg_lower
    })
}

/// Whether a command takes a free-form value argument.
pub fn takes_value_arg(name: &str) -> bool {
    lookup(name)
        .map(|c| {
            c.args
                .iter()
                .any(|a| a.starts_with('<') || a.starts_with('['))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_all_required_commands() {
        let required = [
            "ask", "plan", "code", "auto", "debug", "research", "agents", "tasks",
            "dashboard", "status", "model", "theme", "setup", "vault", "brain",
            "sessions", "resume", "permissions", "budget", "help", "exit",
        ];
        for name in required {
            assert!(
                lookup(name).is_some(),
                "command /{name} must be registered"
            );
        }
    }

    #[test]
    fn every_command_has_group_and_description() {
        for cmd in COMMANDS {
            assert!(!cmd.group.is_empty(), "{}: group", cmd.name);
            assert!(!cmd.description.is_empty(), "{}: description", cmd.name);
            assert!(
                !cmd.description.ends_with('.'),
                "{}: keep descriptions terse (no trailing period)",
                cmd.name
            );
        }
    }

    #[test]
    fn lookup_strips_slash() {
        assert_eq!(lookup("/help").unwrap().name, "help");
        assert_eq!(lookup("help").unwrap().name, "help");
        assert!(lookup("/nope").is_none());
    }

    #[test]
    fn suggestions_match_prefixes() {
        assert!(suggest_commands("a").contains(&"ask"));
        assert!(suggest_commands("auto").contains(&"auto"));
        assert!(suggest_commands("").len() <= 6);
        assert!(suggest_commands("zzz").is_empty());
    }

    #[test]
    fn arg_hints_available() {
        assert_eq!(suggest_args("model"), vec!["--test"]);
        assert!(suggest_args("help").is_empty());
        assert!(!suggest_args("brain").is_empty());
    }

    #[test]
    fn accepts_arg_validates() {
        assert!(accepts_arg("model", "--test"));
        assert!(!accepts_arg("model", "--nope"));
        assert!(!accepts_arg("help", "--anything"));
        // Free-form value commands accept anything.
        assert!(accepts_arg("brain", "how do routers work"));
        assert!(accepts_arg("resume", "1234-abcd"));
    }

    #[test]
    fn groups_are_ordered_and_unique() {
        let groups = list_commands_grouped();
        let mut names: Vec<&str> = groups.iter().map(|(g, _)| *g).collect();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "groups must be unique");
        assert!(count >= 5, "expected several groups, got {count}");
    }

    #[test]
    fn mode_commands_are_in_modes_group() {
        for name in ["ask", "plan", "code", "auto", "debug", "research"] {
            assert_eq!(lookup(name).unwrap().group, "Modes");
        }
    }

    #[test]
    fn takes_value_arg_detects_placeholders() {
        assert!(takes_value_arg("resume"));
        assert!(takes_value_arg("brain"));
        assert!(!takes_value_arg("model"));
        assert!(!takes_value_arg("exit"));
    }
}
