# Codex Token Metrics Live

Native local web dashboard for Codex session token usage and estimated model cost.

`cdx-mtr` scans Codex JSONL session logs, aggregates token usage by day, model, and session, then serves a live dashboard on `127.0.0.1`. It is built as a small Rust binary so the dashboard opens immediately and heavy log parsing happens through an incremental cache.

The release installs two command names:

- `cdx-mtr`, the short default command.
- `codex-token-metrics-live`, the descriptive full command.

## Features

- Native Rust command with no Node or Bun runtime dependency.
- Instant first response from a cached snapshot.
- Background refresh that reparses only changed session files.
- Local-only HTTP server bound to `127.0.0.1`.
- HTML dashboard injection compatible with `~/codex-token-metrics.html`.
- JSON API for raw metrics at `/api/data`.
- Server-Sent Events at `/events` for live dashboard refresh.
- Built-in pricing overrides for recent GPT models, plus custom pricing through env vars.

## Install

Download the latest Linux x86_64 release:

```bash
curl -L -o /tmp/codex-token-metrics-live.tar.gz \
  https://github.com/fxrdhan/codex-token-metrics-live/releases/latest/download/codex-token-metrics-live-x86_64-unknown-linux-gnu.tar.gz

tar -xzf /tmp/codex-token-metrics-live.tar.gz -C /tmp
/tmp/codex-token-metrics-live/install.sh
```

Manual install:

```bash
install -m 755 /tmp/codex-token-metrics-live/codex-token-metrics-live ~/.local/bin/codex-token-metrics-live
ln -sfn codex-token-metrics-live ~/.local/bin/cdx-mtr
```

## Usage

Start on the default port, `8787`:

```bash
cdx-mtr
```

Start on a custom port:

```bash
cdx-mtr 9888
```

Open the printed URL:

```text
http://127.0.0.1:8787
```

## Data Sources

The command reads Codex session logs from:

- `~/.codex/sessions`
- `~/.codex/archived_sessions`

It looks for files named `rollout-*.jsonl` and extracts:

- token usage events
- session timestamps
- session IDs
- model names and turn counts

## Cache

Metrics are cached at:

```text
~/.cache/codex-token-metrics-live/metrics-cache.json
```

On startup, the server responds from the cached snapshot immediately. A background refresh checks session file size and modification time, then reparses only files that changed.

This keeps normal startup in the millisecond range even with gigabytes of historical Codex logs.

## Endpoints

| Endpoint | Description |
| --- | --- |
| `/` | HTML dashboard |
| `/index.html` | HTML dashboard |
| `/api/data` | Current metrics as JSON |
| `/events` | Server-Sent Events stream for refresh notifications |

## Environment Variables

| Variable | Default | Description |
| --- | --- | --- |
| `CODEX_METRICS_PORT` | `8787` | HTTP port. A positional port argument takes precedence. |
| `PORT` | unset | Fallback HTTP port. |
| `CODEX_METRICS_TEMPLATE` | `~/codex-token-metrics.html` | Dashboard HTML template path. |
| `CODEX_METRICS_CACHE` | `~/.cache/codex-token-metrics-live/metrics-cache.json` | Metrics cache path. |
| `CODEX_METRICS_REFRESH_DELAY_MS` | `100` | Delay before a background refresh starts. |
| `CODEX_METRICS_POLL_MS` | `2000` | Filesystem polling interval for session changes. |
| `CODEX_METRICS_RATES_JSON` | unset | JSON object for custom per-model rates. |

Example custom pricing:

```bash
CODEX_METRICS_RATES_JSON='{
  "gpt-5.5": { "input": 5.0, "cachedInput": 0.5, "output": 30.0 }
}' cdx-mtr
```

Rates are USD per 1M tokens.

## Build From Source

```bash
git clone https://github.com/fxrdhan/codex-token-metrics-live.git
cd codex-token-metrics-live
cargo build --release
```

The binary will be at:

```text
target/release/codex-token-metrics-live
```

Install from source:

```bash
cargo build --release
install -m 755 target/release/codex-token-metrics-live ~/.local/bin/codex-token-metrics-live
ln -sfn codex-token-metrics-live ~/.local/bin/cdx-mtr
```

## Release Packaging

Build a release archive locally:

```bash
scripts/package-release.sh
```

The script writes release assets into `dist/`:

- `codex-token-metrics-live-x86_64-unknown-linux-gnu.tar.gz`
- `codex-token-metrics-live-x86_64-unknown-linux-gnu.tar.gz.sha256`

## License

MIT. See [LICENSE](LICENSE).
