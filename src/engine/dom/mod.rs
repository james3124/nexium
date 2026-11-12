//! Independent DOM implementation.

pub mod document;
pub mod node;
pub mod selectors;
mod serialize;

pub use document::{Document, QuirksMode};
pub use node::{
    Attribute, AttributeNamespace, Doctype, Element, Namespace, Node, NodeData, NodeId,
    RAWTEXT_ELEMENTS, VOID_ELEMENTS, is_rawtext_element, is_void_element,
};
pub use selectors::{
    AttributeNamespaceSelector, AttributeOp, AttributeSelector, Combinator, ComplexSelector,
    CompoundSelector, NamespaceSelector, Nth, PseudoClass, Specificity, matches_complex,
    matches_complex_pseudo, parse_selector_list,
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DomError {
    #[error("node {0} does not exist")]
    NodeNotFound(NodeId),

    #[error("node {0} is not a child of node {1}")]
    NotAChild(NodeId, NodeId),

    #[error("node {0} is not an element")]
    NotAnElement(NodeId),

    #[error("node {0} cannot have children")]
    ParentCannotHaveChildren(NodeId),

    #[error("operation would create a cycle in the DOM tree")]
    Cycle,

    #[error("the document node cannot be appended as a child")]
    DocumentAsChild,

    #[error("invalid selector: {0}")]
    InvalidSelector(String),
}
