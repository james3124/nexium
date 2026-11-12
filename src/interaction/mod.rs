//! Headless interaction: viewport/scroll state, hit testing, mouse and
//! keyboard dispatch, accessibility extraction and DOM snapshots.

use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;
use serde::Serialize;
use url::Url;

use crate::browser::Page;
use crate::engine::css::{
    BorderStyle, Color, ComputedStyle, Display, FontStyle, Length, Overflow, Position, TextAlign,
    Viewport, Visibility, WhiteSpace,
};
use crate::engine::dom::{Document, NodeId};
use crate::engine::javascript::{
    ScriptOptions, ScriptRunResult, SessionEvent, run_page_scripts_inline_persistent,
};
use crate::engine::layout::{LayoutBox, LayoutResult, Rect};
use crate::intelligence::collapse_whitespace;
use crate::networking::HttpClient;
use crate::rendering::{RenderOptions, RenderedPage, layout_page, render_page};
use crate::utils::{NxError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    fn index(self) -> i64 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }

    fn mask(self) -> i64 {
        match self {
            Self::Left => 1,
            Self::Middle => 4,
            Self::Right => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ElementRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl From<Rect> for ElementRect {
    fn from(rect: Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ElementInfo {
    pub node: usize,
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub role: Option<String>,
    pub name: Option<String>,
    pub text: Option<String>,
    pub href: Option<String>,
    pub rect: ElementRect,
    pub visible: bool,
    pub in_viewport: bool,
    pub clickable: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputResult {
    pub target: Option<usize>,
    pub prevented: bool,
    pub href: Option<String>,
    pub scroll_x: f32,
    pub scroll_y: f32,
}

/// A pending top-level navigation produced by a link activation or a form
/// submission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Navigation {
    /// Absolute target URL (GET navigations already carry the query string).
    pub url: String,
    /// HTTP method, `GET` or `POST`.
    pub method: String,
    /// Form-encoded request body for POST submissions.
    pub body: Option<String>,
}

/// A text selection over the focused text input, textarea or contenteditable
/// element. Offsets are character (not byte) indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SelectionRange {
    pub node: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct DomSnapshot {
    pub url: String,
    pub title: Option<String>,
    pub html: String,
    pub text: String,
    pub node_count: usize,
    pub element_count: usize,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub scroll_x: f32,
    pub scroll_y: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtractedLink {
    pub text: String,
    pub href: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtractedField {
    pub tag: String,
    pub input_type: Option<String>,
    pub name: Option<String>,
    pub placeholder: Option<String>,
    pub value: Option<String>,
    pub disabled: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtractedPage {
    pub url: String,
    pub title: Option<String>,
    pub text: String,
    pub headings: Vec<String>,
    pub links: Vec<ExtractedLink>,
    pub fields: Vec<ExtractedField>,
    pub buttons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccessibilityNode {
    pub node: usize,
    pub role: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub value: Option<String>,
    pub disabled: bool,
    pub checked: Option<bool>,
    pub expanded: Option<bool>,
    pub children: Vec<AccessibilityNode>,
}

pub struct PageSession {
    page: Page,
    viewport: Viewport,
    scroll_x: f32,
    scroll_y: f32,
    layout: LayoutResult,
    local_storage: HashMap<String, String>,
    session_storage: HashMap<String, String>,
    hovered: Option<NodeId>,
    focus: Option<NodeId>,
    selection: Option<SelectionRange>,
    pending_navigation: Option<Navigation>,
}

impl PageSession {
    pub fn new(page: Page, viewport: Viewport) -> Self {
        let viewport = normalized_viewport(viewport);
        let hovered = page.document.hover_target();
        let mut session = Self {
            layout: layout_page(&page, viewport),
            page,
            viewport,
            scroll_x: 0.0,
            scroll_y: 0.0,
            local_storage: HashMap::new(),
            session_storage: HashMap::new(),
            hovered,
            focus: None,
            selection: None,
            pending_navigation: None,
        };
        if let Some(snapshot) = session
            .page
            .script_session()
            .and_then(|runtime| runtime.snapshot())
        {
            session.local_storage = snapshot.result.local_storage.clone();
            session.session_storage = snapshot.result.session_storage.clone();
            session
                .page
                .apply_session_snapshot(snapshot.result, snapshot.document);
            session.apply_hover_target();
            session.relayout();
        }
        session.publish_layout();
        session
    }

    pub fn page(&self) -> &Page {
        &self.page
    }

    pub fn page_mut(&mut self) -> &mut Page {
        &mut self.page
    }

    pub fn into_page(self) -> Page {
        self.page
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn layout(&self) -> &LayoutResult {
        &self.layout
    }

    pub fn scroll(&self) -> (f32, f32) {
        (self.scroll_x, self.scroll_y)
    }

    pub fn local_storage(&self) -> &HashMap<String, String> {
        &self.local_storage
    }

    pub fn session_storage(&self) -> &HashMap<String, String> {
        &self.session_storage
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = normalized_viewport(viewport);
        self.relayout();
        self.clamp_scroll();
    }

    pub fn scroll_to(&mut self, x: f32, y: f32) {
        self.scroll_x = x.max(0.0);
        self.scroll_y = y.max(0.0);
        self.clamp_scroll();
    }

    pub fn scroll_by(&mut self, dx: f32, dy: f32) {
        self.scroll_to(self.scroll_x + dx, self.scroll_y + dy);
    }

    pub fn max_scroll(&self) -> (f32, f32) {
        (
            0.0,
            (self.layout.document_height - self.viewport.height).max(0.0),
        )
    }

    pub fn scroll_into_view(&mut self, node: NodeId) -> bool {
        let Some(rect) = self.layout.node_rect(node) else {
            return false;
        };
        let mut y = self.scroll_y;
        if rect.y < self.scroll_y {
            y = rect.y;
        } else if rect.bottom() > self.scroll_y + self.viewport.height {
            y = rect.bottom() - self.viewport.height;
        }
        self.scroll_to(self.scroll_x, y);
        true
    }

    pub fn find(&self, selector: &str) -> Option<ElementInfo> {
        let node = self.page.select(selector).ok()??;
        self.element_info(node)
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        let (x, y) = self.to_document(x, y);
        let node = hit_test_box(&self.layout.root, x, y)?;
        let document = &self.page.document;
        let mut current = Some(node);
        while let Some(id) = current {
            if document.is_element(id) {
                return Some(id);
            }
            current = document.parent(id);
        }
        Some(node)
    }

    pub fn element_at(&self, x: f32, y: f32) -> Option<ElementInfo> {
        self.hit_test(x, y).and_then(|node| self.element_info(node))
    }

    pub fn element_info(&self, node: NodeId) -> Option<ElementInfo> {
        let rect = self.layout.node_rect(node)?;
        let tag = self.page.document.tag_name(node)?.to_owned();
        let id = self
            .page
            .document
            .get_attribute(node, "id")
            .map(str::to_owned);
        let classes = self
            .page
            .document
            .get_attribute(node, "class")
            .unwrap_or("")
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect();
        let href = self
            .page
            .document
            .get_attribute(node, "href")
            .map(|href| self.resolve_url(href));
        let text = collapsed_text(&self.page.document, node);
        let role = role_for(&self.page.document, node);
        let name = accessible_name(&self.page.document, node);
        let visible = self.node_visible(node);
        let in_viewport = rect
            .intersection(&Rect {
                x: self.scroll_x,
                y: self.scroll_y,
                width: self.viewport.width,
                height: self.viewport.height,
            })
            .width
            > 0.0
            && rect
                .intersection(&Rect {
                    x: self.scroll_x,
                    y: self.scroll_y,
                    width: self.viewport.width,
                    height: self.viewport.height,
                })
                .height
                > 0.0;
        let clickable = self.is_clickable(node);
        let enabled = !element_disabled(&self.page.document, node);
        Some(ElementInfo {
            node: node.index(),
            tag,
            id,
            classes,
            role,
            name,
            text,
            href,
            rect: rect.into(),
            visible,
            in_viewport,
            clickable,
            enabled,
        })
    }

    pub fn elements(&self) -> Vec<ElementInfo> {
        self.page
            .document
            .descendants(self.page.document.document_node())
            .into_iter()
            .filter_map(|node| self.element_info(node))
            .collect()
    }

    pub fn click(&mut self, x: f32, y: f32) -> InputResult {
        self.click_with_button(x, y, MouseButton::Left)
    }

    pub fn click_with_button(&mut self, x: f32, y: f32, button: MouseButton) -> InputResult {
        let target = self.hit_test(x, y);
        let point = (x, y);
        let mut result =
            self.mouse_input("mousedown", point, target, button, 1, Some(button.mask()));
        self.mouse_input("mouseup", point, target, button, 1, Some(0));
        let click = self.mouse_input("click", point, target, button, 1, Some(0));
        result.prevented = click.prevented;
        result.href = target.and_then(|node| self.node_href(node));
        self.selection = None;
        if let Some(node) = target
            && self.is_focusable(node)
        {
            self.focus = Some(node);
        }
        if self.page.script_session().is_none() {
            self.fallback_default_click(target);
        }
        if let Some(node) = target
            && !click.prevented
        {
            self.submit_via_click(node);
            if self.is_link(node)
                && let Some(href) = result.href.clone()
            {
                self.pending_navigation = Some(Navigation {
                    url: href,
                    method: "GET".to_owned(),
                    body: None,
                });
            }
        }
        self.relayout();
        result.scroll_x = self.scroll_x;
        result.scroll_y = self.scroll_y;
        result
    }

    pub fn double_click(&mut self, x: f32, y: f32) -> InputResult {
        let first = self.click_with_button(x, y, MouseButton::Left);
        if first.prevented {
            return first;
        }
        let target = self.hit_test(x, y);
        let point = (x, y);
        let mut result =
            self.mouse_input("mousedown", point, target, MouseButton::Left, 2, Some(1));
        self.mouse_input("mouseup", point, target, MouseButton::Left, 2, Some(0));
        let click = self.mouse_input("click", point, target, MouseButton::Left, 2, Some(0));
        let dblclick = self.mouse_input("dblclick", point, target, MouseButton::Left, 2, Some(0));
        result.prevented = click.prevented || dblclick.prevented;
        result.href = target.and_then(|node| self.node_href(node));
        self.relayout();
        result
    }

    /// Move the synthetic pointer to `(x, y)`, updating `:hover` state.
    ///
    /// The hovered element is recorded on the document so the selector engine
    /// matches `:hover`, and the page is re-laid out with the updated style.
    /// `mouseout`/`mouseover` only fire when the target changes, `mousemove`
    /// fires on every call and never clears the hover state.
    pub fn hover(&mut self, x: f32, y: f32) -> InputResult {
        let target = self.hit_test(x, y);
        let point = (x, y);
        if self.hovered != target {
            let previous = self.hovered;
            self.hovered = target;
            self.apply_hover_target();
            if let Some(previous) = previous {
                self.mouse_input(
                    "mouseout",
                    point,
                    Some(previous),
                    MouseButton::Left,
                    0,
                    Some(0),
                );
            }
            if let Some(node) = target {
                self.mouse_input(
                    "mouseover",
                    point,
                    Some(node),
                    MouseButton::Left,
                    0,
                    Some(0),
                );
            }
        }
        let result = self.mouse_input("mousemove", point, target, MouseButton::Left, 0, Some(0));
        // Any event dispatch replaces the document with the script runtime
        // snapshot, so the hover target is re-applied before relayout.
        self.apply_hover_target();
        self.relayout();
        result
    }

    /// Clear the hover state (pointer left the page or the element was
    /// removed), firing `mouseout` for the previously hovered element.
    pub fn clear_hover(&mut self) -> InputResult {
        let previous = self.hovered.take();
        if let Some(node) = previous {
            self.mouse_input(
                "mouseout",
                (0.0, 0.0),
                Some(node),
                MouseButton::Left,
                0,
                Some(0),
            );
        }
        self.page.document.set_hover_target(None);
        self.relayout();
        InputResult {
            target: previous.map(NodeId::index),
            prevented: false,
            href: None,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    /// Element currently under the synthetic pointer.
    pub fn hovered_node(&self) -> Option<NodeId> {
        self.hovered
    }

    pub fn mouse_down(&mut self, x: f32, y: f32, button: MouseButton) -> InputResult {
        let target = self.hit_test(x, y);
        self.mouse_input("mousedown", (x, y), target, button, 1, Some(button.mask()))
    }

    pub fn mouse_up(&mut self, x: f32, y: f32, button: MouseButton) -> InputResult {
        let target = self.hit_test(x, y);
        self.mouse_input("mouseup", (x, y), target, button, 1, Some(0))
    }

    pub fn wheel(&mut self, x: f32, y: f32, delta_x: f32, delta_y: f32) -> InputResult {
        let target = self.hit_test(x, y);
        let event = self.event_at("wheel", (x, y), target, MouseButton::Left, 0, Some(0));
        let result = self.dispatch(event);
        if !result.prevented {
            self.scroll_by(delta_x, delta_y);
        }
        InputResult {
            target: target.map(NodeId::index),
            prevented: result.prevented,
            href: None,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    pub fn press_key(&mut self, key: &str) -> InputResult {
        let target = self.hit_test(self.viewport.width / 2.0, self.viewport.height / 2.0);
        let prevented = self
            .page
            .script_session()
            .and_then(|runtime| runtime.press_key(key))
            .map(|snapshot| {
                self.apply_snapshot(snapshot.result, snapshot.document);
                false
            })
            .unwrap_or(false);
        match key {
            "Tab" => {
                self.focus_next();
            }
            "Shift+Tab" | "Shift-Tab" => {
                self.focus_previous();
            }
            "Enter" => {
                self.submit_from_enter();
            }
            "Backspace" => {
                if let Some(node) = self.focus
                    && self.is_contenteditable(node)
                {
                    self.edit_contenteditable(node, EditableAction::Backspace);
                }
            }
            _ => {}
        }
        self.relayout();
        InputResult {
            target: self.focus.or(target).map(NodeId::index),
            prevented,
            href: None,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    pub fn type_text(&mut self, text: &str) -> InputResult {
        self.selection = None;
        let prevented = self
            .page
            .script_session()
            .and_then(|runtime| runtime.type_text(text))
            .map(|snapshot| {
                self.apply_snapshot(snapshot.result, snapshot.document);
                false
            })
            .unwrap_or(false);
        if let Some(node) = self.focus
            && self.is_contenteditable(node)
        {
            self.edit_contenteditable(node, EditableAction::Insert(text.to_owned()));
        }
        self.relayout();
        InputResult {
            target: self.focus.map(NodeId::index),
            prevented,
            href: None,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    /// The element that currently has focus, if any.
    pub fn focused_node(&self) -> Option<NodeId> {
        self.focus
    }

    /// Programmatically focus an element, dispatching `blur`/`focusout` for the
    /// previous element and `focus`/`focusin` for the new one.
    pub fn focus_node(&mut self, node: NodeId) -> bool {
        if !self.is_focusable(node) {
            return false;
        }
        self.move_focus(Some(node));
        true
    }

    /// Move focus to the next element in tab order (wrapping around).
    pub fn focus_next(&mut self) -> Option<NodeId> {
        let order = self.tab_order();
        if order.is_empty() {
            return None;
        }
        let next = match self
            .focus
            .and_then(|node| order.iter().position(|candidate| *candidate == node))
        {
            Some(index) => order[(index + 1) % order.len()],
            None => order[0],
        };
        self.move_focus(Some(next));
        Some(next)
    }

    /// Move focus to the previous element in tab order (wrapping around).
    pub fn focus_previous(&mut self) -> Option<NodeId> {
        let order = self.tab_order();
        if order.is_empty() {
            return None;
        }
        let next = match self
            .focus
            .and_then(|node| order.iter().position(|candidate| *candidate == node))
        {
            Some(0) | None => order[order.len() - 1],
            Some(index) => order[index - 1],
        };
        self.move_focus(Some(next));
        Some(next)
    }

    /// Select `start..end` (character offsets) of the focused editable element
    /// and fire a `select` event.
    pub fn select_text(&mut self, start: usize, end: usize) -> Option<SelectionRange> {
        let node = self.focus?;
        let text = self.editable_text(node)?;
        let length = text.chars().count();
        let start = start.min(length);
        let end = end.min(length);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let range = SelectionRange {
            node: node.index(),
            start,
            end,
        };
        self.selection = Some(range);
        self.dispatch(session_event("select", Some(node), true, false));
        Some(range)
    }

    /// The current text selection, if any.
    pub fn selection(&self) -> Option<SelectionRange> {
        self.selection
    }

    /// Navigation recorded by a link activation or a form submission that has
    /// not been performed yet.
    pub fn pending_navigation(&self) -> Option<&Navigation> {
        self.pending_navigation.as_ref()
    }

    /// Take the pending navigation (if any) out of the session.
    pub fn take_navigation(&mut self) -> Option<Navigation> {
        self.pending_navigation.take()
    }

    /// The form that owns `node`, if any.
    pub fn enclosing_form(&self, node: NodeId) -> Option<NodeId> {
        self.form_owner(node)
    }

    /// Submit `form` through the DOM (`submit` event) and record the resulting
    /// navigation. Returns `None` when submission was prevented.
    pub fn submit_form(&mut self, form: NodeId) -> Option<Navigation> {
        self.submit_form_with(form, None)
    }

    /// Load `href` (resolved against the current URL) as a new page.
    pub async fn follow_link(&mut self, client: &HttpClient, href: &str) -> Result<Page> {
        let url = Url::parse(href)
            .or_else(|_| self.page.url.join(href))
            .map_err(|error| NxError::Browser(format!("invalid link target {href}: {error}")))?;
        let navigation = Navigation {
            url: url.to_string(),
            method: "GET".to_owned(),
            body: None,
        };
        self.navigate_to(client, &navigation).await
    }

    /// Perform a navigation through the page loading path, replacing the
    /// session page with the newly loaded document.
    pub async fn navigate_to(
        &mut self,
        client: &HttpClient,
        navigation: &Navigation,
    ) -> Result<Page> {
        let url = Url::parse(&navigation.url).map_err(|error| {
            NxError::Browser(format!(
                "invalid navigation target {}: {error}",
                navigation.url
            ))
        })?;
        let request =
            Page::navigation_request(url, &navigation.method, navigation.body.as_deref())?;
        let mut page = Page::load_request(client, request).await?;
        let options = ScriptOptions {
            viewport_width: self.viewport.width as f64,
            viewport_height: self.viewport.height as f64,
            ..ScriptOptions::default()
        };
        let (result, session) =
            run_page_scripts_inline_persistent(&page.url, &mut page.document, &options);
        self.local_storage = result.local_storage.clone();
        self.session_storage = result.session_storage.clone();
        page.console = result.console;
        page.script_errors = result.errors;
        page.canvases = result.canvases;
        page.set_script_session(session.map(Arc::new));
        self.page = page;
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.hovered = None;
        self.focus = None;
        self.selection = None;
        self.pending_navigation = None;
        self.relayout();
        Ok(self.page.clone())
    }

    pub fn evaluate(&mut self, code: &str) -> Result<serde_json::Value> {
        let runtime = self
            .page
            .script_session()
            .ok_or_else(|| NxError::Browser("no JavaScript runtime is available".to_owned()))?;
        let (value, snapshot) = runtime
            .evaluate(code)
            .ok_or_else(|| NxError::Browser("script runtime stopped".to_owned()))?;
        self.apply_snapshot(snapshot.result, snapshot.document);
        value.map_err(NxError::Browser)
    }

    pub fn accessibility_tree(&self) -> AccessibilityNode {
        let document = &self.page.document;
        let mut children = Vec::new();
        for child in document.children(document.document_node()) {
            if let Some(node) = build_accessibility_node(document, &self.layout, *child) {
                children.push(node);
            }
        }
        AccessibilityNode {
            node: document.document_node().index(),
            role: "document".to_owned(),
            name: self.page.title(),
            description: None,
            value: None,
            disabled: false,
            checked: None,
            expanded: None,
            children,
        }
    }

    pub fn dom_snapshot(&self) -> DomSnapshot {
        DomSnapshot {
            url: self.page.url.to_string(),
            title: self.page.title(),
            html: self.page.document.serialize(),
            text: crate::intelligence::visible_text(&self.page.document),
            node_count: self.page.document.node_count(),
            element_count: self.page.document.element_count(),
            viewport_width: self.viewport.width,
            viewport_height: self.viewport.height,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    pub async fn screenshot(&self, client: &HttpClient) -> Result<RenderedPage> {
        render_page(
            &self.page,
            client,
            RenderOptions {
                viewport: self.viewport,
                max_height: 20_000,
                load_images: true,
            },
        )
        .await
    }

    pub async fn viewport_image(&self, client: &HttpClient) -> Result<RgbaImage> {
        let rendered = self.screenshot(client).await?;
        Ok(rendered.viewport_image_at(self.scroll_x, self.scroll_y))
    }

    pub async fn element_image(
        &self,
        client: &HttpClient,
        node: NodeId,
    ) -> Result<Option<RgbaImage>> {
        let rendered = self.screenshot(client).await?;
        Ok(rendered.element_image(node))
    }

    fn to_document(&self, x: f32, y: f32) -> (f32, f32) {
        (x + self.scroll_x, y + self.scroll_y)
    }

    fn apply_snapshot(&mut self, result: ScriptRunResult, document: Document) {
        self.local_storage = result.local_storage.clone();
        self.session_storage = result.session_storage.clone();
        self.page.apply_session_snapshot(result, document);
        // The runtime snapshot replaces the document, dropping the `:hover`
        // target; restore it so style resolution keeps matching `:hover`.
        self.apply_hover_target();
        self.relayout();
    }

    fn apply_hover_target(&mut self) {
        if let Some(node) = self.hovered
            && !self
                .page
                .document
                .try_node(node)
                .is_some_and(|candidate| candidate.is_element())
        {
            self.hovered = None;
        }
        self.page.document.set_hover_target(self.hovered);
    }

    fn is_focusable(&self, node: NodeId) -> bool {
        let document = &self.page.document;
        if element_disabled(document, node) || self.layout_hidden(node) {
            return false;
        }
        if document.get_attribute(node, "tabindex").is_some() {
            return true;
        }
        if self.is_contenteditable(node) {
            return true;
        }
        match document.tag_name(node).unwrap_or("") {
            "input" => input_type(document, node) != "hidden",
            "textarea" | "select" | "button" => true,
            "a" | "area" => document.get_attribute(node, "href").is_some(),
            _ => false,
        }
    }

    fn is_tabbable(&self, node: NodeId) -> bool {
        if !self.is_focusable(node) {
            return false;
        }
        if let Some(index) = self
            .page
            .document
            .get_attribute(node, "tabindex")
            .and_then(|value| value.trim().parse::<i64>().ok())
        {
            return index >= 0;
        }
        true
    }

    /// Hidden according to layout: no box (`display: none`), `display: none`
    /// or `visibility: hidden`.
    fn layout_hidden(&self, node: NodeId) -> bool {
        match self.layout.style(node) {
            Some(style) => style.display == Display::None || style.visibility == Visibility::Hidden,
            // Elements without a layout box were not rendered.
            None => true,
        }
    }

    /// Elements reachable with Tab, in tab order: positive `tabindex` values
    /// first (ascending, document order for ties), then zero/native
    /// focusables in document order.
    fn tab_order(&self) -> Vec<NodeId> {
        let document = &self.page.document;
        let mut positive: Vec<(i64, NodeId)> = Vec::new();
        let mut regular: Vec<NodeId> = Vec::new();
        for node in document.descendants(document.document_node()) {
            if !self.is_tabbable(node) {
                continue;
            }
            match document
                .get_attribute(node, "tabindex")
                .and_then(|value| value.trim().parse::<i64>().ok())
            {
                Some(index) if index > 0 => positive.push((index, node)),
                _ => regular.push(node),
            }
        }
        positive.sort_by_key(|(index, _)| *index);
        positive
            .into_iter()
            .map(|(_, node)| node)
            .chain(regular)
            .collect()
    }

    fn move_focus(&mut self, node: Option<NodeId>) {
        if node == self.focus {
            return;
        }
        if let Some(previous) = self.focus {
            self.dispatch(session_event("blur", Some(previous), false, false));
            self.dispatch(session_event("focusout", Some(previous), true, false));
        }
        self.focus = node;
        self.selection = None;
        if let Some(node) = node {
            self.dispatch(session_event("focus", Some(node), false, false));
            self.dispatch(session_event("focusin", Some(node), true, false));
            self.scroll_into_view(node);
        }
    }

    fn is_contenteditable(&self, node: NodeId) -> bool {
        let Some(value) = self.page.document.get_attribute(node, "contenteditable") else {
            return false;
        };
        !value.trim().eq_ignore_ascii_case("false")
    }

    fn editable_text(&self, node: NodeId) -> Option<String> {
        let document = &self.page.document;
        match document.tag_name(node)? {
            "input" => Some(
                document
                    .get_attribute(node, "value")
                    .unwrap_or("")
                    .to_owned(),
            ),
            "textarea" => Some(document.text_content(node)),
            _ if self.is_contenteditable(node) => Some(document.text_content(node)),
            _ => None,
        }
    }

    /// Insert or delete text in a focused contenteditable element, firing
    /// `beforeinput` (cancelable) before the change and `input` afterwards.
    fn edit_contenteditable(&mut self, node: NodeId, action: EditableAction) {
        let mut event = session_event("beforeinput", Some(node), true, true);
        event.text = match &action {
            EditableAction::Insert(text) => Some(text.clone()),
            EditableAction::Backspace => None,
        };
        if self.dispatch(event).prevented {
            return;
        }
        let current = self.page.document.text_content(node);
        let updated = match action {
            EditableAction::Insert(text) => {
                if text.is_empty() {
                    return;
                }
                let mut value = current.clone();
                value.push_str(&text);
                value
            }
            EditableAction::Backspace => {
                let mut value = current.clone();
                value.pop();
                value
            }
        };
        if updated == current {
            return;
        }
        if !self.apply_contenteditable_text(node, &updated) {
            return;
        }
        self.dispatch(session_event("input", Some(node), true, false));
    }

    /// Write the new text into the element. The DOM is mutated through the
    /// script runtime when one is attached so the change survives subsequent
    /// event dispatches; otherwise the page document is edited directly.
    fn apply_contenteditable_text(&mut self, node: NodeId, updated: &str) -> bool {
        if let Some(runtime) = self.page.script_session()
            && let Some(path) = self.node_path(node)
        {
            let value = serde_json::to_string(updated).unwrap_or_else(|_| "\"\"".to_owned());
            let code = format!(
                "(function(){{var n={path};if(!n){{return false;}}n.textContent={value};return true;}})()"
            );
            if let Some((result, snapshot)) = runtime.evaluate(&code) {
                let ok = result.is_ok();
                self.apply_snapshot(snapshot.result, snapshot.document);
                if ok {
                    return true;
                }
            }
        }
        match self.page.document.set_text_content(node, updated) {
            Ok(()) => {
                self.relayout();
                true
            }
            Err(_) => false,
        }
    }

    /// JavaScript expression that resolves to `node` (`document` plus child
    /// node indices).
    fn node_path(&self, node: NodeId) -> Option<String> {
        let document = &self.page.document;
        let mut parts = Vec::new();
        let mut current = node;
        while current != document.document_node() {
            let parent = document.parent(current)?;
            let index = document
                .children(parent)
                .iter()
                .position(|child| *child == current)?;
            parts.push(index);
            current = parent;
        }
        parts.reverse();
        let mut expression = String::from("document");
        for index in parts {
            expression.push_str(&format!(".childNodes[{index}]"));
        }
        Some(expression)
    }

    fn form_owner(&self, node: NodeId) -> Option<NodeId> {
        let document = &self.page.document;
        if document.tag_name(node) == Some("form") {
            return Some(node);
        }
        document
            .ancestors(node)
            .into_iter()
            .find(|ancestor| document.tag_name(*ancestor) == Some("form"))
    }

    fn is_link(&self, node: NodeId) -> bool {
        let document = &self.page.document;
        matches!(document.tag_name(node).unwrap_or(""), "a" | "area")
            && document.get_attribute(node, "href").is_some()
    }

    fn is_submit_control(&self, node: NodeId) -> bool {
        let document = &self.page.document;
        match document.tag_name(node).unwrap_or("") {
            "button" => {
                let value = document
                    .get_attribute(node, "type")
                    .unwrap_or("submit")
                    .trim()
                    .to_ascii_lowercase();
                value.is_empty() || value == "submit"
            }
            "input" => matches!(input_type(document, node).as_str(), "submit" | "image"),
            _ => false,
        }
    }

    /// Default action of clicking `node`: submitting its form when it is a
    /// submit button. The navigation is recorded for [`Self::take_navigation`].
    fn submit_via_click(&mut self, node: NodeId) {
        if !self.is_submit_control(node) {
            return;
        }
        if let Some(form) = self.form_owner(node) {
            let _ = self.submit_form_with(form, Some(node));
        }
    }

    /// Pressing Enter in a text field implicitly submits its form.
    fn submit_from_enter(&mut self) {
        let Some(node) = self.focus else {
            return;
        };
        let document = &self.page.document;
        let submits = document.tag_name(node) == Some("input")
            && !matches!(
                input_type(document, node).as_str(),
                "submit"
                    | "image"
                    | "button"
                    | "reset"
                    | "checkbox"
                    | "radio"
                    | "file"
                    | "hidden"
                    | "range"
                    | "color"
            );
        if !submits {
            return;
        }
        if let Some(form) = self.form_owner(node) {
            let _ = self.submit_form_with(form, None);
        }
    }

    fn submit_form_with(&mut self, form: NodeId, submitter: Option<NodeId>) -> Option<Navigation> {
        if self.page.document.tag_name(form) != Some("form") {
            return None;
        }
        let prevented = self
            .dispatch(session_event("submit", Some(form), true, true))
            .prevented;
        if prevented {
            return None;
        }
        let navigation = self.form_navigation(form, submitter)?;
        self.pending_navigation = Some(navigation.clone());
        Some(navigation)
    }

    fn form_navigation(&self, form: NodeId, submitter: Option<NodeId>) -> Option<Navigation> {
        let document = &self.page.document;
        if document.tag_name(form) != Some("form") {
            return None;
        }
        let method = document
            .get_attribute(form, "method")
            .unwrap_or("get")
            .trim()
            .to_ascii_uppercase();
        let method = if method == "POST" { "POST" } else { "GET" };
        let action = document.get_attribute(form, "action").unwrap_or("").trim();
        let mut url = if action.is_empty() {
            self.page.url.clone()
        } else {
            Url::parse(action)
                .or_else(|_| self.page.url.join(action))
                .ok()?
        };
        url.set_fragment(None);
        let encoded = encode_form(&self.successful_controls(form, submitter));
        let body = if method == "POST" {
            Some(encoded)
        } else {
            if encoded.is_empty() {
                url.set_query(None);
            } else {
                url.set_query(Some(&encoded));
            }
            None
        };
        Some(Navigation {
            url: url.to_string(),
            method: method.to_owned(),
            body,
        })
    }

    /// Successful controls of `form`: enabled, named fields in document order.
    fn successful_controls(
        &self,
        form: NodeId,
        submitter: Option<NodeId>,
    ) -> Vec<(String, String)> {
        let document = &self.page.document;
        let mut data = Vec::new();
        for node in document.descendants(form) {
            if node == form || element_disabled(document, node) {
                continue;
            }
            let name = document
                .get_attribute(node, "name")
                .unwrap_or("")
                .trim()
                .to_owned();
            match document.tag_name(node).unwrap_or("") {
                "input" => {
                    let field_type = input_type(document, node);
                    match field_type.as_str() {
                        "submit" | "image" => {
                            if submitter == Some(node) && !name.is_empty() {
                                data.push((
                                    name,
                                    document
                                        .get_attribute(node, "value")
                                        .unwrap_or("Submit")
                                        .to_owned(),
                                ));
                            }
                        }
                        "button" | "reset" | "file" => {}
                        "checkbox" | "radio" => {
                            if !name.is_empty() && document.get_attribute(node, "checked").is_some()
                            {
                                data.push((
                                    name,
                                    document
                                        .get_attribute(node, "value")
                                        .unwrap_or("on")
                                        .to_owned(),
                                ));
                            }
                        }
                        "hidden" if name.is_empty() => {}
                        _ if name.is_empty() => {}
                        _ => data.push((
                            name,
                            document
                                .get_attribute(node, "value")
                                .unwrap_or("")
                                .to_owned(),
                        )),
                    }
                }
                "textarea" => {
                    if !name.is_empty() {
                        data.push((name, document.text_content(node)));
                    }
                }
                "select" => {
                    if !name.is_empty() {
                        let options: Vec<NodeId> = document
                            .descendants(node)
                            .into_iter()
                            .filter(|option| document.tag_name(*option) == Some("option"))
                            .collect();
                        let selected = options
                            .iter()
                            .copied()
                            .find(|option| document.get_attribute(*option, "selected").is_some())
                            .or_else(|| options.first().copied());
                        if let Some(option) = selected {
                            let value = document
                                .get_attribute(option, "value")
                                .map(str::to_owned)
                                .unwrap_or_else(|| document.text_content(option));
                            data.push((name, value));
                        }
                    }
                }
                "button" if submitter == Some(node) && !name.is_empty() => {
                    data.push((
                        name,
                        document
                            .get_attribute(node, "value")
                            .unwrap_or("")
                            .to_owned(),
                    ));
                }
                _ => {}
            }
        }
        data
    }

    fn relayout(&mut self) {
        self.layout = layout_page(&self.page, self.viewport);
        self.publish_layout();
    }

    fn publish_layout(&self) {
        let Some(runtime) = self.page.script_session() else {
            return;
        };
        let rects: HashMap<usize, [f32; 4]> = self
            .layout
            .node_rects
            .iter()
            .map(|(id, rect)| (id.index(), [rect.x, rect.y, rect.width, rect.height]))
            .collect();
        let styles: HashMap<usize, HashMap<String, String>> = self
            .layout
            .styles
            .iter()
            .map(|(id, style)| {
                let rect = self.layout.node_rects.get(id).copied();
                (id.index(), computed_style_strings(style, rect))
            })
            .collect();
        runtime.set_layout(rects, styles);
    }

    fn clamp_scroll(&mut self) {
        let (max_x, max_y) = self.max_scroll();
        self.scroll_x = self.scroll_x.min(max_x);
        self.scroll_y = self.scroll_y.min(max_y);
    }

    fn node_visible(&self, node: NodeId) -> bool {
        let Some(rect) = self.layout.node_rect(node) else {
            return false;
        };
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return false;
        }
        self.layout.style(node).is_none_or(|style| {
            style.display != Display::None
                && style.visibility == Visibility::Visible
                && style.opacity > 0.0
        })
    }

    fn is_clickable(&self, node: NodeId) -> bool {
        if !self.node_visible(node) {
            return false;
        }
        let document = &self.page.document;
        if element_disabled(document, node) {
            return false;
        }
        if document.get_attribute(node, "onclick").is_some() {
            return true;
        }
        if document.get_attribute(node, "tabindex").is_some() {
            return true;
        }
        match document.tag_name(node).unwrap_or("") {
            "a" | "area" => document.get_attribute(node, "href").is_some(),
            "button" | "input" | "select" | "textarea" | "label" | "summary" | "details" => true,
            _ => document
                .get_attribute(node, "role")
                .is_some_and(|role| matches!(role, "button" | "link" | "checkbox" | "tab")),
        }
    }

    fn node_href(&self, node: NodeId) -> Option<String> {
        self.page
            .document
            .get_attribute(node, "href")
            .map(|href| self.resolve_url(href))
    }

    fn resolve_url(&self, href: &str) -> String {
        Url::parse(href)
            .or_else(|_| self.page.url.join(href))
            .map(|url| url.to_string())
            .unwrap_or_else(|_| href.to_owned())
    }

    fn event_at(
        &self,
        event_type: &str,
        point: (f32, f32),
        target: Option<NodeId>,
        button: MouseButton,
        detail: i64,
        buttons: Option<i64>,
    ) -> SessionEvent {
        let (x, y) = point;
        let (page_x, page_y) = self.to_document(x, y);
        SessionEvent {
            event_type: event_type.to_owned(),
            target,
            bubbles: true,
            cancelable: true,
            client_x: Some(x as f64),
            client_y: Some(y as f64),
            page_x: Some(page_x as f64),
            page_y: Some(page_y as f64),
            button: Some(button.index()),
            buttons,
            detail: Some(detail),
            ..SessionEvent::default()
        }
    }

    fn mouse_input(
        &mut self,
        event_type: &str,
        point: (f32, f32),
        target: Option<NodeId>,
        button: MouseButton,
        detail: i64,
        buttons: Option<i64>,
    ) -> InputResult {
        let event = self.event_at(event_type, point, target, button, detail, buttons);
        let result = self.dispatch(event);
        InputResult {
            target: target.map(NodeId::index),
            prevented: result.prevented,
            href: None,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    fn dispatch(&mut self, event: SessionEvent) -> InputResult {
        let target = event.target.map(NodeId::index);
        let prevented = if let Some(snapshot) = self
            .page
            .script_session()
            .and_then(|runtime| runtime.dispatch(event))
        {
            let prevented = snapshot.prevented;
            self.apply_snapshot(snapshot.result, snapshot.document);
            prevented
        } else {
            false
        };
        InputResult {
            target,
            prevented,
            href: None,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    fn fallback_default_click(&mut self, target: Option<NodeId>) {
        let Some(node) = target else {
            return;
        };
        let toggle = {
            let document = &self.page.document;
            document.tag_name(node) == Some("input")
                && matches!(
                    document
                        .get_attribute(node, "type")
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .as_str(),
                    "checkbox" | "radio"
                )
        };
        if toggle {
            let checked = self.page.document.get_attribute(node, "checked").is_some();
            if checked {
                let _ = self.page.document.remove_attribute(node, "checked");
            } else {
                let _ = self.page.document.set_attribute(node, "checked", "");
            }
        }
    }
}

fn normalized_viewport(viewport: Viewport) -> Viewport {
    Viewport {
        width: viewport.width.max(1.0),
        height: viewport.height.max(1.0),
    }
}

/// Editing action applied to a focused contenteditable element.
enum EditableAction {
    Insert(String),
    Backspace,
}

/// Build a host session event for a DOM dispatch.
fn session_event(
    event_type: &str,
    target: Option<NodeId>,
    bubbles: bool,
    cancelable: bool,
) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        target,
        bubbles,
        cancelable,
        ..SessionEvent::default()
    }
}

/// `application/x-www-form-urlencoded` encoding of form data.
fn encode_form(data: &[(String, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in data {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}

fn hit_test_box(layout_box: &LayoutBox, x: f32, y: f32) -> Option<NodeId> {
    if layout_box.style.visibility == Visibility::Hidden {
        return None;
    }
    for child in layout_box.children.iter().rev() {
        if let Some(node) = hit_test_box(child, x, y) {
            return Some(node);
        }
    }
    for atomic in layout_box.atomics.iter().rev() {
        if let Some(node) = hit_test_box(atomic, x, y) {
            return Some(node);
        }
    }
    if layout_box.rect.contains(x, y) {
        // Inline elements (links, spans) are flattened into line fragments;
        // prefer the fragment so clicks target the inline element itself.
        for line in &layout_box.lines {
            if y < line.y || y > line.y + line.height {
                continue;
            }
            for fragment in &line.fragments {
                if fragment.node.is_some() && x >= fragment.x && x <= fragment.x + fragment.width {
                    return fragment.node;
                }
            }
        }
        layout_box.node
    } else {
        None
    }
}

fn css_number(value: f32) -> String {
    if (value.fract()).abs() < 0.0005 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.3}")
    }
}

fn css_length(length: Length) -> String {
    match length {
        Length::Auto => "auto".to_owned(),
        Length::Px(value) => format!("{}px", css_number(value)),
        Length::Percent(value) => format!("{}%", css_number(value)),
        Length::Em(value) => format!("{}em", css_number(value)),
        Length::Rem(value) => format!("{}rem", css_number(value)),
        Length::Pt(value) => format!("{}pt", css_number(value)),
        Length::Vw(value) => format!("{}vw", css_number(value)),
        Length::Vh(value) => format!("{}vh", css_number(value)),
        Length::Vmin(value) => format!("{}vmin", css_number(value)),
        Length::Vmax(value) => format!("{}vmax", css_number(value)),
        Length::Calc(_) | Length::Min(..) | Length::Max(..) | Length::Clamp(..) => "0px".to_owned(),
    }
}

fn css_color(color: Color) -> String {
    if color.a == 0 {
        "rgba(0, 0, 0, 0)".to_owned()
    } else if color.a == 255 {
        format!("rgb({}, {}, {})", color.r, color.g, color.b)
    } else {
        format!(
            "rgba({}, {}, {}, {})",
            color.r,
            color.g,
            color.b,
            css_number(color.a as f32 / 255.0)
        )
    }
}

fn border_style_name(style: BorderStyle) -> &'static str {
    match style {
        BorderStyle::None => "none",
        BorderStyle::Solid => "solid",
        BorderStyle::Dashed => "dashed",
        BorderStyle::Dotted => "dotted",
        BorderStyle::Double => "double",
    }
}

/// Serialize the computed style subset exposed through `getComputedStyle`.
fn computed_style_strings(style: &ComputedStyle, rect: Option<Rect>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut put = |name: &str, value: String| {
        out.insert(name.to_owned(), value);
    };
    put(
        "display",
        match style.display {
            Display::None => "none",
            Display::Block => "block",
            Display::Inline => "inline",
            Display::InlineBlock => "inline-block",
            Display::Flex => "flex",
            Display::Grid => "grid",
            Display::ListItem => "list-item",
            Display::Table => "table",
            Display::TableRow => "table-row",
            Display::TableRowGroup => "table-row-group",
            Display::TableCell => "table-cell",
        }
        .to_owned(),
    );
    put(
        "position",
        match style.position {
            Position::Static => "static",
            Position::Relative => "relative",
            Position::Absolute => "absolute",
            Position::Fixed => "fixed",
            Position::Sticky => "sticky",
        }
        .to_owned(),
    );
    put(
        "visibility",
        match style.visibility {
            Visibility::Visible => "visible",
            Visibility::Hidden => "hidden",
        }
        .to_owned(),
    );
    put(
        "overflow",
        match style.overflow {
            Overflow::Visible => "visible",
            Overflow::Hidden => "hidden",
            Overflow::Scroll => "scroll",
            Overflow::Auto => "auto",
        }
        .to_owned(),
    );
    put(
        "text-align",
        match style.text_align {
            TextAlign::Left => "left",
            TextAlign::Right => "right",
            TextAlign::Center => "center",
            TextAlign::Justify => "justify",
        }
        .to_owned(),
    );
    put(
        "white-space",
        match style.white_space {
            WhiteSpace::Normal => "normal",
            WhiteSpace::Nowrap => "nowrap",
            WhiteSpace::Pre => "pre",
            WhiteSpace::PreWrap => "pre-wrap",
        }
        .to_owned(),
    );
    put(
        "font-style",
        match style.font_style {
            FontStyle::Normal => "normal",
            FontStyle::Italic => "italic",
        }
        .to_owned(),
    );
    put("color", css_color(style.color));
    put("background-color", css_color(style.background_color));
    put("font-size", format!("{}px", css_number(style.font_size)));
    put("font-weight", style.font_weight.to_string());
    put("font-family", style.font_family.clone());
    put(
        "line-height",
        format!("{}px", css_number(style.line_height)),
    );
    put("opacity", css_number(style.opacity));
    put("z-index", style.z_index.to_string());
    put(
        "width",
        rect.map_or_else(
            || css_length(style.width),
            |rect| format!("{}px", css_number(rect.width)),
        ),
    );
    put(
        "height",
        rect.map_or_else(
            || css_length(style.height),
            |rect| format!("{}px", css_number(rect.height)),
        ),
    );
    for (prefix, length) in [
        ("margin-top", style.margin.top),
        ("margin-right", style.margin.right),
        ("margin-bottom", style.margin.bottom),
        ("margin-left", style.margin.left),
        ("padding-top", style.padding.top),
        ("padding-right", style.padding.right),
        ("padding-bottom", style.padding.bottom),
        ("padding-left", style.padding.left),
        ("min-width", style.min_width),
        ("max-width", style.max_width),
        ("min-height", style.min_height),
        ("max-height", style.max_height),
        ("border-radius", style.border_radius),
        ("top", style.top),
        ("right", style.right),
        ("bottom", style.bottom),
        ("left", style.left),
        ("flex-basis", style.flex_basis),
        ("row-gap", style.row_gap),
        ("column-gap", style.column_gap),
    ] {
        put(prefix, css_length(length));
    }
    for (prefix, width, border, color) in [
        (
            "border-top",
            style.border_width.top,
            style.border_style.top,
            style.border_color.top,
        ),
        (
            "border-right",
            style.border_width.right,
            style.border_style.right,
            style.border_color.right,
        ),
        (
            "border-bottom",
            style.border_width.bottom,
            style.border_style.bottom,
            style.border_color.bottom,
        ),
        (
            "border-left",
            style.border_width.left,
            style.border_style.left,
            style.border_color.left,
        ),
    ] {
        put(
            &format!("{prefix}-width"),
            format!("{}px", css_number(width)),
        );
        put(
            &format!("{prefix}-style"),
            border_style_name(border).to_owned(),
        );
        put(&format!("{prefix}-color"), css_color(color));
    }
    put("flex-grow", css_number(style.flex_grow));
    put("flex-shrink", css_number(style.flex_shrink));
    let transform = style.transform;
    put(
        "transform",
        if transform.is_identity() {
            "none".to_owned()
        } else {
            let translate_x = match transform.translate_x {
                Length::Px(value) => value,
                _ => 0.0,
            };
            let translate_y = match transform.translate_y {
                Length::Px(value) => value,
                _ => 0.0,
            };
            format!(
                "matrix({}, 0, 0, {}, {}, {})",
                css_number(transform.scale_x),
                css_number(transform.scale_y),
                css_number(translate_x),
                css_number(translate_y)
            )
        },
    );
    out
}

pub fn extract_page(page: &Page) -> ExtractedPage {
    let document = &page.document;
    let mut headings = Vec::new();
    let mut links = Vec::new();
    let mut fields = Vec::new();
    let mut buttons = Vec::new();
    for node in document.descendants(document.document_node()) {
        let Some(tag) = document.tag_name(node) else {
            continue;
        };
        let text = collapsed_text(document, node).unwrap_or_default();
        match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if !text.is_empty() {
                    headings.push(text);
                }
            }
            "a" => {
                if let Some(href) = document.get_attribute(node, "href") {
                    let href = Url::parse(href)
                        .or_else(|_| page.url.join(href))
                        .map(|url| url.to_string())
                        .unwrap_or_else(|_| href.to_owned());
                    links.push(ExtractedLink { text, href });
                }
            }
            "input" | "textarea" | "select" => {
                fields.push(ExtractedField {
                    tag: tag.to_owned(),
                    input_type: document.get_attribute(node, "type").map(str::to_owned),
                    name: document.get_attribute(node, "name").map(str::to_owned),
                    placeholder: document
                        .get_attribute(node, "placeholder")
                        .map(str::to_owned),
                    value: document.get_attribute(node, "value").map(str::to_owned),
                    disabled: element_disabled(document, node),
                });
            }
            "button" if !text.is_empty() => {
                buttons.push(text);
            }
            _ => {}
        }
    }
    ExtractedPage {
        url: page.url.to_string(),
        title: page.title(),
        text: crate::intelligence::visible_text(document)
            .chars()
            .take(20_000)
            .collect(),
        headings,
        links,
        fields,
        buttons,
    }
}

fn collapsed_text(document: &Document, node: NodeId) -> Option<String> {
    let text = collapse_whitespace(&document.text_content(node));
    if text.is_empty() {
        None
    } else {
        Some(text.chars().take(240).collect())
    }
}

fn element_disabled(document: &Document, node: NodeId) -> bool {
    document.get_attribute(node, "disabled").is_some()
        || document
            .get_attribute(node, "aria-disabled")
            .is_some_and(|value| value == "true")
}

fn input_type(document: &Document, node: NodeId) -> String {
    document
        .get_attribute(node, "type")
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn role_for(document: &Document, node: NodeId) -> Option<String> {
    if let Some(role) = document.get_attribute(node, "role") {
        let role = role.trim();
        if !role.is_empty() {
            return Some(role.to_owned());
        }
    }
    let tag = document.tag_name(node)?;
    let role = match tag {
        "a" => {
            if document.get_attribute(node, "href").is_some() {
                "link"
            } else {
                "generic"
            }
        }
        "button" => "button",
        "input" => match input_type(document, node).as_str() {
            "checkbox" => "checkbox",
            "radio" => "radio",
            "submit" | "button" | "reset" | "image" => "button",
            "range" => "slider",
            "search" => "searchbox",
            "hidden" => "none",
            _ => "textbox",
        },
        "textarea" => "textbox",
        "select" => "combobox",
        "option" => "option",
        "img" => "img",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "ul" | "ol" => "list",
        "li" => "listitem",
        "table" => "table",
        "thead" | "tbody" | "tfoot" => "rowgroup",
        "tr" => "row",
        "td" => "cell",
        "th" => "columnheader",
        "nav" => "navigation",
        "main" => "main",
        "header" => "banner",
        "footer" => "contentinfo",
        "aside" => "complementary",
        "form" => "form",
        "section" => "region",
        "article" => "article",
        "dialog" => "dialog",
        "p" => "paragraph",
        "label" => "label",
        "html" => "document",
        "body" => "generic",
        _ => "generic",
    };
    Some(role.to_owned())
}

fn accessible_name(document: &Document, node: NodeId) -> Option<String> {
    if let Some(label) = document.get_attribute(node, "aria-label") {
        let label = collapse_whitespace(label);
        if !label.is_empty() {
            return Some(label);
        }
    }
    if let Some(labelledby) = document.get_attribute(node, "aria-labelledby") {
        let mut names = Vec::new();
        for id in labelledby.split_ascii_whitespace() {
            if let Some(target) = document
                .descendants(document.document_node())
                .into_iter()
                .find(|candidate| document.get_attribute(*candidate, "id") == Some(id))
            {
                let text = collapse_whitespace(&document.text_content(target));
                if !text.is_empty() {
                    names.push(text);
                }
            }
        }
        if !names.is_empty() {
            return Some(names.join(" "));
        }
    }
    for attribute in ["alt", "title", "placeholder"] {
        if let Some(value) = document.get_attribute(node, attribute) {
            let value = collapse_whitespace(value);
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    let tag = document.tag_name(node).unwrap_or("");
    if matches!(
        tag,
        "button"
            | "a"
            | "label"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "option"
            | "td"
            | "th"
            | "summary"
            | "figcaption"
    ) {
        return collapsed_text(document, node);
    }
    if matches!(tag, "input" | "textarea" | "select") {
        if let Some(id) = document.get_attribute(node, "id")
            && let Some(label) = document
                .descendants(document.document_node())
                .into_iter()
                .find(|candidate| {
                    document.tag_name(*candidate) == Some("label")
                        && document.get_attribute(*candidate, "for") == Some(id)
                })
        {
            let text = collapse_whitespace(&document.text_content(label));
            if !text.is_empty() {
                return Some(text);
            }
        }
        return document
            .get_attribute(node, "value")
            .map(collapse_whitespace)
            .filter(|value| !value.is_empty());
    }
    None
}

fn accessibility_hidden(document: &Document, layout: &LayoutResult, node: NodeId) -> bool {
    if document
        .get_attribute(node, "aria-hidden")
        .is_some_and(|value| value == "true")
        || document.get_attribute(node, "hidden").is_some()
    {
        return true;
    }
    if document.tag_name(node) == Some("input") && input_type(document, node) == "hidden" {
        return true;
    }
    layout.style(node).is_some_and(|style| {
        style.display == Display::None || style.visibility == Visibility::Hidden
    })
}

fn build_accessibility_node(
    document: &Document,
    layout: &LayoutResult,
    node: NodeId,
) -> Option<AccessibilityNode> {
    if !document.is_element(node) || accessibility_hidden(document, layout, node) {
        return None;
    }
    let tag = document.tag_name(node).unwrap_or("");
    if matches!(
        tag,
        "script" | "style" | "head" | "meta" | "link" | "title" | "template" | "noscript"
    ) {
        return None;
    }
    let mut children = Vec::new();
    for child in document.children(node) {
        if let Some(child) = build_accessibility_node(document, layout, *child) {
            children.push(child);
        }
    }
    let role = role_for(document, node).unwrap_or_else(|| "generic".to_owned());
    let name = accessible_name(document, node);
    if role == "generic" && name.is_none() && children.is_empty() {
        return None;
    }
    let value = match tag {
        "input" => document.get_attribute(node, "value").map(str::to_owned),
        "textarea" => collapsed_text(document, node),
        _ => None,
    };
    let checked = if matches!(tag, "input")
        && matches!(input_type(document, node).as_str(), "checkbox" | "radio")
    {
        Some(
            document.get_attribute(node, "checked").is_some()
                || document
                    .get_attribute(node, "aria-checked")
                    .is_some_and(|value| value == "true"),
        )
    } else {
        None
    };
    let expanded = document
        .get_attribute(node, "aria-expanded")
        .map(|value| value == "true");
    Some(AccessibilityNode {
        node: node.index(),
        role,
        name,
        description: document.get_attribute(node, "title").map(str::to_owned),
        value,
        disabled: element_disabled(document, node),
        checked,
        expanded,
        children,
    })
}
