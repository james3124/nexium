use nexium::{Browser, Config, RenderOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .ok_or("usage: scroll_shot <url> <out.png> [scroll_y]")?;
    let out = args.next().ok_or("missing output path")?;
    let scroll_y: f32 = args
        .next()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(600.0);

    let mut browser = Browser::new(Config::default());
    browser.set_viewport(1280, 800)?;
    browser.open_tab(&url).await?;
    let scroll = browser.scroll_to(0.0, scroll_y)?;
    let page = browser
        .active_page()
        .cloned()
        .ok_or("active tab has no page")?;
    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: browser.viewport(),
                scroll_y: scroll.1,
                ..RenderOptions::default()
            },
        )
        .await?;
    let image = rendered.viewport_image_at(0.0, scroll.1);
    nexium::rendering::save_image(&image, &out)?;
    println!(
        "scrolled to ({:.0}, {:.0}) -> {out} ({}x{})",
        scroll.0,
        scroll.1,
        image.width(),
        image.height()
    );
    Ok(())
}
