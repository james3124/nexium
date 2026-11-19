//! Regression tests for the remaining JavaScript Web API gaps: NodeList-style
//! collections, CSSStyleDeclaration and FormData multipart uploads.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use nexium::engine::css::Viewport;
use nexium::{Browser, Config, PageSession};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn browser() -> Browser {
    Browser::new(Config::default())
}

fn session_for(html: &str) -> PageSession {
    let page = browser()
        .load_html("https://js.test/", html)
        .expect("page must load");
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    PageSession::new(
        page,
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    )
}

fn eval(session: &mut PageSession, code: &str) -> serde_json::Value {
    session
        .evaluate(code)
        .unwrap_or_else(|error| panic!("evaluate({code}) failed: {error}"))
}

fn eval_str(session: &mut PageSession, code: &str) -> String {
    eval(session, code)
        .as_str()
        .unwrap_or_else(|| panic!("expected string for {code}"))
        .to_owned()
}

// ---------------------------------------------------------------------------
// NodeList / HTMLCollection
// ---------------------------------------------------------------------------

#[test]
fn nodelist_query_selector_all_length_index_and_item() {
    let mut session = session_for(
        r#"<body><ul id="list">
             <li class="x">a</li><li>b</li><li class="x">c</li>
           </ul>
           <script>window.items = document.querySelectorAll('#list li');</script>
         </body>"#,
    );
    assert_eq!(eval(&mut session, "items.length"), 3.0);
    assert_eq!(eval_str(&mut session, "items[0].textContent"), "a");
    assert_eq!(eval_str(&mut session, "items[2].textContent"), "c");
    assert!(
        eval(&mut session, "items[3] === undefined")
            .as_bool()
            .unwrap()
    );
    assert_eq!(eval_str(&mut session, "items.item(1).textContent"), "b");
    assert!(
        eval(&mut session, "items.item(99) === null")
            .as_bool()
            .unwrap()
    );
    assert!(
        eval(&mut session, "items.item(-1) === null")
            .as_bool()
            .unwrap()
    );
    assert!(
        eval(&mut session, "items.item(NaN) === null")
            .as_bool()
            .unwrap()
    );
    assert!(
        eval(&mut session, "items.item() === null")
            .as_bool()
            .unwrap()
    );
}

#[test]
fn nodelist_foreach_iteration_and_this_arg() {
    let mut session =
        session_for(r#"<body><ul id="list"><li>a</li><li>b</li><li>c</li></ul></body>"#);
    let result = eval_str(
        &mut session,
        r#"(function () {
             var nodes = document.querySelectorAll('#list li');
             var out = [];
             var receiver = { tag: 'R' };
             nodes.forEach(function (node, index, list) {
               out.push(this.tag + ':' + index + ':' + node.textContent + ':' + (list === nodes));
             }, receiver);
             return out.join('|');
           })()"#,
    );
    assert_eq!(result, "R:0:a:true|R:1:b:true|R:2:c:true");
}

#[test]
fn nodelist_keys_values_entries_and_iterable() {
    let mut session = session_for(r#"<body><ul id="list"><li>a</li><li>b</li></ul></body>"#);
    assert_eq!(
        eval_str(
            &mut session,
            "Array.from(document.querySelectorAll('#list li').keys()).join(',')"
        ),
        "0,1"
    );
    assert_eq!(
        eval_str(
            &mut session,
            "Array.from(document.querySelectorAll('#list li').values()).map(function (n) { return n.textContent; }).join(',')"
        ),
        "a,b"
    );
    assert_eq!(
        eval_str(
            &mut session,
            "Array.from(document.querySelectorAll('#list li').entries()).map(function (e) { return e[0] + '=' + e[1].textContent; }).join(',')"
        ),
        "0=a,1=b"
    );
    // for...of, spread and Array.from all rely on Symbol.iterator.
    assert_eq!(
        eval_str(
            &mut session,
            r#"(function () {
                 var out = [];
                 for (var node of document.querySelectorAll('#list li')) out.push(node.textContent);
                 return out.join('');
               })()"#
        ),
        "ab"
    );
    assert_eq!(
        eval(
            &mut session,
            "Array.from(document.querySelectorAll('#list li')).length"
        ),
        2.0
    );
}

#[test]
fn nodelist_static_and_live_collections() {
    let mut session = session_for(
        r#"<body><div id="host"><span class="c">one</span></div>
           <script>
             window.static = document.querySelectorAll('#host .c');
             window.liveNodes = document.getElementById('host').childNodes;
             window.liveChildren = document.getElementById('host').children;
           </script></body>"#,
    );
    assert_eq!(eval(&mut session, "static.length"), 1.0);
    assert_eq!(eval(&mut session, "liveNodes.length"), 1.0);
    assert_eq!(eval(&mut session, "liveChildren.length"), 1.0);

    eval(
        &mut session,
        r#"(function () {
             var added = document.createElement('span');
             added.className = 'c';
             added.textContent = 'two';
             document.getElementById('host').appendChild(added);
           })()"#,
    );

    // querySelectorAll is static.
    assert_eq!(eval(&mut session, "static.length"), 1.0);
    // childNodes / children are live.
    assert_eq!(eval(&mut session, "liveNodes.length"), 2.0);
    assert_eq!(eval(&mut session, "liveChildren.length"), 2.0);
    assert_eq!(
        eval_str(&mut session, "liveNodes.item(1).textContent"),
        "two"
    );
}

#[test]
fn html_collection_methods_and_liveness() {
    let mut session = session_for(
        r#"<body>
             <div id="a" class="grp alpha">a</div>
             <div id="b" class="grp">b</div>
             <script>
               window.byClass = document.getElementsByClassName('grp');
               window.byTag = document.getElementsByTagName('div');
             </script>
           </body>"#,
    );
    assert_eq!(eval(&mut session, "byClass.length"), 2.0);
    assert_eq!(eval(&mut session, "byTag.length"), 2.0);
    assert_eq!(eval_str(&mut session, "byClass.item(0).id"), "a");
    assert_eq!(
        eval_str(
            &mut session,
            "Array.from(byTag).map(function (el) { return el.id; }).join(',')"
        ),
        "a,b"
    );
    eval(
        &mut session,
        r#"(function () {
             var el = document.createElement('div');
             el.className = 'grp';
             el.id = 'c';
             document.body.appendChild(el);
           })()"#,
    );
    assert_eq!(eval(&mut session, "byClass.length"), 3.0);
    assert_eq!(eval(&mut session, "byTag.length"), 3.0);
    assert_eq!(eval_str(&mut session, "byClass.item(2).id"), "c");
}

// ---------------------------------------------------------------------------
// CSSStyleDeclaration
// ---------------------------------------------------------------------------

#[test]
fn style_get_property_supports_dashed_and_camel_case() {
    let mut session = session_for(
        r#"<body><div id="x" style="color: red; background-color: blue; z-index: 5"></div></body>"#,
    );
    assert_eq!(
        eval_str(
            &mut session,
            "document.getElementById('x').style.getPropertyValue('color')"
        ),
        "red"
    );
    assert_eq!(
        eval_str(
            &mut session,
            "document.getElementById('x').style.getPropertyValue('background-color')"
        ),
        "blue"
    );
    assert_eq!(
        eval_str(
            &mut session,
            "document.getElementById('x').style.getPropertyValue('z-index')"
        ),
        "5"
    );
    // camelCase input is normalized too.
    assert_eq!(
        eval_str(
            &mut session,
            "document.getElementById('x').style.getPropertyValue('backgroundColor')"
        ),
        "blue"
    );
    // camelCase accessors read the same declarations.
    assert_eq!(
        eval_str(
            &mut session,
            "document.getElementById('x').style.backgroundColor"
        ),
        "blue"
    );
    assert_eq!(
        eval_str(&mut session, "document.getElementById('x').style.zIndex"),
        "5"
    );
    assert_eq!(
        eval_str(
            &mut session,
            "document.getElementById('x').style.getPropertyValue('missing')"
        ),
        ""
    );
}

#[test]
fn style_set_property_remove_property_css_text_and_priority() {
    let mut session = session_for(r#"<body><div id="x"></div></body>"#);
    let out = eval_str(
        &mut session,
        r#"(function () {
             var s = document.getElementById('x').style;
             s.setProperty('background-color', 'green');
             s.setProperty('zIndex', '9', 'important');
             var a = s.getPropertyValue('background-color') + '/' + s.getPropertyValue('z-index');
             var p = s.getPropertyPriority('z-index');
             s.removeProperty('background-color');
             var b = s.getPropertyValue('background-color');
             s.cssText = 'color: purple; display: block';
             var c = s.cssText;
             var d = s.getPropertyValue('color');
             return [a, p, b, c, d].join('|');
           })()"#,
    );
    assert_eq!(
        out,
        "green/9|important||color: purple; display: block|purple"
    );
}

#[test]
fn style_common_camel_case_properties_are_exposed() {
    let mut session = session_for(r#"<body><div id="x"></div></body>"#);
    let out = eval_str(
        &mut session,
        r#"(function () {
             var s = document.getElementById('x').style;
             s.boxShadow = '0 0 4px black';
             s.textDecoration = 'underline';
             s.minWidth = '10px';
             s.gridTemplateColumns = '1fr 2fr';
             s.cssFloat = 'left';
             s.overflowX = 'hidden';
             return [
               s.getPropertyValue('box-shadow'),
               s.getPropertyValue('text-decoration'),
               s.minWidth,
               s.gridTemplateColumns,
               s.getPropertyValue('float'),
               s.overflowX
             ].join('|');
           })()"#,
    );
    assert_eq!(out, "0 0 4px black|underline|10px|1fr 2fr|left|hidden");
}

#[test]
fn style_length_and_item_reflect_inline_declarations() {
    let mut session =
        session_for(r#"<body><div id="x" style="color: red; display: block"></div></body>"#);
    assert_eq!(
        eval(&mut session, "document.getElementById('x').style.length"),
        2.0
    );
    assert_eq!(
        eval_str(&mut session, "document.getElementById('x').style.item(0)"),
        "color"
    );
    eval(
        &mut session,
        "document.getElementById('x').style.setProperty('margin', '0')",
    );
    assert_eq!(
        eval(&mut session, "document.getElementById('x').style.length"),
        3.0
    );
}

// ---------------------------------------------------------------------------
// FormData / multipart uploads
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct CapturedRequest {
    content_type: String,
    body: Vec<u8>,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A tiny HTTP server that serves the test page and records every upload's
/// Content-Type and body so multipart encoding can be asserted from Rust.
async fn spawn_capture_server(page_html: &str) -> (SocketAddr, Arc<Mutex<Vec<CapturedRequest>>>) {
    let page_html = page_html.to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captures = Arc::new(Mutex::new(Vec::new()));
    let captures_task = captures.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let captures = captures_task.clone();
            let page_html = page_html.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let mut data: Vec<u8> = Vec::new();
                let header_end = loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                    if let Some(position) = find_subslice(&data, b"\r\n\r\n") {
                        break position + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&data[..header_end]).to_string();
                let mut request_line = headers.lines().next().unwrap_or("").split_whitespace();
                let method = request_line.next().unwrap_or("").to_owned();
                let path = request_line.next().unwrap_or("/").to_owned();
                let header_value = |name: &str| {
                    headers.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        if key.trim().eq_ignore_ascii_case(name) {
                            Some(value.trim().to_owned())
                        } else {
                            None
                        }
                    })
                };
                let content_length = header_value("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                while data.len() < header_end + content_length {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                }
                let end = (header_end + content_length).min(data.len());
                if method == "POST" {
                    captures.lock().unwrap().push(CapturedRequest {
                        content_type: header_value("content-type").unwrap_or_default(),
                        body: data[header_end..end].to_vec(),
                    });
                }
                let (body, content_type) = if path == "/page" {
                    (page_html.into_bytes(), "text/html; charset=utf-8")
                } else {
                    (b"ok".to_vec(), "text/plain")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });
    (addr, captures)
}

fn boundary_of(content_type: &str) -> String {
    let boundary = content_type
        .split(';')
        .find_map(|part| part.trim().strip_prefix("boundary="))
        .expect("content type must include a boundary");
    boundary.trim_matches('"').to_owned()
}

#[tokio::test]
async fn fetch_form_data_sends_multipart_body() {
    let page_html = r#"<html><body><script>
             var fd = new FormData();
             fd.append('user', 'alice');
             var file = new File(['hello from file'], 'note.txt', { type: 'text/plain' });
             fd.append('upload', file);
             fetch('/upload', { method: 'POST', body: fd })
               .then(function (r) { return r.text(); })
               .then(function (t) { document.body.setAttribute('data-result', t); });
           </script></body></html>"#;
    let (addr, captures) = spawn_capture_server(page_html).await;
    let browser = browser();
    let page = browser.open(&format!("http://{addr}/page")).await.unwrap();
    assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
    let body = page.select("body").unwrap().unwrap();
    assert_eq!(
        page.document.get_attribute(body, "data-result"),
        Some("ok"),
        "errors={:?} console={:?}",
        page.script_errors,
        page.console
    );

    let captures = captures.lock().unwrap();
    assert_eq!(captures.len(), 1, "expected one upload request");
    let request = &captures[0];
    assert!(
        request
            .content_type
            .starts_with("multipart/form-data; boundary="),
        "unexpected content type: {}",
        request.content_type
    );
    let boundary = boundary_of(&request.content_type);
    let text = String::from_utf8_lossy(&request.body);
    assert!(text.contains(&format!("--{boundary}\r\n")), "{text}");
    assert!(
        text.contains("Content-Disposition: form-data; name=\"user\""),
        "{text}"
    );
    assert!(text.contains("alice"), "{text}");
    assert!(
        text.contains("Content-Disposition: form-data; name=\"upload\"; filename=\"note.txt\""),
        "{text}"
    );
    assert!(text.contains("Content-Type: text/plain"), "{text}");
    assert!(text.contains("hello from file"), "{text}");
    assert!(text.ends_with(&format!("--{boundary}--\r\n")), "{text}");
}

#[tokio::test]
async fn fetch_request_and_xhr_form_data_are_multipart() {
    // fetch(new Request(..., { body: FormData }))
    {
        let page_html = r#"<html><body><script>
                 var fd = new FormData();
                 fd.append('field', 'value');
                 var request = new Request('/upload', { method: 'POST', body: fd });
                 fetch(request).then(function (r) { return r.text(); })
                   .then(function (t) { document.body.setAttribute('data-result', t); });
               </script></body></html>"#;
        let (addr, captures) = spawn_capture_server(page_html).await;
        let browser = browser();
        let page = browser.open(&format!("http://{addr}/page")).await.unwrap();
        assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
        let body = page.select("body").unwrap().unwrap();
        assert_eq!(page.document.get_attribute(body, "data-result"), Some("ok"));
        let captures = captures.lock().unwrap();
        assert_eq!(captures.len(), 1);
        assert!(
            captures[0]
                .content_type
                .starts_with("multipart/form-data; boundary="),
            "content_type={:?} body={:?}",
            captures[0].content_type,
            String::from_utf8_lossy(&captures[0].body)
        );
        let boundary = boundary_of(&captures[0].content_type);
        let text = String::from_utf8_lossy(&captures[0].body);
        assert!(text.contains("name=\"field\""), "{text}");
        assert!(text.contains("value"), "{text}");
        assert!(text.ends_with(&format!("--{boundary}--\r\n")), "{text}");
    }

    // XMLHttpRequest.send(FormData)
    {
        let page_html = r#"<html><body><script>
                 var fd = new FormData();
                 fd.append('xhr', 'payload');
                 var xhr = new XMLHttpRequest();
                 xhr.open('POST', '/upload');
                 xhr.onload = function () {
                   document.body.setAttribute('data-xhr', xhr.responseText);
                 };
                 xhr.send(fd);
               </script></body></html>"#;
        let (addr, captures) = spawn_capture_server(page_html).await;
        let browser = browser();
        let page = browser.open(&format!("http://{addr}/page")).await.unwrap();
        assert!(page.script_errors.is_empty(), "{:?}", page.script_errors);
        let body = page.select("body").unwrap().unwrap();
        assert_eq!(page.document.get_attribute(body, "data-xhr"), Some("ok"));
        let captures = captures.lock().unwrap();
        assert_eq!(captures.len(), 1);
        assert!(
            captures[0]
                .content_type
                .starts_with("multipart/form-data; boundary=")
        );
        let text = String::from_utf8_lossy(&captures[0].body);
        assert!(text.contains("name=\"xhr\""), "{text}");
        assert!(text.contains("payload"), "{text}");
    }
}
