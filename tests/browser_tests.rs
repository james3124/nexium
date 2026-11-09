mod common;

use common::spawn_test_server;
use nexium::browser::page::decode_html;
use nexium::intelligence::{dump_tree, visible_text};
use nexium::{Browser, Config, Page, parse_url};

#[tokio::test]
async fn loads_and_inspects_a_page() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser.open(&format!("http://{addr}/")).await.unwrap();

    assert_eq!(page.status.as_u16(), 200);
    assert_eq!(page.title().as_deref(), Some("Test Page"));
    assert!(page.is_success());

    let heading = page.select("h1").unwrap().unwrap();
    assert_eq!(page.document.text_content(heading), "Hello");

    let intro = page.select(".intro").unwrap().unwrap();
    assert_eq!(page.document.tag_name(intro), Some("p"));
    assert_eq!(page.document.text_content(intro), "World");

    let text = visible_text(&page.document);
    assert_eq!(text, "Hello World");
}

#[tokio::test]
async fn renders_an_indented_dom_tree() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser.open(&format!("http://{addr}/")).await.unwrap();
    let tree = dump_tree(&page.document);
    assert!(tree.contains("html"));
    assert!(tree.contains("head"));
    assert!(tree.contains("title"));
    assert!(tree.contains("h1"));
    assert!(tree.contains(r#"p.intro"#));
    assert!(tree.contains("\"Hello\""));
}

#[test]
fn builds_pages_from_memory() {
    let url = parse_url("https://example.test/").unwrap();
    let page = Page::from_html(url, "<!DOCTYPE html><title>x</title><p>offline");
    assert_eq!(page.title().as_deref(), Some("x"));
    assert_eq!(
        page.document.serialize(),
        "<!DOCTYPE html><html><head><title>x</title></head><body><p>offline</p></body></html>"
    );
}

#[test]
fn decodes_meta_charset_without_http_header() {
    let bytes = b"<html><head><meta charset=\"iso-8859-1\"></head><body>caf\xe9</body></html>";
    let (text, charset) = decode_html(bytes, None);
    assert!(text.contains("café"), "decoded: {text}");
    assert_eq!(charset, "windows-1252");
}

#[test]
fn http_charset_takes_precedence_over_meta() {
    // UTF-8 bytes; the meta tag lies about iso-8859-1. The HTTP header wins,
    // so the text must decode cleanly rather than as "cafÃ©".
    let bytes = "<meta charset=\"iso-8859-1\"><p>café</p>".as_bytes();
    let (text, charset) = decode_html(bytes, Some("utf-8"));
    assert!(text.contains("café"), "decoded: {text}");
    assert_eq!(charset, "UTF-8");
}

#[test]
fn utf8_bom_is_detected() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"<p>hello</p>");
    let (text, charset) = decode_html(&bytes, None);
    assert!(text.starts_with("<p>hello</p>"));
    assert_eq!(charset, "UTF-8");
}

#[tokio::test]
async fn plain_text_is_rendered_as_preformatted_content() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser.open(&format!("http://{addr}/plain")).await.unwrap();
    let pre = page.select("pre").unwrap().unwrap();
    assert_eq!(page.document.text_content(pre), "just text");
    assert!(!page.is_binary);
}

#[tokio::test]
async fn json_is_displayed_as_text() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser.open(&format!("http://{addr}/json")).await.unwrap();
    let pre = page.select("pre").unwrap().unwrap();
    assert!(page.document.text_content(pre).contains("\"ok\":true"));
}

#[tokio::test]
async fn binary_resources_are_not_parsed_as_html() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser
        .open(&format!("http://{addr}/binary"))
        .await
        .unwrap();
    assert!(page.is_binary);
    assert!(page.body_len > 0);
    assert!(page.select("pre").unwrap().is_none());
}

#[tokio::test]
async fn imported_stylesheets_are_applied() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    for path in ["/import-page", "/import-style", "/import-media"] {
        let page = browser.open(&format!("http://{addr}{path}")).await.unwrap();
        let rendered = browser
            .render(
                &page,
                nexium::RenderOptions {
                    viewport: nexium::engine::css::Viewport {
                        width: 400.0,
                        height: 200.0,
                    },
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let node = page.select("#x").unwrap().unwrap();
        let rect = rendered.layout.node_rect(node).unwrap();
        assert!(rect.height > 40.0, "{path}: {rect:?}");
        let mut red = false;
        'outer: for y in rect.y as u32..(rect.bottom() as u32).min(rendered.image.height()) {
            for x in rect.x as u32..(rect.right() as u32).min(rendered.image.width()) {
                let pixel = rendered.image.get_pixel(x, y);
                if pixel[0] > 150 && pixel[1] < 80 && pixel[2] < 80 {
                    red = true;
                    break 'outer;
                }
            }
        }
        assert!(red, "{path}: expected imported red text");
    }
}

#[tokio::test]
async fn circular_imports_terminate() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser
        .open(&format!("http://{addr}/cycle-page"))
        .await
        .unwrap();
    assert!(!page.stylesheets.is_empty());
    let node = page.select("#x").unwrap().unwrap();
    assert_eq!(page.document.text_content(node), "x");
}
