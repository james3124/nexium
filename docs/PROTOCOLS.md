# Automation Protocols and Compatibility

NexiumX exposes four in-process automation surfaces. Everything is implemented
in this repository (no external driver binary or wrapped browser).

| Surface | Endpoint | Primary clients |
|---|---|---|
| W3C WebDriver | `POST /session`, `/status` | Selenium, WebDriver clients |
| CDP | `/json/*`, `/devtools/browser`, `/devtools/page/{id}` | Puppeteer, DevTools, Selenium `executeCdpCommand` |
| REST | `/browser/*`, `/browser/dom` | custom frameworks, AI agents |
| MCP | stdio, `POST /mcp`, `/mcp/sse` | AI agents (OpenCode, Claude-style clients) |
| WebDriver BiDi | `WS /session` | event-driven automation clients |

## W3C WebDriver (Selenium)

Implemented commands:

- Session: `GET /status`, `POST /session`, `DELETE /session/{id}`
- Navigation: `GET|POST /session/{id}/url`, `title`, `source`,
  `back`/`forward`/`refresh`
- Elements (CSS selectors): `POST element` / `elements`,
  `element/{eid}/text`, `name`, `attribute/{name}`, `property/{name}`,
  `rect`, `size`, `displayed`, `enabled`, `selected`, `click`,
  `value` (send keys), `clear`, `submit`
- Script: `POST /session/{id}/execute/sync` (function body)
- Screenshot: `GET /session/{id}/screenshot` (PNG base64)
- Window: `GET|POST /session/{id}/window/rect`, `window`,
  `window/new`, `window/handles` (handles are tab ids)
- Cookies: `GET|POST|DELETE /session/{id}/cookie`
- Timeouts: `GET|POST /session/{id}/timeouts` (accepted; implicit waits are 0)

Not implemented: XPath / link-text / partial-link-text locators, W3C actions
(`/actions`, pointer/touch input), iframes, alerts, file upload bodies,
`element/{eid}/computedrole`, and shadow DOM APIs.

Typical Selenium flow works: create session → navigate → find by CSS →
click/send keys → read text/attributes → execute script → screenshot.

## Chrome DevTools Protocol

Discovery and transports:

- `GET /json/version` → `Browser`, `Protocol-Version`, WS URLs
- `GET /json` and `/json/list`, `GET|PUT /json/new?url=`,
  `GET /json/close/{id}`, `GET /json/activate/{id}`,
  `GET /json/protocol` (implemented command schema)
- `WS /devtools/page/{id}` (one target)
- `WS /devtools/browser` (Target domain; commands may carry `sessionId`
  from `Target.attachToTarget`, flattened-session style)

Implemented domains/commands:

- **Browser**: `getVersion`, `getBrowserCommandLine`, `close`
- **Target**: `getTargets`, `createTarget`, `closeTarget`,
  `attachToTarget`, `setDiscoverTargets`, `setAutoAttach`,
  `detachFromTarget`
- **Page**: `enable`, `navigate`, `reload`, `captureScreenshot`
  (format, quality, `captureBeyondViewport`, document-space `clip` with
  scale), `getLayoutMetrics`, `getFrameTree`, `getNavigationHistory`,
  `bringToFront`, `setLifecycleEventsEnabled`, `close`
- **Runtime**: `enable`, `evaluate`, `callFunctionOn` (including
  `objectId` values returned by `DOM.resolveNode`), `getProperties`,
  `releaseObject`
- **DOM**: `enable`, `getDocument` (full tree with nodeIds, attributes,
  template content), `describeNode`, `querySelector`, `querySelectorAll`,
  `getOuterHTML`, `getAttributes`, `getBoxModel`, `resolveNode`, `focus`
- **Input**: `dispatchMouseEvent` (move/release → hover/click),
  `dispatchKeyEvent` (key/text), `insertText`
- **Emulation**: `setDeviceMetricsOverride` (viewport resize)
- **Schema**: `getDomains`
- Events: `Page.domContentEventFired`, `Page.loadEventFired`,
  `Network.requestWillBeSent` / `responseReceived` / `loadingFinished`
  (emitted after navigation from the request log)

Additional implemented commands: `Page.getCookies/setCookie/deleteCookie`,
`Storage.getCookies/setCookies/clearDataForOrigin/getUsageAndQuota` (zero
usage/quota; no quota bookkeeping), `Network.setCacheDisabled/
clearBrowserCache`, `Performance.enable/disable/getMetrics` (empty metric
list), `Security.enable/disable`, `DOM.getNodeForLocation/getContentQuads/
scrollIntoViewIfNeeded`, `Target.getTargetInfo/activateTarget`,
`Emulation.clearDeviceMetricsOverride`, `Console.enable`, and
`Runtime.consoleAPICalled`/`Log.entryAdded` events after navigation.

Explicitly unsupported (returns a JSON-RPC error, never a fake success):
`Fetch.enable/disable`, `Security.setIgnoreCertificateErrors`,
`Network.setExtraHTTPHeaders`, `Network.emulateNetworkConditions`.

Not implemented: `DOMDebugger`, `CSS`, `Accessibility`, `Debugger`,
`Profiler`, `HeapProfiler`, `Network` interception/caching controls,
`Fetch`, `Page.printToPDF`, `Page.addScriptToEvaluateOnNewDocument`,
`Overlay`, `Security`, `Storage`, `ServiceWorker`, `WebAudio`, `Media`,
`Cast`, `WebAuthn`, and DOM mutation events.

## WebDriver BiDi

WebSocket endpoint: `GET /session` (separate from HTTP `POST /session`).

Implemented modules:

- **session**: `session.new`, `session.status`, `session.end`, `browser.close`
  (creates/owns an isolated engine session; disconnected sockets close it)
- **browsingContext**: `getTree`, `create`, `navigate` (wait=complete),
  `activate`, `close`, `captureScreenshot` (viewport/document `origin`,
  PNG/JPEG/WebP `format` and `clip`)
- **script**: `evaluate`, `callFunction`, `getRealms`
- **network / log**: `network.getCookies`; after navigation emits
  `browsingContext.navigationStarted`, `browsingContext.load`,
  `network.beforeRequestSent`, `network.responseCompleted`,
  `log.entryAdded` (console and script errors), plus
  `browsingContext.contextCreated/contextDestroyed`

Not implemented: BiDi `input`, `storage`, `preloadScript`, `intercept`,
`emulation`, `webExtension`, and auth/permission modules.

## Vision (REST and MCP)

The vision surface returns layout-grounded facts derived from the rendered
raster and the layout tree. DOM text and element ranking need no model; an
opt-in, dependency-free raster OCR backend (`ocr_backend: "glyph"`) can also
read clean painted text from canvas/`<img>`. Every endpoint accepts the
standard `session_id` (REST body or `X-Nexium-Session` header) and returns the
same payload as the matching MCP tool's `structuredContent`.

| MCP tool | REST endpoint | Purpose |
|---|---|---|
| `vision_snapshot` | `POST /browser/vision/snapshot` | Base64 screenshot plus `elements`, `text_runs`, scroll and viewport. |
| `vision_analyze` | `POST /browser/vision/analyze` | `elements`, `text_runs`, `clickable_regions`, `viewport`, `scroll` and a PNG `image`. |
| `vision_extract_text` | `POST /browser/vision/extract-text` | Painted text in reading order, per-character `runs`, optional phrase `matches`. |
| `vision_find_element` | `POST /browser/vision/find-element` | Rank elements by natural-language `description` with `score` and `signals`. |
| `vision_click_target` | `POST /browser/vision/click-target` | Description → viewport `(x, y)`; `execute` clicks, `verify` diffs the result. |
| `vision_act` | `POST /browser/vision/act` | Describe once → locate, refuse ambiguous ties, scroll into view, act (`click`/`hover`/`scroll_into_view`) and verify. |
| `vision_compare` | `POST /browser/vision/compare` | Pixel `ratio`, `changed_regions`, `text_added`/`text_removed` and optional artifacts. |

Options: `vision_snapshot` takes `full_page`, `format` (`png`/`jpeg`/`jpg`/
`webp`), `quality` and `include_visual`; `vision_analyze` takes `full_page`
and an optional viewport `region`; `vision_extract_text` takes an optional
case-insensitive `query` and `region`; `vision_find_element` requires
`description` and accepts `top_k`/`region`; `vision_click_target` requires
`description` and accepts `execute`, `verify` and `button`; `vision_compare`
accepts `threshold`, `min_pixels` and `save_artifacts`. A `region` is
`{x, y, width, height, scale?}` in viewport CSS pixels (`width`/`height`
required). `snapshot` is a callable MCP alias for `vision_snapshot`; it is not
advertised as a separate tool in `tools/list`.

`vision_snapshot`, `vision_analyze` and `vision_extract_text` accept
`ocr_backend` (`none` default, `glyph` opt-in; an unknown name is a 400).
With `glyph`, responses include `ocr: [{text, rect, confidence, source:"ocr"}]`;
`vision_extract_text` always includes `ocr` (empty with `none`).
`vision_find_element`, `vision_click_target` and `vision_act` accept `locator`
(`heuristic`/`default`; an unknown name is a 400). `vision_act` requires
`description` and also takes `action` (`click` default, `hover`,
`scroll_into_view`), `verify` (default `true`), `threshold`, `min_pixels` and
`max_attempts` (default `2`). On success it returns
`{action, target:{x,y,node_id,label,confidence,score}, ambiguous:false,
executed:true, verification?}`; `verification` adds
`{ratio, changed_regions, text_added, text_removed, success_hint}`. When the
top two candidates tie (score delta <= 0.05 with best > 0.5) or the hit target
has moved, it returns `{action, ambiguous:true, executed:false}` with
`candidates` (or `reason: "target moved"`) and performs no action.

Typical agent loop:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call",
 "params":{"name":"vision_find_element",
           "arguments":{"description":"green Submit order button","top_k":3}}}
```

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call",
 "params":{"name":"vision_click_target",
           "arguments":{"description":"Submit order button","execute":true,"verify":true}}}
```

Or do the whole locate/act/verify sequence in one call:

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call",
 "params":{"name":"vision_act",
           "arguments":{"description":"Submit order button","action":"click","verify":true}}}
```

`vision_enabled = false` (config) disables all seven routes/tools with a
`bad_request` error. `threshold`/`min_pixels` control diff sensitivity, and
`save_artifacts` persists before/after PNGs under `api_artifacts_dir`. DOM
text stays exact via the layout index; the opt-in `glyph` OCR backend adds
painted canvas/`<img>` text (clean opaque text at moderate/large sizes, bounded
tiles, no model weights/GPU). Element ranking is deterministic heuristics;
model-backed locators are a trait-only extension point, not a shipped backend.

## Client compatibility notes

- **Puppeteer**: discovery/target attach, `Page.navigate`, `Runtime.evaluate`
  and `callFunctionOn`, DOM queries, mouse/keyboard input, screenshots and
  layout metrics work. Features needing unimplemented domains (e.g. request
  interception via `Fetch`, PDF, tracing) fail with JSON-RPC method errors.
- **Playwright**: Chromium `connectOverCDP` can attach and drive basic
  navigation/evaluation, but Playwright relies on additional CDP domains and
  object lifetimes not implemented here; treat it as partial.
- **DevTools frontend**: Elements panel can inspect the DOM tree, attributes,
  outer HTML and box model; other panels are unavailable.
- **Selenium 4**: standard WebDriver commands work; `executeCdpCommand`
  works for the CDP subset above (attach first).
- **AI agents**: use MCP tools or REST; `GET /browser/dom` returns the full
  DOM tree with node ids, attributes and children, and the `vision_*` tools
  locate text/elements by description and viewport coordinates instead of
  CSS selectors.

## Known protocol limitations

- Unknown CDP methods return `-32601` (`method not found`).
- WebDriver locators are CSS-only; session ownership is enforced per agent
  (`webdriver` agent for WebDriver sessions, `cdp` for CDP targets).
- CDP events are navigation-triggered; there is no live network stream or
  DOM mutation event stream.
- Protocol parity with a full browser is not claimed; this matrix is the
  source of truth.
