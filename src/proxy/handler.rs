use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::config::{ProfileConfig, ProviderType};
use crate::oauth::AuthType;
use crate::proxy::error_translation::{self, AnthropicError, CircuitDecision};
use crate::proxy::ProxyState;
use crate::router::classifier;

const IMAGE_HISTORY_PLACEHOLDER_PREFIX: &str = "[Previous image omitted by claudex";
const COMPACT_PROMPT_SNIPPET_RADIUS_BYTES: usize = 700;
const COMPACT_PROMPT_MAX_SNIPPETS: usize = 24;
const COMPACT_PROMPT_MAX_INSTRUCTIONS_BYTES: usize = 64 * 1024;
const COMPACT_COMMAND_PATTERNS: &[&str] = &[
    "<command-name>/compact</command-name>",
    "<command-message>compact</command-message>",
];
const COMPACT_DIRECTIVE_PATTERNS: &[&str] = &[
    "create a detailed summary",
    "detailed summary",
    "respond with text only",
    "do not call any tools",
    "pending tasks",
    "current work",
    "previous conversation",
    "ran out of context",
    "summary below covers",
    "compact",
];
const COMPACT_ADDITIONAL_INSTRUCTION: &str = "\
Additional claudex compaction instruction:
For this compaction response, produce a concise continuation handoff, not a full historical narrative.
Prefer current actionable state over exhaustive chronology.
Target 800-1500 words unless essential details require more.
Include: current goal, decisions made, relevant files, commands/tests run, blockers, and exact next step.
Avoid repeating old summaries, long transcripts, or narrative background.";

#[derive(Debug)]
struct TranslatedProxyError {
    profile: String,
    url: String,
    upstream_status: u16,
    request: Value,
    response: Value,
    anthropic: AnthropicError,
}

impl std::fmt::Display for TranslatedProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.anthropic.message)
    }
}

impl std::error::Error for TranslatedProxyError {}

pub async fn handle_messages(
    State(state): State<Arc<ProxyState>>,
    Path(profile_name): Path<String>,
    headers: HeaderMap,
    body: Result<axum::body::Bytes, BytesRejection>,
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
    let content_length = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("(none)");
    let body = match body {
        Ok(body) => body,
        Err(err) => {
            tracing::warn!(
                profile = %profile_name,
                authorization = %auth_header,
                x_api_key = %api_key_header,
                content_length = %content_length,
                error = %err,
                "request body rejected before proxy translation"
            );
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("request body too large or unreadable before proxy translation: {err}"),
            )
                .into_response();
        }
    };

    tracing::info!(
        profile = %profile_name,
        authorization = %auth_header,
        x_api_key = %api_key_header,
        content_length = %content_length,
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
            if let Some(translated) = e.downcast_ref::<TranslatedProxyError>() {
                tracing::error!(
                    profile = %translated.profile,
                    error = %translated.anthropic.message,
                    "proxy request failed with translated provider error"
                );
                dump_proxy_error(
                    &translated.profile,
                    "translated_proxy_error",
                    &translated.url,
                    translated.upstream_status,
                    &translated.request,
                    &translated.response,
                    Some(&translated.anthropic.json()),
                );
                return Response::builder()
                    .status(translated.anthropic.http_status)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&translated.anthropic.json()).unwrap_or_default(),
                    ))
                    .unwrap_or_else(|_| {
                        (
                            StatusCode::BAD_GATEWAY,
                            format!("proxy error: {}", translated.anthropic.message),
                        )
                            .into_response()
                    });
            }
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
        Err(err) if circuit_decision_for_error(err) == CircuitDecision::Retryable => {
            cb.record_failure();
        }
        Err(err) => {
            tracing::info!(
                profile = %profile.name,
                error = %err,
                "not recording deterministic provider error against circuit breaker"
            );
        }
    }
    drop(map);

    result
}

fn circuit_decision_for_error(err: &anyhow::Error) -> CircuitDecision {
    err.downcast_ref::<TranslatedProxyError>()
        .map(|translated| error_translation::circuit_decision(&translated.anthropic))
        .unwrap_or(CircuitDecision::Retryable)
}

/// Forward request to a single provider (used for both primary and backup).
/// Uses ProviderAdapter trait to handle provider-specific translation.
async fn try_forward(
    state: &ProxyState,
    profile: &ProfileConfig,
    headers: &HeaderMap,
    body: &Value,
    is_streaming: bool,
) -> anyhow::Result<Response> {
    let adapter = super::adapter::for_provider(&profile.provider_type);
    let mut body_for_translation = body.clone();
    apply_metadata_from_headers(&mut body_for_translation, headers, profile);
    let mut translated = adapter.translate_request(&body_for_translation, profile)?;
    adapter.filter_translated_body(&mut translated.body, profile);
    let compact_request =
        apply_compact_prompt_overrides(&body_for_translation, &mut translated.body);
    let translated_model = translated
        .body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("<none>");
    let current_image_route = profile.image_model.is_some()
        && crate::proxy::translate::responses::request_has_current_image(&body_for_translation);
    tracing::info!(
        profile = %profile.name,
        provider = %profile.provider_type,
        model = %translated_model,
        current_image_route,
        "translated upstream request model"
    );
    if compact_request {
        tracing::info!(
            profile = %profile.name,
            "applied compact prompt continuation-handoff instruction"
        );
    }
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

    if compact_request {
        dump_compact_prompt_audit(&profile.name, &url, &body_for_translation, &translated.body);
    }

    let mut attempts = 0;
    let resp = loop {
        let mut req = state
            .http_client
            .post(&url)
            .header("content-type", "application/json");

        req = adapter.apply_auth(req, profile);
        req = adapter.apply_extra_headers(req, profile);

        for (k, v) in &profile.custom_headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = match req.json(&translated.body).send().await {
            Ok(resp) => resp,
            Err(err) => {
                let anthropic =
                    error_translation::from_stream_transport(&err.to_string(), Some(&url));
                return Err(TranslatedProxyError {
                    profile: profile.name.clone(),
                    url: url.clone(),
                    upstream_status: anthropic.http_status.as_u16(),
                    request: translated.body.clone(),
                    response: json!(err.to_string()),
                    anthropic,
                }
                .into());
            }
        };
        if resp.status() == StatusCode::TOO_MANY_REQUESTS && attempts < 2 {
            let delay = retry_after_delay(resp.headers());
            let err_body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                profile = %profile.name,
                retry_after_ms = delay.as_millis(),
                body = %err_body,
                "upstream rate-limited request, retrying"
            );
            attempts += 1;
            tokio::time::sleep(delay).await;
            continue;
        }
        break resp;
    };
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
                let anthropic_err = error_translation::from_http_status(status, &err_body);
                dump_proxy_error(
                    &profile.name,
                    "openai_error",
                    &url,
                    status.as_u16(),
                    &translated.body,
                    &err_body,
                    Some(&anthropic_err.json()),
                );
                let response = Response::builder()
                    .status(anthropic_err.http_status)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&anthropic_err.json()).unwrap_or_default(),
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
            dump_proxy_error(
                &profile.name,
                "openai_error",
                &url,
                status.as_u16(),
                &translated.body,
                &err_body,
                None,
            );
            anyhow::bail!("upstream returned HTTP {status}: {err_body}");
        }

        if upstream_is_streaming {
            let stream = resp.bytes_stream();
            if profile.provider_type == ProviderType::OpenAIResponses {
                let preflight = preflight_openai_responses_stream(Box::pin(stream)).await?;
                if preflight.context_overflow {
                    let anthropic_err = json!({
                        "type": "error",
                        "error": {
                            "type": "invalid_request_error",
                            "message": "prompt is too long",
                        }
                    });
                    dump_proxy_error(
                        &profile.name,
                        "claude_error",
                        &url,
                        status.as_u16(),
                        &translated.body,
                        String::from_utf8_lossy(&preflight.buffered).to_string(),
                        Some(&anthropic_err),
                    );
                    let response = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&anthropic_err)?))
                        .map_err(|e| anyhow::anyhow!("failed to build error response: {e}"))?;
                    return Ok(response);
                }

                if let Some(anthropic_err) = preflight.provider_error {
                    dump_proxy_error(
                        &profile.name,
                        "claude_error",
                        &url,
                        status.as_u16(),
                        &translated.body,
                        String::from_utf8_lossy(&preflight.buffered).to_string(),
                        Some(&anthropic_err.json()),
                    );
                    let response = Response::builder()
                        .status(anthropic_err.http_status)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&anthropic_err.json())?))
                        .map_err(|e| anyhow::anyhow!("failed to build error response: {e}"))?;
                    return Ok(response);
                }

                if let Some(anthropic_err) = preflight.transport_error {
                    dump_proxy_error(
                        &profile.name,
                        "claude_error",
                        &url,
                        status.as_u16(),
                        &translated.body,
                        String::from_utf8_lossy(&preflight.buffered).to_string(),
                        Some(&anthropic_err.json()),
                    );
                    let response = Response::builder()
                        .status(anthropic_err.http_status)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&anthropic_err.json())?))
                        .map_err(|e| anyhow::anyhow!("failed to build error response: {e}"))?;
                    return Ok(response);
                }

                let (stream, upstream_capture) = capture_stream(preflight.stream);
                let translated_stream =
                    adapter.translate_stream(Box::pin(stream), translated.tool_name_map);
                let dumped_stream = dump_claude_stream_errors(
                    translated_stream,
                    profile.name.clone(),
                    url.clone(),
                    status.as_u16(),
                    translated.body.clone(),
                    upstream_capture,
                );
                let response = Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(Body::from_stream(dumped_stream))
                    .map_err(|e| anyhow::anyhow!("failed to build response: {e}"))?;
                return Ok(response);
            }

            let (stream, upstream_capture) = capture_stream(stream);
            let translated_stream =
                adapter.translate_stream(Box::pin(stream), translated.tool_name_map);
            let dumped_stream = dump_claude_stream_errors(
                translated_stream,
                profile.name.clone(),
                url.clone(),
                status.as_u16(),
                translated.body.clone(),
                upstream_capture,
            );
            let response = Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(Body::from_stream(dumped_stream))
                .map_err(|e| anyhow::anyhow!("failed to build response: {e}"))?;
            Ok(response)
        } else {
            let resp_json: Value = resp.json().await?;
            let anthropic_resp =
                adapter.translate_response(&resp_json, &translated.tool_name_map)?;
            if anthropic_resp.get("type").and_then(|v| v.as_str()) == Some("error") {
                dump_proxy_error(
                    &profile.name,
                    "claude_error",
                    &url,
                    status.as_u16(),
                    &translated.body,
                    &resp_json,
                    Some(&anthropic_resp),
                );
            }
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

fn apply_metadata_from_headers(body: &mut Value, headers: &HeaderMap, profile: &ProfileConfig) {
    if profile.provider_type != ProviderType::OpenAIResponses {
        return;
    }

    let Some(session_id) = headers
        .get("x-claude-code-session-id")
        .or_else(|| headers.get("anthropic-session-id"))
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    else {
        return;
    };

    if body.pointer("/metadata/session_id").is_some() {
        return;
    }

    if !body.get("metadata").is_some_and(|v| v.is_object()) {
        body["metadata"] = Value::Object(Map::new());
    }
    body["metadata"]["session_id"] = Value::String(session_id.to_string());
}

fn retry_after_delay(headers: &HeaderMap) -> Duration {
    let seconds = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1);
    Duration::from_secs(seconds.min(5))
}

const PREFLIGHT_MAX_BYTES: usize = 2 * 1024 * 1024;

struct ResponsesStreamPreflight {
    context_overflow: bool,
    provider_error: Option<AnthropicError>,
    transport_error: Option<AnthropicError>,
    buffered: Vec<u8>,
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
}

async fn preflight_openai_responses_stream(
    input: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
) -> anyhow::Result<ResponsesStreamPreflight> {
    let mut stream = input;
    let mut buffered = Vec::new();
    let mut context_overflow = false;
    let mut provider_error = None;
    let mut transport_error = None;

    while buffered.len() < PREFLIGHT_MAX_BYTES {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                transport_error = Some(error_translation::from_stream_transport(
                    &err.to_string(),
                    None,
                ));
                break;
            }
        };
        buffered.extend_from_slice(&chunk);
        provider_error = buffered_provider_error(&buffered);
        if provider_error.is_some() {
            break;
        }
        if buffered_has_context_overflow(&buffered) {
            context_overflow = true;
            break;
        }
        if buffered_has_translatable_responses_content(&buffered) {
            break;
        }
    }

    let replay = buffered.clone();
    let output = async_stream::stream! {
        if !replay.is_empty() {
            yield Ok(Bytes::from(replay));
        }
        while let Some(chunk) = stream.next().await {
            yield chunk;
        }
    };

    Ok(ResponsesStreamPreflight {
        context_overflow,
        provider_error,
        transport_error,
        buffered,
        stream: Box::pin(output),
    })
}

fn buffered_provider_error(buffered: &[u8]) -> Option<AnthropicError> {
    let mut verification_recommendation = None;
    for event in parse_buffered_sse_json(buffered) {
        if let Some(recommendation) = verification_recommendation_from_event(&event) {
            verification_recommendation = Some(recommendation);
        }
        if let Some(error) = error_translation::from_responses_event(&event) {
            if error.error_type != "invalid_request_error"
                || !error_translation::is_context_overflow_text(&error.message)
            {
                return Some(
                    verification_recommendation
                        .as_deref()
                        .map(verification_recommendation_error)
                        .unwrap_or(error),
                );
            }
        }
    }
    None
}

fn verification_recommendation_from_event(event: &Value) -> Option<String> {
    event
        .pointer("/metadata/openai_verification_recommendation/0")
        .and_then(|v| v.as_str())
        .or_else(|| {
            event
                .pointer("/response/metadata/openai_verification_recommendation/0")
                .and_then(|v| v.as_str())
        })
        .map(ToOwned::to_owned)
}

fn verification_recommendation_error(recommendation: &str) -> AnthropicError {
    let message = if recommendation == "trusted_access_for_cyber" {
        "OpenAI requires trusted access for cyber for this request: trusted_access_for_cyber"
            .to_string()
    } else {
        format!("OpenAI requires account verification for this request: {recommendation}")
    };
    AnthropicError::new("permission_error", message, StatusCode::FORBIDDEN)
}

fn parse_buffered_sse_json(buffered: &[u8]) -> Vec<Value> {
    let text = String::from_utf8_lossy(buffered);
    let mut events = text.split("\n\n").collect::<Vec<_>>();
    if !text.ends_with("\n\n") {
        events.pop();
    }
    events
        .into_iter()
        .filter_map(|event| {
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                None
            } else {
                serde_json::from_str(&data).ok()
            }
        })
        .collect()
}

fn buffered_has_context_overflow(buffered: &[u8]) -> bool {
    let text = String::from_utf8_lossy(buffered).to_ascii_lowercase();
    text.contains("context_length_exceeded")
        || (text.contains("context window") && text.contains("exceeds"))
}

fn buffered_has_translatable_responses_content(buffered: &[u8]) -> bool {
    parse_buffered_sse_json(buffered)
        .iter()
        .any(responses_event_has_visible_content)
}

fn responses_event_has_visible_content(event: &Value) -> bool {
    match event.get("type").and_then(|v| v.as_str()) {
        Some("response.output_text.delta") => event
            .get("delta")
            .and_then(|v| v.as_str())
            .is_some_and(|delta| !delta.is_empty()),
        Some("response.output_item.added") => {
            event.pointer("/item/type").and_then(|v| v.as_str()) == Some("function_call")
        }
        Some("response.output_item.done") => event.get("item").is_some_and(message_item_has_text),
        Some("response.function_call_arguments.delta") => event
            .get("delta")
            .and_then(|v| v.as_str())
            .is_some_and(|delta| !delta.is_empty()),
        Some("response.completed") | Some("response.incomplete") | Some("response.failed") => true,
        Some("error") => true,
        _ => false,
    }
}

fn message_item_has_text(item: &Value) -> bool {
    item.get("type").and_then(|v| v.as_str()) == Some("message")
        && item
            .get("content")
            .and_then(|v| v.as_array())
            .is_some_and(|content| {
                content.iter().any(|part| {
                    part.get("type").and_then(|v| v.as_str()) == Some("output_text")
                        && part
                            .get("text")
                            .and_then(|v| v.as_str())
                            .is_some_and(|text| !text.is_empty())
                })
            })
}

fn capture_stream<S>(
    input: S,
) -> (
    impl Stream<Item = Result<Bytes, reqwest::Error>> + Send,
    Arc<Mutex<Vec<u8>>>,
)
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let capture = Arc::new(Mutex::new(Vec::new()));
    let stream_capture = capture.clone();
    let output = async_stream::stream! {
        let mut stream = std::pin::pin!(input);
        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = &chunk {
                if let Ok(mut captured) = stream_capture.lock() {
                    captured.extend_from_slice(bytes);
                }
            }
            yield chunk;
        }
    };
    (output, capture)
}

fn dump_claude_stream_errors<S>(
    input: S,
    profile: String,
    url: String,
    upstream_status: u16,
    request: Value,
    upstream_response: Arc<Mutex<Vec<u8>>>,
) -> impl Stream<Item = Result<Bytes, reqwest::Error>> + Send
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    async_stream::stream! {
        let mut stream = std::pin::pin!(input);
        let mut response = Vec::new();
        let mut dumped = false;

        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = &chunk {
                response.extend_from_slice(bytes);
                if !dumped && stream_chunk_has_error_event(bytes) {
                    dumped = true;
                }
            }
            yield chunk;
        }

        if dumped {
            let response_text = String::from_utf8_lossy(&response).to_string();
            let upstream_response_text = upstream_response
                .lock()
                .map(|captured| String::from_utf8_lossy(&captured).to_string())
                .unwrap_or_default();
            dump_proxy_error(
                &profile,
                "claude_error",
                &url,
                upstream_status,
                &request,
                json!({
                    "upstream": upstream_response_text,
                    "downstream": response_text,
                }),
                None,
            );
        }
    }
}

fn stream_chunk_has_error_event(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .is_ok_and(|chunk| chunk.contains("event: error") || chunk.contains("\"type\":\"error\""))
}

fn is_compact_prompt_audit_request(original: &Value, translated: &Value) -> bool {
    value_contains_any_text(original, COMPACT_COMMAND_PATTERNS)
        || value_contains_compact_summary_directive(original)
        || value_contains_compact_summary_directive(translated)
}

fn apply_compact_prompt_overrides(original: &Value, translated: &mut Value) -> bool {
    if !is_compact_prompt_audit_request(original, translated) {
        return false;
    }

    translated["instructions"] = match translated.get("instructions").and_then(|v| v.as_str()) {
        Some(existing) if !existing.contains(COMPACT_ADDITIONAL_INSTRUCTION) => {
            json!(format!("{existing}\n\n{COMPACT_ADDITIONAL_INSTRUCTION}"))
        }
        Some(existing) => json!(existing),
        None => json!(COMPACT_ADDITIONAL_INSTRUCTION),
    };
    if let Some(map) = translated.as_object_mut() {
        map.remove("reasoning");
    }
    true
}

fn value_contains_compact_summary_directive(value: &Value) -> bool {
    let has_summary_task = value_contains_any_text(
        value,
        &[
            "create a detailed summary",
            "respond with text only",
            "do not call any tools",
        ],
    );
    let has_summary_structure = value_contains_any_text(
        value,
        &[
            "pending tasks",
            "current work",
            "previous conversation",
            "ran out of context",
        ],
    );
    has_summary_task && has_summary_structure
}

fn value_contains_any_text(value: &Value, patterns: &[&str]) -> bool {
    patterns
        .iter()
        .any(|pattern| value_contains_text(value, pattern))
}

fn value_contains_text(value: &Value, pattern: &str) -> bool {
    let needle = pattern.to_ascii_lowercase();
    match value {
        Value::String(text) => text.to_ascii_lowercase().contains(&needle),
        Value::Array(items) => items.iter().any(|item| value_contains_text(item, pattern)),
        Value::Object(map) => map.values().any(|item| value_contains_text(item, pattern)),
        _ => false,
    }
}

fn dump_compact_prompt_audit(profile: &str, url: &str, original: &Value, translated: &Value) {
    let Some(audit) = build_compact_prompt_audit(profile, url, original, translated) else {
        return;
    };
    let Some(dir) = dirs::cache_dir().map(|d| d.join("claudex").join("request-dumps")) else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %err, "failed to create compact prompt audit directory");
        return;
    }

    let safe_profile = safe_dump_component(profile);
    let path = dir.join(format!(
        "{}-{}-{}-compact_prompt_audit.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S%.3f"),
        std::process::id(),
        safe_profile
    ));

    match serde_json::to_vec_pretty(&audit) {
        Ok(bytes) => match write_private_dump_file(&path, &bytes) {
            Ok(()) => tracing::info!(
                path = %path.display(),
                profile,
                "compact prompt audit written"
            ),
            Err(err) => tracing::warn!(
                error = %err,
                path = %path.display(),
                "failed to write compact prompt audit"
            ),
        },
        Err(err) => tracing::warn!(error = %err, "failed to serialize compact prompt audit"),
    }
}

fn build_compact_prompt_audit(
    profile: &str,
    url: &str,
    original: &Value,
    translated: &Value,
) -> Option<Value> {
    if !is_compact_prompt_audit_request(original, translated) {
        return None;
    }

    let mut snippets = Vec::new();
    collect_directive_snippets("original", original, &mut snippets);
    collect_directive_snippets("translated", translated, &mut snippets);
    snippets.truncate(COMPACT_PROMPT_MAX_SNIPPETS);

    Some(json!({
        "kind": "compact_prompt_audit",
        "profile": profile,
        "url": url,
        "model": translated.get("model").and_then(|v| v.as_str()),
        "stream": translated.get("stream").and_then(|v| v.as_bool()),
        "store": translated.get("store").and_then(|v| v.as_bool()),
        "original_request": request_summary(original),
        "translated_request": request_summary(translated),
        "instructions": translated
            .get("instructions")
            .and_then(|v| v.as_str())
            .map(instruction_audit),
        "compact_command": find_first_directive_snippet(original, COMPACT_COMMAND_PATTERNS)
            .or_else(|| find_first_directive_snippet(translated, COMPACT_COMMAND_PATTERNS)),
        "directive_snippets": snippets,
    }))
}

fn request_summary(value: &Value) -> Value {
    json!({
        "keys": value_keys(value),
        "json_bytes": serde_json::to_vec(value).map(|bytes| bytes.len()).ok(),
        "sha256": sha256_json(value),
        "messages": value
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|items| shape_array(items)),
        "input": value
            .get("input")
            .and_then(|v| v.as_array())
            .map(|items| shape_array(items)),
    })
}

fn instruction_audit(text: &str) -> Value {
    let truncated = if text.len() > COMPACT_PROMPT_MAX_INSTRUCTIONS_BYTES {
        format!(
            "{}...",
            truncate_at_char_boundary(text, COMPACT_PROMPT_MAX_INSTRUCTIONS_BYTES)
        )
    } else {
        text.to_string()
    };
    json!({
        "chars": text.chars().count(),
        "bytes": text.len(),
        "sha256": sha256_bytes(text.as_bytes()),
        "truncated": text.len() > COMPACT_PROMPT_MAX_INSTRUCTIONS_BYTES,
        "text": truncated,
    })
}

fn value_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn shape_array(items: &[Value]) -> Value {
    Value::Array(
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                json!({
                    "index": index,
                    "role": item.get("role").and_then(|v| v.as_str()),
                    "type": item.get("type").and_then(|v| v.as_str()),
                    "keys": value_keys(item),
                    "json_bytes": serde_json::to_vec(item).map(|bytes| bytes.len()).ok(),
                    "sha256": sha256_json(item),
                    "string_field_count": count_string_fields(item),
                })
            })
            .collect(),
    )
}

fn count_string_fields(value: &Value) -> usize {
    match value {
        Value::String(_) => 1,
        Value::Array(items) => items.iter().map(count_string_fields).sum(),
        Value::Object(map) => map.values().map(count_string_fields).sum(),
        _ => 0,
    }
}

fn find_first_directive_snippet(value: &Value, patterns: &[&str]) -> Option<Value> {
    let mut snippets = Vec::new();
    collect_snippets_for_patterns("request", "$", value, patterns, &mut snippets);
    snippets.into_iter().next()
}

fn collect_directive_snippets(source: &str, value: &Value, snippets: &mut Vec<Value>) {
    collect_snippets_for_patterns(source, "$", value, COMPACT_DIRECTIVE_PATTERNS, snippets);
}

fn collect_snippets_for_patterns(
    source: &str,
    path: &str,
    value: &Value,
    patterns: &[&str],
    snippets: &mut Vec<Value>,
) {
    if snippets.len() >= COMPACT_PROMPT_MAX_SNIPPETS {
        return;
    }

    match value {
        Value::String(text) => {
            if let Some((pattern, position)) = first_pattern_match(text, patterns) {
                snippets.push(json!({
                    "source": source,
                    "path": path,
                    "pattern": pattern,
                    "chars": text.chars().count(),
                    "bytes": text.len(),
                    "sha256": sha256_bytes(text.as_bytes()),
                    "text": snippet_around(text, position, pattern.len()),
                }));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_snippets_for_patterns(
                    source,
                    &format!("{path}[{index}]"),
                    item,
                    patterns,
                    snippets,
                );
                if snippets.len() >= COMPACT_PROMPT_MAX_SNIPPETS {
                    break;
                }
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                collect_snippets_for_patterns(
                    source,
                    &format!("{path}.{key}"),
                    item,
                    patterns,
                    snippets,
                );
                if snippets.len() >= COMPACT_PROMPT_MAX_SNIPPETS {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn first_pattern_match<'a>(text: &str, patterns: &'a [&str]) -> Option<(&'a str, usize)> {
    let lower = text.to_ascii_lowercase();
    patterns
        .iter()
        .filter_map(|pattern| {
            lower
                .find(&pattern.to_ascii_lowercase())
                .map(|position| (*pattern, position))
        })
        .min_by_key(|(_, position)| *position)
}

fn snippet_around(text: &str, position: usize, matched_bytes: usize) -> String {
    let mut start = position.saturating_sub(COMPACT_PROMPT_SNIPPET_RADIUS_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = (position + matched_bytes + COMPACT_PROMPT_SNIPPET_RADIUS_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }

    let mut snippet = String::new();
    if start > 0 {
        snippet.push_str("...");
    }
    snippet.push_str(&text[start..end]);
    if end < text.len() {
        snippet.push_str("...");
    }
    snippet
}

fn sha256_json(value: &Value) -> Option<String> {
    serde_json::to_vec(value)
        .ok()
        .map(|bytes| sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn safe_dump_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
}

fn write_private_dump_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    std::io::Write::write_all(&mut file, bytes)
}

fn dump_proxy_error(
    profile: &str,
    kind: &str,
    url: &str,
    upstream_status: u16,
    request: &Value,
    response: impl serde::Serialize,
    translated_response: Option<&Value>,
) {
    let Some(dir) = dirs::cache_dir().map(|d| d.join("claudex").join("errors")) else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %err, "failed to create proxy error dump directory");
        return;
    }

    let safe_profile = safe_dump_component(profile);
    let path = dir.join(format!(
        "{}-{}-{}-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S%.3f"),
        std::process::id(),
        safe_profile,
        kind
    ));
    let dump = json!({
        "profile": profile,
        "kind": kind,
        "url": url,
        "upstream_status": upstream_status,
        "request": request,
        "response": response,
        "translated_response": translated_response,
    });

    match serde_json::to_vec_pretty(&dump) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(&path, bytes) {
                tracing::warn!(error = %err, path = %path.display(), "failed to write proxy error dump");
            } else {
                tracing::info!(path = %path.display(), kind, profile, "proxy error dump written");
            }
        }
        Err(err) => tracing::warn!(error = %err, "failed to serialize proxy error dump"),
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

    #[test]
    fn deterministic_translated_errors_do_not_count_against_circuit() {
        let err = TranslatedProxyError {
            profile: "codex-sub".to_string(),
            url: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            upstream_status: 400,
            request: json!({"model": "gpt-5.5"}),
            response: json!({
                "error": {
                    "type": "invalid_request_error",
                    "message": "Missing required parameter: 'text.format.name'.",
                    "param": "text.format.name",
                    "code": "missing_required_parameter"
                }
            }),
            anthropic: AnthropicError::new(
                "invalid_request_error",
                "Missing required parameter: 'text.format.name'.",
                StatusCode::BAD_REQUEST,
            ),
        };

        let err: anyhow::Error = err.into();
        assert_eq!(circuit_decision_for_error(&err), CircuitDecision::Direct);
    }

    #[test]
    fn compact_prompt_audit_detects_summary_task_without_dumping_transcript_text() {
        let original = json!({
            "messages": [
                {
                    "role": "user",
                    "content": "ordinary transcript text SECRET_TRANSCRIPT_UNRELATED"
                }
            ]
        });
        let translated = json!({
            "model": "gpt-5.5",
            "stream": true,
            "store": false,
            "instructions": "normal system instructions",
            "input": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.\nYour task is to create a detailed summary of the conversation so far.\nInclude pending tasks and current work."
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "SECRET_TRANSCRIPT_UNRELATED"
                        }
                    ]
                }
            ]
        });

        assert!(is_compact_prompt_audit_request(&original, &translated));
        let audit =
            build_compact_prompt_audit("codex-sub", "https://example.test", &original, &translated)
                .unwrap();
        let serialized = serde_json::to_string(&audit).unwrap();

        assert!(serialized.contains("create a detailed summary"));
        assert!(serialized.contains("directive_snippets"));
        assert!(serialized.contains("sha256"));
        assert!(serialized.contains("json_bytes"));
        assert!(!serialized.contains("SECRET_TRANSCRIPT_UNRELATED"));
    }

    #[test]
    fn compact_prompt_override_appends_instruction_and_removes_reasoning() {
        let original = json!({
            "messages": [
                {
                    "role": "user",
                    "content": "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>"
                }
            ]
        });
        let mut translated = json!({
            "model": "gpt-5.5",
            "instructions": "base instructions",
            "reasoning": {"effort": "high", "summary": "detailed"},
            "stream": true,
            "input": []
        });

        assert!(apply_compact_prompt_overrides(&original, &mut translated));

        let instructions = translated["instructions"].as_str().unwrap();
        assert!(instructions.starts_with("base instructions\n\n"));
        assert!(instructions.contains("Additional claudex compaction instruction"));
        assert!(instructions.contains("Target 800-1500 words"));
        assert!(translated.get("reasoning").is_none());
    }

    #[test]
    fn compact_prompt_override_is_not_applied_to_regular_requests() {
        let original = json!({
            "messages": [
                {
                    "role": "user",
                    "content": "Please summarize this function briefly."
                }
            ]
        });
        let mut translated = json!({
            "model": "gpt-5.5",
            "instructions": "base instructions",
            "reasoning": {"effort": "high", "summary": "detailed"},
            "stream": true,
            "input": []
        });
        let unchanged = translated.clone();

        assert!(!apply_compact_prompt_overrides(&original, &mut translated));
        assert_eq!(translated, unchanged);
    }

    #[test]
    fn compact_prompt_audit_detects_claude_compact_command() {
        let original = json!({
            "messages": [
                {
                    "role": "user",
                    "content": "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>"
                }
            ]
        });
        let translated = json!({
            "model": "gpt-5.5",
            "stream": true,
            "input": []
        });

        assert!(is_compact_prompt_audit_request(&original, &translated));
        let audit =
            build_compact_prompt_audit("codex-sub", "https://example.test", &original, &translated)
                .unwrap();

        assert!(audit["compact_command"]["text"]
            .as_str()
            .unwrap()
            .contains("/compact"));
    }

    #[test]
    fn compact_prompt_audit_ignores_existing_compacted_context_without_task_directive() {
        let original = json!({
            "messages": [
                {
                    "role": "user",
                    "content": "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\nSummary:\n7. Pending Tasks:\n8. Current Work:"
                }
            ]
        });
        let translated = json!({
            "model": "gpt-5.5",
            "stream": true,
            "input": []
        });

        assert!(!is_compact_prompt_audit_request(&original, &translated));
        assert!(build_compact_prompt_audit(
            "codex-sub",
            "https://example.test",
            &original,
            &translated
        )
        .is_none());
    }

    #[test]
    fn transport_errors_count_against_circuit() {
        let err: anyhow::Error = anyhow::anyhow!("connection reset before headers");
        assert_eq!(circuit_decision_for_error(&err), CircuitDecision::Retryable);
    }

    #[tokio::test]
    async fn test_transport_error_records_circuit_breaker_failure() {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let state = ProxyState {
            config: Arc::new(tokio::sync::RwLock::new(
                crate::config::ClaudexConfig::default(),
            )),
            metrics: crate::proxy::metrics::MetricsStore::new(),
            http_client,
            health_status: Arc::new(tokio::sync::RwLock::new(
                crate::proxy::health::HealthMap::new(),
            )),
            circuit_breakers: crate::proxy::fallback::new_circuit_breaker_map(),
            shared_context: crate::context::sharing::SharedContext::new(),
            rag_index: None,
            token_manager: crate::oauth::manager::TokenManager::new(reqwest::Client::new()),
        };
        let profile = ProfileConfig {
            name: "dead-local".to_string(),
            provider_type: ProviderType::OpenAICompatible,
            base_url: "http://127.0.0.1:9/v1".to_string(),
            default_model: "test-model".to_string(),
            api_key: "test".to_string(),
            enabled: true,
            ..ProfileConfig::default()
        };
        let body = json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false
        });

        let result =
            try_with_circuit_breaker(&state, &profile, &HeaderMap::new(), &body, false).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .downcast_ref::<TranslatedProxyError>()
            .is_some());

        let breakers = state.circuit_breakers.read().await;
        let breaker = breakers.get("dead-local").unwrap();
        assert_eq!(breaker.failure_count, 1);
    }

    #[tokio::test]
    async fn test_responses_preflight_detects_context_overflow() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"Your input exceeds the context window\"}}\n\n",
        ))]);

        let preflight = preflight_openai_responses_stream(Box::pin(input))
            .await
            .unwrap();

        assert!(preflight.context_overflow);
        assert!(String::from_utf8_lossy(&preflight.buffered).contains("context_length_exceeded"));
    }

    #[tokio::test]
    async fn test_responses_preflight_ignores_event_names_inside_metadata() {
        let metadata = format!(
            "event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"instructions\":\"mentions response.completed but is not an event\"}}}}\n\n{}",
            "x".repeat(70_000)
        );
        let input = futures::stream::iter(vec![
            Ok(Bytes::from(metadata)),
            Ok(Bytes::from(
                "event: error\ndata: {\"type\":\"error\",\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"Your input exceeds the context window\"}}\n\n",
            )),
        ]);

        let preflight = preflight_openai_responses_stream(Box::pin(input))
            .await
            .unwrap();

        assert!(preflight.context_overflow);
        assert!(String::from_utf8_lossy(&preflight.buffered).contains("context_length_exceeded"));
    }

    #[test]
    fn test_responses_preflight_does_not_treat_hidden_reasoning_as_visible_content() {
        let buffered = b"event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\",\"summary\":[]}}\n\n";

        assert!(!buffered_has_translatable_responses_content(buffered));
    }

    #[test]
    fn test_responses_preflight_treats_function_call_as_visible_content() {
        let buffered = b"event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"name\":\"Read\",\"call_id\":\"call_1\"}}\n\n";

        assert!(buffered_has_translatable_responses_content(buffered));
    }

    #[tokio::test]
    async fn test_responses_preflight_extracts_overloaded_error() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"service_unavailable_error\",\"code\":\"server_is_overloaded\",\"message\":\"Our servers are currently overloaded. Please try again later.\"}}\n\n",
        ))]);

        let preflight = preflight_openai_responses_stream(Box::pin(input))
            .await
            .unwrap();

        let error = preflight.provider_error.unwrap();
        assert_eq!(error.error_type, "overloaded_error");
        assert_eq!(error.http_status, StatusCode::from_u16(529).unwrap());
        assert_eq!(
            error.message,
            "Our servers are currently overloaded. Please try again later."
        );
    }

    #[tokio::test]
    async fn test_responses_preflight_metadata_verification_overrides_overloaded_error() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(concat!(
            "event: response.metadata\n",
            "data: {\"type\":\"response.metadata\",\"metadata\":{\"openai_verification_recommendation\":[\"trusted_access_for_cyber\"]}}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"service_unavailable_error\",\"code\":\"server_is_overloaded\",\"message\":\"Our servers are currently overloaded. Please try again later.\"}}\n\n",
        )))]);

        let preflight = preflight_openai_responses_stream(Box::pin(input))
            .await
            .unwrap();

        let error = preflight.provider_error.unwrap();
        assert_eq!(error.error_type, "permission_error");
        assert_eq!(error.http_status, StatusCode::FORBIDDEN);
        assert!(error.message.contains("trusted_access_for_cyber"));
        assert!(error.message.contains("trusted access for cyber"));
        assert!(!error
            .message
            .contains("Our servers are currently overloaded"));
    }

    #[tokio::test]
    async fn test_responses_preflight_extracts_split_overloaded_error() {
        let input = futures::stream::iter(vec![
            Ok(Bytes::from(
                "event: error\ndata: {\"type\":\"error\",\"error\":",
            )),
            Ok(Bytes::from(
                "{\"code\":\"server_is_overloaded\",\"message\":\"busy\"}}\n\n",
            )),
        ]);

        let preflight = preflight_openai_responses_stream(Box::pin(input))
            .await
            .unwrap();

        let error = preflight.provider_error.unwrap();
        assert_eq!(error.error_type, "overloaded_error");
        assert_eq!(error.message, "busy");
    }

    #[tokio::test]
    async fn test_responses_preflight_replays_normal_stream() {
        let input = futures::stream::iter(vec![
            Ok(Bytes::from(
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            )),
            Ok(Bytes::from("event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n")),
        ]);

        let preflight = preflight_openai_responses_stream(Box::pin(input))
            .await
            .unwrap();
        assert!(!preflight.context_overflow);

        let chunks = preflight
            .stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.unwrap().to_vec()).unwrap())
            .collect::<String>();

        assert!(chunks.contains("response.output_text.delta"));
        assert!(chunks.contains("response.completed"));
    }

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

    #[test]
    fn test_apply_metadata_from_headers_sets_session_id() {
        let mut body = json!({"messages": []});
        let mut headers = HeaderMap::new();
        headers.insert("x-claude-code-session-id", "session-1".parse().unwrap());
        let profile = ProfileConfig {
            provider_type: ProviderType::OpenAIResponses,
            ..ProfileConfig::default()
        };

        apply_metadata_from_headers(&mut body, &headers, &profile);

        assert_eq!(body["metadata"]["session_id"], "session-1");
    }

    #[test]
    fn test_apply_metadata_from_headers_skips_non_responses_profiles() {
        let original = json!({"messages": []});
        let mut body = original.clone();
        let mut headers = HeaderMap::new();
        headers.insert("x-claude-code-session-id", "session-1".parse().unwrap());
        let profile = ProfileConfig {
            provider_type: ProviderType::DirectAnthropic,
            ..ProfileConfig::default()
        };

        apply_metadata_from_headers(&mut body, &headers, &profile);

        assert_eq!(body, original);
    }

    #[test]
    fn test_retry_after_delay_is_capped() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "30".parse().unwrap());

        assert_eq!(retry_after_delay(&headers), Duration::from_secs(5));
    }
}
