//! `MutationObserver`, `IntersectionObserver` and `ResizeObserver` support.
//!
//! Records and entries are queued while scripts run and flushed between tasks
//! by [`flush`], which is already pumped by the host event loop.
//!
//! Geometry for the intersection and resize observers is approximated from the
//! DOM bindings the engine exposes (inline `style` declarations, `width`/
//! `height` attributes and `window.innerWidth`/`innerHeight`) because the
//! JavaScript runtime does not see the layout boxes. Elements are treated as
//! flow-positioned at the origin of the viewport unless `position: absolute`
//! with `left`/`top` (or margins) is present.

use std::collections::HashMap;

use boa_engine::object::builtins::JsArray;
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction};

use super::dom::wrap_node;
use super::{
    document_rc, host_error, js_string_value, load_function, node_id_from_object, store_function,
    with_state,
};
use crate::engine::dom::{Document, NodeData, NodeId};

const OBSERVER_STORE_KEY: &str = "__nexium_observers";

// ---------------------------------------------------------------------------
// Record and entry types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum MutationKind {
    ChildList {
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
    },
    Attributes {
        name: String,
    },
    CharacterData {
        old_value: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct MutationRecord {
    pub target: NodeId,
    pub kind: MutationKind,
}

pub struct MutationObserver {
    pub callback_slot: u32,
    pub target: Option<NodeId>,
    pub child_list: bool,
    pub attributes: bool,
    pub character_data: bool,
    pub subtree: bool,
    pub attribute_filter: Option<Vec<String>>,
    pub character_data_old_value: bool,
    pub records: Vec<MutationRecord>,
    text_snapshot: HashMap<NodeId, String>,
}

/// A rectangle in approximate viewport coordinates (CSS pixels).
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn viewport(width: f64, height: f64) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width,
            height,
        }
    }

    fn area(&self) -> f64 {
        self.width.max(0.0) * self.height.max(0.0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Margins {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

impl Margins {
    fn to_css(self) -> String {
        format!(
            "{} {} {} {}",
            format_px(self.top),
            format_px(self.right),
            format_px(self.bottom),
            format_px(self.left)
        )
    }
}

fn format_px(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}px", value as i64)
    } else {
        format!("{value}px")
    }
}

pub struct IntersectionTarget {
    pub node: NodeId,
    last_ratio: Option<f64>,
    last_intersecting: bool,
    initial: bool,
}

#[derive(Clone, Debug)]
pub struct IntersectionEntry {
    pub target: NodeId,
    pub is_intersecting: bool,
    pub ratio: f64,
    pub bounding: Rect,
    pub intersection: Option<Rect>,
    pub root: Option<Rect>,
}

pub struct IntersectionObserver {
    pub callback_slot: u32,
    pub root: Option<NodeId>,
    root_margin: Margins,
    thresholds: Vec<f64>,
    pub targets: Vec<IntersectionTarget>,
    queued: Vec<IntersectionEntry>,
}

pub struct ResizeTarget {
    pub node: NodeId,
    width: f64,
    height: f64,
    initial: bool,
}

#[derive(Clone, Debug)]
pub struct ResizeEntry {
    pub target: NodeId,
    pub width: f64,
    pub height: f64,
}

pub struct ResizeObserver {
    pub callback_slot: u32,
    pub targets: Vec<ResizeTarget>,
    queued: Vec<ResizeEntry>,
}

pub enum ObserverState {
    Mutation(MutationObserver),
    Intersection(IntersectionObserver),
    Resize(ResizeObserver),
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

pub(crate) fn install_globals(context: &mut Context) -> JsResult<()> {
    let realm = context.realm().clone();
    let store = ObjectInitializer::new(context).build();
    context.register_global_property(
        JsString::from(OBSERVER_STORE_KEY),
        JsValue::from(store.clone()),
        Attribute::empty(),
    )?;
    with_state(|state| state.observer_store = Some(store));

    type Constructor = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;
    let constructors: [(&str, usize, Constructor); 3] = [
        ("MutationObserver", 1usize, js_mutation_construct),
        ("IntersectionObserver", 2usize, js_intersection_construct),
        ("ResizeObserver", 1usize, js_resize_construct),
    ];
    for (name, length, function) in constructors {
        let constructor = FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(function))
            .name(JsString::from(name))
            .length(length)
            .constructor(true)
            .build();
        context.register_global_property(
            JsString::from(name),
            JsValue::from(constructor),
            Attribute::all(),
        )?;
    }
    Ok(())
}

fn insert_observer(observer: ObserverState) -> Option<u64> {
    with_state(|state| {
        let id = state.next_observer_id;
        state.next_observer_id += 1;
        state.observers.insert(id, observer);
        id
    })
}

fn store_observer_object(id: u64, observer: &JsObject, context: &mut Context) {
    if let Some(store) = with_state(|state| state.observer_store.clone()).flatten() {
        let _ = store.set(
            JsString::from(id.to_string()),
            JsValue::from(observer.clone()),
            false,
            context,
        );
    }
}

fn observer_id(this: &JsValue, context: &mut Context) -> Option<u64> {
    this.as_object()?
        .get(JsString::from("__nexium_observer_id"), context)
        .ok()?
        .as_number()
        .map(|value| value as u64)
}

fn observer_object(id: u64, context: &mut Context) -> Option<JsValue> {
    let store = with_state(|state| state.observer_store.clone()).flatten()?;
    store.get(JsString::from(id.to_string()), context).ok()
}

fn add_methods(
    object: &mut ObjectInitializer<'_>,
    realm: &boa_engine::realm::Realm,
    methods: Vec<(&'static str, NativeFunction)>,
) {
    for (name, function) in methods {
        object.property(
            JsString::from(name),
            function.to_js_function(realm),
            Attribute::all(),
        );
    }
}

// ---------------------------------------------------------------------------
// Options helpers
// ---------------------------------------------------------------------------

fn option_flag(options: Option<&JsObject>, name: &str, context: &mut Context) -> bool {
    options
        .and_then(|options| options.get(JsString::from(name), context).ok())
        .and_then(|value| value.as_boolean())
        .unwrap_or(false)
}

fn option_string(options: Option<&JsObject>, name: &str, context: &mut Context) -> Option<String> {
    options
        .and_then(|options| options.get(JsString::from(name), context).ok())
        .filter(|value| value.is_string())
        .and_then(|value| value.as_string().map(|value| value.to_std_string_escaped()))
}

fn option_strings(
    options: Option<&JsObject>,
    name: &str,
    context: &mut Context,
) -> Option<Vec<String>> {
    let value = options?.get(JsString::from(name), context).ok()?;
    let array = JsArray::from_object(value.as_object()?.clone()).ok()?;
    let length = array.length(context).ok()?;
    let mut names = Vec::new();
    for index in 0..length {
        if let Ok(item) = array.get(index, context)
            && let Some(name) = item.as_string()
        {
            names.push(name.to_std_string_escaped());
        }
    }
    Some(names)
}

// ---------------------------------------------------------------------------
// MutationObserver
// ---------------------------------------------------------------------------

fn js_mutation_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let callback = args.first().cloned().unwrap_or(JsValue::undefined());
    if callback.as_function().is_none() {
        return Err(host_error("MutationObserver requires a callback"));
    }
    let callback_slot = store_function(&callback, context)
        .ok_or_else(|| host_error("failed to store observer callback"))?;
    let id = insert_observer(ObserverState::Mutation(MutationObserver {
        callback_slot,
        target: None,
        child_list: false,
        attributes: false,
        character_data: false,
        subtree: false,
        attribute_filter: None,
        character_data_old_value: false,
        records: Vec::new(),
        text_snapshot: HashMap::new(),
    }))
    .ok_or_else(|| host_error("runtime is not active"))?;

    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from("__nexium_observer_id"),
        JsValue::from(id as f64),
        Attribute::empty(),
    );
    add_methods(
        &mut object,
        &realm,
        vec![
            ("observe", NativeFunction::from_fn_ptr(js_mutation_observe)),
            (
                "disconnect",
                NativeFunction::from_fn_ptr(js_mutation_disconnect),
            ),
            (
                "takeRecords",
                NativeFunction::from_fn_ptr(js_mutation_take_records),
            ),
        ],
    );
    let observer = object.build();
    store_observer_object(id, &observer, context);
    Ok(JsValue::from(observer))
}

fn js_mutation_observe(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(id) = observer_id(this, context) else {
        return Ok(JsValue::undefined());
    };
    let Some(target) = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| node_id_from_object(&object, context))
    else {
        return Err(host_error("observe requires a target node"));
    };
    let options = args.get(1).and_then(JsValue::as_object);
    let child_list = option_flag(options.as_ref(), "childList", context);
    let mut attributes = option_flag(options.as_ref(), "attributes", context);
    let subtree = option_flag(options.as_ref(), "subtree", context);
    let character_data = option_flag(options.as_ref(), "characterData", context);
    let character_data_old_value = option_flag(options.as_ref(), "characterDataOldValue", context);
    let attribute_filter = option_strings(options.as_ref(), "attributeFilter", context);
    if attribute_filter.is_some() {
        // Passing an `attributeFilter` implies `attributes: true`.
        attributes = true;
    }
    let snapshot = document_rc()
        .map(|document| {
            let document = document.borrow();
            text_snapshot(&document, target, subtree)
        })
        .unwrap_or_default();
    with_state(|state| {
        if let Some(ObserverState::Mutation(observer)) = state.observers.get_mut(&id) {
            observer.target = Some(target);
            observer.child_list = child_list;
            observer.attributes = attributes;
            observer.character_data = character_data;
            observer.subtree = subtree;
            observer.attribute_filter = attribute_filter;
            observer.character_data_old_value = character_data_old_value;
            observer.text_snapshot = snapshot;
        }
    });
    Ok(JsValue::undefined())
}

fn js_mutation_disconnect(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(id) = observer_id(this, context) {
        with_state(|state| {
            if let Some(ObserverState::Mutation(observer)) = state.observers.get_mut(&id) {
                observer.target = None;
                observer.records.clear();
                observer.text_snapshot.clear();
            }
        });
    }
    Ok(JsValue::undefined())
}

fn js_mutation_take_records(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(id) = observer_id(this, context) else {
        return Ok(JsValue::from(JsArray::new(context)?));
    };
    poll_character_data();
    let records = with_state(|state| match state.observers.get_mut(&id) {
        Some(ObserverState::Mutation(observer)) => std::mem::take(&mut observer.records),
        _ => Vec::new(),
    })
    .unwrap_or_default();
    build_records(records, context)
}

pub(crate) fn record_child_list(target: NodeId, added: &[NodeId], removed: &[NodeId]) {
    push_records(target, |observer| {
        if observer.child_list {
            Some(MutationKind::ChildList {
                added: added.to_vec(),
                removed: removed.to_vec(),
            })
        } else {
            None
        }
    });
}

pub(crate) fn record_attribute(target: NodeId, name: &str) {
    push_records(target, |observer| {
        if !observer.attributes {
            return None;
        }
        if let Some(filter) = &observer.attribute_filter
            && !filter
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(name))
        {
            return None;
        }
        Some(MutationKind::Attributes {
            name: name.to_owned(),
        })
    });
}

fn push_records(target: NodeId, kind: impl Fn(&MutationObserver) -> Option<MutationKind>) {
    let Some(document) = document_rc() else {
        return;
    };
    let document_ref = document.borrow();
    with_state(|state| {
        for observer in state.observers.values_mut() {
            let ObserverState::Mutation(observer) = observer else {
                continue;
            };
            let Some(observer_target) = observer.target else {
                continue;
            };
            let matches = observer_target == target
                || (observer.subtree && document_ref.ancestors(target).contains(&observer_target));
            if !matches {
                continue;
            }
            if let Some(kind) = kind(observer) {
                observer.records.push(MutationRecord { target, kind });
            }
        }
    });
}

fn collect_text(
    document: &Document,
    target: NodeId,
    subtree: bool,
    out: &mut HashMap<NodeId, String>,
) {
    if let NodeData::Text(text) = &document.node(target).data {
        out.insert(target, text.clone());
    }
    if !subtree {
        return;
    }
    for id in document.descendants(target) {
        if let NodeData::Text(text) = &document.node(id).data {
            out.insert(id, text.clone());
        }
    }
}

fn text_snapshot(document: &Document, target: NodeId, subtree: bool) -> HashMap<NodeId, String> {
    let mut snapshot = HashMap::new();
    collect_text(document, target, subtree, &mut snapshot);
    snapshot
}

/// Detect `characterData` mutations.
///
/// The DOM bindings do not expose a `CharacterData.data` setter, so text edits
/// arrive as a text node being replaced by a new one (`element.textContent = …`,
/// `document.title = …`). Those substitutions are paired with the removed text
/// node and reported as a `characterData` record with its old value. In-place
/// changes of an existing text node are detected as well.
fn poll_character_data() {
    let Some(document) = document_rc() else {
        return;
    };
    let document_ref = document.borrow();
    with_state(|state| {
        for observer in state.observers.values_mut() {
            let ObserverState::Mutation(observer) = observer else {
                continue;
            };
            if !observer.character_data {
                continue;
            }
            let Some(target) = observer.target else {
                continue;
            };
            let mut current = HashMap::new();
            collect_text(&document_ref, target, observer.subtree, &mut current);
            let previous = std::mem::take(&mut observer.text_snapshot);
            let old_values = observer.character_data_old_value;
            // Text nodes whose data changed in place.
            for (id, text) in &current {
                let Some(old) = previous.get(id) else {
                    continue;
                };
                if old == text {
                    continue;
                }
                observer.records.push(MutationRecord {
                    target: *id,
                    kind: MutationKind::CharacterData {
                        old_value: old_values.then(|| old.clone()),
                    },
                });
            }
            // Text nodes replaced by a new text node (substitution).
            let mut added: Vec<(NodeId, String)> = current
                .iter()
                .filter(|(id, _)| !previous.contains_key(id))
                .map(|(id, text)| (*id, text.clone()))
                .collect();
            let mut removed: Vec<String> = previous
                .iter()
                .filter(|(id, _)| !current.contains_key(id))
                .map(|(_, text)| text.clone())
                .collect();
            for (id, text) in added.drain(..) {
                let Some(old) = removed.pop() else {
                    continue;
                };
                if old == text {
                    continue;
                }
                observer.records.push(MutationRecord {
                    target: id,
                    kind: MutationKind::CharacterData {
                        old_value: old_values.then_some(old),
                    },
                });
            }
            observer.text_snapshot = current;
        }
    });
}

// ---------------------------------------------------------------------------
// IntersectionObserver
// ---------------------------------------------------------------------------

fn js_intersection_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let callback = args.first().cloned().unwrap_or(JsValue::undefined());
    if callback.as_function().is_none() {
        return Err(host_error("IntersectionObserver requires a callback"));
    }
    let callback_slot = store_function(&callback, context)
        .ok_or_else(|| host_error("failed to store observer callback"))?;
    let options = args.get(1).and_then(JsValue::as_object);
    let root = options
        .as_ref()
        .and_then(|options| options.get(JsString::from("root"), context).ok())
        .and_then(|value| value.as_object())
        .and_then(|object| node_id_from_object(&object, context));
    let root_margin_text =
        option_string(options.as_ref(), "rootMargin", context).unwrap_or_default();
    let root_margin = parse_root_margin(&root_margin_text);
    let threshold_value = options
        .as_ref()
        .and_then(|options| options.get(JsString::from("threshold"), context).ok());
    let thresholds = parse_thresholds(threshold_value, context);

    let id = insert_observer(ObserverState::Intersection(IntersectionObserver {
        callback_slot,
        root,
        root_margin,
        thresholds: thresholds.clone(),
        targets: Vec::new(),
        queued: Vec::new(),
    }))
    .ok_or_else(|| host_error("runtime is not active"))?;

    let realm = context.realm().clone();
    let root_value = match root {
        Some(node) => wrap_node(node, context)?,
        None => JsValue::null(),
    };
    let threshold_array = JsArray::new(context)?;
    for threshold in &thresholds {
        threshold_array.push(JsValue::from(*threshold), context)?;
    }
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("__nexium_observer_id"),
            JsValue::from(id as f64),
            Attribute::empty(),
        )
        .property(JsString::from("root"), root_value, Attribute::all())
        .property(
            JsString::from("rootMargin"),
            js_string_value(&root_margin.to_css()),
            Attribute::all(),
        )
        .property(
            JsString::from("thresholds"),
            JsValue::from(threshold_array),
            Attribute::all(),
        );
    add_methods(
        &mut object,
        &realm,
        vec![
            (
                "observe",
                NativeFunction::from_fn_ptr(js_intersection_observe),
            ),
            (
                "unobserve",
                NativeFunction::from_fn_ptr(js_intersection_unobserve),
            ),
            (
                "disconnect",
                NativeFunction::from_fn_ptr(js_intersection_disconnect),
            ),
            (
                "takeRecords",
                NativeFunction::from_fn_ptr(js_intersection_take_records),
            ),
        ],
    );
    let observer = object.build();
    store_observer_object(id, &observer, context);
    Ok(JsValue::from(observer))
}

fn js_intersection_observe(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(id) = observer_id(this, context) else {
        return Ok(JsValue::undefined());
    };
    let Some(node) = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| node_id_from_object(&object, context))
    else {
        return Err(host_error("observe requires a target element"));
    };
    with_state(|state| {
        if let Some(ObserverState::Intersection(observer)) = state.observers.get_mut(&id)
            && !observer.targets.iter().any(|target| target.node == node)
        {
            observer.targets.push(IntersectionTarget {
                node,
                last_ratio: None,
                last_intersecting: false,
                initial: true,
            });
        }
    });
    Ok(JsValue::undefined())
}

fn js_intersection_unobserve(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(id) = observer_id(this, context) else {
        return Ok(JsValue::undefined());
    };
    let node = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| node_id_from_object(&object, context));
    with_state(|state| {
        if let Some(ObserverState::Intersection(observer)) = state.observers.get_mut(&id)
            && let Some(node) = node
        {
            observer.targets.retain(|target| target.node != node);
        }
    });
    Ok(JsValue::undefined())
}

fn js_intersection_disconnect(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(id) = observer_id(this, context) {
        with_state(|state| {
            if let Some(ObserverState::Intersection(observer)) = state.observers.get_mut(&id) {
                observer.targets.clear();
                observer.queued.clear();
            }
        });
    }
    Ok(JsValue::undefined())
}

fn js_intersection_take_records(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(id) = observer_id(this, context) else {
        return Ok(JsValue::from(JsArray::new(context)?));
    };
    refresh_intersection_for(id, context);
    let entries = with_state(|state| match state.observers.get_mut(&id) {
        Some(ObserverState::Intersection(observer)) => std::mem::take(&mut observer.queued),
        _ => Vec::new(),
    })
    .unwrap_or_default();
    build_intersection_entries(entries, context)
}

fn refresh_intersection_for(id: u64, context: &mut Context) {
    let Some(document) = document_rc() else {
        return;
    };
    let viewport = viewport_size(context);
    let document_ref = document.borrow();
    with_state(|state| {
        if let Some(ObserverState::Intersection(observer)) = state.observers.get_mut(&id) {
            refresh_intersection(observer, &document_ref, viewport);
        }
    });
}

fn refresh_intersection(
    observer: &mut IntersectionObserver,
    document: &Document,
    viewport: (f64, f64),
) {
    let root = root_rect(document, observer, viewport);
    let mut queued = Vec::new();
    for target in &mut observer.targets {
        let bounding = element_rect(document, target.node, viewport).unwrap_or_default();
        let intersection = intersect(bounding, root);
        let area = bounding.area();
        let ratio = match intersection {
            Some(rect) if area > 0.0 => (rect.area() / area).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let is_intersecting = intersection.is_some();
        let crossed = target
            .last_ratio
            .map(|previous| threshold_crossed(&observer.thresholds, previous, ratio))
            .unwrap_or(true);
        let changed = target.initial || crossed || target.last_intersecting != is_intersecting;
        target.last_ratio = Some(ratio);
        target.last_intersecting = is_intersecting;
        target.initial = false;
        if changed {
            queued.push(IntersectionEntry {
                target: target.node,
                is_intersecting,
                ratio,
                bounding,
                intersection,
                root: Some(root),
            });
        }
    }
    observer.queued.extend(queued);
}

fn threshold_crossed(thresholds: &[f64], previous: f64, current: f64) -> bool {
    thresholds
        .iter()
        .any(|threshold| (previous < *threshold) != (current < *threshold))
}

fn root_rect(document: &Document, observer: &IntersectionObserver, viewport: (f64, f64)) -> Rect {
    let mut rect = match observer.root {
        Some(root) => {
            element_rect(document, root, viewport).unwrap_or_else(|| Rect::viewport(0.0, 0.0))
        }
        None => Rect::viewport(viewport.0, viewport.1),
    };
    rect.x -= observer.root_margin.left;
    rect.y -= observer.root_margin.top;
    rect.width += observer.root_margin.left + observer.root_margin.right;
    rect.height += observer.root_margin.top + observer.root_margin.bottom;
    rect
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    if right <= left || bottom <= top {
        return None;
    }
    Some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn viewport_size(context: &mut Context) -> (f64, f64) {
    let global = context.global_object();
    let width = global
        .get(JsString::from("innerWidth"), context)
        .ok()
        .and_then(|value| value.as_number())
        .filter(|width| *width > 0.0)
        .unwrap_or(1280.0);
    let height = global
        .get(JsString::from("innerHeight"), context)
        .ok()
        .and_then(|value| value.as_number())
        .filter(|height| *height > 0.0)
        .unwrap_or(720.0);
    (width, height)
}

fn parse_thresholds(value: Option<JsValue>, context: &mut Context) -> Vec<f64> {
    let mut thresholds = Vec::new();
    if let Some(value) = value {
        if let Some(number) = value.as_number() {
            thresholds.push(number);
        } else if let Some(object) = value.as_object()
            && let Ok(array) = JsArray::from_object(object.clone())
            && let Ok(length) = array.length(context)
        {
            for index in 0..length {
                if let Ok(item) = array.get(index, context)
                    && let Some(number) = item.as_number()
                {
                    thresholds.push(number);
                }
            }
        }
    }
    thresholds.retain(|threshold| threshold.is_finite() && (0.0..=1.0).contains(threshold));
    if thresholds.is_empty() {
        thresholds.push(0.0);
    }
    thresholds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    thresholds.dedup();
    thresholds
}

fn parse_root_margin(value: &str) -> Margins {
    let values: Vec<f64> = value
        .split_whitespace()
        .take(4)
        .map(|part| parse_px(part).unwrap_or(0.0))
        .collect();
    match values.as_slice() {
        [] => Margins::default(),
        [all] => Margins {
            top: *all,
            right: *all,
            bottom: *all,
            left: *all,
        },
        [vertical, horizontal] => Margins {
            top: *vertical,
            right: *horizontal,
            bottom: *vertical,
            left: *horizontal,
        },
        [top, horizontal, bottom] => Margins {
            top: *top,
            right: *horizontal,
            bottom: *bottom,
            left: *horizontal,
        },
        [top, right, bottom, left, ..] => Margins {
            top: *top,
            right: *right,
            bottom: *bottom,
            left: *left,
        },
    }
}

fn parse_px(value: &str) -> Option<f64> {
    let value = value.trim();
    let value = value.strip_suffix("px").unwrap_or(value).trim();
    value.parse::<f64>().ok().filter(|value| value.is_finite())
}

// ---------------------------------------------------------------------------
// ResizeObserver
// ---------------------------------------------------------------------------

fn js_resize_construct(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let callback = args.first().cloned().unwrap_or(JsValue::undefined());
    if callback.as_function().is_none() {
        return Err(host_error("ResizeObserver requires a callback"));
    }
    let callback_slot = store_function(&callback, context)
        .ok_or_else(|| host_error("failed to store observer callback"))?;
    let id = insert_observer(ObserverState::Resize(ResizeObserver {
        callback_slot,
        targets: Vec::new(),
        queued: Vec::new(),
    }))
    .ok_or_else(|| host_error("runtime is not active"))?;

    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object.property(
        JsString::from("__nexium_observer_id"),
        JsValue::from(id as f64),
        Attribute::empty(),
    );
    add_methods(
        &mut object,
        &realm,
        vec![
            ("observe", NativeFunction::from_fn_ptr(js_resize_observe)),
            (
                "unobserve",
                NativeFunction::from_fn_ptr(js_resize_unobserve),
            ),
            (
                "disconnect",
                NativeFunction::from_fn_ptr(js_resize_disconnect),
            ),
            (
                "takeRecords",
                NativeFunction::from_fn_ptr(js_resize_take_records),
            ),
        ],
    );
    let observer = object.build();
    store_observer_object(id, &observer, context);
    Ok(JsValue::from(observer))
}

fn js_resize_observe(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(id) = observer_id(this, context) else {
        return Ok(JsValue::undefined());
    };
    let Some(node) = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| node_id_from_object(&object, context))
    else {
        return Err(host_error("observe requires a target element"));
    };
    with_state(|state| {
        if let Some(ObserverState::Resize(observer)) = state.observers.get_mut(&id)
            && !observer.targets.iter().any(|target| target.node == node)
        {
            observer.targets.push(ResizeTarget {
                node,
                width: 0.0,
                height: 0.0,
                initial: true,
            });
        }
    });
    Ok(JsValue::undefined())
}

fn js_resize_unobserve(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(id) = observer_id(this, context) else {
        return Ok(JsValue::undefined());
    };
    let node = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|object| node_id_from_object(&object, context));
    with_state(|state| {
        if let Some(ObserverState::Resize(observer)) = state.observers.get_mut(&id)
            && let Some(node) = node
        {
            observer.targets.retain(|target| target.node != node);
        }
    });
    Ok(JsValue::undefined())
}

fn js_resize_disconnect(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(id) = observer_id(this, context) {
        with_state(|state| {
            if let Some(ObserverState::Resize(observer)) = state.observers.get_mut(&id) {
                observer.targets.clear();
                observer.queued.clear();
            }
        });
    }
    Ok(JsValue::undefined())
}

fn js_resize_take_records(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(id) = observer_id(this, context) else {
        return Ok(JsValue::from(JsArray::new(context)?));
    };
    refresh_resize_for(id, context);
    let entries = with_state(|state| match state.observers.get_mut(&id) {
        Some(ObserverState::Resize(observer)) => std::mem::take(&mut observer.queued),
        _ => Vec::new(),
    })
    .unwrap_or_default();
    build_resize_entries(entries, context)
}

fn refresh_resize_for(id: u64, context: &mut Context) {
    let Some(document) = document_rc() else {
        return;
    };
    let viewport = viewport_size(context);
    let document_ref = document.borrow();
    with_state(|state| {
        if let Some(ObserverState::Resize(observer)) = state.observers.get_mut(&id) {
            refresh_resize(observer, &document_ref, viewport);
        }
    });
}

fn refresh_resize(observer: &mut ResizeObserver, document: &Document, viewport: (f64, f64)) {
    let mut queued = Vec::new();
    for target in &mut observer.targets {
        let rect = element_rect(document, target.node, viewport).unwrap_or_default();
        let changed = target.initial
            || (rect.width - target.width).abs() > 0.5
            || (rect.height - target.height).abs() > 0.5;
        target.width = rect.width;
        target.height = rect.height;
        target.initial = false;
        if changed {
            queued.push(ResizeEntry {
                target: target.node,
                width: rect.width,
                height: rect.height,
            });
        }
    }
    observer.queued.extend(queued);
}

// ---------------------------------------------------------------------------
// Approximate geometry
// ---------------------------------------------------------------------------

fn style_value(document: &Document, node: NodeId, property: &str) -> Option<String> {
    let style = document.get_attribute(node, "style")?;
    for declaration in style.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(property) {
            return Some(
                value
                    .trim()
                    .trim_end_matches("!important")
                    .trim()
                    .to_owned(),
            );
        }
    }
    None
}

fn length_value(value: &str, reference: f64) -> Option<f64> {
    let value = value.trim();
    if let Some(number) = value.strip_suffix("px") {
        number.trim().parse::<f64>().ok()
    } else if let Some(number) = value.strip_suffix('%') {
        number
            .trim()
            .parse::<f64>()
            .ok()
            .map(|percent| percent / 100.0 * reference)
    } else if let Some(number) = value.strip_suffix("em") {
        number.trim().parse::<f64>().ok().map(|ems| ems * 16.0)
    } else {
        value.parse::<f64>().ok()
    }
}

fn is_hidden(document: &Document, node: NodeId) -> bool {
    if document.get_attribute(node, "hidden").is_some() {
        return true;
    }
    if style_value(document, node, "display")
        .is_some_and(|value| value.eq_ignore_ascii_case("none"))
    {
        return true;
    }
    document.ancestors(node).into_iter().any(|ancestor| {
        document.get_attribute(ancestor, "hidden").is_some()
            || style_value(document, ancestor, "display")
                .is_some_and(|value| value.eq_ignore_ascii_case("none"))
    })
}

fn is_inline_tag(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "abbr"
            | "b"
            | "bdi"
            | "bdo"
            | "cite"
            | "code"
            | "em"
            | "i"
            | "kbd"
            | "label"
            | "small"
            | "span"
            | "strong"
            | "sub"
            | "sup"
            | "time"
            | "u"
            | "var"
    )
}

fn default_width(document: &Document, node: NodeId, tag: &str, viewport: f64) -> f64 {
    match tag {
        tag if is_inline_tag(tag) => {
            let characters = document.text_content(node).chars().count() as f64;
            (characters * 8.0).clamp(8.0, viewport)
        }
        "input" | "button" | "select" | "textarea" => 160.0_f64.min(viewport),
        "img" | "canvas" | "svg" | "video" | "iframe" => 300.0_f64.min(viewport),
        _ => viewport,
    }
}

fn default_height(tag: &str) -> f64 {
    match tag {
        tag if is_inline_tag(tag) => 16.0,
        "input" | "button" | "select" => 24.0,
        "img" | "canvas" | "svg" | "video" | "iframe" => 150.0,
        "textarea" => 64.0,
        _ => 20.0,
    }
}

fn element_rect(document: &Document, node: NodeId, viewport: (f64, f64)) -> Option<Rect> {
    let tag = document.tag_name(node)?;
    if is_hidden(document, node) {
        return None;
    }
    let width = style_value(document, node, "width")
        .and_then(|value| length_value(&value, viewport.0))
        .or_else(|| {
            document
                .get_attribute(node, "width")
                .and_then(|value| length_value(value, viewport.0))
        })
        .unwrap_or_else(|| default_width(document, node, tag, viewport.0));
    let height = style_value(document, node, "height")
        .and_then(|value| length_value(&value, viewport.1))
        .or_else(|| {
            document
                .get_attribute(node, "height")
                .and_then(|value| length_value(value, viewport.1))
        })
        .unwrap_or_else(|| default_height(tag));
    let offset = |property: &str, reference: f64| {
        style_value(document, node, property)
            .and_then(|value| length_value(&value, reference))
            .unwrap_or(0.0)
    };
    let positioned = style_value(document, node, "position").is_some_and(|value| {
        value.eq_ignore_ascii_case("absolute") || value.eq_ignore_ascii_case("fixed")
    });
    let x = if positioned {
        offset("left", viewport.0)
    } else {
        offset("margin-left", viewport.0)
    };
    let y = if positioned {
        offset("top", viewport.1)
    } else {
        offset("margin-top", viewport.1)
    };
    Some(Rect {
        x,
        y,
        width: width.max(0.0),
        height: height.max(0.0),
    })
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// Flush queued observer records and entries. Idempotent: calling it again
/// without intervening changes delivers nothing.
pub(crate) fn flush(context: &mut Context) {
    let mut rounds = 0usize;
    loop {
        rounds += 1;
        if rounds > 100 {
            with_state(|state| {
                state
                    .errors
                    .push("MutationObserver delivery limit exceeded".to_owned())
            });
            break;
        }
        poll_character_data();
        let mutations = deliver_mutations(context);
        let intersections = pump_intersection(context);
        let resizes = pump_resize(context);
        if !(mutations || intersections || resizes) {
            break;
        }
    }
}

fn deliver_mutations(context: &mut Context) -> bool {
    let batches: Vec<(u64, u32, Vec<MutationRecord>)> = with_state(|state| {
        let mut batches = Vec::new();
        for (id, observer) in state.observers.iter_mut() {
            let ObserverState::Mutation(observer) = observer else {
                continue;
            };
            if observer.records.is_empty() {
                continue;
            }
            batches.push((
                *id,
                observer.callback_slot,
                std::mem::take(&mut observer.records),
            ));
        }
        batches
    })
    .unwrap_or_default();
    if batches.is_empty() {
        return false;
    }
    for (id, slot, records) in batches {
        let Some(function) = load_function(slot, context) else {
            continue;
        };
        let Some(observer_value) = observer_object(id, context) else {
            continue;
        };
        let Ok(array) = build_records(records, context) else {
            continue;
        };
        if let Some(function) = function.as_function()
            && let Err(error) =
                function.call(&JsValue::undefined(), &[array, observer_value], context)
        {
            with_state(|state| state.errors.push(error.to_string()));
        }
    }
    true
}

fn has_intersection_observers() -> bool {
    with_state(|state| {
        state
            .observers
            .values()
            .any(|observer| matches!(observer, ObserverState::Intersection(_)))
    })
    .unwrap_or(false)
}

fn has_resize_observers() -> bool {
    with_state(|state| {
        state
            .observers
            .values()
            .any(|observer| matches!(observer, ObserverState::Resize(_)))
    })
    .unwrap_or(false)
}

fn pump_intersection(context: &mut Context) -> bool {
    if !has_intersection_observers() {
        return false;
    }
    if let Some(document) = document_rc() {
        let viewport = viewport_size(context);
        let document_ref = document.borrow();
        with_state(|state| {
            for observer in state.observers.values_mut() {
                if let ObserverState::Intersection(observer) = observer {
                    refresh_intersection(observer, &document_ref, viewport);
                }
            }
        });
    }
    let jobs: Vec<(u64, u32, Vec<IntersectionEntry>)> = with_state(|state| {
        let mut jobs = Vec::new();
        for (id, observer) in state.observers.iter_mut() {
            let ObserverState::Intersection(observer) = observer else {
                continue;
            };
            if observer.queued.is_empty() {
                continue;
            }
            jobs.push((
                *id,
                observer.callback_slot,
                std::mem::take(&mut observer.queued),
            ));
        }
        jobs
    })
    .unwrap_or_default();
    if jobs.is_empty() {
        return false;
    }
    for (id, slot, entries) in jobs {
        let Some(function) = load_function(slot, context) else {
            continue;
        };
        let Some(observer_value) = observer_object(id, context) else {
            continue;
        };
        let Ok(array) = build_intersection_entries(entries, context) else {
            continue;
        };
        if let Some(function) = function.as_function()
            && let Err(error) =
                function.call(&JsValue::undefined(), &[array, observer_value], context)
        {
            with_state(|state| state.errors.push(error.to_string()));
        }
    }
    true
}

fn pump_resize(context: &mut Context) -> bool {
    if !has_resize_observers() {
        return false;
    }
    if let Some(document) = document_rc() {
        let viewport = viewport_size(context);
        let document_ref = document.borrow();
        with_state(|state| {
            for observer in state.observers.values_mut() {
                if let ObserverState::Resize(observer) = observer {
                    refresh_resize(observer, &document_ref, viewport);
                }
            }
        });
    }
    let jobs: Vec<(u64, u32, Vec<ResizeEntry>)> = with_state(|state| {
        let mut jobs = Vec::new();
        for (id, observer) in state.observers.iter_mut() {
            let ObserverState::Resize(observer) = observer else {
                continue;
            };
            if observer.queued.is_empty() {
                continue;
            }
            jobs.push((
                *id,
                observer.callback_slot,
                std::mem::take(&mut observer.queued),
            ));
        }
        jobs
    })
    .unwrap_or_default();
    if jobs.is_empty() {
        return false;
    }
    for (id, slot, entries) in jobs {
        let Some(function) = load_function(slot, context) else {
            continue;
        };
        let Some(observer_value) = observer_object(id, context) else {
            continue;
        };
        let Ok(array) = build_resize_entries(entries, context) else {
            continue;
        };
        if let Some(function) = function.as_function()
            && let Err(error) =
                function.call(&JsValue::undefined(), &[array, observer_value], context)
        {
            with_state(|state| state.errors.push(error.to_string()));
        }
    }
    true
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0)
}

fn build_rect(rect: Rect, context: &mut Context) -> JsResult<JsValue> {
    let mut object = ObjectInitializer::new(context);
    for (name, value) in [
        ("x", rect.x),
        ("y", rect.y),
        ("width", rect.width),
        ("height", rect.height),
        ("top", rect.y),
        ("left", rect.x),
        ("right", rect.x + rect.width),
        ("bottom", rect.y + rect.height),
    ] {
        object.property(JsString::from(name), JsValue::from(value), Attribute::all());
    }
    Ok(JsValue::from(object.build()))
}

fn build_intersection_entries(
    entries: Vec<IntersectionEntry>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let array = JsArray::new(context)?;
    for entry in entries {
        let target = wrap_node(entry.target, context)?;
        let bounding = build_rect(entry.bounding, context)?;
        let intersection = build_rect(entry.intersection.unwrap_or_default(), context)?;
        let root = build_rect(entry.root.unwrap_or_default(), context)?;
        let mut object = ObjectInitializer::new(context);
        object
            .property(JsString::from("target"), target, Attribute::all())
            .property(
                JsString::from("isIntersecting"),
                JsValue::from(entry.is_intersecting),
                Attribute::all(),
            )
            .property(
                JsString::from("isVisible"),
                JsValue::from(entry.is_intersecting),
                Attribute::all(),
            )
            .property(
                JsString::from("intersectionRatio"),
                JsValue::from(entry.ratio),
                Attribute::all(),
            )
            .property(
                JsString::from("boundingClientRect"),
                bounding,
                Attribute::all(),
            )
            .property(
                JsString::from("intersectionRect"),
                intersection,
                Attribute::all(),
            )
            .property(JsString::from("rootBounds"), root, Attribute::all())
            .property(
                JsString::from("time"),
                JsValue::from(now_ms()),
                Attribute::all(),
            );
        array.push(JsValue::from(object.build()), context)?;
    }
    Ok(JsValue::from(array))
}

fn build_resize_entries(entries: Vec<ResizeEntry>, context: &mut Context) -> JsResult<JsValue> {
    let array = JsArray::new(context)?;
    for entry in entries {
        let target = wrap_node(entry.target, context)?;
        let rect = build_rect(
            Rect {
                x: 0.0,
                y: 0.0,
                width: entry.width,
                height: entry.height,
            },
            context,
        )?;
        let mut size = ObjectInitializer::new(context);
        size.property(
            JsString::from("inlineSize"),
            JsValue::from(entry.width),
            Attribute::all(),
        )
        .property(
            JsString::from("blockSize"),
            JsValue::from(entry.height),
            Attribute::all(),
        );
        let size = size.build();
        let border_box = JsArray::new(context)?;
        border_box.push(JsValue::from(size.clone()), context)?;
        let content_box = JsArray::new(context)?;
        content_box.push(JsValue::from(size), context)?;
        let mut object = ObjectInitializer::new(context);
        object
            .property(JsString::from("target"), target, Attribute::all())
            .property(JsString::from("contentRect"), rect, Attribute::all())
            .property(
                JsString::from("borderBoxSize"),
                JsValue::from(border_box),
                Attribute::all(),
            )
            .property(
                JsString::from("contentBoxSize"),
                JsValue::from(content_box),
                Attribute::all(),
            );
        array.push(JsValue::from(object.build()), context)?;
    }
    Ok(JsValue::from(array))
}

fn build_records(records: Vec<MutationRecord>, context: &mut Context) -> JsResult<JsValue> {
    let array = JsArray::new(context)?;
    for record in records {
        let target = wrap_node(record.target, context)?;
        match record.kind {
            MutationKind::ChildList { added, removed } => {
                let added_array = JsArray::new(context)?;
                for node in added {
                    added_array.push(wrap_node(node, context)?, context)?;
                }
                let removed_array = JsArray::new(context)?;
                for node in removed {
                    removed_array.push(wrap_node(node, context)?, context)?;
                }
                let mut object = ObjectInitializer::new(context);
                object
                    .property(JsString::from("target"), target, Attribute::all())
                    .property(
                        JsString::from("type"),
                        js_string_value("childList"),
                        Attribute::all(),
                    )
                    .property(
                        JsString::from("addedNodes"),
                        JsValue::from(added_array),
                        Attribute::all(),
                    )
                    .property(
                        JsString::from("removedNodes"),
                        JsValue::from(removed_array),
                        Attribute::all(),
                    );
                array.push(JsValue::from(object.build()), context)?;
            }
            MutationKind::Attributes { name } => {
                let mut object = ObjectInitializer::new(context);
                object
                    .property(JsString::from("target"), target, Attribute::all())
                    .property(
                        JsString::from("type"),
                        js_string_value("attributes"),
                        Attribute::all(),
                    )
                    .property(
                        JsString::from("attributeName"),
                        js_string_value(&name),
                        Attribute::all(),
                    );
                array.push(JsValue::from(object.build()), context)?;
            }
            MutationKind::CharacterData { old_value } => {
                let mut object = ObjectInitializer::new(context);
                object
                    .property(JsString::from("target"), target, Attribute::all())
                    .property(
                        JsString::from("type"),
                        js_string_value("characterData"),
                        Attribute::all(),
                    );
                if let Some(old_value) = old_value {
                    object.property(
                        JsString::from("oldValue"),
                        js_string_value(&old_value),
                        Attribute::all(),
                    );
                }
                array.push(JsValue::from(object.build()), context)?;
            }
        }
    }
    Ok(JsValue::from(array))
}
