# sloyd-mcp

Thin Rust MCP wrapper for Sloyd Web subscriptions.

## Normal path: browserless

The default path no longer needs Lightpanda or Chromium:

```text
auth.sloyd.ai session cookies
  -> Auth0 silent authorize (PKCE)
  -> access token kept in RAM only
  -> Sloyd /api/jobs/*
  -> jobId
  -> public GCS GLB
```

Browser backends remain optional fallbacks only.

## MCP tools

Exactly four tools are exposed:

- `generate` — Image-to-3D or Text-to-3D
- `status`
- `download` — original GLB only
- `retry`

The service queues work instead of holding an MCP call open while Sloyd is generating. Concurrency is capped at 5.

## Required cookies

For browserless operation the important cookies are the Auth0 server-session cookies from `auth.sloyd.ai`:

- `auth0` — primary session cookie
- `auth0_compat` — compatibility copy; recommended to keep as well

One of those is sufficient for the importer/backend to attempt silent auth; keeping both is recommended.

These are optional and are not treated as login credentials:

- `did`
- `did_compat`
- `__cf_bm`

These are not required by the browserless PKCE flow:

- `auth0.<client>.is.authenticated`
- `_legacy_auth0.<client>.is.authenticated`
- Intercom, Mixpanel, Google Analytics, Partnero, advertising cookies

Importing a full Cookie-Editor export is fine: the importer discards non-Sloyd domains, and the Auth0 client only sends the small Auth0 subset to `auth.sloyd.ai`.

## Setup

Build:

```bash
cd /root/sloyd-mcp
cargo build --release
```

Import the cookie JSON exported from the browser:

```bash
./target/release/sloyd-mcp import-cookies /path/to/sloyd-cookies.json
```

The normalized file is stored at:

```text
/root/sloyd-mcp/state/cookies.json
```

with Unix mode `0600`.

Verify silent login without starting a browser:

```bash
./target/release/sloyd-mcp --engine http session-probe
```

Expected:

```text
authenticated=true
```

Start the MCP server for Sloyd Plus:

```bash
./target/release/sloyd-mcp mcp --concurrency 2
```

For Pro:

```bash
./target/release/sloyd-mcp mcp --concurrency 5
```

`Auto` is the default engine. It tries browserless Auth0 first, then falls back to Lightpanda or Chromium only if a fallback binary exists.

## Claude Code

Example project `.mcp.json`:

```json
{
  "mcpServers": {
    "sloyd": {
      "command": "/root/sloyd-mcp/target/release/sloyd-mcp",
      "args": ["mcp", "--concurrency", "2"]
    }
  }
}
```

## Codex

Example `~/.codex/config.toml`:

```toml
[mcp_servers.sloyd]
command = "/root/sloyd-mcp/target/release/sloyd-mcp"
args = ["mcp", "--concurrency", "2"]
```

## Low-poly settings

Supported face counts:

```text
3000
4000
5000
10000
20000
40000
100000
```

Image-to-3D uploads the reference image directly to the same Sloyd Web job endpoint. Text-to-3D sends the prompt directly.

## GLB download

Completed models are fetched directly from Sloyd's documented/public object path:

```text
https://storage.googleapis.com/ai-services-quality/jobs/<jobId>.glb
```

The wrapper verifies the `glTF` magic before writing the result.

It intentionally does not invoke Sloyd's format converter for FBX/OBJ/etc.

## Turnstile / CAPTCHA

Sloyd's current frontend contains Cloudflare Turnstile for its separate format-conversion flow. The observed Image-to-3D and Text-to-3D job paths do not require a Turnstile token.

If generation or status ever starts returning Turnstile/CAPTCHA/challenge markers, the wrapper fails closed before retrying. It does not attempt to bypass interactive verification.

## Security

- No plaintext password storage.
- Access tokens are kept in RAM only.
- Authorization headers are not intentionally logged.
- Cookie import keeps only `*.sloyd.ai` cookies.
- Browserless Auth0 sends only Auth0-related cookies to `auth.sloyd.ai`.
- Cookie file mode is `0600` on Unix.
- `state/`, downloads, browser binaries, `.env`, keys, and certificates are git-ignored.
- The project does not bypass Sloyd concurrency, subscription, fair-use, CAPTCHA, or Turnstile controls.
