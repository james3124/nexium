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
        // Tiles overlap, so the same glyph can be recognised twice with
        // slightly different boxes and even different letters. Prefer the
        // higher-confidence detection and discard any result that substantially
        // overlaps one already kept.
        results.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        let mut merged: Vec<RecognizedText> = Vec::new();
        for result in results {
            let duplicate = merged.iter().any(|existing| {
                let overlap_x = (existing.rect.x + existing.rect.width)
                    .min(result.rect.x + result.rect.width)
                    - existing.rect.x.max(result.rect.x);
                let overlap_y = (existing.rect.y + existing.rect.height)
                    .min(result.rect.y + result.rect.height)
                    - existing.rect.y.max(result.rect.y);
                if overlap_x <= 0.0 || overlap_y <= 0.0 {
                    return false;
                }
                let smaller = (existing.rect.width * existing.rect.height)
                    .min(result.rect.width * result.rect.height);
                overlap_x * overlap_y >= smaller * 0.5
            });
            if !duplicate {
                merged.push(result);
            }
        }
        merged.sort_by(|a, b| {
            a.rect
                .y
                .total_cmp(&b.rect.y)
                .then(a.rect.x.total_cmp(&b.rect.x))
        });
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
        // Do not reject wide components yet: several touching glyphs can form
        // one wide blob. They are split into per-character pieces inside the
        // line loop below, then the size filter is applied to each piece.
        components.retain(|component| {
            component.area >= 3 && component.height <= 96 && component.height >= 5
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
            let mut matches = 0usize;
            let mut previous_right = 0usize;
            let mut median_advance = 0.0f32;
            let mut ordered = line;
            ordered.sort_by_key(|component| component.x);
            for (index, component) in ordered.iter().enumerate() {
                // Word gaps are only meaningful between separate components;
                // pieces of one split component are glued together below.
                if index > 0 {
                    let gap = component.x.saturating_sub(previous_right) as f32;
                    if median_advance > 0.0 && gap > median_advance * 0.45 {
                        text.push(' ');
                    }
                }
                for part in split_component(&ink, width, component) {
                    if !(2..=48).contains(&part.width)
                        || !(5..=96).contains(&part.height)
                        || part.area < 3
                    {
                        continue;
                    }
                    let crop = component_mask(&ink, width, &part);
                    let aspect = part.width as f32 / part.height.max(1) as f32;
                    let height_ratio = part.height as f32 / estimated.max(1.0);
                    let (character, score) =
                        match_template(&crop, &templates, aspect, height_ratio);
                    text.push(character);
                    confidence_total += score;
                    matches += 1;
                    median_advance = if median_advance == 0.0 {
                        part.width as f32
                    } else {
                        median_advance * 0.7 + part.width as f32 * 0.3
                    };
                    let rect = VisualRect {
                        x: x0 as f32 + part.x as f32,
                        y: y0 as f32 + part.y as f32,
                        width: part.width as f32,
                        height: part.height as f32,
                    };
                    if matches == 1 {
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
                previous_right = component.x + component.width;
            }
            if !text.trim().is_empty() {
                results.push(RecognizedText {
                    text: text.trim().to_owned(),
                    rect: line_rect,
                    confidence: confidence_total / matches.max(1) as f32,
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
    aspect: f32,
    /// Glyph bitmap height divided by font size; separates caps from x-height.
    height_ratio: f32,
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

/// Splits one connected component into per-character pieces when it is wide
/// relative to its height *and* has at least one ink valley separating the
/// glyphs. A single wide glyph (`W`, `M`) lacks a genuine valley and is
/// returned untouched. Work is linear in the component width and the number of
/// pieces is capped at eight, so this cannot blow up on a large tile.
fn split_component(ink: &[bool], stride: usize, component: &Component) -> Vec<Component> {
    let width = component.width;
    let height = component.height;
    if height < 5 || width < 2 || (width as f32 / height as f32) <= 1.35 {
        return vec![*component];
    }

    // Ink count per column over the component's vertical extent.
    let mut columns = vec![0usize; width];
    for cy in component.y..component.y + height {
        let row = cy * stride;
        for (offset, count) in columns.iter_mut().enumerate() {
            if ink[row + component.x + offset] {
                *count += 1;
            }
        }
    }

    // A real inter-glyph gap leaves almost no ink, while the strokes on either
    // side are dense. Requiring dense shoulders two columns away rejects the
    // shallow notches of single wide glyphs and flat bars (e.g. the crossbar
    // of `T`), neither of which is a character boundary.
    let valley_limit = ((height as f32 * 0.16).round() as usize).max(1);
    let shoulder_limit = (valley_limit * 2).max(valley_limit + 2);
    let mut valleys: Vec<usize> = (1..width.saturating_sub(1))
        .filter(|&offset| {
            columns[offset] <= valley_limit
                && columns[offset.saturating_sub(2)] > shoulder_limit
                && columns[(offset + 2).min(width - 1)] > shoulder_limit
        })
        .collect();

    // One glyph advances roughly 0.88 of its painted height. Use that to
    // guess the number of characters and evenly space the target boundaries.
    let advance = (height as f32 * 0.88).max(1.0);
    let min_segment = ((advance * 0.35).round() as usize).max(3);

    // Glyph bitmaps may overlap instead of leaving an empty column. The
    // junction then shows only a shallow waist. Accept those only for very
    // wide blobs, which cannot be a single glyph in the embedded fonts; this
    // keeps wide single letters such as W and M unsplit.
    let shallow = valleys.is_empty() && (width as f32 / height as f32) > 1.6;
    if shallow {
        valleys = (1..width.saturating_sub(1))
            .filter(|&offset| {
                let shoulders =
                    columns[offset.saturating_sub(2)] + columns[(offset + 2).min(width - 1)];
                shoulders >= columns[offset] * 2 + 8
            })
            .collect();
    }

    // Last resort: an extremely wide blob whose glyphs overlap so much that
    // no waist survives. Cut near the even boundary at the lowest-ink column,
    // but only when both halves carry comparable ink mass (two letters).
    let even = valleys.is_empty() && (width as f32 / height as f32) > 1.8;
    if even {
        let half = width / 2;
        let left: usize = columns[..half].iter().sum();
        let right: usize = columns[half..].iter().sum();
        if left > 0 && right > 0 && left.max(right) <= left.min(right) * 2 {
            let from = half.saturating_sub(min_segment).max(1);
            let to = (half + min_segment).min(width.saturating_sub(2));
            if from <= to {
                if let Some(offset) =
                    (from..=to).min_by_key(|&offset| (columns[offset], offset.abs_diff(half)))
                {
                    valleys.push(offset);
                }
            }
        }
    }
    if valleys.is_empty() {
        return vec![*component];
    }

    let count = ((width as f32 / advance).round() as usize)
        .clamp(2, 8)
        .min(valleys.len() + 1);
    let window = ((advance * 0.5).round() as usize).max(min_segment);

    let mut boundaries: Vec<usize> = Vec::new();
    for part in 1..count {
        let target = width * part / count;
        let mut best: Option<(usize, usize, usize)> = None; // rank, tie-break, offset
        for &offset in &valleys {
            if offset < min_segment || width - offset < min_segment {
                continue;
            }
            if boundaries
                .iter()
                .any(|&previous| offset.abs_diff(previous) < min_segment)
            {
                continue;
            }
            let distance = offset.abs_diff(target);
            if distance > window {
                continue;
            }
            // Deep valleys are ranked by depth; shallow waists are ranked by
            // how close they are to the evenly spaced target boundary.
            let candidate = if shallow || even {
                (distance, columns[offset], offset)
            } else {
                (columns[offset], distance, offset)
            };
            if best.map_or(true, |(first, second, _)| candidate < (first, second, 0)) {
                best = Some(candidate);
            }
        }
        if let Some((_, _, offset)) = best {
            boundaries.push(offset);
        }
    }
    if boundaries.is_empty() {
        return vec![*component];
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut pieces = Vec::with_capacity(boundaries.len() + 1);
    let mut start = component.x;
    for &offset in boundaries.iter().chain(std::iter::once(&width)) {
        let end = component.x + offset;
        let Some(piece) = piece_between(ink, stride, component, start, end) else {
            return vec![*component];
        };
        pieces.push(piece);
        start = end;
    }
    if pieces.len() < 2 || pieces.iter().any(|piece| piece.area < 3 || piece.width < 2) {
        return vec![*component];
    }
    pieces
}

/// Rebuilds one sub-component from the ink inside `[start, end)` horizontally
/// and the parent component's vertical span.
fn piece_between(
    ink: &[bool],
    stride: usize,
    component: &Component,
    start: usize,
    end: usize,
) -> Option<Component> {
    let mut min_x = usize::MAX;
    let mut min_y = usize::MAX;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut area = 0usize;
    for cy in component.y..component.y + component.height {
        let row = cy * stride;
        for cx in start..end {
            if ink[row + cx] {
                min_x = min_x.min(cx);
                min_y = min_y.min(cy);
                max_x = max_x.max(cx);
                max_y = max_y.max(cy);
                area += 1;
            }
        }
    }
    if area == 0 {
        return None;
    }
    Some(Component {
        x: min_x,
        y: min_y,
        width: max_x - min_x + 1,
        height: max_y - min_y + 1,
        area,
    })
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
                aspect: metrics.width as f32 / metrics.height.max(1) as f32,
                height_ratio: metrics.height as f32 / size.max(1.0),
            });
        }
    }
    templates
}

fn match_template(
    mask: &[bool],
    templates: &[Template],
    aspect: f32,
    height_ratio: f32,
) -> (char, f32) {
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
        let iou = if union == 0 {
            0.0
        } else {
            intersection as f32 / union as f32
        };
        // Normalised bitmaps make uppercase/lowercase twins (S/s, U/u) look
        // alike; their real glyph aspect ratios differ, so penalise mismatches.
        let ratio = (aspect / template.aspect.max(0.01)).ln().abs();
        let height_delta = (height_ratio / template.height_ratio.max(0.01)).ln().abs();
        let score = iou * (1.0 - (ratio * 0.35).min(0.5)) * (1.0 - (height_delta * 0.6).min(0.7));
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
