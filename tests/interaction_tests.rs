mod common;

use common::spawn_test_server;
use nexium::engine::css::Viewport;
use nexium::{AccessibilityNode, Browser, Config, PageSession};

fn attribute(session: &PageSession, selector: &str, name: &str) -> Option<String> {
    let node = session.page().select(selector).unwrap().unwrap();
    session
        .page()
        .document
        .get_attribute(node, name)
        .map(str::to_owned)
}

fn find_ax<'a>(
    node: &'a AccessibilityNode,
    role: &str,
    name: &str,
) -> Option<&'a AccessibilityNode> {
    if node.role == role && node.name.as_deref() == Some(name) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_ax(child, role, name))
}

#[tokio::test]
async fn mouse_hover_and_click_dispatch_javascript() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser
        .open(&format!("http://{addr}/interact"))
        .await
        .unwrap();
    let mut session = PageSession::new(
        page,
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    );

    let button = session.element_at(50.0, 60.0).expect("button at point");
    assert_eq!(button.tag, "button");
    assert_eq!(button.name.as_deref(), Some("Click me"));
    assert!(button.clickable);
    assert!(button.visible);
    assert!(button.in_viewport);

    session.hover(50.0, 60.0);
    assert_eq!(
        attribute(&session, "#out", "data-hover").as_deref(),
        Some("yes")
    );

    let result = session.click(50.0, 60.0);
    assert_eq!(result.target, Some(button.node));
    assert!(!result.prevented);
    assert_eq!(
        attribute(&session, "#out", "data-count").as_deref(),
        Some("1")
    );
    assert_eq!(
        session
            .page()
            .document
            .text_content(session.page().select("#out").unwrap().unwrap()),
        "clicked 1"
    );

    session.click(50.0, 60.0);
    assert_eq!(
        attribute(&session, "#out", "data-count").as_deref(),
        Some("2")
    );
}

#[tokio::test]
async fn browser_interactions_update_the_active_tab() {
    let addr = spawn_test_server().await;
    let mut browser = Browser::new(Config::default());
    browser.set_viewport(800, 600).unwrap();
    browser
        .open_tab(&format!("http://{addr}/interact"))
        .await
        .unwrap();

    let button = browser.element_at(50.0, 60.0).unwrap().expect("button");
    assert_eq!(button.tag, "button");
    browser.click(50.0, 60.0).unwrap();
    let out = browser
        .active_page()
        .unwrap()
        .select("#out")
        .unwrap()
        .unwrap();
    assert_eq!(
        browser
            .active_page()
            .unwrap()
            .document
            .get_attribute(out, "data-count"),
        Some("1")
    );

    browser.click(50.0, 60.0).unwrap();
    let out = browser
        .active_page()
        .unwrap()
        .select("#out")
        .unwrap()
        .unwrap();
    assert_eq!(
        browser
            .active_page()
            .unwrap()
            .document
            .get_attribute(out, "data-count"),
        Some("2")
    );

    browser.click(60.0, 145.0).unwrap();
    browser.type_text("abc").unwrap();
    let input = browser
        .active_page()
        .unwrap()
        .select("#name")
        .unwrap()
        .unwrap();
    assert_eq!(
        browser
            .active_page()
            .unwrap()
            .document
            .get_attribute(input, "value"),
        Some("abc")
    );

    let tree = browser.accessibility_tree().unwrap();
    assert_eq!(tree.role, "document");
    let snapshot = browser.dom_snapshot().unwrap();
    assert!(snapshot.html.contains("id=\"name\""));
}

#[tokio::test]
async fn typing_and_keyboard_events_reach_focused_input() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser
        .open(&format!("http://{addr}/interact"))
        .await
        .unwrap();
    let mut session = PageSession::new(
        page,
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    );

    let input = session.element_at(60.0, 145.0).expect("input at point");
    assert_eq!(input.tag, "input");
    assert_eq!(input.role.as_deref(), Some("textbox"));

    session.click(60.0, 145.0);
    session.type_text("hello");
    assert_eq!(
        attribute(&session, "#name", "value").as_deref(),
        Some("hello")
    );
    assert_eq!(
        attribute(&session, "#out", "data-value").as_deref(),
        Some("hello")
    );

    session.press_key("Backspace");
    assert_eq!(
        attribute(&session, "#name", "value").as_deref(),
        Some("hell")
    );
    session.press_key("Enter");
    assert_eq!(
        attribute(&session, "#out", "data-enter").as_deref(),
        Some("yes")
    );
}

#[tokio::test]
async fn scrolling_and_hit_testing_use_viewport_coordinates() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser
        .open(&format!("http://{addr}/scroll-page"))
        .await
        .unwrap();
    let mut session = PageSession::new(
        page,
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    );

    let target = session.page().select("#target").unwrap().unwrap();
    let info = session.element_info(target).expect("target geometry");
    assert!(!info.in_viewport);
    assert!(session.scroll_into_view(target));
    let (scroll_x, scroll_y) = session.scroll();
    assert!(scroll_y > 0.0);

    let info = session.element_info(target).expect("target geometry");
    assert!(info.in_viewport);
    let x = info.rect.x + 5.0 - scroll_x;
    let y = info.rect.y + 5.0 - scroll_y;
    let hit = session.hit_test(x, y).expect("hit target");
    assert_eq!(hit, target);

    let before = session.scroll().1;
    session.wheel(10.0, 10.0, 0.0, 120.0);
    assert!(session.scroll().1 >= before);
    session.scroll_to(0.0, 100_000.0);
    let (_, max_y) = session.max_scroll();
    assert_eq!(session.scroll().1, max_y);
}

#[tokio::test]
async fn accessibility_tree_and_dom_snapshot_are_structured() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser
        .open(&format!("http://{addr}/interact"))
        .await
        .unwrap();
    let session = PageSession::new(
        page,
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    );

    let tree = session.accessibility_tree();
    assert_eq!(tree.role, "document");
    assert!(find_ax(&tree, "button", "Click me").is_some());
    assert!(find_ax(&tree, "link", "Go").is_some());
    assert!(find_ax(&tree, "heading", "Heading").is_some());
    assert!(find_ax(&tree, "textbox", "Name").is_some());

    let snapshot = session.dom_snapshot();
    assert_eq!(snapshot.title.as_deref(), Some("Interact"));
    assert!(snapshot.html.contains("id=\"btn\""));
    assert!(snapshot.text.contains("Click me"));
    assert_eq!(snapshot.viewport_width, 800.0);
    assert!(snapshot.node_count > 5);
}

#[tokio::test]
async fn viewport_management_and_screenshots() {
    let addr = spawn_test_server().await;
    let browser = Browser::new(Config::default());
    let page = browser
        .open(&format!("http://{addr}/interact"))
        .await
        .unwrap();
    let mut session = PageSession::new(
        page,
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    );
    session.set_viewport(Viewport {
        width: 400.0,
        height: 300.0,
    });
    assert_eq!(session.viewport().width, 400.0);

    let rendered = session.screenshot(browser.client()).await.unwrap();
    assert_eq!(rendered.image.width(), 400);
    let viewport = rendered.viewport_image_at(0.0, 0.0);
    assert_eq!(viewport.width(), 400);
    assert_eq!(viewport.height(), 300);

    let node = session.page().select("#btn").unwrap().unwrap();
    let info = session.element_info(node).expect("button geometry");
    let button = rendered.element_image(node).expect("button image");
    assert_eq!(
        button.width() as f32,
        (info.rect.x + info.rect.width).ceil() - info.rect.x.floor()
    );
    assert_eq!(
        button.height() as f32,
        (info.rect.y + info.rect.height).ceil() - info.rect.y.floor()
    );

    let full = session.dom_snapshot();
    assert_eq!(full.viewport_height, 300.0);
}
