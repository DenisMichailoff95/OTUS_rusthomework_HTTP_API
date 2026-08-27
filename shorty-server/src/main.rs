//! Точка входа с выбором хранилища.

use std::sync::Arc;

use domain::LinkRepository;
use shorty_server::config::StorageType;
use shorty_server::{AppState, Config, build_router, cleanup, grpc};
use storage::{Cache, CacheConfig, InMemoryRepo, PostgresRepo, spawn_pool_metrics};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const CLEANUP_PERIOD: std::time::Duration = std::time::Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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

/// Инициализация tracing
fn init_tracing() {
    let json_logs = std::env::var("LOG_FORMAT").is_ok_and(|f| f == "json");
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,sqlx=warn"));

    if json_logs {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

/// Создание репозитория в зависимости от конфигурации
async fn create_repository(config: &Config) -> Arc<dyn LinkRepository> {
    match config.storage_type {
        StorageType::InMemory => {
            tracing::info!("Using in-memory storage");
            Arc::new(InMemoryRepo::new())
        }
        StorageType::Postgres => {
            tracing::info!("Using PostgreSQL storage");
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(std::time::Duration::from_secs(3))
                .connect(&config.database_url)
                .await
                .expect("failed to connect to PostgreSQL");

            sqlx::migrate!("../crates/storage/migrations")
                .run(&pool)
                .await
                .expect("failed to run migrations");

            spawn_pool_metrics(pool.clone());

            Arc::new(PostgresRepo::new(pool))
        }
    }
}

/// Создание кеша
async fn create_cache(config: &Config) -> Cache {
    let cache_config = CacheConfig {
        ttl_secs: config.cache_ttl_secs,
        jitter_secs: config.cache_jitter_secs,
        op_timeout_ms: config.cache_op_timeout_ms,
    };

    match &config.redis_url {
        Some(url) => {
            tracing::info!("Redis cache enabled: {}", url);
            Cache::connect(url, cache_config).await
        }
        None => {
            tracing::warn!("Redis cache disabled");
            Cache::disabled()
        }
    }
}

async fn run() {
    init_tracing();

    let config = Arc::new(Config::from_env());
    let http_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let grpc_addr =
        std::env::var("GRPC_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".to_string());

    // Инициализация метрик
    let _metrics_handle = storage::telemetry::init_metrics();
    tracing::info!("metrics initialized");

    // Создаем репозиторий и кеш
    let repo = create_repository(&config).await;
    let cache = create_cache(&config).await;
    let stats_storage = Arc::new(domain::stats::StatsStorage::new());

    let auth_config = config.auth.clone().map(Arc::new);
    let db_pool = match config.storage_type {
        StorageType::Postgres => {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(std::time::Duration::from_secs(3))
                .connect(&config.database_url)
                .await
                .expect("failed to connect to PostgreSQL");
            Some(pool)
        }
        _ => None,
    };

    let shutdown_token = CancellationToken::new();

    let state = AppState {
        repo: repo.clone(),
        stats_storage: stats_storage.clone(),
        config: config.clone(),
        cache: cache.clone(),
        metrics_handle: storage::telemetry::init_metrics(),
        auth: auth_config.clone(),
        shutdown_token: shutdown_token.clone(),
        db_pool,
    };

    // Запуск уборщика
    let tracker = TaskTracker::new();
    cleanup::spawn_cleaner(repo, CLEANUP_PERIOD, &tracker, shutdown_token.clone());

    // Запуск HTTP сервера
    let http_listener = tokio::net::TcpListener::bind(&http_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {http_addr}: {e}"));

    tracing::info!(%http_addr, "HTTP server listening");
    tracing::info!(
        storage_type = ?config.storage_type,
        cache_enabled = config.is_cache_enabled(),
        auth_enabled = auth_config.is_some(),
        "server configuration"
    );

    let grpc_state = state.clone();
    let grpc_shutdown = shutdown_token.clone();
    let grpc_task = tokio::spawn(async move {
        let server = grpc::GrpcServer::new(grpc_state);
        let addr = grpc_addr.parse().expect("invalid gRPC address");
        server.serve(addr, grpc_shutdown).await;
    });

    let http_state = state.clone();
    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, build_router(http_state))
            .with_graceful_shutdown(async move {
                shutdown_signal().await;
                shutdown_token.cancel();
            })
            .await
            .expect("server error");
    });

    tokio::select! {
        _ = http_task => {},
        _ = grpc_task => {},
    };

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
