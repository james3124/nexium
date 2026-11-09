//! Browser management. Phase 1 provides page loading; tabs, contexts and
//! sessions arrive in later phases.

pub mod page;

pub use page::Page;

use url::Url;

use crate::config::Config;
use crate::networking::{HttpClient, parse_url};
use crate::rendering::{RenderOptions, RenderedPage, render_page};
use crate::utils::Result;

pub struct Browser {
    config: Config,
    client: HttpClient,
}

impl Browser {
    pub fn new(config: Config) -> Self {
        let client = HttpClient::new(&config);
        Self { config, client }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn client(&self) -> &HttpClient {
        &self.client
    }

    /// Load a URL (bare hosts default to HTTPS).
    pub async fn open(&self, url: &str) -> Result<Page> {
        let url = parse_url(url)?;
        Page::load(&self.client, url).await
    }

    /// Load a page from in-memory markup.
    pub fn load_html(&self, url: &str, html: &str) -> Result<Page> {
        let url: Url = parse_url(url)?;
        Ok(Page::from_html(url, html))
    }

    /// Render a loaded page into an RGBA image.
    pub async fn render(&self, page: &Page, options: RenderOptions) -> Result<RenderedPage> {
        render_page(page, &self.client, options).await
    }
}
