//! DOM bindings for the JavaScript runtime.

use std::collections::HashMap;

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::{JsArray, JsProxy};
use boa_engine::property::{Attribute, PropertyKey};
use boa_engine::symbol::JsSymbol;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction, Source};

use super::events;
use super::observers;
use super::{
    document_rc, host_error, js_string_value, node_id_from_object, node_id_from_this, string_arg,
    value_to_display, with_state,
};
use crate::engine::css::parser::{ComponentValue, format_number, parse_declaration_list};
use crate::engine::dom::{Attribute as DomAttribute, Document, Namespace, NodeData, NodeId};
use crate::engine::html::parse_html;

pub(crate) fn document_node() -> NodeId {
    document_rc()
        .map(|document| document.borrow().document_node())
        .unwrap_or_else(|| NodeId::from_index(0))
}

pub(crate) fn wrap_node(node: NodeId, context: &mut Context) -> JsResult<JsValue> {
    let cache = with_state(|state| state.node_cache.clone())
        .flatten()
        .ok_or_else(|| host_error("DOM runtime is not active"))?;
    let key = JsString::from(node.index().to_string());
    if cache.has_own_property(key.clone(), context)? {
        return cache.get(key, context);
    }
    let object = build_node_object(node, context)?;
    cache.set(key, JsValue::from(object.clone()), false, context)?;
    Ok(JsValue::from(object))
}

fn wrap_or_null(node: Option<NodeId>, context: &mut Context) -> JsResult<JsValue> {
    match node {
        Some(node) => wrap_node(node, context),
        None => Ok(JsValue::null()),
    }
}

macro_rules! on_accessor {
    ($($getter:ident, $setter:ident, $event:literal);* $(;)?) => {
        $(
            fn $getter(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
                events::get_property_listener(this, $event, context)
            }
            fn $setter(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
                events::set_property_listener(this, $event, args, context)
            }
        )*
    };
}

on_accessor!(
    get_on_click, set_on_click, "click";
    get_on_load, set_on_load, "load";
    get_on_error, set_on_error, "error";
    get_on_input, set_on_input, "input";
    get_on_change, set_on_change, "change";
    get_on_submit, set_on_submit, "submit";
    get_on_keydown, set_on_keydown, "keydown";
    get_on_keyup, set_on_keyup, "keyup";
    get_on_keypress, set_on_keypress, "keypress";
    get_on_mousedown, set_on_mousedown, "mousedown";
    get_on_mouseup, set_on_mouseup, "mouseup";
    get_on_mousemove, set_on_mousemove, "mousemove";
    get_on_mouseover, set_on_mouseover, "mouseover";
    get_on_mouseout, set_on_mouseout, "mouseout";
    get_on_focus, set_on_focus, "focus";
    get_on_blur, set_on_blur, "blur";
    get_on_scroll, set_on_scroll, "scroll";
    get_on_readystatechange, set_on_readystatechange, "readystatechange";
);

fn build_node_object(node: NodeId, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let accessor =
        |get: boa_engine::native_function::NativeFunctionPointer,
         set: Option<boa_engine::native_function::NativeFunctionPointer>| {
            (
                NativeFunction::from_fn_ptr(get).to_js_function(&realm),
                set.map(|set| NativeFunction::from_fn_ptr(set).to_js_function(&realm)),
            )
        };

    let text_content = accessor(get_text_content, Some(set_text_content));
    let inner_text = accessor(get_inner_text, Some(set_inner_text));
    let scripts = accessor(get_scripts, None);
    let inner_html = accessor(get_inner_html, Some(set_inner_html));
    let outer_html = accessor(get_outer_html, Some(set_outer_html));
    let id = accessor(get_id, Some(set_id));
    let class_name = accessor(get_class_name, Some(set_class_name));
    let tag_name = accessor(get_tag_name, None);
    let node_type = accessor(get_node_type, None);
    let parent_node = accessor(get_parent_node, None);
    let parent_element = accessor(get_parent_element, None);
    let dataset = accessor(get_dataset, None);
    let first_child = accessor(get_first_child, None);
    let last_child = accessor(get_last_child, None);
    let next_sibling = accessor(get_next_sibling, None);
    let previous_sibling = accessor(get_previous_sibling, None);
    let children = accessor(get_children, None);
    let child_nodes = accessor(get_child_nodes, None);
    let style = accessor(get_style, None);
    let class_list = accessor(get_class_list, None);
    let value = accessor(get_value, Some(set_value));
    let href = accessor(get_href, Some(set_href));
    let src = accessor(get_src, Some(set_src));
    let checked = accessor(get_checked, Some(set_checked));
    let disabled = accessor(get_disabled, Some(set_disabled));
    let title = accessor(get_title, Some(set_title));
    let body = accessor(get_body, None);
    let document_element = accessor(get_document_element, None);
    let ready_state = accessor(get_ready_state, None);
    let url = accessor(get_url, None);
    let cookie = accessor(get_cookie, Some(set_cookie));

    let mut initializer = ObjectInitializer::new(context);
    initializer
        .property(
            JsString::from(super::NODE_ID_KEY),
            JsValue::from(node.index() as f64),
            Attribute::empty(),
        )
        .accessor(
            JsString::from("textContent"),
            Some(text_content.0),
            text_content.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("innerText"),
            Some(inner_text.0),
            inner_text.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("scripts"),
            Some(scripts.0),
            scripts.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("innerHTML"),
            Some(inner_html.0),
            inner_html.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("outerHTML"),
            Some(outer_html.0),
            outer_html.1,
            Attribute::all(),
        )
        .accessor(JsString::from("id"), Some(id.0), id.1, Attribute::all())
        .accessor(
            JsString::from("className"),
            Some(class_name.0),
            class_name.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("tagName"),
            Some(tag_name.0.clone()),
            tag_name.1.clone(),
            Attribute::all(),
        )
        .accessor(
            JsString::from("nodeName"),
            Some(tag_name.0),
            tag_name.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("nodeType"),
            Some(node_type.0),
            node_type.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("parentNode"),
            Some(parent_node.0),
            parent_node.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("parentElement"),
            Some(parent_element.0),
            parent_element.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("firstChild"),
            Some(first_child.0),
            first_child.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("lastChild"),
            Some(last_child.0),
            last_child.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("nextSibling"),
            Some(next_sibling.0),
            next_sibling.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("previousSibling"),
            Some(previous_sibling.0),
            previous_sibling.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("children"),
            Some(children.0),
            children.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("childNodes"),
            Some(child_nodes.0),
            child_nodes.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("style"),
            Some(style.0),
            style.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("classList"),
            Some(class_list.0),
            class_list.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("dataset"),
            Some(dataset.0),
            dataset.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("value"),
            Some(value.0),
            value.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("href"),
            Some(href.0),
            href.1,
            Attribute::all(),
        )
        .accessor(JsString::from("src"), Some(src.0), src.1, Attribute::all())
        .accessor(
            JsString::from("checked"),
            Some(checked.0),
            checked.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("disabled"),
            Some(disabled.0),
            disabled.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("title"),
            Some(title.0),
            title.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("body"),
            Some(body.0),
            body.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("documentElement"),
            Some(document_element.0),
            document_element.1,
            Attribute::all(),
        )
        .accessor(
            JsString::from("readyState"),
            Some(ready_state.0),
            ready_state.1,
            Attribute::all(),
        )
        .accessor(JsString::from("URL"), Some(url.0), url.1, Attribute::all())
        .accessor(
            JsString::from("cookie"),
            Some(cookie.0),
            cookie.1,
            Attribute::all(),
        );

    let methods: [(&str, NativeFunction, usize); 36] = [
        (
            "getAttribute",
            NativeFunction::from_fn_ptr(js_get_attribute),
            1,
        ),
        (
            "setAttribute",
            NativeFunction::from_fn_ptr(js_set_attribute),
            2,
        ),
        (
            "hasAttribute",
            NativeFunction::from_fn_ptr(js_has_attribute),
            1,
        ),
        (
            "removeAttribute",
            NativeFunction::from_fn_ptr(js_remove_attribute),
            1,
        ),
        (
            "appendChild",
            NativeFunction::from_fn_ptr(js_append_child),
            1,
        ),
        (
            "insertBefore",
            NativeFunction::from_fn_ptr(js_insert_before),
            2,
        ),
        (
            "insertAdjacentHTML",
            NativeFunction::from_fn_ptr(js_insert_adjacent_html),
            2,
        ),
        (
            "insertAdjacentText",
            NativeFunction::from_fn_ptr(js_insert_adjacent_text),
            2,
        ),
        ("prepend", NativeFunction::from_fn_ptr(js_prepend), 0),
        ("append", NativeFunction::from_fn_ptr(js_append), 0),
        (
            "removeChild",
            NativeFunction::from_fn_ptr(js_remove_child),
            1,
        ),
        (
            "replaceChild",
            NativeFunction::from_fn_ptr(js_replace_child),
            2,
        ),
        ("cloneNode", NativeFunction::from_fn_ptr(js_clone_node), 1),
        ("matches", NativeFunction::from_fn_ptr(js_matches), 1),
        ("closest", NativeFunction::from_fn_ptr(js_closest), 1),
        (
            "querySelector",
            NativeFunction::from_fn_ptr(js_query_selector),
            1,
        ),
        (
            "querySelectorAll",
            NativeFunction::from_fn_ptr(js_query_selector_all),
            1,
        ),
        (
            "getElementsByTagName",
            NativeFunction::from_fn_ptr(js_get_elements_by_tag_name),
            1,
        ),
        (
            "getElementsByClassName",
            NativeFunction::from_fn_ptr(js_get_elements_by_class_name),
            1,
        ),
        (
            "getElementById",
            NativeFunction::from_fn_ptr(js_get_element_by_id),
            1,
        ),
        (
            "getBoundingClientRect",
            NativeFunction::from_fn_ptr(js_get_bounding_client_rect),
            0,
        ),
        (
            "getClientRects",
            NativeFunction::from_fn_ptr(js_get_client_rects),
            0,
        ),
        (
            "createElement",
            NativeFunction::from_fn_ptr(js_create_element),
            1,
        ),
        (
            "createElementNS",
            NativeFunction::from_fn_ptr(js_create_element_ns),
            2,
        ),
        (
            "createTextNode",
            NativeFunction::from_fn_ptr(js_create_text_node),
            1,
        ),
        (
            "createComment",
            NativeFunction::from_fn_ptr(js_create_comment),
            1,
        ),
        (
            "createDocumentFragment",
            NativeFunction::from_fn_ptr(js_create_document_fragment),
            0,
        ),
        (
            "addEventListener",
            NativeFunction::from_fn_ptr(events::js_add_listener),
            3,
        ),
        (
            "removeEventListener",
            NativeFunction::from_fn_ptr(events::js_remove_listener),
            3,
        ),
        (
            "dispatchEvent",
            NativeFunction::from_fn_ptr(events::js_dispatch_event),
            1,
        ),
        ("click", NativeFunction::from_fn_ptr(events::js_click), 0),
        (
            "getContext",
            NativeFunction::from_fn_ptr(super::canvas::js_get_context),
            1,
        ),
        (
            "toDataURL",
            NativeFunction::from_fn_ptr(super::canvas::js_element_to_data_url),
            1,
        ),
        ("remove", NativeFunction::from_fn_ptr(js_remove_self), 0),
        ("write", NativeFunction::from_fn_ptr(js_document_write), 1),
        (
            "writeln",
            NativeFunction::from_fn_ptr(js_document_writeln),
            1,
        ),
    ];
    for (name, function, length) in methods {
        let _ = length;
        initializer.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let on_handlers: [(
        &str,
        boa_engine::native_function::NativeFunctionPointer,
        boa_engine::native_function::NativeFunctionPointer,
    ); 18] = [
        ("onclick", get_on_click, set_on_click),
        ("onload", get_on_load, set_on_load),
        ("onerror", get_on_error, set_on_error),
        ("oninput", get_on_input, set_on_input),
        ("onchange", get_on_change, set_on_change),
        ("onsubmit", get_on_submit, set_on_submit),
        ("onkeydown", get_on_keydown, set_on_keydown),
        ("onkeyup", get_on_keyup, set_on_keyup),
        ("onkeypress", get_on_keypress, set_on_keypress),
        ("onmousedown", get_on_mousedown, set_on_mousedown),
        ("onmouseup", get_on_mouseup, set_on_mouseup),
        ("onmousemove", get_on_mousemove, set_on_mousemove),
        ("onmouseover", get_on_mouseover, set_on_mouseover),
        ("onmouseout", get_on_mouseout, set_on_mouseout),
        ("onfocus", get_on_focus, set_on_focus),
        ("onblur", get_on_blur, set_on_blur),
        ("onscroll", get_on_scroll, set_on_scroll),
        (
            "onreadystatechange",
            get_on_readystatechange,
            set_on_readystatechange,
        ),
    ];
    for (name, get, set) in on_handlers {
        initializer.accessor(
            JsString::from(name),
            Some(NativeFunction::from_fn_ptr(get).to_js_function(&realm)),
            Some(NativeFunction::from_fn_ptr(set).to_js_function(&realm)),
            Attribute::all(),
        );
    }
    if document()?.borrow().tag_name(node) == Some("canvas") {
        for (name, get, set) in [
            (
                "width",
                get_canvas_width as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
                set_canvas_width as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
            ),
            ("height", get_canvas_height, set_canvas_height),
        ] {
            initializer.accessor(
                JsString::from(name),
                Some(NativeFunction::from_fn_ptr(get).to_js_function(&realm)),
                Some(NativeFunction::from_fn_ptr(set).to_js_function(&realm)),
                Attribute::all(),
            );
        }
    }
    Ok(initializer.build())
}

fn node_of(this: &JsValue, context: &mut Context) -> JsResult<NodeId> {
    node_id_from_this(this, context).ok_or_else(|| host_error("invalid DOM node receiver"))
}

fn document() -> JsResult<std::rc::Rc<std::cell::RefCell<Document>>> {
    document_rc().ok_or_else(|| host_error("DOM runtime is not active"))
}

fn get_text_content(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let text = document()?.borrow().text_content(node);
    Ok(js_string_value(&text))
}

fn get_inner_text(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    get_text_content(this, _args, context)
}

fn set_inner_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_text_content(this, args, context)
}

fn get_scripts(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let root = node_of(this, context)?;
    let ids: Vec<NodeId> = {
        let document = document()?;
        let document = document.borrow();
        document
            .descendants(root)
            .into_iter()
            .filter(|&id| document.tag_name(id) == Some("script"))
            .collect()
    };
    let array = JsArray::new(context)?;
    for id in ids {
        array.push(wrap_node(id, context)?, context)?;
    }
    Ok(JsValue::from(array))
}

fn set_text_content(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let text = string_arg(args, 0, context).unwrap_or_default();
    let document = document()?;
    let removed = document.borrow().children(node).to_vec();
    document
        .borrow_mut()
        .set_text_content(node, &text)
        .map_err(|error| host_error(error.to_string()))?;
    let added = document.borrow().children(node).to_vec();
    observers::record_child_list(node, &added, &removed);
    Ok(JsValue::undefined())
}

fn get_inner_html(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let html = document()?.borrow().inner_html(node);
    Ok(js_string_value(&html))
}

fn set_inner_html(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let html = string_arg(args, 0, context).unwrap_or_default();
    let document = document()?;
    let parsed = parse_html(&html);
    let body = parsed
        .descendants(parsed.document_node())
        .into_iter()
        .find(|&id| parsed.tag_name(id) == Some("body"));
    let parent = document
        .borrow()
        .element(node)
        .and_then(|element| element.template_contents)
        .unwrap_or(node);
    let removed = document.borrow().children(parent).to_vec();
    clear_children(&mut document.borrow_mut(), parent);
    if let Some(body) = body {
        let children = parsed.children(body).to_vec();
        for child in children {
            let imported = {
                let mut target = document.borrow_mut();
                import_subtree(&mut target, &parsed, child)
            };
            if let Some(imported) = imported {
                let _ = document.borrow_mut().append_child(parent, imported);
            }
        }
    }
    let added = document.borrow().children(parent).to_vec();
    observers::record_child_list(parent, &added, &removed);
    Ok(JsValue::undefined())
}

/// Resolve the insertion point for `insertAdjacent*` methods.
///
/// Returns `None` when `node` has no parent (the call is a no-op), otherwise
/// the parent and the reference child to insert before.
fn adjacent_insertion_point(
    document: &Document,
    node: NodeId,
    position: &str,
    method: &str,
) -> JsResult<Option<(NodeId, Option<NodeId>)>> {
    let content = |node: NodeId| {
        document
            .element(node)
            .and_then(|element| element.template_contents)
            .unwrap_or(node)
    };
    match position {
        "beforebegin" => {
            let Some(parent) = document.parent(node) else {
                return Ok(None);
            };
            Ok(Some((parent, Some(node))))
        }
        "afterbegin" => {
            let target = content(node);
            let first = document.children(target).first().copied();
            Ok(Some((target, first)))
        }
        "beforeend" => Ok(Some((content(node), None))),
        "afterend" => {
            let Some(parent) = document.parent(node) else {
                return Ok(None);
            };
            let children = document.children(parent);
            let reference = children
                .iter()
                .position(|&child| child == node)
                .and_then(|index| children.get(index + 1).copied());
            Ok(Some((parent, reference)))
        }
        _ => Err(host_error(format!("invalid {method} position: {position}"))),
    }
}

fn js_insert_adjacent_html(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let position = string_arg(args, 0, context)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let html = string_arg(args, 1, context).unwrap_or_default();
    let document = document()?;
    let parsed = parse_html(&html);
    let body = parsed
        .descendants(parsed.document_node())
        .into_iter()
        .find(|&id| parsed.tag_name(id) == Some("body"));
    let point = {
        let document_ref = document.borrow();
        adjacent_insertion_point(&document_ref, node, &position, "insertAdjacentHTML")?
    };
    let Some((parent, reference)) = point else {
        return Ok(JsValue::undefined());
    };

    let mut added = Vec::new();
    if let Some(body) = body {
        for child in parsed.children(body).to_vec() {
            let imported = {
                let mut target = document.borrow_mut();
                import_subtree(&mut target, &parsed, child)
            };
            if let Some(imported) = imported {
                let _ = document
                    .borrow_mut()
                    .insert_before(parent, imported, reference);
                added.push(imported);
            }
        }
    }
    observers::record_child_list(parent, &added, &[]);
    Ok(JsValue::undefined())
}

fn js_insert_adjacent_text(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let position = string_arg(args, 0, context)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let text = string_arg(args, 1, context).unwrap_or_default();
    let document = document()?;
    let point = {
        let document_ref = document.borrow();
        adjacent_insertion_point(&document_ref, node, &position, "insertAdjacentText")?
    };
    let Some((parent, reference)) = point else {
        return Ok(JsValue::undefined());
    };
    let text_id = document.borrow_mut().create_text(text);
    document
        .borrow_mut()
        .insert_before(parent, text_id, reference)
        .map_err(|error| host_error(error.to_string()))?;
    observers::record_child_list(parent, &[text_id], &[]);
    Ok(JsValue::undefined())
}

fn set_outer_html(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let html = string_arg(args, 0, context).unwrap_or_default();
    let document = document()?;
    let (parent, is_element) = {
        let document_ref = document.borrow();
        (document_ref.parent(node), document_ref.is_element(node))
    };
    // Documents and detached nodes have no place to insert the replacement.
    let Some(parent) = parent.filter(|_| is_element) else {
        return Ok(JsValue::undefined());
    };
    let parsed = parse_html(&html);
    let body = find_element(&parsed, "body");
    let mut added = Vec::new();
    if let Some(body) = body {
        for child in parsed.children(body).to_vec() {
            let imported = {
                let mut target = document.borrow_mut();
                import_subtree(&mut target, &parsed, child)
            };
            if let Some(imported) = imported {
                let _ = document
                    .borrow_mut()
                    .insert_before(parent, imported, Some(node));
                added.push(imported);
            }
        }
    }
    let _ = document.borrow_mut().remove_child(parent, node);
    observers::record_child_list(parent, &added, &[node]);
    for id in added {
        maybe_execute_script(id, context);
    }
    Ok(JsValue::undefined())
}

fn clear_children(document: &mut Document, parent: NodeId) {
    let children = document.children(parent).to_vec();
    for child in children {
        let _ = document.remove_child(parent, child);
    }
}

fn import_subtree(target: &mut Document, source: &Document, node: NodeId) -> Option<NodeId> {
    match source.node(node).data.clone() {
        NodeData::Text(text) => Some(target.create_text(text)),
        NodeData::Comment(text) => Some(target.create_comment(text)),
        NodeData::DocumentFragment => {
            let fragment = target.create_document_fragment();
            for child in source.children(node) {
                if let Some(imported) = import_subtree(target, source, *child) {
                    let _ = target.append_child(fragment, imported);
                }
            }
            Some(fragment)
        }
        NodeData::Element(element) => {
            let attributes: Vec<DomAttribute> = element.attributes.clone();
            let id = target.create_element_ns(&element.name, element.namespace, attributes);
            if let Some(template_content) = element.template_contents {
                let fragment = target.create_document_fragment();
                for child in source.children(template_content) {
                    if let Some(imported) = import_subtree(target, source, *child) {
                        let _ = target.append_child(fragment, imported);
                    }
                }
                if let Some(element) = target.element_mut(id) {
                    element.template_contents = Some(fragment);
                }
            }
            for child in source.children(node) {
                if let Some(imported) = import_subtree(target, source, *child) {
                    let _ = target.append_child(id, imported);
                }
            }
            Some(id)
        }
        NodeData::Document | NodeData::Doctype(_) => None,
    }
}

fn get_outer_html(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let html = document()?.borrow().outer_html(node);
    Ok(js_string_value(&html))
}

fn get_id(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = document()?
        .borrow()
        .get_attribute(node, "id")
        .unwrap_or("")
        .to_owned();
    Ok(js_string_value(&value))
}

fn set_id(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = string_arg(args, 0, context).unwrap_or_default();
    document()?
        .borrow_mut()
        .set_attribute(node, "id", &value)
        .map_err(|error| host_error(error.to_string()))?;
    Ok(JsValue::undefined())
}

fn get_class_name(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = document()?
        .borrow()
        .get_attribute(node, "class")
        .unwrap_or("")
        .to_owned();
    Ok(js_string_value(&value))
}

fn set_class_name(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = string_arg(args, 0, context).unwrap_or_default();
    document()?
        .borrow_mut()
        .set_attribute(node, "class", &value)
        .map_err(|error| host_error(error.to_string()))?;
    Ok(JsValue::undefined())
}

fn get_tag_name(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let document = document()?;
    let document = document.borrow();
    let Some(element) = document.element(node) else {
        return Ok(JsValue::null());
    };
    let name = if element.namespace == Namespace::Html {
        element.name.to_ascii_uppercase()
    } else {
        element.name.clone()
    };
    Ok(js_string_value(&name))
}

fn get_node_type(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let document = document()?;
    let document = document.borrow();
    let node_type = match &document.node(node).data {
        NodeData::Element(_) => 1,
        NodeData::Text(_) => 3,
        NodeData::Comment(_) => 8,
        NodeData::Document => 9,
        NodeData::Doctype(_) => 10,
        NodeData::DocumentFragment => 11,
    };
    Ok(JsValue::from(node_type))
}

fn get_parent_node(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let parent = document()?.borrow().parent(node);
    wrap_or_null(parent, context)
}

fn get_parent_element(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let parent = {
        let document = document()?;
        let document = document.borrow();
        document
            .parent(node)
            .filter(|&parent| document.is_element(parent))
    };
    wrap_or_null(parent, context)
}

fn get_first_child(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let child = document()?.borrow().children(node).first().copied();
    wrap_or_null(child, context)
}

fn get_last_child(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let child = document()?.borrow().children(node).last().copied();
    wrap_or_null(child, context)
}

fn sibling(this: &JsValue, context: &mut Context, next: bool) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let document = document()?;
    let parent = document.borrow().parent(node);
    let Some(parent) = parent else {
        return Ok(JsValue::null());
    };
    let siblings = document.borrow().children(parent).to_vec();
    let Some(position) = siblings.iter().position(|&id| id == node) else {
        return Ok(JsValue::null());
    };
    let target = if next {
        siblings.get(position + 1)
    } else if position > 0 {
        siblings.get(position - 1)
    } else {
        None
    };
    wrap_or_null(target.copied(), context)
}

fn get_next_sibling(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    sibling(this, context, true)
}

fn get_previous_sibling(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    sibling(this, context, false)
}

fn get_children(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    build_collection(
        CollectionSource::Children {
            owner: node_of(this, context)?,
        },
        context,
    )
}

fn get_child_nodes(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    build_collection(
        CollectionSource::ChildNodes {
            owner: node_of(this, context)?,
        },
        context,
    )
}

// ---------------------------------------------------------------------------
// NodeList / HTMLCollection
//
// `querySelectorAll` produces a *static* collection (a snapshot of the nodes
// that matched at the time of the call), while `childNodes`, `children`,
// `getElementsByTagName` and `getElementsByClassName` produce *live*
// collections that re-query the document on every property read. The live
// behaviour is implemented with a Proxy whose `get` trap resolves `length` and
// numeric indices on demand.
// ---------------------------------------------------------------------------

const COLLECTION_SOURCE_KEY: &str = "__nexium_collection_source";
const ITERATOR_IDS_KEY: &str = "__nexium_iterator_ids";
const ITERATOR_INDEX_KEY: &str = "__nexium_iterator_index";
const ITERATOR_KIND_KEY: &str = "__nexium_iterator_kind";

#[derive(Clone)]
enum CollectionSource {
    Static(Vec<NodeId>),
    ChildNodes { owner: NodeId },
    Children { owner: NodeId },
    ByTag { owner: NodeId, tag: String },
    ByClass { owner: NodeId, names: Vec<String> },
}

/// Resolve the current list of node ids for a collection source.
fn source_ids(source: &CollectionSource) -> Vec<NodeId> {
    let Some(handle) = document_rc() else {
        return Vec::new();
    };
    let document = handle.borrow();
    match source {
        CollectionSource::Static(ids) => ids.clone(),
        CollectionSource::ChildNodes { owner } => document.children(*owner).to_vec(),
        CollectionSource::Children { owner } => document
            .children(*owner)
            .iter()
            .copied()
            .filter(|&id| document.is_element(id))
            .collect(),
        CollectionSource::ByTag { owner, tag } => document
            .descendants(*owner)
            .into_iter()
            .filter(|&id| {
                tag == "*"
                    || document
                        .element(id)
                        .is_some_and(|element| element.name.eq_ignore_ascii_case(tag))
            })
            .collect(),
        CollectionSource::ByClass { owner, names } => document
            .descendants(*owner)
            .into_iter()
            .filter(|&id| {
                document.element(id).is_some_and(|element| {
                    names
                        .iter()
                        .all(|name| element.classes().any(|class| class == name))
                })
            })
            .collect(),
    }
}

fn write_source(source: &CollectionSource, context: &mut Context) -> JsResult<JsObject> {
    let descriptor = ObjectInitializer::new(context).build();
    let (mode, ids, owner, tag, names) = match source {
        CollectionSource::Static(ids) => (0u32, Some(ids), None, None, None),
        CollectionSource::ChildNodes { owner } => (1, None, Some(owner), None, None),
        CollectionSource::Children { owner } => (2, None, Some(owner), None, None),
        CollectionSource::ByTag { owner, tag } => (3, None, Some(owner), Some(tag), None),
        CollectionSource::ByClass { owner, names } => (4, None, Some(owner), None, Some(names)),
    };
    descriptor.set(JsString::from("mode"), JsValue::from(mode), false, context)?;
    if let Some(ids) = ids {
        let array = JsArray::new(context)?;
        for id in ids {
            array.push(JsValue::from(id.index() as f64), context)?;
        }
        descriptor.set(JsString::from("ids"), JsValue::from(array), false, context)?;
    }
    if let Some(owner) = owner {
        descriptor.set(
            JsString::from("owner"),
            JsValue::from(owner.index() as f64),
            false,
            context,
        )?;
    }
    if let Some(tag) = tag {
        descriptor.set(JsString::from("tag"), js_string_value(tag), false, context)?;
    }
    if let Some(names) = names {
        let array = JsArray::new(context)?;
        for name in names {
            array.push(js_string_value(name), context)?;
        }
        descriptor.set(
            JsString::from("names"),
            JsValue::from(array),
            false,
            context,
        )?;
    }
    Ok(descriptor)
}

fn read_source(this: &JsValue, context: &mut Context) -> JsResult<CollectionSource> {
    let object = this
        .as_object()
        .ok_or_else(|| host_error("invalid NodeList receiver"))?;
    let value = object.get(JsString::from(COLLECTION_SOURCE_KEY), context)?;
    let descriptor = value
        .as_object()
        .ok_or_else(|| host_error("invalid NodeList source"))?;
    let mode = descriptor
        .get(JsString::from("mode"), context)?
        .as_number()
        .unwrap_or(0.0) as u32;
    let owner = descriptor
        .get(JsString::from("owner"), context)
        .ok()
        .and_then(|value| value.as_number())
        .and_then(|value| (value >= 0.0).then(|| NodeId::from_index(value as usize)));
    match mode {
        0 => {
            let ids = descriptor
                .get(JsString::from("ids"), context)?
                .as_object()
                .and_then(|object| JsArray::from_object(object.clone()).ok());
            let mut result = Vec::new();
            if let Some(ids) = ids {
                for index in 0..ids.length(context)? {
                    if let Some(value) = ids
                        .get(index, context)
                        .ok()
                        .and_then(|value| value.as_number())
                        .and_then(|value| {
                            (value >= 0.0).then(|| NodeId::from_index(value as usize))
                        })
                    {
                        result.push(value);
                    }
                }
            }
            Ok(CollectionSource::Static(result))
        }
        1 => Ok(CollectionSource::ChildNodes {
            owner: owner.ok_or_else(|| host_error("invalid NodeList owner"))?,
        }),
        2 => Ok(CollectionSource::Children {
            owner: owner.ok_or_else(|| host_error("invalid NodeList owner"))?,
        }),
        3 => {
            let tag = descriptor
                .get(JsString::from("tag"), context)
                .ok()
                .map(|value| value_to_display(&value, context))
                .unwrap_or_default();
            Ok(CollectionSource::ByTag {
                owner: owner.ok_or_else(|| host_error("invalid NodeList owner"))?,
                tag,
            })
        }
        4 => {
            let names = descriptor
                .get(JsString::from("names"), context)
                .ok()
                .and_then(|value| value.as_object())
                .and_then(|object| JsArray::from_object(object).ok());
            let mut result = Vec::new();
            if let Some(names) = names {
                for index in 0..names.length(context)? {
                    if let Ok(value) = names.get(index, context) {
                        result.push(value_to_display(&value, context));
                    }
                }
            }
            Ok(CollectionSource::ByClass {
                owner: owner.ok_or_else(|| host_error("invalid NodeList owner"))?,
                names: result,
            })
        }
        _ => Ok(CollectionSource::Static(Vec::new())),
    }
}

/// Convert a `item()` index argument following WebIDL-style rules. Returns
/// `None` for values that cannot address an element (no argument, NaN,
/// negative or infinite values); fractional values are truncated toward zero.
fn item_index(args: &[JsValue], context: &mut Context) -> Option<usize> {
    let value = args.first()?;
    if value.is_null_or_undefined() {
        return None;
    }
    let number = value.to_number(context).ok()?;
    if number.is_nan() || number.is_infinite() || number < 0.0 {
        return None;
    }
    Some(number.trunc() as usize)
}

fn collection_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(index) = item_index(args, context) else {
        return Ok(JsValue::null());
    };
    let source = read_source(this, context)?;
    let ids = source_ids(&source);
    match ids.get(index) {
        Some(&id) => wrap_node(id, context),
        None => Ok(JsValue::null()),
    }
}

fn make_iterator(ids: Vec<NodeId>, kind: u32, context: &mut Context) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    let array = JsArray::new(context)?;
    for id in ids {
        array.push(JsValue::from(id.index() as f64), context)?;
    }
    let object = ObjectInitializer::new(context)
        .property(
            JsString::from(ITERATOR_IDS_KEY),
            JsValue::from(array),
            Attribute::empty(),
        )
        .property(
            JsString::from(ITERATOR_INDEX_KEY),
            JsValue::from(0.0),
            Attribute::all(),
        )
        .property(
            JsString::from(ITERATOR_KIND_KEY),
            JsValue::from(kind),
            Attribute::empty(),
        )
        .property(
            JsString::from("next"),
            NativeFunction::from_fn_ptr(iterator_next).to_js_function(&realm),
            Attribute::all(),
        )
        .property(
            JsSymbol::iterator(),
            NativeFunction::from_fn_ptr(iterator_self).to_js_function(&realm),
            Attribute::all(),
        )
        .build();
    object.set(
        JsSymbol::to_string_tag(),
        js_string_value("NodeList Iterator"),
        false,
        context,
    )?;
    Ok(JsValue::from(object))
}

fn iterator_self(this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(this.clone())
}

fn iterator_next(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| host_error("invalid NodeList iterator"))?;
    let ids = object
        .get(JsString::from(ITERATOR_IDS_KEY), context)?
        .as_object()
        .and_then(|object| JsArray::from_object(object.clone()).ok())
        .ok_or_else(|| host_error("invalid NodeList iterator ids"))?;
    let length = ids.length(context)?;
    let index = object
        .get(JsString::from(ITERATOR_INDEX_KEY), context)?
        .as_number()
        .unwrap_or(0.0) as u64;
    let kind = object
        .get(JsString::from(ITERATOR_KIND_KEY), context)?
        .as_number()
        .unwrap_or(0.0) as u32;
    if index >= length {
        return Ok(iter_result(JsValue::undefined(), true, context)?);
    }
    object.set(
        JsString::from(ITERATOR_INDEX_KEY),
        JsValue::from((index + 1) as f64),
        false,
        context,
    )?;
    let id_value = ids.get(index, context)?;
    let node = id_value
        .as_number()
        .and_then(|value| (value >= 0.0).then(|| NodeId::from_index(value as usize)))
        .map(|id| wrap_node(id, context))
        .transpose()?
        .unwrap_or(JsValue::undefined());
    let value = match kind {
        1 => JsValue::from(index as f64),
        2 => {
            let pair = JsArray::new(context)?;
            pair.push(JsValue::from(index as f64), context)?;
            pair.push(node, context)?;
            JsValue::from(pair)
        }
        _ => node,
    };
    iter_result(value, false, context)
}

fn iter_result(value: JsValue, done: bool, context: &mut Context) -> JsResult<JsValue> {
    let object = ObjectInitializer::new(context)
        .property(JsString::from("value"), value, Attribute::all())
        .property(
            JsString::from("done"),
            JsValue::from(done),
            Attribute::all(),
        )
        .build();
    Ok(JsValue::from(object))
}

fn collection_for_each(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(callback) = args.first().and_then(JsValue::as_function) else {
        return Ok(JsValue::undefined());
    };
    let this_arg = args.get(1).cloned().unwrap_or(JsValue::undefined());
    let source = read_source(this, context)?;
    let ids = source_ids(&source);
    // Snapshot the nodes so callbacks that mutate the document do not skip
    // entries (matching the "live but pre-counted" behaviour of browsers).
    let nodes = ids
        .into_iter()
        .map(|id| wrap_node(id, context))
        .collect::<JsResult<Vec<_>>>()?;
    for (index, node) in nodes.into_iter().enumerate() {
        callback.call(
            &this_arg,
            &[node, JsValue::from(index as f64), this.clone()],
            context,
        )?;
    }
    Ok(JsValue::undefined())
}

fn collection_values(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let source = read_source(this, context)?;
    make_iterator(source_ids(&source), 0, context)
}

fn collection_keys(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let source = read_source(this, context)?;
    make_iterator(source_ids(&source), 1, context)
}

fn collection_entries(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let source = read_source(this, context)?;
    make_iterator(source_ids(&source), 2, context)
}

fn collection_symbol_iterator(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    collection_values(this, _args, context)
}

fn collection_get(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    // A `get` proxy trap is called with «target, propertyKey, receiver».
    let target = args.first().cloned().unwrap_or(JsValue::undefined());
    let key = args.get(1).cloned().unwrap_or(JsValue::undefined());
    if let Some(name) = key.as_string() {
        let name = name.to_std_string_escaped();
        if name == "length" {
            let source = read_source(&target, context)?;
            return Ok(JsValue::from(source_ids(&source).len() as f64));
        }
        if let Ok(index) = name.parse::<u64>() {
            // Canonical array indices never have leading zeroes.
            if index.to_string() == name {
                let source = read_source(&target, context)?;
                let ids = source_ids(&source);
                return match ids.get(index as usize) {
                    Some(&id) => wrap_node(id, context),
                    None => Ok(JsValue::undefined()),
                };
            }
        }
    }
    let object = target
        .as_object()
        .ok_or_else(|| host_error("invalid NodeList target"))?;
    let property: PropertyKey = if let Some(string) = key.as_string() {
        PropertyKey::from(string)
    } else if let Some(symbol) = key.as_symbol() {
        PropertyKey::from(symbol)
    } else {
        PropertyKey::from(JsString::from(value_to_display(&key, context)))
    };
    object.get(property, context)
}

fn build_collection(source: CollectionSource, context: &mut Context) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    let descriptor = write_source(&source, context)?;
    let target = ObjectInitializer::new(context)
        .property(
            JsString::from(COLLECTION_SOURCE_KEY),
            JsValue::from(descriptor),
            Attribute::empty(),
        )
        .property(
            JsString::from("item"),
            NativeFunction::from_fn_ptr(collection_item).to_js_function(&realm),
            Attribute::all(),
        )
        .property(
            JsString::from("forEach"),
            NativeFunction::from_fn_ptr(collection_for_each).to_js_function(&realm),
            Attribute::all(),
        )
        .property(
            JsString::from("keys"),
            NativeFunction::from_fn_ptr(collection_keys).to_js_function(&realm),
            Attribute::all(),
        )
        .property(
            JsString::from("values"),
            NativeFunction::from_fn_ptr(collection_values).to_js_function(&realm),
            Attribute::all(),
        )
        .property(
            JsString::from("entries"),
            NativeFunction::from_fn_ptr(collection_entries).to_js_function(&realm),
            Attribute::all(),
        )
        .property(
            JsSymbol::iterator(),
            NativeFunction::from_fn_ptr(collection_symbol_iterator).to_js_function(&realm),
            Attribute::all(),
        )
        .build();
    target.set(
        JsSymbol::to_string_tag(),
        js_string_value("NodeList"),
        false,
        context,
    )?;
    let proxy = JsProxy::builder(target)
        .get(collection_get)
        .build(context)?;
    Ok(JsValue::from(proxy))
}

fn js_get_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(JsValue::null());
    };
    let value = document()?
        .borrow()
        .get_attribute(node, &name)
        .map(str::to_owned);
    Ok(value
        .map(|value| js_string_value(&value))
        .unwrap_or(JsValue::null()))
}

fn js_set_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let value = string_arg(args, 1, context).unwrap_or_default();
    document()?
        .borrow_mut()
        .set_attribute(node, &name, &value)
        .map_err(|error| host_error(error.to_string()))?;
    observers::record_attribute(node, &name);
    Ok(JsValue::undefined())
}

fn js_has_attribute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(JsValue::from(false));
    };
    Ok(JsValue::from(
        document()?.borrow().get_attribute(node, &name).is_some(),
    ))
}

fn js_remove_attribute(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let _ = document()?.borrow_mut().remove_attribute(node, &name);
    observers::record_attribute(node, &name);
    Ok(JsValue::undefined())
}

/// Insert `child` (or the children of a document fragment) into `parent`
/// before `reference`. Returns the ids that were actually inserted.
fn insert_into_parent(
    parent: NodeId,
    child: NodeId,
    reference: Option<NodeId>,
    context: &mut Context,
) -> JsResult<Vec<NodeId>> {
    let document = document()?;
    let fragment_children = {
        let document_ref = document.borrow();
        match document_ref.node(child).data {
            NodeData::DocumentFragment => Some(document_ref.children(child).to_vec()),
            _ => None,
        }
    };
    let mut inserted = Vec::new();
    match fragment_children {
        Some(children) => {
            for child in children {
                document
                    .borrow_mut()
                    .insert_before(parent, child, reference)
                    .map_err(|error| host_error(error.to_string()))?;
                inserted.push(child);
            }
        }
        None => {
            document
                .borrow_mut()
                .insert_before(parent, child, reference)
                .map_err(|error| host_error(error.to_string()))?;
            inserted.push(child);
        }
    }
    let _ = context;
    Ok(inserted)
}

fn js_append_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent = node_of(this, context)?;
    let Some(child) = args.first() else {
        return Err(host_error("appendChild requires a node"));
    };
    let Some(child_object) = child.as_object() else {
        return Err(host_error("appendChild requires a node"));
    };
    let child = node_id_from_object(&child_object, context)
        .ok_or_else(|| host_error("appendChild requires a node"))?;
    let added = insert_into_parent(parent, child, None, context)?;
    observers::record_child_list(parent, &added, &[]);
    for id in added {
        maybe_execute_script(id, context);
    }
    Ok(child_object.into())
}

fn js_insert_before(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent = node_of(this, context)?;
    let child = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| node_id_from_object(&object, context))
        .ok_or_else(|| host_error("insertBefore requires a node"))?;
    let reference = args
        .get(1)
        .and_then(JsValue::as_object)
        .and_then(|object| node_id_from_object(&object, context));
    let added = insert_into_parent(parent, child, reference, context)?;
    observers::record_child_list(parent, &added, &[]);
    for id in added {
        maybe_execute_script(id, context);
    }
    Ok(args.first().cloned().unwrap_or(JsValue::undefined()))
}

fn js_replace_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent = node_of(this, context)?;
    let new_object = args
        .first()
        .and_then(JsValue::as_object)
        .ok_or_else(|| host_error("replaceChild requires a node"))?;
    let new_child = node_id_from_object(&new_object, context)
        .ok_or_else(|| host_error("replaceChild requires a node"))?;
    let old_object = args
        .get(1)
        .and_then(JsValue::as_object)
        .ok_or_else(|| host_error("replaceChild requires a node"))?;
    let old_child = node_id_from_object(&old_object, context)
        .ok_or_else(|| host_error("replaceChild requires a node"))?;
    if new_child == old_child {
        return Ok(args[1].clone());
    }
    {
        let document = document()?;
        if document.borrow().parent(old_child) != Some(parent) {
            return Err(host_error(
                "replaceChild: the node to replace is not a child of this node",
            ));
        }
    }
    let added = insert_into_parent(parent, new_child, Some(old_child), context)?;
    document()?
        .borrow_mut()
        .remove_child(parent, old_child)
        .map_err(|error| host_error(error.to_string()))?;
    observers::record_child_list(parent, &added, &[old_child]);
    for id in added {
        maybe_execute_script(id, context);
    }
    Ok(args[1].clone())
}

fn js_clone_node(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let deep = args.first().is_some_and(JsValue::to_boolean);
    let document = document()?;
    let clone = clone_node_within(&mut document.borrow_mut(), node, deep)?;
    wrap_node(clone, context)
}

fn clone_node_within(document: &mut Document, node: NodeId, deep: bool) -> JsResult<NodeId> {
    match document.node(node).data.clone() {
        NodeData::Element(element) => {
            let clone = document.create_element_ns(
                &element.name,
                element.namespace,
                element.attributes.clone(),
            );
            if deep {
                if let Some(template) = element.template_contents {
                    let fragment = document.create_document_fragment();
                    for child in document.children(template).to_vec() {
                        let cloned = clone_node_within(document, child, true)?;
                        let _ = document.append_child(fragment, cloned);
                    }
                    if let Some(cloned_element) = document.element_mut(clone) {
                        cloned_element.template_contents = Some(fragment);
                    }
                }
                for child in document.children(node).to_vec() {
                    let cloned = clone_node_within(document, child, true)?;
                    let _ = document.append_child(clone, cloned);
                }
            }
            Ok(clone)
        }
        NodeData::Text(text) => Ok(document.create_text(text)),
        NodeData::Comment(text) => Ok(document.create_comment(text)),
        NodeData::Doctype(doctype) => {
            Ok(document.create_doctype(&doctype.name, &doctype.public_id, &doctype.system_id))
        }
        NodeData::DocumentFragment => {
            let fragment = document.create_document_fragment();
            if deep {
                for child in document.children(node).to_vec() {
                    let cloned = clone_node_within(document, child, true)?;
                    let _ = document.append_child(fragment, cloned);
                }
            }
            Ok(fragment)
        }
        NodeData::Document => Err(host_error("cloneNode is not supported for Document")),
    }
}

fn js_matches(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(selector) = string_arg(args, 0, context) else {
        return Ok(JsValue::from(false));
    };
    let document = document()?;
    let document_ref = document.borrow();
    if !document_ref.is_element(node) {
        return Ok(JsValue::from(false));
    }
    let matched = document_ref
        .matches(node, &selector)
        .map_err(|error| host_error(error.to_string()))?;
    Ok(JsValue::from(matched))
}

fn js_closest(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(selector) = string_arg(args, 0, context) else {
        return Ok(JsValue::null());
    };
    let found = {
        let document = document()?;
        let document_ref = document.borrow();
        let mut current = Some(node);
        let mut found = None;
        while let Some(id) = current {
            if document_ref.is_element(id)
                && document_ref
                    .matches(id, &selector)
                    .map_err(|error| host_error(error.to_string()))?
            {
                found = Some(id);
                break;
            }
            current = document_ref.parent(id);
        }
        found
    };
    wrap_or_null(found, context)
}

fn node_ids_from_args(args: &[JsValue], context: &mut Context) -> JsResult<Vec<NodeId>> {
    let document = document()?;
    let mut ids = Vec::new();
    for arg in args {
        if let Some(object) = arg.as_object()
            && let Some(id) = node_id_from_object(&object, context)
        {
            ids.push(id);
            continue;
        }
        let text = value_to_display(arg, context);
        ids.push(document.borrow_mut().create_text(&text));
    }
    Ok(ids)
}

fn js_prepend(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent = node_of(this, context)?;
    let ids = node_ids_from_args(args, context)?;
    let reference = document()?.borrow().children(parent).first().copied();
    let mut added = Vec::new();
    for id in ids {
        if let Ok(inserted) = insert_into_parent(parent, id, reference, context) {
            for inserted in inserted {
                maybe_execute_script(inserted, context);
                added.push(inserted);
            }
        }
    }
    observers::record_child_list(parent, &added, &[]);
    Ok(JsValue::undefined())
}

fn js_append(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent = node_of(this, context)?;
    let ids = node_ids_from_args(args, context)?;
    let mut added = Vec::new();
    for id in ids {
        if let Ok(inserted) = insert_into_parent(parent, id, None, context) {
            for inserted in inserted {
                maybe_execute_script(inserted, context);
                added.push(inserted);
            }
        }
    }
    observers::record_child_list(parent, &added, &[]);
    Ok(JsValue::undefined())
}

fn js_remove_child(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent = node_of(this, context)?;
    let child = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| node_id_from_object(&object, context))
        .ok_or_else(|| host_error("removeChild requires a node"))?;
    document()?
        .borrow_mut()
        .remove_child(parent, child)
        .map_err(|error| host_error(error.to_string()))?;
    observers::record_child_list(parent, &[], &[child]);
    Ok(args.first().cloned().unwrap_or(JsValue::undefined()))
}

fn js_remove_self(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let parent = document()?.borrow().parent(node);
    if let Some(parent) = parent {
        let _ = document()?.borrow_mut().remove_child(parent, node);
        observers::record_child_list(parent, &[], &[node]);
    }
    Ok(JsValue::undefined())
}

fn js_query_selector(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let root = node_of(this, context)?;
    let Some(selector) = string_arg(args, 0, context) else {
        return Ok(JsValue::null());
    };
    let found = document()?
        .borrow()
        .query_selector(root, &selector)
        .map_err(|error| host_error(error.to_string()))?;
    wrap_or_null(found, context)
}

fn js_query_selector_all(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let root = node_of(this, context)?;
    let Some(selector) = string_arg(args, 0, context) else {
        return build_collection(CollectionSource::Static(Vec::new()), context);
    };
    let ids = document()?
        .borrow()
        .query_selector_all(root, &selector)
        .map_err(|error| host_error(error.to_string()))?;
    build_collection(CollectionSource::Static(ids), context)
}

fn js_get_elements_by_tag_name(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let root = node_of(this, context)?;
    let Some(tag) = string_arg(args, 0, context) else {
        return build_collection(CollectionSource::Static(Vec::new()), context);
    };
    build_collection(
        CollectionSource::ByTag {
            owner: root,
            tag: tag.to_ascii_lowercase(),
        },
        context,
    )
}

fn js_get_elements_by_class_name(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let root = node_of(this, context)?;
    let Some(names) = string_arg(args, 0, context) else {
        return build_collection(CollectionSource::Static(Vec::new()), context);
    };
    build_collection(
        CollectionSource::ByClass {
            owner: root,
            names: names.split_ascii_whitespace().map(str::to_owned).collect(),
        },
        context,
    )
}

fn camel_case(name: &str) -> String {
    let mut camel = String::with_capacity(name.len());
    let mut upper = false;
    for character in name.chars() {
        if character == '-' {
            upper = true;
        } else if upper {
            camel.push(character.to_ascii_uppercase());
            upper = false;
        } else {
            camel.push(character);
        }
    }
    camel
}

/// Normalize a CSS property name to its canonical dashed form. Custom
/// properties (`--*`) are returned verbatim (they are case-sensitive).
/// Both dashed (`background-color`) and camelCase (`backgroundColor`) inputs
/// are accepted so `getPropertyValue`/`setProperty` behave like browsers that
/// tolerate either spelling for standard properties.
fn normalize_css_property(name: &str) -> String {
    if name.starts_with("--") {
        return name.to_owned();
    }
    let mut kebab = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index != 0 {
                kebab.push('-');
            }
            kebab.push(character.to_ascii_lowercase());
        } else {
            kebab.push(character);
        }
    }
    kebab
}

fn js_style_get_property(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = normalize_css_property(&string_arg(args, 0, context).unwrap_or_default());
    let Some(object) = this.as_object() else {
        return Ok(js_string_value(""));
    };
    // Inline style objects carry the owning node id; computed-style objects
    // carry a snapshot map instead.
    if node_id_from_object(&object, context).is_some() {
        let node = node_of(this, context)?;
        return style_get(node, &name, context);
    }
    let map = object.get(JsString::from("__nexium_style"), context)?;
    let Some(map) = map.as_object() else {
        return Ok(js_string_value(""));
    };
    let value = map.get(JsString::from(name.as_str()), context)?;
    Ok(if value.is_undefined() {
        js_string_value("")
    } else {
        value
    })
}

fn js_style_get_priority(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(object) = this.as_object() else {
        return Ok(js_string_value(""));
    };
    if node_id_from_object(&object, context).is_none() {
        return Ok(js_string_value(""));
    }
    let name = normalize_css_property(&string_arg(args, 0, context).unwrap_or_default());
    let node = node_of(this, context)?;
    let important = inline_declaration(node, context)
        .iter()
        .any(|declaration| declaration.name == name && declaration.important);
    Ok(js_string_value(if important { "important" } else { "" }))
}

pub(crate) fn js_get_computed_style(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(target) = args.first() else {
        return Ok(JsValue::null());
    };
    let node = node_of(target, context)?;
    let styles = with_state(|state| state.computed_styles.get(&node.index()).cloned()).flatten();
    let styles = styles.unwrap_or_else(|| inline_style_map(node));
    let realm = context.realm().clone();
    let mut hidden = ObjectInitializer::new(context);
    for (name, value) in &styles {
        hidden.property(
            JsString::from(name.as_str()),
            js_string_value(value),
            Attribute::all(),
        );
    }
    let hidden = hidden.build();
    let mut object = ObjectInitializer::new(context);
    for (name, value) in &styles {
        object.property(
            JsString::from(name.as_str()),
            js_string_value(value),
            Attribute::all(),
        );
        let camel = camel_case(name);
        if camel != *name {
            object.property(
                JsString::from(camel.as_str()),
                js_string_value(value),
                Attribute::all(),
            );
        }
    }
    object
        .property(
            JsString::from("__nexium_style"),
            JsValue::from(hidden),
            Attribute::empty(),
        )
        .property(
            JsString::from("getPropertyValue"),
            NativeFunction::from_fn_ptr(js_style_get_property).to_js_function(&realm),
            Attribute::all(),
        )
        .property(
            JsString::from("getPropertyPriority"),
            NativeFunction::from_fn_ptr(js_style_get_priority).to_js_function(&realm),
            Attribute::all(),
        );
    Ok(JsValue::from(object.build()))
}

/// During initial script execution no layout has been published yet; fall
/// back to the element's inline declarations so reads like
/// `getComputedStyle(el).color` still work.
fn inline_style_map(node: NodeId) -> HashMap<String, String> {
    let Some(document) = document_rc() else {
        return HashMap::new();
    };
    let text = {
        let document = document.borrow();
        document
            .get_attribute(node, "style")
            .map(str::to_owned)
            .unwrap_or_default()
    };
    crate::engine::css::parse_declaration_list(&text)
        .into_iter()
        .map(|declaration| {
            (
                declaration.name.to_ascii_lowercase(),
                serialize_components(&declaration.value),
            )
        })
        .collect()
}

fn layout_rect_for(node: NodeId) -> [f32; 4] {
    with_state(|state| state.layout_rects.get(&node.index()).copied())
        .flatten()
        .unwrap_or([0.0, 0.0, 0.0, 0.0])
}

fn rect_object(rect: [f32; 4], context: &mut Context) -> JsResult<JsValue> {
    let [x, y, width, height] = rect;
    let mut object = ObjectInitializer::new(context);
    for (name, value) in [
        ("x", x),
        ("y", y),
        ("width", width),
        ("height", height),
        ("top", y),
        ("left", x),
        ("right", x + width),
        ("bottom", y + height),
    ] {
        object.property(
            JsString::from(name),
            JsValue::from(value as f64),
            Attribute::all(),
        );
    }
    Ok(JsValue::from(object.build()))
}

fn js_get_bounding_client_rect(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let rect = layout_rect_for(node);
    rect_object(rect, context)
}

fn js_get_client_rects(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let array = JsArray::new(context)?;
    let node = node_of(this, context)?;
    array.push(rect_object(layout_rect_for(node), context)?, context)?;
    Ok(JsValue::from(array))
}

fn get_cookie(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let pair = with_state(|state| (state.client.clone(), state.base_url.clone()));
    let Some((Some(client), base_url)) = pair else {
        return Ok(js_string_value(""));
    };
    Ok(js_string_value(
        &client.cookie_header_for_url(&base_url).unwrap_or_default(),
    ))
}

fn set_cookie(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(value) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let pair = with_state(|state| (state.client.clone(), state.base_url.clone()));
    if let Some((Some(client), base_url)) = pair {
        client.set_document_cookie(&base_url, &value);
    }
    Ok(JsValue::undefined())
}

fn js_get_element_by_id(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let _root = node_of(this, context)?;
    let Some(id) = string_arg(args, 0, context) else {
        return Ok(JsValue::null());
    };
    let found = document()?.borrow().get_element_by_id(&id);
    wrap_or_null(found, context)
}

fn js_create_element(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _root = node_of(this, context)?;
    let Some(tag) = string_arg(args, 0, context) else {
        return Err(host_error("createElement requires a tag name"));
    };
    let id = {
        let document_handle = document()?;
        let mut document = document_handle.borrow_mut();
        if tag.eq_ignore_ascii_case("svg") {
            document.create_element_ns("svg", Namespace::Svg, Vec::new())
        } else {
            document.create_element(&tag)
        }
    };
    wrap_node(id, context)
}

fn js_create_element_ns(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let _root = node_of(this, context)?;
    let namespace = string_arg(args, 0, context);
    let Some(qualified) = string_arg(args, 1, context) else {
        return Err(host_error("createElementNS requires a qualified name"));
    };
    if qualified.is_empty() {
        return Err(host_error("createElementNS requires a qualified name"));
    }
    let namespace = match namespace.as_deref() {
        None | Some("") | Some("http://www.w3.org/1999/xhtml") => Namespace::Html,
        Some("http://www.w3.org/2000/svg") => Namespace::Svg,
        Some("http://www.w3.org/1998/Math/MathML") => Namespace::MathMl,
        Some(other) => {
            return Err(host_error(format!(
                "createElementNS: unsupported namespace {other}"
            )));
        }
    };
    let id = document()?
        .borrow_mut()
        .create_element_ns(&qualified, namespace, Vec::new());
    wrap_node(id, context)
}

fn js_create_document_fragment(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let _root = node_of(this, context)?;
    let id = document()?.borrow_mut().create_document_fragment();
    wrap_node(id, context)
}

fn js_create_text_node(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let _root = node_of(this, context)?;
    let text = string_arg(args, 0, context).unwrap_or_default();
    let id = document()?.borrow_mut().create_text(text);
    wrap_node(id, context)
}

fn js_create_comment(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _root = node_of(this, context)?;
    let text = string_arg(args, 0, context).unwrap_or_default();
    let id = document()?.borrow_mut().create_comment(text);
    wrap_node(id, context)
}

fn get_canvas_width(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    get_canvas_size(this, context, "width", 300)
}

fn get_canvas_height(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    get_canvas_size(this, context, "height", 150)
}

fn get_canvas_size(
    this: &JsValue,
    context: &mut Context,
    name: &str,
    default: u32,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = document()?
        .borrow()
        .get_attribute(node, name)
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(default);
    Ok(JsValue::from(value as f64))
}

fn set_canvas_width(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_canvas_size(this, args, "width", 300, context)
}

fn set_canvas_height(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_canvas_size(this, args, "height", 150, context)
}

fn set_canvas_size(
    this: &JsValue,
    args: &[JsValue],
    name: &str,
    default: u32,
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_id_from_this(this, context).ok_or_else(|| host_error("invalid canvas"))?;
    let value = args
        .first()
        .and_then(JsValue::as_number)
        .map(|value| value.max(1.0) as u32)
        .unwrap_or(default);
    document()?
        .borrow_mut()
        .set_attribute(node, name, &value.to_string())
        .map_err(|error| host_error(error.to_string()))?;
    super::canvas::reset_canvas(node);
    Ok(JsValue::undefined())
}

fn get_value(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let document = document()?;
    let document = document.borrow();
    let value = document
        .get_attribute(node, "value")
        .map(str::to_owned)
        .or_else(|| {
            (document.tag_name(node) == Some("textarea")).then(|| document.text_content(node))
        })
        .unwrap_or_default();
    Ok(js_string_value(&value))
}

fn set_value(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = string_arg(args, 0, context).unwrap_or_default();
    let is_textarea = document()?.borrow().tag_name(node) == Some("textarea");
    if is_textarea {
        document()?
            .borrow_mut()
            .set_text_content(node, &value)
            .map_err(|error| host_error(error.to_string()))?;
    } else {
        document()?
            .borrow_mut()
            .set_attribute(node, "value", &value)
            .map_err(|error| host_error(error.to_string()))?;
    }
    Ok(JsValue::undefined())
}

fn get_attribute_string(this: &JsValue, context: &mut Context, name: &str) -> JsResult<String> {
    let node = node_of(this, context)?;
    Ok(document()?
        .borrow()
        .get_attribute(node, name)
        .unwrap_or("")
        .to_owned())
}

fn set_attribute_string(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    name: &str,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = string_arg(args, 0, context).unwrap_or_default();
    document()?
        .borrow_mut()
        .set_attribute(node, name, &value)
        .map_err(|error| host_error(error.to_string()))?;
    Ok(JsValue::undefined())
}

fn get_href(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(js_string_value(&get_attribute_string(
        this, context, "href",
    )?))
}
fn set_href(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_attribute_string(this, args, context, "href")
}
fn get_src(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(js_string_value(&get_attribute_string(
        this, context, "src",
    )?))
}
fn set_src(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    set_attribute_string(this, args, context, "src")
}
fn get_checked(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    Ok(JsValue::from(
        document()?
            .borrow()
            .get_attribute(node, "checked")
            .is_some(),
    ))
}
fn set_checked(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let checked = args.first().and_then(JsValue::as_boolean).unwrap_or(false);
    let document_handle = document()?;
    let mut document = document_handle.borrow_mut();
    if checked {
        let _ = document.set_attribute(node, "checked", "");
    } else {
        let _ = document.remove_attribute(node, "checked");
    }
    Ok(JsValue::undefined())
}
fn get_disabled(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    Ok(JsValue::from(
        document()?
            .borrow()
            .get_attribute(node, "disabled")
            .is_some(),
    ))
}
fn set_disabled(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let disabled = args.first().and_then(JsValue::as_boolean).unwrap_or(false);
    let document_handle = document()?;
    let mut document = document_handle.borrow_mut();
    if disabled {
        let _ = document.set_attribute(node, "disabled", "");
    } else {
        let _ = document.remove_attribute(node, "disabled");
    }
    Ok(JsValue::undefined())
}

fn get_title(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let document = document()?;
    let document = document.borrow();
    let value = if node == document.document_node() {
        document.title().unwrap_or_default()
    } else {
        document
            .get_attribute(node, "title")
            .unwrap_or("")
            .to_owned()
    };
    Ok(js_string_value(&value))
}

fn set_title(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = string_arg(args, 0, context).unwrap_or_default();
    let document_handle = document()?;
    let mut document = document_handle.borrow_mut();
    let is_document = node == document.document_node();
    if is_document {
        if let Some(title) = document
            .descendants(document.document_node())
            .into_iter()
            .find(|&id| document.tag_name(id) == Some("title"))
        {
            let _ = document.set_text_content(title, &value);
        }
    } else {
        let _ = document.set_attribute(node, "title", &value);
    }
    Ok(JsValue::undefined())
}

fn find_element(document: &Document, tag: &str) -> Option<NodeId> {
    document
        .descendants(document.document_node())
        .into_iter()
        .find(|&id| document.tag_name(id) == Some(tag))
}

fn get_body(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _node = node_of(this, context)?;
    let found = find_element(&document()?.borrow(), "body");
    wrap_or_null(found, context)
}

fn get_document_element(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let _node = node_of(this, context)?;
    let found = find_element(&document()?.borrow(), "html");
    wrap_or_null(found, context)
}

fn get_ready_state(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(js_string_value("complete"))
}

fn get_url(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = node_id_from_this(this, context);
    let url = with_state(|state| state.base_url.to_string()).unwrap_or_default();
    Ok(js_string_value(&url))
}

pub(crate) fn get_style(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    Ok(JsValue::from(build_style_object(node, context)?))
}

fn get_class_list(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    Ok(JsValue::from(build_class_list(node, context)?))
}

fn inline_declaration(node: NodeId, context: &mut Context) -> Vec<crate::engine::css::Declaration> {
    let style = {
        let document = document_rc();
        let Some(document) = document else {
            return Vec::new();
        };
        document
            .borrow()
            .get_attribute(node, "style")
            .map(str::to_owned)
    };
    let _ = context;
    style
        .as_deref()
        .map(parse_declaration_list)
        .unwrap_or_default()
}

fn style_get(node: NodeId, name: &str, context: &mut Context) -> JsResult<JsValue> {
    let name = normalize_css_property(name);
    let declarations = inline_declaration(node, context);
    let value = declarations
        .iter()
        .find(|declaration| declaration.name.eq_ignore_ascii_case(&name))
        .map(|declaration| serialize_components(&declaration.value))
        .unwrap_or_default();
    Ok(js_string_value(&value))
}

fn style_set(
    node: NodeId,
    name: &str,
    value: &str,
    important: bool,
    context: &mut Context,
) -> JsResult<()> {
    let name = normalize_css_property(name);
    let mut declarations = inline_declaration(node, context);
    let parsed_value = parse_declaration_list(&format!("x: {value};"))
        .into_iter()
        .next()
        .map(|declaration| declaration.value)
        .unwrap_or_else(|| vec![ComponentValue::Ident(value.to_owned())]);
    if let Some(declaration) = declarations
        .iter_mut()
        .find(|declaration| declaration.name.eq_ignore_ascii_case(&name))
    {
        declaration.value = parsed_value;
        declaration.important = important;
    } else {
        declarations.push(crate::engine::css::Declaration {
            name,
            value: parsed_value,
            important,
        });
    }
    let css = serialize_declarations(&declarations);
    let _ = document()?.borrow_mut().set_attribute(node, "style", &css);
    observers::record_attribute(node, "style");
    Ok(())
}

fn style_get_property(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(js_string_value(""));
    };
    style_get(node, &name, context)
}

fn style_set_property(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let value = string_arg(args, 1, context).unwrap_or_default();
    let priority = string_arg(args, 2, context)
        .map(|value| value.eq_ignore_ascii_case("important"))
        .unwrap_or(false);
    style_set(node, &name, &value, priority, context)?;
    Ok(JsValue::undefined())
}

fn style_remove_property(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    let name = normalize_css_property(&name);
    let mut declarations = inline_declaration(node, context);
    declarations.retain(|declaration| !declaration.name.eq_ignore_ascii_case(&name));
    let css = serialize_declarations(&declarations);
    let _ = document()?.borrow_mut().set_attribute(node, "style", &css);
    observers::record_attribute(node, "style");
    Ok(JsValue::undefined())
}

fn style_get_length(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    Ok(JsValue::from(inline_declaration(node, context).len() as f64))
}

fn style_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let Some(index) = item_index(args, context) else {
        return Ok(js_string_value(""));
    };
    let declarations = inline_declaration(node, context);
    Ok(declarations
        .get(index)
        .map(|declaration| js_string_value(&declaration.name))
        .unwrap_or_else(|| js_string_value("")))
}

fn style_get_css_text(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = {
        let document = document()?;
        document
            .borrow()
            .get_attribute(node, "style")
            .unwrap_or("")
            .to_owned()
    };
    Ok(js_string_value(&value))
}

fn style_set_css_text(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let value = string_arg(args, 0, context).unwrap_or_default();
    let _ = document()?
        .borrow_mut()
        .set_attribute(node, "style", &value);
    Ok(JsValue::undefined())
}

macro_rules! style_accessor {
    ($($getter:ident, $setter:ident, $css_name:literal);* $(;)?) => {
        $(
            fn $getter(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
                let node = node_of(this, context)?;
                style_get(node, $css_name, context)
            }
            fn $setter(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
                let node = node_of(this, context)?;
                let value = string_arg(args, 0, context).unwrap_or_default();
                style_set(node, $css_name, &value, false, context)?;
                Ok(JsValue::undefined())
            }
        )*
    };
}

style_accessor!(
    get_style_color, set_style_color, "color";
    get_style_background_color, set_style_background_color, "background-color";
    get_style_background_image, set_style_background_image, "background-image";
    get_style_width, set_style_width, "width";
    get_style_height, set_style_height, "height";
    get_style_display, set_style_display, "display";
    get_style_position, set_style_position, "position";
    get_style_top, set_style_top, "top";
    get_style_right, set_style_right, "right";
    get_style_bottom, set_style_bottom, "bottom";
    get_style_left, set_style_left, "left";
    get_style_margin, set_style_margin, "margin";
    get_style_margin_top, set_style_margin_top, "margin-top";
    get_style_margin_right, set_style_margin_right, "margin-right";
    get_style_margin_bottom, set_style_margin_bottom, "margin-bottom";
    get_style_margin_left, set_style_margin_left, "margin-left";
    get_style_padding, set_style_padding, "padding";
    get_style_padding_top, set_style_padding_top, "padding-top";
    get_style_padding_right, set_style_padding_right, "padding-right";
    get_style_padding_bottom, set_style_padding_bottom, "padding-bottom";
    get_style_padding_left, set_style_padding_left, "padding-left";
    get_style_font_size, set_style_font_size, "font-size";
    get_style_font_weight, set_style_font_weight, "font-weight";
    get_style_font_style, set_style_font_style, "font-style";
    get_style_font_family, set_style_font_family, "font-family";
    get_style_line_height, set_style_line_height, "line-height";
    get_style_text_align, set_style_text_align, "text-align";
    get_style_border, set_style_border, "border";
    get_style_border_color, set_style_border_color, "border-color";
    get_style_border_radius, set_style_border_radius, "border-radius";
    get_style_opacity, set_style_opacity, "opacity";
    get_style_flex_direction, set_style_flex_direction, "flex-direction";
    get_style_justify_content, set_style_justify_content, "justify-content";
    get_style_align_items, set_style_align_items, "align-items";
    get_style_gap, set_style_gap, "gap";
    get_style_transform, set_style_transform, "transform";
);

// Additional common CSS properties. Kept in a separate invocation so the
// original accessor set stays readable while `style` now covers the properties
// scripts most frequently read and write.
style_accessor!(
    get_style_background, set_style_background, "background";
    get_style_background_repeat, set_style_background_repeat, "background-repeat";
    get_style_background_position, set_style_background_position, "background-position";
    get_style_background_size, set_style_background_size, "background-size";
    get_style_background_attachment, set_style_background_attachment, "background-attachment";
    get_style_border_width, set_style_border_width, "border-width";
    get_style_border_style, set_style_border_style, "border-style";
    get_style_border_top, set_style_border_top, "border-top";
    get_style_border_right, set_style_border_right, "border-right";
    get_style_border_bottom, set_style_border_bottom, "border-bottom";
    get_style_border_left, set_style_border_left, "border-left";
    get_style_border_top_width, set_style_border_top_width, "border-top-width";
    get_style_border_right_width, set_style_border_right_width, "border-right-width";
    get_style_border_bottom_width, set_style_border_bottom_width, "border-bottom-width";
    get_style_border_left_width, set_style_border_left_width, "border-left-width";
    get_style_border_top_color, set_style_border_top_color, "border-top-color";
    get_style_border_right_color, set_style_border_right_color, "border-right-color";
    get_style_border_bottom_color, set_style_border_bottom_color, "border-bottom-color";
    get_style_border_left_color, set_style_border_left_color, "border-left-color";
    get_style_box_shadow, set_style_box_shadow, "box-shadow";
    get_style_text_shadow, set_style_text_shadow, "text-shadow";
    get_style_outline, set_style_outline, "outline";
    get_style_outline_color, set_style_outline_color, "outline-color";
    get_style_outline_style, set_style_outline_style, "outline-style";
    get_style_outline_width, set_style_outline_width, "outline-width";
    get_style_z_index, set_style_z_index, "z-index";
    get_style_overflow, set_style_overflow, "overflow";
    get_style_overflow_x, set_style_overflow_x, "overflow-x";
    get_style_overflow_y, set_style_overflow_y, "overflow-y";
    get_style_visibility, set_style_visibility, "visibility";
    get_style_cursor, set_style_cursor, "cursor";
    get_style_css_float, set_style_css_float, "float";
    get_style_clear, set_style_clear, "clear";
    get_style_box_sizing, set_style_box_sizing, "box-sizing";
    get_style_min_width, set_style_min_width, "min-width";
    get_style_max_width, set_style_max_width, "max-width";
    get_style_min_height, set_style_min_height, "min-height";
    get_style_max_height, set_style_max_height, "max-height";
    get_style_text_decoration, set_style_text_decoration, "text-decoration";
    get_style_text_transform, set_style_text_transform, "text-transform";
    get_style_text_indent, set_style_text_indent, "text-indent";
    get_style_text_overflow, set_style_text_overflow, "text-overflow";
    get_style_white_space, set_style_white_space, "white-space";
    get_style_word_break, set_style_word_break, "word-break";
    get_style_word_wrap, set_style_word_wrap, "word-wrap";
    get_style_letter_spacing, set_style_letter_spacing, "letter-spacing";
    get_style_word_spacing, set_style_word_spacing, "word-spacing";
    get_style_vertical_align, set_style_vertical_align, "vertical-align";
    get_style_list_style, set_style_list_style, "list-style";
    get_style_list_style_type, set_style_list_style_type, "list-style-type";
    get_style_flex, set_style_flex, "flex";
    get_style_flex_grow, set_style_flex_grow, "flex-grow";
    get_style_flex_shrink, set_style_flex_shrink, "flex-shrink";
    get_style_flex_basis, set_style_flex_basis, "flex-basis";
    get_style_flex_wrap, set_style_flex_wrap, "flex-wrap";
    get_style_align_content, set_style_align_content, "align-content";
    get_style_align_self, set_style_align_self, "align-self";
    get_style_order, set_style_order, "order";
    get_style_row_gap, set_style_row_gap, "row-gap";
    get_style_column_gap, set_style_column_gap, "column-gap";
    get_style_grid_template_columns, set_style_grid_template_columns, "grid-template-columns";
    get_style_grid_template_rows, set_style_grid_template_rows, "grid-template-rows";
    get_style_transform_origin, set_style_transform_origin, "transform-origin";
    get_style_transition, set_style_transition, "transition";
    get_style_animation, set_style_animation, "animation";
    get_style_object_fit, set_style_object_fit, "object-fit";
    get_style_user_select, set_style_user_select, "user-select";
    get_style_pointer_events, set_style_pointer_events, "pointer-events";
    get_style_border_collapse, set_style_border_collapse, "border-collapse";
    get_style_border_spacing, set_style_border_spacing, "border-spacing";
    get_style_caption_side, set_style_caption_side, "caption-side";
);

fn build_style_object(node: NodeId, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let accessor = |get: boa_engine::native_function::NativeFunctionPointer,
                    set: boa_engine::native_function::NativeFunctionPointer| {
        (
            NativeFunction::from_fn_ptr(get).to_js_function(&realm),
            NativeFunction::from_fn_ptr(set).to_js_function(&realm),
        )
    };
    let entries = [
        ("color", accessor(get_style_color, set_style_color)),
        (
            "backgroundColor",
            accessor(get_style_background_color, set_style_background_color),
        ),
        (
            "backgroundImage",
            accessor(get_style_background_image, set_style_background_image),
        ),
        ("width", accessor(get_style_width, set_style_width)),
        ("height", accessor(get_style_height, set_style_height)),
        ("display", accessor(get_style_display, set_style_display)),
        ("position", accessor(get_style_position, set_style_position)),
        ("top", accessor(get_style_top, set_style_top)),
        ("right", accessor(get_style_right, set_style_right)),
        ("bottom", accessor(get_style_bottom, set_style_bottom)),
        ("left", accessor(get_style_left, set_style_left)),
        ("margin", accessor(get_style_margin, set_style_margin)),
        (
            "marginTop",
            accessor(get_style_margin_top, set_style_margin_top),
        ),
        (
            "marginRight",
            accessor(get_style_margin_right, set_style_margin_right),
        ),
        (
            "marginBottom",
            accessor(get_style_margin_bottom, set_style_margin_bottom),
        ),
        (
            "marginLeft",
            accessor(get_style_margin_left, set_style_margin_left),
        ),
        ("padding", accessor(get_style_padding, set_style_padding)),
        (
            "paddingTop",
            accessor(get_style_padding_top, set_style_padding_top),
        ),
        (
            "paddingRight",
            accessor(get_style_padding_right, set_style_padding_right),
        ),
        (
            "paddingBottom",
            accessor(get_style_padding_bottom, set_style_padding_bottom),
        ),
        (
            "paddingLeft",
            accessor(get_style_padding_left, set_style_padding_left),
        ),
        (
            "fontSize",
            accessor(get_style_font_size, set_style_font_size),
        ),
        (
            "fontWeight",
            accessor(get_style_font_weight, set_style_font_weight),
        ),
        (
            "fontStyle",
            accessor(get_style_font_style, set_style_font_style),
        ),
        (
            "fontFamily",
            accessor(get_style_font_family, set_style_font_family),
        ),
        (
            "lineHeight",
            accessor(get_style_line_height, set_style_line_height),
        ),
        (
            "textAlign",
            accessor(get_style_text_align, set_style_text_align),
        ),
        ("border", accessor(get_style_border, set_style_border)),
        (
            "borderColor",
            accessor(get_style_border_color, set_style_border_color),
        ),
        (
            "borderRadius",
            accessor(get_style_border_radius, set_style_border_radius),
        ),
        ("opacity", accessor(get_style_opacity, set_style_opacity)),
        (
            "flexDirection",
            accessor(get_style_flex_direction, set_style_flex_direction),
        ),
        (
            "justifyContent",
            accessor(get_style_justify_content, set_style_justify_content),
        ),
        (
            "alignItems",
            accessor(get_style_align_items, set_style_align_items),
        ),
        ("gap", accessor(get_style_gap, set_style_gap)),
        (
            "transform",
            accessor(get_style_transform, set_style_transform),
        ),
    ];
    let css_text = accessor(style_get_css_text, style_set_css_text);
    let length = NativeFunction::from_fn_ptr(style_get_length).to_js_function(&realm);
    let mut object = ObjectInitializer::new(context);
    object.accessor(
        JsString::from("length"),
        Some(length),
        None,
        Attribute::all(),
    );
    object.accessor(
        JsString::from("cssText"),
        Some(css_text.0),
        Some(css_text.1),
        Attribute::all(),
    );
    for (name, (get, set)) in entries {
        object.accessor(JsString::from(name), Some(get), Some(set), Attribute::all());
    }
    let extra_entries = [
        (
            "background",
            accessor(get_style_background, set_style_background),
        ),
        (
            "backgroundRepeat",
            accessor(get_style_background_repeat, set_style_background_repeat),
        ),
        (
            "backgroundPosition",
            accessor(get_style_background_position, set_style_background_position),
        ),
        (
            "backgroundSize",
            accessor(get_style_background_size, set_style_background_size),
        ),
        (
            "backgroundAttachment",
            accessor(
                get_style_background_attachment,
                set_style_background_attachment,
            ),
        ),
        (
            "borderWidth",
            accessor(get_style_border_width, set_style_border_width),
        ),
        (
            "borderStyle",
            accessor(get_style_border_style, set_style_border_style),
        ),
        (
            "borderTop",
            accessor(get_style_border_top, set_style_border_top),
        ),
        (
            "borderRight",
            accessor(get_style_border_right, set_style_border_right),
        ),
        (
            "borderBottom",
            accessor(get_style_border_bottom, set_style_border_bottom),
        ),
        (
            "borderLeft",
            accessor(get_style_border_left, set_style_border_left),
        ),
        (
            "borderTopWidth",
            accessor(get_style_border_top_width, set_style_border_top_width),
        ),
        (
            "borderRightWidth",
            accessor(get_style_border_right_width, set_style_border_right_width),
        ),
        (
            "borderBottomWidth",
            accessor(get_style_border_bottom_width, set_style_border_bottom_width),
        ),
        (
            "borderLeftWidth",
            accessor(get_style_border_left_width, set_style_border_left_width),
        ),
        (
            "borderTopColor",
            accessor(get_style_border_top_color, set_style_border_top_color),
        ),
        (
            "borderRightColor",
            accessor(get_style_border_right_color, set_style_border_right_color),
        ),
        (
            "borderBottomColor",
            accessor(get_style_border_bottom_color, set_style_border_bottom_color),
        ),
        (
            "borderLeftColor",
            accessor(get_style_border_left_color, set_style_border_left_color),
        ),
        (
            "boxShadow",
            accessor(get_style_box_shadow, set_style_box_shadow),
        ),
        (
            "textShadow",
            accessor(get_style_text_shadow, set_style_text_shadow),
        ),
        ("outline", accessor(get_style_outline, set_style_outline)),
        (
            "outlineColor",
            accessor(get_style_outline_color, set_style_outline_color),
        ),
        (
            "outlineStyle",
            accessor(get_style_outline_style, set_style_outline_style),
        ),
        (
            "outlineWidth",
            accessor(get_style_outline_width, set_style_outline_width),
        ),
        ("zIndex", accessor(get_style_z_index, set_style_z_index)),
        ("overflow", accessor(get_style_overflow, set_style_overflow)),
        (
            "overflowX",
            accessor(get_style_overflow_x, set_style_overflow_x),
        ),
        (
            "overflowY",
            accessor(get_style_overflow_y, set_style_overflow_y),
        ),
        (
            "visibility",
            accessor(get_style_visibility, set_style_visibility),
        ),
        ("cursor", accessor(get_style_cursor, set_style_cursor)),
        (
            "cssFloat",
            accessor(get_style_css_float, set_style_css_float),
        ),
        ("clear", accessor(get_style_clear, set_style_clear)),
        (
            "boxSizing",
            accessor(get_style_box_sizing, set_style_box_sizing),
        ),
        (
            "minWidth",
            accessor(get_style_min_width, set_style_min_width),
        ),
        (
            "maxWidth",
            accessor(get_style_max_width, set_style_max_width),
        ),
        (
            "minHeight",
            accessor(get_style_min_height, set_style_min_height),
        ),
        (
            "maxHeight",
            accessor(get_style_max_height, set_style_max_height),
        ),
        (
            "textDecoration",
            accessor(get_style_text_decoration, set_style_text_decoration),
        ),
        (
            "textTransform",
            accessor(get_style_text_transform, set_style_text_transform),
        ),
        (
            "textIndent",
            accessor(get_style_text_indent, set_style_text_indent),
        ),
        (
            "textOverflow",
            accessor(get_style_text_overflow, set_style_text_overflow),
        ),
        (
            "whiteSpace",
            accessor(get_style_white_space, set_style_white_space),
        ),
        (
            "wordBreak",
            accessor(get_style_word_break, set_style_word_break),
        ),
        (
            "wordWrap",
            accessor(get_style_word_wrap, set_style_word_wrap),
        ),
        (
            "letterSpacing",
            accessor(get_style_letter_spacing, set_style_letter_spacing),
        ),
        (
            "wordSpacing",
            accessor(get_style_word_spacing, set_style_word_spacing),
        ),
        (
            "verticalAlign",
            accessor(get_style_vertical_align, set_style_vertical_align),
        ),
        (
            "listStyle",
            accessor(get_style_list_style, set_style_list_style),
        ),
        (
            "listStyleType",
            accessor(get_style_list_style_type, set_style_list_style_type),
        ),
        ("flex", accessor(get_style_flex, set_style_flex)),
        (
            "flexGrow",
            accessor(get_style_flex_grow, set_style_flex_grow),
        ),
        (
            "flexShrink",
            accessor(get_style_flex_shrink, set_style_flex_shrink),
        ),
        (
            "flexBasis",
            accessor(get_style_flex_basis, set_style_flex_basis),
        ),
        (
            "flexWrap",
            accessor(get_style_flex_wrap, set_style_flex_wrap),
        ),
        (
            "alignContent",
            accessor(get_style_align_content, set_style_align_content),
        ),
        (
            "alignSelf",
            accessor(get_style_align_self, set_style_align_self),
        ),
        ("order", accessor(get_style_order, set_style_order)),
        ("rowGap", accessor(get_style_row_gap, set_style_row_gap)),
        (
            "columnGap",
            accessor(get_style_column_gap, set_style_column_gap),
        ),
        (
            "gridTemplateColumns",
            accessor(
                get_style_grid_template_columns,
                set_style_grid_template_columns,
            ),
        ),
        (
            "gridTemplateRows",
            accessor(get_style_grid_template_rows, set_style_grid_template_rows),
        ),
        (
            "transformOrigin",
            accessor(get_style_transform_origin, set_style_transform_origin),
        ),
        (
            "transition",
            accessor(get_style_transition, set_style_transition),
        ),
        (
            "animation",
            accessor(get_style_animation, set_style_animation),
        ),
        (
            "objectFit",
            accessor(get_style_object_fit, set_style_object_fit),
        ),
        (
            "userSelect",
            accessor(get_style_user_select, set_style_user_select),
        ),
        (
            "pointerEvents",
            accessor(get_style_pointer_events, set_style_pointer_events),
        ),
        (
            "borderCollapse",
            accessor(get_style_border_collapse, set_style_border_collapse),
        ),
        (
            "borderSpacing",
            accessor(get_style_border_spacing, set_style_border_spacing),
        ),
        (
            "captionSide",
            accessor(get_style_caption_side, set_style_caption_side),
        ),
    ];
    for (name, (get, set)) in extra_entries {
        object.accessor(JsString::from(name), Some(get), Some(set), Attribute::all());
    }
    for (name, function) in [
        (
            "getPropertyValue",
            NativeFunction::from_fn_ptr(style_get_property),
        ),
        (
            "setProperty",
            NativeFunction::from_fn_ptr(style_set_property),
        ),
        (
            "removeProperty",
            NativeFunction::from_fn_ptr(style_remove_property),
        ),
        (
            "getPropertyPriority",
            NativeFunction::from_fn_ptr(js_style_get_priority),
        ),
        ("item", NativeFunction::from_fn_ptr(style_item)),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    // Attach the owning node id so the native getters can work.
    let style = object.build();
    style.set(
        JsString::from(super::NODE_ID_KEY),
        JsValue::from(node.index() as f64),
        false,
        context,
    )?;
    Ok(style)
}

fn class_list_method(
    node: NodeId,
    action: &str,
    name: &str,
    _context: &mut Context,
) -> JsResult<JsValue> {
    let document = document()?;
    let mut classes: Vec<String> = document
        .borrow()
        .get_attribute(node, "class")
        .unwrap_or("")
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect();
    let result = match action {
        "add" => {
            if !classes.iter().any(|class| class == name) {
                classes.push(name.to_owned());
            }
            JsValue::undefined()
        }
        "remove" => {
            classes.retain(|class| class != name);
            JsValue::undefined()
        }
        "toggle" => {
            if let Some(position) = classes.iter().position(|class| class == name) {
                classes.remove(position);
                JsValue::from(false)
            } else {
                classes.push(name.to_owned());
                JsValue::from(true)
            }
        }
        "contains" => JsValue::from(classes.iter().any(|class| class == name)),
        _ => JsValue::undefined(),
    };
    let _ = document
        .borrow_mut()
        .set_attribute(node, "class", &classes.join(" "));
    observers::record_attribute(node, "class");
    Ok(result)
}

fn js_class_add(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let name = string_arg(args, 0, context).unwrap_or_default();
    class_list_method(node, "add", &name, context)
}
fn js_class_remove(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let name = string_arg(args, 0, context).unwrap_or_default();
    class_list_method(node, "remove", &name, context)
}
fn js_class_toggle(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let name = string_arg(args, 0, context).unwrap_or_default();
    class_list_method(node, "toggle", &name, context)
}
fn js_class_contains(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let name = string_arg(args, 0, context).unwrap_or_default();
    class_list_method(node, "contains", &name, context)
}

fn build_class_list(node: NodeId, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    for (name, function) in [
        ("add", NativeFunction::from_fn_ptr(js_class_add)),
        ("remove", NativeFunction::from_fn_ptr(js_class_remove)),
        ("toggle", NativeFunction::from_fn_ptr(js_class_toggle)),
        ("contains", NativeFunction::from_fn_ptr(js_class_contains)),
    ] {
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let class_list = object.build();
    class_list.set(
        JsString::from(super::NODE_ID_KEY),
        JsValue::from(node.index() as f64),
        false,
        context,
    )?;
    Ok(class_list)
}

fn dataset_attribute_name(key: &str) -> String {
    let mut name = String::from("data-");
    for character in key.chars() {
        if character.is_ascii_uppercase() {
            name.push('-');
            name.push(character.to_ascii_lowercase());
        } else {
            name.push(character);
        }
    }
    name
}

fn dataset_property_name(attribute: &str) -> Option<String> {
    let rest = if attribute.len() >= 5 && attribute[..5].eq_ignore_ascii_case("data-") {
        &attribute[5..]
    } else {
        return None;
    };
    if rest.is_empty() {
        return None;
    }
    let mut name = String::with_capacity(rest.len());
    let mut upper = false;
    for character in rest.chars() {
        if upper {
            name.push(character.to_ascii_uppercase());
            upper = false;
        } else if character == '-' {
            upper = true;
        } else {
            name.push(character);
        }
    }
    Some(name)
}

fn dataset_key(args: &[JsValue]) -> Option<String> {
    args.get(1)
        .and_then(JsValue::as_string)
        .map(|key| key.to_std_string_escaped())
}

fn dataset_node(args: &[JsValue], context: &mut Context) -> Option<NodeId> {
    args.first()
        .and_then(JsValue::as_object)
        .and_then(|target| node_id_from_object(&target, context))
}

fn js_dataset_get(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this;
    let Some(key) = dataset_key(args) else {
        return Ok(JsValue::undefined());
    };
    if key == super::NODE_ID_KEY {
        let Some(target) = args.first().and_then(JsValue::as_object) else {
            return Ok(JsValue::undefined());
        };
        return target.get(JsString::from(super::NODE_ID_KEY), context);
    }
    let Some(node) = dataset_node(args, context) else {
        return Ok(JsValue::undefined());
    };
    let attribute = dataset_attribute_name(&key);
    let value = {
        let document = document()?;
        let document = document.borrow();
        document.get_attribute(node, &attribute).map(str::to_owned)
    };
    Ok(value
        .map(|value| js_string_value(&value))
        .unwrap_or(JsValue::undefined()))
}

fn js_dataset_set(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this;
    let Some(key) = dataset_key(args) else {
        return Ok(JsValue::from(true));
    };
    if key == super::NODE_ID_KEY {
        return Ok(JsValue::from(true));
    }
    let Some(node) = dataset_node(args, context) else {
        return Ok(JsValue::from(false));
    };
    let value = args
        .get(2)
        .map(|value| value_to_display(value, context))
        .unwrap_or_default();
    let attribute = dataset_attribute_name(&key);
    document()?
        .borrow_mut()
        .set_attribute(node, &attribute, &value)
        .map_err(|error| host_error(error.to_string()))?;
    observers::record_attribute(node, &attribute);
    Ok(JsValue::from(true))
}

fn js_dataset_has(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this;
    let Some(key) = dataset_key(args) else {
        return Ok(JsValue::from(false));
    };
    if key == super::NODE_ID_KEY {
        return Ok(JsValue::from(true));
    }
    let Some(node) = dataset_node(args, context) else {
        return Ok(JsValue::from(false));
    };
    let attribute = dataset_attribute_name(&key);
    let present = {
        let document = document()?;
        let document = document.borrow();
        document.get_attribute(node, &attribute).is_some()
    };
    Ok(JsValue::from(present))
}

fn js_dataset_delete(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _ = this;
    let Some(key) = dataset_key(args) else {
        return Ok(JsValue::from(true));
    };
    if key == super::NODE_ID_KEY {
        return Ok(JsValue::from(false));
    }
    let Some(node) = dataset_node(args, context) else {
        return Ok(JsValue::from(false));
    };
    let attribute = dataset_attribute_name(&key);
    let _ = document()?.borrow_mut().remove_attribute(node, &attribute);
    observers::record_attribute(node, &attribute);
    Ok(JsValue::from(true))
}

fn js_dataset_own_keys(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let array = JsArray::new(context)?;
    array.push(js_string_value(super::NODE_ID_KEY), context)?;
    if let Some(node) = dataset_node(args, context) {
        let keys: Vec<String> = {
            let document = document()?;
            let document = document.borrow();
            document
                .element(node)
                .map(|element| {
                    element
                        .attributes
                        .iter()
                        .filter_map(|attribute| dataset_property_name(&attribute.name))
                        .collect()
                })
                .unwrap_or_default()
        };
        for key in keys {
            array.push(js_string_value(&key), context)?;
        }
    }
    Ok(JsValue::from(array))
}

fn descriptor_object(
    value: JsValue,
    writable: bool,
    enumerable: bool,
    configurable: bool,
    context: &mut Context,
) -> JsValue {
    let mut descriptor = ObjectInitializer::new(context);
    descriptor
        .property(JsString::from("value"), value, Attribute::all())
        .property(
            JsString::from("writable"),
            JsValue::from(writable),
            Attribute::all(),
        )
        .property(
            JsString::from("enumerable"),
            JsValue::from(enumerable),
            Attribute::all(),
        )
        .property(
            JsString::from("configurable"),
            JsValue::from(configurable),
            Attribute::all(),
        );
    JsValue::from(descriptor.build())
}

fn js_dataset_get_own_property_descriptor(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(key) = dataset_key(args) else {
        return Ok(JsValue::undefined());
    };
    if key == super::NODE_ID_KEY {
        let Some(target) = args.first().and_then(JsValue::as_object) else {
            return Ok(JsValue::undefined());
        };
        let value = target.get(JsString::from(super::NODE_ID_KEY), context)?;
        return Ok(descriptor_object(value, false, false, false, context));
    }
    let Some(node) = dataset_node(args, context) else {
        return Ok(JsValue::undefined());
    };
    let attribute = dataset_attribute_name(&key);
    let value = {
        let document = document()?;
        let document = document.borrow();
        document.get_attribute(node, &attribute).map(str::to_owned)
    };
    let Some(value) = value else {
        return Ok(JsValue::undefined());
    };
    Ok(descriptor_object(
        js_string_value(&value),
        true,
        true,
        true,
        context,
    ))
}

fn build_dataset(node: NodeId, context: &mut Context) -> JsResult<JsObject> {
    let mut target = ObjectInitializer::new(context);
    target.property(
        JsString::from(super::NODE_ID_KEY),
        JsValue::from(node.index() as f64),
        Attribute::empty(),
    );
    let target = target.build();
    let proxy = JsProxy::builder(target)
        .get(js_dataset_get)
        .set(js_dataset_set)
        .has(js_dataset_has)
        .delete_property(js_dataset_delete)
        .own_keys(js_dataset_own_keys)
        .get_own_property_descriptor(js_dataset_get_own_property_descriptor)
        .build(context)?;
    Ok(proxy.into())
}

fn get_dataset(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    Ok(JsValue::from(build_dataset(node, context)?))
}

pub(crate) fn js_document_write(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    document_write(this, args, false, context)
}

fn js_document_writeln(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    document_write(this, args, true, context)
}

fn document_write(
    this: &JsValue,
    args: &[JsValue],
    newline: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let html = string_arg(args, 0, context).unwrap_or_default();
    let parsed = parse_html(&html);
    let body = find_element(&parsed, "body");
    let document = document()?;
    let target = {
        let document_ref = document.borrow();
        if node == document_ref.document_node() {
            find_element(&document_ref, "body").unwrap_or(node)
        } else {
            node
        }
    };
    if let Some(body) = body {
        let children = parsed.children(body).to_vec();
        for child in children {
            let imported = {
                let mut document_ref = document.borrow_mut();
                import_subtree(&mut document_ref, &parsed, child)
            };
            if let Some(imported) = imported {
                let _ = document.borrow_mut().append_child(target, imported);
                observers::record_child_list(target, &[imported], &[]);
                maybe_execute_script(imported, context);
            }
        }
    }
    if newline {
        let text = document.borrow_mut().create_text("\n");
        let _ = document.borrow_mut().append_child(target, text);
        observers::record_child_list(target, &[text], &[]);
    }
    Ok(JsValue::undefined())
}

fn maybe_execute_script(node: NodeId, context: &mut Context) {
    let Ok(document) = document() else {
        return;
    };
    let (is_script, script_type, src, text, connected) = {
        let document = document.borrow();
        if document.tag_name(node) != Some("script") {
            return;
        }
        let connected = document
            .ancestors(node)
            .iter()
            .any(|&ancestor| ancestor == document.document_node());
        (
            true,
            document
                .get_attribute(node, "type")
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase(),
            document.get_attribute(node, "src").map(str::to_owned),
            document.text_content(node),
            connected,
        )
    };
    if !is_script || !connected {
        return;
    }
    if script_type == "module" {
        with_state(|state| {
            state
                .errors
                .push("module scripts are not supported yet".to_owned());
        });
        return;
    }
    if !script_type.is_empty()
        && !matches!(
            script_type.as_str(),
            "text/javascript"
                | "application/javascript"
                | "application/ecmascript"
                | "text/ecmascript"
        )
    {
        return;
    }
    let already_executed =
        with_state(|state| !state.executed_scripts.insert(node.index())).unwrap_or(true);
    if already_executed {
        return;
    }

    let source = if let Some(src) = src {
        let base = with_state(|state| state.base_url.clone());
        let client = with_state(|state| state.client.clone()).flatten();
        let Some(base) = base else {
            return;
        };
        let Ok(url) = base.join(&src) else {
            with_state(|state| state.errors.push(format!("invalid script URL: {src}")));
            return;
        };
        match super::fetch::fetch_text_sync(url, client) {
            Ok(text) => text,
            Err(error) => {
                with_state(|state| state.errors.push(format!("{src}: {error}")));
                return;
            }
        }
    } else {
        text
    };

    if let Err(error) = context.eval(Source::from_bytes(&source)) {
        with_state(|state| state.errors.push(error.to_string()));
    }
}

fn serialize_declarations(declarations: &[crate::engine::css::Declaration]) -> String {
    declarations
        .iter()
        .map(|declaration| {
            format!(
                "{}: {}{}",
                declaration.name,
                serialize_components(&declaration.value),
                if declaration.important {
                    " !important"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn serialize_components(values: &[ComponentValue]) -> String {
    let mut out = String::new();
    for value in values {
        match value {
            ComponentValue::Ident(name) => out.push_str(name),
            ComponentValue::Function { name, args } => {
                out.push_str(name);
                out.push('(');
                out.push_str(&serialize_components(args));
                out.push(')');
            }
            ComponentValue::Hash { value, .. } => {
                out.push('#');
                out.push_str(value);
            }
            ComponentValue::StringLiteral(value) => {
                out.push('"');
                out.push_str(&value.replace('"', "\\\""));
                out.push('"');
            }
            ComponentValue::Url(value) => {
                out.push_str("url(");
                out.push_str(value);
                out.push(')');
            }
            ComponentValue::Delim(value) => out.push(*value),
            ComponentValue::Number(value) => out.push_str(&format_number(*value)),
            ComponentValue::Percentage(value) => {
                out.push_str(&format_number(*value));
                out.push('%');
            }
            ComponentValue::Dimension { value, unit } => {
                out.push_str(&format_number(*value));
                out.push_str(unit);
            }
            ComponentValue::Whitespace => out.push(' '),
            ComponentValue::Colon => out.push(':'),
            ComponentValue::Comma => out.push(','),
            ComponentValue::LParen => out.push('('),
            ComponentValue::RParen => out.push(')'),
            ComponentValue::LBracket => out.push('['),
            ComponentValue::RBracket => out.push(']'),
        }
    }
    out.trim().to_owned()
}

pub(crate) fn _unused_value_display(value: &JsValue, context: &mut Context) -> String {
    value_to_display(value, context)
}
