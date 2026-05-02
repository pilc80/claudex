# Fork Notes

This repository is a fork of [StringKe/claudex](https://github.com/StringKe/claudex), based on upstream `v0.2.4`.

## Current Changes

- Fix Codex Responses backend compatibility when the upstream requires streaming requests.
- Preserve `/compact` summaries from streamed and non-streamed Responses API shapes.
- Preserve image tool results in Responses translation.
- Mitigate repeated base64 image history causing 32 MB request failures.

## Upstream

- Upstream repository: https://github.com/StringKe/claudex
- Base version: `v0.2.4`
- License: MIT

## License

This fork keeps the upstream MIT License and copyright notice in [LICENSE](./LICENSE).
