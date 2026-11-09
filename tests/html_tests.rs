use nexium::engine::dom::Document;
use nexium::parse_html;

fn element_text(doc: &Document, selector: &str) -> String {
    let id = doc
        .select(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no element for {selector}"));
    doc.text_content(id)
}

#[test]
fn implied_structure_and_doctype() {
    let doc = parse_html("<!DOCTYPE html><p>Hi");
    assert_eq!(
        doc.serialize(),
        "<!DOCTYPE html><html><head></head><body><p>Hi</p></body></html>"
    );
}

#[test]
fn attributes_quoted_unquoted_and_entities() {
    let doc = parse_html(r#"<a href="/x?a=1&amp;b=2" title='A "quote"' data-x=unquoted>Link</a>"#);
    let a = doc.select("a").unwrap().unwrap();
    assert_eq!(doc.get_attribute(a, "href"), Some("/x?a=1&b=2"));
    assert_eq!(doc.get_attribute(a, "title"), Some(r#"A "quote""#));
    assert_eq!(doc.get_attribute(a, "data-x"), Some("unquoted"));
    assert_eq!(doc.text_content(a), "Link");
}

#[test]
fn duplicate_attributes_keep_first() {
    let doc = parse_html(r#"<div id="first" id="second">x</div>"#);
    let div = doc.select("div").unwrap().unwrap();
    assert_eq!(doc.get_attribute(div, "id"), Some("first"));
}

#[test]
fn comments_are_preserved() {
    let doc = parse_html("<!-- top --><html><body><!-- inner --><p>x</p></body></html>");
    let html = doc.serialize();
    assert!(html.contains("<!-- top -->"));
    assert!(html.contains("<!-- inner -->"));
    assert!(html.contains("<p>x</p>"));
}

#[test]
fn void_elements_have_no_closing_tag() {
    let doc = parse_html(r#"<div><br><img src="x"></div>"#);
    assert_eq!(
        doc.serialize(),
        r#"<html><head></head><body><div><br><img src="x"></div></body></html>"#
    );
}

#[test]
fn script_contents_are_raw_text() {
    let source = "<script>if (a < b && c > d) { x(); }</script>";
    let doc = parse_html(source);
    assert_eq!(element_text(&doc, "script"), "if (a < b && c > d) { x(); }");
    assert!(doc.serialize().contains(source));
}

#[test]
fn style_contents_are_raw_text() {
    let doc = parse_html("<style>p > a { color: red; }</style>");
    assert_eq!(element_text(&doc, "style"), "p > a { color: red; }");
}

#[test]
fn title_decodes_entities() {
    let doc = parse_html("<title>Tom &amp; Jerry</title>");
    assert_eq!(doc.title().as_deref(), Some("Tom & Jerry"));
}

#[test]
fn malformed_paragraphs_are_recovered() {
    let doc = parse_html("<div><p>one<p>two</div>");
    assert_eq!(
        doc.serialize(),
        "<html><head></head><body><div><p>one</p><p>two</p></div></body></html>"
    );
}

#[test]
fn unclosed_list_items_are_recovered() {
    let doc = parse_html("<ul><li>a<li>b");
    assert_eq!(
        doc.serialize(),
        "<html><head></head><body><ul><li>a</li><li>b</li></ul></body></html>"
    );
}

#[test]
fn table_cells_and_rows_are_recovered() {
    let doc = parse_html("<table><tr><td>1<td>2<tr><td>3</table>");
    assert_eq!(
        doc.serialize(),
        "<html><head></head><body><table><tr><td>1</td><td>2</td></tr><tr><td>3</td></tr></table></body></html>"
    );
}

#[test]
fn textarea_strips_one_leading_newline() {
    let doc = parse_html("<textarea>\nhello</textarea>");
    assert_eq!(element_text(&doc, "textarea"), "hello");
}

#[test]
fn crlf_is_normalized() {
    let doc = parse_html("<p>a\r\nb\rc</p>");
    assert_eq!(element_text(&doc, "p"), "a\nb\nc");
}

#[test]
fn stray_end_tags_are_ignored() {
    let doc = parse_html("</div></span><p>x");
    assert_eq!(doc.select_all("div").unwrap().len(), 0);
    assert_eq!(doc.select_all("span").unwrap().len(), 0);
    assert_eq!(element_text(&doc, "p"), "x");
}

#[test]
fn mismatched_formatting_is_closed_implicitly() {
    let doc = parse_html("<p><b><i>x</b></i></p>");
    assert_eq!(
        doc.serialize(),
        "<html><head></head><body><p><b><i>x</i></b></p></body></html>"
    );
}

#[test]
fn character_reference_edge_cases() {
    let doc = parse_html("<p>&amp; &unknown; &ampx; &#65; &#x1F600; &nbsp;</p>");
    let text = element_text(&doc, "p");
    assert_eq!(text, "& &unknown; &x; A 😀 \u{00A0}");
}

#[test]
fn attribute_entity_without_semicolon_before_equals_is_literal() {
    let doc = parse_html(r#"<a href="?x=1&amp=2">link</a>"#);
    let a = doc.select("a").unwrap().unwrap();
    assert_eq!(doc.get_attribute(a, "href"), Some("?x=1&amp=2"));
}

#[test]
fn non_void_self_closing_slash_is_ignored() {
    let doc = parse_html("<div/>x");
    assert_eq!(
        doc.serialize(),
        "<html><head></head><body><div>x</div></body></html>"
    );
}

#[test]
fn body_attributes_are_merged() {
    let doc = parse_html(r#"<body class="a"><body data-extra="1">x"#);
    let body = doc.select("body").unwrap().unwrap();
    assert_eq!(doc.get_attribute(body, "class"), Some("a"));
    assert_eq!(doc.get_attribute(body, "data-extra"), Some("1"));
}

#[test]
fn text_is_escaped_when_serializing() {
    let doc = parse_html("<p>&lt;tag&gt; &amp; more</p>");
    assert!(doc.serialize().contains("<p>&lt;tag&gt; &amp; more</p>"));
}

#[test]
fn nested_head_elements_stay_in_head() {
    let doc = parse_html("<meta charset=utf-8><title>T</title><p>x");
    let head = doc.select("head").unwrap().unwrap();
    let head_tags: Vec<&str> = doc
        .children(head)
        .iter()
        .filter_map(|&id| doc.tag_name(id))
        .collect();
    assert_eq!(head_tags, vec!["meta", "title"]);
    assert_eq!(element_text(&doc, "p"), "x");
}

#[test]
fn full_named_character_references() {
    let doc = parse_html("<p>&NotEqualTilde; &fjlig; &CounterClockwiseContourIntegral; &nbsp</p>");
    let text = element_text(&doc, "p");
    assert_eq!(text, "≂̸ fj ∳ \u{00A0}");
}

#[test]
fn legacy_entities_without_semicolon_decode_in_text() {
    let doc = parse_html("<p>&copy 2026 &amp x</p>");
    assert_eq!(element_text(&doc, "p"), "© 2026 & x");
}

#[test]
fn quirks_mode_detection() {
    let doc = parse_html("<!DOCTYPE html><p>x</p>");
    assert_eq!(doc.quirks_mode(), nexium::engine::dom::QuirksMode::NoQuirks);
    assert_eq!(doc.compat_mode(), "CSS1Compat");

    let doc = parse_html("<p>x</p>");
    assert_eq!(doc.quirks_mode(), nexium::engine::dom::QuirksMode::Quirks);
    assert_eq!(doc.compat_mode(), "BackCompat");

    let doc =
        parse_html(r#"<!DOCTYPE html PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN"><p>x</p>"#);
    assert_eq!(doc.quirks_mode(), nexium::engine::dom::QuirksMode::Quirks);

    let doc = parse_html(
        r#"<!DOCTYPE html PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN" "http://www.w3.org/TR/html4/loose.dtd"><p>x</p>"#,
    );
    assert_eq!(
        doc.quirks_mode(),
        nexium::engine::dom::QuirksMode::LimitedQuirks
    );

    let doc = parse_html(r#"<!DOCTYPE svg><p>x</p>"#);
    assert_eq!(doc.quirks_mode(), nexium::engine::dom::QuirksMode::Quirks);
}

#[test]
fn template_contents_are_detached_from_the_document() {
    let doc = parse_html("<template id=t><p class=inner>hi</p></template><div id=out>x</div>");
    let template = doc.select("#t").unwrap().unwrap();
    let content = doc.template_content(template).expect("template content");
    let children = doc.children(content);
    assert_eq!(children.len(), 1);
    assert_eq!(doc.tag_name(children[0]), Some("p"));
    assert_eq!(doc.text_content(children[0]), "hi");
    assert_eq!(doc.text_content(template), "");
    assert!(doc.select("template .inner").unwrap().is_none());
    assert_eq!(
        doc.outer_html(template),
        "<template id=\"t\"><p class=\"inner\">hi</p></template>"
    );
}

#[test]
fn nested_template_content_is_routed_to_the_inner_template() {
    let doc = parse_html("<template id=outer><template id=inner><b>x</b></template></template>");
    let outer = doc.select("#outer").unwrap().unwrap();
    let outer_content = doc.template_content(outer).unwrap();
    let inner = doc.children(outer_content)[0];
    assert_eq!(doc.tag_name(inner), Some("template"));
    let inner_content = doc.template_content(inner).unwrap();
    assert_eq!(doc.children(inner_content).len(), 1);
    assert_eq!(doc.text_content(doc.children(inner_content)[0]), "x");
}

#[test]
fn formatting_is_reconstructed_across_paragraphs() {
    let doc = parse_html("<p>1<b>2<p>3");
    assert_eq!(
        doc.serialize(),
        "<html><head></head><body><p>1<b>2</b></p><p><b>3</b></p></body></html>"
    );
}

#[test]
fn formatting_is_reconstructed_after_misnested_end_tag() {
    let doc = parse_html("<b>1<i>2</b>3");
    assert_eq!(
        doc.serialize(),
        "<html><head></head><body><b>1<i>2</i></b><i>3</i></body></html>"
    );
}

#[test]
fn adoption_agency_keeps_open_blocks() {
    let doc = parse_html("<b>1<div>2</b>3</div>");
    let body = doc.select("body").unwrap().unwrap();
    let tags: Vec<&str> = doc
        .children(body)
        .iter()
        .filter_map(|&id| doc.tag_name(id))
        .collect();
    assert_eq!(tags, vec!["b", "div"]);
    assert_eq!(element_text(&doc, "b"), "1");
    assert_eq!(element_text(&doc, "div"), "23");
}

#[test]
fn svg_elements_get_a_foreign_namespace_and_case_is_preserved() {
    let doc = parse_html(
        r#"<svg viewBox="0 0 10 10"><linearGradient id=g></linearGradient><path d="M0 0"/><foreignObject><div>x</div></foreignObject></svg>"#,
    );
    let svg = doc.select("svg").unwrap().unwrap();
    let element = doc.element(svg).unwrap();
    assert_eq!(element.namespace, nexium::engine::dom::Namespace::Svg);
    assert_eq!(element.get_attribute("viewBox"), Some("0 0 10 10"));

    let path = doc.select("svg path").unwrap().unwrap();
    assert_eq!(
        doc.element(path).unwrap().namespace,
        nexium::engine::dom::Namespace::Svg
    );
    assert_eq!(doc.children(path).len(), 0);

    let gradient = doc.select("linearGradient").unwrap().unwrap();
    assert_eq!(doc.tag_name(gradient), Some("linearGradient"));

    let div = doc.select("foreignObject div").unwrap().unwrap();
    assert_eq!(
        doc.element(div).unwrap().namespace,
        nexium::engine::dom::Namespace::Html
    );
}

#[test]
fn foreign_breakout_elements_leave_svg() {
    let doc = parse_html("<svg><path></path><div id=out>html</div></svg>");
    let div = doc.select("#out").unwrap().unwrap();
    assert_eq!(
        doc.element(div).unwrap().namespace,
        nexium::engine::dom::Namespace::Html
    );
    let body = doc.select("body").unwrap().unwrap();
    let tags: Vec<&str> = doc
        .children(body)
        .iter()
        .filter_map(|&id| doc.tag_name(id))
        .collect();
    assert_eq!(tags, vec!["svg", "div"]);
}

#[test]
fn mathml_elements_get_the_math_namespace() {
    let doc = parse_html("<math><mi>x</mi><mo>+</mo></math>");
    let math = doc.select("math").unwrap().unwrap();
    assert_eq!(
        doc.element(math).unwrap().namespace,
        nexium::engine::dom::Namespace::MathMl
    );
    let mi = doc.select("math mi").unwrap().unwrap();
    assert_eq!(doc.tag_name(mi), Some("mi"));
    assert_eq!(
        doc.element(mi).unwrap().namespace,
        nexium::engine::dom::Namespace::MathMl
    );
}

#[test]
fn adoption_agency_rewraps_block_content() {
    let doc = parse_html("<b><p>x</b>y</p>");
    assert_eq!(
        doc.serialize(),
        "<html><head></head><body><b></b><p><b>x</b>y</p></body></html>"
    );

    let doc = parse_html("<b>1<div>2</b>3</div>");
    let div = doc.select("div").unwrap().unwrap();
    assert_eq!(doc.children(div).len(), 2);
    assert_eq!(doc.tag_name(doc.children(div)[0]), Some("b"));
    assert_eq!(doc.text_content(doc.children(div)[0]), "2");
}
