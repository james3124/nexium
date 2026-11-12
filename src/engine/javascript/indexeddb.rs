//! A focused IndexedDB subset persisted per profile and origin: open/upgrade,
//! object stores, transactions, get/put/delete/getAll/getKey/count, key
//! ranges (`IDBKeyRange`), autoIncrement key generators, secondary indexes
//! (including unique constraints), cursors with direction control, and
//! compound (array) keys.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{host_error, js_string_value, string_arg, with_state};
use crate::storage::{read_locked, write_atomic};

const REQUEST_STORE_KEY: &str = "__nexium_idb_requests";
const TX_STORE_KEY: &str = "__nexium_idb_txs";
const DB_NAME_KEY: &str = "__nexium_idb_name";
const STORE_NAME_KEY: &str = "__nexium_idb_store";
const INDEX_NAME_KEY: &str = "__nexium_idb_index";
const TX_ID_KEY: &str = "__nexium_idb_tx";
const RANGE_KEY: &str = "__nexium_idb_range";
const CURSOR_MARK_KEY: &str = "__nexium_idb_cursor_mark";
const CURSOR_REQUEST_KEY: &str = "__nexium_idb_cursor_request";
const CURSOR_DIRECTION_KEY: &str = "__nexium_idb_cursor_direction";
const CURSOR_PRIMARY_KEY: &str = "__nexium_idb_cursor_primary";
const CURSOR_KEYS_ONLY_KEY: &str = "__nexium_idb_cursor_keys_only";

/// Marker prefix for canonical (typed) key encodings. Legacy databases stored
/// plain string keys such as `k1`; anything without the prefix is decoded as a
/// string key so old files keep loading.
const KEY_PREFIX: char = '\u{1}';

type Method = (
    &'static str,
    fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
);

// ---------------------------------------------------------------------------
// Keys and key ranges
// ---------------------------------------------------------------------------

/// A normalised IndexedDB key. Arrays are compound keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Key {
    Number(f64),
    String(String),
    Array(Vec<Key>),
}

impl Key {
    fn rank(&self) -> u8 {
        match self {
            Key::Number(_) => 0,
            Key::String(_) => 1,
            Key::Array(_) => 2,
        }
    }
}

fn key_encode(key: &Key) -> String {
    match serde_json::to_string(key) {
        Ok(text) => format!("{KEY_PREFIX}{text}"),
        Err(_) => format!("{KEY_PREFIX}{{\"String\":\"\"}}"),
    }
}

fn key_decode(raw: &str) -> Key {
    if let Some(rest) = raw.strip_prefix(KEY_PREFIX) {
        if let Ok(key) = serde_json::from_str::<Key>(rest) {
            return key;
        }
        return Key::String(rest.to_owned());
    }
    Key::String(raw.to_owned())
}

fn key_compare(left: &Key, right: &Key) -> Ordering {
    match (left, right) {
        (Key::Number(left), Key::Number(right)) => {
            left.partial_cmp(right).unwrap_or(Ordering::Equal)
        }
        (Key::String(left), Key::String(right)) => left.cmp(right),
        (Key::Array(left), Key::Array(right)) => {
            for (left, right) in left.iter().zip(right.iter()) {
                let ordering = key_compare(left, right);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            left.len().cmp(&right.len())
        }
        _ => left.rank().cmp(&right.rank()),
    }
}

fn key_to_json(key: &Key) -> Value {
    match key {
        Key::Number(number) => serde_json::Number::from_f64(*number)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Key::String(string) => Value::String(string.clone()),
        Key::Array(keys) => Value::Array(keys.iter().map(key_to_json).collect()),
    }
}

fn key_to_js(key: &Key, context: &mut Context) -> JsValue {
    json_to_js(&key_to_json(key), context)
}

fn json_value_to_key(value: &Value) -> Option<Key> {
    match value {
        Value::Number(number) => number.as_f64().map(Key::Number),
        Value::String(string) => Some(Key::String(string.clone())),
        Value::Bool(boolean) => Some(Key::Number(if *boolean { 1.0 } else { 0.0 })),
        Value::Array(values) => {
            let mut keys = Vec::with_capacity(values.len());
            for value in values {
                keys.push(json_value_to_key(value)?);
            }
            Some(Key::Array(keys))
        }
        _ => None,
    }
}

fn js_key_from_value(value: &JsValue, context: &mut Context, depth: usize) -> Option<Key> {
    if depth > 16 {
        return None;
    }
    if let Some(number) = value.as_number() {
        return Some(Key::Number(number));
    }
    if let Some(boolean) = value.as_boolean() {
        return Some(Key::Number(if boolean { 1.0 } else { 0.0 }));
    }
    if let Some(string) = value.as_string() {
        return Some(Key::String(string.to_std_string_escaped()));
    }
    let object = value.as_object()?;
    if let Ok(array) = JsArray::from_object(object.clone()) {
        let length = array.length(context).ok()?;
        let mut keys = Vec::with_capacity(length as usize);
        for index in 0..length {
            let item = array.get(index, context).ok()?;
            keys.push(js_key_from_value(&item, context, depth + 1)?);
        }
        return Some(Key::Array(keys));
    }
    None
}

/// A half-open/closed key range, serialisable so it can travel through hidden
/// JS properties and the task queue.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeyRange {
    #[serde(default)]
    lower: Option<(Key, bool)>,
    #[serde(default)]
    upper: Option<(Key, bool)>,
}

impl KeyRange {
    fn includes(&self, key: &Key) -> bool {
        if let Some((lower, open)) = &self.lower {
            let ordering = key_compare(key, lower);
            if ordering == Ordering::Less || (*open && ordering == Ordering::Equal) {
                return false;
            }
        }
        if let Some((upper, open)) = &self.upper {
            let ordering = key_compare(key, upper);
            if ordering == Ordering::Greater || (*open && ordering == Ordering::Equal) {
                return false;
            }
        }
        true
    }

    fn only(key: Key) -> Self {
        KeyRange {
            lower: Some((key.clone(), false)),
            upper: Some((key, false)),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Query {
    Key(Key),
    Range(KeyRange),
}

impl Query {
    fn matches(&self, key: &Key) -> bool {
        match self {
            Query::Key(target) => key_compare(key, target) == Ordering::Equal,
            Query::Range(range) => range.includes(key),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Next,
    NextUnique,
    Prev,
    PrevUnique,
}

impl Direction {
    fn parse(value: Option<String>) -> Self {
        match value.as_deref() {
            Some("prev") => Direction::Prev,
            Some("nextunique") => Direction::NextUnique,
            Some("prevunique") => Direction::PrevUnique,
            _ => Direction::Next,
        }
    }

    fn reversed(self) -> bool {
        matches!(self, Direction::Prev | Direction::PrevUnique)
    }

    fn unique(self) -> bool {
        matches!(self, Direction::NextUnique | Direction::PrevUnique)
    }

    fn as_str(self) -> &'static str {
        match self {
            Direction::Next => "next",
            Direction::NextUnique => "nextunique",
            Direction::Prev => "prev",
            Direction::PrevUnique => "prevunique",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum QueryKind {
    Get,
    GetKey,
    GetAll,
    GetAllKeys,
    Count,
}

// ---------------------------------------------------------------------------
// State and operations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoreMeta {
    #[serde(default)]
    key_path: Option<String>,
    #[serde(default)]
    auto_increment: bool,
    #[serde(default)]
    generator: f64,
    #[serde(default)]
    indexes: HashMap<String, IndexMeta>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct IndexMeta {
    #[serde(default)]
    key_path: Vec<String>,
    #[serde(default)]
    unique: bool,
}

#[derive(Clone, Debug, Default)]
pub struct IdbDatabase {
    pub version: u64,
    pub stores: HashMap<String, HashMap<String, Value>>,
    pub meta: HashMap<String, StoreMeta>,
}

pub struct IdbTransaction {
    pub db: String,
    pub pending: usize,
    pub completed: bool,
}

#[derive(Clone)]
pub enum IdbOperation {
    Open {
        db: String,
        version: u64,
    },
    DeleteDatabase {
        db: String,
    },
    Put {
        db: String,
        store: String,
        key: Option<Key>,
        value: Value,
    },
    Get {
        db: String,
        store: String,
        query: Query,
    },
    GetKey {
        db: String,
        store: String,
        query: Query,
    },
    GetAll {
        db: String,
        store: String,
        query: Option<Query>,
        count: Option<usize>,
    },
    Delete {
        db: String,
        store: String,
        query: Query,
    },
    Clear {
        db: String,
        store: String,
    },
    Count {
        db: String,
        store: String,
        query: Option<Query>,
    },
    CreateIndex {
        db: String,
        store: String,
        index: String,
        key_path: Vec<String>,
        unique: bool,
    },
    DeleteIndex {
        db: String,
        store: String,
        index: String,
    },
    IndexQuery {
        db: String,
        store: String,
        index: String,
        kind: QueryKind,
        query: Option<Query>,
        count: Option<usize>,
    },
    Cursor {
        db: String,
        store: String,
        index: Option<String>,
        range: Option<KeyRange>,
        direction: Direction,
        cursor: JsObject,
        advance: usize,
        initial: bool,
    },
}

pub struct IdbTask {
    pub request: u64,
    pub tx: Option<u64>,
    pub operation: IdbOperation,
}

#[derive(Serialize, Deserialize)]
struct PersistedDb {
    #[serde(default)]
    version: u64,
    #[serde(default)]
    stores: HashMap<String, HashMap<String, Value>>,
    /// Per-store metadata (key paths, autoIncrement generators, indexes).
    /// Absent in databases written before indexes/autoIncrement existed.
    #[serde(default)]
    meta: HashMap<String, StoreMeta>,
}

struct CursorRow {
    index_key: Option<Key>,
    key: Key,
    value: Value,
}

/// The cursor's last-returned position, recorded by value rather than by list
/// index so deletions or insertions elsewhere cannot make it skip records.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CursorMark {
    #[serde(default)]
    index_key: Option<Key>,
    key: Key,
}

fn cursor_order(row: &CursorRow, mark: &CursorMark) -> Ordering {
    match (&row.index_key, &mark.index_key) {
        (Some(index_key), Some(mark_index)) => {
            key_compare(index_key, mark_index).then_with(|| key_compare(&row.key, &mark.key))
        }
        (None, None) => key_compare(&row.key, &mark.key),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
    }
}

struct CursorConfig {
    db: String,
    store: String,
    index: Option<String>,
    range: Option<KeyRange>,
    direction: Direction,
    request: u64,
    tx: Option<u64>,
    keys_only: bool,
}

struct Outcome {
    success: bool,
    result: Option<Value>,
    result_js: Option<JsValue>,
    error: Option<String>,
    persist: Option<String>,
}

fn success_json(value: Option<Value>) -> Outcome {
    Outcome {
        success: true,
        result: value,
        result_js: None,
        error: None,
        persist: None,
    }
}

fn success_js(value: JsValue) -> Outcome {
    Outcome {
        success: true,
        result: None,
        result_js: Some(value),
        error: None,
        persist: None,
    }
}

fn failure(message: &str) -> Outcome {
    Outcome {
        success: false,
        result: None,
        result_js: None,
        error: Some(message.to_owned()),
        persist: None,
    }
}

// ---------------------------------------------------------------------------
// Globals
// ---------------------------------------------------------------------------

pub(crate) fn install_globals(context: &mut Context) -> JsResult<()> {
    let requests = ObjectInitializer::new(context).build();
    context.register_global_property(
        JsString::from(REQUEST_STORE_KEY),
        JsValue::from(requests.clone()),
        Attribute::empty(),
    )?;
    let txs = ObjectInitializer::new(context).build();
    context.register_global_property(
        JsString::from(TX_STORE_KEY),
        JsValue::from(txs.clone()),
        Attribute::empty(),
    )?;
    with_state(|state| {
        state.idb_request_store = Some(requests);
        state.idb_tx_store = Some(txs);
    });

    let realm = context.realm().clone();
    let mut indexed_db = ObjectInitializer::new(context);
    let methods: [Method; 2] = [
        ("open", js_idb_open),
        ("deleteDatabase", js_idb_delete_database),
    ];
    for (name, function) in methods {
        let function = NativeFunction::from_fn_ptr(function);
        indexed_db.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let indexed_db = indexed_db.build();
    context.register_global_property(
        JsString::from("indexedDB"),
        JsValue::from(indexed_db),
        Attribute::all(),
    )?;

    let realm = context.realm().clone();
    let mut key_range = ObjectInitializer::new(context);
    let range_methods: [Method; 4] = [
        ("only", js_key_range_only),
        ("lowerBound", js_key_range_lower_bound),
        ("upperBound", js_key_range_upper_bound),
        ("bound", js_key_range_bound),
    ];
    for (name, function) in range_methods {
        let function = NativeFunction::from_fn_ptr(function);
        key_range.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let key_range = key_range.build();
    context.register_global_property(
        JsString::from("IDBKeyRange"),
        JsValue::from(key_range),
        Attribute::all(),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn next_id() -> Option<u64> {
    with_state(|state| {
        let id = state.idb_next_id;
        state.idb_next_id += 1;
        id
    })
}

fn new_request(context: &mut Context) -> JsResult<(u64, JsObject)> {
    let id = next_id().ok_or_else(|| host_error("IndexedDB runtime is not active"))?;
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from("__nexium_idb_id"),
            JsValue::from(id as f64),
            Attribute::empty(),
        )
        .property(JsString::from("result"), JsValue::null(), Attribute::all())
        .property(JsString::from("error"), JsValue::null(), Attribute::all())
        .property(
            JsString::from("readyState"),
            js_string_value("pending"),
            Attribute::all(),
        )
        .property(
            JsString::from("onsuccess"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(JsString::from("onerror"), JsValue::null(), Attribute::all())
        .property(
            JsString::from("onupgradeneeded"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(
            JsString::from("onblocked"),
            JsValue::null(),
            Attribute::all(),
        );
    let request = object.build();
    if let Some(store) = with_state(|state| state.idb_request_store.clone()).flatten() {
        store.set(
            JsString::from(id.to_string()),
            JsValue::from(request.clone()),
            false,
            context,
        )?;
    }
    Ok((id, request))
}

fn push_task(task: IdbTask) {
    with_state(|state| state.idb_tasks.push_back(task));
}

fn bump_pending(tx: Option<u64>) {
    let Some(tx) = tx else {
        return;
    };
    with_state(|state| {
        if let Some(transaction) = state.idb_txs.get_mut(&tx) {
            transaction.pending += 1;
        }
    });
}

fn db_name(this: &JsValue, context: &mut Context) -> Option<String> {
    this.as_object()?
        .get(JsString::from(DB_NAME_KEY), context)
        .ok()?
        .as_string()
        .map(|value| value.to_std_string_escaped())
}

fn store_name(this: &JsValue, context: &mut Context) -> Option<String> {
    this.as_object()?
        .get(JsString::from(STORE_NAME_KEY), context)
        .ok()?
        .as_string()
        .map(|value| value.to_std_string_escaped())
}

fn index_name(this: &JsValue, context: &mut Context) -> Option<String> {
    this.as_object()?
        .get(JsString::from(INDEX_NAME_KEY), context)
        .ok()?
        .as_string()
        .map(|value| value.to_std_string_escaped())
}

fn tx_id(this: &JsValue, context: &mut Context) -> Option<u64> {
    this.as_object()?
        .get(JsString::from(TX_ID_KEY), context)
        .ok()?
        .as_number()
        .map(|value| value as u64)
}

fn range_from_value(value: &JsValue, context: &mut Context) -> Option<KeyRange> {
    let object = value.as_object()?;
    let text = object
        .get(JsString::from(RANGE_KEY), context)
        .ok()?
        .as_string()?
        .to_std_string_escaped();
    serde_json::from_str(&text).ok()
}

fn parse_query(value: &JsValue, context: &mut Context) -> JsResult<Query> {
    if let Some(range) = range_from_value(value, context) {
        return Ok(Query::Range(range));
    }
    match js_key_from_value(value, context, 0) {
        Some(key) => Ok(Query::Key(key)),
        None => Err(host_error(
            "DataError: value is not a valid key or key range",
        )),
    }
}

fn parse_optional_query(
    args: &[JsValue],
    index: usize,
    context: &mut Context,
) -> JsResult<Option<Query>> {
    match args.get(index) {
        Some(value) if !value.is_null_or_undefined() => Ok(Some(parse_query(value, context)?)),
        _ => Ok(None),
    }
}

fn parse_count(value: &JsValue) -> Option<usize> {
    value
        .as_number()
        .map(|number| if number <= 0.0 { 0 } else { number as usize })
}

fn key_paths_from_value(value: &JsValue, context: &mut Context) -> JsResult<Vec<String>> {
    if let Some(string) = value.as_string() {
        return Ok(vec![string.to_std_string_escaped()]);
    }
    if let Some(object) = value.as_object()
        && let Ok(array) = JsArray::from_object(object.clone())
    {
        let length = array.length(context)?;
        let mut paths = Vec::with_capacity(length as usize);
        for index in 0..length {
            let item = array.get(index, context)?;
            let Some(path) = item.as_string() else {
                return Err(host_error("createIndex keyPath array must contain strings"));
            };
            paths.push(path.to_std_string_escaped());
        }
        if paths.is_empty() {
            return Err(host_error("createIndex requires a keyPath"));
        }
        return Ok(paths);
    }
    Err(host_error(
        "createIndex keyPath must be a string or an array of strings",
    ))
}

fn key_paths_to_js(paths: &[String], context: &mut Context) -> JsResult<JsValue> {
    if paths.len() == 1 {
        return Ok(js_string_value(&paths[0]));
    }
    let array = JsArray::new(context)?;
    for path in paths {
        array.push(js_string_value(path), context)?;
    }
    Ok(JsValue::from(array))
}

// ---------------------------------------------------------------------------
// IndexedDB / IDBKeyRange entry points
// ---------------------------------------------------------------------------

fn build_range(range: &KeyRange, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let lower = range
        .lower
        .as_ref()
        .map(|(key, _)| key_to_js(key, context))
        .unwrap_or_else(JsValue::undefined);
    let upper = range
        .upper
        .as_ref()
        .map(|(key, _)| key_to_js(key, context))
        .unwrap_or_else(JsValue::undefined);
    let lower_open = range.lower.as_ref().map(|(_, open)| *open).unwrap_or(false);
    let upper_open = range.upper.as_ref().map(|(_, open)| *open).unwrap_or(false);
    let text = serde_json::to_string(range).unwrap_or_else(|_| "{}".to_owned());
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(RANGE_KEY),
            js_string_value(&text),
            Attribute::empty(),
        )
        .property(JsString::from("lower"), lower, Attribute::all())
        .property(JsString::from("upper"), upper, Attribute::all())
        .property(
            JsString::from("lowerOpen"),
            JsValue::from(lower_open),
            Attribute::all(),
        )
        .property(
            JsString::from("upperOpen"),
            JsValue::from(upper_open),
            Attribute::all(),
        );
    let function = NativeFunction::from_fn_ptr(js_range_includes);
    object.property(
        JsString::from("includes"),
        function.to_js_function(&realm),
        Attribute::all(),
    );
    Ok(object.build())
}

fn js_range_includes(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(range) = range_from_value(this, context) else {
        return Ok(JsValue::from(false));
    };
    let Some(key) = args
        .first()
        .and_then(|value| js_key_from_value(value, context, 0))
    else {
        return Ok(JsValue::from(false));
    };
    Ok(JsValue::from(range.includes(&key)))
}

fn js_key_range_only(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(key) = args
        .first()
        .and_then(|value| js_key_from_value(value, context, 0))
    else {
        return Err(host_error("IDBKeyRange.only requires a key"));
    };
    Ok(JsValue::from(build_range(&KeyRange::only(key), context)?))
}

fn js_key_range_lower_bound(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(key) = args
        .first()
        .and_then(|value| js_key_from_value(value, context, 0))
    else {
        return Err(host_error("IDBKeyRange.lowerBound requires a key"));
    };
    let open = args.get(1).and_then(JsValue::as_boolean).unwrap_or(false);
    let range = KeyRange {
        lower: Some((key, open)),
        upper: None,
    };
    Ok(JsValue::from(build_range(&range, context)?))
}

fn js_key_range_upper_bound(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(key) = args
        .first()
        .and_then(|value| js_key_from_value(value, context, 0))
    else {
        return Err(host_error("IDBKeyRange.upperBound requires a key"));
    };
    let open = args.get(1).and_then(JsValue::as_boolean).unwrap_or(false);
    let range = KeyRange {
        lower: None,
        upper: Some((key, open)),
    };
    Ok(JsValue::from(build_range(&range, context)?))
}

fn js_key_range_bound(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(lower) = args
        .first()
        .and_then(|value| js_key_from_value(value, context, 0))
    else {
        return Err(host_error("IDBKeyRange.bound requires a lower key"));
    };
    let Some(upper) = args
        .get(1)
        .and_then(|value| js_key_from_value(value, context, 0))
    else {
        return Err(host_error("IDBKeyRange.bound requires an upper key"));
    };
    if key_compare(&lower, &upper) == Ordering::Greater {
        return Err(host_error(
            "DataError: the lower bound is greater than the upper bound",
        ));
    }
    let range = KeyRange {
        lower: Some((
            lower,
            args.get(2).and_then(JsValue::as_boolean).unwrap_or(false),
        )),
        upper: Some((
            upper,
            args.get(3).and_then(JsValue::as_boolean).unwrap_or(false),
        )),
    };
    Ok(JsValue::from(build_range(&range, context)?))
}

fn js_idb_open(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(name) = string_arg(args, 0, context) else {
        return Err(host_error("indexedDB.open requires a database name"));
    };
    let version = args
        .get(1)
        .and_then(JsValue::as_number)
        .unwrap_or(0.0)
        .max(0.0) as u64;
    let (request_id, request) = new_request(context)?;
    push_task(IdbTask {
        request: request_id,
        tx: None,
        operation: IdbOperation::Open { db: name, version },
    });
    Ok(JsValue::from(request))
}

fn js_idb_delete_database(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(name) = string_arg(args, 0, context) else {
        return Err(host_error("deleteDatabase requires a name"));
    };
    let (request_id, request) = new_request(context)?;
    push_task(IdbTask {
        request: request_id,
        tx: None,
        operation: IdbOperation::DeleteDatabase { db: name },
    });
    Ok(JsValue::from(request))
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

fn build_database(name: &str, database: &IdbDatabase, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let store_names = JsArray::new(context)?;
    for store in database.stores.keys() {
        store_names.push(js_string_value(store), context)?;
    }
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(DB_NAME_KEY),
            js_string_value(name),
            Attribute::empty(),
        )
        .property(
            JsString::from("name"),
            js_string_value(name),
            Attribute::all(),
        )
        .property(
            JsString::from("version"),
            JsValue::from(database.version as f64),
            Attribute::all(),
        )
        .property(
            JsString::from("objectStoreNames"),
            JsValue::from(store_names),
            Attribute::all(),
        );
    let methods: [Method; 4] = [
        ("createObjectStore", js_idb_create_store),
        ("deleteObjectStore", js_idb_delete_store),
        ("transaction", js_idb_transaction),
        ("close", js_idb_close),
    ];
    for (method, function) in methods {
        let function = NativeFunction::from_fn_ptr(function);
        object.property(
            JsString::from(method),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(object.build())
}

fn build_transaction(
    id: u64,
    db: &str,
    stores: &[String],
    mode: &str,
    context: &mut Context,
) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let store_names = JsArray::new(context)?;
    for store in stores {
        store_names.push(js_string_value(store), context)?;
    }
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(TX_ID_KEY),
            JsValue::from(id as f64),
            Attribute::empty(),
        )
        .property(
            JsString::from(DB_NAME_KEY),
            js_string_value(db),
            Attribute::empty(),
        )
        .property(
            JsString::from("mode"),
            js_string_value(mode),
            Attribute::all(),
        )
        .property(
            JsString::from("objectStoreNames"),
            JsValue::from(store_names),
            Attribute::all(),
        )
        .property(
            JsString::from("oncomplete"),
            JsValue::null(),
            Attribute::all(),
        )
        .property(JsString::from("onerror"), JsValue::null(), Attribute::all())
        .property(JsString::from("onabort"), JsValue::null(), Attribute::all());
    let methods: [Method; 2] = [
        ("objectStore", js_idb_tx_object_store),
        ("abort", js_idb_tx_abort),
    ];
    for (method, function) in methods {
        let function = NativeFunction::from_fn_ptr(function);
        object.property(
            JsString::from(method),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let transaction = object.build();
    if let Some(store) = with_state(|state| state.idb_tx_store.clone()).flatten() {
        store.set(
            JsString::from(id.to_string()),
            JsValue::from(transaction.clone()),
            false,
            context,
        )?;
    }
    Ok(transaction)
}

fn build_store(
    db: &str,
    store: &str,
    transaction: Option<u64>,
    context: &mut Context,
) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let meta = store_meta(db, store);
    let index_names = JsArray::new(context)?;
    let mut indexes: Vec<&String> = meta.indexes.keys().collect();
    indexes.sort();
    for index in indexes {
        index_names.push(js_string_value(index), context)?;
    }
    let key_path = match &meta.key_path {
        Some(path) => js_string_value(path),
        None => JsValue::null(),
    };
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(DB_NAME_KEY),
            js_string_value(db),
            Attribute::empty(),
        )
        .property(
            JsString::from(STORE_NAME_KEY),
            js_string_value(store),
            Attribute::empty(),
        )
        .property(
            JsString::from("name"),
            js_string_value(store),
            Attribute::all(),
        )
        .property(JsString::from("keyPath"), key_path, Attribute::all())
        .property(
            JsString::from("autoIncrement"),
            JsValue::from(meta.auto_increment),
            Attribute::all(),
        )
        .property(
            JsString::from("indexNames"),
            JsValue::from(index_names),
            Attribute::all(),
        );
    if let Some(transaction) = transaction {
        object.property(
            JsString::from(TX_ID_KEY),
            JsValue::from(transaction as f64),
            Attribute::empty(),
        );
    }
    let methods: [Method; 11] = [
        ("put", js_idb_store_put),
        ("get", js_idb_store_get),
        ("getKey", js_idb_store_get_key),
        ("getAll", js_idb_store_get_all),
        ("delete", js_idb_store_delete),
        ("clear", js_idb_store_clear),
        ("count", js_idb_store_count),
        ("createIndex", js_idb_create_index),
        ("deleteIndex", js_idb_delete_index),
        ("index", js_idb_store_index),
        ("openCursor", js_idb_store_open_cursor),
    ];
    for (method, function) in methods {
        let function = NativeFunction::from_fn_ptr(function);
        object.property(
            JsString::from(method),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let function = NativeFunction::from_fn_ptr(js_idb_store_open_key_cursor);
    object.property(
        JsString::from("openKeyCursor"),
        function.to_js_function(&realm),
        Attribute::all(),
    );
    Ok(object.build())
}

fn build_index(
    db: &str,
    store: &str,
    name: &str,
    meta: &IndexMeta,
    transaction: Option<u64>,
    context: &mut Context,
) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let key_path = key_paths_to_js(&meta.key_path, context)?;
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(DB_NAME_KEY),
            js_string_value(db),
            Attribute::empty(),
        )
        .property(
            JsString::from(STORE_NAME_KEY),
            js_string_value(store),
            Attribute::empty(),
        )
        .property(
            JsString::from(INDEX_NAME_KEY),
            js_string_value(name),
            Attribute::empty(),
        )
        .property(
            JsString::from("name"),
            js_string_value(name),
            Attribute::all(),
        )
        .property(JsString::from("keyPath"), key_path, Attribute::all())
        .property(
            JsString::from("unique"),
            JsValue::from(meta.unique),
            Attribute::all(),
        )
        .property(
            JsString::from("multiEntry"),
            JsValue::from(false),
            Attribute::all(),
        )
        .property(
            JsString::from("objectStoreName"),
            js_string_value(store),
            Attribute::all(),
        );
    if let Some(transaction) = transaction {
        object.property(
            JsString::from(TX_ID_KEY),
            JsValue::from(transaction as f64),
            Attribute::empty(),
        );
    }
    let methods: [Method; 6] = [
        ("get", js_idb_index_get),
        ("getKey", js_idb_index_get_key),
        ("getAll", js_idb_index_get_all),
        ("getAllKeys", js_idb_index_get_all_keys),
        ("count", js_idb_index_count),
        ("openCursor", js_idb_index_open_cursor),
    ];
    for (method, function) in methods {
        let function = NativeFunction::from_fn_ptr(function);
        object.property(
            JsString::from(method),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    let function = NativeFunction::from_fn_ptr(js_idb_index_open_key_cursor);
    object.property(
        JsString::from("openKeyCursor"),
        function.to_js_function(&realm),
        Attribute::all(),
    );
    Ok(object.build())
}

// ---------------------------------------------------------------------------
// Database methods
// ---------------------------------------------------------------------------

fn js_idb_create_store(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("createObjectStore requires a database"));
    };
    let Some(store) = string_arg(args, 0, context) else {
        return Err(host_error("createObjectStore requires a name"));
    };
    let options = args.get(1).and_then(|value| value.as_object());
    let mut key_path = None;
    let mut auto_increment = false;
    if let Some(options) = &options {
        if let Ok(value) = options.get(JsString::from("keyPath"), context) {
            key_path = value
                .as_string()
                .map(|value| value.to_std_string_escaped())
                .filter(|value| !value.is_empty());
        }
        if let Ok(value) = options.get(JsString::from("autoIncrement"), context) {
            auto_increment = value.as_boolean().unwrap_or(false);
        }
    }
    ensure_loaded(&db);
    with_state(|state| {
        if let Some(database) = state.idb_databases.get_mut(&db) {
            database.stores.entry(store.clone()).or_default();
            // Keep existing metadata (notably the autoIncrement generator)
            // when the store is created again on a later upgrade.
            if !database.meta.contains_key(&store) {
                database.meta.insert(
                    store.clone(),
                    StoreMeta {
                        key_path,
                        auto_increment,
                        generator: 0.0,
                        indexes: HashMap::new(),
                    },
                );
            }
        }
    });
    persist_db(&db);
    Ok(JsValue::from(build_store(&db, &store, None, context)?))
}

fn js_idb_delete_store(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("deleteObjectStore requires a database"));
    };
    let Some(store) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    with_state(|state| {
        if let Some(database) = state.idb_databases.get_mut(&db) {
            database.stores.remove(&store);
            database.meta.remove(&store);
        }
    });
    persist_db(&db);
    Ok(JsValue::undefined())
}

fn js_idb_transaction(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("transaction requires a database"));
    };
    let mut stores = Vec::new();
    if let Some(value) = args.first() {
        if let Some(name) = value.as_string() {
            stores.push(name.to_std_string_escaped());
        } else if let Some(object) = value.as_object()
            && let Ok(array) = JsArray::from_object(object.clone())
        {
            let length = array.length(context)?;
            for index in 0..length {
                if let Ok(value) = array.get(index, context)
                    && let Some(name) = value.as_string()
                {
                    stores.push(name.to_std_string_escaped());
                }
            }
        }
    }
    let mode = string_arg(args, 1, context).unwrap_or_else(|| "readonly".to_owned());
    ensure_loaded(&db);
    let id = next_id().ok_or_else(|| host_error("runtime is not active"))?;
    with_state(|state| {
        state.idb_txs.insert(
            id,
            IdbTransaction {
                db: db.clone(),
                pending: 0,
                completed: false,
            },
        );
    });
    Ok(JsValue::from(build_transaction(
        id, &db, &stores, &mode, context,
    )?))
}

fn js_idb_close(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn js_idb_tx_abort(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn js_idb_tx_object_store(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("objectStore requires a transaction"));
    };
    let Some(tx) = tx_id(this, context) else {
        return Err(host_error("objectStore requires a transaction"));
    };
    let Some(store) = string_arg(args, 0, context) else {
        return Err(host_error("objectStore requires a name"));
    };
    Ok(JsValue::from(build_store(&db, &store, Some(tx), context)?))
}

// ---------------------------------------------------------------------------
// Store requests
// ---------------------------------------------------------------------------

fn store_request(
    this: &JsValue,
    context: &mut Context,
    operation: impl FnOnce(&str, &str, Option<u64>) -> IdbOperation,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("invalid object store"));
    };
    let Some(store) = store_name(this, context) else {
        return Err(host_error("invalid object store"));
    };
    let transaction = tx_id(this, context);
    let (request_id, request) = new_request(context)?;
    bump_pending(transaction);
    push_task(IdbTask {
        request: request_id,
        tx: transaction,
        operation: operation(&db, &store, transaction),
    });
    Ok(JsValue::from(request))
}

fn index_request(
    this: &JsValue,
    context: &mut Context,
    operation: impl FnOnce(&str, &str, &str, Option<u64>) -> IdbOperation,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("invalid index"));
    };
    let Some(store) = store_name(this, context) else {
        return Err(host_error("invalid index"));
    };
    let Some(index) = index_name(this, context) else {
        return Err(host_error("invalid index"));
    };
    let transaction = tx_id(this, context);
    let (request_id, request) = new_request(context)?;
    bump_pending(transaction);
    push_task(IdbTask {
        request: request_id,
        tx: transaction,
        operation: operation(&db, &store, &index, transaction),
    });
    Ok(JsValue::from(request))
}

fn js_idb_store_put(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let value = args
        .first()
        .and_then(|value| js_to_json(value, context, 0))
        .unwrap_or(Value::Null);
    let key = match args.get(1) {
        Some(key) if !key.is_null_or_undefined() => {
            let Some(key) = js_key_from_value(key, context, 0) else {
                return Err(host_error("DataError: value is not a valid IndexedDB key"));
            };
            Some(key)
        }
        _ => None,
    };
    store_request(this, context, move |db, store, _| IdbOperation::Put {
        db: db.to_owned(),
        store: store.to_owned(),
        key,
        value,
    })
}

fn js_idb_store_get(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let query = parse_optional_query(args, 0, context)?
        .ok_or_else(|| host_error("get requires a key or key range"))?;
    store_request(this, context, move |db, store, _| IdbOperation::Get {
        db: db.to_owned(),
        store: store.to_owned(),
        query,
    })
}

fn js_idb_store_get_key(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let query = parse_optional_query(args, 0, context)?
        .ok_or_else(|| host_error("getKey requires a key or key range"))?;
    store_request(this, context, move |db, store, _| IdbOperation::GetKey {
        db: db.to_owned(),
        store: store.to_owned(),
        query,
    })
}

fn parse_all_args(
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<(Option<Query>, Option<usize>)> {
    let mut query = None;
    let mut count = None;
    match args.first() {
        Some(value) if value.as_number().is_some() => {
            count = parse_count(value);
        }
        Some(value) if !value.is_null_or_undefined() => {
            query = Some(parse_query(value, context)?);
            count = args.get(1).and_then(parse_count);
        }
        _ => {}
    }
    Ok((query, count))
}

fn js_idb_store_get_all(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (query, count) = parse_all_args(args, context)?;
    store_request(this, context, move |db, store, _| IdbOperation::GetAll {
        db: db.to_owned(),
        store: store.to_owned(),
        query,
        count,
    })
}

fn js_idb_store_delete(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let query = parse_optional_query(args, 0, context)?
        .ok_or_else(|| host_error("delete requires a key or key range"))?;
    store_request(this, context, move |db, store, _| IdbOperation::Delete {
        db: db.to_owned(),
        store: store.to_owned(),
        query,
    })
}

fn js_idb_store_clear(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    store_request(this, context, |db, store, _| IdbOperation::Clear {
        db: db.to_owned(),
        store: store.to_owned(),
    })
}

fn js_idb_store_count(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let query = parse_optional_query(args, 0, context)?;
    store_request(this, context, move |db, store, _| IdbOperation::Count {
        db: db.to_owned(),
        store: store.to_owned(),
        query,
    })
}

fn js_idb_create_index(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("createIndex requires an object store"));
    };
    let Some(store) = store_name(this, context) else {
        return Err(host_error("createIndex requires an object store"));
    };
    let Some(name) = string_arg(args, 0, context) else {
        return Err(host_error("createIndex requires a name"));
    };
    let Some(key_path_value) = args.get(1) else {
        return Err(host_error("createIndex requires a keyPath"));
    };
    if key_path_value.is_null_or_undefined() {
        return Err(host_error("createIndex requires a keyPath"));
    }
    let key_path = key_paths_from_value(key_path_value, context)?;
    let unique = args
        .get(2)
        .and_then(|value| value.as_object())
        .and_then(|options| {
            options
                .get(JsString::from("unique"), context)
                .ok()
                .and_then(|value| value.as_boolean())
        })
        .unwrap_or(false);
    let index_meta = IndexMeta { key_path, unique };
    ensure_loaded(&db);
    let created = with_state(|state| {
        let Some(database) = state.idb_databases.get_mut(&db) else {
            return false;
        };
        if !database.stores.contains_key(&store) {
            return false;
        }
        database
            .meta
            .entry(store.clone())
            .or_default()
            .indexes
            .insert(name.clone(), index_meta.clone());
        true
    })
    .unwrap_or(false);
    if !created {
        return Err(host_error("NotFoundError: object store does not exist"));
    }
    persist_db(&db);
    let transaction = tx_id(this, context);
    Ok(JsValue::from(build_index(
        &db,
        &store,
        &name,
        &index_meta,
        transaction,
        context,
    )?))
}

fn js_idb_delete_index(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("deleteIndex requires an object store"));
    };
    let Some(store) = store_name(this, context) else {
        return Err(host_error("deleteIndex requires an object store"));
    };
    let Some(name) = string_arg(args, 0, context) else {
        return Ok(JsValue::undefined());
    };
    ensure_loaded(&db);
    with_state(|state| {
        if let Some(database) = state.idb_databases.get_mut(&db)
            && let Some(meta) = database.meta.get_mut(&store)
        {
            meta.indexes.remove(&name);
        }
    });
    persist_db(&db);
    Ok(JsValue::undefined())
}

fn js_idb_store_index(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("index requires an object store"));
    };
    let Some(store) = store_name(this, context) else {
        return Err(host_error("index requires an object store"));
    };
    let Some(name) = string_arg(args, 0, context) else {
        return Err(host_error("index requires a name"));
    };
    let meta = store_meta(&db, &store);
    let Some(index_meta) = meta.indexes.get(&name).cloned() else {
        return Err(host_error("NotFoundError: index does not exist"));
    };
    let transaction = tx_id(this, context);
    Ok(JsValue::from(build_index(
        &db,
        &store,
        &name,
        &index_meta,
        transaction,
        context,
    )?))
}

// ---------------------------------------------------------------------------
// Index requests
// ---------------------------------------------------------------------------

fn js_idb_index_get(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let query = parse_optional_query(args, 0, context)?;
    index_request(this, context, move |db, store, index, _| {
        IdbOperation::IndexQuery {
            db: db.to_owned(),
            store: store.to_owned(),
            index: index.to_owned(),
            kind: QueryKind::Get,
            query,
            count: None,
        }
    })
}

fn js_idb_index_get_key(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let query = parse_optional_query(args, 0, context)?;
    index_request(this, context, move |db, store, index, _| {
        IdbOperation::IndexQuery {
            db: db.to_owned(),
            store: store.to_owned(),
            index: index.to_owned(),
            kind: QueryKind::GetKey,
            query,
            count: None,
        }
    })
}

fn js_idb_index_get_all(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (query, count) = parse_all_args(args, context)?;
    index_request(this, context, move |db, store, index, _| {
        IdbOperation::IndexQuery {
            db: db.to_owned(),
            store: store.to_owned(),
            index: index.to_owned(),
            kind: QueryKind::GetAll,
            query,
            count,
        }
    })
}

fn js_idb_index_get_all_keys(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (query, count) = parse_all_args(args, context)?;
    index_request(this, context, move |db, store, index, _| {
        IdbOperation::IndexQuery {
            db: db.to_owned(),
            store: store.to_owned(),
            index: index.to_owned(),
            kind: QueryKind::GetAllKeys,
            query,
            count,
        }
    })
}

fn js_idb_index_count(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let query = parse_optional_query(args, 0, context)?;
    index_request(this, context, move |db, store, index, _| {
        IdbOperation::IndexQuery {
            db: db.to_owned(),
            store: store.to_owned(),
            index: index.to_owned(),
            kind: QueryKind::Count,
            query,
            count: None,
        }
    })
}

// ---------------------------------------------------------------------------
// Cursors
// ---------------------------------------------------------------------------

fn build_cursor(config: &CursorConfig, context: &mut Context) -> JsResult<JsObject> {
    let realm = context.realm().clone();
    let mut object = ObjectInitializer::new(context);
    object
        .property(
            JsString::from(CURSOR_REQUEST_KEY),
            JsValue::from(config.request as f64),
            Attribute::empty(),
        )
        .property(
            JsString::from(CURSOR_KEYS_ONLY_KEY),
            JsValue::from(config.keys_only),
            Attribute::empty(),
        )
        .property(
            JsString::from(CURSOR_DIRECTION_KEY),
            js_string_value(config.direction.as_str()),
            Attribute::empty(),
        )
        .property(
            JsString::from(CURSOR_PRIMARY_KEY),
            js_string_value(""),
            Attribute::all(),
        )
        .property(
            JsString::from(DB_NAME_KEY),
            js_string_value(&config.db),
            Attribute::empty(),
        )
        .property(
            JsString::from(STORE_NAME_KEY),
            js_string_value(&config.store),
            Attribute::empty(),
        )
        .property(
            JsString::from("key"),
            JsValue::undefined(),
            Attribute::all(),
        )
        .property(
            JsString::from("primaryKey"),
            JsValue::undefined(),
            Attribute::all(),
        )
        .property(
            JsString::from("value"),
            JsValue::undefined(),
            Attribute::all(),
        );
    match &config.index {
        Some(index) => {
            object.property(
                JsString::from(INDEX_NAME_KEY),
                js_string_value(index),
                Attribute::empty(),
            );
        }
        None => {
            object.property(
                JsString::from(INDEX_NAME_KEY),
                JsValue::null(),
                Attribute::empty(),
            );
        }
    }
    if let Some(range) = &config.range
        && let Ok(text) = serde_json::to_string(range)
    {
        object.property(
            JsString::from(RANGE_KEY),
            js_string_value(&text),
            Attribute::empty(),
        );
    }
    if let Some(tx) = config.tx {
        object.property(
            JsString::from(TX_ID_KEY),
            JsValue::from(tx as f64),
            Attribute::empty(),
        );
    }
    let cursor_methods: [Method; 3] = [
        ("continue", js_cursor_continue),
        ("advance", js_cursor_advance),
        ("delete", js_cursor_delete),
    ];
    for (name, function) in cursor_methods {
        let function = NativeFunction::from_fn_ptr(function);
        object.property(
            JsString::from(name),
            function.to_js_function(&realm),
            Attribute::all(),
        );
    }
    Ok(object.build())
}

fn cursor_config(cursor: &JsObject, context: &mut Context) -> Option<CursorConfig> {
    let mut read = |cursor: &JsObject, key: &str| cursor.get(JsString::from(key), context).ok();
    let request = read(cursor, CURSOR_REQUEST_KEY)?.as_number()? as u64;
    let db = read(cursor, DB_NAME_KEY)?
        .as_string()?
        .to_std_string_escaped();
    let store = read(cursor, STORE_NAME_KEY)?
        .as_string()?
        .to_std_string_escaped();
    let index = read(cursor, INDEX_NAME_KEY)
        .and_then(|value| value.as_string())
        .map(|value| value.to_std_string_escaped());
    let direction = Direction::parse(
        read(cursor, CURSOR_DIRECTION_KEY)
            .and_then(|value| value.as_string())
            .map(|value| value.to_std_string_escaped()),
    );
    let range = read(cursor, RANGE_KEY)
        .and_then(|value| value.as_string())
        .and_then(|value| serde_json::from_str(&value.to_std_string_escaped()).ok());
    let tx = read(cursor, TX_ID_KEY)
        .and_then(|value| value.as_number())
        .map(|value| value as u64);
    let keys_only = read(cursor, CURSOR_KEYS_ONLY_KEY)
        .and_then(|value| value.as_boolean())
        .unwrap_or(false);
    Some(CursorConfig {
        db,
        store,
        index,
        range,
        direction,
        request,
        tx,
        keys_only,
    })
}

fn start_cursor(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    keys_only: bool,
) -> JsResult<JsValue> {
    let Some(db) = db_name(this, context) else {
        return Err(host_error("invalid object store or index"));
    };
    let Some(store) = store_name(this, context) else {
        return Err(host_error("invalid object store or index"));
    };
    let index = index_name(this, context);
    let transaction = tx_id(this, context);
    let mut range = args
        .first()
        .filter(|value| !value.is_null_or_undefined())
        .and_then(|value| range_from_value(value, context));
    if range.is_none()
        && let Some(key) = args
            .first()
            .filter(|value| !value.is_null_or_undefined())
            .and_then(|value| js_key_from_value(value, context, 0))
    {
        // Be lenient: a bare key is treated as a single-key range.
        range = Some(KeyRange::only(key));
    }
    let direction = Direction::parse(
        args.get(1)
            .and_then(|value| value.as_string())
            .map(|value| value.to_std_string_escaped()),
    );
    let (request_id, request) = new_request(context)?;
    bump_pending(transaction);
    let config = CursorConfig {
        db,
        store,
        index,
        range,
        direction,
        request: request_id,
        tx: transaction,
        keys_only,
    };
    let cursor = build_cursor(&config, context)?;
    let _ = cursor.set(
        JsString::from("request"),
        JsValue::from(request.clone()),
        false,
        context,
    );
    push_task(IdbTask {
        request: request_id,
        tx: transaction,
        operation: IdbOperation::Cursor {
            db: config.db,
            store: config.store,
            index: config.index,
            range: config.range,
            direction: config.direction,
            cursor,
            advance: 0,
            initial: true,
        },
    });
    Ok(JsValue::from(request))
}

fn js_idb_store_open_cursor(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    start_cursor(this, args, context, false)
}

fn js_idb_store_open_key_cursor(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    start_cursor(this, args, context, true)
}

fn js_idb_index_open_cursor(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    start_cursor(this, args, context, false)
}

fn js_idb_index_open_key_cursor(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    start_cursor(this, args, context, true)
}

fn push_cursor_step(this: &JsValue, advance: usize, context: &mut Context) -> JsResult<JsValue> {
    let Some(cursor) = this.as_object() else {
        return Err(host_error("invalid cursor"));
    };
    let Some(config) = cursor_config(&cursor, context) else {
        return Err(host_error("invalid cursor"));
    };
    bump_pending(config.tx);
    if let Some(request) = request_object(config.request, context) {
        let _ = request.set(
            JsString::from("readyState"),
            js_string_value("pending"),
            false,
            context,
        );
    }
    push_task(IdbTask {
        request: config.request,
        tx: config.tx,
        operation: IdbOperation::Cursor {
            db: config.db,
            store: config.store,
            index: config.index,
            range: config.range,
            direction: config.direction,
            cursor,
            advance,
            initial: false,
        },
    });
    Ok(JsValue::undefined())
}

fn js_cursor_continue(
    this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    push_cursor_step(this, 1, context)
}

fn js_cursor_advance(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let count = args.first().and_then(JsValue::as_number).unwrap_or(1.0);
    let steps = if count < 1.0 { 1 } else { count as usize };
    push_cursor_step(this, steps, context)
}

fn js_cursor_delete(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(cursor) = this.as_object() else {
        return Err(host_error("invalid cursor"));
    };
    let Some(config) = cursor_config(&cursor, context) else {
        return Err(host_error("invalid cursor"));
    };
    let encoded = cursor
        .get(JsString::from(CURSOR_PRIMARY_KEY), context)
        .ok()
        .and_then(|value| value.as_string())
        .map(|value| value.to_std_string_escaped())
        .unwrap_or_default();
    if encoded.is_empty() {
        return Err(host_error("InvalidStateError: cursor is not positioned"));
    }
    let primary = key_decode(&encoded);
    let (request_id, request) = new_request(context)?;
    bump_pending(config.tx);
    push_task(IdbTask {
        request: request_id,
        tx: config.tx,
        operation: IdbOperation::Delete {
            db: config.db,
            store: config.store,
            query: Query::Key(primary),
        },
    });
    Ok(JsValue::from(request))
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn normalize_store(values: HashMap<String, Value>) -> HashMap<String, Value> {
    let mut normalized = HashMap::with_capacity(values.len());
    for (key, value) in values {
        normalized.insert(key_encode(&key_decode(&key)), value);
    }
    normalized
}

fn ensure_loaded(name: &str) {
    let already = with_state(|state| state.idb_databases.contains_key(name)).unwrap_or(false);
    if already {
        return;
    }
    let dir = with_state(|state| state.indexeddb_dir.clone()).flatten();
    let database = dir
        .and_then(|dir| {
            let bytes = read_locked(&db_path(&dir, name)).ok().flatten()?;
            serde_json::from_slice::<PersistedDb>(&bytes).ok()
        })
        .map(|persisted| {
            let mut meta = persisted.meta;
            let stores = persisted
                .stores
                .into_iter()
                .map(|(store, values)| {
                    meta.entry(store.clone()).or_default();
                    (store, normalize_store(values))
                })
                .collect();
            IdbDatabase {
                version: persisted.version,
                stores,
                meta,
            }
        })
        .unwrap_or_default();
    with_state(|state| state.idb_databases.insert(name.to_owned(), database));
}

fn persist_db(name: &str) {
    let Some(dir) = with_state(|state| state.indexeddb_dir.clone()).flatten() else {
        return;
    };
    let Some(database) = with_state(|state| state.idb_databases.get(name).cloned()).flatten()
    else {
        return;
    };
    let persisted = PersistedDb {
        version: database.version,
        stores: database.stores,
        meta: database.meta,
    };
    if let Ok(bytes) = serde_json::to_vec(&persisted) {
        let _ = write_atomic(&db_path(&dir, name), &bytes);
    }
}

fn db_path(dir: &Path, name: &str) -> PathBuf {
    let sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    dir.join(format!("{sanitized}-{:016x}.json", hasher.finish()))
}

fn store_meta(db: &str, store: &str) -> StoreMeta {
    ensure_loaded(db);
    with_state(|state| {
        state
            .idb_databases
            .get(db)
            .and_then(|database| database.meta.get(store).cloned())
    })
    .flatten()
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

pub(crate) fn process_one(context: &mut Context) -> bool {
    let Some(task) = with_state(|state| state.idb_tasks.pop_front()).flatten() else {
        return false;
    };
    match task.operation.clone() {
        IdbOperation::Open { db, version } => complete_open(&task, &db, version, context),
        IdbOperation::DeleteDatabase { db } => {
            with_state(|state| state.idb_databases.remove(&db));
            if let Some(dir) = with_state(|state| state.indexeddb_dir.clone()).flatten() {
                let _ = std::fs::remove_file(db_path(&dir, &db));
            }
            finish(&task, success_json(None), context);
        }
        IdbOperation::Cursor {
            db,
            store,
            index,
            range,
            direction,
            cursor,
            advance,
            initial,
        } => {
            let outcome = apply_cursor(
                CursorStep {
                    db: &db,
                    store: &store,
                    index: index.as_deref(),
                    range: range.as_ref(),
                    direction,
                    cursor: &cursor,
                    advance,
                    initial,
                },
                context,
            );
            finish(&task, outcome, context);
        }
        operation => {
            let outcome = apply_operation(operation);
            finish(&task, outcome, context);
        }
    }
    true
}

fn sorted_entries(values: &HashMap<String, Value>) -> Vec<(Key, Value)> {
    let mut entries: Vec<(Key, Value)> = values
        .iter()
        .map(|(key, value)| (key_decode(key), value.clone()))
        .collect();
    entries.sort_by(|left, right| key_compare(&left.0, &right.0));
    entries
}

fn index_keys(database: &IdbDatabase, store: &str, index: &str) -> Option<Vec<(Key, Key, Value)>> {
    let meta = database.meta.get(store)?;
    let index_meta = meta.indexes.get(index)?;
    let values = database.stores.get(store)?;
    let mut entries = Vec::new();
    for (key, value) in values {
        if let Some(index_key) = index_key(value, &index_meta.key_path) {
            entries.push((index_key, key_decode(key), value.clone()));
        }
    }
    entries.sort_by(|left, right| {
        key_compare(&left.0, &right.0).then_with(|| key_compare(&left.1, &right.1))
    });
    Some(entries)
}

fn cursor_rows(
    database: &IdbDatabase,
    store: &str,
    index: Option<&str>,
    direction: Direction,
) -> Option<Vec<CursorRow>> {
    let mut rows: Vec<CursorRow> = match index {
        None => sorted_entries(database.stores.get(store)?)
            .into_iter()
            .map(|(key, value)| CursorRow {
                index_key: None,
                key,
                value,
            })
            .collect(),
        Some(name) => {
            let entries = index_keys(database, store, name)?;
            if direction.unique() {
                let mut unique_rows = Vec::new();
                let mut previous: Option<Key> = None;
                for (index_key, key, value) in entries {
                    if previous
                        .as_ref()
                        .map(|last| key_compare(last, &index_key) == Ordering::Equal)
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    previous = Some(index_key.clone());
                    unique_rows.push(CursorRow {
                        index_key: Some(index_key),
                        key,
                        value,
                    });
                }
                unique_rows
            } else {
                entries
                    .into_iter()
                    .map(|(index_key, key, value)| CursorRow {
                        index_key: Some(index_key),
                        key,
                        value,
                    })
                    .collect()
            }
        }
    };
    if direction.reversed() {
        rows.reverse();
    }
    Some(rows)
}

/// One cursor step: the query to iterate plus the cursor being advanced.
struct CursorStep<'a> {
    db: &'a str,
    store: &'a str,
    index: Option<&'a str>,
    range: Option<&'a KeyRange>,
    direction: Direction,
    cursor: &'a JsObject,
    advance: usize,
    initial: bool,
}

fn apply_cursor(step: CursorStep<'_>, context: &mut Context) -> Outcome {
    let CursorStep {
        db,
        store,
        index,
        range,
        direction,
        cursor,
        advance,
        initial,
    } = step;
    ensure_loaded(db);
    let keys_only = cursor
        .get(JsString::from(CURSOR_KEYS_ONLY_KEY), context)
        .ok()
        .and_then(|value| value.as_boolean())
        .unwrap_or(false);
    let rows = with_state(|state| {
        state
            .idb_databases
            .get(db)
            .and_then(|database| cursor_rows(database, store, index, direction))
    })
    .flatten();
    let Some(rows) = rows else {
        return failure("NotFoundError: object store or index does not exist");
    };
    let rows: Vec<CursorRow> = rows
        .into_iter()
        .filter(|row| {
            range
                .map(|range| range.includes(row.index_key.as_ref().unwrap_or(&row.key)))
                .unwrap_or(true)
        })
        .collect();
    let mark: Option<CursorMark> = cursor
        .get(JsString::from(CURSOR_MARK_KEY), context)
        .ok()
        .and_then(|value| value.as_string())
        .and_then(|value| serde_json::from_str(&value.to_std_string_escaped()).ok());
    let position = if initial {
        0
    } else {
        let start = match &mark {
            Some(mark) => rows
                .iter()
                .position(|row| {
                    let ordering = cursor_order(row, mark);
                    if direction.reversed() {
                        ordering == Ordering::Less
                    } else {
                        ordering == Ordering::Greater
                    }
                })
                .unwrap_or(rows.len()),
            None => 0,
        };
        start.saturating_add(advance.max(1) - 1)
    };
    let Some(row) = rows.get(position) else {
        let _ = cursor.set(JsString::from("key"), JsValue::undefined(), false, context);
        let _ = cursor.set(
            JsString::from("primaryKey"),
            JsValue::undefined(),
            false,
            context,
        );
        let _ = cursor.set(
            JsString::from("value"),
            JsValue::undefined(),
            false,
            context,
        );
        let _ = cursor.set(
            JsString::from(CURSOR_PRIMARY_KEY),
            js_string_value(""),
            false,
            context,
        );
        return success_js(JsValue::null());
    };
    if let Ok(text) = serde_json::to_string(&CursorMark {
        index_key: row.index_key.clone(),
        key: row.key.clone(),
    }) {
        let _ = cursor.set(
            JsString::from(CURSOR_MARK_KEY),
            js_string_value(&text),
            false,
            context,
        );
    }
    let key = key_to_js(row.index_key.as_ref().unwrap_or(&row.key), context);
    let primary = key_to_js(&row.key, context);
    let value = if keys_only {
        JsValue::undefined()
    } else {
        json_to_js(&row.value, context)
    };
    let _ = cursor.set(JsString::from("key"), key, false, context);
    let _ = cursor.set(JsString::from("primaryKey"), primary, false, context);
    let _ = cursor.set(JsString::from("value"), value, false, context);
    let _ = cursor.set(
        JsString::from(CURSOR_PRIMARY_KEY),
        js_string_value(&key_encode(&row.key)),
        false,
        context,
    );
    success_js(JsValue::from(cursor.clone()))
}

fn apply_operation(operation: IdbOperation) -> Outcome {
    match operation {
        IdbOperation::Put {
            db,
            store,
            key,
            value,
        } => apply_put(&db, &store, key, value),
        IdbOperation::Get { db, store, query } => {
            ensure_loaded(&db);
            let found = with_state(|state| {
                state
                    .idb_databases
                    .get(&db)
                    .and_then(|database| database.stores.get(&store))
                    .and_then(|values| {
                        if let Query::Key(key) = &query
                            && let Some(value) = values.get(&key_encode(key))
                        {
                            return Some(value.clone());
                        }
                        sorted_entries(values)
                            .into_iter()
                            .find(|(key, _)| query.matches(key))
                            .map(|(_, value)| value)
                    })
            })
            .flatten();
            success_json(found)
        }
        IdbOperation::GetKey { db, store, query } => {
            ensure_loaded(&db);
            let found = with_state(|state| {
                state
                    .idb_databases
                    .get(&db)
                    .and_then(|database| database.stores.get(&store))
                    .and_then(|values| {
                        sorted_entries(values)
                            .into_iter()
                            .find(|(key, _)| query.matches(key))
                            .map(|(key, _)| key_to_json(&key))
                    })
            })
            .flatten();
            success_json(found)
        }
        IdbOperation::GetAll {
            db,
            store,
            query,
            count,
        } => {
            ensure_loaded(&db);
            let values = with_state(|state| {
                state
                    .idb_databases
                    .get(&db)
                    .and_then(|database| database.stores.get(&store))
                    .map(|values| {
                        let mut matched: Vec<Value> = sorted_entries(values)
                            .into_iter()
                            .filter(|(key, _)| {
                                query.as_ref().map(|q| q.matches(key)).unwrap_or(true)
                            })
                            .map(|(_, value)| value)
                            .collect();
                        if let Some(count) = count {
                            matched.truncate(count);
                        }
                        matched
                    })
            })
            .flatten()
            .unwrap_or_default();
            success_json(Some(Value::Array(values)))
        }
        IdbOperation::Delete { db, store, query } => {
            ensure_loaded(&db);
            let found = with_state(|state| {
                let database = state.idb_databases.get_mut(&db)?;
                let values = database.stores.get_mut(&store)?;
                match &query {
                    Query::Key(key) => {
                        values.remove(&key_encode(key));
                    }
                    Query::Range(range) => {
                        let doomed: Vec<String> = values
                            .keys()
                            .filter(|key| range.includes(&key_decode(key)))
                            .cloned()
                            .collect();
                        for key in doomed {
                            values.remove(&key);
                        }
                    }
                }
                Some(())
            })
            .flatten();
            match found {
                Some(()) => {
                    let mut outcome = success_json(None);
                    outcome.persist = Some(db);
                    outcome
                }
                None => failure("NotFoundError: object store does not exist"),
            }
        }
        IdbOperation::Clear { db, store } => {
            ensure_loaded(&db);
            let found = with_state(|state| {
                state
                    .idb_databases
                    .get_mut(&db)
                    .and_then(|database| database.stores.get_mut(&store))
                    .map(|values| values.clear())
                    .is_some()
            })
            .unwrap_or(false);
            if found {
                let mut outcome = success_json(None);
                outcome.persist = Some(db);
                outcome
            } else {
                failure("NotFoundError: object store does not exist")
            }
        }
        IdbOperation::Count { db, store, query } => {
            ensure_loaded(&db);
            let count = with_state(|state| {
                state
                    .idb_databases
                    .get(&db)
                    .and_then(|database| database.stores.get(&store))
                    .map(|values| {
                        sorted_entries(values)
                            .into_iter()
                            .filter(|(key, _)| {
                                query.as_ref().map(|q| q.matches(key)).unwrap_or(true)
                            })
                            .count()
                    })
            })
            .flatten()
            .unwrap_or(0);
            success_json(Some(Value::from(count as u64)))
        }
        IdbOperation::CreateIndex {
            db,
            store,
            index,
            key_path,
            unique,
        } => {
            ensure_loaded(&db);
            let index_meta = IndexMeta { key_path, unique };
            let created = with_state(|state| {
                let Some(database) = state.idb_databases.get_mut(&db) else {
                    return false;
                };
                if !database.stores.contains_key(&store) {
                    return false;
                }
                database
                    .meta
                    .entry(store.clone())
                    .or_default()
                    .indexes
                    .insert(index, index_meta);
                true
            })
            .unwrap_or(false);
            if created {
                let mut outcome = success_json(None);
                outcome.persist = Some(db);
                outcome
            } else {
                failure("NotFoundError: object store does not exist")
            }
        }
        IdbOperation::DeleteIndex { db, store, index } => {
            ensure_loaded(&db);
            with_state(|state| {
                if let Some(database) = state.idb_databases.get_mut(&db)
                    && let Some(meta) = database.meta.get_mut(&store)
                {
                    meta.indexes.remove(&index);
                }
            });
            let mut outcome = success_json(None);
            outcome.persist = Some(db);
            outcome
        }
        IdbOperation::IndexQuery {
            db,
            store,
            index,
            kind,
            query,
            count,
        } => {
            ensure_loaded(&db);
            let entries = with_state(|state| {
                state
                    .idb_databases
                    .get(&db)
                    .and_then(|database| index_keys(database, &store, &index))
            })
            .flatten();
            let Some(entries) = entries else {
                return failure("NotFoundError: index does not exist");
            };
            let matches =
                |index_key: &Key| query.as_ref().map(|q| q.matches(index_key)).unwrap_or(true);
            match kind {
                QueryKind::Get => {
                    let value = entries
                        .into_iter()
                        .find(|(index_key, _, _)| matches(index_key))
                        .map(|(_, _, value)| value);
                    success_json(value)
                }
                QueryKind::GetKey => {
                    let key = entries
                        .into_iter()
                        .find(|(index_key, _, _)| matches(index_key))
                        .map(|(_, key, _)| key_to_json(&key));
                    success_json(key)
                }
                QueryKind::GetAll => {
                    let mut values: Vec<Value> = entries
                        .into_iter()
                        .filter(|(index_key, _, _)| matches(index_key))
                        .map(|(_, _, value)| value)
                        .collect();
                    if let Some(count) = count {
                        values.truncate(count);
                    }
                    success_json(Some(Value::Array(values)))
                }
                QueryKind::GetAllKeys => {
                    let mut keys: Vec<Value> = entries
                        .into_iter()
                        .filter(|(index_key, _, _)| matches(index_key))
                        .map(|(_, key, _)| key_to_json(&key))
                        .collect();
                    if let Some(count) = count {
                        keys.truncate(count);
                    }
                    success_json(Some(Value::Array(keys)))
                }
                QueryKind::Count => {
                    let total = entries
                        .into_iter()
                        .filter(|(index_key, _, _)| matches(index_key))
                        .count();
                    success_json(Some(Value::from(total as u64)))
                }
            }
        }
        _ => failure("InvalidStateError"),
    }
}

fn apply_put(db: &str, store: &str, key: Option<Key>, value: Value) -> Outcome {
    ensure_loaded(db);
    let outcome = with_state(|state| {
        let Some(database) = state.idb_databases.get_mut(db) else {
            return failure("InvalidStateError: database is not open");
        };
        if !database.stores.contains_key(store) {
            return failure("NotFoundError: object store does not exist");
        }
        let mut meta = database.meta.get(store).cloned().unwrap_or_default();
        let primary = match key {
            Some(key) => Some(key),
            None => {
                let inline = meta
                    .key_path
                    .as_ref()
                    .and_then(|path| value_key(&value, path));
                match inline {
                    Some(key) => Some(key),
                    None if meta.auto_increment => {
                        meta.generator += 1.0;
                        Some(Key::Number(meta.generator))
                    }
                    None => None,
                }
            }
        };
        let Some(primary) = primary else {
            return failure("DataError: the object store requires a key and has no key generator");
        };
        if meta.auto_increment
            && let Key::Number(number) = &primary
            && *number > meta.generator
        {
            meta.generator = *number;
        }
        let encoded = key_encode(&primary);
        for (name, index) in &meta.indexes {
            if !index.unique {
                continue;
            }
            let Some(candidate) = index_key(&value, &index.key_path) else {
                continue;
            };
            let conflict = database
                .stores
                .get(store)
                .map(|values| {
                    values.iter().any(|(existing_key, existing_value)| {
                        existing_key != &encoded
                            && index_key(existing_value, &index.key_path)
                                .map(|other| key_compare(&other, &candidate) == Ordering::Equal)
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            if conflict {
                return failure(&format!(
                    "ConstraintError: unique index '{name}' already holds this key"
                ));
            }
        }
        if let Some(values) = database.stores.get_mut(store) {
            values.insert(encoded, value);
        }
        database.meta.insert(store.to_owned(), meta);
        success_json(Some(key_to_json(&primary)))
    })
    .unwrap_or_else(|| failure("InvalidStateError: runtime is not active"));
    let mut outcome = outcome;
    if outcome.success {
        outcome.persist = Some(db.to_owned());
    }
    outcome
}

fn complete_open(task: &IdbTask, name: &str, version: u64, context: &mut Context) {
    ensure_loaded(name);
    let Some((database, upgraded)) = with_state(|state| {
        let database = state.idb_databases.get_mut(name)?;
        let requested = if version == 0 {
            database.version.max(1)
        } else {
            version
        };
        let upgraded = requested > database.version;
        if upgraded {
            database.version = requested;
        }
        Some((database.clone(), upgraded))
    })
    .flatten() else {
        finish(
            task,
            failure("InvalidStateError: database could not be opened"),
            context,
        );
        return;
    };
    if version != 0 && version < database.version {
        finish(
            task,
            failure("VersionError: requested version is lower than the existing version"),
            context,
        );
        return;
    }
    let db_object = match build_database(name, &database, context) {
        Ok(object) => object,
        Err(error) => {
            complete_request_error(task.request, &error.to_string(), context);
            return;
        }
    };
    if let Some(request) = request_object(task.request, context) {
        let _ = request.set(
            JsString::from("result"),
            JsValue::from(db_object.clone()),
            false,
            context,
        );
        let _ = request.set(
            JsString::from("readyState"),
            js_string_value("done"),
            false,
            context,
        );
        if upgraded {
            fire_handler(&request, "onupgradeneeded", context);
        }
        fire_handler(&request, "onsuccess", context);
    }
    if upgraded {
        persist_db(name);
    }
    if let Some(tx) = task.tx {
        transaction_done(tx, context);
    }
}

fn finish(task: &IdbTask, outcome: Outcome, context: &mut Context) {
    if let Some(request) = request_object(task.request, context) {
        let _ = request.set(
            JsString::from("readyState"),
            js_string_value("done"),
            false,
            context,
        );
        if outcome.success {
            let value = match outcome.result_js {
                Some(value) => value,
                None => outcome
                    .result
                    .as_ref()
                    .map(|value| json_to_js(value, context))
                    .unwrap_or(JsValue::undefined()),
            };
            let _ = request.set(JsString::from("result"), value, false, context);
            fire_handler(&request, "onsuccess", context);
        } else {
            let message = outcome
                .error
                .unwrap_or_else(|| "IndexedDB error".to_owned());
            let _ = request.set(
                JsString::from("error"),
                js_string_value(&message),
                false,
                context,
            );
            fire_handler(&request, "onerror", context);
        }
    }
    if let Some(db) = outcome.persist {
        persist_db(&db);
    }
    if let Some(tx) = task.tx {
        transaction_done(tx, context);
    }
}

fn complete_request_error(request: u64, message: &str, context: &mut Context) {
    if let Some(request) = request_object(request, context) {
        let _ = request.set(
            JsString::from("readyState"),
            js_string_value("done"),
            false,
            context,
        );
        let _ = request.set(
            JsString::from("error"),
            js_string_value(message),
            false,
            context,
        );
        fire_handler(&request, "onerror", context);
    }
}

fn request_object(id: u64, context: &mut Context) -> Option<JsObject> {
    let store = with_state(|state| state.idb_request_store.clone()).flatten()?;
    store
        .get(JsString::from(id.to_string()), context)
        .ok()
        .and_then(|value| value.as_object())
}

fn transaction_done(id: u64, context: &mut Context) {
    let completed = with_state(|state| {
        let tx = state.idb_txs.get_mut(&id)?;
        tx.pending = tx.pending.saturating_sub(1);
        if tx.pending == 0 && !tx.completed {
            tx.completed = true;
            Some(())
        } else {
            None
        }
    })
    .flatten()
    .is_some();
    if !completed {
        return;
    }
    let Some(store) = with_state(|state| state.idb_tx_store.clone()).flatten() else {
        return;
    };
    let Ok(transaction) = store.get(JsString::from(id.to_string()), context) else {
        return;
    };
    let Some(transaction) = transaction.as_object() else {
        return;
    };
    fire_handler(&transaction, "oncomplete", context);
}

fn fire_handler(target: &JsObject, handler: &str, context: &mut Context) {
    let Ok(value) = target.get(JsString::from(handler), context) else {
        return;
    };
    let Some(function) = value.as_function() else {
        return;
    };
    let mut event = ObjectInitializer::new(context);
    event
        .property(
            JsString::from("type"),
            js_string_value(handler.trim_start_matches("on")),
            Attribute::all(),
        )
        .property(
            JsString::from("target"),
            JsValue::from(target.clone()),
            Attribute::all(),
        );
    let event = event.build();
    let this = JsValue::from(target.clone());
    if let Err(error) = function.call(&this, &[JsValue::from(event)], context) {
        with_state(|state| state.errors.push(error.to_string()));
    }
}

// ---------------------------------------------------------------------------
// Key path extraction
// ---------------------------------------------------------------------------

fn value_key(value: &Value, path: &str) -> Option<Key> {
    if path.is_empty() {
        return json_value_to_key(value);
    }
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    json_value_to_key(current)
}

fn index_key(value: &Value, paths: &[String]) -> Option<Key> {
    if paths.is_empty() {
        return None;
    }
    if paths.len() == 1 {
        return value_key(value, &paths[0]);
    }
    let mut keys = Vec::with_capacity(paths.len());
    for path in paths {
        keys.push(value_key(value, path)?);
    }
    Some(Key::Array(keys))
}

// ---------------------------------------------------------------------------
// JSON conversion (shared with the rest of the runtime)
// ---------------------------------------------------------------------------

pub(crate) fn js_to_json(value: &JsValue, context: &mut Context, depth: usize) -> Option<Value> {
    if depth > 32 {
        return None;
    }
    if value.is_null_or_undefined() {
        return Some(Value::Null);
    }
    if let Some(boolean) = value.as_boolean() {
        return Some(Value::Bool(boolean));
    }
    if let Some(number) = value.as_number() {
        return serde_json::Number::from_f64(number).map(Value::Number);
    }
    if let Some(string) = value.as_string() {
        return Some(Value::String(string.to_std_string_escaped()));
    }
    let object = value.as_object()?;
    if let Ok(array) = JsArray::from_object(object.clone()) {
        let length = array.length(context).ok()?;
        let mut values = Vec::new();
        for index in 0..length {
            let item = array.get(index, context).ok()?;
            values.push(js_to_json(&item, context, depth + 1)?);
        }
        return Some(Value::Array(values));
    }
    let keys = object.own_property_keys(context).ok()?;
    let mut map = serde_json::Map::new();
    for key in keys {
        if let boa_engine::property::PropertyKey::String(name) = key {
            let name = name.to_std_string_escaped();
            let value = object.get(JsString::from(name.clone()), context).ok()?;
            if let Some(value) = js_to_json(&value, context, depth + 1) {
                map.insert(name, value);
            }
        }
    }
    Some(Value::Object(map))
}

pub(crate) fn json_to_js(value: &Value, context: &mut Context) -> JsValue {
    match value {
        Value::Null => JsValue::null(),
        Value::Bool(value) => JsValue::from(*value),
        Value::Number(value) => JsValue::from(value.as_f64().unwrap_or(0.0)),
        Value::String(value) => js_string_value(value),
        Value::Array(values) => {
            if let Ok(array) = JsArray::new(context) {
                for value in values {
                    let _ = array.push(json_to_js(value, context), context);
                }
                JsValue::from(array)
            } else {
                JsValue::null()
            }
        }
        Value::Object(map) => {
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
