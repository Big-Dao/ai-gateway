mod admin;
mod circuit_breaker;
mod json_logger;
mod log_buffer;
mod metrics;
mod middleware;
mod retry;
mod routes;
mod state;
mod static_files;

use std::sync::Arc;
use tracing::info;

use gateway_core::config::AppConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing with JSON output + in-memory log buffer
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let log_buffer_layer = log_buffer::LogBufferLayer;

    tracing_subscriber::registry()
        // Human-readable stdout for development (comment out in production)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(false),
        )
        // Structured JSON to stderr for log aggregation
        .with(crate::json_logger::JsonLoggerLayer)
        // In-memory ring buffer for Admin UI
        .with(log_buffer_layer)
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    "info,gateway_server=debug,gateway_core=debug,providers=debug".into()
                }),
        )
        .init();

    // Load configuration
    let config_path = std::env::var("CONFIG_PATH").ok();
    let config = AppConfig::load(config_path.as_deref())?;
    info!(
        providers_count = config.providers.len(),
        "Configuration loaded"
    );

    // Build app state
    let state = Arc::new(state::AppState::new(config).await?);

    // Secure startup check: refuse to start with auth enabled but no keys
    if state.config.read().await.auth.enabled {
        let store = state.auth_store.read().await;
        if store.list_ids().is_empty() {
            eprintln!(
                "Refusing to start: auth.enabled=true but no API keys configured.\n\
                 Either set [auth].api_keys in config.toml or set AUTH_ENABLED=false."
            );
            std::process::exit(1);
        }
    }

    // Build router
    let app = routes::build_router(state.clone());

    // Start server
    let addr = {
        let config = state.config.read().await;
        format!("{}:{}", config.server.host, config.server.port)
    };
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(address = %addr, "AI Gateway listening");
    info!(admin_url = format!("http://{}/admin", addr), "Admin UI ready");

    // Graceful shutdown: stop accepting new connections, let in-flight
    // requests drain, then exit. SIGINT/SIGTERM typically signal "drain and
    // stop" from orchestrators like Kubernetes.
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            info!("Shutdown signal received, draining in-flight requests...");
        })
        .await?;

    info!("Server shut down cleanly");
    Ok(())
}
