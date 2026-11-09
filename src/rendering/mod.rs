//! Page rendering and screenshot API (headless, no window system).

mod screenshots;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use image::RgbaImage;
use std::time::Instant;

use tracing::debug;

use crate::browser::Page;
use crate::engine::css::{
    ComponentValue, Rule, Viewport, parse_declaration_list, user_agent_stylesheet,
};
use crate::engine::dom::NodeId;
use crate::engine::layout::{LayoutResult, layout_document};
use crate::engine::paint::paint_document;
use crate::engine::text::FontSet;
use crate::networking::{HttpClient, Request};
use crate::utils::Result;

const MAX_IMAGES: usize = 64;

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub viewport: Viewport,
    pub max_height: u32,
    pub load_images: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            viewport: Viewport {
                width: 1280.0,
                height: 720.0,
            },
            max_height: 20_000,
            load_images: true,
        }
    }
}

pub struct RenderedPage {
    pub image: RgbaImage,
    pub layout: LayoutResult,
    pub viewport: Viewport,
}

impl RenderedPage {
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        match extension.as_str() {
            "jpg" | "jpeg" => self.save_jpeg(path, 90),
            _ => self.save_png(path),
        }
    }

    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        screenshots::save_png(&self.image, path.as_ref())
    }

    pub fn save_jpeg(&self, path: impl AsRef<Path>, quality: u8) -> Result<()> {
        screenshots::save_jpeg(&self.image, path.as_ref(), quality)
    }

    pub fn png_bytes(&self) -> Result<Vec<u8>> {
        screenshots::png_bytes(&self.image)
    }

    pub fn jpeg_bytes(&self, quality: u8) -> Result<Vec<u8>> {
        screenshots::jpeg_bytes(&self.image, quality)
    }

    /// Crop to the configured viewport (top of the page).
    pub fn viewport_image(&self) -> RgbaImage {
        let rect = crate::engine::layout::Rect {
            x: 0.0,
            y: 0.0,
            width: self.viewport.width.min(self.image.width() as f32),
            height: self.viewport.height.min(self.image.height() as f32),
        };
        screenshots::crop(&self.image, rect).unwrap_or_else(|| self.image.clone())
    }

    /// Crop to an element's border box, if it was laid out.
    pub fn element_image(&self, node: NodeId) -> Option<RgbaImage> {
        let rect = self.layout.node_rect(node)?;
        screenshots::crop(&self.image, rect)
    }

    pub fn resized(&self, width: u32, height: u32) -> RgbaImage {
        screenshots::resize(&self.image, width, height)
    }

    /// Fraction of pixels that differ from `other` (0.0 = identical).
    pub fn diff(&self, other: &RgbaImage) -> f32 {
        screenshots::diff(&self.image, other)
    }
}

/// Render a loaded page: fetch referenced images, compute styles, lay out and
/// rasterize into an RGBA image.
pub async fn render_page(
    page: &Page,
    client: &HttpClient,
    options: RenderOptions,
) -> Result<RenderedPage> {
    let started = Instant::now();
    let images = if options.load_images {
        load_images(page, client).await
    } else {
        HashMap::new()
    };
    debug!("images: {} loaded in {:?}", images.len(), started.elapsed());

    let started = Instant::now();
    let fonts = FontSet::embedded();
    let resolver = crate::engine::css::StyleResolver::new(
        &page.document,
        user_agent_stylesheet(),
        &page.stylesheets,
        options.viewport,
    );
    let layout = layout_document(&page.document, &resolver, &fonts, &images, Some(&page.url));
    debug!("fonts+layout in {:?}", started.elapsed());

    let width = options.viewport.width.max(1.0).round() as u32;
    let height = layout
        .document_height
        .max(options.viewport.height)
        .min(options.max_height as f32)
        .max(1.0)
        .ceil() as u32;

    let started = Instant::now();
    let image = paint_document(&layout, &fonts, &images, Some(&page.url), width, height);
    debug!("paint in {:?}", started.elapsed());

    Ok(RenderedPage {
        image,
        layout,
        viewport: options.viewport,
    })
}

async fn load_images(page: &Page, client: &HttpClient) -> HashMap<String, Arc<RgbaImage>> {
    let mut raw_urls: Vec<String> = Vec::new();

    for id in page.document.descendants(page.document.document_node()) {
        if page.document.tag_name(id) == Some("img")
            && let Some(src) = page.document.get_attribute(id, "src")
        {
            raw_urls.push(src.to_owned());
        }
        if let Some(style) = page.document.get_attribute(id, "style") {
            for declaration in parse_declaration_list(style) {
                collect_value_urls(&declaration.value, &mut raw_urls);
            }
        }
    }
    for sheet in &page.stylesheets {
        collect_rule_urls(&sheet.rules, &mut raw_urls);
    }

    let mut images = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for raw in raw_urls {
        if images.len() >= MAX_IMAGES {
            break;
        }
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let Ok(url) = page.url.join(raw) else {
            continue;
        };
        let key = url.to_string();
        if !seen.insert(key.clone()) {
            continue;
        }

        let bytes = if url.scheme() == "data" {
            decode_data_url(url.as_str())
        } else if matches!(url.scheme(), "http" | "https") {
            match Request::get(url.clone()).header("Accept", "image/*,*/*;q=0.1") {
                Ok(request) => match client.fetch(request).await {
                    Ok(response) if response.status.is_success() => Some(response.body.to_vec()),
                    Ok(response) => {
                        debug!("image {url}: HTTP {}", response.status.as_u16());
                        None
                    }
                    Err(error) => {
                        debug!("image {url}: {error}");
                        None
                    }
                },
                Err(_) => None,
            }
        } else {
            None
        };

        if let Some(bytes) = bytes
            && let Ok(decoded) = image::load_from_memory(&bytes)
        {
            images.insert(key, Arc::new(decoded.to_rgba8()));
        }
    }
    images
}

fn collect_rule_urls(rules: &[Rule], out: &mut Vec<String>) {
    for rule in rules {
        match rule {
            Rule::Style(style_rule) => {
                for declaration in &style_rule.declarations {
                    collect_value_urls(&declaration.value, out);
                }
            }
            Rule::Media(media) => collect_rule_urls(&media.rules, out),
            Rule::Supports(supports) => collect_rule_urls(&supports.rules, out),
            Rule::Import(_) => {}
        }
    }
}

fn collect_value_urls(values: &[ComponentValue], out: &mut Vec<String>) {
    for value in values {
        if let Some(url) = crate::engine::css::values::parse_url(value) {
            out.push(url);
            continue;
        }
        if let ComponentValue::Function { args, .. } = value {
            collect_value_urls(args, out);
        }
    }
}

fn decode_data_url(url: &str) -> Option<Vec<u8>> {
    let comma = url.find(',')?;
    let meta = &url[..comma];
    let payload = &url[comma + 1..];
    if meta.to_ascii_lowercase().ends_with(";base64") {
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .ok()
    } else {
        Some(percent_decode(payload))
    }
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}
