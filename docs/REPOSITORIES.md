# Repository Navigation and Code Reading

NexiumX can open a GitHub repository, walk its tree, read bounded line ranges
and navigate between paths. The same operations are available through REST
(`/browser/repo/*`), MCP (`repo_*`) and the Rust crate
(`nexium::intelligence::RepoBackend`). Two backends implement the same trait:

| Backend | Source | How it reads | Best for |
|---|---|---|---|
| `github` (default) | `api` | GitHub REST `/contents` + `/git/trees`, raw file endpoint, ETag revalidation | Exact listings, rate-limit awareness, optional private repos |
| `browser` | `browser` | `Browser::open` on the GitHub web UI, DOM anchors and `blob-code` rows | Resilience when API access is unavailable; exercises the browser stack |

The browser backend is best-effort: if GitHub changes its markup, it returns
fewer entries rather than failing or panicking. It never reads or logs tokens.

## Repository references

A single canonical reference is carried through every response as
`owner/repo@ref:path` (`Display` on `RepoRef`). Accepted inputs:

| Input | Resolved ref | Path |
|---|---|---|
| `https://github.com/owner/repo` | none (default branch) | root |
| `https://github.com/owner/repo/tree/main/src` | `main` | `src` |
| `https://github.com/owner/repo/blob/main/src/lib.rs` | `main` | `src/lib.rs` |
| `https://raw.githubusercontent.com/owner/repo/main/src/lib.rs` | `main` | `src/lib.rs` |
| `owner/repo` | none | root |
| `owner/repo@main` | `main` | root |
| `owner/repo@main:src/lib.rs` | `main` | `src/lib.rs` |

Percent-escaped path segments are decoded. Traversal (`..`), absolute
shorthand paths, malformed escapes and non-GitHub hosts are rejected with a
`bad_request` error. `RepoRef::relative` resolves `..` against the current path
for navigation; `RepoRef::at_path` requires a validated in-repo path.

## Tools and endpoints

Every REST call is `POST` with a JSON body (or the matching MCP
`tools/call` arguments) and returns the standard `{ "ok": true, "data": ... }`
envelope. Directory/file responses carry
`{repo, ref, path, kind, source}` plus `entries` or file fields. `source` is
`github` (REST/raw backend) or `browser` (GitHub UI backend).

| MCP tool | REST endpoint | Arguments | Result |
|---|---|---|---|
| `repo_open` | `/browser/repo/open` | `url` **or** `repo`, optional `backend` (`github`/`browser`) | Root metadata + listing; opens a session |
| `repo_tree` | `/browser/repo/tree` | `prefix?` | Capped flat tree, sorted by path |
| `repo_list` | `/browser/repo/list` | `path?` | One location's entries and `truncated` |
| `repo_read` | `/browser/repo/read` | `path`, `start_line?`, `max_lines?` | `text`, `lines:[{n,text}]`, `total_lines`, `truncated`, `binary`, `language_hint` |
| `repo_search` | `/browser/repo/search` | `query`, `paths_only?` | Path/name substring matches from the flat tree |
| `repo_navigate` | `/browser/repo/navigate` | `target`, optional `direction` (`back`/`forward`) | Listing or file for the resolved path |

Each tool accepts the standard `session_id` (REST body or
`X-Nexium-Session` header); `repo_open` creates the per-session repo state,
including back/forward history. `repo_search` is a local substring search over
the bounded tree — it does not call GitHub's code-search API.

`repo_history` is **deferred**: no commit-history tool or endpoint ships.
Model/vision-based folder detection is an **optional enhancement**; the shipped
browser backend detects folders from DOM anchors, with
`vision_find_element`/`vision_click_target` still usable for manual exploration.

## Authentication and configuration

`github_token` in config, or the `GITHUB_TOKEN` environment variable when unset,
is sent only as an `Authorization: Bearer …` header by the `github` backend.
It is never logged, returned in responses or embedded in URLs. Without a token,
public repositories work but rate limits are easier to hit.

Relevant `config/default.toml` keys:

```toml
# github_token = "change-me"          # or export GITHUB_TOKEN
repo_max_file_bytes = 1048576          # larger files are reported as binary/metadata
repo_max_tree_entries = 50000          # cap for listings, trees and searches
repo_cache_ttl_secs = 300              # API response cache lifetime
repo_cache_max_entries = 128
# repo_api_base = "https://api.github.com"
# repo_raw_base = "https://raw.githubusercontent.com"
# repo_web_base = "https://github.com"   # browser backend UI base
```

`repo_api_base`, `repo_raw_base` and `repo_web_base` are overrides for local
mirrors and test servers; ordinary users should leave them unset.

## Limits and degradation

- Reads default to 400 lines and clamp `max_lines` to `1…2000`; files above
  `repo_max_file_bytes`, containing NUL bytes or invalid UTF-8 are returned as
  `binary: true` with no text.
- Listings and trees are capped at `repo_max_tree_entries`. A tree result of
  exactly that length may be truncated; listings carry a `truncated` flag.
- The browser backend walks tree links to a maximum depth of 3, deduplicates by
  path, and ignores anchors outside the requested repository/ref.
- HTTP 403/429 map to a rate-limit error with a retry hint; 404 maps to
  `not_found`. Other upstream failures are reported as `upstream`.

## Examples

```bash
# REST: open, inspect, read, navigate
curl -s -X POST http://127.0.0.1:9222/browser/repo/open \
  -H 'Content-Type: application/json' \
  -d '{"url":"owner/repo@main","session_id":"demo"}'
curl -s -X POST http://127.0.0.1:9222/browser/repo/tree \
  -H 'Content-Type: application/json' \
  -d '{"session_id":"demo","prefix":"src/"}'
curl -s -X POST http://127.0.0.1:9222/browser/repo/read \
  -H 'Content-Type: application/json' \
  -d '{"session_id":"demo","path":"src/lib.rs","start_line":10,"max_lines":25}'
curl -s -X POST http://127.0.0.1:9222/browser/repo/navigate \
  -H 'Content-Type: application/json' \
  -d '{"session_id":"demo","target":"../README.md"}'
```

MCP equivalents (same argument objects):

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call",
 "params":{"name":"repo_open","arguments":{"repo":"owner/repo@main"}}}
```

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call",
 "params":{"name":"repo_list","arguments":{"path":"src"}}}
```

Rust library use:

```rust
use nexium::intelligence::{BrowserRepoBackend, GitHubApiBackend, RepoBackend, RepoRef};
use nexium::Config;

let config = Config::default();
let rf = RepoRef::parse("owner/repo@main:src/lib.rs")?;
let backend = GitHubApiBackend::new(&config);
let file = backend.read(&rf, &rf.path, Some(1), Some(40)).await?;
let fallback = BrowserRepoBackend::new(&config);
let root = fallback.list(&rf, "").await?;
```

## Out of scope

Cloning or writing repositories, commit history (`repo_history`), full GitHub
code search, syntax highlighting/semantic analysis, and non-GitHub hosts
(GitLab/Bitbucket). The `RepoBackend` trait makes additional hosts and backends
straightforward extension points.
