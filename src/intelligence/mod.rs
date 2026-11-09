//! Page intelligence: compact, structured inspection for agents.

pub mod dom;

pub use dom::{collapse_whitespace, dump_tree, visible_text};
