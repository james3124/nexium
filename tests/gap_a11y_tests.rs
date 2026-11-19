//! Gap coverage: `:focus` styling (host and script driven) and an enriched
//! accessibility tree with ARIA states and iframe traversal.

use nexium::engine::css::Viewport;
use nexium::{AccessibilityNode, Browser, Config, PageSession};

fn session(page: nexium::browser::Page) -> PageSession {
    PageSession::new(
        page,
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    )
}

fn background(session: &PageSession, selector: &str) -> (u8, u8, u8) {
    let node = session.page().select(selector).unwrap().unwrap();
    let style = session.layout().style(node).expect("style for node");
    (
        style.background_color.r,
        style.background_color.g,
        style.background_color.b,
    )
}

fn find_ax<'a>(
    node: &'a AccessibilityNode,
    predicate: impl Fn(&AccessibilityNode) -> bool + Copy,
) -> Option<&'a AccessibilityNode> {
    if predicate(node) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_ax(child, predicate))
}

#[tokio::test]
async fn host_click_focus_applies_focus_style() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/focus-host",
            r#"<html><head><style>
  body { margin: 0; }
  #btn { position: absolute; left: 10px; top: 10px; width: 100px; height: 40px; background-color: #0000ff; }
  #btn:focus { background-color: #ff0000; }
</style></head><body>
  <button id="btn">Press</button>
  <input id="name">
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);
    let node = session.page().select("#btn").unwrap().unwrap();

    assert_eq!(background(&session, "#btn"), (0, 0, 255));
    session.click(50.0, 30.0);
    assert_eq!(session.focused_node(), Some(node));
    assert_eq!(
        background(&session, "#btn"),
        (255, 0, 0),
        ":focus must apply after host click focus"
    );
}

#[tokio::test]
async fn script_element_focus_applies_focus_style() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/focus-js",
            r#"<html><head><style>
  body { margin: 0; }
  #btn { position: absolute; left: 10px; top: 10px; width: 100px; height: 40px; background-color: #0000ff; }
  #btn:focus { background-color: #00ff00; }
  #wrap:focus-within { background-color: #ffff00; }
</style></head><body>
  <div id="wrap"><button id="btn">Press</button></div>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);
    let node = session.page().select("#btn").unwrap().unwrap();

    assert_eq!(background(&session, "#btn"), (0, 0, 255));
    session
        .evaluate("document.getElementById('btn').focus()")
        .expect("focus() evaluation");
    assert_eq!(session.focused_node(), Some(node));
    assert_eq!(
        background(&session, "#btn"),
        (0, 255, 0),
        ":focus must apply after script element.focus()"
    );
    assert_eq!(
        background(&session, "#wrap"),
        (255, 255, 0),
        ":focus-within must match the focused element's ancestors"
    );
}

#[tokio::test]
async fn focus_visible_matches_keyboard_focus() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/focus-visible",
            r#"<html><head><style>
  body { margin: 0; }
  input { display: block; width: 100px; height: 20px; border: 2px solid #0000ff; }
  input:focus-visible { border-color: #ff0000; }
</style></head><body>
  <input id="a"><input id="b">
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);
    let node = session.page().select("#b").unwrap().unwrap();
    let border = |s: &PageSession| {
        let n = s.page().select("#b").unwrap().unwrap();
        let style = s.layout().style(n).unwrap();
        (
            style.border_color.top.r,
            style.border_color.top.g,
            style.border_color.top.b,
        )
    };

    assert_eq!(border(&session), (0, 0, 255));
    session.focus_node(node);
    assert_eq!(session.focused_node(), Some(node));
    assert_eq!(
        border(&session),
        (255, 0, 0),
        ":focus-visible must match programmatic/keyboard focus"
    );
}

#[tokio::test]
async fn accessibility_tree_exposes_roles_states_and_iframe_content() {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "http://test.local/a11y",
            r#"<html><head><title>A11y</title></head><body>
  <button id="b" aria-label="Save">Save</button>
  <input type="checkbox" id="cb" checked aria-label="Accept">
  <div id="disc" role="button" aria-expanded="true" aria-label="Disclosure">Details</div>
  <ul role="tablist"><li role="tab" aria-selected="true" id="tab">Tab</li></ul>
  <div id="live" role="status" aria-live="polite">Live region</div>
  <h2 id="head">Heading two</h2>
  <iframe id="frame" title="Frame" srcdoc="&lt;h1 id='inside'&gt;Inside frame&lt;/h1&gt;"></iframe>
</body></html>"#,
        )
        .unwrap();
    let mut session = session(page);

    // Focus the button from script so the tree can report focused state.
    session
        .evaluate("document.getElementById('b').focus()")
        .unwrap();

    let tree = session.accessibility_tree();
    assert_eq!(tree.role, "document");
    assert_eq!(tree.name.as_deref(), Some("A11y"));

    let button_ax = find_ax(&tree, |node| node.name.as_deref() == Some("Save"))
        .expect("button with aria-label");
    assert_eq!(button_ax.role, "button");
    assert!(button_ax.focusable);
    assert!(
        button_ax.focused,
        "script-focused button must be marked focused"
    );

    let checkbox = find_ax(&tree, |node| node.name.as_deref() == Some("Accept"))
        .expect("checkbox with aria-label");
    assert_eq!(checkbox.role, "checkbox");
    assert_eq!(checkbox.checked, Some(true));

    let disclosure =
        find_ax(&tree, |node| node.name.as_deref() == Some("Disclosure")).expect("disclosure");
    assert_eq!(disclosure.expanded, Some(true));
    assert_eq!(disclosure.role, "button");

    let tab = find_ax(&tree, |node| node.role == "tab").expect("tab");
    assert_eq!(tab.selected, Some(true));

    let live =
        find_ax(&tree, |node| node.name.as_deref() == Some("Live region")).expect("live region");
    assert_eq!(live.live.as_deref(), Some("polite"));

    let heading =
        find_ax(&tree, |node| node.name.as_deref() == Some("Heading two")).expect("heading");
    assert_eq!(heading.role, "heading");
    assert_eq!(heading.level, Some(2));

    let frame = find_ax(&tree, |node| node.name.as_deref() == Some("Frame")).expect("iframe");
    assert_eq!(frame.role, "iframe");
    assert!(
        find_ax(frame, |node| node.name.as_deref() == Some("Inside frame")).is_some(),
        "iframe srcdoc content must appear in the accessibility tree"
    );
}
