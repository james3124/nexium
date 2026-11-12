# NEXIUM

An independent, Rust-powered headless browser engine. NEXIUM does not wrap
Chromium, Firefox, WebKit, Playwright, Puppeteer or any other complete browser
engine: the HTML parser, DOM, CSS engine, layout, text rendering, JavaScript
runtime, networking and screenshot pipeline are implemented in this repository.

Implementation status, test coverage, known limitations and licensing are
documented in [docs/STATUS.md](docs/STATUS.md); the phase-by-phase delivery
checklist lives in [docs/ROADMAP.md](docs/ROADMAP.md).

## Build and run

```bash
cargo build --release
cargo test

# Fetch and summarize a page
cargo run -- https://example.com

# Inspect structure
cargo run -- https://example.com --dump-dom
cargo run -- https://example.com --selector "a.link"
cargo run -- https://example.com --json

# Render screenshots
cargo run -- https://example.com --screenshot page.png
cargo run -- https://example.com --screenshot view.png --viewport 1280x720 --viewport-shot
cargo run -- https://example.com --screenshot heading.png --element "h1"

# REST + WebSocket control server
cargo run -- --api --bind 127.0.0.1:9222 --api-token secret

# HTTPS (native TLS termination)
cargo run -- --api --bind 127.0.0.1:9222 \
  --tls-cert nexium-data/tls/cert.pem --tls-key nexium-data/tls/key.pem

# HTTP + HTTPS at the same time
cargo run -- --api --bind 127.0.0.1:9222 \
  --tls-cert nexium-data/tls/cert.pem --tls-key nexium-data/tls/key.pem \
  --tls-bind 127.0.0.1:9443

# MCP server over stdin/stdout
cargo run -- --mcp
```

Configuration lives in [config/default.toml](config/default.toml); pass
`--config <path>` to override it. `api_bind`, `api_token`, `api_tokens`,
`api_tls_cert`, `api_tls_key`, `api_tls_bind`, `api_rate_limit_per_minute`,
`api_audit_capacity`, `api_max_sessions`, `api_session_idle_timeout_ms`,
`max_tabs`, `block_private_networks` and `no_proxy` configure runtime limits
and security. Logging uses `RUST_LOG` (tracing).

Through npm (downloads a prebuilt binary from GitHub Releases):

```bash
npm install --global nexiumx
nexium https://example.com --dump-dom
npx nexiumx --mcp      # MCP over stdio
```

Packaging details and release workflow: [docs/NPM.md](docs/NPM.md).

A self-contained visual demo is available:

```bash
cargo run --example render_demo   # writes /tmp/nexium-demo.png
```

## Control API and MCP tools

The control plane exposes a REST API, a WebSocket command/event channel and an
MCP server. It speaks HTTPS when `--tls-cert`/`--tls-key` (or
`api_tls_cert`/`api_tls_key`) are set, otherwise plain HTTP; adding
`--tls-bind`/`api_tls_bind` serves plain HTTP and HTTPS simultaneously on two
addresses. The HTTP bind also sniffs TLS handshakes, so clients and browsers
that upgrade to `https://` on that port still connect; `GET /` returns a small
JSON endpoint index. Responses use
`{"ok":true,"data":...}` or structured errors.
Requests select an agent with `X-Nexium-Agent` and a session with
`X-Nexium-Session`; when authentication is configured, send
`Authorization: Bearer <token>`.

REST example (token and agent headers are optional only when auth is not
configured):

```bash
curl -X POST http://127.0.0.1:9222/browser/create \
  -H 'Authorization: Bearer secret' -H 'X-Nexium-Agent: agent-1' \
  -H 'Content-Type: application/json' -d '{"url":"https://example.com"}'
# -> {"ok":true,"data":{"session_id":"sess-..."}}

curl -X POST http://127.0.0.1:9222/browser/snapshot \
  -H 'Authorization: Bearer secret' -H 'X-Nexium-Agent: agent-1' \
  -H 'X-Nexium-Session: sess-...' -H 'Content-Type: application/json' -d '{}'
```

REST endpoints:

- `GET /health`
- `POST /browser/create|close|navigate|back|forward|reload`
- `POST /browser/click|type|press|scroll|hover`
- `POST /browser/screenshot|snapshot|extract|evaluate`
- `GET /browser/status|tabs|network|console`
- `POST /browser/tabs/create|close|switch`
- `POST /browser/session/save|restore`
- `GET /v1/audit`
- `GET /v1/admin/tokens`, `POST /v1/admin/tokens/rotate`,
  `DELETE /v1/admin/tokens/{id}`

Transport endpoints:

- `GET /ws` — WebSocket commands plus session/navigation/loading/console/
  network/screenshot/interaction events, filtered per agent.
- `POST /mcp` — MCP JSON-RPC over HTTP (Streamable HTTP); responses may carry
  `Mcp-Session-Id`.
- `GET /mcp` with `Accept: text/event-stream` — Streamable HTTP server stream;
  `DELETE /mcp` terminates the session.
- `GET /mcp/sse` + `POST /mcp/message?sessionId=...` — legacy MCP SSE transport.
- CORS is enabled for loopback origins (`localhost`/`127.0.0.1`/`::1`) so
  browser-based MCP inspectors can connect; preflight and
  `Access-Control-Request-Private-Network` are handled.
- `GET /.well-known/oauth-protected-resource` — RFC 9728 metadata so MCP
  clients that probe for OAuth either use the static bearer token or proceed
  unauthenticated.

MCP exposes `initialize`, `ping`, `tools/list` and `tools/call`. Calls use the
standard JSON-RPC shape:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call",
 "params":{"name":"browser_navigate","arguments":{"url":"https://example.com"}}}
```

The 19 `browser_*` tools are: `browser_create`, `browser_close`,
`browser_navigate`, `browser_click`, `browser_type`, `browser_press`,
`browser_scroll`, `browser_hover`, `browser_screenshot`, `browser_snapshot`,
`browser_extract`, `browser_wait`, `browser_tabs`, `browser_switch_tab`,
`browser_session_save`, `browser_session_restore`, `browser_evaluate`,
`browser_network` and `browser_status`.

## Library usage

```rust
use nexium::engine::css::Viewport;
use nexium::{Browser, Config, RenderOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = Browser::new(Config::default());
    let page = browser.open("https://example.com").await?;

    println!("{} — {}", page.title().unwrap_or_default(), page.url);

    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: Viewport { width: 1280.0, height: 720.0 },
                ..RenderOptions::default()
            },
        )
        .await?;

    rendered.save_png("page.png")?;                 // full document
    rendered.viewport_image().save("viewport.png")?;
    if let Some(node) = page.select("h1")? {
        if let Some(image) = rendered.element_image(node) {
            image.save("heading.png")?;
        }
    }
    Ok(())
}
```

Offline parsing is available through `nexium::parse_html`, `Page::from_html`
and direct `StyleResolver`/`layout_document` calls.

Interaction uses a viewport-bound session over a loaded page:

```rust
use nexium::engine::css::Viewport;
use nexium::{Browser, Config, PageSession};

let browser = Browser::new(Config::default());
let page = browser.open("https://example.com").await?;
let mut session = PageSession::new(page, Viewport { width: 1280.0, height: 720.0 });

if let Some(element) = session.element_at(100.0, 80.0) {
    println!("{} {:?}", element.tag, element.role);
}
session.click(100.0, 80.0);
session.type_text("hello");
session.scroll_by(0.0, 400.0);

let tree = session.accessibility_tree();
let snapshot = session.dom_snapshot();
let image = session.viewport_image(browser.client()).await?;
image.save("viewport.png")?;
let _ = (tree, snapshot);
```

## Architecture (current)

```
src/
├── main.rs               CLI (inspect, screenshot, API/MCP servers)
├── lib.rs                public API
├── config.rs             TOML configuration
├── api/                  Axum REST routes, session service, WebSocket
├── mcp/                  MCP JSON-RPC handlers + browser tool schemas
├── engine/
│   ├── html/             tokenizer + tree builder
│   ├── dom/              arena DOM, selectors, serialization
│   ├── css/              tokenizer, parser, values, cascade, UA sheet
│   ├── javascript/       Boa runtime, DOM/events, IndexedDB, canvas, workers
│   ├── layout/           block/inline/flex/grid/table layout, positioning
│   ├── paint/            CSS/SVG/canvas painting and glyph rasterization
│   └── text.rs           font loading and text measurement
├── networking/           HTTP(S) client, cookies, HTTP cache, proxies/SSRF
├── browser/              tabs, history, profiles, sessions, page loading
├── interaction/          viewport/scroll, hit testing, input, accessibility
├── storage/              atomic filesystem store, profile persistence
├── rendering/            render pipeline + screenshot encode/crop/diff
├── intelligence/         DOM dumps, visible text
└── utils/                errors, logging
assets/fonts/             bundled DejaVu Sans/Serif/Mono (license included)
docs/STATUS.md            implementation status, testing, limitations, license
docs/ROADMAP.md           phase checklist and tracked gaps
```

## License

MIT — see [LICENSE](LICENSE). Bundled DejaVu fonts are covered by the license
in `assets/fonts/LICENSE-DejaVu.txt`; third-party Rust crates retain their own
licenses.
