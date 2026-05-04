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

use crate::config::{ProfileConfig, ProviderType};
use crate::oauth::AuthType;
use crate::proxy::error_translation::{self, AnthropicError, CircuitDecision};
use crate::proxy::ProxyState;
use crate::router::classifier;

const IMAGE_HISTORY_PLACEHOLDER_PREFIX: &str = "[Previous image omitted by claudex";

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
    buffered: Vec<u8>,
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
}

async fn preflight_openai_responses_stream(
    input: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
) -> anyhow::Result<ResponsesStreamPreflight> {
    let mut stream = input;
    let mut buffered = Vec::new();
    let mut context_overflow = false;

    while buffered.len() < PREFLIGHT_MAX_BYTES {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let chunk = chunk?;
        buffered.extend_from_slice(&chunk);
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
        buffered,
        stream: Box::pin(output),
    })
}

fn buffered_has_context_overflow(buffered: &[u8]) -> bool {
    let text = String::from_utf8_lossy(buffered).to_ascii_lowercase();
    text.contains("context_length_exceeded")
        || (text.contains("context window") && text.contains("exceeds"))
}

fn buffered_has_translatable_responses_content(buffered: &[u8]) -> bool {
    let text = String::from_utf8_lossy(buffered);
    text.contains("event: response.output_text.delta")
        || text.contains("event: response.output_item.done")
        || text.contains("event: response.output_item.added")
        || text.contains("event: response.function_call_arguments.delta")
        || text.contains("event: response.completed")
        || text.contains("event: response.failed")
        || text.contains("event: error")
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

    let safe_profile = profile
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
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
            reasoning_bus: crate::proxy::reasoning::ReasoningBus::new(),
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
