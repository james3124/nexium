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
