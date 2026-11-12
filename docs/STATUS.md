# NexiumX — Implementation Status

Updated: 2026-11-11

This document collects the implementation status, test coverage, known
limitations and licensing details. Product overview and usage live in
[README.md](../README.md); the phase-by-phase delivery checklist lives in
[ROADMAP.md](ROADMAP.md).

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
- Formatting-element reconstruction and the WHATWG adoption agency for
  misnested inline markup, including the outer/inner loops, bookmarks and
  clone/re-parent steps.
- Arena-backed DOM with mutation APIs, text content, HTML serialization,
  `QuirksMode`, namespaces, template content, and a CSS selector engine: type,
  `#id`, `.class`, attribute operators with `i`/`s` flags, `html|`/`svg|`/
  `math|`/`*|` namespaces, all sibling combinators, `:first-child`,
  `:nth-child()`, `:not()`, `:root`, `:empty`, `:hover`, plus `::before`/
  `::after` generated-content matching.
- Networking: gzip/deflate/brotli decompression, a domain/path/secure/
  expiry-aware cookie jar with SameSite and public-suffix rules, a fresh-GET
  HTTP cache with `ETag`/`Last-Modified` revalidation, HTTP CONNECT and
  SOCKS5/SOCKS5H proxy support with `no_proxy` bypass lists, and
  request/response interception hooks.
- Non-HTML responses are handled correctly: text/JSON/XML documents become
  preformatted text instead of being parsed as HTML; binary payloads are not
  parsed.

**Phase 2 — CSS, layout and rendering** is implemented and tested:

- CSS tokenizer and parser: rules, declarations, `!important`, nested
  `@media` (width/height, orientation, aspect-ratio, color-scheme; `calc()`,
  `min()`, `max()`, `clamp()` and range syntax), `@supports`, `@import`
  (fetched and expanded in order), `@namespace`, comments, functions, URLs,
  custom properties, and `calc()` linear length expressions.
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
  table layout (rows, cells, colspan, rowspan, cell stretching, border-spacing,
  collapsed borders, captions), relative/absolute/fixed positioning, percentage
  heights, min/max sizing, display:none, overflow clipping.
- Paint pipeline: backgrounds (repeat/position/size), border-radius, solid/
  dashed/dotted/double borders, text glyph rasterization (bundled DejaVu
  Sans/Serif/Mono with synthetic italics), underlines, list markers, replaced
  images, basic z-index ordering, alpha compositing and opacity.
- Screenshots: full page, viewport and element crops; PNG and JPEG output;
  resizing and pixel comparison; `--screenshot` CLI support.
- Author stylesheets: `<style>` blocks, `<link rel="stylesheet">` resources and
  `@import` chains are fetched and applied in document order.

**Phase 3 — JavaScript** is implemented and tested:

- Boa embeddable runtime with a host event loop (microtasks/promises, timers,
  `requestAnimationFrame`) and script loading during page load (inline and
  external `src`, document order, charset decoding, `javascript_enabled`).
- DOM bindings: `document.getElementById/querySelector/querySelectorAll`,
  `createElement`/`createTextNode`/`createComment`, attribute APIs,
  `textContent`, `innerHTML`, `outerHTML`, append/insert/remove, `children`,
  `childNodes`, parent/sibling traversal, `className`, `id`, form `value`,
  `checked`/`disabled`, `style` (camelCase accessors, `cssText`, `setProperty`)
  and `classList`.
- Events: `addEventListener`/`removeEventListener`, bubbling dispatch,
  `preventDefault`/`stopPropagation`, `click()` (checkbox toggling),
  `DOMContentLoaded` and `load`.
- Browser APIs: `console`, `window`/`document`, `navigator`, `location`,
  `screen`, `performance`, `history`, `alert`/`confirm`/`prompt`, `fetch`
  (with `Headers`/`Request`/`AbortController`) + `Response`
  (`text()`/`json()`), a synchronous-style `XMLHttpRequest`,
  `URL`/`URLSearchParams`, `TextEncoder`/`TextDecoder`, `Blob`/`File`/
  `FormData` (with `Blob.arrayBuffer()`), `localStorage`/`sessionStorage`,
  `atob`/`btoa`, `MutationObserver` (childList, attributes, characterData,
  attributeFilter), `IntersectionObserver` and `ResizeObserver`.
- Scripts can inject `<script>` elements and use `document.write`; ES modules
  support static imports/re-exports and dynamic `import()` across relative,
  absolute and external URLs, with one cached instance per module. Event
  listeners support capture and bubble phases,
  `stopPropagation`/`stopImmediatePropagation`, `once`, `passive`, `on*`
  handler properties and inline `on*` attributes; trusted host events report
  `isTrusted`.
- DOM mutations and inline style changes made by scripts are used by layout
  and rendering (screenshots reflect scripted pages).
- Console output and script errors are captured on the page
  (`page.console`, `page.script_errors`).

**Phase 4 — Browser functionality** is implemented and tested:

- Tabs with stable ids, switching, closing, active-tab tracking, and a
  per-tab session-storage map.
- History navigation (`navigate`, `back`, `forward`, `reload`) with index
  rollback when a load fails.
- Named persistent profiles under `data_dir/profiles/<name>` with
  preferences, per-origin local storage, cookie import/export, history
  (capped at 5000 entries), download/session/cache/IndexedDB directories.
- Persistent cookies restored into the HTTP client, a fresh-GET cache with
  metadata/body files and conditional revalidation, and IndexedDB databases
  persisted per origin with object stores, autoIncrement keys, indexes,
  cursors, key ranges and compound keys.
- Session save/restore: tabs, per-tab histories, active index and session
  storage are serialized to JSON and reloaded.
- Atomic filesystem writes (temp file + rename + fsync) behind exclusive
  `fs2` lock files.

**Phase 5 — Visual and interaction system** is implemented and tested:

- A persistent per-page JavaScript runtime keeps listeners, timers,
  observers and IndexedDB alive after load so host input reaches page code.
- `PageSession` viewport management: resizing with relayout, clamped
  `scroll_to`/`scroll_by`/`scroll_into_view`, and max-scroll reporting.
- Hit testing against layout border boxes in viewport coordinates; element
  detection with tag, id, classes, text, role/name, href, geometry,
  visibility, viewport intersection and clickability.
- Mouse interactions: hover/move, down/up, left/middle/right click,
  double-click and wheel (with preventDefault-aware scrolling). `Browser`
  convenience methods apply the same interactions and write the updated page,
  scroll state and storage back into the active tab.
- Keyboard interactions: focused input/textarea/contenteditable targets,
  keydown/keypress/keyup, text insertion, Backspace and modifier flags;
  Tab/Shift+Tab focus traversal, focus/blur and Input events fire as DOM
  events. Clicking a link records the navigation target and form submit
  buttons/Enter submit GET or POST through the page-load path.
- Hover tracks the hit element for `:hover` styling and dispatches trusted
  synthetic events; text selection over focused editable content is exposed.
- Screenshots: full page, scrolled viewport crops and element crops.
- Accessibility tree extraction with inferred roles, accessible names,
  values and checked/expanded/disabled states.
- Structured DOM snapshots (serialized HTML, visible text, counts,
  viewport and scroll state) suitable for JSON responses.

**Phase 6 — AI integration** is implemented and tested:

- Axum REST control plane implementing every documented `/browser/*`
  endpoint (create/close/status, navigation, click/type/press/scroll/hover,
  screenshot/snapshot/extract/evaluate, tabs, session save/restore, network,
  console) with `{ "ok": true, "data": ... }` and structured errors.
- WebSocket `/ws` transport for commands plus real-time session, navigation,
  loading, console, network, screenshot and interaction events, filtered to
  the authenticated agent's own sessions.
- Native MCP JSON-RPC server (`nexium --mcp` over stdio, Streamable HTTP at
  `POST /mcp` + `GET /mcp` SSE stream + `DELETE /mcp`, or the legacy SSE
  transport at `GET /mcp/sse` + `POST /mcp/message`) exposing all 19
  documented `browser_*` tools with descriptions, JSON schemas, structured
  content and tool-level error reporting.
- Multiple isolated agent sessions selected with `X-Nexium-Session`; ownership
  is enforced against `X-Nexium-Agent`, and persistent profile names cannot be
  shared across agents.
- Bearer-token authentication (`api_token` / `--api-token`) plus a token
  registry with per-agent tokens, rotation and revocation
  (`/v1/admin/tokens*`); unauthenticated servers refuse to bind beyond loopback.
- Per-agent rate limiting for REST and WebSocket commands with structured
  `429` errors, and a bounded audit log (`GET /v1/audit`).
- Loopback-origin CORS (including private-network preflight) and RFC 9728
  OAuth protected-resource metadata for browser-based MCP clients.
- `nexium --api` starts the HTTP control plane (default `127.0.0.1:9222`).

**Phase 7 — Modern web compatibility** is implemented as a tested subset:

- CSS Grid: explicit/implicit rows and columns, `repeat()`, `fr`/length/auto
  tracks, row/column spans, line placement, auto-placement, gaps, stretch and
  item alignment.
- Advanced flexbox: row/column reverse and column main-axis `justify-content`.
- SVG painting: `svg`/`g`, viewBox scaling, rect, circle, ellipse, line,
  polyline, polygon, path (`M/L/H/V/C/S/Q/T/A/Z` approximations), fill/stroke,
  opacity, fill rules, linear/radial gradients, patterns, translate/scale
  transforms and text.
- Canvas 2D subset: rects, paths, arcs, fills/strokes, text
  (`fillText`/`strokeText`/`measureText`), linear/radial gradients, `drawImage`
  from canvases and `data:` images, transforms, styles, `globalAlpha`,
  `save`/`restore`, `toDataURL`, and screenshots that include canvas pixels.
- Page WebSocket client (`onopen`/`onmessage`/`onerror`/`onclose`, send/close)
  with echo tested against a real WebSocket server.
- Web Workers: background Boa runtime, external worker scripts, structured
  `postMessage` and `onmessage`.
- Modern APIs: `matchMedia`, `getComputedStyle` (inline-style subset),
  `structuredClone`, `requestIdleCallback`, plus responsive media behavior.

**Phase 8 — Optimization and stability** is implemented as a hardening pass:

- Configurable per-browser tab limit (`max_tabs`) and API session limit, plus
  idle-session sweeping (`api_session_idle_timeout_ms`).
- Bounded resources throughout: response-size caps, cache entry caps, a
  500-entry network log, IndexedDB task queues, JS loop-iteration limits and
  timer/observer delivery limits.
- Graceful cleanup: dropping a profile browser saves its session; JS runtime,
  worker and WebSocket threads exit when their page/session is dropped.
- Crash recovery through the profile session snapshot written on drop and
  restorable with `restore_profile_session`/`browser_session_restore`.
- Optional SSRF hardening (`block_private_networks`) that resolves hostnames
  and rejects any private IPv4/IPv6 target, re-checked on every redirect hop,
  alongside bearer auth, token rotation/revocation, rate limiting, agent/
  session ownership, profile isolation and sanitized session snapshot names.
- Proxy bypass lists (`no_proxy`) for HTTP CONNECT and SOCKS5/5H matching
  hostnames, domains, CIDR ranges and ports.
- Stability/stress tests for tab limits, idle sweeps, repeated
  navigate/render cycles, blocked private requests and profile recovery.

Not implemented yet: CSS transforms/transitions/animations, floats, CSS Grid
subgrid/named lines/auto-fit, flex column wrapping, SVG filters/advanced
gradient transforms, box shadows, and HTTP/3. These are tracked in
[ROADMAP.md](ROADMAP.md).

NexiumX is a work in progress and does **not** claim web-platform compatibility.
See "Known limitations" below.

## Testing

`cargo test` runs 308 tests covering tokenization, malformed HTML recovery, DOM
mutation/selectors/serialization, entities/quirks/templates/foreign content,
formatting reconstruction, CSS parsing/cascade/variables/media queries,
`@import`/`@supports`/`calc()`, generated pseudo content, block/inline/flex/
table layout geometry, margin collapsing, percentage heights, backgrounds,
border radius/strokes, z-index, list markers, JavaScript DOM/web APIs/events/
timers/promises/fetch/modules/observers/external scripts, text rasterization,
screenshots (PNG/JPEG, viewport/element crops), compression, cookies, cache,
proxy tunneling, interception, image decoding and page inspection, plus
browser profiles, cookie/local-storage/IndexedDB persistence, tabs and
navigation, session save/restore and the disk HTTP cache, plus interaction:
hit testing, click/hover/typing dispatch into live scripts, scrolling,
accessibility extraction and DOM snapshots, plus REST auth/session isolation,
MCP tool discovery/execution, WebSocket commands/events, network logging,
CSS Grid geometry, SVG/canvas painting, page WebSockets, Web Workers and
modern browser APIs. Networking and script tests use in-process servers; no
external network access is required. `cargo clippy --all-targets -- -D
warnings` and `cargo fmt --check` are clean.

## Known limitations

HTML/DOM:

- The adoption agency implements the WHATWG outer/inner loops, bookmarks and
  clone/re-parent steps, but its "in scope" check is approximated by stack
  membership, so exotic misnesting can still differ from a browser.
- Interaction state (focus, hover, selection, pending navigation) is held by
  the page session; API calls that recreate the session may not carry Tab
  traversal or pending form/link navigation across separate requests.
- SVG/MathML reach the DOM with correct namespaces and adjusted camelCase
  names/attributes for the standard lists. Qualified attribute namespaces
  (`xlink:`, `xml:`, `xmlns:` and custom `xmlns` bindings) are modelled with
  namespace-aware lookup and round-trip serialization. SVG painting covers
  basic shapes, paths, text, gradients and simple patterns, not filters or
  full gradient/transform syntax; MathML is not painted.
- Template content is correctly detached, but template support in the CSS
  cascade/layout pipeline is minimal (templates are `display: none`).

JavaScript:

- ES modules resolve static imports/re-exports and dynamic `import()` for
  relative, absolute, `data:` and http(s) URLs; bare specifiers (packages,
  import maps) and CORS/MIME enforcement are not implemented.
- `IntersectionObserver`/`ResizeObserver` are approximated from inline styles,
  attributes and window size because the JS runtime cannot see layout boxes.
- Web Workers support external scripts, structured `postMessage`/`onmessage`
  and termination, but no `importScripts`, module workers or nested workers.
- Page `WebSocket` supports text send/receive and close; binary/blob mode,
  subprotocols and backpressure are not implemented. Socket messages are
  delivered while timers or event-loop work are active.
- `fetch` performs a blocking request behind a promise; CORS, streaming
  bodies and true mid-flight abort are not implemented (an already-aborted
  `AbortSignal` rejects before the request). `XMLHttpRequest` is synchronous.
- Event handling supports capture and bubble phases, `once`, `passive`, `on*`
  properties and inline `on*` attributes; there is no event delegation
  emulation beyond normal bubbling, and `document.readyState` is always
  `complete`. `Element.matches`/`closest` are not exposed.
- Timers run during load up to `javascript_max_runtime_ms` (default 2 s);
  very long timeouts are skipped. Animation frames are approximated with a
  16 ms timer.
- DOM collections are plain arrays (no `NodeList`/`HTMLCollection` methods),
  and `style` exposes a fixed set of CSS properties plus `cssText`/
  `setProperty`.
- `MutationObserver` supports `childList`, `attributes`, `characterData`,
  `subtree` and `attributeFilter`; `characterData` is detected by text
  snapshots at delivery time. Callbacks that mutate their own observed subtree
  hit a delivery limit instead of looping forever.
- Script execution is synchronous per document; iframes are not browsed.

CSS:

- Pseudo-element support is limited to `::before`/`::after` generated text
  (strings and `attr()`); other pseudo-elements parse but do not render.
  Generated content is laid out as inline text, including inside block
  containers, but cannot itself be block-level.
- `calc()` supports linear length expressions (`+`, `-`, `*`, `/`, nested
  parentheses); `min()`/`max()`/`clamp()` are supported in properties and
  media queries. No transforms, transitions or animations.
  `getComputedStyle` exposes the inline-style subset rather than the full
  cascaded computed style.
- `@namespace` declarations are parsed and participate in selector matching,
  in addition to the built-in `html`/`svg`/`math`/`*` prefixes.
- Media queries support width/height (min/max and range syntax with
  `calc`/`min`/`max`/`clamp`), orientation, aspect-ratio and
  prefers-color-scheme: light; `print` never matches and resolution queries
  are not implemented.
- `:hover` matches the current pointer target; `:focus` and other dynamic
  pseudo-classes never match.
- CSS Grid covers explicit/implicit tracks, `repeat()`, `fr`, spans, line
  placement, auto-placement and gaps; subgrid, named lines/areas, `auto-fit`
  and dense column flow are not implemented. No floats. Flex wrapping supports
  rows (column wrap is treated as nowrap); row/column reverse and column
  `justify-content` are supported.
- Tables implement rows, cells, colspan, rowspan, border-spacing, collapsed
  borders, captions and cell stretching; `table-layout: fixed` is not
  implemented.
- Margin collapsing covers sibling and parent/child cases for positive margins
  through empty/block containers; empty-block-through and negative-margin edge
  cases are simplified.
- Percentage heights require a definite containing height (as in CSS);
  otherwise they resolve as auto.
- z-index provides basic stacking order for positioned elements, not full
  stacking-context semantics.

Rendering:

- Border radius is uniform (first value) and elliptical corners are not
  supported; backgrounds are rounded, content is not clipped unless overflow
  is hidden.
- Box shadows, CSS gradients and `<video>` content are not rendered. SVG and
  Canvas are rendered through subsets: SVG lacks filters and advanced gradient
  transforms; Canvas text/images/gradients work but lack kerning and path
  effects.
- Background images support repeat/position/size, but gradient functions are
  ignored.
- Font family selection maps serif/sans-serif/monospace and common aliases to
  the bundled DejaVu Sans/Serif/Mono fonts; custom webfonts are not loaded.
  Italics are synthesized; no kerning or subpixel positioning.
- Form controls are approximated (text inputs show value/placeholder, selects
  show the first option).
- `position: sticky` behaves as static at the rendered scroll position.

Browsing and storage:

- Profiles isolate cookies, local storage, history, cache and IndexedDB.
  Session storage is per tab and only survives through explicit session
  save/restore; there is no automatic crash recovery.
- IndexedDB implements `open`/upgrade, object stores, transactions,
  get/put/delete/getAll/count, `autoIncrement`, indexes (including unique
  constraints), cursors, key ranges and compound keys, with JSON-structured
  values. Transaction isolation and cross-tab version-change blocking are not
  implemented.
- Local storage is persisted per profile and origin; cookies are saved after
  each page load. No quota enforcement or storage eviction is performed.
- History keeps the most recent 5000 entries; downloads/bookmarks/permission
  APIs are not exposed yet.

Control plane:

- Authentication is bearer-token based with a per-agent token registry,
  rotation/revocation, rate limiting and an in-memory audit log; there is no
  OAuth. Agent identity is only as trustworthy as its token.
- The server supports native HTTPS (Rustls, HTTP/1.1) when
  `api_tls_cert`/`api_tls_key` or `--tls-cert`/`--tls-key` are set, and can
  serve plain HTTP and HTTPS at the same time by adding `api_tls_bind`/
  `--tls-bind`. HTTP/2 on the server side still requires a TLS terminator.
  Unauthenticated binds outside loopback are refused.
- Sessions live in memory and are swept when idle (on the next create);
  profiles, cookies, storage and session snapshots persist through the
  profile/storage layer, so a restart drops live tabs but can restore saved
  snapshots. SSRF blocking is opt-in, resolves hostnames, rejects any private
  resolved address and re-checks each redirect hop; a residual rebinding window
  remains and proxy-side resolution cannot be pinned.
- JS `evaluate` is limited by script length and Boa loop-iteration limits, not
  a hard wall-clock kill switch.
- Screenshots are returned as base64 inside JSON. MCP supports JSON-RPC POST,
  Streamable HTTP (`GET /mcp` SSE stream, `DELETE /mcp`) and the legacy
  `/mcp/sse` transport; stdio remains the recommended local transport.

Interaction:

- Hit testing walks the layout tree in paint order; z-index/stacking contexts
  and overlapping fixed/absolute content are approximate.
- Host-dispatched events report `isTrusted`; script-created events do not.
  Link clicks record and can follow the resolved href, forms submit GET/POST,
  and Tab/Shift+Tab traverse focus. Pointer hover persists across separate
  API calls; focus, selection and pending navigation may not.
- Hover drives `:hover` styling and JS events; `:focus` does not yet match.
  Keyboard text entry supports input/textarea and contenteditable editing with
  logical selection ranges, but not IME composition.
- Scrolling changes viewport crops and wheel offsets; scrollbars, scroll
  chaining, smooth scrolling and sticky/fixed repaint semantics are not
  modeled.
- Accessibility roles/names are inferred heuristically from tags and ARIA
  attributes; there is no full accessibility-tree conformance, live regions
  or iframe traversal.

Networking:

- Cookies and the HTTP cache are persisted for profiles; non-profile clients
  keep them in memory. SameSite rules are enforced (with same-site derived
  from registrable domains and top-level navigation inferred from GET + HTML
  `Accept`); cookies whose domain is a public suffix are rejected. Stale
  fresh-GET entries revalidate with `ETag`/`Last-Modified` and reuse the cached
  body on 304.
- The cache stores only explicitly fresh (`max-age`/`Expires`) GET responses,
  ignores `Vary`, and never caches responses or requests carrying cookies,
  authorization or `Set-Cookie`.
- Proxies support HTTP CONNECT and SOCKS5/SOCKS5H tunnels (optional username/
  password) and `no_proxy` bypass lists; PAC files and HTTP proxy `ForwardProxy`
  mode are not implemented.
- gzip, deflate and brotli are decoded; zstd and HTTP/3 are not implemented
  (HTTP/3 remains a later milestone).
- No authentication challenge handling; redirects drop `Authorization` on
  cross-origin hops. Request/response interception is available through the
  `RequestInterceptor` API only (no declarative rules).
- Non-text binary payloads are fetched but not decoded/rendered.

## License

NexiumX is released under the MIT License (see [LICENSE](../LICENSE)). Bundled
DejaVu fonts are covered by the license in
`assets/fonts/LICENSE-DejaVu.txt`. Third-party Rust crates retain their own
licenses.
