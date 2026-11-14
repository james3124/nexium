//! Browser-wide throughput and memory benchmark.
//!
//! Usage: cargo run --release --example benchmark -- [nodes] [iterations] [soak_seconds]

use std::error::Error;
use std::time::{Duration, Instant};

use nexium::engine::css::Viewport;
use nexium::{Browser, Config, PageSession, RenderOptions};

fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

fn generated_html(nodes: usize) -> String {
    let mut html = String::with_capacity(nodes * 220 + 512);
    html.push_str(
        "<!doctype html><html><head><meta charset=utf-8><style>\
         body{margin:0;font:15px/1.5 sans-serif;background:#f6f7fb;color:#1d2433}\
         .row{display:flex;gap:10px;padding:8px 14px}\
         .card{flex:1;background:#fff;border:1px solid #d8dfec;padding:10px}\
         .card h2{margin:0 0 4px;font-size:16px}\
         .card p{margin:0;color:#55617a}\
         .accent{color:#c0392b;font-weight:bold}\
         </style></head><body>",
    );
    for index in 0..nodes {
        html.push_str("<div class=row><section class=card><h2>Card ");
        html.push_str(&index.to_string());
        html.push_str("</h2><p>Headless layout and paint <span class=accent>");
        html.push_str(&(index % 97).to_string());
        html.push_str(
            "</span> without Chromium.</p><a href=https://example.com/>detail</a></section></div>",
        );
    }
    html.push_str("</body></html>");
    html
}

fn script_html() -> &'static str {
    "<!doctype html><html><body><div id=out></div><script>\
     var total=0;for(var i=0;i<20000;i++){total+=i}\
     document.getElementById('out').textContent=String(total);\
     </script></body></html>"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let nodes: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1500);
    let iterations: u32 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10);
    let soak_seconds: u64 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);

    let viewport = Viewport {
        width: 1280.0,
        height: 720.0,
    };
    let browser = Browser::new(Config::default());
    let html = generated_html(nodes);
    let bytes = html.len();

    let start = Instant::now();
    for _ in 0..iterations {
        let page = browser.load_html("https://bench.nexium.test/", &html)?;
        std::hint::black_box(page.document.node_count());
    }
    let parse_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(iterations);

    let page = browser.load_html("https://bench.nexium.test/", &html)?;
    let options = RenderOptions {
        viewport,
        max_height: 20_000,
        load_images: false,
    };
    let start = Instant::now();
    for _ in 0..iterations {
        let rendered = browser.render(&page, options.clone()).await?;
        std::hint::black_box(rendered.image.width());
    }
    let render_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(iterations);

    let mut session = PageSession::new(
        browser.load_html("https://bench.nexium.test/", script_html())?,
        viewport,
    );
    let expression = "(function(){var s=0;for(var i=0;i<20000;i++){s+=i}return s})()";
    let start = Instant::now();
    for _ in 0..iterations {
        std::hint::black_box(session.evaluate(expression)?);
    }
    let js_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(iterations);

    println!(
        "nodes={nodes} html_kb={:.1} iterations={iterations}",
        bytes as f64 / 1024.0
    );
    println!("parse_ms={parse_ms:.3} render_ms={render_ms:.3} js_ms={js_ms:.3}");
    if let Some(rss) = rss_kb() {
        println!("rss_kb={rss}");
    }

    if soak_seconds > 0 {
        let baseline = rss_kb().unwrap_or(0);
        let deadline = Instant::now() + Duration::from_secs(soak_seconds);
        let mut renders = 0_u64;
        while Instant::now() < deadline {
            let rendered = browser.render(&page, options.clone()).await?;
            std::hint::black_box(rendered.image.width());
            renders += 1;
        }
        let peak = rss_kb().unwrap_or(0);
        println!(
            "soak_s={soak_seconds} renders={renders} rss_delta_kb={}",
            peak.saturating_sub(baseline)
        );
    }
    Ok(())
}
