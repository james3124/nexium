use tracing_subscriber::{EnvFilter, fmt};

/// Initialise process-wide logging.
///
/// Honours `RUST_LOG` (defaults to `nexium=info`). Safe to call more than
/// once; subsequent calls are ignored.
pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("nexium=info,warn"));

    let _ = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init();
}
