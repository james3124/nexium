use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use encoding_rs::Encoding;
use hyper::StatusCode;
use image::RgbaImage;
use tracing::warn;
use url::Url;

use crate::engine::css::{MediaRule, Rule, Stylesheet, parse_stylesheet};
use crate::engine::dom::{Document, DomError, NodeId};
use crate::engine::html::parse_html;
use crate::engine::javascript::{ConsoleMessage, ScriptSession};
use crate::networking::{HttpClient, Request, Response};
use crate::utils::Result;

const ACCEPT_HTML: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const ACCEPT_CSS: &str = "text/css,*/*;q=0.1";

/// A loaded page: response metadata, parsed DOM and author stylesheets.
#[derive(Clone, Debug)]
pub struct Page {
    pub url: Url,
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub charset: String,
    pub body_len: usize,
    pub is_binary: bool,
    pub console: Vec<ConsoleMessage>,
    pub script_errors: Vec<String>,
    pub document: Document,
    pub stylesheets: Vec<Stylesheet>,
    pub canvases: HashMap<NodeId, RgbaImage>,
    script_session: Option<Arc<ScriptSession>>,
}

impl Page {
    pub async fn load(client: &HttpClient, url: Url) -> Result<Self> {
        let mut request = Request::get(url).header("Accept", ACCEPT_HTML)?;
        request.navigation = true;
        Self::load_request(client, request).await
    }

    /// Load a page from a prepared request (used for link following and form
    /// submission navigation, which may need a POST body).
    pub async fn load_request(client: &HttpClient, request: Request) -> Result<Self> {
        let response = client.fetch(request).await?;
        let mut page = Self::from_response(response);
        page.stylesheets = load_linked_stylesheets(client, &page.document, &page.url).await;
        Ok(page)
    }

    /// Build a top-level navigation request for `url`: an HTML GET, or a
    /// form-encoded POST when `method` is `post`.
    pub fn navigation_request(url: Url, method: &str, body: Option<&str>) -> Result<Request> {
        let mut request = if method.eq_ignore_ascii_case("post") {
            Request::new(hyper::Method::POST, url)
                .header("Accept", ACCEPT_HTML)?
                .header("Content-Type", "application/x-www-form-urlencoded")?
                .body(body.unwrap_or_default().to_owned())
        } else {
            Request::get(url).header("Accept", ACCEPT_HTML)?
        };
        request.navigation = true;
        Ok(request)
    }

    pub fn from_response(response: Response) -> Self {
        let charset_hint = response.charset();
        let content_type = response.content_type().map(str::to_owned);
        let mime = response.mime_type().unwrap_or_default();
        let body_len = response.body.len();

        let (document, charset, is_binary) = if is_html_mime(&mime) {
            let (html, charset) = decode_html(&response.body, charset_hint.as_deref());
            (parse_html(&html), charset, false)
        } else if is_text_mime(&mime) {
            let (text, charset) = decode_text(&response.body, charset_hint.as_deref());
            let escaped = escape_html(&text);
            (parse_html(&format!("<pre>{escaped}</pre>")), charset, false)
        } else {
            (
                parse_html("<html><head></head><body></body></html>"),
                "UTF-8".to_owned(),
                true,
            )
        };
        let stylesheets = collect_inline_stylesheets(&document);
        Self {
            url: response.url,
            status: response.status,
            content_type,
            charset,
            body_len,
            is_binary,
            console: Vec::new(),
            script_errors: Vec::new(),
            document,
            stylesheets,
            canvases: HashMap::new(),
            script_session: None,
        }
    }

    /// Build a page directly from markup (no network access).
    pub fn from_html(url: Url, html: &str) -> Self {
        let document = parse_html(html);
        let stylesheets = collect_inline_stylesheets(&document);
        Self {
            url,
            status: StatusCode::OK,
            content_type: Some("text/html".to_owned()),
            charset: "UTF-8".to_owned(),
            body_len: html.len(),
            is_binary: false,
            console: Vec::new(),
            script_errors: Vec::new(),
            document,
            stylesheets,
            canvases: HashMap::new(),
            script_session: None,
        }
    }

    pub fn has_script_runtime(&self) -> bool {
        self.script_session.is_some()
    }

    pub(crate) fn script_session(&self) -> Option<Arc<ScriptSession>> {
        self.script_session.clone()
    }

    pub(crate) fn set_script_session(&mut self, session: Option<Arc<ScriptSession>>) {
        self.script_session = session;
    }

    pub(crate) fn apply_session_snapshot(
        &mut self,
        result: crate::engine::javascript::ScriptRunResult,
        document: Document,
    ) {
        self.document = document;
        self.console = result.console;
        self.script_errors = result.errors;
        self.canvases = result.canvases;
    }

    pub fn title(&self) -> Option<String> {
        self.document
            .title()
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty())
    }

    pub fn select(&self, selector: &str) -> std::result::Result<Option<NodeId>, DomError> {
        self.document.select(selector)
    }

    pub fn select_all(&self, selector: &str) -> std::result::Result<Vec<NodeId>, DomError> {
        self.document.select_all(selector)
    }

    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// Decode a response body using BOM, HTTP charset, `<meta charset>` and UTF-8
/// fallback, in that order. Returns the text and the encoding name used.
pub fn decode_html(bytes: &[u8], header_charset: Option<&str>) -> (String, String) {
    if let Some((encoding, _)) = Encoding::for_bom(bytes) {
        let (text, _, _) = encoding.decode(bytes);
        return (text.into_owned(), encoding.name().to_owned());
    }

    if let Some(label) = header_charset
        && let Some(encoding) = Encoding::for_label(label.trim().as_bytes())
    {
        let (text, _, _) = encoding.decode(bytes);
        return (text.into_owned(), encoding.name().to_owned());
    }

    if let Some(label) = sniff_meta_charset(bytes)
        && let Some(encoding) = Encoding::for_label(label.as_bytes())
    {
        let (text, _, _) = encoding.decode(bytes);
        return (text.into_owned(), encoding.name().to_owned());
    }

    let (text, _, _) = encoding_rs::UTF_8.decode(bytes);
    (text.into_owned(), "UTF-8".to_owned())
}

fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let prefix = &bytes[..bytes.len().min(1024)];
    let ascii: Vec<u8> = prefix.iter().map(|b| b.to_ascii_lowercase()).collect();
    let text = String::from_utf8_lossy(&ascii);

    let index = text.find("charset")?;
    let rest = &text[index + "charset".len()..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    let rest = rest.trim_start_matches(['"', '\'']);

    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, ';' | '"' | '\'' | '>' | '<'))
        .unwrap_or(rest.len());
    let label = rest[..end].trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_owned())
    }
}

fn collect_inline_stylesheets(doc: &Document) -> Vec<Stylesheet> {
    let mut sheets = Vec::new();
    for id in doc.descendants(doc.document_node()) {
        if doc.tag_name(id) == Some("style") {
            let css = doc.text_content(id);
            let sheet = parse_stylesheet(&css);
            if !sheet.rules.is_empty() {
                sheets.push(sheet);
            }
        }
    }
    sheets
}

/// Load `<style>` blocks and `<link rel="stylesheet">` resources in document
/// order so cascade ordering is preserved.
async fn load_linked_stylesheets(
    client: &HttpClient,
    doc: &Document,
    base: &Url,
) -> Vec<Stylesheet> {
    let mut sheets = Vec::new();
    let mut seen = HashSet::new();
    for id in doc.descendants(doc.document_node()) {
        match doc.tag_name(id) {
            Some("style") => {
                let css = doc.text_content(id);
                let sheet = parse_stylesheet(&css);
                if !sheet.rules.is_empty() {
                    let expanded = expand_imports(client, sheet, base, 0, &mut seen).await;
                    sheets.push(expanded);
                }
            }
            Some("link") => {
                let rel = doc.get_attribute(id, "rel").unwrap_or("");
                let is_stylesheet = rel
                    .split_ascii_whitespace()
                    .any(|part| part.eq_ignore_ascii_case("stylesheet"));
                if !is_stylesheet {
                    continue;
                }
                let Some(href) = doc.get_attribute(id, "href") else {
                    continue;
                };
                let Ok(url) = base.join(href) else {
                    warn!("stylesheet: invalid URL {href}");
                    continue;
                };
                if !matches!(url.scheme(), "http" | "https") {
                    continue;
                }
                if !seen.insert(url.to_string()) {
                    continue;
                }
                if let Some(sheet) = fetch_stylesheet(client, &url).await {
                    let expanded = expand_imports(client, sheet, &url, 0, &mut seen).await;
                    sheets.push(expanded);
                }
            }
            _ => {}
        }
    }
    sheets
}

async fn fetch_stylesheet(client: &HttpClient, url: &Url) -> Option<Stylesheet> {
    let request = Request::get(url.clone())
        .header("Accept", ACCEPT_CSS)
        .ok()?;
    match client.fetch(request).await {
        Ok(response) => {
            if !response.status.is_success() {
                warn!("stylesheet {}: HTTP {}", url, response.status.as_u16());
                return None;
            }
            let css = decode_css(&response.body, response.charset().as_deref());
            let sheet = parse_stylesheet(&css);
            if sheet.rules.is_empty() {
                None
            } else {
                Some(sheet)
            }
        }
        Err(error) => {
            warn!("stylesheet {url}: {error}");
            None
        }
    }
}

fn expand_imports<'a>(
    client: &'a HttpClient,
    sheet: Stylesheet,
    base: &'a Url,
    depth: usize,
    seen: &'a mut HashSet<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Stylesheet> + Send + 'a>> {
    Box::pin(async move {
        if depth >= 8 {
            return Stylesheet {
                rules: Vec::new(),
                namespaces: Vec::new(),
            };
        }
        let namespaces = sheet.namespaces;
        let mut rules = Vec::new();
        for rule in sheet.rules {
            match rule {
                Rule::Import(import) => {
                    let Ok(url) = base.join(&import.url) else {
                        continue;
                    };
                    if !matches!(url.scheme(), "http" | "https") {
                        continue;
                    }
                    if !seen.insert(url.to_string()) {
                        continue;
                    }
                    let Some(imported) = fetch_stylesheet(client, &url).await else {
                        continue;
                    };
                    let imported = expand_imports(client, imported, &url, depth + 1, seen).await;
                    if import.media.is_empty() {
                        rules.extend(imported.rules);
                    } else {
                        rules.push(Rule::Media(MediaRule {
                            queries: import.media,
                            rules: imported.rules,
                        }));
                    }
                }
                other => rules.push(other),
            }
        }
        Stylesheet { rules, namespaces }
    })
}

fn decode_css(bytes: &[u8], charset: Option<&str>) -> String {
    if let Some(label) = charset
        && let Some(encoding) = Encoding::for_label(label.trim().as_bytes())
    {
        let (text, _, _) = encoding.decode(bytes);
        return text.into_owned();
    }
    let (text, _, _) = encoding_rs::UTF_8.decode(bytes);
    text.into_owned()
}

fn is_html_mime(mime: &str) -> bool {
    mime.is_empty() || matches!(mime, "text/html" | "application/xhtml+xml")
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/javascript"
                | "application/xml"
                | "application/x-javascript"
                | "image/svg+xml"
        )
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

fn decode_text(bytes: &[u8], charset: Option<&str>) -> (String, String) {
    if let Some((encoding, _)) = Encoding::for_bom(bytes) {
        let (text, _, _) = encoding.decode(bytes);
        return (text.into_owned(), encoding.name().to_owned());
    }
    if let Some(label) = charset
        && let Some(encoding) = Encoding::for_label(label.trim().as_bytes())
    {
        let (text, _, _) = encoding.decode(bytes);
        return (text.into_owned(), encoding.name().to_owned());
    }
    let (text, _, _) = encoding_rs::UTF_8.decode(bytes);
    (text.into_owned(), "UTF-8".to_owned())
}
