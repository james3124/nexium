//! Screenshot encoding, cropping and comparison helpers.

use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, RgbaImage, imageops};

use crate::engine::layout::Rect;
use crate::utils::{NxError, Result};

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
    image
        .save(path)
        .map_err(|error| NxError::Image(format!("{}: {error}", path.display())))
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
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|error| NxError::Image(error.to_string()))?;
    Ok(bytes.into_inner())
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
