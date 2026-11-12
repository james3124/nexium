//! Foreign attribute namespace modelling (`xlink:`, `xml:`, `xmlns:`).

use nexium::engine::dom::{Attribute, AttributeNamespace, Document, Namespace};
use nexium::parse_html;

#[test]
fn svg_xlink_href_is_preserved_and_namespace_aware() {
    let doc = parse_html(r##"<svg><a xlink:href="#target">x</a></svg>"##);
    let a = doc.select("a").unwrap().expect("svg link");

    assert_eq!(doc.element(a).unwrap().namespace, Namespace::Svg);
    // The qualified name is preserved by the parser and is still reachable by
    // a plain attribute lookup.
    assert_eq!(doc.get_attribute(a, "xlink:href"), Some("#target"));
    // An unprefixed lookup must not see the namespaced attribute.
    assert_eq!(doc.get_attribute(a, "href"), None);

    let attribute = doc
        .element(a)
        .unwrap()
        .find_attribute("xlink:href")
        .expect("qualified attribute");
    assert_eq!(attribute.qualified_name(), "xlink:href");
    assert_eq!(attribute.prefix(), Some("xlink"));
    assert_eq!(attribute.local_name(), "href");
    assert_eq!(attribute.namespace(), AttributeNamespace::XLink);

    assert_eq!(
        doc.get_attribute_ns(a, AttributeNamespace::XLink, "href"),
        Some("#target")
    );
    assert!(doc.has_attribute_ns(a, AttributeNamespace::XLink, "href"));
    assert_eq!(
        doc.get_attribute_ns(a, AttributeNamespace::Xml, "href"),
        None
    );
    assert_eq!(
        doc.attribute_namespace_by_name(a, "xlink:href"),
        Some(AttributeNamespace::XLink)
    );
}

#[test]
fn xml_space_is_modelled_in_the_xml_namespace() {
    let doc = parse_html(r#"<svg><text xml:space="preserve">a  b</text></svg>"#);
    let text = doc.select("text").unwrap().expect("svg text");

    assert_eq!(doc.get_attribute(text, "xml:space"), Some("preserve"));
    assert_eq!(
        doc.get_attribute_ns(text, AttributeNamespace::Xml, "space"),
        Some("preserve")
    );
    assert_eq!(
        doc.get_attribute_ns(text, AttributeNamespace::XLink, "space"),
        None
    );

    let attribute = doc
        .element(text)
        .unwrap()
        .find_attribute("xml:space")
        .expect("qualified attribute");
    assert_eq!(attribute.local_name(), "space");
    assert_eq!(attribute.namespace(), AttributeNamespace::Xml);
    assert_eq!(
        doc.attribute_namespace(text, attribute),
        AttributeNamespace::Xml
    );
}

#[test]
fn xmlns_declarations_are_modelled_in_the_xmlns_namespace() {
    let doc = parse_html(
        r##"<svg xmlns:xlink="http://www.w3.org/1999/xlink"><a xlink:href="#t">x</a></svg>"##,
    );
    let svg = doc.select("svg").unwrap().expect("svg root");
    let a = doc.select("a").unwrap().expect("svg link");

    assert_eq!(
        doc.attribute_namespace_by_name(svg, "xmlns:xlink"),
        Some(AttributeNamespace::Xmlns)
    );
    assert_eq!(
        doc.get_attribute_ns(svg, AttributeNamespace::Xmlns, "xlink"),
        Some(AttributeNamespace::XLINK_URI)
    );
    // The prefix binding makes the xlink attribute resolve to the XLink URI.
    assert_eq!(
        doc.attribute_namespace_by_name(a, "xlink:href"),
        Some(AttributeNamespace::XLink)
    );
    assert_eq!(
        doc.select_all("[xmlns|xlink]").unwrap().len(),
        1,
        "[xmlns|xlink] should match the namespace declaration"
    );
    assert_eq!(
        AttributeNamespace::from_prefix("xlink"),
        Some(AttributeNamespace::XLink)
    );
    assert_eq!(
        AttributeNamespace::from_uri("http://www.w3.org/1999/xlink"),
        Some(AttributeNamespace::XLink)
    );
}

#[test]
fn serialization_round_trips_qualified_names() {
    let source = concat!(
        r#"<svg xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 10 10">"#,
        r##"<a xlink:href="#target" xml:space="preserve">x</a></svg>"##
    );
    let doc = parse_html(source);
    let svg = doc.select("svg").unwrap().expect("svg root");
    assert_eq!(doc.outer_html(svg), source);

    // A full serialize -> parse cycle is stable and keeps the namespaced
    // attributes attached to the right elements.
    let serialized = doc.serialize();
    let reparsed = parse_html(&serialized);
    assert_eq!(reparsed.serialize(), serialized);

    let a = reparsed.select("a").unwrap().expect("svg link");
    assert_eq!(reparsed.get_attribute(a, "xlink:href"), Some("#target"));
    assert_eq!(reparsed.get_attribute(a, "xml:space"), Some("preserve"));
    assert_eq!(
        reparsed.get_attribute_ns(a, AttributeNamespace::XLink, "href"),
        Some("#target")
    );
    assert_eq!(
        reparsed.get_attribute_ns(a, AttributeNamespace::Xml, "space"),
        Some("preserve")
    );
}

#[test]
fn namespace_prefixed_attribute_selectors_match() {
    let doc = parse_html(r##"<svg><a xlink:href="#target">x</a><a href="#plain">y</a></svg>"##);

    assert_eq!(doc.select_all("[xlink|href]").unwrap().len(), 1);
    assert_eq!(
        doc.select_all(r##"[xlink|href="#target"]"##).unwrap().len(),
        1
    );
    assert_eq!(
        doc.select_all(r##"[xlink|href="#other"]"##).unwrap().len(),
        0
    );
    // Unprefixed selectors keep matching only un-namespaced attributes.
    assert_eq!(doc.select_all("[href]").unwrap().len(), 1);
    // `[|href]` is explicitly the no-namespace form.
    assert_eq!(doc.select_all("[|href]").unwrap().len(), 1);
    // `[*|href]` matches either namespace.
    assert_eq!(doc.select_all("[*|href]").unwrap().len(), 2);
    // Dash-match (`|=`) is still parsed as an operator, not a namespace.
    assert_eq!(doc.select_all(r##"[href|="#plain"]"##).unwrap().len(), 1);
}

#[test]
fn custom_prefixes_resolve_through_xmlns_bindings() {
    let doc = parse_html(
        r##"<svg xmlns:xl="http://www.w3.org/1999/xlink"><a xl:href="#custom">x</a></svg>"##,
    );
    let a = doc.select("a").unwrap().expect("svg link");

    assert_eq!(doc.get_attribute(a, "xl:href"), Some("#custom"));
    assert_eq!(
        doc.get_attribute_ns(a, AttributeNamespace::XLink, "href"),
        Some("#custom")
    );
    assert_eq!(
        doc.attribute_namespace_by_name(a, "xl:href"),
        Some(AttributeNamespace::XLink)
    );
    assert_eq!(doc.select_all("[xlink|href]").unwrap().len(), 1);
    assert_eq!(
        doc.select_all(r##"a[xlink|href="#custom"]"##)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn namespace_aware_mutation_helpers() {
    let mut doc = Document::new();
    let root = doc.document_node();
    let a = doc.create_element_ns("a", Namespace::Svg, Vec::new());
    doc.append_child(root, a).unwrap();

    doc.set_attribute_ns(a, AttributeNamespace::XLink, "href", "#set")
        .unwrap();
    assert_eq!(doc.get_attribute(a, "xlink:href"), Some("#set"));
    assert_eq!(
        doc.get_attribute_ns(a, AttributeNamespace::XLink, "href"),
        Some("#set")
    );
    assert_eq!(doc.outer_html(a), r##"<a xlink:href="#set"></a>"##);

    // Setting again updates in place instead of duplicating.
    doc.set_attribute_ns(a, AttributeNamespace::XLink, "href", "#updated")
        .unwrap();
    assert_eq!(doc.element(a).unwrap().attributes.len(), 1);
    assert_eq!(
        doc.get_attribute_ns(a, AttributeNamespace::XLink, "href"),
        Some("#updated")
    );

    assert_eq!(
        doc.remove_attribute_ns(a, AttributeNamespace::XLink, "href")
            .unwrap(),
        Some("#updated".to_owned())
    );
    assert_eq!(doc.get_attribute(a, "xlink:href"), None);

    assert_eq!(
        Attribute::new_ns(AttributeNamespace::Xml, "space", "preserve").name,
        "xml:space"
    );
    assert_eq!(
        Attribute::new_ns(
            AttributeNamespace::Xmlns,
            "xlink",
            AttributeNamespace::XLINK_URI
        )
        .name,
        "xmlns:xlink"
    );
    assert_eq!(
        Attribute::new_ns(
            AttributeNamespace::Xmlns,
            "xmlns",
            AttributeNamespace::XMLNS_URI
        )
        .name,
        "xmlns"
    );
}
