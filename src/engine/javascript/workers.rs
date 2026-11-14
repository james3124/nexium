//! Minimal Web Worker support: background Boa runtimes with postMessage.

use std::sync::mpsc;

use boa_engine::object::ObjectInitializer;
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction};
use serde_json::Value;
use url::Url;

use super::{host_error, indexeddb, js_string_value, modules, string_arg, with_state};
use crate::networking::parse_url;

const WORKER_ID_KEY: &str = "__nexium_worker_id";

pub(crate) struct WorkerHandle {
    pub sender: mpsc::Sender<WorkerCommand>,
}

pub(crate) enum WorkerCommand {
    Message(Value),
    Close,
}

#[derive(Debug)]
pub(crate) enum WorkerEvent {
    Message(u64, Value),
    Error(u64, String),
}

thread_local! {
    static WORKER_OUTBOX: std::cell::RefCell<Option<mpsc::Sender<WorkerEvent>>> =
        const { std::cell::RefCell::new(None) };
    static WORKER_ID: std::cell::RefCell<u64> = const { std::cell::RefCell::new(0) };
    static WORKER_HANDLER: std::cell::RefCell<Option<JsValue>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn install_globals(context: &mut Context) -> JsResult<()> {
    let (sender, receiver) = mpsc::channel();
    let store = ObjectInitializer::new(context).build();
    context.register_global_property(
        JsString::from("__nexium_workers"),
        JsValue::from(store.clone()),
        Attribute::empty(),
    )?;
    with_state(|state| {
        state.worker_event_tx = Some(sender);
        state.worker_event_rx = Some(receiver);
        state.worker_store = Some(store);
    });
    let realm = context.realm().clone();
    let constructor = boa_engine::object::FunctionObjectBuilder::new(
        &realm,
        NativeFunction::from_fn_ptr(js_worker_construct),
    )
    .name(JsString::from("Worker"))
    .length(1)
    .constructor(true)
    .build();
    context.register_global_property(
        JsString::from("Worker"),
        JsValue::from(constructor),
        Attribute::all(),
    )
}

fn js_worker_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(input) = string_arg(args, 0, context) else {
        return Err(host_error("Worker requires a script URL"));
    };
    let url =
        resolve_worker_url(&input).ok_or_else(|| host_error(format!("Invalid URL: {input}")))?;
    let is_module = args
        .get(1)
        .and_then(JsValue::as_object)
        .and_then(|options| options.get(JsString::from("type"), context).ok())
        .and_then(|value| value.to_string(context).ok())
        .is_some_and(|value| value.to_std_string_escaped() == "module");
    let client = with_state(|state| state.client.clone()).flatten();
    let id = with_state(|state| {
        let id = state.worker_next_id;
        state.worker_next_id += 1;
        id
    })
    .ok_or_else(|| host_error("runtime is not active"))?;
    let (sender, receiver) = mpsc::channel();
    let event_sender = with_state(|state| state.worker_event_tx.clone())
        .flatten()
        .ok_or_else(|| host_error("runtime is not active"))?;
    let worker_url = url.clone();
    std::thread::Builder::new()
        .name("nexium-worker".to_owned())
        .spawn(move || run_worker(id, worker_url, client, is_module, receiver, event_sender))
        .map_err(|error| host_error(format!("failed to start worker: {error}")))?;
    with_state(|state| {
        state.workers.insert(id, WorkerHandle { sender });
    });
    build_worker(id, &url, context).map(JsValue::from)
}

fn resolve_worker_url(input: &str) -> Option<Url> {
    if let Ok(url) = Url::parse(input)
        && matches!(url.scheme(), "http" | "https" | "data")
    {
        return Some(url);
    }
    let base = with_state(|state| state.base_url.clone())?;
    base.join(input).ok().or_else(|| parse_url(input).ok())
}

fn run_worker(
    id: u64,
    url: Url,
    client: Option<std::sync::Arc<crate::networking::HttpClient>>,
    is_module: bool,
    receiver: mpsc::Receiver<WorkerCommand>,
    events: mpsc::Sender<WorkerEvent>,
) {
    WORKER_ID.with(|slot| *slot.borrow_mut() = id);
    WORKER_OUTBOX.with(|slot| *slot.borrow_mut() = Some(events.clone()));
    let mut context = if is_module {
        Context::builder()
            .module_loader(modules::ModuleRegistry::new(
                url.clone(),
                client.clone(),
                modules::ImportMap::default(),
            ))
            .build()
            .unwrap_or_default()
    } else {
        Context::default()
    };
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(10_000_000);
    if let Err(error) = install_worker_globals(&mut context) {
        let _ = events.send(WorkerEvent::Error(id, error.to_string()));
        return;
    }
    if is_module {
        if let Some(error) = modules::evaluate_module_url(&url, &mut context) {
            let _ = events.send(WorkerEvent::Error(id, error));
            return;
        }
    } else {
        let code = match super::fetch::fetch_text_sync(url, client) {
            Ok(code) => code,
            Err(error) => {
                let _ = events.send(WorkerEvent::Error(id, error));
                return;
            }
        };
        if let Err(error) = context.eval(boa_engine::Source::from_bytes(&code)) {
            let _ = events.send(WorkerEvent::Error(id, error.to_string()));
            return;
        }
    }
    let _ = context.run_jobs();
    while let Ok(command) = receiver.recv() {
        match command {
            WorkerCommand::Message(value) => {
                let Some(handler) = WORKER_HANDLER.with(|slot| slot.borrow().clone()) else {
                    continue;
                };
                let Some(handler) = handler.as_function() else {
                    continue;
                };
                let data = indexeddb::json_to_js(&value, &mut context);
                let mut event = ObjectInitializer::new(&mut context);
                event
                    .property(
                        JsString::from("type"),
                        js_string_value("message"),
                        Attribute::all(),
                    )
                    .property(JsString::from("data"), data, Attribute::all());
                let event = event.build();
                if let Err(error) =
                    handler.call(&JsValue::undefined(), &[JsValue::from(event)], &mut context)
                {
                    let _ = events.send(WorkerEvent::Error(id, error.to_string()));
                }
                let _ = context.run_jobs();
            }
            WorkerCommand::Close => break,
        }
    }
}

fn install_worker_globals(context: &mut Context) -> JsResult<()> {
    context.register_global_callable(
        JsString::from("postMessage"),
        1,
        NativeFunction::from_fn_ptr(js_worker_post),
    )?;
    context.register_global_callable(
        JsString::from("addEventListener"),
        2,
        NativeFunction::from_fn_ptr(js_worker_add_listener),
    )?;
    let realm = context.realm().clone();
    let descriptor = PropertyDescriptor::builder()
        .get(NativeFunction::from_fn_ptr(js_worker_get_onmessage).to_js_function(&realm))
        .set(NativeFunction::from_fn_ptr(js_worker_set_onmessage).to_js_function(&realm))
        .enumerable(true)
        .configurable(true)
        .build();
    context.global_object().define_property_or_throw(
        JsString::from("onmessage"),
        descriptor,
        context,
    )?;
    let global = context.global_object();
    context.register_global_property(
        JsString::from("self"),
        JsValue::from(global),
        Attribute::all(),
    )
}

fn js_worker_post(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let value = args
        .first()
        .and_then(|value| indexeddb::js_to_json(value, context, 0))
        .unwrap_or(Value::Null);
    WORKER_OUTBOX.with(|slot| {
        if let Some(sender) = slot.borrow().as_ref() {
            let id = WORKER_ID.with(|id| *id.borrow());
            let _ = sender.send(WorkerEvent::Message(id, value.clone()));
        }
    });
    Ok(JsValue::undefined())
}

fn js_worker_add_listener(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let event = string_arg(args, 0, context).unwrap_or_default();
    if event == "message" {
        let callback = args.get(1).cloned().unwrap_or(JsValue::null());
        WORKER_HANDLER.with(|slot| *slot.borrow_mut() = Some(callback));
    }
    Ok(JsValue::undefined())
}

fn js_worker_get_onmessage(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(WORKER_HANDLER
        .with(|slot| slot.borrow().clone())
        .unwrap_or(JsValue::null()))
}

fn js_worker_set_onmessage(
    _this: &JsValue,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    WORKER_HANDLER.with(|slot| *slot.borrow_mut() = args.first().cloned());
    Ok(JsValue::undefined())
}

fn build_worker(id: u64, url: &Url, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(WORKER_ID_KEY),
            JsValue::from(id as f64),
            Attribute::empty(),
        )
        .property(
            JsString::from("url"),
            js_string_value(url.as_str()),
            Attribute::all(),
        )
        .property(
            JsString::from("onmessage"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(JsString::from("onerror"), JsValue::null(), Attribute::all());
    for (name, function) in [
        (
            "postMessage",
            js_main_post as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
        ),
        ("terminate", js_main_terminate),
        ("addEventListener", js_main_add_listener),
        ("removeEventListener", js_main_remove_listener),
    ] {
        object.property(
            JsString::from(name),
            NativeFunction::from_fn_ptr(function).to_js_function(&realm),
            Attribute::all(),
        );
    }
    let worker = object.build();
    if let Some(store) = with_state(|state| state.worker_store.clone()).flatten() {
        store.set(
            JsString::from(id.to_string()),
            JsValue::from(worker.clone()),
            false,
            context,
        )?;
    }
    Ok(worker)
}

fn worker_id(this: &JsValue, context: &mut Context) -> Option<u64> {
    this.as_object()?
        .get(JsString::from(WORKER_ID_KEY), context)
        .ok()?
        .as_number()
        .map(|value| value as u64)
}

fn js_main_post(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = worker_id(this, context).ok_or_else(|| host_error("invalid Worker"))?;
    let value = args
        .first()
        .and_then(|value| indexeddb::js_to_json(value, context, 0))
        .unwrap_or(Value::Null);
    with_state(|state| {
        if let Some(worker) = state.workers.get(&id) {
            let _ = worker.sender.send(WorkerCommand::Message(value));
        }
    });
    Ok(JsValue::undefined())
}

fn js_main_terminate(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = worker_id(this, context).ok_or_else(|| host_error("invalid Worker"))?;
    with_state(|state| {
        if let Some(worker) = state.workers.remove(&id) {
            let _ = worker.sender.send(WorkerCommand::Close);
        }
    });
    Ok(JsValue::undefined())
}

fn js_main_add_listener(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let event = string_arg(args, 0, context).unwrap_or_default();
    let callback = args.get(1).cloned().unwrap_or(JsValue::null());
    if let Some(object) = this.as_object() {
        let _ = object.set(
            JsString::from(format!("on{event}")),
            callback,
            false,
            context,
        );
    }
    Ok(JsValue::undefined())
}

fn js_main_remove_listener(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let event = string_arg(args, 0, context).unwrap_or_default();
    if let Some(object) = this.as_object() {
        let _ = object.set(
            JsString::from(format!("on{event}")),
            JsValue::null(),
            false,
            context,
        );
    }
    Ok(JsValue::undefined())
}

pub(crate) fn process_events(context: &mut Context) -> bool {
    let event = with_state(|state| {
        state
            .worker_event_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
    })
    .flatten();
    let Some(event) = event else {
        return false;
    };
    let (id, name, data, message) = match event {
        WorkerEvent::Message(id, value) => (id, "message", Some(value), String::new()),
        WorkerEvent::Error(id, error) => (id, "error", None, error),
    };
    if let Some(store) = with_state(|state| state.worker_store.clone()).flatten()
        && let Ok(value) = store.get(JsString::from(id.to_string()), context)
        && let Some(worker) = value.as_object()
    {
        let data = data
            .as_ref()
            .map(|data| indexeddb::json_to_js(data, context));
        let mut event = ObjectInitializer::new(context);
        event.property(
            JsString::from("type"),
            js_string_value(name),
            Attribute::all(),
        );
        if let Some(data) = data {
            event.property(JsString::from("data"), data, Attribute::all());
        }
        if !message.is_empty() {
            event.property(
                JsString::from("message"),
                js_string_value(&message),
                Attribute::all(),
            );
        }
        let event = event.build();
        if let Ok(handler) = worker.get(JsString::from(format!("on{name}")), context)
            && let Some(handler) = handler.as_function()
            && let Err(error) = handler.call(
                &JsValue::from(worker.clone()),
                &[JsValue::from(event)],
                context,
            )
        {
            with_state(|state| state.errors.push(error.to_string()));
        }
    }
    true
}
