//! HTML tokenizer and tree builder.

mod entities;
pub mod parser;
pub mod tokenizer;

pub use parser::parse_html;
pub use tokenizer::{Token, Tokenizer, decode_entities_in_text, parse_char_ref_at};
