# NEXIUM Development Roadmap and Status

Updated: 2026-11-09

Legend: `[x]` implemented and tested · `[~]` implemented as a documented subset

## Phase 1 — Foundation (complete)

- [x] Rust project setup (library + CLI, async Tokio runtime, tracing).
- [x] URL parsing and normalization (`url` crate, bare hosts → HTTPS).
- [x] HTTP/1.1 and HTTP/2 client over Rustls with connection pooling.
- [x] Redirect handling (301/302/303/307/308, method rewriting, credential
      stripping on cross-origin hops).
- [x] Request timeouts, cancellation via future drop, response size limits.
- [x] Character-set decoding (BOM → HTTP charset → meta → UTF-8).
- [x] HTML tokenizer: tags, attributes, comments, doctypes, RCDATA/RAWTEXT/
      script/plaintext, and the full WHATWG named character reference table.
- [x] HTML tree builder: implied html/head/body, void elements, common
      auto-closing rules, malformed-input recovery, quirks-mode detection,
      templates with detached content fragments, and SVG/MathML foreign content.
- [x] Formatting-element reconstruction and adoption-agency approximation
      (block splitting and content re-wrapping for common misnesting).
- [x] DOM: arena-backed nodes, element/text/comment/doctype/fragment,
      attributes, namespaces, append/insert/remove, traversal, text content,
      serialization, queries, `compatMode`.
- [x] Query selectors: type/universal/#id/.class/all attribute operators,
      `i`/`s` flags, `html|`/`svg|`/`math|`/`*|` namespaces, all sibling
      combinators, pseudo-classes and `::before`/`::after` matching.
- [x] Document inspection: title, element/node counts, indented tree dump,
      visible-text extraction.
- [x] Page loading, offline page construction, and text/binary payload handling.
- [x] Networking: gzip/deflate/brotli decompression, in-memory cookie jar,
      fresh-GET cache, HTTP CONNECT proxy, request/response interception.
- [x] CLI: summary, `--dump-html`, `--dump-dom`, `--selector`, `--json`,
      timeout/redirect/config overrides.
- [x] Test suite (in-process HTTP server; no external network needed).

Deliverable achieved: load an HTML webpage over HTTP(S) and construct an
inspectable DOM.

### Phase 1 remaining gaps (tracked)

- The adoption-agency pass is pragmatic: exotic nested-formatting cases are
  approximated rather than fully algorithm-complete.
- Foreign content reaches the DOM correctly, but SVG/MathML painting is Phase 7;
  foreign attribute namespaces (e.g. `xlink:`) are not modeled.
- Cookie and cache persistence, profile isolation and conditional revalidation
  are Phase 4; SameSite/public-suffix rules are not enforced yet.
- Proxy support is HTTP CONNECT only (no SOCKS/PAC); HTTP/3 is deferred to a
  later milestone per the project brief.

## Phase 2 — CSS and Layout (complete)

- [x] CSS tokenizer and parser (declarations, rules, `@media`, `@supports`,
      `@import`, `!important`).
- [x] Selector engine extensions: pseudo-classes (`:first-child`,
      `:last-child`, `:only-child`, `:nth-child()`, `:nth-last-child()`,
      `:not()`, `:root`, `:empty`, `:link`, `:checked`), `~`, attribute
      operators, namespaces, case-insensitive flags, and `::before`/`::after`
      generated text (strings and `attr()`).
- [x] Cascade, specificity, origins, inheritance, computed styles.
- [x] CSS variables with fallbacks; all common color formats; px/em/rem/%/pt/
      viewport units; linear `calc()` expressions.
- [x] Box model, block and inline formatting, text measurement (fontdue).
- [x] Sibling *and* parent/child margin collapsing, percentage heights,
      min/max sizing, overflow clipping, relative/absolute/fixed positioning.
- [x] Flexbox with grow/shrink, gap, justify-content, align-items,
      align-content and row flex-wrap.
- [x] Basic table layout: rows, cells, colspan, border-spacing, cell
      stretching.
- [x] Paint pipeline and internal rasterizer (no window manager).
- [x] Background repeat/position/size, border radius, solid/dashed/dotted/
      double borders, list markers and basic z-index ordering.
- [x] Bundled DejaVu Sans/Serif/Mono family selection; synthetic italics.
- [x] PNG/JPEG output for full-page, viewport and element screenshots;
      resizing and comparison helpers.
- [x] Responsive layout via width/height, orientation, aspect-ratio and
      color-scheme media queries.

Deliverable achieved: render basic HTML and CSS webpages into images. A
self-contained demo renders flexbox cards, borders, form controls and text to
`/tmp/nexium-demo.png` (`cargo run --example render_demo`).

### Phase 2 remaining gaps (tracked for later phases)

- CSS Grid, floats, flex column wrapping, row-reverse/column-reverse and full
  stacking-context semantics are not implemented.
- Table layout lacks rowspan, border-collapse and captions.
- Transforms, transitions, animations, gradients and box shadows are not
  implemented (targets for Phase 7).
- Pseudo-elements other than `::before`/`::after` do not render; generated
  content is inline-only.
- `@namespace` declarations, `min()`/`max()`/`clamp()`, calc in media queries,
  web fonts, SVG/Canvas/video painting and full form-control fidelity remain
  future work. Sticky positioning is static at the rendered scroll position.

## Phase 3 — JavaScript (next)

- [ ] Embeddable JS runtime (Boa) and event loop/microtasks.
- [ ] DOM bindings: Document/Element/Node, attributes, mutation.
- [ ] Events, listeners, timers, Promises, async/await.
- [ ] Fetch/XHR, URL, TextEncoder/TextDecoder, observers.
- [ ] Script execution during page load.

## Phase 4 — Browser functionality

- [ ] Navigation history, tabs, contexts.
- [~] Cookies and GET cache exist in memory; persistent cookies/storage,
      IndexedDB and filesystem-backed cache remain.
- [ ] Browser profiles and session save/restore.
- [ ] File-based persistence with atomic writes and locking.

## Phase 5 — Visual and interaction system

- [ ] Viewport management, screenshot comparison.
- [ ] Mouse/keyboard/scroll events and DOM dispatch.
- [ ] Coordinate hit-testing, element visibility/clickability.
- [ ] Accessibility tree, DOM snapshots, element detection.

## Phase 6 — AI integration

- [ ] Axum REST API (browser/navigate/click/type/screenshot/etc.).
- [ ] WebSocket event stream.
- [ ] Native Rust MCP server with the documented tool set.
- [ ] Per-agent isolated sessions and structured responses.

## Phase 7 — Modern web compatibility

- [ ] CSS Grid, advanced Flexbox, transforms/animations.
- [ ] SVG painting, Canvas, WebSockets, Web Workers.
- [ ] HTTP/3 (QUIC) support.
- [ ] Broader Web APIs and complex web applications.

## Phase 8 — Optimization and stability

- [ ] Memory limits, resource cleanup, crash recovery.
- [ ] Security hardening (SSRF, file access, permissions, API auth).
- [ ] WPT subset tracking, stress tests, documentation.

## Test status

- 161 automated tests across HTML, DOM, selectors, entities/quirks/templates,
  CSS (`calc`, `@import`, `@supports`, media queries), generated content,
  block/inline/flex/table layout, margin collapsing, percentage heights,
  backgrounds/radius/borders/z-index/list markers, rendering, networking
  (compression, cookies, cache, proxy, interception), configuration, decoding
  and page inspection — all passing.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are clean.
- Manual verification against `https://example.com` (fetch, DOM inspection and
  screenshot) and `https://news.ycombinator.com` (HTTPS, HTTP/2, real-world
  markup; table rows/cells now lay out side by side).
- No Web Platform Tests are run yet; compatibility claims are intentionally
  conservative.
