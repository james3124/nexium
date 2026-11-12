use std::io::Cursor;

use base64::Engine as _;
use image::{ImageFormat, Rgba, RgbaImage};
use nexium::engine::css::Viewport;
use nexium::{Browser, Config, Page, RenderOptions, RenderedPage};

async fn render(html: &str, width: f32, height: f32) -> (Page, RenderedPage) {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html("https://canvas.test/", html)
        .expect("page");
    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: Viewport { width, height },
                ..RenderOptions::default()
            },
        )
        .await
        .expect("render");
    (page, rendered)
}

fn canvas_bitmap(page: &Page, selector: &str) -> RgbaImage {
    let node = page.select(selector).expect("selector").expect("node");
    page.canvases
        .get(&node)
        .cloned()
        .expect("canvas bitmap should be snapshotted")
}

fn pixel(image: &RgbaImage, x: u32, y: u32) -> Rgba<u8> {
    *image.get_pixel(x.min(image.width() - 1), y.min(image.height() - 1))
}

#[tokio::test]
async fn canvas_fill_text_rasterizes_and_measure_scales() {
    let html = r##"<style>body{margin:0}</style>
<canvas id=c width=200 height=60></canvas>
<script>
var ctx = document.getElementById('c').getContext('2d');
ctx.fillStyle = '#00ff00';
ctx.font = '24px sans-serif';
ctx.fillText('Hi', 5, 40);
var small = ctx.measureText('Hello').width;
var smallAsc = ctx.measureText('Hello').actualBoundingBoxAscent;
var smallDesc = ctx.measureText('Hello').actualBoundingBoxDescent;
ctx.font = 'italic bold 48px serif';
var big = ctx.measureText('Hello').width;
document.body.setAttribute('data-small', String(small));
document.body.setAttribute('data-big', String(big));
document.body.setAttribute('data-asc', String(smallAsc));
document.body.setAttribute('data-desc', String(smallDesc));
</script>"##;
    let (page, _) = render(html, 240.0, 100.0).await;
    let bitmap = canvas_bitmap(&page, "#c");

    let mut green = 0usize;
    for y in 0..bitmap.height() {
        for x in 0..bitmap.width() {
            let px = pixel(&bitmap, x, y);
            if px[1] > 140 && px[0] < 100 && px[2] < 100 && px[3] > 100 {
                green += 1;
            }
        }
    }
    assert!(green > 20, "expected green glyph pixels, got {green}");

    let body = page.select("body").unwrap().unwrap();
    let attribute = |name: &str| {
        page.document
            .get_attribute(body, name)
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let small = attribute("data-small");
    let big = attribute("data-big");
    assert!(small > 0.0, "measureText width should be positive");
    assert!(
        big > small * 1.5,
        "measureText width should scale with font size: {small} vs {big}"
    );
    assert!(attribute("data-asc") > 0.0, "ascent should be positive");
    assert!(attribute("data-desc") > 0.0, "descent should be positive");
}

#[tokio::test]
async fn canvas_linear_gradient_forms_horizontal_ramp() {
    let html = r##"<style>body{margin:0}</style>
<canvas id=c width=100 height=20></canvas>
<script>
var ctx = document.getElementById('c').getContext('2d');
var gradient = ctx.createLinearGradient(0, 0, 100, 0);
gradient.addColorStop(0, '#ff0000');
gradient.addColorStop(1, '#0000ff');
ctx.fillStyle = gradient;
ctx.fillRect(0, 0, 100, 20);
</script>"##;
    let (page, _) = render(html, 120.0, 40.0).await;
    let bitmap = canvas_bitmap(&page, "#c");
    let left = pixel(&bitmap, 2, 10);
    let middle = pixel(&bitmap, 50, 10);
    let right = pixel(&bitmap, 97, 10);
    assert!(
        left[0] > 220 && left[2] < 40,
        "left edge should be red, got {left:?}"
    );
    assert!(
        right[2] > 220 && right[0] < 40,
        "right edge should be blue, got {right:?}"
    );
    assert!(
        middle[0] > 60 && middle[0] < 200 && middle[2] > 60 && middle[2] < 200,
        "middle should interpolate, got {middle:?}"
    );
}

#[tokio::test]
async fn canvas_radial_gradient_samples_center_and_edge() {
    let html = r##"<style>body{margin:0}</style>
<canvas id=c width=100 height=100></canvas>
<script>
var ctx = document.getElementById('c').getContext('2d');
var gradient = ctx.createRadialGradient(50, 50, 0, 50, 50, 45);
gradient.addColorStop(0, '#ff0000');
gradient.addColorStop(1, '#0000ff');
ctx.fillStyle = gradient;
ctx.fillRect(0, 0, 100, 100);
</script>"##;
    let (page, _) = render(html, 120.0, 120.0).await;
    let bitmap = canvas_bitmap(&page, "#c");
    let center = pixel(&bitmap, 50, 50);
    let edge = pixel(&bitmap, 93, 50);
    assert!(
        center[0] > 200 && center[2] < 80,
        "center should be red, got {center:?}"
    );
    assert!(
        edge[2] > 200 && edge[0] < 80,
        "edge should be blue, got {edge:?}"
    );
}

#[tokio::test]
async fn canvas_draw_image_copies_pixels_from_another_canvas() {
    let html = r##"<style>body{margin:0}</style>
<canvas id=src width=10 height=10></canvas>
<canvas id=dst width=40 height=40></canvas>
<script>
var source = document.getElementById('src').getContext('2d');
source.fillStyle = '#ff00ff';
source.fillRect(0, 0, 10, 10);
var dest = document.getElementById('dst').getContext('2d');
dest.drawImage(document.getElementById('src'), 5, 7);
dest.drawImage(document.getElementById('src'), 20, 20, 20, 20);
</script>"##;
    let (page, _) = render(html, 120.0, 120.0).await;
    let bitmap = canvas_bitmap(&page, "#dst");
    let copied = pixel(&bitmap, 8, 10);
    assert!(
        copied[0] > 200 && copied[2] > 200 && copied[1] < 60 && copied[3] > 200,
        "1:1 copy should be magenta, got {copied:?}"
    );
    let outside = pixel(&bitmap, 0, 0);
    assert_eq!(outside[3], 0, "untouched area should stay transparent");
    let scaled = pixel(&bitmap, 30, 30);
    assert!(
        scaled[0] > 200 && scaled[2] > 200 && scaled[1] < 60 && scaled[3] > 200,
        "5-argument drawImage should scale pixels, got {scaled:?}"
    );
}

#[tokio::test]
async fn canvas_draw_image_accepts_data_url_img_sources() {
    let source = RgbaImage::from_pixel(2, 2, Rgba([0, 255, 0, 255]));
    let mut bytes = Vec::new();
    source
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("encode png");
    let data = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    let html = format!(
        r##"<style>body{{margin:0}}</style><img id=i src="{data}"><canvas id=c width=20 height=20></canvas>
<script>
var ctx = document.getElementById('c').getContext('2d');
ctx.drawImage(document.getElementById('i'), 3, 4);
</script>"##
    );
    let (page, _) = render(&html, 120.0, 120.0).await;
    let bitmap = canvas_bitmap(&page, "#c");
    let copied = pixel(&bitmap, 3, 4);
    assert!(
        copied[1] > 200 && copied[0] < 60 && copied[3] > 200,
        "data URL image should be copied, got {copied:?}"
    );
}

#[tokio::test]
async fn svg_linear_gradient_reference_paints_ramp() {
    let html = r##"<style>body{margin:0}</style>
<svg width=100 height=100 viewBox="0 0 100 100">
<defs>
<linearGradient id="ramp">
<stop offset="0%" stop-color="#ff0000"/>
<stop offset="100%" stop-color="#0000ff"/>
</linearGradient>
</defs>
<rect x="0" y="0" width="100" height="100" fill="url(#ramp)"/>
</svg>"##;
    let (page, rendered) = render(html, 200.0, 120.0).await;
    let node = page.select("svg").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    let left = pixel(
        &rendered.image,
        (rect.x + 3.0) as u32,
        (rect.y + 50.0) as u32,
    );
    let right = pixel(
        &rendered.image,
        (rect.x + 96.0) as u32,
        (rect.y + 50.0) as u32,
    );
    assert!(
        left[0] > 200 && left[2] < 60,
        "gradient left should be red, got {left:?}"
    );
    assert!(
        right[2] > 200 && right[0] < 60,
        "gradient right should be blue, got {right:?}"
    );
}

#[tokio::test]
async fn svg_path_evenodd_fill_leaves_a_hole() {
    let html = r##"<style>body{margin:0}</style>
<svg width=100 height=100 viewBox="0 0 100 100">
<path d="M0 0 H100 V100 H0 Z M30 30 H70 V70 H30 Z" fill="#ff0000" fill-rule="evenodd"/>
</svg>"##;
    let (page, rendered) = render(html, 200.0, 120.0).await;
    let node = page.select("svg").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    let body = pixel(
        &rendered.image,
        (rect.x + 10.0) as u32,
        (rect.y + 50.0) as u32,
    );
    let hole = pixel(
        &rendered.image,
        (rect.x + 50.0) as u32,
        (rect.y + 50.0) as u32,
    );
    assert!(
        body[0] > 200 && body[1] < 60,
        "outer path should be filled, got {body:?}"
    );
    assert_eq!(
        hole,
        Rgba([255, 255, 255, 255]),
        "inner subpath should be a hole"
    );
}

#[tokio::test]
async fn svg_pattern_reference_tiles_shapes() {
    let html = r##"<style>body{margin:0}</style>
<svg width=100 height=100 viewBox="0 0 100 100">
<defs>
<pattern id="stripes" patternUnits="userSpaceOnUse" x="0" y="0" width="10" height="10">
<rect x="0" y="0" width="5" height="10" fill="#ff0000"/>
</pattern>
</defs>
<rect x="0" y="0" width="100" height="100" fill="url(#stripes)"/>
</svg>"##;
    let (page, rendered) = render(html, 200.0, 120.0).await;
    let node = page.select("svg").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    let stripe = pixel(
        &rendered.image,
        (rect.x + 2.0) as u32,
        (rect.y + 2.0) as u32,
    );
    let gap = pixel(
        &rendered.image,
        (rect.x + 7.0) as u32,
        (rect.y + 2.0) as u32,
    );
    assert!(
        stripe[0] > 200 && stripe[1] < 60,
        "pattern stripe should be red, got {stripe:?}"
    );
    assert_eq!(
        gap,
        Rgba([255, 255, 255, 255]),
        "pattern gap should show the page background"
    );
}

#[tokio::test]
async fn svg_fill_and_stroke_opacity_are_honored() {
    let html = r##"<style>body{margin:0}</style>
<svg width=100 height=100 viewBox="0 0 100 100">
<rect x="0" y="0" width="100" height="100" fill="#ff0000" fill-opacity="0.5"/>
</svg>"##;
    let (page, rendered) = render(html, 200.0, 120.0).await;
    let node = page.select("svg").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    let blended = pixel(
        &rendered.image,
        (rect.x + 50.0) as u32,
        (rect.y + 50.0) as u32,
    );
    assert!(
        blended[0] > 240 && (60..=200).contains(&blended[1]) && (60..=200).contains(&blended[2]),
        "half-alpha red over white should be pink, got {blended:?}"
    );
}
