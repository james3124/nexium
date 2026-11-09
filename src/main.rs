use std::path::Path;
use std::process::ExitCode;

use nexium::engine::css::Viewport;
use nexium::intelligence::{dump_tree, visible_text};
use nexium::{Browser, Config, NxError, RenderOptions, VERSION};

const HELP: &str = "\
NEXIUM - independent Rust headless browser engine

USAGE:
    nexium <url> [OPTIONS]

OPTIONS:
    --screenshot <path>     Render and save a screenshot (png/jpeg by extension)
    --viewport <WxH>        Rendering viewport (default 1280x720)
    --viewport-shot         Crop the screenshot to the viewport (default: full page)
    --element <selector>    Screenshot only the first matching element
    --dump-html             Print the serialized DOM as HTML
    --dump-dom              Print an indented DOM tree
    --selector <selector>   Query elements with a CSS selector
    --json                  Print a JSON page summary
    --timeout <ms>          Request timeout in milliseconds (default 30000)
    --max-redirects <n>     Redirect limit (default 10)
    --config <path>         Load a TOML configuration file
    -h, --help              Print this help
    -V, --version           Print the version

EXAMPLES:
    nexium https://example.com
    nexium example.com --dump-dom
    nexium https://example.com --screenshot page.png
    nexium https://example.com --screenshot element.png --element \"h1\" --viewport 800x600
";

struct Args {
    url: Option<String>,
    dump_html: bool,
    dump_dom: bool,
    json: bool,
    selector: Option<String>,
    screenshot: Option<String>,
    viewport: Option<(u32, u32)>,
    viewport_shot: bool,
    element: Option<String>,
    timeout_ms: Option<u64>,
    max_redirects: Option<usize>,
    config_path: Option<String>,
}

impl Args {
    fn parse(raw: Vec<String>) -> Result<Self, String> {
        let mut args = Args {
            url: None,
            dump_html: false,
            dump_dom: false,
            json: false,
            selector: None,
            screenshot: None,
            viewport: None,
            viewport_shot: false,
            element: None,
            timeout_ms: None,
            max_redirects: None,
            config_path: None,
        };

        let mut iter = raw.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    print!("{HELP}");
                    std::process::exit(0);
                }
                "-V" | "--version" => {
                    println!("nexium {VERSION}");
                    std::process::exit(0);
                }
                "--dump-html" => args.dump_html = true,
                "--dump-dom" => args.dump_dom = true,
                "--json" => args.json = true,
                "--viewport-shot" => args.viewport_shot = true,
                "--selector" => {
                    let value = iter.next().ok_or("--selector requires a value")?;
                    args.selector = Some(value);
                }
                "--screenshot" => {
                    let value = iter.next().ok_or("--screenshot requires a path")?;
                    args.screenshot = Some(value);
                }
                "--element" => {
                    let value = iter.next().ok_or("--element requires a selector")?;
                    args.element = Some(value);
                }
                "--viewport" => {
                    let value = iter.next().ok_or("--viewport requires WxH")?;
                    let lower = value.to_ascii_lowercase();
                    let (width, height) = lower
                        .split_once('x')
                        .ok_or_else(|| format!("invalid viewport: {value} (expected WxH)"))?;
                    let width: u32 = width
                        .parse()
                        .map_err(|_| format!("invalid viewport width: {width}"))?;
                    let height: u32 = height
                        .parse()
                        .map_err(|_| format!("invalid viewport height: {height}"))?;
                    if width == 0 || height == 0 {
                        return Err("viewport dimensions must be positive".to_owned());
                    }
                    args.viewport = Some((width, height));
                }
                "--timeout" => {
                    let value = iter.next().ok_or("--timeout requires a value")?;
                    args.timeout_ms = Some(
                        value
                            .parse()
                            .map_err(|_| format!("invalid timeout: {value}"))?,
                    );
                }
                "--max-redirects" => {
                    let value = iter.next().ok_or("--max-redirects requires a value")?;
                    args.max_redirects = Some(
                        value
                            .parse()
                            .map_err(|_| format!("invalid redirect limit: {value}"))?,
                    );
                }
                "--config" => {
                    let value = iter.next().ok_or("--config requires a value")?;
                    args.config_path = Some(value);
                }
                other if other.starts_with('-') => {
                    return Err(format!("unknown option: {other}"));
                }
                other => {
                    if args.url.replace(other.to_owned()).is_some() {
                        return Err("only one URL may be given".to_owned());
                    }
                }
            }
        }

        Ok(args)
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    nexium::utils::logging::init();

    let args = match Args::parse(std::env::args().skip(1).collect()) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("nexium: {message}\n\n{HELP}");
            return ExitCode::FAILURE;
        }
    };

    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nexium: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let Some(url) = args.url else {
        eprintln!("nexium: a URL is required\n\n{HELP}");
        std::process::exit(2);
    };

    let config_path = args
        .config_path
        .unwrap_or_else(|| "config/default.toml".to_owned());
    let mut config = Config::load_or_default(Path::new(&config_path))?;
    if let Some(timeout) = args.timeout_ms {
        config.timeout_ms = timeout;
    }
    if let Some(limit) = args.max_redirects {
        config.max_redirects = limit;
    }
    if let Some((width, height)) = args.viewport {
        config.viewport_width = width;
        config.viewport_height = height;
    }
    let viewport = Viewport {
        width: config.viewport_width as f32,
        height: config.viewport_height as f32,
    };

    let browser = Browser::new(config);
    let page = browser.open(&url).await?;

    if let Some(path) = args.screenshot.as_deref() {
        let rendered = browser
            .render(
                &page,
                RenderOptions {
                    viewport,
                    ..RenderOptions::default()
                },
            )
            .await?;

        let image = if let Some(selector) = args.element.as_deref() {
            let node = page
                .select(selector)?
                .ok_or_else(|| NxError::Render(format!("no element matches {selector:?}")))?;
            rendered
                .element_image(node)
                .ok_or_else(|| NxError::Render("element has no rendered box".to_owned()))?
        } else if args.viewport_shot {
            rendered.viewport_image()
        } else {
            rendered.image.clone()
        };

        let path = Path::new(path);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        save_screenshot(&image, path)?;
        println!(
            "Saved {} ({}x{})",
            path.display(),
            image.width(),
            image.height()
        );
        return Ok(());
    }

    if args.dump_html {
        println!("{}", page.document.serialize());
        return Ok(());
    }
    if args.dump_dom {
        print!("{}", dump_tree(&page.document));
        return Ok(());
    }
    if let Some(selector) = args.selector {
        let matches = page.select_all(&selector)?;
        println!("{} match(es) for {selector:?}", matches.len());
        for id in matches.iter().take(20) {
            println!("{}", page.document.outer_html(*id));
        }
        if matches.len() > 20 {
            println!("... {} more", matches.len() - 20);
        }
        return Ok(());
    }

    let title = page.title().unwrap_or_default();
    let text = visible_text(&page.document);
    let excerpt: String = text.chars().take(600).collect();

    if args.json {
        let summary = serde_json::json!({
            "url": page.url.as_str(),
            "status": page.status.as_u16(),
            "title": page.title(),
            "content_type": page.content_type,
            "charset": page.charset,
            "elements": page.document.element_count(),
            "nodes": page.document.node_count(),
            "text": text.chars().take(2000).collect::<String>(),
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("URL:       {}", page.url);
    println!(
        "Status:    {} {}",
        page.status.as_u16(),
        page.status.canonical_reason().unwrap_or("")
    );
    println!("Title:     {title}");
    println!(
        "Type:      {}",
        page.content_type.as_deref().unwrap_or("(unknown)")
    );
    println!("Charset:   {}", page.charset);
    println!("Elements:  {}", page.document.element_count());
    println!("Nodes:     {}", page.document.node_count());
    if !excerpt.is_empty() {
        println!(
            "Text:\n{excerpt}{}",
            if text.chars().count() > 600 {
                "…"
            } else {
                ""
            }
        );
    }

    Ok(())
}

fn save_screenshot(
    image: &image::RgbaImage,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "jpg" | "jpeg") {
        let rgb = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
        let file = std::fs::File::create(path)?;
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 90);
        encoder.encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )?;
    } else {
        image.save(path)?;
    }
    Ok(())
}
