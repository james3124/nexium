use std::path::Path;

use nexium::Config;

#[test]
fn shipped_default_config_parses() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/default.toml");
    let config = Config::load(&path).unwrap();
    assert_eq!(config.viewport_width, 1280);
    assert_eq!(config.viewport_height, 720);
    assert_eq!(config.timeout_ms, 30_000);
    assert!(config.storage_enabled);
    assert!(config.max_response_bytes >= 1024 * 1024);
}

#[test]
fn missing_config_falls_back_to_defaults() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/does-not-exist.toml");
    let config = Config::load_or_default(&path).unwrap();
    assert_eq!(config.max_redirects, 10);
}

#[test]
fn invalid_config_is_reported() {
    let path = std::env::temp_dir().join("nexium-invalid-config.toml");
    std::fs::write(&path, "timeout_ms = \"not a number\"").unwrap();
    let error = Config::load(&path).unwrap_err();
    assert!(matches!(error, nexium::NxError::Config(_)));
    let _ = std::fs::remove_file(path);
}
