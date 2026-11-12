//! Adoption agency / active formatting reconstruction tests.
//!
//! Expected trees follow the WHATWG adoption agency algorithm; the same
//! serialisations are produced by html5lib and mainstream browsers.

use nexium::engine::dom::Document;
use nexium::parse_html;

/// Serialised children of `<body>` for `source`, with the implied
/// `html`/`head`/`body` wrapper stripped.
fn body_html(source: &str) -> String {
    let doc = parse_html(source);
    let html = doc.serialize();
    let prefix = "<html><head></head><body>";
    let suffix = "</body></html>";
    let inner = html
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("unexpected document prefix in {html}"));
    inner
        .strip_suffix(suffix)
        .unwrap_or_else(|| panic!("unexpected document suffix in {html}"))
        .to_owned()
}

fn body_tags(doc: &Document) -> Vec<&str> {
    let body = doc.select("body").unwrap().unwrap();
    doc.children(body)
        .iter()
        .filter_map(|&id| doc.tag_name(id))
        .collect()
}

#[test]
fn end_tag_reopens_formatting_inside_open_paragraph() {
    assert_eq!(
        body_html("<b><p>one</b>two</p>"),
        "<b></b><p><b>one</b>two</p>"
    );

    // DOM shape: the empty <b> stays at body level, the paragraph keeps the
    // text and the reconstructed <b> wraps only the first run of text.
    let doc = parse_html("<b><p>one</b>two</p>");
    assert_eq!(body_tags(&doc), vec!["b", "p"]);
    let p = doc.select("p").unwrap().unwrap();
    let children = doc.children(p);
    assert_eq!(children.len(), 2);
    assert_eq!(doc.tag_name(children[0]), Some("b"));
    assert_eq!(doc.text_content(children[0]), "one");
    assert_eq!(doc.text_content(children[1]), "two");
}

#[test]
fn anchor_end_tag_closes_nested_bold_and_reconstructs_it() {
    assert_eq!(
        body_html("<a><b>one</a>two</b>"),
        "<a><b>one</b></a><b>two</b>"
    );

    let doc = parse_html("<a><b>one</a>two</b>");
    assert_eq!(body_tags(&doc), vec!["a", "b"]);
    let a = doc.select("a").unwrap().unwrap();
    assert_eq!(doc.text_content(a), "one");
    assert_eq!(doc.tag_name(doc.children(a)[0]), Some("b"));
}

#[test]
fn formatting_element_nested_through_multiple_blocks() {
    assert_eq!(
        body_html("<b><div><i>one</b>two</i></div>"),
        "<b></b><div><b><i>one</i></b><i>two</i></div>"
    );

    // The clone of <b> now lives inside the div and still wraps <i>.
    let doc = parse_html("<b><div><i>one</b>two</i></div>");
    assert_eq!(body_tags(&doc), vec!["b", "div"]);
    let div = doc.select("div").unwrap().unwrap();
    let div_children = doc.children(div);
    assert_eq!(div_children.len(), 2);
    assert_eq!(doc.tag_name(div_children[0]), Some("b"));
    assert_eq!(doc.tag_name(div_children[1]), Some("i"));
    let bold = div_children[0];
    assert_eq!(doc.tag_name(doc.children(bold)[0]), Some("i"));
    assert_eq!(doc.text_content(bold), "one");
    assert_eq!(doc.text_content(div_children[1]), "two");
}

#[test]
fn empty_misnested_formatting_elements_close_in_order() {
    assert_eq!(body_html("<b><i></b></i>"), "<b><i></i></b>");

    let doc = parse_html("<b><i></b></i>");
    let b = doc.select("b").unwrap().unwrap();
    assert_eq!(doc.tag_name(doc.children(b)[0]), Some("i"));
}

#[test]
fn misnested_bold_and_italic_split_around_the_end_tag() {
    assert_eq!(
        body_html("<p><b>1<i>2</b>3</i>4</p>"),
        "<p><b>1<i>2</i></b><i>3</i>4</p>"
    );
    // The example used by the HTML standard for the adoption agency.
    assert_eq!(
        body_html("<p>1<b>2<i>3</b>4</i>5</p>"),
        "<p>1<b>2<i>3</i></b><i>4</i>5</p>"
    );

    let doc = parse_html("<p><b>1<i>2</b>3</i>4</p>");
    let p = doc.select("p").unwrap().unwrap();
    let children = doc.children(p);
    assert_eq!(children.len(), 3);
    assert_eq!(doc.tag_name(children[0]), Some("b"));
    assert_eq!(doc.tag_name(children[1]), Some("i"));
    assert_eq!(doc.text_content(children[1]), "3");
    assert_eq!(doc.tag_name(children[2]), None); // trailing text node
    assert_eq!(doc.text_content(children[2]), "4");
}

#[test]
fn formatting_is_reconstructed_after_a_block_boundary() {
    // Closing the block pops the formatting element from the stack, but the
    // active formatting list still re-creates it for the following text.
    assert_eq!(body_html("<div><b>1</div>2"), "<div><b>1</b></div><b>2</b>");
    assert_eq!(
        body_html("<b>1<div>2</b>3</div>"),
        "<b>1</b><div><b>2</b>3</div>"
    );
    assert_eq!(
        body_html("<b><span>1<div>2</b>3</div>"),
        "<b><span>1</span></b><div><b>2</b>3</div>"
    );

    let doc = parse_html("<div><b>1</div>2");
    assert_eq!(body_tags(&doc), vec!["div", "b"]);
    let body = doc.select("body").unwrap().unwrap();
    let reconstructed = doc.children(body)[1];
    assert_eq!(doc.tag_name(reconstructed), Some("b"));
    assert_eq!(doc.text_content(reconstructed), "2");
    let div_bold = doc.select("div b").unwrap().unwrap();
    assert_eq!(doc.text_content(div_bold), "1");
}

#[test]
fn inner_loop_reparents_intermediate_formatting_elements() {
    assert_eq!(
        body_html("<b><i><div>x</b>y</div>"),
        "<b><i></i></b><i><div><b>x</b>y</div></i>"
    );

    // <i> was cloned and moved under the common ancestor, carrying <div>.
    let doc = parse_html("<b><i><div>x</b>y</div>");
    assert_eq!(body_tags(&doc), vec!["b", "i"]);
    let div = doc.select("div").unwrap().unwrap();
    assert_eq!(doc.tag_name(doc.parent(div).unwrap()), Some("i"));
    // The original <i> stays behind, empty, inside <b>.
    let b = doc.select("b").unwrap().unwrap();
    assert_eq!(doc.tag_name(doc.children(b)[0]), Some("i"));
    assert_eq!(doc.text_content(b), "");
}

#[test]
fn nested_blocks_receive_a_clone_per_level() {
    assert_eq!(
        body_html("<b><div><div>x</b>y</div>"),
        "<b></b><div><b></b><div><b>x</b>y</div></div>"
    );
    assert_eq!(
        body_html("<b><div><p>x</b>y"),
        "<b></b><div><b></b><p><b>x</b>y</p></div>"
    );
    // Five block levels exercise the outer loop more than once.
    assert_eq!(
        body_html("<b><div><div><div><div>x</b>y"),
        "<b></b><div><b></b><div><b></b><div><b></b><div><b>x</b>y</div></div></div></div>"
    );
}

#[test]
fn attributes_are_preserved_by_reconstruction() {
    assert_eq!(
        body_html("<a href=q>1<div>2</a>3"),
        "<a href=\"q\">1</a><div><a href=\"q\">2</a>3</div>"
    );
    assert_eq!(
        body_html("<b class=x><p>one</b>two</p>"),
        "<b class=\"x\"></b><p><b class=\"x\">one</b>two</p>"
    );

    let doc = parse_html("<a href=q>1<div>2</a>3");
    let links = doc.select_all("a").unwrap();
    assert_eq!(links.len(), 2);
    for link in links {
        assert_eq!(doc.get_attribute(link, "href"), Some("q"));
    }
}

#[test]
fn blocks_and_formatting_combine_across_the_adoption_agency() {
    assert_eq!(
        body_html("<strong>1<div><em>2</strong>3</em></div>"),
        "<strong>1</strong><div><strong><em>2</em></strong><em>3</em></div>"
    );
    assert_eq!(
        body_html("<em><b>1<div>2</em>3</b></div>"),
        "<em><b>1</b></em><b></b><div><b><em>2</em>3</b></div>"
    );
}

#[test]
fn nested_anchor_start_tag_closes_the_previous_anchor() {
    assert_eq!(
        body_html("<a href=x><a href=y>z"),
        "<a href=\"x\"></a><a href=\"y\">z</a>"
    );

    let doc = parse_html("<a href=x><a href=y>z");
    assert_eq!(body_tags(&doc), vec!["a", "a"]);
    let links = doc.select_all("a").unwrap();
    assert_eq!(doc.get_attribute(links[0], "href"), Some("x"));
    assert_eq!(doc.get_attribute(links[1], "href"), Some("y"));
    assert_eq!(doc.text_content(links[1]), "z");
}

#[test]
#[ignore = "developer aid: prints parser output for manual comparison"]
fn debug_dump_actual() {
    let cases = [
        "<b><p>one</b>two</p>",
        "<a><b>one</a>two</b>",
        "<b><div><i>one</b>two</i></div>",
        "<b><i></b></i>",
        "<p><b>1<i>2</b>3</i>4</p>",
        "<p>1<b>2<i>3</b>4</i>5</p>",
        "<div><b>1</div>2",
        "<b>1<div>2</b>3</div>",
        "<b><span>1<div>2</b>3</div>",
        "<b><i><div>x</b>y</div>",
        "<b><div><div>x</b>y</div>",
        "<b><div><p>x</b>y",
        "<b><div><div><div><div>x</b>y",
        "<a href=q>1<div>2</a>3",
        "<b class=x><p>one</b>two</p>",
        "<strong>1<div><em>2</strong>3</em></div>",
        "<em><b>1<div>2</em>3</b></div>",
        "<a href=x><a href=y>z",
        "<b><img></b>x",
        "<b><em><div>x</b>y</em></div>",
        "<b><code><div>x</b>y</code></div>",
        "<u><b>1<u>2</b>3",
        "<i><b>1</i>2</b>",
        "<span><b>1</span>2</b>",
        "<b><li>x</b>y",
        "<b><p>a</b></p><p>b",
        "<b><ul><li>1</b>2</li></ul>",
        "<p>1<b>2<p>3",
        "<p><b><i>x</b></i></p>",
    ];
    for source in cases {
        println!("{source} => {}", body_html(source));
    }
}
