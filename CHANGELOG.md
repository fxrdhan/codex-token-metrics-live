# Changelog

## v0.2.1

- Add the dashboard HTML template to the repository.
- Embed the dashboard template in the binary as the default UI.
- Include the dashboard template in release archives.

## v0.2.0

- Attribute token usage and estimated cost to the active model for each turn.
- Use `last_token_usage` when available and fall back to cumulative token deltas.
- Skip duplicate token-count snapshots so rate-limit-only updates do not double count usage.
- Rebuild the metrics cache with the new per-model turn attribution format.

## v0.1.1

- Add `cdx-mtr` as a default installed command through a symlink.
- Include `install.sh` in release archives.
- Update installation docs to use `cdx-mtr` as the default command.

## v0.1.0

- Initial Rust binary release.
- Serve a local Codex token metrics dashboard on `127.0.0.1`.
- Parse Codex `rollout-*.jsonl` session logs.
- Cache parsed file metrics and reparse only changed files.
- Serve `/`, `/api/data`, and `/events`.
- Support custom model pricing through `CODEX_METRICS_RATES_JSON`.
