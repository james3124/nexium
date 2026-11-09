//! Display-list-free painter that rasterizes the layout tree into an RGBA
//! canvas using fontdue for glyphs and the `image` crate for decoded images.

use std::collections::HashMap;
use std::sync::Arc;

use image::{Rgba, RgbaImage, imageops};
use url::Url;

use crate::engine::css::{
    BackgroundRepeat, BackgroundSizeMode, BorderStyle, Color, FontStyle, Overflow, Position,
    TextDecoration, Viewport, Visibility,
};
use crate::engine::layout::{FragmentKind, LayoutBox, LayoutResult, LineBox, Rect};
use crate::engine::text::FontSet;

pub struct Painter<'a> {
    canvas: RgbaImage,
    fonts: &'a FontSet,
    images: &'a HashMap<String, Arc<RgbaImage>>,
    base_url: Option<&'a Url>,
    viewport: Viewport,
    clip: Vec<Rect>,
}

pub fn paint_document(
    layout: &LayoutResult,
    fonts: &FontSet,
    images: &HashMap<String, Arc<RgbaImage>>,
    base_url: Option<&Url>,
    width: u32,
    height: u32,
) -> RgbaImage {
    let viewport = layout.viewport;
    let mut painter = Painter {
        canvas: RgbaImage::from_pixel(width.max(1), height.max(1), Rgba([255, 255, 255, 255])),
        fonts,
        images,
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
            if let Some(image_url) = &box_.image
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
