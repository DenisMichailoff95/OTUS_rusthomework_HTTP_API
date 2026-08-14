//! Тесты HTTP-слоя без реального сокета.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use shorty_server::{
    AppState, Config,
    api::{dto::LinkResponse, error::ErrorBody},
    build_router,
};
use storage::InMemoryRepo;
use tower::ServiceExt;

fn test_state() -> (AppState, Arc<InMemoryRepo>) {
    let repo = Arc::new(InMemoryRepo::new());
    let stats_storage = Arc::new(domain::stats::StatsStorage::new());
    let mut config = Config::default();
    config.rate_limit_capacity = 10000;
    config.rate_limit_period_secs = 60;

    let state = AppState {
        repo: repo.clone(),
        stats_storage: stats_storage.clone(),
        config: Arc::new(config),
    };
    (state, repo)
}

fn test_app() -> Router {
    build_router(test_state().0)
}

fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, mime::APPLICATION_JSON.as_ref())
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
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

// ---------------------------------------------------------------------------
// 1. Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_then_get() {
    let app = test_app();

    let response = app
        .clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({
                "target_url": "https://example.com/page",
                "custom_code": "promo2026",
                "ttl_seconds": 3600,
            }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/api/v1/links/promo2026"
    );
    assert!(response.headers().contains_key("x-request-id"));

    let created: LinkResponse = json_body(response).await;
    assert_eq!(created.code, "promo2026");
    assert_eq!(created.target_url, "https://example.com/page");
    assert_eq!(created.hits, 0);
    assert!(created.expires_at_unix.is_some());

    let response = app.oneshot(get("/api/v1/links/promo2026")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched: LinkResponse = json_body(response).await;
    assert_eq!(fetched.code, "promo2026");
    assert_eq!(fetched.hits, 0);
}

#[tokio::test]
async fn create_generates_code_when_custom_is_absent() {
    let response = test_app()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({"target_url": "https://example.com/"}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let created: LinkResponse = json_body(response).await;
    assert_eq!(created.code.len(), Config::default().code_length);
    assert!(
        created
            .code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    );
    assert_eq!(created.expires_at_unix, None);
}

// ---------------------------------------------------------------------------
// 2. Redirect и счетчик
// ---------------------------------------------------------------------------

#[tokio::test]
async fn redirect_increments_counter() {
    let app = test_app();
    app.clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({"target_url": "https://example.com/hot", "custom_code": "hot-link"}),
        ))
        .await
        .unwrap();

    let response = app.clone().oneshot(get("/hot-link")).await.unwrap();
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://example.com/hot"
    );

    let response = app.oneshot(get("/api/v1/links/hot-link")).await.unwrap();
    let stats: LinkResponse = json_body(response).await;
    assert_eq!(stats.hits, 1);
}

#[tokio::test]
async fn redirect_multiple_times_counter_accumulates() {
    let app = test_app();
    let custom_code = "counter-test";

    app.clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({
                "target_url": "https://example.com/test",
                "custom_code": custom_code
            }),
        ))
        .await
        .unwrap();

    for _ in 0..5 {
        let response = app
            .clone()
            .oneshot(get(&format!("/{}", custom_code)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    }

    let response = app
        .oneshot(get(&format!("/api/v1/links/{}", custom_code)))
        .await
        .unwrap();
    let stats: LinkResponse = json_body(response).await;
    assert_eq!(stats.hits, 5);
}

// ---------------------------------------------------------------------------
// 3. DELETE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_then_404() {
    let app = test_app();
    app.clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({"target_url": "https://example.com/", "custom_code": "gone"}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(delete_request("/api/v1/links/gone"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app
        .clone()
        .oneshot(get("/api/v1/links/gone"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .oneshot(delete_request("/api/v1/links/gone"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 4. Контракт ошибок
// ---------------------------------------------------------------------------

#[tokio::test]
async fn duplicate_code_conflict() {
    let app = test_app();
    let payload =
        serde_json::json!({"target_url": "https://example.com/", "custom_code": "occupied"});

    let response = app
        .clone()
        .oneshot(post_json("/api/v1/links", payload.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .oneshot(post_json("/api/v1/links", payload))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error: ErrorBody = json_body(response).await;
    assert_eq!(error.code, "code_taken");
    assert!(error.request_id.is_some());
}

#[tokio::test]
async fn invalid_url_is_422_in_unified_format() {
    for target in ["not a url at all", "ftp://example.com/file"] {
        let response = test_app()
            .oneshot(post_json(
                "/api/v1/links",
                serde_json::json!({"target_url": target}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let error: ErrorBody = json_body(response).await;
        assert_eq!(error.code, "validation_error");
    }
}

#[tokio::test]
async fn invalid_custom_code_is_422() {
    for code in ["ab", "тест-код", "x".repeat(33).as_str()] {
        let response = test_app()
            .oneshot(post_json(
                "/api/v1/links",
                serde_json::json!({"target_url": "https://example.com/", "custom_code": code}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[tokio::test]
async fn malformed_json_is_our_error_format_not_axum_default() {
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/links")
        .header(header::CONTENT_TYPE, mime::APPLICATION_JSON.as_ref())
        .body(Body::from("{not json"))
        .unwrap();
    let response = test_app().oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorBody = json_body(response).await;
    assert_eq!(error.code, "bad_request");
}

#[tokio::test]
async fn unknown_field_rejected_by_deny_unknown_fields() {
    let response = test_app()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({"target_url": "https://example.com/", "tarhet_url_typo": 1}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error: ErrorBody = json_body(response).await;
    assert_eq!(error.code, "validation_error");
}

#[tokio::test]
async fn unknown_paths_fall_back_to_unified_404() {
    for uri in [
        "/no-such-code",
        "/api/v1/links/absent",
        "/deeply/nested/path",
    ] {
        let response = test_app().oneshot(get(uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "uri: {uri}");
        let error: ErrorBody = json_body(response).await;
        assert_eq!(error.code, "not_found");
    }
}

#[tokio::test]
async fn oversized_body_is_413_in_unified_format() {
    let huge = "x".repeat(Config::default().max_body_bytes + 1);
    let response = test_app()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({"target_url": format!("https://example.com/{huge}")}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let error: ErrorBody = json_body(response).await;
    assert_eq!(error.code, "payload_too_large");
}

// ---------------------------------------------------------------------------
// 5. Rate Limiting - просто проверяем что работает
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_works() {
    // Создаем app с обычным лимитом
    let repo = Arc::new(InMemoryRepo::new());
    let stats_storage = Arc::new(domain::stats::StatsStorage::new());
    let config = Arc::new(Config::default());
    let state = AppState {
        repo: repo.clone(),
        stats_storage: stats_storage.clone(),
        config: config.clone(),
    };
    let app = build_router(state);

    // Отправляем несколько запросов
    let mut results = Vec::new();
    for i in 0..15 {
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/v1/links",
                serde_json::json!({
                    "target_url": format!("https://example.com/r{}", i),
                    "custom_code": format!("rl{}", i)
                }),
            ))
            .await
            .unwrap();
        results.push(response.status());
    }

    // Проверяем что хотя бы один запрос был отклонен (лимит 10 в минуту)
    let has_429 = results.iter().any(|&s| s == StatusCode::TOO_MANY_REQUESTS);
    assert!(has_429, "Should have at least one rate limited request");
}

// ---------------------------------------------------------------------------
// 6. Конкурентные тесты
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_redirects_count_exactly() {
    let app = test_app();
    let code = "stress";
    app.clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({"target_url": "https://example.com/", "custom_code": code}),
        ))
        .await
        .unwrap();

    let mut handles = Vec::new();
    for _ in 0..50 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let response = app.oneshot(get(&format!("/{}", code))).await.unwrap();
            assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let response = app
        .oneshot(get(&format!("/api/v1/links/{}", code)))
        .await
        .unwrap();
    let stats: LinkResponse = json_body(response).await;
    assert_eq!(stats.hits, 50);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_redirects_100_requests() {
    let app = test_app();
    let code = "stress100";
    app.clone()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({"target_url": "https://example.com/", "custom_code": code}),
        ))
        .await
        .unwrap();

    let mut handles = Vec::new();
    for _ in 0..100 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let response = app.oneshot(get(&format!("/{}", code))).await.unwrap();
            assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let response = app
        .oneshot(get(&format!("/api/v1/links/{}", code)))
        .await
        .unwrap();
    let stats: LinkResponse = json_body(response).await;
    assert_eq!(stats.hits, 100);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_create_same_code_only_one_succeeds() {
    let app = test_app();
    let code = "unique";
    let payload = serde_json::json!({
        "target_url": "https://example.com/",
        "custom_code": code
    });

    let mut handles = Vec::new();
    for _ in 0..20 {
        let app = app.clone();
        let payload = payload.clone();
        handles.push(tokio::spawn(async move {
            let response = app
                .oneshot(post_json("/api/v1/links", payload))
                .await
                .unwrap();
            response.status()
        }));
    }

    let mut created = 0;
    let mut conflicts = 0;
    for handle in handles {
        let status = handle.await.unwrap();
        match status {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => conflicts += 1,
            _ => panic!("unexpected status: {}", status),
        }
    }

    assert_eq!(created, 1, "Exactly one request should succeed");
    assert_eq!(conflicts, 19, "All others should get 409");
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_create_no_custom_code_all_succeed() {
    let app = test_app();
    let mut handles = Vec::new();

    for i in 0..20 {
        let app = app.clone();
        let payload = serde_json::json!({
            "target_url": format!("https://example.com/{}", i),
        });
        handles.push(tokio::spawn(async move {
            let response = app
                .oneshot(post_json("/api/v1/links", payload))
                .await
                .unwrap();
            response.status()
        }));
    }

    let mut created = 0;
    for handle in handles {
        let status = handle.await.unwrap();
        if status == StatusCode::CREATED {
            created += 1;
        }
    }

    assert_eq!(created, 20, "All 20 requests should succeed");
}

// ---------------------------------------------------------------------------
// 7. Health и Version
// ---------------------------------------------------------------------------

#[tokio::test]
async fn healthz_returns_ok() {
    let response = test_app().oneshot(get("/healthz")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = json_body(response).await;
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn version_returns_version() {
    let response = test_app().oneshot(get("/version")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = json_body(response).await;
    assert!(body["version"].is_string());
}

// ---------------------------------------------------------------------------
// 8. Request ID
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_id_present_in_success_response() {
    let response = test_app().oneshot(get("/healthz")).await.unwrap();
    assert!(response.headers().contains_key("x-request-id"));
}

#[tokio::test]
async fn request_id_present_in_error_response() {
    let response = test_app().oneshot(get("/non-existent-path")).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response.headers().contains_key("x-request-id"));

    let error: ErrorBody = json_body(response).await;
    assert!(error.request_id.is_some());
}

// ---------------------------------------------------------------------------
// 9. Content Type
// ---------------------------------------------------------------------------

#[tokio::test]
async fn content_type_is_json_for_api_errors() {
    let response = test_app().oneshot(get("/non-existent-path")).await.unwrap();

    let content_type = response.headers().get(header::CONTENT_TYPE);
    assert!(content_type.is_some());
    let content_type = content_type.unwrap().to_str().unwrap();
    assert!(content_type.contains("application/json"));
}

// ---------------------------------------------------------------------------
// 10. Валидация TTL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ttl_zero_is_rejected() {
    let response = test_app()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({
                "target_url": "https://example.com/",
                "ttl_seconds": 0
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error: ErrorBody = json_body(response).await;
    assert_eq!(error.code, "validation_error");
}

#[tokio::test]
async fn ttl_less_than_60_is_rejected() {
    let response = test_app()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({
                "target_url": "https://example.com/",
                "ttl_seconds": 30
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error: ErrorBody = json_body(response).await;
    assert_eq!(error.code, "validation_error");
}

// ---------------------------------------------------------------------------
// 11. Длинный URL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn url_too_long_is_rejected() {
    let long_url = format!(
        "https://example.com/{}",
        "a".repeat(Config::default().max_body_bytes)
    );
    let response = test_app()
        .oneshot(post_json(
            "/api/v1/links",
            serde_json::json!({
                "target_url": long_url
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let error: ErrorBody = json_body(response).await;
    assert_eq!(error.code, "payload_too_large");
}
