//! Интеграционные тесты с реальным PostgreSQL.
//!
//! Для запуска: `cargo test --test postgres_integration -- --ignored`
//! Требует поднятого PostgreSQL через docker-compose.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode},
};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use shorty_server::{
    AppState, Config,
    api::{dto::LinkResponse, error::ErrorBody},
    build_router,
};
use storage::{Cache, PostgresRepo, telemetry::init_metrics};
use tower::ServiceExt;

async fn postgres_state() -> AppState {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/shorty".into());

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&database_url)
        .await
        .expect("failed to connect to test PostgreSQL");

    sqlx::migrate!("../crates/storage/migrations")
        .run(&pool)
        .await
        .expect("failed to run migrations");

    sqlx::query("TRUNCATE TABLE links RESTART IDENTITY CASCADE")
        .execute(&pool)
        .await
        .expect("failed to truncate links");

    let repo = Arc::new(PostgresRepo::new(pool));
    let stats_storage = Arc::new(domain::stats::StatsStorage::new());
    let config = Arc::new(Config::default());

    AppState {
        repo,
        stats_storage,
        config,
        cache: Cache::disabled(),
        metrics_handle: init_metrics(),
    }
}

async fn postgres_app() -> Router {
    build_router(postgres_state().await)
}

fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn put_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn delete_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

async fn json_body<T: DeserializeOwned>(response: Response<Body>) -> T {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "invalid json body: {e}; raw: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn postgres_crud_happy_path() {
    let app = postgres_app().await;

    let response = app
        .clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({
                "target_url": "https://example.com/pg-test",
                "custom_code": "pg-crud",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(get("/api/v1/links/pg-crud"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: LinkResponse = json_body(response).await;
    assert_eq!(body.code, "pg-crud");
    assert_eq!(body.target_url, "https://example.com/pg-test");

    let response = app
        .clone()
        .oneshot(put_json(
            "/api/v1/links/pg-crud",
            serde_json::json!({
                "target_url": "https://example.com/updated",
                "version": 1,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let updated: LinkResponse = json_body(response).await;
    assert_eq!(updated.target_url, "https://example.com/updated");
    assert_eq!(updated.version, 2);

    let response = app
        .clone()
        .oneshot(delete_request("/api/v1/links/pg-crud"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app
        .clone()
        .oneshot(get("/api/v1/links/pg-crud"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn postgres_404_when_missing() {
    let app = postgres_app().await;
    let response = app
        .clone()
        .oneshot(get("/api/v1/links/does-not-exist"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: ErrorBody = json_body(response).await;
    assert_eq!(body.code, "not_found");
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn postgres_optimistic_locking_conflict() {
    let app = postgres_app().await;

    app.clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({
                "target_url": "https://example.com/ol",
                "custom_code": "ol-test",
            }),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(put_json(
            "/api/v1/links/ol-test",
            serde_json::json!({
                "target_url": "https://example.com/v2",
                "version": 1,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(put_json(
            "/api/v1/links/ol-test",
            serde_json::json!({
                "target_url": "https://example.com/v3",
                "version": 1,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn postgres_cache_invalidation_on_write() {
    let app = postgres_app().await;

    app.clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({
                "target_url": "https://example.com/cache-test",
                "custom_code": "cache-inv",
            }),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(get("/api/v1/links/cache-inv"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first: LinkResponse = json_body(response).await;
    assert_eq!(first.target_url, "https://example.com/cache-test");

    let response = app
        .clone()
        .oneshot(put_json(
            "/api/v1/links/cache-inv",
            serde_json::json!({
                "target_url": "https://example.com/cache-updated",
                "version": 1,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(get("/api/v1/links/cache-inv"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let after: LinkResponse = json_body(response).await;
    assert_eq!(after.target_url, "https://example.com/cache-updated");
}
