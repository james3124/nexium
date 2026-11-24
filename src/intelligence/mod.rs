//! Page intelligence: compact, structured inspection for agents.

pub mod dom;
pub mod ocr;
pub mod visual;

pub use dom::{collapse_whitespace, dump_tree, visible_text};
pub use ocr::{GlyphOcr, NoOcr, OcrBackend, RecognizedText};
pub use visual::{
    ChangedRegion, ElementLocator, ElementMatch, HeuristicLocator, TextMatch, TextRun,
    VisualComparison, VisualElement, VisualIndex, VisualRect, locator,
};

/// Build the enriched accessibility tree for `page` and return it as JSON.
///
/// This is the public integration point for the API layer (`src/api/service.rs`
/// is owned by another agent). The returned object keeps the existing
/// `AccessibilityNode` field names and adds `tag`, `focused`, `focusable`,
/// `selected`, `live`, `level` and `invalid`.
///
/// Example one-line wiring in `ApiService::snapshot`:
/// `Some(crate::intelligence::accessibility_json(browser.active_page().unwrap(), browser.viewport()))`
pub fn accessibility_json(
    page: &crate::browser::Page,
    viewport: crate::engine::css::Viewport,
) -> serde_json::Value {
    let session = crate::interaction::PageSession::new(page.clone(), viewport);
    serde_json::to_value(session.accessibility_tree()).unwrap_or(serde_json::Value::Null)
}
