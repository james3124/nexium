//! DOM event listeners and dispatch.

use std::cell::RefCell;
use std::collections::HashSet;

use boa_engine::object::ObjectInitializer;
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction};

use super::dom::{document_node, wrap_node};
use super::{
    Listener, SessionEvent, document_rc, host_error, js_string_value, load_function,
    node_id_from_this, store_function, string_arg, value_to_display, with_state,
};
use crate::engine::dom::NodeId;

thread_local! {
    /// Function slots registered with `{ passive: true }`. The flag lives here
    /// because the shared listener record is owned by the runtime state; the
    /// registry is cleared whenever a new runtime installs its globals.
    static PASSIVE_LISTENERS: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
}

fn with_passive_listeners<R>(f: impl FnOnce(&mut HashSet<u32>) -> R) -> R {
    PASSIVE_LISTENERS.with(|cell| f(&mut cell.borrow_mut()))
}

fn listener_is_passive(slot: u32) -> bool {
    PASSIVE_LISTENERS.with(|cell| cell.borrow().contains(&slot))
}

pub(crate) fn install_globals(context: &mut Context) -> JsResult<()> {
    with_passive_listeners(HashSet::clear);
    context.register_global_callable(
        JsString::from("addEventListener"),
        3,
        NativeFunction::from_fn_ptr(js_add_listener),
    )?;
    context.register_global_callable(
        JsString::from("removeEventListener"),
        3,
        NativeFunction::from_fn_ptr(js_remove_listener),
    )?;
    context.register_global_callable(
        JsString::from("dispatchEvent"),
        1,
        NativeFunction::from_fn_ptr(js_dispatch_event),
    )?;
    let realm = context.realm().clone();
    let handlers: [(
        &str,
        boa_engine::native_function::NativeFunctionPointer,
        boa_engine::native_function::NativeFunctionPointer,
    ); 3] = [
        ("onload", js_get_window_load, js_set_window_load),
        ("onerror", js_get_window_error, js_set_window_error),
        ("onclick", js_get_window_click, js_set_window_click),
    ];
    for (name, get, set) in handlers {
        let descriptor = PropertyDescriptor::builder()
            .get(NativeFunction::from_fn_ptr(get).to_js_function(&realm))
            .set(NativeFunction::from_fn_ptr(set).to_js_function(&realm))
            .enumerable(true)
            .configurable(true)
            .build();
        context.global_object().define_property_or_throw(
            JsString::from(name),
            descriptor,
            context,
        )?;
    }
    Ok(())
}

fn js_get_window_load(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    get_property_listener(this, "load", context)
}
fn js_set_window_load(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    set_property_listener(this, "load", args, context)
}
fn js_get_window_error(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    get_property_listener(this, "error", context)
}
fn js_set_window_error(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    set_property_listener(this, "error", args, context)
}
fn js_get_window_click(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    get_property_listener(this, "click", context)
}
fn js_set_window_click(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    set_property_listener(this, "click", args, context)
}

fn event_target_node(this: &JsValue, context: &mut Context) -> NodeId {
    node_id_from_this(this, context).unwrap_or_else(document_node)
}

pub(crate) fn make_event(
    event_type: &str,
    bubbles: bool,
    cancelable: bool,
    context: &mut Context,
) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let mut event = ObjectInitializer::new(context);
    event
        .property(
            JsString::from("type"),
            js_string_value(event_type),
            Attribute::all(),
        )
        .property(
            JsString::from("bubbles"),
            JsValue::from(bubbles),
            Attribute::all(),
        )
        .property(
            JsString::from("cancelable"),
            JsValue::from(cancelable),
            Attribute::all(),
        )
        .property(JsString::from("target"), JsValue::null(), Attribute::all())
        .property(
            JsString::from("currentTarget"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(
            JsString::from("defaultPrevented"),
            JsValue::from(false),
            Attribute::all(),
        )
        .property(
            JsString::from("__nexium_stopped"),
            JsValue::from(false),
            Attribute::WRITABLE,
        )
        .property(
            JsString::from("__nexium_prevented"),
            JsValue::from(false),
            Attribute::WRITABLE,
        )
        .property(
            JsString::from("__nexium_immediate"),
            JsValue::from(false),
            Attribute::WRITABLE,
        )
        .property(
            JsString::from("__nexium_passive"),
            JsValue::from(false),
            Attribute::WRITABLE,
        )
        .property(
            JsString::from("__nexium_trusted"),
            JsValue::from(false),
            Attribute::WRITABLE,
        )
        .accessor(
            JsString::from("isTrusted"),
            Some(NativeFunction::from_fn_ptr(js_event_is_trusted).to_js_function(&realm)),
            None,
            Attribute::all(),
        );
    for (name, function) in [
        (
            "preventDefault",
            NativeFunction::from_fn_ptr(js_prevent_default),
        ),
        (
            "stopPropagation",
            NativeFunction::from_fn_ptr(js_stop_propagation),
        ),
        (
            "stopImmediatePropagation",
            NativeFunction::from_fn_ptr(js_stop_immediate),
        ),
    ] {
        event.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(event.build())
}

/// Build the event object for a host (browser) generated input event.
///
/// Every `SessionEvent` originates from the host interaction layer, so the
/// resulting event is trusted (`event.isTrusted === true`). Events created by
/// page scripts go through [`make_event`] and stay untrusted.
pub(crate) fn make_input_event(input: &SessionEvent, context: &mut Context) -> JsResult<JsObject> {
    let event = make_event(&input.event_type, input.bubbles, input.cancelable, context)?;
    mark_trusted(&event, context);
    for (name, number) in [
        ("clientX", input.client_x),
        ("clientY", input.client_y),
        ("pageX", input.page_x.or(input.client_x)),
        ("pageY", input.page_y.or(input.client_y)),
        ("screenX", input.client_x),
        ("screenY", input.client_y),
        ("deltaX", input.delta_x),
        ("deltaY", input.delta_y),
    ] {
        let Some(number) = number else {
            continue;
        };
        let _ = event.set(JsString::from(name), JsValue::from(number), false, context);
    }
    if let Some(button) = input.button {
        let _ = event.set(
            JsString::from("button"),
            JsValue::from(button),
            false,
            context,
        );
    }
    if let Some(buttons) = input.buttons {
        let _ = event.set(
            JsString::from("buttons"),
            JsValue::from(buttons),
            false,
            context,
        );
    }
    if let Some(detail) = input.detail {
        let _ = event.set(
            JsString::from("detail"),
            JsValue::from(detail),
            false,
            context,
        );
    }
    if let Some(key) = &input.key {
        let _ = event.set(JsString::from("key"), js_string_value(key), false, context);
    }
    if let Some(code) = &input.code {
        let _ = event.set(
            JsString::from("code"),
            js_string_value(code),
            false,
            context,
        );
    }
    if let Some(text) = &input.text {
        let _ = event.set(
            JsString::from("data"),
            js_string_value(text),
            false,
            context,
        );
    }
    for (name, pressed) in [
        ("ctrlKey", input.ctrl),
        ("altKey", input.alt),
        ("shiftKey", input.shift),
        ("metaKey", input.meta),
    ] {
        let _ = event.set(JsString::from(name), JsValue::from(pressed), false, context);
    }
    Ok(event)
}

pub(crate) fn is_prevented(event: &JsObject, context: &mut Context) -> bool {
    event
        .get(JsString::from("__nexium_prevented"), context)
        .ok()
        .and_then(|value| value.as_boolean())
        .unwrap_or(false)
}

fn js_event_is_trusted(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let trusted = this
        .as_object()
        .and_then(|object| object.get(JsString::from("__nexium_trusted"), context).ok())
        .and_then(|value| value.as_boolean())
        .unwrap_or(false);
    Ok(JsValue::from(trusted))
}

/// Mark a host-generated event as trusted (`event.isTrusted === true`).
/// Events created by script must stay untrusted.
pub(crate) fn mark_trusted(event: &JsObject, context: &mut Context) {
    let _ = event.set(
        JsString::from("__nexium_trusted"),
        JsValue::from(true),
        false,
        context,
    );
}

/// Build an input event that represents host (browser) input, i.e. a trusted
/// event. The interaction layer can use this instead of [`make_input_event`].
#[allow(dead_code)]
pub(crate) fn make_trusted_input_event(
    input: &SessionEvent,
    context: &mut Context,
) -> JsResult<JsObject> {
    let event = make_input_event(input, context)?;
    mark_trusted(&event, context);
    Ok(event)
}

fn js_prevent_default(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    // `preventDefault()` on a passive listener must not cancel the event.
    let passive = object
        .get(JsString::from("__nexium_passive"), context)
        .ok()
        .and_then(|value| value.as_boolean())
        .unwrap_or(false);
    if passive {
        return Ok(JsValue::undefined());
    }
    let _ = object.set(
        JsString::from("defaultPrevented"),
        JsValue::from(true),
        false,
        context,
    );
    let _ = object.set(
        JsString::from("__nexium_prevented"),
        JsValue::from(true),
        false,
        context,
    );
    Ok(JsValue::undefined())
}

fn js_stop_propagation(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(object) = this.as_object() {
        let _ = object.set(
            JsString::from("__nexium_stopped"),
            JsValue::from(true),
            false,
            context,
        );
    }
    Ok(JsValue::undefined())
}

fn js_stop_immediate(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(object) = this.as_object() {
        let _ = object.set(
            JsString::from("__nexium_stopped"),
            JsValue::from(true),
            false,
            context,
        );
        let _ = object.set(
            JsString::from("__nexium_immediate"),
            JsValue::from(true),
            false,
            context,
        );
    }
    Ok(JsValue::undefined())
}

pub(crate) fn js_add_listener(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = event_target_node(this, context);
    let Some(event_type) = string_arg(args, 0, context) else {
        return Err(host_error("addEventListener requires an event type"));
    };
    let callback = args.get(1).cloned().unwrap_or(JsValue::undefined());
    let capture = args
        .get(2)
        .and_then(JsValue::as_boolean)
        .or_else(|| {
            args.get(2)
                .and_then(JsValue::as_object)
                .and_then(|options| {
                    options
                        .get(JsString::from("capture"), context)
                        .ok()
                        .and_then(|value| value.as_boolean())
                })
        })
        .unwrap_or(false);
    let options = args.get(2).and_then(JsValue::as_object);
    let once = options
        .as_ref()
        .and_then(|options| options.get(JsString::from("once"), context).ok())
        .and_then(|value| value.as_boolean())
        .unwrap_or(false);
    let passive = options
        .as_ref()
        .and_then(|options| options.get(JsString::from("passive"), context).ok())
        .and_then(|value| value.as_boolean())
        .unwrap_or(false);
    add_listener(
        node,
        &event_type,
        &callback,
        capture,
        once,
        passive,
        false,
        context,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_listener(
    node: NodeId,
    event_type: &str,
    callback: &JsValue,
    capture: bool,
    once: bool,
    passive: bool,
    property: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    if callback.as_function().is_none() {
        return Ok(JsValue::undefined());
    }
    let slot = store_function(callback, context)
        .ok_or_else(|| host_error("failed to store event listener"))?;
    if passive {
        with_passive_listeners(|slots| {
            slots.insert(slot);
        });
    }
    with_state(|state| {
        if property {
            state.listeners.retain(|listener| {
                !(listener.node == node && listener.event == event_type && listener.property)
            });
        }
        state.listeners.push(Listener {
            node,
            event: event_type.to_owned(),
            function_index: slot,
            once,
            capture,
            property,
        });
    });
    Ok(JsValue::undefined())
}

pub(crate) fn get_property_listener(
    this: &JsValue,
    event_type: &str,
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = event_target_node(this, context);
    let index = with_state(|state| {
        state
            .listeners
            .iter()
            .rev()
            .find(|listener| {
                listener.node == node && listener.event == event_type && listener.property
            })
            .map(|listener| listener.function_index)
    })
    .flatten();
    match index.and_then(|index| load_function(index, context)) {
        Some(function) => Ok(function),
        None => Ok(JsValue::null()),
    }
}

pub(crate) fn set_property_listener(
    this: &JsValue,
    event_type: &str,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = event_target_node(this, context);
    let value = args.first().cloned().unwrap_or(JsValue::null());
    with_state(|state| {
        state.listeners.retain(|listener| {
            !(listener.node == node && listener.event == event_type && listener.property)
        });
    });
    if !value.is_null() && value.as_function().is_some() {
        add_listener(node, event_type, &value, false, false, false, true, context)?;
    }
    Ok(JsValue::undefined())
}

pub(crate) fn js_remove_listener(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = event_target_node(this, context);
    let Some(event_type) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let callback = args.get(1).cloned().unwrap_or(JsValue::undefined());
    let indices: Vec<u32> = with_state(|state| {
        state
            .listeners
            .iter()
            .filter(|listener| listener.node == node && listener.event == event_type)
            .map(|listener| listener.function_index)
            .collect()
    })
    .unwrap_or_default();
    let mut remove = Vec::new();
    for index in indices {
        if let Some(stored) = load_function(index, context)
            && JsValue::strict_equals(&stored, &callback)
        {
            remove.push(index);
        }
    }
    with_state(|state| {
        state.listeners.retain(|listener| {
            !(listener.node == node
                && listener.event == event_type
                && remove.contains(&listener.function_index))
        });
    });
    Ok(JsValue::undefined())
}

pub(crate) fn js_dispatch_event(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = event_target_node(this, context);
    let Some(event) = args.first().and_then(JsValue::as_object) else {
        return Err(host_error("dispatchEvent requires an event object"));
    };
    let event_type = event
        .get(JsString::from("type"), context)
        .ok()
        .map(|value| value_to_display(&value, context))
        .unwrap_or_default();
    if event_type.is_empty() {
        return Err(host_error("event object has no type"));
    }
    dispatch_event(node, &event, &event_type, context);
    Ok(JsValue::from(true))
}

pub(crate) fn js_click(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(node) = node_id_from_this(this, context) else {
        return Ok(JsValue::undefined());
    };
    let event = make_event("click", true, true, context)?;
    let event_value = JsValue::from(event.clone());
    dispatch_event(node, &event, "click", context);

    // Simple default actions for checkboxes.
    let toggle = {
        let Some(doc) = document_rc() else {
            return Ok(JsValue::undefined());
        };
        let doc = doc.borrow();
        let tag = doc.tag_name(node);
        let input_type = doc
            .get_attribute(node, "type")
            .unwrap_or("")
            .to_ascii_lowercase();
        tag == Some("input") && matches!(input_type.as_str(), "checkbox" | "radio")
    };
    if toggle {
        let prevented = event
            .get(JsString::from("__nexium_prevented"), context)
            .ok()
            .and_then(|value| value.as_boolean())
            .unwrap_or(false);
        if !prevented && let Some(doc) = document_rc() {
            let checked = {
                let doc = doc.borrow();
                doc.get_attribute(node, "checked").is_some()
            };
            let mut doc = doc.borrow_mut();
            if checked {
                let _ = doc.remove_attribute(node, "checked");
            } else {
                let _ = doc.set_attribute(node, "checked", "");
            }
        }
        let change = make_event("change", true, false, context)?;
        dispatch_event(node, &change, "change", context);
    }
    let _ = event_value;
    Ok(JsValue::undefined())
}

pub(crate) fn dispatch_named_with_context(target: NodeId, event_type: &str, context: &mut Context) {
    dispatch_named_event(target, event_type, false, true, context);
}

pub(crate) fn dispatch_named_event(
    target: NodeId,
    event_type: &str,
    bubbles: bool,
    cancelable: bool,
    context: &mut Context,
) {
    let Ok(event) = make_event(event_type, bubbles, cancelable, context) else {
        return;
    };
    // Focus, input, change and document lifecycle events generated by the
    // runtime on behalf of host input are trusted browser events.
    mark_trusted(&event, context);
    dispatch_event(target, &event, event_type, context);
}

pub(crate) fn dispatch_event(
    target: NodeId,
    event: &JsObject,
    event_type: &str,
    context: &mut Context,
) {
    // Host-driven focus changes update the runtime focus state so later
    // keyboard input reaches the newly focused element. Script dispatched
    // events are untrusted and never move the focus.
    if event_flag(event, "__nexium_trusted", context) {
        match event_type {
            "focus" => {
                with_state(|state| state.focused_node = Some(target));
            }
            "blur" => {
                with_state(|state| {
                    if state.focused_node == Some(target) {
                        state.focused_node = None;
                    }
                });
            }
            _ => {}
        }
    }
    let Some(doc) = document_rc() else {
        return;
    };
    let mut path = vec![target];
    path.extend(doc.borrow().ancestors(target));
    let document = doc.borrow().document_node();
    if path.last() != Some(&document) {
        path.push(document);
    }
    let bubbles = event
        .get(JsString::from("bubbles"), context)
        .ok()
        .and_then(|value| value.as_boolean())
        .unwrap_or(true);

    let target_value = wrap_node(target, context).unwrap_or(JsValue::null());
    let _ = event.set(JsString::from("target"), target_value, false, context);
    let event_value = JsValue::from(event.clone());

    // Capture phase: root -> target (capture listeners only).
    for node in path.iter().rev() {
        let (stopped, immediate) = invoke_at(*node, true, event, event_type, &event_value, context);
        if immediate || stopped {
            return;
        }
    }

    // Target phase: non-capture listeners.
    let (stopped, immediate) = invoke_at(target, false, event, event_type, &event_value, context);
    if immediate || stopped || !bubbles {
        return;
    }

    // Bubble phase: parent -> document.
    for node in path.iter().skip(1) {
        let (stopped, immediate) =
            invoke_at(*node, false, event, event_type, &event_value, context);
        if immediate || stopped {
            break;
        }
    }
}

fn event_flag(event: &JsObject, flag: &str, context: &mut Context) -> bool {
    event
        .get(JsString::from(flag), context)
        .ok()
        .and_then(|value| value.as_boolean())
        .unwrap_or(false)
}

fn invoke_at(
    node: NodeId,
    capture_only: bool,
    event: &JsObject,
    event_type: &str,
    event_value: &JsValue,
    context: &mut Context,
) -> (bool, bool) {
    let current = wrap_node(node, context).unwrap_or(JsValue::null());
    let _ = event.set(
        JsString::from("currentTarget"),
        current.clone(),
        false,
        context,
    );
    let listeners: Vec<(u32, bool)> = with_state(|state| {
        state
            .listeners
            .iter()
            .filter(|listener| {
                listener.node == node
                    && listener.event == event_type
                    && listener.capture == capture_only
            })
            .map(|listener| (listener.function_index, listener.once))
            .collect()
    })
    .unwrap_or_default();

    for (function_index, once) in listeners {
        let Some(function) = load_function(function_index, context) else {
            continue;
        };
        let passive = listener_is_passive(function_index);
        if passive {
            // Keep `preventDefault()` a no-op while the listener runs; nested
            // dispatches of the same event object restore the flag afterwards.
            let _ = event.set(
                JsString::from("__nexium_passive"),
                JsValue::from(true),
                false,
                context,
            );
        }
        let result = function
            .as_function()
            .expect("stored listener must be callable")
            .call(&current, std::slice::from_ref(event_value), context);
        if passive {
            let _ = event.set(
                JsString::from("__nexium_passive"),
                JsValue::from(false),
                false,
                context,
            );
        }
        if let Err(error) = result {
            with_state(|state| state.errors.push(error.to_string()));
        }
        if once {
            with_state(|state| {
                state.listeners.retain(|listener| {
                    !(listener.node == node
                        && listener.event == event_type
                        && listener.function_index == function_index
                        && listener.capture == capture_only)
                });
            });
        }
        if event_flag(event, "__nexium_immediate", context) {
            return (true, true);
        }
    }
    if !capture_only {
        invoke_inline_attribute(node, event, event_type, event_value, &current, context);
    }
    (event_flag(event, "__nexium_stopped", context), false)
}

/// Inline `on<event>` attributes act as listeners on their element. The code
/// is compiled as `function(event){...}` so `this` and `event` resolve like a
/// registered handler.
fn invoke_inline_attribute(
    node: NodeId,
    event: &JsObject,
    event_type: &str,
    event_value: &JsValue,
    current: &JsValue,
    context: &mut Context,
) {
    if event_flag(event, "__nexium_immediate", context) {
        return;
    }
    let attribute = format!("on{event_type}");
    let code = {
        let Some(doc) = document_rc() else {
            return;
        };
        let doc = doc.borrow();
        if !doc.is_element(node) {
            return;
        }
        doc.get_attribute(node, &attribute).map(str::to_owned)
    };
    let Some(code) = code else {
        return;
    };
    if code.trim().is_empty() {
        return;
    }
    let source = format!("(function(event){{{code}}})");
    let function = match context.eval(boa_engine::Source::from_bytes(&source)) {
        Ok(value) => value,
        Err(error) => {
            with_state(|state| state.errors.push(error.to_string()));
            return;
        }
    };
    let Some(function) = function.as_function() else {
        return;
    };
    if let Err(error) = function.call(current, std::slice::from_ref(event_value), context) {
        with_state(|state| state.errors.push(error.to_string()));
    }
}
