//! Web platform APIs: URL, text encoders, Blob/File/FormData, storage,
//! history, performance, screen and fetch request helpers.

use std::time::Instant;

use base64::Engine as _;
use boa_engine::object::builtins::JsUint8Array;
use boa_engine::object::builtins::{AlignedVec, JsArray, JsArrayBuffer};
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction};
use url::Url;

use super::ScriptOptions;
use super::fetch::settled_promise_value;
use super::{host_error, js_string_value, string_arg, value_to_display, with_state};

pub(crate) const HEADERS_KEY: &str = "__nexium_headers";
pub(crate) const REQUEST_KEY: &str = "__nexium_request";

pub(crate) fn install_globals(context: &mut Context, options: &ScriptOptions) -> JsResult<()> {
    install_constructor(context, "URL", js_url_construct, 1)?;
    install_constructor(context, "URLSearchParams", js_search_params_construct, 0)?;
    install_constructor(context, "TextEncoder", js_text_encoder_construct, 0)?;
    install_constructor(context, "TextDecoder", js_text_decoder_construct, 0)?;
    install_constructor(context, "Blob", js_blob_construct, 0)?;
    install_constructor(context, "File", js_file_construct, 2)?;
    install_constructor(context, "FormData", js_form_data_construct, 0)?;
    install_constructor(context, "Headers", js_headers_construct, 0)?;
    install_constructor(context, "Request", js_request_construct, 1)?;
    install_constructor(context, "AbortController", js_abort_construct, 0)?;
    context.register_global_callable(
        JsString::from("matchMedia"),
        1,
        NativeFunction::from_fn_ptr(js_match_media),
    )?;
    context.register_global_callable(
        JsString::from("getComputedStyle"),
        1,
        NativeFunction::from_fn_ptr(js_get_computed_style),
    )?;
    context.register_global_callable(
        JsString::from("structuredClone"),
        1,
        NativeFunction::from_fn_ptr(js_structured_clone),
    )?;
    context.register_global_callable(
        JsString::from("requestIdleCallback"),
        1,
        NativeFunction::from_fn_ptr(js_request_idle_callback),
    )?;
    context.register_global_callable(
        JsString::from("cancelIdleCallback"),
        1,
        NativeFunction::from_fn_ptr(js_cancel_idle_callback),
    )?;
    context.register_global_callable(
        JsString::from("atob"),
        1,
        NativeFunction::from_fn_ptr(js_atob),
    )?;
    context.register_global_callable(
        JsString::from("btoa"),
        1,
        NativeFunction::from_fn_ptr(js_btoa),
    )?;
    install_storage(context)?;
    install_history(context)?;
    install_performance(context)?;
    install_screen(context, options)?;
    Ok(())
}

fn install_constructor(
    context: &mut Context,
    name: &str,
    function: boa_engine::native_function::NativeFunctionPointer,
    length: usize,
) -> JsResult<()> {
    let realm = context.realm().clone();
    let constructor = FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(function))
        .name(JsString::from(name))
        .length(length)
        .constructor(true)
        .build();
    context.register_global_property(
        JsString::from(name),
        JsValue::from(constructor),
        Attribute::all(),
    )
}

// ---------------------------------------------------------------------------
// URL / URLSearchParams
// ---------------------------------------------------------------------------

fn parse_absolute(input: &str) -> Option<Url> {
    if let Ok(url) = Url::parse(input) {
        return Some(url);
    }
    let base = with_state(|state| state.base_url.clone())?;
    base.join(input).ok()
}

fn js_url_construct(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(input) = string_arg(args, 0, context) else {
        return Err(host_error("URL requires an input string"));
    };
    let parsed = if let Some(base) = string_arg(args, 1, context) {
        Url::parse(&base)
            .ok()
            .and_then(|base| base.join(&input).ok())
    } else {
        parse_absolute(&input)
    };
    let Some(url) = parsed else {
        return Err(host_error(format!("Invalid URL: {input}")));
    };
    let realm = context.realm().clone();
    let search_params = build_search_params(url.query().unwrap_or(""), context)?;
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("href"),
            js_string_value(url.as_str()),
            Attribute::all(),
        )
        .property(
            JsString::from("protocol"),
            js_string_value(&format!("{}:", url.scheme())),
            Attribute::all(),
        )
        .property(
            JsString::from("host"),
            js_string_value(&match url.port() {
                Some(port) => format!("{}:{}", url.host_str().unwrap_or(""), port),
                None => url.host_str().unwrap_or("").to_owned(),
            }),
            Attribute::all(),
        )
        .property(
            JsString::from("hostname"),
            js_string_value(url.host_str().unwrap_or("")),
            Attribute::all(),
        )
        .property(
            JsString::from("port"),
            js_string_value(&url.port().map(|port| port.to_string()).unwrap_or_default()),
            Attribute::all(),
        )
        .property(
            JsString::from("pathname"),
            js_string_value(url.path()),
            Attribute::all(),
        )
        .property(
            JsString::from("search"),
            js_string_value(
                &url.query()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default(),
            ),
            Attribute::all(),
        )
        .property(
            JsString::from("hash"),
            js_string_value(
                &url.fragment()
                    .map(|hash| format!("#{hash}"))
                    .unwrap_or_default(),
            ),
            Attribute::all(),
        )
        .property(
            JsString::from("origin"),
            js_string_value(&url.origin().ascii_serialization()),
            Attribute::all(),
        )
        .property(
            JsString::from("username"),
            js_string_value(url.username()),
            Attribute::all(),
        )
        .property(
            JsString::from("password"),
            js_string_value(url.password().unwrap_or("")),
            Attribute::all(),
        )
        .property(
            JsString::from("searchParams"),
            JsValue::from(search_params),
            Attribute::all(),
        );
    for (name, function) in [
        ("toString", NativeFunction::from_fn_ptr(js_url_to_string)),
        ("toJSON", NativeFunction::from_fn_ptr(js_url_to_string)),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(JsValue::from(object.build()))
}

fn js_url_to_string(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(js_string_value(""));
    };
    object
        .get(JsString::from("href"), context)
        .or_else(|_| Ok(js_string_value("")))
}

fn build_search_params(query: &str, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let entries = JsArray::new(context)?;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let entry = JsArray::new(context)?;
        entry.push(js_string_value(&percent_decode(name)), context)?;
        entry.push(js_string_value(&percent_decode(value)), context)?;
        entries.push(JsValue::from(entry), context)?;
    }
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from("__nexium_entries"),
        JsValue::from(entries),
        Attribute::WRITABLE,
    );
    for (name, function) in [
        ("append", NativeFunction::from_fn_ptr(js_params_append)),
        ("delete", NativeFunction::from_fn_ptr(js_params_delete)),
        ("get", NativeFunction::from_fn_ptr(js_params_get)),
        ("getAll", NativeFunction::from_fn_ptr(js_params_get_all)),
        ("has", NativeFunction::from_fn_ptr(js_params_has)),
        ("set", NativeFunction::from_fn_ptr(js_params_set)),
        ("sort", NativeFunction::from_fn_ptr(js_params_sort)),
        ("toString", NativeFunction::from_fn_ptr(js_params_to_string)),
        ("entries", NativeFunction::from_fn_ptr(js_params_entries)),
        ("keys", NativeFunction::from_fn_ptr(js_params_keys)),
        ("values", NativeFunction::from_fn_ptr(js_params_values)),
        ("forEach", NativeFunction::from_fn_ptr(js_params_for_each)),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(object.build())
}

fn js_search_params_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let query = args
        .first()
        .filter(|value| !value.is_null_or_undefined())
        .map(|value| {
            value_to_display(value, context)
                .trim_start_matches('?')
                .to_owned()
        })
        .unwrap_or_default();
    Ok(JsValue::from(build_search_params(&query, context)?))
}

fn params_entries(this: &JsValue, context: &mut Context) -> JsResult<Vec<(String, String)>> {
    let Some(object) = this.as_object() else {
        return Ok(Vec::new());
    };
    let entries = object.get(JsString::from("__nexium_entries"), context)?;
    let Some(entries) = entries.as_object() else {
        return Ok(Vec::new());
    };
    let array = JsArray::from_object(entries.clone())?;
    let length = array.length(context)?;
    let mut out = Vec::new();
    for index in 0..length {
        let entry = array.get(index, context)?;
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let entry = JsArray::from_object(entry.clone())?;
        let name = entry
            .get(0, context)
            .map(|value| value_to_display(&value, context))
            .unwrap_or_default();
        let value = entry
            .get(1, context)
            .map(|value| value_to_display(&value, context))
            .unwrap_or_default();
        out.push((name, value));
    }
    Ok(out)
}

fn set_params_entries(
    this: &JsValue,
    entries: &[(String, String)],
    context: &mut Context,
) -> JsResult<()> {
    let Some(object) = this.as_object() else {
        return Ok(());
    };
    let array = JsArray::new(context)?;
    for (name, value) in entries {
        let entry = JsArray::new(context)?;
        entry.push(js_string_value(name), context)?;
        entry.push(js_string_value(value), context)?;
        array.push(JsValue::from(entry), context)?;
    }
    object.set(
        JsString::from("__nexium_entries"),
        JsValue::from(array),
        false,
        context,
    )?;
    Ok(())
}

fn js_params_append(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let value = string_arg(args, 1, context).unwrap_or_default();
    let mut entries = params_entries(this, context)?;
    entries.push((name, value));
    set_params_entries(this, &entries, context)?;
    Ok(JsValue::undefined())
}

fn js_params_delete(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let mut entries = params_entries(this, context)?;
    entries.retain(|(entry, _)| entry != &name);
    set_params_entries(this, &entries, context)?;
    Ok(JsValue::undefined())
}

fn js_params_get(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let entries = params_entries(this, context)?;
    Ok(entries
        .iter()
        .find(|(entry, _)| entry == &name)
        .map(|(_, value)| js_string_value(value))
        .unwrap_or(JsValue::null()))
}

fn js_params_get_all(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let entries = params_entries(this, context)?;
    let array = JsArray::new(context)?;
    for (_, value) in entries.iter().filter(|(entry, _)| entry == &name) {
        array.push(js_string_value(value), context)?;
    }
    Ok(JsValue::from(array))
}

fn js_params_has(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let entries = params_entries(this, context)?;
    Ok(JsValue::from(
        entries.iter().any(|(entry, _)| entry == &name),
    ))
}

fn js_params_set(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let value = string_arg(args, 1, context).unwrap_or_default();
    let mut entries = params_entries(this, context)?;
    let mut inserted = false;
    entries.retain_mut(|(entry, entry_value)| {
        if entry != &name {
            return true;
        }
        if !inserted {
            *entry_value = value.clone();
            inserted = true;
            true
        } else {
            false
        }
    });
    if !inserted {
        entries.push((name, value));
    }
    set_params_entries(this, &entries, context)?;
    Ok(JsValue::undefined())
}

fn js_params_sort(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut entries = params_entries(this, context)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    set_params_entries(this, &entries, context)?;
    Ok(JsValue::undefined())
}

fn js_params_to_string(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let entries = params_entries(this, context)?;
    let encoded = entries
        .iter()
        .map(|(name, value)| format!("{}={}", percent_encode(name), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    Ok(js_string_value(&encoded))
}

fn js_params_entries(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let entries = params_entries(this, context)?;
    let array = JsArray::new(context)?;
    for (name, value) in entries {
        let pair = JsArray::new(context)?;
        pair.push(js_string_value(&name), context)?;
        pair.push(js_string_value(&value), context)?;
        array.push(JsValue::from(pair), context)?;
    }
    Ok(JsValue::from(array))
}

fn js_params_keys(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let entries = params_entries(this, context)?;
    let array = JsArray::new(context)?;
    for (name, _) in entries {
        array.push(js_string_value(&name), context)?;
    }
    Ok(JsValue::from(array))
}

fn js_params_values(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let entries = params_entries(this, context)?;
    let array = JsArray::new(context)?;
    for (_, value) in entries {
        array.push(js_string_value(&value), context)?;
    }
    Ok(JsValue::from(array))
}

fn js_params_for_each(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(callback) = args.first().and_then(JsValue::as_function) else {
        return Ok(JsValue::undefined());
    };
    let entries = params_entries(this, context)?;
    for (name, value) in entries {
        callback.call(
            &JsValue::undefined(),
            &[js_string_value(&value), js_string_value(&name)],
            context,
        )?;
    }
    Ok(JsValue::undefined())
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'+' {
            out.push(b' ');
            index += 1;
        } else if bytes[index] == b'%' && index + 2 < bytes.len() {
            let value = u8::from_str_radix(&input[index + 1..index + 3], 16);
            if let Ok(value) = value {
                out.push(value);
                index += 3;
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// TextEncoder / TextDecoder
// ---------------------------------------------------------------------------

fn js_text_encoder_construct(
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from("encoding"),
        js_string_value("utf-8"),
        Attribute::all(),
    );
    for (name, function) in [
        ("encode", NativeFunction::from_fn_ptr(js_text_encode)),
        (
            "encodeInto",
            NativeFunction::from_fn_ptr(js_text_encode_into),
        ),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(JsValue::from(object.build()))
}

fn js_text_encode(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this;
    let text = args
        .first()
        .map(|value| value_to_display(value, context))
        .unwrap_or_default();
    let array = JsUint8Array::from_iter(text.into_bytes(), context)?;
    Ok(JsValue::from(array))
}

fn js_text_encode_into(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let text = args
        .first()
        .map(|value| value_to_display(value, context))
        .unwrap_or_default();
    let bytes = text.len();
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("read"),
            JsValue::from(bytes as f64),
            Attribute::all(),
        )
        .property(
            JsString::from("written"),
            JsValue::from(bytes as f64),
            Attribute::all(),
        );
    Ok(JsValue::from(object.build()))
}

fn js_text_decoder_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    let label = string_arg(args, 0, context).unwrap_or_else(|| "utf-8".to_owned());
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("encoding"),
            js_string_value(&label.to_ascii_lowercase()),
            Attribute::all(),
        )
        .property(
            JsString::from("__nexium_label"),
            js_string_value(&label),
            Attribute::empty(),
        );
    object.property(
        JsString::from("decode"),
        NativeFunction::from_fn_ptr(js_text_decode).to_js_function(&realm),
        Attribute::all(),
    );
    Ok(JsValue::from(object.build()))
}

fn typed_array_bytes(value: &JsValue, context: &mut Context) -> Option<Vec<u8>> {
    let object = value.as_object()?;
    if let Ok(array) = JsUint8Array::from_object(object.clone())
        && let Ok(bytes) = array.to_vec(context)
    {
        return Some(bytes);
    }
    let length = object
        .get(JsString::from("length"), context)
        .ok()
        .and_then(|value| value.as_number())
        .unwrap_or(0.0) as usize;
    let mut bytes = Vec::with_capacity(length);
    for index in 0..length {
        let value = object.get(index, context).ok()?.as_number()? as u8;
        bytes.push(value);
    }
    Some(bytes)
}

fn js_text_decode(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let label = this
        .as_object()
        .and_then(|object| object.get(JsString::from("__nexium_label"), context).ok())
        .and_then(|value| value.as_string())
        .map(|value| value.to_std_string_escaped())
        .unwrap_or_else(|| "utf-8".to_owned());
    let bytes = args
        .first()
        .and_then(|value| typed_array_bytes(value, context))
        .unwrap_or_default();
    let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(&bytes);
    Ok(js_string_value(&text))
}

// ---------------------------------------------------------------------------
// Blob / File / FormData
// ---------------------------------------------------------------------------

fn blob_bytes(value: &JsValue, context: &mut Context) -> Vec<u8> {
    if let Some(bytes) = typed_array_bytes(value, context) {
        return bytes;
    }
    if let Some(object) = value.as_object()
        && let Ok(hidden) = object.get(JsString::from("__nexium_bytes"), context)
        && let Some(bytes) = typed_array_bytes(&hidden, context)
    {
        return bytes;
    }
    value_to_display(value, context).into_bytes()
}

fn build_blob(bytes: Vec<u8>, content_type: &str, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let byte_array = JsArray::new(context)?;
    for byte in &bytes {
        byte_array.push(JsValue::from(*byte as f64), context)?;
    }
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("__nexium_bytes"),
            JsValue::from(byte_array),
            Attribute::empty(),
        )
        .property(
            JsString::from("size"),
            JsValue::from(bytes.len() as f64),
            Attribute::all(),
        )
        .property(
            JsString::from("type"),
            js_string_value(content_type),
            Attribute::all(),
        );
    for (name, function) in [
        ("text", NativeFunction::from_fn_ptr(js_blob_text)),
        ("slice", NativeFunction::from_fn_ptr(js_blob_slice)),
        (
            "arrayBuffer",
            NativeFunction::from_fn_ptr(js_blob_array_buffer),
        ),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(object.build())
}

fn js_blob_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let mut bytes = Vec::new();
    if let Some(parts) = args.first().and_then(JsValue::as_object)
        && let Ok(array) = JsArray::from_object(parts.clone())
    {
        let length = array.length(context)?;
        for index in 0..length {
            let part = array.get(index, context)?;
            bytes.extend(blob_bytes(&part, context));
        }
    }
    let content_type = args
        .get(1)
        .and_then(JsValue::as_object)
        .and_then(|options| options.get(JsString::from("type"), context).ok())
        .map(|value| value_to_display(&value, context))
        .unwrap_or_default();
    Ok(JsValue::from(build_blob(bytes, &content_type, context)?))
}

fn js_file_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let mut bytes = Vec::new();
    if let Some(parts) = args.get(1).and_then(JsValue::as_object)
        && let Ok(array) = JsArray::from_object(parts.clone())
    {
        let length = array.length(context)?;
        for index in 0..length {
            let part = array.get(index, context)?;
            bytes.extend(blob_bytes(&part, context));
        }
    }
    let mut content_type = String::new();
    let mut last_modified = 0.0;
    if let Some(options) = args.get(2).and_then(JsValue::as_object) {
        if let Ok(value) = options.get(JsString::from("type"), context) {
            content_type = value_to_display(&value, context);
        }
        if let Ok(value) = options.get(JsString::from("lastModified"), context) {
            last_modified = value.as_number().unwrap_or(0.0);
        }
    }
    let file = build_blob(bytes, &content_type, context)?;
    file.set(
        JsString::from("name"),
        js_string_value(&name),
        false,
        context,
    )?;
    file.set(
        JsString::from("lastModified"),
        JsValue::from(last_modified),
        false,
        context,
    )?;
    Ok(JsValue::from(file))
}

fn js_blob_text(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let bytes = this
        .as_object()
        .and_then(|object| object.get(JsString::from("__nexium_bytes"), context).ok())
        .and_then(|value| typed_array_bytes(&value, context))
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    settled_promise_value(js_string_value(&text), context)
}

fn js_blob_array_buffer(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let bytes = this
        .as_object()
        .and_then(|object| object.get(JsString::from("__nexium_bytes"), context).ok())
        .and_then(|value| typed_array_bytes(&value, context))
        .unwrap_or_default();
    let block = AlignedVec::from_iter(0, bytes);
    let buffer = JsArrayBuffer::from_byte_block(block, context)?;
    settled_promise_value(JsValue::from(buffer), context)
}

fn js_blob_slice(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let bytes = this
        .as_object()
        .and_then(|object| object.get(JsString::from("__nexium_bytes"), context).ok())
        .and_then(|value| typed_array_bytes(&value, context))
        .unwrap_or_default();
    let length = bytes.len() as i64;
    let start = args
        .first()
        .and_then(JsValue::as_number)
        .map(|value| value as i64)
        .unwrap_or(0);
    let end = args
        .get(1)
        .and_then(JsValue::as_number)
        .map(|value| value as i64)
        .unwrap_or(length);
    let start = start.clamp(0, length) as usize;
    let end = end.clamp(0, length) as usize;
    let content_type = args
        .get(2)
        .and_then(|value| value.as_string())
        .map(|value| value.to_std_string_escaped())
        .unwrap_or_default();
    let slice = if start <= end {
        bytes[start..end].to_vec()
    } else {
        Vec::new()
    };
    Ok(JsValue::from(build_blob(slice, &content_type, context)?))
}

fn js_form_data_construct(
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    let entries = JsArray::new(context)?;
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from("__nexium_entries"),
        JsValue::from(entries),
        Attribute::WRITABLE,
    );
    for (name, function) in [
        ("append", NativeFunction::from_fn_ptr(js_form_append)),
        ("set", NativeFunction::from_fn_ptr(js_form_set)),
        ("get", NativeFunction::from_fn_ptr(js_form_get)),
        ("getAll", NativeFunction::from_fn_ptr(js_form_get_all)),
        ("has", NativeFunction::from_fn_ptr(js_form_has)),
        ("delete", NativeFunction::from_fn_ptr(js_form_delete)),
        ("entries", NativeFunction::from_fn_ptr(js_form_entries)),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(JsValue::from(object.build()))
}

fn form_entries(this: &JsValue, context: &mut Context) -> JsResult<JsArray> {
    let object = this
        .as_object()
        .ok_or_else(|| host_error("invalid FormData receiver"))?;
    let entries = object.get(JsString::from("__nexium_entries"), context)?;
    JsArray::from_object(
        entries
            .as_object()
            .ok_or_else(|| host_error("invalid FormData entries"))?,
    )
}

fn set_form_entries(this: &JsValue, entries: JsArray, context: &mut Context) -> JsResult<()> {
    if let Some(object) = this.as_object() {
        object.set(
            JsString::from("__nexium_entries"),
            JsValue::from(entries),
            false,
            context,
        )?;
    }
    Ok(())
}

fn form_pair(name: &str, value: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let pair = JsArray::new(context)?;
    pair.push(js_string_value(name), context)?;
    pair.push(value.clone(), context)?;
    Ok(JsValue::from(pair))
}

fn js_form_append(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let value = args.get(1).cloned().unwrap_or(JsValue::null());
    let entries = form_entries(this, context)?;
    entries.push(form_pair(&name, &value, context)?, context)?;
    set_form_entries(this, entries, context)?;
    Ok(JsValue::undefined())
}

fn js_form_set(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let value = args.get(1).cloned().unwrap_or(JsValue::null());
    let entries = form_entries(this, context)?;
    let length = entries.length(context)?;
    let replacement = JsArray::new(context)?;
    let mut inserted = false;
    for index in 0..length {
        let pair = entries.get(index, context)?;
        let entry_name = pair
            .as_object()
            .and_then(|object| object.get(0, context).ok())
            .map(|value| value_to_display(&value, context))
            .unwrap_or_default();
        if entry_name == name {
            if !inserted {
                replacement.push(form_pair(&name, &value, context)?, context)?;
                inserted = true;
            }
        } else {
            replacement.push(pair, context)?;
        }
    }
    if !inserted {
        replacement.push(form_pair(&name, &value, context)?, context)?;
    }
    set_form_entries(this, replacement, context)?;
    Ok(JsValue::undefined())
}

fn js_form_get(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let entries = form_entries(this, context)?;
    let length = entries.length(context)?;
    for index in 0..length {
        let pair = entries.get(index, context)?;
        if let Some(pair) = pair.as_object() {
            let entry_name = pair
                .get(0, context)
                .map(|value| value_to_display(&value, context))
                .unwrap_or_default();
            if entry_name == name {
                return pair.get(1, context);
            }
        }
    }
    Ok(JsValue::null())
}

fn js_form_get_all(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let entries = form_entries(this, context)?;
    let length = entries.length(context)?;
    let values = JsArray::new(context)?;
    for index in 0..length {
        let pair = entries.get(index, context)?;
        if let Some(pair) = pair.as_object() {
            let entry_name = pair
                .get(0, context)
                .map(|value| value_to_display(&value, context))
                .unwrap_or_default();
            if entry_name == name {
                values.push(pair.get(1, context)?, context)?;
            }
        }
    }
    Ok(JsValue::from(values))
}

fn js_form_has(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let value = js_form_get(this, args, context)?;
    Ok(JsValue::from(!value.is_null()))
}

fn js_form_delete(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let entries = form_entries(this, context)?;
    let length = entries.length(context)?;
    let replacement = JsArray::new(context)?;
    for index in 0..length {
        let pair = entries.get(index, context)?;
        let entry_name = pair
            .as_object()
            .and_then(|object| object.get(0, context).ok())
            .map(|value| value_to_display(&value, context))
            .unwrap_or_default();
        if entry_name != name {
            replacement.push(pair, context)?;
        }
    }
    set_form_entries(this, replacement, context)?;
    Ok(JsValue::undefined())
}

fn js_form_entries(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let entries = form_entries(this, context)?;
    Ok(JsValue::from(entries))
}

// ---------------------------------------------------------------------------
// Headers / Request / AbortController
// ---------------------------------------------------------------------------

fn js_headers_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    let map = ObjectInitializer::new(context).build();
    if let Some(init) = args.first().and_then(JsValue::as_object) {
        let source = init
            .get(JsString::from(HEADERS_KEY), context)
            .ok()
            .and_then(|value| value.as_object())
            .unwrap_or(init);
        if let Ok(keys) = source.own_property_keys(context) {
            for key in keys {
                if let boa_engine::property::PropertyKey::String(name) = key {
                    let name = name.to_std_string_escaped();
                    if let Ok(value) = source.get(JsString::from(name.clone()), context) {
                        let value = value_to_display(&value, context);
                        let _ = map.set(
                            JsString::from(name.to_ascii_lowercase()),
                            js_string_value(&value),
                            false,
                            context,
                        );
                    }
                }
            }
        }
    }
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from(HEADERS_KEY),
        JsValue::from(map),
        Attribute::empty(),
    );
    for (name, function) in [
        ("get", NativeFunction::from_fn_ptr(js_headers_class_get)),
        ("set", NativeFunction::from_fn_ptr(js_headers_class_set)),
        (
            "append",
            NativeFunction::from_fn_ptr(js_headers_class_append),
        ),
        ("has", NativeFunction::from_fn_ptr(js_headers_class_has)),
        (
            "delete",
            NativeFunction::from_fn_ptr(js_headers_class_delete),
        ),
        (
            "forEach",
            NativeFunction::from_fn_ptr(js_headers_class_for_each),
        ),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(JsValue::from(object.build()))
}

fn headers_map(this: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    this.as_object()
        .and_then(|object| object.get(JsString::from(HEADERS_KEY), context).ok())
        .and_then(|value| value.as_object())
        .ok_or_else(|| host_error("invalid Headers receiver"))
}

fn js_headers_class_get(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let map = headers_map(this, context)?;
    match map.get(JsString::from(name.to_ascii_lowercase()), context) {
        Ok(value) if !value.is_undefined() => Ok(value),
        _ => Ok(JsValue::null()),
    }
}

fn js_headers_class_set(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let value = string_arg(args, 1, context).unwrap_or_default();
    let map = headers_map(this, context)?;
    map.set(
        JsString::from(name.to_ascii_lowercase()),
        js_string_value(&value),
        false,
        context,
    )?;
    Ok(JsValue::undefined())
}

fn js_headers_class_append(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    js_headers_class_set(this, args, context)
}

fn js_headers_class_has(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let map = headers_map(this, context)?;
    Ok(JsValue::from(map.has_property(
        JsString::from(name.to_ascii_lowercase()),
        context,
    )?))
}

fn js_headers_class_delete(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let map = headers_map(this, context)?;
    let _ = map.delete_property_or_throw(JsString::from(name.to_ascii_lowercase()), context);
    Ok(JsValue::undefined())
}

fn js_headers_class_for_each(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(callback) = args.first().and_then(JsValue::as_function) else {
        return Ok(JsValue::undefined());
    };
    let map = headers_map(this, context)?;
    for key in map.own_property_keys(context)? {
        if let boa_engine::property::PropertyKey::String(name) = key {
            let value = map.get(name.clone(), context)?;
            callback.call(
                &JsValue::undefined(),
                &[value, JsValue::from(name)],
                context,
            )?;
        }
    }
    Ok(JsValue::undefined())
}

fn js_request_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = string_arg(args, 0, context).unwrap_or_default();
    let mut method = "GET".to_owned();
    let mut body = String::new();
    let mut headers_map: Option<JsValue> = None;
    let mut signal: Option<JsValue> = None;
    if let Some(init) = args.get(1).and_then(JsValue::as_object) {
        if let Ok(value) = init.get(JsString::from("method"), context) {
            method = value_to_display(&value, context);
        }
        if let Ok(value) = init.get(JsString::from("body"), context) {
            body = value_to_display(&value, context);
        }
        if let Ok(headers) = init.get(JsString::from("headers"), context) {
            let built = js_headers_construct(&JsValue::undefined(), &[headers], context)?;
            if let Some(built) = built.as_object()
                && let Ok(map) = built.get(JsString::from(HEADERS_KEY), context)
            {
                headers_map = Some(map);
            }
        }
        if let Ok(value) = init.get(JsString::from("signal"), context)
            && !value.is_null_or_undefined()
        {
            signal = Some(value);
        }
    }
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(REQUEST_KEY),
            JsValue::from(true),
            Attribute::empty(),
        )
        .property(
            JsString::from("__nexium_url"),
            js_string_value(&url),
            Attribute::empty(),
        )
        .property(
            JsString::from("__nexium_method"),
            js_string_value(&method),
            Attribute::empty(),
        )
        .property(
            JsString::from("__nexium_body"),
            js_string_value(&body),
            Attribute::empty(),
        );
    if let Some(map) = headers_map {
        object.property(JsString::from(HEADERS_KEY), map, Attribute::empty());
    }
    if let Some(signal) = signal {
        object.property(
            JsString::from("__nexium_signal"),
            signal,
            Attribute::empty(),
        );
    }
    Ok(JsValue::from(object.build()))
}

fn js_abort_construct(
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    let mut signal = ObjectInitializer::new(context);
    signal
        .property(
            JsString::from("aborted"),
            JsValue::from(false),
            Attribute::all(),
        )
        .property(
            JsString::from("reason"),
            JsValue::undefined(),
            Attribute::all(),
        );
    let signal = signal.build();
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("signal"),
            JsValue::from(signal),
            Attribute::all(),
        )
        .property(
            JsString::from("abort"),
            NativeFunction::from_fn_ptr(js_abort).to_js_function(&realm),
            Attribute::all(),
        );
    Ok(JsValue::from(object.build()))
}

fn js_abort(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(object) = this.as_object()
        && let Ok(signal) = object.get(JsString::from("signal"), context)
        && let Some(signal) = signal.as_object()
    {
        let _ = signal.set(
            JsString::from("aborted"),
            JsValue::from(true),
            false,
            context,
        );
        let _ = signal.set(
            JsString::from("reason"),
            js_string_value("aborted"),
            false,
            context,
        );
    }
    Ok(JsValue::undefined())
}

// ---------------------------------------------------------------------------
// Storage / History / Performance / Screen
// ---------------------------------------------------------------------------

fn storage_get_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let key = string_arg(args, 0, context).unwrap_or_default();
    let session = storage_is_session(this, context);
    let value = with_state(|state| {
        if session {
            state.session_storage.get(&key).cloned()
        } else {
            state.local_storage.get(&key).cloned()
        }
    })
    .flatten();
    Ok(value
        .map(|value| js_string_value(&value))
        .unwrap_or(JsValue::null()))
}

fn storage_set_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let key = string_arg(args, 0, context).unwrap_or_default();
    let value = string_arg(args, 1, context).unwrap_or_default();
    let session = storage_is_session(this, context);
    with_state(|state| {
        if session {
            state.session_storage.insert(key, value);
        } else {
            state.local_storage.insert(key, value);
        }
    });
    Ok(JsValue::undefined())
}

fn storage_remove_item(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = string_arg(args, 0, context).unwrap_or_default();
    let session = storage_is_session(this, context);
    with_state(|state| {
        if session {
            state.session_storage.remove(&key);
        } else {
            state.local_storage.remove(&key);
        }
    });
    Ok(JsValue::undefined())
}

fn storage_clear(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = storage_is_session(this, context);
    with_state(|state| {
        if session {
            state.session_storage.clear();
        } else {
            state.local_storage.clear();
        }
    });
    Ok(JsValue::undefined())
}

fn storage_key(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let index = args.first().and_then(JsValue::as_number).unwrap_or(0.0) as usize;
    let session = storage_is_session(this, context);
    let key = with_state(|state| {
        let mut keys: Vec<&String> = if session {
            state.session_storage.keys().collect()
        } else {
            state.local_storage.keys().collect()
        };
        keys.sort();
        keys.get(index).map(|key| (*key).clone())
    })
    .flatten();
    Ok(key
        .map(|key| js_string_value(&key))
        .unwrap_or(JsValue::null()))
}

fn storage_length(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = storage_is_session(this, context);
    let length = with_state(|state| {
        if session {
            state.session_storage.len()
        } else {
            state.local_storage.len()
        }
    })
    .unwrap_or(0);
    Ok(JsValue::from(length as f64))
}

fn storage_is_session(this: &JsValue, context: &mut Context) -> bool {
    this.as_object()
        .and_then(|object| object.get(JsString::from("__nexium_session"), context).ok())
        .and_then(|value| value.as_boolean())
        .unwrap_or(false)
}

fn build_storage(session: bool, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from("__nexium_session"),
        JsValue::from(session),
        Attribute::empty(),
    );
    for (name, function) in [
        ("getItem", NativeFunction::from_fn_ptr(storage_get_item)),
        ("setItem", NativeFunction::from_fn_ptr(storage_set_item)),
        (
            "removeItem",
            NativeFunction::from_fn_ptr(storage_remove_item),
        ),
        ("clear", NativeFunction::from_fn_ptr(storage_clear)),
        ("key", NativeFunction::from_fn_ptr(storage_key)),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let storage = {
        let length_getter = NativeFunction::from_fn_ptr(storage_length).to_js_function(&realm);
        object.accessor(
            JsString::from("length"),
            Some(length_getter),
            None,
            Attribute::all(),
        );
        object.build()
    };
    Ok(storage)
}

fn install_storage(context: &mut Context) -> JsResult<()> {
    let local = build_storage(false, context)?;
    let session = build_storage(true, context)?;
    context.register_global_property(JsString::from("localStorage"), local, Attribute::all())?;
    context.register_global_property(JsString::from("sessionStorage"), session, Attribute::all())
}

fn install_history(context: &mut Context) -> JsResult<()> {
    let realm = context.realm().clone();
    let history = {
        let mut object = ObjectInitializer::new(context);
        object
            .property(
                JsString::from("length"),
                JsValue::from(1.0),
                Attribute::all(),
            )
            .property(JsString::from("state"), JsValue::null(), Attribute::all());
        for (name, function) in [
            ("pushState", NativeFunction::from_fn_ptr(js_history_state)),
            (
                "replaceState",
                NativeFunction::from_fn_ptr(js_history_state),
            ),
            ("back", NativeFunction::from_fn_ptr(js_history_noop)),
            ("forward", NativeFunction::from_fn_ptr(js_history_noop)),
            ("go", NativeFunction::from_fn_ptr(js_history_noop)),
        ] {
            object.property(
                JsString::from(name),
                function.to_js_function(&realm),
                Attribute::all(),
            );
        }
        object.build()
    };
    context.register_global_property(
        JsString::from("history"),
        JsValue::from(history),
        Attribute::all(),
    )
}

fn js_history_state(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let state = args.first().cloned().unwrap_or(JsValue::null());
    if let Some(object) = this.as_object() {
        object.set(JsString::from("state"), state, false, context)?;
    }
    Ok(JsValue::undefined())
}

fn js_history_noop(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn install_performance(context: &mut Context) -> JsResult<()> {
    let realm = context.realm().clone();
    let performance = {
        let mut object = ObjectInitializer::new(context);
        object
            .property(
                JsString::from("now"),
                NativeFunction::from_fn_ptr(js_performance_now).to_js_function(&realm),
                Attribute::all(),
            )
            .property(
                JsString::from("timeOrigin"),
                JsValue::from(0.0),
                Attribute::all(),
            );
        object.build()
    };
    context.register_global_property(
        JsString::from("performance"),
        JsValue::from(performance),
        Attribute::all(),
    )
}

fn js_performance_now(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let elapsed =
        with_state(|state| state.start_time.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0);
    Ok(JsValue::from(elapsed))
}

fn install_screen(context: &mut Context, options: &ScriptOptions) -> JsResult<()> {
    let screen = {
        let mut screen = ObjectInitializer::new(context);
        for (name, value) in [
            ("width", options.viewport_width),
            ("height", options.viewport_height),
            ("availWidth", options.viewport_width),
            ("availHeight", options.viewport_height),
            ("colorDepth", 24.0),
            ("pixelDepth", 24.0),
        ] {
            screen.property(JsString::from(name), JsValue::from(value), Attribute::all());
        }
        screen.build()
    };
    context.register_global_property(
        JsString::from("screen"),
        JsValue::from(screen),
        Attribute::all(),
    )?;
    for (name, value) in [
        ("innerWidth", options.viewport_width),
        ("innerHeight", options.viewport_height),
        ("outerWidth", options.viewport_width),
        ("outerHeight", options.viewport_height),
        ("devicePixelRatio", 1.0),
        ("scrollX", 0.0),
        ("scrollY", 0.0),
        ("pageXOffset", 0.0),
        ("pageYOffset", 0.0),
    ] {
        context.register_global_property(
            JsString::from(name),
            JsValue::from(value),
            Attribute::all(),
        )?;
    }
    context.register_global_property(
        JsString::from("__nexium_start"),
        JsValue::from(Instant::now().elapsed().as_secs_f64()),
        Attribute::empty(),
    )
}

// ---------------------------------------------------------------------------
// matchMedia / computed style / structuredClone
// ---------------------------------------------------------------------------

fn js_match_media(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let query = string_arg(args, 0, context).unwrap_or_default();
    let matches = media_query_matches(&query, context);
    let realm = context.realm().clone();
    let mut list = ObjectInitializer::new(context);
    list.property(
        JsString::from("media"),
        js_string_value(&query),
        Attribute::all(),
    )
    .property(
        JsString::from("matches"),
        JsValue::from(matches),
        Attribute::all(),
    )
    .property(
        JsString::from("onchange"),
        JsValue::null(),
        Attribute::all(),
    );
    for name in [
        "addEventListener",
        "removeEventListener",
        "addListener",
        "removeListener",
    ] {
        list.property(
            JsString::from(name),
            NativeFunction::from_fn_ptr(js_noop).to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(JsValue::from(list.build()))
}

fn media_query_matches(query: &str, context: &mut Context) -> bool {
    let width = context
        .global_object()
        .get(JsString::from("innerWidth"), context)
        .ok()
        .and_then(|value| value.as_number())
        .unwrap_or(1280.0);
    let height = context
        .global_object()
        .get(JsString::from("innerHeight"), context)
        .ok()
        .and_then(|value| value.as_number())
        .unwrap_or(720.0);
    let query = query.to_ascii_lowercase();
    if query.trim().is_empty() || query.trim() == "all" {
        return true;
    }
    let mut matched_any = false;
    for part in query.split(',') {
        let mut ok = true;
        for feature in part.split('(').skip(1) {
            let feature = feature.split(')').next().unwrap_or("").trim();
            let (name, value) = feature
                .split_once(':')
                .map(|(name, value)| (name.trim(), value.trim()))
                .unwrap_or((feature, ""));
            let number = || {
                value
                    .trim_end_matches("px")
                    .trim_end_matches("em")
                    .parse::<f64>()
                    .ok()
            };
            let value = match name {
                "min-width" => number().is_some_and(|min| width >= min),
                "max-width" => number().is_some_and(|max| width <= max),
                "min-height" => number().is_some_and(|min| height >= min),
                "max-height" => number().is_some_and(|max| height <= max),
                "width" => number().is_some_and(|exact| (width - exact).abs() < 0.5),
                "height" => number().is_some_and(|exact| (height - exact).abs() < 0.5),
                "orientation" => {
                    (value == "portrait" && height > width)
                        || (value == "landscape" && width >= height)
                }
                "prefers-color-scheme" => value == "light",
                "hover" => value != "none",
                "pointer" => value != "none",
                _ => false,
            };
            if !value {
                ok = false;
                break;
            }
        }
        if ok {
            matched_any = true;
            break;
        }
    }
    matched_any
}

fn js_get_computed_style(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(element) = args.first() else {
        return Err(host_error("getComputedStyle requires an element"));
    };
    super::dom::get_style(element, &[], context)
}

fn js_structured_clone(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(value) = args.first() else {
        return Ok(JsValue::undefined());
    };
    let json = context
        .global_object()
        .get(JsString::from("JSON"), context)?;
    let Some(json) = json.as_object() else {
        return Ok(value.clone());
    };
    let stringify = json.get(JsString::from("stringify"), context)?;
    let Some(stringify) = stringify.as_function() else {
        return Ok(value.clone());
    };
    let text = stringify.call(
        &JsValue::from(json.clone()),
        std::slice::from_ref(value),
        context,
    )?;
    let parse = json.get(JsString::from("parse"), context)?;
    let Some(parse) = parse.as_function() else {
        return Ok(value.clone());
    };
    parse.call(&JsValue::from(json), &[text], context)
}

fn js_request_idle_callback(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let timeout = context
        .global_object()
        .get(JsString::from("setTimeout"), context)?;
    let Some(timeout) = timeout.as_function() else {
        return Ok(JsValue::from(0.0));
    };
    timeout.call(
        &JsValue::undefined(),
        &[
            args.first().cloned().unwrap_or(JsValue::undefined()),
            JsValue::from(1.0),
        ],
        context,
    )
}

fn js_cancel_idle_callback(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let clear = context
        .global_object()
        .get(JsString::from("clearTimeout"), context)?;
    if let Some(clear) = clear.as_function() {
        clear.call(&JsValue::undefined(), args, context)?;
    }
    Ok(JsValue::undefined())
}

fn js_noop(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

// ---------------------------------------------------------------------------
// atob / btoa
// ---------------------------------------------------------------------------

fn js_atob(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = string_arg(args, 0, context).unwrap_or_default();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input.trim())
        .map_err(|_| host_error("InvalidCharacterError: invalid base64"))?;
    let text: String = bytes.iter().map(|byte| *byte as char).collect();
    Ok(js_string_value(&text))
}

fn js_btoa(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = string_arg(args, 0, context).unwrap_or_default();
    let bytes: Vec<u8> = input
        .chars()
        .map(|character| u8::try_from(character as u32).unwrap_or(b'?'))
        .collect();
    Ok(js_string_value(
        &base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}
