//! `fetch`, `Response` and a minimal synchronous `XMLHttpRequest` host API.

use std::sync::Arc;

use boa_engine::object::builtins::{JsArray, JsPromise};
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction};
use url::Url;

use super::{host_error, js_string_value, string_arg, value_to_display, with_state};
use crate::networking::{HttpClient, Request};

pub(crate) fn install_globals(context: &mut Context) -> JsResult<()> {
    context.register_global_callable(
        JsString::from("fetch"),
        1,
        NativeFunction::from_fn_ptr(js_fetch),
    )?;
    let realm = context.realm().clone();
    let constructor =
        FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(js_xhr_construct))
            .name(JsString::from("XMLHttpRequest"))
            .length(0)
            .constructor(true)
            .build();
    context.register_global_property(
        JsString::from("XMLHttpRequest"),
        JsValue::from(constructor),
        Attribute::all(),
    )?;
    Ok(())
}

fn absolute_url(input: &str) -> Option<Url> {
    let base = with_state(|state| state.base_url.clone())?;
    base.join(input).ok()
}

fn blocking_fetch(
    client: Option<Arc<HttpClient>>,
    request: Request,
) -> std::result::Result<crate::networking::Response, String> {
    let Some(client) = client else {
        return Err("fetch is unavailable without a network client".to_owned());
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return handle.block_on(async move {
            client
                .fetch(request)
                .await
                .map_err(|error| error.to_string())
        });
    }
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("network runtime: {error}"))?;
        runtime.block_on(async move {
            client
                .fetch(request)
                .await
                .map_err(|error| error.to_string())
        })
    });
    handle
        .join()
        .map_err(|_| "network thread panicked".to_owned())?
}

pub(crate) fn fetch_text_sync(
    url: Url,
    client: Option<Arc<HttpClient>>,
) -> std::result::Result<String, String> {
    let request = Request::get(url);
    match blocking_fetch(client, request) {
        Ok(response) => Ok(String::from_utf8_lossy(&response.body).into_owned()),
        Err(error) => Err(error),
    }
}

pub(crate) fn fetch_bytes_sync(
    url: Url,
    client: Option<Arc<HttpClient>>,
) -> std::result::Result<Vec<u8>, String> {
    let request = Request::get(url);
    blocking_fetch(client, request).map(|response| response.body.to_vec())
}

fn extract_header_pairs(value: &JsValue, context: &mut Context) -> Vec<(String, String)> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let headers_object = object
        .get(JsString::from(super::webapi::HEADERS_KEY), context)
        .ok()
        .and_then(|value| value.as_object())
        .unwrap_or(object);
    let mut pairs = Vec::new();
    if let Ok(keys) = headers_object.own_property_keys(context) {
        for key in keys {
            if let boa_engine::property::PropertyKey::String(name) = key {
                let name = name.to_std_string_escaped();
                if let Ok(value) = headers_object.get(JsString::from(name.clone()), context) {
                    pairs.push((name, value_to_display(&value, context)));
                }
            }
        }
    }
    pairs
}

fn rejected_promise(message: &str, context: &mut Context) -> JsResult<JsValue> {
    let message = message.to_owned();
    let promise = JsPromise::new(
        move |resolvers, context| {
            let _ =
                resolvers
                    .reject
                    .call(&JsValue::undefined(), &[js_string_value(&message)], context);
            Ok(JsValue::undefined())
        },
        context,
    )?;
    Ok(promise.into())
}

fn js_fetch(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut input = string_arg(args, 0, context);
    let mut method = "GET".to_owned();
    let mut body = String::new();
    let mut header_pairs: Vec<(String, String)> = Vec::new();
    let mut aborted = false;

    if let Some(object) = args.first().and_then(JsValue::as_object) {
        let is_request = object
            .get(JsString::from(super::webapi::REQUEST_KEY), context)
            .ok()
            .and_then(|value| value.as_boolean())
            .unwrap_or(false);
        if is_request {
            input = object
                .get(JsString::from("__nexium_url"), context)
                .ok()
                .and_then(|value| value.as_string())
                .map(|value| value.to_std_string_escaped());
            if let Ok(value) = object.get(JsString::from("__nexium_method"), context) {
                method = value_to_display(&value, context);
            }
            if let Ok(value) = object.get(JsString::from("__nexium_body"), context) {
                body = value_to_display(&value, context);
            }
            if let Ok(value) = object.get(JsString::from(super::webapi::HEADERS_KEY), context) {
                header_pairs = extract_header_pairs(&value, context);
            }
            if let Ok(value) = object.get(JsString::from("__nexium_signal"), context)
                && let Some(signal) = value.as_object()
            {
                aborted = signal
                    .get(JsString::from("aborted"), context)
                    .ok()
                    .and_then(|value| value.as_boolean())
                    .unwrap_or(false);
            }
        }
    }

    if let Some(options) = args.get(1).and_then(JsValue::as_object) {
        if let Ok(value) = options.get(JsString::from("method"), context)
            && let Some(value) = value.as_string()
        {
            method = value.to_std_string_escaped();
        }
        if let Ok(value) = options.get(JsString::from("body"), context)
            && !value.is_null_or_undefined()
        {
            body = value_to_display(&value, context);
        }
        if let Ok(headers) = options.get(JsString::from("headers"), context) {
            let pairs = extract_header_pairs(&headers, context);
            if !pairs.is_empty() {
                header_pairs = pairs;
            }
        }
        if let Ok(signal) = options.get(JsString::from("signal"), context)
            && let Some(signal) = signal.as_object()
        {
            aborted = signal
                .get(JsString::from("aborted"), context)
                .ok()
                .and_then(|value| value.as_boolean())
                .unwrap_or(false);
        }
    }

    if aborted {
        return rejected_promise("AbortError: the operation was aborted", context);
    }

    let Some(input) = input else {
        return rejected_promise("fetch requires a URL", context);
    };
    let Some(url) = absolute_url(&input) else {
        return Err(host_error(format!("invalid fetch URL: {input}")));
    };
    let client = with_state(|state| state.client.clone()).flatten();

    let promise = JsPromise::new(
        move |resolvers, context| {
            let mut request = Request::new(
                hyper::Method::from_bytes(method.as_bytes()).unwrap_or(hyper::Method::GET),
                url.clone(),
            )
            .body(body.clone());
            for (name, value) in &header_pairs {
                if let (Ok(header), Ok(header_value)) = (
                    hyper::header::HeaderName::from_bytes(name.as_bytes()),
                    hyper::header::HeaderValue::from_str(value),
                ) {
                    request.headers.insert(header, header_value);
                }
            }
            match blocking_fetch(client.clone(), request) {
                Ok(response) => match build_response(&response, context) {
                    Ok(response_object) => {
                        let _ = resolvers.resolve.call(
                            &JsValue::undefined(),
                            &[JsValue::from(response_object)],
                            context,
                        );
                    }
                    Err(error) => {
                        let _ = resolvers.reject.call(
                            &JsValue::undefined(),
                            &[js_string_value(&error.to_string())],
                            context,
                        );
                    }
                },
                Err(error) => {
                    let _ = resolvers.reject.call(
                        &JsValue::undefined(),
                        &[js_string_value(&error)],
                        context,
                    );
                }
            }
            Ok(JsValue::undefined())
        },
        context,
    )?;
    Ok(promise.into())
}

fn build_response(
    response: &crate::networking::Response,
    context: &mut Context,
) -> JsResult<JsObject> {
    let body = String::from_utf8_lossy(&response.body).into_owned();
    let realm = context.realm().clone();
    let headers_object = {
        let mut get_headers = ObjectInitializer::new(context);
        get_headers.property(
            JsString::from("get"),
            NativeFunction::from_fn_ptr(js_headers_get).to_js_function(&realm),
            Attribute::all(),
        );
        get_headers.build()
    };
    for (name, value) in response.headers.iter() {
        if let Ok(value) = value.to_str() {
            let _ = headers_object.set(
                JsString::from(name.as_str().to_ascii_lowercase()),
                js_string_value(value),
                false,
                context,
            );
        }
    }

    let status = response.status.as_u16();
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("ok"),
            JsValue::from(response.status.is_success()),
            Attribute::all(),
        )
        .property(
            JsString::from("status"),
            JsValue::from(status as f64),
            Attribute::all(),
        )
        .property(
            JsString::from("statusText"),
            js_string_value(response.status.canonical_reason().unwrap_or("")),
            Attribute::all(),
        )
        .property(
            JsString::from("url"),
            js_string_value(response.url.as_str()),
            Attribute::all(),
        )
        .property(
            JsString::from("redirected"),
            JsValue::from(!response.redirects.is_empty()),
            Attribute::all(),
        )
        .property(
            JsString::from("headers"),
            JsValue::from(headers_object),
            Attribute::all(),
        )
        .property(
            JsString::from("__nexium_body"),
            js_string_value(&body),
            Attribute::empty(),
        );
    for (name, function) in [
        ("text", NativeFunction::from_fn_ptr(js_response_text)),
        ("json", NativeFunction::from_fn_ptr(js_response_json)),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(object.build())
}

fn js_headers_get(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(JsValue::null());
    };
    let Some(object) = this.as_object() else {
        return Ok(JsValue::null());
    };
    match object.get(JsString::from(name.to_ascii_lowercase()), context) {
        Ok(value) if !value.is_undefined() => Ok(value),
        _ => Ok(JsValue::null()),
    }
}

fn response_body(this: &JsValue, context: &mut Context) -> Option<String> {
    let object = this.as_object()?;
    object
        .get(JsString::from("__nexium_body"), context)
        .ok()
        .and_then(|value| value.as_string())
        .map(|value| value.to_std_string_escaped())
}

pub(crate) fn settled_promise_value(value: JsValue, context: &mut Context) -> JsResult<JsValue> {
    let promise = JsPromise::new(
        move |resolvers, context| {
            let _ = resolvers.resolve.call(
                &JsValue::undefined(),
                std::slice::from_ref(&value),
                context,
            );
            Ok(JsValue::undefined())
        },
        context,
    )?;
    Ok(promise.into())
}

fn js_response_text(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let body = response_body(this, context).unwrap_or_default();
    settled_promise_value(js_string_value(&body), context)
}

fn js_response_json(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let body = response_body(this, context).unwrap_or_default();
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(value) => settled_promise_value(json_to_js(&value, context), context),
        Err(error) => {
            let promise = JsPromise::new(
                move |resolvers, context| {
                    let _ = resolvers.reject.call(
                        &JsValue::undefined(),
                        &[js_string_value(&format!("invalid JSON: {error}"))],
                        context,
                    );
                    Ok(JsValue::undefined())
                },
                context,
            )?;
            Ok(promise.into())
        }
    }
}

fn json_to_js(value: &serde_json::Value, context: &mut Context) -> JsValue {
    match value {
        serde_json::Value::Null => JsValue::null(),
        serde_json::Value::Bool(value) => JsValue::from(*value),
        serde_json::Value::Number(value) => JsValue::from(value.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(value) => js_string_value(value),
        serde_json::Value::Array(values) => {
            let array = JsArray::new(context).ok();
            if let Some(array) = array {
                for value in values {
                    let _ = array.push(json_to_js(value, context), context);
                }
                JsValue::from(array)
            } else {
                JsValue::null()
            }
        }
        serde_json::Value::Object(map) => {
            let entries: Vec<(String, JsValue)> = map
                .iter()
                .map(|(key, value)| (key.clone(), json_to_js(value, context)))
                .collect();
            let mut object = ObjectInitializer::new(context);
            for (key, value) in entries {
                object.property(JsString::from(key), value, Attribute::all());
            }
            JsValue::from(object.build())
        }
    }
}

fn js_xhr_construct(
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("readyState"),
            JsValue::from(0.0),
            Attribute::all(),
        )
        .property(
            JsString::from("status"),
            JsValue::from(0.0),
            Attribute::all(),
        )
        .property(
            JsString::from("responseText"),
            js_string_value(""),
            Attribute::all(),
        )
        .property(JsString::from("onload"), JsValue::null(), Attribute::all())
        .property(JsString::from("onerror"), JsValue::null(), Attribute::all())
        .property(
            JsString::from("onreadystatechange"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(
            JsString::from("__nexium_method"),
            js_string_value("GET"),
            Attribute::empty(),
        )
        .property(
            JsString::from("__nexium_url"),
            js_string_value(""),
            Attribute::empty(),
        );
    for (name, function) in [
        ("open", NativeFunction::from_fn_ptr(js_xhr_open)),
        (
            "setRequestHeader",
            NativeFunction::from_fn_ptr(js_xhr_set_request_header),
        ),
        ("send", NativeFunction::from_fn_ptr(js_xhr_send)),
        ("abort", NativeFunction::from_fn_ptr(js_xhr_abort)),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let xhr = object.build();
    xhr.set(
        JsString::from("__nexium_headers"),
        JsValue::from(ObjectInitializer::new(context).build()),
        false,
        context,
    )?;
    Ok(JsValue::from(xhr))
}

fn js_xhr_open(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    let Some(method) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let Some(url) = string_arg(args, 1, context) else {
        return Ok(JsValue::undefined());
    };
    let _ = object.set(
        JsString::from("__nexium_method"),
        js_string_value(&method),
        false,
        context,
    );
    let _ = object.set(
        JsString::from("__nexium_url"),
        js_string_value(&url),
        false,
        context,
    );
    let _ = object.set(
        JsString::from("readyState"),
        JsValue::from(1.0),
        false,
        context,
    );
    Ok(JsValue::undefined())
}

fn js_xhr_set_request_header(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    let (Some(name), Some(value)) = (string_arg(args, 0, context), string_arg(args, 1, context))
    else {
        return Ok(JsValue::undefined());
    };
    if let Ok(headers_value) = object.get(JsString::from("__nexium_headers"), context)
        && let Some(headers) = headers_value.as_object()
    {
        let _ = headers.set(
            JsString::from(name.to_ascii_lowercase()),
            js_string_value(&value),
            false,
            context,
        );
    }
    Ok(JsValue::undefined())
}

fn js_xhr_send(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(JsValue::undefined());
    };
    let method = object
        .get(JsString::from("__nexium_method"), context)
        .ok()
        .map(|value| value_to_display(&value, context))
        .unwrap_or_else(|| "GET".to_owned());
    let url_text = object
        .get(JsString::from("__nexium_url"), context)
        .ok()
        .map(|value| value_to_display(&value, context))
        .unwrap_or_default();
    let Some(url) = absolute_url(&url_text) else {
        return Ok(JsValue::undefined());
    };
    let body = args
        .first()
        .and_then(|value| value.as_string())
        .map(|value| value.to_std_string_escaped())
        .unwrap_or_default();
    let mut request = Request::new(
        hyper::Method::from_bytes(method.as_bytes()).unwrap_or(hyper::Method::GET),
        url,
    )
    .body(body);
    if let Ok(headers_value) = object.get(JsString::from("__nexium_headers"), context)
        && let Some(headers) = headers_value.as_object()
        && let Ok(keys) = headers.own_property_keys(context)
    {
        for key in keys {
            if let boa_engine::property::PropertyKey::String(name) = key {
                let name = name.to_std_string_escaped();
                if let Ok(value) = headers.get(JsString::from(name.clone()), context) {
                    let value = value_to_display(&value, context);
                    if let (Ok(header), Ok(header_value)) = (
                        hyper::header::HeaderName::from_bytes(name.as_bytes()),
                        hyper::header::HeaderValue::from_str(&value),
                    ) {
                        request.headers.insert(header, header_value);
                    }
                }
            }
        }
    }
    let client = with_state(|state| state.client.clone()).flatten();
    let result = blocking_fetch(client, request);
    let (ready_state, status, text) = match result {
        Ok(response) => (
            4.0,
            response.status.as_u16() as f64,
            String::from_utf8_lossy(&response.body).into_owned(),
        ),
        Err(_) => (4.0, 0.0, String::new()),
    };
    let _ = object.set(
        JsString::from("readyState"),
        JsValue::from(ready_state),
        false,
        context,
    );
    let _ = object.set(
        JsString::from("status"),
        JsValue::from(status),
        false,
        context,
    );
    let _ = object.set(
        JsString::from("responseText"),
        js_string_value(&text),
        false,
        context,
    );
    for handler in ["onreadystatechange", "onload"] {
        if let Ok(value) = object.get(JsString::from(handler), context)
            && let Some(function) = value.as_function()
        {
            let this_value = JsValue::from(object.clone());
            let _ = function.call(&this_value, &[], context);
        }
    }
    Ok(JsValue::undefined())
}

fn js_xhr_abort(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}
