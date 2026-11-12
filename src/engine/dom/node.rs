use std::fmt;

/// Stable handle to a node inside a [`Document`](super::Document) arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub(crate) usize);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl NodeId {
    pub(crate) fn index(self) -> usize {
        self.0
    }

    pub(crate) fn from_index(index: usize) -> Self {
        NodeId(index)
    }
}

/// Namespace of a foreign (non-HTML) attribute.
///
/// Attributes keep their qualified name (`xlink:href`, `xml:space`,
/// `xmlns:xlink`) as the single source of truth; the namespace is derived from
/// the prefix (and, for non-standard prefixes, from in-scope `xmlns:` bindings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AttributeNamespace {
    /// No namespace (ordinary HTML/SVG attributes).
    #[default]
    None,
    /// `http://www.w3.org/1999/xlink` (`xlink:*`).
    XLink,
    /// `http://www.w3.org/XML/1998/namespace` (`xml:*`).
    Xml,
    /// `http://www.w3.org/2000/xmlns/` (`xmlns` / `xmlns:*`).
    Xmlns,
}

impl AttributeNamespace {
    pub const XLINK_URI: &'static str = "http://www.w3.org/1999/xlink";
    pub const XML_URI: &'static str = "http://www.w3.org/XML/1998/namespace";
    pub const XMLNS_URI: &'static str = "http://www.w3.org/2000/xmlns/";

    /// Namespace URI, or `None` for attributes outside any namespace.
    pub fn uri(self) -> Option<&'static str> {
        match self {
            AttributeNamespace::None => None,
            AttributeNamespace::XLink => Some(Self::XLINK_URI),
            AttributeNamespace::Xml => Some(Self::XML_URI),
            AttributeNamespace::Xmlns => Some(Self::XMLNS_URI),
        }
    }

    /// Canonical prefix used when constructing a qualified name.
    pub fn prefix(self) -> Option<&'static str> {
        match self {
            AttributeNamespace::None => None,
            AttributeNamespace::XLink => Some("xlink"),
            AttributeNamespace::Xml => Some("xml"),
            AttributeNamespace::Xmlns => Some("xmlns"),
        }
    }

    /// Well-known namespace prefix (`xlink`, `xml`, `xmlns`), if recognised.
    /// Prefixes that are merely declared via `xmlns:prefix` are resolved
    /// separately through [`AttributeNamespace::from_uri`].
    pub fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix.to_ascii_lowercase().as_str() {
            "xlink" => Some(AttributeNamespace::XLink),
            "xml" => Some(AttributeNamespace::Xml),
            "xmlns" => Some(AttributeNamespace::Xmlns),
            _ => None,
        }
    }

    /// Namespace for an `xmlns:prefix` binding URI.
    pub fn from_uri(uri: &str) -> Option<Self> {
        match uri.trim() {
            Self::XLINK_URI => Some(AttributeNamespace::XLink),
            Self::XML_URI => Some(AttributeNamespace::Xml),
            Self::XMLNS_URI => Some(AttributeNamespace::Xmlns),
            _ => None,
        }
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

    /// Create an attribute with a qualified name for `namespace` + `local_name`.
    ///
    /// `AttributeNamespace::None` yields the bare local name; the other
    /// namespaces prepend their canonical prefix (`xlink:href`, `xml:space`,
    /// `xmlns:xlink`).
    pub fn new_ns(
        namespace: AttributeNamespace,
        local_name: impl AsRef<str>,
        value: impl Into<String>,
    ) -> Self {
        Self::new(
            namespace_qualified_name(namespace, local_name.as_ref()),
            value,
        )
    }

    /// The name as authored, including any prefix (`xlink:href`).
    pub fn qualified_name(&self) -> &str {
        &self.name
    }

    /// Namespace prefix, if the qualified name contains a `:`.
    pub fn prefix(&self) -> Option<&str> {
        self.name.split_once(':').map(|(prefix, _)| prefix)
    }

    /// Local name without the namespace prefix (`href` for `xlink:href`).
    pub fn local_name(&self) -> &str {
        self.name
            .split_once(':')
            .map(|(_, local)| local)
            .unwrap_or(self.name.as_str())
    }

    /// Namespace implied by the well-known prefix (`xlink`, `xml`, `xmlns`).
    ///
    /// Custom prefixes declared through `xmlns:prefix` are resolved by
    /// [`Element::attribute_namespace`]/[`Document::attribute_namespace`].
    pub fn namespace(&self) -> AttributeNamespace {
        if self.name.eq_ignore_ascii_case("xmlns") {
            return AttributeNamespace::Xmlns;
        }
        match self.prefix() {
            Some(prefix) => AttributeNamespace::from_prefix(prefix).unwrap_or_default(),
            None => AttributeNamespace::None,
        }
    }
}

fn namespace_qualified_name(namespace: AttributeNamespace, local_name: &str) -> String {
    match (namespace, namespace.prefix()) {
        (AttributeNamespace::None, _) => local_name.to_owned(),
        // `xmlns` itself is unprefixed; `xmlns:xlink` declares a binding.
        (AttributeNamespace::Xmlns, _) if local_name.eq_ignore_ascii_case("xmlns") => {
            "xmlns".to_owned()
        }
        (_, Some(prefix)) => format!("{prefix}:{local_name}"),
        (_, None) => local_name.to_owned(),
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

    /// Look up the first attribute matching `qualified_name` (`xlink:href`).
    pub fn find_attribute(&self, qualified_name: &str) -> Option<&Attribute> {
        self.attributes
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(qualified_name))
    }

    /// Namespace of `attr`, resolving non-standard prefixes through
    /// `xmlns:prefix` declarations on this element.
    pub fn attribute_namespace(&self, attr: &Attribute) -> AttributeNamespace {
        let namespace = attr.namespace();
        if namespace != AttributeNamespace::None || attr.prefix().is_none() {
            return namespace;
        }
        let prefix = attr.prefix().unwrap_or_default();
        self.namespace_binding(prefix)
            .and_then(AttributeNamespace::from_uri)
            .unwrap_or_default()
    }

    /// URI declared by `xmlns:prefix` on this element.
    pub fn namespace_binding(&self, prefix: &str) -> Option<&str> {
        let declared = format!("xmlns:{prefix}");
        self.get_attribute(&declared)
    }

    /// Look up an attribute by namespace + local name (`XLink`, `href`).
    ///
    /// Only bindings declared on this element are considered; the document
    /// level [`Document::get_attribute_ns`](super::Document::get_attribute_ns)
    /// also honours ancestor declarations.
    pub fn get_attribute_ns(
        &self,
        namespace: AttributeNamespace,
        local_name: &str,
    ) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| {
                a.local_name().eq_ignore_ascii_case(local_name)
                    && self.attribute_namespace(a) == namespace
            })
            .map(|a| a.value.as_str())
    }

    pub fn has_attribute_ns(&self, namespace: AttributeNamespace, local_name: &str) -> bool {
        self.get_attribute_ns(namespace, local_name).is_some()
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

    /// Set an attribute addressed by namespace + local name.
    ///
    /// An existing attribute in the same namespace replaces its value and keeps
    /// its original prefix; otherwise a new attribute with the canonical prefix
    /// is appended.
    pub fn set_attribute_ns(
        &mut self,
        namespace: AttributeNamespace,
        local_name: &str,
        value: &str,
    ) {
        let existing = self.attributes.iter().position(|a| {
            a.local_name().eq_ignore_ascii_case(local_name)
                && self.attribute_namespace(a) == namespace
        });
        match existing {
            Some(pos) => self.attributes[pos].value = value.to_owned(),
            None => self
                .attributes
                .push(Attribute::new_ns(namespace, local_name, value)),
        }
    }

    pub fn remove_attribute(&mut self, name: &str) -> Option<String> {
        let pos = self
            .attributes
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(name))?;
        Some(self.attributes.remove(pos).value)
    }

    /// Remove an attribute addressed by namespace + local name.
    pub fn remove_attribute_ns(
        &mut self,
        namespace: AttributeNamespace,
        local_name: &str,
    ) -> Option<String> {
        let pos = self.attributes.iter().position(|a| {
            a.local_name().eq_ignore_ascii_case(local_name)
                && self.attribute_namespace(a) == namespace
        })?;
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
