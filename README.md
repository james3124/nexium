<div align="center">

# NexiumX

**The headless browser engine written from scratch, in Rust.**

No Chromium. No WebKit. No Playwright wrapper. The HTML parser, DOM, CSS
engine, layout, JavaScript runtime, networking and renderer are all
implemented in this repository.

[![npm](https://img.shields.io/npm/v/nexiumx)](https://www.npmjs.com/package/nexiumx)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust)](https://www.rust-lang.org)

</div>

## Why another engine?

Because a headless browser is not a browser with the window hidden — it is a
different product with different constraints. NexiumX is built for servers,
crawlers and AI agents:

- **Independent implementation.** Nothing is delegated to an existing browser
  engine, so behavior is inspectable and the whole pipeline is in one language.
- **Headless first.** The primary outputs are DOM trees, structured snapshots,
  accessibility trees and PNG/JPEG screenshots — not a window.
- **Automation-native.** A persistent JavaScript runtime, viewport-bound input,
  REST API, WebSocket events and an MCP server ship in the same binary.
- **Embeddable.** Use the Rust crate directly, drive the CLI, or connect over
  HTTP/MCP from another process or language.
- **Honest scope.** Unsupported web-platform features are tracked explicitly in
  [docs/STATUS.md](docs/STATUS.md).

## Features

| Area | What works today |
|---|---|
| Parsing | WHATWG HTML tokenizer and tree builder, adoption agency, templates, SVG/MathML namespaces, charset detection |
| DOM | Arena-backed DOM, attributes and namespaces, selectors (combinators, `:nth-child`, `:not`, `:hover`), HTML serialization |
| CSS | Cascade/specificity, inheritance, variables, `calc`/`min`/`max`/`clamp`, `@media` with range syntax, `@supports`, `@import`, `@namespace` |
| Layout | Block/inline flow, margin collapsing, flexbox, basic grid, tables (colspan, rowspan, border-collapse, captions), positioning |
| Paint | Backgrounds, borders/radius, text rasterization (bundled DejaVu), z-index, SVG shapes/gradients/patterns, Canvas 2D with text |
| JavaScript | Boa runtime, DOM/events, ES modules with imports and dynamic `import()`, observers, cookies/storage, IndexedDB, Web Workers |
| Networking | HTTP/1.1 + HTTP/2 over Rustls, redirects, cookies (SameSite), disk cache with revalidation, CONNECT/SOCKS5 proxies, `no_proxy`, SSRF checks |
| Interaction | Hit testing, mouse/keyboard events, `:hover`, Tab focus, form submission, contenteditable, accessibility tree, DOM snapshots |
| Control plane | REST + WebSocket + MCP (stdio, Streamable HTTP, legacy SSE), per-agent sessions, tokens, rate limits, audit log, native HTTPS |

## Quick start

### Install

Through npm (the launcher downloads a platform binary from GitHub Releases):

```bash
npm install --global nexiumx
nexium https://example.com --dump-dom
```

Or download a release archive directly:

| Platform | Archive |
|---|---|
| Linux x86_64 | `nexium-v0.8.1-linux-x64.tar.gz` |
| Linux arm64 | `nexium-v0.8.1-linux-arm64.tar.gz` |
| macOS arm64 | `nexium-v0.8.1-darwin-arm64.tar.gz` |
| macOS x86_64 | `nexium-v0.8.1-darwin-x64.tar.gz` |
| Windows x86_64 | `nexium-v0.8.1-win32-x64.tar.gz` |

```bash
curl -L -o nexium.tar.gz \
  https://github.com/james3124/nexium/releases/download/v0.8.1/nexium-v0.8.1-linux-x64.tar.gz
tar -xzf nexium.tar.gz
./nexium --version
```

Or build from source:

```bash
git clone https://github.com/james3124/nexium
cd nexium
cargo build --release
./target/release/nexium --help
```

### Fetch and inspect a page

```bash
nexium https://example.com                 # summary
nexium https://example.com --dump-html     # serialized DOM
nexium https://example.com --dump-dom      # indented tree
nexium https://example.com --selector "a"  # query elements
nexium https://example.com --json          # JSON summary
```

### Render screenshots

```bash
nexium https://example.com --screenshot page.png
nexium https://example.com --screenshot view.png --viewport 1280x720 --viewport-shot
nexium https://example.com --screenshot heading.png --element "h1"
```

### Run the MCP server

```bash
nexium --mcp                               # stdio transport
nexium --api --bind 127.0.0.1:9222         # REST + WebSocket + MCP over HTTP
```

MCP client configuration:

```json
{
  "mcpServers": {
    "nexium": {
      "command": "npx",
      "args": ["-y", "nexiumx", "--mcp"]
    }
  }
}
```

HTTP transports: `POST /mcp` (JSON-RPC), Streamable HTTP (`GET /mcp` SSE
stream, `DELETE /mcp`), and legacy `GET /mcp/sse` + `POST /mcp/message`.

### Tool calls

MCP servers expose `initialize`, `ping`, `tools/list` and `tools/call`:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call",
 "params":{"name":"browser_navigate","arguments":{"url":"https://example.com"}}}
```

The 19 `browser_*` tools: `browser_create`, `browser_close`,
`browser_navigate`, `browser_click`, `browser_type`, `browser_press`,
`browser_scroll`, `browser_hover`, `browser_screenshot`, `browser_snapshot`,
`browser_extract`, `browser_wait`, `browser_tabs`, `browser_switch_tab`,
`browser_session_save`, `browser_session_restore`, `browser_evaluate`,
`browser_network`, `browser_status`.

### REST control plane

```bash
curl -X POST http://127.0.0.1:9222/browser/create \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com"}'
# -> {"ok":true,"data":{"session_id":"sess-..."}}

curl -X POST http://127.0.0.1:9222/browser/snapshot \
  -H 'X-Nexium-Session: sess-...' \
  -H 'Content-Type: application/json' -d '{}'
```

Endpoints: `/health`, `/browser/{create,close,status,navigate,back,forward,
reload,click,type,press,scroll,hover,screenshot,snapshot,extract,evaluate,
tabs,tabs/create,tabs/close,tabs/switch,session/save,session/restore,network,
console}`, `/ws`, `/v1/audit`, `/v1/admin/tokens*`.

Responses use `{"ok":true,"data":...}` or structured errors. Authentication
uses `Authorization: Bearer <token>`; requests select an agent with
`X-Nexium-Agent` and a session with `X-Nexium-Session`. Browser-based MCP
inspectors are supported through loopback-origin CORS.

### HTTPS

```bash
nexium --api --bind 127.0.0.1:9222 \
  --tls-cert cert.pem --tls-key key.pem          # HTTPS on the bind port
nexium --api --bind 127.0.0.1:9222 \
  --tls-cert cert.pem --tls-key key.pem \
  --tls-bind 127.0.0.1:9443                      # HTTP + HTTPS together
```

The API port also accepts both schemes on one port (TLS is detected per
connection), so clients that upgrade to `https://` still connect.

## Use it as a library

```rust
use nexium::engine::css::Viewport;
use nexium::{Browser, Config, RenderOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = Browser::new(Config::default());
    let page = browser.open("https://example.com").await?;

    let rendered = browser
        .render(&page, RenderOptions {
            viewport: Viewport { width: 1280.0, height: 720.0 },
            ..RenderOptions::default()
        })
        .await?;

    rendered.save_png("page.png")?;
    println!("{} — {}", page.title().unwrap_or_default(), page.url);
    Ok(())
}
```

Interaction uses a viewport-bound session over a loaded page:

```rust
use nexium::engine::css::Viewport;
use nexium::{Browser, Config, PageSession};

let browser = Browser::new(Config::default());
let page = browser.open("https://example.com").await?;
let mut session = PageSession::new(page, Viewport { width: 1280.0, height: 720.0 });

session.click(100.0, 80.0);
session.type_text("hello");
session.scroll_by(0.0, 400.0);

let tree = session.accessibility_tree();
let snapshot = session.dom_snapshot();
```

## Architecture

```
src/
├── main.rs               CLI (inspect, screenshot, API/MCP servers)
├── api/                  Axum REST routes, sessions, WebSocket, TLS
├── mcp/                  MCP JSON-RPC handlers and tool schemas
├── engine/
│   ├── html/             tokenizer + tree builder
│   ├── dom/              arena DOM, selectors, serialization
│   ├── css/              tokenizer, parser, cascade, computed styles
│   ├── javascript/       Boa runtime, DOM/events, IndexedDB, canvas, workers
│   ├── layout/           block/inline/flex/grid/table layout
│   ├── paint/            CSS/SVG/canvas painting and glyph rasterization
│   └── text.rs           font loading and text measurement
├── networking/           HTTP(S) client, cookies, cache, proxies, SSRF
├── browser/              tabs, history, profiles, sessions
├── interaction/          viewport/scroll, hit testing, input, accessibility
├── storage/              atomic filesystem store, profiles
├── rendering/            render pipeline + screenshot encode/crop/diff
└── intelligence/         DOM dumps, visible text
```

## Status and roadmap

NexiumX implements Phases 1–8 as documented, with explicit gaps tracked rather
than hidden:

- Runtime status, test coverage and known limitations:
  [docs/STATUS.md](docs/STATUS.md)
- Phase-by-phase delivery and remaining gaps:
  [docs/ROADMAP.md](docs/ROADMAP.md)
- npm packaging and release workflow: [docs/NPM.md](docs/NPM.md)

Not implemented yet: CSS transforms/transitions/animations, floats, subgrid,
flex column wrapping, SVG filters, box shadows, HTTP/3, and full WPT
conformance.

## Development

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The test suite (308 tests) runs entirely in-process — no external network is
required. A self-contained rendering demo is available:

```bash
cargo run --example render_demo   # writes /tmp/nexium-demo.png
```

## Contributing

Issues and pull requests are welcome. Please keep changes scoped, run the
checks above, and update [docs/STATUS.md](docs/STATUS.md) when behavior or
limitations change.

## License

[MIT](LICENSE). Bundled DejaVu fonts are covered by
`assets/fonts/LICENSE-DejaVu.txt`; third-party Rust crates retain their own
licenses.
