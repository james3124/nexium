//! JavaScript execution: Boa runtime, script loading, console, timers and the
//! host event loop.

pub mod canvas;
pub mod dom;
pub mod events;
pub mod fetch;
pub mod indexeddb;
mod modules;
pub mod observers;
pub mod webapi;
pub mod websockets;
pub mod workers;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use boa_engine::builtins::promise::PromiseState;
use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::{JsArray, JsPromise};
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source,
};
use serde::Serialize;
use url::Url;

use crate::engine::dom::{Document, NodeId};
use crate::networking::{HttpClient, Request};

const NODE_ID_KEY: &str = "__nexium_node_id";

#[derive(Clone, Debug, Serialize)]
pub struct ConsoleMessage {
    pub level: String,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct ScriptRunResult {
    pub console: Vec<ConsoleMessage>,
    pub errors: Vec<String>,
    pub executed_scripts: usize,
    pub local_storage: HashMap<String, String>,
    pub session_storage: HashMap<String, String>,
    pub canvases: HashMap<NodeId, image::RgbaImage>,
}

#[derive(Clone, Debug)]
pub struct ScriptOptions {
    pub enabled: bool,
    pub user_agent: String,
    pub max_runtime: Duration,
    pub viewport_width: f64,
    pub viewport_height: f64,
    pub local_storage: HashMap<String, String>,
    pub session_storage: HashMap<String, String>,
    pub indexeddb_dir: Option<std::path::PathBuf>,
}

impl Default for ScriptOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            user_agent: "Nexium/0.0".to_owned(),
            max_runtime: Duration::from_millis(2000),
            viewport_width: 1280.0,
            viewport_height: 720.0,
            local_storage: HashMap::new(),
            session_storage: HashMap::new(),
            indexeddb_dir: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SessionEvent {
    pub event_type: String,
    pub target: Option<NodeId>,
    pub bubbles: bool,
    pub cancelable: bool,
    pub client_x: Option<f64>,
    pub client_y: Option<f64>,
    pub page_x: Option<f64>,
    pub page_y: Option<f64>,
    pub button: Option<i64>,
    pub buttons: Option<i64>,
    pub detail: Option<i64>,
    pub delta_x: Option<f64>,
    pub delta_y: Option<f64>,
    pub key: Option<String>,
    pub code: Option<String>,
    pub text: Option<String>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

pub(crate) struct SessionSnapshot {
    pub result: ScriptRunResult,
    pub document: Document,
    pub prevented: bool,
}

enum SessionCommand {
    Event {
        event: Box<SessionEvent>,
        reply: mpsc::Sender<SessionSnapshot>,
    },
    Key {
        key: String,
        reply: mpsc::Sender<SessionSnapshot>,
    },
    Text {
        text: String,
        reply: mpsc::Sender<SessionSnapshot>,
    },
    Snapshot {
        reply: mpsc::Sender<SessionSnapshot>,
    },
    Evaluate {
        code: String,
        reply: mpsc::Sender<(
            std::result::Result<serde_json::Value, String>,
            SessionSnapshot,
        )>,
    },
    Layout {
        rects: HashMap<usize, [f32; 4]>,
        styles: HashMap<usize, HashMap<String, String>>,
    },
}

/// A live JavaScript runtime attached to a loaded page. It keeps event
/// listeners and timers alive so host interactions can dispatch real events.
#[derive(Debug)]
pub struct ScriptSession {
    sender: mpsc::Sender<SessionCommand>,
}

impl ScriptSession {
    pub(crate) fn dispatch(&self, event: SessionEvent) -> Option<SessionSnapshot> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(SessionCommand::Event {
                event: Box::new(event),
                reply,
            })
            .ok()?;
        receiver.recv().ok()
    }

    pub(crate) fn press_key(&self, key: &str) -> Option<SessionSnapshot> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(SessionCommand::Key {
                key: key.to_owned(),
                reply,
            })
            .ok()?;
        receiver.recv().ok()
    }

    pub(crate) fn type_text(&self, text: &str) -> Option<SessionSnapshot> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(SessionCommand::Text {
                text: text.to_owned(),
                reply,
            })
            .ok()?;
        receiver.recv().ok()
    }

    /// Publish viewport geometry and computed styles for the DOM bindings.
    pub(crate) fn set_layout(
        &self,
        rects: HashMap<usize, [f32; 4]>,
        styles: HashMap<usize, HashMap<String, String>>,
    ) {
        let _ = self.sender.send(SessionCommand::Layout { rects, styles });
    }

    pub(crate) fn snapshot(&self) -> Option<SessionSnapshot> {
        let (reply, receiver) = mpsc::channel();
        self.sender.send(SessionCommand::Snapshot { reply }).ok()?;
        receiver.recv().ok()
    }

    pub(crate) fn evaluate(
        &self,
        code: &str,
    ) -> Option<(
        std::result::Result<serde_json::Value, String>,
        SessionSnapshot,
    )> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(SessionCommand::Evaluate {
                code: code.to_owned(),
                reply,
            })
            .ok()?;
        receiver.recv().ok()
    }

    fn start(
        base_url: Url,
        document: Document,
        sources: Vec<ScriptSource>,
        client: Option<Arc<HttpClient>>,
        options: ScriptOptions,
    ) -> std::result::Result<(Self, ScriptRunResult, Document), String> {
        let (init_sender, init_receiver) = mpsc::channel();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("nexium-js".to_owned())
            .spawn(move || {
                let (mut context, document, host) =
                    initialize_runtime(base_url, document, sources, client, &options);
                let executed = host.executed_scripts;
                let _ = init_sender.send((host, document.borrow().clone()));
                while let Ok(command) = receiver.recv() {
                    match command {
                        SessionCommand::Event { event, reply } => {
                            let prevented = apply_session_event(&event, &mut context);
                            drain_ready(&mut context);
                            let _ = reply.send(session_snapshot(&document, executed, prevented));
                        }
                        SessionCommand::Key { key, reply } => {
                            apply_key(&key, &mut context);
                            drain_ready(&mut context);
                            let _ = reply.send(session_snapshot(&document, executed, false));
                        }
                        SessionCommand::Text { text, reply } => {
                            for character in text.chars() {
                                apply_key(&character.to_string(), &mut context);
                            }
                            drain_ready(&mut context);
                            let _ = reply.send(session_snapshot(&document, executed, false));
                        }
                        SessionCommand::Layout { rects, styles } => {
                            with_state(|state| {
                                state.layout_rects = rects;
                                state.computed_styles = styles;
                            });
                        }
                        SessionCommand::Snapshot { reply } => {
                            drain_ready(&mut context);
                            let _ = reply.send(session_snapshot(&document, executed, false));
                        }
                        SessionCommand::Evaluate { code, reply } => {
                            let value = evaluate_value(&code, &mut context);
                            drain_ready(&mut context);
                            let snapshot = session_snapshot(&document, executed, false);
                            let _ = reply.send((value, snapshot));
                        }
                    }
                }
                let _ = STATE.with(|cell| cell.borrow_mut().take());
                drop(context);
            })
            .map_err(|error| format!("failed to start script runtime: {error}"))?;
        let (host, document) = init_receiver
            .recv()
            .map_err(|_| "script runtime stopped during startup".to_owned())?;
        let result = ScriptRunResult {
            console: host.console,
            errors: host.errors,
            executed_scripts: host.executed_scripts,
            local_storage: host.local_storage,
            session_storage: host.session_storage,
            canvases: host.canvases,
        };
        Ok((Self { sender }, result, document))
    }
}

fn session_snapshot(
    document: &Rc<RefCell<Document>>,
    executed: usize,
    prevented: bool,
) -> SessionSnapshot {
    let host = snapshot_host_result(executed);
    SessionSnapshot {
        result: ScriptRunResult {
            console: host.console,
            errors: host.errors,
            executed_scripts: host.executed_scripts,
            local_storage: host.local_storage,
            session_storage: host.session_storage,
            canvases: host.canvases,
        },
        document: document.borrow().clone(),
        prevented,
    }
}

pub(crate) struct Listener {
    pub node: NodeId,
    pub event: String,
    pub function_index: u32,
    pub once: bool,
    pub capture: bool,
    pub property: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimerKind {
    Timeout,
    Interval,
    AnimationFrame,
}

pub(crate) struct Timer {
    pub id: u64,
    pub due: Instant,
    pub interval: Option<Duration>,
    pub function_index: u32,
    pub kind: TimerKind,
}

pub(crate) struct JsState {
    pub document: Rc<RefCell<Document>>,
    pub base_url: Url,
    pub layout_rects: HashMap<usize, [f32; 4]>,
    pub computed_styles: HashMap<usize, HashMap<String, String>>,
    pub node_cache: Option<JsObject>,
    pub function_store: Option<JsObject>,
    pub next_function_slot: u32,
    pub listeners: Vec<Listener>,
    pub timers: Vec<Timer>,
    pub next_timer_id: u64,
    pub console: Vec<ConsoleMessage>,
    pub errors: Vec<String>,
    pub client: Option<Arc<HttpClient>>,
    pub max_runtime: Duration,
    pub start_time: Instant,
    pub local_storage: HashMap<String, String>,
    pub session_storage: HashMap<String, String>,
    pub executed_scripts: HashSet<usize>,
    pub observers: HashMap<u64, observers::ObserverState>,
    pub next_observer_id: u64,
    pub observer_store: Option<JsObject>,
    pub indexeddb_dir: Option<std::path::PathBuf>,
    pub idb_databases: HashMap<String, indexeddb::IdbDatabase>,
    pub idb_tasks: std::collections::VecDeque<indexeddb::IdbTask>,
    pub idb_next_id: u64,
    pub idb_request_store: Option<JsObject>,
    pub idb_tx_store: Option<JsObject>,
    pub idb_txs: HashMap<u64, indexeddb::IdbTransaction>,
    pub focused_node: Option<NodeId>,
    pub canvases: HashMap<NodeId, canvas::CanvasState>,
    pub websockets: HashMap<u64, websockets::WebSocketHandle>,
    pub ws_event_tx: Option<mpsc::Sender<websockets::WsEvent>>,
    pub ws_event_rx: Option<mpsc::Receiver<websockets::WsEvent>>,
    pub ws_store: Option<JsObject>,
    pub ws_next_id: u64,
    pub workers: HashMap<u64, workers::WorkerHandle>,
    pub worker_event_tx: Option<mpsc::Sender<workers::WorkerEvent>>,
    pub worker_event_rx: Option<mpsc::Receiver<workers::WorkerEvent>>,
    pub worker_store: Option<JsObject>,
    pub worker_next_id: u64,
}

thread_local! {
    static STATE: RefCell<Option<JsState>> = const { RefCell::new(None) };
}

pub(crate) fn with_state<R>(f: impl FnOnce(&mut JsState) -> R) -> Option<R> {
    STATE.with(|cell| cell.borrow_mut().as_mut().map(f))
}

pub(crate) fn document_rc() -> Option<Rc<RefCell<Document>>> {
    with_state(|state| Rc::clone(&state.document))
}

pub(crate) fn host_error(message: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

pub(crate) fn js_string_value(value: &str) -> JsValue {
    JsValue::from(JsString::from(value))
}

pub(crate) fn string_arg(args: &[JsValue], index: usize, context: &mut Context) -> Option<String> {
    let value = args.get(index)?;
    if value.is_undefined() || value.is_null() {
        return None;
    }
    value
        .to_string(context)
        .ok()
        .map(|value| value.to_std_string_escaped())
}

pub(crate) fn value_to_display(value: &JsValue, context: &mut Context) -> String {
    value
        .to_string(context)
        .map(|value| value.to_std_string_escaped())
        .unwrap_or_else(|_| "<unprintable>".to_owned())
}

pub(crate) fn node_id_from_object(object: &JsObject, context: &mut Context) -> Option<NodeId> {
    let value = object.get(JsString::from(NODE_ID_KEY), context).ok()?;
    let id = value.as_number()?;
    if id < 0.0 {
        return None;
    }
    Some(NodeId::from_index(id as usize))
}

pub(crate) fn node_id_from_this(this: &JsValue, context: &mut Context) -> Option<NodeId> {
    let object = this.as_object()?;
    node_id_from_object(&object, context)
}

pub(crate) fn store_function(value: &JsValue, context: &mut Context) -> Option<u32> {
    let store = with_state(|state| state.function_store.clone())??;
    let slot = with_state(|state| {
        let slot = state.next_function_slot;
        state.next_function_slot += 1;
        slot
    })?;
    store
        .set(
            JsString::from(slot.to_string()),
            value.clone(),
            false,
            context,
        )
        .ok()?;
    Some(slot)
}

pub(crate) fn load_function(slot: u32, context: &mut Context) -> Option<JsValue> {
    let store = with_state(|state| state.function_store.clone())??;
    store
        .get(JsString::from(slot.to_string()), context)
        .ok()
        .filter(|value| value.as_function().is_some())
}

/// Collect and execute scripts for a loaded document.
pub async fn run_page_scripts(
    url: &Url,
    document: &mut Document,
    client: Option<Arc<HttpClient>>,
    options: &ScriptOptions,
) -> ScriptRunResult {
    let mut result = ScriptRunResult::default();
    if !options.enabled {
        return result;
    }

    let sources = load_script_sources(url, document, client.clone(), &mut result.errors).await;
    let base = url.clone();
    let options = options.clone();
    let document_owned = std::mem::take(document);
    let task = tokio::task::spawn_blocking(move || {
        let mut document_owned = document_owned;
        let host = run_sources(base, &mut document_owned, sources, client, &options);
        (document_owned, host)
    })
    .await;

    match task {
        Ok((document_owned, host)) => {
            *document = document_owned;
            result.console = host.console;
            result.errors.extend(host.errors);
            result.executed_scripts = host.executed_scripts;
            result.local_storage = host.local_storage;
            result.session_storage = host.session_storage;
            result.canvases = host.canvases;
        }
        Err(error) => {
            *document = Document::new();
            result
                .errors
                .push(format!("script execution thread failed: {error}"));
        }
    }
    result
}

/// Start a persistent script runtime for a loaded page. The returned session
/// keeps listeners, timers and observers alive for host-driven interactions.
pub async fn run_page_scripts_persistent(
    url: &Url,
    document: &mut Document,
    client: Option<Arc<HttpClient>>,
    options: &ScriptOptions,
) -> (ScriptRunResult, Option<ScriptSession>) {
    let mut result = ScriptRunResult::default();
    if !options.enabled {
        return (result, None);
    }
    let sources = load_script_sources(url, document, client.clone(), &mut result.errors).await;
    let base = url.clone();
    let options = options.clone();
    let document_owned = std::mem::take(document);
    let task = tokio::task::spawn_blocking(move || {
        ScriptSession::start(base, document_owned, sources, client, options)
    })
    .await;
    match task {
        Ok(Ok((session, host, document_owned))) => {
            *document = document_owned;
            result.console = host.console;
            result.errors.extend(host.errors);
            result.executed_scripts = host.executed_scripts;
            result.local_storage = host.local_storage;
            result.session_storage = host.session_storage;
            result.canvases = host.canvases;
            (result, Some(session))
        }
        Ok(Err(error)) => {
            *document = Document::new();
            result
                .errors
                .push(format!("script runtime failed: {error}"));
            (result, None)
        }
        Err(error) => {
            *document = Document::new();
            result
                .errors
                .push(format!("script execution thread failed: {error}"));
            (result, None)
        }
    }
}

async fn load_script_sources(
    url: &Url,
    document: &Document,
    client: Option<Arc<HttpClient>>,
    errors: &mut Vec<String>,
) -> Vec<ScriptSource> {
    let mut sources = Vec::new();
    for script in collect_scripts(document, url) {
        match script {
            ScriptRef::Inline { code, module } => sources.push(ScriptSource {
                code,
                module,
                url: None,
            }),
            ScriptRef::External {
                url: target,
                module,
            } => {
                let Some(client) = client.as_ref() else {
                    errors.push(format!(
                        "cannot load external script without network: {target}"
                    ));
                    continue;
                };
                let request = match Request::get(target.clone()).header("Accept", "*/*") {
                    Ok(request) => request,
                    Err(error) => {
                        errors.push(format!("{target}: {error}"));
                        continue;
                    }
                };
                match client.fetch(request).await {
                    Ok(response) if response.status.is_success() => {
                        let charset = response.charset();
                        let code = decode_script(&response.body, charset.as_deref());
                        sources.push(ScriptSource {
                            code,
                            module,
                            url: Some(target),
                        });
                    }
                    Ok(response) => {
                        errors.push(format!("{target}: HTTP {}", response.status.as_u16()))
                    }
                    Err(error) => errors.push(format!("{target}: {error}")),
                }
            }
            ScriptRef::Skipped(message) => errors.push(message),
        }
    }
    sources
}

/// Execute only inline scripts (no network access), used for offline pages.
pub fn run_page_scripts_inline(
    url: &Url,
    document: &mut Document,
    options: &ScriptOptions,
) -> ScriptRunResult {
    let mut result = ScriptRunResult::default();
    if !options.enabled {
        return result;
    }
    let sources = inline_script_sources(url, document, &mut result.errors);
    let host = run_sources(url.clone(), document, sources, None, options);
    result.console = host.console;
    result.errors.extend(host.errors);
    result.executed_scripts = host.executed_scripts;
    result.local_storage = host.local_storage;
    result.session_storage = host.session_storage;
    result.canvases = host.canvases;
    result
}

/// Start a persistent inline-only script runtime for offline pages.
pub fn run_page_scripts_inline_persistent(
    url: &Url,
    document: &mut Document,
    options: &ScriptOptions,
) -> (ScriptRunResult, Option<ScriptSession>) {
    let mut result = ScriptRunResult::default();
    if !options.enabled {
        return (result, None);
    }
    let sources = inline_script_sources(url, document, &mut result.errors);
    let document_owned = std::mem::take(document);
    match ScriptSession::start(url.clone(), document_owned, sources, None, options.clone()) {
        Ok((session, host, document_owned)) => {
            *document = document_owned;
            result.console = host.console;
            result.errors.extend(host.errors);
            result.executed_scripts = host.executed_scripts;
            result.local_storage = host.local_storage;
            result.session_storage = host.session_storage;
            result.canvases = host.canvases;
            (result, Some(session))
        }
        Err(error) => {
            *document = Document::new();
            result
                .errors
                .push(format!("script runtime failed: {error}"));
            (result, None)
        }
    }
}

fn inline_script_sources(
    url: &Url,
    document: &Document,
    errors: &mut Vec<String>,
) -> Vec<ScriptSource> {
    let mut sources = Vec::new();
    for script in collect_scripts(document, url) {
        match script {
            ScriptRef::Inline { code, module } => sources.push(ScriptSource {
                code,
                module,
                url: None,
            }),
            ScriptRef::External { url: target, .. } => {
                errors.push(format!("external script skipped offline: {target}"))
            }
            ScriptRef::Skipped(message) => errors.push(message),
        }
    }
    sources
}

#[derive(Clone)]
struct ScriptSource {
    code: String,
    module: bool,
    /// Resolved URL for external scripts, `None` for inline scripts.
    url: Option<Url>,
}

enum ScriptRef {
    Inline { code: String, module: bool },
    External { url: Url, module: bool },
    Skipped(String),
}

fn collect_scripts(document: &Document, base_url: &Url) -> Vec<ScriptRef> {
    let mut scripts = Vec::new();
    for id in document.descendants(document.document_node()) {
        if document.tag_name(id) != Some("script") {
            continue;
        }
        let script_type = document
            .get_attribute(id, "type")
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let module = script_type == "module";
        if !script_type.is_empty()
            && !module
            && !matches!(
                script_type.as_str(),
                "text/javascript"
                    | "application/javascript"
                    | "application/ecmascript"
                    | "text/ecmascript"
                    | "text/jscript"
            )
        {
            continue;
        }
        if let Some(src) = document.get_attribute(id, "src") {
            let target = match Url::parse(src) {
                Ok(url) => url,
                Err(_) => match base_url.join(src) {
                    Ok(url) => url,
                    Err(_) => {
                        scripts.push(ScriptRef::Skipped(format!("invalid script URL: {src}")));
                        continue;
                    }
                },
            };
            scripts.push(ScriptRef::External {
                url: target,
                module,
            });
        } else {
            scripts.push(ScriptRef::Inline {
                code: document.text_content(id),
                module,
            });
        }
    }
    scripts
}

fn decode_script(bytes: &[u8], charset: Option<&str>) -> String {
    if let Some(label) = charset
        && let Some(encoding) = encoding_rs::Encoding::for_label(label.trim().as_bytes())
    {
        let (text, _, _) = encoding.decode(bytes);
        return text.into_owned();
    }
    let (text, _, _) = encoding_rs::UTF_8.decode(bytes);
    text.into_owned()
}

struct HostResult {
    console: Vec<ConsoleMessage>,
    errors: Vec<String>,
    executed_scripts: usize,
    local_storage: HashMap<String, String>,
    session_storage: HashMap<String, String>,
    canvases: HashMap<NodeId, image::RgbaImage>,
}

fn initialize_runtime(
    base_url: Url,
    document: Document,
    sources: Vec<ScriptSource>,
    client: Option<Arc<HttpClient>>,
    options: &ScriptOptions,
) -> (Context, Rc<RefCell<Document>>, HostResult) {
    let document_rc = Rc::new(RefCell::new(document));
    let import_map = modules::ImportMap::from_document(&document_rc.borrow(), &base_url);
    let module_registry =
        modules::ModuleRegistry::new(base_url.clone(), client.clone(), import_map);
    let state = JsState {
        document: Rc::clone(&document_rc),
        base_url: base_url.clone(),
        layout_rects: HashMap::new(),
        computed_styles: HashMap::new(),
        node_cache: None,
        function_store: None,
        next_function_slot: 0,
        listeners: Vec::new(),
        timers: Vec::new(),
        next_timer_id: 1,
        console: Vec::new(),
        errors: Vec::new(),
        client,
        max_runtime: options.max_runtime,
        start_time: Instant::now(),
        local_storage: options.local_storage.clone(),
        session_storage: options.session_storage.clone(),
        executed_scripts: HashSet::new(),
        observers: HashMap::new(),
        next_observer_id: 1,
        observer_store: None,
        indexeddb_dir: options.indexeddb_dir.clone(),
        idb_databases: HashMap::new(),
        idb_tasks: std::collections::VecDeque::new(),
        idb_next_id: 1,
        idb_request_store: None,
        idb_tx_store: None,
        idb_txs: HashMap::new(),
        focused_node: None,
        canvases: HashMap::new(),
        websockets: HashMap::new(),
        ws_event_tx: None,
        ws_event_rx: None,
        ws_store: None,
        ws_next_id: 1,
        workers: HashMap::new(),
        worker_event_tx: None,
        worker_event_rx: None,
        worker_store: None,
        worker_next_id: 1,
    };
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(state);
    });

    let mut context = Context::builder()
        .module_loader(module_registry)
        .build()
        .unwrap_or_default();
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(10_000_000);
    let mut executed = 0usize;

    if let Err(error) = install_globals(&mut context, &base_url, options) {
        with_state(|state| state.errors.push(format!("runtime setup: {error}")));
    }

    for source in &sources {
        executed += 1;
        if let Some(error) = evaluate_script(source, &mut context) {
            with_state(|state| state.errors.push(error));
        }
    }

    let _ = context.run_jobs();
    observers::flush(&mut context);
    events::dispatch_named_with_context(dom::document_node(), "DOMContentLoaded", &mut context);
    let _ = context.run_jobs();
    observers::flush(&mut context);
    drive_event_loop(&mut context);
    events::dispatch_named_with_context(dom::document_node(), "load", &mut context);
    let _ = context.run_jobs();
    observers::flush(&mut context);
    drive_event_loop(&mut context);
    observers::flush(&mut context);

    let host = snapshot_host_result(executed);
    (context, document_rc, host)
}

fn snapshot_host_result(executed: usize) -> HostResult {
    let canvases = canvas::snapshot_canvases();
    let (console, errors, local_storage, session_storage) = with_state(|state| {
        (
            state.console.clone(),
            state.errors.clone(),
            state.local_storage.clone(),
            state.session_storage.clone(),
        )
    })
    .unwrap_or_default();
    HostResult {
        console,
        errors,
        executed_scripts: executed,
        local_storage,
        session_storage,
        canvases,
    }
}

fn run_sources(
    base_url: Url,
    document: &mut Document,
    sources: Vec<ScriptSource>,
    client: Option<Arc<HttpClient>>,
    options: &ScriptOptions,
) -> HostResult {
    let document_owned = std::mem::take(document);
    let (context, document_rc, host) =
        initialize_runtime(base_url, document_owned, sources, client, options);
    let _ = STATE.with(|cell| cell.borrow_mut().take());
    drop(context);
    match Rc::try_unwrap(document_rc) {
        Ok(cell) => *document = cell.into_inner(),
        Err(shared) => {
            *document = shared.borrow().clone();
        }
    }
    host
}

fn apply_session_event(event: &SessionEvent, context: &mut Context) -> bool {
    let target = event
        .target
        .or_else(|| with_state(|state| state.focused_node).flatten())
        .unwrap_or_else(dom::document_node);
    let Ok(event_object) = events::make_input_event(event, context) else {
        return false;
    };
    events::dispatch_event(target, &event_object, &event.event_type, context);
    let prevented = events::is_prevented(&event_object, context);
    if matches!(event.event_type.as_str(), "mousedown" | "click") {
        focus_if_focusable(target, context);
    }
    if event.event_type == "click" && !prevented {
        default_click_action(target, context);
    }
    prevented
}

fn element_is_disabled(document: &Document, node: NodeId) -> bool {
    document.get_attribute(node, "disabled").is_some()
        || document
            .get_attribute(node, "aria-disabled")
            .is_some_and(|value| value == "true")
}

fn element_is_readonly(document: &Document, node: NodeId) -> bool {
    document.get_attribute(node, "readonly").is_some()
        || document
            .get_attribute(node, "aria-readonly")
            .is_some_and(|value| value == "true")
}

fn is_focusable(document: &Document, node: NodeId) -> bool {
    let Some(tag) = document.tag_name(node) else {
        return false;
    };
    if element_is_disabled(document, node) {
        return false;
    }
    match tag {
        "input" | "textarea" | "select" | "button" | "option" => true,
        "a" | "area" => document.get_attribute(node, "href").is_some(),
        _ => {
            document.get_attribute(node, "contenteditable").is_some()
                || document.get_attribute(node, "tabindex").is_some()
        }
    }
}

fn focus_if_focusable(node: NodeId, context: &mut Context) {
    let (focusable, previous) = {
        let Some(document) = document_rc() else {
            return;
        };
        let document = document.borrow();
        (
            is_focusable(&document, node),
            with_state(|state| state.focused_node).flatten(),
        )
    };
    if !focusable || previous == Some(node) {
        return;
    }
    if let Some(previous) = previous {
        events::dispatch_named_event(previous, "blur", false, true, context);
        events::dispatch_named_event(previous, "focusout", true, false, context);
    }
    with_state(|state| state.focused_node = Some(node));
    events::dispatch_named_event(node, "focus", false, true, context);
    events::dispatch_named_event(node, "focusin", true, false, context);
}

fn default_click_action(node: NodeId, context: &mut Context) {
    let toggle = {
        let Some(document) = document_rc() else {
            return;
        };
        let document = document.borrow();
        document.tag_name(node) == Some("input")
            && matches!(
                document
                    .get_attribute(node, "type")
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str(),
                "checkbox" | "radio"
            )
    };
    if toggle {
        let Some(document) = document_rc() else {
            return;
        };
        let checked = {
            let document = document.borrow();
            document.get_attribute(node, "checked").is_some()
        };
        {
            let mut document = document.borrow_mut();
            if checked {
                let _ = document.remove_attribute(node, "checked");
            } else {
                let _ = document.set_attribute(node, "checked", "");
            }
        }
        events::dispatch_named_event(node, "change", true, false, context);
    }
}

fn apply_key(key: &str, context: &mut Context) {
    let target = with_state(|state| state.focused_node)
        .flatten()
        .unwrap_or_else(dom::document_node);
    let key_event = SessionEvent {
        event_type: "keydown".to_owned(),
        target: Some(target),
        bubbles: true,
        cancelable: true,
        key: Some(key.to_owned()),
        code: Some(key_code(key)),
        ..SessionEvent::default()
    };
    let prevented = dispatch_session(&key_event, context);
    if prevented {
        dispatch_keyup(target, key, context);
        return;
    }
    if key == "Backspace" {
        apply_backspace(context);
    } else if key.chars().count() == 1 {
        let text_event = SessionEvent {
            event_type: "keypress".to_owned(),
            target: Some(target),
            bubbles: true,
            cancelable: true,
            key: Some(key.to_owned()),
            code: Some(key_code(key)),
            text: Some(key.to_owned()),
            ..SessionEvent::default()
        };
        if !dispatch_session(&text_event, context) {
            apply_text_input(key, context);
        }
    }
    dispatch_keyup(target, key, context);
}

fn dispatch_keyup(target: NodeId, key: &str, context: &mut Context) {
    let event = SessionEvent {
        event_type: "keyup".to_owned(),
        target: Some(target),
        bubbles: true,
        cancelable: true,
        key: Some(key.to_owned()),
        code: Some(key_code(key)),
        ..SessionEvent::default()
    };
    dispatch_session(&event, context);
}

fn dispatch_session(event: &SessionEvent, context: &mut Context) -> bool {
    let Ok(event_object) = events::make_input_event(event, context) else {
        return false;
    };
    let target = event
        .target
        .or_else(|| with_state(|state| state.focused_node).flatten())
        .unwrap_or_else(dom::document_node);
    events::dispatch_event(target, &event_object, &event.event_type, context);
    events::is_prevented(&event_object, context)
}

fn key_code(key: &str) -> String {
    match key {
        "Backspace" | "Enter" | "Tab" | "Escape" | "ArrowLeft" | "ArrowRight" | "ArrowUp"
        | "ArrowDown" | "Delete" | "Home" | "End" | "PageUp" | "PageDown" => key.to_owned(),
        " " => "Space".to_owned(),
        other if other.chars().count() == 1 => {
            let character = other.chars().next().unwrap_or(' ');
            if character.is_ascii_alphabetic() {
                format!("Key{}", character.to_ascii_uppercase())
            } else if character.is_ascii_digit() {
                format!("Digit{character}")
            } else {
                other.to_owned()
            }
        }
        other => other.to_owned(),
    }
}

fn editable_value(document: &Document, node: NodeId) -> Option<String> {
    match document.tag_name(node)? {
        "input" => Some(
            document
                .get_attribute(node, "value")
                .unwrap_or("")
                .to_owned(),
        ),
        "textarea" => Some(document.text_content(node)),
        _ => None,
    }
}

fn set_editable_value(document: &mut Document, node: NodeId, value: &str) {
    match document.tag_name(node) {
        Some("input") => {
            let _ = document.set_attribute(node, "value", value);
        }
        Some("textarea") => {
            let _ = document.set_text_content(node, value);
        }
        _ => {}
    }
}

fn apply_text_input(text: &str, context: &mut Context) {
    let Some(node) = with_state(|state| state.focused_node).flatten() else {
        return;
    };
    let Some(document) = document_rc() else {
        return;
    };
    let (updated, editable) = {
        let document = document.borrow();
        let editable = !element_is_disabled(&document, node)
            && !element_is_readonly(&document, node)
            && editable_value(&document, node).is_some();
        let updated = editable_value(&document, node).map(|mut value| {
            value.push_str(text);
            value
        });
        (updated, editable)
    };
    if !editable {
        return;
    }
    if let Some(updated) = updated {
        set_editable_value(&mut document.borrow_mut(), node, &updated);
        events::dispatch_named_event(node, "input", true, false, context);
    }
}

fn apply_backspace(context: &mut Context) {
    let Some(node) = with_state(|state| state.focused_node).flatten() else {
        return;
    };
    let Some(document) = document_rc() else {
        return;
    };
    let (updated, editable) = {
        let document = document.borrow();
        let editable = !element_is_disabled(&document, node)
            && !element_is_readonly(&document, node)
            && editable_value(&document, node).is_some();
        let updated = editable_value(&document, node).map(|mut value| {
            value.pop();
            value
        });
        (updated, editable)
    };
    if !editable {
        return;
    }
    if let Some(updated) = updated {
        set_editable_value(&mut document.borrow_mut(), node, &updated);
        events::dispatch_named_event(node, "input", true, false, context);
    }
}

fn evaluate_value(
    code: &str,
    context: &mut Context,
) -> std::result::Result<serde_json::Value, String> {
    let value = context
        .eval(Source::from_bytes(code))
        .map_err(|error| format_js_error(&error, context))?;
    let _ = context.run_jobs();
    let value = if let Some(object) = value.as_object()
        && let Ok(promise) = JsPromise::from_object(object.clone())
    {
        match promise.state() {
            PromiseState::Fulfilled(value) => value,
            PromiseState::Rejected(value) => return Err(value_to_display(&value, context)),
            PromiseState::Pending => {
                return Err("evaluation returned a pending promise".to_owned());
            }
        }
    } else {
        value
    };
    Ok(indexeddb::js_to_json(&value, context, 0).unwrap_or(serde_json::Value::Null))
}

fn evaluate_script(source: &ScriptSource, context: &mut Context) -> Option<String> {
    if source.module {
        return match &source.url {
            Some(url) => modules::evaluate_external_module(url, &source.code, context),
            None => modules::evaluate_inline_module(&source.code, context),
        };
    }
    match &source.url {
        Some(url) => {
            // Parse external classic scripts with their URL so dynamic
            // `import()` inside them resolves relative to the script.
            let path = std::path::Path::new(url.as_str());
            let script = boa_engine::Script::parse(
                Source::from_bytes(&source.code).with_path(path),
                None,
                context,
            )
            .map_err(|error| format_js_error(&error, context));
            match script {
                Ok(script) => script
                    .evaluate(context)
                    .err()
                    .map(|error| format_js_error(&error, context)),
                Err(message) => Some(message),
            }
        }
        None => context
            .eval(Source::from_bytes(&source.code))
            .err()
            .map(|error| format_js_error(&error, context)),
    }
}

fn format_js_error(error: &boa_engine::JsError, _context: &mut Context) -> String {
    error.to_string()
}

fn install_globals(context: &mut Context, base_url: &Url, options: &ScriptOptions) -> JsResult<()> {
    let cache = ObjectInitializer::new(context).build();
    context.register_global_property(
        JsString::from("__nexium_nodes"),
        cache.clone(),
        Attribute::empty(),
    )?;
    let functions = ObjectInitializer::new(context).build();
    context.register_global_property(
        JsString::from("__nexium_functions"),
        functions.clone(),
        Attribute::empty(),
    )?;
    with_state(|state| {
        state.node_cache = Some(cache);
        state.function_store = Some(functions);
    });

    let document_object = dom::wrap_node(dom::document_node(), context)?;
    context.register_global_property(
        JsString::from("document"),
        document_object,
        Attribute::all(),
    )?;
    let window = context.global_object();
    context.register_global_property(JsString::from("window"), window.clone(), Attribute::all())?;
    context.register_global_property(JsString::from("self"), window, Attribute::all())?;

    install_console(context)?;
    install_navigator(context, options)?;
    install_location(context, base_url)?;
    install_timers(context)?;
    events::install_globals(context)?;
    fetch::install_globals(context)?;
    webapi::install_globals(context, options)?;
    observers::install_globals(context)?;
    indexeddb::install_globals(context)?;
    canvas::install_globals(context)?;
    websockets::install_globals(context)?;
    workers::install_globals(context)?;

    let alert = NativeFunction::from_fn_ptr(js_alert);
    context.register_global_callable(JsString::from("alert"), 1, alert)?;
    let confirm = NativeFunction::from_fn_ptr(js_confirm);
    context.register_global_callable(JsString::from("confirm"), 1, confirm)?;
    let prompt = NativeFunction::from_fn_ptr(js_prompt);
    context.register_global_callable(JsString::from("prompt"), 1, prompt)?;
    let computed_style = NativeFunction::from_fn_ptr(dom::js_get_computed_style);
    context.register_global_callable(JsString::from("getComputedStyle"), 1, computed_style)?;

    Ok(())
}

fn install_console(context: &mut Context) -> JsResult<()> {
    let realm = context.realm().clone();
    let mut console = ObjectInitializer::new(context);
    for (name, function) in [
        ("log", NativeFunction::from_fn_ptr(js_console_log)),
        ("info", NativeFunction::from_fn_ptr(js_console_info)),
        ("warn", NativeFunction::from_fn_ptr(js_console_warn)),
        ("error", NativeFunction::from_fn_ptr(js_console_error)),
        ("debug", NativeFunction::from_fn_ptr(js_console_debug)),
    ] {
        console.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let console = console.build();
    context.register_global_property(JsString::from("console"), console, Attribute::all())
}

fn push_console(level: &str, args: &[JsValue], context: &mut Context) -> JsValue {
    let text = args
        .iter()
        .map(|value| value_to_display(value, context))
        .collect::<Vec<_>>()
        .join(" ");
    with_state(|state| {
        state.console.push(ConsoleMessage {
            level: level.to_owned(),
            text,
        });
    });
    JsValue::undefined()
}

fn js_console_log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(push_console("log", args, context))
}
fn js_console_info(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(push_console("info", args, context))
}
fn js_console_warn(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(push_console("warn", args, context))
}
fn js_console_error(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(push_console("error", args, context))
}
fn js_console_debug(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(push_console("debug", args, context))
}

fn js_alert(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(push_console("alert", args, context))
}
fn js_confirm(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(false))
}
fn js_prompt(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::null())
}

fn install_navigator(context: &mut Context, options: &ScriptOptions) -> JsResult<()> {
    let languages = JsArray::new(context)?;
    languages.push(js_string_value("en-US"), context)?;
    languages.push(js_string_value("en"), context)?;
    let mut navigator = ObjectInitializer::new(context);
    navigator.property(
        JsString::from("userAgent"),
        js_string_value(&options.user_agent),
        Attribute::all(),
    );
    navigator.property(
        JsString::from("language"),
        js_string_value("en-US"),
        Attribute::all(),
    );
    navigator.property(
        JsString::from("languages"),
        JsValue::from(languages),
        Attribute::all(),
    );
    navigator.property(
        JsString::from("platform"),
        js_string_value("Linux aarch64"),
        Attribute::all(),
    );
    let navigator = navigator.build();
    context.register_global_property(JsString::from("navigator"), navigator, Attribute::all())
}

fn install_location(context: &mut Context, base_url: &Url) -> JsResult<()> {
    let mut location = ObjectInitializer::new(context);
    location.property(
        JsString::from("href"),
        js_string_value(base_url.as_str()),
        Attribute::all(),
    );
    location.property(
        JsString::from("protocol"),
        js_string_value(&format!("{}:", base_url.scheme())),
        Attribute::all(),
    );
    location.property(
        JsString::from("host"),
        js_string_value(base_url.host_str().unwrap_or("")),
        Attribute::all(),
    );
    location.property(
        JsString::from("hostname"),
        js_string_value(base_url.host_str().unwrap_or("")),
        Attribute::all(),
    );
    location.property(
        JsString::from("pathname"),
        js_string_value(base_url.path()),
        Attribute::all(),
    );
    location.property(
        JsString::from("search"),
        js_string_value(
            base_url
                .query()
                .map(|query| format!("?{query}"))
                .as_deref()
                .unwrap_or(""),
        ),
        Attribute::all(),
    );
    location.property(
        JsString::from("hash"),
        js_string_value(
            base_url
                .fragment()
                .map(|fragment| format!("#{fragment}"))
                .as_deref()
                .unwrap_or(""),
        ),
        Attribute::all(),
    );
    location.property(
        JsString::from("origin"),
        js_string_value(&base_url.origin().ascii_serialization()),
        Attribute::all(),
    );
    let location = location.build();
    context.register_global_property(JsString::from("location"), location, Attribute::all())
}

fn install_timers(context: &mut Context) -> JsResult<()> {
    context.register_global_callable(
        JsString::from("setTimeout"),
        2,
        NativeFunction::from_fn_ptr(js_set_timeout),
    )?;
    context.register_global_callable(
        JsString::from("clearTimeout"),
        1,
        NativeFunction::from_fn_ptr(js_clear_timeout),
    )?;
    context.register_global_callable(
        JsString::from("setInterval"),
        2,
        NativeFunction::from_fn_ptr(js_set_interval),
    )?;
    context.register_global_callable(
        JsString::from("clearInterval"),
        1,
        NativeFunction::from_fn_ptr(js_clear_interval),
    )?;
    context.register_global_callable(
        JsString::from("requestAnimationFrame"),
        1,
        NativeFunction::from_fn_ptr(js_request_animation_frame),
    )?;
    context.register_global_callable(
        JsString::from("cancelAnimationFrame"),
        1,
        NativeFunction::from_fn_ptr(js_cancel_animation_frame),
    )?;
    Ok(())
}

fn schedule_timer(
    callback: &JsValue,
    delay_ms: f64,
    interval: Option<Duration>,
    kind: TimerKind,
    context: &mut Context,
) -> JsResult<JsValue> {
    if callback.as_function().is_none() {
        return Err(host_error("timer callback must be a function"));
    }
    let slot = store_function(callback, context)
        .ok_or_else(|| host_error("failed to store timer callback"))?;
    let id = with_state(|state| {
        let id = state.next_timer_id;
        state.next_timer_id += 1;
        state.timers.push(Timer {
            id,
            due: Instant::now() + Duration::from_millis(delay_ms.max(0.0) as u64),
            interval,
            function_index: slot,
            kind,
        });
        id
    })
    .ok_or_else(|| host_error("runtime is not active"))?;
    Ok(JsValue::from(id as f64))
}

fn js_set_timeout(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let callback = args.first().cloned().unwrap_or(JsValue::undefined());
    let delay = args.get(1).and_then(JsValue::as_number).unwrap_or(0.0);
    schedule_timer(&callback, delay, None, TimerKind::Timeout, context)
}

fn js_set_interval(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let callback = args.first().cloned().unwrap_or(JsValue::undefined());
    let delay = args.get(1).and_then(JsValue::as_number).unwrap_or(0.0);
    schedule_timer(
        &callback,
        delay,
        Some(Duration::from_millis(delay.max(1.0) as u64)),
        TimerKind::Interval,
        context,
    )
}

fn js_request_animation_frame(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let callback = args.first().cloned().unwrap_or(JsValue::undefined());
    schedule_timer(&callback, 16.0, None, TimerKind::AnimationFrame, context)
}

fn clear_timer(id: f64) {
    let id = id as u64;
    with_state(|state| state.timers.retain(|timer| timer.id != id));
}

fn js_clear_timeout(
    _this: &JsValue,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(id) = args.first().and_then(JsValue::as_number) {
        clear_timer(id);
    }
    Ok(JsValue::undefined())
}

fn js_clear_interval(
    _this: &JsValue,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(id) = args.first().and_then(JsValue::as_number) {
        clear_timer(id);
    }
    Ok(JsValue::undefined())
}

fn js_cancel_animation_frame(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    js_clear_timeout(this, args, context)
}

type PendingTimer = (u64, Instant, u32, Option<Duration>, bool);

fn next_pending_timer() -> Option<PendingTimer> {
    with_state(|state| {
        state
            .timers
            .iter()
            .min_by_key(|timer| timer.due)
            .map(|timer| {
                (
                    timer.id,
                    timer.due,
                    timer.function_index,
                    timer.interval,
                    timer.kind == TimerKind::AnimationFrame,
                )
            })
    })
    .flatten()
}

fn fire_timer(timer: PendingTimer, context: &mut Context) {
    let (id, _due, function_index, interval, animation_frame) = timer;
    with_state(|state| {
        if let Some(timer) = state.timers.iter_mut().find(|timer| timer.id == id) {
            if let Some(interval) = interval {
                timer.due = Instant::now() + interval;
            } else {
                state.timers.retain(|timer| timer.id != id);
            }
        }
    });
    if let Some(function) = load_function(function_index, context) {
        let result = if animation_frame {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as f64)
                .unwrap_or(0.0);
            function.as_function().expect("function").call(
                &JsValue::undefined(),
                &[JsValue::from(timestamp)],
                context,
            )
        } else {
            function
                .as_function()
                .expect("function")
                .call(&JsValue::undefined(), &[], context)
        };
        if let Err(error) = result {
            let message = format_js_error(&error, context);
            with_state(|state| state.errors.push(message));
        }
        observers::flush(context);
    }
}

fn drive_event_loop(context: &mut Context) {
    let deadline =
        with_state(|state| Instant::now() + state.max_runtime).unwrap_or_else(Instant::now);
    let mut iterations = 0usize;
    loop {
        let _ = context.run_jobs();
        observers::flush(context);
        if indexeddb::process_one(context) {
            continue;
        }
        if websockets::process_events(context) {
            continue;
        }
        if workers::process_events(context) {
            continue;
        }
        let Some(timer) = next_pending_timer() else {
            break;
        };
        let due = timer.1;
        let now = Instant::now();
        if due > deadline {
            break;
        }
        if due > now {
            let wait = due - now;
            std::thread::sleep(wait.min(Duration::from_millis(50)));
            continue;
        }
        iterations += 1;
        if iterations > 1000 {
            with_state(|state| state.errors.push("script timer limit exceeded".to_owned()));
            break;
        }
        fire_timer(timer, context);
    }
    let _ = context.run_jobs();
}

/// Run microtasks, observers, IndexedDB tasks and timers that are already due
/// without waiting for future timers.
fn drain_ready(context: &mut Context) {
    let mut iterations = 0usize;
    loop {
        let _ = context.run_jobs();
        observers::flush(context);
        if indexeddb::process_one(context) {
            continue;
        }
        if websockets::process_events(context) {
            continue;
        }
        if workers::process_events(context) {
            continue;
        }
        let Some(timer) = next_pending_timer() else {
            break;
        };
        if timer.1 > Instant::now() {
            break;
        }
        iterations += 1;
        if iterations > 1000 {
            with_state(|state| state.errors.push("script timer limit exceeded".to_owned()));
            break;
        }
        fire_timer(timer, context);
    }
    let _ = context.run_jobs();
}
