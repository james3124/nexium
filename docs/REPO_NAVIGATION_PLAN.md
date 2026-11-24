# Repository Navigation & Code Reading — Improvement Plan

## Goal

Let an agent ask Nexium to open a GitHub repository, walk its folder tree, and read
source files — without hand-crafting raw API calls or scraping fragile page markup
manually. Two complementary paths:

1. **Structured path (primary):** GitHub REST API + raw file endpoints via Nexium's
   existing `HttpClient`. Exact, fast, rate-limit-aware.
2. **Browser path (fallback):** navigate the GitHub web UI with the existing browser,
   DOM/vision tools and coordinate clicks. Works when API access is unavailable and
   proves the browser automation stack can drive a real code-hosting site.

The browser path is not a replacement for the API path; it is a resilience and
integration story, and it reuses `browser_navigate`, `browser_snapshot`,
`vision_find_element`, `vision_click_target` and `browser_extract_text` as-is.

## Design principles

- **One repo reference everywhere.** `owner/repo@ref:path` is parsed once and carried
  through every tool response so callers can navigate back and forth deterministically.
- **Directory and file are one operation.** Reading a path returns either a listing or
  file content with the same response envelope; callers do not branch on URL shape.
- **Bounded reads.** Cap file size, line ranges and tree size; never dump a multi-megabyte
  generated file into an agent context.
- **No secrets in output.** Tokens are read from config/env, sent only as headers, and
  never logged or returned.
- **Reuse existing infrastructure.** `src/networking/client.rs` (`HttpClient`, `Request`,
  `Response`), `src/api/service.rs` option/method patterns, `src/mcp/tools.rs` schema
  helpers, artifact store, and event bus.

---

## Architecture

```
                    repo_open / repo_navigate (URL or owner/repo@ref:path)
                                      │
                                      ▼
                        src/intelligence/repo.rs
                    RepoRef::parse / display / resolve
                                      │
                    ┌─────────────────┴─────────────────┐
                    ▼                                   ▼
          GitHubApiBackend                     BrowserRepoBackend
          HttpClient GET                       browser_navigate + DOM/vision
          /git/trees (recursive)               folder/file anchor detection
          /contents/{path}                     click → breadcrumb path
          raw.githubusercontent.com           extract visible code text
                    └─────────────────┬─────────────────┘
                                      ▼
                 RepoToolService (in ApiService) + NavState per session
                                      ▼
              REST /browser/repo/*  ·  MCP repo_* tools  ·  events
```

New module: `src/intelligence/repo.rs`, registered in `src/intelligence/mod.rs` next to
`dom.rs`, `ocr.rs`, `visual.rs`. New service methods live beside the vision methods in
`src/api/service.rs`; tools in `src/mcp/tools.rs`; routes in `src/api/routes.rs`.

---

## Phase 1 — Repo reference and URL parsing

- `RepoRef { host: String, owner: String, repo: String, rf: Option<String>, path: String }`.
- Parse:
  - `https://github.com/owner/repo`
  - `https://github.com/owner/repo/tree/<ref>/<path>`
  - `https://github.com/owner/repo/blob/<ref>/<path>`
  - `https://raw.githubusercontent.com/owner/repo/<ref>/<path>`
  - shorthand `owner/repo`, `owner/repo@main`, `owner/repo@main:src/lib.rs`
- Reject traversal (`..`), absolute paths, unsupported hosts (bad_request with a clear
  message). Percent-decode path segments. Default `rf` to the repository's default
  branch, resolved lazily via `/repos/{owner}/{repo}`.
- `RepoRef::display()` → canonical `owner/repo@ref:path` used in every response.

## Phase 2 — GitHub API backend

- `trait RepoBackend` with `tree(&RepoRef)`, `list(&RepoRef, path)`, `read(&RepoRef, path, range)`, `history(&RepoRef, path)`, `search(&RepoRef, query)`.
- `GitHubApiBackend` using `HttpClient`:
  - `GET /repos/{o}/{r}` → `default_branch`, visibility, size.
  - `GET /repos/{o}/{r}/git/trees/{ref}?recursive=1` → flat entries; cache by `(repo, ref, etag)`.
  - `GET /repos/{o}/{r}/contents/{path}?ref=...` → directory entries with `type`, `size`, `download_url`; for files prefer `raw.githubusercontent.com/{o}/{r}/{ref}/{path}` for plain text.
  - `GET /repos/{o}/{r}/commits?path=...&per_page=N` for history (optional phase).
  - `GET /search/code?q=...+repo:{o}/{r}` requires auth; degrade gracefully with a clear "token required" error.
- Headers: `Accept: application/vnd.github+json`, `X-GitHub-Api-Version`, `User-Agent`;
  `Authorization: Bearer <token>` only when configured.
- Config: `github_token: Option<String>` (env `GITHUB_TOKEN` fallback), `repo_max_file_bytes` (default 1 MiB), `repo_max_tree_entries` (default 50k), `repo_cache_ttl_secs` (default 300), `repo_cache_dir` under `data_dir`.
- Map HTTP 403/429 to structured errors with retry hints; 404 → not_found; redact token from any error text.
- Cache: in-memory map + optional disk JSON, keyed by canonical request + ETag; prune by TTL and size.

## Phase 3 — Tool surface

| Tool / REST endpoint | Purpose | Key inputs |
|---|---|---|
| `repo_open` · `POST /browser/repo/open` | Resolve a URL/shorthand, load default-branch metadata | `url` or `repo` |
| `repo_tree` · `POST /browser/repo/tree` | Full flat tree (path/type/size), capped | `session_id`, `path?` prefix |
| `repo_list` · `POST /browser/repo/list` | One directory's entries | `session_id`, `path` |
| `repo_read` · `POST /browser/repo/read` | File text with line numbers | `session_id`, `path`, `start_line?`, `max_lines?` |
| `repo_search` · `POST /browser/repo/search` | Search code/paths within repo | `session_id`, `query`, `paths_only?` |
| `repo_navigate` · `POST /browser/repo/navigate` | Follow a relative path or URL, back/forward; returns list or file | `session_id`, `target` (`..`, `../src`, URL, path), `direction?` |
| `repo_history` · `POST /browser/repo/history` | Recent commits touching a path (optional) | `session_id`, `path?`, `limit?` |

Every response carries `{repo, rf, path, kind, source}` where `source` is `"api"` or
`"browser"`. Directory responses add `entries:[{name,path,type,size}]`; file responses
add `text`, `lines:[{n,text}]`, `total_lines`, `truncated`, `language_hint`.

Session state: `RepoNavState { rf, path, history: Vec<String>, index }` stored alongside
the existing API session (reuse session save/restore so it survives restarts). Tools
accept `session_id` with the established body/header precedence.

## Phase 4 — Browser UI fallback

- `browser_navigate` to the repo URL; `browser_snapshot` + `browser_extract_text` to
  read the file tree.
- Locate folder/file links by href pattern `/{owner}/{repo}/(tree|blob)/{ref}/{path}` from
  the DOM; use `vision_find_element` with descriptions like "the src folder" when DOM
  selectors are insufficient; `vision_click_target` (or `browser_click` with the returned
  coordinates) to enter a folder.
- Track the current path from the URL (or breadcrumb elements) and expose it in the
  response so the API and browser paths are interchangeable.
- For a blob page, read code from the DOM text (GitHub wraps lines in table cells) or
  use `browser_extract_text`; fall back to `vision_extract_text` with OCR only when the
  code is rendered as an image.
- This path is a backend implementation of the same `RepoBackend` trait, selected by
  `backend: "browser"` per request or automatically when API auth/rate limits fail.

## Phase 5 — Code reading experience

- Line numbering and range reads so large files are consumable (`start_line`, `max_lines`).
- Language hint from extension for downstream syntax highlighting; no highlighting in core.
- Binary detection (NUL bytes, non-text extensions) → return metadata instead of bytes.
- Optional `repo_render`: write a small local HTML page with numbered lines into the
  artifact store and open it in a tab, so `browser_screenshot` / `vision_analyze` can
  inspect code visually. Stretch goal; not required for the first release.
- Emit `repo` events (opened/navigated/read) on the existing event bus for agents.

## Phase 6 — Testing and documentation

- **Parsing tests** (`tests/repo_parse_tests.rs`): all URL/shorthand forms, invalid paths,
  percent-decoding, canonical display.
- **API backend tests** (`tests/repo_api_tests.rs`): local mock GitHub server returns
  fixture tree/contents/raw responses; assert listing, ranged reads, truncation, binary
  handling, auth header, ETag cache reuse, 403/429 retry errors, token redaction.
- **Browser backend tests** (`tests/repo_browser_tests.rs`): local HTML fixture mimics
  GitHub tree/blob pages; assert folder navigation via DOM/vision tools and code text
  extraction.
- **Tool tests** (`tests/repo_tools_tests.rs`): REST/MCP parity, session isolation,
  back/forward history, `source` field, unknown backend/path errors.
- Docs: new `docs/REPOSITORIES.md` (tool reference + examples), updates to
  `PROTOCOLS.md`, `STATUS.md`, `ROADMAP.md`, `README.md`.

---

## Workstreams and sequencing

- **W1 — Core + API backend** (`src/intelligence/repo.rs`, `src/config.rs`): parser,
  `RepoBackend`, `GitHubApiBackend`, cache, config. Foundation; do first.
- **W2 — Service/REST/MCP** (`src/api/service.rs`, `routes.rs`, `mcp/tools.rs`):
  nav state, methods, seven tools/routes, events. Depends on W1 trait.
- **W3 — Browser backend + tests** (`src/intelligence/repo.rs` browser impl,
  `tests/repo_browser_tests.rs`): can run once W1/W2 signatures are stable.
- **W4 — Docs + API/tool tests**: can run in parallel with W3.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| GitHub rate limits / 403 | Cache, ETag revalidation, structured retry errors, optional token, browser fallback. |
| Huge repos / generated files | Tree entry cap, file byte cap, line ranges, truncation flag. |
| Token leakage | Header-only, never logged, redacted in errors, no token in responses. |
| GitHub UI markup changes | DOM anchors are primary, vision/link-text fallback; browser path is best-effort. |
| Path traversal / host confusion | Strict `RepoRef` parsing, host allowlist, segment validation. |
| Binary or minified content | Binary detection, extension hints, metadata-only response. |

## Out of scope

- Cloning repositories or writing files locally.
- Full code search across GitHub (search API is optional and token-gated).
- Syntax highlighting, semantic analysis, or diffing between revisions.
- Non-GitHub hosts (GitLab/Bitbucket) — trait makes them straightforward later.

## Definition of done

- From a repo URL, an agent can list the root, enter nested folders, read a file with
  line numbers, navigate back, and do all of it through either REST or MCP.
- Every response identifies the canonical repo reference, resolved ref, path and source.
- Large/binary files and rate limits produce clear, bounded responses rather than
  failures or unbounded payloads.
- New test suites pass alongside the existing full suite; docs describe the seven tools
  and both backends.
