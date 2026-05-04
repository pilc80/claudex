use axum::http::StatusCode;
use serde_json::{json, Value};

use crate::config::ProviderType;
use crate::proxy::util::format_sse;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicError {
    pub error_type: &'static str,
    pub message: String,
    pub http_status: StatusCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitDecision {
    Direct,
    Retryable,
}

impl AnthropicError {
    pub fn new(
        error_type: &'static str,
        message: impl Into<String>,
        http_status: StatusCode,
    ) -> Self {
        Self {
            error_type,
            message: message.into(),
            http_status,
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "type": "error",
            "error": {
                "type": self.error_type,
                "message": self.message,
            }
        })
    }

    pub fn sse(&self) -> String {
        format_sse("error", &self.json())
    }
}

pub fn from_http_status(status: StatusCode, body: &str) -> AnthropicError {
    let message = extract_error_message(body).unwrap_or_else(|| body.to_string());
    let error_type = classify_error_type(Some(status.as_u16()), body, Some(&message));
    AnthropicError::new(
        error_type,
        normalize_message(error_type, message),
        anthropic_status(error_type, status),
    )
}

pub fn from_responses_event(event: &Value) -> Option<AnthropicError> {
    let event_type = event
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if event_type != "error"
        && event_type != "response.failed"
        && event_type != "response.incomplete"
    {
        return None;
    }

    if is_incomplete_context_overflow(event) {
        return Some(context_overflow());
    }

    let code = event
        .pointer("/error/code")
        .and_then(|v| v.as_str())
        .or_else(|| {
            event
                .pointer("/response/error/code")
                .and_then(|v| v.as_str())
        });
    let upstream_type = event
        .pointer("/error/type")
        .and_then(|v| v.as_str())
        .or_else(|| {
            event
                .pointer("/response/error/type")
                .and_then(|v| v.as_str())
        });
    let message = event
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .or_else(|| {
            event
                .pointer("/response/error/message")
                .and_then(|v| v.as_str())
        })
        .unwrap_or("upstream response failed");

    let haystack = [code, upstream_type, Some(message)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let error_type = classify_error_type(None, &haystack, Some(message));
    Some(AnthropicError::new(
        error_type,
        normalize_message(error_type, message.to_string()),
        anthropic_status(error_type, StatusCode::BAD_GATEWAY),
    ))
}

pub fn from_stream_transport(message: &str, url: Option<&str>) -> AnthropicError {
    let message = if let Some(url) = url.filter(|url| is_local_url(url)) {
        format!("Local model server unavailable: failed to connect to {url}: {message}")
    } else {
        format!("upstream stream error: {message}")
    };
    AnthropicError::new("api_error", message, StatusCode::BAD_GATEWAY)
}

pub fn from_empty_stream(provider_type: ProviderType, url: Option<&str>) -> AnthropicError {
    let provider = match provider_type {
        ProviderType::OpenAIResponses => "OpenAI Responses",
        ProviderType::OpenAICompatible => "OpenAI-compatible",
        ProviderType::DirectAnthropic => "Anthropic",
    };
    let target = url.map(|u| format!(" from {u}")).unwrap_or_default();
    AnthropicError::new(
        "api_error",
        format!("{provider} upstream returned an empty or untranslatable stream{target}"),
        StatusCode::BAD_GATEWAY,
    )
}

pub fn context_overflow() -> AnthropicError {
    AnthropicError::new(
        "invalid_request_error",
        "prompt is too long",
        StatusCode::BAD_REQUEST,
    )
}

pub fn is_context_overflow_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("context_length_exceeded")
        || lower.contains("maximum context length")
        || lower.contains("prompt is too long")
        || lower.contains("prompt too long")
        || (lower.contains("context window") && lower.contains("exceed"))
}

pub fn circuit_decision(error: &AnthropicError) -> CircuitDecision {
    match error.error_type {
        "rate_limit_error" | "api_error" | "timeout_error" | "overloaded_error" => {
            CircuitDecision::Retryable
        }
        "authentication_error"
        | "billing_error"
        | "invalid_request_error"
        | "not_found_error"
        | "permission_error"
        | "request_too_large" => CircuitDecision::Direct,
        _ if error.http_status.is_server_error() => CircuitDecision::Retryable,
        _ if error.http_status == StatusCode::TOO_MANY_REQUESTS => CircuitDecision::Retryable,
        _ => CircuitDecision::Direct,
    }
}

fn classify_error_type(status: Option<u16>, text: &str, message: Option<&str>) -> &'static str {
    let lower = text.to_ascii_lowercase();
    if is_context_overflow_text(&lower) {
        return "invalid_request_error";
    }
    if lower.contains("server_is_overloaded")
        || lower.contains("service_unavailable_error")
        || lower.contains("overloaded")
    {
        return "overloaded_error";
    }
    if lower.contains("rate_limit") || lower.contains("rate limit") || status == Some(429) {
        return "rate_limit_error";
    }
    if lower.contains("invalid_api_key")
        || lower.contains("invalid token")
        || lower.contains("authentication")
        || status == Some(401)
    {
        return "authentication_error";
    }
    if lower.contains("billing") || lower.contains("payment") {
        return "billing_error";
    }
    if lower.contains("insufficient_quota") || lower.contains("quota") {
        return "billing_error";
    }
    if lower.contains("permission")
        || lower.contains("forbidden")
        || lower.contains("not allowed")
        || status == Some(403)
    {
        return "permission_error";
    }
    if lower.contains("not_found") || lower.contains("not found") || status == Some(404) {
        return "not_found_error";
    }
    if lower.contains("request_too_large")
        || lower.contains("payload too large")
        || status == Some(413)
    {
        return "request_too_large";
    }
    if lower.contains("timeout") || status == Some(408) || status == Some(504) {
        return "timeout_error";
    }

    match status {
        Some(400) | Some(422) => "invalid_request_error",
        Some(401) => "authentication_error",
        Some(402) => "billing_error",
        Some(403) => "permission_error",
        Some(404) => "not_found_error",
        Some(413) => "request_too_large",
        Some(429) => "rate_limit_error",
        Some(500..=599) => "api_error",
        _ => {
            if message.is_some_and(|m| !m.is_empty()) {
                "api_error"
            } else {
                "invalid_request_error"
            }
        }
    }
}

fn normalize_message(error_type: &'static str, message: String) -> String {
    if error_type == "invalid_request_error" && is_context_overflow_text(&message) {
        "prompt is too long".to_string()
    } else {
        message
    }
}

fn anthropic_status(error_type: &str, fallback: StatusCode) -> StatusCode {
    match error_type {
        "invalid_request_error" => StatusCode::BAD_REQUEST,
        "authentication_error" => StatusCode::UNAUTHORIZED,
        "billing_error" => StatusCode::PAYMENT_REQUIRED,
        "permission_error" => StatusCode::FORBIDDEN,
        "not_found_error" => StatusCode::NOT_FOUND,
        "request_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "rate_limit_error" => StatusCode::TOO_MANY_REQUESTS,
        "timeout_error" => StatusCode::GATEWAY_TIMEOUT,
        "overloaded_error" => StatusCode::from_u16(529).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
        "api_error" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => fallback,
    }
}

fn extract_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("message").and_then(|v| v.as_str()))
        .map(ToOwned::to_owned)
}

fn is_incomplete_context_overflow(event: &Value) -> bool {
    let event_type = event.get("type").and_then(|v| v.as_str());
    let status = event.pointer("/response/status").and_then(|v| v.as_str());
    let reason = event
        .pointer("/response/incomplete_details/reason")
        .and_then(|v| v.as_str());
    (event_type == Some("response.incomplete") || status == Some("incomplete"))
        && reason == Some("max_output_tokens")
}

fn is_local_url(url: &str) -> bool {
    url.contains("127.0.0.1") || url.contains("localhost") || url.contains("[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_length_exceeded_maps_to_compact_trigger() {
        let err = from_http_status(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"context_length_exceeded: maximum context length exceeded"}}"#,
        );
        assert_eq!(err.error_type, "invalid_request_error");
        assert_eq!(err.message, "prompt is too long");
        assert_eq!(err.http_status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn responses_incomplete_max_output_tokens_maps_to_compact_trigger() {
        let err = from_responses_event(&json!({
            "type": "response.incomplete",
            "response": {
                "status": "incomplete",
                "incomplete_details": {"reason": "max_output_tokens"},
                "usage": {"output_tokens": 82, "output_tokens_details": {"reasoning_tokens": 82}}
            }
        }))
        .unwrap();
        assert_eq!(err.error_type, "invalid_request_error");
        assert_eq!(err.message, "prompt is too long");
    }

    #[test]
    fn responses_service_unavailable_maps_to_overloaded() {
        let err = from_responses_event(&json!({
            "type": "error",
            "error": {
                "type": "service_unavailable_error",
                "code": "server_is_overloaded",
                "message": "Our servers are currently overloaded. Please try again later."
            }
        }))
        .unwrap();
        assert_eq!(err.error_type, "overloaded_error");
        assert_eq!(
            err.message,
            "Our servers are currently overloaded. Please try again later."
        );
    }

    #[test]
    fn response_failed_overloaded_maps_to_overloaded() {
        let err = from_responses_event(&json!({
            "type": "response.failed",
            "response": {"error": {"code": "server_is_overloaded", "message": "busy"}}
        }))
        .unwrap();
        assert_eq!(err.error_type, "overloaded_error");
        assert_eq!(err.message, "busy");
    }

    #[test]
    fn common_status_codes_map_to_anthropic_types() {
        assert_eq!(
            from_http_status(StatusCode::UNAUTHORIZED, "bad token").error_type,
            "authentication_error"
        );
        assert_eq!(
            from_http_status(StatusCode::FORBIDDEN, "model forbidden").error_type,
            "permission_error"
        );
        assert_eq!(
            from_http_status(StatusCode::NOT_FOUND, "missing").error_type,
            "not_found_error"
        );
        assert_eq!(
            from_http_status(StatusCode::PAYLOAD_TOO_LARGE, "payload too large").error_type,
            "request_too_large"
        );
        assert_eq!(
            from_http_status(StatusCode::TOO_MANY_REQUESTS, "rate limited").error_type,
            "rate_limit_error"
        );
        assert_eq!(
            from_http_status(StatusCode::GATEWAY_TIMEOUT, "timeout").error_type,
            "timeout_error"
        );
    }

    #[test]
    fn schema_bug_preserves_invalid_request_message() {
        let err = from_http_status(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"Missing required parameter: 'text.format.name'."}}"#,
        );
        assert_eq!(err.error_type, "invalid_request_error");
        assert_eq!(
            err.message,
            "Missing required parameter: 'text.format.name'."
        );
    }

    #[test]
    fn circuit_decision_keeps_request_errors_direct() {
        let schema = from_http_status(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"type":"invalid_request_error","message":"Missing required parameter: 'text.format.name'.","param":"text.format.name","code":"missing_required_parameter"}}"#,
        );
        assert_eq!(circuit_decision(&schema), CircuitDecision::Direct);

        let unsupported = from_http_status(
            StatusCode::BAD_REQUEST,
            r#"{"detail":"The 'gpt-5.5-mini' model is not supported when using Codex with a ChatGPT account."}"#,
        );
        assert_eq!(circuit_decision(&unsupported), CircuitDecision::Direct);
    }

    #[test]
    fn circuit_decision_retries_provider_health_errors() {
        let rate_limit = from_http_status(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
        assert_eq!(circuit_decision(&rate_limit), CircuitDecision::Retryable);

        let timeout = from_http_status(StatusCode::GATEWAY_TIMEOUT, "timeout");
        assert_eq!(circuit_decision(&timeout), CircuitDecision::Retryable);

        let overloaded = from_responses_event(&json!({
            "type": "error",
            "error": {"code": "server_is_overloaded", "message": "busy"}
        }))
        .unwrap();
        assert_eq!(circuit_decision(&overloaded), CircuitDecision::Retryable);
    }

    #[test]
    fn circuit_decision_keeps_auth_and_account_errors_direct() {
        let auth = from_http_status(StatusCode::UNAUTHORIZED, "bad token");
        assert_eq!(circuit_decision(&auth), CircuitDecision::Direct);

        let billing = from_http_status(StatusCode::PAYMENT_REQUIRED, "billing issue");
        assert_eq!(circuit_decision(&billing), CircuitDecision::Direct);

        let permission = from_http_status(StatusCode::FORBIDDEN, "model forbidden");
        assert_eq!(circuit_decision(&permission), CircuitDecision::Direct);
    }

    #[test]
    fn local_transport_error_is_clear_api_error() {
        let err = from_stream_transport(
            "error sending request",
            Some("http://127.0.0.1:8080/v1/chat/completions"),
        );
        assert_eq!(err.error_type, "api_error");
        assert!(err.message.contains("Local model server unavailable"));
        assert!(err.message.contains("127.0.0.1:8080"));
    }

    #[test]
    fn empty_stream_is_clear_api_error() {
        let err = from_empty_stream(
            ProviderType::OpenAICompatible,
            Some("http://localhost:8080/v1/chat/completions"),
        );
        assert_eq!(err.error_type, "api_error");
        assert!(err.message.contains("empty or untranslatable stream"));
    }
}
