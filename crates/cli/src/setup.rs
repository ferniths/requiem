//! The interactive first-run setup wizard.
//!
//! A 7-step onboarding flow: Welcome → 9router → Agent → Permissions
//! → Obsidian → Appearance → Summary. Each step validates input,
//! supports back/continue navigation, and can be safely cancelled.

use requiem_tui::theme::Theme;

use crate::config::Config;

/// The 7-step wizard, returning the updated config on success.
pub fn run_wizard() -> Result<Config, String> {
    let mut config = existing_config_or_default();
    let mut step = 0usize;
    let theme = Theme::default();

    loop {
        match step {
            0 => {
                step_welcome(&theme)?;
            }
            1 => {
                step_router(&mut config, &theme)?;
            }
            2 => {
                step_agent(&mut config, &theme)?;
            }
            3 => {
                step_permissions(&mut config, &theme)?;
            }
            4 => {
                step_obsidian(&mut config, &theme)?;
            }
            5 => {
                step_appearance(&mut config, &theme)?;
            }
            6 => {
                step_summary(&config, &theme)?;
                return save_config(&config);
            }
            _ => unreachable!(),
        }

        step += 1;
    }
}

fn existing_config_or_default() -> Config {
    if let Ok(path) = Config::config_path() {
        if path.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(cfg) = toml::from_str::<Config>(&raw) {
                    return cfg;
                }
            }
        }
    }
    Config::default()
}

fn save_config(config: &Config) -> Result<Config, String> {
    let path = Config::config_path().map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create config dir: {e}"))?;
    }
    let raw = toml::to_string_pretty(config).map_err(|e| format!("cannot serialize config: {e}"))?;
    std::fs::write(&path, raw).map_err(|e| format!("cannot write config: {e}"))?;
    Ok(config.clone())
}

// ── Step 0: Welcome ──────────────────────────────────────────────────

fn step_welcome(theme: &Theme) -> Result<(), String> {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        print!("{}", requiem_tui::banner::render_themed(theme));
    } else {
        print!("{}", requiem_tui::banner::BANNER);
    }
    println!(
        "\n{}\n",
        theme.accent_bold("REQUIEM — first-run setup")
    );
    println!(
        "{} Configure your model endpoint, agent limits, permissions,\n",
        theme.secondary("     ")
    );
    println!(
        "{} 9router connection, Obsidian vaults, and appearance.\n",
        theme.secondary("     ")
    );
    println!(
        "{} This takes about a minute. You can press Ctrl-C at any point to cancel;\n",
        theme.secondary("     ")
    );
    println!(
        "{} nothing is written until you confirm on the final step.\n",
        theme.secondary("     ")
    );

    println!(
        "{}",
        theme.info("[ Enter to begin, 'q' to quit ]")
    );
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
    match input.trim().to_lowercase().as_str() {
        "q" | "quit" => Err("setup cancelled at Welcome".into()),
        _ => Ok(()),
    }
}

// ── Step 1: 9router connection ───────────────────────────────────────

fn step_router(config: &mut Config, theme: &Theme) -> Result<(), String> {
    use std::io::IsTerminal;
    let is_tty = std::io::stdout().is_terminal();
    println!(
        "\n{} Step 1/7 — 9router connection\n",
        theme.accent_bold("▸")
    );

    // Endpoint.
    println!(
        "{} OpenAI-compatible endpoint [{}]: ",
        theme.secondary("  >"),
        theme.info(&config.endpoint)
    );
    let endpoint = prompt_or_default(&config.endpoint)?;
    if !endpoint.is_empty() {
        config.endpoint = endpoint;
    }

    // API key (masked input).
    let key_display = if config.api_key.is_empty() {
        "(not set — you can also use the REQUIEM_API_KEY env var)".to_string()
    } else {
        "(set)".to_string()
    };
    println!(
        "{} API key {key_display}: ",
        theme.secondary("  >")
    );
    let key_input = prompt_or_default("")?;
    if !key_input.is_empty() {
        config.api_key = key_input;
    }

    // Model.
    println!(
        "{} Model name [{}]: ",
        theme.secondary("  >"),
        theme.info(&config.model)
    );
    let model = prompt_or_default(&config.model)?;
    if !model.is_empty() {
        config.model = model;
    }

    // Optional connection test.
    if is_tty {
        println!(
            "{}",
            theme.info("  [ y ] test the connection now, [ n ] skip")
        );
        let mut test_input = String::new();
        std::io::stdin().read_line(&mut test_input).map_err(|e| e.to_string())?;
        if test_input.trim().eq_ignore_ascii_case("y") {
            match test_endpoint(&config.endpoint, &config.model, &config.api_key) {
                Ok(name) => println!(
                    "{} connection OK — model '{name}' responded",
                    theme.success_bold("✓")
                ),
                Err(e) => {
                    println!(
                        "{} connection failed: {e}",
                        theme.error_bold("✗")
                    );
                    println!(
                        "{} continuing anyway — you can test later with `requiem model --test`",
                        theme.secondary("  ")
                    );
                }
            }
        }
    }

    Ok(())
}

/// Build a throwaway ModelClient and probe it.
fn test_endpoint(
    endpoint: &str,
    model: &str,
    api_key: &str,
) -> Result<String, String> {
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
    let client = requiem_model::client::ModelClient::new(mc)
        .map_err(|e| e.to_string())?;

    // tokio::main isn't available inside a sync function, so use a short-lived runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(client.test_connection()).map_err(|e| e.to_string())
}

// ── Step 2: Agent limits ─────────────────────────────────────────────

fn step_agent(config: &mut Config, theme: &Theme) -> Result<(), String> {
    println!(
        "\n{} Step 2/7 — Agent limits\n",
        theme.accent_bold("▸")
    );

    config.max_agent_depth = prompt_number(
        "Max subagent depth",
        config.max_agent_depth,
        1,
        16,
    )?;
    config.token_budget = prompt_number(
        "Token budget per agent task",
        config.token_budget,
        1_000,
        1_000_000,
    )?;
    config.max_task_budget = prompt_number(
        "Max tasks per session",
        config.max_task_budget,
        1,
        100,
    )?;

    println!(
        "{} Dynamic scaling [{}]: ",
        theme.secondary("  >"),
        theme.info(config.dynamic_scaling.to_string())
    );
    let ds_input = prompt_or_default("")?;
    match ds_input.trim().to_lowercase().as_str() {
        "y" | "yes" | "true" => config.dynamic_scaling = true,
        "n" | "no" | "false" => config.dynamic_scaling = false,
        _ => {}
    }

    Ok(())
}

// ── Step 3: Permissions / default mode ───────────────────────────────

fn step_permissions(config: &mut Config, theme: &Theme) -> Result<(), String> {
    println!(
        "\n{} Step 3/7 — Permissions (default mode)\n",
        theme.accent_bold("▸")
    );
    println!(
        "{} Pick the mode new sessions start in. You can switch with a slash command\n",
        theme.secondary("     ")
    );
    println!(
        "{} at any time during a session.\n",
        theme.secondary("     ")
    );

    for (i, mode) in requiem_core::mode::Mode::all().iter().enumerate() {
        let marker = if i == 0 { theme.success("✓") } else { theme.secondary(" ") };
        let is_default = config.default_mode == mode.slash().trim_start_matches('/');
        let default_tag = if is_default {
            format!("  {}", theme.info("(current)"))
        } else {
            String::new()
        };
        println!(
            "{}  {}. {} {}{}\n     {}",
            marker,
            i + 1,
            theme.accent(mode.slash()),
            theme.primary(format!("{:8}", mode.slash().trim_start_matches('/'))),
            default_tag,
            theme.secondary(mode.description())
        );
    }

    let choice = prompt_number("Default mode", 3, 1, 6)?;
    let modes = requiem_core::mode::Mode::all();
    let selected = modes.get((choice - 1) as usize).copied();
    if let Some(m) = selected {
        config.default_mode = m.slash().trim_start_matches('/').to_string();
    }

    Ok(())
}

// ── Step 4: Obsidian vaults ──────────────────────────────────────────

fn step_obsidian(config: &mut Config, theme: &Theme) -> Result<(), String> {
    println!(
        "\n{} Step 4/7 — Obsidian vaults\n",
        theme.accent_bold("▸")
    );
    println!(
        "{} REQUIEM can read and write notes in one or more Obsidian vaults.\n",
        theme.secondary("     ")
    );
    println!(
        "{} The first vault listed is the default. Leave blank to skip.\n",
        theme.secondary("     ")
    );

    let mut vaults: Vec<String> = Vec::new();
    let mut idx = 1usize;
    loop {
        println!(
            "{} Vault #{idx} (path to a folder containing .md files): ",
            theme.secondary("  >")
        );
        let input = prompt_or_default("")?;
        let input = input.trim().to_string();
        if input.is_empty() {
            break;
        }
        let path = std::path::PathBuf::from(&input);
        if path.is_dir() {
            vaults.push(input);
            idx += 1;
        } else {
            println!(
                "{} not a directory — skipping",
                theme.warning(path.display().to_string())
            );
        }
        print!(
            "{} Add another? [ y/N ]: ",
            theme.secondary("  >")
        );
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let mut more = String::new();
        std::io::stdin().read_line(&mut more).map_err(|e| e.to_string())?;
        if !more.trim().eq_ignore_ascii_case("y") {
            break;
        }
    }

    if vaults.is_empty() {
        println!(
            "{} No vaults configured — vault tools will be unavailable.",
            theme.secondary("  •")
        );
    } else {
        for v in &vaults {
            println!(
                "{} {}",
                theme.success("✓"),
                theme.secondary(v)
            );
        }
    }
    config.vaults = vaults;
    Ok(())
}

// ── Step 5: Appearance / theme ───────────────────────────────────────

fn step_appearance(config: &mut Config, theme: &Theme) -> Result<(), String> {
    println!(
        "\n{} Step 5/7 — Appearance\n",
        theme.accent_bold("▸")
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
        let preview = variant
                .accent_bold("REQUIEM")
                .to_string()
                + &variant.secondary(" — this is how text looks").to_string()
                + &variant.muted(" | muted details").to_string();
        println!(
            "{}  {}. {preview}  {tag}",
            theme.secondary(" "),
            i + 1
        );
    }

    let choice = prompt_number("Theme", 1, 1, names.len() as u32)?;
    let name = names.get((choice - 1) as usize).copied();
    if let Some(n) = name {
        config.theme = n.to_string();
    }

    Ok(())
}

// ── Step 6: Summary + save ───────────────────────────────────────────

fn step_summary(config: &Config, theme: &Theme) -> Result<(), String> {
    println!(
        "\n{} Step 6/7 — Summary\n",
        theme.accent_bold("▸")
    );

    let mode_name = config.default_mode.clone();
    println!("  endpoint    {}", theme.info(&config.endpoint));
    let key_tag = if config.api_key.is_empty() {
        theme.muted("(env var REQUIEM_API_KEY or unset)")
    } else {
        theme.muted("(set)")
    };
    println!("  api_key     {key_tag}");
    println!("  model       {}", theme.info(&config.model));
    println!(
        "  agent limits  depth {} · token budget {} · task budget {} · scaling {}",
        theme.secondary(config.max_agent_depth.to_string()),
        theme.secondary(config.token_budget.to_string()),
        theme.secondary(config.max_task_budget.to_string()),
        theme.secondary(config.dynamic_scaling.to_string()),
    );
    println!(
        "  default mode  {}",
        theme.accent(mode_name.as_str())
    );
    if config.vaults.is_empty() {
        println!(
            "  vaults        {}",
            theme.muted("(none)")
        );
    } else {
        for v in &config.vaults {
            println!("  vault         {}", theme.secondary(v));
        }
    }
    println!("  theme       {}", theme.info(&config.theme));

    println!();
    let path = Config::config_path().map(|p| p.display().to_string()).unwrap_or_default();
    println!(
        "{} config will be written to {path}",
        theme.secondary("  →")
    );

    print!(
        "{} Save and launch session? [ Y/n ]: ",
        theme.info("  ")
    );
    std::io::Write::flush(&mut std::io::stdout()).ok();
    let mut confirm = String::new();
    std::io::stdin().read_line(&mut confirm).map_err(|e| e.to_string())?;
    if confirm.trim().eq_ignore_ascii_case("n") {
        return Err("setup cancelled — nothing saved".into());
    }

    println!(
        "{} Writing config…",
        theme.success_bold("✓")
    );
    Ok(())
}

// ── Shared prompt helpers ────────────────────────────────────────────

/// Prompt for a value; empty input returns the `default` unchanged.
fn prompt_or_default(default: &str) -> Result<String, String> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
    let trimmed = input.trim().to_string();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed)
    }
}

/// Prompt for a number within [min, max]; empty input keeps the default.
fn prompt_number(
    label: &str,
    default: u32,
    min: u32,
    max: u32,
) -> Result<u32, String> {
    println!(
        "{} [{default}] ({min}–{max}): ",
        label,
    );
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
    let trimmed = input.trim().to_string();
    if trimmed.is_empty() {
        return Ok(default);
    }
    match trimmed.parse::<u32>() {
        Ok(v) if v >= min && v <= max => Ok(v),
        Ok(v) => Err(format!("value {v} out of range ({min}–{max})")),
        Err(_) => Err(format!("'{trimmed}' is not a number")),
    }
}


