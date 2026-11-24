//! Screenshot encoding, cropping and comparison helpers.

use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbaImage, imageops};

use crate::engine::layout::Rect;
use crate::utils::{NxError, Result};

/// Menyoki-style defaults: fast PNG compression with the Sub filter.
pub(crate) const DEFAULT_PNG_COMPRESSION: CompressionType = CompressionType::Fast;
pub(crate) const DEFAULT_PNG_FILTER: FilterType = FilterType::Sub;

pub(crate) fn crop(image: &RgbaImage, rect: Rect) -> Option<RgbaImage> {
    let width = image.width() as f32;
    let height = image.height() as f32;
    let x = rect.x.floor().clamp(0.0, width) as u32;
    let y = rect.y.floor().clamp(0.0, height) as u32;
    let right = rect.right().ceil().clamp(0.0, width) as u32;
    let bottom = rect.bottom().ceil().clamp(0.0, height) as u32;
    if right <= x || bottom <= y {
        return None;
    }
    Some(imageops::crop_imm(image, x, y, right - x, bottom - y).to_image())
}

pub(crate) fn save_png(image: &RgbaImage, path: &Path) -> Result<()> {
    let bytes = png_bytes(image)?;
    std::fs::write(path, bytes).map_err(NxError::Io)
}

pub(crate) fn save_webp(image: &RgbaImage, path: &Path) -> Result<()> {
    let bytes = webp_bytes(image)?;
    std::fs::write(path, bytes).map_err(NxError::Io)
}

pub(crate) fn save_jpeg(image: &RgbaImage, path: &Path, quality: u8) -> Result<()> {
    let rgb = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
    let file = std::fs::File::create(path).map_err(NxError::Io)?;
    let mut encoder = JpegEncoder::new_with_quality(file, quality.clamp(1, 100));
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| NxError::Image(format!("{}: {error}", path.display())))
}

pub(crate) fn png_bytes(image: &RgbaImage) -> Result<Vec<u8>> {
    png_bytes_with(image, DEFAULT_PNG_COMPRESSION, DEFAULT_PNG_FILTER)
}

pub(crate) fn png_bytes_with(
    image: &RgbaImage,
    compression: CompressionType,
    filter: FilterType,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    PngEncoder::new_with_quality(&mut bytes, compression, filter)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|error| NxError::Image(error.to_string()))?;
    Ok(bytes)
}

pub(crate) fn webp_bytes(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    WebPEncoder::new_lossless(&mut bytes)
        .encode(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|error| NxError::Image(error.to_string()))?;
    Ok(bytes)
}

pub(crate) fn jpeg_bytes(image: &RgbaImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, quality.clamp(1, 100));
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| NxError::Image(error.to_string()))?;
    Ok(bytes)
}

/// Resize a screenshot using a triangular filter.
pub(crate) fn resize(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    imageops::resize(
        image,
        width.max(1),
        height.max(1),
        imageops::FilterType::Triangle,
    )
}

/// A bounding box of changed pixels returned by [`diff_regions`].
#[derive(Clone, Copy, Debug)]
pub struct DiffRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: u64,
}

/// Compare two equally sized images and return the differing fraction plus
/// grouped bounding boxes of changed areas. Pixels differing by more than
/// `threshold` in any channel are grouped on a coarse grid so a moving caret
/// or subpixel text jitter does not produce hundreds of tiny regions.
pub(crate) fn diff_regions(
    a: &RgbaImage,
    b: &RgbaImage,
    threshold: u8,
    min_pixels: u64,
) -> (f32, Vec<DiffRegion>) {
    if a.width() != b.width() || a.height() != b.height() {
        return (
            1.0,
            vec![DiffRegion {
                x: 0,
                y: 0,
                width: a.width().max(b.width()),
                height: a.height().max(b.height()),
                pixels: 0,
            }],
        );
    }
    const CELL: u32 = 16;
    let cells_x = a.width().div_ceil(CELL);
    let cells_y = a.height().div_ceil(CELL);
    let mut counts = vec![0u64; (cells_x * cells_y) as usize];
    let mut changed = 0u64;
    for (y, (_, row)) in a.enumerate_rows().enumerate() {
        let y = y as u32;
        for (x, _, pa) in row {
            let pb = b.get_pixel(x, y);
            if pa
                .0
                .iter()
                .zip(pb.0.iter())
                .any(|(left, right)| left.abs_diff(*right) > threshold)
            {
                changed += 1;
                let index = (y as u32 / CELL * cells_x + x / CELL) as usize;
                counts[index] += 1;
            }
        }
    }
    let total = (a.width() * a.height()) as u64;
    let ratio = if total == 0 {
        0.0
    } else {
        changed as f32 / total as f32
    };

    let mut visited = vec![false; counts.len()];
    let mut regions = Vec::new();
    for start in 0..counts.len() {
        if visited[start] || counts[start] == 0 {
            continue;
        }
        let mut stack = vec![start];
        visited[start] = true;
        let mut min_cx = cells_x;
        let mut min_cy = cells_y;
        let mut max_cx = 0u32;
        let mut max_cy = 0u32;
        let mut pixels = 0u64;
        while let Some(index) = stack.pop() {
            let cx = index as u32 % cells_x;
            let cy = index as u32 / cells_x;
            min_cx = min_cx.min(cx);
            min_cy = min_cy.min(cy);
            max_cx = max_cx.max(cx);
            max_cy = max_cy.max(cy);
            pixels += counts[index];
            for (nx, ny) in [
                (cx, cy.wrapping_sub(1)),
                (cx, cy + 1),
                (cx.wrapping_sub(1), cy),
                (cx + 1, cy),
            ] {
                if nx < cells_x && ny < cells_y {
                    let next = (ny * cells_x + nx) as usize;
                    if !visited[next] && counts[next] > 0 {
                        visited[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        if pixels < min_pixels {
            continue;
        }
        let x = min_cx * CELL;
        let y = min_cy * CELL;
        let right = ((max_cx + 1) * CELL).min(a.width());
        let bottom = ((max_cy + 1) * CELL).min(a.height());
        regions.push(DiffRegion {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
            pixels,
        });
    }
    regions.sort_by_key(|region| region.pixels);
    regions.reverse();
    (ratio, regions)
}

/// Fraction of pixels differing by more than 8 in any channel.
pub(crate) fn diff(a: &RgbaImage, b: &RgbaImage) -> f32 {
    if a.width() != b.width() || a.height() != b.height() {
        return 1.0;
    }
    let total = (a.width() * a.height()) as f32;
    if total == 0.0 {
        return 0.0;
    }
    let mut differing = 0u64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        if pa
            .0
            .iter()
            .zip(pb.0.iter())
            .any(|(x, y)| x.abs_diff(*y) > 8)
        {
            differing += 1;
        }
    }
    differing as f32 / total
}
