//! Optional raster OCR for text that does not come from the DOM (canvas and
//! `<img>` content). The default backend is a no-op; [`GlyphOcr`] recognises
//! text rendered with the engine's embedded fonts by matching glyph templates.

use image::RgbaImage;
use serde::Serialize;

use crate::engine::css::ComputedStyle;
use crate::engine::layout::Rect;
use crate::engine::text::FontSet;
use crate::intelligence::visual::VisualRect;

/// A piece of text recognised from pixels.
#[derive(Clone, Debug, Serialize)]
pub struct RecognizedText {
    pub text: String,
    pub rect: VisualRect,
    pub confidence: f32,
    pub source: String,
}

/// Pluggable OCR backend.
pub trait OcrBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn recognize(&self, image: &RgbaImage, region: Rect) -> Vec<RecognizedText>;
}

/// Backend that reports no raster text. This keeps the default build free of
/// OCR work while still exposing the interface.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoOcr;

impl OcrBackend for NoOcr {
    fn name(&self) -> &'static str {
        "none"
    }

    fn recognize(&self, _image: &RgbaImage, _region: Rect) -> Vec<RecognizedText> {
        Vec::new()
    }
}

/// Template-matching OCR using the same embedded fonts as the paint pipeline.
///
/// It handles clean, opaque text at moderate-to-large sizes. Anti-aliased
/// small text or decorative fonts may be missed; DOM text is always handled
/// exactly by the layout index, so this backend only needs to cover pixels
/// that layout cannot describe.
#[derive(Clone, Copy, Debug)]
pub struct GlyphOcr {
    pub min_size: f32,
    pub max_size: f32,
    pub ink_threshold: u8,
    /// Pixels processed per tile. Bounds peak memory regardless of region size.
    pub max_tile_pixels: u32,
    /// Overlap between tiles so glyphs on a boundary are not cut in half.
    pub tile_overlap: u32,
    /// Hard cap on tiles per region, keeping work predictable on small devices.
    pub max_tiles: usize,
}

impl Default for GlyphOcr {
    fn default() -> Self {
        Self {
            min_size: 10.0,
            max_size: 64.0,
            ink_threshold: 48,
            max_tile_pixels: 256 * 192,
            tile_overlap: 12,
            max_tiles: 64,
        }
    }
}

impl OcrBackend for GlyphOcr {
    fn name(&self) -> &'static str {
        "glyph"
    }

    /// Scan a region in bounded tiles and merge results in reading order.
    fn recognize(&self, image: &RgbaImage, region: Rect) -> Vec<RecognizedText> {
        let width = region.width.max(0.0);
        let height = region.height.max(0.0);
        if width * height <= self.max_tile_pixels as f32 {
            return self.recognize_tile(image, region);
        }
        let tile_side = (self.max_tile_pixels as f32).sqrt().max(32.0);
        let aspect = (width / height.max(1.0)).clamp(0.25, 4.0);
        let tile_w = (tile_side * aspect.sqrt()).round() as u32;
        let tile_h = (self.max_tile_pixels / tile_w.max(1)).max(32);
        let stride_x = tile_w.saturating_sub(self.tile_overlap).max(1);
        let stride_y = tile_h.saturating_sub(self.tile_overlap).max(1);
        let mut results = Vec::new();
        let mut tiles = 0usize;
        let mut y = 0u32;
        while (y as f32) < height && tiles < self.max_tiles {
            let mut x = 0u32;
            while (x as f32) < width && tiles < self.max_tiles {
                let tile = Rect {
                    x: region.x + x as f32,
                    y: region.y + y as f32,
                    width: tile_w.min(width.ceil() as u32) as f32,
                    height: tile_h.min(height.ceil() as u32) as f32,
                };
                results.extend(self.recognize_tile(image, tile));
                tiles += 1;
                x += stride_x;
            }
            y += stride_y;
        }
        results.sort_by(|a, b| {
            a.rect
                .y
                .total_cmp(&b.rect.y)
                .then(a.rect.x.total_cmp(&b.rect.x))
        });
        let mut merged: Vec<RecognizedText> = Vec::new();
        for result in results {
            let duplicate = merged.iter().any(|existing| {
                existing.text == result.text
                    && (existing.rect.y - result.rect.y).abs() <= 8.0
                    && (existing.rect.x - result.rect.x).abs() <= self.tile_overlap as f32 * 2.0
            });
            if !duplicate {
                merged.push(result);
            }
        }
        merged
    }
}

impl GlyphOcr {
    fn recognize_tile(&self, image: &RgbaImage, region: Rect) -> Vec<RecognizedText> {
        let x0 = region.x.max(0.0).floor() as u32;
        let y0 = region.y.max(0.0).floor() as u32;
        let x1 = (region.right().ceil() as u32).min(image.width());
        let y1 = (region.bottom().ceil() as u32).min(image.height());
        if x1 <= x0 || y1 <= y0 {
            return Vec::new();
        }
        let width = (x1 - x0) as usize;
        let height = (y1 - y0) as usize;
        let background = estimate_background(image, x0, y0, width, height);
        let mut ink = vec![false; width * height];
        for y in 0..height {
            for x in 0..width {
                let pixel = image.get_pixel(x0 + x as u32, y0 + y as u32);
                let luma = luminance(pixel.0[0], pixel.0[1], pixel.0[2]);
                ink[y * width + x] = luma.abs_diff(background) > self.ink_threshold;
            }
        }

        let mut components = connected_components(&ink, width, height);
        components.retain(|component| {
            component.area >= 3
                && component.width <= 48
                && component.height <= 96
                && component.height >= 5
        });
        if components.is_empty() {
            return Vec::new();
        }
        let median_height = median(components.iter().map(|component| component.height as f32));
        let mut lines: Vec<Vec<Component>> = Vec::new();
        for component in components {
            let cy = component.y as f32 + component.height as f32 / 2.0;
            if let Some(line) = lines.iter_mut().find(|line| {
                let line_cy = median(
                    line.iter()
                        .map(|item| item.y as f32 + item.height as f32 / 2.0),
                );
                (cy - line_cy).abs() <= (median_height * 0.7).max(3.0)
            }) {
                line.push(component);
            } else {
                lines.push(vec![component]);
            }
        }
        lines.sort_by_key(|line| line.iter().map(|item| item.y).min().unwrap_or(0));

        let fonts = FontSet::embedded();
        let style = ComputedStyle::default();
        let font = fonts.font_for(&style);
        let mut results = Vec::new();
        for line in lines {
            let estimated = (median_height / 0.72).clamp(self.min_size, self.max_size);
            let sizes = [
                (estimated * 0.85).clamp(self.min_size, self.max_size),
                estimated,
                (estimated * 1.15).clamp(self.min_size, self.max_size),
            ];
            let templates = build_templates(font, &sizes);
            let mut line_rect = VisualRect::default();
            let mut text = String::new();
            let mut confidence_total = 0.0;
            let mut previous_right = 0usize;
            let mut median_advance = 0.0f32;
            let mut ordered = line;
            ordered.sort_by_key(|component| component.x);
            for (index, component) in ordered.iter().enumerate() {
                let crop = component_mask(&ink, width, component);
                let (character, score) = match_template(&crop, &templates);
                if index > 0 {
                    let gap = component.x.saturating_sub(previous_right) as f32;
                    if median_advance > 0.0 && gap > median_advance * 0.45 {
                        text.push(' ');
                    }
                }
                text.push(character);
                confidence_total += score;
                median_advance = if median_advance == 0.0 {
                    component.width as f32
                } else {
                    median_advance * 0.7 + component.width as f32 * 0.3
                };
                previous_right = component.x + component.width;
                let rect = VisualRect {
                    x: x0 as f32 + component.x as f32,
                    y: y0 as f32 + component.y as f32,
                    width: component.width as f32,
                    height: component.height as f32,
                };
                if index == 0 {
                    line_rect = rect;
                } else {
                    let right = (line_rect.x + line_rect.width).max(rect.x + rect.width);
                    let bottom = (line_rect.y + line_rect.height).max(rect.y + rect.height);
                    line_rect.x = line_rect.x.min(rect.x);
                    line_rect.y = line_rect.y.min(rect.y);
                    line_rect.width = right - line_rect.x;
                    line_rect.height = bottom - line_rect.y;
                }
            }
            if !text.trim().is_empty() {
                results.push(RecognizedText {
                    text: text.trim().to_owned(),
                    rect: line_rect,
                    confidence: confidence_total / ordered.len() as f32,
                    source: "ocr".to_owned(),
                });
            }
        }
        results
    }
}

#[derive(Clone, Copy, Debug)]
struct Component {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    area: usize,
}

#[derive(Clone)]
struct Template {
    character: char,
    mask: Vec<bool>,
}

const GRID: usize = 16;

fn luminance(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29) / 256) as u8
}

fn estimate_background(image: &RgbaImage, x: u32, y: u32, width: usize, height: usize) -> u8 {
    let mut samples = Vec::new();
    for sx in [0usize, width / 2, width.saturating_sub(1)] {
        for sy in [0usize, height / 2, height.saturating_sub(1)] {
            let pixel = image.get_pixel(x + sx as u32, y + sy as u32);
            samples.push(luminance(pixel.0[0], pixel.0[1], pixel.0[2]));
        }
    }
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn connected_components(mask: &[bool], width: usize, height: usize) -> Vec<Component> {
    let mut visited = vec![false; mask.len()];
    let mut components = Vec::new();
    for start in 0..mask.len() {
        if visited[start] || !mask[start] {
            continue;
        }
        let mut stack = vec![start];
        visited[start] = true;
        let mut min_x = usize::MAX;
        let mut min_y = usize::MAX;
        let mut max_x = 0usize;
        let mut max_y = 0usize;
        let mut area = 0usize;
        while let Some(index) = stack.pop() {
            let x = index % width;
            let y = index / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            area += 1;
            for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    let next = ny * width + nx;
                    if !visited[next] && mask[next] {
                        visited[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        components.push(Component {
            x: min_x,
            y: min_y,
            width: max_x - min_x + 1,
            height: max_y - min_y + 1,
            area,
        });
    }
    components
}

fn component_mask(ink: &[bool], width: usize, component: &Component) -> Vec<bool> {
    let mut grid = vec![false; GRID * GRID];
    for gy in 0..GRID {
        for gx in 0..GRID {
            let sx = component.x + (gx * component.width) / GRID;
            let sy = component.y + (gy * component.height) / GRID;
            grid[gy * GRID + gx] = ink[sy * width + sx];
        }
    }
    grid
}

fn build_templates(font: &fontdue::Font, sizes: &[f32]) -> Vec<Template> {
    let mut templates = Vec::new();
    let characters: Vec<char> =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!?.,-:"
            .chars()
            .collect();
    for size in sizes {
        for character in &characters {
            let (metrics, bitmap) = font.rasterize(*character, *size);
            if metrics.width == 0 || metrics.height == 0 {
                continue;
            }
            let mut grid = vec![false; GRID * GRID];
            for gy in 0..GRID {
                for gx in 0..GRID {
                    let sx = (gx * metrics.width) / GRID;
                    let sy = (gy * metrics.height) / GRID;
                    grid[gy * GRID + gx] = bitmap[sy * metrics.width + sx] > 96;
                }
            }
            templates.push(Template {
                character: *character,
                mask: grid,
            });
        }
    }
    templates
}

fn match_template(mask: &[bool], templates: &[Template]) -> (char, f32) {
    let mut best = ('?', 0.0f32);
    for template in templates {
        let mut intersection = 0usize;
        let mut union = 0usize;
        for (left, right) in mask.iter().zip(template.mask.iter()) {
            if *left || *right {
                union += 1;
            }
            if *left && *right {
                intersection += 1;
            }
        }
        let score = if union == 0 {
            0.0
        } else {
            intersection as f32 / union as f32
        };
        if score > best.1 {
            best = (template.character, score);
        }
    }
    best
}

fn median(values: impl Iterator<Item = f32>) -> f32 {
    let mut values: Vec<f32> = values.collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    values[values.len() / 2]
}
