use super::node::{
    Attribute, AttributeNamespace, Doctype, Element, Namespace, Node, NodeData, NodeId,
};
use super::{DomError, selectors, serialize};

/// An arena-backed DOM document.
///
/// Nodes are addressed by [`NodeId`] and are only mutated through the
/// `Document` API so that parent/child invariants stay intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuirksMode {
    NoQuirks,
    LimitedQuirks,
    Quirks,
}

#[derive(Debug, Clone)]
pub struct Document {
    nodes: Vec<Node>,
    quirks_mode: QuirksMode,
    hover_target: Option<NodeId>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Self {
            nodes: vec![Node {
                parent: None,
                children: Vec::new(),
                data: NodeData::Document,
            }],
            quirks_mode: QuirksMode::NoQuirks,
            hover_target: None,
        }
    }

    /// Element currently under the synthetic pointer, used for `:hover`.
    pub fn hover_target(&self) -> Option<NodeId> {
        self.hover_target
    }

    pub(crate) fn set_hover_target(&mut self, target: Option<NodeId>) {
        self.hover_target = target;
    }

    pub fn quirks_mode(&self) -> QuirksMode {
        self.quirks_mode
    }

    /// `document.compatMode` equivalent.
    pub fn compat_mode(&self) -> &'static str {
        match self.quirks_mode {
            QuirksMode::Quirks => "BackCompat",
            QuirksMode::NoQuirks | QuirksMode::LimitedQuirks => "CSS1Compat",
        }
    }

    pub(crate) fn set_quirks_mode(&mut self, mode: QuirksMode) {
        self.quirks_mode = mode;
    }

    pub fn document_node(&self) -> NodeId {
        NodeId(0)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn element_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_element()).count()
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0]
    }

    pub fn try_node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0)
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id.0).and_then(|n| n.parent)
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.0].children
    }

    pub fn element(&self, id: NodeId) -> Option<&Element> {
        self.nodes.get(id.0).and_then(Node::element)
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        self.nodes.get_mut(id.0).and_then(|n| match &mut n.data {
            NodeData::Element(el) => Some(el),
            _ => None,
        })
    }

    pub fn is_element(&self, id: NodeId) -> bool {
        self.nodes.get(id.0).is_some_and(Node::is_element)
    }

    pub fn tag_name(&self, id: NodeId) -> Option<&str> {
        self.element(id).map(|el| el.name.as_str())
    }

    pub fn create_element(&mut self, name: &str) -> NodeId {
        self.create_node(NodeData::Element(Element::new(name.to_ascii_lowercase())))
    }

    pub(crate) fn create_element_with_attributes(
        &mut self,
        name: &str,
        attributes: Vec<Attribute>,
    ) -> NodeId {
        self.create_element_ns(&name.to_ascii_lowercase(), Namespace::Html, attributes)
    }

    pub fn create_element_ns(
        &mut self,
        name: &str,
        namespace: Namespace,
        attributes: Vec<Attribute>,
    ) -> NodeId {
        let mut element = Element::new(name);
        element.namespace = namespace;
        element.attributes = attributes;
        self.create_node(NodeData::Element(element))
    }

    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.create_node(NodeData::Text(text.into()))
    }

    pub fn create_comment(&mut self, text: impl Into<String>) -> NodeId {
        self.create_node(NodeData::Comment(text.into()))
    }

    pub fn create_doctype(&mut self, name: &str, public_id: &str, system_id: &str) -> NodeId {
        self.create_node(NodeData::Doctype(Doctype {
            name: name.to_owned(),
            public_id: public_id.to_owned(),
            system_id: system_id.to_owned(),
        }))
    }

    pub fn create_document_fragment(&mut self) -> NodeId {
        self.create_node(NodeData::DocumentFragment)
    }

    pub fn template_content(&self, template: NodeId) -> Option<NodeId> {
        self.element(template)?.template_contents
    }

    fn create_node(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(Node {
            parent: None,
            children: Vec::new(),
            data,
        });
        id
    }

    /// Append `child` to `parent`, detaching it from any previous parent.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), DomError> {
        self.ensure_exists(parent)?;
        self.ensure_exists(child)?;
        self.check_can_have_children(parent)?;
        self.check_appendable(parent, child)?;

        if let Some(old_parent) = self.nodes[child.0].parent {
            self.detach(old_parent, child);
        }
        self.nodes[parent.0].children.push(child);
        self.nodes[child.0].parent = Some(parent);
        Ok(())
    }

    /// Insert `child` before `reference` (append when `reference` is `None`).
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        child: NodeId,
        reference: Option<NodeId>,
    ) -> Result<(), DomError> {
        let Some(reference) = reference else {
            return self.append_child(parent, child);
        };
        self.ensure_exists(parent)?;
        self.ensure_exists(child)?;
        self.check_can_have_children(parent)?;
        self.check_appendable(parent, child)?;
        if child == reference {
            return Ok(());
        }
        if !self.nodes[parent.0].children.contains(&reference) {
            return Err(DomError::NotAChild(reference, parent));
        }

        if let Some(old_parent) = self.nodes[child.0].parent {
            self.detach(old_parent, child);
        }
        let pos = self.nodes[parent.0]
            .children
            .iter()
            .position(|&c| c == reference)
            .ok_or(DomError::NotAChild(reference, parent))?;
        self.nodes[parent.0].children.insert(pos, child);
        self.nodes[child.0].parent = Some(parent);
        Ok(())
    }

    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), DomError> {
        self.ensure_exists(parent)?;
        self.ensure_exists(child)?;
        if !self.nodes[parent.0].children.contains(&child) {
            return Err(DomError::NotAChild(child, parent));
        }
        self.detach(parent, child);
        Ok(())
    }

    fn detach(&mut self, parent: NodeId, child: NodeId) {
        self.nodes[parent.0].children.retain(|&c| c != child);
        self.nodes[child.0].parent = None;
    }

    fn check_can_have_children(&self, parent: NodeId) -> Result<(), DomError> {
        match self.nodes[parent.0].data {
            NodeData::Element(_) | NodeData::Document | NodeData::DocumentFragment => Ok(()),
            _ => Err(DomError::ParentCannotHaveChildren(parent)),
        }
    }

    fn check_appendable(&self, parent: NodeId, child: NodeId) -> Result<(), DomError> {
        if parent == child {
            return Err(DomError::Cycle);
        }
        if matches!(
            self.nodes[child.0].data,
            NodeData::Document | NodeData::DocumentFragment
        ) {
            return Err(DomError::DocumentAsChild);
        }
        let mut current = Some(parent);
        while let Some(id) = current {
            if id == child {
                return Err(DomError::Cycle);
            }
            current = self.nodes[id.0].parent;
        }
        Ok(())
    }

    fn ensure_exists(&self, id: NodeId) -> Result<(), DomError> {
        if id.0 < self.nodes.len() {
            Ok(())
        } else {
            Err(DomError::NodeNotFound(id))
        }
    }

    pub fn set_attribute(&mut self, id: NodeId, name: &str, value: &str) -> Result<(), DomError> {
        let el = self.element_mut(id).ok_or(DomError::NotAnElement(id))?;
        el.set_attribute(name, value);
        Ok(())
    }

    pub fn get_attribute<'a>(&'a self, id: NodeId, name: &str) -> Option<&'a str> {
        self.element(id)?.get_attribute(name)
    }

    /// Resolve the namespace of `attribute` on element `id`, honouring
    /// `xmlns:prefix` declarations on the element and its ancestors.
    pub fn attribute_namespace(&self, id: NodeId, attribute: &Attribute) -> AttributeNamespace {
        let namespace = attribute.namespace();
        if namespace != AttributeNamespace::None || attribute.prefix().is_none() {
            return namespace;
        }
        let prefix = attribute.prefix().unwrap_or_default();
        for node in std::iter::once(id).chain(self.ancestors(id)) {
            let Some(element) = self.element(node) else {
                continue;
            };
            if let Some(uri) = element.namespace_binding(prefix)
                && let Some(namespace) = AttributeNamespace::from_uri(uri)
            {
                return namespace;
            }
        }
        AttributeNamespace::None
    }

    /// Namespace of the attribute with the given qualified name, if present.
    pub fn attribute_namespace_by_name(
        &self,
        id: NodeId,
        qualified_name: &str,
    ) -> Option<AttributeNamespace> {
        let attribute = self.element(id)?.find_attribute(qualified_name)?;
        Some(self.attribute_namespace(id, attribute))
    }

    /// Look up an attribute by namespace + local name, e.g.
    /// `get_attribute_ns(a, AttributeNamespace::XLink, "href")`.
    pub fn get_attribute_ns<'a>(
        &'a self,
        id: NodeId,
        namespace: AttributeNamespace,
        local_name: &str,
    ) -> Option<&'a str> {
        let element = self.element(id)?;
        element
            .attributes
            .iter()
            .find(|attribute| {
                attribute.local_name().eq_ignore_ascii_case(local_name)
                    && self.attribute_namespace(id, attribute) == namespace
            })
            .map(|attribute| attribute.value.as_str())
    }

    pub fn has_attribute_ns(
        &self,
        id: NodeId,
        namespace: AttributeNamespace,
        local_name: &str,
    ) -> bool {
        self.get_attribute_ns(id, namespace, local_name).is_some()
    }

    pub fn set_attribute_ns(
        &mut self,
        id: NodeId,
        namespace: AttributeNamespace,
        local_name: &str,
        value: &str,
    ) -> Result<(), DomError> {
        let el = self.element_mut(id).ok_or(DomError::NotAnElement(id))?;
        el.set_attribute_ns(namespace, local_name, value);
        Ok(())
    }

    pub fn remove_attribute_ns(
        &mut self,
        id: NodeId,
        namespace: AttributeNamespace,
        local_name: &str,
    ) -> Result<Option<String>, DomError> {
        let el = self.element_mut(id).ok_or(DomError::NotAnElement(id))?;
        Ok(el.remove_attribute_ns(namespace, local_name))
    }

    pub fn remove_attribute(&mut self, id: NodeId, name: &str) -> Result<Option<String>, DomError> {
        let el = self.element_mut(id).ok_or(DomError::NotAnElement(id))?;
        Ok(el.remove_attribute(name))
    }

    pub fn id_attribute(&self, id: NodeId) -> Option<&str> {
        self.element(id)?.id()
    }

    pub fn get_element_by_id(&self, id: &str) -> Option<NodeId> {
        self.descendants(self.document_node())
            .into_iter()
            .find(|&node| self.element(node).and_then(Element::id) == Some(id))
    }

    /// Pre-order traversal of descendants (does not include `root`).
    pub fn descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.nodes[root.0].children.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            out.push(id);
            stack.extend(self.nodes[id.0].children.iter().rev().copied());
        }
        out
    }

    /// Ancestors starting with the immediate parent.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut current = self.nodes.get(id.0).and_then(|n| n.parent);
        while let Some(node) = current {
            out.push(node);
            current = self.nodes[node.0].parent;
        }
        out
    }

    /// Text content of a node (concatenation of all descendant text nodes).
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id.0].data {
            NodeData::Text(text) => out.push_str(text),
            NodeData::Element(_) | NodeData::Document | NodeData::DocumentFragment => {
                for &child in &self.nodes[id.0].children {
                    self.collect_text(child, out);
                }
            }
            NodeData::Doctype(_) | NodeData::Comment(_) => {}
        }
    }

    /// Replace all children of `id` with a single text node (unless empty).
    pub fn set_text_content(&mut self, id: NodeId, text: &str) -> Result<(), DomError> {
        self.ensure_exists(id)?;
        self.check_can_have_children(id)?;
        let children = std::mem::take(&mut self.nodes[id.0].children);
        for child in children {
            self.nodes[child.0].parent = None;
        }
        if !text.is_empty() {
            let text_id = self.create_text(text);
            self.nodes[id.0].children.push(text_id);
            self.nodes[text_id.0].parent = Some(id);
        }
        Ok(())
    }

    pub fn inner_html(&self, id: NodeId) -> String {
        match &self.node(id).data {
            NodeData::Element(el) if el.is_void() => String::new(),
            NodeData::Element(el) => match el.template_contents {
                Some(content) => serialize::serialize_children(self, content),
                None => serialize::serialize_children(self, id),
            },
            NodeData::Text(_) | NodeData::Comment(_) | NodeData::Doctype(_) => String::new(),
            _ => serialize::serialize_children(self, id),
        }
    }

    pub fn outer_html(&self, id: NodeId) -> String {
        serialize::serialize_node(self, id)
    }

    pub fn serialize(&self) -> String {
        serialize::serialize_document(self)
    }

    pub fn title(&self) -> Option<String> {
        let title_id = self
            .descendants(self.document_node())
            .into_iter()
            .find(|&id| self.tag_name(id) == Some("title"))?;
        Some(self.text_content(title_id))
    }

    pub fn query_selector(&self, root: NodeId, selector: &str) -> Result<Option<NodeId>, DomError> {
        selectors::parse_and_query_one(self, root, selector)
    }

    pub fn query_selector_all(
        &self,
        root: NodeId,
        selector: &str,
    ) -> Result<Vec<NodeId>, DomError> {
        selectors::parse_and_query_all(self, root, selector)
    }

    pub fn matches(&self, id: NodeId, selector: &str) -> Result<bool, DomError> {
        selectors::parse_and_match(self, id, selector)
    }

    pub fn select(&self, selector: &str) -> Result<Option<NodeId>, DomError> {
        self.query_selector(self.document_node(), selector)
    }

    pub fn select_all(&self, selector: &str) -> Result<Vec<NodeId>, DomError> {
        self.query_selector_all(self.document_node(), selector)
    }
}
