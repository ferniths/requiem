//! REQUIEM CLI entry point.
//!
//! Parses arguments and dispatches subcommands:
//!
//! * **default** (no subcommand) — boots the interactive session; on a
//!   fresh install (or a legacy config without the initialized flag) it
//!   auto-launches the setup wizard first.
//! * `session` — interactive multi-agent session (full-screen TUI on a
//!   TTY, plain event-printing loop when piped).
//! * `setup` — the configuration wizard.
//! * `model` — show or probe the configured model endpoint.
//! * `banner` / `help` — themed banner and slash-command reference.
//! * `install` / `uninstall` — put the real binary on PATH (and take it
//!   back off).

use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use std::process::ExitCode;
use uuid::Uuid;

use requiem_cli::config::Config;
use requiem_tui::theme::{theme_from_name, Theme};

type BoxResult = Result<String, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Parser)]
#[command(name = "requiem", version, about = "REQUIEM multi-agent CLI harness")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show the startup banner.
    Banner,
    /// Show or test the configured model.
    Model {
        /// Probe the endpoint with a real request.
        #[arg(long)]
        test: bool,
    },
    /// List the slash commands available inside a session.
    Help,
    /// Start an interactive multi-agent session.
    Session {
        /// Resume a previous session by ID.
        #[arg(long)]
        resume: Option<Uuid>,
    },
    /// Interactive configuration: endpoint, API key, model, agents, vaults.
    Setup,
    /// Copy this binary into a PATH-visible install location.
    Install {
        /// Target directory (default: %LOCALAPPDATA%\bin, or ~/.local/bin).
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
    /// Remove the installed `requiem` binary.
    Uninstall {
        /// Install directory to remove from (default: the standard one).
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Also delete the config directory (~/.requiem).
        #[arg(long, default_value_t = false)]
        purge: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    // Resolve the colour capability once, before anything prints.
    requiem_tui::init_color_mode();

    let cli = Cli::parse();
    let theme = configured_theme();

    match cli.command {
        None => run_default(&theme).await,
        Some(Commands::Banner) => {
            print_banner(&theme);
            print_version_line(&theme);
            ExitCode::SUCCESS
        }
        Some(Commands::Model { test }) => finish(run_model_command(test).await, &theme),
        Some(Commands::Help) => {
            print_banner(&theme);
            print_cli_usage(&theme);
            print_slash_help(&theme);
            ExitCode::SUCCESS
        }
        Some(Commands::Session { resume }) => {
            // The TUI renders its own welcome screen for fresh sessions and
            // restores the conversation for resumed ones. Skip the
            // pre-printed banner to avoid a flash-and-disappear artifact.
            finish(requiem_cli::session::run_session(resume).await, &theme)
        }
        Some(Commands::Setup) => finish(run_setup(&theme), &theme),
        Some(Commands::Install { dir }) => finish(run_install(dir, &theme), &theme),
        Some(Commands::Uninstall { dir, purge }) => {
            finish(run_uninstall(dir, purge, &theme), &theme)
        }
    }
}

/// Print an Ok/Err outcome as themed output with matching exit code.
fn finish(result: BoxResult, theme: &Theme) -> ExitCode {
    match result {
        Ok(msg) => {
            println!("{}", theme.success(&msg));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", theme.error(e.to_string()));
            ExitCode::FAILURE
        }
    }
}

// ── Default path ─────────────────────────────────────────────────────

/// The no-subcommand entry point: auto-launch the setup wizard on a
/// fresh install, then drop into an interactive session.
async fn run_default(theme: &Theme) -> ExitCode {
    let is_tty = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();

    match Config::load() {
        Ok(config) if config.is_initialized() => {
            if is_tty {
                // The TUI renders its own branded welcome screen; skip the
                // pre-printed banner so the user doesn't see it flash and
                // disappear when the alternate screen takes over.
                finish(requiem_cli::session::run_session(None).await, theme)
            } else {
                // Piped/redirected usage without a subcommand: help text,
                // not a surprise REPL hanging on stdin.
                print_banner(theme);
                print_cli_usage(theme);
                print_slash_help(theme);
                ExitCode::SUCCESS
            }
        }
        Ok(_) | Err(requiem_cli::config::ConfigError::NotInitialized) => {
            // Fresh install or legacy config: the wizard runs once, then
            // the config is marked initialized.
            if !is_tty {
                print_banner(theme);
                print_cli_usage(theme);
                eprintln!(
                    "{}",
                    theme.error("not configured yet — run `requiem setup` first")
                );
                return ExitCode::FAILURE;
            }
            print_banner(theme);
            println!(
                "{}",
                theme.info("▸ First launch detected — starting the setup wizard…")
            );
            match requiem_cli::setup::run_wizard() {
                Ok(_) => {
                    println!("{}", theme.success_bold("✓ setup complete"));
                    println!(
                        "{}",
                        theme.info("▸ launching session… (type /help for commands)")
                    );
                    finish(requiem_cli::session::run_session(None).await, theme)
                }
                Err(e) => {
                    eprintln!("{}", theme.error(format!("setup cancelled: {e}")));
                    eprintln!(
                        "{}",
                        theme.muted("run `requiem setup` any time to configure REQUIEM")
                    );
                    ExitCode::SUCCESS
                }
            }
        }
        Err(e) => {
            eprintln!("{}", theme.error(e.to_string()));
            ExitCode::FAILURE
        }
    }
}

// ── Subcommands ──────────────────────────────────────────────────────

/// `requiem model [--test]`.
async fn run_model_command(test: bool) -> BoxResult {
    let config = load_config_for_cli()?;
    let client = requiem_model::client::ModelClient::new(config.to_model_config())?;
    if test {
        let model = client.test_connection().await.map_err(|e| e.to_string())?;
        Ok(format!(
            "model reachable: {model} — endpoint {}",
            client.endpoint()
        ))
    } else {
        Ok(format!(
            "model: {} — endpoint {}",
            client.model(),
            client.endpoint()
        ))
    }
}

/// Any config that loads is usable for informational commands; a missing
/// config file is a clean, actionable error.
fn load_config_for_cli() -> Result<Config, String> {
    match Config::load() {
        Ok(c) => Ok(c),
        Err(requiem_cli::config::ConfigError::NotInitialized) => {
            Err("no configuration yet — run `requiem setup` first".into())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// `requiem setup`: the wizard, plus optional vault creation.
fn run_setup(theme: &Theme) -> BoxResult {
    let config = requiem_cli::setup::run_wizard()?;

    // Offer to materialize a configured vault that does not exist yet.
    if let Some(first_vault) = config.vaults.first() {
        let vault_path = std::path::PathBuf::from(first_vault);
        if !vault_path.is_dir() {
            println!(
                "{} vault directory {} does not exist — create it? [y/N]: ",
                theme.info("▸"),
                vault_path.display()
            );
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if answer.trim().eq_ignore_ascii_case("y") {
                std::fs::create_dir_all(&vault_path)?;
                println!("✓ vault created at {}", vault_path.display());
            }
        }
    }

    Ok("setup complete — run `requiem session` to start".into())
}

/// `requiem install`: copy the real binary into a PATH-visible dir and
/// verify the installed copy runs.
fn run_install(dir: Option<std::path::PathBuf>, theme: &Theme) -> BoxResult {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot resolve current executable path: {e}"))?;

    let target_dir = match dir {
        Some(d) => d,
        None => default_install_dir()?,
    };
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("cannot create {}: {e}", target_dir.display()))?;

    let target_name = if cfg!(windows) {
        "requiem.exe"
    } else {
        "requiem"
    };
    let target = target_dir.join(target_name);
    std::fs::copy(&exe, &target)
        .map_err(|e| format!("cannot copy to {}: {e}", target.display()))?;

    println!(
        "{}",
        theme.success(format!("✓ installed to {}", target.display()))
    );

    // Verify the installed copy actually runs — installing a broken or
    // wrong-architecture binary should fail loudly here.
    match std::process::Command::new(&target)
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout);
            let v = v.trim();
            if !v.is_empty() {
                println!(
                    "{}",
                    theme.muted(format!("  installed binary reports: {v}"))
                );
            }
        }
        Ok(out) => {
            return Err(format!(
                "installed binary exited with {} on `--version` — install is broken",
                out.status
            )
            .into());
        }
        Err(e) => {
            println!(
                "{}",
                theme.warning(format!(
                    "  warning: could not launch the installed binary to verify it ({e})"
                ))
            );
        }
    }

    if !is_dir_on_path(&target_dir) {
        println!(
            "{}",
            theme.warning(format!(
                "⚠ {} is not on PATH — add it, or run via the full path: {}",
                target_dir.display(),
                target.display()
            ))
        );
    }

    Ok("install complete".into())
}

/// The platform-standard install directory for the `requiem` binary.
fn default_install_dir() -> Result<std::path::PathBuf, String> {
    if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA")
            .map(|p| std::path::PathBuf::from(p).join("bin"))
            .map_err(|_| "cannot resolve %LOCALAPPDATA%; use --dir <path>".to_string())?;
        Ok(local)
    } else {
        let home = dirs::home_dir().ok_or_else(|| "cannot resolve home directory".to_string())?;
        Ok(home.join(".local").join("bin"))
    }
}

/// Whether `dir` resolves to an entry the shell will find on PATH.
fn is_dir_on_path(dir: &std::path::Path) -> bool {
    let path_env = match std::env::var("PATH") {
        Ok(p) => p,
        Err(_) => return false,
    };
    let dir_canonical = dir.canonicalize().ok();
    if dir_canonical.is_none() {
        return false;
    }
    std::env::split_paths(&path_env)
        .filter_map(|p| p.canonicalize().ok())
        .any(|p| Some(p) == dir_canonical)
}

/// `requiem uninstall`: remove the binary, optionally purge the config.
fn run_uninstall(dir: Option<std::path::PathBuf>, purge: bool, theme: &Theme) -> BoxResult {
    let install_dir = match dir {
        Some(d) => d,
        None => default_install_dir()?,
    };
    let target_name = if cfg!(windows) {
        "requiem.exe"
    } else {
        "requiem"
    };
    let target = install_dir.join(target_name);

    let mut removed = Vec::new();
    if target.is_file() {
        std::fs::remove_file(&target)
            .map_err(|e| format!("cannot remove {}: {e}", target.display()))?;
        removed.push(target.display().to_string());
    } else {
        // Not present at the standard location — report, don't fail (the
        // user may have installed elsewhere or via a package manager).
        println!(
            "{}",
            theme.muted(format!(
                "· {} not found — nothing to remove",
                target.display()
            ))
        );
    }

    if purge {
        let config_dir = dirs::home_dir().map(|h| h.join(".requiem"));
        if let Some(cfg_dir) = config_dir {
            if cfg_dir.is_dir() {
                std::fs::remove_dir_all(&cfg_dir)?;
                removed.push(cfg_dir.display().to_string());
            }
        }
    }

    let mut msg = "uninstalled requiem".to_string();
    if !removed.is_empty() {
        msg.push_str(&format!(" — removed {}", removed.join(", ")));
    }
    if !purge {
        msg.push_str("\n  config kept at ~/.requiem (add --purge to remove it)");
    }
    Ok(msg)
}

// ── Output helpers ───────────────────────────────────────────────────

/// The theme from the existing config, or the REQUIEM default.
fn configured_theme() -> Theme {
    match Config::load() {
        Ok(c) => theme_from_name(&c.theme),
        Err(_) => theme_from_name("default"),
    }
}

/// Print the compile-time banner, exactly themed.
fn print_banner(theme: &Theme) {
    println!("{}", requiem_tui::banner::render_themed(theme));
    println!();
}

/// One-line version + model summary under the banner.
fn print_version_line(theme: &Theme) {
    let version = env!("CARGO_PKG_VERSION");
    let model = Config::load()
        .map(|c| c.model)
        .unwrap_or_else(|_| "not configured".to_string());
    println!(
        "v{} · model: {}",
        theme.accent_bold(version),
        theme.secondary(model)
    );
}

/// CLI-level usage block.
fn print_cli_usage(theme: &Theme) {
    println!("{}", theme.accent_bold("usage:"));
    let lines = [
        (
            "requiem",
            "interactive session (auto-runs setup on first launch)",
        ),
        ("requiem session --resume <id>", "resume a previous session"),
        ("requiem setup", "configure endpoint, key, model, vaults"),
        (
            "requiem model [--test]",
            "show or probe the configured model",
        ),
        ("requiem banner | help | install | uninstall", "misc"),
    ];
    for (cmd, desc) in lines {
        println!(
            "  {} {}",
            theme.accent(format!("{cmd:<38}")),
            theme.secondary(desc)
        );
    }
    println!();
}

/// The slash-command reference available inside a session.
fn print_slash_help(theme: &Theme) {
    println!(
        "{}",
        theme.accent_bold("slash commands (inside a session):")
    );
    for (group, cmds) in requiem_cli::commands::list_commands_grouped() {
        println!("  {}", theme.violet(group));
        for (name, desc) in cmds {
            println!(
                "    {} {}",
                theme.secondary(format!("/{name:<10}")),
                theme.muted(desc)
            );
        }
    }
    println!(
        "  {}",
        theme.muted("(any other text is submitted as a task to the agent)")
    );
    println!();
}
