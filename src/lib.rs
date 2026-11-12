//! NexiumX — an independent, Rust-powered headless browser engine.
//!
//! Phase 1: URL handling, HTTP(S) networking, HTML tokenizer/tree builder,
//! DOM with query selectors and serialization, and page inspection.
//!
//! Phase 2: CSS parsing and cascade, computed styles, block/inline/flex/table
//! layout, text measurement, headless rasterization and screenshots.
//!
//! Phase 3: JavaScript execution through Boa with DOM bindings, events,
//! timers/promises, fetch/XHR and script loading during page load.
//!
//! Phase 4: tabs, navigation history, isolated profiles, persistent cookies,
//! local storage, HTTP cache and IndexedDB, plus session save/restore.
//!
//! Phase 5: viewport/scroll management, geometric hit testing, synthetic mouse
//! and keyboard events against live page scripts, element detection,
//! accessibility extraction and structured DOM snapshots.
//!
//! Phase 6: authenticated REST and WebSocket control plane, isolated agent
//! sessions, network/console event streams, and a native MCP server exposing
//! the browser tool set over stdio or HTTP JSON-RPC.
//!
//! Phase 7: basic CSS Grid, advanced flex directions, SVG and canvas painting,
//! page WebSocket and Worker APIs, matchMedia/getComputedStyle and improved
//! responsive behavior.
//!
//! Phase 8: tab/session/idle limits, bounded network logging, optional SSRF
//! blocking, graceful profile session recovery, resource cleanup and stress
//! coverage.

pub mod api;
pub mod browser;
pub mod config;
pub mod engine;
pub mod intelligence;
pub mod interaction;
pub mod mcp;
pub mod networking;
pub mod rendering;
pub mod storage;
pub mod utils;

pub use api::{ApiError, ApiService, AuthContext, serve as serve_api};
pub use browser::{Browser, Page};
pub use config::Config;
pub use engine::dom::{Document, DomError, NodeId};
pub use engine::html::parse_html;
pub use interaction::{
    AccessibilityNode, DomSnapshot, ElementInfo, ElementRect, ExtractedField, ExtractedLink,
    ExtractedPage, InputResult, MouseButton, PageSession, extract_page,
};
pub use networking::{HttpClient, Request, Response, parse_url};
pub use rendering::{RenderOptions, RenderedPage, layout_page, render_page};
pub use utils::{NxError, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
