//! Layout-grounded visual analysis.
//!
//! The raster produced by the rendering pipeline and the layout tree that
//! produced it are analysed together: text runs and element boxes come from
//! layout (exact CSS-pixel geometry), while the image is sampled to confirm
//! what was actually painted. No model or OCR dependency is needed for DOM
//! text; optional backends can be added later for image/canvas content.

use image::RgbaImage;
use serde::Serialize;

use crate::engine::css::{Display, Visibility};
use crate::engine::dom::{Document, NodeId};
use crate::engine::layout::{LayoutBox, LayoutResult, LineBox, Rect};
use crate::engine::text::FontSet;
use crate::rendering::screenshots::diff_regions;

/// A CSS-pixel rectangle, serialised for tool responses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct VisualRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl From<Rect> for VisualRect {
    fn from(rect: Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

impl VisualRect {
    fn shifted(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x - dx,
            y: self.y - dy,
            ..self
        }
    }

    fn right(&self) -> f32 {
        self.x + self.width
    }

    fn bottom(&self) -> f32 {
        self.y + self.height
    }

    fn intersects(&self, other: &VisualRect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

/// A piece of text as painted, with per-character boxes.
#[derive(Clone, Debug, Serialize)]
pub struct TextRun {
    pub text: String,
    pub rect: VisualRect,
    pub viewport_rect: VisualRect,
    pub char_rects: Vec<VisualRect>,
    pub node_id: Option<usize>,
    pub font_size: f32,
    pub color: [u8; 4],
    pub line_y: f32,
    pub source: String,
}

/// A contiguous text match found on screen.
#[derive(Clone, Debug, Serialize)]
pub struct TextMatch {
    pub text: String,
    pub rect: VisualRect,
    pub viewport_rect: VisualRect,
    pub confidence: f32,
}

/// A UI element candidate derived from layout, computed style and raster.
#[derive(Clone, Debug, Serialize)]
pub struct VisualElement {
    pub node_id: usize,
    pub tag: String,
    pub label: String,
    pub role: Option<String>,
    pub rect: VisualRect,
    pub viewport_rect: VisualRect,
    pub center: (f32, f32),
    pub clickable: bool,
    pub visible: bool,
    pub confidence: f32,
    pub signals: Vec<String>,
}

/// A ranked result from [`VisualIndex::find`].
#[derive(Clone, Debug, Serialize)]
pub struct ElementMatch {
    pub element: VisualElement,
    pub score: f32,
    pub signals: Vec<String>,
}

/// A grouped region of pixels that changed between two screenshots.
#[derive(Clone, Debug, Serialize)]
pub struct ChangedRegion {
    pub rect: VisualRect,
    pub pixels: u64,
}

/// Result of comparing two screenshots.
#[derive(Clone, Debug, Serialize)]
pub struct VisualComparison {
    pub ratio: f32,
    pub changed_regions: Vec<ChangedRegion>,
    pub text_added: Vec<String>,
    pub text_removed: Vec<String>,
}

/// Compare two screenshots, returning the changed fraction and grouped boxes.
pub fn compare_images(
    before: &RgbaImage,
    after: &RgbaImage,
    threshold: u8,
    min_pixels: u64,
) -> VisualComparison {
    let (ratio, regions) = diff_regions(before, after, threshold, min_pixels);
    VisualComparison {
        ratio,
        changed_regions: regions
            .into_iter()
            .map(|region| ChangedRegion {
                rect: VisualRect {
                    x: region.x as f32,
                    y: region.y as f32,
                    width: region.width as f32,
                    height: region.height as f32,
                },
                pixels: region.pixels,
            })
            .collect(),
        text_added: Vec::new(),
        text_removed: Vec::new(),
    }
}

/// Visual index over one rendered viewport.
///
/// Construct this with the same page, layout and scroll offset that produced
/// the image; all viewport rectangles are relative to that scroll position.
pub struct VisualIndex<'a> {
    document: &'a Document,
    layout: &'a LayoutResult,
    image: &'a RgbaImage,
    scroll: (f32, f32),
    viewport: crate::engine::css::Viewport,
    text_runs: Vec<TextRun>,
    elements: Vec<VisualElement>,
}

impl<'a> VisualIndex<'a> {
    pub fn new(
        document: &'a Document,
        layout: &'a LayoutResult,
        image: &'a RgbaImage,
        viewport: crate::engine::css::Viewport,
        scroll: (f32, f32),
    ) -> Self {
        let text_runs = collect_text_runs(layout, scroll);
        let elements = collect_elements(document, layout, image, viewport, scroll);
        Self {
            document,
            layout,
            image,
            scroll,
            viewport,
            text_runs,
            elements,
        }
    }

    pub fn text_runs(&self) -> &[TextRun] {
        &self.text_runs
    }

    pub fn elements(&self) -> &[VisualElement] {
        &self.elements
    }

    pub fn viewport(&self) -> crate::engine::css::Viewport {
        self.viewport
    }

    pub fn scroll(&self) -> (f32, f32) {
        self.scroll
    }

    /// Find visible text, including text split across inline fragments on the
    /// same line. Matching is case-insensitive and returns screen boxes.
    pub fn find_text(&self, query: &str) -> Vec<TextMatch> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let needle = query.to_lowercase();
        let mut lines: Vec<(i32, Vec<&TextRun>)> = Vec::new();
        for run in &self.text_runs {
            let key = (run.line_y / 2.0).round() as i32;
            if let Some(entry) = lines.iter_mut().find(|(y, _)| *y == key) {
                entry.1.push(run);
            } else {
                lines.push((key, vec![run]));
            }
        }
        let mut matches = Vec::new();
        for (_, mut runs) in lines {
            runs.sort_by(|a, b| a.rect.x.total_cmp(&b.rect.x));
            let mut chars: Vec<(char, VisualRect, VisualRect)> = Vec::new();
            for run in runs {
                for (ch, rect) in run.text.chars().zip(run.char_rects.iter()) {
                    chars.push((ch, *rect, run.viewport_rect));
                }
            }
            let text: String = chars.iter().map(|(ch, _, _)| *ch).collect();
            let lowered = text.to_lowercase();
            let mut offset = 0usize;
            while let Some(position) = lowered[offset..].find(&needle) {
                let start = offset + position;
                let end = start + needle.chars().count();
                let selected = &chars[start..end.min(chars.len())];
                if selected.is_empty() {
                    break;
                }
                let mut x = selected[0].1.x;
                let mut y = selected[0].1.y;
                let mut right = selected[0].1.right();
                let mut bottom = selected[0].1.bottom();
                let mut vx = selected[0].2.x;
                let mut vy = selected[0].2.y;
                for (_, rect, viewport_rect) in selected {
                    x = x.min(rect.x);
                    y = y.min(rect.y);
                    right = right.max(rect.right());
                    bottom = bottom.max(rect.bottom());
                    vx = vx.min(viewport_rect.x);
                    vy = vy.min(viewport_rect.y);
                }
                let matched: String = selected.iter().map(|(ch, _, _)| *ch).collect();
                matches.push(TextMatch {
                    text: matched,
                    rect: VisualRect {
                        x,
                        y,
                        width: right - x,
                        height: bottom - y,
                    },
                    viewport_rect: VisualRect {
                        x: vx,
                        y: vy,
                        width: right - x,
                        height: bottom - y,
                    },
                    confidence: 1.0,
                });
                offset = start + needle.len().max(1);
                if offset >= lowered.len() {
                    break;
                }
            }
        }
        matches
    }

    /// Locate elements using the default heuristic locator.
    pub fn find(&self, description: &str, top_k: usize, region: Option<Rect>) -> Vec<ElementMatch> {
        HeuristicLocator.locate(description, &self.elements, self.image, top_k, region)
    }

    /// Locate elements with a caller-supplied locator (for example a model-backed
    /// backend registered elsewhere).
    pub fn find_with(
        &self,
        locator: &dyn ElementLocator,
        description: &str,
        top_k: usize,
        region: Option<Rect>,
    ) -> Vec<ElementMatch> {
        locator.locate(description, &self.elements, self.image, top_k, region)
    }

    /// Extract text from a region using an OCR backend. Combined with
    /// [`Self::text_runs`], this covers both DOM text and painted text.
    pub fn ocr_text(
        &self,
        backend: &dyn crate::intelligence::ocr::OcrBackend,
        region: Option<Rect>,
    ) -> Vec<crate::intelligence::ocr::RecognizedText> {
        let area = region.unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: self.viewport.width,
            height: self.viewport.height,
        });
        let area = Rect {
            x: area.x + self.scroll.0,
            y: area.y + self.scroll.1,
            width: area.width,
            height: area.height,
        };
        backend.recognize(self.image, area)
    }
    /// Deepest element under a viewport point.
    ///
    /// Descendants are visited in pre-order, so a plain `find` would return
    /// `html`/`body` for almost every point. Pick the smallest containing box
    /// instead, falling back to the document root only when nothing else hits.
    pub fn element_at_point(&self, x: f32, y: f32) -> Option<&VisualElement> {
        let doc_x = x + self.scroll.0;
        let doc_y = y + self.scroll.1;
        let contains = |element: &&VisualElement| {
            element.rect.x <= doc_x
                && doc_x <= element.rect.x + element.rect.width
                && element.rect.y <= doc_y
                && doc_y <= element.rect.y + element.rect.height
        };
        let area = |element: &&VisualElement| element.rect.width * element.rect.height;
        let candidates = self.elements.iter().filter(|element| contains(element));
        candidates
            .clone()
            .filter(|element| !matches!(element.tag.as_str(), "html" | "body"))
            .min_by(|a, b| area(a).total_cmp(&area(b)))
            .or_else(|| candidates.min_by(|a, b| area(a).total_cmp(&area(b))))
    }

    /// Compare this index's text with another capture's text.
    pub fn text_diff(&self, other: &VisualIndex<'_>) -> (Vec<String>, Vec<String>) {
        let before: Vec<String> = self.text_runs.iter().map(|run| run.text.clone()).collect();
        let after: Vec<String> = other.text_runs.iter().map(|run| run.text.clone()).collect();
        let added = after
            .iter()
            .filter(|text| !before.contains(text))
            .cloned()
            .collect();
        let removed = before
            .iter()
            .filter(|text| !after.contains(text))
            .cloned()
            .collect();
        (added, removed)
    }

    pub fn document(&self) -> &'a Document {
        self.document
    }

    pub fn layout(&self) -> &'a LayoutResult {
        self.layout
    }

    pub fn image(&self) -> &'a RgbaImage {
        self.image
    }
}

/// Pluggable element-localisation strategy. The default heuristic is
/// deterministic; a model-backed backend can implement this trait and be
/// supplied through [`VisualIndex::find_with`].
pub trait ElementLocator: Send + Sync {
    fn name(&self) -> &'static str;
    fn locate(
        &self,
        description: &str,
        candidates: &[VisualElement],
        image: &RgbaImage,
        top_k: usize,
        region: Option<Rect>,
    ) -> Vec<ElementMatch>;
}

/// Default locator: token/affordance scoring over labelled element candidates.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeuristicLocator;

impl ElementLocator for HeuristicLocator {
    fn name(&self) -> &'static str {
        "heuristic"
    }

    fn locate(
        &self,
        description: &str,
        candidates: &[VisualElement],
        _image: &RgbaImage,
        top_k: usize,
        region: Option<Rect>,
    ) -> Vec<ElementMatch> {
        let tokens = description_tokens(description);
        if tokens.is_empty() {
            return Vec::new();
        }
        let role_words = role_words(description);
        let mut matches: Vec<ElementMatch> = candidates
            .iter()
            .filter(|element| {
                region.is_none()
                    || VisualRect::from(region.expect("checked above")).intersects(&element.rect)
            })
            .filter_map(|element| {
                let haystack = description_tokens(&format!(
                    "{} {} {} {}",
                    element.label,
                    element.role.as_deref().unwrap_or(""),
                    element.tag,
                    element.signals.join(" ")
                ));
                let matched: Vec<&String> = tokens
                    .iter()
                    .filter(|token| haystack.iter().any(|word| word.contains(token.as_str())))
                    .collect();
                if matched.is_empty() {
                    return None;
                }
                let mut score =
                    (matched.len() as f32 / tokens.len() as f32) * 0.6 + element.confidence * 0.2;
                let matched_names: Vec<&str> = matched.iter().map(|name| name.as_str()).collect();
                let mut signals = vec![format!("matched:{}", matched_names.join(","))];
                if role_words.iter().any(|word| {
                    element
                        .role
                        .as_deref()
                        .is_some_and(|role| role.contains(word))
                }) {
                    score += 0.2;
                    signals.push("role-match".to_owned());
                }
                if element.clickable {
                    score += 0.1;
                    signals.push("clickable".to_owned());
                }
                if element.visible {
                    score += 0.05;
                }
                Some(ElementMatch {
                    element: element.clone(),
                    score: score.min(1.0),
                    signals,
                })
            })
            .collect();
        matches.sort_by(|a, b| b.score.total_cmp(&a.score));
        matches.truncate(top_k.max(1));
        matches
    }
}

/// Resolve a locator name to a built-in strategy. Unknown names return `None`
/// so callers can report a clear error instead of silently falling back.
pub fn locator(name: Option<&str>) -> Option<&'static dyn ElementLocator> {
    match name.unwrap_or("heuristic").to_ascii_lowercase().as_str() {
        "heuristic" | "default" => Some(&HeuristicLocator),
        _ => None,
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn description_tokens(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "please", "click", "find", "locate", "me", "with", "on", "that", "this",
        "for", "to",
    ];
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric() && ch != '-' && ch != '_')
        .filter(|word| !word.is_empty() && !STOP.contains(word))
        .map(str::to_owned)
        .collect()
}

fn role_words(text: &str) -> Vec<String> {
    const ROLES: &[&str] = &[
        "button", "link", "input", "textbox", "checkbox", "radio", "menu", "tab", "heading",
        "image", "combobox", "select", "field",
    ];
    description_tokens(text)
        .into_iter()
        .filter(|word| ROLES.contains(&word.as_str()))
        .collect()
}

fn collect_text_runs(layout: &LayoutResult, scroll: (f32, f32)) -> Vec<TextRun> {
    type RawRun = (String, Rect, Option<NodeId>, f32, [u8; 4], f32, bool);
    fn walk(box_: &LayoutBox, out: &mut Vec<RawRun>) {
        for line in &box_.lines {
            collect_line(line, out);
        }
        for child in &box_.children {
            walk(child, out);
        }
        for atomic in &box_.atomics {
            walk(atomic, out);
        }
    }
    fn collect_line(line: &LineBox, out: &mut Vec<RawRun>) {
        let fonts = FontSet::embedded();
        for fragment in &line.fragments {
            let (text, is_space) = match &fragment.kind {
                crate::engine::layout::FragmentKind::Text(text) if !text.trim().is_empty() => {
                    (text.clone(), false)
                }
                crate::engine::layout::FragmentKind::Space => (" ".to_owned(), true),
                _ => continue,
            };
            let style = &fragment.style;
            let ascent = fonts.ascent(style);
            let descent = fonts.descent(style);
            let rect = Rect {
                x: fragment.x,
                y: line.baseline - ascent,
                width: fragment.width,
                height: ascent + descent,
            };
            let color = [style.color.r, style.color.g, style.color.b, style.color.a];
            out.push((
                text.clone(),
                rect,
                fragment.node,
                style.font_size,
                color,
                line.baseline,
                is_space,
            ));
        }
    }

    let mut raw = Vec::new();
    walk(&layout.root, &mut raw);
    let fonts = FontSet::embedded();
    raw.sort_by(|a, b| a.1.y.total_cmp(&b.1.y).then(a.1.x.total_cmp(&b.1.x)));
    raw.into_iter()
        .map(|(text, rect, node, font_size, color, baseline, is_space)| {
            let style = node
                .and_then(|node| layout.style(node))
                .cloned()
                .unwrap_or_default();
            let mut char_rects = Vec::new();
            if is_space {
                char_rects.push(VisualRect::from(rect));
            } else {
                let mut pen = rect.x;
                for ch in text.chars() {
                    let advance = if ch == '\t' {
                        fonts.text_width("    ", &style)
                    } else {
                        fonts.advance(ch, &style)
                    };
                    char_rects.push(VisualRect {
                        x: pen,
                        y: rect.y,
                        width: advance,
                        height: rect.height,
                    });
                    pen += advance;
                }
            }
            let visual = VisualRect::from(rect);
            TextRun {
                text,
                viewport_rect: visual.shifted(scroll.0, scroll.1),
                rect: visual,
                char_rects,
                node_id: node.map(|node| node.index()),
                font_size,
                color,
                line_y: baseline,
                source: "layout".to_owned(),
            }
        })
        .collect()
}

fn infer_role(document: &Document, node: NodeId, tag: &str) -> Option<String> {
    if let Some(role) = document.get_attribute(node, "role") {
        return Some(
            role.split_ascii_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase(),
        );
    }
    let input_type = document
        .get_attribute(node, "type")
        .unwrap_or("")
        .to_ascii_lowercase();
    let role = match tag {
        "a" if document.get_attribute(node, "href").is_some() => "link",
        "button" => "button",
        "input" if input_type == "checkbox" => "checkbox",
        "input" if input_type == "radio" => "radio",
        "input" if input_type == "submit" || input_type == "button" => "button",
        "input" => "textbox",
        "textarea" => "textbox",
        "select" => "combobox",
        "summary" => "button",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "img" => "img",
        "nav" => "navigation",
        "main" => "main",
        "dialog" => "dialog",
        "ul" | "ol" => "list",
        "table" => "table",
        _ => return None,
    };
    Some(role.to_owned())
}

fn label_for(document: &Document, node: NodeId, tag: &str) -> String {
    for attribute in ["aria-label", "title", "placeholder", "alt", "value"] {
        if let Some(value) = document.get_attribute(node, attribute) {
            let value = collapse(value);
            if !value.is_empty() {
                return value.chars().take(80).collect();
            }
        }
    }
    let text = collapse(&document.text_content(node));
    if !text.is_empty() {
        return text.chars().take(80).collect();
    }
    document
        .get_attribute(node, "id")
        .map(str::to_owned)
        .unwrap_or_else(|| tag.to_owned())
}

fn element_clickable(document: &Document, node: NodeId, tag: &str, role: Option<&str>) -> bool {
    if document.get_attribute(node, "disabled").is_some()
        || document.get_attribute(node, "aria-disabled") == Some("true")
    {
        return false;
    }
    if matches!(
        tag,
        "a" | "button" | "input" | "select" | "textarea" | "summary" | "option" | "label"
    ) {
        return true;
    }
    if matches!(
        role,
        Some(
            "button"
                | "link"
                | "checkbox"
                | "radio"
                | "switch"
                | "tab"
                | "menuitem"
                | "combobox"
                | "textbox"
                | "option"
                | "slider"
        )
    ) {
        return true;
    }
    document.get_attribute(node, "onclick").is_some()
        || document.get_attribute(node, "tabindex").is_some()
}

fn collect_elements(
    document: &Document,
    layout: &LayoutResult,
    image: &RgbaImage,
    viewport: crate::engine::css::Viewport,
    scroll: (f32, f32),
) -> Vec<VisualElement> {
    let visible_rect = Rect {
        x: scroll.0,
        y: scroll.1,
        width: viewport.width,
        height: viewport.height,
    };
    let mut elements = Vec::new();
    for node in document.descendants(document.document_node()) {
        if !document.is_element(node) {
            continue;
        }
        let Some(rect) = layout.node_rect(node) else {
            continue;
        };
        if rect.width < 1.0 || rect.height < 1.0 {
            continue;
        }
        let tag = document.tag_name(node).unwrap_or("").to_ascii_lowercase();
        let role = infer_role(document, node, &tag);
        let label = label_for(document, node, &tag);
        let clickable = element_clickable(document, node, &tag, role.as_deref());
        let style = layout.style(node);
        let styled_visible = style.is_some_and(|style| {
            style.visibility == Visibility::Visible
                && style.display != Display::None
                && style.opacity > 0.0
        });
        let visual = VisualRect::from(rect);
        let viewport_rect = visual.shifted(scroll.0, scroll.1);
        let in_viewport = visual.intersects(&VisualRect::from(visible_rect));
        let visible = styled_visible && in_viewport;
        let mut signals = vec![format!("tag:{tag}")];
        if let Some(role) = role.as_deref() {
            signals.push(format!("role:{role}"));
        }
        if !label.is_empty() {
            signals.push(format!("text:{label}"));
        }
        if clickable {
            signals.push("clickable".to_owned());
        }
        if in_viewport {
            signals.push("in-viewport".to_owned());
        }
        let cx = (rect.x + rect.width / 2.0).floor() as u32;
        let cy = (rect.y + rect.height / 2.0).floor() as u32;
        if cx < image.width() && cy < image.height() {
            let pixel = image.get_pixel(cx, cy);
            signals.push(format!(
                "bg:#{:02x}{:02x}{:02x}",
                pixel.0[0], pixel.0[1], pixel.0[2]
            ));
        }
        let confidence: f32 = 0.35
            + if clickable { 0.2 } else { 0.0 }
            + if role.is_some() { 0.15 } else { 0.0 }
            + if !label.is_empty() { 0.15 } else { 0.0 }
            + if in_viewport { 0.1 } else { 0.0 }
            + if styled_visible { 0.05 } else { 0.0 };
        elements.push(VisualElement {
            node_id: node.index(),
            tag,
            label,
            role,
            rect: visual,
            center: (
                viewport_rect.x + viewport_rect.width / 2.0,
                viewport_rect.y + viewport_rect.height / 2.0,
            ),
            viewport_rect,
            clickable,
            visible,
            confidence: confidence.min(1.0),
            signals,
        });
    }
    elements
}
