//! Phase 5 gap coverage: `:hover` CSS, trusted synthetic events, tab focus
//! traversal, link/form navigation and contenteditable editing.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use nexium::engine::css::Viewport;
use nexium::{Browser, Config, PageSession};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const VIEWPORT: Viewport = Viewport {
    width: 800.0,
    height: 600.0,
};

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    target: String,
    body: String,
}

fn session(page: nexium::Page) -> PageSession {
    PageSession::new(page, VIEWPORT)
}

fn attribute(session: &PageSession, selector: &str, name: &str) -> Option<String> {
    let node = session.page().select(selector).unwrap().unwrap();
    session
        .page()
        .document
        .get_attribute(node, name)
        .map(str::to_owned)
}

/// Spawn a minimal HTTP server that records the request line and body.
async fn spawn_recording_server() -> (SocketAddr, Arc<Mutex<Vec<Recorded>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let entries = Arc::clone(&log);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let entries = Arc::clone(&entries);
            tokio::spawn(handle(stream, entries));
        }
    });
    (addr, log)
}

async fn handle(mut stream: TcpStream, log: Arc<Mutex<Vec<Recorded>>>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut chunk).await {
            Ok(0) => return,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break position;
                }
                if buffer.len() > 128 * 1024 {
                    return;
                }
            }
            Err(_) => return,
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("").to_owned();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    let content_length = lines
        .clone()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    log.lock().unwrap().push(Recorded {
        method,
        target,
        body: String::from_utf8_lossy(&body).to_string(),
    });
    let page = b"<html><body><h1 id=\"result\">submitted</h1></body></html>".to_vec();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&page).await;
    let _ = stream.shutdown().await;
}

#[tokio::test]
async fn hover_applies_hover_style_and_survives_mousemove() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/hover",
            r#"<html><head><style>
  body { margin: 0; }
  #box { position: absolute; left: 10px; top: 10px; width: 100px; height: 100px; background-color: #00ff00; }
  #box:hover { background-color: #ff0000; }
</style></head><body>
  <div id="box">box</div>
  <div id="out"></div>
  <script>
    var out = document.getElementById('out');
    var over = 0, moves = 0;
    var box = document.getElementById('box');
    box.addEventListener('mouseover', function () { over += 1; out.setAttribute('data-over', String(over)); });
    box.addEventListener('mousemove', function () { moves += 1; out.setAttribute('data-move', String(moves)); });
  </script>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);
    let node = session.page().select("#box").unwrap().unwrap();

    let before = session.layout().style(node).unwrap().background_color;
    assert_eq!((before.r, before.g, before.b), (0, 255, 0));

    session.hover(50.0, 50.0);
    assert_eq!(session.hovered_node(), Some(node));
    let after = session.layout().style(node).unwrap().background_color;
    assert_eq!(
        (after.r, after.g, after.b),
        (255, 0, 0),
        ":hover rule must apply after relayout"
    );

    // Moving inside the same element must not drop the hover state and must
    // not fire another mouseover.
    session.hover(60.0, 60.0);
    let after_move = session.layout().style(node).unwrap().background_color;
    assert_eq!((after_move.r, after_move.g, after_move.b), (255, 0, 0));
    assert_eq!(
        attribute(&session, "#out", "data-over").as_deref(),
        Some("1")
    );
    assert_eq!(
        attribute(&session, "#out", "data-move").as_deref(),
        Some("2")
    );

    session.clear_hover();
    assert_eq!(session.hovered_node(), None);
    let cleared = session.layout().style(node).unwrap().background_color;
    assert_eq!((cleared.r, cleared.g, cleared.b), (0, 255, 0));
}

#[tokio::test]
async fn host_events_are_trusted_and_script_events_are_not() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/trust",
            r#"<html><head><style>
  body { margin: 0; }
  #b { position: absolute; left: 10px; top: 10px; width: 100px; height: 40px; }
  #i { position: absolute; left: 10px; top: 60px; width: 200px; height: 30px; }
  #out { position: absolute; left: 10px; top: 120px; }
</style></head><body>
  <button id="b">Click</button>
  <input id="i">
  <div id="out"></div>
  <script>
    var out = document.getElementById('out');
    document.getElementById('b').addEventListener('click', function (e) {
      out.setAttribute('data-click', String(e.isTrusted));
    });
    document.getElementById('i').addEventListener('focus', function (e) {
      out.setAttribute('data-focus', String(e.isTrusted));
    });
    document.getElementById('i').addEventListener('keydown', function (e) {
      out.setAttribute('data-key', String(e.isTrusted) + ':' + e.key);
    });
    document.getElementById('i').addEventListener('input', function (e) {
      out.setAttribute('data-input', String(e.isTrusted));
    });
    document.addEventListener('wheel', function (e) {
      out.setAttribute('data-wheel', String(e.isTrusted));
    });
    var synthetic = document.createElement('button');
    synthetic.addEventListener('click', function (e) {
      out.setAttribute('data-synthetic', String(e.isTrusted));
    });
    synthetic.click();
  </script>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);

    session.click(50.0, 25.0);
    assert_eq!(
        attribute(&session, "#out", "data-click").as_deref(),
        Some("true")
    );
    assert_eq!(
        attribute(&session, "#out", "data-synthetic").as_deref(),
        Some("false"),
        "script created events stay untrusted"
    );

    session.click(50.0, 70.0);
    assert_eq!(
        attribute(&session, "#out", "data-focus").as_deref(),
        Some("true")
    );

    session.type_text("a");
    assert_eq!(
        attribute(&session, "#out", "data-input").as_deref(),
        Some("true")
    );
    assert_eq!(
        attribute(&session, "#out", "data-key").as_deref(),
        Some("true:a")
    );

    session.wheel(50.0, 70.0, 0.0, 10.0);
    assert_eq!(
        attribute(&session, "#out", "data-wheel").as_deref(),
        Some("true")
    );
}

#[tokio::test]
async fn tab_moves_focus_in_order_and_skips_hidden_controls() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/tab",
            r##"<html><head><style>
  body { margin: 0; }
  a, button, input { display: block; height: 20px; }
</style></head><body>
  <a id="l1" href="#">one</a>
  <button id="b1">b1</button>
  <button id="b2" disabled>b2</button>
  <input id="i1">
  <div id="hidden" style="display: none"><button id="bh">hidden</button></div>
  <div id="h2" style="visibility: hidden"><input id="ih"></div>
  <button id="b3" tabindex="2">t2</button>
  <button id="b4" tabindex="1">t1</button>
  <div id="out"></div>
  <script>
    var out = document.getElementById('out');
    document.addEventListener('focusin', function (e) {
      var seq = out.getAttribute('data-seq') || '';
      out.setAttribute('data-seq', seq + (e.target.id || e.target.tagName) + ',');
      out.setAttribute('data-trusted', String(e.isTrusted));
    });
  </script>
</body></html>"##,
        )
        .unwrap();
    let mut session = session(page);

    let expected = ["b4", "b3", "l1", "b1", "i1"];
    for id in expected {
        let node = session.focus_next().expect("focusable element");
        let info = session.element_info(node).expect("focused geometry");
        assert_eq!(info.id.as_deref(), Some(id));
        assert_eq!(session.focused_node(), Some(node));
    }
    assert_eq!(
        attribute(&session, "#out", "data-seq").as_deref(),
        Some("b4,b3,l1,b1,i1,")
    );
    assert_eq!(
        attribute(&session, "#out", "data-trusted").as_deref(),
        Some("true")
    );

    // Shift+Tab walks backwards through the same order.
    session.press_key("Shift+Tab");
    let previous = session.focused_node().unwrap();
    assert_eq!(
        session.element_info(previous).unwrap().id.as_deref(),
        Some("b1")
    );

    session.press_key("Tab");
    let next = session.focused_node().unwrap();
    assert_eq!(
        session.element_info(next).unwrap().id.as_deref(),
        Some("i1")
    );
}

#[tokio::test]
async fn clicking_submit_button_navigates_with_a_get_form() {
    let (addr, log) = spawn_recording_server().await;
    let browser = Browser::new(Config::default());
    let html = format!(
        r#"<html><head><style>body {{ margin: 0; }}</style></head><body>
  <form id="f" action="http://{addr}/submit" method="get">
    <input id="name" name="name" value="Alice">
    <input type="hidden" name="token" value="t1">
    <input name="skip" value="no" disabled>
    <select name="color"><option value="red">Red</option><option value="blue" selected>Blue</option></select>
    <textarea name="msg">hi</textarea>
    <button id="go" type="submit" name="btn" value="OK">Go</button>
  </form>
</body></html>"#
    );
    let page = browser
        .load_html(&format!("http://{addr}/form"), &html)
        .unwrap();
    let mut session = session(page);
    let button = session.page().select("#go").unwrap().unwrap();
    let info = session.element_info(button).expect("button geometry");
    let x = info.rect.x + info.rect.width / 2.0;
    let y = info.rect.y + info.rect.height / 2.0;

    let result = session.click(x, y);
    assert!(!result.prevented);
    let navigation = session.take_navigation().expect("pending navigation");
    assert_eq!(navigation.method, "GET");
    assert!(navigation.url.contains("name=Alice"), "{}", navigation.url);
    assert!(navigation.url.contains("token=t1"), "{}", navigation.url);
    assert!(navigation.url.contains("color=blue"), "{}", navigation.url);
    assert!(navigation.url.contains("msg=hi"), "{}", navigation.url);
    assert!(navigation.url.contains("btn=OK"), "{}", navigation.url);
    assert!(!navigation.url.contains("skip="), "{}", navigation.url);
    assert_eq!(navigation.body, None);

    let loaded = session
        .navigate_to(browser.client(), &navigation)
        .await
        .unwrap();
    assert_eq!(loaded.url.path(), "/submit");
    let recorded = log.lock().unwrap();
    let entry = recorded.last().expect("recorded request");
    assert_eq!(entry.method, "GET");
    assert!(
        entry.target.starts_with("/submit?name=Alice"),
        "{}",
        entry.target
    );
    assert!(entry.target.contains("color=blue"), "{}", entry.target);
    assert!(entry.body.is_empty());
}

#[tokio::test]
async fn enter_in_text_input_submits_the_form_with_post() {
    let (addr, log) = spawn_recording_server().await;
    let browser = Browser::new(Config::default());
    let html = format!(
        r#"<html><head><style>body {{ margin: 0; }}</style></head><body>
  <form id="f" action="http://{addr}/submit" method="post">
    <input id="name" name="name" value="Alice">
    <input type="submit" value="Go">
  </form>
</body></html>"#
    );
    let page = browser
        .load_html(&format!("http://{addr}/form"), &html)
        .unwrap();
    let mut session = session(page);
    let input = session.page().select("#name").unwrap().unwrap();
    let info = session.element_info(input).expect("input geometry");
    session.click(info.rect.x + 5.0, info.rect.y + 5.0);
    assert_eq!(session.focused_node(), Some(input));

    session.press_key("Enter");
    let navigation = session.take_navigation().expect("pending navigation");
    assert_eq!(navigation.method, "POST");
    assert_eq!(navigation.body.as_deref(), Some("name=Alice"));

    session
        .navigate_to(browser.client(), &navigation)
        .await
        .unwrap();
    let recorded = log.lock().unwrap();
    let entry = recorded.last().expect("recorded request");
    assert_eq!(entry.method, "POST");
    assert_eq!(entry.target, "/submit");
    assert_eq!(entry.body, "name=Alice");
}

#[tokio::test]
async fn contenteditable_typing_backspace_and_selection() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/editable",
            r#"<html><head><style>
  body { margin: 0; }
  #ce { position: absolute; left: 10px; top: 10px; width: 200px; height: 40px; }
  #out { position: absolute; left: 10px; top: 80px; }
</style></head><body>
  <div id="ce" contenteditable="true">start</div>
  <div id="out"></div>
  <script>
    var out = document.getElementById('out');
    var ce = document.getElementById('ce');
    ce.addEventListener('beforeinput', function (e) {
      out.setAttribute('data-before', String(e.isTrusted));
    });
    ce.addEventListener('input', function () {
      out.setAttribute('data-input', ce.textContent);
      out.setAttribute('data-input-trusted', String(arguments[0] ? arguments[0].isTrusted : 'missing'));
    });
    ce.addEventListener('select', function (e) {
      out.setAttribute('data-select', String(e.isTrusted));
    });
  </script>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);
    let ce = session.page().select("#ce").unwrap().unwrap();
    let text = |session: &PageSession| {
        session
            .page()
            .document
            .text_content(session.page().select("#ce").unwrap().unwrap())
    };

    session.click(50.0, 20.0);
    assert_eq!(session.focused_node(), Some(ce));
    assert_eq!(text(&session), "start");

    session.type_text("hi");
    assert_eq!(text(&session), "starthi");
    assert_eq!(
        attribute(&session, "#out", "data-before").as_deref(),
        Some("true")
    );
    assert_eq!(
        attribute(&session, "#out", "data-input").as_deref(),
        Some("starthi")
    );

    session.press_key("Backspace");
    assert_eq!(text(&session), "starth");
    assert_eq!(
        attribute(&session, "#out", "data-input").as_deref(),
        Some("starth")
    );
    assert_eq!(
        attribute(&session, "#out", "data-before").as_deref(),
        Some("true")
    );

    let selection = session
        .select_text(0, 3)
        .expect("selection over focused node");
    assert_eq!(selection.start, 0);
    assert_eq!(selection.end, 3);
    assert_eq!(session.selection(), Some(selection));
    assert_eq!(
        attribute(&session, "#out", "data-select").as_deref(),
        Some("true")
    );
}

#[tokio::test]
async fn clicking_link_records_navigation_and_follow_link_loads_it() {
    let (addr, log) = spawn_recording_server().await;
    let browser = Browser::new(Config::default());
    let html = format!(
        r#"<html><head><style>
  body {{ margin: 0; }}
  #l {{ position: absolute; left: 10px; top: 10px; width: 120px; height: 20px; }}
</style></head><body>
  <a id="l" href="http://{addr}/next">next</a>
</body></html>"#
    );
    let page = browser
        .load_html(&format!("http://{addr}/links"), &html)
        .unwrap();
    let mut session = session(page);
    let link = session.page().select("#l").unwrap().unwrap();
    let info = session.element_info(link).expect("link geometry");
    let result = session.click(info.rect.x + 5.0, info.rect.y + 5.0);
    let expected = format!("http://{addr}/next");
    assert_eq!(result.href.as_deref(), Some(expected.as_str()));

    let navigation = session.take_navigation().expect("link navigation");
    assert_eq!(navigation.method, "GET");
    assert_eq!(navigation.url, expected);

    let loaded = session
        .follow_link(browser.client(), &navigation.url)
        .await
        .unwrap();
    assert_eq!(loaded.url.path(), "/next");
    let recorded = log.lock().unwrap();
    assert_eq!(recorded.last().expect("recorded request").target, "/next");
}

#[tokio::test]
async fn prevented_submit_does_not_record_a_navigation() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/prevent",
            r#"<html><head><style>body { margin: 0; }</style></head><body>
  <form id="f" action="http://test.local/submit">
    <input id="name" name="name" value="Alice">
    <button id="go" type="submit">Go</button>
  </form>
  <script>
    document.getElementById('f').addEventListener('submit', function (event) {
      event.preventDefault();
      document.body.setAttribute('data-submitted', 'prevented');
    });
  </script>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);
    let button = session.page().select("#go").unwrap().unwrap();
    let info = session.element_info(button).expect("button geometry");
    session.click(info.rect.x + 5.0, info.rect.y + 5.0);

    assert!(session.pending_navigation().is_none());
    assert!(session.take_navigation().is_none());
    let body = session.page().select("body").unwrap().unwrap();
    assert_eq!(
        session
            .page()
            .document
            .get_attribute(body, "data-submitted"),
        Some("prevented")
    );
}

#[tokio::test]
async fn inline_on_click_attribute_becomes_a_listener() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/inline-handler",
            r#"<html><body style="margin:0">
  <button id="btn"
    onclick="document.getElementById('out').textContent='clicked'; this.setAttribute('data-fired','1')">Go</button>
  <div id="out">idle</div>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);
    let button = session.page().select("#btn").unwrap().unwrap();
    let info = session.element_info(button).expect("button geometry");
    session.click(info.rect.x + 5.0, info.rect.y + 5.0);

    let out = session.page().select("#out").unwrap().unwrap();
    assert_eq!(session.page().document.text_content(out), "clicked");
    assert_eq!(
        attribute(&session, "#btn", "data-fired").as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn hover_state_survives_a_new_page_session() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/hover-persist",
            r#"<html><head><style>
  body { margin: 0; }
  #box { position: absolute; left: 10px; top: 10px; width: 100px; height: 100px; background-color: #00ff00; }
  #box:hover { background-color: #ff0000; }
</style></head><body>
  <div id="box"></div>
  <script>window.ready = true;</script>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);
    session.hover(50.0, 50.0);

    // Recreating the session mimics separate API calls; the runtime snapshot
    // must not drop the pointer hover target.
    let page = session.into_page();
    let next = PageSession::new(page, VIEWPORT);
    let node = next.page().select("#box").unwrap().unwrap();
    assert_eq!(next.hovered_node(), Some(node));
    let style = next.layout().style(node).expect("box style");
    assert_eq!(
        (
            style.background_color.r,
            style.background_color.g,
            style.background_color.b
        ),
        (255, 0, 0),
        ":hover must survive session recreation"
    );
}

#[tokio::test]
async fn layout_geometry_and_document_cookie_are_exposed() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/geometry",
            r#"<html><body style="margin:0">
  <div id="box" style="width:300px;height:50px;background:#000">x</div>
  <script>document.cookie = 'tok=1; path=/'</script>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);

    let rect = session
        .evaluate("JSON.stringify(document.getElementById('box').getBoundingClientRect())")
        .unwrap();
    let rect: serde_json::Value = serde_json::from_str(rect.as_str().unwrap()).unwrap();
    assert_eq!(rect["width"], 300.0);
    assert_eq!(rect["height"], 50.0);
    assert_eq!(rect["right"], rect["left"].as_f64().unwrap() + 300.0);

    // Offline pages run without an HTTP client, so cookie storage is a no-op;
    // fetched pages read the network jar (covered by the live API tests).
    let cookie = session.evaluate("document.cookie").unwrap();
    assert!(cookie.is_string(), "document.cookie should be a string");

    let display = session
        .evaluate("getComputedStyle(document.getElementById('box')).display")
        .unwrap();
    assert_eq!(display.as_str(), Some("block"));
    let width = session
        .evaluate("getComputedStyle(document.getElementById('box')).width")
        .unwrap();
    assert_eq!(width.as_str(), Some("300px"));
    let background = session
        .evaluate(
            "getComputedStyle(document.getElementById('box')).getPropertyValue('background-color')",
        )
        .unwrap();
    assert_eq!(background.as_str(), Some("rgb(0, 0, 0)"));
}
