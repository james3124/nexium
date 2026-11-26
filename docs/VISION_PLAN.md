# Visual Interaction Intelligence & Tool Integration — Implementation Plan

Target roadmap phases: **4. Visual Interaction Intelligence** and **5. Tool Integration**.

## Goal

Let Nexium use rendered screen information — not only DOM selectors — to drive browser
automation. The engine already renders a pixel-accurate raster and retains a full layout
tree, so the vision system should be **layout-grounded**: derive visual facts from the
rendered image plus the geometry/text/styles that produced it. This is deterministic,
fast, dependency-free, and testable without a model server. An optional pluggable OCR /
model backend can be added later for canvas text and image content that the DOM cannot
describe.

## Implementation status

Shipped as of 2026-11-23. This plan's Phase 4 (visual intelligence) landed in
`src/intelligence/visual.rs` and is exposed through `ApiService`; this plan's
Phase 5 (tool integration) landed in the REST and MCP layers. In the main
[ROADMAP](ROADMAP.md) these correspond to Phases 5 and 6.

- **Core:** `VisualIndex` builds text runs with per-character viewport boxes,
  elements with styled/raster signals, description ranking, phrase lookup and
  before/after diffs. Space fragments are preserved as real space runs, so
  phrases spanning inline elements (for example `Hello <span>vision</span>
  world`) match across fragment boundaries.
- **Service/REST:** the original six `vision_*` methods and their option structs
  are exposed at `POST /browser/vision/{snapshot,analyze,extract-text,
  find-element,click-target,compare}`; `vision_enabled` is the master switch.
  `vision_act` was added later (see below).
- **MCP:** the original six tools with JSON schemas in `tools/list`, dispatched
  through `tools/call`, plus the `snapshot` alias (callable, not advertised
  twice); `vision_act` was added later.
  MCP `structuredContent` and REST `data` are the same service payload.
- **Tests:** `tests/vision_core_tests.rs`, `tests/vision_api_tests.rs` and
  `tests/vision_mcp_tests.rs` cover index geometry/localization, REST parity
  and MCP listing/execution (including an `execute: true` click with
  verification, and compare artifacts); `tests/api_tests.rs` asserts the tool
  names. The later `tests/vision_depth_tests.rs` and
  `tests/vision_depth_api_tests.rs` cover OCR, the locator registry and
  `vision_act` over REST and MCP (7 and 6 tests respectively). Rendering/vision
  regressions total 42 tests (21 rendering + 8 vision API + 6 vision core +
  7 vision MCP); 0 failed.
- **Docs:** [PROTOCOLS.md](PROTOCOLS.md), [STATUS.md](STATUS.md),
  [ROADMAP.md](ROADMAP.md) and [README.md](../README.md) describe the shipped
  surface. The original tool table below is the design draft: delivered
  schemas follow the option structs in `src/api/service.rs` and capture the
  live session (no `image`-input parameter). The `ocr_backend`, `action` and
  `locator` options have since shipped; see below.

Shipped after this plan (2026-11-24): a pluggable OCR backend, a pluggable
element locator and the composite `vision_act` tool.

- **OCR backend (shipped):** `OcrBackend` in `src/intelligence/ocr.rs` with
  `NoOcr` (name `none`, the default) and `GlyphOcr` (name `glyph`), a
  dependency-free raster OCR that matches glyph templates rendered with the
  engine's embedded DejaVu fonts. `vision_snapshot`, `vision_analyze` and
  `vision_extract_text` accept an `ocr_backend` option (`none`/`glyph`; an
  unknown name is a 400); `vision_extract_text` always returns
  `ocr: [{text, rect, confidence, source:"ocr"}]` (empty with `none`). Glyph
  OCR is **bounded**: a fixed per-tile pixel budget (256x192 default), 12 px
  tile overlap, a 64-tile cap, then reading-order merging with dedup and
  aspect-/height-ratio case discrimination. Before the size filter, wide
  connected components are split into per-character pieces using vertical ink
  projections and valley/waist detection, so tightly spaced or touching glyphs
  (e.g. `GO`) are read as separate letters; a genuine deep valley is required,
  so single wide glyphs such as `W` are not over-split; shallow-waist and
  balanced-mass fallbacks apply only to very wide components. It targets clean, opaque text at
  moderate/large sizes; DOM text stays exact via the layout index. There are no
  model weights, GPU or VLM: the bounded-working-
  set / tiling / ordered-merge *concepts* are borrowed from
  baidu/Unlimited-OCR, not its model or code.
- **Pluggable locator (shipped):** `ElementLocator` in
  `src/intelligence/visual.rs`, `HeuristicLocator` (default), a `locator()`
  registry (`heuristic`/`default`), `VisualIndex::find_with`, and a corrected
  `element_at_point` that returns the deepest element rather than `html`/
  `body`. `vision_find_element`, `vision_click_target` and `vision_act` accept
  a `locator` option; unknown names are a 400.
- **Composite `vision_act` (shipped):** one `description` drives locate →
  disambiguate → scroll into view → act → verify. `action` is
  `click` (default) / `hover` / `scroll_into_view`, `verify` defaults to
  `true`, with `threshold`, `min_pixels`, `locator` and `max_attempts`
  (default `2`). Tied top-two candidates (score delta <= 0.05 with best >
  0.5) or a stale hit target return `{ambiguous:true, executed:false}` with
  `candidates` (or `reason: "target moved"`) instead of acting. Success
  returns `{action, target:{x,y,node_id,label,confidence,score},
  ambiguous:false, executed:true, verification?}`, where `verification`
  carries `{ratio, changed_regions, text_added, text_removed, success_hint}`.

Extension points, not shipped backends:

- **Model-backed locators/OCR:** `ElementLocator` and `OcrBackend` are traits,
  so a trained or hosted vision backend can be added later. No such backend
  ships, and no model weights or GPU are used.

## Design principles

1. **Layout + raster, not pixels alone.** DOM text positions, element rects, computed
   styles and accessible names are ground truth; the raster confirms what is actually
   painted (colors, clipping, overlap, final position).
2. **Coordinates are explicit.** Every result carries both document-space and
   viewport-space rectangles, plus the scroll offset used, so callers never guess.
3. **No new hard dependencies.** The core uses the existing `image` crate and layout
   metadata. OCR/locator backends sit behind traits and are selected per request;
   no Cargo feature flag or model weights are required.
4. **Tools are thin.** New tools validate arguments, call one `ApiService` method, and
   return the same `{ok, data}` / `structuredContent` shapes as existing tools.
5. **Explainable results.** Element matches include the signals that produced them
   (text match, role match, visual affordance, visibility) rather than an opaque score.

---

## Architecture

```
                       ┌────────────────────────────────────────────┐
                       │ Browser / PageSession (current scroll)     │
                       │  page + layout + scroll_x/y + viewport     │
                       └───────────────┬────────────────────────────┘
                                       │ render_page(RenderOptions{scroll…})
                                       ▼
                       ┌────────────────────────────────────────────┐
                       │ RenderedPage { image, layout, viewport }   │
                       └───────┬──────────────────────┬─────────────┘
                               │                      │
              ┌────────────────▼────────┐   ┌─────────▼──────────────────┐
              │ Raster analysis         │   │ Layout/Label analysis      │
              │ colors, borders, diff,  │   │ text runs, node_rects,     │
              │ changed regions         │   │ roles, styles, a11y names  │
              └────────────────┬────────┘   └─────────┬──────────────────┘
                               └──────────┬───────────┘
                                          ▼
                       ┌────────────────────────────────────────────┐
                       │ intelligence::visual::VisualIndex          │
                       │  visual_elements(), text_runs(),            │
                       │  find(description), compare(before,after)  │
                       └───────────────┬────────────────────────────┘
                                       │
              ┌────────────────────────┼────────────────────────┐
              ▼                        ▼                        ▼
        MCP tools / REST        coordinate click           artifacts
        (vision_*)              (browser_click x,y)        (before/after PNG)
```

New module: `src/intelligence/visual.rs`, registered next to `dom.rs` in
`src/intelligence/mod.rs:3`. Service methods live beside `screenshot` in
`src/api/service.rs`; tools in `src/mcp/tools.rs`; REST routes in `src/api/routes.rs`.

---

## Phase 4 — Visual Interaction Intelligence

### 4.1 Text-run index (OCR-free text finding)

**Problem solved:** find specific text on screen and get its coordinates without OCR.

**Implementation:**
- Add `LayoutResult::text_runs()` in `src/engine/layout/mod.rs` (next to `node_rect`/`style`,
  L148–164). Walk `LayoutBox.lines` (`LineBox` L124–130) and `InlineFragment::Text(String)`
  (L132–146). Compute each run's box from `fragment.x`, `fragment.width`, `line.baseline`,
  and `FontSet` ascent/descent (`src/engine/text.rs:127–158`).
- For per-character boxes, advance the pen with `FontSet::advance` exactly as
  `draw_text` does (`src/engine/paint/mod.rs:1598–1635`) so coordinates match painted glyphs.
- Include: `text`, `rect` (document px), `viewport_rect`, `node_id`, `font_size`, `color`,
  `line_index`, `reading_order`. Exclude `FragmentKind::Space` and hidden/clipped boxes.
- Expose through `PageSession::text_runs()` so scroll offsets are applied consistently.

**Files:** `src/engine/layout/mod.rs`, `src/engine/text.rs` (if a helper is needed),
`src/interaction/mod.rs`, `src/intelligence/visual.rs`.

**Optional raster OCR backend (design draft, superseded):** the original sketch was a
`LayoutOcr` backed by the text-run index plus a feature-gated `ocr-tesseract`. What shipped is
the `OcrBackend` trait with `NoOcr` and the bounded, template-based `GlyphOcr` described under
"Shipped after this plan" above — no feature flag and no external OCR dependency.

### 4.2 Visual element index & clickable-area detection

**Implementation:**
- Build `VisualElement` candidates from three sources and merge them by `NodeId`:
  1. **Semantic:** `node_rects` + `ElementInfo` (`src/interaction/mod.rs:71–86`) for tag,
     role, name, href, text, enabled/clickable state.
  2. **Style:** `LayoutResult.style(node)` for `cursor: pointer`, background/border colors,
     `border-radius`, underline, opacity, display/visibility.
  3. **Raster:** sample the rendered image inside the rect (center + corners) to confirm
     painted background color, detect borders/rounded corners, and check contrast with the
     surrounding pixels. Mark candidates that are clipped or fully occluded.
- Produce `rect` (document), `viewport_rect`, `center` (viewport), `label`, `confidence`,
  and `signals: Vec<String>` explaining the match.
- Cap candidates to elements intersecting the current viewport unless `full_page` is set.

**Files:** `src/intelligence/visual.rs`, `src/engine/layout/mod.rs` (style accessor if needed),
`src/interaction/mod.rs` (reuse `element_info`).

### 4.3 Natural-language element localization

**Problem solved:** detect UI elements using visual descriptions rather than DOM selectors.

**Scoring model (deterministic, explainable):**
- Tokenize the description; match against accessible name, visible text, `aria-label`,
  `placeholder`, `title`, `id`/`class`, role/tag synonyms (button/submit → `button`,
  `[role=button]`, `input[type=submit]`; link → `a`, `[role=link]`), href text, and nearby
  text runs within a small radius.
- Boost: `cursor: pointer`, button-like raster appearance, `:focus`/`:hover` state,
  larger visible area, closeness to viewport center when multiple matches tie.
- Penalize: zero-area, offscreen, `aria-hidden`, disabled, low contrast, clipped content.
- Return top-k with `confidence` and per-signal breakdown; never auto-act on ambiguous
  results unless the caller opts into `select_first`.

**Files:** `src/intelligence/visual.rs`.

### 4.4 Coordinate interaction (fallback when selectors are unavailable)

- `browser_click` already accepts `x`/`y` viewport coordinates and dispatches through
  `PageSession::hit_test` (`src/interaction/mod.rs:316–328`, `887–889`). Add:
  - `vision_click_target` tool that resolves a description to a point and returns
    `{x, y, viewport, node_id, label, confidence}`; with `execute: true` it calls the
    existing click path and returns the click result.
  - Optional `scroll_into_view` in the target tool: call `PageSession::scroll_into_view`
    (L297–309) before locating, re-render, then return the updated viewport coordinates.
- Validate proposed points with `ApiService::element_at_point` (L1093) / `hit_test` so a
  stale coordinate cannot click the wrong element after a scroll.

### 4.5 Screenshot verification (before/after)

- Add `diff_regions(before, after, threshold, min_area) -> Vec<Rect>` in
  `src/rendering/screenshots.rs` alongside `diff` (L113–134): block-grid or connected-
  component grouping of changed pixels, returning changed bounding boxes and a ratio.
- Add `VisualIndex::compare(before, after)` returning
  `{ratio, changed_regions, text_added, text_removed, url_changed}`. Reuse text-run diffs
  for semantic changes and `screenshots::diff` for pixel changes.
- Capture helpers already exist: `PageSession::viewport_image` (L862–876),
  `ApiService::screenshot` (L1419), artifact store (L1361–1417). A new
  `ApiService::vision_compare` captures two screenshots in one session and returns the
  analysis, saving both as artifacts when requested.

---

## Phase 5 — Tool Integration

### 5.1 Tool surface (MCP + REST)

Existing patterns: `mcp/tools.rs::call` (L26–110), `tool_definitions` (L174–509),
`ApiService` option structs (L120–255), REST handlers (`routes.rs:603–656`). The generic
`tools/call` dispatcher in `mcp/mod.rs` needs no change.

| Tool | Purpose | Key inputs | Output |
|---|---|---|---|
| `vision_snapshot` (alias `snapshot`) | Capture current viewport/full page **with visual metadata** | `session_id`, `full_page`, `format`, `include_visual` | base64 image + `visual_elements`, `text_runs`, scroll/viewport info, optional artifact path |
| `vision_analyze` | Analyze a captured screenshot or the live viewport | `session_id`, optional `image` (base64 or artifact name), `regions` | `{elements, text_runs, clickable_regions, viewport, analysis}` |
| `vision_extract_text` | Extract visible text with boxes | `session_id`, `region`, `ocr_backend` | `{text, runs:[{text, rect, confidence}], reading_order}` |
| `vision_find_element` | Locate by natural-language description | `session_id`, `description`, `top_k`, `region` | `{matches:[{node_id, label, rect, viewport_rect, confidence, signals}]}` |
| `vision_click_target` | Resolve description → coordinates, optionally click | `session_id`, `description`, `execute`, `verify`, `button` | `{x, y, node_id, confidence, click_result?, verification?}` |
| `vision_compare` *(recommended extra)* | Verify an action via before/after screenshots | `session_id`, `action` (optional tool call to perform between captures), `threshold` | `{ratio, changed_regions, text_diff, success_hint}` |

Notes:
- `browser_snapshot` (DOM/accessibility) stays unchanged for compatibility; `snapshot`
  in the requirements maps to the new capture-oriented `vision_snapshot`. If an exact
  tool name `snapshot` is required, register it as an alias pointing to the same method.
- All tools accept the standard `session_id` (body/query/header precedence via
  `pick_session`, `routes.rs:1077–1081`).
- Results are returned as service `Value`, so MCP `structuredContent` and REST `data` are
  identical.
- Update the tool-name assertion list in `tests/api_tests.rs:627–649`.

### 5.2 Service methods & routes

- Add option structs beside the existing ones in `service.rs:120–255`:
  `VisionOptions`, `VisionAnalyzeOptions`, `VisionTextOptions`, `VisionFindOptions`,
  `VisionClickOptions`, `VisionCompareOptions` (each with `session_id`).
- Add methods: `vision_snapshot`, `vision_analyze`, `vision_extract_text`,
  `vision_find_element`, `vision_click_target`, `vision_compare` — all following the
  `resolve(auth,id)` + `browser.with_page_session` pattern used by `click`/`screenshot`.
- REST endpoints in `routes.rs` near the existing `/browser/screenshot` and
  `/browser/snapshot` routes:
  `/browser/vision/snapshot`, `/analyze`, `/extract-text`, `/find-element`,
  `/click-target`, `/compare` (POST, `ApiJson<…>` handler template at L603–614).

### 5.3 Agent workflow integration

- Emit `screenshot`, `click` and `vision` events on the existing event bus so agents can
  subscribe to visual actions (same pattern as `service.rs:1557–1565`).
- Allow agents to chain: `vision_snapshot` → `vision_find_element` → `vision_click_target`
  → `vision_compare` using one `session_id`, with artifacts retained for audit.
- Document the tool sequence and examples in `docs/PROTOCOLS.md` and `README.md`.

### 5.4 Configuration

Add to `Config` (`src/config.rs:15–89`) with defaults (L91–134) and sample entries in
`config/default.toml`:

```
vision_enabled = true              # master switch for vision tools
vision_text_index = true           # layout-derived text runs (default backend)
vision_ocr_backend = "layout"      # "layout" | "tesseract" (feature-gated) | "none"
vision_max_candidates = 20
vision_region_padding = 8          # px around a region for nearby-text matching
vision_diff_threshold = 8          # per-channel delta, matches screenshots::diff
vision_save_artifacts = false      # persist before/after images
```

The `vision_ocr_backend` line is design-draft only: OCR and locator selection shipped as
per-request `ocr_backend`/`locator` options rather than config entries, and there is no
`"layout"` or `"tesseract"` backend.

---

## Workstreams & sequencing

Four largely independent workstreams; W1 and W2 can run in parallel, then W3 and W4:

- **W1 — Text & visual index** (layout text runs, raster sampling, `VisualIndex`, OCR trait).
  Owns `src/engine/layout/`, `src/engine/text.rs`, `src/intelligence/visual.rs`,
  `src/interaction/mod.rs`.
- **W2 — Diff & verification** (`diff_regions`, compare logic, artifact wiring).
  Owns `src/rendering/screenshots.rs`, `src/rendering/mod.rs`, diff tests.
- **W3 — Service & REST** (option structs, methods, routes, config). Owns `src/api/`,
  `src/config.rs`, `config/default.toml`.
- **W4 — MCP tools & docs/tests** (`mcp/tools.rs`, tool tests, docs, README). Owns
  `src/mcp/`, `tests/`, `docs/`.

W3/W4 can start against stub `VisualIndex` APIs once W1/W2 signatures are agreed.

---

## Testing strategy

- **Unit/geometry:** `tests/vision_geometry_tests.rs` — render known HTML, assert text-run
  rects match painted glyph locations (compare against pixels from `rendering_tests`
  helpers), assert document↔viewport coordinate transforms at several scroll offsets.
- **Localization:** `tests/vision_localization_tests.rs` — descriptions like "the green
  Submit button", "Sign in link", "search box" return the expected `node_id`/rect; include
  ambiguous and negative cases.
- **Interaction:** `tests/vision_interaction_tests.rs` — `vision_click_target` with
  `execute: true` follows a link / submits a form; verify via `vision_compare` that the
  changed region is non-trivial and text diff shows the expected new content.
- **API/MCP:** `tests/gap_vision_api_tests.rs` — register tools, call each through
  `mcp::handle_request` with `service.local_auth(...)`, assert REST endpoints return the
  same `data`, artifact paths exist and are valid images, session isolation holds.
- **Regression:** extend `tests/api_tests.rs` tool-name assertion list; run the existing
  rendering/interaction/api suites unchanged.
- Use existing test conventions: `Browser::load_html("https://render.test/", html)` for
  offline pages, `common::spawn_test_server` + `spawn_api` for HTTP/MCP, unique temp dirs
  and cleanup.

---

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Text-run coordinates drift from painted glyphs (kerning, ligatures, subpixel) | Use the same `FontSet::advance` path as `draw_text`; add pixel-anchored tests; require tolerance in comparisons. |
| Description scoring mis-ranks visually similar elements | Return top-k with signals; keep `execute: false` default; let caller disambiguate. |
| Screenshot diff false positives (caret blinking, animations, hover) | Threshold + minimum area; ignore small regions; optional CSS-freeze before capture. |
| Canvas/`<img>` text invisible to layout index | Opt-in `glyph` OCR backend (no feature flag); results carry `source: "layout" | "ocr"`. |
| Large pages → slow analysis | Restrict to viewport by default; `full_page` opt-in; cap candidates and text runs. |
| Scroll/coordinate confusion | Every result carries document rect, viewport rect and scroll offset; validate points with `hit_test` before clicking. |

## Out of scope for this phase

- Full deep-learning object detection or hosted vision-model calls (pluggable trait only).
- Real-time video/scrolling capture; vision operates on discrete screenshots.
- Pixel-perfect OCR: the shipped glyph backend is bounded and template-based
  (16x16 glyph masks). Anti-aliased or very small text may be missed, extreme
  overlap with no valley/waist (e.g. ~0.8x advance) can still be rejected, and
  exotic fonts or descender-heavy lowercase text remain limited by the template
  matcher. OCR covers painted/canvas text only; DOM text is handled exactly by
  the layout index.
- Modifying the paint pipeline to retain glyph raster metadata.

## Definition of done

- All six tools callable via MCP and REST with identical response shapes.
- Text finding returns screen coordinates that match rendered text within a small tolerance.
- A natural-language description locates buttons/links/inputs on the local test fixture with
  correct coordinates, and `vision_click_target` can actually activate them.
- Before/after comparison detects a real UI change and stays quiet for an unchanged page.
- New test files pass alongside the existing full suite; docs and tool schema lists updated.

All definition-of-done items above are met (2026-11-23); OCR, the locator registry and
`vision_act` shipped afterwards (2026-11-24). Model-backed backends remain trait-only
extension points; see "Implementation status".
