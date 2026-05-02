# Claudex Codex/OpenAI Responses Fork

This repository is a fork of [StringKe/claudex](https://github.com/StringKe/claudex),
based on upstream `v0.2.4`.

Use the upstream README and docs for the general Claudex feature set, CLI,
provider list, smart routing, config discovery, and non-Codex providers:

- Upstream README: https://github.com/StringKe/claudex
- Upstream docs: https://stringke.github.io/claudex/

This README documents only the fork-specific Codex/OpenAI Responses work and
local setup details.

## What This Fork Changes

The main path improved here is:

```text
Claude Code
  -> Anthropic Messages API shape
  -> claudex OpenAIResponses adapter
  -> ChatGPT/Codex Responses endpoint
```

Fork fixes and additions:

- Better Anthropic Messages -> OpenAI Responses request conversion.
- Better Responses -> Anthropic response and SSE stream conversion.
- `/compact` support for streamed and non-streamed Responses shapes.
- Current-turn image routing via optional `image_model`.
- Old base64 image history pruning to avoid oversized requests.
- Tool-result image preservation for current requests.
- Reasoning effort request mapping.
- Structured output request mapping.
- Document/file block mapping where Responses can represent it.
- Prompt cache key mapping from Claude session metadata.
- Cached-token usage mapping back to Anthropic-style usage.
- Upstream `429` retry with capped `Retry-After` delay.
- Responses stream hardening for upstream failure/rate-limit events.
- `/v1/models` exposes configured Claude model slots without duplicates.

## Known Limits

- Codex hidden reasoning output is not displayed as Claude thinking.
- Anthropic-hosted server tools are not implemented proxy-side.
- Web/search/fetch behavior depends on Claude Code tools and upstream support.
- The ChatGPT/Codex backend may reject some model names for your account.
- Already-running proxy processes keep their old binary after symlink changes.

If a model alias returns `400 model is not supported`, map that Claude slot to a
model accepted by your ChatGPT/Codex account.

## Install

There is an `install.sh`, but in this fork it still targets upstream releases:

```sh
REPO="StringKe/claudex"
```

So there is no correct fork one-liner installer until this fork publishes its
own release assets and changes the installer repo.

For now, install from source:

```bash
cargo install --git https://github.com/pilc80/claudex
```

For a private SSH checkout:

```bash
cargo install --git ssh://git@github.com/pilc80/claudex.git
```

For local development:

```bash
git clone git@github.com:pilc80/claudex.git
cd claudex
cargo build --release
ln -sfn "$PWD/target/release/claudex" "$HOME/.local/bin/claudex"
```

Make sure `$HOME/.local/bin` is in `PATH`.

## ChatGPT/Codex Setup

Create or edit a profile like this:

```toml
[[profiles]]
name = "codex-sub"
provider_type = "OpenAIResponses"
base_url = "https://chatgpt.com/backend-api/codex"
default_model = "gpt-5.3-codex"
auth_type = "oauth"
oauth_provider = "openai"
enabled = true

# Optional: route only current-turn image requests to another model.
# Use only a model accepted by your ChatGPT/Codex account.
# image_model = "gpt-5.3-codex"

[profiles.models]
haiku = "gpt-5.3-codex"
sonnet = "gpt-5.3-codex"
opus = "gpt-5.3-codex"
```

Login and run:

```bash
claudex auth login chatgpt --profile codex-sub
claudex run codex-sub
```

Headless login:

```bash
claudex auth login chatgpt --profile codex-sub --force --headless
```

If Claude Code selects a haiku/sonnet/opus slot, claudex sends the mapped model
from `[profiles.models]`. Keep those aliases on models your account can use.

## Deploying A Local Build

Build and point the local command at the exact release binary:

```bash
cargo build --release
ln -sfn "$PWD/target/release/claudex" "$HOME/.local/bin/claudex"
```

Verify:

```bash
which claudex
readlink "$HOME/.local/bin/claudex"
shasum -a 256 "$HOME/.local/bin/claudex" target/release/claudex
```

Restart the proxy after deploying a new binary:

```bash
claudex proxy stop
claudex run codex-sub
```

Changing the symlink does not update already-running processes. If an old proxy
is alive, `claudex run` may reuse it instead of starting the new binary.

## Tests

Before publishing or deploying changes, run:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## Upstream Compatibility

This fork tries to keep non-OpenAIResponses behavior scoped and compatible with
upstream Claudex. DirectAnthropic and OpenAICompatible providers should keep
using the upstream paths unless a change is explicitly guarded for Responses.

## License

This fork keeps the upstream MIT License and copyright notice. See
[LICENSE](./LICENSE).
