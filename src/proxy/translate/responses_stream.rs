use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Value};
use std::error::Error;
use std::pin::Pin;

use crate::proxy::error_translation::{self, AnthropicError};
use crate::proxy::util::{format_sse, ToolNameMap};

/// Translates an OpenAI Responses API SSE stream to Anthropic SSE format.
///
/// Responses API events: response.created, response.output_text.delta, etc.
/// Anthropic events: message_start, content_block_start, content_block_delta, etc.
pub fn translate_responses_stream<S>(
    input: S,
    tool_name_map: ToolNameMap,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let mut state = ResponsesStreamState::new(tool_name_map);

    let output = async_stream::stream! {
        let mut stream = std::pin::pin!(input);
        let mut buffer = String::new();
        let mut message_started = false;

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    // Process complete SSE events (separated by double newline or single newline)
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].trim_end_matches('\r').to_string();
                        buffer = buffer[pos + 1..].to_string();

                        if line.is_empty() {
                            state.finish_sse_event();
                            continue;
                        }

                        for event in state.process_line(&line) {
                            if event.starts_with("event: error") {
                                yield Ok(Bytes::from(event));
                                return;
                            }
                            if !message_started {
                                yield Ok(Bytes::from(message_start_event()));
                                message_started = true;
                            }
                            yield Ok(Bytes::from(event));
                        }
                    }
                }
                Err(e) => {
                    log_stream_read_error(&e, &state);
                    yield Ok(Bytes::from(format_error_event(&format!("upstream stream read error: {e}"))));
                    return;
                }
            }
        }

        if !buffer.is_empty() {
            let line = buffer.trim_end_matches('\r');
            for event in state.process_line(line) {
                if event.starts_with("event: error") {
                    yield Ok(Bytes::from(event));
                    return;
                }
                if !message_started {
                    yield Ok(Bytes::from(message_start_event()));
                    message_started = true;
                }
                yield Ok(Bytes::from(event));
            }
        }

        if !message_started {
            tracing::warn!(
                saw_upstream_data = state.saw_upstream_data,
                last_event_type = ?state.last_event_type,
                "Responses stream ended without translatable content"
            );
            yield Ok(Bytes::from(error_translation::from_empty_stream(
                crate::config::ProviderType::OpenAIResponses,
                None,
            ).sse()));
            return;
        }

        // Finalize: close any open block and send message_delta + message_stop
        if state.block_started {
            yield Ok(Bytes::from(format_sse("content_block_stop", &json!({
                "type": "content_block_stop",
                "index": state.block_index,
            }))));
        }

        let stop_reason = if state.has_tool_use { "tool_use" } else { &state.stop_reason };
        yield Ok(Bytes::from(format_sse("message_delta", &json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": {"output_tokens": state.output_tokens}
        }))));
        yield Ok(Bytes::from(format_sse("message_stop", &json!({"type": "message_stop"}))));
    };

    Box::pin(output)
}

fn message_start_event() -> String {
    format_sse(
        "message_start",
        &json!({
            "type": "message_start",
            "message": {
                "id": format!("msg_{}", uuid::Uuid::new_v4()),
                "type": "message",
                "role": "assistant",
                "model": "claudex-proxy",
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }
        }),
    )
}

fn format_error_event(message: &str) -> String {
    AnthropicError::new(
        "api_error",
        message,
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    )
    .sse()
}

fn source_chain(error: &(dyn Error + 'static)) -> Vec<String> {
    let mut chain = Vec::new();
    let mut source = error.source();
    while let Some(err) = source {
        chain.push(err.to_string());
        source = err.source();
    }
    chain
}

fn log_stream_read_error(error: &reqwest::Error, state: &ResponsesStreamState) {
    let sources = source_chain(error);
    tracing::warn!(
        error = %error,
        error_debug = ?error,
        is_decode = error.is_decode(),
        is_timeout = error.is_timeout(),
        is_body = error.is_body(),
        is_connect = error.is_connect(),
        source_chain = ?sources,
        last_event_type = ?state.last_event_type,
        block_started = state.block_started,
        has_tool_use = state.has_tool_use,
        current_tool_name = ?state.current_tool_name,
        current_tool_saw_argument_delta = state.current_tool_saw_argument_delta,
        "Responses stream read error"
    );
}

fn format_context_overflow_event() -> String {
    error_translation::context_overflow().sse()
}

fn format_provider_error_event(event: &Value) -> Option<String> {
    error_translation::from_responses_event(event).map(|err| err.sse())
}

fn verification_recommendation(event: &Value) -> Option<String> {
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

fn format_verification_recommendation_error(recommendation: &str) -> String {
    let message = if recommendation == "trusted_access_for_cyber" {
        "OpenAI requires trusted access for cyber for this request: trusted_access_for_cyber"
            .to_string()
    } else {
        format!("OpenAI requires account verification for this request: {recommendation}")
    };
    AnthropicError::new("permission_error", message, axum::http::StatusCode::FORBIDDEN).sse()
}

fn is_context_overflow_error(message: &str) -> bool {
    error_translation::is_context_overflow_text(message)
}

fn sanitize_tool_input(tool_name: &str, mut input: Value) -> Value {
    if tool_name == "Read" {
        sanitize_read_pages(&mut input);
    }
    input
}

fn sanitize_read_pages(input: &mut Value) {
    let Some(obj) = input.as_object_mut() else {
        return;
    };
    let pages = obj.get("pages").and_then(|v| v.as_str());
    let file_path = obj.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
    let is_pdf = file_path.to_ascii_lowercase().ends_with(".pdf");
    if pages == Some("") || !is_pdf {
        obj.remove("pages");
    }
}

struct ResponsesStreamState {
    tool_name_map: ToolNameMap,
    block_index: usize,
    block_started: bool,
    has_tool_use: bool,
    stop_reason: String,
    output_tokens: u64,
    saw_text_delta: bool,
    saw_upstream_data: bool,
    pending_event_type: Option<String>,
    last_event_type: Option<String>,
    current_tool_name: Option<String>,
    current_tool_saw_argument_delta: bool,
    verification_recommendation: Option<String>,
}

impl ResponsesStreamState {
    fn new(tool_name_map: ToolNameMap) -> Self {
        Self {
            tool_name_map,
            block_index: 0,
            block_started: false,
            has_tool_use: false,
            stop_reason: "end_turn".to_string(),
            output_tokens: 0,
            saw_text_delta: false,
            saw_upstream_data: false,
            pending_event_type: None,
            last_event_type: None,
            current_tool_name: None,
            current_tool_saw_argument_delta: false,
            verification_recommendation: None,
        }
    }

    fn process_line(&mut self, line: &str) -> Vec<String> {
        // Responses API SSE format: "event: <type>\ndata: <json>" or just "data: <json>"
        // We may receive "event:" and "data:" lines separately
        if line.starts_with("event:") {
            self.pending_event_type = line
                .strip_prefix("event:")
                .map(str::trim)
                .filter(|event_type| !event_type.is_empty())
                .map(ToOwned::to_owned);
            self.last_event_type = self.pending_event_type.clone();
            return vec![];
        }

        let data = if let Some(stripped) = line.strip_prefix("data: ") {
            stripped
        } else if let Some(stripped) = line.strip_prefix("data:") {
            stripped
        } else {
            return vec![];
        };
        self.saw_upstream_data = true;
        let fallback_event_type = self.pending_event_type.take();

        if data == "[DONE]" {
            return vec![];
        }

        let json: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    last_event_type = ?self.last_event_type,
                    error = %err,
                    "ignoring malformed Responses stream data line"
                );
                return vec![];
            }
        };

        let event_type = json
            .get("type")
            .and_then(|t| t.as_str())
            .or(fallback_event_type.as_deref())
            .unwrap_or("");
        if !event_type.is_empty() {
            self.last_event_type = Some(event_type.to_string());
        }
        capture_reasoning_event(event_type, &json);
        match event_type {
            "response.metadata" => {
                if let Some(recommendation) = verification_recommendation(&json) {
                    self.verification_recommendation = Some(recommendation);
                }
                vec![]
            }
            "error" => {
                if let Some(recommendation) = self.verification_recommendation.as_deref() {
                    vec![format_verification_recommendation_error(recommendation)]
                } else if let Some(event) = format_provider_error_event(&json) {
                    vec![event]
                } else {
                    let message = json
                        .pointer("/error/message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("upstream Responses error");
                    vec![format_error_event(message)]
                }
            }
            "response.output_text.delta" => {
                let delta = json.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return vec![];
                }
                self.saw_text_delta = true;

                let mut events = Vec::new();

                // Start content block if not started
                if !self.block_started {
                    events.push(format_sse(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": self.block_index,
                            "content_block": {"type": "text", "text": ""},
                        }),
                    ));
                    self.block_started = true;
                }

                events.push(format_sse(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": self.block_index,
                        "delta": {"type": "text_delta", "text": delta},
                    }),
                ));

                events
            }
            "response.output_text.done" | "response.content_part.done" => {
                if self.block_started {
                    self.block_started = false;
                    let event = format_sse(
                        "content_block_stop",
                        &json!({
                            "type": "content_block_stop",
                            "index": self.block_index,
                        }),
                    );
                    self.block_index += 1;
                    return vec![event];
                }
                vec![]
            }
            "response.output_item.done" => {
                let empty = json!({});
                let item = json.get("item").unwrap_or(&empty);
                if self.saw_text_delta
                    || item.get("type").and_then(|t| t.as_str()) != Some("message")
                {
                    return vec![];
                }
                let text = extract_message_text(item);
                if text.is_empty() {
                    return vec![];
                }
                self.emit_text_block(&text)
            }
            "response.output_item.added" => {
                // Check if it's a function_call
                let empty = json!({});
                let item = json.get("item").unwrap_or(&empty);
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

                if item_type == "function_call" {
                    self.has_tool_use = true;
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let original_name = self
                        .tool_name_map
                        .get(name)
                        .cloned()
                        .unwrap_or(name.to_string());
                    self.current_tool_name = Some(original_name.clone());
                    self.current_tool_saw_argument_delta = false;
                    let call_id = item
                        .get("call_id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("call_0");

                    // Close any previous block
                    let mut events = Vec::new();
                    if self.block_started {
                        events.push(format_sse(
                            "content_block_stop",
                            &json!({
                                "type": "content_block_stop",
                                "index": self.block_index,
                            }),
                        ));
                        self.block_index += 1;
                        self.block_started = false;
                    }

                    events.push(format_sse(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": self.block_index,
                            "content_block": {
                                "type": "tool_use",
                                "id": call_id,
                                "name": original_name,
                                "input": {},
                            },
                        }),
                    ));
                    self.block_started = true;

                    return events;
                }
                vec![]
            }
            "response.function_call_arguments.delta" => {
                if !self.block_started || !self.has_tool_use {
                    tracing::warn!(
                        last_event_type = ?self.last_event_type,
                        "ignoring orphan Responses function-call argument delta"
                    );
                    return vec![];
                }
                let delta = json.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return vec![];
                }
                self.current_tool_saw_argument_delta = true;

                vec![format_sse(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": self.block_index,
                        "delta": {"type": "input_json_delta", "partial_json": delta},
                    }),
                )]
            }
            "response.function_call_arguments.done" => {
                if self.block_started {
                    let arguments = json
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    let input = sanitize_tool_input(
                        self.current_tool_name.as_deref().unwrap_or(""),
                        serde_json::from_str(arguments).unwrap_or_else(|_| json!({})),
                    );
                    let mut events = Vec::new();
                    if !self.current_tool_saw_argument_delta && input != json!({}) {
                        events.push(format_sse(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": self.block_index,
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": serde_json::to_string(&input).unwrap_or_default(),
                                },
                            }),
                        ));
                    }
                    self.block_started = false;
                    self.current_tool_name = None;
                    self.current_tool_saw_argument_delta = false;
                    events.push(format_sse(
                        "content_block_stop",
                        &json!({
                            "type": "content_block_stop",
                            "index": self.block_index,
                        }),
                    ));
                    self.block_index += 1;
                    return events;
                }
                vec![]
            }
            "response.completed" | "response.incomplete" => {
                // Extract usage from the terminal response
                if let Some(resp) = json.get("response") {
                    if let Some(usage) = resp.get("usage") {
                        self.output_tokens = usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                    let status = resp
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("completed");
                    let incomplete_reason = resp
                        .pointer("/incomplete_details/reason")
                        .and_then(|v| v.as_str());
                    if status == "incomplete" || event_type == "response.incomplete" {
                        self.stop_reason = "max_tokens".to_string();
                    }
                    if incomplete_reason == Some("max_output_tokens")
                        && !self.saw_text_delta
                        && !self.block_started
                    {
                        return vec![format_context_overflow_event()];
                    }
                    if !self.saw_text_delta && !self.block_started {
                        if let Some(output) = resp.get("output").and_then(|o| o.as_array()) {
                            for item in output {
                                if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                                    let text = extract_message_text(item);
                                    if !text.is_empty() {
                                        return self.emit_text_block(&text);
                                    }
                                }
                            }
                        }
                    }
                }
                // Don't emit anything here — finalization happens in the outer stream
                vec![]
            }
            "response.failed" => {
                if let Some(recommendation) = self.verification_recommendation.as_deref() {
                    vec![format_verification_recommendation_error(recommendation)]
                } else if let Some(event) = format_provider_error_event(&json) {
                    vec![event]
                } else {
                    let message = json
                        .pointer("/response/error/message")
                        .and_then(|v| v.as_str())
                        .or_else(|| json.pointer("/error/message").and_then(|v| v.as_str()))
                        .unwrap_or("upstream response failed");
                    vec![format_error_event(message)]
                }
            }
            "codex.rate_limits" => {
                let message = json
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Codex rate limit event received");
                vec![AnthropicError::new(
                    "rate_limit_error",
                    message,
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                )
                .sse()]
            }
            _ => vec![],
        }
    }

    fn log_parsed_event(&self, event_type: &str, event: &Value) {
        match event_type {
            "response.output_text.delta" => {
                tracing::debug!(
                    event_type,
                    delta_len = event
                        .get("delta")
                        .and_then(|v| v.as_str())
                        .map(str::len)
                        .unwrap_or(0),
                    block_started = self.block_started,
                    has_tool_use = self.has_tool_use,
                    "Responses stream text delta"
                );
            }
            "response.function_call_arguments.delta" => {
                tracing::debug!(
                    event_type,
                    delta_len = event
                        .get("delta")
                        .and_then(|v| v.as_str())
                        .map(str::len)
                        .unwrap_or(0),
                    block_started = self.block_started,
                    has_tool_use = self.has_tool_use,
                    "Responses stream function arguments delta"
                );
            }
            "response.output_item.added" | "response.output_item.done" => {
                let item = event.get("item");
                tracing::info!(
                    event_type,
                    item_type = item
                        .and_then(|v| v.get("type"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("<none>"),
                    item_status = item
                        .and_then(|v| v.get("status"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("<none>"),
                    name = item
                        .and_then(|v| v.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("<none>"),
                    call_id = item
                        .and_then(|v| v.get("call_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("<none>"),
                    output_index = event.get("output_index").and_then(|v| v.as_u64()),
                    block_started = self.block_started,
                    has_tool_use = self.has_tool_use,
                    "Responses stream item event"
                );
            }
            "response.function_call_arguments.done" => {
                tracing::info!(
                    event_type,
                    name = event
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<none>"),
                    arguments_len = event
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .map(str::len)
                        .unwrap_or(0),
                    block_started = self.block_started,
                    has_tool_use = self.has_tool_use,
                    "Responses stream function arguments done"
                );
            }
            "response.completed" | "response.incomplete" => {
                let response = event.get("response");
                tracing::info!(
                    event_type,
                    response_status = response
                        .and_then(|v| v.get("status"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("<none>"),
                    output_item_types = ?response
                        .and_then(|v| v.get("output"))
                        .and_then(|v| v.as_array())
                        .map(|items| output_item_types(items))
                        .unwrap_or_default(),
                    output_tokens = response
                        .and_then(|v| v.get("usage"))
                        .and_then(|v| v.get("output_tokens"))
                        .and_then(|v| v.as_u64()),
                    block_started = self.block_started,
                    has_tool_use = self.has_tool_use,
                    saw_text_delta = self.saw_text_delta,
                    "Responses stream completed"
                );
            }
            "response.failed" | "codex.rate_limits" => {
                tracing::warn!(event_type, "Responses stream terminal error event");
            }
            "" => {
                tracing::warn!("Responses stream data had no event type");
            }
            _ => {
                tracing::info!(
                    event_type,
                    item_type = event
                        .get("item")
                        .and_then(|v| v.get("type"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("<none>"),
                    "Responses stream unhandled event"
                );
            }
        }
    }

    fn finish_sse_event(&mut self) {
        self.pending_event_type = None;
    }

    fn emit_text_block(&mut self, text: &str) -> Vec<String> {
        let events = vec![
            format_sse(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": self.block_index,
                    "content_block": {"type": "text", "text": ""},
                }),
            ),
            format_sse(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": self.block_index,
                    "delta": {"type": "text_delta", "text": text},
                }),
            ),
        ];
        self.block_started = true;
        self.saw_text_delta = true;
        events
    }
}

fn capture_reasoning_event(event_type: &str, event: &Value) {
    let mut texts = Vec::new();
    if event_type.contains("reasoning") {
        if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
            texts.push(delta.to_string());
        }
        collect_reasoning_text(event, &mut texts);
    } else if event_type == "response.output_item.added" {
        if let Some(item) = event.get("item") {
            if item.get("type").and_then(|v| v.as_str()) == Some("reasoning") {
                collect_reasoning_text(item, &mut texts);
            }
        }
    } else if event_type == "response.output_item.done" {
        return;
    } else if event_type == "response.completed" || event_type == "response.incomplete" {
        if let Some(items) = event.pointer("/response/output").and_then(|v| v.as_array()) {
            for item in items {
                if item.get("type").and_then(|v| v.as_str()) == Some("reasoning") {
                    collect_reasoning_text(item, &mut texts);
                }
            }
        }
        if let Some(tokens) = event
            .pointer("/response/usage/output_tokens_details/reasoning_tokens")
            .and_then(|v| v.as_u64())
        {
            crate::proxy::reasoning::publish(
                crate::reasoning::ReasoningEvent::new(
                    event
                        .pointer("/response/id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown"),
                    "turn",
                    "openai-responses",
                    "reasoning_tokens",
                )
                .value(json!(tokens)),
            );
        }
    }

    let text = texts
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        crate::proxy::reasoning::publish(
            crate::reasoning::ReasoningEvent::new(
                event
                    .pointer("/response/id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("stream"),
                event
                    .get("item_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("turn"),
                "openai-responses",
                event_type,
            )
            .text(text),
        );
    }
}

fn collect_reasoning_text(value: &Value, texts: &mut Vec<String>) {
    match value {
        Value::String(text) => texts.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                collect_reasoning_text(item, texts);
            }
        }
        Value::Object(map) => {
            for key in ["summary", "text", "content", "reasoning", "reasoning_text"] {
                if let Some(value) = map.get(key) {
                    collect_reasoning_text(value, texts);
                }
            }
        }
        _ => {}
    }
}

fn output_item_types(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            item.get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("<none>")
                .to_string()
        })
        .collect()
}

fn extract_message_text(item: &Value) -> String {
    item.get("content")
        .and_then(|c| c.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter(|p| {
                    matches!(
                        p.get("type").and_then(|t| t.as_str()),
                        Some("output_text") | Some("text")
                    )
                })
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[test]
    fn test_text_delta() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        let events = state.process_line(
            r#"data: {"type":"response.output_text.delta","delta":"Hello","output_index":0,"content_index":0}"#,
        );
        // Should get content_block_start + content_block_delta
        assert_eq!(events.len(), 2);
        assert!(events[0].contains("content_block_start"));
        assert!(events[1].contains("text_delta"));
        assert!(events[1].contains("Hello"));
    }

    #[test]
    fn test_function_call_flow() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());

        // function_call added
        let events = state.process_line(
            r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"get_weather","arguments":"","status":"in_progress"}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("tool_use"));
        assert!(events[0].contains("get_weather"));

        // argument delta
        let events = state.process_line(
            r#"data: {"type":"response.function_call_arguments.delta","delta":"{\"loc\""}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("input_json_delta"));

        // arguments done
        let events = state.process_line(
            r#"data: {"type":"response.function_call_arguments.done","name":"get_weather","arguments":"{\"location\":\"Paris\"}"}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("content_block_stop"));
        assert!(state.has_tool_use);
    }

    #[test]
    fn test_read_function_call_arguments_done_strips_invalid_pages() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        state.process_line(
            r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_read","name":"Read","arguments":"","status":"in_progress"}}"#,
        );
        let events = state.process_line(
            r#"data: {"type":"response.function_call_arguments.done","arguments":"{\"file_path\":\"/tmp/a.md\",\"pages\":\"\",\"limit\":10}"}"#,
        );
        let output = events.join("\n");
        assert!(output.contains("content_block_delta"));
        assert!(output.contains("/tmp/a.md"));
        assert!(!output.contains("pages"));
    }

    #[test]
    fn test_read_pdf_function_call_arguments_done_keeps_pages() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        state.process_line(
            r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_read","name":"Read","arguments":"","status":"in_progress"}}"#,
        );
        let events = state.process_line(
            r#"data: {"type":"response.function_call_arguments.done","arguments":"{\"file_path\":\"/tmp/a.pdf\",\"pages\":\"1-2\"}"}"#,
        );
        assert!(events.join("\n").contains("pages"));
    }

    #[test]
    fn test_completed_extracts_usage() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        state.process_line(
            r#"data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":100,"output_tokens":50,"total_tokens":150}}}"#,
        );
        assert_eq!(state.output_tokens, 50);
    }

    #[test]
    fn test_output_item_done_message_text() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        let events = state.process_line(
            r#"data: {"type":"response.output_item.done","item":{"type":"message","content":[{"type":"text","text":"Compact summary"}]}}"#,
        );
        assert_eq!(events.len(), 2);
        assert!(events[0].contains("content_block_start"));
        assert!(events[1].contains("Compact summary"));
    }

    #[test]
    fn test_response_failed_emits_anthropic_error() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        let events = state.process_line(
            r#"data: {"type":"response.failed","response":{"error":{"message":"rate limited"}}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("event: error"));
        assert!(events[0].contains("rate limited"));
    }

    #[test]
    fn test_response_failed_overloaded_translates_to_overloaded_error() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        let events = state.process_line(
            r#"data: {"type":"response.failed","response":{"error":{"code":"server_is_overloaded","message":"Our servers are currently overloaded. Please try again later."}}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("event: error"));
        assert!(events[0].contains("overloaded_error"));
        assert!(events[0].contains("Our servers are currently overloaded"));
    }

    #[test]
    fn test_event_error_overloaded_translates_to_overloaded_error() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        let events = state.process_line(
            r#"data: {"type":"error","error":{"type":"service_unavailable_error","code":"server_is_overloaded","message":"Our servers are currently overloaded. Please try again later."}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("event: error"));
        assert!(events[0].contains("overloaded_error"));
        assert!(events[0].contains("Our servers are currently overloaded"));
    }

    #[test]
    fn test_metadata_verification_overrides_later_overloaded_error() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        assert!(state
            .process_line(
                r#"data: {"type":"response.metadata","metadata":{"openai_verification_recommendation":["trusted_access_for_cyber"]}}"#,
            )
            .is_empty());

        let events = state.process_line(
            r#"data: {"type":"error","error":{"type":"service_unavailable_error","code":"server_is_overloaded","message":"Our servers are currently overloaded. Please try again later."}}"#,
        );

        assert_eq!(events.len(), 1);
        assert!(events[0].contains("event: error"));
        assert!(events[0].contains("permission_error"));
        assert!(events[0].contains("trusted_access_for_cyber"));
        assert!(events[0].contains("trusted access for cyber"));
        assert!(!events[0].contains("overloaded_error"));
        assert!(!events[0].contains("Our servers are currently overloaded"));
    }

    #[tokio::test]
    async fn test_metadata_verification_stream_error_before_message_start() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(concat!(
            "event: response.metadata\n",
            "data: {\"type\":\"response.metadata\",\"metadata\":{\"openai_verification_recommendation\":[\"trusted_access_for_cyber\"]}}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"service_unavailable_error\",\"code\":\"server_is_overloaded\",\"message\":\"Our servers are currently overloaded. Please try again later.\"}}\n\n",
        )))]);
        let mut stream = translate_responses_stream(input, ToolNameMap::new());
        let first = stream.next().await.unwrap().unwrap();
        let text = String::from_utf8(first.to_vec()).unwrap();

        assert!(text.contains("event: error"));
        assert!(text.contains("permission_error"));
        assert!(text.contains("trusted_access_for_cyber"));
        assert!(text.contains("trusted access for cyber"));
        assert!(!text.contains("message_start"));
        assert!(!text.contains("overloaded_error"));
        assert!(!text.contains("Our servers are currently overloaded"));
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn test_context_overflow_translates_to_claude_compact_error() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        let events = state.process_line(
            r#"data: {"type":"response.failed","response":{"error":{"message":"Your input exceeds the context window of this model. Please adjust your input and try again."}}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("event: error"));
        assert!(events[0].contains("invalid_request_error"));
        assert!(events[0].contains("prompt is too long"));
        assert!(!events[0].contains("Your input exceeds the context window"));
    }

    #[test]
    fn test_incomplete_max_output_without_visible_output_translates_to_compact_error() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        assert!(state
            .process_line(r#"data: {"type":"response.output_item.added","item":{"type":"reasoning","summary":[]}}"#)
            .is_empty());
        assert!(state
            .process_line(r#"data: {"type":"response.output_item.done","item":{"type":"reasoning","summary":[]}}"#)
            .is_empty());

        let events = state.process_line(
            r#"data: {"type":"response.incomplete","response":{"status":"incomplete","error":null,"incomplete_details":{"reason":"max_output_tokens"},"usage":{"output_tokens":82,"output_tokens_details":{"reasoning_tokens":82}}}}"#,
        );

        assert_eq!(events.len(), 1);
        assert!(events[0].contains("event: error"));
        assert!(events[0].contains("invalid_request_error"));
        assert!(events[0].contains("prompt is too long"));
        assert!(!events[0].contains("ended without translatable content"));
    }

    #[tokio::test]
    async fn test_incomplete_max_output_with_visible_output_finishes_as_max_tokens() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(
            concat!(
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
                "event: response.incomplete\n",
                "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"usage\":{\"output_tokens\":82}}}\n\n",
            ),
        ))]);
        let mut stream = translate_responses_stream(input, ToolNameMap::new());

        let mut output = String::new();
        while let Some(chunk) = stream.next().await {
            output.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
        }

        assert!(output.contains("partial"));
        assert!(output.contains("message_delta"));
        assert!(output.contains("max_tokens"));
        assert!(!output.contains("event: error"));
    }

    #[test]
    fn test_codex_rate_limits_event_emits_error() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        let events = state
            .process_line(r#"data: {"type":"codex.rate_limits","message":"rate limit exceeded"}"#);
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("event: error"));
        assert!(events[0].contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn test_stream_error_before_content_does_not_emit_message_start() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n",
        ))]);
        let mut stream = translate_responses_stream(input, ToolNameMap::new());
        let first = stream.next().await.unwrap().unwrap();
        let text = String::from_utf8(first.to_vec()).unwrap();
        assert!(text.contains("event: error"));
        assert!(!text.contains("message_start"));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_event_line_supplies_type_when_data_omits_type() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(
            "event: response.output_text.delta\ndata: {\"delta\":\"Hello\"}\n\n",
        ))]);
        let mut stream = translate_responses_stream(input, ToolNameMap::new());

        let mut output = String::new();
        while let Some(chunk) = stream.next().await {
            output.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
        }

        assert!(output.contains("event: message_start"));
        assert!(output.contains("event: content_block_delta"));
        assert!(output.contains("Hello"));
        assert!(!output.contains("event: error"));
    }

    #[tokio::test]
    async fn test_untranslatable_stream_returns_error_before_message_start() {
        let input = futures::stream::iter(vec![Ok(Bytes::from("event: ping\ndata: {}\n\n"))]);
        let mut stream = translate_responses_stream(input, ToolNameMap::new());

        let first = stream.next().await.unwrap().unwrap();
        let text = String::from_utf8(first.to_vec()).unwrap();
        assert!(text.contains("event: error"));
        assert!(text.contains("empty or untranslatable stream"));
        assert!(!text.contains("message_start"));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_final_data_line_without_newline_is_processed() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(
            "event: response.output_text.delta\ndata: {\"delta\":\"tail\"}",
        ))]);
        let mut stream = translate_responses_stream(input, ToolNameMap::new());

        let mut output = String::new();
        while let Some(chunk) = stream.next().await {
            output.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
        }

        assert!(output.contains("tail"));
        assert!(!output.contains("event: error"));
    }

    #[test]
    fn test_stream_state_tracks_open_tool_for_diagnostics() {
        let mut state = ResponsesStreamState::new(ToolNameMap::new());
        state.process_line(
            r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_read","name":"Read","arguments":"","status":"in_progress"}}"#,
        );
        state.process_line(
            r#"data: {"type":"response.function_call_arguments.delta","delta":"{\"file_path\""}"#,
        );

        assert_eq!(
            state.last_event_type.as_deref(),
            Some("response.function_call_arguments.delta")
        );
        assert!(state.block_started);
        assert!(state.has_tool_use);
        assert_eq!(state.current_tool_name.as_deref(), Some("Read"));
        assert!(state.current_tool_saw_argument_delta);
    }

    #[tokio::test]
    async fn test_orphan_function_arguments_delta_returns_error_before_message_start() {
        let input = futures::stream::iter(vec![Ok(Bytes::from(
            "event: response.function_call_arguments.delta\ndata: {\"delta\":\"{\\\"path\\\"\"}\n\n",
        ))]);
        let mut stream = translate_responses_stream(input, ToolNameMap::new());

        let first = stream.next().await.unwrap().unwrap();
        let text = String::from_utf8(first.to_vec()).unwrap();
        assert!(text.contains("event: error"));
        assert!(text.contains("empty or untranslatable stream"));
        assert!(!text.contains("message_start"));
        assert!(!text.contains("content_block_delta"));
        assert!(stream.next().await.is_none());
    }
}
