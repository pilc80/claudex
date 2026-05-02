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
mod tui;
mod update;
mod util;

use anyhow::{Context, Result};
use clap::Parser;
use std::ffi::OsStr;
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
    let model = std::env::var("CLAUDEX_MODEL").ok();
    process::launch::launch_claude(&config, &profile, model.as_deref(), &args, false)?;

    if let Some(log_path) = proxy::proxy_log_path() {
        if log_path.exists() {
            eprintln!("\nClaudex proxy log: {}", log_path.display());
        }
    }

    Ok(())
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
            ProxyAction::Start {
                port,
                daemon: as_daemon,
            } => {
                if as_daemon {
                    start_proxy_background(&config).await?;
                } else {
                    proxy::start_proxy(config, port).await?;
                }
            }
            ProxyAction::Stop => {
                process::daemon::stop_proxy()?;
            }
            ProxyAction::Status => {
                process::daemon::proxy_status()?;
            }
        },

        Some(Commands::Dashboard) => {
            let config_arc = std::sync::Arc::new(tokio::sync::RwLock::new(config));
            let metrics_store = proxy::metrics::MetricsStore::new();
            let health =
                std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
            tui::run_tui(config_arc, metrics_store, health).await?;
        }

        Some(Commands::Config { action }) => {
            config::cmd::dispatch(action, &mut config).await?;
        }

        Some(Commands::Update { check }) => {
            if check {
                match update::check_update().await? {
                    Some(version) => println!("New version available: {version}"),
                    None => println!("Already up to date (v{})", env!("CARGO_PKG_VERSION")),
                }
            } else {
                update::self_update().await?;
            }
        }

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
            // Default: launch TUI if profiles exist, else show help
            if config.profiles.is_empty() {
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
            } else {
                let config_arc = std::sync::Arc::new(tokio::sync::RwLock::new(config));
                let metrics_store = proxy::metrics::MetricsStore::new();
                let health =
                    std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
                tui::run_tui(config_arc, metrics_store, health).await?;
            }
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
            .is_some_and(|limit| limit >= proxy::REQUEST_BODY_LIMIT_BYTES)
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
    fn proxy_health_requires_current_version_and_body_limit() {
        assert!(is_current_proxy_health(
            Some(env!("CARGO_PKG_VERSION")),
            Some(&proxy::REQUEST_BODY_LIMIT_BYTES.to_string())
        ));
        assert!(is_current_proxy_health(
            Some(env!("CARGO_PKG_VERSION")),
            Some(&(proxy::REQUEST_BODY_LIMIT_BYTES + 1).to_string())
        ));
        assert!(!is_current_proxy_health(None, None));
        assert!(!is_current_proxy_health(
            Some("0.0.0"),
            Some(&proxy::REQUEST_BODY_LIMIT_BYTES.to_string())
        ));
        assert!(!is_current_proxy_health(
            Some(env!("CARGO_PKG_VERSION")),
            Some("2097152")
        ));
    }
}
