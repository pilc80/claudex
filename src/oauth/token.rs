//! Backward-compatible re-exports from source.rs
//!
//! 新代码应直接使用 source.rs 和 exchange.rs 中的函数。

use anyhow::Result;

use super::OAuthToken;

// ── Re-exports from source.rs ────────────────────────────────────────────

pub fn store_token(profile_name: &str, token: &OAuthToken) -> Result<()> {
    let _ = (profile_name, token);
    anyhow::bail!("OAuth keyring storage is disabled")
}

pub fn load_token(profile_name: &str) -> Result<OAuthToken> {
    let _ = profile_name;
    anyhow::bail!("OAuth keyring storage is disabled")
}

pub fn delete_token(profile_name: &str) -> Result<()> {
    let _ = profile_name;
    anyhow::bail!("OAuth keyring storage is disabled")
}

pub fn read_claude_credentials() -> Result<OAuthToken> {
    super::source::read_claude_credentials().map(|c| c.into_oauth_token())
}

pub fn read_codex_credentials() -> Result<OAuthToken> {
    super::source::read_codex_credentials().map(|c| c.into_oauth_token())
}

pub fn read_external_token(provider: &super::OAuthProvider) -> Result<OAuthToken> {
    super::source::load_credential_chain(provider).map(|c| c.into_oauth_token())
}

pub fn write_codex_credentials(token: &OAuthToken) -> Result<()> {
    super::source::write_codex_credentials_atomic(token)
}

pub fn extract_jwt_exp_pub(token: &str) -> Option<i64> {
    super::source::extract_jwt_exp(token)
}

pub fn extract_jwt_claim_pub(token: &str, namespace: &str, field: &str) -> Option<String> {
    super::source::extract_jwt_claim(token, namespace, field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reexport_extract_jwt_exp() {
        use base64::Engine;
        let payload = serde_json::json!({"exp": 1700000000_i64});
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        let fake_jwt = format!("eyJhbGciOiJub25lIn0.{payload_b64}.sig");
        assert_eq!(extract_jwt_exp_pub(&fake_jwt), Some(1700000000000_i64));
    }

    #[test]
    fn token_keyring_wrappers_are_disabled() {
        let token = OAuthToken {
            access_token: "token".to_string(),
            refresh_token: None,
            expires_at: None,
            token_type: Some("Bearer".to_string()),
            scopes: None,
            extra: None,
        };

        assert!(store_token("codex-sub", &token)
            .unwrap_err()
            .to_string()
            .contains("keyring storage is disabled"));
        assert!(load_token("codex-sub")
            .unwrap_err()
            .to_string()
            .contains("keyring storage is disabled"));
        assert!(delete_token("codex-sub")
            .unwrap_err()
            .to_string()
            .contains("keyring storage is disabled"));
    }
}
