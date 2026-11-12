//! Phase 3 gap tests: ES module import/export resolution and dynamic `import()`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread;

use nexium::{Browser, Config, Page};

fn browser() -> Browser {
    Browser::new(Config::default())
}

fn attr(page: &Page, selector: &str, name: &str) -> Option<String> {
    let node = page.select(selector).expect("selector").expect("node");
    page.document.get_attribute(node, name).map(str::to_owned)
}

/// A dependency served from a `data:` URL (no network needed).
const DATA_DEP: &str = "data:text/javascript,export const named='named-value';export const value=7;export default 'default-value';";

#[test]
fn static_imports_and_reexports_from_data_modules() {
    // Nested data URLs: `export ... from`, `export * from` and `export * as ns`.
    let reexports = r#"data:text/javascript,export { named as re } from 'data:text/javascript,export const named=\"re-exported\"';export * from 'data:text/javascript,export const extra=\"star-value\"';export * as depNs from 'data:text/javascript,export const named=\"ns-value\"';"#;
    let html = format!(
        r#"<body><p id="out"></p><script type="module">
import def, {{ named, value as alias }} from "{DATA_DEP}";
import * as ns from "{DATA_DEP}";
import {{ re, extra, depNs }} from "{reexports}";
var out = document.getElementById('out');
out.setAttribute('data-default', def);
out.setAttribute('data-named', named);
out.setAttribute('data-alias', String(alias));
out.setAttribute('data-ns-value', String(ns.value));
out.setAttribute('data-ns-default', ns.default);
out.setAttribute('data-reexport', re);
out.setAttribute('data-star', extra);
out.setAttribute('data-ns-reexport', depNs.named);
</script></body>"#
    );
    let page = browser().load_html("https://modules.test/", &html).unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    assert_eq!(
        attr(&page, "#out", "data-default").as_deref(),
        Some("default-value")
    );
    assert_eq!(
        attr(&page, "#out", "data-named").as_deref(),
        Some("named-value")
    );
    assert_eq!(attr(&page, "#out", "data-alias").as_deref(), Some("7"));
    assert_eq!(attr(&page, "#out", "data-ns-value").as_deref(), Some("7"));
    assert_eq!(
        attr(&page, "#out", "data-ns-default").as_deref(),
        Some("default-value")
    );
    assert_eq!(
        attr(&page, "#out", "data-reexport").as_deref(),
        Some("re-exported")
    );
    assert_eq!(
        attr(&page, "#out", "data-star").as_deref(),
        Some("star-value")
    );
    assert_eq!(
        attr(&page, "#out", "data-ns-reexport").as_deref(),
        Some("ns-value")
    );
}

#[test]
fn separate_inline_modules_share_one_module_instance() {
    let dep = "data:text/javascript,globalThis.__sharedModuleCount = (globalThis.__sharedModuleCount || 0) + 1;";
    let html = format!(
        r#"<body>
<script type="module">
import "{dep}";
document.body.setAttribute('data-first', String(globalThis.__sharedModuleCount));
</script>
<script type="module">
import "{dep}";
document.body.setAttribute('data-second', String(globalThis.__sharedModuleCount));
</script>
</body>"#
    );
    let page = browser().load_html("https://modules.test/", &html).unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    // Both inline modules resolve to the same URL, so the dependency is
    // evaluated exactly once.
    assert_eq!(attr(&page, "body", "data-first").as_deref(), Some("1"));
    assert_eq!(attr(&page, "body", "data-second").as_deref(), Some("1"));
}

#[test]
fn dynamic_import_returns_cached_namespace() {
    let html = r#"<body><p id="out"></p>
<script>
import("data:text/javascript,export const fromClassic = 'classic-value';").then(function (ns) {
  document.getElementById('out').setAttribute('data-classic', ns.fromClassic);
});
</script>
<script type="module">
globalThis.__dynamicEvaluations = 0;
const target = "data:text/javascript,globalThis.__dynamicEvaluations = globalThis.__dynamicEvaluations + 1;export default {ok:true};";
Promise.all([import(target), import(target)]).then(function (modules) {
  var out = document.getElementById('out');
  out.setAttribute('data-same-namespace', String(modules[0] === modules[1]));
  out.setAttribute('data-same-default', String(modules[0].default === modules[1].default));
  out.setAttribute('data-evaluations', String(globalThis.__dynamicEvaluations));
  out.setAttribute('data-ok', String(modules[0].default.ok));
});
</script></body>"#;
    let page = browser().load_html("https://modules.test/", html).unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    assert_eq!(
        attr(&page, "#out", "data-classic").as_deref(),
        Some("classic-value")
    );
    assert_eq!(
        attr(&page, "#out", "data-same-namespace").as_deref(),
        Some("true")
    );
    assert_eq!(
        attr(&page, "#out", "data-same-default").as_deref(),
        Some("true")
    );
    assert_eq!(
        attr(&page, "#out", "data-evaluations").as_deref(),
        Some("1")
    );
    assert_eq!(attr(&page, "#out", "data-ok").as_deref(), Some("true"));
}

#[test]
fn missing_modules_reject_and_report_errors() {
    let html = r#"<body>
<script type="module">
import('./missing-relative.js').then(
  function () { document.body.setAttribute('data-dynamic', 'resolved'); },
  function () { document.body.setAttribute('data-dynamic', 'rejected'); }
);
import('not-a-real-package').then(
  function () { document.body.setAttribute('data-bare', 'resolved'); },
  function () { document.body.setAttribute('data-bare', 'rejected'); }
);
</script>
</body>"#;
    let page = browser().load_html("https://modules.test/", html).unwrap();
    assert_eq!(
        attr(&page, "body", "data-dynamic").as_deref(),
        Some("rejected")
    );
    assert_eq!(
        attr(&page, "body", "data-bare").as_deref(),
        Some("rejected")
    );

    // A failing static import is surfaced as a script error and does not abort
    // the rest of the page.
    let html = r#"<body><script type="module">import value from './absent.js';</script></body>"#;
    let page = browser().load_html("https://modules.test/", html).unwrap();
    assert!(
        page.script_errors
            .iter()
            .any(|error| error.contains("absent.js")),
        "{:?}",
        page.script_errors
    );
}

#[test]
fn import_meta_url_and_top_level_await() {
    let html = r#"<body>
<script type="module">
document.body.setAttribute('data-meta-url', import.meta.url);
const value = await Promise.resolve(40);
document.body.setAttribute('data-tla', String(value + 2));
</script>
</body>"#;
    let page = browser()
        .load_html("https://modules.test/page.html", html)
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    assert_eq!(
        attr(&page, "body", "data-meta-url").as_deref(),
        Some("https://modules.test/page.html")
    );
    assert_eq!(attr(&page, "body", "data-tla").as_deref(), Some("42"));
}

fn spawn_module_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        request.extend_from_slice(&chunk[..count]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&request);
            let path = text
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let (content_type, body) = module_response(path);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    addr
}

fn module_response(path: &str) -> (&'static str, &'static str) {
    match path {
        "/graph.html" => (
            "text/html",
            r#"<!doctype html><body><p id="out"></p>
<script type="module">
import def, { named, meta } from './dep.js';
import * as ns from './dep.js';
import { re, extra } from '/reexport.js';
import './side.js';
var out = document.getElementById('out');
out.setAttribute('data-default', def);
out.setAttribute('data-named', named);
out.setAttribute('data-alias', String(ns.value));
out.setAttribute('data-meta', meta);
out.setAttribute('data-reexport', re);
out.setAttribute('data-star', extra);
out.setAttribute('data-evals', String(window.__depEvaluations));
</script>
<script type="module" src="/external.js"></script>
</body>"#,
        ),
        "/dep.js" => (
            "text/javascript",
            r#"window.__depEvaluations = (window.__depEvaluations || 0) + 1;
export const named = 'named-http';
export const value = 7;
export const meta = import.meta.url;
export default 'default-http';"#,
        ),
        "/reexport.js" => (
            "text/javascript",
            "export { named as re } from './dep.js';export * from './extra.js';",
        ),
        "/extra.js" => ("text/javascript", "export const extra = 'star-http';"),
        "/side.js" => (
            "text/javascript",
            "document.getElementById('out').setAttribute('data-side', 'ran');",
        ),
        "/external.js" => (
            "text/javascript",
            r#"import { named } from './dep.js';
document.getElementById('out').setAttribute('data-external', named);"#,
        ),
        "/cycle.html" => (
            "text/html",
            r#"<!doctype html><body><p id="out"></p><script type="module" src="/cycle-a.js"></script></body>"#,
        ),
        "/cycle-a.js" => (
            "text/javascript",
            r#"import { b } from './cycle-b.js';
export const a = 'a';
document.getElementById('out').setAttribute('data-cycle', a + ':' + b);"#,
        ),
        "/cycle-b.js" => (
            "text/javascript",
            r#"import { a } from './cycle-a.js';
export const b = 'b';"#,
        ),
        _ => ("text/plain", "missing"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_graph_resolves_relative_absolute_and_external_scripts() {
    let addr = spawn_module_server();
    let page = browser()
        .open(&format!("http://{addr}/graph.html"))
        .await
        .expect("page");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let dep_url = format!("http://{addr}/dep.js");
    assert_eq!(
        attr(&page, "#out", "data-default").as_deref(),
        Some("default-http")
    );
    assert_eq!(
        attr(&page, "#out", "data-named").as_deref(),
        Some("named-http")
    );
    assert_eq!(attr(&page, "#out", "data-alias").as_deref(), Some("7"));
    assert_eq!(
        attr(&page, "#out", "data-meta").as_deref(),
        Some(dep_url.as_str())
    );
    assert_eq!(
        attr(&page, "#out", "data-reexport").as_deref(),
        Some("named-http")
    );
    assert_eq!(
        attr(&page, "#out", "data-star").as_deref(),
        Some("star-http")
    );
    assert_eq!(attr(&page, "#out", "data-side").as_deref(), Some("ran"));
    assert_eq!(
        attr(&page, "#out", "data-external").as_deref(),
        Some("named-http")
    );
    // Imported by the inline module, the re-export and the external script, but
    // evaluated only once.
    assert_eq!(attr(&page, "#out", "data-evals").as_deref(), Some("1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn circular_imports_do_not_hang() {
    let addr = spawn_module_server();
    let page = browser()
        .open(&format!("http://{addr}/cycle.html"))
        .await
        .expect("page");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    assert_eq!(attr(&page, "#out", "data-cycle").as_deref(), Some("a:b"));
}
