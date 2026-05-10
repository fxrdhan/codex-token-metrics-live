# Changelog

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
