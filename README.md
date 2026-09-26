# sloyd-mcp

Thin Rust MCP wrapper around a normal authenticated Sloyd Web session.

## What it exposes

Only four agent tools:

- `generate` — Image-to-3D or Text-to-3D low-poly generation
- `status`
- `download` — original GLB only
- `retry`

The wrapper captures the authenticated Web session headers in memory, then submits jobs directly to the same Sloyd Web backend. Completed GLBs are downloaded directly from Sloyd's public Google Cloud Storage object path.

## Why Lightpanda

On the same Sloyd Image-to-3D page we measured roughly 91 MB RSS for Lightpanda versus about 893 MB across Chromium's process tree. Chromium remains a fallback.

## Bot verification

Sloyd's current frontend includes Cloudflare Turnstile for its format-conversion endpoint. The observed Image-to-3D and Text-to-3D job endpoints do not use a Turnstile token. This wrapper downloads GLB directly and does not invoke the converter.

If a generation/status response starts requiring Turnstile/CAPTCHA, the wrapper fails closed instead of retrying or attempting to bypass verification.

## Setup

Download a Lightpanda binary to `bin/lightpanda`, or use Chromium fallback.

Import browser-exported Sloyd cookies:

```bash
cargo run -- import-cookies /path/to/cookies.json
cargo run -- session-probe
```

Sloyd Plus:

```bash
cargo run -- mcp --concurrency 2
```

Sloyd Pro:

```bash
cargo run -- mcp --concurrency 5
```

Supported low-poly face counts are 3000, 4000, 5000, 10000, 20000, 40000, and 100000.

## Security

- No plaintext password storage.
- Cookie import keeps only `*.sloyd.ai` cookies.
- Cookie file is written with mode 0600 on Unix.
- `state/`, binaries, downloads, keys, and environment files are git-ignored.
- Authorization/session headers are kept in memory and never intentionally logged.

## Scope

This project automates normal Web subscription usage. It does not bypass Sloyd concurrency, fair-use, CAPTCHA, Turnstile, or subscription controls.
