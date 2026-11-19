mod common;

use common::spawn_test_server;
use futures_util::{SinkExt, StreamExt};
use nexium::engine::css::Viewport;
use nexium::{Browser, Config, RenderOptions};
use tokio_tungstenite::tungstenite::Message;

async fn spawn_ws_server() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message {
                let _ = socket
                    .send(Message::Text(format!("echo:{text}").into()))
                    .await;
            }
        }
    });
    addr
}

fn browser() -> Browser {
    Browser::new(Config::default())
}

#[test]
fn inline_scripts_mutate_the_dom() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<title>Old</title><div id="app"></div>
<script>
  document.title = "New";
  const app = document.getElementById("app");
  app.textContent = "hello";
  app.setAttribute("data-x", "1");
  console.log("log", 5);
</script>"#,
        )
        .unwrap();
    assert_eq!(page.title().as_deref(), Some("New"));
    let app = page.select("#app").unwrap().unwrap();
    assert_eq!(page.document.text_content(app), "hello");
    assert_eq!(page.document.get_attribute(app, "data-x"), Some("1"));
    assert_eq!(
        page.console.last().map(|message| message.text.as_str()),
        Some("log 5")
    );
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_workers_exchange_messages() {
    let addr = spawn_test_server().await;
    let browser = browser();
    let page = browser
        .open(&format!("http://{addr}/worker-page"))
        .await
        .unwrap();
    let body = page.select("body").unwrap().unwrap();
    assert_eq!(page.document.get_attribute(body, "data-worker"), Some("42"));
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_client_receives_messages() {
    let addr = spawn_ws_server().await;
    let browser = browser();
    let html = format!(
        r#"<body><script>
  var ws = new WebSocket('ws://{addr}');
  ws.onopen = function () {{ ws.send('hello'); }};
  ws.onmessage = function (event) {{
    document.body.setAttribute('data-ws', event.data);
    ws.close();
    clearInterval(timer);
  }};
  var tries = 0;
  var timer = setInterval(function () {{ if (++tries > 200) clearInterval(timer); }}, 10);
</script></body>"#
    );
    let page = browser.load_html("http://ws.test/", &html).expect("page");
    let body = page.select("body").unwrap().unwrap();
    assert_eq!(
        page.document.get_attribute(body, "data-ws"),
        Some("echo:hello")
    );
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn canvas_to_data_url_exports_png() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<canvas id="c" width="40" height="20"></canvas><script>
  var ctx = document.getElementById('c').getContext('2d');
  ctx.fillStyle = 'blue';
  ctx.fillRect(0, 0, 10, 10);
</script>"#,
        )
        .unwrap();
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 100.0,
            height: 100.0,
        },
    );
    let value = session
        .evaluate("document.getElementById('c').toDataURL()")
        .unwrap();
    assert!(
        value
            .as_str()
            .unwrap_or("")
            .starts_with("data:image/png;base64,"),
        "{value}"
    );
}

#[test]
fn dom_nodes_and_functions_serialize_to_null_without_hanging() {
    let browser = browser();
    let page = browser
        .load_html("https://js.test/", "<body><p id=target>x</p></body>")
        .unwrap();
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    for script in [
        "document.body",
        "document.getElementById('target')",
        "document.querySelector('p')",
        "(function(){})",
    ] {
        assert_eq!(
            session.evaluate(script).unwrap(),
            serde_json::Value::Null,
            "{script}"
        );
    }
    let value = session.evaluate("1 + 1").unwrap();
    assert_eq!(value.as_f64(), Some(2.0));
}

#[test]
fn adjacent_html_inner_text_and_scripts_collection() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="app"><p id="first">first</p></div><script>
  const app = document.getElementById("app");
  document.getElementById("first").insertAdjacentHTML("afterend", "<p id='second'>second</p>");
  app.insertAdjacentHTML("afterbegin", "<p id='zero'>zero</p>");
  app.insertAdjacentHTML("beforeend", "<p id='last'>last</p>");
  document.body.insertAdjacentHTML("beforeend", "<p id='tail'>tail</p>");
  const pre = document.createElement("span");
  pre.id = "pre";
  pre.textContent = "pre";
  document.body.prepend(pre);
  document.body.append("tail-text");
  document.body.setAttribute("data-scripts", String(document.scripts.length));
  document.body.setAttribute("data-inner", app.innerText);
</script></body>"#,
        )
        .unwrap();
    for id in ["second", "zero", "last", "tail", "pre"] {
        assert!(page.select(&format!("#{id}")).unwrap().is_some(), "{id}");
    }
    let app = page.select("#app").unwrap().unwrap();
    assert_eq!(page.document.text_content(app), "zerofirstsecondlast");
    let body = page.select("body").unwrap().unwrap();
    assert_eq!(page.document.get_attribute(body, "data-scripts"), Some("1"));
    assert_eq!(
        page.document.get_attribute(body, "data-inner"),
        Some("zerofirstsecondlast")
    );
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn modern_browser_apis_work_in_scripts() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="x" style="color:red;width:50px"></div><script>
  const media = matchMedia('(min-width: 600px)');
  document.body.setAttribute('data-mq', String(media.matches));
  const style = getComputedStyle(document.getElementById('x'));
  document.body.setAttribute('data-color', style.getPropertyValue('color'));
  const clone = structuredClone({ a: [1, 2], b: 'x' });
  document.body.setAttribute('data-clone', clone.a.join(',') + clone.b);
  requestIdleCallback(function () {
    document.body.setAttribute('data-idle', 'yes');
  });
</script></body>"#,
        )
        .unwrap();
    let body = page.select("body").unwrap().unwrap();
    assert_eq!(page.document.get_attribute(body, "data-mq"), Some("true"));
    assert_eq!(page.document.get_attribute(body, "data-color"), Some("red"));
    assert_eq!(
        page.document.get_attribute(body, "data-clone"),
        Some("1,2x")
    );
    assert_eq!(page.document.get_attribute(body, "data-idle"), Some("yes"));
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn scripts_can_create_and_replace_elements() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="app"></div><script>
  const created = document.createElement("div");
  created.id = "created";
  created.textContent = "child";
  document.body.appendChild(created);
  document.getElementById("app").innerHTML = "<span class='inner'>inner</span>";
</script></body>"#,
        )
        .unwrap();
    let created = page.select("#created").unwrap().unwrap();
    assert_eq!(page.document.text_content(created), "child");
    let inner = page.select(".inner").unwrap().unwrap();
    assert_eq!(page.document.text_content(inner), "inner");
}

#[test]
fn scripts_can_style_elements_and_toggle_classes() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r##"<div id="x"></div><script>
  const el = document.querySelector("#x");
  el.style.color = "red";
  el.style.setProperty("font-size", "20px");
  el.classList.add("active");
  el.classList.toggle("hidden");
</script>"##,
        )
        .unwrap();
    let x = page.select("#x").unwrap().unwrap();
    let style = page.document.get_attribute(x, "style").unwrap_or("");
    assert!(style.contains("color: red"), "{style}");
    assert!(style.contains("font-size: 20px"), "{style}");
    assert_eq!(
        page.document.get_attribute(x, "class"),
        Some("active hidden")
    );
}

#[test]
fn event_listeners_and_dom_content_loaded_run() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><button id="btn">go</button><p id="out"></p>
<script>
  document.addEventListener("DOMContentLoaded", () => {
    document.getElementById("out").textContent = "loaded";
  });
  document.getElementById("btn").addEventListener("click", () => {
    document.getElementById("out").setAttribute("data-click", "1");
  });
  document.getElementById("btn").click();
</script></body>"#,
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "loaded");
    assert_eq!(page.document.get_attribute(out, "data-click"), Some("1"));
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn promises_timers_and_raf_are_drained() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><p id="out"></p><script>
  Promise.resolve().then(() => {
    document.getElementById("out").textContent = "promise";
    setTimeout(() => {
      document.getElementById("out").setAttribute("data-timer", "1");
    }, 0);
  });
  requestAnimationFrame(() => {
    document.getElementById("out").setAttribute("data-raf", "1");
  });
</script></body>"#,
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "promise");
    assert_eq!(page.document.get_attribute(out, "data-timer"), Some("1"));
    assert_eq!(page.document.get_attribute(out, "data-raf"), Some("1"));
}

#[test]
fn script_errors_are_reported() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            "<script>throw new Error('boom');</script>",
        )
        .unwrap();
    assert!(
        page.script_errors
            .iter()
            .any(|error| error.contains("boom")),
        "{:?}",
        page.script_errors
    );
}

#[tokio::test]
async fn external_scripts_are_loaded() {
    let addr = spawn_test_server().await;
    let browser = browser();
    let page = browser
        .open(&format!("http://{addr}/js-page"))
        .await
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "external");
    assert!(
        page.console
            .iter()
            .any(|message| message.text.contains("external log"))
    );
}

#[tokio::test]
async fn fetch_resolves_into_the_dom() {
    let addr = spawn_test_server().await;
    let browser = browser();
    let page = browser
        .open(&format!("http://{addr}/fetch-js"))
        .await
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(
        page.document.text_content(out),
        "ok",
        "errors={:?} console={:?}",
        page.script_errors,
        page.console
    );
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn javascript_can_be_disabled() {
    let config = Config {
        javascript_enabled: false,
        ..Config::default()
    };
    let browser = Browser::new(config);
    let page = browser
        .load_html(
            "https://js.test/",
            "<div id=app></div><script>document.getElementById('app').textContent='x';</script>",
        )
        .unwrap();
    let app = page.select("#app").unwrap().unwrap();
    assert_eq!(page.document.text_content(app), "");
}

#[tokio::test]
async fn script_style_changes_are_rendered() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r##"<style>body{margin:0} #box{width:50px;height:50px}</style>
<div id="box"></div>
<script>document.getElementById("box").style.backgroundColor = "#00ff00";</script>"##,
        )
        .unwrap();
    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: Viewport {
                    width: 100.0,
                    height: 100.0,
                },
                ..RenderOptions::default()
            },
        )
        .await
        .unwrap();
    let pixel = rendered.image.get_pixel(5, 5);
    assert_eq!([pixel[0], pixel[1], pixel[2]], [0, 255, 0]);
}

#[test]
fn web_apis_work_in_scripts() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://example.test/base",
            r#"<p id="out"></p><p id="fd"></p><script>
  const u = new URL("/a?x=1", "https://example.test/base");
  const params = u.searchParams;
  params.append("y", "2");
  const bytes = new TextEncoder().encode("hé");
  const decoded = new TextDecoder().decode(bytes);
  localStorage.setItem("k", "v");
  const blob = new Blob(["ab"], { type: "text/plain" });
  const form = new FormData();
  form.append("a", "1");
  document.getElementById("fd").textContent = form.get("a");
  blob.text().then(text => {
    document.getElementById("out").textContent = [
      u.pathname, params.get("x"), params.get("y"), decoded,
      localStorage.getItem("k"), text, screen.width, performance.now() >= 0
    ].join("|");
  });
</script>"#,
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "/a|1|2|hé|v|ab|1280|true");
    let fd = page.select("#fd").unwrap().unwrap();
    assert_eq!(page.document.text_content(fd), "1");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn event_capture_phases_and_on_handlers() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r##"<div id="outer"><button id="inner">go</button></div><p id="out"></p><p id="load"></p>
<script>
  const log = [];
  document.getElementById("outer").addEventListener("click", () => log.push("outer-bubble"));
  document.getElementById("outer").addEventListener("click", () => log.push("outer-capture"), true);
  document.getElementById("inner").addEventListener("click", () => log.push("inner"));
  document.getElementById("inner").click();
  document.getElementById("out").textContent = log.join(",");
  window.onload = () => {
    document.getElementById("load").textContent = "loaded";
  };
</script>"##,
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(
        page.document.text_content(out),
        "outer-capture,inner,outer-bubble"
    );
    let load = page.select("#load").unwrap().unwrap();
    assert_eq!(page.document.text_content(load), "loaded");
}

#[test]
fn stop_immediate_propagation_halts_listeners() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r##"<div id="outer"><button id="inner">go</button></div><p id="out"></p>
<script>
  const log = [];
  document.getElementById("outer").addEventListener("click", () => log.push("outer"));
  const inner = document.getElementById("inner");
  inner.addEventListener("click", event => { log.push("first"); event.stopImmediatePropagation(); });
  inner.addEventListener("click", () => log.push("second"));
  inner.click();
  document.getElementById("out").textContent = log.join(",");
</script>"##,
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "first");
}

#[test]
fn dynamic_scripts_and_document_write_execute() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r##"<p id="out"></p><script>
  document.write('<b id="written">w</b>');
  const s = document.createElement("script");
  s.textContent = "document.getElementById('out').textContent = 'dynamic';";
  document.body.appendChild(s);
</script>"##,
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "dynamic");
    let written = page.select("#written").unwrap().unwrap();
    assert_eq!(page.document.text_content(written), "w");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn mutation_observer_receives_records() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r##"<body><div id="app"></div><p id="out"></p><script>
  const seen = [];
  const observer = new MutationObserver(records => {
    for (const record of records) {
      seen.push(record.type + ":" + (record.addedNodes ? record.addedNodes.length : 0) + ":" + (record.attributeName || ""));
    }
    document.getElementById("out").textContent = seen.join("|");
  });
  const app = document.getElementById("app");
  observer.observe(app, { childList: true, attributes: true });
  const created = document.createElement("div");
  created.id = "created";
  app.appendChild(created);
  app.setAttribute("data-x", "1");
  app.textContent = "text";
</script></body>"##,
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    let text = page.document.text_content(out);
    assert!(text.contains("childList:1:"), "{text}");
    assert!(text.contains("attributes:0:data-x"), "{text}");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn inline_modules_execute() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r##"<p id="out"></p><script type="module">
  globalThis.__moduleValue = 40 + 2;
  document.getElementById("out").textContent = "module";
</script>"##,
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "module");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[tokio::test]
async fn external_modules_execute() {
    let addr = spawn_test_server().await;
    let browser = browser();
    let page = browser
        .open(&format!("http://{addr}/module-page"))
        .await
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "module-external");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[tokio::test]
async fn request_headers_and_abort_are_supported() {
    let addr = spawn_test_server().await;
    let browser = browser();
    let page = browser
        .open(&format!("http://{addr}/fetch-request"))
        .await
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "abc");
    let abort = page.select("#abort").unwrap().unwrap();
    assert_eq!(page.document.text_content(abort), "aborted");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn invalid_import_maps_are_ignored() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<script type="importmap">{not json</script>
<script type="module">import "missing";</script>"#,
        )
        .unwrap();
    assert!(
        page.script_errors
            .iter()
            .any(|error| error.contains("bare module specifier `missing`")),
        "{:?}",
        page.script_errors
    );
}

fn data_module(source: &str) -> String {
    let mut url = String::from("data:text/javascript,");
    for byte in source.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

#[test]
fn import_map_resolves_static_bare_specifiers() {
    let browser = browser();
    let helper = data_module("export const answer = 40;");
    let prefix = data_module("export const value = 4");
    let page = browser
        .load_html(
            "https://js.test/app/",
            &format!(
                r#"<p id="out"></p>
<script type="importmap">{{"imports":{{"lib/answer":"{helper}","pkg/":"{prefix}"}}}}</script>
<script type="module">
  import {{ answer }} from "lib/answer";
  import {{ value }} from "pkg/2";
  document.getElementById("out").textContent = String(answer + 2) + ":" + String(value);
</script>"#
            ),
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "42:42");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn import_map_resolves_dynamic_imports() {
    let browser = browser();
    let helper = data_module("export const value = 21;");
    let page = browser
        .load_html(
            "https://js.test/",
            &format!(
                r#"<p id="out"></p>
<script type="importmap">{{"imports":{{"dyn":"{helper}"}}}}</script>
<script>
  import("dyn").then(function (module) {{
    document.getElementById("out").textContent = String(module.value * 2);
  }});
</script>"#
            ),
        )
        .unwrap();
    let out = page.select("#out").unwrap().unwrap();
    assert_eq!(page.document.text_content(out), "42");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}

#[test]
fn node_types_and_parent_element_are_exposed() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="box"><span id="inner">text</span><!--note--></div></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    for (script, expected) in [
        ("document.nodeType", 9.0),
        ("document.body.nodeType", 1.0),
        ("document.getElementById('inner').firstChild.nodeType", 3.0),
        ("document.getElementById('box').lastChild.nodeType", 8.0),
        ("document.createDocumentFragment().nodeType", 11.0),
    ] {
        assert_eq!(
            session.evaluate(script).unwrap().as_f64(),
            Some(expected),
            "{script}"
        );
    }
    assert_eq!(
        session
            .evaluate("document.getElementById('inner').parentElement.id")
            .unwrap()
            .as_str(),
        Some("box")
    );
    assert_eq!(
        session
            .evaluate("document.getElementById('inner').firstChild.parentElement.id")
            .unwrap()
            .as_str(),
        Some("inner")
    );
    assert_eq!(
        session
            .evaluate("document.documentElement.parentElement")
            .unwrap(),
        serde_json::Value::Null
    );
}

#[test]
fn get_elements_by_class_name_requires_all_classes() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="a" class="alpha beta"></div><div id="b" class="alpha"></div><div id="c" class="beta alpha gamma"></div></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    assert_eq!(
        session
            .evaluate(
                "Array.from(document.getElementsByClassName('alpha beta')).map(function (el) { return el.id; }).join(',')"
            )
            .unwrap()
            .as_str(),
        Some("a,c")
    );
    assert_eq!(
        session
            .evaluate("document.body.getElementsByClassName('alpha').length")
            .unwrap()
            .as_f64(),
        Some(3.0)
    );
    assert_eq!(
        session
            .evaluate("document.getElementsByClassName('missing').length")
            .unwrap()
            .as_f64(),
        Some(0.0)
    );
}

#[test]
fn matches_and_closest_use_the_selector_engine() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="outer" class="wrap"><section id="mid" class="panel"><span id="leaf" class="item">x</span></section></div></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    for (script, expected) in [
        (
            "document.getElementById('leaf').matches('.panel .item')",
            true,
        ),
        (
            "document.getElementById('leaf').matches('section > span.item')",
            true,
        ),
        (
            "document.getElementById('leaf').matches('div > span')",
            false,
        ),
    ] {
        assert_eq!(
            session.evaluate(script).unwrap().as_bool(),
            Some(expected),
            "{script}"
        );
    }
    assert_eq!(
        session
            .evaluate("document.getElementById('leaf').closest('.wrap').id")
            .unwrap()
            .as_str(),
        Some("outer")
    );
    assert_eq!(
        session
            .evaluate("document.getElementById('leaf').closest('span').id")
            .unwrap()
            .as_str(),
        Some("leaf")
    );
    assert_eq!(
        session
            .evaluate("document.getElementById('leaf').closest('.missing')")
            .unwrap(),
        serde_json::Value::Null
    );
}

#[test]
fn clone_node_shallow_and_deep_are_isolated() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="src" data-kind="original"><span class="child">original</span></div></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    let value = session
        .evaluate(
            r#"(function () {
  var source = document.getElementById('src');
  var shallow = source.cloneNode(false);
  shallow.id = 'shallow';
  var deep = source.cloneNode(true);
  deep.id = 'deep';
  deep.querySelector('.child').textContent = 'changed';
  document.body.appendChild(shallow);
  document.body.appendChild(deep);
  return {
    shallowAttr: shallow.getAttribute('data-kind'),
    shallowChildren: shallow.childNodes.length,
    deepAttr: deep.getAttribute('data-kind'),
    deepText: deep.textContent,
    originalText: source.textContent,
    isolated: deep.querySelector('.child') !== source.querySelector('.child'),
    shallowId: shallow.id
  };
})()"#,
        )
        .unwrap();
    assert_eq!(value["shallowAttr"].as_str(), Some("original"));
    assert_eq!(value["shallowChildren"].as_f64(), Some(0.0));
    assert_eq!(value["deepAttr"].as_str(), Some("original"));
    assert_eq!(value["deepText"].as_str(), Some("changed"));
    assert_eq!(value["originalText"].as_str(), Some("original"));
    assert_eq!(value["isolated"].as_bool(), Some(true));
    assert_eq!(value["shallowId"].as_str(), Some("shallow"));
}

#[test]
fn replace_child_returns_old_node_and_updates_children() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><ul id="list"><li id="old">old</li><li id="tail">tail</li></ul></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    let value = session
        .evaluate(
            r#"(function () {
  var list = document.getElementById('list');
  var old = document.getElementById('old');
  var fresh = document.createElement('li');
  fresh.id = 'fresh';
  fresh.textContent = 'fresh';
  var returned = list.replaceChild(fresh, old);
  return {
    returned: returned.id,
    oldGone: document.getElementById('old') === null,
    order: Array.from(list.children).map(function (child) { return child.id; }).join(','),
    text: list.textContent
  };
})()"#,
        )
        .unwrap();
    assert_eq!(value["returned"].as_str(), Some("old"));
    assert_eq!(value["oldGone"].as_bool(), Some(true));
    assert_eq!(value["order"].as_str(), Some("fresh,tail"));
    assert_eq!(value["text"].as_str(), Some("freshtail"));
}

#[test]
fn document_fragments_move_their_children() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="host"><i id="anchor">a</i></div></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    let value = session
        .evaluate(
            r#"(function () {
  var fragment = document.createDocumentFragment();
  var one = document.createElement('b');
  one.id = 'one';
  one.textContent = '1';
  var two = document.createElement('b');
  two.id = 'two';
  two.textContent = '2';
  fragment.appendChild(one);
  fragment.appendChild(two);
  var host = document.getElementById('host');
  host.insertBefore(fragment, host.firstChild);
  var tail = document.createDocumentFragment();
  var three = document.createElement('b');
  three.id = 'three';
  three.textContent = '3';
  tail.appendChild(three);
  host.appendChild(tail);
  return {
    order: Array.from(host.children).map(function (child) { return child.id; }).join(','),
    leftover: fragment.childNodes.length + tail.childNodes.length,
    text: host.textContent
  };
})()"#,
        )
        .unwrap();
    assert_eq!(value["order"].as_str(), Some("one,two,anchor,three"));
    assert_eq!(value["leftover"].as_f64(), Some(0.0));
    assert_eq!(value["text"].as_str(), Some("12a3"));
}

#[test]
fn create_element_ns_builds_foreign_elements() {
    let browser = browser();
    let page = browser
        .load_html("https://js.test/", "<body></body>")
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    let value = session
        .evaluate(
            r#"(function () {
  var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.id = 'icon';
  svg.setAttribute('viewBox', '0 0 10 10');
  document.body.appendChild(svg);
  var html = document.createElementNS(null, 'div');
  var unknown;
  try {
    document.createElementNS('urn:unknown', 'x');
    unknown = 'created';
  } catch (error) {
    unknown = 'threw';
  }
  return { tag: svg.tagName, htmlTag: html.tagName, outer: svg.outerHTML, unknown: unknown };
})()"#,
        )
        .unwrap();
    assert_eq!(value["tag"].as_str(), Some("svg"));
    assert_eq!(value["htmlTag"].as_str(), Some("DIV"));
    assert!(
        value["outer"].as_str().unwrap_or("").contains("<svg"),
        "{value}"
    );
    assert!(
        value["outer"]
            .as_str()
            .unwrap_or("")
            .contains("viewBox=\"0 0 10 10\""),
        "{value}"
    );
    assert_eq!(value["unknown"].as_str(), Some("threw"));
    let icon = session.page().select("#icon").unwrap().unwrap();
    assert_eq!(
        session.page().document.element(icon).unwrap().namespace,
        nexium::engine::dom::Namespace::Svg
    );
    assert!(session.page().script_errors.is_empty());
}

#[test]
fn insert_adjacent_text_supports_all_positions() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="host"><span id="target">T</span></div></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    let value = session
        .evaluate(
            r#"(function () {
  var target = document.getElementById('target');
  target.insertAdjacentText('beforebegin', 'B');
  target.insertAdjacentText('afterbegin', 'A');
  target.insertAdjacentText('beforeend', 'C');
  target.insertAdjacentText('afterend', 'D');
  var host = document.getElementById('host');
  return {
    text: host.textContent,
    hostNodes: host.childNodes.length,
    spanNodes: target.childNodes.length
  };
})()"#,
        )
        .unwrap();
    assert_eq!(value["text"].as_str(), Some("BATCD"));
    assert_eq!(value["hostNodes"].as_f64(), Some(3.0));
    assert_eq!(value["spanNodes"].as_f64(), Some(3.0));
}

#[test]
fn outer_html_setter_replaces_the_element() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body><div id="wrap"><span id="old">old</span></div></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    let value = session
        .evaluate(
            r#"(function () {
  var old = document.getElementById('old');
  old.outerHTML = '<b id="replacement">new</b>';
  var detached = document.createElement('div');
  detached.outerHTML = '<p>ignored</p>';
  return {
    oldGone: document.getElementById('old') === null,
    replacement: document.getElementById('replacement').textContent,
    wrap: document.getElementById('wrap').textContent,
    detachedNodes: detached.childNodes.length,
    detachedHtml: detached.outerHTML
  };
})()"#,
        )
        .unwrap();
    assert_eq!(value["oldGone"].as_bool(), Some(true));
    assert_eq!(value["replacement"].as_str(), Some("new"));
    assert_eq!(value["wrap"].as_str(), Some("new"));
    assert_eq!(value["detachedNodes"].as_f64(), Some(0.0));
    assert_eq!(value["detachedHtml"].as_str(), Some("<div></div>"));
}

#[test]
fn dataset_reads_writes_deletes_and_serializes_to_null() {
    let browser = browser();
    let page = browser
        .load_html(
            "https://js.test/",
            r#"<body data-user-id="7" data-role="main"></body>"#,
        )
        .unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let mut session = nexium::PageSession::new(
        page,
        Viewport {
            width: 640.0,
            height: 480.0,
        },
    );
    assert_eq!(
        session
            .evaluate("document.body.dataset.userId")
            .unwrap()
            .as_str(),
        Some("7")
    );
    assert_eq!(
        session
            .evaluate("'role' in document.body.dataset")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert_eq!(
        session
            .evaluate(
                "document.body.dataset.role = 'admin'; document.body.getAttribute('data-role')"
            )
            .unwrap()
            .as_str(),
        Some("admin")
    );
    assert_eq!(
        session
            .evaluate("delete document.body.dataset.role; document.body.hasAttribute('data-role')")
            .unwrap()
            .as_bool(),
        Some(false)
    );
    assert_eq!(
        session
            .evaluate("Object.keys(document.body.dataset).includes('userId')")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert_eq!(
        session.evaluate("document.body.dataset.missing").unwrap(),
        serde_json::Value::Null
    );
    assert_eq!(
        session.evaluate("document.body.dataset").unwrap(),
        serde_json::Value::Null
    );
    assert_eq!(session.evaluate("1 + 1").unwrap().as_f64(), Some(2.0));
    assert!(session.page().script_errors.is_empty());
}

#[test]
fn module_workers_import_helpers_and_post_results() {
    let browser = browser();
    let helper = data_module("export const double = (value) => value * 2;");
    let worker = data_module(&format!(
        "import {{ double }} from {helper:?}; self.onmessage = (event) => postMessage({{ value: double(event.data.value) }});"
    ));
    let page = browser
        .load_html(
            "https://js.test/",
            &format!(
                r#"<body data-worker=""><script>
  var worker = new Worker('{worker}', {{ type: 'module' }});
  worker.onmessage = function (event) {{
    document.body.setAttribute('data-worker', String(event.data.value));
    clearInterval(timer);
  }};
  worker.postMessage({{ value: 21 }});
  var tries = 0;
  var timer = setInterval(function () {{ if (++tries > 200) clearInterval(timer); }}, 10);
</script></body>"#
            ),
        )
        .unwrap();
    let body = page.select("body").unwrap().unwrap();
    assert_eq!(page.document.get_attribute(body, "data-worker"), Some("42"));
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
}
