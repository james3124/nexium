//! DOM inspection helpers for agents and debugging.

use std::fmt::Write as _;

use crate::engine::dom::{Document, NodeData, NodeId};

const SKIP_TEXT_ELEMENTS: &[&str] = &[
    "script", "style", "noscript", "template", "head", "iframe", "object", "embed",
];

/// Build an indented, human-readable tree of the document.
pub fn dump_tree(doc: &Document) -> String {
    let mut out = String::new();
    for &child in doc.children(doc.document_node()) {
        write_node(doc, child, 0, &mut out);
    }
    out
}

fn write_node(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match &doc.node(id).data {
        NodeData::Document | NodeData::DocumentFragment => {
            for &child in doc.children(id) {
                write_node(doc, child, depth, out);
            }
            return;
        }
        NodeData::Doctype(dt) => {
            let _ = writeln!(out, "{indent}<!DOCTYPE {}>", dt.name);
        }
        NodeData::Comment(text) => {
            let _ = writeln!(out, "{indent}<!-- {} -->", truncate(text, 60));
        }
        NodeData::Text(text) => {
            let display = truncate(text, 60);
            let _ = writeln!(out, "{indent}\"{}\"", escape(&display));
        }
        NodeData::Element(el) => {
            let mut descriptor = el.name.clone();
            if let Some(id_attr) = el.id() {
                let _ = write!(descriptor, "#{id_attr}");
            }
            if let Some(class) = el.get_attribute("class") {
                for cls in class.split_ascii_whitespace() {
                    let _ = write!(descriptor, ".{cls}");
                }
            }
            let mut extras: Vec<String> = Vec::new();
            for attr in &el.attributes {
                if matches!(attr.name.as_str(), "id" | "class") {
                    continue;
                }
                extras.push(format!("{}={:?}", attr.name, truncate(&attr.value, 40)));
                if extras.len() == 4 {
                    extras.push("...".to_owned());
                    break;
                }
            }
            if !extras.is_empty() {
                let _ = write!(descriptor, " [{}]", extras.join(" "));
            }
            let _ = writeln!(out, "{indent}{descriptor}");
        }
    }

    for &child in doc.children(id) {
        write_node(doc, child, depth + 1, out);
    }
}

/// Extract visible-ish text: body text with script/style/noscript content
/// removed and whitespace collapsed.
pub fn visible_text(doc: &Document) -> String {
    let root = doc
        .descendants(doc.document_node())
        .into_iter()
        .find(|&id| doc.tag_name(id) == Some("body"))
        .unwrap_or_else(|| doc.document_node());
    let mut out = String::new();
    collect_text(doc, root, &mut out);
    collapse_whitespace(&out)
}

fn collect_text(doc: &Document, id: NodeId, out: &mut String) {
    match &doc.node(id).data {
        NodeData::Text(text) => {
            out.push_str(text);
            out.push(' ');
        }
        NodeData::Element(el) => {
            if SKIP_TEXT_ELEMENTS.contains(&el.name.as_str()) {
                return;
            }
            for &child in doc.children(id) {
                collect_text(doc, child, out);
            }
        }
        NodeData::Document | NodeData::DocumentFragment => {
            for &child in doc.children(id) {
                collect_text(doc, child, out);
            }
        }
        NodeData::Doctype(_) | NodeData::Comment(_) => {}
    }
}

pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}
