mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use common::spawn_test_server;
use nexium::Browser;
use nexium::Config;
use nexium::networking::{HttpClient, Request, parse_url};
use nexium::storage::{FsStore, Profile};
use serde_json::json;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-test-{tag}-{}-{nanos}-{count}",
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

#[test]
fn filesystem_store_is_atomic_and_path_safe() {
    let root = temp_dir("fs");
    let store = FsStore::new(&root).unwrap();
    store
        .write_json("nested/value.json", &json!({"answer": 42}))
        .unwrap();
    let value: serde_json::Value = store.read_json("nested/value.json").unwrap().unwrap();
    assert_eq!(value["answer"], 42);
    store.write_atomic("raw.bin", b"abc").unwrap();
    assert_eq!(store.read("raw.bin").unwrap().unwrap(), b"abc");
    assert!(store.resolve("../escape").is_err());
    assert!(store.resolve("/absolute").is_err());
    assert!(store.read("missing").unwrap().is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn profiles_persist_preferences_cookies_storage_and_history() {
    let root = temp_dir("profile");
    {
        let profile = Profile::open(&root, "agent").unwrap();
        let mut preferences = profile.preferences().unwrap();
        preferences.homepage = Some("https://example.com/".to_owned());
        profile.save_preferences(&preferences).unwrap();
        profile
            .save_cookies(&[nexium::networking::CookieRecord {
                name: "session".to_owned(),
                value: "abc".to_owned(),
                domain: "example.com".to_owned(),
                path: "/".to_owned(),
                secure: false,
                http_only: true,
                host_only: true,
                expires: None,
                same_site: nexium::networking::SameSite::Lax,
            }])
            .unwrap();
        let mut storage = std::collections::HashMap::new();
        storage.insert("k".to_owned(), "v".to_owned());
        profile
            .save_local_storage("https://example.com", &storage)
            .unwrap();
        profile
            .append_history("https://example.com/a", Some("A"))
            .unwrap();
    }
    {
        let profile = Profile::open(&root, "agent").unwrap();
        assert_eq!(
            profile.preferences().unwrap().homepage.as_deref(),
            Some("https://example.com/")
        );
        let cookies = profile.load_cookies().unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].value, "abc");
        let storage = profile.load_local_storage("https://example.com").unwrap();
        assert_eq!(storage.get("k").map(String::as_str), Some("v"));
        let history = profile.history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].url, "https://example.com/a");
        // A different origin must not see the stored keys.
        assert!(
            profile
                .load_local_storage("https://other.example")
                .unwrap()
                .is_empty()
        );
    }
    assert!(Profile::open(&root, "../bad").is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn cookies_persist_across_profile_sessions() {
    let root = temp_dir("cookies");
    let addr = spawn_test_server().await;
    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/set-cookie"))
            .await
            .unwrap();
        let page = browser
            .open_tab(&format!("http://{addr}/echo-cookie"))
            .await
            .unwrap();
        let _ = page;
        let active = browser.active_page().unwrap();
        let text = active
            .document
            .text_content(active.document.document_node());
        assert!(text.contains("session=abc123"), "{text}");
        assert!(root.join("profiles/agent/cookies/cookies.json").exists());
    }
    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/echo-cookie"))
            .await
            .unwrap();
        let active = browser.active_page().unwrap();
        let text = active
            .document
            .text_content(active.document.document_node());
        assert!(text.contains("session=abc123"), "{text}");
    }
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn cookies_are_isolated_between_profiles() {
    let root = temp_dir("isolation");
    let addr = spawn_test_server().await;
    let mut first = Browser::with_profile(config_with_data_dir(&root), "one").unwrap();
    first
        .open_tab(&format!("http://{addr}/set-cookie"))
        .await
        .unwrap();
    let mut second = Browser::with_profile(config_with_data_dir(&root), "two").unwrap();
    second
        .open_tab(&format!("http://{addr}/echo-cookie"))
        .await
        .unwrap();
    let active = second.active_page().unwrap();
    let text = active
        .document
        .text_content(active.document.document_node());
    assert!(!text.contains("session=abc123"), "{text}");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn local_storage_persists_per_profile_origin() {
    let root = temp_dir("localstorage");
    let addr = spawn_test_server().await;
    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/ls-write"))
            .await
            .unwrap();
    }
    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/ls-read"))
            .await
            .unwrap();
        let active = browser.active_page().unwrap();
        let body = active.select("body").unwrap().unwrap();
        assert_eq!(active.document.get_attribute(body, "data-v"), Some("v"));
    }
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn indexeddb_persists_across_profile_sessions() {
    let root = temp_dir("indexeddb");
    let addr = spawn_test_server().await;
    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/idb-write"))
            .await
            .unwrap();
        let active = browser.active_page().unwrap();
        let body = active.select("body").unwrap().unwrap();
        assert_eq!(active.document.get_attribute(body, "data-put"), Some("ok"));
        assert_eq!(
            active.document.get_attribute(body, "data-done"),
            Some("yes")
        );
    }
    let idb_dir = root.join("profiles/agent/storage/indexeddb");
    assert!(idb_dir.exists(), "IndexedDB directory was not created");
    assert!(
        std::fs::read_dir(&idb_dir).unwrap().next().is_some(),
        "IndexedDB database was not persisted"
    );
    {
        let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/idb-read"))
            .await
            .unwrap();
        let active = browser.active_page().unwrap();
        let body = active.select("body").unwrap().unwrap();
        assert_eq!(active.document.get_attribute(body, "data-v"), Some("hello"));
    }
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn tabs_navigation_history_and_reload() {
    let addr = spawn_test_server().await;
    let mut browser = Browser::new(Config::default());
    let first = browser
        .open_tab(&format!("http://{addr}/one"))
        .await
        .unwrap();
    assert_eq!(
        browser.active_page().unwrap().title().as_deref(),
        Some("One")
    );

    let second = browser
        .open_tab(&format!("http://{addr}/two"))
        .await
        .unwrap();
    assert_eq!(browser.tabs().len(), 2);
    assert_eq!(browser.active_tab().unwrap().id, second);

    assert!(browser.switch_tab(first));
    assert_eq!(
        browser.active_page().unwrap().title().as_deref(),
        Some("One")
    );

    let page = browser
        .navigate(&format!("http://{addr}/two"))
        .await
        .unwrap();
    assert_eq!(page.title().as_deref(), Some("Two"));
    let back = browser.back().await.unwrap().unwrap();
    assert_eq!(back.title().as_deref(), Some("One"));
    let forward = browser.forward().await.unwrap().unwrap();
    assert_eq!(forward.title().as_deref(), Some("Two"));
    let reloaded = browser.reload().await.unwrap();
    assert_eq!(reloaded.title().as_deref(), Some("Two"));

    assert!(browser.close_tab(first));
    assert_eq!(browser.tabs().len(), 1);
    assert!(browser.close_tab(second));
    assert!(browser.tabs().is_empty());
    assert!(browser.active_page().is_none());
}

#[tokio::test]
async fn session_save_and_restore() {
    let addr = spawn_test_server().await;
    let file = temp_dir("session").join("session.json");
    let mut browser = Browser::new(Config::default());
    browser
        .open_tab(&format!("http://{addr}/one"))
        .await
        .unwrap();
    browser
        .open_tab(&format!("http://{addr}/two"))
        .await
        .unwrap();
    browser
        .open_tab(&format!("http://{addr}/ss-write"))
        .await
        .unwrap();
    assert_eq!(
        browser.tabs()[2]
            .session_storage
            .get("s")
            .map(String::as_str),
        Some("per-tab")
    );
    browser.save_session(&file).unwrap();
    assert!(file.exists());

    let mut restored = Browser::new(Config::default());
    restored.restore_session(&file).await.unwrap();
    assert_eq!(restored.tabs().len(), 3);
    assert_eq!(
        restored
            .active_tab()
            .unwrap()
            .session_storage
            .get("s")
            .map(String::as_str),
        Some("per-tab")
    );
    assert_eq!(restored.tabs()[0].title.as_deref(), Some("One"));
    assert_eq!(restored.tabs()[1].title.as_deref(), Some("Two"));
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[tokio::test]
async fn profile_history_is_recorded() {
    let root = temp_dir("history");
    let addr = spawn_test_server().await;
    let mut browser = Browser::with_profile(config_with_data_dir(&root), "agent").unwrap();
    browser
        .open_tab(&format!("http://{addr}/one"))
        .await
        .unwrap();
    browser
        .navigate(&format!("http://{addr}/two"))
        .await
        .unwrap();
    let history = browser.history();
    assert!(history.len() >= 2);
    assert!(
        history
            .iter()
            .any(|entry| entry.title.as_deref() == Some("One"))
    );
    assert!(history.iter().any(|entry| entry.url.ends_with("/two")));
    assert!(root.join("profiles/agent/history/history.json").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn http_cache_persists_on_disk() {
    use std::sync::atomic::Ordering;

    let root = temp_dir("cache");
    let cache_dir = root.join("cache");
    let config = Config::default();
    let (addr, count) = common::spawn_counting_server("max-age=60").await;
    let url = parse_url(&format!("http://{addr}/cached")).unwrap();

    let first = HttpClient::new(&config);
    first.set_cache_dir(&cache_dir).unwrap();
    let body = first.fetch(Request::get(url.clone())).await.unwrap().body;
    let _ = first.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(first);

    let second = HttpClient::new(&config);
    second.set_cache_dir(&cache_dir).unwrap();
    let cached = second.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(cached.body, body);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    second.clear_cache();
    let refetched = second.fetch(Request::get(url)).await.unwrap();
    assert_eq!(refetched.status.as_u16(), 200);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(root);
}
