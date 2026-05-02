use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::config::ProfileConfig;
use crate::oauth::AuthType;
use crate::proxy::ProxyState;
use crate::router::classifier;

const IMAGE_HISTORY_PLACEHOLDER_PREFIX: &str = "[Previous image omitted by claudex";

pub async fn handle_messages(
    State(state): State<Arc<ProxyState>>,
    Path(profile_name): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let start = Instant::now();

    // 入站请求日志
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            if s.len() > 20 {
                format!("{}...", &s[..20])
            } else {
                s.to_string()
            }
        })
        .unwrap_or_else(|| "(none)".to_string());
    let api_key_header = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            if s.len() > 20 {
                format!("{}...", &s[..20])
            } else {
                s.to_string()
            }
        })
        .unwrap_or_else(|| "(none)".to_string());

    tracing::info!(
        profile = %profile_name,
        authorization = %auth_header,
        x_api_key = %api_key_header,
        body_len = %body.len(),
        "incoming request"
    );

    let mut body_value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")).into_response();
        }
    };
    let image_history = prune_historical_images(&mut body_value);
    if image_history.omitted_images > 0 {
        tracing::info!(
            omitted_images = image_history.omitted_images,
            omitted_base64_bytes = image_history.omitted_base64_bytes,
            kept_image_message = ?image_history.kept_message_index,
            "pruned historical image payloads"
        );
    }

    // --- Smart Routing: resolve "auto" profile ---
    let resolved_profile_name = if profile_name == "auto" {
        resolve_auto_profile(&state, &body_value).await
    } else {
        profile_name.clone()
    };

    let config = state.config.read().await;

    let mut profile = match config.find_profile(&resolved_profile_name) {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                format!("profile '{resolved_profile_name}' not found"),
            )
                .into_response();
        }
    };

    if !profile.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("profile '{resolved_profile_name}' is disabled"),
        )
            .into_response();
    }

    // Collect backup provider profiles
    let backup_profiles: Vec<ProfileConfig> = profile
        .backup_providers
        .iter()
        .filter_map(|name| config.find_profile(name).cloned())
        .filter(|p| p.enabled)
        .collect();

    let context_config = config.context.clone();
    let full_config = config.clone();
    let metrics = state.metrics.get_or_create(&resolved_profile_name);
    drop(config);

    // OAuth token lazy refresh via TokenManager
    if profile.auth_type == AuthType::OAuth {
        match state.token_manager.get_token(&profile).await {
            Ok(token) => {
                crate::oauth::manager::apply_token_to_profile(&mut profile, &token);
            }
            Err(e) => {
                return (StatusCode::UNAUTHORIZED, format!("OAuth token error: {e}"))
                    .into_response();
            }
        }
    }

    // --- Context Engine: apply pre-processing ---
    super::context_engine::apply_context_engine(
        &mut body_value,
        &state,
        &resolved_profile_name,
        &context_config,
        &full_config,
    )
    .await;

    let is_streaming = body_value
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // --- Circuit Breaker + Failover ---
    // Try primary provider
    let mut primary_result =
        try_with_circuit_breaker(&state, &profile, &headers, &body_value, is_streaming).await;

    // 401 retry: OAuth profile 的 token 可能已过期，清除缓存重试一次
    if let Ok(ref response) = primary_result {
        if response.status() == StatusCode::UNAUTHORIZED && profile.auth_type == AuthType::OAuth {
            tracing::info!(
                profile = %profile.name,
                "got 401, invalidating token cache and retrying"
            );
            match state.token_manager.invalidate_and_retry(&profile).await {
                Ok(new_token) => {
                    crate::oauth::manager::apply_token_to_profile(&mut profile, &new_token);
                    primary_result = try_with_circuit_breaker(
                        &state,
                        &profile,
                        &headers,
                        &body_value,
                        is_streaming,
                    )
                    .await;
                }
                Err(e) => {
                    tracing::warn!(
                        profile = %profile.name,
                        error = %e,
                        "token refresh after 401 failed"
                    );
                }
            }
        }
    }

    let result = match primary_result {
        Ok(response) => Ok(response),
        Err(primary_err) => {
            tracing::warn!(
                profile = %profile.name,
                error = %primary_err,
                "primary provider failed, trying backups"
            );

            // Try backup providers in order
            let mut last_err = primary_err;
            let mut success = None;

            for backup in &backup_profiles {
                match try_with_circuit_breaker(&state, backup, &headers, &body_value, is_streaming)
                    .await
                {
                    Ok(response) => {
                        tracing::info!(
                            backup = %backup.name,
                            "failover succeeded"
                        );
                        success = Some(response);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            backup = %backup.name,
                            error = %e,
                            "backup provider also failed"
                        );
                        last_err = e;
                    }
                }
            }

            match success {
                Some(response) => Ok(response),
                None => Err(last_err),
            }
        }
    };

    let latency = start.elapsed();

    match result {
        Ok(response) => {
            metrics.record_request(true, latency, 0);
            response
        }
        Err(e) => {
            metrics.record_request(false, latency, 0);
            tracing::error!(profile = %resolved_profile_name, error = %e, "proxy request failed");
            (StatusCode::BAD_GATEWAY, format!("proxy error: {e}")).into_response()
        }
    }
}

/// Resolve "auto" profile via smart router
async fn resolve_auto_profile(state: &ProxyState, body: &Value) -> String {
    let config = state.config.read().await;

    if !config.router.enabled {
        let default = config.router.resolve_profile("default").unwrap_or_else(|| {
            config
                .enabled_profiles()
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "default".to_string())
        });
        return default;
    }

    let router_config = config.router.clone();

    // Resolve classifier profile endpoint
    let endpoint = crate::context::resolve_profile_endpoint(
        &config,
        &router_config.profile,
        &router_config.model,
    );
    drop(config);

    let user_message = classifier::extract_last_user_message(body).unwrap_or_default();

    if user_message.is_empty() {
        return router_config
            .resolve_profile("default")
            .unwrap_or_else(|| "default".to_string());
    }

    let (base_url, api_key, model) = match endpoint {
        Some(v) => v,
        None => {
            tracing::warn!(
                profile = %router_config.profile,
                "router classifier profile not found, using default"
            );
            return router_config
                .resolve_profile("default")
                .unwrap_or_else(|| "default".to_string());
        }
    };

    match classifier::classify_intent(
        &base_url,
        &api_key,
        &model,
        &user_message,
        &state.http_client,
    )
    .await
    {
        Ok(intent) => {
            let profile_name = router_config.resolve_profile(&intent).unwrap_or_else(|| {
                router_config
                    .resolve_profile("default")
                    .unwrap_or_else(|| "default".to_string())
            });
            tracing::info!(intent = %intent, profile = %profile_name, "smart routing resolved");
            profile_name
        }
        Err(e) => {
            tracing::warn!(error = %e, "intent classification failed, using default");
            router_config
                .resolve_profile("default")
                .unwrap_or_else(|| "default".to_string())
        }
    }
}

/// Try forwarding to a single provider with circuit breaker protection
async fn try_with_circuit_breaker(
    state: &ProxyState,
    profile: &ProfileConfig,
    headers: &HeaderMap,
    body: &Value,
    is_streaming: bool,
) -> anyhow::Result<Response> {
    // Check circuit breaker (single lock scope to avoid race condition)
    {
        let mut map = state.circuit_breakers.write().await;
        let cb = map
            .entry(profile.name.clone())
            .or_insert_with(Default::default);
        if !cb.can_attempt() {
            anyhow::bail!("circuit breaker open for profile '{}'", profile.name);
        }
    }
    // Lock is released here — forward can take seconds, don't hold it

    let result = try_forward(state, profile, headers, body, is_streaming).await;

    // Record result atomically
    let mut map = state.circuit_breakers.write().await;
    let cb = map
        .entry(profile.name.clone())
        .or_insert_with(Default::default);
    match &result {
        Ok(_) => cb.record_success(),
        Err(_) => cb.record_failure(),
    }
    drop(map);

    result
}

/// Forward request to a single provider (used for both primary and backup).
/// Uses ProviderAdapter trait to handle provider-specific translation.
async fn try_forward(
    state: &ProxyState,
    profile: &ProfileConfig,
    _headers: &HeaderMap,
    body: &Value,
    is_streaming: bool,
) -> anyhow::Result<Response> {
    let adapter = super::adapter::for_provider(&profile.provider_type);
    let mut translated = adapter.translate_request(body, profile)?;
    adapter.filter_translated_body(&mut translated.body, profile);
    let upstream_is_streaming = translated
        .body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(is_streaming);

    let mut url = format!(
        "{}{}",
        profile.base_url.trim_end_matches('/'),
        adapter.endpoint_path()
    );
    if !profile.query_params.is_empty() {
        let qs: String = profile
            .query_params
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        url = if url.contains('?') {
            format!("{url}&{qs}")
        } else {
            format!("{url}?{qs}")
        };
    }
    let key_preview = super::util::format_key_preview(&profile.api_key);

    tracing::info!(
        profile = %profile.name,
        url = %url,
        api_key = %key_preview,
        streaming = %upstream_is_streaming,
        model = %translated.body.get("model").and_then(|v| v.as_str()).unwrap_or("-"),
        "forwarding request"
    );

    // Debug: log translated request body (truncated)
    if tracing::enabled!(tracing::Level::DEBUG) {
        let body_str = serde_json::to_string(&translated.body).unwrap_or_default();
        let preview = if body_str.len() > 2000 {
            format!(
                "{}...(truncated, total {} bytes)",
                truncate_at_char_boundary(&body_str, 2000),
                body_str.len()
            )
        } else {
            body_str
        };
        tracing::debug!(
            profile = %profile.name,
            body = %preview,
            "translated request body"
        );
    }

    let mut req = state
        .http_client
        .post(&url)
        .header("content-type", "application/json");

    req = adapter.apply_auth(req, profile);
    req = adapter.apply_extra_headers(req, profile);

    for (k, v) in &profile.custom_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    req = req.json(&translated.body);

    let resp = req.send().await?;
    let status = resp.status();

    tracing::info!(
        profile = %profile.name,
        status = %status,
        "upstream response"
    );

    if adapter.passthrough() {
        // Direct passthrough (e.g., DirectAnthropic): no error/response translation
        tracing::debug!(
            profile = %profile.name,
            content_type = ?resp.headers().get("content-type"),
            transfer_encoding = ?resp.headers().get("transfer-encoding"),
            content_length = ?resp.headers().get("content-length"),
            streaming = is_streaming,
            "passthrough: upstream response headers"
        );

        if is_streaming {
            let stream = resp.bytes_stream();
            let response = Response::builder()
                .status(status.as_u16())
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(Body::from_stream(stream))
                .map_err(|e| anyhow::anyhow!("failed to build response: {e}"))?;
            Ok(response)
        } else {
            let resp_bytes = resp.bytes().await?;
            tracing::debug!(
                profile = %profile.name,
                len = resp_bytes.len(),
                "passthrough: non-streaming response received"
            );
            if let Ok(resp_json) = serde_json::from_slice::<Value>(&resp_bytes) {
                extract_and_store_context(state, &profile.name, &resp_json);
            }
            let response = Response::builder()
                .status(status.as_u16())
                .header("content-type", "application/json")
                .body(Body::from(resp_bytes))
                .map_err(|e| anyhow::anyhow!("failed to build response: {e}"))?;
            Ok(response)
        }
    } else {
        // Translated path: handle errors, then translate response
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();

            if status.is_client_error() {
                // 4xx: non-retryable, translate to Anthropic error format
                tracing::warn!(
                    profile = %profile.name,
                    status = %status,
                    body = %err_body,
                    "client error (non-retryable)"
                );
                let anthropic_err = super::util::to_anthropic_error(status.as_u16(), &err_body);
                let response = Response::builder()
                    .status(status.as_u16())
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&anthropic_err).unwrap_or_default(),
                    ))
                    .map_err(|e| anyhow::anyhow!("failed to build error response: {e}"))?;
                return Ok(response);
            }

            // 5xx: retryable, bail for circuit breaker + failover
            tracing::error!(
                profile = %profile.name,
                status = %status,
                body = %err_body,
                "upstream error"
            );
            anyhow::bail!("upstream returned HTTP {status}: {err_body}");
        }

        if upstream_is_streaming {
            let stream = resp.bytes_stream();
            let translated_stream =
                adapter.translate_stream(Box::pin(stream), translated.tool_name_map);
            let response = Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(Body::from_stream(translated_stream))
                .map_err(|e| anyhow::anyhow!("failed to build response: {e}"))?;
            Ok(response)
        } else {
            let resp_json: Value = resp.json().await?;
            let anthropic_resp =
                adapter.translate_response(&resp_json, &translated.tool_name_map)?;
            extract_and_store_context(state, &profile.name, &anthropic_resp);
            let response = Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&anthropic_resp)?))
                .map_err(|e| anyhow::anyhow!("failed to build response: {e}"))?;
            Ok(response)
        }
    }
}

/// Extract assistant text from an Anthropic-format response and store for sharing.
fn extract_and_store_context(state: &ProxyState, profile_name: &str, resp_body: &Value) {
    let text = resp_body
        .get("content")
        .and_then(|c| c.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    if text.len() >= 100 {
        let truncated = if text.len() > 500 {
            format!("{}...", truncate_at_char_boundary(&text, 500))
        } else {
            text
        };
        let shared_context = state.shared_context.clone();
        let name = profile_name.to_string();
        tokio::spawn(async move {
            shared_context.store(&name, truncated).await;
        });
    }
}

/// Truncate a string at the given byte limit, ensuring we don't split a multi-byte UTF-8 character.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ImageHistoryStats {
    kept_message_index: Option<usize>,
    omitted_images: usize,
    omitted_base64_bytes: usize,
}

fn prune_historical_images(body: &mut Value) -> ImageHistoryStats {
    let messages = match body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(messages) => messages,
        None => return ImageHistoryStats::default(),
    };

    let kept_message_index = messages.iter().rposition(message_has_image);
    let mut stats = ImageHistoryStats {
        kept_message_index,
        ..ImageHistoryStats::default()
    };

    for (index, message) in messages.iter_mut().enumerate() {
        if Some(index) != kept_message_index {
            prune_images_in_message(message, &mut stats);
        }
    }

    stats
}

fn message_has_image(message: &Value) -> bool {
    message
        .get("content")
        .map(content_has_image)
        .unwrap_or(false)
}

fn content_has_image(content: &Value) -> bool {
    match content {
        Value::Array(parts) => parts.iter().any(|part| {
            part.get("type").and_then(|t| t.as_str()) == Some("image")
                || part.get("content").map(content_has_image).unwrap_or(false)
        }),
        _ => false,
    }
}

fn prune_images_in_message(message: &mut Value, stats: &mut ImageHistoryStats) {
    if let Some(content) = message.get_mut("content") {
        prune_images_in_content(content, stats);
    }
}

fn prune_images_in_content(content: &mut Value, stats: &mut ImageHistoryStats) {
    let parts = match content.as_array_mut() {
        Some(parts) => parts,
        None => return,
    };

    let mut replacement = Vec::with_capacity(parts.len());
    for mut part in std::mem::take(parts) {
        if part.get("type").and_then(|t| t.as_str()) == Some("image") {
            stats.omitted_images += 1;
            let (media_type, approx_bytes) = image_metadata(&part);
            stats.omitted_base64_bytes += approx_bytes;
            replacement.push(json_text_block(format!(
                "{IMAGE_HISTORY_PLACEHOLDER_PREFIX}: {media_type}, approx {approx_bytes} base64 bytes. Re-attach the image if visual details are needed.]"
            )));
            continue;
        }

        if let Some(nested) = part.get_mut("content") {
            prune_images_in_content(nested, stats);
        }
        replacement.push(part);
    }

    *parts = replacement;
}

fn image_metadata(image: &Value) -> (&str, usize) {
    let source = image.get("source");
    let media_type = source
        .and_then(|s| s.get("media_type"))
        .and_then(|m| m.as_str())
        .unwrap_or("image/*");
    let approx_bytes = source
        .and_then(|s| s.get("data"))
        .and_then(|d| d.as_str())
        .map(str::len)
        .unwrap_or(0);
    (media_type, approx_bytes)
}

fn json_text_block(text: String) -> Value {
    serde_json::json!({
        "type": "text",
        "text": text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── truncate_at_char_boundary ──

    #[test]
    fn test_truncate_ascii_within_limit() {
        assert_eq!(truncate_at_char_boundary("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_ascii_at_limit() {
        assert_eq!(truncate_at_char_boundary("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_ascii_over_limit() {
        assert_eq!(truncate_at_char_boundary("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_utf8_boundary() {
        // "日本語" is 3 chars, each 3 bytes = 9 bytes total
        let s = "日本語";
        // Truncating at 4 bytes should give us just "日" (3 bytes)
        assert_eq!(truncate_at_char_boundary(s, 4), "日");
        // Truncating at 6 bytes should give us "日本"
        assert_eq!(truncate_at_char_boundary(s, 6), "日本");
    }

    #[test]
    fn test_truncate_empty_string() {
        assert_eq!(truncate_at_char_boundary("", 0), "");
        assert_eq!(truncate_at_char_boundary("", 10), "");
    }

    #[test]
    fn test_truncate_zero_length() {
        assert_eq!(truncate_at_char_boundary("hello", 0), "");
    }

    // ── extract_and_store_context ──

    #[test]
    fn test_extract_text_from_response() {
        let resp = serde_json::json!({
            "content": [
                {"type": "text", "text": "Hello world"},
                {"type": "text", "text": " more text"}
            ]
        });
        let text = resp
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        assert_eq!(text, "Hello world\n more text");
    }

    #[test]
    fn test_extract_skips_tool_use_blocks() {
        let resp = serde_json::json!({
            "content": [
                {"type": "tool_use", "name": "test"},
                {"type": "text", "text": "Only text"}
            ]
        });
        let text = resp
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        assert_eq!(text, "Only text");
    }

    #[test]
    fn test_extract_empty_content() {
        let resp = serde_json::json!({"content": []});
        let text = resp
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        assert!(text.is_empty());
    }

    #[test]
    fn test_extract_no_content_field() {
        let resp = serde_json::json!({"role": "assistant"});
        let text = resp
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        assert!(text.is_empty());
    }

    #[test]
    fn test_prune_keeps_newest_image_message_and_prunes_older_images() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "old screenshot"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aaaa"}}
                ]},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": [
                    {"type": "text", "text": "new screenshot"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "bbbb"}}
                ]}
            ]
        });

        let stats = prune_historical_images(&mut body);

        assert_eq!(stats.kept_message_index, Some(2));
        assert_eq!(stats.omitted_images, 1);
        assert_eq!(stats.omitted_base64_bytes, 4);
        let old_content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(old_content.len(), 2);
        assert_eq!(old_content[1]["type"], "text");
        assert!(old_content[1]["text"]
            .as_str()
            .unwrap()
            .contains("Previous image omitted by claudex"));
        assert_eq!(body["messages"][2]["content"][1]["type"], "image");
    }

    #[test]
    fn test_prune_keeps_last_image_across_followup_text_turns() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aaaa"}}
                ]},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": "what about that image?"}
            ]
        });

        let stats = prune_historical_images(&mut body);

        assert_eq!(stats.kept_message_index, Some(0));
        assert_eq!(stats.omitted_images, 0);
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
    }

    #[test]
    fn test_prune_images_inside_old_tool_results() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                        {"type": "text", "text": "Image loaded."},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/webp", "data": "cccccc"}}
                    ]}
                ]},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "dddd"}}
                ]}
            ]
        });

        let stats = prune_historical_images(&mut body);

        assert_eq!(stats.kept_message_index, Some(2));
        assert_eq!(stats.omitted_images, 1);
        assert_eq!(stats.omitted_base64_bytes, 6);
        let nested = body["messages"][0]["content"][0]["content"]
            .as_array()
            .unwrap();
        assert_eq!(nested.len(), 2);
        assert_eq!(nested[1]["type"], "text");
        assert!(nested[1]["text"].as_str().unwrap().contains("image/webp"));
    }
}
