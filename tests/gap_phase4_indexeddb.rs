mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use nexium::Browser;
use nexium::Config;
use serde_json::{Value, json};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-idb-gap-{tag}-{}-{nanos}-{count}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn config_with_data_dir(root: &Path) -> Config {
    Config {
        data_dir: root.to_path_buf(),
        ..Config::default()
    }
}

/// Minimal HTTP server that returns the same small HTML document for every
/// path, so pages have a real origin to key IndexedDB storage against.
async fn spawn_page_server() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 8192];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await;
                let body =
                    b"<html><head><title>IDB</title></head><body><p id=out>ready</p></body></html>";
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, head.as_bytes()).await;
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, body).await;
                let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
            });
        }
    });
    addr
}

fn eval(browser: &mut Browser, code: &str) -> Value {
    match browser.evaluate(code) {
        Ok(value) => value,
        Err(error) => panic!("evaluate failed: {error}\n-- code --\n{code}"),
    }
}

/// Flatten a JSON array of numbers (JS numbers arrive as JSON floats).
fn numbers(value: Value) -> Vec<f64> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array, got {value}"))
        .iter()
        .map(|item| {
            item.as_f64()
                .unwrap_or_else(|| panic!("expected a number, got {item}"))
        })
        .collect()
}

/// Recursively collect `<name>.json` files in the IndexedDB directory.
fn idb_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(idb_files(&path));
        } else if path.extension().and_then(|value| value.to_str()) == Some("json") {
            files.push(path);
        }
    }
    files
}

#[tokio::test]
async fn auto_increment_keys_are_generated_and_persisted() {
    let root = temp_dir("autoinc");
    let addr = spawn_page_server().await;
    let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
    browser.open_tab(&format!("http://{addr}/")).await.unwrap();

    eval(
        &mut browser,
        r#"
        var open = indexedDB.open('autoinc', 1);
        open.onupgradeneeded = function () {
          open.result.createObjectStore('items', { autoIncrement: true });
        };
        open.onsuccess = function () { globalThis.__db = open.result; };
        'open requested';
        "#,
    );
    assert!(
        eval(&mut browser, "typeof globalThis.__db") == json!("object"),
        "database was not opened"
    );
    assert_eq!(
        eval(
            &mut browser,
            "globalThis.__db.transaction('items').objectStore('items').autoIncrement"
        ),
        json!(true)
    );

    eval(
        &mut browser,
        r#"
        globalThis.__keys = [];
        var store = globalThis.__db.transaction('items', 'readwrite').objectStore('items');
        ['a', 'b', 'c'].forEach(function (label) {
          var request = store.put({ label: label });
          request.onsuccess = function () { globalThis.__keys.push(request.result); };
        });
        'puts queued';
        "#,
    );
    assert_eq!(
        numbers(eval(&mut browser, "globalThis.__keys")),
        vec![1.0, 2.0, 3.0],
        "autoIncrement keys should be 1, 2, 3"
    );

    eval(
        &mut browser,
        r#"
        var request = globalThis.__db.transaction('items').objectStore('items').getAll();
        request.onsuccess = function () { globalThis.__all = request.result; };
        var first = globalThis.__db.transaction('items').objectStore('items').get(1);
        first.onsuccess = function () { globalThis.__first = first.result; };
        'reads queued';
        "#,
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__all"),
        json!([{"label": "a"}, {"label": "b"}, {"label": "c"}])
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__first"),
        json!({"label": "a"})
    );

    // A second tab (fresh JS runtime) reloads from disk; the key generator
    // must continue after the persisted maximum.
    browser
        .open_tab(&format!("http://{addr}/second"))
        .await
        .unwrap();
    eval(
        &mut browser,
        r#"
        var open = indexedDB.open('autoinc');
        open.onsuccess = function () { globalThis.__db2 = open.result; };
        'reopened';
        "#,
    );
    assert_eq!(eval(&mut browser, "globalThis.__db2.version"), json!(1.0));
    eval(
        &mut browser,
        r#"
        globalThis.__next = null;
        var store = globalThis.__db2.transaction('items', 'readwrite').objectStore('items');
        var request = store.put({ label: 'd' });
        request.onsuccess = function () { globalThis.__next = request.result; };
        'put queued';
        "#,
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__next"),
        json!(4.0),
        "key generator should resume at 4 after reload"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn unique_index_rejects_duplicates_and_supports_lookup() {
    let root = temp_dir("unique");
    let addr = spawn_page_server().await;
    let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
    browser.open_tab(&format!("http://{addr}/")).await.unwrap();

    eval(
        &mut browser,
        r#"
        var open = indexedDB.open('people', 1);
        open.onupgradeneeded = function () {
          var store = open.result.createObjectStore('people', { keyPath: 'id' });
          store.createIndex('byEmail', 'email', { unique: true });
        };
        open.onsuccess = function () { globalThis.__db = open.result; };
        'open requested';
        "#,
    );

    eval(
        &mut browser,
        r#"
        globalThis.__errors = [];
        globalThis.__keys = [];
        var store = globalThis.__db.transaction('people', 'readwrite').objectStore('people');
        var ok = store.put({ id: 1, email: 'a@example.com', name: 'Ada' });
        ok.onsuccess = function () { globalThis.__keys.push(ok.result); };
        var clash = store.put({ id: 2, email: 'a@example.com', name: 'Grace' });
        clash.onerror = function () { globalThis.__errors.push(clash.error); };
        'writes queued';
        "#,
    );

    assert_eq!(numbers(eval(&mut browser, "globalThis.__keys")), vec![1.0]);
    let errors = eval(&mut browser, "globalThis.__errors");
    let message = errors
        .as_array()
        .and_then(|values| values.first())
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    assert!(
        message.contains("ConstraintError"),
        "expected a ConstraintError, got {message:?}"
    );

    // The rejected write must not have been stored.
    eval(
        &mut browser,
        r#"
        var store = globalThis.__db.transaction('people').objectStore('people');
        var all = store.getAll();
        all.onsuccess = function () { globalThis.__all = all.result; };
        var index = store.index('byEmail');
        globalThis.__unique = index.unique;
        globalThis.__keyPath = index.keyPath;
        globalThis.__indexNames = store.indexNames.slice ? store.indexNames.slice() : store.indexNames;
        var lookup = index.get('a@example.com');
        lookup.onsuccess = function () { globalThis.__byEmail = lookup.result; };
        var pk = index.getKey('a@example.com');
        pk.onsuccess = function () { globalThis.__byEmailKey = pk.result; };
        'reads queued';
        "#,
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__all"),
        json!([{"id": 1.0, "email": "a@example.com", "name": "Ada"}])
    );
    assert_eq!(eval(&mut browser, "globalThis.__unique"), json!(true));
    assert_eq!(eval(&mut browser, "globalThis.__keyPath"), json!("email"));
    assert_eq!(
        eval(&mut browser, "globalThis.__indexNames"),
        json!(["byEmail"])
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__byEmail.name"),
        json!("Ada")
    );
    assert_eq!(eval(&mut browser, "globalThis.__byEmailKey"), json!(1.0));

    // Index count and unique index cursor.
    eval(
        &mut browser,
        r#"
        var index = globalThis.__db.transaction('people').objectStore('people').index('byEmail');
        var counted = index.count();
        counted.onsuccess = function () { globalThis.__count = counted.result; };
        globalThis.__cursorValues = [];
        var cursor = index.openCursor();
        cursor.onsuccess = function () {
          var row = cursor.result;
          if (row) { globalThis.__cursorValues.push(row.value.name); row.continue(); }
        };
        'queued';
        "#,
    );
    assert_eq!(eval(&mut browser, "globalThis.__count"), json!(1.0));
    assert_eq!(
        eval(&mut browser, "globalThis.__cursorValues"),
        json!(["Ada"])
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn key_ranges_filter_reads_and_compound_keys_work() {
    let root = temp_dir("ranges");
    let addr = spawn_page_server().await;
    let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
    browser.open_tab(&format!("http://{addr}/")).await.unwrap();

    eval(
        &mut browser,
        r#"
        var open = indexedDB.open('ranges', 1);
        open.onupgradeneeded = function () { open.result.createObjectStore('nums'); };
        open.onsuccess = function () { globalThis.__db = open.result; };
        'open requested';
        "#,
    );
    eval(
        &mut browser,
        r#"
        var store = globalThis.__db.transaction('nums', 'readwrite').objectStore('nums');
        for (var n = 1; n <= 6; n += 1) { store.put({ n: n }, n); }
        store.put({ pair: true }, ['x', 2]);
        store.put({ pair: false }, ['x', 1]);
        'writes queued';
        "#,
    );

    eval(
        &mut browser,
        r#"
        globalThis.__range = IDBKeyRange.bound(2, 5, false, true);
        globalThis.__includes = [
          globalThis.__range.includes(2),
          globalThis.__range.includes(5),
          globalThis.__range.includes(6)
        ];
        var store = globalThis.__db.transaction('nums').objectStore('nums');
        var all = store.getAll(globalThis.__range);
        all.onsuccess = function () { globalThis.__ranged = all.result; };
        var counted = store.count(globalThis.__range);
        counted.onsuccess = function () { globalThis.__rangedCount = counted.result; };
        var lower = store.getAll(IDBKeyRange.lowerBound(5));
        lower.onsuccess = function () { globalThis.__lower = lower.result; };
        var upper = store.getAll(IDBKeyRange.upperBound(2, true));
        upper.onsuccess = function () { globalThis.__upper = upper.result; };
        var only = store.get(IDBKeyRange.only(4));
        only.onsuccess = function () { globalThis.__only = only.result; };
        'range reads queued';
        "#,
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__includes"),
        json!([true, false, false])
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__ranged"),
        json!([{"n": 2.0}, {"n": 3.0}, {"n": 4.0}])
    );
    assert_eq!(eval(&mut browser, "globalThis.__rangedCount"), json!(3.0));
    assert_eq!(eval(&mut browser, "globalThis.__only"), json!({"n": 4.0}));
    // Ordering is number < string < array, so the lowerBound(5) read returns
    // the high numbers before the compound string keys.
    assert_eq!(
        eval(&mut browser, "globalThis.__lower"),
        json!([{"n": 5.0}, {"n": 6.0}, {"pair": false}, {"pair": true}])
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__upper"),
        json!([{"n": 1.0}])
    );

    // Compound keys round-trip through get().
    eval(
        &mut browser,
        r#"
        var store = globalThis.__db.transaction('nums').objectStore('nums');
        var pair = store.get(['x', 2]);
        pair.onsuccess = function () { globalThis.__pair = pair.result; };
        var missing = store.get(['x', 3]);
        missing.onsuccess = function () { globalThis.__missing = missing.result === undefined; };
        var del = store.delete(IDBKeyRange.bound(1, 3));
        del.onsuccess = function () { globalThis.__deleted = true; };
        'more range reads queued';
        "#,
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__pair"),
        json!({"pair": true})
    );
    assert_eq!(eval(&mut browser, "globalThis.__missing"), json!(true));
    assert_eq!(eval(&mut browser, "globalThis.__deleted"), json!(true));
    eval(
        &mut browser,
        r#"
        var store = globalThis.__db.transaction('nums').objectStore('nums');
        var counted = store.count();
        counted.onsuccess = function () { globalThis.__remaining = counted.result; };
        'count queued';
        "#,
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__remaining"),
        json!(5.0),
        "numeric keys 4-6 and the two compound keys should be left"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn cursors_iterate_in_both_directions_with_ranges() {
    let root = temp_dir("cursors");
    let addr = spawn_page_server().await;
    let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
    browser.open_tab(&format!("http://{addr}/")).await.unwrap();

    eval(
        &mut browser,
        r#"
        var open = indexedDB.open('cursors', 1);
        open.onupgradeneeded = function () { open.result.createObjectStore('log'); };
        open.onsuccess = function () { globalThis.__db = open.result; };
        'open requested';
        "#,
    );
    eval(
        &mut browser,
        r#"
        var store = globalThis.__db.transaction('log', 'readwrite').objectStore('log');
        [1, 2, 3, 4, 5].forEach(function (n) { store.put({ n: n }, n); });
        'writes queued';
        "#,
    );

    eval(
        &mut browser,
        r#"
        function collect(cursorFactory, sink) {
          var request = cursorFactory();
          request.onsuccess = function () {
            var row = request.result;
            if (row) {
              sink.push([row.key, row.primaryKey, row.value.n]);
              row.continue();
            }
          };
        }
        var store = globalThis.__db.transaction('log').objectStore('log');
        globalThis.__ascending = [];
        collect(function () { return store.openCursor(); }, globalThis.__ascending);
        'ascending queued';
        "#,
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__ascending"),
        json!([
            [1.0, 1.0, 1.0],
            [2.0, 2.0, 2.0],
            [3.0, 3.0, 3.0],
            [4.0, 4.0, 4.0],
            [5.0, 5.0, 5.0]
        ])
    );

    eval(
        &mut browser,
        r#"
        globalThis.__descending = [];
        var store = globalThis.__db.transaction('log').objectStore('log');
        var request = store.openCursor(null, 'prev');
        request.onsuccess = function () {
          var row = request.result;
          if (row) { globalThis.__descending.push(row.key); row.continue(); }
        };
        'descending queued';
        "#,
    );
    assert_eq!(
        numbers(eval(&mut browser, "globalThis.__descending")),
        vec![5.0, 4.0, 3.0, 2.0, 1.0]
    );

    // Cursor honouring a key range.
    eval(
        &mut browser,
        r#"
        globalThis.__ranged = [];
        var store = globalThis.__db.transaction('log').objectStore('log');
        var request = store.openCursor(IDBKeyRange.lowerBound(3, true));
        request.onsuccess = function () {
          var row = request.result;
          if (row) { globalThis.__ranged.push(row.key); row.continue(); }
        };
        'range cursor queued';
        "#,
    );
    assert_eq!(
        numbers(eval(&mut browser, "globalThis.__ranged")),
        vec![4.0, 5.0]
    );

    // advance() skips entries and delete() removes the current record.
    eval(
        &mut browser,
        r#"
        globalThis.__advanced = [];
        var store = globalThis.__db.transaction('log', 'readwrite').objectStore('log');
        var request = store.openCursor();
        request.onsuccess = function () {
          var row = request.result;
          if (!row) { return; }
          if (row.key === 1) { row.advance(2); return; }
          if (row.key === 3) { row.delete(); }
          globalThis.__advanced.push(row.key);
          row.continue();
        };
        'advance cursor queued';
        "#,
    );
    assert_eq!(
        numbers(eval(&mut browser, "globalThis.__advanced")),
        vec![3.0, 4.0, 5.0]
    );
    eval(
        &mut browser,
        r#"
        var store = globalThis.__db.transaction('log').objectStore('log');
        var counted = store.count();
        counted.onsuccess = function () { globalThis.__afterDelete = counted.result; };
        'count queued';
        "#,
    );
    assert_eq!(
        eval(&mut browser, "globalThis.__afterDelete"),
        json!(4.0),
        "cursor.delete() should remove exactly one record"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn legacy_schema_files_still_load() {
    let root = temp_dir("legacy");
    let addr = spawn_page_server().await;
    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser.open_tab(&format!("http://{addr}/")).await.unwrap();
        eval(
            &mut browser,
            r#"
            var open = indexedDB.open('legacy', 1);
            open.onupgradeneeded = function () { open.result.createObjectStore('items'); };
            open.onsuccess = function () { globalThis.__db = open.result; };
            'open requested';
            "#,
        );
        eval(
            &mut browser,
            r#"
            var store = globalThis.__db.transaction('items', 'readwrite').objectStore('items');
            store.put({ text: 'old' }, 'k1');
            'write queued';
            "#,
        );
        assert_eq!(
            eval(
                &mut browser,
                r#"
                var store = globalThis.__db.transaction('items').objectStore('items');
                var request = store.get('k1');
                request.onsuccess = function () { globalThis.__check = request.result.text; };
                'queued';
                "#,
            ),
            json!("queued")
        );
        assert_eq!(eval(&mut browser, "globalThis.__check"), json!("old"));
    }

    // Rewrite the persisted file in the pre-indexes/autoIncrement format:
    // plain string keys and no metadata block.
    let index_dir = root.join("profiles/agent/storage/indexeddb");
    let files = idb_files(&index_dir);
    assert_eq!(files.len(), 1, "expected a single persisted database file");
    let raw = std::fs::read_to_string(&files[0]).unwrap();
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    let version = parsed.get("version").cloned().unwrap_or(json!(1));
    let stored = parsed
        .get("stores")
        .and_then(|stores| stores.get("items"))
        .cloned()
        .unwrap_or(json!({}));
    let mut legacy_store = serde_json::Map::new();
    for value in stored
        .as_object()
        .map(|entries| entries.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        legacy_store.insert("k1".to_owned(), value);
    }
    let legacy = json!({
        "version": version,
        "stores": { "items": legacy_store }
    });
    std::fs::write(&files[0], serde_json::to_vec(&legacy).unwrap()).unwrap();

    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/reload"))
            .await
            .unwrap();
        eval(
            &mut browser,
            r#"
            var open = indexedDB.open('legacy');
            open.onsuccess = function () { globalThis.__db = open.result; };
            'reopened';
            "#,
        );
        eval(
            &mut browser,
            r#"
            var store = globalThis.__db.transaction('items').objectStore('items');
            var request = store.get('k1');
            request.onsuccess = function () { globalThis.__value = request.result; };
            var all = store.getAll();
            all.onsuccess = function () { globalThis.__size = all.result.length; };
            var counted = store.count();
            counted.onsuccess = function () { globalThis.__count = counted.result; };
            'reads queued';
            "#,
        );
        assert_eq!(
            eval(&mut browser, "globalThis.__value"),
            json!({"text": "old"}),
            "legacy string keys must still be readable"
        );
        assert_eq!(eval(&mut browser, "globalThis.__size"), json!(1.0));
        assert_eq!(eval(&mut browser, "globalThis.__count"), json!(1.0));
        // A write on the legacy database must keep working and persist.
        eval(
            &mut browser,
            r#"
            var store = globalThis.__db.transaction('items', 'readwrite').objectStore('items');
            var request = store.put({ text: 'new' }, 'k2');
            request.onsuccess = function () { globalThis.__written = true; };
            'write queued';
            "#,
        );
        assert_eq!(eval(&mut browser, "globalThis.__written"), json!(true));
    }

    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/final"))
            .await
            .unwrap();
        eval(
            &mut browser,
            r#"
            var open = indexedDB.open('legacy');
            open.onsuccess = function () { globalThis.__db = open.result; };
            'reopened';
            "#,
        );
        eval(
            &mut browser,
            r#"
            var store = globalThis.__db.transaction('items').objectStore('items');
            var request = store.getAll();
            request.onsuccess = function () { globalThis.__size = request.result.length; };
            'reads queued';
            "#,
        );
        assert_eq!(eval(&mut browser, "globalThis.__size"), json!(2.0));
    }

    let _ = std::fs::remove_dir_all(root);
}
