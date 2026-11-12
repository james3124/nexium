//! DOM bindings for the JavaScript runtime.

use std::collections::HashMap;

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
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
    let inner_html = accessor(get_inner_html, Some(set_inner_html));
    let outer_html = accessor(get_outer_html, None);
    let id = accessor(get_id, Some(set_id));
    let class_name = accessor(get_class_name, Some(set_class_name));
    let tag_name = accessor(get_tag_name, None);
    let parent_node = accessor(get_parent_node, None);
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
            JsString::from("parentNode"),
            Some(parent_node.0),
            parent_node.1,
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

    let methods: [(&str, NativeFunction, usize); 25] = [
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
            "removeChild",
            NativeFunction::from_fn_ptr(js_remove_child),
            1,
        ),
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

fn get_parent_node(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let node = node_of(this, context)?;
    let parent = document()?.borrow().parent(node);
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

fn children_array(node: NodeId, elements_only: bool, context: &mut Context) -> JsResult<JsValue> {
    let ids: Vec<NodeId> = {
        let document = document()?;
        let document = document.borrow();
        document
            .children(node)
            .iter()
            .copied()
            .filter(|&child| !elements_only || document.is_element(child))
            .collect()
    };
    let array = JsArray::new(context)?;
    for id in ids {
        array.push(wrap_node(id, context)?, context)?;
    }
    Ok(JsValue::from(array))
}

fn get_children(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    children_array(node_of(this, context)?, true, context)
}

fn get_child_nodes(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    children_array(node_of(this, context)?, false, context)
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
    document()?
        .borrow_mut()
        .append_child(parent, child)
        .map_err(|error| host_error(error.to_string()))?;
    observers::record_child_list(parent, &[child], &[]);
    maybe_execute_script(child, context);
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
    document()?
        .borrow_mut()
        .insert_before(parent, child, reference)
        .map_err(|error| host_error(error.to_string()))?;
    observers::record_child_list(parent, &[child], &[]);
    maybe_execute_script(child, context);
    Ok(args.first().cloned().unwrap_or(JsValue::undefined()))
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
        return Ok(JsValue::from(JsArray::new(context)?));
    };
    let ids = document()?
        .borrow()
        .query_selector_all(root, &selector)
        .map_err(|error| host_error(error.to_string()))?;
    let array = JsArray::new(context)?;
    for id in ids {
        array.push(wrap_node(id, context)?, context)?;
    }
    Ok(JsValue::from(array))
}

fn js_get_elements_by_tag_name(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let root = node_of(this, context)?;
    let Some(tag) = string_arg(args, 0, context) else {
        return Ok(JsValue::from(JsArray::new(context)?));
    };
    let tag = tag.to_ascii_lowercase();
    let ids: Vec<NodeId> = {
        let document = document()?;
        let document = document.borrow();
        document
            .descendants(root)
            .into_iter()
            .filter(|&id| {
                tag == "*"
                    || document
                        .element(id)
                        .is_some_and(|element| element.name.eq_ignore_ascii_case(&tag))
            })
            .collect()
    };
    let array = JsArray::new(context)?;
    for id in ids {
        array.push(wrap_node(id, context)?, context)?;
    }
    Ok(JsValue::from(array))
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

fn js_style_get_property(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = string_arg(args, 0, context).unwrap_or_default();
    let name = name.to_ascii_lowercase();
    let Some(object) = this.as_object() else {
        return Ok(js_string_value(""));
    };
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
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(js_string_value(""))
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
    let declarations = inline_declaration(node, context);
    let value = declarations
        .iter()
        .find(|declaration| declaration.name.eq_ignore_ascii_case(name))
        .map(|declaration| serialize_components(&declaration.value))
        .unwrap_or_default();
    Ok(js_string_value(&value))
}

fn style_set(node: NodeId, name: &str, value: &str, context: &mut Context) -> JsResult<()> {
    let mut declarations = inline_declaration(node, context);
    if let Some(declaration) = declarations
        .iter_mut()
        .find(|declaration| declaration.name.eq_ignore_ascii_case(name))
    {
        declaration.value = parse_declaration_list(&format!("x: {value};"))
            .into_iter()
            .next()
            .map(|declaration| declaration.value)
            .unwrap_or_else(|| vec![ComponentValue::Ident(value.to_owned())]);
    } else {
        let parsed = parse_declaration_list(&format!("x: {value};"))
            .into_iter()
            .next();
        declarations.push(crate::engine::css::Declaration {
            name: name.to_owned(),
            value: parsed
                .map(|declaration| declaration.value)
                .unwrap_or_else(|| vec![ComponentValue::Ident(value.to_owned())]),
            important: false,
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
    style_set(node, &name, &value, context)?;
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
    let mut declarations = inline_declaration(node, context);
    declarations.retain(|declaration| !declaration.name.eq_ignore_ascii_case(&name));
    let css = serialize_declarations(&declarations);
    let _ = document()?.borrow_mut().set_attribute(node, "style", &css);
    observers::record_attribute(node, "style");
    Ok(JsValue::undefined())
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
                style_set(node, $css_name, &value, context)?;
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
    let mut object = ObjectInitializer::new(context);
    object.accessor(
        JsString::from("cssText"),
        Some(css_text.0),
        Some(css_text.1),
        Attribute::all(),
    );
    for (name, (get, set)) in entries {
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
