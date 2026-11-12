mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use common::spawn_test_server;
use nexium::api::service::ApiService;
use nexium::networking::{HttpClient, Request, parse_url};
use nexium::{Browser, Config};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-stability-{tag}-{}-{nanos}-{count}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[tokio::test]
async fn tab_limit_is_enforced() {
    let addr = spawn_test_server().await;
    let config = Config {
        max_tabs: 2,
        ..Config::default()
    };
    let mut browser = Browser::new(config);
    browser
        .open_tab(&format!("http://{addr}/one"))
        .await
        .unwrap();
    browser
        .open_tab(&format!("http://{addr}/two"))
        .await
        .unwrap();
    let error = browser
        .open_tab(&format!("http://{addr}/one"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("tab limit"), "{error}");
    assert_eq!(browser.tab_count(), 2);
}

#[tokio::test]
async fn private_network_requests_can_be_blocked() {
    let addr = spawn_test_server().await;
    let config = Config {
        block_private_networks: true,
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let url = parse_url(&format!("http://{addr}/one")).unwrap();
    let error = client.fetch(Request::get(url)).await.unwrap_err();
    assert!(error.to_string().contains("private-network"), "{error}");
    assert!(client.network_events().iter().any(|event| {
        event
            .error
            .as_deref()
            .is_some_and(|error| error.contains("private-network"))
    }));
}

#[tokio::test]
async fn idle_api_sessions_are_swept() {
    let config = Config {
        api_session_idle_timeout_ms: 10,
        ..Config::default()
    };
    let service = ApiService::new(config);
    let auth = service.local_auth("sweep-agent");
    let created = service.create(&auth, Default::default()).await.unwrap();
    let id = created["session_id"].as_str().unwrap().to_owned();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert_eq!(service.sweep_idle().await, 1);
    assert!(service.status(&auth, Some(&id)).await.is_err());
}

#[tokio::test]
async fn repeated_navigation_and_rendering_stays_bounded() {
    let addr = spawn_test_server().await;
    let mut browser = Browser::new(Config::default());
    browser
        .open_tab(&format!("http://{addr}/interact"))
        .await
        .unwrap();
    let viewport = browser.viewport();
    for index in 0..6 {
        let page = browser
            .navigate(&format!(
                "http://{addr}/{}",
                if index % 2 == 0 { "two" } else { "interact" }
            ))
            .await
            .unwrap();
        let rendered = browser
            .render(
                &page,
                nexium::RenderOptions {
                    viewport,
                    max_height: 2000,
                    load_images: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(rendered.image.width(), viewport.width as u32);
        assert!(browser.tab_count() <= 1);
    }
    assert_eq!(browser.client().network_events().len(), 7);
}

#[tokio::test]
async fn dropping_a_profile_browser_recovers_its_session() {
    let root = temp_dir("recovery");
    let addr = spawn_test_server().await;
    let config = Config {
        data_dir: root.clone(),
        ..Config::default()
    };
    {
        let mut browser = Browser::with_profile(config.clone(), "agent").unwrap();
        browser
            .open_tab(&format!("http://{addr}/one"))
            .await
            .unwrap();
    }
    let mut browser = Browser::with_profile(config, "agent").unwrap();
    assert!(browser.restore_profile_session().await.is_ok());
    assert_eq!(browser.tabs().len(), 1);
    assert_eq!(
        browser.active_page().unwrap().title().as_deref(),
        Some("One")
    );
    let _ = std::fs::remove_dir_all(root);
}
