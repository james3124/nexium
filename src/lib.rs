//! NEXIUM — an independent, Rust-powered headless browser engine.
//!
//! Phase 1: URL handling, HTTP(S) networking, HTML tokenizer/tree builder,
//! DOM with query selectors and serialization, and page inspection.
//!
//! Phase 2: CSS parsing and cascade, computed styles, block/inline/flex
//! layout, text measurement, headless rasterization and screenshots.

pub mod browser;
pub mod config;
pub mod engine;
pub mod intelligence;
pub mod networking;
pub mod rendering;
pub mod utils;

pub use browser::{Browser, Page};
pub use config::Config;
pub use engine::dom::{Document, DomError, NodeId};
pub use engine::html::parse_html;
pub use networking::{HttpClient, Request, Response, parse_url};
pub use rendering::{RenderOptions, RenderedPage, render_page};
pub use utils::{NxError, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
