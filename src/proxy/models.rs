use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::config::{ProfileConfig, ProviderType};
use crate::proxy::ProxyState;

pub async fn list_models(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    let config = state.config.read().await;
    let profiles = config.enabled_profiles();

    let mut models = Vec::new();

    for profile in profiles {
        models.extend(profile_model_entries(profile));
    }

    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "data": models,
        })),
    )
}

fn profile_model_entries(profile: &ProfileConfig) -> Vec<serde_json::Value> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    let candidates = [
        Some(profile.default_model.as_str()),
        profile.models.haiku.as_deref(),
        profile.models.sonnet.as_deref(),
        profile.models.opus.as_deref(),
    ];

    for id in candidates.into_iter().flatten().filter(|id| !id.is_empty()) {
        if seen.insert(id.to_string()) {
            ids.push(id);
        }
    }

    ids.into_iter()
        .map(|id| {
            json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": profile.name,
                "x-claudex-profile": profile.name,
                "x-claudex-provider": match profile.provider_type {
                    ProviderType::DirectAnthropic => "anthropic",
                    ProviderType::OpenAICompatible => "openai-compatible",
                    ProviderType::OpenAIResponses => "openai-responses",
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProfileConfig, ProfileModels};

    #[test]
    fn test_profile_model_entries_include_slots_without_duplicates() {
        let profile = ProfileConfig {
            name: "codex".to_string(),
            default_model: "gpt-5.5".to_string(),
            models: ProfileModels {
                haiku: Some("gpt-5.5-mini".to_string()),
                sonnet: Some("gpt-5.5".to_string()),
                opus: Some("gpt-5.5-pro".to_string()),
            },
            ..ProfileConfig::default()
        };

        let entries = profile_model_entries(&profile);
        let ids = entries
            .iter()
            .map(|entry| entry["id"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["gpt-5.5", "gpt-5.5-mini", "gpt-5.5-pro"]);
    }
}
