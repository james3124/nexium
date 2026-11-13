//! Browser management: profiles, contexts, tabs, navigation history and
//! session persistence.

pub mod page;

pub use page::Page;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::Config;
use crate::engine::css::Viewport;
use crate::engine::javascript::{
    ScriptOptions, run_page_scripts_inline_persistent, run_page_scripts_persistent,
};
use crate::interaction::{
    AccessibilityNode, DomSnapshot, ElementInfo, ExtractedPage, InputResult, MouseButton,
    PageSession, extract_page,
};
use crate::networking::{HttpClient, Request, parse_url};
use crate::rendering::{RenderOptions, RenderedPage, render_page};
use crate::storage::{HistoryEntry, Profile, read_locked, write_atomic};
use crate::utils::{NxError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub page: Option<Page>,
    pub history: Vec<Url>,
    pub history_index: usize,
    pub session_storage: HashMap<String, String>,
    pub title: Option<String>,
    pub scroll_x: f32,
    pub scroll_y: f32,
}

impl Tab {
    fn empty(id: TabId) -> Self {
        Self {
            id,
            page: None,
            history: Vec::new(),
            history_index: 0,
            session_storage: HashMap::new(),
            title: None,
            scroll_x: 0.0,
            scroll_y: 0.0,
        }
    }

    pub fn url(&self) -> Option<&Url> {
        self.page
            .as_ref()
            .map(|page| &page.url)
            .or_else(|| self.history.get(self.history_index))
    }

    pub fn can_go_back(&self) -> bool {
        self.history_index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }
}

#[derive(Serialize, Deserialize)]
struct SessionData {
    active: usize,
    tabs: Vec<SessionTab>,
}

#[derive(Serialize, Deserialize)]
struct SessionTab {
    url: String,
    history: Vec<String>,
    history_index: usize,
    session_storage: HashMap<String, String>,
    #[serde(default)]
    scroll_x: f32,
    #[serde(default)]
    scroll_y: f32,
}

pub struct Browser {
    config: Config,
    client: Arc<HttpClient>,
    profile: Option<Profile>,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
}

impl Browser {
    pub fn new(config: Config) -> Self {
        let client = Arc::new(HttpClient::new(&config));
        Self {
            config,
            client,
            profile: None,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
        }
    }

    /// Open a browser context using a persistent profile under `data_dir`.
    pub fn with_profile(config: Config, name: &str) -> Result<Self> {
        let profile = Profile::open(&config.data_dir, name)?;
        let preferences = profile.preferences()?;
        let mut config = config;
        if let Some(user_agent) = preferences.user_agent {
            config.user_agent = user_agent;
        }
        let client = Arc::new(HttpClient::new(&config));
        if let Ok(records) = profile.load_cookies() {
            client.restore_cookies(&records);
        }
        let _ = client.set_cache_dir(&profile.cache_dir());
        Ok(Self {
            config,
            client,
            profile: Some(profile),
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn client(&self) -> &HttpClient {
        &self.client
    }

    pub fn profile(&self) -> Option<&Profile> {
        self.profile.as_ref()
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_page(&self) -> Option<&Page> {
        self.active_tab().and_then(|tab| tab.page.as_ref())
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn history(&self) -> Vec<HistoryEntry> {
        self.profile
            .as_ref()
            .and_then(|profile| profile.history().ok())
            .unwrap_or_default()
    }

    pub fn viewport(&self) -> Viewport {
        Viewport {
            width: self.config.viewport_width.max(1) as f32,
            height: self.config.viewport_height.max(1) as f32,
        }
    }

    pub fn set_viewport(&mut self, width: u32, height: u32) -> Result<()> {
        self.config.viewport_width = width.max(1);
        self.config.viewport_height = height.max(1);
        if self.active_page().is_some() {
            self.with_page_session(|_| ())?;
        }
        Ok(())
    }

    pub fn scroll_to(&mut self, x: f32, y: f32) -> Result<(f32, f32)> {
        self.with_page_session(|session| {
            session.scroll_to(x, y);
            session.scroll()
        })
    }

    pub fn scroll_by(&mut self, dx: f32, dy: f32) -> Result<(f32, f32)> {
        self.with_page_session(|session| {
            session.scroll_by(dx, dy);
            session.scroll()
        })
    }

    pub fn element_at(&mut self, x: f32, y: f32) -> Result<Option<ElementInfo>> {
        self.with_page_session(|session| session.element_at(x, y))
    }

    pub fn element(&mut self, selector: &str) -> Result<Option<ElementInfo>> {
        self.with_page_session(|session| session.find(selector))
    }

    pub fn elements(&mut self, selector: &str) -> Result<Vec<ElementInfo>> {
        self.with_page_session(|session| {
            session
                .page()
                .select_all(selector)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|node| session.element_info(node))
                .collect()
        })
    }

    pub fn element_info_node(&mut self, index: usize) -> Result<Option<ElementInfo>> {
        self.with_page_session(|session| {
            session.element_info(crate::engine::dom::NodeId::from_index(index))
        })
    }

    pub fn node_outer_html(&mut self, index: usize) -> Result<Option<String>> {
        self.with_page_session(|session| {
            let document = &session.page().document;
            if index >= document.node_count() {
                return None;
            }
            Some(document.serialize_node(crate::engine::dom::NodeId::from_index(index)))
        })
    }

    pub fn node_attributes(&mut self, index: usize) -> Result<Vec<(String, String)>> {
        self.with_page_session(|session| {
            let document = &session.page().document;
            document
                .element(crate::engine::dom::NodeId::from_index(index))
                .map(|element| {
                    element
                        .attributes
                        .iter()
                        .map(|attribute| (attribute.name.clone(), attribute.value.clone()))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    pub fn node_query_in(&mut self, root: usize, selector: &str, all: bool) -> Result<Vec<usize>> {
        self.with_page_session(|session| {
            let document = &session.page().document;
            let root = crate::engine::dom::NodeId::from_index(root);
            let nodes = if all {
                document
                    .query_selector_all(root, selector)
                    .unwrap_or_default()
            } else {
                document
                    .query_selector(root, selector)
                    .ok()
                    .flatten()
                    .into_iter()
                    .collect()
            };
            nodes.into_iter().map(|node| node.index()).collect()
        })
    }

    /// Build a unique CSS selector path (`div:nth-of-type(2) > a`) for a node.
    pub fn node_selector(&mut self, index: usize) -> Result<Option<String>> {
        self.with_page_session(|session| {
            node_selector_path(
                &session.page().document,
                crate::engine::dom::NodeId::from_index(index),
            )
        })
    }

    pub fn scroll_element_into_view(&mut self, selector: &str) -> Result<bool> {
        self.with_page_session(|session| {
            let Some(node) = session.page().select(selector).ok().flatten() else {
                return false;
            };
            session.scroll_into_view(node)
        })
    }

    pub fn click(&mut self, x: f32, y: f32) -> Result<InputResult> {
        self.click_with_button(x, y, MouseButton::Left)
    }

    pub fn click_with_button(
        &mut self,
        x: f32,
        y: f32,
        button: MouseButton,
    ) -> Result<InputResult> {
        self.with_page_session(|session| session.click_with_button(x, y, button))
    }

    /// Click and return any navigation the click requested (links, submit
    /// buttons). Callers decide whether to follow it.
    pub fn click_with_navigation(
        &mut self,
        x: f32,
        y: f32,
        button: MouseButton,
    ) -> Result<(InputResult, Option<crate::interaction::Navigation>)> {
        self.with_page_session(|session| {
            let result = session.click_with_button(x, y, button);
            let navigation = session.take_navigation();
            (result, navigation)
        })
    }

    pub fn double_click(&mut self, x: f32, y: f32) -> Result<InputResult> {
        self.with_page_session(|session| session.double_click(x, y))
    }

    pub fn hover(&mut self, x: f32, y: f32) -> Result<InputResult> {
        self.with_page_session(|session| session.hover(x, y))
    }

    pub fn wheel(&mut self, x: f32, y: f32, delta_x: f32, delta_y: f32) -> Result<InputResult> {
        self.with_page_session(|session| session.wheel(x, y, delta_x, delta_y))
    }

    pub fn press_key(&mut self, key: &str) -> Result<InputResult> {
        self.with_page_session(|session| session.press_key(key))
    }

    pub fn type_text(&mut self, text: &str) -> Result<InputResult> {
        self.with_page_session(|session| session.type_text(text))
    }

    pub fn accessibility_tree(&mut self) -> Result<AccessibilityNode> {
        self.with_page_session(|session| session.accessibility_tree())
    }

    pub fn dom_snapshot(&mut self) -> Result<DomSnapshot> {
        self.with_page_session(|session| session.dom_snapshot())
    }

    pub fn extract(&mut self) -> Result<ExtractedPage> {
        self.with_page_session(|session| extract_page(session.page()))
    }

    pub fn evaluate(&mut self, code: &str) -> Result<serde_json::Value> {
        self.with_page_session(|session| session.evaluate(code))?
    }

    fn with_page_session<R>(&mut self, action: impl FnOnce(&mut PageSession) -> R) -> Result<R> {
        let index = self.active;
        let viewport = self.viewport();
        let (page, scroll_x, scroll_y) = {
            let tab = self
                .tabs
                .get(index)
                .ok_or_else(|| NxError::Browser("no active tab".to_owned()))?;
            let page = tab
                .page
                .clone()
                .ok_or_else(|| NxError::Browser("active tab has no page".to_owned()))?;
            (page, tab.scroll_x, tab.scroll_y)
        };
        let mut session = PageSession::new(page, viewport);
        session.scroll_to(scroll_x, scroll_y);
        let result = action(&mut session);
        let (scroll_x, scroll_y) = session.scroll();
        let session_storage = session.session_storage().clone();
        let local_storage = session.local_storage().clone();
        let origin = session.page().url.origin().ascii_serialization();
        let title = session.page().title();
        let page = session.into_page();
        {
            let tab = &mut self.tabs[index];
            tab.scroll_x = scroll_x;
            tab.scroll_y = scroll_y;
            tab.session_storage = session_storage;
            tab.title = title;
            tab.page = Some(page);
        }
        if let Some(profile) = &self.profile {
            let _ = profile.save_local_storage(&origin, &local_storage);
            let _ = profile.save_cookies(&self.client.cookie_records());
        }
        Ok(result)
    }

    fn script_options(
        &self,
        local_storage: HashMap<String, String>,
        session_storage: HashMap<String, String>,
        indexeddb_dir: Option<std::path::PathBuf>,
    ) -> ScriptOptions {
        ScriptOptions {
            enabled: self.config.javascript_enabled,
            user_agent: self.config.user_agent.clone(),
            max_runtime: Duration::from_millis(self.config.javascript_max_runtime_ms),
            viewport_width: self.config.viewport_width as f64,
            viewport_height: self.config.viewport_height as f64,
            local_storage,
            session_storage,
            indexeddb_dir,
        }
    }

    async fn load_into_tab(&self, tab: &mut Tab, url: Url) -> Result<Page> {
        let request = Request::get(url);
        self.load_request_into_tab(tab, request).await
    }

    async fn load_request_into_tab(&self, tab: &mut Tab, request: Request) -> Result<Page> {
        let mut page = Page::load_request(&self.client, request).await?;
        let origin = page.url.origin().ascii_serialization();
        let local_storage = match &self.profile {
            Some(profile) => profile.load_local_storage(&origin)?,
            None => HashMap::new(),
        };
        let indexeddb_dir = match &self.profile {
            Some(profile) => Some(profile.indexeddb_dir(&origin)?),
            None => None,
        };
        let options =
            self.script_options(local_storage, tab.session_storage.clone(), indexeddb_dir);
        let (result, session) = run_page_scripts_persistent(
            &page.url,
            &mut page.document,
            Some(Arc::clone(&self.client)),
            &options,
        )
        .await;
        page.console = result.console;
        page.script_errors = result.errors;
        page.canvases = result.canvases;
        page.set_script_session(session.map(Arc::new));
        tab.session_storage = result.session_storage;

        if let Some(profile) = &self.profile {
            profile.save_local_storage(&origin, &result.local_storage)?;
            profile.save_cookies(&self.client.cookie_records())?;
            profile.append_history(page.url.as_str(), page.title().as_deref())?;
        }
        tab.title = page.title();
        Ok(page)
    }

    /// Load a URL without creating a persistent tab.
    pub async fn open(&self, url: &str) -> Result<Page> {
        let url = parse_url(url)?;
        let mut tab = Tab::empty(TabId(0));
        self.load_into_tab(&mut tab, url).await
    }

    /// Load a page from in-memory markup and run inline scripts.
    pub fn load_html(&self, url: &str, html: &str) -> Result<Page> {
        let url: Url = parse_url(url)?;
        Ok(self.load_offline(url, html))
    }

    fn load_offline(&self, url: Url, html: &str) -> Page {
        let mut page = Page::from_html(url, html);
        let options = self.script_options(HashMap::new(), HashMap::new(), None);
        let (result, session) =
            run_page_scripts_inline_persistent(&page.url, &mut page.document, &options);
        page.console = result.console;
        page.script_errors = result.errors;
        page.canvases = result.canvases;
        page.set_script_session(session.map(Arc::new));
        page
    }

    /// Open a URL in a new tab.
    pub async fn open_tab(&mut self, url: &str) -> Result<TabId> {
        self.check_tab_limit()?;
        let url = parse_url(url)?;
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        let mut tab = Tab::empty(id);
        let page = self.load_into_tab(&mut tab, url).await?;
        tab.history = vec![page.url.clone()];
        tab.history_index = 0;
        tab.title = page.title();
        tab.page = Some(page);
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        Ok(id)
    }

    /// Create an empty `about:blank` tab.
    pub fn new_blank_tab(&mut self) -> Result<TabId> {
        self.check_tab_limit()?;
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        let mut tab = Tab::empty(id);
        if let Ok(url) = Url::parse("about:blank") {
            tab.page = Some(self.load_offline(url, "<html><head></head><body></body></html>"));
        }
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        Ok(id)
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    fn check_tab_limit(&self) -> Result<()> {
        let limit = self.config.max_tabs.max(1);
        if self.tabs.len() >= limit {
            return Err(NxError::Browser(format!("tab limit reached ({limit})")));
        }
        Ok(())
    }

    pub fn switch_tab(&mut self, id: TabId) -> bool {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
            self.active = index;
            true
        } else {
            false
        }
    }

    pub fn close_tab(&mut self, id: TabId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if index < self.active {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        true
    }

    /// Navigate the active tab, recording a new history entry.
    pub async fn navigate(&mut self, url: &str) -> Result<Page> {
        let url = parse_url(url)?;
        let request = Request::get(url);
        self.navigate_request(request).await
    }

    /// Follow a click-generated navigation (link GET or form GET/POST).
    pub async fn navigate_navigation(
        &mut self,
        navigation: crate::interaction::Navigation,
    ) -> Result<Page> {
        let url = parse_url(&navigation.url)?;
        let request =
            Page::navigation_request(url, &navigation.method, navigation.body.as_deref())?;
        self.navigate_request(request).await
    }

    async fn navigate_request(&mut self, request: Request) -> Result<Page> {
        if self.tabs.is_empty() {
            return Err(NxError::Browser("no active tab".to_owned()));
        }
        let index = self.active;
        let mut tab = self.tabs.remove(index);
        tab.history.truncate(tab.history_index + 1);
        let result = self.load_request_into_tab(&mut tab, request).await;
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                self.tabs.insert(index, tab);
                return Err(error);
            }
        };
        tab.history.push(page.url.clone());
        tab.history_index = tab.history.len() - 1;
        tab.title = page.title();
        tab.page = Some(page.clone());
        self.tabs.insert(index, tab);
        Ok(page)
    }

    pub async fn back(&mut self) -> Result<Option<Page>> {
        if self.tabs.is_empty() {
            return Err(NxError::Browser("no active tab".to_owned()));
        }
        let index = self.active;
        let mut tab = self.tabs.remove(index);
        if tab.history_index == 0 {
            self.tabs.insert(index, tab);
            return Ok(None);
        }
        tab.history_index -= 1;
        let target = tab.history_index;
        let url = tab.history[target].clone();
        let result = self.load_into_tab(&mut tab, url).await;
        match result {
            Ok(page) => {
                tab.title = page.title();
                tab.page = Some(page.clone());
                self.tabs.insert(index, tab);
                Ok(Some(page))
            }
            Err(error) => {
                tab.history_index = target + 1;
                self.tabs.insert(index, tab);
                Err(error)
            }
        }
    }

    pub async fn forward(&mut self) -> Result<Option<Page>> {
        if self.tabs.is_empty() {
            return Err(NxError::Browser("no active tab".to_owned()));
        }
        let index = self.active;
        let mut tab = self.tabs.remove(index);
        if tab.history_index + 1 >= tab.history.len() {
            self.tabs.insert(index, tab);
            return Ok(None);
        }
        tab.history_index += 1;
        let target = tab.history_index;
        let url = tab.history[target].clone();
        let result = self.load_into_tab(&mut tab, url).await;
        match result {
            Ok(page) => {
                tab.title = page.title();
                tab.page = Some(page.clone());
                self.tabs.insert(index, tab);
                Ok(Some(page))
            }
            Err(error) => {
                tab.history_index = target - 1;
                self.tabs.insert(index, tab);
                Err(error)
            }
        }
    }

    pub async fn reload(&mut self) -> Result<Page> {
        if self.tabs.is_empty() {
            return Err(NxError::Browser("no active tab".to_owned()));
        }
        let index = self.active;
        let mut tab = self.tabs.remove(index);
        let Some(url) = tab.url().cloned() else {
            self.tabs.insert(index, tab);
            return Err(NxError::Browser("active tab has no page".to_owned()));
        };
        let result = self.load_into_tab(&mut tab, url).await;
        match result {
            Ok(page) => {
                tab.title = page.title();
                tab.page = Some(page.clone());
                self.tabs.insert(index, tab);
                Ok(page)
            }
            Err(error) => {
                self.tabs.insert(index, tab);
                Err(error)
            }
        }
    }

    /// Save all tabs, their history and session storage to a JSON file.
    pub fn save_session(&self, path: &Path) -> Result<()> {
        let data = SessionData {
            active: self.active,
            tabs: self
                .tabs
                .iter()
                .map(|tab| SessionTab {
                    url: tab
                        .url()
                        .map(|url| url.to_string())
                        .unwrap_or_else(|| "about:blank".to_owned()),
                    history: tab.history.iter().map(Url::to_string).collect(),
                    history_index: tab.history_index,
                    session_storage: tab.session_storage.clone(),
                    scroll_x: tab.scroll_x,
                    scroll_y: tab.scroll_y,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&data)
            .map_err(|error| NxError::Storage(format!("serialize session: {error}")))?;
        write_atomic(path, &bytes)
    }

    pub fn save_profile_session(&self) -> Result<()> {
        let Some(profile) = &self.profile else {
            return Err(NxError::Browser("no profile is active".to_owned()));
        };
        self.save_session(&profile.session_path())
    }

    /// Restore tabs from a saved session file, loading each tab's page.
    pub async fn restore_session(&mut self, path: &Path) -> Result<()> {
        let bytes = read_locked(path)?
            .ok_or_else(|| NxError::Storage(format!("session not found: {}", path.display())))?;
        let data: SessionData = serde_json::from_slice(&bytes)
            .map_err(|error| NxError::Storage(format!("parse session: {error}")))?;
        self.tabs.clear();
        for saved in data.tabs {
            let history: Vec<Url> = saved
                .history
                .iter()
                .filter_map(|url| Url::parse(url).ok())
                .collect();
            let mut tab = Tab::empty(TabId(self.next_tab_id));
            self.next_tab_id += 1;
            tab.history = history;
            tab.history_index = saved.history_index.min(tab.history.len().saturating_sub(1));
            tab.session_storage = saved.session_storage;
            tab.scroll_x = saved.scroll_x.max(0.0);
            tab.scroll_y = saved.scroll_y.max(0.0);
            let url = tab
                .history
                .get(tab.history_index)
                .cloned()
                .or_else(|| Url::parse(&saved.url).ok());
            if let Some(url) = url {
                let page = self.load_into_tab(&mut tab, url).await?;
                tab.title = page.title();
                tab.page = Some(page);
            }
            self.tabs.push(tab);
        }
        self.active = data.active.min(self.tabs.len().saturating_sub(1));
        Ok(())
    }

    pub async fn restore_profile_session(&mut self) -> Result<()> {
        let Some(profile) = &self.profile else {
            return Err(NxError::Browser("no profile is active".to_owned()));
        };
        let path = profile.session_path();
        if !path.exists() {
            return Ok(());
        }
        self.restore_session(&path).await
    }

    /// Render a loaded page into an RGBA image.
    pub async fn render(&self, page: &Page, options: RenderOptions) -> Result<RenderedPage> {
        render_page(page, &self.client, options).await
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(profile) = &self.profile
            && !self.tabs.is_empty()
        {
            let _ = self.save_session(&profile.session_path());
        }
    }
}

fn node_selector_path(
    doc: &crate::engine::dom::Document,
    node: crate::engine::dom::NodeId,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = Some(node);
    while let Some(id) = current {
        if id == doc.document_node() {
            break;
        }
        let tag = doc.tag_name(id)?;
        if let Some(existing) = doc.get_attribute(id, "id") {
            parts.push(format!("#{existing}"));
            break;
        }
        let parent = doc.parent(id)?;
        let mut total = 0usize;
        let mut position = 0usize;
        for &sibling in doc.children(parent) {
            if doc.tag_name(sibling) == Some(tag) {
                total += 1;
                if sibling == id {
                    position = total;
                }
            }
        }
        parts.push(if total > 1 {
            format!("{tag}:nth-of-type({position})")
        } else {
            tag.to_owned()
        });
        current = Some(parent);
    }
    parts.reverse();
    (!parts.is_empty()).then(|| parts.join(" > "))
}
