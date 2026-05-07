# Claudex - Claude Code Project Guide

## Project Overview

Claudex is a Rust-based multi-instance Claude Code manager with a built-in
translation proxy. It translates Claude Code's Anthropic API requests through a
local proxy and forwards them to multiple AI providers.

## Technology Stack

- **Language**: Rust (Edition 2021)
- **Async runtime**: Tokio
- **Web framework**: Axum 0.8
- **HTTP client**: reqwest (rustls-tls)
- **TUI**: ratatui + crossterm
- **Configuration**: TOML (toml crate)
- **Logging**: tracing + tracing-subscriber
- **Error handling**: anyhow + thiserror

## Project Structure

```text
src/
├── main.rs              # Entry point + CLI dispatch
├── cli.rs               # clap subcommand definitions
├── config.rs            # Config parsing; API keys are stored in config, no automatic keyring read
├── profile.rs           # Profile management
├── launch.rs            # Launches the claude process, including Claude OAuth special handling
├── oauth/               # OAuth subscription auth
│   ├── mod.rs           # AuthType, OAuthProvider, OAuthToken types
│   ├── token.rs         # External CLI token reads (Codex/Claude/Gemini)
│   ├── server.rs        # Local callback server + device-code polling
│   └── providers.rs     # Login/refresh/status logic for each platform
├── daemon.rs            # PID file + process management
├── metrics.rs           # Request metrics
├── proxy/               # Translation proxy
│   ├── mod.rs           # Axum server
│   ├── handler.rs       # Request handling
│   ├── translation.rs   # Anthropic <-> OpenAI translation
│   ├── streaming.rs     # SSE streaming translation
│   ├── fallback.rs      # Circuit breaker
│   ├── health.rs        # Health checks
│   └── models.rs        # /v1/models endpoint
├── router/              # Smart routing
│   ├── mod.rs
│   └── classifier.rs    # Intent classification
├── context/             # Context engine
│   ├── mod.rs
│   ├── compression.rs   # Conversation compression
│   ├── sharing.rs       # Cross-profile sharing
│   └── rag.rs           # Local RAG
└── tui/                 # TUI dashboard
    ├── mod.rs
    ├── dashboard.rs
    ├── widgets.rs
    └── input.rs
```

## Build and Run

```bash
# Development build
cargo build

# Release build
cargo build --release

# Check
cargo check

# Clippy
cargo clippy

# Run
cargo run -- profile list
cargo run -- run grok
cargo run -- proxy start
```

## Core Concepts

### Provider Types

- `DirectAnthropic`: native Anthropic API passthrough, for providers such as
  Anthropic and MiniMax.
- `OpenAICompatible`: OpenAI-compatible API that requires protocol translation,
  for providers such as OpenRouter, Grok, OpenAI, DeepSeek, Kimi, GLM, and
  Ollama.

### Translation Layer

`proxy/translation.rs` implements Anthropic <-> OpenAI request and response
translation:

- Request translation: system prompt, messages including images and `tool_use`,
  tools, and `tool_choice`.
- Response translation: content blocks, tool calls, usage, and `stop_reason`.
- Streaming translation (`proxy/streaming.rs`): SSE event conversion and tool
  call state handling.

### Authentication

- **API key** (default): configure `api_key` or `api_key_keyring`.
- **OAuth subscription**: configure `auth_type = "oauth"` and `oauth_provider`;
  complete setup through `claudex auth login`.
  - Claude subscription special case: skip the proxy and let Claude Code use its
    own OAuth session directly.
  - Other providers: store OAuth tokens in the keyring; the proxy loads and
    refreshes them automatically.

### Configuration

The config file is at `~/.config/claudex/config.toml`. See
`config.example.toml`.

## Development Rules

- Compile check: run `cargo check` after changes.
- Code check: run `cargo clippy` before committing; keep it warning-free.
- Error handling: use `anyhow::Result` and `?`; do not use `unwrap()` in
  production code.
- Logging: use `tracing::info!`, `tracing::warn!`, and `tracing::error!`.
- Formatting: run `cargo fmt`.
- Before committing, pushing, or releasing changes that touch config, install,
  auth, proxy startup, profile selection, or version/health checks:
  1. Review whether `claudex-config config doctor` still covers the changed path.
  2. Update doctor checks and tests if the change can affect setup readiness.
  3. Run doctor-related tests plus `claudex-config config doctor` against the
     installed binary.
  4. Do not commit/release if doctor gives stale, incomplete, or misleading
     guidance.

## Deployment Rules

- Before each new server/proxy binary deploy, increment the `Cargo.toml` patch
  version: `0.9.x -> 0.9.x+1`. Bump the version first, then run
  `cargo build --release`, then update and verify the deployment.
- Do not deploy new behavior with the same version as the currently deployed
  binary. The health version and stale-proxy checks depend on the version.

## Key Files

| File | Change frequency | Notes |
|------|------------------|-------|
| `config.rs` | Medium | Update when adding config fields |
| `translation.rs` | High | Core translation logic; new providers may need special handling |
| `streaming.rs` | High | Streaming translation is stateful and needs careful handling |
| `handler.rs` | Medium | Request routing/middleware, including lazy OAuth token refresh |
| `cli.rs` | Low | Update when adding subcommands |
| `oauth/` | Low | OAuth auth module; update when adding providers |
| `launch.rs` | Low | Contains Claude OAuth subscription special handling |
