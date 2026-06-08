use anyhow::{Context, Result};
use semver::Version;
use serde::Deserialize;
use std::process::Command;

const REPO_OWNER: &str = "pilc80";
const REPO_NAME: &str = "claudex";
const BIN_NAME: &str = "claudex";
const RELEASE_MANIFEST_NAME: &str = "claudex-release-manifest.json";

#[derive(Deserialize)]
struct ReleaseManifest {
    version: String,
}

/// Check if a newer version is available on GitHub Releases.
/// Returns Some(version) if newer, None if up-to-date.
pub async fn check_update() -> Result<Option<String>> {
    tokio::task::spawn_blocking(check_update_blocking).await?
}

fn check_update_blocking() -> Result<Option<String>> {
    let current = env!("CARGO_PKG_VERSION");
    let latest_version = fetch_latest_manifest_version()?
        .trim_start_matches('v')
        .to_string();
    newer_version(latest_version, current)
}

fn fetch_latest_manifest_version() -> Result<String> {
    let url = format!(
        "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/latest/download/{RELEASE_MANIFEST_NAME}"
    );
    let mut body = Vec::new();
    self_update::Download::from_url(&url)
        .set_header(reqwest::header::USER_AGENT, "claudex".parse()?)
        .download_to(&mut body)?;
    let manifest: ReleaseManifest = serde_json::from_slice(&body)?;
    Ok(manifest.version)
}

fn newer_version(latest_version: String, current: &str) -> Result<Option<String>> {
    let latest_version = latest_version.trim_start_matches('v').to_string();
    let latest = Version::parse(&latest_version)
        .with_context(|| format!("invalid latest Claudex version: {latest_version}"))?;
    let current = Version::parse(current)
        .with_context(|| format!("invalid current Claudex version: {current}"))?;

    if latest > current {
        Ok(Some(latest_version))
    } else {
        Ok(None)
    }
}

/// Download and install the latest version.
pub async fn self_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: v{current}");
    println!("Checking for updates...");

    let latest = tokio::task::spawn_blocking(fetch_latest_manifest_version).await??;
    match newer_version(latest, current)? {
        Some(version) => {
            println!("Claudex v{version} is available.");
            run_installer_update()?;
        }
        None => println!("Already up to date (v{current})"),
    }

    Ok(())
}

pub fn run_installer_update() -> Result<()> {
    if cfg!(windows) {
        eprintln!(
            "On Windows, close other Claudex sessions before updating. First legacy-to-shim migration may fail while old claudex.exe is still running."
        );
    }

    let (program, args) = installer_command();
    let status = Command::new(program).args(args).status().with_context(|| {
        format!(
            "failed to run Claudex installer command: {}",
            installer_command_display()
        )
    })?;

    if !status.success() {
        anyhow::bail!("Claudex installer exited with status {status}");
    }

    Ok(())
}

pub fn installer_command_display() -> &'static str {
    if cfg!(windows) {
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -Command \"irm https://raw.githubusercontent.com/pilc80/claudex/main/install.ps1 | iex\""
    } else {
        "curl -fL --progress-bar https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash"
    }
}

fn installer_command() -> (&'static str, &'static [&'static str]) {
    if cfg!(windows) {
        (
            "powershell.exe",
            &[
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "irm https://raw.githubusercontent.com/pilc80/claudex/main/install.ps1 | iex",
            ],
        )
    } else {
        (
            "sh",
            &[
                "-c",
                "curl -fL --progress-bar https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash",
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_version_strips_v_prefix() {
        assert_eq!(
            newer_version("v0.9.42".to_string(), "0.9.41").unwrap(),
            Some("0.9.42".to_string())
        );
    }

    #[test]
    fn newer_version_returns_none_for_current() {
        assert_eq!(
            newer_version("v0.9.41".to_string(), "0.9.41").unwrap(),
            None
        );
    }

    #[test]
    fn newer_version_returns_none_when_current_is_newer_than_latest() {
        assert_eq!(
            newer_version("v0.9.43".to_string(), "0.9.44").unwrap(),
            None
        );
    }

    #[test]
    fn release_manifest_parses_version() {
        let manifest: ReleaseManifest =
            serde_json::from_str(r#"{"version":"v0.9.42","artifacts":[]}"#).unwrap();
        assert_eq!(manifest.version, "v0.9.42");
    }

    #[test]
    fn installer_command_uses_installer_script() {
        let display = installer_command_display();
        let (program, args) = installer_command();
        if cfg!(windows) {
            assert_eq!(program, "powershell.exe");
            assert!(args.contains(&"-NoProfile"));
            assert!(display.contains("install.ps1"));
            assert!(display.contains("irm "));
        } else {
            assert_eq!(program, "sh");
            assert!(args.contains(&"-c"));
            assert!(display.contains("install.sh"));
            assert!(display.contains("curl -fL"));
        }
    }
}
