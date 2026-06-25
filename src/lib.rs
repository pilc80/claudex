// The upstream single-binary modules are shared by two thin binaries in this fork.
// Some internal helpers are intentionally dormant for optional providers/features.
#![allow(dead_code)]

mod cli;
mod config;
mod context;
mod oauth;
mod process;
mod proxy;
mod router;
mod sets;
mod terminal;
mod update;
mod util;

use anyhow::{Context, Result};
use clap::Parser;
use std::ffi::OsStr;
use std::io::IsTerminal;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use cli::{AuthAction, Cli, Commands, ProfileAction, ProxyAction, SetsAction};
use config::{ClaudexConfig, HyperlinksConfig};

const SHIM_MODE_ENV: &str = "CLAUDEX_EXECUTABLE_MODE";
const SHIM_BYPASS_ENV: &str = "CLAUDEX_SHIM_BYPASS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutableMode {
    Launcher,
    Config,
}

pub async fn run_from_argv0() -> Result<()> {
    if run_windows_shim_if_needed()? {
        return Ok(());
    }

    run_with_mode(executable_mode_from_env_or_arg0(
        std::env::args_os()
            .next()
            .as_deref()
            .unwrap_or_else(|| OsStr::new("claudex")),
    ))
    .await
}

pub async fn run_launcher_binary() -> Result<()> {
    run_with_mode(ExecutableMode::Launcher).await
}

pub async fn run_config_binary() -> Result<()> {
    run_with_mode(ExecutableMode::Config).await
}

async fn run_with_mode(mode: ExecutableMode) -> Result<()> {
    match mode {
        ExecutableMode::Launcher => run_launcher().await,
        ExecutableMode::Config => run_config_cli().await,
    }
}

fn executable_mode_from_env_or_arg0(arg0: impl AsRef<OsStr>) -> ExecutableMode {
    match std::env::var(SHIM_MODE_ENV).ok().as_deref() {
        Some("launcher") => return ExecutableMode::Launcher,
        Some("config") => return ExecutableMode::Config,
        _ => {}
    }

    executable_mode_from_arg0(arg0)
}

fn executable_mode_from_arg0(arg0: impl AsRef<OsStr>) -> ExecutableMode {
    let path = Path::new(arg0.as_ref());
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("claudex");

    if name == "claudex-config" {
        ExecutableMode::Config
    } else {
        ExecutableMode::Launcher
    }
}

fn run_windows_shim_if_needed() -> Result<bool> {
    if !cfg!(windows) || std::env::var_os(SHIM_BYPASS_ENV).is_some() {
        return Ok(false);
    }

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let Some(file_name) = exe.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };

    let mode = match file_name.to_ascii_lowercase().as_str() {
        "claudex.exe" => ExecutableMode::Launcher,
        "claudex-config.exe" => ExecutableMode::Config,
        _ => return Ok(false),
    };

    let Some(target) = windows_shim_target(&exe, mode) else {
        return Ok(false);
    };

    let status = Command::new(target)
        .args(std::env::args_os().skip(1))
        .env(SHIM_BYPASS_ENV, "1")
        .env(
            SHIM_MODE_ENV,
            match mode {
                ExecutableMode::Launcher => "launcher",
                ExecutableMode::Config => "config",
            },
        )
        .status()
        .context("failed to launch Claudex versioned binary")?;

    std::process::exit(status.code().unwrap_or(1));
}

fn windows_shim_target(shim_path: &Path, mode: ExecutableMode) -> Option<PathBuf> {
    let install_dir = shim_path.parent()?;
    let latest = std::fs::read_to_string(install_dir.join("latest.txt")).ok()?;
    let version = latest.trim();
    if version.is_empty() || version.contains(['/', '\\']) {
        return None;
    }

    let real_name = match mode {
        ExecutableMode::Launcher => "claudex-real.exe",
        ExecutableMode::Config => "claudex-config-real.exe",
    };
    Some(install_dir.join("versions").join(version).join(real_name))
}

async fn run_launcher() -> Result<()> {
    let config_path = std::env::var_os("CLAUDEX_CONFIG").map(std::path::PathBuf::from);
    let mut config = ClaudexConfig::load(config_path.as_deref())?;

    if let Some(hyperlinks) =
        hyperlinks_from_env(std::env::var("CLAUDEX_HYPERLINKS").ok().as_deref())?
    {
        config.hyperlinks = hyperlinks;
    }

    init_logging(&config, true);

    eprintln!("Claudex v{}", env!("CARGO_PKG_VERSION"));

    let args: Vec<String> = std::env::args().skip(1).collect();
    maybe_check_release_before_startup(&args).await?;

    ensure_launcher_proxy(&mut config).await?;

    let profile_name =
        resolve_launcher_profile_name(&config, std::env::var("CLAUDEX_PROFILE").ok().as_deref())?;
    let profile = config
        .find_profile(&profile_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "profile '{}' not found; configure it with `claudex-config auth login chatgpt --profile {}` or set CLAUDEX_PROFILE",
                profile_name,
                profile_name
            )
        })?
        .clone();

    maybe_reauth_oauth_before_startup(&mut config, &profile, &args).await?;
    let model = std::env::var("CLAUDEX_MODEL").ok();
    process::launch::launch_claude(&config, &profile, model.as_deref(), &args, false)?;

    if let Some(log_path) = proxy::proxy_log_path() {
        if log_path.exists() {
            eprintln!("\nClaudex proxy log: {}", log_path.display());
        }
    }

    Ok(())
}

const OAUTH_STARTUP_WARNING_DAYS: i64 = 3;
const UPDATE_CHECK_INTERVAL_SECS: i64 = 3 * 60 * 60;
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_millis(1500);

async fn maybe_check_release_before_startup(args: &[String]) -> Result<()> {
    if !is_interactive_startup(args) || !should_run_update_check(chrono::Utc::now().timestamp())? {
        return Ok(());
    }

    eprintln!("Claudex is checking GitHub releases for updates...");
    let check = tokio::time::timeout(UPDATE_CHECK_TIMEOUT, update::check_update()).await;

    let result = match check {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => {
            eprintln!("Claudex update check skipped: {err}");
            record_update_check(chrono::Utc::now().timestamp())?;
            return Ok(());
        }
        Err(_) => {
            eprintln!("Claudex update check skipped: timed out");
            record_update_check(chrono::Utc::now().timestamp())?;
            return Ok(());
        }
    };

    eprintln!("{}", result.startup_summary());
    record_update_check(chrono::Utc::now().timestamp())?;

    if result.verdict != update::UpdateCheckVerdict::UpdateAvailable {
        return Ok(());
    }

    let latest = result.latest_version;
    if !prompt_yes_no(
        &format!("Claudex v{latest} is available. Update now?"),
        false,
    )? {
        eprintln!("Update command:\n  {}", update::installer_command_display());
        record_update_check(chrono::Utc::now().timestamp())?;
        return Ok(());
    }

    if cfg!(windows) {
        eprintln!(
            "On Windows, close other Claudex sessions before updating. First legacy-to-shim migration may fail while old claudex.exe is still running."
        );
        if !prompt_yes_no("Continue update?", false)? {
            record_update_check(chrono::Utc::now().timestamp())?;
            return Ok(());
        }
    }

    match update::run_installer_update() {
        Ok(()) => {
            record_update_check(chrono::Utc::now().timestamp())?;
            eprintln!("Claudex update finished. Restart Claudex to use v{latest}.");
            std::process::exit(0);
        }
        Err(err) => {
            eprintln!("Claudex update failed: {err}");
            eprintln!("Run manually:\n  {}", update::installer_command_display());
        }
    }

    Ok(())
}

fn is_interactive_startup(args: &[String]) -> bool {
    std::io::stdin().is_terminal()
        && !args.iter().any(|arg| arg == "-p" || arg == "--print")
        && args.first().is_none_or(|arg| arg.starts_with('-'))
}

fn should_run_update_check(now_secs: i64) -> Result<bool> {
    Ok(update_check_due(read_last_update_check()?, now_secs))
}

fn update_check_due(last_checked: Option<i64>, now_secs: i64) -> bool {
    let Some(last_checked) = last_checked else {
        return true;
    };
    now_secs - last_checked >= UPDATE_CHECK_INTERVAL_SECS
}

fn update_check_state_path() -> Result<PathBuf> {
    let mut path = ClaudexConfig::config_path()?;
    path.set_file_name("update-check");
    path.set_extension("txt");
    Ok(path)
}

fn read_last_update_check() -> Result<Option<i64>> {
    let path = update_check_state_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let value = std::fs::read_to_string(path)?.trim().parse::<i64>().ok();
    Ok(value)
}

fn record_update_check(now_secs: i64) -> Result<()> {
    let path = update_check_state_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, now_secs.to_string())?;
    Ok(())
}

async fn maybe_reauth_oauth_before_startup(
    config: &mut ClaudexConfig,
    profile: &config::ProfileConfig,
    args: &[String],
) -> Result<()> {
    let Some(health) = startup_oauth_health(profile, args) else {
        return Ok(());
    };

    let force_login = match health {
        StartupOAuthHealth::Missing => {
            eprintln!(
                "Claudex warning: OAuth token for '{}' is not available or unreadable.",
                profile.name
            );
            false
        }
        StartupOAuthHealth::Expired => {
            eprintln!(
                "Claudex warning: OAuth token for '{}' is expired.",
                profile.name
            );
            true
        }
        StartupOAuthHealth::Expiring { days } => {
            eprintln!(
                "Claudex warning: OAuth token for '{}' expires in {days} day(s).",
                profile.name
            );
            true
        }
    };

    if prompt_yes_no("Authenticate ChatGPT/Codex before starting Claude?", false)? {
        oauth::providers::login(config, "chatgpt", &profile.name, force_login, false, None).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupOAuthHealth {
    Missing,
    Expired,
    Expiring { days: i64 },
}

fn startup_oauth_health(
    profile: &config::ProfileConfig,
    args: &[String],
) -> Option<StartupOAuthHealth> {
    if !std::io::stdin().is_terminal()
        || args.iter().any(|arg| arg == "-p" || arg == "--print")
        || args.first().is_some_and(|arg| !arg.starts_with('-'))
        || profile.auth_type != oauth::AuthType::OAuth
        || !profile
            .oauth_provider
            .as_ref()
            .is_some_and(|provider| provider.normalize() == oauth::OAuthProvider::Chatgpt)
    {
        return None;
    }
    eprintln!(
        "Claudex will check OAuth token health before starting Claude using configured provider credential files and environment variables."
    );
    startup_oauth_health_from_token_result(
        oauth::source::load_credential_chain(profile.oauth_provider.as_ref().unwrap())
            .map(|cred| cred.into_oauth_token()),
        chrono::Utc::now().timestamp_millis(),
    )
}

fn startup_oauth_health_from_token_result(
    token: Result<oauth::OAuthToken>,
    now_ms: i64,
) -> Option<StartupOAuthHealth> {
    let token = match token {
        Ok(token) => token,
        Err(_) => return Some(StartupOAuthHealth::Missing),
    };
    let expires_at = token.expires_at?;
    oauth_expiry_status(expires_at, now_ms)
}

fn oauth_expiry_status(expires_at: i64, now_ms: i64) -> Option<StartupOAuthHealth> {
    let remaining_ms = expires_at - now_ms;
    if remaining_ms <= 0 {
        return Some(StartupOAuthHealth::Expired);
    }
    let warning_ms = OAUTH_STARTUP_WARNING_DAYS * 24 * 60 * 60 * 1000;
    if remaining_ms <= warning_ms {
        let days = (remaining_ms + 86_399_999) / 86_400_000;
        return Some(StartupOAuthHealth::Expiring { days });
    }
    None
}

fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(default);
    }
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    eprint!("{prompt} {suffix} ");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    if answer.is_empty() {
        return Ok(default);
    }
    Ok(matches!(answer, "y" | "Y" | "yes" | "YES" | "Yes"))
}

fn print_config_help() {
    println!("Manage configuration");
    println!();
    println!("Usage: claudex-config config [OPTIONS] <COMMAND>");
    println!();
    println!("Commands:");
    println!("  show    Show config paths and loaded config summary");
    println!("  doctor  Check and repair Claudex setup health");
    println!("  help    Print this message or the help of the given subcommand(s)");
    println!();
    println!("Options:");
    println!("      --config <PATH>  Override config file path");
    println!("  -h, --help           Print help");
}

fn resolve_launcher_profile_name(
    config: &ClaudexConfig,
    env_profile: Option<&str>,
) -> Result<String> {
    if let Some(profile) = env_profile.filter(|p| !p.trim().is_empty()) {
        return Ok(profile.to_string());
    }

    if config.find_profile("codex-sub").is_some() {
        return Ok("codex-sub".to_string());
    }

    if let Some(profile) = config.enabled_profiles().first() {
        return Ok(profile.name.clone());
    }

    anyhow::bail!(
        "no enabled claudex profile found; run `claudex-config auth login chatgpt --profile codex-sub` or set CLAUDEX_PROFILE"
    )
}

fn hyperlinks_from_env(value: Option<&str>) -> Result<Option<HyperlinksConfig>> {
    let Some(value) = value else {
        return Ok(None);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "auto" => Ok(Some(HyperlinksConfig::Auto)),
        "on" | "true" | "1" | "yes" | "enabled" => Ok(Some(HyperlinksConfig::Enabled)),
        "off" | "false" | "0" | "no" | "disabled" => Ok(Some(HyperlinksConfig::Disabled)),
        other => anyhow::bail!(
            "invalid CLAUDEX_HYPERLINKS value '{}'; use auto, on, or off",
            other
        ),
    }
}

async fn run_config_cli() -> Result<()> {
    let cli = Cli::parse();

    let mut config = ClaudexConfig::load(cli.config.as_deref())?;

    // Launcher sessions write proxy logs only to file, preserving Claude Code UI output.
    let is_run_command = matches!(&cli.command, Some(Commands::Run { .. }));
    init_logging(&config, is_run_command);

    match cli.command {
        Some(Commands::Run {
            profile: profile_name,
            model,
            hyperlinks,
            args,
        }) => {
            eprintln!("Claudex v{}", env!("CARGO_PKG_VERSION"));
            maybe_check_release_before_startup(&args).await?;
            ensure_launcher_proxy(&mut config).await?;

            let profile = config
                .find_profile(&profile_name)
                .ok_or_else(|| anyhow::anyhow!("profile '{}' not found", profile_name))?
                .clone();

            maybe_reauth_oauth_before_startup(&mut config, &profile, &args).await?;
            maybe_check_release_before_startup(&args).await?;
            process::launch::launch_claude(&config, &profile, model.as_deref(), &args, hyperlinks)?;

            // Claude 退出后，输出日志文件路径
            if let Some(log_path) = proxy::proxy_log_path() {
                if log_path.exists() {
                    eprintln!("\nClaudex proxy log: {}", log_path.display());
                }
            }
        }

        Some(Commands::Profile { action }) => match action {
            ProfileAction::List => {
                config::profile::list_profiles(&config).await;
            }
            ProfileAction::Show { name } => {
                config::profile::show_profile(&config, &name).await?;
            }
            ProfileAction::Test { name } => {
                config::profile::test_profile(&config, &name).await?;
            }
            ProfileAction::Add => {
                config::profile::interactive_add(&mut config).await?;
            }
            ProfileAction::Remove { name } => {
                config::profile::remove_profile(&mut config, &name)?;
            }
        },

        Some(Commands::Proxy { action }) => match action {
            ProxyAction::Start { port } => {
                let port = Some(select_proxy_start_port(&config, port)?);
                proxy::start_proxy(config, port).await?;
            }
            ProxyAction::Stop => {
                process::daemon::stop_proxy()?;
            }
            ProxyAction::Status => {
                process::daemon::proxy_status()?;
            }
        },

        Some(Commands::Config(command)) => match command.action {
            Some(action) => config::cmd::dispatch(action, &mut config).await?,
            None => print_config_help(),
        },

        Some(Commands::Sets { action }) => match action {
            SetsAction::Add {
                source,
                global,
                r#ref,
            } => {
                sets::add(&source, global, r#ref.as_deref()).await?;
            }
            SetsAction::Remove { name, global } => {
                sets::remove(&name, global).await?;
            }
            SetsAction::List { global } => {
                sets::list(global)?;
            }
            SetsAction::Update { name, global } => {
                sets::update(name.as_deref(), global).await?;
            }
            SetsAction::Show { name, global } => {
                sets::show(&name, global)?;
            }
        },

        Some(Commands::Auth { action }) => match action {
            AuthAction::Login {
                provider,
                profile,
                force,
                headless,
                enterprise_url,
            } => {
                if let Some(provider) = provider {
                    let profile_name = profile.unwrap_or_else(|| provider.clone());
                    oauth::providers::login(
                        &mut config,
                        &provider,
                        &profile_name,
                        force,
                        headless,
                        enterprise_url.as_deref(),
                    )
                    .await?;
                } else {
                    // No provider specified: surface the accessible options
                    // instead of erroring, so the user knows what to type.
                    println!("No provider specified. Available OAuth providers:");
                    println!("  chatgpt (openai, codex)   claude   google (gemini)");
                    println!("  qwen   kimi (moonshot)   github (copilot)   gitlab");
                    println!();
                    println!("Your profiles:");
                    crate::config::profile::list_profiles(&config).await;
                    println!();
                    println!(
                        "Example: claudex-config auth login <provider> --profile <profile> --force"
                    );
                }
            }
            AuthAction::Status { profile } => {
                oauth::providers::status(&config, profile.as_deref()).await?;
            }
            AuthAction::Logout { profile } => {
                oauth::providers::logout(&config, &profile).await?;
            }
            AuthAction::Refresh { profile } => {
                oauth::providers::refresh(&config, &profile).await?;
            }
        },

        None => {
            println!("Welcome to Claudex!");
            println!();
            println!("Get started:");
            println!("  1. Create config: claudex-config config init");
            println!(
                "  2. Add a profile: edit {:?}",
                ClaudexConfig::config_path()?
            );
            println!("  3. Run claude:    CLAUDEX_PROFILE=<profile> claudex");
            println!();
            println!("Use --help for more options.");
        }
    }

    Ok(())
}

fn init_logging(config: &ClaudexConfig, quiet_stderr: bool) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    let file_layer = proxy::proxy_log_path().and_then(|log_path| {
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok()
            .map(|file| {
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(file))
            })
    });

    let stderr_layer = if quiet_stderr {
        None
    } else {
        Some(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
}

async fn start_proxy_background(config: &ClaudexConfig) -> Result<()> {
    let port = config.proxy_port;
    let host = config.proxy_host.clone();

    // Spawn proxy in a background task
    let config_clone = config.clone();
    tokio::spawn(async move {
        if let Err(e) = proxy::start_proxy(config_clone, None).await {
            tracing::error!("proxy failed: {e}");
        }
    });

    // Wait for it to be ready
    let client = reqwest::Client::new();
    let health_url = format!("http://{host}:{port}/health");
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if client.get(&health_url).send().await.is_ok() {
            tracing::info!("proxy is ready");
            return Ok(());
        }
    }

    anyhow::bail!("proxy failed to start within 2 seconds")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyHealth {
    Current,
    StaleOrUnknown,
    Unreachable,
}

async fn ensure_launcher_proxy(config: &mut ClaudexConfig) -> Result<()> {
    let previous_port = config.proxy_port;
    config.proxy_port = find_available_local_port(&config.proxy_host)?;
    tracing::info!(
        previous_port,
        port = config.proxy_port,
        "starting private proxy for this session"
    );

    start_proxy_background(config).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

async fn probe_proxy_health(client: &reqwest::Client, host: &str, port: u16) -> ProxyHealth {
    let health_url = format!("http://{host}:{port}/health");
    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let headers = resp.headers();
            if is_current_proxy_health(
                headers
                    .get(proxy::HEALTH_VERSION_HEADER)
                    .and_then(|v| v.to_str().ok()),
                headers
                    .get(proxy::HEALTH_BODY_LIMIT_HEADER)
                    .and_then(|v| v.to_str().ok()),
            ) {
                ProxyHealth::Current
            } else {
                ProxyHealth::StaleOrUnknown
            }
        }
        Ok(_) => ProxyHealth::StaleOrUnknown,
        Err(_) => ProxyHealth::Unreachable,
    }
}

fn is_current_proxy_health(version: Option<&str>, body_limit: Option<&str>) -> bool {
    version == Some(env!("CARGO_PKG_VERSION"))
        && body_limit
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|limit| limit == current_request_body_limit_bytes())
}

fn current_request_body_limit_bytes() -> usize {
    proxy::request_body_limit_bytes_from_env(
        std::env::var(proxy::REQUEST_BODY_LIMIT_ENV).ok().as_deref(),
    )
    .unwrap_or(proxy::DEFAULT_REQUEST_BODY_LIMIT_BYTES)
}

fn select_proxy_start_port(config: &ClaudexConfig, port_override: Option<u16>) -> Result<u16> {
    match port_override {
        Some(port) => Ok(port),
        None => find_available_local_port(&config.proxy_host),
    }
}

fn find_available_local_port(host: &str) -> Result<u16> {
    let listener = TcpListener::bind((host, 0))
        .with_context(|| format!("failed to bind an ephemeral proxy port on {host}"))?;
    Ok(listener.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HyperlinksConfig, ProfileConfig};

    #[test]
    fn executable_mode_detects_config_binary() {
        assert_eq!(
            executable_mode_from_arg0("claudex-config"),
            ExecutableMode::Config
        );
        assert_eq!(
            executable_mode_from_arg0("/usr/local/bin/claudex-config.exe"),
            ExecutableMode::Config
        );
        assert_eq!(
            executable_mode_from_arg0("claudex"),
            ExecutableMode::Launcher
        );
    }

    #[test]
    fn launcher_profile_prefers_env_then_codex_sub_then_first_enabled() {
        let mut config = ClaudexConfig {
            profiles: vec![
                ProfileConfig {
                    name: "first".to_string(),
                    enabled: true,
                    ..ProfileConfig::default()
                },
                ProfileConfig {
                    name: "codex-sub".to_string(),
                    enabled: true,
                    ..ProfileConfig::default()
                },
            ],
            ..ClaudexConfig::default()
        };

        assert_eq!(
            resolve_launcher_profile_name(&config, Some("manual")).unwrap(),
            "manual"
        );
        assert_eq!(
            resolve_launcher_profile_name(&config, None).unwrap(),
            "codex-sub"
        );

        config.profiles.retain(|p| p.name != "codex-sub");
        assert_eq!(
            resolve_launcher_profile_name(&config, None).unwrap(),
            "first"
        );
    }

    #[test]
    fn launcher_hyperlinks_env_maps_to_config() {
        assert_eq!(
            hyperlinks_from_env(Some("on")).unwrap(),
            Some(HyperlinksConfig::Enabled)
        );
        assert_eq!(
            hyperlinks_from_env(Some("off")).unwrap(),
            Some(HyperlinksConfig::Disabled)
        );
        assert_eq!(
            hyperlinks_from_env(Some("auto")).unwrap(),
            Some(HyperlinksConfig::Auto)
        );
    }

    #[test]
    fn proxy_start_without_override_selects_ephemeral_port() {
        let config = ClaudexConfig {
            proxy_host: "127.0.0.1".to_string(),
            proxy_port: 13456,
            ..ClaudexConfig::default()
        };

        let selected = select_proxy_start_port(&config, None).unwrap();

        assert_ne!(selected, 13456);
        assert!(selected > 0);
    }

    #[test]
    fn proxy_start_keeps_explicit_port_override() {
        let config = ClaudexConfig::default();

        assert_eq!(select_proxy_start_port(&config, Some(7777)).unwrap(), 7777);
    }

    #[test]
    fn find_available_local_port_returns_bindable_port() {
        let port = find_available_local_port("127.0.0.1").unwrap();
        let listener = TcpListener::bind(("127.0.0.1", port));

        assert!(listener.is_ok());
    }

    #[test]
    fn oauth_expiry_status_warns_for_expired_token() {
        assert_eq!(
            oauth_expiry_status(1_000, 2_000),
            Some(StartupOAuthHealth::Expired)
        );
    }

    #[test]
    fn oauth_expiry_status_warns_within_three_days() {
        let now = 1_000_000;
        let two_days = 2 * 24 * 60 * 60 * 1000;
        assert_eq!(
            oauth_expiry_status(now + two_days, now),
            Some(StartupOAuthHealth::Expiring { days: 2 })
        );
    }

    #[test]
    fn oauth_expiry_status_ignores_tokens_after_warning_window() {
        let now = 1_000_000;
        let four_days = 4 * 24 * 60 * 60 * 1000;
        assert_eq!(oauth_expiry_status(now + four_days, now), None);
    }

    #[test]
    fn update_check_runs_after_three_hour_cache_window() {
        let now = 10_000;
        assert!(update_check_due(None, now));
        assert!(!update_check_due(
            Some(now - UPDATE_CHECK_INTERVAL_SECS + 1),
            now
        ));
        assert!(update_check_due(
            Some(now - UPDATE_CHECK_INTERVAL_SECS),
            now
        ));
    }

    #[test]
    fn noninteractive_startup_args_skip_release_check() {
        assert!(!is_interactive_startup(&["-p".to_string()]));
        assert!(!is_interactive_startup(&["--print".to_string()]));
        assert!(!is_interactive_startup(&["hello".to_string()]));
    }

    #[test]
    fn executable_mode_env_overrides_argv0_for_shims() {
        std::env::set_var(SHIM_MODE_ENV, "config");
        assert_eq!(
            executable_mode_from_env_or_arg0("claudex-real.exe"),
            ExecutableMode::Config
        );

        std::env::set_var(SHIM_MODE_ENV, "launcher");
        assert_eq!(
            executable_mode_from_env_or_arg0("claudex-config-real.exe"),
            ExecutableMode::Launcher
        );
        std::env::remove_var(SHIM_MODE_ENV);
    }

    #[test]
    fn windows_shim_target_uses_latest_metadata_and_mode() {
        let root = std::env::temp_dir().join(format!("claudex-shim-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("latest.txt"), "0.9.41\n").unwrap();

        assert_eq!(
            windows_shim_target(&root.join("claudex.exe"), ExecutableMode::Launcher).unwrap(),
            root.join("versions")
                .join("0.9.41")
                .join("claudex-real.exe")
        );
        assert_eq!(
            windows_shim_target(&root.join("claudex-config.exe"), ExecutableMode::Config).unwrap(),
            root.join("versions")
                .join("0.9.41")
                .join("claudex-config-real.exe")
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installer_command_display_uses_platform_installer() {
        let command = update::installer_command_display();
        if cfg!(windows) {
            assert_eq!(
                command,
                "powershell.exe -NoProfile -ExecutionPolicy Bypass -Command \"irm https://raw.githubusercontent.com/pilc80/claudex/main/install.ps1 | iex\""
            );
        } else {
            assert_eq!(
                command,
                "curl -fL --progress-bar https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash"
            );
        }
    }

    #[test]
    fn startup_oauth_health_reports_missing_token() {
        assert_eq!(
            startup_oauth_health_from_token_result(
                Err(anyhow::anyhow!(
                    "no OAuth token found in configured sources"
                )),
                1_000
            ),
            Some(StartupOAuthHealth::Missing)
        );
    }

    fn proxy_health_requires_current_version_and_body_limit() {
        let current_limit = current_request_body_limit_bytes();
        assert!(is_current_proxy_health(
            Some(env!("CARGO_PKG_VERSION")),
            Some(&current_limit.to_string())
        ));
        assert!(!is_current_proxy_health(None, None));
        assert!(!is_current_proxy_health(
            Some("0.0.0"),
            Some(&current_limit.to_string())
        ));
        assert!(!is_current_proxy_health(
            Some(env!("CARGO_PKG_VERSION")),
            Some(&(current_limit + 1).to_string())
        ));
        assert!(!is_current_proxy_health(
            Some(env!("CARGO_PKG_VERSION")),
            Some("not-a-number")
        ));
    }
}
