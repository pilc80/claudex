use anyhow::Result;
use reqwest::RequestBuilder;
use serde_json::Value;

use super::{ByteStream, ProviderAdapter, TranslatedRequest};
use crate::config::ProfileConfig;
use crate::proxy::util::ToolNameMap;

pub struct ResponsesAdapter;

impl ProviderAdapter for ResponsesAdapter {
    fn endpoint_path(&self) -> &str {
        "/responses"
    }

    fn translate_request(
        &self,
        body: &Value,
        profile: &ProfileConfig,
    ) -> Result<TranslatedRequest> {
        let (mut responses_body, tool_name_map) =
            crate::proxy::translate::responses::anthropic_to_responses(
                body,
                &profile.default_model,
            )?;
        if let Some(image_model) = &profile.image_model {
            if crate::proxy::translate::responses::request_has_current_image(body) {
                responses_body["model"] = serde_json::json!(image_model);
            }
        }
        if profile.base_url.contains("chatgpt.com/backend-api/codex") {
            responses_body["stream"] = serde_json::json!(true);
        }
        Ok(TranslatedRequest {
            body: responses_body,
            tool_name_map,
        })
    }

    fn apply_auth(&self, builder: RequestBuilder, profile: &ProfileConfig) -> RequestBuilder {
        if !profile.api_key.is_empty() {
            builder.header("Authorization", format!("Bearer {}", profile.api_key))
        } else {
            builder
        }
    }

    fn apply_extra_headers(
        &self,
        builder: RequestBuilder,
        profile: &ProfileConfig,
    ) -> RequestBuilder {
        if let Some(account_id) = profile.extra_env.get("CHATGPT_ACCOUNT_ID") {
            builder.header("ChatGPT-Account-ID", account_id.as_str())
        } else {
            builder
        }
    }

    fn translate_response(&self, body: &Value, tool_name_map: &ToolNameMap) -> Result<Value> {
        crate::proxy::translate::responses::responses_to_anthropic(body, tool_name_map)
    }

    fn translate_stream(&self, stream: ByteStream, tool_name_map: ToolNameMap) -> ByteStream {
        crate::proxy::translate::responses_stream::translate_responses_stream(stream, tool_name_map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileConfig;
    use serde_json::json;

    #[test]
    fn test_current_image_uses_image_model() {
        let profile = ProfileConfig {
            default_model: "gpt-5.5".to_string(),
            image_model: Some("gpt-5.5-mini".to_string()),
            base_url: "https://api.openai.com/v1".to_string(),
            ..ProfileConfig::default()
        };

        let body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "Describe"},
                    {"type": "image", "source": {"type": "base64", "data": "abc", "media_type": "image/png"}}
                ]
            }]
        });

        let translated = ResponsesAdapter.translate_request(&body, &profile).unwrap();
        assert_eq!(translated.body["model"], "gpt-5.5-mini");
    }

    #[test]
    fn test_old_image_history_does_not_force_image_model() {
        let profile = ProfileConfig {
            default_model: "gpt-5.5".to_string(),
            image_model: Some("gpt-5.5-mini".to_string()),
            base_url: "https://api.openai.com/v1".to_string(),
            ..ProfileConfig::default()
        };

        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "data": "abc", "media_type": "image/png"}}
                ]},
                {"role": "user", "content": "Now answer text-only."}
            ]
        });

        let translated = ResponsesAdapter.translate_request(&body, &profile).unwrap();
        assert_eq!(translated.body["model"], "gpt-5.5");
    }
}
