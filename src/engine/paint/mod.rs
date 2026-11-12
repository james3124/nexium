//! Display-list-free painter that rasterizes the layout tree into an RGBA
//! canvas using fontdue for glyphs and the `image` crate for decoded images.

use std::collections::HashMap;
use std::sync::Arc;

use image::{Rgba, RgbaImage, imageops};
use url::Url;

use crate::engine::css::{
    BackgroundRepeat, BackgroundSizeMode, BorderStyle, Color, ComputedStyle, FontStyle, Overflow,
    Position, TextDecoration, Viewport, Visibility,
};
use crate::engine::dom::{Document, NodeData, NodeId};
use crate::engine::layout::{FragmentKind, LayoutBox, LayoutResult, LineBox, Rect};
use crate::engine::text::FontSet;

pub struct Painter<'a> {
    canvas: RgbaImage,
    fonts: &'a FontSet,
    images: &'a HashMap<String, Arc<RgbaImage>>,
    canvases: &'a HashMap<NodeId, RgbaImage>,
    document: &'a Document,
    base_url: Option<&'a Url>,
    viewport: Viewport,
    clip: Vec<Rect>,
}

#[allow(clippy::too_many_arguments)]
pub fn paint_document(
    layout: &LayoutResult,
    fonts: &FontSet,
    images: &HashMap<String, Arc<RgbaImage>>,
    canvases: &HashMap<NodeId, RgbaImage>,
    document: &Document,
    base_url: Option<&Url>,
    width: u32,
    height: u32,
) -> RgbaImage {
    let viewport = layout.viewport;
    let mut painter = Painter {
        canvas: RgbaImage::from_pixel(width.max(1), height.max(1), Rgba([255, 255, 255, 255])),
        fonts,
        images,
        canvases,
        document,
        base_url,
        viewport,
        clip: Vec::new(),
    };

    painter.paint_page_background(&layout.root, viewport.width);
    painter.paint_box(&layout.root, 1.0);
    painter.canvas
}

impl<'a> Painter<'a> {
    fn paint_page_background(&mut self, root: &LayoutBox, width: f32) {
        let mut background = Color::WHITE;
        if root.style.background_color.a > 0 {
            background = root.style.background_color;
        }
        // CSS background propagation: the body background is used for the
        // canvas when the root background is transparent.
        if root.style.background_color.a == 0
            && let Some(body) = root
                .children
                .iter()
                .find(|child| child.style.display == crate::engine::css::Display::Block)
            && body.style.background_color.a > 0
        {
            background = body.style.background_color;
        }
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: width.max(self.canvas.width() as f32),
            height: self.canvas.height() as f32,
        };
        self.fill_rect(rect, background, 1.0);
    }

    fn paint_box(&mut self, box_: &LayoutBox, opacity: f32) {
        if box_.style.visibility == Visibility::Hidden {
            return;
        }
        let opacity = opacity * box_.style.opacity;
        if opacity <= 0.001 {
            return;
        }

        let clipped = box_.style.overflow == Overflow::Hidden
            || box_.style.overflow == Overflow::Scroll
            || box_.style.overflow == Overflow::Auto;
        if clipped {
            let clip = self.effective_clip().unwrap_or(Rect {
                x: 0.0,
                y: 0.0,
                width: self.canvas.width() as f32,
                height: self.canvas.height() as f32,
            });
            self.clip.push(clip.intersection(&box_.padding_box()));
        }

        let radius = resolve_radius(box_, self.viewport);
        if radius > 0.5 {
            if box_.style.background_color.a > 0 {
                self.fill_rounded_rect(box_.rect, radius, box_.style.background_color, opacity);
            }
            self.paint_background_image(box_, opacity);
            self.paint_rounded_borders(box_, radius, opacity);
        } else {
            if box_.style.background_color.a > 0 {
                self.fill_rect(box_.rect, box_.style.background_color, opacity);
            }
            self.paint_background_image(box_, opacity);
            self.paint_borders(box_, opacity);
        }

        if box_.kind == crate::engine::layout::BoxKind::Replaced {
            let tag = box_
                .node
                .and_then(|node| self.document.tag_name(node))
                .unwrap_or("");
            if tag == "svg" {
                if let Some(node) = box_.node {
                    self.paint_svg(node, box_, opacity);
                }
            } else if tag == "canvas" {
                if let Some(node) = box_.node
                    && let Some(image) = self.canvases.get(&node)
                {
                    let dest = Rect {
                        x: box_.content_x(),
                        y: box_.content_y(),
                        width: box_.content_width(),
                        height: box_.content_height,
                    };
                    self.draw_image(image, dest, opacity);
                }
            } else if let Some(image_url) = &box_.image
                && let Some(image) = self.lookup_image(image_url)
            {
                let dest = Rect {
                    x: box_.content_x(),
                    y: box_.content_y(),
                    width: box_.content_width(),
                    height: box_.content_height,
                };
                self.draw_image(&image, dest, opacity);
            } else if let Some(text) = &box_.text
                && !text.is_empty()
            {
                self.paint_replaced_text(box_, text, opacity);
            }
        }

        for line in &box_.lines {
            self.paint_line(line, &box_.atomics, opacity);
        }

        let mut children: Vec<&LayoutBox> = box_.children.iter().collect();
        children.sort_by_key(|child| {
            let positioned = matches!(
                child.style.position,
                Position::Relative | Position::Absolute | Position::Fixed | Position::Sticky
            );
            let group = if !positioned {
                0
            } else if child.style.z_index < 0 {
                -1
            } else {
                1
            };
            (group, child.style.z_index)
        });
        for child in children {
            self.paint_box(child, opacity);
        }

        if clipped {
            self.clip.pop();
        }
    }

    fn paint_line(&mut self, line: &LineBox, atomics: &[LayoutBox], opacity: f32) {
        for fragment in &line.fragments {
            match &fragment.kind {
                FragmentKind::Space => {}
                FragmentKind::Atomic(index) => {
                    if let Some(atomic) = atomics.get(*index) {
                        self.paint_box(atomic, opacity);
                    }
                }
                FragmentKind::Text(text) => {
                    let style = &fragment.style;
                    if style.background_color.a > 0 {
                        self.fill_rect(
                            Rect {
                                x: fragment.x,
                                y: line.y,
                                width: fragment.width,
                                height: line.height,
                            },
                            style.background_color,
                            opacity,
                        );
                    }
                    self.draw_text(text, fragment.x, line.baseline, style, opacity);
                    match style.text_decoration {
                        TextDecoration::Underline => {
                            self.fill_rect(
                                Rect {
                                    x: fragment.x,
                                    y: line.baseline + 2.0,
                                    width: fragment.width,
                                    height: (style.font_size * 0.06).max(1.0),
                                },
                                style.color,
                                opacity,
                            );
                        }
                        TextDecoration::LineThrough => {
                            self.fill_rect(
                                Rect {
                                    x: fragment.x,
                                    y: line.baseline - style.font_size * 0.32,
                                    width: fragment.width,
                                    height: (style.font_size * 0.06).max(1.0),
                                },
                                style.color,
                                opacity,
                            );
                        }
                        TextDecoration::None => {}
                    }
                }
            }
        }
    }

    fn paint_background_image(&mut self, box_: &LayoutBox, opacity: f32) {
        let Some(url) = &box_.style.background_image else {
            return;
        };
        let Some(image) = self.lookup_image(url) else {
            return;
        };
        let origin = box_.padding_box();
        if origin.width <= 0.0 || origin.height <= 0.0 {
            return;
        }
        let intrinsic_width = image.width() as f32;
        let intrinsic_height = image.height() as f32;
        if intrinsic_width <= 0.0 || intrinsic_height <= 0.0 {
            return;
        }
        let style = &box_.style;
        let size = style.background_size;
        let (tile_width, tile_height) = match size.mode {
            BackgroundSizeMode::Cover | BackgroundSizeMode::Contain => {
                let scale_x = origin.width / intrinsic_width;
                let scale_y = origin.height / intrinsic_height;
                let scale = if size.mode == BackgroundSizeMode::Cover {
                    scale_x.max(scale_y)
                } else {
                    scale_x.min(scale_y)
                };
                (intrinsic_width * scale, intrinsic_height * scale)
            }
            BackgroundSizeMode::Auto => (intrinsic_width, intrinsic_height),
            BackgroundSizeMode::Explicit => {
                let width = if size.width.is_auto() {
                    None
                } else {
                    size.width.resolve(
                        style.font_size,
                        style.font_size,
                        self.viewport,
                        origin.width,
                    )
                };
                let height = if size.height.is_auto() {
                    None
                } else {
                    size.height.resolve(
                        style.font_size,
                        style.font_size,
                        self.viewport,
                        origin.height,
                    )
                };
                match (width, height) {
                    (Some(width), Some(height)) => (width, height),
                    (Some(width), None) => (width, width * intrinsic_height / intrinsic_width),
                    (None, Some(height)) => (height * intrinsic_width / intrinsic_height, height),
                    (None, None) => (intrinsic_width, intrinsic_height),
                }
            }
        };
        if tile_width <= 0.0 || tile_height <= 0.0 {
            return;
        }

        let position_x = style
            .background_position
            .0
            .resolve(
                style.font_size,
                style.font_size,
                self.viewport,
                origin.width - tile_width,
            )
            .unwrap_or(0.0);
        let position_y = style
            .background_position
            .1
            .resolve(
                style.font_size,
                style.font_size,
                self.viewport,
                origin.height - tile_height,
            )
            .unwrap_or(0.0);
        let base_x = origin.x + position_x;
        let base_y = origin.y + position_y;

        let clip = self
            .effective_clip()
            .map(|clip| clip.intersection(&box_.rect))
            .unwrap_or(box_.rect);
        if clip.width <= 0.0 || clip.height <= 0.0 {
            return;
        }

        let repeat_x = matches!(
            style.background_repeat,
            BackgroundRepeat::Repeat | BackgroundRepeat::RepeatX
        );
        let repeat_y = matches!(
            style.background_repeat,
            BackgroundRepeat::Repeat | BackgroundRepeat::RepeatY
        );

        let tile_px_width = tile_width.round().max(1.0) as u32;
        let tile_px_height = tile_height.round().max(1.0) as u32;
        let scaled = if image.width() == tile_px_width && image.height() == tile_px_height {
            (*image).clone()
        } else {
            imageops::resize(
                image.as_ref(),
                tile_px_width,
                tile_px_height,
                imageops::FilterType::Triangle,
            )
        };

        let mut start_x = base_x;
        let mut start_y = base_y;
        if repeat_x && tile_width > 0.0 {
            let shift = ((start_x - clip.x) / tile_width).ceil();
            start_x -= shift * tile_width;
        }
        if repeat_y && tile_height > 0.0 {
            let shift = ((start_y - clip.y) / tile_height).ceil();
            start_y -= shift * tile_height;
        }

        let max_tiles = 4096usize;
        let mut drawn = 0usize;
        let mut y = start_y;
        loop {
            if y >= clip.bottom() || drawn >= max_tiles {
                break;
            }
            let mut x = start_x;
            loop {
                if x >= clip.right() || drawn >= max_tiles {
                    break;
                }
                if x + tile_width > clip.x && y + tile_height > clip.y {
                    self.blit_image(&scaled, x, y, opacity);
                    drawn += 1;
                }
                if !repeat_x {
                    break;
                }
                x += tile_width;
            }
            if !repeat_y {
                break;
            }
            y += tile_height;
        }
    }

    fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color, opacity: f32) {
        let alpha = (color.a as f32 / 255.0) * opacity;
        if alpha <= 0.0 || rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let clip = self.effective_clip().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        let bounds = rect.intersection(&clip).intersection(&Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }
        let x0 = bounds.x.floor().max(0.0) as u32;
        let y0 = bounds.y.floor().max(0.0) as u32;
        let x1 = bounds.right().ceil().min(self.canvas.width() as f32) as u32;
        let y1 = bounds.bottom().ceil().min(self.canvas.height() as f32) as u32;
        let src = [color.r, color.g, color.b];
        for y in y0..y1 {
            for x in x0..x1 {
                if !rounded_contains(rect, radius, x as f32 + 0.5, y as f32 + 0.5) {
                    continue;
                }
                let pixel = self.canvas.get_pixel_mut(x, y);
                blend_pixel(pixel, src, alpha);
            }
        }
    }

    fn paint_rounded_borders(&mut self, box_: &LayoutBox, radius: f32, opacity: f32) {
        let b = box_.border;
        let styles = [
            box_.style.border_style.top,
            box_.style.border_style.right,
            box_.style.border_style.bottom,
            box_.style.border_style.left,
        ];
        if styles.iter().all(|style| *style == BorderStyle::None)
            || (b.top <= 0.0 && b.right <= 0.0 && b.bottom <= 0.0 && b.left <= 0.0)
        {
            return;
        }
        let rect = box_.rect;
        let inner = Rect {
            x: rect.x + b.left,
            y: rect.y + b.top,
            width: (rect.width - b.left - b.right).max(0.0),
            height: (rect.height - b.top - b.bottom).max(0.0),
        };
        let inner_radius = (radius - b.top.max(b.right).max(b.bottom).max(b.left)).max(0.0);
        let colors = [
            box_.style.border_color.top,
            box_.style.border_color.right,
            box_.style.border_color.bottom,
            box_.style.border_color.left,
        ];
        let uniform_color = colors.iter().all(|color| *color == colors[0]);
        let clip = self.effective_clip().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        let bounds = rect.intersection(&clip).intersection(&Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }
        let x0 = bounds.x.floor().max(0.0) as u32;
        let y0 = bounds.y.floor().max(0.0) as u32;
        let x1 = bounds.right().ceil().min(self.canvas.width() as f32) as u32;
        let y1 = bounds.bottom().ceil().min(self.canvas.height() as f32) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                if !rounded_contains(rect, radius, px, py)
                    || rounded_contains(inner, inner_radius, px, py)
                {
                    continue;
                }
                let color = if uniform_color {
                    colors[0]
                } else {
                    let distances = [
                        py - rect.y,
                        rect.right() - px,
                        rect.bottom() - py,
                        px - rect.x,
                    ];
                    let mut nearest = 0usize;
                    for (index, distance) in distances.iter().enumerate() {
                        if *distance < distances[nearest] {
                            nearest = index;
                        }
                    }
                    colors[nearest]
                };
                let alpha = (color.a as f32 / 255.0) * opacity;
                if alpha <= 0.0 {
                    continue;
                }
                let pixel = self.canvas.get_pixel_mut(x, y);
                blend_pixel(pixel, [color.r, color.g, color.b], alpha);
            }
        }
    }

    fn paint_replaced_text(&mut self, box_: &LayoutBox, text: &str, opacity: f32) {
        let content = Rect {
            x: box_.content_x(),
            y: box_.content_y(),
            width: box_.content_width(),
            height: box_.content_height,
        };
        if content.width <= 0.0 || content.height <= 0.0 {
            return;
        }
        let previous = self.effective_clip();
        let clip = previous.unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        self.clip.push(clip.intersection(&content));
        let ascent = self.fonts.ascent(&box_.style);
        let descent = self.fonts.descent(&box_.style);
        let line_height = box_.style.line_height.max(ascent + descent);
        let offset = ((line_height - (ascent + descent)) / 2.0).max(0.0);
        for (index, line) in text.split('\n').enumerate() {
            let baseline = content.y + index as f32 * line_height + offset + ascent;
            if baseline - ascent > content.bottom() {
                break;
            }
            self.draw_text(line, content.x, baseline, &box_.style, opacity);
        }
        self.clip.pop();
    }

    fn paint_borders(&mut self, box_: &LayoutBox, opacity: f32) {
        let b = box_.border;
        let r = box_.rect;
        let edges = [
            (
                b.top,
                Rect {
                    x: r.x,
                    y: r.y,
                    width: r.width,
                    height: b.top,
                },
                box_.style.border_color.top,
                box_.style.border_style.top,
                true,
            ),
            (
                b.bottom,
                Rect {
                    x: r.x,
                    y: r.bottom() - b.bottom,
                    width: r.width,
                    height: b.bottom,
                },
                box_.style.border_color.bottom,
                box_.style.border_style.bottom,
                true,
            ),
            (
                b.left,
                Rect {
                    x: r.x,
                    y: r.y + b.top,
                    width: b.left,
                    height: (r.height - b.top - b.bottom).max(0.0),
                },
                box_.style.border_color.left,
                box_.style.border_style.left,
                false,
            ),
            (
                b.right,
                Rect {
                    x: r.right() - b.right,
                    y: r.y + b.top,
                    width: b.right,
                    height: (r.height - b.top - b.bottom).max(0.0),
                },
                box_.style.border_color.right,
                box_.style.border_style.right,
                false,
            ),
        ];
        for (width, rect, color, style, horizontal) in edges {
            if width > 0.0 {
                self.paint_edge(rect, horizontal, width, style, color, opacity);
            }
        }
    }

    fn paint_edge(
        &mut self,
        rect: Rect,
        horizontal: bool,
        width: f32,
        style: BorderStyle,
        color: Color,
        opacity: f32,
    ) {
        match style {
            BorderStyle::None => {}
            BorderStyle::Solid => self.fill_rect(rect, color, opacity),
            BorderStyle::Double => {
                let line = (width / 3.0).max(1.0);
                if horizontal {
                    self.fill_rect(
                        Rect {
                            height: line,
                            ..rect
                        },
                        color,
                        opacity,
                    );
                    self.fill_rect(
                        Rect {
                            y: rect.bottom() - line,
                            height: line,
                            ..rect
                        },
                        color,
                        opacity,
                    );
                } else {
                    self.fill_rect(
                        Rect {
                            width: line,
                            ..rect
                        },
                        color,
                        opacity,
                    );
                    self.fill_rect(
                        Rect {
                            x: rect.right() - line,
                            width: line,
                            ..rect
                        },
                        color,
                        opacity,
                    );
                }
            }
            BorderStyle::Dashed | BorderStyle::Dotted => {
                let (dash, gap) = if style == BorderStyle::Dashed {
                    ((width * 2.5).max(2.0), (width * 1.5).max(2.0))
                } else {
                    (width.max(1.0), width.max(1.0))
                };
                let length = if horizontal { rect.width } else { rect.height };
                let mut offset = 0.0f32;
                while offset < length {
                    let segment = dash.min(length - offset);
                    let segment_rect = if horizontal {
                        Rect {
                            x: rect.x + offset,
                            width: segment,
                            ..rect
                        }
                    } else {
                        Rect {
                            y: rect.y + offset,
                            height: segment,
                            ..rect
                        }
                    };
                    self.fill_rect(segment_rect, color, opacity);
                    offset += dash + gap;
                }
            }
        }
    }

    fn effective_clip(&self) -> Option<Rect> {
        self.clip.iter().copied().reduce(|a, b| a.intersection(&b))
    }

    fn fill_rect(&mut self, rect: Rect, color: Color, opacity: f32) {
        let alpha = (color.a as f32 / 255.0) * opacity;
        if alpha <= 0.0 || rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let bounds = match self.effective_clip() {
            Some(clip) => rect.intersection(&clip),
            None => rect,
        };
        let bounds = bounds.intersection(&Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }
        let x0 = bounds.x.floor().max(0.0) as u32;
        let y0 = bounds.y.floor().max(0.0) as u32;
        let x1 = (bounds.right().ceil()).min(self.canvas.width() as f32) as u32;
        let y1 = (bounds.bottom().ceil()).min(self.canvas.height() as f32) as u32;
        let src = [color.r, color.g, color.b];
        for y in y0..y1 {
            for x in x0..x1 {
                let pixel = self.canvas.get_pixel_mut(x, y);
                blend_pixel(pixel, src, alpha);
            }
        }
    }

    fn paint_svg(&mut self, node: NodeId, box_: &LayoutBox, opacity: f32) {
        let content = Rect {
            x: box_.content_x(),
            y: box_.content_y(),
            width: box_.content_width(),
            height: box_.content_height,
        };
        if content.width <= 0.0 || content.height <= 0.0 {
            return;
        }
        let view_box = self
            .document
            .get_attribute(node, "viewBox")
            .map(parse_view_box)
            .unwrap_or(None);
        let transform = if let Some((vx, vy, vw, vh)) = view_box {
            if vw <= 0.0 || vh <= 0.0 {
                SvgTransform::identity(content.x, content.y)
            } else {
                let scale = (content.width / vw).min(content.height / vh);
                let offset_x = content.x + (content.width - vw * scale) / 2.0;
                let offset_y = content.y + (content.height - vh * scale) / 2.0;
                SvgTransform {
                    tx: offset_x - vx * scale,
                    ty: offset_y - vy * scale,
                    sx: scale,
                    sy: scale,
                }
            }
        } else {
            SvgTransform::identity(content.x, content.y)
        };
        let style = self.svg_style(node, &SvgStyle::default());
        self.clip.push(content);
        self.paint_svg_children(node, transform, &style, opacity);
        self.clip.pop();
    }

    fn svg_style(&self, node: NodeId, parent: &SvgStyle) -> SvgStyle {
        let mut style = parent.clone();
        if let Some(fill) = self.document.get_attribute(node, "fill") {
            style.fill = svg_paint(fill);
        }
        if let Some(stroke) = self.document.get_attribute(node, "stroke") {
            style.stroke = svg_paint(stroke);
        }
        if let Some(width) = self
            .document
            .get_attribute(node, "stroke-width")
            .and_then(|value| value.parse::<f32>().ok())
        {
            style.stroke_width = width.max(0.0);
        }
        if let Some(rule) = self.document.get_attribute(node, "fill-rule") {
            style.fill_rule = if rule.trim().eq_ignore_ascii_case("evenodd") {
                FillRule::EvenOdd
            } else {
                FillRule::NonZero
            };
        }
        if let Some(opacity) = self
            .document
            .get_attribute(node, "opacity")
            .and_then(|value| value.parse::<f32>().ok())
        {
            style.opacity *= opacity.clamp(0.0, 1.0);
        }
        if let Some(opacity) = self
            .document
            .get_attribute(node, "fill-opacity")
            .and_then(|value| value.parse::<f32>().ok())
        {
            style.fill_opacity *= opacity.clamp(0.0, 1.0);
        }
        if let Some(opacity) = self
            .document
            .get_attribute(node, "stroke-opacity")
            .and_then(|value| value.parse::<f32>().ok())
        {
            style.stroke_opacity *= opacity.clamp(0.0, 1.0);
        }
        style.fill_opacity *= style.opacity;
        style.stroke_opacity *= style.opacity;
        style
    }

    /// Resolve an SVG paint (solid color or `url(#gradient)`) into a paint the
    /// shape rasterizer can sample. `bounds` is the shape's page-space bounding
    /// box, used for `objectBoundingBox` gradient units.
    fn resolve_svg_paint(
        &self,
        paint: &Option<SvgPaint>,
        bounds: Rect,
        transform: SvgTransform,
    ) -> Option<ResolvedPaint> {
        match paint {
            None => None,
            Some(SvgPaint::Solid(color)) => Some(ResolvedPaint::Solid(*color)),
            Some(SvgPaint::Reference(id)) => {
                let reference = id.trim().trim_start_matches('#');
                if reference.is_empty() {
                    return None;
                }
                if let Some(node) = self.document.get_element_by_id(reference)
                    && self.document.tag_name(node) == Some("pattern")
                {
                    return self.resolve_svg_pattern(node, bounds, transform);
                }
                self.resolve_svg_gradient(id, bounds, transform)
            }
        }
    }

    /// Resolve a basic `<pattern>` into a tiling paint.
    fn resolve_svg_pattern(
        &self,
        node: NodeId,
        bounds: Rect,
        transform: SvgTransform,
    ) -> Option<ResolvedPaint> {
        let user_space = self
            .document
            .get_attribute(node, "patternUnits")
            .map(|value| value.trim().eq_ignore_ascii_case("userSpaceOnUse"))
            .unwrap_or(false);
        let width = bounds.width.max(1e-6);
        let height = bounds.height.max(1e-6);
        let (px, py, pw, ph) = if user_space {
            let gx = svg_ratio(self.document, node, "x", 0.0);
            let gy = svg_ratio(self.document, node, "y", 0.0);
            let (px, py) = transform.map(gx, gy);
            (
                px,
                py,
                svg_ratio(self.document, node, "width", 0.0) * transform.sx.abs(),
                svg_ratio(self.document, node, "height", 0.0) * transform.sy.abs(),
            )
        } else {
            (
                bounds.x + svg_ratio(self.document, node, "x", 0.0) * width,
                bounds.y + svg_ratio(self.document, node, "y", 0.0) * height,
                svg_ratio(self.document, node, "width", 0.0) * width,
                svg_ratio(self.document, node, "height", 0.0) * height,
            )
        };
        if pw <= 0.0 || ph <= 0.0 {
            return None;
        }
        let mut shapes = Vec::new();
        for child in self.document.children(node).to_vec() {
            let Some(tag) = self.document.tag_name(child) else {
                continue;
            };
            let color = self
                .document
                .get_attribute(child, "fill")
                .and_then(svg_color)
                .unwrap_or(Color::BLACK);
            match tag {
                "rect" => shapes.push(SvgPatternShape {
                    kind: SvgPatternShapeKind::Rect {
                        x: svg_number(self.document, child, "x").unwrap_or(0.0),
                        y: svg_number(self.document, child, "y").unwrap_or(0.0),
                        width: svg_number(self.document, child, "width").unwrap_or(0.0),
                        height: svg_number(self.document, child, "height").unwrap_or(0.0),
                    },
                    color,
                }),
                "circle" => shapes.push(SvgPatternShape {
                    kind: SvgPatternShapeKind::Circle {
                        cx: svg_number(self.document, child, "cx").unwrap_or(0.0),
                        cy: svg_number(self.document, child, "cy").unwrap_or(0.0),
                        r: svg_number(self.document, child, "r").unwrap_or(0.0),
                    },
                    color,
                }),
                _ => {}
            }
        }
        if shapes.is_empty() {
            return None;
        }
        Some(ResolvedPaint::Pattern(SvgPattern {
            x: px,
            y: py,
            width: pw,
            height: ph,
            shapes,
        }))
    }

    fn resolve_svg_gradient(
        &self,
        id: &str,
        bounds: Rect,
        transform: SvgTransform,
    ) -> Option<ResolvedPaint> {
        let id = id.trim().trim_start_matches('#');
        if id.is_empty() {
            return None;
        }
        let node = self.document.get_element_by_id(id)?;
        let tag = self.document.tag_name(node)?;
        let user_space = self
            .document
            .get_attribute(node, "gradientUnits")
            .map(|value| value.trim().eq_ignore_ascii_case("userSpaceOnUse"))
            .unwrap_or(false);
        let stops = self.svg_gradient_stops(node);
        if stops.is_empty() {
            return None;
        }
        let bounds = Rect {
            width: bounds.width.max(1e-6),
            height: bounds.height.max(1e-6),
            ..bounds
        };
        let geometry = match tag {
            "linearGradient" => {
                let x1 = svg_ratio(self.document, node, "x1", 0.0);
                let y1 = svg_ratio(self.document, node, "y1", 0.0);
                let x2 = svg_ratio(self.document, node, "x2", 1.0);
                let y2 = svg_ratio(self.document, node, "y2", 0.0);
                if user_space {
                    let (ax, ay) = transform.map(x1, y1);
                    let (bx, by) = transform.map(x2, y2);
                    SvgGradientGeometry::Linear {
                        x0: ax,
                        y0: ay,
                        x1: bx,
                        y1: by,
                    }
                } else {
                    SvgGradientGeometry::Linear {
                        x0: bounds.x + x1 * bounds.width,
                        y0: bounds.y + y1 * bounds.height,
                        x1: bounds.x + x2 * bounds.width,
                        y1: bounds.y + y2 * bounds.height,
                    }
                }
            }
            "radialGradient" => {
                let cx = svg_ratio(self.document, node, "cx", 0.5);
                let cy = svg_ratio(self.document, node, "cy", 0.5);
                let r = svg_ratio(self.document, node, "r", 0.5);
                if user_space {
                    let (px, py) = transform.map(cx, cy);
                    SvgGradientGeometry::Radial {
                        cx: px,
                        cy: py,
                        rx: (r * transform.sx.abs()).max(1e-6),
                        ry: (r * transform.sy.abs()).max(1e-6),
                    }
                } else {
                    SvgGradientGeometry::Radial {
                        cx: bounds.x + cx * bounds.width,
                        cy: bounds.y + cy * bounds.height,
                        rx: (r * bounds.width).max(1e-6),
                        ry: (r * bounds.height).max(1e-6),
                    }
                }
            }
            _ => return None,
        };
        Some(ResolvedPaint::Gradient(SvgGradient { geometry, stops }))
    }

    fn svg_gradient_stops(&self, node: NodeId) -> Vec<SvgStop> {
        let mut stops = Vec::new();
        for child in self.document.children(node).to_vec() {
            if self.document.tag_name(child) != Some("stop") {
                continue;
            }
            let offset = self
                .document
                .get_attribute(child, "offset")
                .map(|value| {
                    let value = value.trim();
                    if let Some(percent) = value.strip_suffix('%') {
                        percent.trim().parse::<f32>().unwrap_or(0.0) / 100.0
                    } else {
                        value.parse::<f32>().unwrap_or(0.0)
                    }
                })
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let mut color = self
                .document
                .get_attribute(child, "stop-color")
                .and_then(svg_color)
                .unwrap_or(Color::BLACK);
            if let Some(opacity) = self
                .document
                .get_attribute(child, "stop-opacity")
                .and_then(|value| value.parse::<f32>().ok())
            {
                color.a = ((color.a as f32) * opacity.clamp(0.0, 1.0)) as u8;
            }
            stops.push(SvgStop { offset, color });
        }
        stops.sort_by(|a, b| {
            a.offset
                .partial_cmp(&b.offset)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        stops
    }

    fn paint_svg_children(
        &mut self,
        node: NodeId,
        transform: SvgTransform,
        style: &SvgStyle,
        opacity: f32,
    ) {
        for child in self.document.children(node).to_vec() {
            let NodeData::Element(element) = &self.document.node(child).data else {
                continue;
            };
            let tag = element.name.as_str();
            let mut child_transform = transform;
            if let Some(value) = self.document.get_attribute(child, "transform") {
                child_transform = transform.combine(&parse_svg_transform(value));
            }
            let child_style = self.svg_style(child, style);
            match tag {
                "g" | "svg" | "defs" => {
                    if tag != "defs" {
                        self.paint_svg_children(child, child_transform, &child_style, opacity);
                    }
                }
                "rect" => {
                    let x = svg_number(self.document, child, "x").unwrap_or(0.0);
                    let y = svg_number(self.document, child, "y").unwrap_or(0.0);
                    let width = svg_number(self.document, child, "width").unwrap_or(0.0);
                    let height = svg_number(self.document, child, "height").unwrap_or(0.0);
                    let (x0, y0) = child_transform.map(x, y);
                    let (x1, y1) = child_transform.map(x + width, y + height);
                    let rect = Rect {
                        x: x0.min(x1),
                        y: y0.min(y1),
                        width: (x1 - x0).abs(),
                        height: (y1 - y0).abs(),
                    };
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.fill, rect, child_transform)
                    {
                        self.fill_rect_paint(rect, &paint, opacity * child_style.fill_opacity);
                    }
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.stroke, rect, child_transform)
                    {
                        self.stroke_rect(
                            rect,
                            &paint,
                            child_style.stroke_width * transform.sx.abs(),
                            opacity * child_style.stroke_opacity,
                        );
                    }
                }
                "circle" | "ellipse" => {
                    let cx = svg_number(self.document, child, "cx").unwrap_or(0.0);
                    let cy = svg_number(self.document, child, "cy").unwrap_or(0.0);
                    let rx = if tag == "circle" {
                        svg_number(self.document, child, "r").unwrap_or(0.0)
                    } else {
                        svg_number(self.document, child, "rx").unwrap_or(0.0)
                    };
                    let ry = if tag == "circle" {
                        rx
                    } else {
                        svg_number(self.document, child, "ry").unwrap_or(0.0)
                    };
                    let (px, py) = child_transform.map(cx, cy);
                    let rx = rx * child_transform.sx.abs();
                    let ry = ry * child_transform.sy.abs();
                    let bounds = Rect {
                        x: px - rx,
                        y: py - ry,
                        width: rx * 2.0,
                        height: ry * 2.0,
                    };
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.fill, bounds, child_transform)
                    {
                        self.fill_ellipse(
                            px,
                            py,
                            rx,
                            ry,
                            &paint,
                            opacity * child_style.fill_opacity,
                        );
                    }
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.stroke, bounds, child_transform)
                    {
                        self.stroke_ellipse(
                            px,
                            py,
                            rx,
                            ry,
                            &paint,
                            child_style.stroke_width * transform.sx.abs(),
                            opacity * child_style.stroke_opacity,
                        );
                    }
                }
                "line" => {
                    let x1 = svg_number(self.document, child, "x1").unwrap_or(0.0);
                    let y1 = svg_number(self.document, child, "y1").unwrap_or(0.0);
                    let x2 = svg_number(self.document, child, "x2").unwrap_or(0.0);
                    let y2 = svg_number(self.document, child, "y2").unwrap_or(0.0);
                    let (a, b) = child_transform.map(x1, y1);
                    let (c, d) = child_transform.map(x2, y2);
                    let bounds = Rect {
                        x: a.min(c),
                        y: b.min(d),
                        width: (a - c).abs(),
                        height: (b - d).abs(),
                    };
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.stroke, bounds, child_transform)
                    {
                        self.stroke_line(
                            a,
                            b,
                            c,
                            d,
                            &paint,
                            child_style.stroke_width * transform.sx.abs(),
                            opacity * child_style.stroke_opacity,
                        );
                    }
                }
                "polyline" | "polygon" => {
                    let points = self
                        .document
                        .get_attribute(child, "points")
                        .map(parse_svg_points)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(x, y)| child_transform.map(x, y))
                        .collect::<Vec<_>>();
                    let bounds = points_bounds(&points).unwrap_or(Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                    if tag == "polygon"
                        && let Some(paint) =
                            self.resolve_svg_paint(&child_style.fill, bounds, child_transform)
                    {
                        self.fill_path(
                            std::slice::from_ref(&points),
                            &paint,
                            opacity * child_style.fill_opacity,
                            child_style.fill_rule,
                        );
                    }
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.stroke, bounds, child_transform)
                    {
                        self.stroke_polyline(
                            &points,
                            &paint,
                            child_style.stroke_width * transform.sx.abs(),
                            opacity * child_style.stroke_opacity,
                        );
                    }
                }
                "path" => {
                    let subpaths = self
                        .document
                        .get_attribute(child, "d")
                        .map(parse_svg_path)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|points| {
                            points
                                .into_iter()
                                .map(|(x, y)| child_transform.map(x, y))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    let bounds = subpaths
                        .iter()
                        .filter_map(|points| points_bounds(points))
                        .reduce(union_rect)
                        .unwrap_or(Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 0.0,
                            height: 0.0,
                        });
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.fill, bounds, child_transform)
                    {
                        self.fill_path(
                            &subpaths,
                            &paint,
                            opacity * child_style.fill_opacity,
                            child_style.fill_rule,
                        );
                    }
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.stroke, bounds, child_transform)
                    {
                        for points in &subpaths {
                            self.stroke_polyline(
                                points,
                                &paint,
                                child_style.stroke_width * transform.sx.abs(),
                                opacity * child_style.stroke_opacity,
                            );
                        }
                    }
                }
                "text" => {
                    let x = svg_number(self.document, child, "x").unwrap_or(0.0);
                    let y = svg_number(self.document, child, "y").unwrap_or(0.0);
                    let (px, py) = child_transform.map(x, y);
                    let point = Rect {
                        x: px,
                        y: py,
                        width: 1.0,
                        height: 1.0,
                    };
                    if let Some(paint) =
                        self.resolve_svg_paint(&child_style.fill, point, child_transform)
                    {
                        let color = paint.sample(px, py);
                        let mut text_style = ComputedStyle::initial();
                        text_style.color = color;
                        text_style.font_size = svg_number(self.document, child, "font-size")
                            .unwrap_or(16.0)
                            * child_transform.sy.abs();
                        let text = self.document.text_content(child);
                        let width = self.fonts.text_width(&text, &text_style);
                        let x = match self
                            .document
                            .get_attribute(child, "text-anchor")
                            .unwrap_or("start")
                        {
                            "middle" => px - width / 2.0,
                            "end" => px - width,
                            _ => px,
                        };
                        self.draw_text(&text, x, py, &text_style, opacity);
                    }
                }
                _ => {
                    self.paint_svg_children(child, child_transform, &child_style, opacity);
                }
            }
        }
    }

    fn stroke_rect(&mut self, rect: Rect, paint: &ResolvedPaint, width: f32, opacity: f32) {
        let width = width.max(1.0);
        self.fill_rect_paint(
            Rect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: width,
            },
            paint,
            opacity,
        );
        self.fill_rect_paint(
            Rect {
                x: rect.x,
                y: rect.bottom() - width,
                width: rect.width,
                height: width,
            },
            paint,
            opacity,
        );
        self.fill_rect_paint(
            Rect {
                x: rect.x,
                y: rect.y,
                width,
                height: rect.height,
            },
            paint,
            opacity,
        );
        self.fill_rect_paint(
            Rect {
                x: rect.right() - width,
                y: rect.y,
                width,
                height: rect.height,
            },
            paint,
            opacity,
        );
    }

    /// Fill a rectangle with a solid or gradient paint.
    fn fill_rect_paint(&mut self, rect: Rect, paint: &ResolvedPaint, opacity: f32) {
        if let ResolvedPaint::Solid(color) = paint {
            self.fill_rect(rect, *color, opacity);
            return;
        }
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let canvas_bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        };
        let bounds = {
            let clipped = match self.effective_clip() {
                Some(clip) => rect.intersection(&clip),
                None => rect,
            };
            clipped.intersection(&canvas_bounds)
        };
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }
        let x0 = bounds.x.floor().max(0.0) as u32;
        let y0 = bounds.y.floor().max(0.0) as u32;
        let x1 = bounds.right().ceil().min(canvas_bounds.width) as u32;
        let y1 = bounds.bottom().ceil().min(canvas_bounds.height) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let color = paint.sample(x as f32 + 0.5, y as f32 + 0.5);
                let alpha = (color.a as f32 / 255.0) * opacity;
                if alpha <= 0.0 {
                    continue;
                }
                blend_pixel(
                    self.canvas.get_pixel_mut(x, y),
                    [color.r, color.g, color.b],
                    alpha,
                );
            }
        }
    }

    fn fill_ellipse(
        &mut self,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        paint: &ResolvedPaint,
        opacity: f32,
    ) {
        if rx <= 0.0 || ry <= 0.0 {
            return;
        }
        let clip = self.effective_clip().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        let left = (cx - rx).floor().max(clip.x).max(0.0) as u32;
        let right = (cx + rx)
            .ceil()
            .min(clip.right())
            .min(self.canvas.width() as f32) as u32;
        let top = (cy - ry).floor().max(clip.y).max(0.0) as u32;
        let bottom = (cy + ry)
            .ceil()
            .min(clip.bottom())
            .min(self.canvas.height() as f32) as u32;
        for y in top..bottom {
            for x in left..right {
                let dx = (x as f32 + 0.5 - cx) / rx;
                let dy = (y as f32 + 0.5 - cy) / ry;
                if dx * dx + dy * dy > 1.0 {
                    continue;
                }
                let color = paint.sample(x as f32 + 0.5, y as f32 + 0.5);
                let alpha = (color.a as f32 / 255.0) * opacity;
                if alpha <= 0.0 {
                    continue;
                }
                blend_pixel(
                    self.canvas.get_pixel_mut(x, y),
                    [color.r, color.g, color.b],
                    alpha,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke_ellipse(
        &mut self,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        paint: &ResolvedPaint,
        width: f32,
        opacity: f32,
    ) {
        if rx <= 0.0 || ry <= 0.0 {
            return;
        }
        let steps = ((rx.max(ry) * 6.0) as usize).clamp(16, 512);
        for step in 0..steps {
            let angle = step as f32 / steps as f32 * std::f32::consts::TAU;
            self.fill_ellipse(
                cx + angle.cos() * rx,
                cy + angle.sin() * ry,
                width / 2.0,
                width / 2.0,
                paint,
                opacity,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke_line(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        paint: &ResolvedPaint,
        width: f32,
        opacity: f32,
    ) {
        let dx = x2 - x1;
        let dy = y2 - y1;
        let length = (dx * dx + dy * dy).sqrt();
        let steps = (length * 2.0).ceil().max(1.0) as usize;
        let radius = width.max(1.0) / 2.0;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            self.fill_ellipse(x1 + dx * t, y1 + dy * t, radius, radius, paint, opacity);
        }
    }

    fn stroke_polyline(
        &mut self,
        points: &[(f32, f32)],
        paint: &ResolvedPaint,
        width: f32,
        opacity: f32,
    ) {
        for pair in points.windows(2) {
            self.stroke_line(
                pair[0].0, pair[0].1, pair[1].0, pair[1].1, paint, width, opacity,
            );
        }
    }

    /// Fill one or more polygons with the given fill rule.
    fn fill_path(
        &mut self,
        subpaths: &[Vec<(f32, f32)>],
        paint: &ResolvedPaint,
        opacity: f32,
        rule: FillRule,
    ) {
        let mut min_y = f32::INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for points in subpaths {
            for (_, y) in points {
                min_y = min_y.min(*y);
                max_y = max_y.max(*y);
            }
        }
        if !min_y.is_finite() || !max_y.is_finite() {
            return;
        }
        let min_y = min_y.floor().max(0.0) as i32;
        let max_y = max_y.ceil().min(self.canvas.height() as f32) as i32;
        for y in min_y..max_y {
            let scan = y as f32 + 0.5;
            let mut crossings: Vec<(f32, i32)> = Vec::new();
            for points in subpaths {
                if points.len() < 2 {
                    continue;
                }
                for pair in points
                    .iter()
                    .zip(points.iter().cycle().skip(1))
                    .take(points.len())
                {
                    let (x1, y1) = *pair.0;
                    let (x2, y2) = *pair.1;
                    if y1 == y2 {
                        continue;
                    }
                    if (y1 <= scan && y2 > scan) || (y2 <= scan && y1 > scan) {
                        let t = (scan - y1) / (y2 - y1);
                        crossings.push((x1 + t * (x2 - x1), if y2 > y1 { 1 } else { -1 }));
                    }
                }
            }
            crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            match rule {
                FillRule::EvenOdd => {
                    for pair in crossings.chunks(2) {
                        if let [(left, _), (right, _)] = pair {
                            self.fill_span(y, *left, *right, paint, opacity);
                        }
                    }
                }
                FillRule::NonZero => {
                    let mut winding = 0i32;
                    for index in 0..crossings.len().saturating_sub(1) {
                        winding += crossings[index].1;
                        if winding != 0 {
                            self.fill_span(
                                y,
                                crossings[index].0,
                                crossings[index + 1].0,
                                paint,
                                opacity,
                            );
                        }
                    }
                }
            }
        }
    }

    fn fill_span(&mut self, y: i32, x0: f32, x1: f32, paint: &ResolvedPaint, opacity: f32) {
        let canvas_width = self.canvas.width() as i32;
        let mut left = x0.floor() as i32;
        let mut right = x1.ceil() as i32;
        if let Some(clip) = self.effective_clip() {
            left = left.max(clip.x.floor() as i32);
            right = right.min(clip.right().ceil() as i32);
        }
        left = left.max(0);
        right = right.min(canvas_width);
        if right <= left || y < 0 || y >= self.canvas.height() as i32 {
            return;
        }
        if let ResolvedPaint::Solid(color) = paint {
            self.fill_rect(
                Rect {
                    x: left as f32,
                    y: y as f32,
                    width: (right - left) as f32,
                    height: 1.0,
                },
                *color,
                opacity,
            );
            return;
        }
        for x in left..right {
            let color = paint.sample(x as f32 + 0.5, y as f32 + 0.5);
            let alpha = (color.a as f32 / 255.0) * opacity;
            if alpha <= 0.0 {
                continue;
            }
            blend_pixel(
                self.canvas.get_pixel_mut(x as u32, y as u32),
                [color.r, color.g, color.b],
                alpha,
            );
        }
    }

    fn draw_text(
        &mut self,
        text: &str,
        x: f32,
        baseline: f32,
        style: &crate::engine::css::ComputedStyle,
        opacity: f32,
    ) {
        let font = self.fonts.font_for(style);
        let italic = style.font_style == FontStyle::Italic;
        let color_alpha = (style.color.a as f32 / 255.0) * opacity;
        let space_advance = font.metrics(' ', style.font_size).advance_width;
        let mut pen = x;
        for ch in text.chars() {
            if ch == '\t' {
                pen += space_advance * 4.0;
                continue;
            }
            let (metrics, bitmap) = font.rasterize(ch, style.font_size);
            if ch.is_whitespace() || metrics.width == 0 || metrics.height == 0 {
                pen += metrics.advance_width;
                continue;
            }
            let gx = pen + metrics.xmin as f32;
            let gy = baseline - (metrics.height as f32 + metrics.ymin as f32);
            self.blit_glyph(
                &bitmap,
                metrics.width,
                metrics.height,
                gx,
                gy,
                style.color,
                color_alpha,
                italic,
            );
            pen += metrics.advance_width;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_glyph(
        &mut self,
        bitmap: &[u8],
        width: usize,
        height: usize,
        x: f32,
        y: f32,
        color: Color,
        opacity: f32,
        italic: bool,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        let clip = self.effective_clip().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        let shear = if italic { 0.2 } else { 0.0 };
        for row in 0..height {
            let dy = y + row as f32;
            if dy < clip.y || dy >= clip.bottom() {
                continue;
            }
            let row_x = x + shear * (height - row) as f32;
            for col in 0..width {
                let coverage = bitmap[row * width + col];
                if coverage == 0 {
                    continue;
                }
                let dx = row_x + col as f32;
                if dx < clip.x || dx >= clip.right() {
                    continue;
                }
                let px = dx as u32;
                let py = dy as u32;
                if px >= self.canvas.width() || py >= self.canvas.height() {
                    continue;
                }
                let alpha = (coverage as f32 / 255.0) * opacity;
                let pixel = self.canvas.get_pixel_mut(px, py);
                blend_pixel(pixel, [color.r, color.g, color.b], alpha);
            }
        }
    }

    fn lookup_image(&self, url: &str) -> Option<Arc<RgbaImage>> {
        if let Some(image) = self.images.get(url) {
            return Some(Arc::clone(image));
        }
        if let Some(base) = self.base_url
            && let Ok(resolved) = base.join(url)
        {
            return self.images.get(resolved.as_str()).cloned();
        }
        None
    }

    fn draw_image(&mut self, image: &RgbaImage, dest: Rect, opacity: f32) {
        if dest.width <= 0.0 || dest.height <= 0.0 {
            return;
        }
        let width = dest.width.round().max(1.0) as u32;
        let height = dest.height.round().max(1.0) as u32;
        if image.width() == width && image.height() == height {
            self.blit_image(image, dest.x, dest.y, opacity);
            return;
        }
        let scaled = imageops::resize(image, width, height, imageops::FilterType::Triangle);
        self.blit_image(&scaled, dest.x, dest.y, opacity);
    }

    fn blit_image(&mut self, source: &RgbaImage, x: f32, y: f32, opacity: f32) {
        let clip = self.effective_clip().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: self.canvas.width() as f32,
            height: self.canvas.height() as f32,
        });
        let dest = Rect {
            x,
            y,
            width: source.width() as f32,
            height: source.height() as f32,
        };
        let visible = dest.intersection(&clip);
        if visible.width <= 0.0 || visible.height <= 0.0 {
            return;
        }
        let x0 = visible.x.floor().max(0.0) as u32;
        let y0 = visible.y.floor().max(0.0) as u32;
        let x1 = (visible.right().ceil() as u32).min(x as u32 + source.width());
        let y1 = (visible.bottom().ceil() as u32).min(y as u32 + source.height());
        let origin_x = x as u32;
        let origin_y = y as u32;
        for py in y0..y1 {
            for px in x0..x1 {
                if px >= self.canvas.width() || py >= self.canvas.height() {
                    continue;
                }
                let sx = px
                    .saturating_sub(origin_x)
                    .min(source.width().saturating_sub(1));
                let sy = py
                    .saturating_sub(origin_y)
                    .min(source.height().saturating_sub(1));
                let source_pixel = source.get_pixel(sx, sy);
                if source_pixel[3] == 0 {
                    continue;
                }
                let alpha = (source_pixel[3] as f32 / 255.0) * opacity;
                let pixel = self.canvas.get_pixel_mut(px, py);
                blend_pixel(
                    pixel,
                    [source_pixel[0], source_pixel[1], source_pixel[2]],
                    alpha,
                );
            }
        }
    }
}

fn resolve_radius(box_: &LayoutBox, viewport: Viewport) -> f32 {
    let radius = box_.style.border_radius.resolve(
        box_.style.font_size,
        box_.style.font_size,
        viewport,
        box_.rect.width.min(box_.rect.height),
    );
    radius.unwrap_or(0.0).max(0.0)
}

fn rounded_contains(rect: Rect, radius: f32, x: f32, y: f32) -> bool {
    if x < rect.x || x > rect.right() || y < rect.y || y > rect.bottom() {
        return false;
    }
    let radius = radius.min(rect.width / 2.0).min(rect.height / 2.0);
    if radius <= 0.0 {
        return true;
    }
    let corners = [
        (
            rect.x + radius,
            rect.y + radius,
            x < rect.x + radius && y < rect.y + radius,
        ),
        (
            rect.right() - radius,
            rect.y + radius,
            x > rect.right() - radius && y < rect.y + radius,
        ),
        (
            rect.x + radius,
            rect.bottom() - radius,
            x < rect.x + radius && y > rect.bottom() - radius,
        ),
        (
            rect.right() - radius,
            rect.bottom() - radius,
            x > rect.right() - radius && y > rect.bottom() - radius,
        ),
    ];
    for (cx, cy, in_corner) in corners {
        if in_corner {
            let dx = x - cx;
            let dy = y - cy;
            return dx * dx + dy * dy <= radius * radius;
        }
    }
    true
}

#[derive(Clone, Copy, Debug)]
struct SvgTransform {
    tx: f32,
    ty: f32,
    sx: f32,
    sy: f32,
}

impl SvgTransform {
    fn identity(offset_x: f32, offset_y: f32) -> Self {
        Self {
            tx: offset_x,
            ty: offset_y,
            sx: 1.0,
            sy: 1.0,
        }
    }

    fn map(self, x: f32, y: f32) -> (f32, f32) {
        (self.tx + x * self.sx, self.ty + y * self.sy)
    }

    fn combine(self, local: &SvgTransform) -> Self {
        Self {
            tx: self.tx + local.tx * self.sx,
            ty: self.ty + local.ty * self.sy,
            sx: self.sx * local.sx,
            sy: self.sy * local.sy,
        }
    }
}

#[derive(Clone)]
enum SvgPaint {
    Solid(Color),
    Reference(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone)]
enum ResolvedPaint {
    Solid(Color),
    Gradient(SvgGradient),
    Pattern(SvgPattern),
}

impl ResolvedPaint {
    fn sample(&self, x: f32, y: f32) -> Color {
        match self {
            ResolvedPaint::Solid(color) => *color,
            ResolvedPaint::Gradient(gradient) => gradient.sample(x, y),
            ResolvedPaint::Pattern(pattern) => pattern.sample(x, y),
        }
    }
}

#[derive(Clone)]
struct SvgGradient {
    geometry: SvgGradientGeometry,
    stops: Vec<SvgStop>,
}

#[derive(Clone, Copy)]
enum SvgGradientGeometry {
    Linear { x0: f32, y0: f32, x1: f32, y1: f32 },
    Radial { cx: f32, cy: f32, rx: f32, ry: f32 },
}

#[derive(Clone, Copy)]
struct SvgStop {
    offset: f32,
    color: Color,
}

/// A basic `<pattern>`: a repeating tile composed of filled rects and circles.
#[derive(Clone)]
struct SvgPattern {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    shapes: Vec<SvgPatternShape>,
}

#[derive(Clone, Copy)]
struct SvgPatternShape {
    kind: SvgPatternShapeKind,
    color: Color,
}

#[derive(Clone, Copy)]
enum SvgPatternShapeKind {
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
    },
}

impl SvgPatternShape {
    fn contains(&self, x: f32, y: f32) -> bool {
        match self.kind {
            SvgPatternShapeKind::Rect {
                x: rx,
                y: ry,
                width,
                height,
            } => x >= rx && x < rx + width && y >= ry && y < ry + height,
            SvgPatternShapeKind::Circle { cx, cy, r } => {
                let dx = x - cx;
                let dy = y - cy;
                dx * dx + dy * dy <= r * r
            }
        }
    }
}

impl SvgPattern {
    fn sample(&self, x: f32, y: f32) -> Color {
        if self.width <= 0.0 || self.height <= 0.0 {
            return Color::TRANSPARENT;
        }
        let lx = (x - self.x).rem_euclid(self.width);
        let ly = (y - self.y).rem_euclid(self.height);
        for shape in &self.shapes {
            if shape.contains(lx, ly) {
                return shape.color;
            }
        }
        Color::TRANSPARENT
    }
}

impl SvgGradient {
    fn sample(&self, x: f32, y: f32) -> Color {
        let t = match self.geometry {
            SvgGradientGeometry::Linear { x0, y0, x1, y1 } => {
                let dx = x1 - x0;
                let dy = y1 - y0;
                let len2 = dx * dx + dy * dy;
                if len2 <= f32::EPSILON {
                    0.0
                } else {
                    ((x - x0) * dx + (y - y0) * dy) / len2
                }
            }
            SvgGradientGeometry::Radial { cx, cy, rx, ry } => {
                let nx = (x - cx) / rx.max(1e-6);
                let ny = (y - cy) / ry.max(1e-6);
                (nx * nx + ny * ny).sqrt()
            }
        };
        self.color_at(t)
    }

    fn color_at(&self, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let Some(first) = self.stops.first() else {
            return Color::TRANSPARENT;
        };
        if t <= first.offset {
            return first.color;
        }
        for pair in self.stops.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if t <= b.offset {
                let span = (b.offset - a.offset).max(f32::EPSILON);
                let f = ((t - a.offset) / span).clamp(0.0, 1.0);
                return lerp_svg_color(a.color, b.color, f);
            }
        }
        self.stops
            .last()
            .map(|stop| stop.color)
            .unwrap_or(first.color)
    }
}

fn lerp_svg_color(a: Color, b: Color, f: f32) -> Color {
    let mix = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * f)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: mix(a.a, b.a),
    }
}

#[derive(Clone)]
struct SvgStyle {
    fill: Option<SvgPaint>,
    stroke: Option<SvgPaint>,
    stroke_width: f32,
    opacity: f32,
    fill_opacity: f32,
    stroke_opacity: f32,
    fill_rule: FillRule,
}

impl Default for SvgStyle {
    fn default() -> Self {
        Self {
            fill: Some(SvgPaint::Solid(Color::BLACK)),
            stroke: None,
            stroke_width: 1.0,
            opacity: 1.0,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            fill_rule: FillRule::NonZero,
        }
    }
}

fn svg_number(document: &Document, node: NodeId, name: &str) -> Option<f32> {
    document
        .get_attribute(node, name)
        .and_then(|value| value.trim().trim_end_matches("px").parse::<f32>().ok())
}

/// Resolve an SVG paint value: `none`, a color, or `url(#id)`.
fn svg_paint(value: &str) -> Option<SvgPaint> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("url(") {
        let inner = rest.strip_suffix(')').unwrap_or(rest).trim();
        let inner = inner.trim_matches('"').trim_matches('\'');
        let id = inner.strip_prefix('#').unwrap_or(inner);
        if id.is_empty() {
            return None;
        }
        return Some(SvgPaint::Reference(id.to_owned()));
    }
    svg_color(value).map(SvgPaint::Solid)
}

/// Parse a gradient coordinate: plain numbers are used as-is for
/// `userSpaceOnUse` (or fractions for `objectBoundingBox`), percentages are
/// divided by 100.
fn svg_ratio(document: &Document, node: NodeId, name: &str, default: f32) -> f32 {
    let Some(value) = document.get_attribute(node, name) else {
        return default;
    };
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent.trim().parse::<f32>().unwrap_or(default * 100.0) / 100.0;
    }
    value
        .trim_end_matches("px")
        .parse::<f32>()
        .unwrap_or(default)
}

fn points_bounds(points: &[(f32, f32)]) -> Option<Rect> {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in points {
        min_x = min_x.min(*x);
        min_y = min_y.min(*y);
        max_x = max_x.max(*x);
        max_y = max_y.max(*y);
    }
    if !min_x.is_finite() || !max_x.is_finite() {
        return None;
    }
    Some(Rect {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(0.0),
        height: (max_y - min_y).max(0.0),
    })
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect {
        x,
        y,
        width: (right - x).max(0.0),
        height: (bottom - y).max(0.0),
    }
}

fn parse_view_box(value: &str) -> Option<(f32, f32, f32, f32)> {
    let values: Vec<f32> = value
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f32>().ok())
        .collect();
    match values.as_slice() {
        [x, y, width, height] => Some((*x, *y, *width, *height)),
        _ => None,
    }
}

fn parse_svg_transform(value: &str) -> SvgTransform {
    let mut transform = SvgTransform {
        tx: 0.0,
        ty: 0.0,
        sx: 1.0,
        sy: 1.0,
    };
    let mut rest = value.trim();
    while !rest.is_empty() {
        let Some(open) = rest.find('(') else {
            break;
        };
        let name = rest[..open].trim();
        let Some(close) = rest[open..].find(')').map(|close| open + close) else {
            break;
        };
        let numbers: Vec<f32> = rest[open + 1..close]
            .split(|character: char| character.is_ascii_whitespace() || character == ',')
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse::<f32>().ok())
            .collect();
        match name {
            "translate" => {
                transform.tx += numbers.first().copied().unwrap_or(0.0);
                transform.ty += numbers.get(1).copied().unwrap_or(0.0);
            }
            "scale" => {
                let sx = numbers.first().copied().unwrap_or(1.0);
                let sy = numbers.get(1).copied().unwrap_or(sx);
                transform.sx *= sx;
                transform.sy *= sy;
            }
            _ => {}
        }
        rest = rest[close + 1..].trim();
    }
    transform
}

fn parse_svg_points(value: &str) -> Vec<(f32, f32)> {
    let mut points = Vec::new();
    let mut numbers = Vec::new();
    for token in value.split(|character: char| character.is_ascii_whitespace() || character == ',')
    {
        if !token.is_empty()
            && let Ok(number) = token.parse::<f32>()
        {
            numbers.push(number);
        }
    }
    for pair in numbers.as_chunks::<2>().0 {
        points.push((pair[0], pair[1]));
    }
    points
}

fn svg_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if let Some(hex) = value.strip_prefix('#') {
        let digits: Vec<u8> = hex
            .chars()
            .filter_map(|c| c.to_digit(16).map(|d| d as u8))
            .collect();
        return match digits.as_slice() {
            [r, g, b] => Some(Color {
                r: r * 17,
                g: g * 17,
                b: b * 17,
                a: 255,
            }),
            [r1, r2, g1, g2, b1, b2] => Some(Color {
                r: r1 * 16 + r2,
                g: g1 * 16 + g2,
                b: b1 * 16 + b2,
                a: 255,
            }),
            [r1, r2, g1, g2, b1, b2, a1, a2] => Some(Color {
                r: r1 * 16 + r2,
                g: g1 * 16 + g2,
                b: b1 * 16 + b2,
                a: a1 * 16 + a2,
            }),
            _ => None,
        };
    }
    if let Some(inner) = value
        .strip_prefix("rgb(")
        .or_else(|| value.strip_prefix("rgba("))
        .and_then(|inner| inner.strip_suffix(')'))
    {
        let parts: Vec<f32> = inner
            .split(',')
            .filter_map(|part| part.trim().parse::<f32>().ok())
            .collect();
        if parts.len() >= 3 {
            let alpha = parts.get(3).copied().unwrap_or(1.0);
            return Some(Color {
                r: parts[0].clamp(0.0, 255.0) as u8,
                g: parts[1].clamp(0.0, 255.0) as u8,
                b: parts[2].clamp(0.0, 255.0) as u8,
                a: if alpha <= 1.0 {
                    (alpha * 255.0) as u8
                } else {
                    alpha.clamp(0.0, 255.0) as u8
                },
            });
        }
    }
    let color = match value.to_ascii_lowercase().as_str() {
        "black" => Color::BLACK,
        "white" => Color::WHITE,
        "red" => Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        },
        "green" => Color {
            r: 0,
            g: 128,
            b: 0,
            a: 255,
        },
        "blue" => Color {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        },
        "yellow" => Color {
            r: 255,
            g: 255,
            b: 0,
            a: 255,
        },
        "orange" => Color {
            r: 255,
            g: 165,
            b: 0,
            a: 255,
        },
        "purple" => Color {
            r: 128,
            g: 0,
            b: 128,
            a: 255,
        },
        "gray" | "grey" => Color {
            r: 128,
            g: 128,
            b: 128,
            a: 255,
        },
        "currentcolor" => Color::BLACK,
        _ => return None,
    };
    Some(color)
}

fn svg_path_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut number = String::new();
    let flush = |number: &mut String, tokens: &mut Vec<String>| {
        if !number.is_empty() {
            tokens.push(std::mem::take(number));
        }
    };
    for character in value.chars() {
        if character.is_ascii_alphabetic() {
            flush(&mut number, &mut tokens);
            tokens.push(character.to_string());
        } else if character.is_ascii_digit()
            || matches!(character, '.' | '-' | '+')
            || (matches!(character, 'e' | 'E') && !number.is_empty())
        {
            number.push(character);
        } else {
            flush(&mut number, &mut tokens);
        }
    }
    flush(&mut number, &mut tokens);
    tokens
}

fn svg_is_command(token: &str) -> bool {
    token.len() == 1
        && token
            .chars()
            .next()
            .is_some_and(|character| "MmLlHhVvCcSsQqTtAaZz".contains(character))
}

fn parse_svg_path(value: &str) -> Vec<Vec<(f32, f32)>> {
    let tokens = svg_path_tokens(value);
    let mut index = 0usize;
    let mut command = 'M';
    let mut current = (0.0f32, 0.0f32);
    let mut start = (0.0f32, 0.0f32);
    let mut subpaths: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut points: Vec<(f32, f32)> = Vec::new();
    let take = |index: &mut usize| -> Option<f32> {
        let token = tokens.get(*index)?;
        if svg_is_command(token) {
            return None;
        }
        *index += 1;
        token.parse::<f32>().ok()
    };
    while index < tokens.len() {
        if svg_is_command(&tokens[index]) {
            command = tokens[index].chars().next().unwrap_or('M');
            index += 1;
            if command == 'Z' || command == 'z' {
                if points.len() > 1 {
                    points.push(start);
                }
                if !points.is_empty() {
                    subpaths.push(std::mem::take(&mut points));
                }
                continue;
            }
            continue;
        }
        let relative = command.is_ascii_lowercase();
        let upper = command.to_ascii_uppercase();
        let (x, y) = current;
        match upper {
            'M' | 'L' => {
                let Some(mut nx) = take(&mut index) else {
                    break;
                };
                let Some(mut ny) = take(&mut index) else {
                    break;
                };
                if relative {
                    nx += x;
                    ny += y;
                }
                if upper == 'M' && !points.is_empty() {
                    subpaths.push(std::mem::take(&mut points));
                }
                if upper == 'M' {
                    start = (nx, ny);
                }
                points.push((nx, ny));
                current = (nx, ny);
                if upper == 'M' {
                    command = if relative { 'l' } else { 'L' };
                }
            }
            'H' => {
                let Some(mut nx) = take(&mut index) else {
                    break;
                };
                if relative {
                    nx += x;
                }
                points.push((nx, y));
                current = (nx, y);
            }
            'V' => {
                let Some(mut ny) = take(&mut index) else {
                    break;
                };
                if relative {
                    ny += y;
                }
                points.push((x, ny));
                current = (x, ny);
            }
            'C' => {
                let Some(mut c1x) = take(&mut index) else {
                    break;
                };
                let Some(mut c1y) = take(&mut index) else {
                    break;
                };
                let Some(mut c2x) = take(&mut index) else {
                    break;
                };
                let Some(mut c2y) = take(&mut index) else {
                    break;
                };
                let Some(mut ex) = take(&mut index) else {
                    break;
                };
                let Some(mut ey) = take(&mut index) else {
                    break;
                };
                if relative {
                    c1x += x;
                    c1y += y;
                    c2x += x;
                    c2y += y;
                    ex += x;
                    ey += y;
                }
                for step in 1..=12 {
                    let t = step as f32 / 12.0;
                    let inv = 1.0 - t;
                    let px = inv * inv * inv * x
                        + 3.0 * inv * inv * t * c1x
                        + 3.0 * inv * t * t * c2x
                        + t * t * t * ex;
                    let py = inv * inv * inv * y
                        + 3.0 * inv * inv * t * c1y
                        + 3.0 * inv * t * t * c2y
                        + t * t * t * ey;
                    points.push((px, py));
                }
                current = (ex, ey);
            }
            'Q' | 'S' => {
                let Some(mut c1x) = take(&mut index) else {
                    break;
                };
                let Some(mut c1y) = take(&mut index) else {
                    break;
                };
                let Some(mut ex) = take(&mut index) else {
                    break;
                };
                let Some(mut ey) = take(&mut index) else {
                    break;
                };
                if relative {
                    c1x += x;
                    c1y += y;
                    ex += x;
                    ey += y;
                }
                for step in 1..=10 {
                    let t = step as f32 / 10.0;
                    let inv = 1.0 - t;
                    let px = inv * inv * x + 2.0 * inv * t * c1x + t * t * ex;
                    let py = inv * inv * y + 2.0 * inv * t * c1y + t * t * ey;
                    points.push((px, py));
                }
                current = (ex, ey);
            }
            'A' => {
                let mut values = Vec::new();
                for _ in 0..7 {
                    let Some(value) = take(&mut index) else { break };
                    values.push(value);
                }
                if values.len() == 7 {
                    let (mut ex, mut ey) = (values[5], values[6]);
                    if relative {
                        ex += x;
                        ey += y;
                    }
                    points.push((ex, ey));
                    current = (ex, ey);
                }
            }
            _ => {
                index += 1;
            }
        }
    }
    if !points.is_empty() {
        subpaths.push(points);
    }
    subpaths
}

fn blend_pixel(pixel: &mut Rgba<u8>, src: [u8; 3], alpha: f32) {
    if alpha >= 0.999 {
        *pixel = Rgba([src[0], src[1], src[2], 255]);
        return;
    }
    let inv = 1.0 - alpha;
    let dst = pixel.0;
    pixel.0 = [
        (src[0] as f32 * alpha + dst[0] as f32 * inv).round() as u8,
        (src[1] as f32 * alpha + dst[1] as f32 * inv).round() as u8,
        (src[2] as f32 * alpha + dst[2] as f32 * inv).round() as u8,
        255,
    ];
}
