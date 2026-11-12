use std::io::Cursor;

use base64::Engine as _;
use image::{ImageFormat, Rgba, RgbaImage};
use nexium::engine::css::Viewport;
use nexium::{Browser, Config, Page, RenderOptions, RenderedPage};

async fn render(html: &str, width: f32, height: f32) -> (Page, RenderedPage) {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html("https://render.test/", html)
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

fn pixel(image: &RgbaImage, x: u32, y: u32) -> Rgba<u8> {
    *image.get_pixel(x.min(image.width() - 1), y.min(image.height() - 1))
}

#[tokio::test]
async fn canvas_covers_the_document() {
    let html = "<style>body{margin:0} #tall{height:900px}</style><div id=tall></div>";
    let (_, rendered) = render(html, 800.0, 600.0).await;
    assert_eq!(rendered.image.width(), 800);
    assert!(rendered.image.height() >= 900);
}

#[tokio::test]
async fn svg_shapes_are_painted() {
    let html = "<style>body{margin:0}</style><svg width=100 height=100 viewBox=\"0 0 100 100\"><rect x=10 y=10 width=40 height=40 fill=\"#ff0000\"/><circle cx=70 cy=70 r=20 fill=\"blue\"/></svg>";
    let (page, rendered) = render(html, 200.0, 120.0).await;
    let node = page.select("svg").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    assert_eq!(
        pixel(
            &rendered.image,
            (rect.x + 20.0) as u32,
            (rect.y + 20.0) as u32
        ),
        Rgba([255, 0, 0, 255])
    );
    assert_eq!(
        pixel(
            &rendered.image,
            (rect.x + 78.0) as u32,
            (rect.y + 78.0) as u32
        ),
        Rgba([0, 0, 255, 255])
    );
}

#[tokio::test]
async fn canvas_2d_drawing_is_painted() {
    let html = "<style>body{margin:0}</style><canvas id=c width=100 height=50></canvas><script>var ctx=document.getElementById('c').getContext('2d');ctx.fillStyle='red';ctx.fillRect(10,10,30,20);</script>";
    let (page, rendered) = render(html, 200.0, 100.0).await;
    let node = page.select("#c").unwrap().unwrap();
    assert!(page.canvases.contains_key(&node));
    let rect = rendered.layout.node_rect(node).unwrap();
    assert_eq!(
        pixel(
            &rendered.image,
            (rect.x + 15.0) as u32,
            (rect.y + 15.0) as u32
        ),
        Rgba([255, 0, 0, 255])
    );
}

#[tokio::test]
async fn body_background_propagates_to_canvas() {
    let html = "<body style=\"margin:0;background-color:#3366ff\"><p>x</p></body>";
    let (_, rendered) = render(html, 200.0, 100.0).await;
    assert_eq!(pixel(&rendered.image, 10, 10), Rgba([51, 102, 255, 255]));
}

#[tokio::test]
async fn text_is_rasterized_inside_its_box() {
    let html = "<style>body{margin:0} h1{margin:0;font-size:40px;color:#ff0000;width:300px}</style><h1>Hello</h1>";
    let (page, rendered) = render(html, 400.0, 200.0).await;
    let node = page.select("h1").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    assert!(rect.height > 30.0);

    let mut found = false;
    let x0 = rect.x as u32;
    let y0 = rect.y as u32;
    for y in y0..(rect.bottom() as u32).min(rendered.image.height()) {
        for x in x0..(rect.right() as u32).min(rendered.image.width()) {
            let px = pixel(&rendered.image, x, y);
            if px[0] > 150 && px[1] < 100 && px[2] < 100 {
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    assert!(found, "expected red glyph pixels inside {rect:?}");
}

#[tokio::test]
async fn element_and_viewport_screenshots_crop_correctly() {
    let html = "<style>body{margin:0} #tall{height:900px;background:#eeeeee} h1{margin:0;font-size:20px}</style><h1>Title</h1><div id=tall></div>";
    let (page, rendered) = render(html, 640.0, 480.0).await;

    let viewport = rendered.viewport_image();
    assert_eq!(viewport.width(), 640);
    assert_eq!(viewport.height(), 480);

    let node = page.select("h1").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    let element = rendered.element_image(node).expect("element image");
    assert!((element.width() as f32 - rect.width.round()).abs() <= 1.0);
    assert!((element.height() as f32 - rect.height.round()).abs() <= 1.0);
}

#[tokio::test]
async fn encodes_png_and_jpeg() {
    let html = "<body style=\"margin:0;background:#00aa00\">hello</body>";
    let (_, rendered) = render(html, 120.0, 80.0).await;

    let png = rendered.png_bytes().unwrap();
    assert_eq!(&png[..4], b"\x89PNG");
    let jpeg = rendered.jpeg_bytes(80).unwrap();
    assert_eq!(&jpeg[..2], b"\xff\xd8");

    let dir = std::env::temp_dir().join("nexium-render-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let png_path = dir.join("shot.png");
    let jpg_path = dir.join("shot.jpg");
    rendered.save(&png_path).unwrap();
    rendered.save(&jpg_path).unwrap();
    assert!(std::fs::metadata(&png_path).unwrap().len() > 50);
    assert!(std::fs::metadata(&jpg_path).unwrap().len() > 50);
    let _ = std::fs::remove_file(png_path);
    let _ = std::fs::remove_file(jpg_path);
}

#[tokio::test]
async fn renders_data_url_images() {
    let mut source = RgbaImage::new(2, 2);
    for pixel in source.pixels_mut() {
        *pixel = Rgba([255, 0, 0, 255]);
    }
    let mut png = Cursor::new(Vec::new());
    source.write_to(&mut png, ImageFormat::Png).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());

    let html = format!(
        "<style>body{{margin:0}} img{{width:20px;height:20px}}</style><img id=i src=\"data:image/png;base64,{encoded}\">"
    );
    let (page, rendered) = render(&html, 100.0, 100.0).await;
    let node = page.select("#i").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    assert!((rect.width - 20.0).abs() < 0.5);
    let center = pixel(
        &rendered.image,
        (rect.x + 10.0) as u32,
        (rect.y + 10.0) as u32,
    );
    assert_eq!(center, Rgba([255, 0, 0, 255]), "image rect {rect:?}");
}

#[tokio::test]
async fn resizing_and_diffing_screenshots() {
    let html = "<body style=\"margin:0;background:#ffffff\">x</body>";
    let (_, rendered) = render(html, 200.0, 100.0).await;
    let resized = rendered.resized(100, 50);
    assert_eq!(resized.dimensions(), (100, 50));
    assert_eq!(rendered.diff(&rendered.image), 0.0);

    let mut other = rendered.image.clone();
    other.put_pixel(0, 0, Rgba([0, 0, 0, 255]));
    assert!(rendered.diff(&other) > 0.0);
}

#[tokio::test]
async fn display_none_is_not_painted() {
    let html = "<style>body{margin:0} #hidden{display:none;width:100px;height:100px;background:#ff0000}</style><div id=hidden></div>";
    let (_, rendered) = render(html, 200.0, 100.0).await;
    for y in 0..50 {
        for x in 0..50 {
            assert_ne!(pixel(&rendered.image, x, y), Rgba([255, 0, 0, 255]));
        }
    }
}

#[tokio::test]
async fn absolute_badge_paints_on_top() {
    let html = "<style>body{margin:0} #p{position:relative;width:200px;height:100px;background:#ffffff} #b{position:absolute;left:20px;top:10px;width:50px;height:30px;background:#0000ff}</style><div id=p><div id=b></div></div>";
    let (page, rendered) = render(html, 300.0, 200.0).await;
    let node = page.select("#b").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    assert_eq!(
        pixel(
            &rendered.image,
            (rect.x + 5.0) as u32,
            (rect.y + 5.0) as u32
        ),
        Rgba([0, 0, 255, 255])
    );
}

#[tokio::test]
async fn generated_content_is_painted() {
    let html = "<style>body{margin:0} h1{margin:0;font-size:40px;color:#000000;width:300px} h1::before{content:\"R\";color:#ff0000}</style><h1>H</h1>";
    let (page, rendered) = render(html, 400.0, 200.0).await;
    let node = page.select("h1").unwrap().unwrap();
    let rect = rendered.layout.node_rect(node).unwrap();
    let mut found = false;
    for y in rect.y as u32..(rect.bottom() as u32).min(rendered.image.height()) {
        for x in rect.x as u32..(rect.right() as u32).min(rendered.image.width()) {
            let px = pixel(&rendered.image, x, y);
            if px[0] > 150 && px[1] < 100 && px[2] < 100 {
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    assert!(found, "expected red ::before glyph pixels");
}

#[tokio::test]
async fn border_radius_clips_backgrounds() {
    let html = "<style>body{margin:0} #r{width:100px;height:100px;background:#ff0000;border-radius:50px}</style><div id=r></div>";
    let (_, rendered) = render(html, 200.0, 200.0).await;
    assert_eq!(pixel(&rendered.image, 2, 2), Rgba([255, 255, 255, 255]));
    assert_eq!(pixel(&rendered.image, 50, 50), Rgba([255, 0, 0, 255]));
}

#[tokio::test]
async fn dashed_borders_have_gaps() {
    let html = "<style>body{margin:0} #d{width:100px;height:40px;border:4px dashed #ff0000;background:#ffffff;box-sizing:border-box}</style><div id=d></div>";
    let (_, rendered) = render(html, 200.0, 100.0).await;
    let mut red = 0;
    let mut white = 0;
    for x in 0..100 {
        let px = pixel(&rendered.image, x, 1);
        if px == Rgba([255, 0, 0, 255]) {
            red += 1;
        } else if px == Rgba([255, 255, 255, 255]) {
            white += 1;
        }
    }
    assert!(red > 0 && white > 0, "red={red} white={white}");
}

#[tokio::test]
async fn background_images_repeat() {
    let mut source = RgbaImage::new(2, 2);
    for pixel in source.pixels_mut() {
        *pixel = Rgba([255, 0, 0, 255]);
    }
    let mut png = Cursor::new(Vec::new());
    source.write_to(&mut png, ImageFormat::Png).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let html = format!(
        "<style>body{{margin:0}} #r{{width:20px;height:20px;background-image:url('data:image/png;base64,{encoded}');background-repeat:repeat}}</style><div id=r></div>"
    );
    let (_, rendered) = render(&html, 100.0, 100.0).await;
    for (x, y) in [(1, 1), (10, 10), (18, 18)] {
        assert_eq!(pixel(&rendered.image, x, y), Rgba([255, 0, 0, 255]));
    }
}

#[tokio::test]
async fn z_index_orders_positioned_children() {
    let html = "<style>body{margin:0} #p{position:relative;width:100px;height:100px;background:#ffffff} .b{position:absolute;left:0;top:0;width:50px;height:50px} #r{background:#ff0000;z-index:-1} #g{background:#00ff00;z-index:0} #b{background:#0000ff;z-index:5}</style><div id=p><div class=b id=r></div><div class=b id=g></div><div class=b id=b></div></div>";
    let (_, rendered) = render(html, 150.0, 150.0).await;
    assert_eq!(pixel(&rendered.image, 10, 10), Rgba([0, 0, 255, 255]));
}
