# NEXIUM

An independent, Rust-powered headless browser engine. NEXIUM does not wrap
Chromium, Firefox, WebKit, Playwright, Puppeteer or any other complete browser
engine: the HTML parser, DOM, CSS engine, layout, text rendering, networking
and screenshot pipeline are implemented in this repository.

## Status

**Phase 1 — Foundation** is implemented and tested:

- URL parsing and normalization (bare hosts default to HTTPS).
- Async HTTP/1.1 + HTTP/2 client over Rustls with connection pooling,
  redirects, timeouts, response size limits and buffered bodies.
- Character-set decoding: BOM, HTTP `charset`, `<meta charset>`, UTF-8 fallback
  (via `encoding_rs`, covering Windows-1252, GBK, Shift_JIS and others).
- HTML tokenizer and tree builder: tags, attributes, comments, doctypes, the
  full WHATWG named character reference table, RCDATA/RAWTEXT/script, implied
  elements, void elements, quirks-mode detection, templates with detached
  content fragments, and SVG/MathML foreign content (namespaces, case
  restoration, self-closing tags, HTML integration points and breakout).
- Formatting-element reconstruction and a pragmatic adoption-agency pass for
  misnested inline markup, including block splitting and re-wrapping.
- Arena-backed DOM with mutation APIs, text content, HTML serialization,
  `QuirksMode`, namespaces, template content, and a CSS selector engine: type,
  `#id`, `.class`, attribute operators with `i`/`s` flags, `html|`/`svg|`/
  `math|`/`*|` namespaces, all sibling combinators, `:first-child`,
  `:nth-child()`, `:not()`, `:root`, `:empty`, plus `::before`/`::after`
  generated-content matching.
- Networking: gzip/deflate/brotli decompression, an in-memory cookie jar
  (domain/path/secure/expiry aware), a fresh-GET HTTP cache, HTTP CONNECT
  proxy support, and request/response interception hooks.
- Non-HTML responses are handled correctly: text/JSON/XML documents become
  preformatted text instead of being parsed as HTML; binary payloads are not
  parsed.

**Phase 2 — CSS, layout and rendering** is implemented and tested:

- CSS tokenizer and parser: rules, declarations, `!important`, nested
  `@media` (width/height, orientation, aspect-ratio, color-scheme),
  `@supports`, `@import` (fetched and expanded in order), comments, functions,
  URLs, custom properties, and `calc()` linear length expressions.
- Cascade and computed styles: UA + author + inline origins, specificity,
  source order, `!important`, inheritance, `var()` with fallbacks, the common
  color formats (named, hex, `rgb()`, `rgba()`, `hsl()`), and relative units
  (`em`, `rem`, `%`, `pt`, `vw`, `vh`, `vmin`, `vmax`, `calc()`).
- Shorthand expansion for `margin`, `padding`, `border*`, `inset`, `font`,
  `flex`, `gap`, `background`, `list-style`.
- Layout engine: block formatting with sibling and parent/child margin
  collapsing, inline formatting with word wrapping and CJK breaks, text
  measurement through fontdue, inline-block, multi-line flexbox (row,
  grow/shrink, gap, flex-wrap, justify-content, align-items, align-content),
  basic table layout (rows, cells, colspan, cell stretching, border-spacing),
  relative/absolute/fixed positioning, percentage heights, min/max sizing,
  display:none, overflow clipping.
- Paint pipeline: backgrounds (repeat/position/size), border-radius, solid/
  dashed/dotted/double borders, text glyph rasterization (bundled DejaVu
  Sans/Serif/Mono with synthetic italics), underlines, list markers, replaced
  images, basic z-index ordering, alpha compositing and opacity.
- Screenshots: full page, viewport and element crops; PNG and JPEG output;
  resizing and pixel comparison; `--screenshot` CLI support.
- Author stylesheets: `<style>` blocks, `<link rel="stylesheet">` resources and
  `@import` chains are fetched and applied in document order.

Not implemented yet: JavaScript execution and DOM bindings, persistent
storage (cookies/cache are currently in-memory), tabs and sessions,
REST/WebSocket/MCP APIs, mouse/keyboard interaction, accessibility tree, CSS
Grid, CSS transforms/transitions/animations, SVG/canvas painting, box shadows,
gradients, and HTTP/3. These are tracked in [docs/ROADMAP.md](docs/ROADMAP.md).

NEXIUM is a work in progress and does **not** claim web-platform compatibility.
See "Known limitations" below.

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
```

Configuration lives in [config/default.toml](config/default.toml); pass
`--config <path>` to override it. Logging uses `RUST_LOG` (tracing).

A self-contained visual demo is available:

```bash
cargo run --example render_demo   # writes /tmp/nexium-demo.png
```

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

## Architecture (current)

```
src/
├── main.rs               CLI (inspect + screenshot)
├── lib.rs                public API
├── config.rs             TOML configuration
├── engine/
│   ├── html/             tokenizer + tree builder
│   ├── dom/              arena DOM, selectors, serialization
│   ├── css/              tokenizer, parser, values, cascade, UA sheet
│   ├── layout/           box tree, block/inline/flex layout, positioning
│   ├── paint/            display painting and glyph rasterization
│   └── text.rs           font loading and text measurement
├── networking/client.rs  HTTP(S) client
├── browser/              page loading, stylesheet collection
├── rendering/            render pipeline + screenshot encode/crop/diff
├── intelligence/         DOM dumps, visible text
└── utils/                errors, logging
assets/fonts/             bundled DejaVu Sans/Serif/Mono (license included)
```

## Testing

`cargo test` runs 161 tests covering tokenization, malformed HTML recovery, DOM
mutation/selectors/serialization, entities/quirks/templates/foreign content,
formatting reconstruction, CSS parsing/cascade/variables/media queries,
`@import`/`@supports`/`calc()`, generated pseudo content, block/inline/flex/
table layout geometry, margin collapsing, percentage heights, backgrounds,
border radius/strokes, z-index, list markers, text rasterization, screenshots
(PNG/JPEG, viewport/element crops), compression, cookies, cache, proxy
tunneling, interception, image decoding and page inspection. Networking tests
use an in-process HTTP server; no external network access is required.
`cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are clean.

## Known limitations

HTML/DOM:

- The adoption-agency pass covers common misnesting; exotic nested formatting
  combinations are approximated rather than byte-for-byte spec-perfect.
- SVG/MathML reach the DOM with correct namespaces and adjusted camelCase
  names/attributes for the standard lists; foreign attribute namespaces
  (e.g. `xlink:`) are not modeled, and SVG content is not painted.
- Template content is correctly detached, but template support in the CSS
  cascade/layout pipeline is minimal (templates are `display: none`).

CSS:

- Pseudo-element support is limited to `::before`/`::after` generated text
  (strings and `attr()`); other pseudo-elements parse but do not render.
  Generated content is laid out as inline text, including inside block
  containers, but cannot itself be block-level.
- `calc()` supports linear length expressions (`+`, `-`, `*`, `/`, nested
  parentheses); `min()`/`max()`/`clamp()` and calc in media queries are not
  implemented. No gradients, transforms, transitions or animations.
- `@namespace` declarations are not parsed; the built-in `html`/`svg`/`math`/
  `*` prefixes are supported.
- Media queries support width/height (min/max), orientation, aspect-ratio and
  prefers-color-scheme: light; `print` never matches and resolution/range
  queries are not implemented.
- Unknown/dynamic pseudo-classes (`:hover`, `:focus`, ...) never match.
- No CSS Grid or floats. Flex wrapping supports rows (column wrap is treated as
  nowrap); `flex-direction: *-reverse` is laid out in forward order.
- Tables implement rows, cells, colspan, border-spacing and cell stretching;
  rowspan, border-collapse, table-layout and captions are not implemented.
- Margin collapsing covers sibling and parent/child cases for positive margins
  through empty/block containers; empty-block-through and negative-margin edge
  cases are simplified.
- Percentage heights require a definite containing height (as in CSS);
  otherwise they resolve as auto.
- z-index provides basic stacking order for positioned elements, not full
  stacking-context semantics.

Rendering:

- Tables render with basic table layout; rowspan/border-collapse are missing.
- Border radius is uniform (first value) and elliptical corners are not
  supported; backgrounds are rounded, content is not clipped unless overflow
  is hidden.
- Box shadows, gradients, SVG, Canvas and `<video>` content are not rendered.
- Background images support repeat/position/size, but gradient functions are
  ignored.
- Font family selection maps serif/sans-serif/monospace and common aliases to
  the bundled DejaVu Sans/Serif/Mono fonts; custom webfonts are not loaded.
  Italics are synthesized; no kerning or subpixel positioning.
- Form controls are approximated (text inputs show value/placeholder, selects
  show the first option).
- `position: sticky` behaves as static at the rendered scroll position.

Networking:

- Cookies and the HTTP cache are in-memory only; persistence and profile
  isolation arrive in Phase 4. There is no SameSite enforcement, public-suffix
  handling or conditional revalidation (`ETag`/`Last-Modified`).
- The cache stores only explicitly fresh (`max-age`/`Expires`) GET responses,
  ignores `Vary`, and never caches responses or requests carrying cookies,
  authorization or `Set-Cookie`.
- Proxies are HTTP CONNECT tunnels (Basic auth from the proxy URL); SOCKS,
  PAC and proxy bypass lists are not implemented.
- gzip, deflate and brotli are decoded; zstd and HTTP/3 are not implemented
  (HTTP/3 remains a later milestone).
- No authentication challenge handling; redirects drop `Authorization` on
  cross-origin hops. Request/response interception is available through the
  `RequestInterceptor` API only (no declarative rules).
- Non-text binary payloads are fetched but not decoded/rendered.

## License

AGPL-3.0-or-later (see Cargo.toml). Bundled DejaVu fonts are covered by the
license in `assets/fonts/LICENSE-DejaVu.txt`. Third-party Rust crates retain
their own licenses.
