//! Layout engine: style-driven box tree, block flow, inline formatting,
//! basic flexbox and absolute/fixed positioning.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use image::RgbaImage;
use url::Url;

use crate::engine::css::{
    AlignContent, AlignItems, AlignSelf, BorderCollapse, BorderStyle, CaptionSide, ComputedStyle,
    Display, Edges, FlexWrap, GridPlacement, GridTrack, JustifyContent, Length, Overflow, Position,
    StyleResolver, TextAlign, Viewport, WhiteSpace, parse_declaration_list,
};
use crate::engine::dom::{Document, NodeData, NodeId};
use crate::engine::text::FontSet;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    pub fn intersection(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect {
            x,
            y,
            width: (right - x).max(0.0),
            height: (bottom - y).max(0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxKind {
    Block,
    InlineBlock,
    Flex,
    Replaced,
    AnonymousBlock,
}

#[derive(Clone, Debug)]
pub struct LayoutBox {
    pub node: Option<NodeId>,
    pub kind: BoxKind,
    pub style: Arc<ComputedStyle>,
    /// Border box.
    pub rect: Rect,
    pub margin: Edges<f32>,
    pub border: Edges<f32>,
    pub padding: Edges<f32>,
    pub children: Vec<LayoutBox>,
    pub lines: Vec<LineBox>,
    pub atomics: Vec<LayoutBox>,
    pub image: Option<String>,
    pub text: Option<String>,
    pub content_height: f32,
}

impl LayoutBox {
    pub fn content_x(&self) -> f32 {
        self.rect.x + self.border.left + self.padding.left
    }

    pub fn content_y(&self) -> f32 {
        self.rect.y + self.border.top + self.padding.top
    }

    pub fn content_width(&self) -> f32 {
        (self.rect.width
            - self.border.left
            - self.border.right
            - self.padding.left
            - self.padding.right)
            .max(0.0)
    }

    pub fn padding_box(&self) -> Rect {
        Rect {
            x: self.rect.x + self.border.left,
            y: self.rect.y + self.border.top,
            width: (self.rect.width - self.border.left - self.border.right).max(0.0),
            height: (self.rect.height - self.border.top - self.border.bottom).max(0.0),
        }
    }

    pub fn margin_box_height(&self) -> f32 {
        self.margin.top + self.rect.height + self.margin.bottom
    }

    pub fn margin_box_width(&self) -> f32 {
        self.margin.left + self.rect.width + self.margin.right
    }

    /// Resize a replaced box to a border-box width (images stretch).
    pub fn set_border_width(&mut self, width: f32) {
        let noncontent =
            self.border.left + self.border.right + self.padding.left + self.padding.right;
        self.rect.width = width.max(noncontent);
    }
}

#[derive(Clone, Debug)]
pub struct LineBox {
    pub y: f32,
    pub height: f32,
    pub baseline: f32,
    pub fragments: Vec<InlineFragment>,
}

#[derive(Clone, Debug)]
pub struct InlineFragment {
    pub x: f32,
    pub width: f32,
    pub style: Arc<ComputedStyle>,
    pub node: Option<NodeId>,
    pub kind: FragmentKind,
}

#[derive(Clone, Debug)]
pub enum FragmentKind {
    Text(String),
    Space,
    Atomic(usize),
}

pub struct LayoutResult {
    pub root: LayoutBox,
    pub viewport: Viewport,
    pub document_height: f32,
    pub node_rects: HashMap<NodeId, Rect>,
    pub styles: HashMap<NodeId, Arc<ComputedStyle>>,
}

impl LayoutResult {
    pub fn node_rect(&self, node: NodeId) -> Option<Rect> {
        self.node_rects.get(&node).copied()
    }

    pub fn style(&self, node: NodeId) -> Option<&Arc<ComputedStyle>> {
        self.styles.get(&node)
    }
}

pub fn layout_document(
    doc: &Document,
    resolver: &StyleResolver<'_>,
    fonts: &FontSet,
    images: &HashMap<String, Arc<RgbaImage>>,
    base_url: Option<&Url>,
) -> LayoutResult {
    let viewport = resolver.viewport();
    let mut engine = LayoutEngine {
        doc,
        resolver,
        fonts,
        images,
        base_url,
        viewport,
        root_font_size: 16.0,
        containing_height: Some(viewport.height),
        styles: HashMap::new(),
        positioned: Vec::new(),
        placed_positioned: HashSet::new(),
    };

    let html = doc
        .descendants(doc.document_node())
        .into_iter()
        .find(|&id| doc.tag_name(id) == Some("html"));

    let mut root = if let Some(html) = html {
        let parent = Arc::new(ComputedStyle::initial());
        let style = engine.style_for(html, &parent, 16.0);
        engine.root_font_size = style.font_size;
        let top_margin = engine.effective_top_margin(html, &style, viewport.width, 0);
        engine.layout_element(html, style, viewport.width, 0.0, top_margin, None)
    } else {
        LayoutBox {
            node: None,
            kind: BoxKind::Block,
            style: Arc::new(ComputedStyle::initial()),
            rect: Rect::default(),
            margin: Edges::<f32>::ZERO,
            border: Edges::<f32>::ZERO,
            padding: Edges::<f32>::ZERO,
            children: Vec::new(),
            lines: Vec::new(),
            atomics: Vec::new(),
            image: None,
            text: None,
            content_height: 0.0,
        }
    };

    apply_relative_offsets(&mut root, viewport);

    let mut node_rects = HashMap::new();
    collect_node_rects(&root, &mut node_rects);
    engine.layout_positioned(&mut root, &mut node_rects);

    let document_height = root.rect.bottom().max(viewport.height).max(0.0);

    LayoutResult {
        root,
        viewport,
        document_height,
        node_rects,
        styles: engine.styles,
    }
}

struct LayoutEngine<'a> {
    doc: &'a Document,
    resolver: &'a StyleResolver<'a>,
    fonts: &'a FontSet,
    images: &'a HashMap<String, Arc<RgbaImage>>,
    base_url: Option<&'a Url>,
    viewport: Viewport,
    root_font_size: f32,
    /// Definite containing-block content height for percentage heights.
    containing_height: Option<f32>,
    styles: HashMap<NodeId, Arc<ComputedStyle>>,
    positioned: Vec<NodeId>,
    placed_positioned: HashSet<NodeId>,
}

#[derive(Clone, Debug)]
enum InlineToken {
    Word {
        text: String,
        style: Arc<ComputedStyle>,
        node: NodeId,
        wrap: bool,
    },
    Space {
        style: Arc<ComputedStyle>,
    },
    Break {
        style: Arc<ComputedStyle>,
    },
    Atomic {
        index: usize,
        style: Arc<ComputedStyle>,
        node: NodeId,
    },
}

impl<'a> LayoutEngine<'a> {
    fn style_for(
        &mut self,
        node: NodeId,
        parent: &Arc<ComputedStyle>,
        root_font_size: f32,
    ) -> Arc<ComputedStyle> {
        if let Some(style) = self.styles.get(&node) {
            return Arc::clone(style);
        }
        let inline = self
            .doc
            .get_attribute(node, "style")
            .map(parse_declaration_list)
            .unwrap_or_default();
        let style = Arc::new(self.resolver.resolve(node, parent, &inline, root_font_size));
        self.styles.insert(node, Arc::clone(&style));
        style
    }

    fn resolve_edges(&self, edges: Edges<Length>, style: &ComputedStyle, base: f32) -> Edges<f32> {
        let resolve = |length: Length| {
            length
                .resolve(style.font_size, self.root_font_size, self.viewport, base)
                .unwrap_or(0.0)
                .max(0.0)
        };
        Edges {
            top: resolve(edges.top),
            right: resolve(edges.right),
            bottom: resolve(edges.bottom),
            left: resolve(edges.left),
        }
    }

    fn resolve_margins(&self, style: &ComputedStyle, base: f32) -> Edges<f32> {
        let resolve = |length: Length| {
            length
                .resolve(style.font_size, self.root_font_size, self.viewport, base)
                .unwrap_or(0.0)
        };
        Edges {
            top: resolve(style.margin.top),
            right: resolve(style.margin.right),
            bottom: resolve(style.margin.bottom),
            left: resolve(style.margin.left),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_element(
        &mut self,
        node: NodeId,
        style: Arc<ComputedStyle>,
        containing_width: f32,
        border_x: f32,
        border_y: f32,
        forced_border_width: Option<f32>,
    ) -> LayoutBox {
        let margin = self.resolve_margins(&style, containing_width);
        let padding = self.resolve_edges(style.padding, &style, containing_width);
        let border = style.border_width;

        let horizontal_noncontent = padding.left + padding.right + border.left + border.right;
        let mut content_width = if let Some(forced) = forced_border_width {
            (forced - horizontal_noncontent).max(0.0)
        } else if style.width.is_auto() {
            (containing_width - margin.left - margin.right - horizontal_noncontent).max(0.0)
        } else {
            let width = style
                .width
                .resolve(
                    style.font_size,
                    self.root_font_size,
                    self.viewport,
                    containing_width,
                )
                .unwrap_or(0.0)
                .max(0.0);
            if style.box_sizing == crate::engine::css::BoxSizing::BorderBox {
                (width - horizontal_noncontent).max(0.0)
            } else {
                width
            }
        };

        let min_width = style
            .min_width
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                containing_width,
            )
            .unwrap_or(0.0);
        let max_width = style.max_width.resolve(
            style.font_size,
            self.root_font_size,
            self.viewport,
            containing_width,
        );
        content_width = content_width.max(min_width);
        if let Some(max_width) = max_width {
            content_width = content_width.min(max_width.max(0.0));
        }

        let margin_left = margin.left;
        let margin_right = margin.right;
        let border_width = content_width + horizontal_noncontent;

        let mut layout = LayoutBox {
            node: Some(node),
            kind: if style.display == Display::Flex {
                BoxKind::Flex
            } else {
                BoxKind::Block
            },
            style: Arc::clone(&style),
            rect: Rect {
                x: border_x,
                y: border_y,
                width: border_width,
                height: 0.0,
            },
            margin: Edges {
                left: margin_left,
                right: margin_right,
                ..margin
            },
            border,
            padding,
            children: Vec::new(),
            lines: Vec::new(),
            atomics: Vec::new(),
            image: None,
            text: None,
            content_height: 0.0,
        };

        let content_x = layout.content_x();
        let content_y = layout.content_y();

        let intrinsic_noncontent =
            layout.border.top + layout.border.bottom + layout.padding.top + layout.padding.bottom;
        let explicit_content_height = match style.height {
            Length::Auto => None,
            Length::Percent(percent) => self
                .containing_height
                .map(|height| (height * percent / 100.0).max(0.0)),
            length => length.resolve(style.font_size, self.root_font_size, self.viewport, 0.0),
        }
        .map(|height| {
            if style.box_sizing == crate::engine::css::BoxSizing::BorderBox {
                (height - intrinsic_noncontent).max(0.0)
            } else {
                height
            }
        });

        let outer_containing_height = self.containing_height;
        self.containing_height = explicit_content_height;

        let content_height = if style.display == Display::Grid {
            let (children, height) =
                self.layout_grid_children(node, &style, content_width, content_x, content_y);
            layout.children = children;
            height
        } else if style.display == Display::Table {
            let (children, height) =
                self.layout_table_children(node, &style, content_width, content_x, content_y);
            layout.children = children;
            height
        } else if style.display == Display::Flex {
            let (children, height) =
                self.layout_flex_children(node, &style, content_width, content_x, content_y);
            layout.children = children;
            height
        } else if self.has_block_children(node, &style) {
            let (children, height) =
                self.layout_block_children(node, &style, content_width, content_x, content_y);
            layout.children = children;
            height
        } else {
            let children: Vec<NodeId> = self.doc.children(node).to_vec();
            let (lines, atomics, height) = self.layout_inline_with_generated(
                node,
                &style,
                &children,
                content_width,
                content_x,
                content_y,
            );
            layout.lines = lines;
            layout.atomics = atomics;
            height
        };

        self.containing_height = outer_containing_height;

        let mut final_height = explicit_content_height.unwrap_or(content_height);
        if let Some(min_height) = resolve_block_height(
            style.min_height,
            &style,
            self.containing_height,
            self.root_font_size,
            self.viewport,
        ) {
            final_height = final_height.max(min_height);
        }
        if let Some(max_height) = resolve_block_height(
            style.max_height,
            &style,
            self.containing_height,
            self.root_font_size,
            self.viewport,
        ) {
            final_height = final_height.min(max_height.max(0.0));
        }

        layout.content_height = final_height;
        layout.rect.height = final_height
            + layout.border.top
            + layout.border.bottom
            + layout.padding.top
            + layout.padding.bottom;
        layout
    }

    fn has_block_children(&mut self, node: NodeId, style: &Arc<ComputedStyle>) -> bool {
        let children: Vec<NodeId> = self.doc.children(node).to_vec();
        for child in children {
            if let NodeData::Element(el) = &self.doc.node(child).data {
                let child_style = self.style_for(child, style, self.root_font_size);
                if child_style.display == Display::None {
                    continue;
                }
                if is_block_level(child_style.display) {
                    return true;
                }
                // An inline element that contains block descendants also
                // forces block formatting at this level.
                if el.name == "br" {
                    continue;
                }
                if self.has_nested_block(child, &child_style) {
                    return true;
                }
            }
        }
        false
    }

    fn has_nested_block(&mut self, node: NodeId, style: &Arc<ComputedStyle>) -> bool {
        let children: Vec<NodeId> = self.doc.children(node).to_vec();
        for child in children {
            if let NodeData::Element(_) = &self.doc.node(child).data {
                let child_style = self.style_for(child, style, self.root_font_size);
                if child_style.display == Display::None {
                    continue;
                }
                if is_block_level(child_style.display) {
                    return true;
                }
                if self.has_nested_block(child, &child_style) {
                    return true;
                }
            }
        }
        false
    }

    fn layout_block_children(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        content_width: f32,
        content_x: f32,
        content_y: f32,
    ) -> (Vec<LayoutBox>, f32) {
        let children: Vec<NodeId> = self.doc.children(node).to_vec();
        let mut boxes = Vec::new();
        let mut y = content_y;
        let mut prev_bottom = 0.0f32;
        let mut first = true;
        let mut inline_group: Vec<NodeId> = Vec::new();

        if let Some((text, pseudo_style)) = self.pseudo_content(node, style, "before") {
            let generated =
                self.layout_generated_text(&text, &pseudo_style, node, content_width, content_x, y);
            y += generated.rect.height;
            boxes.push(generated);
            prev_bottom = 0.0;
            first = false;
        }

        for child in children {
            match &self.doc.node(child).data {
                NodeData::Text(text) => {
                    let preserve =
                        matches!(style.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap);
                    if preserve || !text.trim().is_empty() {
                        inline_group.push(child);
                    }
                }
                NodeData::Element(_) => {
                    let child_style = self.style_for(child, style, self.root_font_size);
                    if child_style.display == Display::None {
                        continue;
                    }
                    if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                        self.positioned.push(child);
                        continue;
                    }
                    if is_block_level(child_style.display) {
                        if !inline_group.is_empty() {
                            let group = std::mem::take(&mut inline_group);
                            let anonymous = self.layout_anonymous_inline(
                                &group,
                                style,
                                content_width,
                                content_x,
                                y,
                            );
                            y += anonymous.rect.height;
                            boxes.push(anonymous);
                            prev_bottom = 0.0;
                            first = false;
                        }

                        let child_margin = self.resolve_margins(&child_style, content_width);
                        let child_top =
                            self.effective_top_margin(child, &child_style, content_width, 0);
                        let child_bottom =
                            self.effective_bottom_margin(child, &child_style, content_width, 0);
                        let collapses_into_parent = first
                            && inline_group.is_empty()
                            && self.allows_top_margin_collapse(style, content_width);
                        let gap = if first {
                            if collapses_into_parent {
                                0.0
                            } else {
                                child_top
                            }
                        } else {
                            collapse_margins(prev_bottom, child_top)
                        };
                        y += gap;
                        let child_x = content_x + child_margin.left;
                        let mut child_box = if self.doc.tag_name(child).is_some_and(is_replaced_tag)
                        {
                            let mut box_ = self.layout_replaced(child, &child_style, content_width);
                            let (old_x, old_y) = (box_.rect.x, box_.rect.y);
                            shift_box(&mut box_, child_x - old_x, y - old_y);
                            box_
                        } else {
                            self.layout_element(
                                child,
                                child_style.clone(),
                                content_width,
                                child_x,
                                y,
                                None,
                            )
                        };
                        self.apply_auto_margins(&mut child_box, &child_style, content_width);
                        y += child_box.rect.height;
                        prev_bottom = child_bottom;
                        boxes.push(child_box);
                        first = false;
                    } else {
                        inline_group.push(child);
                    }
                }
                _ => {}
            }
        }

        if !inline_group.is_empty() {
            let anonymous =
                self.layout_anonymous_inline(&inline_group, style, content_width, content_x, y);
            y += anonymous.rect.height;
            boxes.push(anonymous);
            prev_bottom = 0.0;
        }

        if !self.allows_bottom_margin_collapse(style, content_width) {
            y += prev_bottom;
        }
        if let Some((text, pseudo_style)) = self.pseudo_content(node, style, "after") {
            let generated =
                self.layout_generated_text(&text, &pseudo_style, node, content_width, content_x, y);
            y += generated.rect.height;
            boxes.push(generated);
        }
        (boxes, (y - content_y).max(0.0))
    }

    fn list_marker(&mut self, node: NodeId, style: &Arc<ComputedStyle>) -> Option<String> {
        if style.display != Display::ListItem {
            return None;
        }
        match style.list_style_type {
            crate::engine::css::ListStyleType::None => None,
            crate::engine::css::ListStyleType::Disc => Some("• ".to_owned()),
            crate::engine::css::ListStyleType::Circle => Some("◦ ".to_owned()),
            crate::engine::css::ListStyleType::Square => Some("▪ ".to_owned()),
            crate::engine::css::ListStyleType::Decimal => {
                let parent = self.doc.parent(node)?;
                let mut count = 0usize;
                for &sibling in self.doc.children(parent) {
                    if sibling == node {
                        break;
                    }
                    if self.doc.tag_name(sibling) == Some("li") {
                        count += 1;
                    }
                }
                Some(format!("{}. ", count + 1))
            }
        }
    }

    fn pseudo_content(
        &mut self,
        node: NodeId,
        element_style: &Arc<ComputedStyle>,
        pseudo: &str,
    ) -> Option<(String, Arc<ComputedStyle>)> {
        let style = self
            .resolver
            .resolve_pseudo(node, element_style, pseudo, self.root_font_size);
        let text = style.content.clone().unwrap_or_default();
        if text.is_empty() {
            return None;
        }
        Some((text, Arc::new(style)))
    }

    fn layout_inline_with_generated(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        children: &[NodeId],
        content_width: f32,
        content_x: f32,
        content_y: f32,
    ) -> (Vec<LineBox>, Vec<LayoutBox>, f32) {
        let before = self.pseudo_content(node, style, "before");
        let after = self.pseudo_content(node, style, "after");
        let mut tokens = Vec::new();
        let mut atomics = Vec::new();
        if let Some(marker) = self.list_marker(node, style) {
            push_text_tokens(&marker, style, node, &mut tokens);
        }
        if let Some((text, pseudo_style)) = before {
            push_text_tokens(&text, &pseudo_style, node, &mut tokens);
        }
        self.flatten_nodes(children, style, content_width, &mut tokens, &mut atomics);
        if let Some((text, pseudo_style)) = after {
            push_text_tokens(&text, &pseudo_style, node, &mut tokens);
        }
        let (lines, height) = self.build_lines(
            &tokens,
            content_width,
            content_x,
            content_y,
            &mut atomics,
            style,
        );
        (lines, atomics, height)
    }

    fn layout_generated_text(
        &mut self,
        text: &str,
        style: &Arc<ComputedStyle>,
        node: NodeId,
        content_width: f32,
        content_x: f32,
        y: f32,
    ) -> LayoutBox {
        let mut tokens = Vec::new();
        push_text_tokens(text, style, node, &mut tokens);
        let mut atomics = Vec::new();
        let (lines, height) =
            self.build_lines(&tokens, content_width, content_x, y, &mut atomics, style);
        let mut wrapper = (**style).clone();
        wrapper.display = Display::Block;
        wrapper.margin = Edges::<Length>::ZERO;
        wrapper.padding = Edges::<Length>::ZERO;
        wrapper.border_width = Edges::<f32>::ZERO;
        wrapper.border_style = Edges::all(BorderStyle::None);
        wrapper.background_color = crate::engine::css::Color::TRANSPARENT;
        wrapper.background_image = None;
        wrapper.width = Length::Auto;
        wrapper.height = Length::Auto;
        LayoutBox {
            node: None,
            kind: BoxKind::AnonymousBlock,
            style: Arc::new(wrapper),
            rect: Rect {
                x: content_x,
                y,
                width: content_width,
                height,
            },
            margin: Edges::<f32>::ZERO,
            border: Edges::<f32>::ZERO,
            padding: Edges::<f32>::ZERO,
            children: Vec::new(),
            lines,
            atomics,
            image: None,
            text: None,
            content_height: height,
        }
    }

    fn collapse_allowed_base(&self, style: &ComputedStyle, containing_width: f32) -> bool {
        if !matches!(style.display, Display::Block | Display::ListItem) {
            return false;
        }
        if matches!(style.position, Position::Absolute | Position::Fixed) {
            return false;
        }
        if style.overflow != Overflow::Visible {
            return false;
        }
        if style.border_width.top > 0.0 || style.border_width.bottom > 0.0 {
            return false;
        }
        let padding_top = style
            .padding
            .top
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                containing_width,
            )
            .unwrap_or(0.0);
        let padding_bottom = style
            .padding
            .bottom
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                containing_width,
            )
            .unwrap_or(0.0);
        padding_top <= 0.0 && padding_bottom <= 0.0
    }

    fn allows_top_margin_collapse(&self, style: &ComputedStyle, containing_width: f32) -> bool {
        self.collapse_allowed_base(style, containing_width)
    }

    fn allows_bottom_margin_collapse(&self, style: &ComputedStyle, containing_width: f32) -> bool {
        self.collapse_allowed_base(style, containing_width) && style.height.is_auto()
    }

    fn first_block_child(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
    ) -> Option<(NodeId, Arc<ComputedStyle>)> {
        let children: Vec<NodeId> = self.doc.children(node).to_vec();
        for child in children {
            match &self.doc.node(child).data {
                NodeData::Text(text) => {
                    let preserve =
                        matches!(style.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap);
                    if preserve || !text.trim().is_empty() {
                        return None;
                    }
                }
                NodeData::Element(_) => {
                    let child_style = self.style_for(child, style, self.root_font_size);
                    if child_style.display == Display::None
                        || matches!(child_style.position, Position::Absolute | Position::Fixed)
                    {
                        continue;
                    }
                    if is_block_level(child_style.display) {
                        return Some((child, child_style));
                    }
                    return None;
                }
                _ => {}
            }
        }
        None
    }

    fn last_block_child(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
    ) -> Option<(NodeId, Arc<ComputedStyle>)> {
        let children: Vec<NodeId> = self.doc.children(node).to_vec();
        for child in children.into_iter().rev() {
            match &self.doc.node(child).data {
                NodeData::Text(text) => {
                    let preserve =
                        matches!(style.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap);
                    if preserve || !text.trim().is_empty() {
                        return None;
                    }
                }
                NodeData::Element(_) => {
                    let child_style = self.style_for(child, style, self.root_font_size);
                    if child_style.display == Display::None
                        || matches!(child_style.position, Position::Absolute | Position::Fixed)
                    {
                        continue;
                    }
                    if is_block_level(child_style.display) {
                        return Some((child, child_style));
                    }
                    return None;
                }
                _ => {}
            }
        }
        None
    }

    fn effective_top_margin(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        containing_width: f32,
        depth: usize,
    ) -> f32 {
        let own = style
            .margin
            .top
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                containing_width,
            )
            .unwrap_or(0.0);
        if depth >= 32 || !self.allows_top_margin_collapse(style, containing_width) {
            return own;
        }
        let Some((child, child_style)) = self.first_block_child(node, style) else {
            return own;
        };
        let child_top = self.effective_top_margin(child, &child_style, containing_width, depth + 1);
        collapse_margins(own, child_top)
    }

    fn effective_bottom_margin(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        containing_width: f32,
        depth: usize,
    ) -> f32 {
        let own = style
            .margin
            .bottom
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                containing_width,
            )
            .unwrap_or(0.0);
        if depth >= 32 || !self.allows_bottom_margin_collapse(style, containing_width) {
            return own;
        }
        let Some((child, child_style)) = self.last_block_child(node, style) else {
            return own;
        };
        let child_bottom =
            self.effective_bottom_margin(child, &child_style, containing_width, depth + 1);
        collapse_margins(own, child_bottom)
    }

    fn apply_auto_margins(
        &self,
        box_: &mut LayoutBox,
        style: &ComputedStyle,
        containing_width: f32,
    ) {
        if style.position != Position::Static || style.width.is_auto() {
            return;
        }
        if !(style.margin.left.is_auto() || style.margin.right.is_auto()) {
            return;
        }
        let remaining =
            (containing_width - box_.rect.width - box_.margin.left - box_.margin.right).max(0.0);
        let (left, right) = match (style.margin.left.is_auto(), style.margin.right.is_auto()) {
            (true, true) => (remaining / 2.0, remaining / 2.0),
            (true, false) => (remaining, box_.margin.right),
            (false, true) => (box_.margin.left, remaining),
            (false, false) => return,
        };
        let dx = left - box_.margin.left;
        box_.margin.left = left;
        box_.margin.right = right;
        if dx != 0.0 {
            shift_box(box_, dx, 0.0);
        }
    }

    fn layout_anonymous_inline(
        &mut self,
        nodes: &[NodeId],
        parent_style: &Arc<ComputedStyle>,
        content_width: f32,
        content_x: f32,
        y: f32,
    ) -> LayoutBox {
        let (lines, atomics, height) =
            self.layout_inline_nodes(nodes, parent_style, content_width, content_x, y);
        let mut style = (**parent_style).clone();
        style.display = Display::Block;
        style.margin = Edges::<Length>::ZERO;
        style.padding = Edges::<Length>::ZERO;
        style.border_width = Edges::<f32>::ZERO;
        style.border_style = Edges::all(BorderStyle::None);
        style.width = Length::Auto;
        style.height = Length::Auto;
        LayoutBox {
            node: None,
            kind: BoxKind::AnonymousBlock,
            style: Arc::new(style),
            rect: Rect {
                x: content_x,
                y,
                width: content_width,
                height,
            },
            margin: Edges::<f32>::ZERO,
            border: Edges::<f32>::ZERO,
            padding: Edges::<f32>::ZERO,
            children: Vec::new(),
            lines,
            atomics,
            image: None,
            text: None,
            content_height: height,
        }
    }

    fn layout_inline_nodes(
        &mut self,
        nodes: &[NodeId],
        parent_style: &Arc<ComputedStyle>,
        content_width: f32,
        content_x: f32,
        content_y: f32,
    ) -> (Vec<LineBox>, Vec<LayoutBox>, f32) {
        let mut tokens = Vec::new();
        let mut atomics = Vec::new();
        self.flatten_nodes(
            nodes,
            parent_style,
            content_width,
            &mut tokens,
            &mut atomics,
        );
        let (lines, height) = self.build_lines(
            &tokens,
            content_width,
            content_x,
            content_y,
            &mut atomics,
            parent_style,
        );
        (lines, atomics, height)
    }

    fn flatten_nodes(
        &mut self,
        nodes: &[NodeId],
        style: &Arc<ComputedStyle>,
        available_width: f32,
        tokens: &mut Vec<InlineToken>,
        atomics: &mut Vec<LayoutBox>,
    ) {
        for &node in nodes {
            match self.doc.node(node).data.clone() {
                NodeData::Text(text) => {
                    push_text_tokens(&text, style, node, tokens);
                }
                NodeData::Element(element) => {
                    let child_style = self.style_for(node, style, self.root_font_size);
                    if child_style.display == Display::None {
                        continue;
                    }
                    if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                        self.positioned.push(node);
                        continue;
                    }
                    if element.name == "br" {
                        tokens.push(InlineToken::Break {
                            style: Arc::clone(&child_style),
                        });
                        continue;
                    }
                    if is_replaced_tag(&element.name) {
                        let atomic = self.layout_replaced(node, &child_style, available_width);
                        let index = atomics.len();
                        atomics.push(atomic);
                        tokens.push(InlineToken::Atomic {
                            index,
                            style: Arc::clone(&child_style),
                            node,
                        });
                        continue;
                    }
                    match child_style.display {
                        Display::Inline => {
                            let children: Vec<NodeId> = self.doc.children(node).to_vec();
                            self.flatten_nodes(
                                &children,
                                &child_style,
                                available_width,
                                tokens,
                                atomics,
                            );
                        }
                        Display::InlineBlock => {
                            let token_style = Arc::clone(&child_style);
                            let atomic =
                                self.layout_inline_block(node, child_style, available_width);
                            let index = atomics.len();
                            atomics.push(atomic);
                            tokens.push(InlineToken::Atomic {
                                index,
                                style: token_style,
                                node,
                            });
                        }
                        _ => {
                            let token_style = Arc::clone(&child_style);
                            let atomic = self.layout_element(
                                node,
                                child_style,
                                available_width,
                                0.0,
                                0.0,
                                None,
                            );
                            let index = atomics.len();
                            atomics.push(atomic);
                            tokens.push(InlineToken::Atomic {
                                index,
                                style: token_style,
                                node,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn layout_inline_block(
        &mut self,
        node: NodeId,
        style: Arc<ComputedStyle>,
        available_width: f32,
    ) -> LayoutBox {
        let margins = self.resolve_margins(&style, available_width);
        let padding = self.resolve_edges(style.padding, &style, available_width);
        let border = style.border_width;
        let noncontent = padding.left + padding.right + border.left + border.right;
        let border_width = if style.width.is_auto() {
            let intrinsic = self.intrinsic_width(node, &style);
            let limit = (available_width - margins.left - margins.right).max(0.0);
            (intrinsic + noncontent).min(limit).max(noncontent)
        } else {
            let width = style
                .width
                .resolve(
                    style.font_size,
                    self.root_font_size,
                    self.viewport,
                    available_width,
                )
                .unwrap_or(0.0);
            match style.box_sizing {
                crate::engine::css::BoxSizing::BorderBox => width.max(noncontent),
                crate::engine::css::BoxSizing::ContentBox => width + noncontent,
            }
        };
        let mut box_ =
            self.layout_element(node, style, available_width, 0.0, 0.0, Some(border_width));
        box_.kind = BoxKind::InlineBlock;
        box_
    }

    fn intrinsic_width(&mut self, node: NodeId, style: &Arc<ComputedStyle>) -> f32 {
        match self.doc.node(node).data.clone() {
            NodeData::Text(text) => self.fonts.text_width(&text, style),
            NodeData::Element(element) => {
                if is_replaced_tag(&element.name) {
                    return self.replaced_intrinsic_width(node, style);
                }
                if !style.width.is_auto() {
                    return style
                        .width
                        .resolve(style.font_size, self.root_font_size, self.viewport, 0.0)
                        .unwrap_or(0.0);
                }
                let children: Vec<NodeId> = self.doc.children(node).to_vec();

                if style.display == Display::Flex {
                    let row = matches!(
                        style.flex_direction,
                        crate::engine::css::FlexDirection::Row
                            | crate::engine::css::FlexDirection::RowReverse
                    );
                    let gap = if row { style.column_gap } else { style.row_gap }
                        .resolve(style.font_size, self.root_font_size, self.viewport, 0.0)
                        .unwrap_or(0.0)
                        .max(0.0);
                    let mut total = 0.0f32;
                    let mut maximum = 0.0f32;
                    let mut count = 0usize;
                    for child in children {
                        let width = match self.doc.node(child).data.clone() {
                            NodeData::Text(text) => {
                                if text.trim().is_empty() {
                                    continue;
                                }
                                self.fonts.text_width(&text, style)
                            }
                            NodeData::Element(_) => {
                                let child_style = self.style_for(child, style, self.root_font_size);
                                if child_style.display == Display::None {
                                    continue;
                                }
                                let margins = self.resolve_margins(&child_style, 0.0);
                                self.intrinsic_width(child, &child_style)
                                    + margins.left
                                    + margins.right
                            }
                            _ => continue,
                        };
                        total += width;
                        maximum = maximum.max(width);
                        count += 1;
                    }
                    return if row {
                        total + gap * count.saturating_sub(1) as f32
                    } else {
                        maximum
                    };
                }

                let mut max_line = 0.0f32;
                let mut line = 0.0f32;
                for child in children {
                    match self.doc.node(child).data.clone() {
                        NodeData::Text(text) => {
                            line += self.fonts.text_width(&text, style);
                        }
                        NodeData::Element(element) => {
                            if element.name == "br" {
                                max_line = max_line.max(line);
                                line = 0.0;
                                continue;
                            }
                            let child_style = self.style_for(child, style, self.root_font_size);
                            if child_style.display == Display::None {
                                continue;
                            }
                            let width = self.intrinsic_width(child, &child_style);
                            if is_block_level(child_style.display) {
                                max_line = max_line.max(line);
                                line = 0.0;
                                max_line = max_line.max(width);
                            } else {
                                let margins = self.resolve_margins(&child_style, 0.0);
                                line += width + margins.left + margins.right;
                            }
                        }
                        _ => {}
                    }
                }
                max_line.max(line)
            }
            _ => 0.0,
        }
    }

    fn replaced_intrinsic_width(&mut self, node: NodeId, style: &Arc<ComputedStyle>) -> f32 {
        self.replaced_box(node, style, 10000.0).rect.width
    }

    fn layout_replaced(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        available_width: f32,
    ) -> LayoutBox {
        self.replaced_box(node, style, available_width)
    }

    fn replaced_box(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        available_width: f32,
    ) -> LayoutBox {
        let tag = self.doc.tag_name(node).unwrap_or("").to_owned();
        let margin = self.resolve_margins(style, available_width);
        let padding = self.resolve_edges(style.padding, style, available_width);
        let border = style.border_width;

        let attr = |name: &str| {
            self.doc
                .get_attribute(node, name)
                .and_then(|value| value.trim().parse::<f32>().ok())
        };

        let mut image_url = None;
        let mut intrinsic = None;
        if tag == "img"
            && let Some(src) = self.doc.get_attribute(node, "src")
            && let Some(resolved) = self.resolve_url(src)
        {
            if let Some(image) = self.images.get(&resolved) {
                intrinsic = Some((image.width() as f32, image.height() as f32));
            }
            image_url = Some(resolved);
        }

        let css_width = if style.width.is_auto() {
            None
        } else {
            style.width.resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                available_width,
            )
        };
        let css_height = match style.height {
            Length::Auto | Length::Percent(_) => None,
            length => length.resolve(style.font_size, self.root_font_size, self.viewport, 0.0),
        };

        let default_size = match tag.as_str() {
            "input" => {
                let input_type = self
                    .doc
                    .get_attribute(node, "type")
                    .unwrap_or("text")
                    .to_ascii_lowercase();
                if matches!(input_type.as_str(), "checkbox" | "radio") {
                    (13.0, 13.0)
                } else {
                    (173.0, 21.0)
                }
            }
            "select" => (100.0, 21.0),
            "textarea" => (200.0, 60.0),
            _ => (300.0, 150.0),
        };

        let natural_width = intrinsic
            .map(|(w, _)| w)
            .or_else(|| attr("width"))
            .unwrap_or(default_size.0);
        let natural_height = intrinsic
            .map(|(_, h)| h)
            .or_else(|| attr("height"))
            .unwrap_or(default_size.1);

        let content_width = css_width
            .or_else(|| attr("width"))
            .or_else(|| intrinsic.map(|(w, _)| w))
            .unwrap_or(natural_width)
            .max(0.0);
        let content_height = if let Some(height) = css_height.or_else(|| attr("height")) {
            height.max(0.0)
        } else if let Some((iw, ih)) = intrinsic {
            if let Some(width) = css_width.or_else(|| attr("width")) {
                if iw > 0.0 { width * ih / iw } else { ih }
            } else {
                ih
            }
        } else {
            natural_height
        };

        let border_width =
            content_width + padding.left + padding.right + border.left + border.right;
        let border_height =
            content_height + padding.top + padding.bottom + border.top + border.bottom;

        let replaced_text = match tag.as_str() {
            "input" => {
                let input_type = self
                    .doc
                    .get_attribute(node, "type")
                    .unwrap_or("text")
                    .to_ascii_lowercase();
                if matches!(
                    input_type.as_str(),
                    "checkbox" | "radio" | "hidden" | "file" | "range"
                ) {
                    None
                } else if let Some(value) = self.doc.get_attribute(node, "value") {
                    Some(value.to_owned())
                } else {
                    self.doc
                        .get_attribute(node, "placeholder")
                        .map(str::to_owned)
                }
            }
            "textarea" => Some(self.doc.text_content(node)),
            "select" => self
                .doc
                .descendants(node)
                .into_iter()
                .find(|&id| self.doc.tag_name(id) == Some("option"))
                .map(|id| self.doc.text_content(id).trim().to_owned()),
            _ => None,
        };

        LayoutBox {
            node: Some(node),
            kind: BoxKind::Replaced,
            style: Arc::clone(style),
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: border_width,
                height: border_height,
            },
            margin,
            border,
            padding,
            children: Vec::new(),
            lines: Vec::new(),
            atomics: Vec::new(),
            image: image_url,
            text: replaced_text,
            content_height,
        }
    }

    fn resolve_url(&self, raw: &str) -> Option<String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some(base) = self.base_url {
            base.join(raw).ok().map(|url| url.to_string())
        } else {
            Url::parse(raw).ok().map(|url| url.to_string())
        }
    }

    fn build_lines(
        &self,
        tokens: &[InlineToken],
        max_width: f32,
        origin_x: f32,
        origin_y: f32,
        atomics: &mut [LayoutBox],
        container_style: &Arc<ComputedStyle>,
    ) -> (Vec<LineBox>, f32) {
        let mut lines: Vec<LineBox> = Vec::new();
        let mut fragments: Vec<InlineFragment> = Vec::new();
        let mut line_width = 0.0f32;
        let mut y = origin_y;
        let mut pending_space: Option<Arc<ComputedStyle>> = None;
        let align = container_style.text_align;

        for token in tokens {
            match token {
                InlineToken::Word {
                    text,
                    style,
                    node,
                    wrap,
                } => {
                    let word_width = self.fonts.text_width(text, style);
                    let space_width = pending_space
                        .as_ref()
                        .map(|space_style| self.fonts.text_width(" ", space_style))
                        .unwrap_or(0.0);
                    let needed = if fragments.is_empty() {
                        word_width
                    } else {
                        space_width + word_width
                    };
                    if !fragments.is_empty() && *wrap && line_width + needed > max_width + 0.01 {
                        self.finish_line(
                            std::mem::take(&mut fragments),
                            line_width,
                            &mut y,
                            &mut lines,
                            atomics,
                            max_width,
                            align,
                            container_style,
                        );
                        line_width = 0.0;
                        pending_space = None;
                    }
                    if !fragments.is_empty()
                        && let Some(space_style) = pending_space.take()
                    {
                        let width = self.fonts.text_width(" ", &space_style);
                        fragments.push(InlineFragment {
                            x: origin_x + line_width,
                            width,
                            style: space_style,
                            node: Some(*node),
                            kind: FragmentKind::Space,
                        });
                        line_width += width;
                    }
                    pending_space = None;
                    fragments.push(InlineFragment {
                        x: origin_x + line_width,
                        width: word_width,
                        style: Arc::clone(style),
                        node: Some(*node),
                        kind: FragmentKind::Text(text.clone()),
                    });
                    line_width += word_width;
                }
                InlineToken::Space { style } => {
                    pending_space = Some(Arc::clone(style));
                }
                InlineToken::Break { style } => {
                    pending_space = None;
                    self.finish_line(
                        std::mem::take(&mut fragments),
                        line_width,
                        &mut y,
                        &mut lines,
                        atomics,
                        max_width,
                        align,
                        style,
                    );
                    line_width = 0.0;
                }
                InlineToken::Atomic { index, style, node } => {
                    let atomic = &atomics[*index];
                    let width = atomic.margin_box_width();
                    let space_width = pending_space
                        .as_ref()
                        .map(|space_style| self.fonts.text_width(" ", space_style))
                        .unwrap_or(0.0);
                    let needed = if fragments.is_empty() {
                        width
                    } else {
                        space_width + width
                    };
                    if !fragments.is_empty() && line_width + needed > max_width + 0.01 {
                        self.finish_line(
                            std::mem::take(&mut fragments),
                            line_width,
                            &mut y,
                            &mut lines,
                            atomics,
                            max_width,
                            align,
                            container_style,
                        );
                        line_width = 0.0;
                        pending_space = None;
                    }
                    if !fragments.is_empty()
                        && let Some(space_style) = pending_space.take()
                    {
                        let space = self.fonts.text_width(" ", &space_style);
                        fragments.push(InlineFragment {
                            x: origin_x + line_width,
                            width: space,
                            style: space_style,
                            node: Some(*node),
                            kind: FragmentKind::Space,
                        });
                        line_width += space;
                    }
                    pending_space = None;
                    fragments.push(InlineFragment {
                        x: origin_x + line_width,
                        width,
                        style: Arc::clone(style),
                        node: Some(*node),
                        kind: FragmentKind::Atomic(*index),
                    });
                    line_width += width;
                }
            }
        }

        if !fragments.is_empty() {
            self.finish_line(
                std::mem::take(&mut fragments),
                line_width,
                &mut y,
                &mut lines,
                atomics,
                max_width,
                align,
                container_style,
            );
        }

        (lines, (y - origin_y).max(0.0))
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_line(
        &self,
        fragments: Vec<InlineFragment>,
        line_width: f32,
        y: &mut f32,
        lines: &mut Vec<LineBox>,
        atomics: &mut [LayoutBox],
        max_width: f32,
        align: TextAlign,
        default_style: &Arc<ComputedStyle>,
    ) {
        if fragments.is_empty() {
            let ascent = self.fonts.ascent(default_style) + 2.0;
            let height = default_style.line_height.max(ascent + 2.0);
            lines.push(LineBox {
                y: *y,
                height,
                baseline: *y + ascent,
                fragments: Vec::new(),
            });
            *y += height;
            return;
        }

        let mut ascent = 0.0f32;
        let mut descent = 0.0f32;
        for fragment in &fragments {
            match fragment.kind {
                FragmentKind::Atomic(index) => {
                    let atomic = &atomics[index];
                    ascent = ascent.max(atomic.margin_box_height());
                }
                _ => {
                    let a = self.fonts.ascent(&fragment.style);
                    let d = self.fonts.descent(&fragment.style);
                    let leading = (fragment.style.line_height - (a + d)).max(0.0) / 2.0;
                    ascent = ascent.max(a + leading);
                    descent = descent.max(d + leading);
                }
            }
        }
        let height = ascent + descent;
        let baseline = *y + ascent;
        let offset = match align {
            TextAlign::Center => ((max_width - line_width) / 2.0).max(0.0),
            TextAlign::Right => (max_width - line_width).max(0.0),
            _ => 0.0,
        };

        let mut placed = fragments;
        for fragment in &mut placed {
            fragment.x += offset;
            if let FragmentKind::Atomic(index) = fragment.kind {
                let atomic = &mut atomics[index];
                atomic.rect.x = fragment.x + atomic.margin.left;
                atomic.rect.y = baseline - atomic.margin.bottom - atomic.rect.height;
            }
        }

        lines.push(LineBox {
            y: *y,
            height,
            baseline,
            fragments: placed,
        });
        *y += height.max(1.0);
    }

    fn layout_table_children(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        content_width: f32,
        content_x: f32,
        content_y: f32,
    ) -> (Vec<LayoutBox>, f32) {
        let rows = self.collect_table_rows(node, style);
        let collapse = self.table_border_collapse(node);

        // Collapsed borders consume the spacing between cells; the legacy
        // `cellspacing` attribute only applies to the separated model.
        let mut spacing_x = if collapse {
            0.0
        } else {
            style.border_spacing_x
        };
        let mut spacing_y = if collapse {
            0.0
        } else {
            style.border_spacing_y
        };
        if !collapse
            && let Some(value) = self
                .doc
                .get_attribute(node, "cellspacing")
                .and_then(|value| value.trim().parse::<f32>().ok())
        {
            spacing_x = value.max(0.0);
            spacing_y = value.max(0.0);
        }

        let (placements, column_count) = self.place_table_cells(&rows);

        let mut top_captions: Vec<(NodeId, Arc<ComputedStyle>)> = Vec::new();
        let mut bottom_captions: Vec<(NodeId, Arc<ComputedStyle>)> = Vec::new();
        for (caption, caption_style) in self.collect_table_captions(node, style) {
            if self.caption_is_bottom(caption, node) {
                bottom_captions.push((caption, caption_style));
            } else {
                top_captions.push((caption, caption_style));
            }
        }

        let (mut boxes, top_caption_height) =
            self.layout_caption_group(&top_captions, content_width, content_x, content_y);
        let grid_start = content_y + top_caption_height;
        let (grid_boxes, grid_height) = self.layout_table_grid(
            &rows,
            &placements,
            column_count,
            content_width,
            content_x,
            grid_start,
            spacing_x,
            spacing_y,
            collapse,
        );
        let (bottom_caption_boxes, bottom_caption_height) = self.layout_caption_group(
            &bottom_captions,
            content_width,
            content_x,
            grid_start + grid_height,
        );

        boxes.extend(grid_boxes);
        boxes.extend(bottom_caption_boxes);
        (
            boxes,
            top_caption_height + grid_height + bottom_caption_height,
        )
    }

    /// Place every cell into a row/column grid, honouring `rowspan` occupancy
    /// so later cells shift right around cells that span into their row.
    fn place_table_cells(
        &mut self,
        rows: &[Vec<(NodeId, Arc<ComputedStyle>)>],
    ) -> (Vec<TableCellPlacement>, usize) {
        let mut placements = Vec::new();
        let mut occupied: Vec<Vec<bool>> = vec![Vec::new(); rows.len()];
        let mut column_count = 0usize;

        for (row_index, row) in rows.iter().enumerate() {
            let mut column = 0usize;
            for (cell, cell_style) in row {
                while occupied[row_index].get(column).copied().unwrap_or(false) {
                    column += 1;
                }
                let colspan = self.cell_colspan(*cell);
                let rowspan = self
                    .cell_rowspan(*cell)
                    .min(rows.len().saturating_sub(row_index))
                    .max(1);
                for row_slots in occupied.iter_mut().skip(row_index).take(rowspan) {
                    if row_slots.len() < column + colspan {
                        row_slots.resize(column + colspan, false);
                    }
                    for slot in &mut row_slots[column..column + colspan] {
                        *slot = true;
                    }
                }
                column_count = column_count.max(column + colspan);
                placements.push(TableCellPlacement {
                    cell: *cell,
                    style: Arc::clone(cell_style),
                    row: row_index,
                    column,
                    colspan,
                    rowspan,
                });
                column += colspan;
            }
        }
        (placements, column_count)
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_table_grid(
        &mut self,
        rows: &[Vec<(NodeId, Arc<ComputedStyle>)>],
        placements: &[TableCellPlacement],
        column_count: usize,
        content_width: f32,
        content_x: f32,
        start_y: f32,
        spacing_x: f32,
        spacing_y: f32,
        collapse: bool,
    ) -> (Vec<LayoutBox>, f32) {
        if column_count == 0 || rows.is_empty() {
            return (Vec::new(), 0.0);
        }

        // Border-collapse: half of every internal border belongs to the cell
        // on each side, so abutting cells share a single border whose width is
        // roughly the average of the two halves. Outer cell edges keep their
        // full border, matching the table border that surrounds them.
        let effective: Vec<Arc<ComputedStyle>> = placements
            .iter()
            .map(|placement| {
                if !collapse {
                    return Arc::clone(&placement.style);
                }
                let mut cell_style = (*placement.style).clone();
                let left_edge = placement.column == 0;
                let right_edge = placement.column + placement.colspan >= column_count;
                let top_edge = placement.row == 0;
                let bottom_edge = placement.row + placement.rowspan >= rows.len();
                if !left_edge {
                    cell_style.border_width.left *= 0.5;
                }
                if !right_edge {
                    cell_style.border_width.right *= 0.5;
                }
                if !top_edge {
                    cell_style.border_width.top *= 0.5;
                }
                if !bottom_edge {
                    cell_style.border_width.bottom *= 0.5;
                }
                Arc::new(cell_style)
            })
            .collect();

        let mut column_widths = vec![0.0f32; column_count];
        for (placement, cell_style) in placements.iter().zip(&effective) {
            let span = placement
                .colspan
                .min(column_count.saturating_sub(placement.column))
                .max(1);
            let padding = self.resolve_edges(cell_style.padding, cell_style, content_width);
            let border = cell_style.border_width;
            let noncontent = padding.left
                + padding.right
                + border.left
                + border.right
                + cell_style
                    .margin
                    .left
                    .resolve(
                        cell_style.font_size,
                        self.root_font_size,
                        self.viewport,
                        content_width,
                    )
                    .unwrap_or(0.0)
                + cell_style
                    .margin
                    .right
                    .resolve(
                        cell_style.font_size,
                        self.root_font_size,
                        self.viewport,
                        content_width,
                    )
                    .unwrap_or(0.0);
            let width = if cell_style.width.is_auto() {
                self.intrinsic_width(placement.cell, cell_style) + noncontent
            } else {
                let width = cell_style
                    .width
                    .resolve(
                        cell_style.font_size,
                        self.root_font_size,
                        self.viewport,
                        content_width,
                    )
                    .unwrap_or(0.0);
                if cell_style.box_sizing == crate::engine::css::BoxSizing::ContentBox {
                    width + noncontent
                } else {
                    width
                }
            }
            .max(0.0);
            if span <= 1 {
                column_widths[placement.column] = column_widths[placement.column].max(width);
            } else {
                let existing: f32 = column_widths[placement.column..placement.column + span]
                    .iter()
                    .sum::<f32>()
                    + spacing_x * (span - 1) as f32;
                if width > existing {
                    let extra = (width - existing) / span as f32;
                    for column_width in
                        &mut column_widths[placement.column..placement.column + span]
                    {
                        *column_width += extra;
                    }
                }
            }
        }

        let spacing_total = spacing_x * (column_count + 1) as f32;
        let widths_total: f32 = column_widths.iter().sum();
        let available = (content_width - spacing_total).max(0.0);
        if widths_total > 0.0 {
            let scale = if available > 0.0 {
                available / widths_total
            } else {
                0.0
            };
            for width in &mut column_widths {
                *width *= scale;
            }
        } else if available > 0.0 {
            let share = available / column_count as f32;
            column_widths.fill(share);
        }

        let mut column_xs = vec![0.0f32; column_count];
        let mut x = content_x + spacing_x;
        for (column, width) in column_widths.iter().enumerate() {
            column_xs[column] = x;
            x += width + spacing_x;
        }

        // Lay cells out once at a provisional y; row heights are resolved
        // afterwards and the boxes are shifted/stretched into place.
        let mut boxes: Vec<LayoutBox> = Vec::with_capacity(placements.len());
        for (placement, cell_style) in placements.iter().zip(&effective) {
            let span = placement
                .colspan
                .min(column_count.saturating_sub(placement.column))
                .max(1);
            let width = column_widths[placement.column..placement.column + span]
                .iter()
                .sum::<f32>()
                + spacing_x * (span - 1) as f32;
            boxes.push(self.layout_element(
                placement.cell,
                Arc::clone(cell_style),
                content_width,
                column_xs[placement.column],
                start_y,
                Some(width),
            ));
        }

        let mut row_heights = vec![0.0f32; rows.len()];
        for (index, placement) in placements.iter().enumerate() {
            if placement.rowspan <= 1 {
                row_heights[placement.row] =
                    row_heights[placement.row].max(boxes[index].margin_box_height());
            }
        }

        // A spanning cell's height covers all rows it occupies (plus the
        // spacing between them). Distribute any shortfall across those rows.
        for _ in 0..4 {
            let mut changed = false;
            for (index, placement) in placements.iter().enumerate() {
                if placement.rowspan <= 1 {
                    continue;
                }
                let end = (placement.row + placement.rowspan).min(rows.len());
                let span_rows = end.saturating_sub(placement.row);
                if span_rows == 0 {
                    continue;
                }
                let available: f32 = row_heights[placement.row..end].iter().sum::<f32>()
                    + spacing_y * (span_rows - 1) as f32;
                let required = boxes[index].margin_box_height();
                if required > available + 0.01 {
                    let extra = (required - available) / span_rows as f32;
                    for height in &mut row_heights[placement.row..end] {
                        *height += extra;
                    }
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut row_tops = vec![0.0f32; rows.len()];
        let mut y = start_y + spacing_y;
        for (row, height) in row_heights.iter().enumerate() {
            row_tops[row] = y;
            y += height + spacing_y;
        }
        let total_height = (y - start_y).max(0.0);

        for (index, placement) in placements.iter().enumerate() {
            let end = (placement.row + placement.rowspan).min(rows.len());
            let span_rows = end.saturating_sub(placement.row).max(1);
            let span_height: f32 = row_heights[placement.row..end].iter().sum::<f32>()
                + spacing_y * (span_rows - 1) as f32;
            let box_ = &mut boxes[index];
            let dy = row_tops[placement.row] - start_y;
            if dy.abs() > f32::EPSILON {
                shift_box(box_, 0.0, dy);
            }
            if box_.style.height.is_auto() {
                let height = (span_height - box_.margin.top - box_.margin.bottom).max(0.0);
                box_.content_height = (height
                    - box_.border.top
                    - box_.border.bottom
                    - box_.padding.top
                    - box_.padding.bottom)
                    .max(0.0);
                box_.rect.height = height;
            }
        }

        (boxes, total_height)
    }

    fn collect_table_captions(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
    ) -> Vec<(NodeId, Arc<ComputedStyle>)> {
        let children: Vec<NodeId> = self.doc.children(node).to_vec();
        let mut captions = Vec::new();
        for child in children {
            if !matches!(self.doc.node(child).data, NodeData::Element(_)) {
                continue;
            }
            if self.doc.tag_name(child) != Some("caption") {
                continue;
            }
            let child_style = self.style_for(child, style, self.root_font_size);
            if child_style.display == Display::None {
                continue;
            }
            captions.push((child, child_style));
        }
        captions
    }

    fn layout_caption_group(
        &mut self,
        captions: &[(NodeId, Arc<ComputedStyle>)],
        content_width: f32,
        content_x: f32,
        start_y: f32,
    ) -> (Vec<LayoutBox>, f32) {
        let mut boxes = Vec::with_capacity(captions.len());
        let mut y = start_y;
        for (caption, caption_style) in captions {
            let margin_top = self.resolve_margins(caption_style, content_width).top;
            let box_ = self.layout_element(
                *caption,
                Arc::clone(caption_style),
                content_width,
                content_x,
                y + margin_top,
                Some(content_width),
            );
            y += box_.margin_box_height();
            boxes.push(box_);
        }
        (boxes, (y - start_y).max(0.0))
    }

    fn caption_is_bottom(&self, caption: NodeId, table: NodeId) -> bool {
        self.styles
            .get(&caption)
            .or_else(|| self.styles.get(&table))
            .is_some_and(|style| style.caption_side == CaptionSide::Bottom)
    }

    fn table_border_collapse(&self, node: NodeId) -> bool {
        self.styles
            .get(&node)
            .is_some_and(|style| style.border_collapse == BorderCollapse::Collapse)
    }

    fn collect_table_rows(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
    ) -> Vec<Vec<(NodeId, Arc<ComputedStyle>)>> {
        let mut rows = Vec::new();
        let mut current: Vec<(NodeId, Arc<ComputedStyle>)> = Vec::new();
        self.collect_rows_into(node, style, &mut rows, &mut current);
        if !current.is_empty() {
            rows.push(current);
        }
        rows
    }

    fn collect_rows_into(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        rows: &mut Vec<Vec<(NodeId, Arc<ComputedStyle>)>>,
        current: &mut Vec<(NodeId, Arc<ComputedStyle>)>,
    ) {
        let children: Vec<NodeId> = self.doc.children(node).to_vec();
        for child in children {
            if !matches!(self.doc.node(child).data, NodeData::Element(_)) {
                continue;
            }
            let child_style = self.style_for(child, style, self.root_font_size);
            if child_style.display == Display::None {
                continue;
            }
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                self.positioned.push(child);
                continue;
            }
            match child_style.display {
                Display::TableRow => {
                    if !current.is_empty() {
                        rows.push(std::mem::take(current));
                    }
                    let cells = self.collect_cells(child, &child_style);
                    if !cells.is_empty() {
                        rows.push(cells);
                    }
                }
                Display::TableRowGroup => {
                    if !current.is_empty() {
                        rows.push(std::mem::take(current));
                    }
                    self.collect_rows_into(child, &child_style, rows, current);
                }
                Display::TableCell => current.push((child, child_style)),
                _ => {
                    if !current.is_empty() {
                        rows.push(std::mem::take(current));
                    }
                }
            }
        }
    }

    fn cell_colspan(&self, cell: NodeId) -> usize {
        self.doc
            .get_attribute(cell, "colspan")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, 64)
    }

    fn cell_rowspan(&self, cell: NodeId) -> usize {
        self.doc
            .get_attribute(cell, "rowspan")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, 64)
    }

    fn collect_cells(
        &mut self,
        row: NodeId,
        row_style: &Arc<ComputedStyle>,
    ) -> Vec<(NodeId, Arc<ComputedStyle>)> {
        let mut cells = Vec::new();
        let children: Vec<NodeId> = self.doc.children(row).to_vec();
        for child in children {
            if !matches!(self.doc.node(child).data, NodeData::Element(_)) {
                continue;
            }
            let child_style = self.style_for(child, row_style, self.root_font_size);
            if child_style.display == Display::TableCell {
                cells.push((child, child_style));
            }
        }
        cells
    }

    fn layout_flex_children(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        content_width: f32,
        content_x: f32,
        content_y: f32,
    ) -> (Vec<LayoutBox>, f32) {
        let children: Vec<NodeId> = self.doc.children(node).to_vec();
        let mut items: Vec<(usize, NodeId, Arc<ComputedStyle>)> = Vec::new();
        for child in children {
            match &self.doc.node(child).data {
                NodeData::Text(text) => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let anon = self.style_for_flex_anonymous(style);
                    items.push((items.len(), child, anon));
                }
                NodeData::Element(_) => {
                    let child_style = self.style_for(child, style, self.root_font_size);
                    if child_style.display == Display::None {
                        continue;
                    }
                    if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                        self.positioned.push(child);
                        continue;
                    }
                    items.push((items.len(), child, child_style));
                }
                _ => {}
            }
        }
        items.sort_by(|a, b| a.2.order.cmp(&b.2.order).then_with(|| a.0.cmp(&b.0)));

        let row = matches!(
            style.flex_direction,
            crate::engine::css::FlexDirection::Row | crate::engine::css::FlexDirection::RowReverse
        );
        let gap = style
            .column_gap
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                content_width,
            )
            .unwrap_or(0.0)
            .max(0.0);
        let row_gap = style
            .row_gap
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                content_width,
            )
            .unwrap_or(0.0)
            .max(0.0);

        if row {
            let mut base_widths: Vec<f32> = Vec::with_capacity(items.len());
            let mut boxes: Vec<LayoutBox> = Vec::with_capacity(items.len());
            for (_, child, child_style) in &items {
                if let NodeData::Text(_) = &self.doc.node(*child).data {
                    let mut anon_style = (**child_style).clone();
                    anon_style.display = Display::Block;
                    let (lines, atomics, height) = self.layout_inline_nodes(
                        &[*child],
                        child_style,
                        content_width,
                        content_x,
                        content_y,
                    );
                    let intrinsic = lines
                        .iter()
                        .map(|line| line.fragments.iter().map(|f| f.width).sum::<f32>())
                        .fold(0.0f32, f32::max);
                    let box_ = LayoutBox {
                        node: Some(*child),
                        kind: BoxKind::AnonymousBlock,
                        style: Arc::new(anon_style),
                        rect: Rect {
                            x: content_x,
                            y: content_y,
                            width: intrinsic.min(content_width),
                            height,
                        },
                        margin: Edges::<f32>::ZERO,
                        border: Edges::<f32>::ZERO,
                        padding: Edges::<f32>::ZERO,
                        children: Vec::new(),
                        lines,
                        atomics,
                        image: None,
                        text: None,
                        content_height: height,
                    };
                    base_widths.push(box_.rect.width);
                    boxes.push(box_);
                    continue;
                }
                if self.doc.tag_name(*child).is_some_and(is_replaced_tag) {
                    let box_ = self.replaced_box(*child, child_style, content_width);
                    base_widths.push(box_.rect.width);
                    boxes.push(box_);
                    continue;
                }
                let margins = self.resolve_margins(child_style, content_width);
                let padding = self.resolve_edges(child_style.padding, child_style, content_width);
                let border = child_style.border_width;
                let noncontent = margins.left
                    + margins.right
                    + padding.left
                    + padding.right
                    + border.left
                    + border.right;
                let base = match child_style.flex_basis {
                    Length::Auto => {
                        if child_style.width.is_auto() {
                            self.intrinsic_width(*child, child_style).min(content_width)
                        } else {
                            child_style
                                .width
                                .resolve(
                                    child_style.font_size,
                                    self.root_font_size,
                                    self.viewport,
                                    content_width,
                                )
                                .unwrap_or(0.0)
                        }
                    }
                    basis => basis
                        .resolve(
                            child_style.font_size,
                            self.root_font_size,
                            self.viewport,
                            content_width,
                        )
                        .unwrap_or(0.0),
                };
                base_widths.push((base + noncontent).max(0.0));
                boxes.push(self.layout_element(
                    *child,
                    Arc::clone(child_style),
                    content_width,
                    0.0,
                    0.0,
                    Some((base + noncontent).max(0.0)),
                ));
            }

            // Split items into flex lines when wrapping is enabled.
            let wrapping = style.flex_wrap == FlexWrap::Wrap;
            let mut line_indices: Vec<Vec<usize>> = Vec::new();
            let mut current_line: Vec<usize> = Vec::new();
            let mut current_width = 0.0f32;
            for (index, box_) in boxes.iter().enumerate() {
                let outer = box_.margin_box_width();
                let extra = if current_line.is_empty() {
                    outer
                } else {
                    gap + outer
                };
                if wrapping
                    && !current_line.is_empty()
                    && current_width + extra > content_width + 0.01
                {
                    line_indices.push(std::mem::take(&mut current_line));
                    current_width = 0.0;
                }
                if !current_line.is_empty() {
                    current_width += gap;
                }
                current_line.push(index);
                current_width += outer;
            }
            if !current_line.is_empty() {
                line_indices.push(current_line);
            }

            struct FlexLine {
                items: Vec<usize>,
                widths: Vec<f32>,
                cross: f32,
            }

            let mut flex_lines: Vec<FlexLine> = Vec::new();
            for line in &line_indices {
                let count = line.len();
                let mut widths: Vec<f32> = line.iter().map(|&index| base_widths[index]).collect();
                let total = widths.iter().sum::<f32>() + gap * count.saturating_sub(1) as f32;
                let free = content_width - total;
                if free > 0.0 {
                    let grow_total: f32 = line.iter().map(|&index| items[index].2.flex_grow).sum();
                    if grow_total > 0.0 {
                        for (slot, &index) in line.iter().enumerate() {
                            let grow = items[index].2.flex_grow;
                            if grow > 0.0 {
                                widths[slot] += free * grow / grow_total;
                            }
                        }
                    }
                } else if free < 0.0 {
                    let shrink_total: f32 = line
                        .iter()
                        .map(|&index| items[index].2.flex_shrink * base_widths[index])
                        .sum();
                    if shrink_total > 0.0 {
                        for (slot, &index) in line.iter().enumerate() {
                            let share =
                                items[index].2.flex_shrink * base_widths[index] / shrink_total;
                            widths[slot] = (widths[slot] + free * share).max(0.0);
                        }
                    }
                }

                for (slot, &index) in line.iter().enumerate() {
                    let (_, child, child_style) = &items[index];
                    if matches!(self.doc.node(*child).data, NodeData::Text(_)) {
                        continue;
                    }
                    if self.doc.tag_name(*child).is_some_and(is_replaced_tag) {
                        let mut box_ = self.replaced_box(*child, child_style, content_width);
                        if (box_.rect.width - widths[slot]).abs() > 0.5 {
                            box_.set_border_width(widths[slot]);
                        }
                        boxes[index] = box_;
                    } else {
                        boxes[index] = self.layout_element(
                            *child,
                            Arc::clone(child_style),
                            content_width,
                            0.0,
                            0.0,
                            Some(widths[slot].max(0.0)),
                        );
                    }
                }

                let cross = line
                    .iter()
                    .map(|&index| boxes[index].margin_box_height())
                    .fold(0.0f32, f32::max);
                flex_lines.push(FlexLine {
                    items: line.clone(),
                    widths,
                    cross,
                });
            }

            let base_height = flex_lines.iter().map(|line| line.cross).sum::<f32>()
                + row_gap * flex_lines.len().saturating_sub(1) as f32;
            let definite_height = match style.height {
                Length::Auto | Length::Percent(_) => None,
                length => length.resolve(style.font_size, self.root_font_size, self.viewport, 0.0),
            };
            let mut line_offset = 0.0f32;
            let mut extra_line_gap = 0.0f32;
            let mut line_stretch = 0.0f32;
            if let Some(container_height) = definite_height {
                let free = (container_height - base_height).max(0.0);
                match style.align_content {
                    AlignContent::FlexStart => {}
                    AlignContent::FlexEnd => line_offset = free,
                    AlignContent::Center => line_offset = free / 2.0,
                    AlignContent::SpaceBetween if flex_lines.len() > 1 => {
                        extra_line_gap = free / (flex_lines.len() - 1) as f32;
                    }
                    AlignContent::SpaceAround => {
                        let extra = free / flex_lines.len().max(1) as f32;
                        line_offset = extra / 2.0;
                        extra_line_gap = extra;
                    }
                    AlignContent::Stretch => {
                        line_stretch = free / flex_lines.len().max(1) as f32;
                    }
                    AlignContent::SpaceBetween => {}
                }
            }

            let mut y = content_y + line_offset;
            for line in &flex_lines {
                let count = line.items.len();
                let free_after = (content_width
                    - line.widths.iter().sum::<f32>()
                    - gap * count.saturating_sub(1) as f32)
                    .max(0.0);
                let (offset, extra_gap) = match style.justify_content {
                    JustifyContent::FlexStart => (0.0, 0.0),
                    JustifyContent::FlexEnd => (free_after, 0.0),
                    JustifyContent::Center => (free_after / 2.0, 0.0),
                    JustifyContent::SpaceBetween if count > 1 => {
                        (0.0, free_after / (count - 1) as f32)
                    }
                    JustifyContent::SpaceAround => {
                        let extra = free_after / count.max(1) as f32;
                        (extra / 2.0, extra)
                    }
                    JustifyContent::SpaceEvenly => {
                        let extra = free_after / (count + 1) as f32;
                        (extra, extra)
                    }
                    JustifyContent::SpaceBetween => (0.0, 0.0),
                };
                let mut start = content_x + offset;
                let line_cross = line.cross + line_stretch;

                for &index in &line.items {
                    let box_ = &mut boxes[index];
                    let margins = box_.margin;
                    let border_width = box_.rect.width;
                    let align = match box_.style.align_self {
                        AlignSelf::Auto => style.align_items,
                        AlignSelf::Stretch => AlignItems::Stretch,
                        AlignSelf::FlexStart => AlignItems::FlexStart,
                        AlignSelf::FlexEnd => AlignItems::FlexEnd,
                        AlignSelf::Center => AlignItems::Center,
                        AlignSelf::Baseline => AlignItems::FlexStart,
                    };
                    let outer_height = box_.margin_box_height();
                    let cross_offset = match align {
                        AlignItems::FlexStart | AlignItems::Baseline => 0.0,
                        AlignItems::FlexEnd => (line_cross - outer_height).max(0.0),
                        AlignItems::Center => ((line_cross - outer_height) / 2.0).max(0.0),
                        AlignItems::Stretch => {
                            if box_.style.height.is_auto() {
                                let new_height =
                                    (line_cross - margins.top - margins.bottom).max(0.0);
                                box_.content_height = new_height;
                                box_.rect.height = new_height
                                    + box_.border.top
                                    + box_.border.bottom
                                    + box_.padding.top
                                    + box_.padding.bottom;
                            }
                            0.0
                        }
                    };
                    let target_x = start + margins.left;
                    let target_y = y + margins.top + cross_offset;
                    shift_box(box_, target_x - box_.rect.x, target_y - box_.rect.y);
                    start += border_width + margins.right + margins.left + gap + extra_gap;
                }

                y += line_cross + row_gap + extra_line_gap;
            }

            if style.flex_direction == crate::engine::css::FlexDirection::RowReverse {
                for box_ in &mut boxes {
                    let margin_left = box_.rect.x - box_.margin.left;
                    let new_margin_left = content_x + content_width
                        - (margin_left - content_x)
                        - box_.margin_box_width();
                    let target_x = new_margin_left + box_.margin.left;
                    shift_box(box_, target_x - box_.rect.x, 0.0);
                }
            }
            let height = definite_height.unwrap_or(base_height);
            (boxes, height)
        } else {
            let definite_height = match style.height {
                Length::Auto => None,
                Length::Percent(percent) => self
                    .containing_height
                    .map(|height| (height * percent / 100.0).max(0.0)),
                length => length.resolve(
                    style.font_size,
                    self.root_font_size,
                    self.viewport,
                    self.containing_height.unwrap_or(0.0),
                ),
            };
            let mut boxes = Vec::with_capacity(items.len());
            let mut y = content_y;
            let mut max_item_width = 0.0f32;
            for (_, child, child_style) in &items {
                let margins = self.resolve_margins(child_style, content_width);
                let box_ = if matches!(self.doc.node(*child).data, NodeData::Text(_)) {
                    let (lines, atomics, height) = self.layout_inline_nodes(
                        &[*child],
                        child_style,
                        content_width,
                        content_x,
                        y,
                    );
                    LayoutBox {
                        node: Some(*child),
                        kind: BoxKind::AnonymousBlock,
                        style: Arc::clone(child_style),
                        rect: Rect {
                            x: content_x,
                            y,
                            width: content_width,
                            height,
                        },
                        margin: margins,
                        border: Edges::<f32>::ZERO,
                        padding: Edges::<f32>::ZERO,
                        children: Vec::new(),
                        lines,
                        atomics,
                        image: None,
                        text: None,
                        content_height: height,
                    }
                } else {
                    let mut forced = None;
                    if !child_style.width.is_auto() {
                        let width = child_style
                            .width
                            .resolve(
                                child_style.font_size,
                                self.root_font_size,
                                self.viewport,
                                content_width,
                            )
                            .unwrap_or(0.0);
                        forced = Some(width);
                    } else if matches!(child_style.align_self, AlignSelf::Stretch | AlignSelf::Auto)
                        && style.align_items == AlignItems::Stretch
                    {
                        forced = Some((content_width - margins.left - margins.right).max(0.0));
                    }
                    if self.doc.tag_name(*child).is_some_and(is_replaced_tag) {
                        let mut box_ = self.replaced_box(*child, child_style, content_width);
                        let stretch =
                            matches!(child_style.align_self, AlignSelf::Stretch | AlignSelf::Auto)
                                && style.align_items == AlignItems::Stretch;
                        if stretch && child_style.width.is_auto() {
                            box_.set_border_width(
                                (content_width - margins.left - margins.right).max(0.0),
                            );
                        }
                        box_
                    } else {
                        self.layout_element(
                            *child,
                            Arc::clone(child_style),
                            content_width,
                            content_x,
                            y,
                            forced,
                        )
                    }
                };
                max_item_width = max_item_width.max(box_.margin_box_width());
                y += box_.margin_box_height() + row_gap;
                boxes.push(box_);
            }
            let count = boxes.len();
            let base_height = if count > 0 {
                (y - content_y - row_gap).max(0.0)
            } else {
                0.0
            };
            let free = definite_height
                .map(|height| (height - base_height).max(0.0))
                .unwrap_or(0.0);
            let (main_offset, extra_gap) = match style.justify_content {
                JustifyContent::FlexStart => (0.0, 0.0),
                JustifyContent::FlexEnd => (free, 0.0),
                JustifyContent::Center => (free / 2.0, 0.0),
                JustifyContent::SpaceBetween if count > 1 => (0.0, free / (count - 1) as f32),
                JustifyContent::SpaceAround => {
                    let extra = free / count.max(1) as f32;
                    (extra / 2.0, extra)
                }
                JustifyContent::SpaceEvenly => {
                    let extra = free / (count + 1) as f32;
                    (extra, extra)
                }
                JustifyContent::SpaceBetween => (0.0, 0.0),
            };
            let mut delta = main_offset;
            for (index, box_) in boxes.iter_mut().enumerate() {
                if index > 0 {
                    delta += extra_gap;
                }
                if delta != 0.0 {
                    shift_box(box_, 0.0, delta);
                }
            }
            let height = definite_height.unwrap_or(base_height);
            let align = match style.align_items {
                AlignItems::Center => AlignItems::Center,
                AlignItems::FlexEnd => AlignItems::FlexEnd,
                _ => AlignItems::FlexStart,
            };
            for box_ in &mut boxes {
                let free = content_width - box_.margin_box_width();
                let offset = match align {
                    AlignItems::Center => (free / 2.0).max(0.0),
                    AlignItems::FlexEnd => free.max(0.0),
                    _ => 0.0,
                };
                if offset != 0.0 {
                    shift_box(box_, offset, 0.0);
                }
            }
            if style.flex_direction == crate::engine::css::FlexDirection::ColumnReverse {
                for box_ in &mut boxes {
                    let margin_top = box_.rect.y - box_.margin.top;
                    let new_margin_top =
                        content_y + height - (margin_top - content_y) - box_.margin_box_height();
                    let target_y = new_margin_top + box_.margin.top;
                    shift_box(box_, 0.0, target_y - box_.rect.y);
                }
            }
            let _ = max_item_width;
            (boxes, height)
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn layout_grid_children(
        &mut self,
        node: NodeId,
        style: &Arc<ComputedStyle>,
        content_width: f32,
        content_x: f32,
        content_y: f32,
    ) -> (Vec<LayoutBox>, f32) {
        struct GridItem {
            node: NodeId,
            style: Arc<ComputedStyle>,
            order: usize,
            row: Option<usize>,
            col: Option<usize>,
            row_span: usize,
            col_span: usize,
            placed_row: usize,
            placed_col: usize,
        }

        let mut items: Vec<GridItem> = Vec::new();
        for (order, child) in self.doc.children(node).to_vec().into_iter().enumerate() {
            let NodeData::Element(_) = &self.doc.node(child).data else {
                continue;
            };
            let child_style = self.style_for(child, style, self.root_font_size);
            if child_style.display == Display::None {
                continue;
            }
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                self.positioned.push(child);
                continue;
            }
            let explicit_cols = style.grid_template_columns.len().max(1);
            let explicit_rows = style.grid_template_rows.len().max(1);
            let (col, col_span) = grid_axis(
                child_style.grid_column_start,
                child_style.grid_column_end,
                explicit_cols,
            );
            let (row, row_span) = grid_axis(
                child_style.grid_row_start,
                child_style.grid_row_end,
                explicit_rows,
            );
            items.push(GridItem {
                node: child,
                style: child_style,
                order,
                row,
                col,
                row_span,
                col_span,
                placed_row: 0,
                placed_col: 0,
            });
        }
        items.sort_by(|a, b| {
            a.style
                .order
                .cmp(&b.style.order)
                .then(a.order.cmp(&b.order))
        });

        let mut columns = style.grid_template_columns.len().max(1);
        let mut rows = style.grid_template_rows.len().max(1);
        for item in &items {
            if let Some(col) = item.col {
                columns = columns.max(col + item.col_span);
            }
            if let Some(row) = item.row {
                rows = rows.max(row + item.row_span);
            }
        }

        let mut occupied: Vec<Vec<bool>> = Vec::new();
        let fits = |occupied: &Vec<Vec<bool>>, row: usize, col: usize, rs: usize, cs: usize| {
            for r in row..row + rs {
                let Some(line) = occupied.get(r) else {
                    continue;
                };
                for c in col..col + cs {
                    if line.get(c).copied().unwrap_or(false) {
                        return false;
                    }
                }
            }
            true
        };
        let mark = |occupied: &mut Vec<Vec<bool>>, row: usize, col: usize, rs: usize, cs: usize| {
            while occupied.len() < row + rs {
                occupied.push(Vec::new());
            }
            for line in occupied.iter_mut().take(row + rs).skip(row) {
                if line.len() < col + cs {
                    line.resize(col + cs, false);
                }
                for cell in line.iter_mut().take(col + cs).skip(col) {
                    *cell = true;
                }
            }
        };

        // Place items with explicit row and column first.
        for item in items.iter_mut() {
            if let (Some(row), Some(col)) = (item.row, item.col) {
                mark(&mut occupied, row, col, item.row_span, item.col_span);
                item.placed_row = row;
                item.placed_col = col;
            }
        }
        // Auto-place the rest in row-major order.
        for item in items.iter_mut() {
            if item.row.is_some() && item.col.is_some() {
                continue;
            }
            if let (None, Some(col)) = (item.row, item.col) {
                let mut row = 0usize;
                while !fits(&occupied, row, col, item.row_span, item.col_span) {
                    row += 1;
                }
                item.placed_row = row;
                item.placed_col = col;
                mark(&mut occupied, row, col, item.row_span, item.col_span);
                rows = rows.max(row + item.row_span);
                continue;
            }
            if let (Some(row), None) = (item.row, item.col) {
                let mut col = 0usize;
                while !fits(&occupied, row, col, item.row_span, item.col_span) {
                    col += 1;
                }
                item.placed_row = row;
                item.placed_col = col;
                mark(&mut occupied, row, col, item.row_span, item.col_span);
                columns = columns.max(col + item.col_span);
                continue;
            }
            let mut row = 0usize;
            let (found_row, found_col) = 'search: loop {
                let mut col = 0usize;
                loop {
                    if fits(&occupied, row, col, item.row_span, item.col_span) {
                        if col < columns {
                            break 'search (row, col);
                        }
                        break;
                    }
                    col += 1;
                }
                row += 1;
            };
            item.placed_row = found_row;
            item.placed_col = found_col;
            mark(
                &mut occupied,
                found_row,
                found_col,
                item.row_span,
                item.col_span,
            );
            rows = rows.max(found_row + item.row_span);
            columns = columns.max(found_col + item.col_span);
        }

        rows = rows.max(
            items
                .iter()
                .map(|item| item.placed_row + item.row_span)
                .max()
                .unwrap_or(1),
        );
        columns = columns.max(
            items
                .iter()
                .map(|item| item.placed_col + item.col_span)
                .max()
                .unwrap_or(1),
        );

        let column_gap = style
            .column_gap
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                content_width,
            )
            .unwrap_or(0.0)
            .max(0.0);
        let row_gap = style
            .row_gap
            .resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                content_width,
            )
            .unwrap_or(0.0)
            .max(0.0);

        let mut column_widths = vec![0.0f32; columns];
        let mut auto_columns = Vec::new();
        let mut fr_total = 0.0f32;
        for column in 0..columns {
            let track = style
                .grid_template_columns
                .get(column)
                .cloned()
                .unwrap_or_else(|| style.grid_auto_columns.clone());
            match track {
                GridTrack::Length(length) => {
                    column_widths[column] = length
                        .resolve(
                            style.font_size,
                            self.root_font_size,
                            self.viewport,
                            content_width,
                        )
                        .unwrap_or(0.0)
                        .max(0.0);
                }
                GridTrack::Fr(fr) => fr_total += fr.max(0.0),
                GridTrack::Auto | GridTrack::MinContent | GridTrack::MaxContent => {
                    auto_columns.push(column);
                }
            }
        }

        for item in &items {
            let intrinsic = if item.style.width.is_auto() {
                self.intrinsic_width(item.node, &item.style)
            } else {
                item.style
                    .width
                    .resolve(
                        item.style.font_size,
                        self.root_font_size,
                        self.viewport,
                        content_width,
                    )
                    .unwrap_or(0.0)
            };
            let span = item.col_span.max(1);
            let share =
                ((intrinsic - column_gap * span.saturating_sub(1) as f32).max(0.0)) / span as f32;
            for column in item.placed_col..item.placed_col + item.col_span {
                if auto_columns.contains(&column) {
                    column_widths[column] = column_widths[column].max(share);
                }
            }
        }

        let gaps = column_gap * columns.saturating_sub(1) as f32;
        let used: f32 = column_widths.iter().sum::<f32>() + gaps;
        let remaining = (content_width - used).max(0.0);
        if fr_total > 0.0 && remaining > 0.0 {
            for column in 0..columns {
                let track = style
                    .grid_template_columns
                    .get(column)
                    .cloned()
                    .unwrap_or_else(|| style.grid_auto_columns.clone());
                if let GridTrack::Fr(fr) = track {
                    column_widths[column] += remaining * fr.max(0.0) / fr_total;
                }
            }
        } else if !auto_columns.is_empty() && remaining > 0.0 {
            let share = remaining / auto_columns.len() as f32;
            for column in auto_columns {
                column_widths[column] += share;
            }
        }

        let mut row_heights = vec![0.0f32; rows];
        let mut auto_rows = Vec::new();
        let mut fr_rows = Vec::new();
        let mut fr_row_total = 0.0f32;
        for row in 0..rows {
            let track = style
                .grid_template_rows
                .get(row)
                .cloned()
                .unwrap_or_else(|| style.grid_auto_rows.clone());
            match track {
                GridTrack::Length(length) => {
                    row_heights[row] = length
                        .resolve(
                            style.font_size,
                            self.root_font_size,
                            self.viewport,
                            self.containing_height.unwrap_or(0.0),
                        )
                        .unwrap_or(0.0)
                        .max(0.0);
                }
                GridTrack::Fr(fr) => {
                    fr_rows.push(row);
                    fr_row_total += fr.max(0.0);
                }
                _ => auto_rows.push(row),
            }
        }

        let mut boxes: Vec<Option<LayoutBox>> = (0..items.len()).map(|_| None).collect();
        for (index, item) in items.iter().enumerate() {
            let cell_width = column_widths
                .iter()
                .skip(item.placed_col)
                .take(item.col_span)
                .sum::<f32>()
                + column_gap * item.col_span.saturating_sub(1) as f32;
            let margins = self.resolve_margins(&item.style, cell_width);
            let forced = if item.style.width.is_auto() {
                Some((cell_width - margins.left - margins.right).max(0.0))
            } else {
                None
            };
            let box_ = self.layout_element(
                item.node,
                Arc::clone(&item.style),
                cell_width,
                0.0,
                0.0,
                forced,
            );
            let height = box_.margin_box_height();
            for row in item.placed_row..item.placed_row + item.row_span {
                if row < rows {
                    let share = height / item.row_span.max(1) as f32;
                    if !fr_rows.contains(&row) {
                        row_heights[row] = row_heights[row].max(share);
                    }
                }
            }
            boxes[index] = Some(box_);
        }

        let definite_height = self.containing_height.filter(|height| *height > 0.0);
        let fixed_rows: f32 =
            row_heights.iter().sum::<f32>() + row_gap * rows.saturating_sub(1) as f32;
        if let Some(container_height) = definite_height {
            let free = (container_height - fixed_rows).max(0.0);
            if fr_row_total > 0.0 && free > 0.0 {
                for row in fr_rows {
                    row_heights[row] += free / fr_row_total;
                }
            } else if !auto_rows.is_empty() && free > 0.0 {
                let share = free / auto_rows.len() as f32;
                for row in auto_rows {
                    row_heights[row] += share;
                }
            }
        }

        let mut y = content_y;
        let mut row_offsets = Vec::with_capacity(rows);
        for height in &row_heights {
            row_offsets.push(y);
            y += height + row_gap;
        }
        let mut x_offsets = Vec::with_capacity(columns);
        let mut x = content_x;
        for width in &column_widths {
            x_offsets.push(x);
            x += width + column_gap;
        }

        let mut children = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let Some(mut box_) = boxes[index].take() else {
                continue;
            };
            let cell_x = x_offsets.get(item.placed_col).copied().unwrap_or(content_x);
            let cell_y = row_offsets
                .get(item.placed_row)
                .copied()
                .unwrap_or(content_y);
            let cell_width = column_widths
                .iter()
                .skip(item.placed_col)
                .take(item.col_span)
                .sum::<f32>()
                + column_gap * item.col_span.saturating_sub(1) as f32;
            let cell_height = row_heights
                .iter()
                .skip(item.placed_row)
                .take(item.row_span)
                .sum::<f32>()
                + row_gap * item.row_span.saturating_sub(1) as f32;
            if style.align_items == AlignItems::Stretch && item.style.height.is_auto() {
                let stretched = (cell_height - box_.margin.top - box_.margin.bottom).max(0.0);
                box_.content_height = stretched;
                box_.rect.height = stretched
                    + box_.border.top
                    + box_.border.bottom
                    + box_.padding.top
                    + box_.padding.bottom;
            }
            let outer_height = box_.margin_box_height();
            let offset = match style.align_items {
                AlignItems::Center => ((cell_height - outer_height) / 2.0).max(0.0),
                AlignItems::FlexEnd => (cell_height - outer_height).max(0.0),
                _ => 0.0,
            };
            let target_x = cell_x + box_.margin.left;
            let target_y = cell_y + box_.margin.top + offset;
            let (old_x, old_y) = (box_.rect.x, box_.rect.y);
            shift_box(&mut box_, target_x - old_x, target_y - old_y);
            let _ = cell_width;
            children.push(box_);
        }

        let content_height = definite_height.unwrap_or_else(|| {
            (row_heights.iter().sum::<f32>() + row_gap * rows.saturating_sub(1) as f32).max(0.0)
        });
        (children, content_height)
    }

    fn style_for_flex_anonymous(&self, style: &Arc<ComputedStyle>) -> Arc<ComputedStyle> {
        let mut anon = (**style).clone();
        anon.display = Display::Block;
        anon.margin = Edges::<Length>::ZERO;
        anon.padding = Edges::<Length>::ZERO;
        anon.border_width = Edges::<f32>::ZERO;
        anon.flex_grow = 0.0;
        anon.flex_shrink = 1.0;
        anon.flex_basis = Length::Auto;
        Arc::new(anon)
    }

    fn layout_positioned(&mut self, root: &mut LayoutBox, node_rects: &mut HashMap<NodeId, Rect>) {
        let mut index = 0;
        while index < self.positioned.len() {
            let node = self.positioned[index];
            index += 1;
            if !self.placed_positioned.insert(node) {
                continue;
            }
            let Some(style) = self.styles.get(&node).cloned() else {
                continue;
            };
            if !matches!(style.position, Position::Absolute | Position::Fixed) {
                continue;
            }

            let (cb, cb_is_padding) = if style.position == Position::Fixed {
                (
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        width: self.viewport.width,
                        height: self.viewport.height,
                    },
                    false,
                )
            } else {
                let mut containing = Rect {
                    x: 0.0,
                    y: 0.0,
                    width: self.viewport.width,
                    height: self.viewport.height,
                };
                for ancestor in self.doc.ancestors(node) {
                    if let Some(ancestor_style) = self.styles.get(&ancestor)
                        && ancestor_style.position != Position::Static
                        && let Some(rect) = node_rects.get(&ancestor)
                    {
                        containing = *rect;
                        break;
                    }
                }
                (containing, true)
            };

            let base = if cb_is_padding {
                // Approximate the padding box by using the border box.
                cb
            } else {
                cb
            };

            let margin = self.resolve_margins(&style, base.width);
            let padding = self.resolve_edges(style.padding, &style, base.width);
            let border = style.border_width;
            let noncontent = padding.left + padding.right + border.left + border.right;
            let left = style.left.resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                base.width,
            );
            let right = style.right.resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                base.width,
            );
            let top = style.top.resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                base.height,
            );
            let bottom = style.bottom.resolve(
                style.font_size,
                self.root_font_size,
                self.viewport,
                base.height,
            );

            let forced_width = if style.width.is_auto() {
                if let (Some(left), Some(right)) = (left, right) {
                    Some((base.width - left - right - margin.left - margin.right).max(noncontent))
                } else {
                    let intrinsic = self.intrinsic_width(node, &style) + noncontent;
                    let available = (base.width - left.unwrap_or(0.0) - margin.left - margin.right)
                        .max(noncontent);
                    Some(intrinsic.min(available).max(noncontent))
                }
            } else {
                None
            };

            let x = base.x + left.unwrap_or(0.0) + margin.left;
            let y = base.y + top.unwrap_or(0.0) + margin.top;

            let mut box_ =
                self.layout_element(node, Arc::clone(&style), base.width, x, y, forced_width);

            if let Some(bottom) = bottom
                && top.is_none()
            {
                box_.rect.y = base.y + base.height - bottom - box_.rect.height - margin.bottom;
            }
            if let Some(right) = right
                && left.is_none()
            {
                box_.rect.x = base.x + base.width - right - box_.rect.width - margin.right;
            }

            collect_node_rects(&box_, node_rects);
            let parent_node = self.doc.parent(node);
            let inserted = parent_node
                .map(|parent| attach_box(root, parent, box_.clone()))
                .unwrap_or(false);
            if !inserted {
                root.children.push(box_);
            }
        }
    }
}

fn resolve_block_height(
    length: Length,
    style: &ComputedStyle,
    containing_height: Option<f32>,
    root_font_size: f32,
    viewport: Viewport,
) -> Option<f32> {
    let value = match length {
        Length::Auto => return None,
        Length::Percent(percent) => containing_height.map(|height| height * percent / 100.0)?,
        other => other.resolve(style.font_size, root_font_size, viewport, 0.0)?,
    };
    if style.box_sizing == crate::engine::css::BoxSizing::BorderBox {
        let noncontent = style.border_width.top
            + style.border_width.bottom
            + style
                .padding
                .top
                .resolve(style.font_size, root_font_size, viewport, 0.0)
                .unwrap_or(0.0)
            + style
                .padding
                .bottom
                .resolve(style.font_size, root_font_size, viewport, 0.0)
                .unwrap_or(0.0);
        Some((value - noncontent).max(0.0))
    } else {
        Some(value)
    }
}

fn grid_axis(start: GridPlacement, end: GridPlacement, explicit: usize) -> (Option<usize>, usize) {
    let line_index = |line: i32| -> usize {
        if line > 0 {
            (line - 1) as usize
        } else {
            (explicit as i32 + line + 1).max(0) as usize
        }
    };
    let start_index = match start {
        GridPlacement::Line(line) => Some(line_index(line)),
        _ => None,
    };
    let span = match end {
        GridPlacement::Span(span) => span.max(1) as usize,
        GridPlacement::Line(line) => start_index
            .map(|start| line_index(line).saturating_sub(start).max(1))
            .unwrap_or(1),
        GridPlacement::Auto => match start {
            GridPlacement::Span(span) => span.max(1) as usize,
            _ => 1,
        },
    };
    (start_index, span)
}

fn is_block_level(display: Display) -> bool {
    matches!(
        display,
        Display::Block | Display::Flex | Display::Grid | Display::ListItem
    )
}

fn is_replaced_tag(name: &str) -> bool {
    matches!(
        name,
        "img"
            | "input"
            | "select"
            | "textarea"
            | "video"
            | "canvas"
            | "iframe"
            | "svg"
            | "object"
            | "embed"
            | "audio"
    )
}

fn collapse_margins(a: f32, b: f32) -> f32 {
    if a >= 0.0 && b >= 0.0 {
        a.max(b)
    } else {
        a + b
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x2E80..=0x2EFF | 0x3040..=0x30FF | 0x3400..=0x4DBF |
        0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

fn push_text_tokens(
    text: &str,
    style: &Arc<ComputedStyle>,
    node: NodeId,
    tokens: &mut Vec<InlineToken>,
) {
    match style.white_space {
        WhiteSpace::Normal | WhiteSpace::Nowrap => {
            let wrap = style.white_space == WhiteSpace::Normal;
            let mut word = String::new();
            let mut in_whitespace = false;
            for ch in text.chars() {
                if ch.is_whitespace() {
                    if !word.is_empty() {
                        tokens.push(InlineToken::Word {
                            text: std::mem::take(&mut word),
                            style: Arc::clone(style),
                            node,
                            wrap,
                        });
                    }
                    if !in_whitespace {
                        tokens.push(InlineToken::Space {
                            style: Arc::clone(style),
                        });
                        in_whitespace = true;
                    }
                } else {
                    in_whitespace = false;
                    if is_cjk(ch) {
                        if !word.is_empty() {
                            tokens.push(InlineToken::Word {
                                text: std::mem::take(&mut word),
                                style: Arc::clone(style),
                                node,
                                wrap,
                            });
                        }
                        tokens.push(InlineToken::Word {
                            text: ch.to_string(),
                            style: Arc::clone(style),
                            node,
                            wrap: true,
                        });
                    } else {
                        word.push(ch);
                    }
                }
            }
            if !word.is_empty() {
                tokens.push(InlineToken::Word {
                    text: word,
                    style: Arc::clone(style),
                    node,
                    wrap,
                });
            }
        }
        WhiteSpace::Pre => {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    tokens.push(InlineToken::Break {
                        style: Arc::clone(style),
                    });
                }
                if !line.is_empty() {
                    tokens.push(InlineToken::Word {
                        text: line.to_owned(),
                        style: Arc::clone(style),
                        node,
                        wrap: false,
                    });
                }
            }
        }
        WhiteSpace::PreWrap => {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    tokens.push(InlineToken::Break {
                        style: Arc::clone(style),
                    });
                }
                let mut word = String::new();
                for ch in line.chars() {
                    if ch.is_whitespace() || is_cjk(ch) {
                        if !word.is_empty() {
                            tokens.push(InlineToken::Word {
                                text: std::mem::take(&mut word),
                                style: Arc::clone(style),
                                node,
                                wrap: true,
                            });
                        }
                        tokens.push(InlineToken::Word {
                            text: ch.to_string(),
                            style: Arc::clone(style),
                            node,
                            wrap: true,
                        });
                    } else {
                        word.push(ch);
                    }
                }
                if !word.is_empty() {
                    tokens.push(InlineToken::Word {
                        text: word,
                        style: Arc::clone(style),
                        node,
                        wrap: true,
                    });
                }
            }
        }
    }
}

fn apply_relative_offsets(root: &mut LayoutBox, viewport: Viewport) {
    apply_relative_recursive(root, None, viewport);
}

fn apply_relative_recursive(root: &mut LayoutBox, offset: Option<(f32, f32)>, viewport: Viewport) {
    if let Some((dx, dy)) = offset {
        shift_box(root, dx, dy);
    }
    let own_offset = if matches!(root.style.position, Position::Relative | Position::Sticky) {
        let dx = if !root.style.left.is_auto() {
            root.style
                .left
                .resolve(
                    root.style.font_size,
                    root.style.font_size,
                    viewport,
                    root.rect.width,
                )
                .unwrap_or(0.0)
        } else if !root.style.right.is_auto() {
            -root
                .style
                .right
                .resolve(
                    root.style.font_size,
                    root.style.font_size,
                    viewport,
                    root.rect.width,
                )
                .unwrap_or(0.0)
        } else {
            0.0
        };
        let dy = if !root.style.top.is_auto() {
            root.style
                .top
                .resolve(
                    root.style.font_size,
                    root.style.font_size,
                    viewport,
                    root.rect.height,
                )
                .unwrap_or(0.0)
        } else if !root.style.bottom.is_auto() {
            -root
                .style
                .bottom
                .resolve(
                    root.style.font_size,
                    root.style.font_size,
                    viewport,
                    root.rect.height,
                )
                .unwrap_or(0.0)
        } else {
            0.0
        };
        if dx != 0.0 || dy != 0.0 {
            Some((dx, dy))
        } else {
            None
        }
    } else {
        None
    };
    if let Some((dx, dy)) = own_offset {
        shift_box(root, dx, dy);
    }
    let children = &mut root.children;
    for child in children {
        apply_relative_recursive(child, None, viewport);
    }
    let atomics = &mut root.atomics;
    for atomic in atomics {
        apply_relative_recursive(atomic, None, viewport);
    }
}

/// A cell placed into the table grid, including how many rows/columns it
/// occupies starting at (`row`, `column`).
#[derive(Clone, Debug)]
struct TableCellPlacement {
    cell: NodeId,
    style: Arc<ComputedStyle>,
    row: usize,
    column: usize,
    colspan: usize,
    rowspan: usize,
}

fn shift_box(box_: &mut LayoutBox, dx: f32, dy: f32) {
    box_.rect.x += dx;
    box_.rect.y += dy;
    for line in &mut box_.lines {
        line.y += dy;
        line.baseline += dy;
        for fragment in &mut line.fragments {
            fragment.x += dx;
        }
    }
    for child in &mut box_.children {
        shift_box(child, dx, dy);
    }
    for atomic in &mut box_.atomics {
        shift_box(atomic, dx, dy);
    }
}

fn collect_node_rects(box_: &LayoutBox, out: &mut HashMap<NodeId, Rect>) {
    if let Some(node) = box_.node {
        out.entry(node).or_insert(box_.rect);
    }
    for child in &box_.children {
        collect_node_rects(child, out);
    }
    for atomic in &box_.atomics {
        collect_node_rects(atomic, out);
    }
}

fn attach_box(root: &mut LayoutBox, parent_node: NodeId, child: LayoutBox) -> bool {
    if root.node == Some(parent_node) {
        root.children.push(child);
        return true;
    }
    for existing in &mut root.children {
        if attach_box(existing, parent_node, child.clone()) {
            return true;
        }
    }
    for atomic in &mut root.atomics {
        if attach_box(atomic, parent_node, child.clone()) {
            return true;
        }
    }
    false
}

impl LayoutBox {
    pub fn find_node(&self, node: NodeId) -> Option<&LayoutBox> {
        if self.node == Some(node) {
            return Some(self);
        }
        for child in &self.children {
            if let Some(found) = child.find_node(node) {
                return Some(found);
            }
        }
        for atomic in &self.atomics {
            if let Some(found) = atomic.find_node(node) {
                return Some(found);
            }
        }
        None
    }

    pub fn clips_overflow(&self) -> bool {
        matches!(
            self.style.overflow,
            Overflow::Hidden | Overflow::Scroll | Overflow::Auto
        )
    }
}
