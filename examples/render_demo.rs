use nexium::engine::css::Viewport;
use nexium::{Browser, Config, RenderOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let html = r#"
<!DOCTYPE html>
<html>
<head>
<title>NexiumX render demo</title>
<style>
  body { margin: 0; font-family: sans-serif; background-color: #f4f6fb; color: #1c2430; }
  header { display: flex; justify-content: space-between; align-items: center;
           background-color: #20304a; color: #ffffff; padding: 14px 24px; }
  header .brand { font-size: 22px; font-weight: bold; }
  nav { display: flex; gap: 18px; }
  nav a { color: #9fc4ff; text-decoration: none; font-size: 14px; }
  .hero { padding: 28px 24px; background-color: #ffffff; margin: 18px 24px;
          border: 1px solid #d7dfef; }
  .hero h1 { margin: 0 0 10px 0; font-size: 30px; }
  .hero p { margin: 0; color: #4a5568; line-height: 1.5; }
  .cards { display: flex; gap: 16px; margin: 0 24px; }
  .card { flex: 1; background-color: #ffffff; border: 1px solid #d7dfef; padding: 16px; }
  .card h2 { font-size: 17px; margin: 0 0 6px 0; color: #20304a; }
  .card p { margin: 0; font-size: 13px; color: #5a6577; line-height: 1.45; }
  .accent { color: #d64545; font-weight: bold; }
  .tag { background-color: #e8f0ff; color: #20304a; padding: 2px 8px; font-size: 12px; }
  form { display: flex; gap: 10px; align-items: center; margin: 22px 24px; }
  input { width: 220px; padding: 6px 8px; border: 1px solid #9aa7bd; }
  button { background-color: #2f6fed; color: #ffffff; border: 1px solid #2f6fed; padding: 7px 16px; }
  footer { margin: 28px 24px; font-size: 12px; color: #7b8496; }
</style>
</head>
<body>
  <header>
    <div class="brand">NexiumX</div>
    <nav>
      <a href="/">Home</a>
      <a href="/docs">Docs</a>
      <a href="/about">About</a>
    </nav>
  </header>

  <section class="hero">
    <h1>Independent Rust browser engine</h1>
    <p>HTML parsing, CSS cascade, block and flex layout, text shaping and
       headless rasterization &mdash; without Chromium, WebKit or Gecko.</p>
  </section>

  <div class="cards">
    <div class="card">
      <h2>Phase 1</h2>
      <p>DOM, HTML and HTTP(S) networking are implemented in Rust.</p>
    </div>
    <div class="card">
      <h2>Phase 2 <span class="tag">now</span></h2>
      <p>CSS cascade and box layout render real pages to PNG and JPEG.</p>
    </div>
    <div class="card">
      <h2>Next</h2>
      <p>JavaScript bindings, interaction and MCP <span class="accent">agents</span>.</p>
    </div>
  </div>

  <form>
    <input type="text" value="Search">
    <button>Go</button>
  </form>

  <footer>&copy; 2026 NexiumX &middot; headless rendering demo</footer>
</body>
</html>
"#;

    let browser = Browser::new(Config::default());
    let page = browser.load_html("https://demo.nexium.test/", html)?;
    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: Viewport {
                    width: 900.0,
                    height: 680.0,
                },
                ..RenderOptions::default()
            },
        )
        .await?;
    rendered.save_png("/tmp/nexium-demo.png")?;
    println!(
        "saved {}x{}",
        rendered.image.width(),
        rendered.image.height()
    );
    Ok(())
}
