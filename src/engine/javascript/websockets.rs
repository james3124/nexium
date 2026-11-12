//! Minimal WebSocket client exposed to page JavaScript.

use std::sync::mpsc;
use std::time::Duration;

use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use super::{host_error, js_string_value, string_arg, value_to_display, with_state};

const WS_ID_KEY: &str = "__nexium_ws_id";

pub(crate) struct WebSocketHandle {
    pub sender: tokio::sync::mpsc::UnboundedSender<WsCommand>,
}

pub(crate) enum WsCommand {
    Text(String),
    Close,
}

#[derive(Debug)]
pub(crate) enum WsEvent {
    Opened(u64),
    Message(u64, String),
    Closed(u64, Option<u16>, String),
    Error(u64, String),
}

pub(crate) fn install_globals(context: &mut Context) -> JsResult<()> {
    let (sender, receiver) = mpsc::channel();
    let store = ObjectInitializer::new(context).build();
    context.register_global_property(
        JsString::from("__nexium_websockets"),
        JsValue::from(store.clone()),
        Attribute::empty(),
    )?;
    with_state(|state| {
        state.ws_event_tx = Some(sender);
        state.ws_event_rx = Some(receiver);
        state.ws_store = Some(store);
    });
    let realm = context.realm().clone();
    let constructor = boa_engine::object::FunctionObjectBuilder::new(
        &realm,
        NativeFunction::from_fn_ptr(js_websocket_construct),
    )
    .name(JsString::from("WebSocket"))
    .length(1)
    .constructor(true)
    .build();
    context.register_global_property(
        JsString::from("WebSocket"),
        JsValue::from(constructor),
        Attribute::all(),
    )
}

fn js_websocket_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(input) = string_arg(args, 0, context) else {
        return Err(host_error("WebSocket requires a URL"));
    };
    let url = resolve_url(&input).ok_or_else(|| host_error(format!("Invalid URL: {input}")))?;
    let id = with_state(|state| {
        let id = state.ws_next_id;
        state.ws_next_id += 1;
        id
    })
    .ok_or_else(|| host_error("runtime is not active"))?;
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let event_sender = with_state(|state| state.ws_event_tx.clone())
        .flatten()
        .ok_or_else(|| host_error("runtime is not active"))?;
    let event_target = event_sender.clone();
    let socket_url = url.clone();
    std::thread::Builder::new()
        .name("nexium-websocket".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                let _ = event_target.send(WsEvent::Error(id, "websocket runtime".to_owned()));
                return;
            };
            runtime.block_on(async move {
                let socket = tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio_tungstenite::connect_async(socket_url.as_str()),
                )
                .await;
                let socket = match socket {
                    Ok(Ok((socket, _))) => socket,
                    Ok(Err(error)) => {
                        let _ = event_target.send(WsEvent::Error(id, error.to_string()));
                        let _ = event_target.send(WsEvent::Closed(id, None, String::new()));
                        return;
                    }
                    Err(_) => {
                        let _ = event_target.send(WsEvent::Error(id, "connect timeout".to_owned()));
                        let _ = event_target.send(WsEvent::Closed(id, None, String::new()));
                        return;
                    }
                };
                let _ = event_target.send(WsEvent::Opened(id));
                let (mut write, mut read) = socket.split();
                let mut receiver = receiver;
                loop {
                    tokio::select! {
                        incoming = read.next() => {
                            match incoming {
                                Some(Ok(Message::Text(text))) => {
                                    let _ = event_target.send(WsEvent::Message(id, text.to_string()));
                                }
                                Some(Ok(Message::Binary(bytes))) => {
                                    let text = String::from_utf8_lossy(&bytes).into_owned();
                                    let _ = event_target.send(WsEvent::Message(id, text));
                                }
                                Some(Ok(Message::Close(frame))) => {
                                    let code = frame.as_ref().map(|frame| frame.code.into());
                                    let reason = frame.map(|frame| frame.reason.to_string()).unwrap_or_default();
                                    let _ = event_target.send(WsEvent::Closed(id, code, reason));
                                    break;
                                }
                                Some(Err(error)) => {
                                    let _ = event_target.send(WsEvent::Error(id, error.to_string()));
                                    break;
                                }
                                None => {
                                    let _ = event_target.send(WsEvent::Closed(id, None, String::new()));
                                    break;
                                }
                                _ => {}
                            }
                        }
                        command = receiver.recv() => {
                            match command {
                                Some(WsCommand::Text(text)) => {
                                    if let Err(error) = write.send(Message::Text(text.into())).await {
                                        let _ = event_target.send(WsEvent::Error(id, error.to_string()));
                                        break;
                                    }
                                }
                                Some(WsCommand::Close) | None => {
                                    let _ = write.close().await;
                                    break;
                                }
                            }
                        }
                    }
                }
            });
        })
        .map_err(|error| host_error(format!("failed to start websocket: {error}")))?;
    if let Some(store) = with_state(|state| state.ws_store.clone()).flatten() {
        with_state(|state| {
            state.websockets.insert(id, WebSocketHandle { sender });
        });
        let _ = store.set(
            JsString::from(id.to_string()),
            JsValue::null(),
            false,
            context,
        );
    }
    build_socket(id, &url, context).map(JsValue::from)
}

fn resolve_url(input: &str) -> Option<Url> {
    let base = with_state(|state| state.base_url.clone());
    let mut url = Url::parse(input)
        .ok()
        .or_else(|| base.as_ref()?.join(input).ok())?;
    match url.scheme() {
        "http" => {
            let _ = url.set_scheme("ws");
        }
        "https" => {
            let _ = url.set_scheme("wss");
        }
        "ws" | "wss" => {}
        _ => return None,
    }
    Some(url)
}

fn build_socket(id: u64, url: &Url, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(WS_ID_KEY),
            JsValue::from(id as f64),
            Attribute::empty(),
        )
        .property(
            JsString::from("url"),
            js_string_value(url.as_str()),
            Attribute::all(),
        )
        .property(
            JsString::from("readyState"),
            JsValue::from(0.0),
            Attribute::all(),
        )
        .property(JsString::from("onopen"), JsValue::null(), Attribute::all())
        .property(
            JsString::from("onmessage"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(JsString::from("onerror"), JsValue::null(), Attribute::all())
        .property(JsString::from("onclose"), JsValue::null(), Attribute::all())
        .property(
            JsString::from("binaryType"),
            js_string_value("blob"),
            Attribute::all(),
        );
    for (name, function) in [
        (
            "send",
            js_send as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
        ),
        ("close", js_close),
        ("addEventListener", js_add_listener),
        ("removeEventListener", js_remove_listener),
    ] {
        object.property(
            JsString::from(name),
            NativeFunction::from_fn_ptr(function).to_js_function(&realm),
            Attribute::all(),
        );
    }
    let socket = object.build();
    if let Some(store) = with_state(|state| state.ws_store.clone()).flatten() {
        store.set(
            JsString::from(id.to_string()),
            JsValue::from(socket.clone()),
            false,
            context,
        )?;
    }
    Ok(socket)
}

fn socket_id(this: &JsValue, context: &mut Context) -> Option<u64> {
    this.as_object()?
        .get(JsString::from(WS_ID_KEY), context)
        .ok()?
        .as_number()
        .map(|value| value as u64)
}

fn js_send(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = socket_id(this, context).ok_or_else(|| host_error("invalid WebSocket"))?;
    let text = value_to_display(args.first().unwrap_or(&JsValue::undefined()), context);
    with_state(|state| {
        if let Some(socket) = state.websockets.get(&id) {
            let _ = socket.sender.send(WsCommand::Text(text));
        }
    });
    Ok(JsValue::undefined())
}

fn js_close(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = socket_id(this, context).ok_or_else(|| host_error("invalid WebSocket"))?;
    with_state(|state| {
        if let Some(socket) = state.websockets.get(&id) {
            let _ = socket.sender.send(WsCommand::Close);
        }
    });
    Ok(JsValue::undefined())
}

fn js_add_listener(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
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

fn js_remove_listener(
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
            .ws_event_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
    })
    .flatten();
    let Some(event) = event else {
        return false;
    };
    let (id, name, data) = match event {
        WsEvent::Opened(id) => (id, "open", serde_json::json!({})),
        WsEvent::Message(id, text) => (id, "message", serde_json::json!({ "data": text })),
        WsEvent::Closed(id, code, reason) => (
            id,
            "close",
            serde_json::json!({ "code": code, "reason": reason, "wasClean": true }),
        ),
        WsEvent::Error(id, message) => (id, "error", serde_json::json!({ "message": message })),
    };
    if let Some(store) = with_state(|state| state.ws_store.clone()).flatten()
        && let Ok(value) = store.get(JsString::from(id.to_string()), context)
        && let Some(socket) = value.as_object()
    {
        let ready_state = if name == "open" {
            1.0
        } else if name == "close" || name == "error" {
            3.0
        } else {
            1.0
        };
        let _ = socket.set(
            JsString::from("readyState"),
            JsValue::from(ready_state),
            false,
            context,
        );
        let mut event = ObjectInitializer::new(context);
        event
            .property(
                JsString::from("type"),
                js_string_value(name),
                Attribute::all(),
            )
            .property(
                JsString::from("target"),
                JsValue::from(socket.clone()),
                Attribute::all(),
            );
        if let Some(object) = data.as_object() {
            for (key, value) in object {
                let value = match value {
                    serde_json::Value::String(text) => js_string_value(text),
                    serde_json::Value::Number(number) => {
                        JsValue::from(number.as_f64().unwrap_or(0.0))
                    }
                    serde_json::Value::Bool(value) => JsValue::from(*value),
                    _ => JsValue::null(),
                };
                event.property(JsString::from(key.as_str()), value, Attribute::all());
            }
        }
        let event = event.build();
        if let Ok(handler) = socket.get(JsString::from(format!("on{name}")), context)
            && let Some(handler) = handler.as_function()
            && let Err(error) = handler.call(
                &JsValue::from(socket.clone()),
                &[JsValue::from(event)],
                context,
            )
        {
            with_state(|state| state.errors.push(error.to_string()));
        }
    }
    if name == "close" || name == "error" {
        with_state(|state| state.websockets.remove(&id));
    }
    true
}
