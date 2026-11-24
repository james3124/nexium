use image::{Rgba, RgbaImage};
use nexium::engine::css::Viewport;
use nexium::intelligence::visual::{VisualIndex, compare_images};
use nexium::{Browser, Config, Page, RenderOptions, RenderedPage};

async fn render(html: &str, width: f32, height: f32, scroll_y: f32) -> (Page, RenderedPage) {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html("https://render.test/", html)
        .expect("page");
    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: Viewport { width, height },
                scroll_y,
                ..RenderOptions::default()
            },
        )
        .await
        .expect("render");
    (page, rendered)
}

#[tokio::test]
async fn text_runs_carry_screen_coordinates() {
    let html = "<body style='margin:0;background:#fff;color:#000'><p style='font:20px sans-serif'>Hello vision world</p></body>";
    let (page, rendered) = render(html, 400.0, 200.0, 0.0).await;
    let index = VisualIndex::new(
        &page.document,
        &rendered.layout,
        &rendered.image,
        rendered.viewport,
        (0.0, 0.0),
    );

    let runs = index.text_runs();
    assert!(
        runs.iter().any(|run| run.text.contains("vision")),
        "expected a text run containing 'vision', got {runs:?}"
    );
    let matches = index.find_text("vision world");
    assert!(!matches.is_empty(), "expected to find 'vision world'");
    let found = &matches[0];
    assert!(found.rect.width > 10.0 && found.rect.height > 5.0);
    assert_eq!(found.viewport_rect, found.rect);
}

#[tokio::test]
async fn text_matching_joins_inline_fragments() {
    let html =
        "<body style='margin:0;background:#fff'><p>Hello <span>vision</span> world</p></body>";
    let (page, rendered) = render(html, 400.0, 200.0, 0.0).await;
    let index = VisualIndex::new(
        &page.document,
        &rendered.layout,
        &rendered.image,
        rendered.viewport,
        (0.0, 0.0),
    );
    let matches = index.find_text("Hello vision");
    assert!(
        matches.iter().any(|found| found.text == "Hello vision"),
        "expected a cross-fragment match, got {matches:?}"
    );
}

#[tokio::test]
async fn natural_language_description_finds_a_button() {
    let html = "<body style='margin:0;background:#fff'><button id='go' style='background:#2ea043;color:#fff'>Submit</button><a id='signin' href='#'>Sign in</a></body>";
    let (page, rendered) = render(html, 400.0, 200.0, 0.0).await;
    let index = VisualIndex::new(
        &page.document,
        &rendered.layout,
        &rendered.image,
        rendered.viewport,
        (0.0, 0.0),
    );

    let matches = index.find("green Submit button", 3, None);
    assert!(!matches.is_empty(), "expected a button match");
    let best = &matches[0];
    assert_eq!(best.element.tag, "button");
    assert_eq!(best.element.label, "Submit");
    assert!(best.element.clickable);
    assert!(best.score > 0.0);
    assert!(
        best.element
            .signals
            .iter()
            .any(|signal| signal.starts_with("bg:")),
        "expected a sampled background colour, got {:?}",
        best.element.signals
    );

    let link = index.find("Sign in link", 3, None);
    assert_eq!(link[0].element.tag, "a");
}

#[tokio::test]
async fn viewport_rects_follow_the_scroll_offset() {
    let html = "<body style='margin:0;background:#fff'><div style='height:600px'></div><button id='low'>Low button</button></body>";
    let (page, rendered) = render(html, 300.0, 200.0, 250.0).await;
    let index = VisualIndex::new(
        &page.document,
        &rendered.layout,
        &rendered.image,
        rendered.viewport,
        (0.0, 250.0),
    );
    let element = index
        .elements()
        .iter()
        .find(|element| element.label == "Low button")
        .expect("element");
    assert_eq!(element.viewport_rect.y, element.rect.y - 250.0);
    assert!(element.center.1 >= 0.0 && element.center.1 <= 200.0);
}

#[tokio::test]
async fn element_at_point_returns_the_deepest_hit() {
    let html = "<body style='margin:0;background:#fff'><div id='outer' style='width:200px;height:100px'><button id='inner' style='width:60px;height:30px'>X</button></div></body>";
    let (page, rendered) = render(html, 300.0, 200.0, 0.0).await;
    let index = VisualIndex::new(
        &page.document,
        &rendered.layout,
        &rendered.image,
        rendered.viewport,
        (0.0, 0.0),
    );
    let button = index
        .elements()
        .iter()
        .find(|element| element.tag == "button")
        .expect("button");
    let hit = index
        .element_at_point(button.center.0, button.center.1)
        .expect("hit");
    assert_eq!(hit.label, "X");
    assert_eq!(hit.tag, "button");
}

#[tokio::test]
async fn diff_regions_are_grouped_and_unchanged_images_are_quiet() {
    let mut before = RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 255]));
    let after = before.clone();
    let result = compare_images(&before, &after, 8, 10);
    assert_eq!(result.ratio, 0.0);
    assert!(result.changed_regions.is_empty());

    for y in 10..20 {
        for x in 12..24 {
            before.put_pixel(x, y, Rgba([255, 0, 0, 255]));
        }
    }
    let result = compare_images(&before, &after, 8, 10);
    assert!(result.ratio > 0.0);
    assert_eq!(result.changed_regions.len(), 1);
    let region = &result.changed_regions[0];
    assert!(region.rect.x <= 12.0 && region.rect.y <= 10.0);
    assert!(region.rect.x + region.rect.width >= 24.0);
    assert!(region.rect.y + region.rect.height >= 20.0);
    assert!(region.pixels >= 120);
}
