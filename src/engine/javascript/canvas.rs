//! A focused 2D canvas implementation for `<canvas>` elements.
//!
//! Supports rectangles, paths, arcs, transforms, save/restore, solid and
//! gradient styles, text (`fillText`/`strokeText`/`measureText`) and image
//! drawing from other canvases (and `data:` URL `<img>` sources).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use base64::Engine as _;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction};
use image::{Rgba, RgbaImage};

use super::{document_rc, js_string_value, node_id_from_this, string_arg, with_state};
use crate::engine::css::{Color, ComputedStyle, FontStyle};
use crate::engine::dom::NodeId;
use crate::engine::text::FontSet;

const CANVAS_NODE_KEY: &str = "__nexium_canvas_node";
const GRADIENT_ID_KEY: &str = "__nexium_gradient_id";

/// A fill/stroke style: either a solid color or a live gradient.
#[derive(Clone)]
enum PaintStyle {
    Solid(Color),
    Gradient(Rc<RefCell<Gradient>>),
}

impl PaintStyle {
    #[allow(dead_code)]
    fn solid(&self) -> Color {
        match self {
            PaintStyle::Solid(color) => *color,
            PaintStyle::Gradient(gradient) => gradient.borrow().first_color(),
        }
    }
}

#[derive(Clone, Copy)]
enum GradientKind {
    Linear {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    },
    Radial {
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
    },
}

#[derive(Clone, Copy)]
struct GradientStop {
    offset: f32,
    color: Color,
}

struct Gradient {
    id: u64,
    kind: GradientKind,
    stops: Vec<GradientStop>,
}

impl Gradient {
    fn new(id: u64, kind: GradientKind) -> Self {
        Self {
            id,
            kind,
            stops: Vec::new(),
        }
    }

    fn add_stop(&mut self, offset: f32, color: Color) {
        self.stops.push(GradientStop {
            offset: offset.clamp(0.0, 1.0),
            color,
        });
        self.stops.sort_by(|a, b| {
            a.offset
                .partial_cmp(&b.offset)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    fn first_color(&self) -> Color {
        self.stops
            .first()
            .map(|stop| stop.color)
            .unwrap_or(Color::TRANSPARENT)
    }

    /// Sample the gradient at a point in user space.
    fn sample(&self, x: f32, y: f32) -> Color {
        let t = match self.kind {
            GradientKind::Linear { x0, y0, x1, y1 } => {
                let dx = x1 - x0;
                let dy = y1 - y0;
                let len2 = dx * dx + dy * dy;
                if len2 <= f32::EPSILON {
                    0.0
                } else {
                    ((x - x0) * dx + (y - y0) * dy) / len2
                }
            }
            GradientKind::Radial {
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
            } => {
                let _ = (x0, y0, r0);
                let dx = x - x1;
                let dy = y - y1;
                let distance = (dx * dx + dy * dy).sqrt();
                let span = r1 - r0;
                if span.abs() <= f32::EPSILON {
                    0.0
                } else {
                    (distance - r0) / span
                }
            }
        };
        self.color_at(t)
    }

    /// Interpolate color stops at parameter `t` in sRGB space.
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
                return lerp_color(a.color, b.color, f);
            }
        }
        self.stops
            .last()
            .map(|stop| stop.color)
            .unwrap_or(first.color)
    }
}

fn lerp_color(a: Color, b: Color, f: f32) -> Color {
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

#[derive(Default)]
struct GradientRegistry {
    next_id: u64,
    entries: HashMap<u64, Rc<RefCell<Gradient>>>,
}

thread_local! {
    static GRADIENTS: RefCell<GradientRegistry> = RefCell::new(GradientRegistry { next_id: 1, entries: HashMap::new() });
}

fn with_fonts<R>(f: impl FnOnce(&FontSet) -> R) -> R {
    f(FontSet::embedded())
}

fn register_gradient(kind: GradientKind) -> Rc<RefCell<Gradient>> {
    GRADIENTS.with(|registry| {
        let mut registry = registry.borrow_mut();
        let id = registry.next_id;
        registry.next_id += 1;
        let gradient = Rc::new(RefCell::new(Gradient::new(id, kind)));
        registry.entries.insert(id, Rc::clone(&gradient));
        gradient
    })
}

fn lookup_gradient(id: u64) -> Option<Rc<RefCell<Gradient>>> {
    GRADIENTS.with(|registry| registry.borrow().entries.get(&id).cloned())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextAlign {
    Start,
    Center,
    End,
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextBaseline {
    Alphabetic,
    Top,
    Hanging,
    Middle,
    Bottom,
    Ideographic,
}

#[derive(Clone)]
struct CanvasFont {
    size: f32,
    weight: u16,
    italic: bool,
    family: String,
    source: String,
}

impl Default for CanvasFont {
    fn default() -> Self {
        Self {
            size: 10.0,
            weight: 400,
            italic: false,
            family: "sans-serif".to_owned(),
            source: "10px sans-serif".to_owned(),
        }
    }
}

impl CanvasFont {
    fn style(&self) -> ComputedStyle {
        let mut style = ComputedStyle::initial();
        style.font_size = self.size.max(1.0);
        style.font_weight = self.weight;
        style.font_style = if self.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        style.font_family = self.family.clone();
        style
    }
}

#[derive(Clone)]
pub(crate) struct CanvasState {
    pub width: u32,
    pub height: u32,
    pub image: RgbaImage,
    fill: PaintStyle,
    stroke: PaintStyle,
    line_width: f32,
    alpha: f32,
    path: Vec<(f32, f32)>,
    subpaths: Vec<Vec<(f32, f32)>>,
    transform: (f32, f32, f32, f32),
    saved: Vec<SavedState>,
    font: CanvasFont,
    text_align: TextAlign,
    text_baseline: TextBaseline,
}

#[derive(Clone)]
struct SavedState {
    fill: PaintStyle,
    stroke: PaintStyle,
    line_width: f32,
    alpha: f32,
    transform: (f32, f32, f32, f32),
    font: CanvasFont,
    text_align: TextAlign,
    text_baseline: TextBaseline,
}

struct TextMetrics {
    width: f32,
    actual_ascent: f32,
    actual_descent: f32,
    font_ascent: f32,
    font_descent: f32,
}

impl CanvasState {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            image: RgbaImage::from_pixel(width.max(1), height.max(1), Rgba([0, 0, 0, 0])),
            fill: PaintStyle::Solid(Color::BLACK),
            stroke: PaintStyle::Solid(Color::BLACK),
            line_width: 1.0,
            alpha: 1.0,
            path: Vec::new(),
            subpaths: Vec::new(),
            transform: (0.0, 0.0, 1.0, 1.0),
            saved: Vec::new(),
            font: CanvasFont::default(),
            text_align: TextAlign::Start,
            text_baseline: TextBaseline::Alphabetic,
        }
    }

    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let (tx, ty, sx, sy) = self.transform;
        (tx + x * sx, ty + y * sy)
    }

    fn unmap(&self, x: f32, y: f32) -> (f32, f32) {
        let (tx, ty, sx, sy) = self.transform;
        let sx = if sx.abs() < 1e-6 { 1.0 } else { sx };
        let sy = if sy.abs() < 1e-6 { 1.0 } else { sy };
        ((x - tx) / sx, (y - ty) / sy)
    }

    /// Resolve the color of a paint style at a device-space pixel center.
    fn paint_at(&self, paint: &PaintStyle, px: f32, py: f32) -> Color {
        match paint {
            PaintStyle::Solid(color) => *color,
            PaintStyle::Gradient(gradient) => {
                let (ux, uy) = self.unmap(px, py);
                gradient.borrow().sample(ux, uy)
            }
        }
    }

    fn blend(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let alpha = (color.a as f32 / 255.0) * coverage * self.alpha;
        if alpha <= 0.0 {
            return;
        }
        let pixel = self.image.get_pixel_mut(x as u32, y as u32);
        let inv = 1.0 - alpha;
        pixel.0 = [
            (color.r as f32 * alpha + pixel.0[0] as f32 * inv).round() as u8,
            (color.g as f32 * alpha + pixel.0[1] as f32 * inv).round() as u8,
            (color.b as f32 * alpha + pixel.0[2] as f32 * inv).round() as u8,
            ((pixel.0[3] as f32 * inv + 255.0 * alpha).min(255.0)) as u8,
        ];
    }

    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, paint: &PaintStyle) {
        let (x0, y0) = self.map(x, y);
        let (x1, y1) = self.map(x + width, y + height);
        let left = x0.min(x1).floor() as i32;
        let right = x0.max(x1).ceil() as i32;
        let top = y0.min(y1).floor() as i32;
        let bottom = y0.max(y1).ceil() as i32;
        for py in top..bottom {
            for px in left..right {
                let color = self.paint_at(paint, px as f32 + 0.5, py as f32 + 0.5);
                self.blend(px, py, color, 1.0);
            }
        }
    }

    fn stroke_rect(&mut self, x: f32, y: f32, width: f32, height: f32, paint: &PaintStyle) {
        let line = self.line_width.max(1.0);
        self.fill_rect(x, y, width, line, paint);
        self.fill_rect(x, y + height - line, width, line, paint);
        self.fill_rect(x, y, line, height, paint);
        self.fill_rect(x + width - line, y, line, height, paint);
    }

    fn clear_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let (x0, y0) = self.map(x, y);
        let (x1, y1) = self.map(x + width, y + height);
        for py in y0.min(y1).floor() as i32..y0.max(y1).ceil() as i32 {
            for px in x0.min(x1).floor() as i32..x0.max(x1).ceil() as i32 {
                if px >= 0 && py >= 0 && px < self.width as i32 && py < self.height as i32 {
                    self.image
                        .put_pixel(px as u32, py as u32, Rgba([0, 0, 0, 0]));
                }
            }
        }
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, paint: &PaintStyle) {
        let (ax, ay) = self.map(x1, y1);
        let (bx, by) = self.map(x2, y2);
        let dx = bx - ax;
        let dy = by - ay;
        let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as i32;
        let radius = (self.line_width.max(1.0) / 2.0).round() as i32;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            let cx = (ax + dx * t).round() as i32;
            let cy = (ay + dy * t).round() as i32;
            for oy in -radius..=radius {
                for ox in -radius..=radius {
                    if ox * ox + oy * oy <= radius * radius {
                        let color =
                            self.paint_at(paint, (cx + ox) as f32 + 0.5, (cy + oy) as f32 + 0.5);
                        self.blend(cx + ox, cy + oy, color, 1.0);
                    }
                }
            }
        }
    }

    fn fill_polygon(&mut self, points: &[(f32, f32)], paint: &PaintStyle) {
        if points.len() < 3 {
            return;
        }
        let mapped: Vec<(f32, f32)> = points.iter().map(|(x, y)| self.map(*x, *y)).collect();
        let min_y = mapped.iter().map(|(_, y)| *y).fold(f32::INFINITY, f32::min);
        let max_y = mapped
            .iter()
            .map(|(_, y)| *y)
            .fold(f32::NEG_INFINITY, f32::max);
        for y in min_y.floor() as i32..max_y.ceil() as i32 {
            let scan = y as f32 + 0.5;
            let mut crossings = Vec::new();
            for pair in mapped
                .iter()
                .zip(mapped.iter().cycle().skip(1))
                .take(mapped.len())
            {
                let (x1, y1) = *pair.0;
                let (x2, y2) = *pair.1;
                if (y1 <= scan && y2 > scan) || (y2 <= scan && y1 > scan) {
                    crossings.push(x1 + (scan - y1) / (y2 - y1) * (x2 - x1));
                }
            }
            crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            for pair in crossings.chunks(2) {
                if let [left, right] = pair {
                    for x in (*left).floor() as i32..(*right).ceil() as i32 {
                        let color = self.paint_at(paint, x as f32 + 0.5, scan);
                        self.blend(x, y, color, 1.0);
                    }
                }
            }
        }
    }

    fn stroke_polyline(&mut self, points: &[(f32, f32)], paint: &PaintStyle) {
        for pair in points.windows(2) {
            self.line(pair[0].0, pair[0].1, pair[1].0, pair[1].1, paint);
        }
    }

    fn arc(&mut self, cx: f32, cy: f32, radius: f32, start: f32, end: f32) {
        let steps = ((radius.abs() * 4.0) as usize).clamp(12, 256);
        for step in 0..=steps {
            let angle = start + (end - start) * step as f32 / steps as f32;
            self.path
                .push((cx + angle.cos() * radius, cy + angle.sin() * radius));
        }
    }

    fn flush_path(&mut self) -> Vec<Vec<(f32, f32)>> {
        let mut subpaths = std::mem::take(&mut self.subpaths);
        if self.path.len() > 1 {
            subpaths.push(std::mem::take(&mut self.path));
        } else {
            self.path.clear();
        }
        subpaths
    }

    fn measure(&self, fonts: &FontSet, text: &str) -> TextMetrics {
        let style = self.font.style();
        let font = fonts.font_for(&style);
        let size = style.font_size;
        let mut width = 0.0f32;
        let mut actual_ascent = 0.0f32;
        let mut actual_descent = 0.0f32;
        let mut previous: Option<char> = None;
        for ch in text.chars() {
            if ch == '\t' {
                width += font.metrics(' ', size).advance_width * 4.0;
                previous = None;
                continue;
            }
            if let Some(prev) = previous
                && let Some(kern) = font.horizontal_kern(prev, ch, size)
            {
                width += kern;
            }
            previous = Some(ch);
            let metrics = font.metrics(ch, size);
            width += metrics.advance_width;
            if metrics.height > 0 {
                actual_ascent = actual_ascent.max(metrics.ymin as f32 + metrics.height as f32);
                actual_descent = actual_descent.max(-(metrics.ymin as f32));
            }
        }
        let line = font.horizontal_line_metrics(size);
        let font_ascent = line.map(|metrics| metrics.ascent).unwrap_or(size * 0.8);
        let font_descent = line
            .map(|metrics| metrics.descent.abs())
            .unwrap_or(size * 0.2);
        TextMetrics {
            width,
            actual_ascent,
            actual_descent,
            font_ascent,
            font_descent,
        }
    }

    fn draw_text(
        &mut self,
        fonts: &FontSet,
        text: &str,
        x: f32,
        y: f32,
        paint: &PaintStyle,
        outlined: bool,
    ) {
        let style = self.font.style();
        let font = fonts.font_for(&style);
        let sy = self.transform.3.abs().max(0.01);
        let sx = self.transform.2.abs().max(0.01);
        let size = style.font_size * sy;
        let line = font.horizontal_line_metrics(size);
        let ascent = line.map(|metrics| metrics.ascent).unwrap_or(size * 0.8);
        let descent = line
            .map(|metrics| metrics.descent.abs())
            .unwrap_or(size * 0.2);
        let (dx, dy) = self.map(x, y);
        let baseline = dy
            + match self.text_baseline {
                TextBaseline::Alphabetic | TextBaseline::Ideographic => 0.0,
                TextBaseline::Top | TextBaseline::Hanging => ascent,
                TextBaseline::Middle => (ascent - descent) / 2.0,
                TextBaseline::Bottom => -descent,
            };
        let text_width = self.measure(fonts, text).width * sy;
        let start = dx
            - match self.text_align {
                TextAlign::Start | TextAlign::Left => 0.0,
                TextAlign::Center => text_width / 2.0,
                TextAlign::End | TextAlign::Right => text_width,
            };
        let horizontal_scale = (sx / sy).clamp(0.05, 20.0);
        let shear = if self.font.italic { 0.2 } else { 0.0 };
        let mut pen = start;
        let mut previous: Option<char> = None;
        for ch in text.chars() {
            if ch == '\t' {
                pen += font.metrics(' ', size).advance_width * 4.0 * horizontal_scale;
                previous = None;
                continue;
            }
            if let Some(prev) = previous
                && let Some(kern) = font.horizontal_kern(prev, ch, size)
            {
                pen += kern * horizontal_scale;
            }
            previous = Some(ch);
            let (metrics, bitmap) = font.rasterize(ch, size);
            if ch.is_whitespace() || metrics.width == 0 || metrics.height == 0 {
                pen += metrics.advance_width * horizontal_scale;
                continue;
            }
            let gx = pen + metrics.xmin as f32 * horizontal_scale;
            let gy = baseline - (metrics.height as f32 + metrics.ymin as f32);
            let dilation = if outlined {
                (self.line_width * sy / 2.0).max(0.5)
            } else {
                0.0
            };
            self.blit_glyph(
                &bitmap,
                metrics.width,
                metrics.height,
                gx,
                gy,
                paint,
                shear,
                dilation,
            );
            pen += metrics.advance_width * horizontal_scale;
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
        paint: &PaintStyle,
        shear: f32,
        dilation: f32,
    ) {
        for row in 0..height {
            let row_y = y + row as f32;
            let row_x = x + shear * (height - row) as f32;
            for col in 0..width {
                let coverage = bitmap[row * width + col];
                if coverage == 0 {
                    continue;
                }
                let base_alpha = coverage as f32 / 255.0;
                let gx = row_x + col as f32;
                if dilation <= 0.0 {
                    let color = self.paint_at(paint, gx + 0.5, row_y + 0.5);
                    self.blend(gx.round() as i32, row_y.round() as i32, color, base_alpha);
                    continue;
                }
                // Approximate glyph outlines by dilating the coverage mask.
                let radius = dilation.ceil() as i32;
                for oy in -radius..=radius {
                    for ox in -radius..=radius {
                        if ox * ox + oy * oy > radius * radius {
                            continue;
                        }
                        let px = gx.round() as i32 + ox;
                        let py = row_y.round() as i32 + oy;
                        let color = self.paint_at(paint, px as f32 + 0.5, py as f32 + 0.5);
                        self.blend(px, py, color, base_alpha * 0.7);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_image(
        &mut self,
        source: &RgbaImage,
        sx: f32,
        sy: f32,
        sw: f32,
        sh: f32,
        dx: f32,
        dy: f32,
        dw: f32,
        dh: f32,
    ) {
        if source.width() == 0 || source.height() == 0 || sw <= 0.0 || sh <= 0.0 {
            return;
        }
        if dw <= 0.0 || dh <= 0.0 {
            return;
        }
        let (x0, y0) = self.map(dx, dy);
        let (x1, y1) = self.map(dx + dw, dy + dh);
        let left = x0.min(x1);
        let right = x0.max(x1);
        let top = y0.min(y1);
        let bottom = y0.max(y1);
        let span_x = (right - left).max(f32::EPSILON);
        let span_y = (bottom - top).max(f32::EPSILON);
        let px_left = left.floor().max(0.0) as u32;
        let px_right = right.ceil().min(self.width as f32) as u32;
        let px_top = top.floor().max(0.0) as u32;
        let px_bottom = bottom.ceil().min(self.height as f32) as u32;
        for py in px_top..px_bottom {
            let v = (py as f32 + 0.5 - top) / span_y;
            let src_y = sy + v * sh;
            let src_row = (src_y.floor().max(0.0) as u32).min(source.height() - 1);
            for px in px_left..px_right {
                let u = (px as f32 + 0.5 - left) / span_x;
                let src_x = sx + u * sw;
                if src_x < 0.0 || src_y < 0.0 {
                    continue;
                }
                let src_col = (src_x.floor() as u32).min(source.width() - 1);
                let pixel = *source.get_pixel(src_col, src_row);
                if pixel[3] == 0 {
                    continue;
                }
                let alpha = pixel[3] as f32 / 255.0;
                self.blend(
                    px as i32,
                    py as i32,
                    Color::rgba(pixel[0], pixel[1], pixel[2], 255),
                    alpha,
                );
            }
        }
    }
}

pub(crate) fn install_globals(context: &mut Context) -> JsResult<()> {
    let _ = context;
    Ok(())
}

pub(crate) fn js_get_context(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(node) = node_id_from_this(this, context) else {
        return Ok(JsValue::null());
    };
    let Some(document) = document_rc() else {
        return Ok(JsValue::null());
    };
    if document.borrow().tag_name(node) != Some("canvas") {
        return Ok(JsValue::null());
    }
    let kind = string_arg(args, 0, context).unwrap_or_default();
    if kind != "2d" {
        return Ok(JsValue::null());
    }
    ensure_canvas(node);
    build_context(node, context).map(JsValue::from)
}

fn ensure_canvas(node: NodeId) {
    let exists = with_state(|state| state.canvases.contains_key(&node)).unwrap_or(false);
    if exists {
        return;
    }
    let Some(document) = document_rc() else {
        return;
    };
    let (width, height) = {
        let document = document.borrow();
        let width = document
            .get_attribute(node, "width")
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(300)
            .clamp(1, 4096);
        let height = document
            .get_attribute(node, "height")
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(150)
            .clamp(1, 4096);
        (width, height)
    };
    with_state(|state| {
        state.canvases.insert(node, CanvasState::new(width, height));
    });
}

fn gradient_object(id: u64, context: &mut Context) -> JsObject {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from(GRADIENT_ID_KEY),
        JsValue::from(id as f64),
        Attribute::empty(),
    );
    object.property(
        JsString::from("addColorStop"),
        NativeFunction::from_fn_ptr(js_add_color_stop).to_js_function(&realm),
        Attribute::all(),
    );
    object.build()
}

fn build_context(node: NodeId, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from(CANVAS_NODE_KEY),
        JsValue::from(node.index() as f64),
        Attribute::empty(),
    );
    for (name, function) in [
        (
            "fillRect",
            js_fill_rect as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
        ),
        ("strokeRect", js_stroke_rect),
        ("clearRect", js_clear_rect),
        ("beginPath", js_begin_path),
        ("moveTo", js_move_to),
        ("lineTo", js_line_to),
        ("closePath", js_close_path),
        ("rect", js_rect),
        ("arc", js_arc),
        ("fill", js_fill),
        ("stroke", js_stroke),
        ("save", js_save),
        ("restore", js_restore),
        ("translate", js_translate),
        ("scale", js_scale),
        ("setTransform", js_set_transform),
        ("fillText", js_fill_text),
        ("strokeText", js_stroke_text),
        ("measureText", js_measure_text),
        ("createLinearGradient", js_create_linear_gradient),
        ("createRadialGradient", js_create_radial_gradient),
        ("drawImage", js_draw_image),
        ("toDataURL", js_to_data_url),
    ] {
        object.property(
            JsString::from(name),
            NativeFunction::from_fn_ptr(function).to_js_function(&realm),
            Attribute::all(),
        );
    }
    let accessors: [(
        &str,
        boa_engine::native_function::NativeFunctionPointer,
        boa_engine::native_function::NativeFunctionPointer,
    ); 6] = [
        ("fillStyle", js_get_fill_style, js_set_fill_style),
        ("strokeStyle", js_get_stroke_style, js_set_stroke_style),
        ("lineWidth", js_get_line_width, js_set_line_width),
        ("globalAlpha", js_get_alpha, js_set_alpha),
        ("font", js_get_font, js_set_font),
        ("textAlign", js_get_text_align, js_set_text_align),
    ];
    for (name, get, set) in accessors {
        object.accessor(
            JsString::from(name),
            Some(NativeFunction::from_fn_ptr(get).to_js_function(&realm)),
            Some(NativeFunction::from_fn_ptr(set).to_js_function(&realm)),
            Attribute::all(),
        );
    }
    object.accessor(
        JsString::from("textBaseline"),
        Some(NativeFunction::from_fn_ptr(js_get_text_baseline).to_js_function(&realm)),
        Some(NativeFunction::from_fn_ptr(js_set_text_baseline).to_js_function(&realm)),
        Attribute::all(),
    );
    Ok(object.build())
}

fn canvas_node(this: &JsValue, context: &mut Context) -> Option<NodeId> {
    let object = this.as_object()?;
    let value = object.get(JsString::from(CANVAS_NODE_KEY), context).ok()?;
    let index = value.as_number()?;
    Some(NodeId::from_index(index as usize))
}

fn with_canvas<R>(
    this: &JsValue,
    context: &mut Context,
    f: impl FnOnce(&mut CanvasState) -> R,
) -> Option<R> {
    let node = canvas_node(this, context)?;
    with_state(|state| state.canvases.get_mut(&node).map(f)).flatten()
}

fn number_arg(args: &[JsValue], index: usize) -> Option<f32> {
    args.get(index)
        .and_then(JsValue::as_number)
        .map(|value| value as f32)
}

fn js_fill_rect(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(x), Some(y), Some(width), Some(height)) = (
        number_arg(args, 0),
        number_arg(args, 1),
        number_arg(args, 2),
        number_arg(args, 3),
    ) {
        with_canvas(this, context, |canvas| {
            let fill = canvas.fill.clone();
            canvas.fill_rect(x, y, width, height, &fill);
        });
    }
    Ok(JsValue::undefined())
}

fn js_stroke_rect(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(x), Some(y), Some(width), Some(height)) = (
        number_arg(args, 0),
        number_arg(args, 1),
        number_arg(args, 2),
        number_arg(args, 3),
    ) {
        with_canvas(this, context, |canvas| {
            let stroke = canvas.stroke.clone();
            canvas.stroke_rect(x, y, width, height, &stroke);
        });
    }
    Ok(JsValue::undefined())
}

fn js_clear_rect(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(x), Some(y), Some(width), Some(height)) = (
        number_arg(args, 0),
        number_arg(args, 1),
        number_arg(args, 2),
        number_arg(args, 3),
    ) {
        with_canvas(this, context, |canvas| {
            canvas.clear_rect(x, y, width, height)
        });
    }
    Ok(JsValue::undefined())
}

fn js_begin_path(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_canvas(this, context, |canvas| {
        canvas.path.clear();
        canvas.subpaths.clear();
    });
    Ok(JsValue::undefined())
}

fn js_move_to(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(x), Some(y)) = (number_arg(args, 0), number_arg(args, 1)) {
        with_canvas(this, context, |canvas| {
            if canvas.path.len() > 1 {
                canvas.subpaths.push(std::mem::take(&mut canvas.path));
            } else {
                canvas.path.clear();
            }
            canvas.path.push((x, y));
        });
    }
    Ok(JsValue::undefined())
}

fn js_line_to(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(x), Some(y)) = (number_arg(args, 0), number_arg(args, 1)) {
        with_canvas(this, context, |canvas| canvas.path.push((x, y)));
    }
    Ok(JsValue::undefined())
}

fn js_close_path(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_canvas(this, context, |canvas| {
        if let (Some(first), Some(last)) =
            (canvas.path.first().copied(), canvas.path.last().copied())
            && first != last
        {
            canvas.path.push(first);
        }
    });
    Ok(JsValue::undefined())
}

fn js_rect(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(x), Some(y), Some(width), Some(height)) = (
        number_arg(args, 0),
        number_arg(args, 1),
        number_arg(args, 2),
        number_arg(args, 3),
    ) {
        with_canvas(this, context, |canvas| {
            if canvas.path.len() > 1 {
                canvas.subpaths.push(std::mem::take(&mut canvas.path));
            } else {
                canvas.path.clear();
            }
            canvas.path.extend([
                (x, y),
                (x + width, y),
                (x + width, y + height),
                (x, y + height),
                (x, y),
            ]);
        });
    }
    Ok(JsValue::undefined())
}

fn js_arc(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(cx), Some(cy), Some(radius), Some(start), Some(end)) = (
        number_arg(args, 0),
        number_arg(args, 1),
        number_arg(args, 2),
        number_arg(args, 3),
        number_arg(args, 4),
    ) {
        with_canvas(this, context, |canvas| {
            canvas.arc(cx, cy, radius, start, end)
        });
    }
    Ok(JsValue::undefined())
}

fn js_fill(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_canvas(this, context, |canvas| {
        let paths = canvas.flush_path();
        let fill = canvas.fill.clone();
        for path in paths {
            canvas.fill_polygon(&path, &fill);
        }
    });
    Ok(JsValue::undefined())
}

fn js_stroke(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_canvas(this, context, |canvas| {
        let paths = canvas.flush_path();
        let stroke = canvas.stroke.clone();
        for path in paths {
            canvas.stroke_polyline(&path, &stroke);
        }
    });
    Ok(JsValue::undefined())
}

fn js_save(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_canvas(this, context, |canvas| {
        canvas.saved.push(SavedState {
            fill: canvas.fill.clone(),
            stroke: canvas.stroke.clone(),
            line_width: canvas.line_width,
            alpha: canvas.alpha,
            transform: canvas.transform,
            font: canvas.font.clone(),
            text_align: canvas.text_align,
            text_baseline: canvas.text_baseline,
        });
    });
    Ok(JsValue::undefined())
}

fn js_restore(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_canvas(this, context, |canvas| {
        if let Some(saved) = canvas.saved.pop() {
            canvas.fill = saved.fill;
            canvas.stroke = saved.stroke;
            canvas.line_width = saved.line_width;
            canvas.alpha = saved.alpha;
            canvas.transform = saved.transform;
            canvas.font = saved.font;
            canvas.text_align = saved.text_align;
            canvas.text_baseline = saved.text_baseline;
        }
    });
    Ok(JsValue::undefined())
}

fn js_translate(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(x), Some(y)) = (number_arg(args, 0), number_arg(args, 1)) {
        with_canvas(this, context, |canvas| {
            canvas.transform.0 += x * canvas.transform.2;
            canvas.transform.1 += y * canvas.transform.3;
        });
    }
    Ok(JsValue::undefined())
}

fn js_scale(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let (Some(x), Some(y)) = (number_arg(args, 0), number_arg(args, 1)) {
        with_canvas(this, context, |canvas| {
            canvas.transform.2 *= x;
            canvas.transform.3 *= y;
        });
    }
    Ok(JsValue::undefined())
}

fn js_set_transform(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if args.len() >= 6 {
        let values: Vec<f32> = args
            .iter()
            .filter_map(|value| value.as_number().map(|n| n as f32))
            .collect();
        if values.len() >= 6 {
            with_canvas(this, context, |canvas| {
                canvas.transform = (values[4], values[5], values[0], values[3]);
            });
        }
    }
    Ok(JsValue::undefined())
}

fn js_fill_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = string_arg(args, 0, context).unwrap_or_default();
    if let (Some(x), Some(y)) = (number_arg(args, 1), number_arg(args, 2)) {
        with_canvas(this, context, |canvas| {
            let fill = canvas.fill.clone();
            with_fonts(|fonts| canvas.draw_text(fonts, &text, x, y, &fill, false));
        });
    }
    Ok(JsValue::undefined())
}

fn js_stroke_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = string_arg(args, 0, context).unwrap_or_default();
    if let (Some(x), Some(y)) = (number_arg(args, 1), number_arg(args, 2)) {
        with_canvas(this, context, |canvas| {
            let stroke = canvas.stroke.clone();
            with_fonts(|fonts| canvas.draw_text(fonts, &text, x, y, &stroke, true));
        });
    }
    Ok(JsValue::undefined())
}

fn js_measure_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = string_arg(args, 0, context).unwrap_or_default();
    let metrics = with_canvas(this, context, |canvas| {
        with_fonts(|fonts| {
            let metrics = canvas.measure(fonts, &text);
            (
                metrics.width,
                metrics.actual_ascent,
                metrics.actual_descent,
                metrics.font_ascent,
                metrics.font_descent,
            )
        })
    });
    let (width, ascent, descent, font_ascent, font_descent) =
        metrics.unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from("width"),
        JsValue::from(width as f64),
        Attribute::all(),
    );
    object.property(
        JsString::from("actualBoundingBoxAscent"),
        JsValue::from(ascent as f64),
        Attribute::all(),
    );
    object.property(
        JsString::from("actualBoundingBoxDescent"),
        JsValue::from(descent as f64),
        Attribute::all(),
    );
    object.property(
        JsString::from("actualBoundingBoxLeft"),
        JsValue::from(0.0),
        Attribute::all(),
    );
    object.property(
        JsString::from("actualBoundingBoxRight"),
        JsValue::from(width as f64),
        Attribute::all(),
    );
    object.property(
        JsString::from("fontBoundingBoxAscent"),
        JsValue::from(font_ascent as f64),
        Attribute::all(),
    );
    object.property(
        JsString::from("fontBoundingBoxDescent"),
        JsValue::from(font_descent as f64),
        Attribute::all(),
    );
    Ok(JsValue::from(object.build()))
}

fn js_create_linear_gradient(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let x0 = number_arg(args, 0).unwrap_or(0.0);
    let y0 = number_arg(args, 1).unwrap_or(0.0);
    let x1 = number_arg(args, 2).unwrap_or(0.0);
    let y1 = number_arg(args, 3).unwrap_or(0.0);
    let gradient = register_gradient(GradientKind::Linear { x0, y0, x1, y1 });
    let id = gradient.borrow().id;
    Ok(JsValue::from(gradient_object(id, context)))
}

fn js_create_radial_gradient(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let x0 = number_arg(args, 0).unwrap_or(0.0);
    let y0 = number_arg(args, 1).unwrap_or(0.0);
    let r0 = number_arg(args, 2).unwrap_or(0.0).max(0.0);
    let x1 = number_arg(args, 3).unwrap_or(0.0);
    let y1 = number_arg(args, 4).unwrap_or(0.0);
    let r1 = number_arg(args, 5).unwrap_or(0.0).max(0.0);
    let gradient = register_gradient(GradientKind::Radial {
        x0,
        y0,
        r0,
        x1,
        y1,
        r1,
    });
    let id = gradient.borrow().id;
    Ok(JsValue::from(gradient_object(id, context)))
}

fn js_add_color_stop(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    let Ok(id_value) = object.get(JsString::from(GRADIENT_ID_KEY), context) else {
        return Ok(JsValue::undefined());
    };
    let Some(id) = id_value.as_number() else {
        return Ok(JsValue::undefined());
    };
    let Some(offset) = number_arg(args, 0) else {
        return Ok(JsValue::undefined());
    };
    let Some(color) = string_arg(args, 1, context).and_then(|value| parse_color(&value)) else {
        return Ok(JsValue::undefined());
    };
    if let Some(gradient) = lookup_gradient(id as u64) {
        gradient.borrow_mut().add_stop(offset, color);
    }
    Ok(JsValue::undefined())
}

fn image_from_source(value: &JsValue, context: &mut Context) -> Option<RgbaImage> {
    let object = value.as_object()?;
    let node = if let Ok(nested) = object.get(JsString::from(CANVAS_NODE_KEY), context)
        && let Some(index) = nested.as_number()
    {
        NodeId::from_index(index as usize)
    } else {
        super::node_id_from_object(&object, context)?
    };
    if let Some(image) =
        with_state(|state| state.canvases.get(&node).map(|c| c.image.clone())).flatten()
    {
        return Some(image);
    }
    let document = document_rc()?;
    let (tag, src) = {
        let document = document.borrow();
        (
            document.tag_name(node).map(str::to_owned),
            document.get_attribute(node, "src").map(str::to_owned),
        )
    };
    if tag.as_deref() == Some("img") {
        let src = src?;
        if let Some(image) = decode_data_url_image(&src) {
            return Some(image);
        }
        let client = with_state(|state| state.client.clone()).flatten();
        let base = with_state(|state| state.base_url.clone())?;
        let url = base.join(&src).ok()?;
        let bytes = super::fetch::fetch_bytes_sync(url, client).ok()?;
        return image::load_from_memory(&bytes)
            .ok()
            .map(|image| image.to_rgba8());
    }
    None
}

fn decode_data_url_image(src: &str) -> Option<RgbaImage> {
    let src = src.trim();
    let comma = src.find(',')?;
    let meta = &src[..comma];
    if !meta.starts_with("data:") || !meta.to_ascii_lowercase().contains(";base64") {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&src[comma + 1..])
        .ok()?;
    image::load_from_memory(&bytes)
        .ok()
        .map(|image| image.to_rgba8())
}

fn js_draw_image(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(source) = args
        .first()
        .and_then(|value| image_from_source(value, context))
    else {
        return Ok(JsValue::undefined());
    };
    let source_width = source.width() as f32;
    let source_height = source.height() as f32;
    let (sx, sy, sw, sh, dx, dy, dw, dh) = if args.len() >= 9 {
        let sx = number_arg(args, 1).unwrap_or(0.0);
        let sy = number_arg(args, 2).unwrap_or(0.0);
        let sw = number_arg(args, 3).unwrap_or(source_width).max(0.0);
        let sh = number_arg(args, 4).unwrap_or(source_height).max(0.0);
        (
            sx,
            sy,
            sw,
            sh,
            number_arg(args, 5).unwrap_or(0.0),
            number_arg(args, 6).unwrap_or(0.0),
            number_arg(args, 7).unwrap_or(sw),
            number_arg(args, 8).unwrap_or(sh),
        )
    } else if args.len() >= 5 {
        (
            0.0,
            0.0,
            source_width,
            source_height,
            number_arg(args, 1).unwrap_or(0.0),
            number_arg(args, 2).unwrap_or(0.0),
            number_arg(args, 3).unwrap_or(source_width),
            number_arg(args, 4).unwrap_or(source_height),
        )
    } else {
        (
            0.0,
            0.0,
            source_width,
            source_height,
            number_arg(args, 1).unwrap_or(0.0),
            number_arg(args, 2).unwrap_or(0.0),
            source_width,
            source_height,
        )
    };
    with_canvas(this, context, |canvas| {
        canvas.draw_image(&source, sx, sy, sw, sh, dx, dy, dw, dh);
    });
    Ok(JsValue::undefined())
}

pub(crate) fn js_element_to_data_url(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let kind = string_arg(args, 0, context).unwrap_or_else(|| "image/png".to_owned());
    let Some(node) = node_id_from_this(this, context) else {
        return Ok(js_string_value("data:,"));
    };
    let image =
        with_state(|state| state.canvases.get(&node).map(|canvas| canvas.image.clone())).flatten();
    let Some(image) = image else {
        return Ok(js_string_value("data:,"));
    };
    Ok(js_string_value(&encode_data_url(&kind, &image)))
}

fn js_to_data_url(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = string_arg(args, 0, context).unwrap_or_else(|| "image/png".to_owned());
    let Some(node) = canvas_node(this, context) else {
        return Ok(js_string_value("data:,"));
    };
    let image =
        with_state(|state| state.canvases.get(&node).map(|canvas| canvas.image.clone())).flatten();
    let Some(image) = image else {
        return Ok(js_string_value("data:,"));
    };
    Ok(js_string_value(&encode_data_url(&kind, &image)))
}

fn encode_data_url(kind: &str, image: &RgbaImage) -> String {
    let Some(bytes) = png_bytes(image) else {
        return "data:,".to_owned();
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mime = if kind.contains("jpeg") {
        "image/jpeg"
    } else {
        "image/png"
    };
    format!("data:{mime};base64,{encoded}")
}

fn png_bytes(image: &RgbaImage) -> Option<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    image.write_to(&mut cursor, image::ImageFormat::Png).ok()?;
    Some(cursor.into_inner())
}

fn style_from_value(value: &JsValue, context: &mut Context) -> Option<PaintStyle> {
    if let Some(object) = value.as_object() {
        let id = object
            .get(JsString::from(GRADIENT_ID_KEY), context)
            .ok()?
            .as_number()?;
        let gradient = lookup_gradient(id as u64)?;
        return Some(PaintStyle::Gradient(gradient));
    }
    let text = value.to_string(context).ok()?.to_std_string_escaped();
    parse_color(&text).map(PaintStyle::Solid)
}

fn style_value(paint: &PaintStyle, context: &mut Context) -> JsValue {
    match paint {
        PaintStyle::Solid(color) => {
            js_string_value(&format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b))
        }
        PaintStyle::Gradient(gradient) => {
            let id = gradient.borrow().id;
            JsValue::from(gradient_object(id, context))
        }
    }
}

fn js_get_fill_style(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let paint = with_canvas(this, context, |canvas| canvas.fill.clone())
        .unwrap_or(PaintStyle::Solid(Color::BLACK));
    Ok(style_value(&paint, context))
}

fn js_get_stroke_style(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let paint = with_canvas(this, context, |canvas| canvas.stroke.clone())
        .unwrap_or(PaintStyle::Solid(Color::BLACK));
    Ok(style_value(&paint, context))
}

fn set_style(this: &JsValue, args: &[JsValue], context: &mut Context, fill: bool) {
    let Some(value) = args.first() else {
        return;
    };
    let Some(paint) = style_from_value(value, context) else {
        return;
    };
    with_canvas(this, context, |canvas| {
        if fill {
            canvas.fill = paint;
        } else {
            canvas.stroke = paint;
        }
    });
}

fn js_set_fill_style(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_style(this, args, context, true);
    Ok(JsValue::undefined())
}

fn js_set_stroke_style(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    set_style(this, args, context, false);
    Ok(JsValue::undefined())
}

fn js_get_line_width(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let width = with_canvas(this, context, |canvas| canvas.line_width).unwrap_or(1.0);
    Ok(JsValue::from(width as f64))
}

fn js_set_line_width(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(width) = number_arg(args, 0) {
        with_canvas(this, context, |canvas| canvas.line_width = width.max(0.0));
    }
    Ok(JsValue::undefined())
}

fn js_get_alpha(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let alpha = with_canvas(this, context, |canvas| canvas.alpha).unwrap_or(1.0);
    Ok(JsValue::from(alpha as f64))
}

fn js_set_alpha(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(alpha) = number_arg(args, 0) {
        with_canvas(this, context, |canvas| canvas.alpha = alpha.clamp(0.0, 1.0));
    }
    Ok(JsValue::undefined())
}

fn js_get_font(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let font = with_canvas(this, context, |canvas| canvas.font.source.clone())
        .unwrap_or_else(|| "10px sans-serif".to_owned());
    Ok(js_string_value(&font))
}

fn js_set_font(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(value) = string_arg(args, 0, context)
        && let Some(font) = parse_canvas_font(&value)
    {
        with_canvas(this, context, |canvas| canvas.font = font);
    }
    Ok(JsValue::undefined())
}

fn js_get_text_align(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let align = with_canvas(this, context, |canvas| canvas.text_align).unwrap_or(TextAlign::Start);
    let value = match align {
        TextAlign::Start | TextAlign::Left => "start",
        TextAlign::Center => "center",
        TextAlign::End | TextAlign::Right => "end",
    };
    Ok(js_string_value(value))
}

fn js_set_text_align(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(value) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let align = match value.trim().to_ascii_lowercase().as_str() {
        "start" => TextAlign::Start,
        "left" => TextAlign::Left,
        "center" => TextAlign::Center,
        "right" => TextAlign::Right,
        "end" => TextAlign::End,
        _ => return Ok(JsValue::undefined()),
    };
    with_canvas(this, context, |canvas| canvas.text_align = align);
    Ok(JsValue::undefined())
}

fn js_get_text_baseline(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let baseline = with_canvas(this, context, |canvas| canvas.text_baseline)
        .unwrap_or(TextBaseline::Alphabetic);
    let value = match baseline {
        TextBaseline::Alphabetic => "alphabetic",
        TextBaseline::Top => "top",
        TextBaseline::Hanging => "hanging",
        TextBaseline::Middle => "middle",
        TextBaseline::Bottom => "bottom",
        TextBaseline::Ideographic => "ideographic",
    };
    Ok(js_string_value(value))
}

fn js_set_text_baseline(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(value) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let baseline = match value.trim().to_ascii_lowercase().as_str() {
        "alphabetic" => TextBaseline::Alphabetic,
        "top" => TextBaseline::Top,
        "hanging" => TextBaseline::Hanging,
        "middle" => TextBaseline::Middle,
        "bottom" => TextBaseline::Bottom,
        "ideographic" => TextBaseline::Ideographic,
        _ => return Ok(JsValue::undefined()),
    };
    with_canvas(this, context, |canvas| canvas.text_baseline = baseline);
    Ok(JsValue::undefined())
}

/// Parse the canvas `font` shorthand: `[style] [weight] <size>px <family>`.
fn parse_canvas_font(value: &str) -> Option<CanvasFont> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut weight: u16 = 400;
    let mut italic = false;
    let mut size: Option<f32> = None;
    let mut family_parts: Vec<&str> = Vec::new();
    for token in value.split_whitespace() {
        if size.is_some() {
            family_parts.push(token);
            continue;
        }
        let lower = token.to_ascii_lowercase();
        if lower == "italic" || lower == "oblique" {
            italic = true;
            continue;
        }
        if lower == "bold" {
            weight = 700;
            continue;
        }
        if lower == "normal" {
            continue;
        }
        if let Some(number) = lower.strip_suffix("px")
            && let Ok(parsed) = number.trim().parse::<f32>()
        {
            if parsed > 0.0 {
                size = Some(parsed);
            }
            continue;
        }
        if let Some(number) = lower.strip_suffix("pt")
            && let Ok(parsed) = number.trim().parse::<f32>()
        {
            if parsed > 0.0 {
                size = Some(parsed * 4.0 / 3.0);
            }
            continue;
        }
        if let Ok(parsed) = lower.parse::<f32>() {
            if (100.0..1000.0).contains(&parsed) && parsed >= 100.0 {
                weight = parsed as u16;
            } else if (1.0..=500.0).contains(&parsed) {
                size = Some(parsed);
            }
            continue;
        }
    }
    let size = size?;
    let family = if family_parts.is_empty() {
        "sans-serif".to_owned()
    } else {
        family_parts
            .join(" ")
            .trim_matches('"')
            .trim_matches('\'')
            .to_owned()
    };
    Some(CanvasFont {
        size,
        weight,
        italic,
        family,
        source: value.to_owned(),
    })
}

pub(crate) fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("transparent") {
        return Some(Color::TRANSPARENT);
    }
    if let Some(hex) = value.strip_prefix('#') {
        let digits: Vec<u8> = hex
            .chars()
            .filter_map(|character| character.to_digit(16).map(|digit| digit as u8))
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
                a: (alpha.clamp(0.0, 1.0) * 255.0) as u8,
            });
        }
    }
    Some(match value.to_ascii_lowercase().as_str() {
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
        _ => return None,
    })
}

pub(crate) fn reset_canvas(node: NodeId) {
    with_state(|state| state.canvases.remove(&node));
}

pub(crate) fn snapshot_canvases() -> HashMap<NodeId, RgbaImage> {
    with_state(|state| {
        state
            .canvases
            .iter()
            .map(|(node, canvas)| (*node, canvas.image.clone()))
            .collect()
    })
    .unwrap_or_default()
}
