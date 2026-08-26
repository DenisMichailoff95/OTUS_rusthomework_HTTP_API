//! Observability: метрики для sqlx пула.

use sqlx::PgPool;
use std::sync::OnceLock;
use std::time::Duration;

static METRICS_HANDLE: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();

/// Запускает сбор метрик пула соединений sqlx
pub fn spawn_pool_metrics(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        loop {
            ticker.tick().await;
            metrics::gauge!("db_pool_connections").set(pool.size() as f64);
            metrics::gauge!("db_pool_idle_connections").set(pool.num_idle() as f64);
        }
    });
}

/// Инициализация Prometheus метрик
pub fn init_metrics() -> metrics_exporter_prometheus::PrometheusHandle {
    use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};

    METRICS_HANDLE
        .get_or_init(|| {
            PrometheusBuilder::new()
                .set_buckets_for_metric(
                    Matcher::Full("http_request_duration_seconds".into()),
                    &[
                        0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
                    ],
                )
                .expect("histogram buckets are not empty")
                .install_recorder()
                .expect("install prometheus recorder")
        })
        .clone()
}
