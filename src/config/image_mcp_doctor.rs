use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::image_mcp::{MANAGED_BY, MANAGED_BY_ENV, SERVER_NAME, VERSION_ENV};
use crate::sets::lock::{Scope, SetsLockFile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageMcpInstallStatus {
    Missing,
    Current,
    Stale(String),
    Conflict,
}

pub fn build_image_mcp_server_config(binary_path: &Path, profile: &str, version: &str) -> Value {
    json!({
        "type": "stdio",
        "command": binary_path.to_string_lossy(),
        "args": ["--profile", profile],
        "env": {
            MANAGED_BY_ENV: MANAGED_BY,
            VERSION_ENV: version
        }
    })
}

pub fn image_mcp_install_status(doc: &Value, expected: &Value) -> ImageMcpInstallStatus {
    let Some(server) = doc
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|servers| servers.get(SERVER_NAME))
    else {
        return ImageMcpInstallStatus::Missing;
    };

    if !is_managed(server) {
        return ImageMcpInstallStatus::Conflict;
    }
    if server == expected {
        return ImageMcpInstallStatus::Current;
    }
    ImageMcpInstallStatus::Stale(stale_reason(server, expected))
}

pub fn upsert_image_mcp_server(doc: &mut Value, expected: Value) -> Result<()> {
    let obj = doc
        .as_object_mut()
        .context("claude.json is not an object")?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .context("mcpServers is not an object")?;
    if let Some(existing) = servers.get(SERVER_NAME) {
        if !is_managed(existing) {
            anyhow::bail!("refusing to overwrite unmanaged '{SERVER_NAME}' MCP server");
        }
    }
    servers.insert(SERVER_NAME.to_string(), expected);
    Ok(())
}

pub fn install_image_mcp_server(profile: &str) -> Result<ImageMcpInstallStatus> {
    let binary_path = resolve_image_mcp_binary_path();
    let expected = build_image_mcp_server_config(&binary_path, profile, env!("CARGO_PKG_VERSION"));
    let path = SetsLockFile::claude_json_path(Scope::Global)?;
    let mut doc = read_json_or_empty(&path)?;
    let status = image_mcp_install_status(&doc, &expected);
    match status {
        ImageMcpInstallStatus::Missing | ImageMcpInstallStatus::Stale(_) => {
            upsert_image_mcp_server(&mut doc, expected)?;
            write_json(&path, &doc)?;
        }
        ImageMcpInstallStatus::Current | ImageMcpInstallStatus::Conflict => {}
    }
    Ok(status)
}

pub fn current_image_mcp_status(profile: &str) -> Result<ImageMcpInstallStatus> {
    let binary_path = resolve_image_mcp_binary_path();
    let expected = build_image_mcp_server_config(&binary_path, profile, env!("CARGO_PKG_VERSION"));
    let path = SetsLockFile::claude_json_path(Scope::Global)?;
    let doc = read_json_or_empty(&path)?;
    Ok(image_mcp_install_status(&doc, &expected))
}

pub fn resolve_image_mcp_binary_path() -> PathBuf {
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join(binary_name());
            if sibling.exists() {
                return sibling;
            }
        }
    }
    if let Ok(path) = which::which(binary_name()) {
        return path;
    }
    PathBuf::from(binary_name())
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "claudex-image-mcp.exe"
    } else {
        "claudex-image-mcp"
    }
}

fn is_managed(server: &Value) -> bool {
    server
        .get("env")
        .and_then(Value::as_object)
        .and_then(|env| env.get(MANAGED_BY_ENV))
        .and_then(Value::as_str)
        == Some(MANAGED_BY)
}

fn stale_reason(server: &Value, expected: &Value) -> String {
    for key in ["command", "args", "env"] {
        if server.get(key) != expected.get(key) {
            return format!("{key} differs from expected {SERVER_NAME} MCP config");
        }
    }
    "server config differs from expected managed MCP config".to_string()
}

fn read_json_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

fn write_json(path: &Path, doc: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(doc)? + "\n")
        .with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected(profile: &str, version: &str) -> Value {
        build_image_mcp_server_config(Path::new("/bin/claudex-image-mcp"), profile, version)
    }

    #[test]
    fn status_missing_when_server_absent() {
        assert_eq!(
            image_mcp_install_status(&json!({"mcpServers": {}}), &expected("codex-sub", "1")),
            ImageMcpInstallStatus::Missing
        );
    }

    #[test]
    fn status_current_when_config_matches() {
        let expected = expected("codex-sub", "1");
        let doc = json!({"mcpServers": {SERVER_NAME: expected.clone()}});
        assert_eq!(
            image_mcp_install_status(&doc, &expected),
            ImageMcpInstallStatus::Current
        );
    }

    #[test]
    fn status_stale_when_version_differs() {
        let expected_config = expected("codex-sub", "2");
        let doc = json!({"mcpServers": {SERVER_NAME: expected("codex-sub", "1")}});
        assert!(matches!(
            image_mcp_install_status(&doc, &expected_config),
            ImageMcpInstallStatus::Stale(_)
        ));
    }

    #[test]
    fn status_stale_when_profile_differs() {
        let expected_config = expected("codex-sub", "1");
        let doc = json!({"mcpServers": {SERVER_NAME: expected("other", "1")}});
        assert!(matches!(
            image_mcp_install_status(&doc, &expected_config),
            ImageMcpInstallStatus::Stale(_)
        ));
    }

    #[test]
    fn status_conflict_when_unmanaged() {
        let doc = json!({"mcpServers": {SERVER_NAME: {"type": "stdio", "command": "other"}}});
        assert_eq!(
            image_mcp_install_status(&doc, &expected("codex-sub", "1")),
            ImageMcpInstallStatus::Conflict
        );
    }

    #[test]
    fn upsert_preserves_other_servers() {
        let mut doc = json!({"mcpServers": {"other": {"type": "stdio", "command": "x"}}});
        upsert_image_mcp_server(&mut doc, expected("codex-sub", "1")).unwrap();
        assert!(doc["mcpServers"]["other"].is_object());
        assert!(doc["mcpServers"][SERVER_NAME].is_object());
    }

    #[test]
    fn upsert_refuses_unmanaged_conflict() {
        let mut doc = json!({"mcpServers": {SERVER_NAME: {"type": "stdio", "command": "other"}}});
        let err = upsert_image_mcp_server(&mut doc, expected("codex-sub", "1")).unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"));
    }
}
