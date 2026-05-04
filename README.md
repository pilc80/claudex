# claudex — Claude Code proxy for ChatGPT/Codex

Use your ChatGPT Plus/Pro or Codex subscription in Claude Code via OAuth, with
no OpenAI API key.

Claudex is a high-fidelity Claude Code -> OpenAI Responses proxy for
ChatGPT/Codex users. It preserves Claude Code workflows across images, files,
tool results, `/compact`, context-limit recovery, prompt-cache usage mapping,
and hardened streaming errors.

```bash
curl -fL --progress-bar https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash
# When prompted, choose ChatGPT/Codex OAuth setup.
claudex
```

## Why this fork?

Most Claude Code -> Codex proxies are fine for text-only request routing. This
fork focuses on preserving full Claude Code workflows when OpenAI Responses/Codex
behaves differently from Anthropic Messages:

- `/compact` works with streamed and non-streamed Responses shapes.
- claudex intentionally does not invent cross-protocol auto-compaction. When
  Codex says the context window is full, claudex returns Claude Code's normal
  context-limit prompt so users can run `/compact` or `/clear`.
- Current-turn images and current tool-result images are preserved.
- Old base64 image history is pruned before oversized requests; upstream
  `v0.2.4` resends historical images and can hit the 32 MB request body limit.
- Document/file blocks are mapped where Responses can represent them.
- Prompt cache keys and cached-token usage are mapped back to Claude Code.
- Native Claude Code error semantics: unlike lightweight proxies that often
  collapse provider failures into generic 502s, claudex maps upstream failures
  to the closest Anthropic error type so Claude Code can compact, back off, or
  surface auth/config failures the same way it would against Anthropic.
  Deterministic request/account errors are returned directly; only retryable
  provider-health failures feed failover and circuit breakers.
- Error-only proxy dumps help diagnose upstream OpenAI and Claude-visible errors.

## Fork and upstream scope

This repository is a fork of [StringKe/claudex](https://github.com/StringKe/claudex),
based on upstream `v0.2.4`.

Use the upstream README and docs for the general Claudex feature set, CLI,
provider list, smart routing, config discovery, and non-Codex providers:

- Upstream README: https://github.com/StringKe/claudex
- Upstream docs: https://stringke.github.io/claudex/

This README documents the fork-specific Codex/OpenAI Responses work and local
setup details. The OpenAI Responses / ChatGPT/Codex path is the main validation
target; non-Responses providers stay close to upstream Claudex behavior.

## Goal

Make OpenAI/ChatGPT/Codex models run from Claude Code with high practical
fidelity: preserve Claude workflows, translate the Anthropic Messages protocol
accurately, and work around Responses/Codex endpoint limits where possible.

The main target path is:

```text
Claude Code
  -> Anthropic Messages API shape
  -> claudex OpenAIResponses adapter
  -> ChatGPT/Codex Responses endpoint
```

## Feature Status

Legend: `✅` works well, `☑️` is verified working with external/version drift
risk, `⚠️` is not fully confirmed, and `❌` has known issues or is
intentionally unsupported.

- ✅ Anthropic Messages -> OpenAI Responses request conversion.
- ✅ Responses -> Anthropic response and SSE stream conversion.
- ✅ `/compact` with streamed and non-streamed Responses shapes.
- ✅ Codex context-overflow errors translate to Claude Code's context-limit
  prompt so users can run `/compact` or `/clear` instead of seeing malformed
  proxy responses.
- ✅ Native Claude Code error semantics for context overflow, overload, rate
  limits, auth/permission failures, request-size errors, and upstream transport
  failures.
- ✅ Actionable proxy error classification: deterministic request/account errors
  are returned directly, while only retryable provider-health failures feed
  retry, failover, and circuit breakers.
- ✅ Error-only proxy dumps for upstream OpenAI errors and Claude-visible
  translated errors.
- ✅ Current-turn images, including optional `image_model` routing.
- ✅ Old base64 image-history pruning to avoid oversized requests.
- ✅ Tool calls/results, including current tool-result images.
- ✅ Reasoning effort request mapping.
- ✅ Structured output request mapping.
- ✅ Document/file block mapping where Responses can represent it.
- ✅ Prompt cache key and cached-token usage mapping.
- ✅ Upstream `429` retry with capped `Retry-After` delay.
- ✅ Responses stream hardening for failure/rate-limit events.
- ✅ `/v1/models` exposes Claude model slots without duplicates.
- ✅ ChatGPT/Codex profiles intentionally map every Claude model slot to
  `gpt-5.5` by default because this account rejects `gpt-5.5-mini`.
- ✅ Claude Code visible `Web Search` works as a client-side tool.
- ℹ️ Server-side Anthropic-hosted tools are intentionally out of proxy scope;
  claudex keeps Claude Code's local tool path instead.
- ℹ️ Codex hidden reasoning is not exposed as Claude Code thinking blocks.

Model aliases are an account-compatibility feature, not a bug workaround. Claude
Code may choose a haiku/sonnet/opus slot internally, but ChatGPT/Codex accounts
do not necessarily expose separate mini/pro/pro-tier model names through the
Codex backend. This account currently returns `400 model is not supported` for
`gpt-5.5-mini`, so the safe default maps every slot to `gpt-5.5`. If your
account accepts a different model, put that model in `[profiles.models]`.

When Codex reports that the request exceeds the model context window, claudex
returns Claude Code's normal context-limit error shape instead of streaming a
malformed proxy response. The user can then run `/compact` or `/clear` from
Claude Code.

Provider errors are translated by root cause to Anthropic-compatible error
semantics. Unlike lightweight Claude Code proxies that often collapse provider
failures into generic 502s, claudex keeps the error actionable for Claude Code
and the user: context overflow triggers Claude Code's normal compact flow,
overloaded and rate-limited upstreams use Claude Code's native backoff/error UX,
auth and permission failures stay recognizable, and local-model transport
failures report the unavailable local server instead of a generic malformed
HTTP 200 stream. Deterministic request/account errors such as invalid request
schemas, unsupported models, missing parameters, auth, billing, permissions, and
request-size failures are returned directly and do not poison circuit breakers;
retryable provider-health failures such as rate limits, overload, timeouts, 5xx,
and transport resets still feed retry, failover, and circuit-breaker protection.

claudex does not attempt fragile cross-protocol auto-compaction on purpose:
Claude Code's compaction behavior and OpenAI Responses/Codex error shapes can
change independently. Catching the context-limit signal and handing control back
to Claude Code is simpler, safer, and less likely to break on future API drift.

## When to choose this fork

Choose this fork if you want Claude Code to use a ChatGPT Plus/Pro or Codex
subscription through OAuth and you care about full Claude Code workflow fidelity,
not just text-only request routing:

- `/compact` and context-limit recovery.
- Current-turn images, current tool-result images, and historical image pruning
  to avoid repeatedly resending base64 images until the request exceeds 32 MB.
- Document/file block mapping.
- Prompt-cache usage mapping.
- Hardened OpenAI Responses streaming, rate-limit, and failure events.
- Actionable error classification: deterministic request/account errors stay
  visible instead of poisoning circuit breakers, while retryable provider-health
  failures still back off and fail over.
- Release installer checksums, latest-version validation, and stale-proxy restart
  guidance.

Use upstream Claudex if you primarily need a broad multi-provider manager and
are not relying on image-heavy Codex workflows. Use simpler proxies for text-only
request routing. Use this fork for full Claude Code workflow fidelity on
ChatGPT/Codex: images, tool-result images, files, `/compact`, context-limit
recovery, prompt-cache usage, model-slot mapping, and hardened Responses streams.

## Install

Release installer:

```bash
# macOS / Linux
curl -fL --progress-bar https://raw.githubusercontent.com/pilc80/claudex/main/install.sh | bash

# Windows PowerShell
irm https://raw.githubusercontent.com/pilc80/claudex/main/install.ps1 | iex
```

The installer downloads the latest release assets from `pilc80/claudex`, verifies
SHA256 checksums from the release manifest or `.sha256` files, installs both
`claudex` and `claudex-config`, checks that the latest version is the one found
in `PATH`, and can optionally set up a ChatGPT/Codex OAuth profile. If a running
proxy still uses the old binary, the installer can stop it or prints the exact
restart commands.

If no matching release asset exists for your platform, the installer can fall
back to `cargo install` unless source fallback is disabled.

Safer Windows flow:

```powershell
irm https://raw.githubusercontent.com/pilc80/claudex/main/install.ps1 -OutFile install.ps1
notepad .\install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

Useful installer options:

```bash
# macOS / Linux
sh install.sh --dry-run
sh install.sh --yes --no-setup
sh install.sh --profile codex-sub
sh install.sh --install-dir "$HOME/.local/bin"
sh install.sh --no-source-fallback

# Windows PowerShell
.\install.ps1 -DryRun
.\install.ps1 -Yes -NoSetup
.\install.ps1 -Profile codex-sub
.\install.ps1 -InstallDir "$HOME\.local\bin"
.\install.ps1 -NoSourceFallback
```

Source install:

```bash
cargo install --git https://github.com/pilc80/claudex --force
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
ln -sfn "$PWD/target/release/claudex-config" "$HOME/.local/bin/claudex-config"
```

Make sure `$HOME/.local/bin` is in `PATH`.

## Command Split

This fork intentionally separates the Claude-compatible launcher from setup:

```text
claudex
  Claude-compatible launcher. It passes all arguments to Claude Code unchanged.
  After a `codex-sub` profile exists, plain `claudex` uses it by default.

claudex-config
  Claudex setup and management: auth, proxy, config/profile, sets,
  and the inherited `run <profile>` command family.
```

This keeps `claudex` close to `claude` at the launcher layer: flags such as
`claudex --resume <session-id>` are forwarded to Claude Code instead of being
claimed by the management CLI. Management stays in `claudex-config`; obsolete
side commands such as the old dashboard, self-update command, and fake
`proxy start --daemon` path are intentionally not part of the fork CLI.
The previous Claudex management behavior remains available through
`claudex-config`, including profiles, proxy control, OAuth, configuration sets,
and `claudex-config run <profile>`.

Normal launch options come from environment variables:

```bash
claudex --resume <session-id>
CLAUDEX_MODEL=gpt-5.5 claudex
CLAUDEX_HYPERLINKS=on claudex
CLAUDEX_PROFILE=other-profile claudex
```

`CLAUDEX_PROFILE` defaults to `codex-sub` when that profile exists, otherwise
to the first enabled profile. `CLAUDEX_CONFIG` can point at a custom config file.

## ChatGPT/Codex OAuth Setup

The installer can offer this setup interactively. To run it manually:

```bash
claudex-config auth login chatgpt --profile codex-sub
claudex
```

Headless device-code login:

```bash
claudex-config auth login chatgpt --profile codex-sub --force --headless
```

Manual profile shape:

```toml
[[profiles]]
name = "codex-sub"
provider_type = "OpenAIResponses"
base_url = "https://chatgpt.com/backend-api/codex"
default_model = "gpt-5.5"
auth_type = "oauth"
oauth_provider = "openai"
enabled = true

# Optional: route only current-turn image requests to another accepted model.
# Keep unset unless your account accepts that model.
# image_model = "gpt-5.5"

[profiles.models]
haiku = "gpt-5.5"
sonnet = "gpt-5.5"
opus = "gpt-5.5"
```

Keep `haiku = "gpt-5.5"` for ChatGPT/Codex unless you have tested that your
account accepts another model. This is intentional: Claude Code may ask for a
haiku slot, but the Codex backend can still reject `gpt-5.5-mini` for ChatGPT
subscription accounts with `400 model is not supported`.
If Claude Code selects a haiku/sonnet/opus slot, claudex sends the mapped model
from `[profiles.models]`. Keep those aliases on models your account can use.

## Deploying A Local Build

Build and point the local command at the exact release binary:

```bash
cargo build --release
ln -sfn "$PWD/target/release/claudex" "$HOME/.local/bin/claudex"
ln -sfn "$PWD/target/release/claudex-config" "$HOME/.local/bin/claudex-config"
```

Verify:

```bash
which claudex
readlink "$HOME/.local/bin/claudex"
readlink "$HOME/.local/bin/claudex-config"
claudex-config --version
shasum -a 256 target/release/claudex target/release/claudex-config
```

Restart the proxy after deploying a new binary:

```bash
claudex-config proxy stop
claudex
```

Changing the symlink does not update already-running processes. If an old proxy
is alive, `claudex` may reuse it instead of starting the new binary.

## Release Integrity

Each release should publish:

```text
claudex-<version>-<target>.tar.gz
claudex-<version>-<target>.tar.gz.sha256
claudex-<version>-x86_64-pc-windows-msvc.zip
claudex-<version>-x86_64-pc-windows-msvc.zip.sha256
claudex-release-manifest.json
```

Archives contain both `claudex` and `claudex-config`.

The release workflow also emits GitHub artifact attestations for release
archives and the manifest. Use GitHub CLI attestation verification for provenance
checks when needed.

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
