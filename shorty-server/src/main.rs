//! Тонкий бинарник.

use std::{sync::Arc, time::Duration};

use domain::LinkRepository;
use shorty_server::{AppState, Config, build_router, cleanup};
use storage::InMemoryRepo;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing_subscriber::EnvFilter;

const CLEANUP_PERIOD: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Ctrl+C received, stopping server"),
        _ = terminate => tracing::info!("SIGTERM received, stopping server"),
    }
}

fn load_config() -> Config {
    let mut config = Config::default();
    
    if let Ok(val) = std::env::var("CODE_LENGTH") {
        config.code_length = val.parse().expect("CODE_LENGTH must be a number");
    }
    
    if let Ok(val) = std::env::var("RATE_LIMIT_CAPACITY") {
        config.rate_limit_capacity = val.parse().expect("RATE_LIMIT_CAPACITY must be a number");
    }
    
    if let Ok(val) = std::env::var("RATE_LIMIT_PERIOD_SECS") {
        config.rate_limit_period_secs = val.parse().expect("RATE_LIMIT_PERIOD_SECS must be a number");
    }
    
    // CLEANUP_INTERVAL_SECS - пока используем константу, но переменная доступна
    if let Ok(_val) = std::env::var("CLEANUP_INTERVAL_SECS") {
        // Для простоты оставим как есть, но можно использовать для переопределения
        // В будущем можно добавить: config.cleanup_interval = _val.parse()...
    }
    
    config
}

async fn run() {
    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));

    let config = Arc::new(load_config());
    let repo: Arc<dyn LinkRepository> = Arc::new(InMemoryRepo::new());
    let stats_storage = Arc::new(domain::stats::StatsStorage::new());
    
    let state = AppState {
        repo: repo.clone(),
        stats_storage: stats_storage.clone(),
        config: config.clone(),
    };

    let shutdown_token = CancellationToken::new();
    let tracker = TaskTracker::new();
    cleanup::spawn_cleaner(repo, CLEANUP_PERIOD, &tracker, shutdown_token.clone());

    tracing::info!(%addr, "shorty server listening");
    tracing::info!(rate_limit_capacity = config.rate_limit_capacity, "rate limiter configured");

    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    shutdown_token.cancel();
    tracker.close();
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, tracker.wait()).await {
        Ok(()) => tracing::info!("all background tasks finished, bye"),
        Err(_) => tracing::warn!(
            timeout_secs = SHUTDOWN_TIMEOUT.as_secs(),
            "background tasks did not finish in time, exiting anyway"
        ),
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Ok(n) = std::env::var("SHORTY_WORKER_THREADS") {
        let n: usize = n.parse().expect("SHORTY_WORKER_THREADS must be a number");
        builder.worker_threads(n);
        tracing::info!(workers = n, "worker thread count overridden");
    }
    builder
        .build()
        .expect("failed to build tokio runtime")
        .block_on(run());
}