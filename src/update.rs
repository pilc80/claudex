use anyhow::Result;
use serde::Deserialize;

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
    if latest_version != current {
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
            println!("Run update command:\n  {}", claudex_update_command());
        }
        None => println!("Already up to date (v{current})"),
    }

    Ok(())
}

fn claudex_update_command() -> &'static str {
    if cfg!(windows) {
        "irm https://raw.githubusercontent.com/pilc80/claudex/main/install.ps1 | iex"
    } else {
        "curl -fL --progress-bar https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash"
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
    fn release_manifest_parses_version() {
        let manifest: ReleaseManifest =
            serde_json::from_str(r#"{"version":"v0.9.42","artifacts":[]}"#).unwrap();
        assert_eq!(manifest.version, "v0.9.42");
    }

    #[test]
    fn update_command_uses_installer_script() {
        let command = claudex_update_command();
        if cfg!(windows) {
            assert!(command.contains("install.ps1"));
            assert!(command.contains("irm "));
        } else {
            assert!(command.contains("install.sh"));
            assert!(command.contains("curl -fL"));
        }
    }
}
