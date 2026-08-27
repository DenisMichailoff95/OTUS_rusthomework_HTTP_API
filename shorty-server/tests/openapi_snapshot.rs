//! Snapshot-тест для OpenAPI спецификации.
//!
//! Запуск: `cargo test --test openapi_snapshot -- --ignored`
//! Для обновления снепшота: `UPDATE_SNAPSHOT=1 cargo test --test openapi_snapshot -- --ignored`

use std::sync::Arc;

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

use shorty_server::{AppState, Config, build_router};
use storage::{Cache, InMemoryRepo, telemetry::init_metrics};

fn test_app() -> Router {
    let repo = Arc::new(InMemoryRepo::new());
    let stats_storage = Arc::new(domain::stats::StatsStorage::new());
    let config = Arc::new(Config::default());

    let state = AppState {
        repo,
        stats_storage,
        config,
        cache: Cache::disabled(),
        metrics_handle: init_metrics(),
        auth: None,
    };

    build_router(state)
}

#[tokio::test]
#[ignore = "snapshot test - run with UPDATE_SNAPSHOT=1 to update"]
async fn openapi_snapshot() {
    let app = test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api-docs/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let openapi_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // Форматируем для сравнения
    let pretty = serde_json::to_string_pretty(&openapi_json).unwrap();

    let snapshot_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("openapi.json");

    // Проверяем наличие переменной для обновления
    let update = std::env::var("UPDATE_SNAPSHOT").is_ok();

    if update {
        std::fs::create_dir_all(snapshot_path.parent().unwrap()).unwrap();
        std::fs::write(&snapshot_path, pretty).unwrap();
        eprintln!("✅ Updated snapshot: {}", snapshot_path.display());
        return;
    }

    // Сравниваем с существующим снепшотом
    let expected = std::fs::read_to_string(&snapshot_path).unwrap_or_else(|_| {
        panic!(
            "Snapshot not found at {}. Run with UPDATE_SNAPSHOT=1 to create it.",
            snapshot_path.display()
        )
    });

    assert_eq!(
        pretty, expected,
        "OpenAPI spec changed. Run with UPDATE_SNAPSHOT=1 to update."
    );
}
