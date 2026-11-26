use fontdue::{Font, FontSettings};
use image::{Rgba, RgbaImage};
use nexium::engine::css::Viewport;
use nexium::intelligence::{GlyphOcr, NoOcr, OcrBackend, RecognizedText, VisualIndex, locator};
use nexium::{Browser, Config, RenderOptions};

fn draw_text(image: &mut RgbaImage, text: &str, x: u32, y: u32, size: f32) {
    let font = Font::from_bytes(
        &include_bytes!("../assets/fonts/DejaVuSans.ttf")[..],
        FontSettings::default(),
    )
    .expect("font");
    let mut pen = x as f32;
    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, size);
        for py in 0..metrics.height {
            for px in 0..metrics.width {
                let alpha = bitmap[py * metrics.width + px];
                if alpha > 0 {
                    let target = image.get_pixel_mut(x + pen as u32 + px as u32, y + py as u32);
                    let amount = alpha as u16 * 255 / 255;
                    let value = 255u16.saturating_sub(amount) as u8;
                    *target = Rgba([value, value, value, 255]);
                }
            }
        }
        pen += metrics.advance_width;
    }
}

/// Paint text with a negative effective letter spacing so neighbouring glyph
/// bitmaps touch and merge into a single connected component. This is the
/// pixel pattern behind the live-canvas bug where "GO" was read as "UU".
fn draw_text_tight(image: &mut RgbaImage, text: &str, x: u32, y: u32, size: f32, overlap: f32) {
    let font = Font::from_bytes(
        &include_bytes!("../assets/fonts/DejaVuSans.ttf")[..],
        FontSettings::default(),
    )
    .expect("font");
    let mut pen = x as f32;
    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, size);
        for py in 0..metrics.height {
            for px in 0..metrics.width {
                let alpha = bitmap[py * metrics.width + px];
                if alpha > 0 {
                    let target = image.get_pixel_mut(pen as u32 + px as u32, y + py as u32);
                    let value = 255u16.saturating_sub(alpha as u16) as u8;
                    *target = Rgba([value, value, value, 255]);
                }
            }
        }
        pen += (metrics.advance_width - overlap).max(1.0);
    }
}

/// Reading order across results; whitespace inserted between separated glyphs
/// must not hide the underlying letter sequence.
fn recognized_text(results: &[RecognizedText]) -> String {
    results
        .iter()
        .flat_map(|result| result.text.chars())
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

fn full_region(width: u32, height: u32) -> nexium::engine::layout::Rect {
    nexium::engine::layout::Rect {
        x: 0.0,
        y: 0.0,
        width: width as f32,
        height: height as f32,
    }
}

async fn blank_index() -> (
    image::RgbaImage,
    nexium::engine::layout::LayoutResult,
    Viewport,
) {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "https://render.test/",
            "<body style='margin:0;background:#fff'></body>",
        )
        .expect("page");
    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: Viewport {
                    width: 320.0,
                    height: 120.0,
                },
                ..RenderOptions::default()
            },
        )
        .await
        .expect("render");
    (
        RgbaImage::from_pixel(320, 120, Rgba([255, 255, 255, 255])),
        rendered.layout,
        rendered.viewport,
    )
}

#[tokio::test]
async fn no_ocr_reports_no_text() {
    let mut image = RgbaImage::from_pixel(200, 80, Rgba([255, 255, 255, 255]));
    draw_text(&mut image, "HELLO", 10, 20, 30.0);
    let results = NoOcr.recognize(
        &image,
        nexium::engine::layout::Rect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 80.0,
        },
    );
    assert!(results.is_empty());
    assert_eq!(NoOcr.name(), "none");
}

#[tokio::test]
async fn glyph_ocr_recognises_large_painted_text() {
    let mut image = RgbaImage::from_pixel(320, 120, Rgba([255, 255, 255, 255]));
    draw_text(&mut image, "STOP", 20, 30, 36.0);
    let ocr = GlyphOcr::default();
    let results = ocr.recognize(
        &image,
        nexium::engine::layout::Rect {
            x: 0.0,
            y: 0.0,
            width: 320.0,
            height: 120.0,
        },
    );
    assert!(!results.is_empty(), "OCR returned no results");
    assert!(
        results.iter().any(|result| result.text.contains("STOP")),
        "expected STOP, got {:?}",
        results
            .iter()
            .map(|result| result.text.clone())
            .collect::<Vec<_>>()
    );
    assert!(results.iter().all(|result| result.source == "ocr"));
}

#[tokio::test]
async fn glyph_ocr_recognises_touching_glyphs_in_tight_go() {
    let mut image = RgbaImage::from_pixel(320, 120, Rgba([255, 255, 255, 255]));
    // A 5.5px overlap (~85% of the normal G advance at 48px) joins the G and O
    // bitmaps while leaving a clear waist between them: the live canvas case
    // that was read as a single "U".
    draw_text_tight(&mut image, "GO", 20, 30, 48.0, 5.5);
    let results = GlyphOcr::default().recognize(&image, full_region(320, 120));
    let text = recognized_text(&results);
    let found: Vec<&str> = results.iter().map(|result| result.text.as_str()).collect();
    assert!(
        text.contains("GO"),
        "touching glyphs must be read as G then O, not one substitute letter: {text:?} from {found:?}"
    );
}

#[tokio::test]
async fn glyph_ocr_does_not_over_split_a_single_wide_glyph() {
    let mut image = RgbaImage::from_pixel(320, 120, Rgba([255, 255, 255, 255]));
    draw_text_tight(&mut image, "W", 20, 30, 44.0, 0.0);
    let results = GlyphOcr::default().recognize(&image, full_region(320, 120));
    let text = recognized_text(&results);
    let found: Vec<&str> = results.iter().map(|result| result.text.as_str()).collect();
    assert_eq!(
        text, "W",
        "one wide glyph must stay one character rather than being split: {text:?} from {found:?}"
    );
}

#[tokio::test]
async fn glyph_ocr_tiling_has_no_duplicate_overlapping_results() {
    // 1200x600 exceeds the default 256x192 tile budget, so recognition runs in
    // overlapping tiles. Place words across both tile seams (x~301, y~145/290).
    let (width, height) = (1200u32, 600u32);
    let mut image = RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]));
    draw_text_tight(&mut image, "STOP", 296, 140, 36.0, 0.0);
    draw_text_tight(&mut image, "GO", 598, 430, 48.0, 5.5);
    let results = GlyphOcr::default().recognize(&image, full_region(width, height));
    assert!(
        !results.is_empty(),
        "tiled OCR returned no results: {results:?}"
    );
    for (index, left) in results.iter().enumerate() {
        for right in results.iter().skip(index + 1) {
            let overlap_x = (left.rect.x + left.rect.width).min(right.rect.x + right.rect.width)
                - left.rect.x.max(right.rect.x);
            let overlap_y = (left.rect.y + left.rect.height).min(right.rect.y + right.rect.height)
                - left.rect.y.max(right.rect.y);
            if overlap_x <= 0.0 || overlap_y <= 0.0 {
                continue;
            }
            let smaller =
                (left.rect.width * left.rect.height).min(right.rect.width * right.rect.height);
            let ratio = overlap_x * overlap_y / smaller.max(1.0);
            assert!(
                ratio < 0.7,
                "duplicate overlapping OCR results {:?} and {:?} overlap {ratio:.2}",
                left.text,
                right.text
            );
        }
    }
}

#[tokio::test]
async fn visual_index_exposes_ocr_text_from_an_image() {
    let (mut image, layout, viewport) = blank_index().await;
    draw_text(&mut image, "GO", 20, 30, 32.0);
    let document = nexium::engine::dom::Document::new();
    let index = VisualIndex::new(&document, &layout, &image, viewport, (0.0, 0.0));
    let results = index.ocr_text(
        &GlyphOcr::default(),
        Some(nexium::engine::layout::Rect {
            x: 0.0,
            y: 0.0,
            width: 320.0,
            height: 120.0,
        }),
    );
    assert!(results.iter().any(|result| result.text.contains("G")));
}

#[tokio::test]
async fn locator_registry_and_find_with_match_the_default() {
    assert!(locator(Some("heuristic")).is_some());
    assert!(locator(Some("default")).is_some());
    assert!(locator(Some("unknown-model")).is_none());

    let browser = Browser::new(Config::default());
    let page = browser
        .load_html(
            "https://render.test/",
            "<body style='margin:0;background:#fff'><button id='ok'>OK</button></body>",
        )
        .expect("page");
    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: Viewport {
                    width: 300.0,
                    height: 200.0,
                },
                ..RenderOptions::default()
            },
        )
        .await
        .expect("render");
    let index = VisualIndex::new(
        &page.document,
        &rendered.layout,
        &rendered.image,
        rendered.viewport,
        (0.0, 0.0),
    );
    let default = index.find("OK button", 3, None);
    let via_trait = index.find_with(locator(None).expect("locator"), "OK button", 3, None);
    assert_eq!(default.len(), via_trait.len());
    assert_eq!(default[0].element.node_id, via_trait[0].element.node_id);
}
