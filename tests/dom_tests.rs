use nexium::engine::dom::{Document, DomError, parse_selector_list};
use nexium::parse_html;

#[test]
fn create_mutate_and_serialize() {
    let mut doc = Document::new();
    let root = doc.document_node();
    let div = doc.create_element("div");
    doc.append_child(root, div).unwrap();
    doc.set_attribute(div, "id", "main").unwrap();

    let span = doc.create_element("span");
    doc.set_attribute(span, "class", "x").unwrap();
    doc.append_child(div, span).unwrap();

    let text = doc.create_text("hi & bye");
    doc.append_child(span, text).unwrap();

    assert_eq!(doc.text_content(div), "hi & bye");
    assert_eq!(
        doc.outer_html(div),
        r#"<div id="main"><span class="x">hi &amp; bye</span></div>"#
    );
    assert_eq!(
        doc.inner_html(div),
        r#"<span class="x">hi &amp; bye</span>"#
    );
    assert_eq!(doc.id_attribute(div), Some("main"));
}

#[test]
fn insert_before_orders_children() {
    let mut doc = Document::new();
    let root = doc.document_node();
    let p = doc.create_element("p");
    doc.append_child(root, p).unwrap();
    let first = doc.create_text("first");
    let second = doc.create_text("second");
    doc.append_child(p, second).unwrap();
    doc.insert_before(p, first, Some(second)).unwrap();
    assert_eq!(doc.text_content(p), "firstsecond");
}

#[test]
fn removing_a_non_child_errors() {
    let mut doc = Document::new();
    let root = doc.document_node();
    let a = doc.create_element("a");
    let b = doc.create_element("b");
    doc.append_child(root, a).unwrap();
    let error = doc.remove_child(root, b).unwrap_err();
    assert!(matches!(error, DomError::NotAChild(_, _)));
}

#[test]
fn cycles_are_rejected() {
    let mut doc = Document::new();
    let root = doc.document_node();
    let parent = doc.create_element("div");
    let child = doc.create_element("span");
    doc.append_child(root, parent).unwrap();
    doc.append_child(parent, child).unwrap();
    let error = doc.append_child(child, parent).unwrap_err();
    assert!(matches!(error, DomError::Cycle));
}

#[test]
fn selectors_type_class_id_and_attributes() {
    let doc = parse_html(
        r#"<div id="main" class="a b"><p class="a" data-x="1">one</p><p>two</p><span data-x="2">three</span></div>"#,
    );
    assert_eq!(doc.select_all("#main").unwrap().len(), 1);
    assert_eq!(doc.select_all(".a").unwrap().len(), 2);
    assert_eq!(doc.select_all("div.a.b").unwrap().len(), 1);
    assert_eq!(doc.select_all("[data-x]").unwrap().len(), 2);
    assert_eq!(doc.select_all(r#"[data-x="2"]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all("p").unwrap().len(), 2);
    assert_eq!(doc.select_all("*").unwrap().len(), doc.element_count());
}

#[test]
fn selector_combinators() {
    let doc =
        parse_html(r#"<div><p class="a">one</p><p>two</p><span data-x="2">three</span></div>"#);
    assert_eq!(doc.select_all("div > p").unwrap().len(), 2);
    assert_eq!(doc.select_all("div p").unwrap().len(), 2);
    assert_eq!(doc.select_all("p + p").unwrap().len(), 1);
    assert_eq!(doc.select_all("p + span").unwrap().len(), 1);
    assert_eq!(doc.select_all("p, span").unwrap().len(), 3);
    assert_eq!(doc.select_all("body p").unwrap().len(), 2);
    assert_eq!(doc.select_all("p > span").unwrap().len(), 0);
}

#[test]
fn query_selector_returns_first_in_document_order() {
    let doc = parse_html("<div><p id=one>1</p><p id=two>2</p></div>");
    let first = doc.select("p").unwrap().unwrap();
    assert_eq!(doc.get_attribute(first, "id"), Some("one"));
}

#[test]
fn element_queries_exclude_the_element_itself() {
    let doc = parse_html("<div id=outer><div id=inner></div></div>");
    let outer = doc.get_element_by_id("outer").unwrap();
    assert!(doc.query_selector(outer, "div").unwrap().is_some());
    assert!(doc.query_selector(outer, "#outer").unwrap().is_none());
    assert!(doc.select("#outer").unwrap().is_some());
}

#[test]
fn invalid_selectors_are_rejected() {
    let doc = parse_html("<p>x</p>");
    for selector in ["", "div >", "[", ".", "#"] {
        let error = doc.select_all(selector).unwrap_err();
        assert!(matches!(error, DomError::InvalidSelector(_)), "{selector}");
    }
}

#[test]
fn selector_specificity_is_computed() {
    let selectors = parse_selector_list("#a .b > div").unwrap();
    assert_eq!(selectors[0].specificity().0, 1);
    assert_eq!(selectors[0].specificity().1, 1);
    assert_eq!(selectors[0].specificity().2, 1);
}

#[test]
fn matches_checks_individual_elements() {
    let doc = parse_html(r#"<p class="a">x</p><p>y</p>"#);
    let first = doc.select("p").unwrap().unwrap();
    assert!(doc.matches(first, "p.a").unwrap());
    assert!(!doc.matches(first, "p.b").unwrap());
}

#[test]
fn text_content_and_title() {
    let doc = parse_html("<title>My &amp; Title</title><body><p>Hello <b>world</b></p></body>");
    assert_eq!(doc.title().as_deref(), Some("My & Title"));
    let p = doc.select("p").unwrap().unwrap();
    assert_eq!(doc.text_content(p), "Hello world");
}

#[test]
fn set_text_content_replaces_children() {
    let mut doc = parse_html("<p><b>old</b></p>");
    let p = doc.select("p").unwrap().unwrap();
    doc.set_text_content(p, "new").unwrap();
    assert_eq!(doc.outer_html(p), "<p>new</p>");
    doc.set_text_content(p, "").unwrap();
    assert_eq!(doc.inner_html(p), "");
}

#[test]
fn void_elements_have_empty_inner_html() {
    let doc = parse_html("<div><br></div>");
    let br = doc.select("br").unwrap().unwrap();
    assert_eq!(doc.inner_html(br), "");
    assert_eq!(doc.outer_html(br), "<br>");
}

#[test]
fn attributes_can_be_removed() {
    let mut doc = parse_html("<div id=x class=y></div>");
    let div = doc.select("div").unwrap().unwrap();
    assert_eq!(
        doc.remove_attribute(div, "id").unwrap(),
        Some("x".to_owned())
    );
    assert_eq!(doc.remove_attribute(div, "id").unwrap(), None);
    assert_eq!(doc.outer_html(div), r#"<div class="y"></div>"#);
}

#[test]
fn get_element_by_id_returns_none_when_missing() {
    let doc = parse_html("<div id=exists></div>");
    assert!(doc.get_element_by_id("exists").is_some());
    assert!(doc.get_element_by_id("missing").is_none());
}

#[test]
fn attribute_selector_case_insensitive_flag() {
    let doc = parse_html(r#"<a data-x="AbC" data-y="abc"></a>"#);
    assert_eq!(doc.select_all(r#"[data-x="abc"]"#).unwrap().len(), 0);
    assert_eq!(doc.select_all(r#"[data-x="abc" i]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[data-y="ABC" i]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[data-x^="ab" i]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[data-x$="BC" i]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[data-x*="bC" i]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[data-x="abc" s]"#).unwrap().len(), 0);
}

#[test]
fn namespace_selectors_match_foreign_content() {
    let doc =
        parse_html("<svg><path id=p></path><foreignObject><div id=d></div></foreignObject></svg>");
    assert_eq!(doc.select_all("svg|path").unwrap().len(), 1);
    assert_eq!(doc.select_all("*|path").unwrap().len(), 1);
    assert_eq!(doc.select_all("svg|div").unwrap().len(), 0);
    assert_eq!(doc.select_all("html|div").unwrap().len(), 1);
    assert!(matches!(
        doc.select_all("unknown|x"),
        Err(DomError::InvalidSelector(_))
    ));
}

#[test]
fn html_type_selectors_remain_case_insensitive() {
    let doc = parse_html("<div id=x></div>");
    assert_eq!(doc.select_all("DIV").unwrap().len(), 1);
    assert_eq!(doc.select_all("Div").unwrap().len(), 1);
}
