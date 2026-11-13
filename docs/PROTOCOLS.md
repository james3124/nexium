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
- **Page**: `enable`, `navigate`, `reload`, `captureScreenshot`,
  `getLayoutMetrics`, `getFrameTree`, `getNavigationHistory`,
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
  `activate`, `close`, `captureScreenshot`
- **script**: `evaluate`, `callFunction`, `getRealms`
- **network / log**: `network.getCookies`; after navigation emits
  `browsingContext.navigationStarted`, `browsingContext.load`,
  `network.beforeRequestSent`, `network.responseCompleted`,
  `log.entryAdded` (console and script errors), plus
  `browsingContext.contextCreated/contextDestroyed`

Not implemented: BiDi `input`, `storage`, `preloadScript`, `intercept`,
`emulation`, `webExtension`, and auth/permission modules.

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
  DOM tree with node ids, attributes and children.

## Known protocol limitations

- Unknown CDP methods return `-32601` (`method not found`).
- WebDriver locators are CSS-only; session ownership is enforced per agent
  (`webdriver` agent for WebDriver sessions, `cdp` for CDP targets).
- CDP events are navigation-triggered; there is no live network stream or
  DOM mutation event stream.
- Protocol parity with a full browser is not claimed; this matrix is the
  source of truth.
