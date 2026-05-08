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
use std::time::Duration;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use cli::{AuthAction, Cli, Commands, ProfileAction, ProxyAction, SetsAction};
use config::{ClaudexConfig, HyperlinksConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutableMode {
    Launcher,
    Config,
}

pub async fn run_from_argv0() -> Result<()> {
    run_with_mode(executable_mode_from_arg0(
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

fn executable_mode_from_arg0(arg0: impl AsRef<OsStr>) -> ExecutableMode {
    let path = std::path::Path::new(arg0.as_ref());
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

async fn run_launcher() -> Result<()> {
    let config_path = std::env::var_os("CLAUDEX_CONFIG").map(std::path::PathBuf::from);
    let mut config = ClaudexConfig::load(config_path.as_deref())?;

    if let Some(hyperlinks) =
        hyperlinks_from_env(std::env::var("CLAUDEX_HYPERLINKS").ok().as_deref())?
    {
        config.hyperlinks = hyperlinks;
    }

    init_logging(&config, true);

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

    let args: Vec<String> = std::env::args().skip(1).collect();
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

const OAUTH_STARTUP_WARNING_DAYS: i64 = 7;

async fn maybe_reauth_oauth_before_startup(
    config: &mut ClaudexConfig,
    profile: &config::ProfileConfig,
    args: &[String],
) -> Result<()> {
    let Some(expiry) = startup_oauth_expiry(profile, args) else {
        return Ok(());
    };

    match expiry {
        StartupOAuthExpiry::Expired => eprintln!(
            "Claudex warning: OAuth token for '{}' is expired.",
            profile.name
        ),
        StartupOAuthExpiry::Expiring { days } => eprintln!(
            "Claudex warning: OAuth token for '{}' expires in {days} day(s).",
            profile.name
        ),
    }

    if prompt_yes_no(
        "Re-authenticate ChatGPT/Codex before starting Claude?",
        false,
    )? {
        oauth::providers::login(config, "chatgpt", &profile.name, true, false, None).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupOAuthExpiry {
    Expired,
    Expiring { days: i64 },
}

fn startup_oauth_expiry(
    profile: &config::ProfileConfig,
    args: &[String],
) -> Option<StartupOAuthExpiry> {
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
        "Claudex will check OAuth token health before starting Claude and may ask for \x1b[33mkeychain access\x1b[0m."
    );
    let token = oauth::source::load_keyring(&profile.name).ok()?;
    let expires_at = token.expires_at?;
    oauth_expiry_status(expires_at, chrono::Utc::now().timestamp_millis())
}

fn oauth_expiry_status(expires_at: i64, now_ms: i64) -> Option<StartupOAuthExpiry> {
    let remaining_ms = expires_at - now_ms;
    if remaining_ms <= 0 {
        return Some(StartupOAuthExpiry::Expired);
    }
    let warning_ms = OAUTH_STARTUP_WARNING_DAYS * 24 * 60 * 60 * 1000;
    if remaining_ms <= warning_ms {
        let days = (remaining_ms + 86_399_999) / 86_400_000;
        return Some(StartupOAuthExpiry::Expiring { days });
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
            ensure_launcher_proxy(&mut config).await?;

            let profile = config
                .find_profile(&profile_name)
                .ok_or_else(|| anyhow::anyhow!("profile '{}' not found", profile_name))?
                .clone();

            maybe_reauth_oauth_before_startup(&mut config, &profile, &args).await?;
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
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()?;
    let health = probe_proxy_health(&client, &config.proxy_host, config.proxy_port).await;

    if health == ProxyHealth::Current {
        return Ok(());
    }

    if health == ProxyHealth::StaleOrUnknown {
        let previous_port = config.proxy_port;
        config.proxy_port = find_available_local_port(&config.proxy_host)?;
        tracing::warn!(
            previous_port,
            new_port = config.proxy_port,
            "existing proxy is stale or missing health metadata; starting private proxy for this session"
        );
    } else if process::daemon::is_proxy_running()? {
        tracing::warn!(
            port = config.proxy_port,
            "proxy PID exists but configured health endpoint is unreachable; starting proxy on configured port"
        );
    } else {
        tracing::info!("proxy not running, starting in background...");
    }

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
    fn oauth_expiry_status_warns_for_expired_token() {
        assert_eq!(
            oauth_expiry_status(1_000, 2_000),
            Some(StartupOAuthExpiry::Expired)
        );
    }

    #[test]
    fn oauth_expiry_status_warns_within_seven_days() {
        let now = 1_000_000;
        let six_days = 6 * 24 * 60 * 60 * 1000;
        assert_eq!(
            oauth_expiry_status(now + six_days, now),
            Some(StartupOAuthExpiry::Expiring { days: 6 })
        );
    }

    #[test]
    fn oauth_expiry_status_ignores_tokens_after_warning_window() {
        let now = 1_000_000;
        let eight_days = 8 * 24 * 60 * 60 * 1000;
        assert_eq!(oauth_expiry_status(now + eight_days, now), None);
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
