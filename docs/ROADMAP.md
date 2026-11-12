# NEXIUM Development Roadmap and Status

Updated: 2026-11-11

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
- [x] Formatting-element reconstruction and the WHATWG adoption agency
      (outer/inner loops, bookmarks, clone and re-parent steps; the "in scope"
      check is approximated by stack membership).
- [x] DOM: arena-backed nodes, element/text/comment/doctype/fragment,
      attributes, namespaces, append/insert/remove, traversal, text content,
      serialization, queries, `compatMode`.
- [x] Query selectors: type/universal/#id/.class/all attribute operators,
      `i`/`s` flags, `html|`/`svg|`/`math|`/`*|` namespaces, all sibling
      combinators, pseudo-classes and `::before`/`::after` matching.
- [x] Document inspection: title, element/node counts, indented tree dump,
      visible-text extraction.
- [x] Page loading, offline page construction, and text/binary payload handling.
- [x] Networking: gzip/deflate/brotli decompression, in-memory cookie jar
      with SameSite/public-suffix rules, fresh-GET cache with conditional
      revalidation, HTTP CONNECT and SOCKS5/SOCKS5H proxies,
      request/response interception.
- [x] CLI: summary, `--dump-html`, `--dump-dom`, `--selector`, `--json`,
      timeout/redirect/config overrides.
- [x] Test suite (in-process HTTP server; no external network needed).

Deliverable achieved: load an HTML webpage over HTTP(S) and construct an
inspectable DOM.

### Phase 1 remaining gaps (tracked)

- The adoption agency implements the WHATWG outer/inner loops, bookmark
  handling and clone/re-parent steps; its "in scope" check is approximated by
  stack membership, so exotic misnesting across foreign or template content
  may still differ from a full browser.
- Foreign content reaches the DOM with qualified attribute namespaces
  (`xlink:`, `xml:`, `xmlns:` and custom `xmlns` bindings), namespace-aware
  lookup and round-trip serialization. Basic SVG painting exists; MathML is
  still not painted.
- Cookie and cache persistence and profile isolation were delivered in
  Phase 4. SameSite enforcement and public-suffix rejection are implemented,
  but same-site is derived from registrable domains and top-level navigation
  is inferred from GET + HTML `Accept`. Fresh-GET cache entries revalidate
  with `ETag`/`Last-Modified` and reuse the body on 304.
- Proxies support HTTP CONNECT and SOCKS5/SOCKS5H (optional username/password)
  plus `no_proxy` bypass lists (hostnames, domain suffixes, wildcards, IPv4/
  IPv6 CIDR, `host:port`, `*`, env fallback). PAC files and proxy
  auto-configuration are not implemented. HTTP/3 is deferred to a later
  milestone per the project brief.

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
      viewport units; linear `calc()` plus `min()`/`max()`/`clamp()`;
      `@namespace` declarations; function/range syntax in media queries.
- [x] Box model, block and inline formatting, text measurement (fontdue).
- [x] Sibling *and* parent/child margin collapsing, percentage heights,
      min/max sizing, overflow clipping, relative/absolute/fixed positioning.
- [x] Flexbox with grow/shrink, gap, justify-content, align-items,
      align-content and row flex-wrap.
- [x] Table layout: rows, cells, colspan, rowspan, border-spacing,
      border-collapse, captions and cell stretching.
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

- Basic CSS Grid now exists (Phase 7), as do reverse flex directions; floats,
  flex column wrapping, subgrid and full stacking-context semantics remain
  unimplemented.
- Table layout now covers rowspan, border-collapse and captions;
  `table-layout: fixed` and column-width algorithms remain unimplemented.
- Transforms, transitions, animations and box shadows are not implemented
  (targets for Phase 7).
- Pseudo-elements other than `::before`/`::after` do not render; generated
  content is inline-only.
- `@namespace` declarations, `min()`/`max()`/`clamp()` and calc/range syntax in
  media queries are implemented; web fonts, advanced SVG/Canvas/video painting
  and full form-control fidelity remain future work. Sticky positioning is
  static at the rendered scroll position.

## Phase 3 — JavaScript (complete)

- [x] Embeddable Boa runtime, host event loop, microtasks/promises and timers.
- [x] DOM bindings: Document/Element/Node, attributes, selectors, mutation,
      text/HTML content, style, classList and form values.
- [x] Events and listeners with capture/bubble phases, `click()`,
      `preventDefault`/`stopPropagation`/`stopImmediatePropagation`, `once`,
      `on*` properties, `DOMContentLoaded` and `load`.
- [x] Timers (`setTimeout`/`setInterval`), promises/async-await,
      `requestAnimationFrame`.
- [x] Fetch + Response (`text()`/`json()`), `Headers`, `Request` and
      `AbortController` (pre-aborted signals), plus a synchronous-style
      `XMLHttpRequest`; `console`, `navigator`, `location`, `screen`,
      `performance`, `history`, `window`.
- [x] `URL`/`URLSearchParams`, `TextEncoder`/`TextDecoder`, `Blob`/`File`/
      `FormData`, `localStorage`/`sessionStorage`, `atob`/`btoa`.
- [x] `MutationObserver` (childList/attributes/characterData/subtree and
      attributeFilter), `IntersectionObserver` and `ResizeObserver` with
      delivery limits; `passive` listeners and `Blob.arrayBuffer()`.
- [x] Dynamic `<script>` injection and `document.write`; ES modules with
      static import/export/re-export resolution and dynamic `import()` across
      inline, relative, absolute, `data:` and http(s) sources, cached per URL.
- [x] Script execution during page load (inline and external sources, order,
      charset, enable/disable, error/console capture).

Deliverable achieved: execute JavaScript, let scripts modify the page, and
render the updated DOM.

### Phase 3 remaining gaps (tracked)

- ES modules resolve static import/export/re-export and dynamic `import()`
  across relative/absolute/data/http(s) URLs with cached instances; bare
  specifiers (packages, import maps), CORS/MIME enforcement and module workers
  remain unimplemented.
- IntersectionObserver/ResizeObserver exist but approximate geometry from
  inline styles/attributes/window size because the JS runtime cannot see
  layout boxes; `characterData` is detected by snapshot diffing.
- Fetch is blocking behind a promise with no CORS, streaming bodies or true
  mid-flight abort; XHR is synchronous and minimal.
- NodeList-style collection methods are not implemented; `style` exposes a
  fixed property set.
- File/FormData multipart uploads and history navigation side effects are not
  implemented.
- Timers are bounded by `javascript_max_runtime_ms` during load.

## Phase 4 — Browser functionality (complete)

- [x] Tab management (open/switch/close, stable ids, active tab), per-tab
      session storage, navigation history with back/forward/reload and
      failed-load index rollback.
- [x] Cookie import/export plus persistent cookie jars in profiles.
- [x] Per-origin local storage and a filesystem-backed fresh-GET HTTP cache
      (metadata + body files), both isolated per profile.
- [x] IndexedDB subset: databases, version upgrades, object stores,
      transactions, indexes (unique), cursors, key ranges, compound keys,
      autoIncrement and get/put/delete/getAll/count with per-origin
      filesystem persistence.
- [x] Named browser profiles with preferences, history (5000 entries),
      downloads/session/cache/IndexedDB directories and isolation.
- [x] Session save/restore (tabs, histories, active index, session storage).
- [x] Atomic file writes (temp + rename + fsync) with exclusive lock files.

Deliverable achieved: operate as a persistent browsing environment with
multiple tabs, profiles and restored sessions.

### Phase 4 remaining gaps (tracked)

- IndexedDB now covers indexes (including unique constraints), cursors,
  key ranges, compound keys and autoIncrement; transaction isolation and
  cross-tab version-change blocking remain unimplemented. Stored values are
  JSON-structured rather than full structured-clones.
- Cookies implement SameSite/public-suffix rules, and fresh-GET cache entries
  revalidate with `ETag`/`Last-Modified`.
- Session storage survives only explicit session save/restore; there is no
  crash recovery, quota enforcement or storage eviction.
- No bookmarks, downloads queue or permission API is exposed yet.

## Phase 5 — Visual and interaction system (complete)

- [x] Persistent per-page JavaScript runtime preserving listeners, timers,
      observers and IndexedDB across host interactions.
- [x] Viewport management (resize/relayout), clamped scrolling and
      scroll-into-view; full, scrolled-viewport and element screenshots.
- [x] Synthetic mouse events (hover/move/down/up/click/double-click/context
      buttons/wheel) and keyboard events (keydown/keypress/keyup, text input,
      Backspace, modifiers) dispatched into live page scripts; host events
      report `isTrusted`, and hover drives `:hover` styling.
- [x] Focus tracking (`focus`/`blur`/`focusin`/`focusout`), Tab/Shift+Tab
      traversal, checkbox/radio toggling, input/textarea/contenteditable
      editing, `input`/`change` events and logical text selection.
- [x] Link activation records and follows navigation; forms submit GET/POST
      through the page-load path.
- [x] Coordinate hit-testing against layout boxes, element detection with
      geometry, visibility, viewport intersection and clickability.
- [x] Accessibility tree extraction (roles, names, values, checked/expanded/
      disabled states) and structured DOM snapshots.

Deliverable achieved: interact with rendered webpages without a visible
browser window, using viewport coordinates and live DOM events.

### Phase 5 remaining gaps (tracked)

- Host events report `isTrusted`; link clicks record/follow navigation, forms
  submit GET/POST, Tab/Shift+Tab traverse focus, and contenteditable editing
  plus logical selection ranges work. IME composition is not implemented and
  interaction state may not persist across API calls that recreate the page
  session.
- Hover drives `:hover` styling and JS events; `:focus` does not yet match.
- Hit testing approximates stacking contexts; scrollbars, scroll chaining,
  smooth scrolling and sticky/fixed repaint semantics are not modeled.
- Viewport screenshots are crops of the full-page raster; no incremental
  repaint or composited layers.
- Accessibility roles/names are heuristic (ARIA attributes and tag mapping),
  not a fully conformant accessibility tree; iframes are not traversed.

## Phase 6 — AI integration (complete)

- [x] Axum REST API covering browser create/close/status, navigation,
  click/type/press/scroll/hover, screenshot/snapshot/extract/evaluate, tabs,
  session save/restore, network and console, with structured envelopes and
  structured errors (including invalid request bodies).
- [x] WebSocket `/ws` command transport plus session, navigation, loading,
  console, network, screenshot and interaction events, filtered per agent.
- [x] Native Rust MCP server over stdio and HTTP: initialize, ping,
  tools/list and tools/call with all 19 documented browser tools, JSON
  schemas, descriptions, structured content and tool error results.
- [x] Per-agent isolated sessions (`X-Nexium-Session` + `X-Nexium-Agent`),
  profile ownership checks, session limits and bearer-token auth with a
  per-agent token registry, rotation/revocation, rate limiting and audit log;
  MCP JSON-RPC POST plus SSE transport.
- [x] Live `browser_evaluate`, network request log, page extraction and
  configurable screenshot formats.

Deliverable achieved: external AI agents can control NEXIUM through REST,
WebSocket or MCP without internal Rust access.

### Phase 6 remaining gaps (tracked)

- Bearer tokens support a per-agent registry with rotation and revocation,
  per-agent REST/WebSocket rate limiting and a bounded audit log, plus
  loopback-origin CORS and RFC 9728 protected-resource metadata; an OAuth
  authorization server/token issuance is not implemented. Native HTTPS termination is available via `api_tls_cert`/
  `api_tls_key` (HTTP/1.1), optionally alongside plain HTTP via
  `api_tls_bind`; HTTP/2 server-side needs a TLS terminator.
- MCP supports JSON-RPC POST, Streamable HTTP (`GET /mcp` server stream,
  `DELETE /mcp` session termination) and the legacy SSE transport
  (`GET /mcp/sse` + `POST /mcp/message`); stdio is the local default.
- Sessions are in memory and lost on restart (profiles and storage persist).
- Screenshot payloads are base64 JSON and can be large; no streaming or
  artifact store.
- JS evaluation is bounded by script size and Boa loop limits, not a hard
  wall-clock termination.

## Phase 7 — Modern web compatibility (complete subset)

- [x] CSS Grid: explicit/implicit tracks, `repeat()`, fr/length/auto tracks,
      spans, line placement, auto-placement, gaps, stretch and alignment.
- [x] Advanced flexbox: row/column reverse and column main-axis justify;
      row wrapping retained (column wrapping remains tracked).
- [x] SVG painting: groups, viewBox scaling, rect/circle/ellipse/line/
      polyline/polygon/path, fill/stroke, opacity, fill rules, linear/radial
      gradients, patterns, simple transforms and text.
- [x] Canvas 2D subset: rects, paths, arcs, text, gradients, `drawImage`,
      styles, transforms, save/restore, toDataURL, and screenshot integration.
- [x] Page WebSocket client and Web Workers with postMessage, plus
      matchMedia/getComputedStyle/structuredClone/requestIdleCallback.
- [x] Responsive media behavior via the CSS engine and JS matchMedia.

Deliverable achieved: substantially broader compatibility for modern layouts,
graphics and scripted pages, with unsupported areas explicitly tracked.

### Phase 7 remaining gaps (tracked)

- CSS transforms, transitions, animations, floats, subgrid, named grid
  lines/areas, auto-fit and flex column wrapping.
- Canvas text, gradients and `drawImage` (canvases and `data:` images) exist;
  kerning, network-image pixels and path effects remain. SVG gradients,
  patterns, fill rules and opacity exist; filters, gradient transforms and
  full transform syntax remain. HTTP/3 is still deferred.
- Full getComputedStyle cascade values; broader Web APIs and complex web
  application compatibility are incremental.

## Phase 8 — Optimization and stability (complete hardening pass)

- [x] Resource limits: `max_tabs`, API session cap, response/cache/network-log
      bounds, JS loop limits, timer and observer delivery limits.
- [x] Resource cleanup: JS/worker/WebSocket threads exit on page/session drop;
      API close and idle sweeps release browsers; profile browsers save their
      session on drop.
- [x] Crash recovery: profile session snapshots are written on drop and can be
      restored on the next browser instance.
- [x] Security: optional SSRF blocking that resolves hostnames and rejects
      private IPv4/IPv6 addresses on every redirect hop, bearer-token API auth
      with a per-agent token registry (rotation/revocation), rate limiting,
      audit logging, agent/session ownership checks, profile isolation and
      sanitized snapshot names.
- [x] Stress/stability tests for tab limits, idle sweeping, repeated
      navigation/rendering, blocked private requests and recovery.
- [x] Documentation updated with limits, security controls and compatibility
      status.

Deliverable achieved: a more stable, bounded and hardened engine with explicit
limits, cleanup and recovery behavior.

### Phase 8 remaining gaps (tracked)

- No WPT harness or browser-wide performance benchmark suite; stress tests are
  targeted integration tests rather than long-running soak tests.
- SSRF protection resolves hostnames and rejects any private resolved address,
  re-checking redirect hops; a residual DNS-rebinding window remains and
  proxy-side resolution cannot be pinned.
- Rate limiting and token rotation/revocation are implemented; sandboxed
  process isolation is not.
- Session snapshots recover tabs/history/storage, not live JS timers or
  in-flight network requests.

## Test status

- 306 automated tests across HTML, DOM, selectors, entities/quirks/templates,
  CSS (`calc`, `min`/`max`/`clamp`, `@namespace`, `@import`, `@supports`,
  media queries incl. range syntax), generated content, block/inline/flex/
  table layout (colspan, rowspan, border-collapse, captions), margin
  collapsing, percentage heights, backgrounds/radius/borders/z-index/list
  markers, JavaScript DOM/web APIs/events/timers/promises/fetch/ES modules
  with imports/observers, rendering, networking (compression, cookies, cache,
  proxy bypass, interception), configuration, decoding and page inspection —
  plus profiles, persistence (cookies, local storage, IndexedDB with indexes/
  cursors/key ranges/autoIncrement, disk cache), tabs/navigation, session
  restore, hit testing, live click/typing dispatch, hover `:hover` styling,
  trusted events, Tab focus traversal, link/form navigation, contenteditable,
  scrolling, accessibility and DOM snapshots, REST auth/session isolation,
  token rotation, rate limiting, audit log, MCP tool discovery/execution and
  SSE transport, WebSocket commands/events, CSS Grid geometry, SVG/canvas
  painting (gradients/patterns/text), page WebSockets, Web Workers, modern
  browser APIs, and stability/security checks for tab limits, idle cleanup,
  resolved-address SSRF blocking and crash recovery — all passing (1 ignored).
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are clean.
- Manual verification against `https://example.com` (fetch, DOM inspection and
  screenshot) and `https://news.ycombinator.com` (HTTPS, HTTP/2, real-world
  markup; table rows/cells now lay out side by side).
- No Web Platform Tests are run yet; compatibility claims are intentionally
  conservative.
