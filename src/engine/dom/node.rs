use std::fmt;

/// Stable handle to a node inside a [`Document`](super::Document) arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub(crate) usize);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub value: String,
}

impl Attribute {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// An HTML element (local name + attributes). Child relationships live on [`Node`].
#[derive(Clone, Debug, Default)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub namespace: Namespace,
    /// `<template>` content lives in a detached document fragment.
    pub template_contents: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Namespace {
    #[default]
    Html,
    Svg,
    MathMl,
}

impl Element {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Vec::new(),
            namespace: Namespace::Html,
            template_contents: None,
        }
    }

    pub fn get_attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .map(|a| a.value.as_str())
    }

    pub fn has_attribute(&self, name: &str) -> bool {
        self.get_attribute(name).is_some()
    }

    /// Replace the value of an existing attribute or append a new one.
    pub fn set_attribute(&mut self, name: &str, value: &str) {
        if let Some(attr) = self
            .attributes
            .iter_mut()
            .find(|a| a.name.eq_ignore_ascii_case(name))
        {
            attr.value = value.to_owned();
        } else {
            self.attributes.push(Attribute::new(name, value));
        }
    }

    pub fn remove_attribute(&mut self, name: &str) -> Option<String> {
        let pos = self
            .attributes
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(name))?;
        Some(self.attributes.remove(pos).value)
    }

    pub fn id(&self) -> Option<&str> {
        self.get_attribute("id")
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.get_attribute("class")
            .unwrap_or("")
            .split_ascii_whitespace()
    }

    pub fn is_void(&self) -> bool {
        is_void_element(&self.name)
    }
}

#[derive(Clone, Debug)]
pub enum NodeData {
    Document,
    DocumentFragment,
    Doctype(Doctype),
    Element(Element),
    Text(String),
    Comment(String),
}

#[derive(Clone, Debug, Default)]
pub struct Doctype {
    pub name: String,
    pub public_id: String,
    pub system_id: String,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
}

impl Node {
    pub fn is_element(&self) -> bool {
        matches!(self.data, NodeData::Element(_))
    }

    pub fn is_text(&self) -> bool {
        matches!(self.data, NodeData::Text(_))
    }

    pub fn is_comment(&self) -> bool {
        matches!(self.data, NodeData::Comment(_))
    }

    pub fn element(&self) -> Option<&Element> {
        match &self.data {
            NodeData::Element(el) => Some(el),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match &self.data {
            NodeData::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// Elements that never have contents in HTML serialisation.
pub const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input",
    "keygen", "link", "meta", "param", "source", "track", "wbr",
];

/// Elements whose contents are serialised without character escaping.
pub const RAWTEXT_ELEMENTS: &[&str] = &[
    "script",
    "style",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "plaintext",
];

pub fn is_void_element(name: &str) -> bool {
    VOID_ELEMENTS.contains(&name)
}

pub fn is_rawtext_element(name: &str) -> bool {
    RAWTEXT_ELEMENTS.contains(&name)
}
