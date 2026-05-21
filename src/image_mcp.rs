use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::ClaudexConfig;

pub const SERVER_NAME: &str = "claudex-image";
pub const DEFAULT_PROFILE: &str = "codex-sub";
pub const MANAGED_BY_ENV: &str = "CLAUDEX_IMAGE_MANAGED_BY";
pub const VERSION_ENV: &str = "CLAUDEX_IMAGE_MCP_VERSION";
pub const MANAGED_BY: &str = "claudex";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMcpArgs {
    pub profile: String,
    pub config_path: Option<PathBuf>,
}

impl ImageMcpArgs {
    pub fn parse_from_env_and_args() -> Result<Self> {
        Self::parse_from(
            std::env::args().skip(1),
            std::env::var("CLAUDEX_IMAGE_PROFILE").ok(),
            std::env::var_os("CLAUDEX_CONFIG").map(PathBuf::from),
        )
    }

    pub fn parse_from<I>(
        args: I,
        env_profile: Option<String>,
        env_config_path: Option<PathBuf>,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut profile = env_profile.unwrap_or_else(|| DEFAULT_PROFILE.to_string());
        let mut config_path = env_config_path;
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--profile" => {
                    profile = iter.next().context("--profile requires a profile name")?;
                }
                "--config" => {
                    config_path = Some(PathBuf::from(
                        iter.next().context("--config requires a path")?,
                    ));
                }
                other => anyhow::bail!("unknown claudex-image-mcp argument: {other}"),
            }
        }
        Ok(Self {
            profile,
            config_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

impl ImageConfig {
    pub async fn from_args(args: &ImageMcpArgs) -> Result<Self> {
        match Self::from_claudex_profile(&args.profile, args.config_path.as_deref()).await {
            Ok(config) => Ok(config),
            Err(profile_error) => Self::from_env().with_context(|| {
                format!(
                    "failed to load Claudex image profile '{}': {profile_error}",
                    args.profile
                )
            }),
        }
    }

    pub async fn from_claudex_profile(
        profile_name: &str,
        config_path: Option<&Path>,
    ) -> Result<Self> {
        let config = ClaudexConfig::load(config_path)?;
        let mut profile = config
            .find_profile(profile_name)
            .with_context(|| format!("profile '{profile_name}' not found"))?
            .clone();
        crate::oauth::providers::ensure_valid_token(&mut profile).await?;
        if profile.api_key.is_empty() {
            anyhow::bail!(
                "profile '{profile_name}' does not provide an API key or OAuth bearer token"
            );
        }
        Ok(Self {
            api_key: profile.api_key,
            base_url: resolve_images_base_url(&profile.base_url)?,
            model: profile
                .image_model
                .unwrap_or_else(|| profile.default_model.clone()),
        })
    }

    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .or_else(|_| std::env::var("CLAUDEX_IMAGE_OPENAI_API_KEY"))
            .context("OPENAI_API_KEY or CLAUDEX_IMAGE_OPENAI_API_KEY is required")?;
        Ok(Self {
            api_key,
            base_url: std::env::var("CLAUDEX_IMAGE_OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            model: std::env::var("CLAUDEX_IMAGE_MODEL")
                .unwrap_or_else(|_| "gpt-image-2".to_string()),
        })
    }
}

fn resolve_images_base_url(base_url: &str) -> Result<String> {
    let explicit = std::env::var("CLAUDEX_IMAGE_OPENAI_BASE_URL").ok();
    if base_url.contains("chatgpt.com/backend-api/codex") {
        return explicit.context(
            "ChatGPT/Codex profile URLs do not expose the official Images API; set CLAUDEX_IMAGE_OPENAI_BASE_URL",
        );
    }
    Ok(explicit.unwrap_or_else(|| base_url.trim_end_matches('/').to_string()))
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
struct GenerateImageArgs {
    prompt: String,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    quality: Option<String>,
    #[serde(default)]
    output_format: Option<String>,
    #[serde(default)]
    background: Option<String>,
    #[serde(default)]
    moderation: Option<String>,
    #[serde(default)]
    output_compression: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct ImagesResponse {
    data: Vec<ImageData>,
}

#[derive(Debug, Deserialize)]
struct ImageData {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

pub async fn run() -> Result<()> {
    let args = ImageMcpArgs::parse_from_env_and_args()?;
    let config = ImageConfig::from_args(&args).await?;
    run_stdio(config).await
}

async fn run_stdio(config: ImageConfig) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_line(&config, &line).await;
        if let Some(response) = response {
            stdout
                .write_all(serde_json::to_string(&response)?.as_bytes())
                .await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

async fn handle_line(config: &ImageConfig, line: &str) -> Option<RpcResponse> {
    let request: RpcRequest = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(e) => {
            return Some(error_response(
                Value::Null,
                -32700,
                format!("parse error: {e}"),
            ));
        }
    };
    let id = request.id.clone().unwrap_or(Value::Null);
    request.id.as_ref()?;

    let result = match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => Ok(json!({"tools": [tool_schema()]})),
        "tools/call" => call_tool(config, &request.params).await,
        _ => Err(anyhow::anyhow!("method not found: {}", request.method)),
    };

    Some(match result {
        Ok(result) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        },
        Err(e) => error_response(id, -32000, e.to_string()),
    })
}

fn tool_schema() -> Value {
    json!({
        "name": "generate_image",
        "description": "Generate an image with OpenAI Images API using the configured Claudex profile.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prompt": {"type": "string"},
                "size": {"type": "string", "default": "1024x1024"},
                "quality": {"type": "string", "default": "auto"},
                "output_format": {"type": "string", "default": "png"},
                "background": {"type": "string", "default": "auto"},
                "moderation": {"type": "string", "default": "auto"},
                "output_compression": {"type": "integer", "minimum": 0, "maximum": 100}
            },
            "required": ["prompt"]
        }
    })
}

async fn call_tool(config: &ImageConfig, params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .context("tools/call params.name is required")?;
    if name != "generate_image" {
        anyhow::bail!("unknown tool: {name}");
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let args: GenerateImageArgs = serde_json::from_value(arguments)?;
    let image = generate_image(config, args).await?;
    Ok(json!({
        "content": [{"type": "text", "text": image}],
        "isError": false
    }))
}

async fn generate_image(config: &ImageConfig, args: GenerateImageArgs) -> Result<String> {
    let mut body = json!({
        "model": config.model,
        "prompt": args.prompt,
        "size": args.size.unwrap_or_else(|| "1024x1024".to_string()),
        "quality": args.quality.unwrap_or_else(|| "auto".to_string()),
        "output_format": args.output_format.unwrap_or_else(|| "png".to_string()),
        "background": args.background.unwrap_or_else(|| "auto".to_string()),
        "moderation": args.moderation.unwrap_or_else(|| "auto".to_string())
    });
    if let Some(output_compression) = args.output_compression {
        body["output_compression"] = json!(output_compression);
    }
    let endpoint = format!(
        "{}/images/generations",
        config.base_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .bearer_auth(&config.api_key)
        .json(&body)
        .send()
        .await
        .context("failed to call Images API")?;
    let status = response.status();
    let text = response
        .text()
        .await
        .context("failed to read Images API response")?;
    if !status.is_success() {
        anyhow::bail!("Images API returned {status}: {text}");
    }
    let parsed: ImagesResponse =
        serde_json::from_str(&text).context("failed to parse Images API response")?;
    let first = parsed
        .data
        .first()
        .context("Images API returned no images")?;
    if let Some(b64) = &first.b64_json {
        Ok(format!("data:image/png;base64,{b64}"))
    } else if let Some(url) = &first.url {
        Ok(url.clone())
    } else {
        anyhow::bail!("Images API image item has neither b64_json nor url")
    }
}

fn error_response(id: Value, code: i64, message: String) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(json!({"code": code, "message": message})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_defaults_to_codex_sub() {
        let args = ImageMcpArgs::parse_from(Vec::new(), None, None).unwrap();
        assert_eq!(args.profile, DEFAULT_PROFILE);
        assert!(args.config_path.is_none());
    }

    #[test]
    fn cli_uses_profile_and_config_args() {
        let args = ImageMcpArgs::parse_from(
            vec![
                "--profile".to_string(),
                "images".to_string(),
                "--config".to_string(),
                "/tmp/claudex.toml".to_string(),
            ],
            Some("env-profile".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(args.profile, "images");
        assert_eq!(args.config_path, Some(PathBuf::from("/tmp/claudex.toml")));
    }

    #[test]
    fn chatgpt_codex_requires_explicit_image_base_url() {
        let err = resolve_images_base_url("https://chatgpt.com/backend-api/codex").unwrap_err();
        assert!(err.to_string().contains("CLAUDEX_IMAGE_OPENAI_BASE_URL"));
    }
}
