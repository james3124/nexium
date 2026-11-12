//! Phase 2 CSS gap coverage: inherited table properties, `@namespace`
//! declarations and function-valued media queries.

use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;
use nexium::engine::css::ua::UA_CSS;
use nexium::engine::css::{
    BorderCollapse, CaptionSide, Color, ComputedStyle, Rule, StyleResolver, Viewport,
    parse_declaration_list, parse_stylesheet, user_agent_stylesheet,
};
use nexium::engine::dom::{Document, NodeId};
use nexium::engine::layout::{LayoutResult, layout_document};
use nexium::engine::text::FontSet;
use nexium::{Page, parse_html};

fn inline_style(doc: &Document, node: NodeId) -> Vec<nexium::engine::css::Declaration> {
    doc.get_attribute(node, "style")
        .map(parse_declaration_list)
        .unwrap_or_default()
}

fn compute(css: &str, html: &str, selector: &str, viewport: Viewport) -> ComputedStyle {
    let doc = parse_html(html);
    let author = [parse_stylesheet(css)];
    let resolver = StyleResolver::new(&doc, user_agent_stylesheet(), &author, viewport);
    let node = doc.select(selector).unwrap().unwrap();
    let root = doc.select("html").unwrap().unwrap();

    let initial = ComputedStyle::initial();
    let root_style = resolver.resolve(root, &initial, &inline_style(&doc, root), 16.0);
    let root_font_size = root_style.font_size;

    let mut chain = doc.ancestors(node);
    chain.reverse();
    let mut style = root_style;
    for &ancestor in chain.iter().skip(1) {
        style = resolver.resolve(
            ancestor,
            &style,
            &inline_style(&doc, ancestor),
            root_font_size,
        );
    }
    resolver.resolve(node, &style, &inline_style(&doc, node), root_font_size)
}

fn compute_pseudo(
    css: &str,
    html: &str,
    selector: &str,
    pseudo: &str,
    viewport: Viewport,
) -> ComputedStyle {
    let doc = parse_html(html);
    let author = [parse_stylesheet(css)];
    let resolver = StyleResolver::new(&doc, user_agent_stylesheet(), &author, viewport);
    let node = doc.select(selector).unwrap().unwrap();
    let root = doc.select("html").unwrap().unwrap();
    let initial = ComputedStyle::initial();
    let root_style = resolver.resolve(root, &initial, &[], 16.0);
    let root_font_size = root_style.font_size;
    let mut chain = doc.ancestors(node);
    chain.reverse();
    let mut style = root_style;
    for &ancestor in chain.iter().skip(1) {
        style = resolver.resolve(
            ancestor,
            &style,
            &inline_style(&doc, ancestor),
            root_font_size,
        );
    }
    let element_style = resolver.resolve(node, &style, &inline_style(&doc, node), root_font_size);
    resolver.resolve_pseudo(node, &element_style, pseudo, root_font_size)
}

fn default_viewport() -> Viewport {
    Viewport {
        width: 800.0,
        height: 600.0,
    }
}

fn layout(html: &str, width: f32, height: f32) -> LayoutResult {
    let page = Page::from_html(url::Url::parse("https://css-gap.test/").unwrap(), html);
    let viewport = Viewport { width, height };
    let resolver = StyleResolver::new(
        &page.document,
        user_agent_stylesheet(),
        &page.stylesheets,
        viewport,
    );
    let fonts = FontSet::embedded();
    let images: HashMap<String, Arc<RgbaImage>> = HashMap::new();
    layout_document(&page.document, &resolver, fonts, &images, None)
}

/// Render `html` at `width` and return the laid-out width of `#box`.
fn box_width(html: &str, width: f32) -> f32 {
    let result = layout(html, width, 600.0);
    let page = Page::from_html(url::Url::parse("https://css-gap.test/").unwrap(), html);
    let node = page.select("#box").unwrap().unwrap();
    result
        .node_rects
        .get(&node)
        .copied()
        .expect("layout rect for #box")
        .width
}

fn media_html(query: &str) -> String {
    format!(
        "<style>body{{margin:0}}#box{{width:50px}} \
         @media {query} {{#box{{width:123px}}}}</style><div id=box></div>",
    )
}

#[test]
fn border_collapse_and_caption_side_parse_and_inherit() {
    // Property ids/values are recognized from declaration lists.
    let declarations = parse_declaration_list("border-collapse: collapse; caption-side: bottom");
    assert_eq!(declarations.len(), 2);

    // Initial values.
    let initial = ComputedStyle::initial();
    assert_eq!(initial.border_collapse, BorderCollapse::Separate);
    assert_eq!(initial.caption_side, CaptionSide::Top);

    let css = "table { border-collapse: collapse; caption-side: bottom; }";
    let html = "<table><caption id=c>t</caption><tr><td id=d>x</td></tr></table>";

    // Both properties are inherited: cells pick them up from the table.
    let cell = compute(css, html, "#d", default_viewport());
    assert_eq!(cell.border_collapse, BorderCollapse::Collapse);
    assert_eq!(cell.caption_side, CaptionSide::Bottom);

    let caption = compute(css, html, "#c", default_viewport());
    assert_eq!(caption.border_collapse, BorderCollapse::Collapse);
    assert_eq!(caption.caption_side, CaptionSide::Bottom);

    // A descendant may override the inherited value.
    let cell = compute(
        "table { border-collapse: collapse; } td { border-collapse: separate; }",
        html,
        "#d",
        default_viewport(),
    );
    assert_eq!(cell.border_collapse, BorderCollapse::Separate);

    // Unstyled tables keep the initial values.
    let cell = compute("", html, "#d", default_viewport());
    assert_eq!(cell.border_collapse, BorderCollapse::Separate);
    assert_eq!(cell.caption_side, CaptionSide::Top);
}

#[test]
fn namespace_prefixed_selectors_use_declared_prefixes() {
    let css = "@namespace svg url(http://www.w3.org/2000/svg);\n\
               rect { color: green; }\n\
               svg|rect { color: red; }";
    let sheet = parse_stylesheet(css);
    assert_eq!(sheet.namespaces.len(), 1);
    assert_eq!(sheet.namespaces[0].prefix.as_deref(), Some("svg"));
    assert_eq!(sheet.namespaces[0].url, "http://www.w3.org/2000/svg");

    // Quoted `url("...")` form is accepted too.
    let sheet = parse_stylesheet("@namespace q url('http://www.w3.org/1999/xhtml');");
    assert_eq!(sheet.namespaces.len(), 1);
    assert_eq!(sheet.namespaces[0].prefix.as_deref(), Some("q"));

    let style = compute(
        css,
        "<svg><rect id=r></rect></svg>",
        "#r",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(255, 0, 0, 255));

    // A custom prefix bound to XHTML matches HTML elements.
    let style = compute(
        "@namespace h url(http://www.w3.org/1999/xhtml); h|p { color: red; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(255, 0, 0, 255));

    // A prefix bound to an unknown namespace matches nothing.
    let style = compute(
        "@namespace x url(http://example.com/not-a-dom-namespace); x|p { color: red; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::BLACK);
}

#[test]
fn default_namespace_restricts_unprefixed_type_selectors() {
    let css = "@namespace url(http://www.w3.org/2000/svg);\n\
               rect { color: red; }\n\
               div { color: blue; }";
    let sheet = parse_stylesheet(css);
    assert_eq!(sheet.namespaces.len(), 1);
    assert!(sheet.namespaces[0].prefix.is_none());

    // `rect` is an SVG element, so it matches the unprefixed type selector.
    let style = compute(
        css,
        "<svg><rect id=r></rect></svg>",
        "#r",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(255, 0, 0, 255));

    // `div` is HTML: the same unprefixed selector must not match it.
    let style = compute(css, "<div id=d>x</div>", "#d", default_viewport());
    assert_eq!(style.color, Color::BLACK);

    // `*|` explicitly opts out of the default namespace.
    let style = compute(
        "@namespace url(http://www.w3.org/2000/svg); *|div { color: blue; }",
        "<div id=d>x</div>",
        "#d",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0, 0, 255, 255));

    // An explicit universal selector is interpreted in the default namespace
    // as well.
    let style = compute(
        "@namespace url(http://www.w3.org/2000/svg); * { color: red; }",
        "<div id=d>x</div>",
        "#d",
        default_viewport(),
    );
    assert_eq!(style.color, Color::BLACK);
    let style = compute(
        "@namespace url(http://www.w3.org/2000/svg); * { color: red; }",
        "<svg><rect id=r></rect></svg>",
        "#r",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(255, 0, 0, 255));
}

#[test]
fn media_query_functions_and_ranges_change_layout_width() {
    // min(): min(600px, calc(50vw + 300px)) is 550px at 500px wide and
    // 600px at 800px wide.
    let html = media_html("(min-width: min(600px, calc(50vw + 300px)))");
    assert_eq!(box_width(&html, 800.0), 123.0);
    assert_eq!(box_width(&html, 500.0), 50.0);

    // max(): max(600px, calc(40vw + 400px)) is 720px at 800px wide.
    let html = media_html("(max-width: max(600px, calc(40vw + 400px)))");
    assert_eq!(box_width(&html, 500.0), 123.0);
    assert_eq!(box_width(&html, 800.0), 50.0);

    // clamp(): 90vw clamped to [700px, 900px].
    let html = media_html("(min-width: clamp(700px, 90vw, 900px))");
    assert_eq!(box_width(&html, 800.0), 123.0);
    assert_eq!(box_width(&html, 600.0), 50.0);

    // calc() with mixed units: 2vw + 700px.
    let html = media_html("(min-width: calc(2vw + 700px))");
    assert_eq!(box_width(&html, 800.0), 123.0);
    assert_eq!(box_width(&html, 600.0), 50.0);

    // Range syntax: width > calc(100px + 2em), with em = 16px.
    let html = media_html("(width > calc(100px + 2em))");
    assert_eq!(box_width(&html, 800.0), 123.0);
    assert_eq!(box_width(&html, 100.0), 50.0);
}

#[test]
fn media_query_example_forms_parse() {
    // Literal forms from the task specification.
    let sheet = parse_stylesheet("@media (min-width: min(600px, 50vw)) { p { color: red; } }");
    let Rule::Media(media) = &sheet.rules[0] else {
        panic!("expected a media rule");
    };
    assert!(media.matches(700.0, 600.0));
    assert!(media.matches(300.0, 600.0));

    let sheet = parse_stylesheet("@media (width > calc(100px + 2em)) { p { color: red; } }");
    let Rule::Media(media) = &sheet.rules[0] else {
        panic!("expected a media rule");
    };
    assert!(media.matches(800.0, 600.0));
    assert!(!media.matches(100.0, 600.0));
}

#[test]
fn hover_rules_do_not_apply_without_a_hover_target() {
    // `Document::set_hover_target` is crate-private; the positive matching
    // case lives in the `selectors` unit test. Here we verify that a `:hover`
    // rule stays inert for a document with no hover target, and that the
    // generated-content and UA paths agree.
    let style = compute(
        "p { color: green; } p:hover { color: red; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0, 128, 0, 255));

    let pseudo = compute_pseudo(
        "p:hover::before { content: 'hover'; }",
        "<p>x</p>",
        "p",
        "before",
        default_viewport(),
    );
    assert_eq!(pseudo.content, None);

    let doc = parse_html("<p id=p>x</p>");
    let p = doc.select("#p").unwrap().unwrap();
    assert!(!doc.matches(p, "p:hover").unwrap());
    assert!(doc.select("p:hover").unwrap().is_none());

    // The user-agent stylesheet must not contain pseudo-hover rules; hover
    // matching is purely DOM-driven.
    assert!(!UA_CSS.contains(":hover"));
}

#[test]
fn grid_auto_fit_repeat_expands_to_container_width() {
    let items: String = (0..6)
        .map(|index| format!("<div class=\"item\" id=\"i{index}\"></div>"))
        .collect();
    let html = format!(
        "<html><body style=\"margin:0\"><div id=\"g\" style=\"display:grid;\
         grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:10px;padding:10px\">\
         {items}</div><style>.item{{height:20px;background:#ccc}}</style></body></html>"
    );
    let result = layout(&html, 1280.0, 720.0);
    let page = Page::from_html(url::Url::parse("https://css-gap.test/").unwrap(), &html);
    let ids = page.select_all(".item").unwrap();
    assert_eq!(ids.len(), 6);
    let rects: Vec<_> = ids
        .iter()
        .map(|id| result.node_rects.get(id).copied().expect("grid item rect"))
        .collect();
    // ~1260px content with 10px gaps fits four 300px-minimum tracks.
    assert!(rects[0].width > 280.0, "width {}", rects[0].width);
    for rect in &rects {
        assert!((rect.width - rects[0].width).abs() < 1.0);
    }
    assert!(
        (rects[3].y - rects[0].y).abs() < 1.0,
        "four items share a row"
    );
    assert!(rects[4].y > rects[0].y + 10.0, "fifth item wraps");
}

#[test]
fn transform_translate_and_scale_change_geometry() {
    let html = r#"<html><body style="margin:0"><div id="box"
        style="width:100px;height:50px;transform:translate(20px,10px)"></div></body></html>"#;
    let result = layout(html, 800.0, 600.0);
    let page = Page::from_html(url::Url::parse("https://css-gap.test/").unwrap(), html);
    let node = page.select("#box").unwrap().unwrap();
    let rect = result.node_rects.get(&node).copied().unwrap();
    assert!((rect.x - 20.0).abs() < 0.5, "x {}", rect.x);
    assert!((rect.y - 10.0).abs() < 0.5, "y {}", rect.y);

    let html = r#"<html><body style="margin:0"><div id="box"
        style="width:100px;height:50px;transform:scale(2)"></div></body></html>"#;
    let result = layout(html, 800.0, 600.0);
    let page = Page::from_html(url::Url::parse("https://css-gap.test/").unwrap(), html);
    let node = page.select("#box").unwrap().unwrap();
    let rect = result.node_rects.get(&node).copied().unwrap();
    assert!((rect.width - 200.0).abs() < 0.5, "width {}", rect.width);
    assert!((rect.height - 100.0).abs() < 0.5, "height {}", rect.height);
    assert!(
        (rect.x + 50.0).abs() < 0.5,
        "scaled about center: x {}",
        rect.x
    );
}
