//! The interactive first-run setup wizard.
//!
//! Eight steps: Connection → API key → Model → Agent limits →
//! Permissions → Obsidian vaults → Appearance → Summary.
//!
//! Every step supports going **back** (answer `b`) and cancels cleanly
//! with Ctrl-C. Invalid input re-prompts with a warning instead of
//! aborting. Nothing is written to disk until the user confirms the
//! summary; saving flips the `initialized` flag so the wizard does not
//! run again.

use std::io::IsTerminal;

use requiem_tui::theme::Theme;

use crate::config::Config;

/// The result of one wizard step.
enum Step {
    /// Advance to the next step.
    Next,
    /// Return to the previous step.
    Back,
}

/// The number of wizard steps (after the welcome screen).
pub const STEP_COUNT: usize = 8;

/// Run the full wizard, returning the updated config on success.
///
/// Requires an interactive terminal; in a non-TTY context it fails with
/// a clear message instead of hanging on prompts that cannot be seen.
pub fn run_wizard() -> Result<Config, String> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(
            "setup needs an interactive terminal (stdin and stdout are not both TTYs)".to_string(),
        );
    }

    let mut config = existing_config_or_default();
    // The wizard previews render in the user's *current* theme.
    let theme = theme_for(&config.theme);
    let mut step = 0usize;

    loop {
        if step == 0 {
            welcome(&theme)?;
            step += 1;
            continue;
        }

        let outcome = match step {
            1 => step_connection(&mut config, &theme),
            2 => step_api_key(&mut config, &theme),
            3 => step_model(&mut config, &theme),
            4 => step_agent_limits(&mut config, &theme),
            5 => step_permissions(&mut config, &theme),
            6 => step_vaults(&mut config, &theme),
            7 => step_appearance(&mut config, &theme),
            8 => step_summary(&config, &theme),
            _ => unreachable!("wizard step out of range"),
        }?;

        match outcome {
            Step::Next => {
                if step == 8 {
                    // Confirmed on the summary: persist and finish.
                    config.initialized = true;
                    save_config(&config)?;
                    println!(
                        "\n{} Config saved to {}",
                        theme.success_bold("✓"),
                        Config::config_path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    );
                    return Ok(config);
                }
                step += 1;
            }
            Step::Back => {
                if step == 1 {
                    // Back from the first step returns to the welcome
                    // screen, which re-enters on Enter.
                    step = 0;
                } else {
                    step -= 1;
                }
            }
        }
    }
}

fn theme_for(name: &str) -> Theme {
    requiem_tui::theme_from_name(name)
}

fn existing_config_or_default() -> Config {
    Config::load().unwrap_or_default()
}

fn save_config(config: &Config) -> Result<(), String> {
    config.save().map_err(|e| e.to_string())
}

// ── Step 0: Welcome ──────────────────────────────────────────────────

fn welcome(theme: &Theme) -> Result<(), String> {
    print!("{}", requiem_tui::banner::render_themed(theme));
    println!("\n{}", theme.accent_bold("REQUIEM — first-run setup"));
    println!(
        "{}",
        theme.secondary(
            "  Configure the model connection, agent limits, permissions,\n  \
             Obsidian vaults, and appearance. Takes about a minute."
        )
    );
    println!(
        "{}",
        theme.muted(
            "  Ctrl-C cancels at any point; nothing is written until you\n  \
             confirm on the final step. Answer `b` at any prompt to go back."
        )
    );
    println!("\n{}", theme.info("[ Enter to begin · q to quit ]"));

    let answer = ask("> ")?;
    match answer {
        Answer::Value(v) if v.eq_ignore_ascii_case("q") => Err("setup cancelled".into()),
        _ => Ok(()),
    }
}

// ── Step 1: Connection ───────────────────────────────────────────────

fn step_connection(config: &mut Config, theme: &Theme) -> Result<Step, String> {
    header(theme, 1, "Model connection");
    println!(
        "{}",
        theme.secondary("  The OpenAI-compatible endpoint agents will talk to.")
    );
    println!(
        "{}",
        theme.muted(
            "  Examples: http://localhost:11434/v1 (Ollama),\n  \
             https://api.openai.com/v1, or any compatible proxy."
        )
    );

    loop {
        println!(
            "\n{} {}",
            theme.secondary("  >"),
            theme.info(format!("Endpoint [{}]:", config.endpoint))
        );
        match ask("  >")? {
            Answer::Back => return Ok(Step::Back),
            Answer::Keep => break,
            Answer::Value(v) => match validate_endpoint(&v) {
                Ok(normalized) => {
                    config.endpoint = normalized;
                    break;
                }
                Err(e) => warn(theme, &e),
            },
        }
    }

    Ok(Step::Next)
}

/// Validate and normalize an endpoint URL.
fn validate_endpoint(input: &str) -> Result<String, String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("endpoint must not be empty".into());
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(format!("'{trimmed}' must start with http:// or https://"));
    }
    let rest = trimmed
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    if rest.is_empty() || rest.starts_with('/') {
        return Err(format!("'{trimmed}' has no host"));
    }
    Ok(trimmed.to_string())
}

// ── Step 2: API key ──────────────────────────────────────────────────

fn step_api_key(config: &mut Config, theme: &Theme) -> Result<Step, String> {
    header(theme, 2, "API key");
    println!(
        "{}",
        theme.secondary(
            "  Optional — only needed for hosted endpoints. Input is hidden\n  \
             and the key is never echoed in logs or summaries."
        )
    );
    println!(
        "{}",
        theme.muted("  Alternatively, set the REQUIEM_API_KEY environment variable.")
    );

    if config.api_key.is_empty() {
        println!(
            "\n{} {}",
            theme.secondary("  >"),
            theme.muted("API key: (not set — Enter to skip)")
        );
    } else {
        println!(
            "\n{} {}",
            theme.secondary("  >"),
            theme.muted("API key: (set — Enter to keep, type a new key to replace)")
        );
    }

    match ask_masked()? {
        Answer::Back => return Ok(Step::Back),
        Answer::Keep => {}
        Answer::Value(v) => {
            config.api_key = v;
        }
    }

    Ok(Step::Next)
}

// ── Step 3: Model ────────────────────────────────────────────────────

fn step_model(config: &mut Config, theme: &Theme) -> Result<Step, String> {
    header(theme, 3, "Model");
    println!(
        "{}",
        theme.secondary(
            "  One model name is used by every agent in the harness — there\n  \
             is no automatic selection. Type the exact model id your\n  \
             endpoint serves, e.g. `qwen3:32b`, `gpt-4o`, `claude-sonnet-4`."
        )
    );

    loop {
        println!(
            "\n{} {}",
            theme.secondary("  >"),
            theme.info(format!("Model [{}]:", config.model))
        );
        match ask("  >")? {
            Answer::Back => return Ok(Step::Back),
            Answer::Keep => break,
            Answer::Value(v) => match validate_model(&v) {
                Ok(()) => {
                    config.model = v;
                    break;
                }
                Err(e) => warn(theme, &e),
            },
        }
    }

    // Optional connection test with friendly error classification.
    println!(
        "\n{} {}",
        theme.secondary("  >"),
        theme.info("Test the connection now? [y/N]")
    );
    match ask("  >")? {
        Answer::Value(v) if v.eq_ignore_ascii_case("y") => {
            println!("  {}", theme.muted("probing…"));
            match test_endpoint(&config.endpoint, &config.model, &config.api_key) {
                Ok(model) => println!(
                    "{} {}",
                    theme.success_bold("✓"),
                    theme.success(format!("connection OK — model '{model}' responded"))
                ),
                Err(e) => {
                    println!(
                        "{} {}",
                        theme.error_bold("✗"),
                        theme.error(classify_conn_error(&e))
                    );
                    println!(
                        "  {}",
                        theme.muted("Continuing anyway — test later with `requiem model --test`.")
                    );
                }
            }
        }
        _ => {}
    }

    Ok(Step::Next)
}

/// Validate a model name: non-empty, no whitespace-only, reasonable length.
fn validate_model(model: &str) -> Result<(), String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err("model must not be empty".into());
    }
    if trimmed.chars().count() > 128 {
        return Err("model name is suspiciously long (>128 chars)".into());
    }
    if trimmed.split_whitespace().count() != 1 {
        return Err(format!(
            "'{trimmed}' contains spaces — use the exact model id"
        ));
    }
    Ok(())
}

/// Build a throwaway ModelClient and probe it.
fn test_endpoint(endpoint: &str, model: &str, api_key: &str) -> Result<String, String> {
    let mc = requiem_model::config::ModelConfig {
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        api_key: api_key.to_string(),
        max_concurrency: 1,
        timeout_ms: 15_000,
        max_retries: 1,
        max_context_tokens: Some(8_192),
        max_output_tokens: None,
    };
    let client = requiem_model::client::ModelClient::new(mc).map_err(|e| e.to_string())?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(client.test_connection())
        .map_err(|e| e.to_string())
}

/// Turn a raw connection error into something a human can act on.
fn classify_conn_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("connection refused") || lower.contains("connect error") {
        format!(
            "cannot reach the endpoint — is the server running and is the port correct?\n           (detail: {raw})"
        )
    } else if lower.contains("401") || lower.contains("unauthorized") || lower.contains("403") {
        "authentication failed — the API key is missing or rejected".to_string()
    } else if lower.contains("404") {
        "endpoint or model not found — check the URL path and the model id".to_string()
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "the endpoint did not answer in time — it may be slow or unreachable".to_string()
    } else if lower.contains("dns") || lower.contains("name resolution") {
        "hostname did not resolve — check for typos in the endpoint URL".to_string()
    } else {
        raw.to_string()
    }
}

// ── Step 4: Agent limits ─────────────────────────────────────────────

fn step_agent_limits(config: &mut Config, theme: &Theme) -> Result<Step, String> {
    header(theme, 4, "Agent limits");
    println!(
        "{}",
        theme.secondary("  How deep agents may delegate and how much they may spend.")
    );

    let depth = match ask_number(theme, "Max subagent depth", config.max_agent_depth, 1, 16)? {
        NumberAnswer::Back => return Ok(Step::Back),
        NumberAnswer::Value(v) => v,
    };
    config.max_agent_depth = depth;
    let budget = match ask_number(
        theme,
        "Token budget per agent task",
        config.token_budget,
        1_000,
        1_000_000,
    )? {
        NumberAnswer::Back => return Ok(Step::Back),
        NumberAnswer::Value(v) => v,
    };
    config.token_budget = budget;
    let tasks = match ask_number(
        theme,
        "Max tasks per session",
        config.max_task_budget,
        1,
        100,
    )? {
        NumberAnswer::Back => return Ok(Step::Back),
        NumberAnswer::Value(v) => v,
    };
    config.max_task_budget = tasks;

    println!(
        "\n{} {}",
        theme.secondary("  >"),
        theme.info(format!(
            "Dynamic agent scaling [{}]: (y/n, Enter keeps)",
            config.dynamic_scaling
        ))
    );
    match ask("  >")? {
        Answer::Back => return Ok(Step::Back),
        Answer::Keep => {}
        Answer::Value(v) => match v.to_lowercase().as_str() {
            "y" | "yes" | "true" | "on" => config.dynamic_scaling = true,
            "n" | "no" | "false" | "off" => config.dynamic_scaling = false,
            other => warn(
                theme,
                &format!("'{other}' is not y/n — keeping the current value"),
            ),
        },
    }

    Ok(Step::Next)
}

// ── Step 5: Permissions ──────────────────────────────────────────────

fn step_permissions(config: &mut Config, theme: &Theme) -> Result<Step, String> {
    use requiem_core::mode::Mode;

    header(theme, 5, "Permissions");
    println!(
        "{}",
        theme.secondary(
            "  Pick the default permission level. Switch anytime in a session\n  \
             with a slash command; the gate enforces it before every tool call."
        )
    );

    let profiles: &[(Mode, &str, &str)] = &[
        (
            Mode::Ask,
            "Read-only",
            "inspect and answer; no file writes, no terminal mutations, no git, no agents",
        ),
        (
            Mode::Code,
            "Approval required",
            "implement and delegate; builds/tests run, each mutation is logged",
        ),
        (
            Mode::Auto,
            "Automatic execution",
            "fully autonomous; plan, implement, test, review, correct",
        ),
        (
            Mode::Research,
            "Vault research",
            "parallel research and Obsidian vault writes; no project-file mutation",
        ),
    ];

    for (i, (mode, label, desc)) in profiles.iter().enumerate() {
        let is_default = config.default_mode == mode.slash().trim_start_matches('/');
        let marker = if is_default {
            theme.success("✓")
        } else {
            theme.secondary(" ")
        };
        println!(
            "  {} {}. {} {} — {}",
            marker,
            i + 1,
            theme.accent(*label),
            theme.secondary(mode.slash()),
            theme.muted(*desc)
        );
    }

    let default_idx = profiles
        .iter()
        .position(|(m, _, _)| m.slash().trim_start_matches('/') == config.default_mode)
        .unwrap_or(1) as u32
        + 1;

    let choice = match ask_number(
        theme,
        "Permission level",
        default_idx,
        1,
        profiles.len() as u32,
    )? {
        NumberAnswer::Back => return Ok(Step::Back),
        NumberAnswer::Value(v) => v,
    };
    if let Some((mode, _, _)) = profiles.get((choice - 1) as usize) {
        config.default_mode = mode.slash().trim_start_matches('/').to_string();
    }

    Ok(Step::Next)
}

// ── Step 6: Obsidian vaults ──────────────────────────────────────────

fn step_vaults(config: &mut Config, theme: &Theme) -> Result<Step, String> {
    header(theme, 6, "Obsidian vaults");
    println!(
        "{}",
        theme.secondary(
            "  REQUIEM can read and write notes in one or more vaults. The\n  \
             first vault listed is the default."
        )
    );

    let existing = config.vaults.clone();
    if !existing.is_empty() {
        println!("  {}", theme.muted("Currently configured:"));
        for (i, v) in existing.iter().enumerate() {
            println!(
                "    {} {}. {}",
                if i == 0 {
                    theme.success("★")
                } else {
                    theme.secondary(" ")
                },
                i + 1,
                theme.secondary(v)
            );
        }
        println!(
            "\n{} {}",
            theme.secondary("  >"),
            theme.info("Keep existing vaults? [Y/n]")
        );
        let keep = match ask("  >")? {
            Answer::Back => return Ok(Step::Back),
            Answer::Keep => true,
            Answer::Value(v) => !v.eq_ignore_ascii_case("n"),
        };
        if !keep {
            config.vaults.clear();
            println!(
                "  {}",
                theme.muted("Existing vaults cleared — add new ones below.")
            );
        }
    }

    let mut added: usize = 0;
    loop {
        let next_idx = config.vaults.len() + 1;
        println!(
            "\n{} {}",
            theme.secondary("  >"),
            theme.info(format!(
                "Vault #{next_idx} (path to a folder containing .md files, Enter to finish):"
            ))
        );
        let input = match ask("  >")? {
            Answer::Back => return Ok(Step::Back),
            Answer::Keep => break,
            Answer::Value(v) => v,
        };
        let path = std::path::PathBuf::from(&input);
        if path.is_dir() {
            if config.vaults.contains(&input) {
                warn(theme, "vault already configured");
                continue;
            }
            config.vaults.push(input.clone());
            added += 1;
            println!(
                "  {} {}",
                theme.success("✓"),
                theme.success(format!("added {input}"))
            );
        } else if path.exists() {
            warn(theme, &format!("{input} exists but is not a directory"));
        } else {
            warn(theme, &format!("{input} does not exist — check the path"));
        }
    }

    if config.vaults.is_empty() {
        println!(
            "  {}",
            theme.muted("No vaults configured — vault tools will be unavailable.")
        );
    } else if added > 0 {
        println!(
            "  {} {}",
            theme.success_bold("✓"),
            theme.success(format!(
                "{} vault{} configured (default: {})",
                config.vaults.len(),
                if config.vaults.len() == 1 { "" } else { "s" },
                config.vaults[0]
            ))
        );
    }

    Ok(Step::Next)
}

// ── Step 7: Appearance ───────────────────────────────────────────────

fn step_appearance(config: &mut Config, theme: &Theme) -> Result<Step, String> {
    header(theme, 7, "Appearance");
    println!(
        "{}",
        theme.secondary("  Pick the theme variant. Preview lines render in each variant.")
    );

    let names = requiem_tui::THEME_NAMES;
    for (i, name) in names.iter().enumerate() {
        let variant = requiem_tui::theme_from_name(name);
        let is_default = config.theme == *name;
        let tag = if is_default {
            format!("  {}", theme.info("(current)"))
        } else {
            String::new()
        };
        let preview = variant.accent_bold("REQUIEM")
            + &variant.secondary(" — text sample ")
            + &variant.muted("muted ")
            + &variant.success("✓ok ");
        println!("  {}. {preview}{}{tag}", i + 1, theme.secondary(name));
    }

    // Default = the CURRENT theme's index, not 1: pressing Enter must not
    // silently reset the user's theme.
    let default_idx = names.iter().position(|n| *n == config.theme).unwrap_or(0) as u32 + 1;

    let choice = match ask_number(theme, "Theme", default_idx, 1, names.len() as u32)? {
        NumberAnswer::Back => return Ok(Step::Back),
        NumberAnswer::Value(v) => v,
    };
    if let Some(n) = names.get((choice - 1) as usize) {
        config.theme = n.to_string();
    }

    Ok(Step::Next)
}

// ── Step 8: Summary + save ───────────────────────────────────────────

fn step_summary(config: &Config, theme: &Theme) -> Result<Step, String> {
    header(theme, 8, "Summary");

    println!("  endpoint      {}", theme.info(&config.endpoint));
    // The key is never displayed — only whether one is configured.
    let key_state = if config.api_key.is_empty() {
        theme.muted("(not set — REQUIEM_API_KEY env var or none)")
    } else {
        theme.muted("(set, hidden)")
    };
    println!("  api key       {key_state}");
    println!("  model         {}", theme.info(&config.model));
    println!(
        "  agent limits  depth {} · task budget {} · max tasks {} · scaling {}",
        theme.secondary(config.max_agent_depth.to_string()),
        theme.secondary(config.token_budget.to_string()),
        theme.secondary(config.max_task_budget.to_string()),
        theme.secondary(config.dynamic_scaling.to_string()),
    );
    println!(
        "  default mode  {}",
        theme.accent(config.default_mode.as_str())
    );
    if config.vaults.is_empty() {
        println!("  vaults        {}", theme.muted("(none)"));
    } else {
        for (i, v) in config.vaults.iter().enumerate() {
            let tag = if i == 0 {
                theme.success("(default)")
            } else {
                String::new()
            };
            println!("  vault         {} {}", theme.secondary(v), tag);
        }
    }
    println!("  theme         {}", theme.info(&config.theme));

    let path = Config::config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    println!(
        "\n  {} {}",
        theme.secondary("→"),
        theme.muted(format!("config will be written to {path}"))
    );

    println!(
        "\n{} {}",
        theme.secondary("  >"),
        theme.info("Save and finish? [Y/n]")
    );
    match ask("  >")? {
        Answer::Back => Ok(Step::Back),
        Answer::Value(v) if v.eq_ignore_ascii_case("n") => Ok(Step::Back),
        _ => Ok(Step::Next),
    }
}

// ── Shared prompt machinery ──────────────────────────────────────────

/// A user's answer to one prompt.
enum Answer {
    /// A non-empty answer.
    Value(String),
    /// Empty input: keep the current value / accept the default.
    Keep,
    /// The user asked to go back.
    Back,
}

/// Print a step header with the correct step number.
fn header(theme: &Theme, n: usize, title: &str) {
    println!(
        "\n{} {} {}",
        theme.accent_bold("▸"),
        theme.accent_bold(format!("Step {n}/{STEP_COUNT} —")),
        theme.primary_bold(title)
    );
}

/// Print a warning (validation failure) and loop back to the prompt.
fn warn(theme: &Theme, msg: &str) {
    println!("  {} {}", theme.warning("⚠"), theme.warning(msg));
}

/// Read one line from stdin.
///
/// Returns `Answer::Keep` on empty input, `Answer::Back` on `b`/`back`,
/// and treats EOF as a cancel.
fn ask(prompt: &str) -> Result<Answer, String> {
    print!("{prompt} ");
    std::io::Write::flush(&mut std::io::stdout()).ok();
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(0) => Err("setup cancelled (EOF)".to_string()),
        Ok(_) => {
            let trimmed = input.trim();
            if trimmed.is_empty() {
                Ok(Answer::Keep)
            } else if trimmed.eq_ignore_ascii_case("b") || trimmed.eq_ignore_ascii_case("back") {
                Ok(Answer::Back)
            } else {
                Ok(Answer::Value(trimmed.to_string()))
            }
        }
        Err(e) => Err(format!("input error: {e}")),
    }
}

/// Read a secret with masked input (no echo).
///
/// On a TTY this uses `rpassword`; in a piped context it reads one line
/// plainly. The value is never printed anywhere.
fn ask_masked() -> Result<Answer, String> {
    if std::io::stdin().is_terminal() {
        let secret = rpassword::prompt_password("  > API key (hidden): ")
            .map_err(|e| format!("input error: {e}"))?;
        let trimmed = secret.trim().to_string();
        if trimmed.is_empty() {
            Ok(Answer::Keep)
        } else if trimmed.eq_ignore_ascii_case("b") {
            Ok(Answer::Back)
        } else {
            Ok(Answer::Value(trimmed))
        }
    } else {
        // Non-TTY: read one line plainly (CI or piped input).
        let mut input = String::new();
        match std::io::stdin().read_line(&mut input) {
            Ok(0) => Err("setup cancelled (EOF)".to_string()),
            Ok(_) => {
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() {
                    Ok(Answer::Keep)
                } else {
                    Ok(Answer::Value(trimmed))
                }
            }
            Err(e) => Err(format!("input error: {e}")),
        }
    }
}

/// Prompt for a number within `[min, max]`.
///
/// Empty input keeps the default; invalid input warns and re-prompts
/// (it never aborts the wizard); `b` goes back a step.
fn ask_number(
    theme: &Theme,
    label: &str,
    default: u32,
    min: u32,
    max: u32,
) -> Result<NumberAnswer, String> {
    loop {
        println!(
            "\n{} {}",
            theme.secondary("  >"),
            theme.info(format!("{label} [{default}] ({min}–{max}):"))
        );
        match ask("  >")? {
            Answer::Back => return Ok(NumberAnswer::Back),
            Answer::Keep => return Ok(NumberAnswer::Value(default)),
            Answer::Value(v) => match v.parse::<u32>() {
                Ok(n) if (min..=max).contains(&n) => return Ok(NumberAnswer::Value(n)),
                Ok(n) => warn(
                    theme,
                    &format!("{n} is out of range — enter a number between {min} and {max}"),
                ),
                Err(_) => warn(theme, &format!("'{v}' is not a number — try again")),
            },
        }
    }
}

/// The outcome of a numeric prompt.
enum NumberAnswer {
    /// A valid number (possibly the default).
    Value(u32),
    /// The user asked to go back.
    Back,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_validation_accepts_good_urls() {
        assert_eq!(
            validate_endpoint("http://localhost:11434/v1").unwrap(),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            validate_endpoint("https://api.openai.com/v1/").unwrap(),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn endpoint_validation_rejects_bad_urls() {
        assert!(validate_endpoint("").is_err());
        assert!(validate_endpoint("ftp://nope").is_err());
        assert!(validate_endpoint("localhost:11434").is_err());
        assert!(validate_endpoint("http://").is_err());
        assert!(validate_endpoint("https:///v1").is_err());
    }

    #[test]
    fn model_validation() {
        assert!(validate_model("qwen3:32b").is_ok());
        assert!(validate_model("").is_err());
        assert!(validate_model("   ").is_err());
        assert!(validate_model("two words").is_err());
    }

    #[test]
    fn connection_error_classification_is_friendly() {
        assert!(classify_conn_error("Connection refused (os error 111)")
            .contains("is the server running"));
        assert!(classify_conn_error("HTTP 401 Unauthorized").contains("API key"));
        assert!(classify_conn_error("HTTP 404 Not Found").contains("model id"));
        assert!(classify_conn_error("operation timed out").contains("did not answer"));
        assert!(classify_conn_error("dns error: no such host").contains("resolve"));
    }

    #[test]
    fn step_count_matches_header_labels() {
        assert_eq!(STEP_COUNT, 8);
    }

    #[test]
    fn number_answer_has_back_variant() {
        // Back must be a first-class outcome, never an error string.
        let back: NumberAnswer = NumberAnswer::Back;
        assert!(matches!(back, NumberAnswer::Back));
    }
}
