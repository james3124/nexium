use std::fmt::Write as _;

use super::{Document, NodeData, NodeId, is_rawtext_element, is_void_element};

pub(crate) fn serialize_document(doc: &Document) -> String {
    let mut out = String::new();
    for &child in doc.children(doc.document_node()) {
        write_node(doc, child, &mut out, false);
    }
    out
}

pub(crate) fn serialize_node(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    write_node(doc, id, &mut out, false);
    out
}

pub(crate) fn serialize_children(doc: &Document, id: NodeId) -> String {
    let rawtext = doc
        .element(id)
        .map(|el| is_rawtext_element(&el.name))
        .unwrap_or(false);
    let mut out = String::new();
    for &child in doc.children(id) {
        write_node(doc, child, &mut out, rawtext);
    }
    out
}

fn write_node(doc: &Document, id: NodeId, out: &mut String, in_rawtext: bool) {
    match &doc.node(id).data {
        NodeData::Document | NodeData::DocumentFragment => {
            for &child in doc.children(id) {
                write_node(doc, child, out, false);
            }
        }
        NodeData::Doctype(dt) => {
            out.push_str("<!DOCTYPE");
            if !dt.name.is_empty() {
                out.push(' ');
                out.push_str(&dt.name);
            }
            if !dt.public_id.is_empty() || !dt.system_id.is_empty() {
                out.push_str(" PUBLIC \"");
                escape_attr_into(&dt.public_id, out);
                out.push_str("\" \"");
                escape_attr_into(&dt.system_id, out);
                out.push('"');
            }
            out.push('>');
        }
        NodeData::Comment(data) => {
            out.push_str("<!--");
            out.push_str(data);
            out.push_str("-->");
        }
        NodeData::Text(text) => {
            if in_rawtext {
                out.push_str(text);
            } else {
                escape_text_into(text, out);
            }
        }
        NodeData::Element(el) => {
            let _ = write!(out, "<{}", el.name);
            for attr in &el.attributes {
                // Qualified names are serialized verbatim so foreign attribute
                // namespaces survive a parse -> serialize round-trip
                // (`xlink:href`, `xml:space`, `xmlns:xlink`).
                out.push(' ');
                out.push_str(attr.qualified_name());
                out.push_str("=\"");
                escape_attr_into(&attr.value, out);
                out.push('"');
            }
            out.push('>');

            if is_void_element(&el.name) {
                return;
            }

            let rawtext = is_rawtext_element(&el.name);
            let content = el.template_contents.unwrap_or(id);
            for &child in doc.children(content) {
                write_node(doc, child, out, rawtext);
            }
            let _ = write!(out, "</{}>", el.name);
        }
    }
}

fn escape_text_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{00A0}' => out.push_str("&nbsp;"),
            other => out.push(other),
        }
    }
}

fn escape_attr_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\u{00A0}' => out.push_str("&nbsp;"),
            other => out.push(other),
        }
    }
}
